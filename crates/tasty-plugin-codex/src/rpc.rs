//! Codex app-server JSON-RPC 클라이언트.
//!
//! 연결 대상은 셋이다. Tasty가 띄운 세션 서버의 unix socket(Unix), 같은 서버의 loopback
//! WebSocket(Windows, capability token 필수), 사용자의 공유 daemon에 붙는 공식
//! `codex app-server proxy`의 stdio다. 주소나 token을 추측하지 않고 Tasty가 만든 endpoint 또는
//! 공식 proxy만 사용한다.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ws::WsConn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// Tasty 세션 서버의 unix socket 경로.
    Unix(PathBuf),
    /// Tasty 세션 서버의 loopback WebSocket 포트와 capability token.
    Ws { port: u16, token: String },
    /// 사용자의 공유 daemon. codex_home이 있으면 proxy 프로세스에만 CODEX_HOME으로 준다.
    Proxy {
        codex_home: Option<PathBuf>,
        execution: Option<crate::execution::CodexExecution>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RpcError {
    /// 요청을 보내기 전에 실패했다(연결·handshake·initialize).
    NotSent(String),
    /// 서버가 JSON-RPC 오류로 답했다.
    Server { code: i64, message: String },
    /// 요청을 보낸 뒤 응답을 받지 못했다. 서버가 처리했는지 알 수 없다.
    Ambiguous(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent(m) => write!(f, "not sent: {m}"),
            Self::Server { code, message } => write!(f, "server error {code}: {message}"),
            Self::Ambiguous(m) => write!(f, "no response: {m}"),
        }
    }
}

trait Duplex: Read + Write + Send {}
impl<T: Read + Write + Send> Duplex for T {}

struct ProxyStream {
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl Read for ProxyStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Write for ProxyStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stdin.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stdin.flush()
    }
}

/// proxy 프로세스와 그 하위 프로세스. npm의 `codex`는 실제 proxy를 다시 띄우는 래퍼이고 실제
/// proxy가 stdout 파이프를 그대로 물려받는다. 래퍼만 끝내면 실제 proxy가 파이프를 쥔 채 남아 막힌
/// 읽기가 풀리지 않는다. Unix는 proxy를 자기 프로세스 그룹으로 띄워 그룹 전체를, Windows는 Job
/// Object로 하위 프로세스까지 끝낸다. proxy는 이미 실행 중인 daemon의 소켓에 바이트만 전달하므로
/// 그룹을 끝내도 사용자의 daemon은 영향을 받지 않는다.
///
/// 정리는 한 번만 한다. 첫 [`OwnedProxy::stop`]이 자식의 소유권을 가져가 회수하므로, 감시 스레드와
/// guard drop이 모두 불러도 회수한 래퍼 PID(다른 프로세스에 재사용될 수 있다)로 그룹 신호를 다시
/// 보내지 않는다.
struct OwnedProxy {
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<tasty_reaper::JobObject>,
}

impl OwnedProxy {
    fn spawn(mut cmd: Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let child = cmd.spawn()?;
        #[cfg(windows)]
        let job = match tasty_reaper::JobObject::new().and_then(|job| {
            job.assign_pid(child.id())?;
            Ok(job)
        }) {
            Ok(job) => Some(job),
            Err(e) => {
                tracing::warn!("codex proxy job object: {e}");
                None
            }
        };
        Ok(Self {
            child: Some(child),
            #[cfg(windows)]
            job,
        })
    }

    /// 그룹(또는 job) 전체를 바로 끝내고 래퍼를 회수한다. 래퍼가 이미 끝났어도 첫 호출은 남은
    /// 하위 프로세스를 끝낸다. 상태를 가진 프로그램이 아니므로 정상 종료를 기다리지 않는다.
    /// 이미 정리했으면 아무것도 하지 않고 false를 돌려준다.
    fn stop(&mut self) -> bool {
        let Some(mut child) = self.child.take() else {
            return false;
        };
        #[cfg(windows)]
        if let Some(job) = self.job.take()
            && let Err(e) = job.terminate()
        {
            tracing::debug!("codex proxy job terminate: {e}");
        }
        if let Err(e) = tasty_reaper::terminate_process_group(&mut child, Duration::ZERO) {
            tracing::warn!("codex proxy stop: {e}");
        }
        true
    }
}

/// proxy 프로세스를 소유한다. 기한이 지나면 감시 스레드가 종료해 막힌 읽기를 푼다.
struct ProxyGuard {
    proxy: Arc<Mutex<OwnedProxy>>,
    cancel: Option<mpsc::Sender<()>>,
}

impl Drop for ProxyGuard {
    fn drop(&mut self) {
        // 감시 스레드를 먼저 깨우고 소유한 proxy를 정리한다.
        drop(self.cancel.take());
        tasty_utils::poison::recover_mutex(
            self.proxy.lock(),
            "codex proxy child",
            &PROXY_POISON_REPORTED,
        )
        .stop();
    }
}

/// proxy를 띄우고 `timeout` 뒤 그룹을 끝내는 감시 스레드를 붙인다.
fn start_proxy(cmd: Command, timeout: Duration) -> Result<(ProxyStream, ProxyGuard), RpcError> {
    let not_sent = |e: std::io::Error| RpcError::NotSent(e.to_string());
    let mut proxy = OwnedProxy::spawn(cmd).map_err(not_sent)?;
    let stdio = proxy
        .child
        .as_mut()
        .map(|c| (c.stdin.take(), c.stdout.take()));
    let Some((Some(stdin), Some(stdout))) = stdio else {
        proxy.stop();
        return Err(RpcError::NotSent("codex proxy stdio unavailable".into()));
    };
    let proxy = Arc::new(Mutex::new(proxy));
    let (cancel, cancelled) = mpsc::channel::<()>();
    let watched = proxy.clone();
    let deadline = Instant::now() + timeout;
    let watcher = std::thread::Builder::new()
        .name("codex-proxy-deadline".into())
        .spawn(move || {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Err(mpsc::RecvTimeoutError::Timeout) = cancelled.recv_timeout(left) {
                tasty_utils::poison::recover_mutex(
                    watched.lock(),
                    "codex proxy child",
                    &PROXY_POISON_REPORTED,
                )
                .stop();
            }
        });
    // 감시 스레드가 없으면 guard drop이 proxy를 정리한다.
    let guard = ProxyGuard {
        proxy,
        cancel: Some(cancel),
    };
    watcher.map_err(not_sent)?;
    Ok((ProxyStream { stdin, stdout }, guard))
}

const PROXY_ARGS: [&str; 2] = ["app-server", "proxy"];

fn proxy_command(
    codex_home: Option<&std::path::Path>,
    execution: Option<&crate::execution::CodexExecution>,
) -> std::io::Result<Command> {
    let mut cmd = match execution {
        Some(execution) => execution.command()?,
        None => codex_command()?,
    };
    cmd.args(PROXY_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = codex_home {
        cmd.env("CODEX_HOME", home);
    }
    Ok(cmd)
}

#[cfg(all(test, unix))]
#[test]
fn a_proxy_uses_the_saved_launcher_and_interpreter_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("caller bin;literal");
    std::fs::create_dir(&bin).unwrap();
    let launcher = bin.join("codex");
    let interpreter = bin.join("tasty-probe-node");
    std::fs::write(&launcher, "#!/usr/bin/env tasty-probe-node\n").unwrap();
    std::fs::write(
        &interpreter,
        "#!/bin/sh\nprintf '%s\\n' \"$2\" \"$3\" \"$CODEX_HOME\"\n",
    )
    .unwrap();
    for p in [&launcher, &interpreter] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let execution = crate::execution::CodexExecution {
        executable: launcher.clone(),
        search_path: bin.to_str().unwrap().into(),
    };
    let home = dir.path().join("home with spaces");
    let out = proxy_command(Some(&home), Some(&execution))
        .unwrap()
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("app-server\nproxy\n{}\n", home.display())
    );
    std::fs::remove_file(launcher).unwrap();
    assert!(
        proxy_command(Some(&home), Some(&execution))
            .unwrap()
            .output()
            .is_err()
    );
}

static PROXY_POISON_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 오래된 호출자·기록은 현재 프로세스 환경에서 실행 파일을 찾는다.
pub(crate) fn codex_command() -> std::io::Result<Command> {
    Ok(tasty_utils::execution::ExecutionContext::capture("codex")?.command())
}

pub(crate) struct Client {
    ws: WsConn<Box<dyn Duplex>>,
    next_id: u64,
    _proxy: Option<ProxyGuard>,
    /// 소켓 읽기 기한을 없앤다. 기한 없이 계속 읽을 연결([`Client::hold`])에 쓴다.
    /// proxy 파이프는 원래 기한이 없어 None이다.
    clear_read_timeout: Option<Box<dyn Fn() -> std::io::Result<()> + Send>>,
}

impl Client {
    /// 연결하고 initialize 교환까지 마친다. timeout은 연결 전체의 기한이다.
    pub(crate) fn connect(endpoint: &Endpoint, timeout: Duration) -> Result<Self, RpcError> {
        let not_sent = |e: std::io::Error| RpcError::NotSent(e.to_string());
        type ClearTimeout = Option<Box<dyn Fn() -> std::io::Result<()> + Send>>;
        let (stream, host, bearer, proxy, clear): (
            Box<dyn Duplex>,
            String,
            Option<String>,
            _,
            ClearTimeout,
        ) = match endpoint {
            #[cfg(unix)]
            Endpoint::Unix(path) => {
                let s = std::os::unix::net::UnixStream::connect(path).map_err(not_sent)?;
                s.set_read_timeout(Some(timeout)).map_err(not_sent)?;
                s.set_write_timeout(Some(timeout)).map_err(not_sent)?;
                let handle = s.try_clone().map_err(not_sent)?;
                let clear: ClearTimeout = Some(Box::new(move || handle.set_read_timeout(None)));
                (Box::new(s), "localhost".into(), None, None, clear)
            }
            #[cfg(not(unix))]
            Endpoint::Unix(_) => {
                return Err(RpcError::NotSent(
                    "unix socket endpoints are not available on this platform".into(),
                ));
            }
            Endpoint::Ws { port, token } => {
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], *port));
                let s = std::net::TcpStream::connect_timeout(&addr, timeout).map_err(not_sent)?;
                s.set_read_timeout(Some(timeout)).map_err(not_sent)?;
                s.set_write_timeout(Some(timeout)).map_err(not_sent)?;
                let handle = s.try_clone().map_err(not_sent)?;
                let clear: ClearTimeout = Some(Box::new(move || handle.set_read_timeout(None)));
                (
                    Box::new(s),
                    format!("127.0.0.1:{port}"),
                    Some(token.clone()),
                    None,
                    clear,
                )
            }
            Endpoint::Proxy {
                codex_home,
                execution,
            } => {
                let command =
                    proxy_command(codex_home.as_deref(), execution.as_ref()).map_err(not_sent)?;
                let program = command.get_program().to_string_lossy().into_owned();
                let (stream, guard) = start_proxy(command, timeout).map_err(|e| {
                    RpcError::NotSent(format!("Codex proxy executable {program}: {e}"))
                })?;
                (
                    Box::new(stream),
                    "localhost".into(),
                    None,
                    Some(guard),
                    None,
                )
            }
        };
        let mut client = Self::over(stream, &host, bearer.as_deref(), proxy)?;
        client.clear_read_timeout = clear;
        Ok(client)
    }

    /// 열린 전송로에서 handshake와 initialize 교환을 마친다.
    fn over(
        stream: Box<dyn Duplex>,
        host: &str,
        bearer: Option<&str>,
        proxy: Option<ProxyGuard>,
    ) -> Result<Self, RpcError> {
        let not_sent = |e: std::io::Error| RpcError::NotSent(e.to_string());
        let ws = WsConn::handshake(stream, host, bearer).map_err(not_sent)?;
        let mut client = Self {
            ws,
            next_id: 1,
            _proxy: proxy,
            clear_read_timeout: None,
        };
        client
            .call(
                "initialize",
                json!({
                    "clientInfo": { "name": "tasty", "version": crate::PLUGIN_VERSION },
                    "capabilities": { "experimentalApi": true },
                }),
            )
            .map_err(|e| RpcError::NotSent(format!("initialize: {e}")))?;
        client
            .notify("initialized")
            .map_err(|e| RpcError::NotSent(format!("initialized: {e}")))?;
        Ok(client)
    }

    fn notify(&mut self, method: &str) -> std::io::Result<()> {
        self.ws
            .send_text(&json!({ "jsonrpc": "2.0", "method": method }).to_string())
    }

    /// 요청을 보내고 같은 id의 응답을 기다린다. 알림과 서버 요청은 건너뛴다.
    pub(crate) fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.ws
            .send_text(&request.to_string())
            .map_err(|e| RpcError::Ambiguous(e.to_string()))?;
        loop {
            let text = self
                .ws
                .recv_text()
                .map_err(|e| RpcError::Ambiguous(e.to_string()))?;
            let Ok(message) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if message.get("method").is_some() || message.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(RpcError::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// 연결을 열어 둔 채 서버가 보내는 메시지를 별도 스레드에서 읽어 버린다. 구독한 thread를
    /// 이 연결이 계속 붙잡게 한다. 서버 요청에는 응답하지 않는다. 같은 요청은 그 thread를 구독한
    /// 다른 연결(TUI)에도 가고 한 응답으로 해결된다. 서버가 연결을 닫거나 끝나면 스레드도 끝난다.
    pub(crate) fn hold(mut self) -> std::io::Result<std::thread::JoinHandle<()>> {
        if let Some(clear) = self.clear_read_timeout.take() {
            clear()?;
        }
        std::thread::Builder::new()
            .name("codex-rpc-hold".into())
            .spawn(move || {
                loop {
                    if let Err(e) = self.ws.recv_text() {
                        tracing::debug!("codex rpc hold ended: {e}");
                        return;
                    }
                }
            })
    }

    pub(crate) fn close(mut self) {
        if let Err(e) = self.ws.close() {
            tracing::debug!("codex rpc close: {e}");
        }
    }
}

/// 목록 응답의 thread ID. 문자열 또는 `{id}` 객체를 받는다.
fn listed_ids(result: &Value) -> impl Iterator<Item = &str> {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().or_else(|| v.get("id").and_then(Value::as_str)))
}

/// 그 thread가 서버에 로드돼 있는지 확인한다.
pub(crate) fn thread_loaded(client: &mut Client, thread: &str) -> Result<bool, RpcError> {
    let mut cursor: Option<String> = None;
    // 페이지가 끝나지 않는 서버에서도 멈추도록 횟수를 제한한다.
    for _ in 0..50 {
        let mut params = json!({});
        if let Some(c) = &cursor {
            params["cursor"] = json!(c);
        }
        let result = client.call("thread/loaded/list", params)?;
        if listed_ids(&result).any(|id| id.eq_ignore_ascii_case(thread)) {
            return Ok(true);
        }
        match result.get("nextCursor").and_then(Value::as_str) {
            Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
            _ => return Ok(false),
        }
    }
    Ok(false)
}

/// 대기열에 같은 clientUserMessageId 항목이 있는지 확인한다. 이미 턴으로 시작된 항목은
/// 대기열에서 빠지므로 없다는 결과가 미수락을 뜻하지는 않는다.
pub(crate) fn queue_contains(
    client: &mut Client,
    thread: &str,
    client_message_id: &str,
) -> Result<bool, RpcError> {
    let result = client.call("thread/queue/list", json!({ "threadId": thread }))?;
    Ok(result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|item| {
            item.get("clientUserMessageId").and_then(Value::as_str) == Some(client_message_id)
        }))
}

/// 텍스트 입력 하나를 thread 대기열에 넣는다. 성공하면 queuedSubmission ID를 돌려준다.
pub(crate) fn queue_add(
    client: &mut Client,
    thread: &str,
    text: &str,
    client_message_id: &str,
) -> Result<Option<String>, RpcError> {
    let result = client.call(
        "thread/queue/add",
        json!({
            "threadId": thread,
            "input": [{ "type": "text", "text": text, "text_elements": [] }],
            "clientUserMessageId": client_message_id,
        }),
    )?;
    Ok(result
        .get("queuedSubmission")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// 다른 모듈의 시험이 실제 proxy를 같은 방식으로 띄울 때 쓴다.
#[cfg(all(test, target_os = "linux"))]
pub(crate) mod tests_support {
    use super::*;

    pub(crate) fn proxy_args() -> [&'static str; 2] {
        PROXY_ARGS
    }

    /// `cmd`를 proxy로 띄워 연결한다. 실패하면 그 오류를 돌려준다.
    pub(crate) fn connect_proxy_command(mut cmd: Command, timeout: Duration) -> Option<RpcError> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (stream, guard) = match start_proxy(cmd, timeout) {
            Ok(started) => started,
            Err(e) => return Some(e),
        };
        Client::over(Box::new(stream), "localhost", None, Some(guard)).err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::test_server;

    #[test]
    fn calls_skip_notifications_and_report_server_errors() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let head = test_server::accept_handshake(&mut s);
            assert!(head.contains("Bearer t0k"), "{head}");
            let init: Value =
                serde_json::from_str(&test_server::read_text(&mut s).unwrap()).unwrap();
            assert_eq!(init["method"], "initialize");
            test_server::write_text(&mut s, &json!({"id": init["id"], "result": {}}).to_string());
            let initialized: Value =
                serde_json::from_str(&test_server::read_text(&mut s).unwrap()).unwrap();
            assert_eq!(initialized["method"], "initialized");
            let req: Value =
                serde_json::from_str(&test_server::read_text(&mut s).unwrap()).unwrap();
            test_server::write_text(
                &mut s,
                &json!({"method": "thread/status/changed", "params": {}}).to_string(),
            );
            test_server::write_text(
                &mut s,
                &json!({"id": req["id"], "error": {"code": -32600, "message": "bad"}}).to_string(),
            );
        });
        let mut client = Client::connect(
            &Endpoint::Ws {
                port,
                token: "t0k".into(),
            },
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            client.call("thread/queue/add", json!({})),
            Err(RpcError::Server {
                code: -32600,
                message: "bad".into()
            })
        );
        server.join().unwrap();
    }

    /// npm `codex`처럼 실제 proxy를 다시 띄우는 래퍼를 흉내 낸다. 하위 프로세스가 stdout을 쥐고
    /// 응답하지 않아도 기한에 연결이 끝나고 그 하위 프로세스도 남지 않는다.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_proxy_deadline_ends_a_wrapper_and_its_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("inner.pid");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 20 & echo $! > \"$1\"; wait", "wrapper"])
            .arg(&pid_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let started = Instant::now();
        let (stream, guard) = start_proxy(cmd, Duration::from_secs(1)).unwrap();
        let err = Client::over(Box::new(stream), "localhost", None, Some(guard))
            .err()
            .unwrap();
        let elapsed = started.elapsed();
        assert!(matches!(err, RpcError::NotSent(_)), "{err:?}");
        assert!(
            elapsed < Duration::from_secs(4),
            "1초 기한의 연결이 {elapsed:?} 걸렸다"
        );
        let inner = std::fs::read_to_string(&pid_file).unwrap();
        let proc_dir = PathBuf::from(format!("/proc/{}", inner.trim()));
        let deadline = Instant::now() + Duration::from_secs(2);
        while proc_dir.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !proc_dir.exists(),
            "래퍼의 하위 프로세스 {} 가 남았다",
            inner.trim()
        );
    }

    /// 래퍼가 먼저 끝나 회수됐어도 첫 정리는 남은 하위 프로세스를 끝낸다. 두 번째 정리는 회수한
    /// PID로 그룹 신호를 보내지 않는다.
    #[cfg(target_os = "linux")]
    #[test]
    fn proxy_cleanup_runs_once_and_still_ends_an_exited_wrappers_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("inner.pid");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 20 & echo $! > \"$1\"", "wrapper"])
            .arg(&pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut proxy = OwnedProxy::spawn(cmd).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while proxy.child.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "래퍼가 끝나지 않았다");
            std::thread::sleep(Duration::from_millis(10));
        }
        let inner = std::fs::read_to_string(&pid_file).unwrap();
        let proc_dir = PathBuf::from(format!("/proc/{}", inner.trim()));
        assert!(proc_dir.exists(), "하위 프로세스가 먼저 끝났다");
        assert!(proxy.stop(), "첫 정리는 회수된 래퍼여도 그룹을 정리한다");
        let deadline = Instant::now() + Duration::from_secs(2);
        while proc_dir.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !proc_dir.exists(),
            "하위 프로세스 {} 가 남았다",
            inner.trim()
        );
        assert!(!proxy.stop(), "두 번째 정리는 신호를 보내지 않는다");
    }

    #[test]
    fn a_refused_connection_is_not_sent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let err = Client::connect(
            &Endpoint::Ws {
                port,
                token: "x".into(),
            },
            Duration::from_millis(500),
        )
        .err()
        .unwrap();
        assert!(matches!(err, RpcError::NotSent(_)), "{err:?}");
    }
}
