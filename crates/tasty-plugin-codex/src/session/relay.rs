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
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
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
        out.timeout(Some(Duration::from_secs(10)))?;
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
            Self::Tcp(s) => s.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Self::Unix(s) => s.shutdown(Shutdown::Both),
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
}
impl Read for Duplex {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.read.read(b)
    }
}
impl Write for Duplex {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.write
            .lock()
            .map_err(|_| io::Error::other("relay writer poisoned"))?
            .write(b)
    }
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.write
            .lock()
            .map_err(|_| io::Error::other("relay writer poisoned"))?
            .write_all(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn pair(s: &Stream, server: bool) -> io::Result<(WsConn<Duplex>, WsConn<Duplex>)> {
    let write = Arc::new(Mutex::new(s.clone_stream()?));
    Ok((
        WsConn::established(
            Duplex {
                read: s.clone_stream()?,
                write: write.clone(),
            },
            server,
        ),
        WsConn::established(
            Duplex {
                read: s.clone_stream()?,
                write,
            },
            server,
        ),
    ))
}

fn header(s: &mut Stream) -> io::Result<String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut b = [0];
        s.read_exact(&mut b)?;
        bytes.push(b[0]);
        if bytes.len() > 16384 {
            return Err(io::Error::other("relay header too large"));
        }
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

pub(crate) struct Relay {
    pub url: String,
    pub token: String,
    stop: Arc<AtomicBool>,
    streams: Arc<Mutex<Vec<Weak<Stream>>>>,
    task: Option<JoinHandle<()>>,
}
impl Relay {
    pub(crate) fn start(
        endpoint: Endpoint,
        settings: ResumeSettings,
        bind: Bind,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("ws://127.0.0.1:{}", listener.local_addr()?.port());
        let token = crate::ids::random_hex(32)?;
        let stop = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(Mutex::new(Vec::new()));
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
    sockets: &Arc<Mutex<Vec<Weak<Stream>>>>,
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

/// 열린 연결을 모두 닫는다.
fn close_streams(sockets: &Mutex<Vec<Weak<Stream>>>) {
    // 레지스트리는 Weak 목록만 retain/push한다. 프레임 writer를 복구하는 것이 아니라
    // 남아 있는 소켓의 shutdown 의무를 이행하므로 poison 뒤에도 목록을 읽을 수 있다.
    let sockets = tasty_utils::poison::recover_mutex(
        sockets.lock(),
        "codex relay socket registry",
        &SOCKETS_POISON_REPORTED,
    );
    for socket in sockets.iter() {
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

fn track(socket: &Stream, sockets: &Mutex<Vec<Weak<Stream>>>) -> io::Result<CloseOnDrop> {
    let socket = Arc::new(socket.clone_stream()?);
    let mut live = sockets
        .lock()
        .map_err(|_| io::Error::other("relay socket list poisoned"))?;
    live.retain(|socket| socket.strong_count() > 0);
    live.push(Arc::downgrade(&socket));
    Ok(CloseOnDrop(socket))
}

fn connection(
    mut front: Stream,
    endpoint: &Endpoint,
    token: &str,
    (settings, chosen): (&ResumeSettings, Chosen),
    bind: Bind,
    sockets: &Mutex<Vec<Weak<Stream>>>,
) -> io::Result<()> {
    let _front_guard = track(&front, sockets)?;
    front.timeout(Some(Duration::from_secs(10)))?;
    let request = header(&mut front)?;
    let auth = format!("Bearer {token}");
    if !request
        .lines()
        .filter_map(|l| l.split_once(':'))
        .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && v.trim() == auth)
    {
        front.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")?;
        return Err(io::Error::other("relay authentication failed"));
    }
    let mut backend = Stream::connect(endpoint)?;
    let _backend_guard = track(&backend, sockets)?;
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
    backend.write_all(forwarded.as_bytes())?;
    let response = header(&mut backend)?;
    front.write_all(response.as_bytes())?;
    if !response.lines().next().is_some_and(|l| l.contains(" 101 ")) {
        return Err(io::Error::other("backend refused relay connection"));
    }
    front.timeout(None)?;
    backend.timeout(None)?;
    let (mut from_tui, mut to_tui) = pair(&front, true)?;
    let (mut from_server, mut to_server) = pair(&backend, false)?;
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let (requests, overrides) = (pending.clone(), settings.clone());
    let close = backend.clone_stream()?;
    let upstream = std::thread::spawn(move || {
        let result = (|| -> io::Result<()> {
            loop {
                let text = from_tui.recv_text()?;
                let rewritten = message::request(&text, &overrides, &chosen, &requests)?;
                to_server.send_text(rewritten.as_deref().unwrap_or(&text))?;
            }
        })();
        report_transfer_error("TUI to server", &result);
        close.close();
        result
    });
    let result = (|| -> io::Result<()> {
        loop {
            let text = from_server.recv_text()?;
            let rewritten = message::reply(&text, &pending, &bind)?;
            to_tui.send_text(rewritten.as_deref().unwrap_or(&text))?;
        }
    })();
    report_transfer_error("server to TUI", &result);
    front.close();
    backend.close();
    // Both directions report their original error before closing the peer reader. Do not lose
    // a normal thread return containing Err when joining the TUI-to-server worker.
    let upstream_result = upstream
        .join()
        .map_err(|_| io::Error::other("codex relay upstream panicked"))?;
    result.and(upstream_result)
}

fn report_transfer_error(direction: &str, result: &io::Result<()>) {
    if let Err(error) = result {
        match error.kind() {
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe => {
                tracing::debug!(direction, %error, "codex relay disconnected");
            }
            _ => tracing::warn!(direction, %error, "codex relay transfer failed"),
        }
    }
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "relay_large_tests.rs"]
mod large_tests;
