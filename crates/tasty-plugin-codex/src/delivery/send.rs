//! 완료 참조의 실제 송신. 플러그인 worker를 막지 않도록 별도 스레드에서 실행하고, 결과는
//! self_invoke로 worker에 돌려준다.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::rpc::{self, Client, Endpoint, RpcError};

/// 연결과 요청 하나의 기한.
pub(crate) const RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    /// 대기열에 넣는다.
    Send,
    /// unknown 이벤트가 대기열에 있는지 한 번 확인한다. 다시 보내지 않는다.
    Verify,
}

#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub kind: JobKind,
    pub event_id: String,
    pub thread: String,
    pub endpoint: Endpoint,
    /// 외부 부모는 보내기 전에 로드 여부를 확인한다.
    pub check_loaded: bool,
    pub text: String,
}

impl Job {
    /// 같은 이벤트의 송신과 확인이 같은 값을 쓴다. 서버는 이 값으로 중복을 막지 않으므로
    /// 대기열 조회의 대조에만 쓴다.
    pub(crate) fn client_message_id(&self) -> String {
        format!("tasty-{}", self.event_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Accepted {
        submission_id: Option<String>,
        /// queue_add 응답 또는 queue_list 확인.
        confirmed_by: String,
    },
    Rejected {
        code: i64,
        message: String,
    },
    NotLoaded,
    /// 아무것도 보내지 못했다.
    NotSent {
        detail: String,
    },
    /// 보낸 뒤 응답을 잃었다.
    Unknown {
        detail: String,
    },
    /// 확인했지만 대기열에 없었다(이미 턴으로 시작됐을 수도 있다).
    StillUnknown {
        detail: String,
    },
}

/// 고정 템플릿. 자식이 만든 문자열은 넣지 않는다.
pub(crate) fn reference_text(provider: &str, status: &str, event_id: &str) -> String {
    format!(
        "[tasty] A child agent reported a turn boundary. provider={provider} status={status} \
         event={event_id}. This is a reference, not the result. Read the result with: \
         tasty codex result {event_id}"
    )
}

fn verify(job: &Job) -> Outcome {
    let still = |detail: String| Outcome::StillUnknown { detail };
    let mut client = match Client::connect(&job.endpoint, RPC_TIMEOUT) {
        Ok(c) => c,
        Err(e) => return still(e.to_string()),
    };
    let found = rpc::queue_contains(&mut client, &job.thread, &job.client_message_id());
    client.close();
    match found {
        Ok(true) => Outcome::Accepted {
            submission_id: None,
            confirmed_by: "queue_list".into(),
        },
        Ok(false) => still("not in the thread queue".into()),
        Err(e) => still(e.to_string()),
    }
}

pub(crate) fn run(job: &Job) -> Outcome {
    if job.kind == JobKind::Verify {
        return verify(job);
    }
    let mut client = match Client::connect(&job.endpoint, RPC_TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            return Outcome::NotSent {
                detail: e.to_string(),
            };
        }
    };
    if job.check_loaded {
        match rpc::thread_loaded(&mut client, &job.thread) {
            Ok(true) => {}
            Ok(false) => {
                client.close();
                return Outcome::NotLoaded;
            }
            // 조회는 상태를 바꾸지 않으므로 응답을 잃어도 보내지 않은 것이다.
            Err(e) => {
                client.close();
                return Outcome::NotSent {
                    detail: e.to_string(),
                };
            }
        }
    }
    let added = rpc::queue_add(
        &mut client,
        &job.thread,
        &job.text,
        &job.client_message_id(),
    );
    client.close();
    match added {
        Ok(submission_id) => Outcome::Accepted {
            submission_id,
            confirmed_by: "queue_add".into(),
        },
        Err(RpcError::Server { code, message }) => Outcome::Rejected { code, message },
        Err(RpcError::NotSent(detail)) => Outcome::NotSent { detail },
        Err(RpcError::Ambiguous(detail)) => match verify(job) {
            accepted @ Outcome::Accepted { .. } => accepted,
            _ => Outcome::Unknown { detail },
        },
    }
}

/// 백그라운드 실행을 맡는 쪽. 제품은 스레드와 self_invoke, 시험은 수집기를 쓴다.
pub(crate) trait Dispatch {
    fn dispatch(&self, job: Job) -> Result<(), String>;
    /// delay 뒤 그 thread의 펌프를 다시 실행하게 한다.
    fn wake_after(&self, thread: &str, delay: Duration) -> Result<(), String>;
    /// 소유 서버에서 턴 상태를 확인한다.
    fn check_turn(&self, job: super::turns::TurnJob) -> Result<(), String>;
    /// `at_ms`(UNIX ms)에 턴 후보를 다시 확인하게 한다. 예약은 스케줄러 하나에 합쳐진다.
    fn wake_turns_at(&self, at_ms: u64) -> Result<(), String>;
    /// attach 원격 부모에게 갈 결과를 서버 host 에 맡기거나 그 상태를 묻는다(`attach.child_notice`).
    /// 호출하는 곳은 worker 이며 이 호출은 codex namespace 를 다시 부르지 않는다.
    fn child_notice(
        &self,
        surface: u32,
        key: &str,
        payload: serde_json::Value,
    ) -> Result<tasty_plugin_agent_common::remote_attach::NoticeStatus, String>;
}

/// 스레드에서 송신하고 결과를 worker로 돌려준다.
pub(crate) struct ThreadDispatch {
    pub host: tasty_plugin_sdk::HostHandle,
    /// 이 프로세스가 만든 결과만 받기 위한 값. IPC로 들어온 위조 결과를 거른다.
    pub token: String,
    /// 턴 후보 재확인 깨우기를 맡는 프로세스 하나의 스케줄러.
    pub turn_scheduler: std::sync::Arc<super::turn_scheduler::TurnScheduler>,
}

impl Dispatch for ThreadDispatch {
    fn dispatch(&self, job: Job) -> Result<(), String> {
        let host = self.host.clone();
        let token = self.token.clone();
        std::thread::Builder::new()
            .name(format!("codex-deliver-{}", &job.event_id[..8]))
            .spawn(move || {
                let outcome = run(&job);
                let params = serde_json::json!({
                    "event": job.event_id,
                    "token": token,
                    "kind": match job.kind { JobKind::Send => "send", JobKind::Verify => "verify" },
                    "result": outcome,
                });
                // 실패하면 상태가 sending으로 남고 다음 시작 때 unknown으로 바뀐다.
                if let Err(e) = host.self_invoke("codex.delivery_outcome", params) {
                    tracing::warn!("codex delivery {}: outcome lost: {e}", job.event_id);
                }
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn check_turn(&self, job: super::turns::TurnJob) -> Result<(), String> {
        super::turns::spawn_check(self.host.clone(), self.token.clone(), job)
    }

    fn child_notice(
        &self,
        surface: u32,
        key: &str,
        payload: serde_json::Value,
    ) -> Result<tasty_plugin_agent_common::remote_attach::NoticeStatus, String> {
        tasty_plugin_agent_common::remote_attach::child_notice(&self.host, surface, key, payload)
    }

    fn wake_turns_at(&self, at_ms: u64) -> Result<(), String> {
        let host = self.host.clone();
        let token = self.token.clone();
        self.turn_scheduler.arm(at_ms, move || {
            Box::new(move |seq| {
                let params = serde_json::json!({ "token": token, "seq": seq });
                if let Err(e) = host.self_invoke("codex.turn_wake", params) {
                    tracing::warn!("codex turn: retry wake {seq} lost: {e}");
                }
            })
        })
    }

    fn wake_after(&self, thread: &str, delay: Duration) -> Result<(), String> {
        let host = self.host.clone();
        let token = self.token.clone();
        let thread = thread.to_string();
        std::thread::Builder::new()
            .name("codex-deliver-retry".into())
            .spawn(move || {
                std::thread::sleep(delay);
                let params = serde_json::json!({ "thread": thread, "token": token });
                if let Err(e) = host.self_invoke("codex.delivery_wake", params) {
                    tracing::warn!("codex delivery: retry wake for {thread} lost: {e}");
                }
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! 시험용 app-server. 정해진 응답 규칙으로 요청에 답하고 받은 요청을 기록한다.
    //! 시험이 실패했을 때 원인을 가를 수 있도록 accept 결과와 연결마다의 종료 사유를
    //! 사건 기록에 남긴다. accept 오류가 나도 리스너는 끝내지 않는다.
    use std::io::{self, Read, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use crate::ws::test_server;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(crate) enum QueueAdd {
        Accept,
        Reject,
        /// 요청을 받은 뒤 응답 없이 연결을 끊는다.
        Drop,
    }

    #[derive(Clone)]
    pub(crate) struct Rules {
        pub loaded: Vec<String>,
        pub queue_add: QueueAdd,
        /// queue/list가 돌려줄 clientUserMessageId.
        pub queued: Vec<String>,
    }

    /// accept 오류로 남기는 줄의 상한. 넘으면 "accept errors truncated" 한 줄만 더한다.
    pub(crate) const ACCEPT_ERROR_LINES: usize = 16;

    /// 서버가 겪은 사건. 연결 순번은 accept된 순서로 1부터 센다.
    #[derive(Default)]
    pub(crate) struct Events {
        lines: Vec<String>,
        accepted: usize,
        ended: usize,
        /// 직전 accept 오류의 내용, 그 줄의 위치, 이어진 횟수.
        last_accept_error: Option<(String, usize, usize)>,
        accept_error_lines: usize,
        accept_errors_truncated: bool,
    }

    impl Events {
        fn push(&mut self, line: String) {
            self.lines.push(line);
        }

        pub(crate) fn lines(&self) -> &[String] {
            &self.lines
        }

        /// 연결을 받았다. 이어지던 accept 오류의 반복 세기를 끊는다.
        pub(crate) fn note_accepted(&mut self) -> usize {
            self.last_accept_error = None;
            self.accepted += 1;
            let conn = self.accepted;
            self.push(format!("conn {conn}: accepted"));
            conn
        }

        /// accept 오류를 남긴다. 직전과 같은 오류가 이어지면 그 줄의 횟수만 올리고,
        /// 오류 줄이 상한을 넘으면 더 쌓지 않는다. 오류가 계속돼도 기록은 자라지 않는다.
        pub(crate) fn note_accept_error(&mut self, error: String) {
            if let Some((last, at, count)) = &mut self.last_accept_error
                && *last == error
            {
                *count += 1;
                self.lines[*at] = format!("accept error: {error} (x{count})");
                return;
            }
            if self.accept_error_lines >= ACCEPT_ERROR_LINES {
                if !self.accept_errors_truncated {
                    self.accept_errors_truncated = true;
                    self.push("accept errors truncated".into());
                }
                self.last_accept_error = None;
                return;
            }
            self.accept_error_lines += 1;
            self.push(format!("accept error: {error}"));
            self.last_accept_error = Some((error, self.lines.len() - 1, 1));
        }
    }

    pub(crate) struct Server {
        pub port: u16,
        pub seen: Arc<Mutex<Vec<Value>>>,
        pub events: Arc<Mutex<Events>>,
    }

    fn describe(e: &io::Error) -> String {
        format!("kind={:?} os={:?} {e}", e.kind(), e.raw_os_error())
    }

    /// 마지막 읽기·쓰기 오류와 EOF를 기억하는 래퍼. test_server 함수는 오류를 버리므로
    /// 종료 사유는 여기서 읽는다.
    struct Tap<'a> {
        s: &'a mut TcpStream,
        err: Option<String>,
        eof: bool,
    }

    impl Read for Tap<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.s.read(buf) {
                Ok(0) if !buf.is_empty() => {
                    self.eof = true;
                    Ok(0)
                }
                // read_exact는 Interrupted를 그 자리에서 다시 읽으므로 종료 사유가 아니다.
                Err(e) if e.kind() != io::ErrorKind::Interrupted => {
                    self.err = Some(describe(&e));
                    Err(e)
                }
                other => other,
            }
        }
    }

    impl Write for Tap<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.s.write(buf).inspect_err(|e| {
                // write_all도 Interrupted는 다시 쓴다.
                if e.kind() != io::ErrorKind::Interrupted {
                    self.err = Some(describe(e));
                }
            })
        }
        fn flush(&mut self) -> io::Result<()> {
            self.s.flush()
        }
    }

    impl Tap<'_> {
        fn reason(&mut self, what: &str) -> String {
            match (self.err.take(), self.eof) {
                (Some(e), _) => format!("{what} error: {e}"),
                (None, true) => format!("{what}: peer closed (eof)"),
                (None, false) => format!("{what}: failed without io error"),
            }
        }
    }

    enum Next {
        Text(String),
        End(String),
    }

    /// 텍스트 프레임 하나를 읽는다. pong은 건너뛴다. 읽지 못하면 종료 사유를 돌려준다.
    fn next_text(tap: &mut Tap<'_>) -> Next {
        loop {
            let Some((opcode, data)) = test_server::read_frame(tap) else {
                return Next::End(tap.reason("read"));
            };
            match opcode {
                0xA => continue,
                0x8 => return Next::End("close frame".into()),
                _ => {
                    return match String::from_utf8(data) {
                        Ok(text) => Next::Text(text),
                        Err(e) => Next::End(format!("non-utf8 text: {e}")),
                    };
                }
            }
        }
    }

    fn send(tap: &mut Tap<'_>, value: &Value) -> Result<(), String> {
        let mut frame = Vec::new();
        test_server::write_text(&mut frame, &value.to_string());
        tap.write_all(&frame).map_err(|_| tap.reason("write"))
    }

    /// 연결 하나를 처리하고 종료 사유를 돌려준다.
    fn serve(
        s: &mut TcpStream,
        rules: &Rules,
        log: &Mutex<Vec<Value>>,
        note: &dyn Fn(String),
    ) -> String {
        let mut tap = Tap {
            s,
            err: None,
            eof: false,
        };
        let shook = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            test_server::accept_handshake(&mut tap)
        }));
        if shook.is_err() {
            return tap.reason("handshake");
        }
        note("handshake ok".into());
        loop {
            let text = match next_text(&mut tap) {
                Next::Text(text) => text,
                Next::End(reason) => return reason,
            };
            let req: Value = serde_json::from_str(&text).unwrap();
            log.lock().unwrap().push(req.clone());
            let method = req["method"].as_str().unwrap_or_default().to_string();
            note(format!("recv {method}"));
            let Some(id) = req.get("id").cloned() else {
                continue;
            };
            let reply = match method.as_str() {
                "thread/loaded/list" => {
                    json!({"id": id, "result": {"data": rules.loaded, "nextCursor": null}})
                }
                "thread/queue/list" => json!({"id": id, "result": {"data": rules.queued.iter()
                    .map(|c| json!({"id":"q","clientUserMessageId":c})).collect::<Vec<_>>()}}),
                "thread/queue/add" => match rules.queue_add {
                    QueueAdd::Accept => {
                        json!({"id": id, "result": {"queuedSubmission": {"id": "sub-1"}}})
                    }
                    QueueAdd::Reject => json!({"id": id,
                        "error": {"code": -32600, "message": "thread not found"}}),
                    QueueAdd::Drop => return "dropped after thread/queue/add (intended)".into(),
                },
                _ => json!({"id": id, "result": {}}),
            };
            if let Err(reason) = send(&mut tap, &reply) {
                return reason;
            }
            note(format!("sent reply to {method}"));
        }
    }

    pub(crate) fn start(rules: Rules) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Events::default()));
        let log = seen.clone();
        let ev = events.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut s = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        // 오류 하나로 리스너를 끝내지 않는다. 기록하고 다음 연결을 받는다.
                        ev.lock().unwrap().note_accept_error(describe(&e));
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                };
                let conn = ev.lock().unwrap().note_accepted();
                let rules = rules.clone();
                let log = log.clone();
                let ev = ev.clone();
                std::thread::spawn(move || {
                    let note =
                        |line: String| ev.lock().unwrap().push(format!("conn {conn}: {line}"));
                    let reason = serve(&mut s, &rules, &log, &note);
                    let mut ev = ev.lock().unwrap();
                    ev.push(format!("conn {conn}: end: {reason}"));
                    ev.ended += 1;
                });
            }
        });
        Server { port, seen, events }
    }

    impl Server {
        pub(crate) fn methods(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter_map(|v| v["method"].as_str().map(str::to_string))
                .collect()
        }

        /// 실패 메시지용 사건 기록과 받은 메서드 목록. 연결 스레드가 종료 사유를 남길
        /// 때까지 잠깐 기다린다. 판정에는 쓰지 않는다.
        pub(crate) fn report(&self) -> String {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let ev = self.events.lock().unwrap();
                if ev.ended >= ev.accepted || Instant::now() >= deadline {
                    let mut out = format!(
                        "server events (accepted {}, ended {}):",
                        ev.accepted, ev.ended
                    );
                    for line in &ev.lines {
                        out.push_str("\n  ");
                        out.push_str(line);
                    }
                    out.push_str(&format!("\nmethods: {:?}", self.methods()));
                    return out;
                }
                drop(ev);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{QueueAdd, Rules, start};
    use super::*;

    const THREAD: &str = "01a12f74-dab9-7a00-8000-0123456789ab";

    fn job(port: u16, kind: JobKind, check_loaded: bool) -> Job {
        Job {
            kind,
            event_id: "0123456789abcdef0123456789abcdef".into(),
            thread: THREAD.into(),
            endpoint: Endpoint::Ws {
                port,
                token: "t".into(),
            },
            check_loaded,
            text: reference_text("claude", "turn_ended", "0123456789abcdef0123456789abcdef"),
        }
    }

    fn rules(queue_add: QueueAdd) -> Rules {
        Rules {
            loaded: vec![THREAD.into()],
            queue_add,
            queued: vec![],
        }
    }

    #[test]
    fn an_accepted_reference_carries_only_the_fixed_template() {
        let server = start(rules(QueueAdd::Accept));
        let out = run(&job(server.port, JobKind::Send, true));
        assert_eq!(
            out,
            Outcome::Accepted {
                submission_id: Some("sub-1".into()),
                confirmed_by: "queue_add".into()
            }
        );
        let seen = server.seen.lock().unwrap();
        let add = seen
            .iter()
            .find(|v| v["method"] == "thread/queue/add")
            .unwrap();
        assert_eq!(add["params"]["threadId"], THREAD);
        assert_eq!(
            add["params"]["clientUserMessageId"],
            "tasty-0123456789abcdef0123456789abcdef"
        );
        let text = add["params"]["input"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("[tasty] ") && text.contains("tasty codex result 0123"));
    }

    #[test]
    fn a_rejection_keeps_the_server_code_and_message() {
        let server = start(rules(QueueAdd::Reject));
        assert_eq!(
            run(&job(server.port, JobKind::Send, false)),
            Outcome::Rejected {
                code: -32600,
                message: "thread not found".into()
            }
        );
    }

    #[test]
    fn an_unloaded_external_thread_is_not_queued() {
        let mut r = rules(QueueAdd::Accept);
        r.loaded = vec!["other".into()];
        let server = start(r);
        assert_eq!(
            run(&job(server.port, JobKind::Send, true)),
            Outcome::NotLoaded
        );
        assert!(!server.methods().contains(&"thread/queue/add".to_string()));
    }

    #[test]
    fn a_lost_response_is_unknown_unless_the_queue_shows_it() {
        let server = start(rules(QueueAdd::Drop));
        let out = run(&job(server.port, JobKind::Send, false));
        assert!(
            matches!(out, Outcome::Unknown { .. }),
            "out: {out:?}\n{}",
            server.report()
        );
        let adds = server
            .methods()
            .iter()
            .filter(|m| *m == "thread/queue/add")
            .count();
        assert_eq!(
            adds,
            1,
            "응답을 잃은 요청을 다시 보내지 않는다\n{}",
            server.report()
        );

        let mut r = rules(QueueAdd::Drop);
        r.queued = vec!["tasty-0123456789abcdef0123456789abcdef".into()];
        let server = start(r);
        assert_eq!(
            run(&job(server.port, JobKind::Send, false)),
            Outcome::Accepted {
                submission_id: None,
                confirmed_by: "queue_list".into()
            },
            "{}",
            server.report()
        );
    }

    #[test]
    fn repeated_accept_errors_keep_the_fake_server_log_bounded() {
        use super::fake::{ACCEPT_ERROR_LINES, Events};
        let mut ev = Events::default();
        for _ in 0..37 {
            ev.note_accept_error("kind=Other os=Some(24) too many open files".into());
        }
        assert_eq!(
            ev.lines(),
            ["accept error: kind=Other os=Some(24) too many open files (x37)"]
        );

        let mut ev = Events::default();
        for i in 0..ACCEPT_ERROR_LINES + 5 {
            ev.note_accept_error(format!("kind=Other os=Some({i}) distinct"));
            ev.note_accept_error(format!("kind=Other os=Some({i}) distinct"));
        }
        let lines = ev.lines();
        assert_eq!(lines.len(), ACCEPT_ERROR_LINES + 1, "{lines:#?}");
        assert_eq!(
            lines[0],
            "accept error: kind=Other os=Some(0) distinct (x2)"
        );
        assert_eq!(lines[ACCEPT_ERROR_LINES], "accept errors truncated");

        // 받은 연결이 끼면 같은 오류라도 새 줄이다.
        let mut ev = Events::default();
        ev.note_accept_error("e".into());
        assert_eq!(ev.note_accepted(), 1);
        ev.note_accept_error("e".into());
        assert_eq!(
            ev.lines(),
            ["accept error: e", "conn 1: accepted", "accept error: e"]
        );
    }

    #[test]
    fn verification_never_sends_again() {
        let mut r = rules(QueueAdd::Accept);
        r.queued = vec![];
        let server = start(r);
        assert!(matches!(
            run(&job(server.port, JobKind::Verify, false)),
            Outcome::StillUnknown { .. }
        ));
        assert!(!server.methods().contains(&"thread/queue/add".to_string()));
    }

    #[test]
    fn an_unreachable_endpoint_times_out_as_not_sent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // 연결은 받지만 handshake에 답하지 않는 서버.
        let _hold = std::thread::spawn(move || {
            let _conn = listener.accept();
            std::thread::sleep(Duration::from_secs(30));
        });
        let started = std::time::Instant::now();
        assert!(matches!(
            run(&job(port, JobKind::Send, false)),
            Outcome::NotSent { .. }
        ));
        assert!(started.elapsed() < RPC_TIMEOUT + Duration::from_secs(5));
    }
}
