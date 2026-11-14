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
    use std::sync::{Arc, Mutex};

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

    pub(crate) struct Server {
        pub port: u16,
        pub seen: Arc<Mutex<Vec<Value>>>,
    }

    pub(crate) fn start(rules: Rules) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let rules = rules.clone();
                let log = log.clone();
                std::thread::spawn(move || {
                    test_server::accept_handshake(&mut s);
                    while let Some(text) = test_server::read_text(&mut s) {
                        let req: Value = serde_json::from_str(&text).unwrap();
                        log.lock().unwrap().push(req.clone());
                        let Some(id) = req.get("id").cloned() else {
                            continue;
                        };
                        let result = match req["method"].as_str().unwrap_or_default() {
                            "thread/loaded/list" => {
                                json!({"data": rules.loaded, "nextCursor": null})
                            }
                            "thread/queue/list" => json!({"data": rules.queued.iter()
                                .map(|c| json!({"id":"q","clientUserMessageId":c})).collect::<Vec<_>>()}),
                            "thread/queue/add" => match rules.queue_add {
                                QueueAdd::Accept => json!({"queuedSubmission": {"id": "sub-1"}}),
                                QueueAdd::Reject => {
                                    test_server::write_text(
                                        &mut s,
                                        &json!({"id": id,
                                        "error": {"code": -32600, "message": "thread not found"}})
                                        .to_string(),
                                    );
                                    continue;
                                }
                                QueueAdd::Drop => return,
                            },
                            _ => json!({}),
                        };
                        test_server::write_text(
                            &mut s,
                            &json!({"id": id, "result": result}).to_string(),
                        );
                    }
                });
            }
        });
        Server { port, seen }
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
        assert!(matches!(
            run(&job(server.port, JobKind::Send, false)),
            Outcome::Unknown { .. }
        ));
        let adds = server
            .methods()
            .iter()
            .filter(|m| *m == "thread/queue/add")
            .count();
        assert_eq!(adds, 1, "응답을 잃은 요청을 다시 보내지 않는다");

        let mut r = rules(QueueAdd::Drop);
        r.queued = vec!["tasty-0123456789abcdef0123456789abcdef".into()];
        let server = start(r);
        assert_eq!(
            run(&job(server.port, JobKind::Send, false)),
            Outcome::Accepted {
                submission_id: None,
                confirmed_by: "queue_list".into()
            }
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
