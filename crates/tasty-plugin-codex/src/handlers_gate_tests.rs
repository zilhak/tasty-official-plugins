//! 부모 완료 로그의 줄 수를 훅 처리기·tell·notify-caller 를 함께 돌려 확인한다. 호스트는 상태·meta·훅을 기억하고,
//! 발화한 훅의 명령을 모아 두었다가 [`Rig::drain`] 이 notify-caller 로 실행한다.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use super::*;
use crate::delivery::testing::Fixture;

const PARENT: u32 = 3;
const CHILD: u32 = 5;
const THREAD: &str = "019f55e7-3dfa-7a00-8000-0123456789ab";

struct Hook {
    id: u64,
    surface: u32,
    event: String,
    command: String,
    once: bool,
}

#[derive(Default)]
struct GateHost {
    states: RefCell<HashMap<u32, String>>,
    meta: RefCell<HashMap<(u32, String), String>>,
    hooks: RefCell<Vec<Hook>>,
    next_id: RefCell<u64>,
    queued: RefCell<Vec<String>>,
    alive: RefCell<HashSet<u32>>,
    methods: RefCell<Vec<String>>,
    screen: RefCell<String>,
}

impl GateHost {
    fn done_hooks(&self) -> Vec<(String, String)> {
        self.hooks
            .borrow()
            .iter()
            .filter(|h| h.command.contains("notify-caller"))
            .map(|h| (h.event.clone(), h.command.clone()))
            .collect()
    }

    fn fire(&self, surface: u32, event: &str) {
        self.call(
            "surface.fire_hook",
            json!({ "surface_id": surface, "event": event }),
        )
        .unwrap();
    }
}

impl HostCall for GateHost {
    fn call(&self, method: &str, p: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.methods.borrow_mut().push(match method {
            "surface.meta.get" => format!("{method}:{}", p["key"].as_str().unwrap_or("")),
            _ => method.to_string(),
        });
        let surface = |key: &str| p[key].as_u64().unwrap_or(0) as u32;
        let meta_key = || {
            (
                surface("surface_id"),
                p["key"].as_str().unwrap().to_string(),
            )
        };
        Ok(match method {
            "terminal.state" => {
                json!({ "state": self.states.borrow().get(&surface("surface")).cloned() })
            }
            "terminal.set_state" => {
                self.states
                    .borrow_mut()
                    .insert(surface("surface"), p["state"].as_str().unwrap().into());
                json!({})
            }
            "terminal.tell" => {
                self.states
                    .borrow_mut()
                    .insert(surface("surface"), "active".into());
                json!({ "ok": true })
            }
            "surface.meta.get" => match self.meta.borrow().get(&meta_key()) {
                Some(v) => json!({ "value": v }),
                None => json!({ "value": null }),
            },
            "surface.meta.set" => {
                let value = p["value"].as_str().unwrap().to_string();
                self.meta.borrow_mut().insert(meta_key(), value);
                json!({})
            }
            "hook.set" => {
                let mut id = self.next_id.borrow_mut();
                *id += 1;
                self.hooks.borrow_mut().push(Hook {
                    id: *id,
                    surface: surface("surface_id"),
                    event: p["event"].as_str().unwrap().into(),
                    command: p["command"].as_str().unwrap().into(),
                    once: p["once"].as_bool().unwrap_or(false),
                });
                json!({ "hook_id": *id })
            }
            "hook.list" => {
                let s = surface("surface_id");
                Value::Array(
                    self.hooks
                        .borrow()
                        .iter()
                        .filter(|h| h.surface == s)
                        .map(|h| json!({ "id": h.id, "event": h.event, "command": h.command }))
                        .collect(),
                )
            }
            "hook.unset" => {
                let id = p["hook_id"].as_u64().unwrap();
                self.hooks.borrow_mut().retain(|h| h.id != id);
                json!({})
            }
            "surface.fire_hook" => {
                let (s, event) = (surface("surface_id"), p["event"].as_str().unwrap());
                let mut hooks = self.hooks.borrow_mut();
                for h in hooks.iter().filter(|h| h.surface == s && h.event == event) {
                    self.queued.borrow_mut().push(h.command.clone());
                }
                hooks.retain(|h| !(h.surface == s && h.event == event && h.once));
                json!({})
            }
            "terminal.spawn" => {
                // 새 자식 터미널. 셸만 있고 아직 활동이 없다.
                self.alive.borrow_mut().insert(CHILD);
                json!({ "child_surface_id": CHILD })
            }
            "surface.locate" => {
                json!({ "exists": self.alive.borrow().contains(&surface("surface_id")) })
            }
            "surface.screen_text" => json!({ "text": self.screen.borrow().clone() }),
            _ => json!({}),
        })
    }
}

/// 명령 문자열을 notify-caller 요청 인자로 바꾼다. CLI 가 하는 일과 같다.
fn params_of(command: &str) -> Value {
    let mut words = command
        .strip_prefix("tasty codex notify-caller ")
        .expect("notify-caller command")
        .split_whitespace();
    let mut params = serde_json::Map::new();
    while let (Some(flag), Some(value)) = (words.next(), words.next()) {
        let name = flag.trim_start_matches("--").replace('-', "_");
        let value = value
            .parse::<u64>()
            .map_or_else(|_| json!(value), |n| json!(n));
        params.insert(name, value);
    }
    Value::Object(params)
}

struct Rig {
    fx: Fixture,
    generation: String,
    host: GateHost,
}

impl Rig {
    fn new() -> Self {
        let fx = Fixture::new();
        let generation = fx.open(CHILD, &[]);
        let host = GateHost::default();
        host.alive.borrow_mut().insert(CHILD);
        crate::notice::test_log::take();
        Self {
            fx,
            generation,
            host,
        }
    }

    fn hook(&self, event: &str) {
        let params =
            json!({ "surface": CHILD, "event": event, "session": THREAD, "gen": self.generation });
        self.fx
            .with(|d| handle_hook(&self.host, &params, &self.fx.tr, d))
            .expect("hook");
    }

    fn tell(&self) {
        let params = json!({ "surface": CHILD, "message": "go", "caller_surface": PARENT });
        self.fx
            .with(|d| handle_tell(&self.host, &params, &self.fx.tr, d))
            .expect("tell");
    }

    /// 실제 spawn 진입점으로 자식을 만든다. 완료 훅은 이 경로가 건다.
    fn spawn(&self) {
        let params = json!({ "surface": PARENT, "workspace": "1", "prompt": "go" });
        self.fx
            .with(|d| handle_spawn(&self.host, &params, &self.fx.tr, d))
            .expect("spawn");
    }

    fn drain(&self) -> Vec<String> {
        let queued: Vec<String> = self.host.queued.borrow_mut().drain(..).collect();
        for command in queued.iter().filter(|c| c.contains("notify-caller")) {
            handle_notify_caller(&self.host, &params_of(command), &self.fx.tr)
                .expect("notify-caller");
        }
        crate::notice::test_log::take()
            .into_iter()
            .map(|(caller, line)| {
                assert_eq!(caller, PARENT);
                line
            })
            .collect()
    }

    fn turn(&self, end: &str) {
        self.hook("prompt-submit");
        self.hook(end);
    }
}

/// C1(B1): tell 은 상태와 활동 순번을 보내기 전에 읽고, idle 대상의 문턱은 다음 순번이다.
#[test]
fn a_tell_reads_state_and_activity_before_sending() {
    let rig = Rig::new();
    rig.turn("stop");
    rig.host.methods.borrow_mut().clear();
    rig.tell();
    let methods = rig.host.methods.borrow();
    let at = |m: &str| methods.iter().position(|x| x == m).unwrap();
    let seq = format!("surface.meta.get:{ACTIVITY_SEQ_META_KEY}");
    assert!(at("terminal.state") < at("terminal.tell"), "{methods:?}");
    assert!(at(&seq) < at("terminal.tell"), "{methods:?}");
    let (_, idle) = rig
        .host
        .done_hooks()
        .into_iter()
        .find(|(e, _)| e == "codex-idle")
        .unwrap();
    assert_eq!(params_of(&idle)["done_from"], json!(2));
}

/// C1(B3·B4): 새 활동 없는 턴 종료는 줄이 없고, 일을 시작한 턴의 끝은 한 번 알린다. 그 뒤 스스로 연 턴은 줄이 없다.
#[test]
fn one_finished_turn_line_per_tell() {
    let rig = Rig::new();
    rig.host
        .states
        .borrow_mut()
        .insert(CHILD, "idle".to_string());
    rig.tell();
    rig.host
        .states
        .borrow_mut()
        .insert(CHILD, "idle".to_string());
    rig.host.fire(CHILD, "codex-idle");
    assert!(rig.drain().is_empty());
    assert_eq!(rig.host.done_hooks().len(), 3);

    rig.turn("stop");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("[info] surface 5 state changed (via tell)"),
        "{lines:?}"
    );
    assert!(
        !rig.host.done_hooks().iter().any(|(e, _)| e == "codex-idle"),
        "턴 종료 훅은 다시 걸지 않는다"
    );
    rig.turn("stop");
    assert!(rig.drain().is_empty());
    rig.tell();
    rig.turn("stop");
    assert_eq!(rig.drain().len(), 1);
}

/// C1(B10): 새 tell 이 다시 건 뒤 실행된 옛 무장의 처리는 줄을 쓰지 않고 새 무장을 건드리지 않는다.
#[test]
fn a_late_run_of_a_replaced_arm_writes_nothing() {
    let rig = Rig::new();
    rig.tell();
    rig.turn("stop");
    let late: Vec<String> = rig.host.queued.borrow_mut().drain(..).collect();
    rig.tell();
    let armed = rig.host.done_hooks();
    rig.host.queued.borrow_mut().extend(late);
    assert!(rig.drain().is_empty());
    assert_eq!(rig.host.done_hooks(), armed);
}

/// C2: 승인 대기는 다른 문구의 warning 줄 하나. 승인 뒤 턴 종료는 완료 줄 하나.
#[test]
fn an_approval_wait_has_its_own_line() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit");
    rig.hook("permission-request");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("[warning] surface 5 waiting for approval (via tell)"),
        "{lines:?}"
    );
    rig.hook("post-tool-use");
    rig.hook("stop");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed"), "{lines:?}");
}

/// C3: interrupt 도 턴 종료로 알린다.
#[test]
fn an_interrupt_ends_the_turn() {
    let rig = Rig::new();
    rig.tell();
    rig.turn("interrupt");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed (via tell)"), "{lines:?}");
}

/// spawn 의 첫 턴 끝은 한 줄. 종료는 활동이 없어도 알리고, 대상이 사라지면 다시 걸지 않는다.
#[test]
fn spawn_notices_its_turn_once_and_an_exit_always() {
    let rig = Rig::new();
    rig.spawn();
    rig.hook("session-start");
    assert!(rig.drain().is_empty());
    rig.turn("stop");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("(via spawn)"), "{lines:?}");

    rig.host
        .states
        .borrow_mut()
        .insert(CHILD, "exited".to_string());
    rig.host.alive.borrow_mut().clear();
    rig.host.fire(CHILD, "process-exit");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("[error] surface 5 "), "{lines:?}");
    assert!(rig.host.done_hooks().is_empty());
}

/// 꼬리가 없는 옛 훅은 상태로 줄을 정하고 새 형식으로 다시 건다. 샌드박스 안내도 그대로 붙는다.
#[test]
fn an_old_hook_writes_by_state_and_rearms_in_the_new_form() {
    let rig = Rig::new();
    register_notify_hooks(&rig.host, PARENT, CHILD, "tell", None);
    rig.host.screen.replace("RTM_NEWADDR".into());
    rig.host
        .states
        .borrow_mut()
        .insert(CHILD, "idle".to_string());
    rig.host.fire(CHILD, "codex-idle");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("[error] "), "{lines:?}");
    let hooks = rig.host.done_hooks();
    assert_eq!(hooks.len(), 2, "{hooks:?}");
    assert!(
        hooks.iter().all(|(_, c)| c.contains("--event")),
        "{hooks:?}"
    );
}

/// 같은 무장의 승인 대기 처리가 이미 발화한 턴 종료를 지우지 않는다. 승인 대기 처리는 상태가 이미 idle 이라
/// 줄이 없고, 뒤의 턴 종료 처리가 완료 줄 하나를 쓴다.
#[test]
fn a_late_approval_wait_run_keeps_the_fired_turn_end() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit");
    rig.hook("permission-request");
    rig.hook("post-tool-use");
    rig.hook("stop");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed (via tell)"), "{lines:?}");
}

/// 같은 무장의 늦은 턴 종료 처리가 이미 발화한 승인 대기를 지우지 않는다. 앞 턴의 끝은 처리할 때 이미 다음 턴이
/// 시작돼 완료로 셀 수 없으므로 줄이 없고, 승인 대기 줄 하나가 나온다. 턴 종료 훅은 같은 문턱으로 남아 그 턴의
/// 끝에 완료 줄 하나를 쓴다(tell 하나에 완료 한 번).
#[test]
fn a_late_turn_end_run_keeps_the_fired_approval_wait() {
    let rig = Rig::new();
    rig.tell();
    rig.turn("stop");
    rig.hook("prompt-submit");
    rig.hook("permission-request");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("waiting for approval (via tell)"),
        "{lines:?}"
    );
    rig.hook("post-tool-use");
    rig.hook("stop");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed (via tell)"), "{lines:?}");
    rig.turn("stop");
    assert!(rig.drain().is_empty(), "완료는 tell 하나에 한 번");
}

/// 다음 tell 뒤에 늦게 실행된 옛 무장의 승인 대기·턴 종료 처리는 줄을 쓰지 않고 새 무장을 건드리지 않는다.
#[test]
fn late_runs_of_a_replaced_arm_stay_superseded() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit");
    rig.hook("permission-request");
    rig.hook("post-tool-use");
    rig.hook("stop");
    let late: Vec<String> = rig.host.queued.borrow_mut().drain(..).collect();
    assert_eq!(late.len(), 2, "{late:?}");
    rig.tell();
    let armed = rig.host.done_hooks();
    rig.host.queued.borrow_mut().extend(late);
    assert!(rig.drain().is_empty());
    assert_eq!(rig.host.done_hooks(), armed);
}

/// 승인 요청에 tell 로 답하면 대기하던 턴이 이어진다. post-tool-use 없이 끝나도 그 턴의 끝에 완료 줄 하나가 나온다.
#[test]
fn a_tell_that_answers_an_approval_wait_notices_that_turn_end() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit");
    rig.hook("permission-request");
    let first = rig.drain();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("waiting for approval"), "{first:?}");
    rig.tell();
    rig.hook("stop");
    let second = rig.drain();
    assert_eq!(second.len(), 1, "{second:?}");
    assert!(second[0].contains("state changed (via tell)"), "{second:?}");
}

/// 진행 중인 child 에 보낸 tell 이 다음 턴으로 넘어가 처리되면, 지금 턴의 끝과 처리한 턴의 끝에 한 줄씩 나온다.
/// 그 뒤 스스로 연 턴은 줄이 없다.
#[test]
fn a_tell_queued_for_the_next_turn_notices_both_turn_ends() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit");
    rig.tell();
    rig.hook("stop");
    let first = rig.drain();
    rig.turn("stop");
    let second = rig.drain();
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(second.len(), 1, "{second:?}");
    rig.turn("stop");
    assert!(rig.drain().is_empty());
}

/// 한계: 진행 중인 child 에 보낸 tell 이 지금 턴에 흡수되면, 그 뒤 스스로 연 턴 하나의 끝에도 줄이 나온다. 두 번째
/// 줄 뒤에는 더 나오지 않는다.
#[test]
fn a_tell_absorbed_by_the_running_turn_may_notice_one_more_turn() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit");
    rig.tell();
    rig.hook("stop");
    assert_eq!(rig.drain().len(), 1);
    rig.turn("stop");
    assert_eq!(rig.drain().len(), 1, "스스로 연 턴 하나");
    rig.turn("stop");
    assert!(rig.drain().is_empty());
}

/// 원격 tell 응답의 문턱(`done_twice`)으로 건 훅은 두 턴 끝에 한 줄씩 쓰고, 그 뒤 스스로 연 턴은 줄이 없다. 키가
/// 없는 응답은 한 번만 쓴다.
#[test]
fn a_remote_reply_that_asks_twice_notices_two_turn_ends() {
    for (reply, expected) in [
        (json!({ "done_from": 1, "done_twice": true }), [1, 1, 0]),
        (json!({ "done_from": 1 }), [1, 0, 0]),
    ] {
        let rig = Rig::new();
        rig.hook("prompt-submit");
        let threshold = completion_gate::Threshold::of_reply(&reply, DONE_FROM_KEY);
        register_notify_hooks(&rig.host, PARENT, CHILD, "tell", threshold);
        rig.hook("stop");
        let mut counts = vec![rig.drain().len()];
        for _ in 0..2 {
            rig.turn("stop");
            counts.push(rig.drain().len());
        }
        assert_eq!(counts, expected, "{reply}");
    }
}

/// 승인 대기에 tell 로 답하면 그 턴의 끝과 다음 턴의 끝에 한 줄씩 나오고, 그다음 턴은 줄이 없다.
#[test]
fn a_tell_that_answers_an_approval_wait_notices_one_more_turn_end() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit");
    rig.hook("permission-request");
    assert_eq!(rig.drain().len(), 1);
    rig.tell();
    rig.hook("stop");
    let mut counts = vec![rig.drain().len()];
    for _ in 0..2 {
        rig.turn("stop");
        counts.push(rig.drain().len());
    }
    assert_eq!(counts, [1, 1, 0]);
}
