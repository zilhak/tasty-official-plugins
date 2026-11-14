//! attach 원격 자식의 client·서버 분기. Codex 와 host 는 흉내 낸다.

use std::cell::RefCell;
use std::path::PathBuf;

use tasty_plugin_agent_common::remote_attach::NoticeStatus;
use tasty_plugin_sdk::PluginError;

use super::*;
use crate::delivery::attach_out;
use crate::delivery::testing::Fixture;

const PARENT: &str = "019f55e7-3dfa-7a00-8000-0000000000aa";
/// mirror 자식(L)과 서버 자식(R).
const L: u32 = 15;
const R: u32 = 99;
const ROUTE: &str = "0123456789abcdef0123456789abcdef";

type Answer = Box<dyn Fn(&str, &Value) -> Result<Value, PluginError>>;

/// 호출을 기록하고, 호출 순간의 L 반환 경로도 남긴다.
struct Host {
    root: PathBuf,
    seen: RefCell<Vec<(String, Value)>>,
    route_at_commit: RefCell<Vec<Option<ledger::ChildReturn>>>,
    answer: Answer,
}

impl HostCall for Host {
    fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        if method == "attach.agent_commit" {
            self.route_at_commit
                .borrow_mut()
                .push(ledger::child_receiver(&self.root, L).unwrap_or(None));
        }
        let out = (self.answer)(method, &params);
        self.seen.borrow_mut().push((method.to_string(), params));
        out
    }
}

impl Host {
    fn new(
        fx: &Fixture,
        answer: impl Fn(&str, &Value) -> Result<Value, PluginError> + 'static,
    ) -> Self {
        Self {
            root: fx.root().to_path_buf(),
            seen: RefCell::default(),
            route_at_commit: RefCell::default(),
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

/// mirror 를 가진 client host. 원격 spawn 은 L/R 자식 하나를 돌려준다.
fn client(method: &str, params: &Value) -> Result<Value, PluginError> {
    Ok(match method {
        "attach.agent_target" => {
            json!({ "remote": true, "capable": true, "anchor_surface_id": 12, "child": params.get("surface_id").is_some() })
        }
        "attach.agent_call" => json!({
            "call_id": 7,
            "result": { "child_surface_id": R },
            "children": [{ "surface_id": L, "remote_surface_id": R, "index": 1 }],
        }),
        "terminal.children" => json!({ "children": [{ "surface_id": L, "index": 1 }] }),
        _ => json!({}),
    })
}

/// PARENT 가 surface 5 의 Tasty helper 세션에서 실행 중이라 결과를 받을 수 있다.
fn owned_parent(fx: &Fixture) {
    let generation = fx.open(5, &[]);
    let host = Host::new(fx, client);
    fx.with(|d| {
        super::super::handle_hook(
            &host,
            &json!({"surface": 5, "event": "session-start", "session": PARENT, "gen": generation}),
            &fx.tr,
            d,
        )
    })
    .unwrap();
}

fn codex(mut params: Value) -> Value {
    params["_tasty_context"] = json!({ "provider": "codex", "thread_id": PARENT });
    params
}

fn spawn(fx: &Fixture, host: &Host, params: Value) -> Result<Value, IpcMethodError> {
    fx.with(|d| super::super::handle_spawn(host, &params, &fx.tr, d))
}

fn receiver_of_l(fx: &Fixture) -> String {
    ledger::child_receiver(fx.root(), L)
        .unwrap()
        .unwrap()
        .receiver_id
}

/// 마지막 원격 호출에 실린 route 토큰.
fn route_of(host: &Host) -> String {
    host.params_of("attach.agent_call")
        .iter()
        .rev()
        .find_map(|c| c["params"]["return_route"].as_str().map(str::to_string))
        .expect("a call with a route token")
}

fn notice(key: &str, route: &str) -> Value {
    json!({
        "kind": "notice",
        "surface_id": L,
        "key": key,
        "payload": { "completion": {
            "completion_key": key,
            "provider": "codex",
            "child_surface": R,
            "child_session": null,
            "status": "turn_ended",
            "body": "done",
            "attempt_id": null,
            "route": route,
        }},
    })
}

fn receive(fx: &Fixture, host: &Host, params: Value) -> Value {
    fx.with(|d| receive_fn(host, d, &params, &fx.tr)).unwrap()
}

use super::receive as receive_fn;

/// Codex 부모의 mirror spawn: 서버에서 실행하고 L 에 결과 경로를 묶은 뒤에 commit 한다. 이 Tasty 의 호출 문맥과
/// 부모·수신처는 서버로 보내지 않는다.
#[test]
fn a_codex_parent_spawns_on_the_server_and_binds_before_commit() {
    let fx = Fixture::new();
    owned_parent(&fx);
    let host = Host::new(&fx, client);
    let out = spawn(
        &fx,
        &host,
        codex(json!({ "workspace": "3", "prompt": "hi", "sandbox": "read-only", "cwd": "/srv/w" })),
    )
    .unwrap();
    assert_eq!(out["child_surface_id"], json!(L));
    assert_eq!(out["remote_child_surface_id"], json!(R));
    assert!(host.params_of("terminal.spawn").is_empty());
    assert!(host.params_of("surface.send").is_empty());

    let call = &host.params_of("attach.agent_call")[0];
    assert_eq!(call["method"], json!("spawn"));
    assert_eq!(call["parent"]["external"], json!(format!("codex:{PARENT}")));
    let sent = call["params"].as_object().unwrap();
    assert!(
        sent["return_route"]
            .as_str()
            .is_some_and(remote_attach::is_route_token)
    );
    assert_eq!(sent["sandbox"], json!("read-only"));
    assert_eq!(sent["cwd"], json!("/srv/w"));
    for key in ["_tasty_context", "workspace", "surface", "parent_external"] {
        assert!(!sent.contains_key(key), "{key} must not be forwarded");
    }
    let routes = host.route_at_commit.borrow();
    assert_eq!(routes.len(), 1);
    assert!(
        routes[0].is_some(),
        "route must exist when the notices are released"
    );
    let methods = host.methods();
    assert_eq!(
        methods.last().map(String::as_str),
        Some("attach.agent_commit")
    );
}

/// 물리 부모의 mirror spawn: 완료 알림 훅을 L 에 건 뒤 commit 한다. 턴 본문을 돌려받을 토큰만 서버로 보낸다.
#[test]
fn a_surface_parent_gets_its_notify_hooks_before_commit() {
    let fx = Fixture::new();
    let host = Host::new(&fx, client);
    spawn(&fx, &host, json!({ "workspace": 3, "surface": 5 })).unwrap();
    let call = &host.params_of("attach.agent_call")[0];
    assert_eq!(call["parent"]["surface"], json!(5));
    assert!(
        call["params"]["return_route"]
            .as_str()
            .is_some_and(remote_attach::is_route_token)
    );
    let methods = host.methods();
    let commit = methods
        .iter()
        .position(|m| m == "attach.agent_commit")
        .unwrap();
    assert!(
        methods[..commit].iter().any(|m| m == "hook.set"),
        "{methods:?}"
    );
    assert_eq!(ledger::child_receiver(fx.root(), L).unwrap(), None);
}

/// 물리 부모 5 가 살아 있고 Claude 세션 `sess-a` 를 쓰는 client host.
fn surface_parent(method: &str, params: &Value) -> Result<Value, PluginError> {
    match method {
        "surface.locate" => Ok(json!({ "exists": true })),
        "surface.meta.get" if params["surface_id"] == json!(5) => Ok(json!({ "value": "sess-a" })),
        _ => client(method, params),
    }
}

/// 물리 부모: spawn·tell 마다 받은 턴 본문을 원장 대신 결과 보관에 저장하고 부모 완료 로그에 결과 ID 와 조회 명령을
/// 남긴다. 재전송은 다시 알리지 않고, 조회 명령은 본문을 준다.
#[test]
fn a_surface_parent_gets_each_turn_body_stored_before_its_notice() {
    let fx = Fixture::new();
    test_log::take();
    let host = Host::new(&fx, surface_parent);
    spawn(&fx, &host, json!({ "workspace": 3, "surface": 5 })).unwrap();
    let spawn_route = route_of(&host);
    assert_eq!(
        receive(&fx, &host, notice("codex:a:t1", &spawn_route))["status"],
        json!("recorded")
    );
    assert!(host.params_of("codex.deliver").is_empty());
    let lines = test_log::take();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].0, 5);
    assert!(lines[0].1.starts_with("[info] surface "), "{}", lines[0].1);
    let id = lines[0].1.split_whitespace().last().unwrap().to_string();
    assert!(
        lines[0]
            .1
            .contains(&format!("tasty codex remote-results --id {id}")),
        "{}",
        lines[0].1
    );
    let shown = fx
        .with(|d| super::results(d, &json!({ "id": id }), &fx.tr))
        .unwrap();
    assert_eq!(shown["result"]["body"], json!("done"));
    assert_eq!(shown["result"]["provider"], json!("codex"));
    assert_eq!(
        receive(&fx, &host, notice("codex:a:t1", &spawn_route))["status"],
        json!("duplicate")
    );
    assert!(test_log::take().is_empty());

    fx.with(|d| {
        super::super::handle_tell(
            &host,
            &json!({ "surface": L, "message": "next", "caller_surface": 5 }),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    let tell_route = route_of(&host);
    assert_ne!(tell_route, spawn_route);
    assert_eq!(
        receive(&fx, &host, notice("codex:a:t2", &tell_route))["status"],
        json!("recorded")
    );
    let lines = test_log::take();
    assert_eq!(lines.len(), 1);
    assert!(!lines[0].1.contains(&id));
    let bad = fx
        .with(|d| super::results(d, &json!({ "id": "nope" }), &fx.tr))
        .unwrap_err();
    assert_eq!(bad.code, -32602);
}

/// 부모 세션이 바뀌었으면 본문은 저장하되 알리지 않는다.
#[test]
fn a_surface_parent_result_for_another_session_is_kept_quiet() {
    let fx = Fixture::new();
    test_log::take();
    let host = Host::new(&fx, surface_parent);
    spawn(&fx, &host, json!({ "workspace": 3, "surface": 5 })).unwrap();
    let route = route_of(&host);
    let other = Host::new(&fx, |method, params| match method {
        "surface.meta.get" => Ok(json!({ "value": "sess-b" })),
        _ => surface_parent(method, params),
    });
    assert_eq!(
        receive(&fx, &other, notice("codex:b:t1", &route))["status"],
        json!("recorded")
    );
    assert!(test_log::take().is_empty());
    let list = fx.with(|d| super::results(d, &json!({}), &fx.tr)).unwrap();
    assert_eq!(
        list["results"][0]["undelivered"],
        json!("parent_session_changed")
    );
}

/// 부모 세션 조회 실패는 세션 없음이 아니다. 발급 때는 원격 지시 전에 오류이고, 받을 때는 알림을 억제하지 않고
/// 오류로 답해 host 가 다시 보내게 한다.
#[test]
fn a_failed_parent_session_lookup_stops_the_order_and_retries_the_result() {
    let fx = Fixture::new();
    test_log::take();
    let broken = Host::new(&fx, |method, params| match method {
        "surface.meta.get" => fail(-32000, "meta store busy"),
        _ => surface_parent(method, params),
    });
    assert!(spawn(&fx, &broken, json!({ "workspace": 3, "surface": 5 })).is_err());
    assert!(broken.params_of("attach.agent_call").is_empty());

    let host = Host::new(&fx, surface_parent);
    spawn(&fx, &host, json!({ "workspace": 3, "surface": 5 })).unwrap();
    let route = route_of(&host);
    let told = fx.with(|d| {
        super::super::handle_tell(
            &broken,
            &json!({ "surface": L, "message": "next", "caller_surface": 5 }),
            &fx.tr,
            d,
        )
    });
    assert!(told.is_err());
    assert!(broken.params_of("attach.agent_call").is_empty());

    let failed = fx.with(|d| receive_fn(&broken, d, &notice("codex:m:t1", &route), &fx.tr));
    assert!(failed.is_err());
    assert!(test_log::take().is_empty());
    assert_eq!(
        receive(&fx, &host, notice("codex:m:t1", &route))["status"],
        json!("recorded")
    );
    assert_eq!(test_log::take().len(), 1);
}

/// 새로 만든, 아직 소유자가 없는 원격 자식 L 의 기록.
fn unowned_child(fx: &Fixture) {
    tasty_plugin_agent_common::remote_owner::reset(
        &fx.root().join(tasty_plugin_agent_common::remote_owner::DIR),
        L,
        None,
    )
    .unwrap();
}

/// 소유자 기록이 없는 원격 자식(기록 이전에 만든 자식, 유실)은 첫 tell 이 가져가지 못한다. 원격 호출 없이 거절한다.
#[test]
fn a_remote_child_without_an_owner_record_is_refused() {
    let fx = Fixture::new();
    let host = Host::new(&fx, surface_parent);
    spawn(&fx, &host, json!({ "workspace": 3, "surface": 5 })).unwrap();
    std::fs::remove_file(
        fx.root()
            .join(tasty_plugin_agent_common::remote_owner::DIR)
            .join(format!("{L}.json")),
    )
    .unwrap();
    for caller in [json!(5), json!(6), Value::Null] {
        let err = fx
            .with(|d| {
                super::super::handle_tell(
                    &host,
                    &json!({ "surface": L, "message": "m", "caller_surface": caller }),
                    &fx.tr,
                    d,
                )
            })
            .unwrap_err();
        assert!(err.message.contains(&L.to_string()), "{}", err.message);
    }
    assert!(
        !host
            .params_of("attach.agent_call")
            .iter()
            .any(|c| c["method"] == json!("tell"))
    );
}

/// 물리 부모 둘이 같은 원격 자식에 지시한다. 자식을 만든 부모만 지시할 수 있고, 다른 surface 는 결과가 왔든 아니든
/// 원격 호출 없이 거절된다.
#[test]
fn a_physical_parent_cannot_order_another_parents_remote_child() {
    let fx = Fixture::new();
    test_log::take();
    let host = Host::new(&fx, surface_parent);
    let tell = |caller: u32| {
        fx.with(|d| {
            super::super::handle_tell(
                &host,
                &json!({ "surface": L, "message": "m", "caller_surface": caller }),
                &fx.tr,
                d,
            )
        })
    };
    let tells = || {
        host.params_of("attach.agent_call")
            .iter()
            .filter(|c| c["method"] == json!("tell"))
            .count()
    };
    spawn(
        &fx,
        &host,
        json!({ "workspace": 3, "surface": 5, "prompt": "go" }),
    )
    .unwrap();
    let r1 = route_of(&host);
    assert!(tell(6).is_err());
    assert_eq!(tells(), 0);
    receive(&fx, &host, notice("codex:o:t1", &r1));
    assert!(tell(6).is_err());
    assert_eq!(tells(), 0);
    tell(5).unwrap();
    tell(5).unwrap();
    assert_eq!(tells(), 2);
}

/// 일반 tell 의 조회 실패는 Codex 부모의 경로를 바꾸기 전에 끝난다.
#[test]
fn a_failed_parent_lookup_leaves_the_codex_route_as_it_was() {
    let fx = Fixture::new();
    owned_parent(&fx);
    let host = Host::new(&fx, client);
    spawn(&fx, &host, codex(json!({ "workspace": 3 }))).unwrap();
    let before = ledger::child_receiver(fx.root(), L).unwrap();
    assert!(before.is_some());
    let broken = Host::new(&fx, |method, params| match method {
        "surface.meta.get" => fail(-32000, "meta store busy"),
        _ => client(method, params),
    });
    let told = fx.with(|d| {
        super::super::handle_tell(
            &broken,
            &json!({ "surface": L, "message": "plain", "caller_surface": 5 }),
            &fx.tr,
            d,
        )
    });
    assert!(told.is_err());
    assert_eq!(ledger::child_receiver(fx.root(), L).unwrap(), before);
    assert!(broken.params_of("attach.agent_call").is_empty());
}

/// 경로를 묶지 못하면 commit 하지 않고(첫 완료를 풀지 않는다) 서버의 자식을 닫는다.
#[test]
fn a_child_whose_route_cannot_be_bound_is_not_committed_and_is_closed() {
    let fx = Fixture::new();
    owned_parent(&fx);
    std::fs::write(fx.root().join("children"), "not a directory").unwrap();
    let host = Host::new(&fx, client);
    let err = spawn(&fx, &host, codex(json!({ "workspace": 3 }))).unwrap_err();
    assert!(err.message.contains(&L.to_string()), "{}", err.message);
    assert!(host.params_of("attach.agent_commit").is_empty());
    let calls = host.params_of("attach.agent_call");
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1]["method"], json!("kill"));
    assert_eq!(calls[1]["target_surface_id"], json!(L));
}

/// 원격 결과는 자기 namespace 를 다시 부르지 않고 L 경로의 수신처 원장에 직접 기록한다. 재전송은 같은 키라
/// duplicate 이고, 경로가 없으면 rejected 다.
#[test]
fn remote_results_are_recorded_once_without_calling_codex_deliver() {
    let fx = Fixture::new();
    owned_parent(&fx);
    let host = Host::new(&fx, client);
    spawn(&fx, &host, codex(json!({ "workspace": 3 }))).unwrap();
    let receiver = receiver_of_l(&fx);
    let route = route_of(&host);

    let first = receive(&fx, &host, notice("k1", &route));
    assert_eq!(first["status"], json!("recorded"), "{first}");
    assert_eq!(
        receive(&fx, &host, notice("k1", &route))["status"],
        json!("duplicate")
    );
    assert!(!host.methods().iter().any(|m| m.starts_with("codex.")));
    let thread = ledger::read_receiver(fx.root(), &receiver)
        .unwrap()
        .unwrap()
        .thread;
    let ids = ledger::pending_ids(fx.root(), &thread);
    assert_eq!(ids.len(), 1);
    let event = ledger::read_event(fx.root(), &ids[0]).unwrap().unwrap();
    assert_eq!(event.completion_key, "r.k1");
    assert_eq!(event.child_surface, L);

    let unknown = notice("k2", "ffffffffffffffffffffffffffffffff");
    assert_eq!(receive(&fx, &host, unknown)["status"], json!("rejected"));
    assert_eq!(
        receive(&fx, &host, notice("k3", ""))["status"],
        json!("rejected")
    );
}

/// 원격 kill 은 서버로 보내고, 경로는 끝난 기록으로 남겨 늦은 결과를 받는다.
#[test]
fn a_killed_remote_child_still_records_a_late_result() {
    let fx = Fixture::new();
    owned_parent(&fx);
    let host = Host::new(&fx, |m, p| match m {
        "attach.agent_call" if p["method"] == json!("kill") => {
            Ok(json!({ "call_id": 8, "result": { "killed_surface_id": R } }))
        }
        _ => client(m, p),
    });
    spawn(&fx, &host, codex(json!({ "workspace": 3 }))).unwrap();
    let out = fx
        .with(|d| super::super::handle_kill(&host, &codex(json!({ "child": 1 })), &fx.tr, d))
        .unwrap();
    assert_eq!(out["killed_surface_id"], json!(L));
    assert!(host.params_of("terminal.kill").is_empty());
    let kill = host.params_of("attach.agent_call").pop().unwrap();
    assert_eq!(kill["target_surface_id"], json!(L));
    let record = ledger::child_receiver(fx.root(), L).unwrap().unwrap();
    assert!(record.ended_at_ms.is_some());
    assert_eq!(
        receive(&fx, &host, notice("late", &route_of(&host)))["status"],
        json!("recorded")
    );
}

/// 원격 tell 은 결과 경로를 먼저 정하고 입력은 서버로 보낸다.
#[test]
fn a_remote_tell_binds_the_route_then_sends_to_the_server() {
    let fx = Fixture::new();
    owned_parent(&fx);
    unowned_child(&fx);
    let host = Host::new(&fx, |m, p| match m {
        "terminal.parent" => {
            Ok(json!({ "parent_external": format!("codex:{PARENT}"), "status": "active" }))
        }
        _ => client(m, p),
    });
    let out = fx
        .with(|d| {
            super::super::handle_tell(
                &host,
                &codex(json!({ "surface": L, "message": "go" })),
                &fx.tr,
                d,
            )
        })
        .unwrap();
    assert_eq!(out["remote"], json!(true));
    assert!(host.params_of("terminal.tell").is_empty());
    let call = host.params_of("attach.agent_call").pop().unwrap();
    assert_eq!(call["method"], json!("tell"));
    assert!(call["params"]["return_route"].is_string());
    assert!(ledger::child_receiver(fx.root(), L).unwrap().is_some());
}

/// 결과불명 spawn 뒤 늦게 생긴 자식은 기다리던 부모로 묶는다. 기다리는 부모가 없으면 닫고 거절한다.
#[test]
fn a_late_child_is_bound_to_the_waiting_parent_or_closed() {
    let fx = Fixture::new();
    owned_parent(&fx);
    let host = Host::new(&fx, |m, p| match m {
        "attach.agent_call" if p["method"] == json!("spawn") => {
            fail(-32010, "outcome unknown for call 4242")
        }
        _ => client(m, p),
    });
    let err = spawn(&fx, &host, codex(json!({ "workspace": 3 }))).unwrap_err();
    assert!(err.message.contains("not known"), "{}", err.message);
    let late = json!({
        "kind": "late_spawn",
        "call_id": 4242,
        "children": [{ "surface_id": L, "remote_surface_id": R, "index": 1 }],
        "result": {},
        "error": null,
    });
    assert_eq!(
        receive(&fx, &host, late.clone())["status"],
        json!("recorded")
    );
    assert!(ledger::child_receiver(fx.root(), L).unwrap().is_some());

    // 같은 call 을 다시 받으면 기다리는 부모가 없으므로 닫는다.
    assert_eq!(receive(&fx, &host, late)["status"], json!("rejected"));
    let kill = host.params_of("attach.agent_call").pop().unwrap();
    assert_eq!(kill["method"], json!("kill"));
}

fn origin(extra: Value) -> Value {
    let mut p = json!({
        "parent_external": "attach:o3",
        "attach_context": "opaque",
        "workspace": "2",
        "return_route": "0123456789abcdef0123456789abcdef",
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

fn server(method: &str, params: &Value) -> Result<Value, PluginError> {
    Ok(match method {
        // R 은 attach 원격 부모의 자식이고 그 밖은 이 서버의 일반 자식이다.
        "terminal.parent" if params["surface"] == json!(R) => {
            json!({ "parent_external": "attach:o3", "status": "active" })
        }
        "terminal.parent" => json!({ "parent_surface_id": 5, "status": "active" }),
        "terminal.spawn" => json!({ "child_surface_id": R, "child_index": 1 }),
        "terminal.children" => json!({ "children": [{ "surface_id": R, "index": 1 }] }),
        "terminal.kill" => json!({ "killed_surface_id": R }),
        _ => json!({}),
    })
}

/// 서버: 원격 출처 부모의 자식은 서버 설정으로 실행하고, 알림 훅 없이 결과를 attach 부모로 돌려보낸다.
#[test]
fn the_server_runs_the_child_and_routes_its_turns_to_the_attach_parent() {
    let fx = Fixture::new();
    let host = Host::new(&fx, server);
    let out = spawn(&fx, &host, origin(json!({ "prompt": "hi" }))).unwrap();
    assert_eq!(out["parent_external"], json!("attach:o3"));
    let sp = &host.params_of("terminal.spawn")[0];
    assert_eq!(sp["parent_external"], json!("attach:o3"));
    assert_eq!(sp["attach_context"], json!("opaque"));
    assert_eq!(host.params_of("surface.send").len(), 1);
    assert!(
        !host.methods().iter().any(|m| m.contains("hook")),
        "{:?}",
        host.methods()
    );
    let record = ledger::child_receiver(fx.root(), R).unwrap().unwrap();
    assert_eq!(record.attach_parent.as_deref(), Some("attach:o3"));
    assert_eq!(
        record.receiver_id, "0123456789abcdef0123456789abcdef",
        "the server keeps only the client's route token, not a receiver"
    );
}

/// 서버: tell·kill 은 host 가 지정한 자식에만 간다.
#[test]
fn the_server_only_acts_on_the_named_child() {
    let fx = Fixture::new();
    let host = Host::new(&fx, server);
    let err = fx
        .with(|d| super::super::handle_tell(&host, &origin(json!({ "message": "go" })), &fx.tr, d))
        .unwrap_err();
    assert!(err.message.contains("did not name"), "{}", err.message);
    fx.with(|d| {
        super::super::handle_tell(
            &host,
            &origin(json!({ "message": "go", "attach_target_surface": R })),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    assert_eq!(host.params_of("terminal.tell")[0]["surface"], json!(R));
    fx.with(|d| {
        super::super::handle_kill(
            &host,
            &origin(json!({ "attach_target_surface": R })),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    assert_eq!(host.params_of("terminal.kill")[0]["child"], json!(1));
}

fn completion() -> crate::delivery::ledger::Completion {
    crate::delivery::ledger::Completion {
        completion_key: "turn-1".into(),
        provider: "codex".into(),
        child_surface: R,
        child_session: None,
        status: "turn_ended".into(),
        body: Some("done".into()),
        attempt_id: None,
    }
}

/// 서버: 맡긴 결과는 pending 이면 남기고 다시 묻는다. delivered·rejected 를 받은 뒤에만 지운다.
#[test]
fn an_attach_result_is_kept_until_the_client_settles_it() {
    let clock = crate::store::test_clock::freeze();
    let retry_after = crate::delivery::backoff(1);
    let fx = Fixture::new();
    let name = "a".repeat(64);
    attach_out::queue(
        fx.root(),
        &name,
        &completion(),
        "0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    let next = fx.with(attach_out::pump);
    assert!(next.is_some());
    assert_eq!(attach_out::pending(fx.root()), 1);
    let (surface, key, payload) = fx.rec.notices.borrow()[0].clone();
    assert_eq!((surface, key.as_str()), (R, "turn-1"));
    assert_eq!(payload["completion"]["body"], json!("done"));
    // 다음 시각 전에는 다시 묻지 않는다.
    fx.with(attach_out::pump);
    assert_eq!(fx.rec.notices.borrow().len(), 1);

    let name2 = "b".repeat(64);
    attach_out::queue(
        fx.root(),
        &name2,
        &completion(),
        "0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    *fx.rec.notice_status.borrow_mut() = Some(NoticeStatus::Delivered);
    fx.with(attach_out::pump);
    assert_eq!(
        attach_out::pending(fx.root()),
        1,
        "only the delivered one is gone"
    );

    let name3 = "c".repeat(64);
    attach_out::queue(
        fx.root(),
        &name3,
        &completion(),
        "0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    *fx.rec.notice_status.borrow_mut() = Some(NoticeStatus::Rejected("no route".into()));
    fx.with(attach_out::pump);
    // 영구 거절은 지우지 않고 포기한 기록으로 남기며 더 묻지 않는다.
    assert_eq!(attach_out::pending(fx.root()), 2);
    assert_eq!(attach_out::given_up(fx.root()), 1);
    let asked = fx.rec.notices.borrow().len();
    clock.advance(retry_after - std::time::Duration::from_millis(1));
    fx.with(attach_out::pump);
    assert_eq!(
        fx.rec.notices.borrow().len(),
        asked,
        "the waiting record is not asked before its time"
    );
    clock.advance(std::time::Duration::from_millis(1));
    fx.with(attach_out::pump);
    assert_eq!(
        fx.rec.notices.borrow().len(),
        asked + 1,
        "only the record still waiting is asked again"
    );
}

/// 첫 실패부터 자동 재시도 기한이 지나면 결과를 지우지 않고 포기한 기록으로 남긴다.
#[test]
fn an_attach_result_past_the_retry_window_is_kept_as_given_up() {
    let fx = Fixture::new();
    attach_out::queue(fx.root(), &"d".repeat(64), &completion(), ROUTE).unwrap();
    fx.with(attach_out::pump);
    assert_eq!(attach_out::given_up(fx.root()), 0);
    attach_out::age_failures(fx.root(), attach_out::AUTO_RETRY_WINDOW.as_millis() as u64);
    let next = fx.with(attach_out::pump);
    assert_eq!(attach_out::pending(fx.root()), 1, "the result is kept");
    assert_eq!(attach_out::given_up(fx.root()), 1);
    assert_eq!(next, None, "nothing left to retry");
    let asked = fx.rec.notices.borrow().len();
    fx.with(attach_out::pump);
    assert_eq!(fx.rec.notices.borrow().len(), asked);
}

/// 두 provider 모두 같은 형태로 원격 attach 를 선언한다.
#[test]
fn the_manifest_declares_remote_attach() {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasty-plugin.toml"),
    )
    .unwrap();
    let manifest: toml::Value = toml::from_str(&text).unwrap();
    let ra = &manifest["contributes"]["remote_attach"];
    assert_eq!(
        ra["methods"].as_array().unwrap(),
        &vec![
            toml::Value::from("spawn"),
            toml::Value::from("tell"),
            toml::Value::from("kill")
        ]
    );
    assert_eq!(ra["receive"].as_str(), Some("remote_receive"));
    assert!(ra.get("relay_meta").is_none());
}

#[test]
fn call_id_is_read_from_the_unknown_outcome_message() {
    assert_eq!(
        call_id_in("outcome unknown for call 4242 (timeout)"),
        Some(4242)
    );
    assert_eq!(call_id_in("no id here"), None);
}

/// 서버의 공개 호출(사용자 CLI 등)은 attach 원격 자식을 바꾸지 못한다. 출처 키를 꾸며 넣어도 그 부모의 자식이 아닌
/// surface 에는 닿지 않는다.
#[test]
fn server_local_public_calls_cannot_drive_attach_children() {
    let fx = Fixture::new();
    let host = Host::new(&fx, server);
    let err = refuse_public(
        &host,
        "tell",
        &json!({ "surface": R, "message": "m" }),
        &fx.tr,
    )
    .unwrap_err();
    assert!(err.message.contains(&R.to_string()), "{}", err.message);
    for m in ["kill", "respawn", "broadcast"] {
        assert!(
            refuse_public(
                &host,
                m,
                &json!({ "parent_external": "attach:o3", "child": 1 }),
                &fx.tr
            )
            .is_err(),
            "{m}"
        );
    }
    assert!(refuse_public(&host, "reboot", &json!({ "surface": R }), &fx.tr).is_err());
    assert!(
        refuse_public(
            &host,
            "tell",
            &json!({ "surface": 7, "message": "m" }),
            &fx.tr
        )
        .is_ok()
    );

    let forged = origin(json!({ "message": "m", "attach_target_surface": 7 }));
    let err = fx
        .with(|d| super::super::handle_tell(&host, &forged, &fx.tr, d))
        .unwrap_err();
    assert!(err.message.contains("did not name"), "{}", err.message);
    assert!(host.params_of("terminal.tell").is_empty());
}

/// 서버는 턴이 끝날 때 그 자식의 마지막 토큰을 싣는다. 그래서 원격 자식의 결과 소유자는 만든 부모로 고정하고, 다른
/// 부모(일반 셸·호스트가 자식으로 넘겨받은 다른 Codex 대화)의 지시는 원격 호출·경로 변경 없이 거절한다. 같은 부모의
/// 후속은 보내고 그 결과는 지시한 부모에게 간다.
#[test]
fn a_remote_child_takes_orders_only_from_its_creating_parent() {
    const OTHER: &str = "019f55e7-3dfa-7a00-8000-0000000000bb";
    let fx = Fixture::new();
    owned_parent(&fx);
    let generation = fx.open(6, &[]);
    let parent_now = std::rc::Rc::new(RefCell::new(format!("codex:{PARENT}")));
    let current = parent_now.clone();
    let host = Host::new(&fx, move |m, p| match m {
        "terminal.parent" => {
            Ok(json!({ "parent_external": *current.borrow(), "status": "active" }))
        }
        _ => client(m, p),
    });
    fx.with(|d| {
        super::super::handle_hook(
            &host,
            &json!({"surface": 6, "event": "session-start", "session": OTHER, "gen": generation}),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    spawn(&fx, &host, codex(json!({ "workspace": 3 }))).unwrap();
    let route_a = route_of(&host);
    let receiver_a = receiver_of_l(&fx);
    let refused = |p: &Value| {
        let calls = host.params_of("attach.agent_call").len();
        let before = ledger::child_receiver(fx.root(), L).unwrap();
        let err = fx
            .with(|d| super::super::handle_tell(&host, p, &fx.tr, d))
            .unwrap_err();
        assert!(err.message.contains(&L.to_string()), "{}", err.message);
        assert_eq!(host.params_of("attach.agent_call").len(), calls);
        assert_eq!(ledger::child_receiver(fx.root(), L).unwrap(), before);
    };
    refused(&json!({ "surface": L, "message": "plain" }));
    assert_eq!(
        receive(&fx, &host, notice("a1", &route_a))["status"],
        json!("recorded")
    );
    // 결과가 온 뒤에도, 호스트가 B 를 부모로 답해도 B 는 거절된다.
    *parent_now.borrow_mut() = format!("codex:{OTHER}");
    let mut b = json!({ "surface": L, "message": "b" });
    b["_tasty_context"] = json!({ "provider": "codex", "thread_id": OTHER });
    refused(&b);
    *parent_now.borrow_mut() = format!("codex:{PARENT}");
    fx.with(|d| {
        super::super::handle_tell(
            &host,
            &codex(json!({ "surface": L, "message": "a" })),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    let route_a2 = route_of(&host);
    assert_ne!(route_a, route_a2);
    assert_eq!(
        receive(&fx, &host, notice("a2", &route_a2))["status"],
        json!("recorded")
    );
    let thread = ledger::read_receiver(fx.root(), &receiver_a)
        .unwrap()
        .unwrap()
        .thread;
    let keys: Vec<String> = ledger::pending_ids(fx.root(), &thread)
        .iter()
        .map(|id| {
            ledger::read_event(fx.root(), id)
                .unwrap()
                .unwrap()
                .completion_key
        })
        .collect();
    assert_eq!(keys, vec!["r.a1".to_string(), "r.a2".to_string()]);
}

/// 서버: 원격 부모가 준 경로는 서버 기준 절대 경로여야 하고 서버에 있어야 한다. 터미널을 만들기 전에 거절한다.
#[test]
fn the_server_takes_only_absolute_existing_paths() {
    let fx = Fixture::new();
    let host = Host::new(&fx, server);
    let err = spawn(&fx, &host, origin(json!({ "cwd": "work" }))).unwrap_err();
    assert!(
        err.message.contains("--cwd work is not an absolute path"),
        "{}",
        err.message
    );
    // 절대 경로이지만 없는 폴더. OS마다 절대 경로 형식이 달라 픽스처 폴더 아래에서 만든다.
    let missing = fx.root().join("missing").join("tasty-remote-cwd");
    let err = spawn(
        &fx,
        &host,
        origin(json!({ "cwd": missing.to_string_lossy() })),
    )
    .unwrap_err();
    assert!(
        err.message.contains("does not exist on the remote"),
        "{}",
        err.message
    );
    assert!(host.params_of("terminal.spawn").is_empty());
    let cwd = fx.root().to_string_lossy().to_string();
    spawn(&fx, &host, origin(json!({ "cwd": cwd }))).unwrap();
    assert_eq!(host.params_of("terminal.spawn")[0]["cwd"], json!(cwd));
}

/// 결과불명 tell 은 서버에서 실행됐을 수 있으므로 새 경로와 토큰을 되돌리지 않는다. 확정된 실패만 되돌린다.
#[test]
fn an_unknown_remote_tell_keeps_the_new_route() {
    let fx = Fixture::new();
    owned_parent(&fx);
    unowned_child(&fx);
    let unknown = std::rc::Rc::new(std::cell::Cell::new(true));
    let flag = unknown.clone();
    let host = Host::new(&fx, move |m, p| match m {
        "terminal.parent" => {
            Ok(json!({ "parent_external": format!("codex:{PARENT}"), "status": "active" }))
        }
        "attach.agent_call" if p["method"] == json!("tell") => {
            if flag.get() {
                fail(-32010, "outcome unknown for call 5")
            } else {
                fail(-32000, "refused")
            }
        }
        _ => client(m, p),
    });
    let tell = || {
        fx.with(|d| {
            super::super::handle_tell(
                &host,
                &codex(json!({ "surface": L, "message": "go" })),
                &fx.tr,
                d,
            )
        })
    };
    let err = tell().unwrap_err();
    assert!(err.message.contains("not known"), "{}", err.message);
    let kept = ledger::child_receiver(fx.root(), L).unwrap().unwrap();
    let route = route_of(&host);
    assert_eq!(
        ledger::attach_route(fx.root(), &route)
            .unwrap()
            .map(|r| r.receiver_id),
        Some(kept.receiver_id.clone())
    );
    assert_eq!(
        receive(&fx, &host, notice("u1", &route))["status"],
        json!("recorded")
    );

    // 서버가 거절했으면 이전 경로(여기서는 없음)로 되돌린다.
    ledger::clear_child_receiver(fx.root(), L).unwrap();
    unknown.set(false);
    tell().unwrap_err();
    assert_eq!(ledger::child_receiver(fx.root(), L).unwrap(), None);
}

/// 묶기 실패 뒤 kill 응답이 닫았다고 하지 않으면 닫았다고 알리지 않는다.
#[test]
fn an_unconfirmed_kill_is_not_reported_as_closed() {
    let fx = Fixture::new();
    owned_parent(&fx);
    std::fs::write(fx.root().join("children"), "not a directory").unwrap();
    let host = Host::new(&fx, |m, p| match m {
        "attach.agent_call" if p["method"] == json!("kill") => {
            Ok(json!({ "call_id": 9, "result": { "killed": false } }))
        }
        _ => client(m, p),
    });
    let err = spawn(&fx, &host, codex(json!({ "workspace": 3 }))).unwrap_err();
    assert!(
        err.message.contains("could not be closed"),
        "{}",
        err.message
    );
    assert!(
        !err.message.contains("the child was closed"),
        "{}",
        err.message
    );
}

/// worker 에서 도는 결과 펌프는 host 호출이 실패하면 남은 기록을 이번에 묻지 않고 다음 깨우기로 미룬다.
#[test]
fn a_failed_hand_over_stops_this_pump_round() {
    let fx = Fixture::new();
    for n in ["a", "b", "c"] {
        attach_out::queue(fx.root(), &n.repeat(64), &completion(), ROUTE).unwrap();
    }
    fx.rec.notice_fails.set(true);
    let next = fx.with(attach_out::pump);
    assert_eq!(
        fx.rec.notices.borrow().len(),
        1,
        "one call per failing round"
    );
    assert_eq!(attach_out::pending(fx.root()), 3);
    assert!(next.is_some(), "the rest is asked again on the next wake");
}
