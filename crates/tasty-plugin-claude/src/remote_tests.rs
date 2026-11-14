//! attach 원격 자식의 client·서버 분기. 모델과 host 는 흉내 낸다.

use std::cell::RefCell;

use super::*;
use crate::codex_return::{ReturnStatus, TurnEnd};
use tasty_plugin_sdk::PluginError;

const THREAD: &str = "0193a6f2-7c1e-7b3d-9a40-1f2e3d4c5b6a";
const RECEIVER: &str = "0123456789abcdef0123456789abcdef";
const ROUTE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ROUTE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

type Answer = Box<dyn Fn(&str, &Value) -> Result<Value, PluginError>>;

struct Host {
    seen: RefCell<Vec<(String, Value)>>,
    answer: Answer,
}

impl HostCall for Host {
    fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        let out = (self.answer)(method, &params);
        self.seen.borrow_mut().push((method.to_string(), params));
        out
    }
}

impl Host {
    fn new(answer: impl Fn(&str, &Value) -> Result<Value, PluginError> + 'static) -> Self {
        Self {
            seen: RefCell::new(Vec::new()),
            answer: Box::new(answer),
        }
    }

    fn methods(&self) -> Vec<String> {
        self.seen.borrow().iter().map(|(m, _)| m.clone()).collect()
    }

    fn params_of(&self, method: &str) -> Vec<Value> {
        self.seen
            .borrow()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

fn fail(code: i32, message: &str) -> Result<Value, PluginError> {
    Err(PluginError::HostCall {
        method: "m".into(),
        message: message.into(),
        code: Some(code),
    })
}

fn tr() -> Translator {
    Translator::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang"),
        "en",
    )
}

fn store() -> (tempfile::TempDir, Returns) {
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    (dir, returns)
}

fn local_binding() -> Binding {
    Binding {
        receiver_id: RECEIVER.into(),
        parent_external: None,
        attach: false,
    }
}

fn scanner() -> Arc<Mutex<ErrorScanner>> {
    Arc::new(Mutex::new(ErrorScanner::new()))
}

/// client host: workspace 2 는 mirror(anchor 12), mirror 자식 L=15 는 서버 R=99 다.
fn client_host(capable: bool, call: fn() -> Result<Value, PluginError>) -> Host {
    Host::new(move |method, params| match method {
        "attach.agent_target" => {
            if params.get("workspace") == Some(&json!(2))
                || params.get("surface_id") == Some(&json!(15))
            {
                Ok(json!({
                    "remote": true, "capable": capable, "workspace": 2,
                    "anchor_surface_id": 12,
                    "child": params.get("surface_id") == Some(&json!(15)),
                }))
            } else {
                Ok(json!({ "remote": false }))
            }
        }
        "attach.agent_call" => call(),
        "attach.agent_commit" => Ok(json!({ "committed": 1 })),
        "codex.parent" => Ok(json!({
            "receiver_id": RECEIVER,
            "terminal_parent": { "parent_external": format!("codex:{THREAD}") },
        })),
        "codex.deliver" => Ok(json!({ "event_id": "ev", "delivery_state": "captured" })),
        "terminal.children" => Ok(json!({ "children": [{ "surface_id": 15, "index": 2 }] })),
        "hook.list" => Ok(json!([])),
        _ => Ok(json!({})),
    })
}

fn spawned() -> Result<Value, PluginError> {
    Ok(json!({
        "call_id": 7,
        "result": { "child_surface_id": 99, "child_index": 0, "requested_session_options": {} },
        "children": [{ "surface_id": 15, "remote_surface_id": 99, "index": 2 }],
    }))
}

fn spawn_params(extra: Value) -> Value {
    let mut p = json!({
        "surface": 3,
        "workspace": "2",
        "prompt": "do it",
        "role": "worker",
        "caller_surface": 3,
        "_tasty_context": { "provider": "cli" },
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

/// 물리 부모: 서버로는 허용 인자만 가고, 알림 훅을 mirror 자식에 건 뒤 commit 한다. 이 Tasty 에 터미널을 만들지 않는다.
#[test]
fn a_mirror_spawn_runs_on_the_server_and_binds_hooks_before_commit() {
    let (_d, returns) = store();
    let host = client_host(true, spawned);
    let out = crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap();
    assert_eq!(out["child_surface_id"], json!(15));
    assert_eq!(out["child_index"], json!(2));
    assert_eq!(out["remote"], json!(true));
    assert_eq!(out["remote_child_surface_id"], json!(99));
    assert_eq!(out["parent_surface_id"], json!(3));
    let methods = host.methods();
    assert!(
        !methods
            .iter()
            .any(|m| m == "terminal.spawn" || m == "surface.send")
    );
    let call = &host.params_of("attach.agent_call")[0];
    assert_eq!(call["method"], json!("spawn"));
    assert_eq!(call["anchor_surface_id"], json!(12));
    assert_eq!(call["parent"], json!({ "surface": 3 }));
    assert_eq!(call["role"], json!("worker"));
    let sent = call["params"].as_object().unwrap();
    for forbidden in [
        "_tasty_context",
        "surface",
        "parent",
        "workspace",
        "caller_surface",
        "role",
    ] {
        assert!(
            !sent.contains_key(forbidden),
            "{forbidden} leaked: {sent:?}"
        );
    }
    assert_eq!(sent["prompt"], json!("do it"));
    // 물리 부모도 턴 본문을 돌려받도록 불투명 토큰만 보낸다(부모 surface 는 보내지 않는다).
    assert!(remote_attach::is_route_token(
        sent[RETURN_ROUTE_KEY].as_str().unwrap()
    ));
    let last_hook = methods
        .iter()
        .rposition(|m| m == "hook.set")
        .expect("notify hooks");
    let commit = methods
        .iter()
        .position(|m| m == "attach.agent_commit")
        .expect("commit");
    assert!(last_hook < commit, "{methods:?}");
    assert!(
        host.params_of("hook.set")
            .iter()
            .all(|p| p["surface_id"] == json!(15))
    );
}

/// Codex 대화 부모: 결과 경로를 mirror 자식에 묶고 서버에 턴 결과를 요청한다. 알림 훅은 걸지 않는다.
#[test]
fn a_codex_parent_binds_its_route_on_the_mirror_child_and_asks_for_turns() {
    let (_d, returns) = store();
    let host = client_host(true, spawned);
    let mut params =
        spawn_params(json!({ "_tasty_context": { "provider": "codex", "thread_id": THREAD } }));
    params.as_object_mut().unwrap().remove("surface");
    let out =
        crate::handlers::handle_spawn(&scanner(), &returns, &host, &params, None, &tr()).unwrap();
    assert_eq!(out["parent_external"], json!(format!("codex:{THREAD}")));
    let call = &host.params_of("attach.agent_call")[0];
    assert_eq!(
        call["parent"],
        json!({ "external": format!("codex:{THREAD}") })
    );
    let route = call["params"][RETURN_ROUTE_KEY]
        .as_str()
        .unwrap()
        .to_string();
    assert!(remote_attach::is_route_token(&route));
    assert!(host.params_of("hook.set").is_empty());
    assert_eq!(host.params_of("attach.agent_commit").len(), 1);
    // 서버에서 온 결과는 이 경로의 수신처로 기록된다.
    let r = returns
        .remote_receive(
            &host,
            &json!({
                "surface_id": 15, "remote_surface_id": 99, "key": "s:1",
                "payload": { "completion": {
                    "completion_key": "claude:abc:t1", "provider": "claude", "child_surface": 99,
                    "child_session": "sess", "status": "turn_ended", "body": "done",
                    "route": route,
                } },
            }),
            &tr(),
        )
        .unwrap();
    assert_eq!(r, json!({ "status": "recorded" }));
    let delivered = &host.params_of("codex.deliver")[0];
    assert_eq!(delivered["receiver_id"], json!(RECEIVER));
    assert_eq!(delivered["completion_key"], json!("r.claude:abc:t1"));
    assert_eq!(delivered["child_surface"], json!(15));
    assert!(delivered.get("attach").is_none());
    assert!(delivered.get("route").is_none());
}

#[test]
fn an_old_server_is_refused_before_any_call() {
    let (_d, returns) = store();
    let host = client_host(false, spawned);
    let err = crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap_err();
    assert!(err.message.contains("does not support"), "{}", err.message);
    let methods = host.methods();
    assert!(
        !methods
            .iter()
            .any(|m| m.starts_with("attach.agent_call") || m == "terminal.spawn")
    );
}

/// 결과를 모르면 성공으로도 실패로도 단정하지 않고, 경로·훅·commit 을 하지 않는다.
#[test]
fn an_unknown_outcome_is_reported_as_unknown() {
    let (_d, returns) = store();
    let host = client_host(true, || {
        fail(
            remote_attach::OUTCOME_UNKNOWN_CODE,
            "call 7 outcome unknown",
        )
    });
    let err = crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap_err();
    assert!(err.message.contains("unknown"), "{}", err.message);
    assert!(err.message.contains("call 7"), "{}", err.message);
    let methods = host.methods();
    assert!(
        !methods
            .iter()
            .any(|m| m == "hook.set" || m == "attach.agent_commit")
    );
}

/// 사용자가 명시한 --profile-file 경로는 그대로 서버로 가고, 서버가 자기 파일시스템에서 확인한다.
#[test]
fn an_explicit_profile_file_is_checked_on_the_server() {
    let (_d, returns) = store();
    let host = client_host(true, spawned);
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({ "profile_file": "/srv/profiles/p.json" })),
        None,
        &tr(),
    )
    .unwrap();
    assert_eq!(
        host.params_of("attach.agent_call")[0]["params"]["profile_file"],
        json!("/srv/profiles/p.json")
    );

    let server = server_host("pending");
    // 절대 경로이지만 없는 파일. OS마다 절대 경로 형식이 달라 임시 폴더 아래에서 만든다.
    let missing = _d.path().join("missing").join("tasty-remote-profile.json");
    let err = crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &server,
        &origin_params(json!({ "profile_file": missing.to_string_lossy() })),
        None,
        &tr(),
    )
    .unwrap_err();
    assert!(
        err.message.contains("does not exist on the remote"),
        "{}",
        err.message
    );
    for relative in [
        json!({ "profile_file": "p.json" }),
        json!({ "cwd": "work" }),
    ] {
        let err = crate::handlers::handle_spawn(
            &scanner(),
            &returns,
            &server,
            &origin_params(relative),
            None,
            &tr(),
        )
        .unwrap_err();
        assert!(
            err.message.contains("not an absolute path"),
            "{}",
            err.message
        );
    }
    assert!(server.params_of("terminal.spawn").is_empty());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("p.json");
    std::fs::write(&file, "{}").unwrap();
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &server,
        &origin_params(json!({ "profile_file": file.to_string_lossy() })),
        None,
        &tr(),
    )
    .unwrap();
    assert_eq!(server.params_of("terminal.spawn").len(), 1);
}

/// 경로를 묶지 못하면 commit 하지 않는다(첫 완료를 풀지 않는다). 받을 곳 없는 원격 자식은 닫는다.
/// 저장소는 토큰을 발급할 때는 쓸 수 있고, 서버가 자식을 만든 뒤에는 쓸 수 없게 된다.
#[cfg(unix)]
#[test]
fn a_child_whose_route_cannot_be_bound_is_not_committed_and_is_closed() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, returns) = store();
    let book = dir.path().join("codex-return");
    let locked = book.clone();
    let host = Host::new(move |method, params| {
        if method == "attach.agent_call" && params["method"] == json!("spawn") {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        client_host(true, spawned).call(method, params.clone())
    });
    let mut params =
        spawn_params(json!({ "_tasty_context": { "provider": "codex", "thread_id": THREAD } }));
    params.as_object_mut().unwrap().remove("surface");
    let err = crate::handlers::handle_spawn(&scanner(), &returns, &host, &params, None, &tr())
        .unwrap_err();
    assert!(err.message.contains("15"), "{}", err.message);
    // 이 시험의 kill 응답은 닫았다고 하지 않으므로 닫았다고 알리지 않는다.
    assert!(
        err.message.contains("could not be closed"),
        "{}",
        err.message
    );
    assert!(
        host.params_of("attach.agent_commit").is_empty(),
        "must not release notices"
    );
    let calls = host.params_of("attach.agent_call");
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1]["method"], json!("kill"));
    assert_eq!(calls[1]["target_surface_id"], json!(15));
    std::fs::set_permissions(&book, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// mirror 가 아닌 workspace 는 기존 로컬 spawn 그대로다.
#[test]
fn a_local_workspace_keeps_the_local_spawn() {
    let (_d, returns) = store();
    let host = Host::new(|method, _| match method {
        "attach.agent_target" => Ok(json!({ "remote": false })),
        "terminal.spawn" => Ok(json!({ "child_surface_id": 50, "child_index": 0 })),
        "hook.list" => Ok(json!([])),
        _ => Ok(json!({})),
    });
    let out = crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({ "workspace": "8" })),
        None,
        &tr(),
    )
    .unwrap();
    assert_eq!(out["child_surface_id"], json!(50));
    assert!(host.params_of("attach.agent_call").is_empty());
    assert!(host.methods().iter().any(|m| m == "terminal.spawn"));
}

/// 경로 먼저, 입력 나중. 원격 자식의 입력은 서버로 가고 이 Tasty 의 terminal.tell 은 쓰지 않는다.
#[test]
fn a_tell_to_a_mirror_child_goes_to_the_server_after_the_route() {
    let (_d, returns) = store();
    // 새로 만든, 아직 소유자가 없는 자식이다.
    tasty_plugin_agent_common::remote_owner::reset(&returns.owners_dir().unwrap(), 15, None)
        .unwrap();
    let host = client_host(true, || {
        Ok(json!({ "call_id": 8, "result": { "ok": true }, "children": [] }))
    });
    let params = json!({
        "surface": 15, "message": "next",
        "_tasty_context": { "provider": "codex", "thread_id": THREAD },
    });
    crate::handlers::handle_tell(&returns, &host, &params, &tr()).unwrap();
    let methods = host.methods();
    assert!(!methods.iter().any(|m| m == "terminal.tell"));
    let call = &host.params_of("attach.agent_call")[0];
    assert_eq!(call["method"], json!("tell"));
    assert_eq!(call["target_surface_id"], json!(15));
    assert_eq!(call["params"]["message"], json!("next"));
    let route = call["params"][RETURN_ROUTE_KEY]
        .as_str()
        .unwrap()
        .to_string();
    // 토큰이 저장돼 있어 이 지시의 결과가 이 대화로 간다.
    let r = returns
        .remote_receive(
            &host,
            &json!({
                "surface_id": 15, "key": "k",
                "payload": { "completion": { "completion_key": "claude:x:t2", "provider": "claude",
                    "child_surface": 99, "status": "turn_ended", "route": route } },
            }),
            &tr(),
        )
        .unwrap();
    assert_eq!(r["status"], json!("recorded"));
}

/// kill 뒤에도 서버에 남은 결과는 기록된다. 경로는 끝난 경로로만 남는다.
#[test]
fn a_killed_mirror_child_still_records_late_results() {
    let (_d, returns) = store();
    returns.bind_new_child(15, &local_binding(), false).unwrap();
    let route = returns
        .issue_attach_route(&RouteTo::Codex(RECEIVER.into()))
        .unwrap();
    let host = client_host(true, || {
        Ok(json!({ "call_id": 9, "result": { "killed": true }, "children": [] }))
    });
    let out = crate::handlers::handle_kill(
        &scanner(),
        &returns,
        &host,
        &json!({ "surface": 3, "child_index": 2 }),
        &tr(),
    )
    .unwrap();
    assert_eq!(out["killed"], json!(true));
    assert_eq!(out["killed_surface_id"], json!(15));
    assert!(!host.methods().iter().any(|m| m == "terminal.kill"));
    assert_eq!(
        host.params_of("attach.agent_call")[0]["method"],
        json!("kill")
    );
    let r = returns
        .remote_receive(
            &host,
            &json!({
                "surface_id": 15, "key": "k",
                "payload": { "completion": { "completion_key": "claude:x:t3", "provider": "claude",
                    "child_surface": 99, "status": "exited", "route": route } },
            }),
            &tr(),
        )
        .unwrap();
    assert_eq!(r["status"], json!("recorded"));
}

#[test]
fn a_result_without_a_route_is_rejected_and_a_failed_record_is_retried() {
    let (_d, returns) = store();
    let host = Host::new(|method, _| match method {
        "codex.deliver" => fail(-32000, "receiver offline"),
        _ => Ok(json!({})),
    });
    let payload = json!({ "completion": { "completion_key": "claude:x:t1", "provider": "claude",
        "child_surface": 1, "status": "turn_ended" } });
    let r = returns
        .remote_receive(
            &host,
            &json!({ "surface_id": 15, "key": "k", "payload": payload }),
            &tr(),
        )
        .unwrap();
    assert_eq!(r["status"], json!("rejected"));
    let route = returns
        .issue_attach_route(&RouteTo::Codex(RECEIVER.into()))
        .unwrap();
    let mut payload = payload;
    payload["completion"]["route"] = json!(route);
    assert!(
        returns
            .remote_receive(
                &host,
                &json!({ "surface_id": 15, "key": "k", "payload": payload }),
                &tr()
            )
            .is_err(),
        "a temporary failure must make the host retry"
    );
    let dup = Host::new(|method, _| match method {
        "codex.deliver" => {
            Ok(json!({ "event_id": "ev", "delivery_state": "sent", "duplicate": true }))
        }
        _ => Ok(json!({})),
    });
    let r = returns
        .remote_receive(
            &dup,
            &json!({ "surface_id": 15, "key": "k", "payload": payload }),
            &tr(),
        )
        .unwrap();
    assert_eq!(r["status"], json!("duplicate"));
}

/// 물리 부모 3 의 client host. 부모 세션과 존재 여부를 시험 중에 바꿀 수 있다.
fn surface_parent_host(
    session: std::rc::Rc<RefCell<&'static str>>,
    exists: std::rc::Rc<RefCell<bool>>,
) -> Host {
    Host::new(move |method, params| match method {
        "attach.agent_target" => Ok(json!({
            "remote": true, "capable": true, "workspace": 2, "anchor_surface_id": 12,
            "child": params.get("surface_id") == Some(&json!(15)),
        })),
        "attach.agent_call" => match params["method"].as_str() {
            Some("spawn") => spawned(),
            Some("kill") => {
                Ok(json!({ "call_id": 9, "result": { "killed": true }, "children": [] }))
            }
            _ => Ok(json!({ "call_id": 8, "result": { "ok": true }, "children": [] })),
        },
        "attach.agent_commit" => Ok(json!({ "committed": 1 })),
        "surface.locate" => Ok(json!({ "exists": *exists.borrow() })),
        "surface.meta.get"
            if params["surface_id"] == json!(3)
                && params["key"] == json!(remote_results::PARENT_SESSION_META) =>
        {
            match *session.borrow() {
                META_FAILS => fail(-32000, "meta store busy"),
                value => Ok(json!({ "value": value })),
            }
        }
        "terminal.children" => Ok(json!({ "children": [{ "surface_id": 15, "index": 2 }] })),
        "hook.list" => Ok(json!([])),
        _ => Ok(json!({})),
    })
}

/// `surface_parent_host` 의 세션 값이 이것이면 부모 세션 조회가 실패한다.
const META_FAILS: &str = "!meta-fails";

fn turn(key: &str, route: &str, body: &str) -> Value {
    json!({ "surface_id": 15, "remote_surface_id": 99, "key": key, "payload": { "completion": {
        "completion_key": key, "provider": "claude", "child_surface": 99, "child_session": "c",
        "status": "turn_ended", "body": body, "route": route } } })
}

fn last_route(host: &Host) -> String {
    host.params_of("attach.agent_call").last().unwrap()["params"][RETURN_ROUTE_KEY]
        .as_str()
        .unwrap()
        .to_string()
}

/// 물리 부모: spawn·tell 마다 받은 턴 본문을 저장한 뒤 부모 완료 로그에 결과 ID 와 조회 명령을 남긴다. 재전송은
/// 다시 알리지 않고, 자식을 닫은 뒤 온 결과도 저장한다. 조회 명령은 본문을 그대로 준다.
#[test]
fn a_surface_parent_gets_each_turn_body_stored_before_its_notice() {
    let (_d, returns) = store();
    crate::codex_return::test_log::take();
    let session = std::rc::Rc::new(RefCell::new("sess-a"));
    let exists = std::rc::Rc::new(RefCell::new(true));
    let host = surface_parent_host(session.clone(), exists.clone());
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap();
    let spawn_route = last_route(&host);
    let r = returns
        .remote_receive(&host, &turn("claude:a:t1", &spawn_route, "BODY-1"), &tr())
        .unwrap();
    assert_eq!(r["status"], json!("recorded"));
    let lines = crate::codex_return::test_log::take();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].0, 3);
    let id = lines[0].1.split_whitespace().last().unwrap().to_string();
    assert!(
        lines[0]
            .1
            .contains(&format!("tasty claude remote-results --id {id}")),
        "{}",
        lines[0].1
    );
    assert!(
        lines[0].1.starts_with("[info] surface 15: "),
        "{}",
        lines[0].1
    );
    let shown =
        crate::codex_return::handle_remote_results(&returns, &json!({ "id": id }), &tr()).unwrap();
    assert_eq!(shown["result"]["body"], json!("BODY-1"));
    assert_eq!(shown["result"]["parent_surface"], json!(3));
    // 재전송은 다시 알리지 않는다.
    let r = returns
        .remote_receive(&host, &turn("claude:a:t1", &spawn_route, "BODY-1"), &tr())
        .unwrap();
    assert_eq!(r["status"], json!("duplicate"));
    assert!(crate::codex_return::test_log::take().is_empty());

    // 같은 자식에게 보낸 tell 은 새 토큰이고, 그 턴은 별도 결과다.
    crate::handlers::handle_tell(
        &returns,
        &host,
        &json!({ "surface": 15, "message": "next", "caller_surface": 3 }),
        &tr(),
    )
    .unwrap();
    let tell_route = last_route(&host);
    assert_ne!(tell_route, spawn_route);
    // 자식을 닫은 뒤 늦게 온 결과도 저장하고 알린다.
    crate::handlers::handle_kill(
        &scanner(),
        &returns,
        &host,
        &json!({ "surface": 3, "child_index": 2 }),
        &tr(),
    )
    .unwrap();
    returns
        .remote_receive(&host, &turn("claude:a:t2", &tell_route, "BODY-2"), &tr())
        .unwrap();
    let lines = crate::codex_return::test_log::take();
    assert_eq!(lines.len(), 1);
    let id2 = lines[0].1.split_whitespace().last().unwrap().to_string();
    assert_ne!(id, id2);
    let list = crate::codex_return::handle_remote_results(&returns, &json!({}), &tr()).unwrap();
    assert_eq!(list["results"].as_array().unwrap().len(), 2);
    assert!(list["results"][0].get("body").is_none());
}

/// 부모 세션이 바뀌었거나 부모 surface 가 없으면 본문은 저장하되 알리지 않는다. 알림을 쓰지 못하면 오류로 답해
/// host 가 다시 보내게 하고, 다시 오면 알림만 한다.
#[test]
fn a_surface_parent_result_is_kept_quiet_for_another_session_and_retried_on_log_failure() {
    let (_d, returns) = store();
    crate::codex_return::test_log::take();
    let session = std::rc::Rc::new(RefCell::new("sess-a"));
    let exists = std::rc::Rc::new(RefCell::new(true));
    let host = surface_parent_host(session.clone(), exists.clone());
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap();
    let route = last_route(&host);

    crate::codex_return::test_log::fail(true);
    assert!(
        returns
            .remote_receive(&host, &turn("claude:q:t1", &route, "B1"), &tr())
            .is_err()
    );
    crate::codex_return::test_log::fail(false);
    let r = returns
        .remote_receive(&host, &turn("claude:q:t1", &route, "B1"), &tr())
        .unwrap();
    assert_eq!(r["status"], json!("recorded"));
    assert_eq!(crate::codex_return::test_log::take().len(), 1);

    *session.borrow_mut() = "sess-b";
    let r = returns
        .remote_receive(&host, &turn("claude:q:t2", &route, "B2"), &tr())
        .unwrap();
    assert_eq!(r["status"], json!("recorded"));
    *exists.borrow_mut() = false;
    returns
        .remote_receive(&host, &turn("claude:q:t3", &route, "B3"), &tr())
        .unwrap();
    assert!(crate::codex_return::test_log::take().is_empty());
    let list = crate::codex_return::handle_remote_results(&returns, &json!({}), &tr()).unwrap();
    let reasons: std::collections::BTreeMap<String, Value> = list["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["completion_key"].as_str().unwrap().to_string(),
                r["undelivered"].clone(),
            )
        })
        .collect();
    assert_eq!(reasons["r.claude:q:t1"], Value::Null);
    assert_eq!(reasons["r.claude:q:t2"], json!("parent_session_changed"));
    assert_eq!(reasons["r.claude:q:t3"], json!("parent_gone"));
}

/// 부모 세션 조회 실패는 세션 없음이 아니다. 발급 때는 원격 지시 전에 오류이고, 받을 때는 알림을 억제하지 않고
/// 오류로 답해 host 가 다시 보내게 한다.
#[test]
fn a_failed_parent_session_lookup_stops_the_order_and_retries_the_result() {
    let (_d, returns) = store();
    crate::codex_return::test_log::take();
    let session = std::rc::Rc::new(RefCell::new(META_FAILS));
    let exists = std::rc::Rc::new(RefCell::new(true));
    let host = surface_parent_host(session.clone(), exists);
    let spawned = crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    );
    assert!(spawned.is_err());
    assert!(host.params_of("attach.agent_call").is_empty());

    *session.borrow_mut() = "sess-a";
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap();
    let route = last_route(&host);
    let calls = host.params_of("attach.agent_call").len();
    *session.borrow_mut() = META_FAILS;
    let told = crate::handlers::handle_tell(
        &returns,
        &host,
        &json!({ "surface": 15, "message": "next", "caller_surface": 3 }),
        &tr(),
    );
    assert!(told.is_err());
    assert_eq!(host.params_of("attach.agent_call").len(), calls);

    assert!(
        returns
            .remote_receive(&host, &turn("claude:m:t1", &route, "B1"), &tr())
            .is_err()
    );
    assert!(crate::codex_return::test_log::take().is_empty());
    *session.borrow_mut() = "sess-a";
    let r = returns
        .remote_receive(&host, &turn("claude:m:t1", &route, "B1"), &tr())
        .unwrap();
    assert_eq!(r["status"], json!("recorded"));
    assert_eq!(crate::codex_return::test_log::take().len(), 1);
}

/// 물리 부모 둘이 같은 원격 자식에 지시한다. 자식을 만든 부모(surface 와 그때의 Claude 세션)만 지시할 수 있다. 다른
/// surface 나 같은 surface 의 다른 세션은 결과가 왔든 아니든 원격 호출 없이 거절된다.
#[test]
fn a_physical_parent_cannot_order_another_parents_remote_child() {
    let (_d, returns) = store();
    crate::codex_return::test_log::take();
    let session = std::rc::Rc::new(RefCell::new("sess-a"));
    let exists = std::rc::Rc::new(RefCell::new(true));
    let host = surface_parent_host(session.clone(), exists);
    let tell = |caller: u32| {
        crate::handlers::handle_tell(
            &returns,
            &host,
            &json!({ "surface": 15, "message": "m", "caller_surface": caller }),
            &tr(),
        )
    };
    let tells = || {
        host.params_of("attach.agent_call")
            .iter()
            .filter(|c| c["method"] == json!("tell"))
            .count()
    };
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap();
    let r1 = last_route(&host);
    let err = tell(4).unwrap_err();
    assert!(err.message.contains("15"), "{}", err.message);
    assert_eq!(tells(), 0);
    returns
        .remote_receive(&host, &turn("claude:o:t1", &r1, "B1"), &tr())
        .unwrap();
    assert!(tell(4).is_err());
    assert_eq!(tells(), 0);
    tell(3).unwrap();
    tell(3).unwrap();
    assert_eq!(tells(), 2);
    // 같은 surface 에서 새로 시작한 대화도 다른 부모다.
    *session.borrow_mut() = "sess-b";
    assert!(tell(3).is_err());
    assert_eq!(tells(), 2);
}

/// 소유자 기록이 없는 원격 자식(기록 이전에 만든 자식, 유실)은 첫 tell 이 가져가지 못한다. 원격 호출 없이 거절한다.
#[test]
fn a_remote_child_without_an_owner_record_is_refused() {
    let (_d, returns) = store();
    let session = std::rc::Rc::new(RefCell::new("sess-a"));
    let host = surface_parent_host(session, std::rc::Rc::new(RefCell::new(true)));
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap();
    std::fs::remove_file(returns.owners_dir().unwrap().join("15.json")).unwrap();
    for caller in [json!(3), json!(4), Value::Null] {
        let err = crate::handlers::handle_tell(
            &returns,
            &host,
            &json!({ "surface": 15, "message": "m", "caller_surface": caller }),
            &tr(),
        )
        .unwrap_err();
        assert!(err.message.contains("15"), "{}", err.message);
    }
    assert!(
        !host
            .params_of("attach.agent_call")
            .iter()
            .any(|c| c["method"] == json!("tell"))
    );
}

fn origin_params(extra: Value) -> Value {
    let mut p = json!({
        "parent_external": "attach:o3",
        "attach_context": "ctx",
        "workspace": "5",
        "pane": 6,
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

fn server_host(notice: &'static str) -> Host {
    Host::new(move |method, params| match method {
        // surface 40 은 attach 원격 부모의 자식이고 7 은 이 서버의 일반 자식이다.
        "terminal.parent" if params["surface"] == json!(40) => {
            Ok(json!({ "parent_external": "attach:o3", "status": "active" }))
        }
        "terminal.parent" => Ok(json!({ "parent_surface_id": 5, "status": "active" })),
        "terminal.spawn" => Ok(json!({ "child_surface_id": 40, "child_index": 0, "pane_id": 6 })),
        "attach.child_notice" => Ok(json!({ "status": notice })),
        "terminal.children" => Ok(json!({ "children": [{ "surface_id": 40, "index": 0 }] })),
        "terminal.kill" => Ok(json!({ "killed_surface_id": 40, "child_index": 0 })),
        _ => Ok(json!({})),
    })
}

/// 서버: 원격 출처 부모에게는 알림 훅·Codex 부모 해석을 쓰지 않고, 턴 결과는 attach.child_notice 로 넘긴다.
/// client 가 기록을 확인할 때까지 영속 요청을 지우지 않는다.
#[test]
fn the_server_runs_the_child_here_and_returns_turns_through_the_attach_parent() {
    let (d, returns) = store();
    let host = server_host("pending");
    // 서버는 실재하는 절대 cwd 만 받는다. OS 와 무관하게 실재하는 임시 폴더를 쓴다.
    let cwd = d.path().to_string_lossy().into_owned();
    let params = origin_params(json!({ "prompt": "p", "cwd": cwd, RETURN_ROUTE_KEY: ROUTE_A }));
    let out =
        crate::handlers::handle_spawn(&scanner(), &returns, &host, &params, None, &tr()).unwrap();
    assert_eq!(out["parent_external"], json!("attach:o3"));
    let spawn = &host.params_of("terminal.spawn")[0];
    for key in [
        "parent_external",
        "attach_context",
        "workspace",
        "pane",
        "cwd",
    ] {
        assert!(spawn.get(key).is_some(), "{key} missing in {spawn}");
    }
    let methods = host.methods();
    assert!(
        !methods
            .iter()
            .any(|m| m == "hook.set" || m == "codex.parent" || m.starts_with("attach.agent"))
    );

    returns.observe("session-start", 40, Some("sA"), None);
    returns.turn_end(TurnEnd {
        surface: 40,
        session: Some("sA"),
        status: ReturnStatus::TurnEnded,
        body: Some("done"),
        attempt_id: None,
        turn: None,
    });
    returns.pump_once(&host, 0);
    let notice = &host.params_of("attach.child_notice")[0];
    assert_eq!(notice["surface_id"], json!(40));
    let key = notice["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("claude:"), "{key}");
    assert_eq!(notice["payload"]["completion"]["body"], json!("done"));
    assert!(notice["payload"]["completion"].get("receiver_id").is_none());
    assert_eq!(notice["payload"]["completion"]["route"], json!(ROUTE_A));
    assert!(host.params_of("codex.deliver").is_empty());
    assert_eq!(
        returns.pending().unwrap().len(),
        1,
        "pending keeps the record"
    );

    // 같은 키로 다시 묻고, delivered 를 받은 뒤에야 지운다.
    let done = server_host("delivered");
    returns.pump_once(&done, u64::MAX / 2);
    assert_eq!(done.params_of("attach.child_notice")[0]["key"], json!(key));
    assert!(returns.pending().unwrap().is_empty());
}

/// 물리 부모의 원격 자식에는 턴 결과 경로를 두지 않는다(상태·이벤트는 host 가 중계한다).
#[test]
fn a_physical_attach_parent_gets_no_turn_results() {
    let (_d, returns) = store();
    let host = server_host("pending");
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &origin_params(json!({ "prompt": "p" })),
        None,
        &tr(),
    )
    .unwrap();
    returns.observe("session-start", 40, Some("sA"), None);
    returns.turn_end(TurnEnd {
        surface: 40,
        session: Some("sA"),
        status: ReturnStatus::TurnEnded,
        body: Some("done"),
        attempt_id: None,
        turn: None,
    });
    returns.pump_once(&host, 0);
    assert!(host.params_of("attach.child_notice").is_empty());
}

#[test]
fn the_server_tells_and_kills_only_the_child_the_host_named() {
    let (_d, returns) = store();
    let host = server_host("pending");
    let err = crate::handlers::handle_tell(
        &returns,
        &host,
        &origin_params(json!({ "message": "m" })),
        &tr(),
    )
    .unwrap_err();
    assert!(err.message.contains("did not name"), "{}", err.message);
    crate::handlers::handle_tell(
        &returns,
        &host,
        &origin_params(json!({ "message": "m", "attach_target_surface": 40 })),
        &tr(),
    )
    .unwrap();
    assert_eq!(
        host.params_of("terminal.tell")[0],
        json!({ "surface": 40, "text": "m" })
    );
    assert!(host.params_of("hook.set").is_empty());

    let out = crate::handlers::handle_kill(
        &scanner(),
        &returns,
        &host,
        &origin_params(json!({ "attach_target_surface": 40 })),
        &tr(),
    )
    .unwrap();
    assert_eq!(out["killed"], json!(true));
    let kill = &host.params_of("terminal.kill")[0];
    assert_eq!(kill["parent_external"], json!("attach:o3"));
    assert_eq!(kill["child"], json!(0));
}

/// 문맥 토큰 없이 `attach:` 부모만 온 호출은 원격 출처로 다루지 않는다.
#[test]
fn a_parent_key_without_the_host_context_is_not_an_attach_origin() {
    let (_d, returns) = store();
    let host = server_host("pending");
    let mut p = origin_params(json!({ "prompt": "p" }));
    p.as_object_mut().unwrap().remove("attach_context");
    let err =
        crate::handlers::handle_spawn(&scanner(), &returns, &host, &p, None, &tr()).unwrap_err();
    assert!(
        host.params_of("terminal.spawn").is_empty(),
        "{}",
        err.message
    );
}

/// 매니페스트가 원격 실행 메서드·수신 메서드와 부모 알림 문구가 읽는 메타를 선언한다.
#[test]
fn the_manifest_declares_the_remote_attach_contract() {
    let text = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasty-plugin.toml"),
    )
    .unwrap();
    let section = text
        .split("[contributes.remote_attach]")
        .nth(1)
        .expect("remote_attach section")
        .split("\n[")
        .next()
        .unwrap();
    assert!(
        section.contains(r#"methods = ["spawn", "tell", "kill"]"#),
        "{section}"
    );
    assert!(
        section.contains(r#"receive = "remote_receive""#),
        "{section}"
    );
    for key in [
        crate::hook::STOP_FAILURE_META_KEY,
        crate::hook::BACKGROUND_WAIT_META_KEY,
        crate::task_watch::BACKGROUND_QUIET_META_KEY,
    ] {
        assert!(section.contains(&format!("\"{key}\"")), "{key} not relayed");
    }
}

#[test]
fn the_call_number_is_read_from_the_unknown_outcome_message() {
    assert_eq!(call_id_in("call 42 outcome unknown"), Some(42));
    assert_eq!(call_id_in("remote call 7: reconnect failed"), Some(7));
    assert_eq!(call_id_in("outcome unknown"), None);
}

/// 결과불명이었던 spawn 의 자식이 늦게 생기면 그때의 부모로 경로를 묶고 recorded 로 답한다(host 가 commit).
#[test]
fn a_late_child_of_an_unknown_spawn_is_bound_to_its_waiting_parent() {
    let (_d, returns) = store();
    let host = client_host(true, || {
        fail(
            remote_attach::OUTCOME_UNKNOWN_CODE,
            "call 9101 outcome unknown",
        )
    });
    crate::handlers::handle_spawn(
        &scanner(),
        &returns,
        &host,
        &spawn_params(json!({})),
        None,
        &tr(),
    )
    .unwrap_err();
    let r = receive(
        &returns,
        &host,
        &json!({
            "kind": "late_spawn", "call_id": 9101, "result": { "child_surface_id": 99 }, "error": null,
            "children": [{ "surface_id": 15, "remote_surface_id": 99, "index": 2 }],
        }),
        &tr(),
    )
    .unwrap();
    assert_eq!(r, json!({ "status": "recorded" }));
    assert!(
        host.params_of("hook.set")
            .iter()
            .any(|p| p["surface_id"] == json!(15))
    );
    assert!(
        host.params_of("attach.agent_commit").is_empty(),
        "the host commits"
    );
}

/// 기다리는 부모가 없으면(재시작·만료) 늦은 자식을 닫고 거절한다.
#[test]
fn a_late_child_nobody_waits_for_is_closed_and_refused() {
    let (_d, returns) = store();
    let host = client_host(true, || {
        Ok(json!({ "call_id": 1, "result": { "killed": true }, "children": [] }))
    });
    let r = receive(
        &returns,
        &host,
        &json!({
            "kind": "late_spawn", "call_id": 9202, "result": null, "error": null,
            "children": [{ "surface_id": 15, "remote_surface_id": 99, "index": 2 }],
        }),
        &tr(),
    )
    .unwrap();
    assert_eq!(r["status"], json!("rejected"));
    let kill = &host.params_of("attach.agent_call")[0];
    assert_eq!(kill["method"], json!("kill"));
    assert_eq!(kill["target_surface_id"], json!(15));
    assert!(host.params_of("hook.set").is_empty());
}

/// 서버의 공개 호출(사용자 CLI 등)은 attach 원격 자식을 바꾸지 못한다. 출처 키를 꾸며 넣어도 그 부모의 자식이 아닌
/// surface 에는 닿지 않는다.
#[test]
fn server_local_public_calls_cannot_drive_attach_children() {
    let (_d, returns) = store();
    let host = server_host("pending");
    let err = refuse_public(
        &host,
        "tell",
        &json!({ "surface": 40, "message": "m" }),
        &tr(),
    )
    .unwrap_err();
    assert!(err.message.contains("40"), "{}", err.message);
    for m in ["kill", "respawn", "child_profile", "broadcast"] {
        assert!(
            refuse_public(
                &host,
                m,
                &json!({ "parent_external": "attach:o3", "child": 0 }),
                &tr()
            )
            .is_err(),
            "{m}"
        );
    }
    assert!(refuse_public(&host, "reboot", &json!({ "surface": 40 }), &tr()).is_err());
    assert!(
        refuse_public(
            &host,
            "tell",
            &json!({ "surface": 7, "message": "m" }),
            &tr()
        )
        .is_ok()
    );

    let forged = origin_params(json!({ "message": "m", "attach_target_surface": 7 }));
    let err = crate::handlers::handle_tell(&returns, &host, &forged, &tr()).unwrap_err();
    assert!(err.message.contains("did not name"), "{}", err.message);
    assert!(host.params_of("terminal.tell").is_empty());
}

/// client: 서버는 턴이 끝날 때 그 자식의 마지막 토큰을 싣는다. 그래서 원격 자식의 결과 소유자는 만든 부모로 고정하고,
/// 다른 부모(일반 셸·다른 Codex 대화)의 지시는 결과가 왔든 아니든 원격 호출 없이 거절한다. 같은 부모의 후속은 보낸다.
#[test]
fn a_remote_child_takes_orders_only_from_its_creating_parent() {
    const OTHER: &str = "0193a6f2-7c1e-7b3d-9a40-1f2e3d4c5b6b";
    const RECEIVER_B: &str = "fedcba9876543210fedcba9876543210";
    let (_d, returns) = store();
    let host = Host::new(|method, params| match method {
        "codex.parent" if params["_tasty_context"]["thread_id"] == json!(OTHER) => Ok(json!({
            "receiver_id": RECEIVER_B,
            "terminal_parent": { "parent_external": format!("codex:{OTHER}") },
        })),
        "attach.agent_call" => Ok(json!({ "call_id": 7, "result": {},
            "children": [{ "surface_id": 15, "remote_surface_id": 99, "index": 2 }] })),
        _ => client_host(true, spawned).call(method, params.clone()),
    });
    let mut params =
        spawn_params(json!({ "_tasty_context": { "provider": "codex", "thread_id": THREAD } }));
    params.as_object_mut().unwrap().remove("surface");
    crate::handlers::handle_spawn(&scanner(), &returns, &host, &params, None, &tr()).unwrap();
    let route_a = last_route(&host);
    let result = |key: &str, route: &str| {
        json!({ "surface_id": 15, "key": key, "payload": { "completion": {
            "completion_key": key, "provider": "claude", "child_surface": 99,
            "status": "turn_ended", "route": route } } })
    };
    let shell = json!({ "surface": 15, "message": "x" });
    let b = json!({ "surface": 15, "message": "y",
        "_tasty_context": { "provider": "codex", "thread_id": OTHER } });
    let refused = |p: &Value| {
        let calls = host.params_of("attach.agent_call").len();
        let saved = returns.child_routes(15).unwrap();
        let err = crate::handlers::handle_tell(&returns, &host, p, &tr()).unwrap_err();
        assert!(err.message.contains("15"), "{}", err.message);
        assert_eq!(host.params_of("attach.agent_call").len(), calls);
        assert_eq!(returns.child_routes(15).unwrap(), saved);
    };
    refused(&shell);
    refused(&b);
    returns
        .remote_receive(&host, &result("claude:r:t1", &route_a), &tr())
        .unwrap();
    // 결과가 온 뒤에도 다른 부모는 거절된다.
    refused(&shell);
    refused(&b);
    let a = json!({ "surface": 15, "message": "z",
        "_tasty_context": { "provider": "codex", "thread_id": THREAD } });
    crate::handlers::handle_tell(&returns, &host, &a, &tr()).unwrap();
    let route_a2 = last_route(&host);
    assert_ne!(route_a, route_a2);
    returns
        .remote_receive(&host, &result("claude:r:t2", &route_a2), &tr())
        .unwrap();
    let delivered = host.params_of("codex.deliver");
    let to: Vec<(&str, &str)> = delivered
        .iter()
        .map(|d| {
            (
                d["completion_key"].as_str().unwrap(),
                d["receiver_id"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        to,
        vec![("r.claude:r:t1", RECEIVER), ("r.claude:r:t2", RECEIVER)]
    );
}

/// client: 일반 tell 의 조회 실패는 Codex 부모의 경로를 바꾸기 전에 끝난다. 소유 부모의 tell 이 확정 실패하면 경로를
/// 되돌리고, 결과불명이면 서버에서 실행됐을 수 있으므로 새 경로를 그대로 둔다.
#[test]
fn a_failed_tell_leaves_the_codex_route_as_it_was() {
    let (_d, returns) = store();
    let mode = std::rc::Rc::new(RefCell::new(""));
    let m = mode.clone();
    let host = Host::new(move |method, params| match (method, *m.borrow()) {
        ("surface.meta.get", "meta") => fail(-32000, "meta store busy"),
        ("attach.agent_call", "refused") if params["method"] == json!("tell") => {
            fail(-32602, "refused by holder")
        }
        ("attach.agent_call", "unknown") if params["method"] == json!("tell") => fail(
            remote_attach::OUTCOME_UNKNOWN_CODE,
            "call 8 outcome unknown",
        ),
        ("attach.agent_call", _) => Ok(json!({ "call_id": 7, "result": {},
            "children": [{ "surface_id": 15, "remote_surface_id": 99, "index": 2 }] })),
        _ => client_host(true, spawned).call(method, params.clone()),
    });
    let mut params =
        spawn_params(json!({ "_tasty_context": { "provider": "codex", "thread_id": THREAD } }));
    params.as_object_mut().unwrap().remove("surface");
    crate::handlers::handle_spawn(&scanner(), &returns, &host, &params, None, &tr()).unwrap();
    // 끝난 경로로 둔다. 소유 부모의 tell 은 이 경로를 다시 열므로 되돌림을 관찰할 수 있다.
    returns.observe("session-start", 15, Some("sA"), None);
    returns.end_remote_child(15);
    let saved = returns.child_routes(15).unwrap();
    *mode.borrow_mut() = "meta";
    let plain = json!({ "surface": 15, "message": "x", "caller_surface": 3 });
    assert!(crate::handlers::handle_tell(&returns, &host, &plain, &tr()).is_err());
    assert_eq!(returns.child_routes(15).unwrap(), saved);
    let a = json!({ "surface": 15, "message": "y",
        "_tasty_context": { "provider": "codex", "thread_id": THREAD } });
    *mode.borrow_mut() = "refused";
    assert!(crate::handlers::handle_tell(&returns, &host, &a, &tr()).is_err());
    assert_eq!(returns.child_routes(15).unwrap(), saved);
    *mode.borrow_mut() = "unknown";
    assert!(crate::handlers::handle_tell(&returns, &host, &a, &tr()).is_err());
    assert_ne!(returns.child_routes(15).unwrap(), saved);
}

/// 서버: Stop 때 그 턴의 토큰을 고정한다. claim 뒤 다른 지시가 경로를 새 토큰으로 묶어도 앞 턴의 결과는 앞 토큰을 싣는다.
#[test]
fn the_server_keeps_the_route_token_of_the_turn() {
    let (_d, returns) = store();
    let host = server_host("pending");
    let params = origin_params(json!({ "prompt": "p", RETURN_ROUTE_KEY: ROUTE_A }));
    crate::handlers::handle_spawn(&scanner(), &returns, &host, &params, None, &tr()).unwrap();
    returns.observe("session-start", 40, Some("sA"), None);
    returns.observe("prompt-submit", 40, Some("sA"), None);
    returns.turn_end(TurnEnd {
        surface: 40,
        session: Some("sA"),
        status: ReturnStatus::TurnEnded,
        body: Some("a"),
        attempt_id: None,
        turn: None,
    });
    crate::handlers::handle_tell(
        &returns,
        &host,
        &origin_params(
            json!({ "message": "b", "attach_target_surface": 40, RETURN_ROUTE_KEY: ROUTE_B }),
        ),
        &tr(),
    )
    .unwrap();
    returns.pump_once(&host, 0);
    let notices = host.params_of("attach.child_notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["payload"]["completion"]["route"], json!(ROUTE_A));
}

/// 서버: client 가 영구히 받을 수 없다고 하면(rejected) 결과를 지우지 않고 사유와 함께 포기한 요청으로 남긴다.
#[test]
fn a_refused_attach_result_is_kept_as_given_up() {
    let (_d, returns) = store();
    let host = server_host("pending");
    let params = origin_params(json!({ "prompt": "p", RETURN_ROUTE_KEY: ROUTE_A }));
    crate::handlers::handle_spawn(&scanner(), &returns, &host, &params, None, &tr()).unwrap();
    returns.observe("session-start", 40, Some("sA"), None);
    returns.turn_end(TurnEnd {
        surface: 40,
        session: Some("sA"),
        status: ReturnStatus::TurnEnded,
        body: Some("done"),
        attempt_id: None,
        turn: None,
    });
    let refused = Host::new(|method, _| match method {
        "attach.child_notice" => Ok(json!({ "status": "rejected", "reason": "origin is gone" })),
        _ => Ok(json!({})),
    });
    returns.pump_once(&refused, 0);
    let pending = returns.pending().unwrap();
    assert_eq!(pending.len(), 1, "the result is kept");
    assert_eq!(pending[0]["gave_up"], json!(true));
    assert!(
        pending[0]["error"]
            .as_str()
            .unwrap()
            .contains("origin is gone"),
        "{}",
        pending[0]
    );
    returns.pump_once(&refused, u64::MAX / 2);
    assert_eq!(
        refused.params_of("attach.child_notice").len(),
        1,
        "not asked again"
    );
}
