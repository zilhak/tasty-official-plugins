use super::*;
use crate::rpc::Client;
use crate::ws::test_server;

fn run_reply(approval: &str, reject_bind: bool) -> (Result<Value, crate::rpc::RpcError>, usize) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let approval = approval.to_owned();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let head = test_server::accept_handshake(&mut socket);
        assert!(head.contains("Authorization: Bearer backend-secret"));
        let init: Value =
            serde_json::from_str(&test_server::read_text(&mut socket).unwrap()).unwrap();
        test_server::write_text(
            &mut socket,
            &json!({"id":init["id"],"result":{}}).to_string(),
        );
        let initialized: Value =
            serde_json::from_str(&test_server::read_text(&mut socket).unwrap()).unwrap();
        assert_eq!(initialized["method"], "initialized");
        let request: Value =
            serde_json::from_str(&test_server::read_text(&mut socket).unwrap()).unwrap();
        assert_eq!(request["method"], "thread/resume");
        assert_eq!(request["params"]["threadId"], "selected-by-picker");
        assert_eq!(request["params"]["approvalPolicy"], "never");
        assert_eq!(request["params"]["sandbox"], "read-only");
        assert_eq!(request["params"]["custom"], "preserved");
        test_server::write_text(&mut socket, &json!({"id":request["id"],"result":{"thread":{"id":"selected-by-picker"},"approvalPolicy":approval,"sandbox":{"type":"readOnly"}}}).to_string());
        // Keep the backend open until the relay/client closes, including error responses.
        test_server::read_text(&mut socket);
    });
    let called = Arc::new(Mutex::new(0));
    let calls = called.clone();
    let relay = Relay::start(
        Endpoint::Ws {
            port,
            token: "backend-secret".into(),
        },
        ResumeSettings {
            approval: Some("never".into()),
            sandbox: Some("read-only".into()),
            model: None,
            effort: None,
        },
        Arc::new(move |result| {
            assert_eq!(result["thread"]["id"], "selected-by-picker");
            *calls.lock().unwrap() += 1;
            if reject_bind {
                Err("stale generation".into())
            } else {
                Ok(())
            }
        }),
    )
    .unwrap();
    let port = relay.url.rsplit(':').next().unwrap().parse().unwrap();
    let mut client = Client::connect(
        &Endpoint::Ws {
            port,
            token: relay.token.clone(),
        },
        Duration::from_secs(3),
    )
    .unwrap();
    let result = client.call(
        "thread/resume",
        json!({"threadId":"selected-by-picker","custom":"preserved"}),
    );
    drop(client);
    drop(relay);
    server.join().unwrap();
    let count = *called.lock().unwrap();
    (result, count)
}

#[test]
fn selected_thread_is_registered_before_success_reaches_the_tui() {
    let (reply, calls) = run_reply("never", false);
    assert_eq!(reply.unwrap()["thread"]["id"], "selected-by-picker");
    assert_eq!(calls, 1);
}
#[test]
fn wrong_permissions_or_failed_registration_are_not_reported_as_success() {
    let (reply, calls) = run_reply("on-request", false);
    assert!(reply.unwrap_err().to_string().contains("requested never"));
    assert_eq!(calls, 0);
    let (reply, calls) = run_reply("never", true);
    assert!(reply.unwrap_err().to_string().contains("stale generation"));
    assert_eq!(calls, 1);
}
#[test]
fn unauthenticated_connections_never_reach_the_backend() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let relay = Relay::start(
        Endpoint::Ws {
            port: listener.local_addr().unwrap().port(),
            token: "secret".into(),
        },
        ResumeSettings::default(),
        Arc::new(|_| panic!("must not bind")),
    )
    .unwrap();
    let port: u16 = relay.url.rsplit(':').next().unwrap().parse().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.contains("401"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn later_explicit_permission_choices_are_not_replaced_by_launch_defaults() {
    let mut params =
        json!({"approvalPolicy":"on-request","sandbox":"read-only","model":"chosen-model"})
            .as_object()
            .unwrap()
            .clone();
    let original = params.clone();
    let effective = apply_settings(
        &mut params,
        &ResumeSettings {
            approval: Some("never".into()),
            sandbox: Some("danger-full-access".into()),
            model: Some("launch-model".into()),
            effort: None,
        },
        true,
    );
    assert_eq!(params, original);
    assert_eq!(effective.approval.as_deref(), Some("on-request"));
    assert_eq!(effective.sandbox.as_deref(), Some("read-only"));
    assert_eq!(effective.model.as_deref(), Some("chosen-model"));
}

#[test]
fn explicit_permission_profile_config_is_not_combined_with_launch_bypass() {
    let mut params = json!({"config":{"default_permissions":"read-only","model":"profile-model"}})
        .as_object()
        .unwrap()
        .clone();
    let original = params.clone();
    let effective = apply_settings(
        &mut params,
        &ResumeSettings {
            approval: Some("never".into()),
            sandbox: Some("danger-full-access".into()),
            model: Some("launch-model".into()),
            effort: None,
        },
        false,
    );
    assert_eq!(params, original);
    assert_eq!(effective, ResumeSettings::default());
}

/// Runs the given client calls through a relay and returns the thread request parameters the
/// backend received. The backend echoes the requested policy in the 0.154 reply shape.
fn backend_params(launch: ResumeSettings, calls: &[(&str, Value)]) -> Vec<Value> {
    relay_run(launch, calls).0
}

/// Like `backend_params`, and also returns the thread IDs the relay registered.
fn relay_run(launch: ResumeSettings, calls: &[(&str, Value)]) -> (Vec<Value>, Vec<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        test_server::accept_handshake(&mut socket);
        let mut seen = Vec::new();
        while let Some(text) = test_server::read_text(&mut socket) {
            let request: Value = serde_json::from_str(&text).unwrap();
            let Some(id) = request.get("id") else {
                continue;
            };
            let params = &request["params"];
            let result = if request["method"]
                .as_str()
                .is_some_and(|m| m.starts_with("thread/") && m != "thread/settings/update")
            {
                seen.push(params.clone());
                let sandbox = match params["sandbox"].as_str() {
                    Some("read-only") => "readOnly",
                    Some("workspace-write") => "workspaceWrite",
                    Some("danger-full-access") => "dangerFullAccess",
                    _ => "workspaceWrite",
                };
                let id = if params["ephemeral"] == true {
                    "aux"
                } else {
                    "t"
                };
                json!({"thread":{"id":id},"approvalPolicy":params["approvalPolicy"],"sandbox":{"type":sandbox},"model":params["model"],"reasoningEffort":params["config"]["model_reasoning_effort"]})
            } else {
                json!({})
            };
            test_server::write_text(&mut socket, &json!({"id":id,"result":result}).to_string());
        }
        seen
    });
    let bound = Arc::new(Mutex::new(Vec::new()));
    let record = bound.clone();
    let relay = Relay::start(
        Endpoint::Ws {
            port,
            token: "backend-secret".into(),
        },
        launch,
        Arc::new(move |reply: &Value| {
            record
                .lock()
                .unwrap()
                .push(reply["thread"]["id"].to_string());
            Ok(())
        }),
    )
    .unwrap();
    let port = relay.url.rsplit(':').next().unwrap().parse().unwrap();
    let mut client = Client::connect(
        &Endpoint::Ws {
            port,
            token: relay.token.clone(),
        },
        Duration::from_secs(3),
    )
    .unwrap();
    for (method, params) in calls {
        client.call(method, params.clone()).unwrap();
    }
    drop(client);
    drop(relay);
    let seen = server.join().unwrap();
    let bound = bound.lock().unwrap().clone();
    (seen, bound)
}

fn bypass() -> ResumeSettings {
    ResumeSettings::from_tui_args(&["--dangerously-bypass-approvals-and-sandbox".into()])
}

/// Policy fields as Codex 0.154 TUI sends them without launch overrides (measured).
fn tui_defaults(sandbox: &str) -> Value {
    json!({"approvalPolicy":"on-request","sandbox":sandbox,"model":null,"config":{"personality":"pragmatic","web_search":"cached"}})
}

fn policy(params: &Value) -> (Value, Value) {
    (params["approvalPolicy"].clone(), params["sandbox"].clone())
}

#[test]
fn launch_policy_replaces_tui_defaults_until_the_user_changes_permissions() {
    let seen = backend_params(
        bypass(),
        &[
            ("thread/start", tui_defaults("workspace-write")),
            ("thread/start", tui_defaults("read-only")),
            (
                "thread/resume",
                json!({"threadId":"t","approvalPolicy":null,"sandbox":null}),
            ),
            (
                "thread/fork",
                json!({"threadId":"t","approvalPolicy":"on-request","sandbox":"read-only"}),
            ),
            // A model change carries no permission fields and is not a permission choice.
            (
                "thread/settings/update",
                json!({"threadId":"t","approvalPolicy":null,"sandboxPolicy":null,"permissions":null,"model":"other"}),
            ),
            ("thread/start", tui_defaults("workspace-write")),
        ],
    );
    for params in &seen {
        assert_eq!(
            policy(params),
            (json!("never"), json!("danger-full-access"))
        );
    }
    assert_eq!(seen.len(), 5);
    assert_eq!(seen[0]["config"]["personality"], "pragmatic");
}

#[test]
fn a_later_permission_choice_in_the_tui_is_kept() {
    let seen = backend_params(
        bypass(),
        &[
            ("thread/start", tui_defaults("workspace-write")),
            (
                "thread/settings/update",
                json!({"threadId":"t","approvalPolicy":"on-request","approvalsReviewer":"user","sandboxPolicy":null,"permissions":":workspace","model":null}),
            ),
            ("thread/start", tui_defaults("workspace-write")),
        ],
    );
    assert_eq!(
        policy(&seen[0]),
        (json!("never"), json!("danger-full-access"))
    );
    assert_eq!(
        policy(&seen[1]),
        (json!("on-request"), json!("workspace-write"))
    );
}

#[test]
fn separate_or_missing_launch_options_only_set_what_was_given() {
    let sandbox_only = ResumeSettings::from_tui_args(&["-s".into(), "read-only".into()]);
    let seen = backend_params(
        sandbox_only,
        &[("thread/start", tui_defaults("workspace-write"))],
    );
    assert_eq!(policy(&seen[0]), (json!("on-request"), json!("read-only")));
    let both = ResumeSettings::from_tui_args(&[
        "-a".into(),
        "on-request".into(),
        "-s".into(),
        "read-only".into(),
    ]);
    let seen = backend_params(
        both,
        &[(
            "thread/start",
            json!({"approvalPolicy":"never","sandbox":"danger-full-access"}),
        )],
    );
    assert_eq!(policy(&seen[0]), (json!("on-request"), json!("read-only")));
    // No launch options: the TUI's values pass through and nothing is widened.
    let seen = backend_params(
        ResumeSettings::default(),
        &[("thread/start", tui_defaults("read-only"))],
    );
    assert_eq!(policy(&seen[0]), (json!("on-request"), json!("read-only")));
}

#[test]
fn the_reply_is_verified_against_the_launch_policy_not_the_tui_default() {
    let mut params = tui_defaults("workspace-write").as_object().unwrap().clone();
    let effective = apply_settings(&mut params, &bypass(), false);
    assert_eq!(effective.approval.as_deref(), Some("never"));
    assert_eq!(effective.sandbox.as_deref(), Some("danger-full-access"));
    let kept_default = json!({"approvalPolicy":"on-request","sandbox":{"type":"workspaceWrite"}});
    assert!(effective.verify(&kept_default).is_err());
}

#[test]
fn ephemeral_helper_threads_keep_their_policy_and_are_not_registered() {
    // Codex 0.154 TUI starts an ephemeral read-only thread for structured side tasks such as
    // naming the conversation (measured). It is not the user's conversation.
    let helper =
        json!({"approvalPolicy":"never","sandbox":"read-only","model":"small","ephemeral":true});
    let (seen, bound) = relay_run(
        bypass(),
        &[
            (
                "thread/start",
                json!({"approvalPolicy":"on-request","sandbox":"workspace-write","ephemeral":false}),
            ),
            ("thread/start", helper.clone()),
        ],
    );
    assert_eq!(
        policy(&seen[0]),
        (json!("never"), json!("danger-full-access"))
    );
    assert_eq!(seen[1], helper);
    assert_eq!(bound, vec!["\"t\"".to_string()]);
}

fn poisoned_socket_registry() -> (Arc<Stream>, TcpStream, Arc<Sockets>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let socket = Arc::new(Stream::Tcp(listener.accept().unwrap().0));
    let sockets = Arc::new(Sockets::new());
    sockets
        .state
        .lock()
        .unwrap()
        .live
        .push(Arc::downgrade(&socket));
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = sockets.state.lock().unwrap();
        panic!("poison socket registry");
    }));
    assert!(poisoned.is_err());
    assert!(sockets.state.is_poisoned());
    (socket, peer, sockets)
}

#[test]
fn poisoned_registry_still_closes_registered_sockets() {
    let (_live_socket, mut peer, sockets) = poisoned_socket_registry();
    close_streams(&sockets);
    assert_eq!(
        peer.read(&mut [0]).expect("shutdown releases the reader"),
        0
    );
}

#[test]
fn poisoned_registry_is_closed_and_joined_during_unwind() {
    let (_live_socket, mut peer, streams) = poisoned_socket_registry();
    let (tx, rx) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        tx.send(peer.read(&mut [0])).unwrap();
    });
    let stop = Arc::new(AtomicBool::new(false));
    let relay = Relay {
        url: String::new(),
        token: String::new(),
        stop: stop.clone(),
        streams,
        task: Some(task),
    };
    let original_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _relay = relay;
        panic!("unwind while owning relay");
    }));
    assert!(original_panic.is_err());
    assert!(stop.load(Ordering::Acquire));
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(0)),
        "Drop must shut down the socket before joining its reader"
    );
}

#[test]
fn child_efforts_reach_start_and_resume_without_cross_session_defaults() {
    let _logs =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
    for (model, effort) in [("provider/model-a", "medium"), ("provider:model-b", "high")] {
        let settings = ResumeSettings::from_tui_args(&[
            "-m".into(),
            model.into(),
            "-c".into(),
            format!("model_reasoning_effort={effort}"),
        ]);
        let seen = backend_params(
            settings,
            &[
                ("thread/start", json!({"config":{"unrelated":true}})),
                ("thread/resume", json!({"threadId":"t","config":null})),
            ],
        );
        assert_eq!(seen.len(), 2);
        tracing::info!(
            "mock backend start/resume: {}",
            serde_json::to_string(&seen).unwrap()
        );
        for request in &seen {
            assert_eq!(request["model"], model);
            assert_eq!(request["config"]["model_reasoning_effort"], effort);
        }
        assert_eq!(seen[0]["config"]["unrelated"], true);
    }
    let seen = backend_params(ResumeSettings::default(), &[("thread/start", json!({}))]);
    assert!(seen[0].get("model").is_none());
    assert!(seen[0].get("config").is_none());
}

#[test]
fn explicit_request_config_effort_takes_precedence_and_is_verified() {
    let settings = ResumeSettings {
        effort: Some("medium".into()),
        ..Default::default()
    };
    let seen = backend_params(
        settings.clone(),
        &[(
            "thread/start",
            json!({
                "config":{"model_reasoning_effort":"high","unrelated":true}
            }),
        )],
    );
    assert_eq!(seen[0]["config"]["model_reasoning_effort"], "high");
    assert!(settings.verify(&json!({"reasoningEffort":"low"})).is_err());
    assert!(settings.verify(&json!({})).is_err());
    assert!(
        settings
            .verify(&json!({"reasoningEffort":"medium"}))
            .is_ok()
    );
}

#[test]
fn upstream_model_and_effort_rejection_is_never_rewritten_as_success() {
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let chosen = Arc::new(AtomicBool::new(false));
    let defaults = ResumeSettings {
        model: Some("unavailable-model".into()),
        effort: Some("max".into()),
        ..Default::default()
    };
    message::request(
        r#"{"id":1,"method":"thread/start","params":{}}"#,
        &defaults,
        &chosen,
        &pending,
    )
    .unwrap();
    let bind: Bind = Arc::new(|_| panic!("rejected request must not bind"));
    let raw = r#"{"id":1,"error":{"code":400,"message":"unsupported model or effort"}}"#;
    assert!(message::reply(raw, &pending, &bind).unwrap().is_none());
    assert!(pending.lock().unwrap().is_empty());
}

/// 받아들인 쪽 소켓과 그 상대. 상대는 relay 바깥의 TUI나 백엔드 역할이다.
fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    (listener.accept().unwrap().0, peer)
}

/// 상대가 닫힘을 봤는지 본다. 읽기 기한 만료는 닫히지 않은 것이다.
fn assert_peer_closed(peer: &mut TcpStream, what: &str) {
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    match peer.read(&mut [0u8; 1]) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            ) => {}
        other => panic!("{what}: the peer did not see the socket close: {other:?}"),
    }
}

/// 시험 표지를 채널로 넘기는 probe. `hold`에 든 지점에서는 해제 신호를 기다린다.
struct Gate {
    points: std::sync::mpsc::Receiver<Point>,
    release: std::sync::mpsc::Sender<()>,
}
fn gated_sockets(hold: &'static [Point]) -> (Sockets, Gate) {
    let (point_tx, points) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let point_tx = Mutex::new(point_tx);
    let release_rx = Mutex::new(release_rx);
    let probe: Probe = Arc::new(move |point: Point| {
        if let Point::Failed {
            site,
            source,
            raw_os_error,
        } = &point
        {
            eprintln!("RELAY_FAILED site={site:?} source={source:?} raw={raw_os_error:?}");
        }
        let held = hold.contains(&point);
        let _ = point_tx.lock().unwrap().send(point);
        if held {
            let released = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
            assert!(
                released.is_ok(),
                "RELAY_PROBE_SETUP the test never released the probe"
            );
        }
    });
    (Sockets::with_probe(probe), Gate { points, release })
}
impl Gate {
    /// 지정한 지점이 올 때까지 표지를 받는다. 받은 표지를 모두 돌려준다.
    fn wait_for(&self, point: &Point) -> Vec<Point> {
        let mut seen = Vec::new();
        loop {
            match self.points.recv_timeout(Duration::from_secs(5)) {
                Ok(next) => {
                    let found = &next == point;
                    seen.push(next);
                    if found {
                        return seen;
                    }
                }
                Err(error) => panic!("RELAY_PROBE_SETUP {point:?} never came ({error}): {seen:?}"),
            }
        }
    }
    /// 지정한 지점이 모두 올 때까지 표지를 받는다. 받은 표지를 모두 돌려준다.
    fn wait_all(&self, wanted: &[Point]) -> Vec<Point> {
        let mut seen = Vec::new();
        while !wanted.iter().all(|point| seen.contains(point)) {
            match self.points.recv_timeout(Duration::from_secs(5)) {
                Ok(next) => seen.push(next),
                Err(error) => panic!("RELAY_PROBE_SETUP {wanted:?} never came ({error}): {seen:?}"),
            }
        }
        seen
    }
    fn release(&self) {
        let _ = self.release.send(());
    }
    fn rest(&self) -> Vec<Point> {
        self.points.try_iter().collect()
    }
}

#[test]
fn a_socket_tracked_after_closing_is_closed_and_not_registered() {
    let (stream, mut peer) = socket_pair();
    let stream = Stream::Tcp(stream);
    let sockets = Sockets::new();
    sockets.state.lock().unwrap().closing = true;
    let refused = track(&stream, &sockets);
    assert_eq!(
        refused.err().map(|e| e.to_string()).as_deref(),
        Some("relay closing")
    );
    assert!(sockets.state.lock().unwrap().live.is_empty());
    // 원래 stream이 아직 살아 있으므로 상대의 닫힘은 track 안의 닫기에서만 온다.
    assert_peer_closed(&mut peer, "refused track");
    drop(stream);
}

#[test]
fn a_connection_that_reaches_track_after_drop_started_is_closed_at_once() {
    // 백엔드는 쓰이지 않는다. 연결은 front를 등록하기 전에 끝나야 한다.
    let unused = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let (sockets, gate) = gated_sockets(&[Point::BeforeTrack(Side::Front)]);
    let relay = Relay::start_with(
        Endpoint::Ws {
            port: unused.local_addr().unwrap().port(),
            token: "unused".into(),
        },
        ResumeSettings::default(),
        Arc::new(|_| panic!("must not bind")),
        sockets,
    )
    .unwrap();
    let port: u16 = relay.url.rsplit(':').next().unwrap().parse().unwrap();
    let mut tui = TcpStream::connect(("127.0.0.1", port)).unwrap();
    gate.wait_for(&Point::BeforeTrack(Side::Front));
    let registry = relay.streams.clone();
    let (done_tx, done) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(relay);
        let _ = done_tx.send(());
    });
    let waited = std::time::Instant::now();
    while !registry.state.lock().unwrap().closing {
        assert!(
            waited.elapsed() < Duration::from_secs(2),
            "RELAY_PROBE_SETUP drop never set the closing mark"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    gate.release();
    if done.recv_timeout(Duration::from_secs(2)).is_err() {
        let _ = tui.shutdown(std::net::Shutdown::Both);
        drop(tui);
        match done.recv_timeout(Duration::from_secs(10)) {
            Ok(()) => {
                dropper.join().unwrap();
                panic!("drop did not return within 2s after a late track");
            }
            Err(error) => panic!("RELAY_PROBE_SETUP drop never returned ({error})"),
        }
    }
    dropper.join().unwrap();
    assert_peer_closed(&mut tui, "late track");
    let rest = gate.rest();
    assert!(rest.contains(&Point::TrackRefused), "{rest:?}");
    assert!(!rest.contains(&Point::AfterTrack(Side::Front)), "{rest:?}");
}

#[test]
fn dropping_a_relay_ends_reads_blocked_after_the_upgrade() {
    use std::time::Instant;
    let backend = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let backend_port = backend.local_addr().unwrap().port();
    let (accepted_tx, accepted) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = backend.accept().unwrap();
        test_server::accept_handshake(&mut socket);
        let _ = accepted_tx.send(socket);
    });
    let (sockets, gate) = gated_sockets(&[]);
    let relay = Relay::start_with(
        Endpoint::Ws {
            port: backend_port,
            token: "backend".into(),
        },
        ResumeSettings::default(),
        Arc::new(|_| panic!("must not bind")),
        sockets,
    )
    .unwrap();
    let port: u16 = relay.url.rsplit(':').next().unwrap().parse().unwrap();
    let mut tui = TcpStream::connect(("127.0.0.1", port)).unwrap();
    // WsConn이 쥔 복제가 남으면 원본을 닫아도 FIN이 나가지 않으므로 handshake 뒤 바로 놓는다.
    drop(WsConn::handshake(tui.try_clone().unwrap(), "localhost", Some(&relay.token)).unwrap());
    let mut peer = accepted
        .recv_timeout(Duration::from_secs(5))
        .expect("RELAY_PROBE_SETUP the backend never finished the handshake");
    server.join().unwrap();
    let mut entered = Vec::new();
    while !(entered.contains(&Point::BeforeRead(Direction::Up))
        && entered.contains(&Point::BeforeRead(Direction::Down)))
    {
        match gate.points.recv_timeout(Duration::from_secs(5)) {
            Ok(point) => entered.push(point),
            Err(error) => {
                panic!("RELAY_PROBE_SETUP both reads never started ({error}): {entered:?}")
            }
        }
    }
    std::thread::sleep(Duration::from_millis(50));
    let (time_tx, times) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        let _ = time_tx.send(Instant::now());
        drop(relay);
        let _ = time_tx.send(Instant::now());
    });
    let started = times
        .recv_timeout(Duration::from_secs(5))
        .expect("RELAY_PROBE_SETUP the drop thread never started");
    let limit = Duration::from_secs(2);
    let deadline = started + limit;
    let waited = times.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    match waited {
        Ok(ended) => {
            let took = ended - started;
            if took > limit {
                dropper.join().unwrap();
                panic!("RELAY_DROP_EXCEEDED 2s: drop took {took:?}");
            }
        }
        Err(_) => {
            // 상한까지 돌아오지 않았다. 상대를 닫아 relay의 읽기를 풀고 끝 시각을 한 번 더 받는다.
            let _ = tui.shutdown(std::net::Shutdown::Both);
            let _ = peer.shutdown(std::net::Shutdown::Both);
            drop(tui);
            drop(peer);
            match times.recv_timeout(Duration::from_secs(10)) {
                Ok(ended) => {
                    dropper.join().unwrap();
                    let took = ended - started;
                    if took > limit {
                        panic!("RELAY_DROP_EXCEEDED 2s: drop took {took:?}");
                    }
                    panic!("RELAY_PROBE_SETUP late signal, drop within limit ({took:?})");
                }
                Err(error) => panic!("RELAY_PROBE_SETUP drop never returned ({error})"),
            }
        }
    }
    dropper.join().unwrap();
    assert_peer_closed(&mut tui, "TUI after drop");
    assert_peer_closed(&mut peer, "backend after drop");
}

const RELAY_TOKEN: &str = "relay-token";

/// `connection()`을 직접 돌리는 연결 하나. 결과는 완료 채널로 받는다.
struct Running {
    done: std::sync::mpsc::Receiver<io::Result<()>>,
    thread: JoinHandle<()>,
}
impl Running {
    /// 2s 안에 받은 연결 결과. 넘기면 상대를 닫아 회수한 뒤 실패한다.
    fn finish(self, peers: &[&TcpStream]) -> io::Result<()> {
        match self.done.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => {
                self.thread.join().unwrap();
                result
            }
            Err(_) => {
                for peer in peers {
                    let _ = peer.shutdown(std::net::Shutdown::Both);
                }
                match self.done.recv_timeout(Duration::from_secs(10)) {
                    Ok(result) => {
                        self.thread.join().unwrap();
                        panic!("the connection did not end within 2s: {result:?}");
                    }
                    Err(error) => panic!("RELAY_PROBE_SETUP the connection never ended ({error})"),
                }
            }
        }
    }
}

/// TUI 상대를 붙이고 그 front 소켓으로 `connection()`을 띄운다. 레지스트리는 시험이 쥔다.
fn run_connection(endpoint: Endpoint, sockets: &Arc<Sockets>) -> (Running, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let tui = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    tui.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let front = listener.accept().unwrap().0;
    let sockets = sockets.clone();
    let (finished, done) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = connection(
            Stream::Tcp(front),
            &endpoint,
            RELAY_TOKEN,
            (&ResumeSettings::default(), Arc::new(AtomicBool::new(false))),
            Arc::new(|_| panic!("must not bind")),
            &sockets,
        );
        let _ = finished.send(result);
    });
    (Running { done, thread }, tui)
}

/// 업그레이드까지 간 연결. TUI 상대, TUI의 WebSocket, 백엔드 상대를 돌려준다.
fn upgraded(sockets: &Arc<Sockets>) -> (Running, TcpStream, WsConn<TcpStream>, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted_tx, accepted) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            test_server::accept_handshake(&mut socket);
            let _ = accepted_tx.send(socket);
        }
    });
    let (running, tui) = run_connection(
        Endpoint::Ws {
            port,
            token: "backend".into(),
        },
        sockets,
    );
    let ws = WsConn::handshake(tui.try_clone().unwrap(), "localhost", Some(RELAY_TOKEN)).unwrap();
    let backend = accepted
        .recv_timeout(Duration::from_secs(5))
        .expect("RELAY_PROBE_SETUP the backend never finished the handshake");
    (running, tui, ws, backend)
}

fn reports(points: &[Point]) -> Vec<(&'static str, bool, bool)> {
    points
        .iter()
        .filter_map(|point| match point {
            Point::Reported {
                direction,
                closing,
                warn,
            } => Some((*direction, *closing, *warn)),
            _ => None,
        })
        .collect()
}

fn failures(points: &[Point]) -> Vec<(Site, Source)> {
    points
        .iter()
        .filter_map(|point| match point {
            Point::Failed { site, source, .. } => Some((*site, *source)),
            _ => None,
        })
        .collect()
}

fn assert_relay_closing(result: io::Result<()>, seen: &[Point]) {
    assert_eq!(
        result.err().map(|error| error.to_string()).as_deref(),
        Some("relay closing"),
        "{seen:?}"
    );
    assert!(!reports(seen).iter().any(|(_, _, warn)| *warn), "{seen:?}");
}

/// 두 전송 루프가 닫힌 소켓을 읽고 relay closing으로 끝났는지 본다.
fn assert_both_reads_closed(seen: &[Point]) {
    let mut failed = failures(seen);
    failed.sort_by_key(|(site, _)| format!("{site:?}"));
    assert_eq!(
        failed,
        vec![
            (Site::Read(Direction::Down), Source::SocketIo),
            (Site::Read(Direction::Up), Source::SocketIo),
        ],
        "{seen:?}"
    );
    let mut reported = reports(seen);
    reported.sort();
    assert_eq!(
        reported,
        vec![
            ("TUI to server", true, false),
            ("server to TUI", true, false)
        ],
        "{seen:?}"
    );
}

#[test]
fn a_timeout_set_on_a_socket_closed_by_the_relay_ends_as_relay_closing() {
    let unused = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let (sockets, gate) = gated_sockets(&[Point::AfterTrack(Side::Front)]);
    let sockets = Arc::new(sockets);
    let (running, tui) = run_connection(
        Endpoint::Ws {
            port: unused.local_addr().unwrap().port(),
            token: "unused".into(),
        },
        &sockets,
    );
    let mut seen = gate.wait_for(&Point::AfterTrack(Side::Front));
    close_streams(&sockets);
    gate.release();
    let result = running.finish(&[&tui]);
    seen.extend(gate.rest());
    assert_relay_closing(result, &seen);
    // 전송 루프 전에 끝나므로 보고가 없다.
    assert!(reports(&seen).is_empty(), "{seen:?}");
    // macOS는 닫힌 소켓의 기한 설정이 EINVAL이고, 기한 설정이 되는 OS는 다음 header 읽기가 EOF다.
    let failed = failures(&seen);
    assert!(
        failed == [(Site::FrontTimeout, Source::Timeout)]
            || failed == [(Site::FrontHeader, Source::SocketIo)],
        "{seen:?}"
    );
}

/// 업그레이드 뒤 한쪽 기한을 풀기 직전에 relay를 닫는다.
fn clear_deadline_after_close(side: Side, hold: &'static [Point], site: Site) {
    let (sockets, gate) = gated_sockets(hold);
    let sockets = Arc::new(sockets);
    let (running, tui, ws, backend) = upgraded(&sockets);
    drop(ws);
    let mut seen = gate.wait_for(&Point::BeforeUnlimit(side));
    close_streams(&sockets);
    gate.release();
    let result = running.finish(&[&tui, &backend]);
    seen.extend(gate.rest());
    assert_relay_closing(result, &seen);
    if failures(&seen).first() == Some(&(site, Source::Timeout)) {
        // macOS: 기한 해제가 EINVAL로 실패해 전송 루프 전에 끝난다.
        assert_eq!(failures(&seen).len(), 1, "{seen:?}");
        assert!(reports(&seen).is_empty(), "{seen:?}");
    } else {
        // 기한 해제가 되는 OS: 두 전송 루프가 닫힌 소켓을 읽고 끝난다.
        assert_both_reads_closed(&seen);
    }
}

#[test]
fn clearing_the_front_deadline_after_the_relay_closed_ends_as_relay_closing() {
    clear_deadline_after_close(
        Side::Front,
        &[Point::BeforeUnlimit(Side::Front)],
        Site::FrontUnlimit,
    );
}

#[test]
fn clearing_the_backend_deadline_after_the_relay_closed_ends_as_relay_closing() {
    clear_deadline_after_close(
        Side::Backend,
        &[Point::BeforeUnlimit(Side::Backend)],
        Site::BackendUnlimit,
    );
}

#[test]
fn transfers_cut_by_relay_closing_end_without_a_warning() {
    let (sockets, gate) = gated_sockets(&[]);
    let sockets = Arc::new(sockets);
    let (running, tui, ws, backend) = upgraded(&sockets);
    drop(ws);
    let mut seen = gate.wait_all(&[
        Point::BeforeRead(Direction::Up),
        Point::BeforeRead(Direction::Down),
    ]);
    std::thread::sleep(Duration::from_millis(50));
    close_streams(&sockets);
    let result = running.finish(&[&tui, &backend]);
    seen.extend(gate.rest());
    assert_relay_closing(result, &seen);
    assert_both_reads_closed(&seen);
}

#[test]
fn an_upstream_write_cut_by_relay_closing_ends_without_a_warning() {
    let (sockets, gate) = gated_sockets(&[Point::BeforeWrite(Direction::Up)]);
    let sockets = Arc::new(sockets);
    let (running, tui, mut ws, backend) = upgraded(&sockets);
    ws.send_text(r#"{"id":1,"method":"noop"}"#).unwrap();
    let mut seen = gate.wait_for(&Point::BeforeWrite(Direction::Up));
    close_streams(&sockets);
    gate.release();
    let result = running.finish(&[&tui, &backend]);
    drop(ws);
    seen.extend(gate.rest());
    assert_relay_closing(result, &seen);
    assert!(
        failures(&seen).contains(&(Site::Write(Direction::Up), Source::SocketIo)),
        "{seen:?}"
    );
    assert!(
        reports(&seen).contains(&("TUI to server", true, false)),
        "{seen:?}"
    );
}

#[test]
fn a_downstream_write_cut_by_relay_closing_ends_without_a_warning() {
    let (sockets, gate) = gated_sockets(&[Point::BeforeWrite(Direction::Down)]);
    let sockets = Arc::new(sockets);
    let (running, tui, ws, mut backend) = upgraded(&sockets);
    drop(ws);
    test_server::write_text(&mut backend, r#"{"method":"noop"}"#);
    let mut seen = gate.wait_for(&Point::BeforeWrite(Direction::Down));
    close_streams(&sockets);
    gate.release();
    let result = running.finish(&[&tui, &backend]);
    seen.extend(gate.rest());
    assert_relay_closing(result, &seen);
    assert!(
        failures(&seen).contains(&(Site::Write(Direction::Down), Source::SocketIo)),
        "{seen:?}"
    );
    assert!(
        reports(&seen).contains(&("server to TUI", true, false)),
        "{seen:?}"
    );
}

#[test]
fn each_relay_connection_half_sets_only_its_own_fault_flag() {
    let (stream, _peer) = socket_pair();
    let stream = Stream::Tcp(stream);
    let (reader_fault, writer_fault) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (mut reader, mut writer) =
        pair(&stream, true, reader_fault.clone(), writer_fault.clone()).unwrap();
    stream.close();
    let flags = || {
        (
            reader_fault.swap(false, Ordering::AcqRel),
            writer_fault.swap(false, Ordering::AcqRel),
        )
    };
    // reader 쪽 쓰기는 pong과 같은 `Duplex::write_all` 경로다.
    assert!(reader.send_text("pong path").is_err());
    assert_eq!(flags(), (true, false));
    assert!(reader.recv_text().is_err());
    assert_eq!(flags(), (true, false));
    assert!(writer.send_text("forward").is_err());
    assert_eq!(flags(), (false, true));
}

#[test]
fn a_parse_error_while_closing_is_still_reported_as_itself() {
    let (sockets, gate) = gated_sockets(&[]);
    let sockets = Arc::new(sockets);
    let (running, tui, mut ws, backend) = upgraded(&sockets);
    let mut seen = gate.wait_all(&[
        Point::BeforeRead(Direction::Up),
        Point::BeforeRead(Direction::Down),
    ]);
    // 표지만 세우고 소켓은 닫지 않는다.
    sockets.state.lock().unwrap().closing = true;
    ws.send_text("not json").unwrap();
    let result = running.finish(&[&tui, &backend]);
    drop(ws);
    seen.extend(gate.rest());
    let error = result.expect_err("a parse error must end the connection");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{seen:?}");
    assert!(
        error.to_string().starts_with("invalid relay JSON at line "),
        "{error} {seen:?}"
    );
    let reported = reports(&seen);
    assert!(
        reported.contains(&("TUI to server", false, true)),
        "{seen:?}"
    );
    assert!(
        reported.contains(&("server to TUI", true, false)),
        "{seen:?}"
    );
}
