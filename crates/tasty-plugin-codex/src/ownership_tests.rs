//! Codex 호출자의 자식 소유 검사와 반환 경로 기록 순서.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use crate::delivery::ledger::{self, ChildReturn};
use crate::delivery::testing::Fixture;
use crate::handlers;

const PARENT: &str = "019f55e7-3dfa-7a00-8000-0000000000aa";
const OTHER: &str = "019f55e7-3dfa-7a00-8000-0000000000bb";

/// terminal.parent·surface.list를 정해 둔 대로 답하고 호출을 기록한다. terminal.tell 순간의
/// 반환 경로를 함께 남긴다.
struct Host {
    root: PathBuf,
    /// child surface → terminal.parent 응답.
    parents: RefCell<HashMap<u32, Value>>,
    /// surface → workspace.
    workspaces: RefCell<HashMap<u32, u32>>,
    calls: RefCell<Vec<(String, Value)>>,
    route_at_tell: RefCell<Vec<Option<ChildReturn>>>,
    fail_tell: Cell<bool>,
}

impl Host {
    fn new(fx: &Fixture) -> Self {
        Self {
            root: fx.root().to_path_buf(),
            parents: RefCell::default(),
            workspaces: RefCell::default(),
            calls: RefCell::default(),
            route_at_tell: RefCell::default(),
            fail_tell: Cell::new(false),
        }
    }

    fn child_of_surface(&self, child: u32, parent: u32) {
        self.parents.borrow_mut().insert(
            child,
            json!({ "parent_surface_id": parent, "parent_external": null, "status": "active" }),
        );
    }

    fn child_of_external(&self, child: u32, key: &str, workspace: u32) {
        self.parents.borrow_mut().insert(
            child,
            json!({ "parent_surface_id": null, "parent_external": key, "status": "active" }),
        );
        self.workspaces.borrow_mut().insert(child, workspace);
    }

    /// 요청의 부모 키를 가진 모든 자식 surface.
    fn children_of(&self, params: &Value) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .parents
            .borrow()
            .iter()
            .filter(|(_, p)| {
                (params.get("parent_external").is_some()
                    && p["parent_external"] == params["parent_external"])
                    || (params.get("surface").is_some()
                        && p["parent_surface_id"] == params["surface"])
            })
            .map(|(c, _)| *c)
            .collect();
        out.sort_unstable();
        out
    }

    fn calls_of(&self, method: &str) -> Vec<Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

impl HostCall for Host {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.calls
            .borrow_mut()
            .push((method.to_string(), params.clone()));
        let surface = params["surface"].as_u64().map(|v| v as u32);
        Ok(match method {
            "terminal.parent" => self
                .parents
                .borrow()
                .get(&surface.unwrap())
                .cloned()
                .unwrap_or(json!({ "parent_surface_id": null, "status": "none" })),
            "terminal.tell" => {
                if self.fail_tell.get() {
                    return Err(tasty_plugin_sdk::PluginError::HostCall {
                        method: method.into(),
                        message: "pty closed".into(),
                        code: None,
                    });
                }
                self.route_at_tell
                    .borrow_mut()
                    .push(ledger::child_receiver(&self.root, surface.unwrap()).unwrap());
                json!({})
            }
            "surface.list" => Value::Array(
                self.workspaces
                    .borrow()
                    .iter()
                    .map(|(s, w)| json!({ "id": s, "workspace_id": w }))
                    .collect(),
            ),
            "terminal.spawn" => json!({ "child_surface_id": 40, "child_index": 1 }),
            "terminal.respawn" => json!({ "child_surface_id": 41, "child_index": 1 }),
            // 시험의 자식 index n은 surface 39+n이다.
            // 그 부모의 자식이 아니면 실제 호스트처럼 거절한다.
            "terminal.kill" => {
                let child = 39 + params["child"].as_u64().unwrap() as u32;
                if !self.children_of(&params).contains(&child) {
                    return Err(tasty_plugin_sdk::PluginError::HostCall {
                        method: method.into(),
                        message: "child not found under this parent".into(),
                        code: None,
                    });
                }
                json!({ "killed_surface_id": child })
            }
            // 이 모의 호스트는 모든 workspace를 한 창으로 본다. 실제 GUI 호스트는 workspace로 고른
            // 창 안의 자식에게만 보내므로 다른 창의 자식은 그 창의 workspace로 따로 요청해야 한다.
            "terminal.children" => json!({ "children": self.children_of(&params) }),
            "terminal.broadcast" => {
                let children = self.children_of(&params);
                json!({ "sent_count": children.len(), "children": children })
            }
            _ => json!({}),
        })
    }
}

fn codex(mut params: Value) -> Value {
    params["_tasty_context"] = json!({ "provider": "codex", "thread_id": PARENT });
    params
}

/// PARENT가 surface 5의 Tasty helper 세션에서 실행 중이다.
fn owned_parent(fx: &Fixture, host: &Host) {
    let generation = fx.open(5, &[]);
    fx.with(|d| {
        handlers::handle_hook(
            host,
            &json!({"surface": 5, "event": "session-start", "session": PARENT, "gen": generation}),
            &fx.tr,
            d,
        )
    })
    .unwrap();
}

fn rid(fx: &Fixture) -> String {
    fx.with(|d| {
        crate::delivery::handle_parent(d, &codex(json!({})))
            .map(|v| v["receiver_id"].as_str().unwrap().to_string())
    })
    .unwrap()
}

fn tell(
    fx: &Fixture,
    host: &Host,
    params: Value,
) -> Result<Value, tasty_plugin_sdk::IpcMethodError> {
    fx.with(|d| handlers::handle_tell(host, &params, &fx.tr, d))
}

/// Codex 부모는 호스트가 자기 자식으로 기록한 surface에만 tell한다. 반환 경로는 전송 전에
/// 기록하고, 전송이 실패하면 이전 경로로 되돌린다.
#[test]
fn a_codex_caller_tells_only_its_own_children_and_binds_before_sending() {
    let fx = Fixture::new();
    let host = Host::new(&fx);
    owned_parent(&fx, &host);
    let rid = rid(&fx);
    host.child_of_external(31, &format!("codex:{PARENT}"), 1);
    host.child_of_surface(32, 9);
    fx.open(31, &[]);
    fx.open(32, &[]);

    tell(&fx, &host, codex(json!({ "surface": 31, "message": "go" }))).unwrap();
    assert_eq!(
        host.route_at_tell.borrow().as_slice(),
        &[Some(ChildReturn {
            receiver_id: rid.clone(),
            child_index: None,
            attach_parent: None,
            ended_at_ms: None,
        })],
        "첫 훅이 읽을 경로가 전송 순간에 이미 있다"
    );

    let err = tell(&fx, &host, codex(json!({ "surface": 32, "message": "go" }))).unwrap_err();
    assert!(err.message.contains("32"), "{}", err.message);
    assert_eq!(
        host.calls_of("terminal.tell").len(),
        1,
        "남의 자식에게는 보내지 않는다"
    );
    assert_eq!(ledger::child_receiver(fx.root(), 32).unwrap(), None);

    // 전송이 실패하면 경로를 바꾸지 않은 것으로 되돌린다.
    ledger::set_child_receiver(fx.root(), 31, "0123456789abcdef0123456789abcdef", Some(4)).unwrap();
    host.fail_tell.set(true);
    assert!(tell(&fx, &host, codex(json!({ "surface": 31, "message": "x" }))).is_err());
    assert_eq!(
        ledger::child_receiver(fx.root(), 31).unwrap().unwrap(),
        ChildReturn {
            receiver_id: "0123456789abcdef0123456789abcdef".into(),
            child_index: Some(4),
            attach_parent: None,
            ended_at_ms: None,
        }
    );
}

/// Codex 대화가 아닌 호출자의 새 지시는 이전 Codex 반환 경로를 지시 전에 지운다. 이미 기록한
/// 결과는 남는다.
#[test]
fn a_legacy_instruction_clears_the_codex_route_but_keeps_captured_results() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let host = Host::new(&fx);
    let rid = rid(&fx);
    ledger::set_child_receiver(fx.root(), 31, &rid, Some(1)).unwrap();
    let captured = fx
        .with(|d| {
            crate::delivery::handle_deliver(
                d,
                &json!({ "receiver_id": rid, "completion_key": "k:1", "provider": "codex",
                    "child_surface": 31, "child_session": null, "status": "turn_ended",
                    "body": "earlier" }),
            )
        })
        .unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();

    tell(
        &fx,
        &host,
        json!({ "surface": 31, "message": "other work" }),
    )
    .unwrap();
    assert_eq!(host.route_at_tell.borrow().as_slice(), &[None]);
    assert!(ledger::read_event(fx.root(), &captured).unwrap().is_some());

    ledger::set_child_receiver(fx.root(), 41, &rid, Some(1)).unwrap();
    fx.with(|d| handlers::handle_respawn(&host, &json!({ "surface": 7, "child": 1 }), &fx.tr, d))
        .unwrap();
    assert_eq!(ledger::child_receiver(fx.root(), 41).unwrap(), None);
}

/// Codex 훅이 없는 surface에는 완료를 만들지 못하는 경로를 두지 않고 그렇다고 알린다.
#[test]
fn a_tell_to_a_non_codex_child_installs_no_route_and_says_so() {
    let fx = Fixture::new();
    let host = Host::new(&fx);
    owned_parent(&fx, &host);
    rid(&fx);
    host.child_of_external(33, &format!("codex:{PARENT}"), 1);
    let out = tell(&fx, &host, codex(json!({ "surface": 33, "message": "go" }))).unwrap();
    assert!(out["warning"].as_str().unwrap().contains("33"), "{out}");
    assert_eq!(host.route_at_tell.borrow().as_slice(), &[None]);
}

/// Codex 부모가 다른 부모를 명시하면 호스트에 요청하지 않고 거절한다. 실행 중인 자기 surface는
/// 받되 호스트에는 thread 부모 키를 넘긴다.
#[test]
fn an_explicit_other_parent_is_refused_for_every_command() {
    let fx = Fixture::new();
    let host = Host::new(&fx);
    owned_parent(&fx, &host);
    host.child_of_external(40, &format!("codex:{PARENT}"), 1);
    let cases: [(&str, Value); 5] = [
        ("spawn", json!({ "workspace": "w" })),
        ("children", json!({})),
        ("broadcast", json!({ "text": "t" })),
        ("kill", json!({ "child": 1 })),
        ("respawn", json!({ "child": 1 })),
    ];
    let run = |name: &str, params: &Value| {
        fx.with(|d| match name {
            "spawn" => handlers::handle_spawn(&host, params, &fx.tr, d),
            "children" => handlers::handle_children(&host, params, &fx.tr, d),
            "broadcast" => handlers::handle_broadcast(&host, params, &fx.tr, d),
            "kill" => handlers::handle_kill(&host, params, &fx.tr, d),
            _ => handlers::handle_respawn(&host, params, &fx.tr, d),
        })
    };
    for (name, base) in &cases {
        let mut other = codex(base.clone());
        other["surface"] = json!(9);
        let before = host.calls.borrow().len();
        assert!(run(name, &other).is_err(), "{name}");
        assert!(
            host.calls.borrow()[before..]
                .iter()
                .all(|(m, _)| !m.starts_with("terminal.")),
            "{name}: 거절한 요청은 호스트로 가지 않는다"
        );
        let mut own = codex(base.clone());
        own["surface"] = json!(5);
        assert!(run(name, &own).is_ok(), "{name}");
    }
    let spawned = host.calls_of("terminal.spawn");
    assert_eq!(
        (&spawned[0]["parent_external"], &spawned[0]["parent"]),
        (&json!(format!("codex:{PARENT}")), &Value::Null)
    );
    for (method, params) in host.calls.borrow().iter() {
        if ["spawn", "children", "broadcast", "kill", "respawn"]
            .iter()
            .any(|m| method == &format!("terminal.{m}"))
        {
            assert_eq!(params.get("surface"), None, "{method}: {params}");
        }
    }
}

/// 같은 helper surface 5에서 A가 자식을 만든 뒤 `/new`로 B가 되면 B는 A의 자식을 관리하지
/// 못한다. 자식 소유는 물리 surface가 아니라 Codex thread다.
#[test]
fn a_new_conversation_on_the_same_surface_does_not_manage_the_previous_children() {
    let fx = Fixture::new();
    let host = Host::new(&fx);
    owned_parent(&fx, &host);
    fx.with(|d| handlers::handle_spawn(&host, &codex(json!({ "workspace": "1" })), &fx.tr, d))
        .unwrap();
    let spawned = host.calls_of("terminal.spawn");
    let a_key = format!("codex:{PARENT}");
    assert_eq!(spawned[0]["parent_external"], json!(a_key));
    host.child_of_external(40, &a_key, 1);
    // 이전 버전이 surface 부모로 만든 자식도 B의 자식이 아니다.
    host.child_of_surface(34, 5);
    fx.open(40, &[]);
    fx.open(34, &[]);

    // /new: 같은 세대가 B로 옮긴다.
    let generation = crate::session::registry::current_gen(fx.root(), 5)
        .unwrap()
        .unwrap();
    fx.ready(&generation, "unix:///owned/s.sock");
    fx.set_live(&[OTHER]);
    fx.with(|d| {
        handlers::handle_hook(
            &host,
            &json!({"surface": 5, "event": "session-start", "session": OTHER, "gen": generation}),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    let b = |mut params: Value| {
        params["_tasty_context"] = json!({ "provider": "codex", "thread_id": OTHER });
        params
    };
    let b_key = json!(format!("codex:{OTHER}"));
    let before = host.calls_of("terminal.tell").len();
    let a_route = ledger::child_receiver(fx.root(), 40).unwrap();
    assert!(a_route.is_some());
    for child in [40, 34] {
        let err = tell(&fx, &host, b(json!({ "surface": child, "message": "x" }))).unwrap_err();
        assert!(err.message.contains(&child.to_string()), "{}", err.message);
    }
    assert_eq!(host.calls_of("terminal.tell").len(), before);

    let out = fx
        .with(|d| handlers::handle_children(&host, &b(json!({})), &fx.tr, d))
        .unwrap();
    assert_eq!(out["children"], json!([]));
    fx.with(|d| handlers::handle_broadcast(&host, &b(json!({ "text": "t" })), &fx.tr, d))
        .unwrap();
    assert!(
        fx.with(|d| handlers::handle_kill(&host, &b(json!({ "child": 1 })), &fx.tr, d))
            .is_err()
    );
    assert_eq!(
        ledger::child_receiver(fx.root(), 40).unwrap(),
        a_route,
        "A의 경로가 남는다"
    );
    for method in ["terminal.children", "terminal.broadcast", "terminal.kill"] {
        let sent = host.calls_of(method);
        let last = sent.last().unwrap();
        assert_eq!(
            (&last["parent_external"], last.get("surface")),
            (&b_key, None),
            "{method}"
        );
    }
    // A로 돌아가면 A의 자식을 다시 관리한다.
    fx.set_live(&[PARENT]);
    let a = codex(json!({ "surface": 40, "message": "back" }));
    fx.with(|d| {
        handlers::handle_hook(
            &host,
            &json!({"surface": 5, "event": "prompt-submit", "session": PARENT, "gen": generation,
                "turn_id": "t"}),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    tell(&fx, &host, a).unwrap();
}

/// 외부 부모의 요청은 한 번만 보낸다. 호스트의 workspace는 창을 고를 뿐 자식을 거르지 않으므로
/// workspace마다 나눠 보내면 같은 자식에게 두 번 보낸다. index 명령은 그 자식의 workspace를,
/// 목록·broadcast는 자식이 한 workspace에만 있을 때 그 workspace를 쓰고, 여럿이면 `--workspace`를
/// 요구한다.
#[test]
fn an_external_parent_sends_one_request_routed_by_the_child_workspace() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let host = Host::new(&fx);
    let rid = rid(&fx);
    let key = format!("codex:{PARENT}");
    ledger::set_child_receiver(fx.root(), 40, &rid, Some(1)).unwrap();
    ledger::set_child_receiver(fx.root(), 41, &rid, Some(2)).unwrap();
    // 다른 대화의 자식 기록은 고려하지 않는다.
    ledger::set_child_receiver(fx.root(), 42, "0123456789abcdef0123456789abcdef", Some(2)).unwrap();
    host.child_of_external(40, &key, 3);
    host.child_of_external(41, &key, 7);
    host.workspaces.borrow_mut().insert(42, 8);
    let children =
        |params: Value| fx.with(|d| handlers::handle_children(&host, &codex(params), &fx.tr, d));
    let broadcast =
        |params: Value| fx.with(|d| handlers::handle_broadcast(&host, &codex(params), &fx.tr, d));

    let err = children(json!({})).unwrap_err();
    assert!(err.message.contains("3, 7"), "{}", err.message);
    assert!(broadcast(json!({ "text": "t" })).is_err());
    assert!(host.calls_of("terminal.children").is_empty());
    assert!(host.calls_of("terminal.broadcast").is_empty());

    let out = children(json!({ "workspace": "3" })).unwrap();
    assert_eq!(out["children"], json!([40, 41]), "중복 행이 없다");
    let sent = broadcast(json!({ "text": "t", "workspace": "3" })).unwrap();
    assert_eq!(sent["sent_count"], 2, "각 자식에게 한 번씩");
    assert_eq!(host.calls_of("terminal.broadcast").len(), 1);

    fx.with(|d| handlers::handle_kill(&host, &codex(json!({ "child": 2 })), &fx.tr, d))
        .unwrap();
    let kill = &host.calls_of("terminal.kill")[0];
    assert_eq!(
        (&kill["parent_external"], &kill["workspace"], &kill["child"]),
        (&json!(key), &json!("7"), &json!(2))
    );

    // 닫은 자식의 경로가 지워져 자식이 한 workspace에만 남으면 그 workspace로 보낸다.
    assert_eq!(ledger::child_receiver(fx.root(), 41).unwrap(), None);
    children(json!({})).unwrap();
    assert_eq!(
        host.calls_of("terminal.children").last().unwrap()["workspace"],
        json!("3")
    );
}

/// 외부 부모를 알리지 않는 호스트에서는 외부 부모의 소유를 증명할 수 없으므로 tell하지 않는다.
#[test]
fn an_external_parent_cannot_prove_ownership_on_a_host_without_external_parents() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let host = Host::new(&fx);
    rid(&fx);
    host.parents
        .borrow_mut()
        .insert(40, json!({ "parent_surface_id": null, "status": "active" }));
    fx.open(40, &[]);
    assert!(tell(&fx, &host, codex(json!({ "surface": 40, "message": "go" }))).is_err());
    assert!(host.calls_of("terminal.tell").is_empty());
}
