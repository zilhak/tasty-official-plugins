//! A launch-scoped relay for the native resume picker. Permission overrides are applied to
//! thread requests because the remote TUI cannot send them when resuming a conversation.
//!
//! The native TUI is started without launch permission options so that its own `/resume`
//! keeps working. Codex 0.154 then fills thread requests with its configured defaults, so the
//! launch policy replaces those values until the user picks permissions in the TUI, which the
//! TUI reports with `thread/settings/update`.
#[path = "relay_message.rs"]
mod message;

use super::command::ResumeSettings;
use crate::{rpc::Endpoint, ws::WsConn};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

type Bind = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;
type Pending = Arc<Mutex<HashMap<String, ResumeSettings>>>;
/// Set once the TUI reports a permission choice made after launch.
type Chosen = Arc<AtomicBool>;

#[derive(Debug)]
enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}
impl Stream {
    fn connect(endpoint: &Endpoint) -> io::Result<Self> {
        let out = match endpoint {
            Endpoint::Ws { port, .. } => Self::Tcp(TcpStream::connect_timeout(
                &([127, 0, 0, 1], *port).into(),
                Duration::from_secs(5),
            )?),
            #[cfg(unix)]
            Endpoint::Unix(path) => Self::Unix(std::os::unix::net::UnixStream::connect(path)?),
            _ => return Err(io::Error::other("relay requires an owned server")),
        };
        Ok(out)
    }
    fn clone_stream(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(s) => s.try_clone().map(Self::Tcp),
            #[cfg(unix)]
            Self::Unix(s) => s.try_clone().map(Self::Unix),
        }
    }
    fn timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => {
                s.set_read_timeout(timeout)?;
                s.set_write_timeout(timeout)
            }
            #[cfg(unix)]
            Self::Unix(s) => {
                s.set_read_timeout(timeout)?;
                s.set_write_timeout(timeout)
            }
        }
    }
    fn close(&self) {
        let result = match self {
            // Windows에서는 shutdown만으로 다른 복제의 막힌 읽기가 깨어나지 않는다.
            Self::Tcp(s) => tasty_utils::socket_wake::shutdown_both_and_wake(s),
            #[cfg(unix)]
            Self::Unix(s) => s.shutdown(std::net::Shutdown::Both),
        };
        if let Err(error) = result {
            tracing::debug!("codex relay close: {error}");
        }
    }
}
impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(b),
            #[cfg(unix)]
            Self::Unix(s) => s.read(b),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(b),
            #[cfg(unix)]
            Self::Unix(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// A reader may answer pings while the opposite direction forwards messages. Whole frames
// share a writer lock so they cannot interleave on a cloned socket.
struct Duplex {
    read: Stream,
    write: Arc<Mutex<Stream>>,
    /// 이 `Duplex`를 쓰는 전송 방향의 소켓 실패 표지. 소켓 읽기·쓰기가 실패하면 선다.
    /// WebSocket 프레임 판정 오류와 소켓 오류를 가르는 데 쓴다.
    fault: Arc<AtomicBool>,
}
impl Read for Duplex {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let result = self.read.read(b);
        // read_exact가 Interrupted를 다시 시도하므로 그것만 뺀다. 빈 버퍼가 아닌데 0이면 EOF다.
        match &result {
            Err(error) if error.kind() != io::ErrorKind::Interrupted => {
                self.fault.store(true, Ordering::Release)
            }
            Ok(0) if !b.is_empty() => self.fault.store(true, Ordering::Release),
            _ => {}
        }
        result
    }
}
impl Write for Duplex {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let result = self
            .write
            .lock()
            .map_err(|_| io::Error::other("relay writer poisoned"))?
            .write(b);
        if result.is_err() {
            self.fault.store(true, Ordering::Release);
        }
        result
    }
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        let result = self
            .write
            .lock()
            .map_err(|_| io::Error::other("relay writer poisoned"))?
            .write_all(b);
        if result.is_err() {
            self.fault.store(true, Ordering::Release);
        }
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// 한 소켓의 reader 쪽과 writer 쪽 연결. 표지는 각 쪽을 쓰는 전송 방향의 것이다.
fn pair(
    s: &Stream,
    server: bool,
    reader_fault: Arc<AtomicBool>,
    writer_fault: Arc<AtomicBool>,
) -> io::Result<(WsConn<Duplex>, WsConn<Duplex>)> {
    let write = Arc::new(Mutex::new(s.clone_stream()?));
    Ok((
        WsConn::established(
            Duplex {
                read: s.clone_stream()?,
                write: write.clone(),
                fault: reader_fault,
            },
            server,
        ),
        WsConn::established(
            Duplex {
                read: s.clone_stream()?,
                write,
                fault: writer_fault,
            },
            server,
        ),
    ))
}

fn header(s: &mut Stream, site: Site) -> Result<String, Failure> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut b = [0];
        s.read_exact(&mut b).at(site, Source::SocketIo)?;
        bytes.push(b[0]);
        if bytes.len() > 16384 {
            return Err(io::Error::other("relay header too large")).at(site, Source::Other);
        }
    }
    String::from_utf8(bytes)
        .map_err(io::Error::other)
        .at(site, Source::Other)
}

/// 연결이 실패한 호출의 종류. 오류 코드가 아니라 실패한 호출 자리로 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// 소켓 기한 설정.
    Timeout,
    /// 소켓 읽기·쓰기.
    SocketIo,
    /// 그 밖(프레임·메시지 판정, 인증, 연결, 복제 등).
    Other,
}

/// 연결 안에서 실패한 호출 자리.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Site {
    FrontTrack,
    FrontTimeout,
    FrontHeader,
    FrontReject,
    Auth,
    Connect,
    BackendTimeout,
    BackendTrack,
    BackendRequest,
    BackendHeader,
    FrontResponse,
    Refused,
    FrontUnlimit,
    BackendUnlimit,
    Pair,
    CloseHandle,
    Read(Direction),
    Write(Direction),
    Message(Direction),
    /// upstream 스레드 회수.
    Join,
}

struct Failure {
    site: Site,
    source: Source,
    error: io::Error,
}

trait At<T> {
    fn at(self, site: Site, source: Source) -> Result<T, Failure>;
}
impl<T> At<T> for io::Result<T> {
    fn at(self, site: Site, source: Source) -> Result<T, Failure> {
        self.map_err(|error| Failure {
            site,
            source,
            error,
        })
    }
}

/// 전송 루프의 실패. 그 방향의 소켓 실패 표지가 섰으면 소켓 I/O 실패다.
fn transfer<T>(result: io::Result<T>, site: Site, fault: &AtomicBool) -> Result<T, Failure> {
    result.map_err(|error| Failure {
        site,
        source: if fault.load(Ordering::Acquire) {
            Source::SocketIo
        } else {
            Source::Other
        },
        error,
    })
}

/// 분류한 연결의 끝.
enum Ended {
    /// relay 종료와 겹친 기한 설정·소켓 I/O 실패. 원 오류는 debug 로그에만 남긴다.
    Closing(Failure),
    /// 그 밖의 실패. 원 오류 그대로 보고한다.
    Failed(io::Error),
}
impl Ended {
    fn into_error(self) -> io::Error {
        match self {
            Self::Closing(_) => io::Error::other("relay closing"),
            Self::Failed(error) => error,
        }
    }
}

/// 실패 직후 relay 종료 표지를 보고 연결의 끝을 정한다.
fn classify(failure: Failure, sockets: &Sockets) -> Ended {
    #[cfg(test)]
    sockets.mark(Point::Failed {
        site: failure.site,
        source: failure.source,
        raw_os_error: failure.error.raw_os_error(),
    });
    // close_streams와 같은 복구 규칙으로 값만 복사하고 가드를 곧바로 놓는다.
    let closing = tasty_utils::poison::recover_mutex(
        sockets.state.lock(),
        "codex relay socket registry",
        &SOCKETS_POISON_REPORTED,
    )
    .closing;
    match failure.source {
        Source::Timeout | Source::SocketIo if closing => Ended::Closing(failure),
        _ => Ended::Failed(failure.error),
    }
}

pub(crate) struct Relay {
    pub url: String,
    pub token: String,
    stop: Arc<AtomicBool>,
    streams: Arc<Sockets>,
    task: Option<JoinHandle<()>>,
}
impl Relay {
    pub(crate) fn start(
        endpoint: Endpoint,
        settings: ResumeSettings,
        bind: Bind,
    ) -> io::Result<Self> {
        Self::start_with(endpoint, settings, bind, Sockets::new())
    }

    fn start_with(
        endpoint: Endpoint,
        settings: ResumeSettings,
        bind: Bind,
        sockets: Sockets,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("ws://127.0.0.1:{}", listener.local_addr()?.port());
        let token = crate::ids::random_hex(32)?;
        let stop = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(sockets);
        let (done, sockets, secret) = (stop.clone(), streams.clone(), token.clone());
        let chosen: Chosen = Arc::new(AtomicBool::new(false));
        let task = std::thread::Builder::new()
            .name("codex-tui-relay".into())
            .spawn(move || {
                let mut connections: Vec<JoinHandle<()>> = Vec::new();
                while !done.load(Ordering::Acquire) {
                    reap_finished(&mut connections);
                    match listener.accept() {
                        Ok((socket, _)) => {
                            if connections.len() >= 16 {
                                drop(socket);
                                continue;
                            }
                            // Windows와 BSD 계열에서는 accept한 소켓이 listener의 nonblocking을 물려받는다.
                            if let Err(error) = socket.set_nonblocking(false) {
                                tracing::warn!("codex relay socket: {error}");
                                continue;
                            }
                            connections.push(spawn_connection(
                                socket, &endpoint, &settings, &chosen, &bind, &secret, &sockets,
                            ));
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        Err(error) => {
                            tracing::warn!("codex relay accept: {error}");
                            break;
                        }
                    }
                }
                close_streams(&sockets);
                join_all(connections);
            })?;
        Ok(Self {
            url,
            token,
            stop,
            streams,
            task: Some(task),
        })
    }
}

/// 끝난 연결 스레드를 거둔다.
fn reap_finished(connections: &mut Vec<JoinHandle<()>>) {
    let mut index = 0;
    while index < connections.len() {
        if connections[index].is_finished() {
            if connections.swap_remove(index).join().is_err() {
                tracing::warn!("codex relay connection panicked");
            }
        } else {
            index += 1;
        }
    }
}

/// 받아들인 연결 하나를 새 스레드에서 중계한다.
fn spawn_connection(
    socket: TcpStream,
    endpoint: &Endpoint,
    settings: &ResumeSettings,
    chosen: &Chosen,
    bind: &Bind,
    secret: &str,
    sockets: &Arc<Sockets>,
) -> JoinHandle<()> {
    let (endpoint, settings, chosen, bind, secret, sockets) = (
        endpoint.clone(),
        settings.clone(),
        chosen.clone(),
        bind.clone(),
        secret.to_string(),
        sockets.clone(),
    );
    std::thread::spawn(move || {
        if let Err(error) = connection(
            Stream::Tcp(socket),
            &endpoint,
            &secret,
            (&settings, chosen),
            bind,
            &sockets,
        ) {
            tracing::debug!("codex relay connection: {error}");
        }
    })
}

/// 남은 연결 스레드가 끝날 때까지 기다린다.
fn join_all(connections: Vec<JoinHandle<()>>) {
    for task in connections {
        if task.join().is_err() {
            tracing::warn!("codex relay connection panicked");
        }
    }
}

static SOCKETS_POISON_REPORTED: AtomicBool = AtomicBool::new(false);

/// 연결이 쓰는 소켓 목록과 relay 종료 표지. 표지와 등록은 같은 락 아래에서만 바뀐다.
struct Sockets {
    state: Mutex<SocketState>,
    #[cfg(test)]
    probe: Option<Probe>,
}
struct SocketState {
    /// relay 종료가 시작됐다. 한 번 서면 내려가지 않는다.
    closing: bool,
    live: Vec<Weak<Stream>>,
}
impl Sockets {
    fn new() -> Self {
        Self {
            state: Mutex::new(SocketState {
                closing: false,
                live: Vec::new(),
            }),
            #[cfg(test)]
            probe: None,
        }
    }
    #[cfg(test)]
    fn with_probe(probe: Probe) -> Self {
        Self {
            probe: Some(probe),
            ..Self::new()
        }
    }
    /// 시험이 연결의 진행 지점을 보고 순서를 정한다. 레지스트리 락을 쥔 채 부르지 않는다.
    #[cfg(test)]
    fn mark(&self, point: Point) {
        if let Some(probe) = &self.probe {
            probe(point);
        }
    }
}

#[cfg(test)]
type Probe = Arc<dyn Fn(Point) + Send + Sync>;
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Front,
    Backend,
}
/// 전송 방향.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// TUI에서 서버로.
    Up,
    /// 서버에서 TUI로.
    Down,
}
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
enum Point {
    BeforeTrack(Side),
    AfterTrack(Side),
    TrackRefused,
    /// 전송 루프가 `recv_text`에 들어가기 직전.
    BeforeRead(Direction),
    /// 업그레이드 뒤 소켓 기한을 풀기 직전.
    BeforeUnlimit(Side),
    /// 전송 루프가 `send_text`에 들어가기 직전.
    BeforeWrite(Direction),
    /// `report_transfer_error`가 고른 분기.
    Reported {
        direction: &'static str,
        closing: bool,
        warn: bool,
    },
    /// 분류 직전의 실패 자리와 원 오류.
    Failed {
        site: Site,
        source: Source,
        raw_os_error: Option<i32>,
    },
}

/// 종료 표지를 세우고 열린 연결을 모두 닫는다. 두 번 불려도 된다.
fn close_streams(sockets: &Sockets) {
    // 레지스트리는 Weak 목록만 retain/push한다. 프레임 writer를 복구하는 것이 아니라
    // 남아 있는 소켓의 shutdown 의무를 이행하므로 poison 뒤에도 목록을 읽을 수 있다.
    let mut state = tasty_utils::poison::recover_mutex(
        sockets.state.lock(),
        "codex relay socket registry",
        &SOCKETS_POISON_REPORTED,
    );
    // 표지를 같은 락 안에서 세우므로, 이 순회 뒤에 등록하려는 소켓은 track이 거절한다.
    state.closing = true;
    for socket in state.live.iter() {
        if let Some(socket) = socket.upgrade() {
            socket.close();
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        close_streams(&self.streams);
        if let Some(task) = self.task.take()
            && task.join().is_err()
        {
            tracing::warn!("codex relay panicked");
        }
    }
}

/// Whether a `thread/settings/update` from the TUI carries a permission choice. Codex 0.154
/// sends these fields as null when only the model or effort changes.
fn is_permission_choice(params: &Value) -> bool {
    [
        "approvalPolicy",
        "approvalsReviewer",
        "sandboxPolicy",
        "permissions",
    ]
    .iter()
    .any(|key| params.get(*key).is_some_and(|value| !value.is_null()))
}

/// Puts the launch policy into a thread request and returns the settings the reply must show.
/// Before the user picks permissions in the TUI, its approval and sandbox values are defaults
/// and are replaced. After a choice, present values are kept and only missing ones are filled.
fn apply_settings(
    params: &mut serde_json::Map<String, Value>,
    defaults: &ResumeSettings,
    chosen: bool,
) -> ResumeSettings {
    let config = params.get("config").and_then(Value::as_object);
    let permission_config = config.is_some_and(|config| {
        [
            "approval_policy",
            "approvals_reviewer",
            "sandbox_mode",
            "default_permissions",
            "permissions",
            "network",
            "sandbox_workspace_write",
        ]
        .iter()
        .any(|key| config.get(*key).is_some_and(|value| !value.is_null()))
    });
    let model_config =
        config.is_some_and(|config| config.get("model").is_some_and(|value| !value.is_null()));
    for (key, value) in defaults.params("").as_object().into_iter().flatten() {
        if (permission_config && matches!(key.as_str(), "approvalPolicy" | "sandbox"))
            || (model_config && key == "model")
        {
            continue;
        }
        if key == "threadId" || key == "excludeTurns" || key == "config" {
            continue;
        }
        let launch_wins = !chosen && matches!(key.as_str(), "approvalPolicy" | "sandbox");
        if launch_wins || params.get(key).is_none_or(Value::is_null) {
            params.insert(key.clone(), value.clone());
        }
    }
    // Fill an omitted or null effort from this invocation, preserving explicit effort and
    // unrelated config keys. The final request effort is verified against the server reply.
    if let Some(effort) = &defaults.effort {
        let config = params
            .entry("config")
            .or_insert_with(|| serde_json::json!({}));
        if config.is_null() {
            *config = serde_json::json!({});
        }
        if let Some(config) = config.as_object_mut()
            && config
                .get("model_reasoning_effort")
                .is_none_or(Value::is_null)
        {
            config.insert("model_reasoning_effort".into(), effort.clone().into());
        }
    }
    let effort = params
        .get("config")
        .and_then(|c| c.get("model_reasoning_effort"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let string = |key| params.get(key).and_then(Value::as_str).map(str::to_owned);
    ResumeSettings {
        approval: string("approvalPolicy"),
        sandbox: string("sandbox"),
        model: string("model"),
        effort,
    }
}

struct CloseOnDrop(Arc<Stream>);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// 소켓을 relay 종료 때 닫을 목록에 넣는다. 종료가 이미 시작됐으면 넣지 않고 바로 닫는다.
fn track(socket: &Stream, sockets: &Sockets) -> io::Result<CloseOnDrop> {
    let socket = Arc::new(socket.clone_stream()?);
    let mut state = sockets
        .state
        .lock()
        .map_err(|_| io::Error::other("relay socket list poisoned"))?;
    if state.closing {
        drop(state);
        socket.close();
        #[cfg(test)]
        sockets.mark(Point::TrackRefused);
        return Err(io::Error::other("relay closing"));
    }
    state.live.retain(|socket| socket.strong_count() > 0);
    state.live.push(Arc::downgrade(&socket));
    Ok(CloseOnDrop(socket))
}

fn connection(
    mut front: Stream,
    endpoint: &Endpoint,
    token: &str,
    (settings, chosen): (&ResumeSettings, Chosen),
    bind: Bind,
    sockets: &Arc<Sockets>,
) -> io::Result<()> {
    // 등록한 소켓은 연결이 끝날 때 닫는다.
    let mut guards = Vec::new();
    let upgraded = (|| -> Result<Stream, Failure> {
        #[cfg(test)]
        sockets.mark(Point::BeforeTrack(Side::Front));
        guards.push(track(&front, sockets).at(Site::FrontTrack, Source::Other)?);
        #[cfg(test)]
        sockets.mark(Point::AfterTrack(Side::Front));
        front
            .timeout(Some(Duration::from_secs(10)))
            .at(Site::FrontTimeout, Source::Timeout)?;
        let request = header(&mut front, Site::FrontHeader)?;
        let auth = format!("Bearer {token}");
        if !request
            .lines()
            .filter_map(|l| l.split_once(':'))
            .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && v.trim() == auth)
        {
            front
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .at(Site::FrontReject, Source::SocketIo)?;
            return Err(io::Error::other("relay authentication failed"))
                .at(Site::Auth, Source::Other);
        }
        let mut backend = Stream::connect(endpoint).at(Site::Connect, Source::Other)?;
        backend
            .timeout(Some(Duration::from_secs(10)))
            .at(Site::BackendTimeout, Source::Timeout)?;
        #[cfg(test)]
        sockets.mark(Point::BeforeTrack(Side::Backend));
        guards.push(track(&backend, sockets).at(Site::BackendTrack, Source::Other)?);
        #[cfg(test)]
        sockets.mark(Point::AfterTrack(Side::Backend));
        let mut forwarded = request
            .lines()
            .filter(|l| {
                !l.is_empty()
                    && !l
                        .split_once(':')
                        .is_some_and(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        if let Endpoint::Ws { token, .. } = endpoint {
            forwarded.push_str(&format!("\r\nAuthorization: Bearer {token}"));
        }
        forwarded.push_str("\r\n\r\n");
        backend
            .write_all(forwarded.as_bytes())
            .at(Site::BackendRequest, Source::SocketIo)?;
        let response = header(&mut backend, Site::BackendHeader)?;
        front
            .write_all(response.as_bytes())
            .at(Site::FrontResponse, Source::SocketIo)?;
        if !response.lines().next().is_some_and(|l| l.contains(" 101 ")) {
            return Err(io::Error::other("backend refused relay connection"))
                .at(Site::Refused, Source::Other);
        }
        #[cfg(test)]
        sockets.mark(Point::BeforeUnlimit(Side::Front));
        front
            .timeout(None)
            .at(Site::FrontUnlimit, Source::Timeout)?;
        #[cfg(test)]
        sockets.mark(Point::BeforeUnlimit(Side::Backend));
        backend
            .timeout(None)
            .at(Site::BackendUnlimit, Source::Timeout)?;
        Ok(backend)
    })();
    let backend = match upgraded {
        Ok(backend) => backend,
        Err(failure) => {
            let ended = classify(failure, sockets);
            if let Ended::Closing(failure) = &ended {
                tracing::debug!(site = ?failure.site, error = %failure.error, "codex relay closing");
            }
            return Err(ended.into_error());
        }
    };
    let ended = (|| -> Result<Ended, Failure> {
        // 방향마다 소켓 실패 표지 하나. upstream은 TUI 읽기와 서버 쓰기, downstream은 그 반대다.
        let (up, down) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (mut from_tui, mut to_tui) =
            pair(&front, true, up.clone(), down.clone()).at(Site::Pair, Source::Other)?;
        let (mut from_server, mut to_server) =
            pair(&backend, false, down.clone(), up.clone()).at(Site::Pair, Source::Other)?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (requests, overrides) = (pending.clone(), settings.clone());
        let close = backend
            .clone_stream()
            .at(Site::CloseHandle, Source::Other)?;
        let registry = sockets.clone();
        let upstream = std::thread::spawn(move || {
            let result = (|| -> Result<Infallible, Failure> {
                loop {
                    #[cfg(test)]
                    registry.mark(Point::BeforeRead(Direction::Up));
                    let text = transfer(from_tui.recv_text(), Site::Read(Direction::Up), &up)?;
                    let rewritten = message::request(&text, &overrides, &chosen, &requests)
                        .at(Site::Message(Direction::Up), Source::Other)?;
                    #[cfg(test)]
                    registry.mark(Point::BeforeWrite(Direction::Up));
                    transfer(
                        to_server.send_text(rewritten.as_deref().unwrap_or(&text)),
                        Site::Write(Direction::Up),
                        &up,
                    )?;
                }
            })();
            let Err(failure) = result;
            let ended = classify(failure, &registry);
            report_transfer_error("TUI to server", &ended, &registry);
            close.close();
            ended
        });
        let result = (|| -> Result<Infallible, Failure> {
            loop {
                #[cfg(test)]
                sockets.mark(Point::BeforeRead(Direction::Down));
                let text = transfer(from_server.recv_text(), Site::Read(Direction::Down), &down)?;
                let rewritten = message::reply(&text, &pending, &bind)
                    .at(Site::Message(Direction::Down), Source::Other)?;
                #[cfg(test)]
                sockets.mark(Point::BeforeWrite(Direction::Down));
                transfer(
                    to_tui.send_text(rewritten.as_deref().unwrap_or(&text)),
                    Site::Write(Direction::Down),
                    &down,
                )?;
            }
        })();
        let Err(failure) = result;
        let downstream = classify(failure, sockets);
        report_transfer_error("server to TUI", &downstream, sockets);
        front.close();
        backend.close();
        // Both directions report their original error before closing the peer reader. Do not lose
        // a normal thread return containing Err when joining the TUI-to-server worker.
        let upstream = upstream.join().map_err(|_| Failure {
            site: Site::Join,
            source: Source::Other,
            error: io::Error::other("codex relay upstream panicked"),
        })?;
        // relay closing으로 분류한 쪽보다 원 오류를 앞세운다. 그 밖은 downstream 먼저.
        Ok(match (downstream, upstream) {
            (Ended::Closing(_), upstream @ Ended::Failed(_)) => upstream,
            (downstream, _) => downstream,
        })
    })();
    let ended = match ended {
        Ok(ended) => ended,
        Err(failure) => classify(failure, sockets),
    };
    Err(ended.into_error())
}

fn report_transfer_error(direction: &'static str, ended: &Ended, sockets: &Sockets) {
    let warn = match ended {
        Ended::Closing(failure) => {
            tracing::debug!(direction, site = ?failure.site, error = %failure.error, "codex relay closing");
            false
        }
        Ended::Failed(error) => match error.kind() {
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe => {
                tracing::debug!(direction, %error, "codex relay disconnected");
                false
            }
            _ => {
                tracing::warn!(direction, %error, "codex relay transfer failed");
                true
            }
        },
    };
    #[cfg(test)]
    sockets.mark(Point::Reported {
        direction,
        closing: matches!(ended, Ended::Closing(_)),
        warn,
    });
    #[cfg(not(test))]
    let _ = (warn, sockets);
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "relay_large_tests.rs"]
mod large_tests;
