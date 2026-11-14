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

type SocketRegistry = Arc<Mutex<Vec<Weak<Stream>>>>;

fn poisoned_socket_registry() -> (Arc<Stream>, TcpStream, SocketRegistry) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let socket = Arc::new(Stream::Tcp(listener.accept().unwrap().0));
    let sockets = Arc::new(Mutex::new(vec![Arc::downgrade(&socket)]));
    let poisoned = std::panic::catch_unwind(|| {
        let _held = sockets.lock().unwrap();
        panic!("poison socket registry");
    });
    assert!(poisoned.is_err());
    assert!(sockets.is_poisoned());
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
