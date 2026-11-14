//! tiny_http 기반 SSE 서버. accept 스레드와 연결별 스레드를 사용한다(docs/features/webhook/index.md).
//! 연결 수가 적다는 전제이며, 규모가 커지면 docs/plugins/agent-stream/index.md#sse-엔드포인트를 재검토한다.
//! 연결별 스레드는 시작할 때 재전송 버퍼를 읽고 이후에는 자기 구독 큐를 사용한다.

#[cfg(test)]
pub(crate) mod test_support;

use std::collections::BTreeMap;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use tiny_http::{Method, Request, Response, Server};

use crate::registry::{Replay, StreamRegistry};
use crate::sse::hub::{SseHub, SubOptions, Subscription};
use crate::sse::{ServeConfig, frame, request};

/// accept 루프가 종료 신호를 확인하는 주기.
const ACCEPT_POLL: Duration = Duration::from_millis(250);

/// 연결 스레드가 종료 신호를 확인하는 주기.
const STREAM_POLL: Duration = Duration::from_millis(250);

/// 이 시간 동안 보낼 것이 없으면 keep-alive 주석을 흘린다.
const KEEP_ALIVE_EVERY: Duration = Duration::from_secs(15);

/// 종료 시 연결 스레드가 빠지길 기다리는 상한.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// 소비자가 끊겼을 때 재접속까지 기다릴 시간(ms) 힌트. 끊김이 정상 경로라 명시한다.
const RETRY_HINT_MS: u64 = 3000;

/// 연결 핸들 목록의 poison을 처음 한 번 보고한다.
const CONNECTIONS_WHAT: &str = "the SSE connection-handle list";
static CONNECTIONS_POISON_REPORTED: AtomicBool = AtomicBool::new(false);

/// 재전송 레지스트리의 poison을 처음 한 번 보고한다.
const STREAM_REGISTRY_WHAT: &str = "the SSE stream registry";
static STREAM_REGISTRY_POISON_REPORTED: AtomicBool = AtomicBool::new(false);

/// Content-Length 없이 이어 보내는 SSE 응답 헤더.
const STREAM_PREAMBLE: &str = concat!(
    "HTTP/1.1 200 OK\r\n",
    "Content-Type: text/event-stream\r\n",
    "Cache-Control: no-cache\r\n",
    "Connection: close\r\n",
    // nginx 류 중간 프록시의 응답 버퍼링을 끈다 — 버퍼링되면 이벤트가 뭉쳐서 도착한다.
    "X-Accel-Buffering: no\r\n",
    "\r\n",
);

type Shared = Arc<Mutex<StreamRegistry>>;

/// 연결 스레드가 공유하는 것들.
struct ConnCtx {
    hub: Arc<SseHub>,
    registry: Shared,
    token: Option<String>,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
}

/// 살아 있는 SSE 서버 한 대.
pub struct SseServer {
    config: ServeConfig,
    /// 실제로 bind 된 주소. 설정과 같지만 커널이 확정한 값을 노출한다.
    bound: Option<SocketAddr>,
    hub: Arc<SseHub>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    active: Arc<AtomicUsize>,
}

impl std::fmt::Debug for SseServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseServer")
            .field("config", &self.config)
            .field("bound", &self.bound)
            .finish_non_exhaustive()
    }
}

/// 설정대로 bind 하고 accept 스레드를 띄운다. bind 실패는 **폴백 없이** 그대로 올린다.
pub fn start(config: ServeConfig, hub: Arc<SseHub>, registry: Shared) -> Result<SseServer, String> {
    let addr = format!("{}:{}", config.bind, config.port);
    let server = Server::http(addr.as_str()).map_err(|e| e.to_string())?;
    start_bound(config, server, hub, registry)
}

fn start_bound(
    config: ServeConfig,
    server: Server,
    hub: Arc<SseHub>,
    registry: Shared,
) -> Result<SseServer, String> {
    let bound = server.server_addr().to_ip();
    let stop = Arc::new(AtomicBool::new(false));
    let connections: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let ctx = Arc::new(ConnCtx {
        hub: hub.clone(),
        registry,
        token: config.token.clone(),
        stop: stop.clone(),
        active: active.clone(),
    });
    let accept = std::thread::Builder::new()
        .name("agent-stream-sse".into())
        .spawn({
            let connections = connections.clone();
            move || accept_loop(server, ctx, connections)
        })
        .map_err(|e| e.to_string())?;
    tracing::info!(
        "agent-stream: SSE endpoint listening on http://{}:{}/events",
        config.bind,
        config.port
    );
    Ok(SseServer {
        config,
        bound,
        hub,
        stop,
        accept: Some(accept),
        connections,
        active,
    })
}

impl SseServer {
    /// 서버 상태와 구독 통계를 조회한다. 토큰은 제외한다.
    /// accept 루프가 오류로 끝난 경우도 반영하도록 running은 stop 플래그로 판단한다.
    pub fn to_json(&self) -> Value {
        let (subs, total_dropped) = self.hub.stats();
        let host = match self.bound {
            Some(addr) => addr.to_string(),
            None => format!("{}:{}", self.config.bind, self.config.port),
        };
        let mut info = self.config.to_public_json();
        let map = info.as_object_mut().expect("to_public_json object");
        map.insert(
            "running".into(),
            Value::from(!self.stop.load(Ordering::SeqCst)),
        );
        map.insert("url".into(), Value::from(format!("http://{host}/events")));
        map.insert(
            "subscribers".into(),
            Value::from(subs.iter().map(|s| s.to_json()).collect::<Vec<_>>()),
        );
        map.insert("total_dropped".into(), Value::from(total_dropped));
        info
    }

    /// 구독과 accept 스레드를 닫고 연결 스레드는 제한 시간 동안 기다린다.
    /// 소켓 쓰기에서 멈춘 연결 때문에 IPC dispatch까지 무한히 기다리지 않게 한다.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // 송신단을 닫아 대기 중인 수신단이 종료를 확인할 수 있게 한다.
        self.hub.close_all();
        if let Some(handle) = self.accept.take()
            && handle.join().is_err()
        {
            tracing::warn!("agent-stream: the SSE accept thread panicked");
        }
        self.join_connections();
    }

    fn join_connections(&self) {
        self.wait_for_idle_connections();
        let stuck = join_finished(self.take_connection_handles());
        if stuck > 0 {
            tracing::warn!(
                "agent-stream: {stuck} SSE connection thread(s) did not finish within {}s — detached (their sockets close when the process exits)",
                SHUTDOWN_GRACE.as_secs()
            );
        }
    }

    /// 연결 스레드가 전부 빠질 때까지 기다린다(상한 [`SHUTDOWN_GRACE`]).
    fn wait_for_idle_connections(&self) {
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while self.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn take_connection_handles(&self) -> Vec<JoinHandle<()>> {
        let mut list = tasty_utils::poison::recover_mutex(
            self.connections.lock(),
            CONNECTIONS_WHAT,
            &CONNECTIONS_POISON_REPORTED,
        );
        list.drain(..).collect()
    }
}

/// 끝난 스레드만 join하고 아직 실행 중인 스레드는 분리한다. 분리한 수를 반환한다.
fn join_finished(handles: Vec<JoinHandle<()>>) -> usize {
    let mut stuck = 0usize;
    for handle in handles {
        if handle.is_finished() {
            join_one(handle);
        } else {
            stuck += 1;
        }
    }
    stuck
}

fn join_one(handle: JoinHandle<()>) {
    if handle.join().is_err() {
        tracing::warn!("agent-stream: an SSE connection thread panicked");
    }
}

impl Drop for SseServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(server: Server, ctx: Arc<ConnCtx>, connections: Arc<Mutex<Vec<JoinHandle<()>>>>) {
    while !ctx.stop.load(Ordering::SeqCst) {
        match server.recv_timeout(ACCEPT_POLL) {
            Ok(Some(req)) => spawn_connection(req, &ctx, &connections),
            // 타임아웃 — stop 플래그를 다시 본다.
            Ok(None) => {}
            Err(e) => {
                // 조회 결과와 연결 스레드에도 종료를 알린다.
                ctx.stop.store(true, Ordering::SeqCst);
                tracing::warn!("agent-stream: SSE accept failed: {e} — the listener stops");
                return;
            }
        }
        reap_finished(&connections);
    }
}

fn spawn_connection(
    req: Request,
    ctx: &Arc<ConnCtx>,
    connections: &Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    ctx.active.fetch_add(1, Ordering::SeqCst);
    let active = ctx.active.clone();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("agent-stream-sse-conn".into())
        .spawn(move || {
            handle_request(req, &ctx);
            ctx.active.fetch_sub(1, Ordering::SeqCst);
        });
    match spawned {
        Ok(handle) => tasty_utils::poison::recover_mutex(
            connections.lock(),
            CONNECTIONS_WHAT,
            &CONNECTIONS_POISON_REPORTED,
        )
        .push(handle),
        Err(e) => {
            active.fetch_sub(1, Ordering::SeqCst);
            tracing::warn!("agent-stream: cannot spawn an SSE connection thread: {e}");
        }
    }
}

/// 끝난 연결 스레드의 핸들을 정리한다.
fn reap_finished(connections: &Arc<Mutex<Vec<JoinHandle<()>>>>) {
    let mut list = tasty_utils::poison::recover_mutex(
        connections.lock(),
        CONNECTIONS_WHAT,
        &CONNECTIONS_POISON_REPORTED,
    );
    let mut kept = Vec::with_capacity(list.len());
    for handle in list.drain(..) {
        if handle.is_finished() {
            join_one(handle);
        } else {
            kept.push(handle);
        }
    }
    *list = kept;
}

fn handle_request(req: Request, ctx: &ConnCtx) {
    let url = req.url().to_string();
    let headers = collect_headers(&req);
    let query = request::parse_query(&url);
    if request::path_of(&url) != request::STREAM_PATH {
        respond_empty(req, 404);
        return;
    }
    if req.method() != &Method::Get {
        respond_empty(req, 405);
        return;
    }
    if !request::authorize(ctx.token.as_deref(), &headers, &query) {
        // 거부 응답은 **빈 바디**다 — 어떤 내부 상태도 싣지 않는다.
        tracing::warn!("agent-stream: rejected an unauthenticated SSE subscription");
        respond_empty(req, 401);
        return;
    }
    stream(req, ctx, &headers, &query);
}

fn collect_headers(req: &Request) -> BTreeMap<String, String> {
    req.headers()
        .iter()
        .map(|h| {
            (
                h.field.as_str().as_str().to_ascii_lowercase(),
                h.value.as_str().to_string(),
            )
        })
        .collect()
}

fn respond_empty(req: Request, status: u16) {
    if let Err(e) = req.respond(Response::empty(status)) {
        tracing::debug!("agent-stream: cannot send the {status} response: {e}");
    }
}

fn stream(
    req: Request,
    ctx: &ConnCtx,
    headers: &BTreeMap<String, String>,
    query: &BTreeMap<String, String>,
) {
    let opts = request::sub_options(query);
    let resume = request::resume_from(headers, query);
    // 구독을 먼저 등록해 재전송을 읽는 사이의 이벤트를 받는다. 겹치는 부분은 seq로 제외한다.
    let sub = ctx.hub.subscribe(opts);
    let replay = collect_replay(&ctx.registry, resume, opts);

    let mut writer = req.into_writer();
    if !write_str(&mut writer, STREAM_PREAMBLE) {
        return;
    }
    if !write_str(&mut writer, &format!("retry: {RETRY_HINT_MS}\n\n")) {
        return;
    }
    let mut last_seq = 0u64;
    if let Some((from, to)) = replay.gap {
        // 누락 구간을 먼저 알리되 gap 알림 자체로 소비자의 커서를 전진시키지 않는다.
        let payload = format!(r#"{{"kind":"gap","from":{from},"to":{to}}}"#);
        if !write_str(
            &mut writer,
            &frame::encode(from.saturating_sub(1), "gap", &payload),
        ) {
            return;
        }
    }
    for event in replay.events {
        if !write_str(&mut writer, &event.frame) {
            return;
        }
        last_seq = event.seq;
    }
    pump_stream(&mut writer, &sub, ctx, last_seq);
}

/// 재개 커서가 있으면 수집 버퍼에서 그 뒤의 이벤트를 꺼낸다. 없으면 재전송하지 않는다.
fn collect_replay(registry: &Shared, resume: Option<u64>, opts: SubOptions) -> Replay {
    let Some(after_seq) = resume else {
        return Replay::default();
    };
    let reg = tasty_utils::poison::recover_mutex(
        registry.lock(),
        STREAM_REGISTRY_WHAT,
        &STREAM_REGISTRY_POISON_REPORTED,
    );
    reg.replay_after(after_seq, opts)
}

fn pump_stream(
    writer: &mut Box<dyn Write + Send + 'static>,
    sub: &Subscription,
    ctx: &ConnCtx,
    mut last_seq: u64,
) {
    let mut idle_since = Instant::now();
    while !ctx.stop.load(Ordering::SeqCst) {
        match sub.rx.recv_timeout(STREAM_POLL) {
            Ok(event) => {
                // replay 와 겹친 구간은 건너뛴다 — seq 가 단조 증가라 이 비교로 충분하다.
                if event.seq <= last_seq {
                    continue;
                }
                if !write_str(writer, &event.frame) {
                    return;
                }
                last_seq = event.seq;
                idle_since = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                if idle_since.elapsed() < KEEP_ALIVE_EVERY {
                    continue;
                }
                if !write_str(writer, frame::KEEP_ALIVE) {
                    return;
                }
                idle_since = Instant::now();
            }
            // 종료 또는 연속 누락 상한 도달로 송신단이 닫혔다.
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// 소켓에 쓰고 flush한다. 실패하면 로그를 남기고 해당 연결을 끝낸다.
fn write_str(writer: &mut Box<dyn Write + Send + 'static>, text: &str) -> bool {
    if let Err(e) = writer.write_all(text.as_bytes()) {
        tracing::debug!("agent-stream: SSE subscriber went away while writing: {e}");
        return false;
    }
    if let Err(e) = writer.flush() {
        tracing::debug!("agent-stream: SSE subscriber went away while flushing: {e}");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_preamble_declares_an_unbuffered_event_stream_without_a_content_length() {
        assert!(STREAM_PREAMBLE.contains("Content-Type: text/event-stream"));
        assert!(STREAM_PREAMBLE.contains("Cache-Control: no-cache"));
        assert!(STREAM_PREAMBLE.contains("X-Accel-Buffering: no"));
        assert!(
            !STREAM_PREAMBLE
                .to_ascii_lowercase()
                .contains("content-length")
        );
        assert!(STREAM_PREAMBLE.ends_with("\r\n\r\n"));
    }

    #[test]
    fn an_occupied_address_fails_loudly_instead_of_falling_back_to_another_port() {
        let registry = Arc::new(Mutex::new(StreamRegistry::new(None)));
        let hub = Arc::new(SseHub::default());
        // The competing listener remains owned throughout the production bind attempt.
        let occupied = test_support::ReservedEndpoint::new();
        let config = ServeConfig {
            bind: "127.0.0.1".into(),
            port: occupied.port(),
            token: Some("t".into()),
        };
        let err = start(config, hub, registry).expect_err("bind must fail");
        assert!(!err.is_empty());
    }

    #[test]
    fn a_started_server_reports_its_url_without_leaking_the_token() {
        let registry = Arc::new(Mutex::new(StreamRegistry::new(None)));
        let hub = Arc::new(SseHub::default());
        let config = ServeConfig {
            bind: "127.0.0.1".into(),
            // 0 은 `validate()` 가 막지만, 테스트는 커널 할당 포트로 bind 자체를 검증한다.
            port: 0,
            token: Some("super-secret".into()),
        };
        let mut server = start(config, hub, registry).expect("bind");
        let info = server.to_json();
        assert_eq!(info["running"], json!(true));
        assert_eq!(info["auth"], json!(true));
        assert!(!info.to_string().contains("super-secret"));
        assert!(
            info["url"].as_str().expect("url").ends_with("/events"),
            "{info}"
        );
        server.shutdown();
    }

    #[test]
    fn a_stopped_listener_no_longer_reports_itself_as_running() {
        let (mut server, _port, _registry) = serve_on_ephemeral_port(None);
        assert_eq!(server.to_json()["running"], json!(true));

        // 서버 값을 보관하고 있어도 종료 뒤에는 running=false여야 한다.
        server.shutdown();

        let info = server.to_json();
        assert_eq!(info["running"], json!(false), "{info}");
    }

    // 실제 소켓에서 헤더·인증·프레임 전달을 검사한다.

    use std::io::Read;
    use std::net::TcpStream;

    use crate::record::{EventKind, StreamEvent};

    fn serve_on_ephemeral_port(token: Option<&str>) -> (SseServer, u16, Shared) {
        let registry = Arc::new(Mutex::new(StreamRegistry::new(None)));
        let hub = registry.lock().expect("lock").hub();
        let config = ServeConfig {
            bind: "127.0.0.1".into(),
            port: 0,
            token: token.map(str::to_string),
        };
        let server = start(config, hub, registry.clone()).expect("bind");
        let port = server.bound.expect("ip addr").port();
        (server, port, registry)
    }

    fn get(port: u16, target: &str) -> TcpStream {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let mut writer = &stream;
        write!(writer, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").expect("request");
        writer.flush().expect("flush");
        stream
    }

    /// `needle` 이 보일 때까지 읽는다. 타임아웃/EOF 면 지금까지 읽은 것을 돌려준다.
    fn read_until(stream: &mut TcpStream, needle: &str) -> String {
        let mut seen = String::new();
        let mut buf = [0u8; 1024];
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if seen.contains(needle) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        seen
    }

    fn push(registry: &Shared, seq_text: &str) {
        let mut event = StreamEvent::turn_end("placeholder");
        event.kind = EventKind::Text;
        event.reason = None;
        event.text = Some(seq_text.to_string());
        registry.lock().expect("lock").push_event(7, "sess", event);
    }

    #[test]
    fn a_subscriber_gets_an_event_stream_response_and_then_live_frames() {
        let (mut server, port, registry) = serve_on_ephemeral_port(None);
        let mut stream = get(port, "/events");
        let head = read_until(&mut stream, "retry:");
        assert!(head.contains("200 OK"), "{head}");
        assert!(head.contains("text/event-stream"), "{head}");

        push(&registry, "hello-from-the-agent");
        let body = read_until(&mut stream, "hello-from-the-agent");
        assert!(body.contains("event: text"), "{body}");
        assert!(body.contains("data: "), "{body}");
        assert!(
            body.contains("\n\n"),
            "frames must end with a blank line: {body}"
        );
        server.shutdown();
    }

    #[test]
    fn thinking_is_only_streamed_when_the_subscription_asks_for_it() {
        let (mut server, port, registry) = serve_on_ephemeral_port(None);
        let mut plain = get(port, "/events");
        let mut full = get(port, "/events?thinking=1");
        read_until(&mut plain, "retry:");
        read_until(&mut full, "retry:");

        let mut thought = StreamEvent::turn_end("placeholder");
        thought.kind = EventKind::Thinking;
        thought.reason = None;
        thought.text = Some("private-reasoning".into());
        registry.lock().expect("lock").push_event(7, "s", thought);
        push(&registry, "public-answer");

        let opted_in = read_until(&mut full, "public-answer");
        assert!(opted_in.contains("private-reasoning"), "{opted_in}");
        let default_view = read_until(&mut plain, "public-answer");
        assert!(
            !default_view.contains("private-reasoning"),
            "{default_view}"
        );
        server.shutdown();
    }

    #[test]
    fn last_event_id_replays_from_the_collection_buffer_without_duplicating_live_frames() {
        let (mut server, port, registry) = serve_on_ephemeral_port(None);
        push(&registry, "before-one");
        push(&registry, "before-two");

        let mut stream = get(port, "/events");
        // seq 1 뒤부터 — 두 번째 이벤트만 재전송된다.
        let mut resumed = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        resumed
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        write!(
            resumed,
            "GET /events HTTP/1.1\r\nHost: localhost\r\nLast-Event-ID: 1\r\n\r\n"
        )
        .expect("request");
        let replayed = read_until(&mut resumed, "before-two");
        assert!(replayed.contains("id: 2"), "{replayed}");
        assert!(!replayed.contains("before-one"), "{replayed}");

        // 재개하지 않은 구독자에게는 과거 이벤트가 가지 않는다.
        let live_only = read_until(&mut stream, "retry:");
        assert!(!live_only.contains("before-two"), "{live_only}");

        push(&registry, "after-resume");
        let live = read_until(&mut resumed, "after-resume");
        // replay 로 이미 보낸 seq 2 가 라이브로 한 번 더 나오지 않는다.
        assert_eq!(live.matches("before-two").count(), 0, "{live}");
        server.shutdown();
    }

    #[test]
    fn a_cursor_that_fell_out_of_the_buffer_gets_a_gap_frame_before_the_replay() {
        let (mut server, port, registry) = serve_on_ephemeral_port(None);
        for i in 0..(crate::registry::EVENT_BUFFER_CAP + 5) {
            push(&registry, &format!("e{i}"));
        }

        // surface 필터로 재전송 본문은 비우고 갭 통지만 본다 — 갭 판정은 필터와 무관하다.
        let mut resumed = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        resumed
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        write!(
            resumed,
            "GET /events?surface=999 HTTP/1.1\r\nHost: localhost\r\nLast-Event-ID: 0\r\n\r\n"
        )
        .expect("request");
        let seen = read_until(&mut resumed, "\"to\":5");
        assert!(seen.contains("event: gap"), "{seen}");
        assert!(seen.contains("\"from\":1"), "{seen}");
        // 커서를 전진시키지 않는다 — 갭 프레임의 id 는 소비자가 보낸 커서 그대로다.
        assert!(seen.contains("id: 0\nevent: gap"), "{seen}");
        server.shutdown();
    }

    #[test]
    fn a_missing_or_wrong_token_is_rejected_with_an_empty_body() {
        let (mut server, port, _registry) = serve_on_ephemeral_port(Some("s3cret"));
        for target in ["/events", "/events?token=wrong"] {
            let mut stream = get(port, target);
            let response = read_until(&mut stream, "\r\n\r\n");
            assert!(response.contains("401"), "{target}: {response}");
            // 거부 응답에 내부 데이터가 실리지 않는다.
            assert!(!response.contains("data:"), "{target}: {response}");
            assert!(!response.contains("s3cret"), "{target}: {response}");
        }
        let mut ok = get(port, "/events?token=s3cret");
        assert!(read_until(&mut ok, "retry:").contains("200 OK"));
        server.shutdown();
    }

    #[test]
    fn any_other_path_is_a_plain_404() {
        let (mut server, port, _registry) = serve_on_ephemeral_port(None);
        let mut stream = get(port, "/");
        let response = read_until(&mut stream, "\r\n\r\n");
        assert!(response.contains("404"), "{response}");
        server.shutdown();
    }
}
