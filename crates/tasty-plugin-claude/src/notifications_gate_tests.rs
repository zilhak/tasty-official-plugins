//! 부모 완료 로그의 줄 수를 훅 처리기·tell·notify-done 을 함께 돌려 확인한다. 호스트는 상태·meta·훅을 기억하고,
//! 발화한 훅의 명령을 모아 두었다가 [`Rig::drain`] 이 notify-done 으로 실행한다.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_sdk::i18n::Translator;

use tasty_plugin_agent_common::completion_gate;

use crate::error_scan::ErrorScanner;
use crate::state::ClaudeState;
use crate::stop_pairing::{HeldStop, StopKind, StopPairing, lock_pairing};

const PARENT: u32 = 3;
const CHILD: u32 = 5;

fn tr() -> Translator {
    Translator::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang"),
        "en",
    )
}

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
    /// 발화해 아직 실행하지 않은 훅 명령.
    queued: RefCell<Vec<String>>,
    alive: RefCell<HashSet<u32>>,
    methods: RefCell<Vec<String>>,
}

impl GateHost {
    fn done_hooks(&self) -> Vec<(String, String)> {
        self.hooks
            .borrow()
            .iter()
            .filter(|h| h.command.contains("notify-done"))
            .map(|h| (h.event.clone(), h.command.clone()))
            .collect()
    }

    fn set_state(&self, state: &str) {
        self.states.borrow_mut().insert(CHILD, state.to_string());
    }

    fn seq(&self) -> u64 {
        self.meta
            .borrow()
            .get(&(CHILD, crate::hook::ACTIVITY_SEQ_META_KEY.to_string()))
            .map_or(0, |v| v.parse().unwrap())
    }
}

impl HostCall for GateHost {
    fn call(&self, method: &str, p: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.methods.borrow_mut().push(match method {
            "surface.meta.get" => format!("{method}:{}", p["key"].as_str().unwrap_or("")),
            _ => method.to_string(),
        });
        let surface = |key: &str| p[key].as_u64().unwrap_or(0) as u32;
        Ok(match method {
            "terminal.state" => {
                let state = self.states.borrow().get(&surface("surface")).cloned();
                json!({ "state": state })
            }
            "terminal.set_state" => {
                self.states
                    .borrow_mut()
                    .insert(surface("surface"), p["state"].as_str().unwrap().into());
                json!({})
            }
            "terminal.tell" => {
                // 호스트는 보낸 직후 상태를 active 로 바꾼다.
                self.states
                    .borrow_mut()
                    .insert(surface("surface"), "active".into());
                json!({ "ok": true })
            }
            "surface.meta.get" => {
                let key = (
                    surface("surface_id"),
                    p["key"].as_str().unwrap().to_string(),
                );
                match self.meta.borrow().get(&key) {
                    Some(v) => json!({ "value": v }),
                    None => json!({ "value": null }),
                }
            }
            "surface.meta.set" => {
                let key = (
                    surface("surface_id"),
                    p["key"].as_str().unwrap().to_string(),
                );
                let value = p["value"].as_str().unwrap().to_string();
                self.meta.borrow_mut().insert(key, value);
                json!({})
            }
            "surface.meta.unset" => {
                let key = (
                    surface("surface_id"),
                    p["key"].as_str().unwrap().to_string(),
                );
                self.meta.borrow_mut().remove(&key);
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
                let list: Vec<Value> = self
                    .hooks
                    .borrow()
                    .iter()
                    .filter(|h| h.surface == s)
                    .map(|h| json!({ "id": h.id, "event": h.event, "command": h.command }))
                    .collect();
                Value::Array(list)
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
                let s = surface("surface_id");
                json!({ "exists": self.alive.borrow().contains(&s) })
            }
            _ => json!({}),
        })
    }
}

/// 명령 문자열을 notify-done 요청 인자로 바꾼다. CLI 가 하는 일과 같다.
fn params_of(command: &str) -> Value {
    let mut words = command
        .strip_prefix("tasty claude notify-done ")
        .expect("notify-done command")
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
    state: Mutex<ClaudeState>,
    scanner: Arc<Mutex<ErrorScanner>>,
    resume: Mutex<crate::auto_resume::ResumeTable>,
    pairing: Mutex<StopPairing>,
    returns: crate::codex_return::Returns,
    _dir: tempfile::TempDir,
    host: GateHost,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let host = GateHost::default();
        host.alive.borrow_mut().insert(CHILD);
        crate::notice::test_log::take();
        Self {
            state: Mutex::new(ClaudeState::default()),
            scanner: Arc::new(Mutex::new(ErrorScanner::new())),
            resume: Mutex::new(Default::default()),
            pairing: Mutex::new(StopPairing::default()),
            returns: crate::codex_return::Returns::new(Some(dir.path())),
            _dir: dir,
            host,
        }
    }

    fn hook(&self, event: &str, extra: Value) {
        let mut params = json!({ "event": event, "surface": CHILD, "session": "s1" });
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        crate::hook::handle_claude_hook(
            &self.state,
            &self.scanner,
            &self.resume,
            &self.pairing,
            &self.returns,
            &self.host,
            &params,
            None,
            &tr(),
        )
        .expect("hook");
    }

    fn tell(&self) {
        crate::handlers::handle_tell(
            &self.returns,
            &self.pairing,
            &self.host,
            &json!({ "surface": CHILD, "message": "go", "caller_surface": PARENT }),
            &tr(),
        )
        .expect("tell");
    }

    /// 실제 spawn 진입점으로 자식을 만든다. 완료 훅은 이 경로가 건다.
    fn spawn(&self) {
        crate::handlers::handle_spawn(
            &self.scanner,
            &self.returns,
            &self.host,
            &json!({
                "surface": PARENT,
                "workspace": "2",
                "prompt": "go",
                "caller_surface": PARENT,
            }),
            None,
            &tr(),
        )
        .expect("spawn");
    }

    /// 발화한 훅을 실행하고 이번에 쓴 줄을 돌려준다.
    fn drain(&self) -> Vec<String> {
        let queued: Vec<String> = self.host.queued.borrow_mut().drain(..).collect();
        for command in queued.iter().filter(|c| c.contains("notify-done")) {
            super::handle_notify_done(&self.scanner, &self.host, &params_of(command), &tr())
                .expect("notify-done");
        }
        crate::notice::test_log::take()
            .into_iter()
            .map(|(caller, line)| {
                assert_eq!(caller, PARENT);
                line
            })
            .collect()
    }

    /// 한 턴: 시작 → 끝.
    fn turn(&self) {
        self.hook("prompt-submit", json!({}));
        self.hook("stop", json!({}));
    }

    fn done_from_of(&self, event: &str) -> u64 {
        let (_, command) = self
            .host
            .done_hooks()
            .into_iter()
            .find(|(e, _)| e == event)
            .expect("hook armed");
        params_of(&command)["done_from"].as_u64().unwrap()
    }
}

/// B1: tell 은 상태와 활동 순번을 보내기 전에 읽는다.
#[test]
fn a_tell_reads_state_and_activity_before_sending() {
    let rig = Rig::new();
    rig.tell();
    let methods = rig.host.methods.borrow();
    let at = |m: &str| methods.iter().position(|x| x == m).unwrap();
    let sent = at("terminal.tell");
    assert!(at("terminal.state") < sent, "{methods:?}");
    let seq = format!("surface.meta.get:{}", crate::hook::ACTIVITY_SEQ_META_KEY);
    assert!(at(&seq) < sent, "{methods:?}");
}

/// B2: idle 대상은 다음 순번, 진행 중인 대상은 지금 순번, 보류 Stop 이 있는 진행 중 대상은 다음 순번.
#[test]
fn the_tell_threshold_follows_the_state_before_sending() {
    let rig = Rig::new();
    rig.hook("prompt-submit", json!({}));
    rig.hook("stop", json!({}));
    assert_eq!(rig.host.seq(), 1);
    rig.tell();
    assert_eq!(rig.done_from_of("claude-idle"), 2);

    let rig = Rig::new();
    rig.hook("prompt-submit", json!({}));
    rig.tell();
    assert_eq!(rig.done_from_of("claude-idle"), 1);

    let rig = Rig::new();
    rig.hook("prompt-submit", json!({}));
    lock_pairing(&rig.pairing).stop(
        "s1",
        HeldStop {
            surface_id: CHILD,
            prompt_id: None,
            kind: StopKind::Idle,
            final_answer: None,
            turn: None,
            seq: 0,
        },
        1,
        std::time::Instant::now(),
    );
    rig.tell();
    assert_eq!(rig.done_from_of("claude-idle"), 2);
}

/// B3: tell 뒤 새 활동 없이 온 턴 종료는 줄을 쓰지 않고 같은 무장으로 자기 훅만 다시 건다.
#[test]
fn an_idle_without_new_activity_writes_nothing_and_rearms() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    let first = rig.host.done_hooks();
    rig.host.set_state("idle");
    rig.hook("stop", json!({}));
    assert!(rig.drain().is_empty());
    let again = rig.host.done_hooks();
    assert_eq!(again.len(), 3, "{again:?}");
    let events: HashSet<&str> = again.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(
        events,
        HashSet::from(["claude-idle", "needs-input", "process-exit"])
    );
    let (first, again): (HashSet<_>, HashSet<_>) =
        (first.into_iter().collect(), again.into_iter().collect());
    assert_eq!(first, again, "같은 무장 id");
}

/// B4 와 (나): tell 하나에 턴 종료는 한 번. 그 뒤 스스로 연 턴은 줄이 없고, 다음 tell 은 다시 한 번 알린다.
#[test]
fn one_finished_turn_line_per_tell() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.turn();
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("[info] surface 5: state changed (via tell)"),
        "{lines:?}"
    );
    // 턴 종료 훅은 다시 걸지 않는다. 입력 대기와 종료는 남는다.
    let events: Vec<String> = rig.host.done_hooks().into_iter().map(|(e, _)| e).collect();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(!events.iter().any(|e| e == "claude-idle"), "{events:?}");

    rig.turn();
    assert!(rig.drain().is_empty(), "스스로 연 턴");

    rig.tell();
    rig.turn();
    assert_eq!(rig.drain().len(), 1);
}

/// B5·B6: 질문 하나에 입력 대기 줄 하나(warning, 다른 문구). 답한 뒤 턴 종료 줄 하나.
#[test]
fn one_input_wait_line_per_question() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    rig.hook("pre-tool-use", json!({ "tool_name": "AskUserQuestion" }));
    let first = rig.drain();
    rig.hook(
        "notification",
        json!({ "notification_type": "permission_prompt" }),
    );
    let second = rig.drain();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(second.is_empty(), "{second:?}");
    assert!(
        first[0].starts_with("[warning] surface 5: waiting for input (via tell)"),
        "{first:?}"
    );
    rig.hook("post-tool-use", json!({}));
    rig.hook("stop", json!({}));
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed"), "{lines:?}");
}

/// B7: 같은 턴의 질문 두 번은 줄 두 개다.
#[test]
fn two_questions_in_a_turn_write_two_lines() {
    let rig = Rig::new();
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    let mut lines = Vec::new();
    for _ in 0..2 {
        rig.hook("pre-tool-use", json!({ "tool_name": "AskUserQuestion" }));
        lines.extend(rig.drain());
        rig.hook("post-tool-use", json!({}));
        lines.extend(rig.drain());
    }
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|l| l.contains("waiting for input")));
}

/// B8: child-profile 의 재기동(세션 종료·입력 대기로 시작)은 줄을 남기지 않는다.
#[test]
fn a_profile_restart_writes_nothing() {
    let rig = Rig::new();
    rig.turn();
    let done_from = super::next_activity_threshold(&rig.host, CHILD);
    super::register_notify_hooks(
        &rig.host,
        PARENT,
        CHILD,
        "child-profile",
        Some(completion_gate::Threshold::once(done_from)),
    );
    rig.hook("session-end", json!({ "reason": "other" }));
    rig.hook("session-start", json!({ "source": "resume" }));
    assert!(rig.drain().is_empty());
    assert_eq!(rig.host.states.borrow()[&CHILD], "idle");
    rig.turn();
    assert_eq!(rig.drain().len(), 1);
}

/// B9: 앞 턴의 늦은 턴 종료가 새 턴이 시작된 뒤에 처리되면 줄이 없다. 새 턴의 끝은 알린다.
#[test]
fn a_late_idle_of_the_previous_turn_writes_nothing() {
    let rig = Rig::new();
    rig.hook("prompt-submit", json!({}));
    rig.tell();
    // 보류했던 앞 턴의 끝이 새 프롬프트에서 확정돼 claude-idle 을 쏜다. 처리는 새 턴이 시작된 뒤다.
    rig.host.set_state("idle");
    rig.host
        .call(
            "surface.fire_hook",
            json!({ "surface_id": CHILD, "event": "claude-idle" }),
        )
        .unwrap();
    rig.hook("prompt-submit", json!({}));
    assert!(rig.drain().is_empty());
    rig.hook("stop", json!({}));
    assert_eq!(rig.drain().len(), 1);
}

/// B10: 새 tell 이 다시 건 뒤 실행된 옛 무장의 처리는 줄을 쓰지 않고 새 무장을 건드리지 않는다.
#[test]
fn a_late_run_of_a_replaced_arm_writes_nothing() {
    let rig = Rig::new();
    rig.tell();
    rig.turn();
    let late: Vec<String> = rig.host.queued.borrow_mut().drain(..).collect();
    rig.tell();
    let armed = rig.host.done_hooks();
    rig.host.queued.borrow_mut().extend(late);
    assert!(rig.drain().is_empty());
    assert_eq!(rig.host.done_hooks(), armed);
}

/// B11: 같은 무장의 턴 종료 처리가 두 번 실행돼도 줄은 하나다.
#[test]
fn the_same_arm_run_twice_writes_one_line() {
    let rig = Rig::new();
    rig.tell();
    rig.turn();
    let queued = rig.host.queued.borrow().clone();
    rig.host.queued.borrow_mut().extend(queued);
    assert_eq!(rig.drain().len(), 1);
}

/// B12: 순번은 새 활동에서만 오른다. 입력 대기로 시작한 세션은 올리지 않고, 프롬프트를 실어 실행한 세션은 올린다.
#[test]
fn activity_counts_only_new_work() {
    let rig = Rig::new();
    rig.hook("session-start", json!({ "source": "startup" }));
    assert_eq!(rig.host.seq(), 0);
    rig.host.meta.borrow_mut().insert(
        (
            CHILD,
            crate::session_start::LAUNCH_PROMPT_META_KEY.to_string(),
        ),
        "1".into(),
    );
    rig.hook("session-start", json!({ "source": "startup" }));
    assert_eq!(rig.host.seq(), 1);
    rig.hook("stop", json!({}));
    rig.hook("pre-tool-use", json!({}));
    rig.hook(
        "notification",
        json!({ "notification_type": "permission_prompt" }),
    );
    assert_eq!(rig.host.seq(), 1);
    rig.hook("post-tool-use", json!({}));
    rig.hook("active", json!({}));
    assert_eq!(rig.host.seq(), 3);
    // 순번은 meta 에서 이어 간다(플러그인 재시작).
    let restarted = Rig::new();
    restarted.host.meta.borrow_mut().insert(
        (CHILD, crate::hook::ACTIVITY_SEQ_META_KEY.to_string()),
        "41".into(),
    );
    restarted.hook("prompt-submit", json!({}));
    assert_eq!(restarted.host.seq(), 42);
}

/// B13: 꼬리가 없는 옛 훅은 상태로 줄을 정하고 새 형식으로 다시 건다.
#[test]
fn an_old_hook_writes_by_state_and_rearms_in_the_new_form() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    super::register_notify_hooks(&rig.host, PARENT, CHILD, "tell", None);
    assert!(
        rig.host
            .done_hooks()
            .iter()
            .all(|(_, c)| !c.contains("--event"))
    );
    rig.hook("stop", json!({}));
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed (via tell)"), "{lines:?}");
    let hooks = rig.host.done_hooks();
    assert!(
        hooks.iter().all(|(_, c)| c.contains("--event")),
        "{hooks:?}"
    );
    assert!(!hooks.iter().any(|(e, _)| e == "claude-idle"), "{hooks:?}");
}

/// spawn: 시작 직후 줄이 없고 첫 턴 끝에 한 줄. 종료는 활동이 없어도 알린다.
#[test]
fn spawn_notices_its_turn_once_and_an_exit_always() {
    let rig = Rig::new();
    rig.spawn();
    rig.hook("session-start", json!({ "source": "startup" }));
    assert!(rig.drain().is_empty());
    rig.turn();
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("(via spawn)"), "{lines:?}");

    let dead = Rig::new();
    dead.spawn();
    dead.host.set_state("exited");
    dead.host.alive.borrow_mut().clear();
    dead.host
        .call(
            "surface.fire_hook",
            json!({ "surface_id": CHILD, "event": "process-exit" }),
        )
        .unwrap();
    let lines = dead.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("[error] surface 5:"), "{lines:?}");
    assert!(dead.host.done_hooks().is_empty());
}

/// 같은 무장의 입력 대기 처리가 이미 발화한 턴 종료를 지우지 않는다. 입력 대기 처리는 상태가 이미 idle 이라
/// 줄이 없고, 뒤의 턴 종료 처리가 완료 줄 하나를 쓴다.
#[test]
fn a_late_input_wait_run_keeps_the_fired_turn_end() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    rig.hook("pre-tool-use", json!({ "tool_name": "AskUserQuestion" }));
    rig.hook("post-tool-use", json!({}));
    rig.hook("stop", json!({}));
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed (via tell)"), "{lines:?}");
}

/// 같은 무장의 늦은 턴 종료 처리가 이미 발화한 입력 대기를 지우지 않는다. 앞 턴의 끝은 처리할 때 이미 다음
/// 턴이 시작돼 완료로 셀 수 없으므로 줄이 없고, 입력 대기 줄 하나가 나온다. 턴 종료 훅은 같은 문턱으로 남아
/// 그 턴의 끝에 완료 줄 하나를 쓴다(tell 하나에 완료 한 번).
#[test]
fn a_late_turn_end_run_keeps_the_fired_input_wait() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.turn();
    rig.hook("prompt-submit", json!({}));
    rig.hook("pre-tool-use", json!({ "tool_name": "AskUserQuestion" }));
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("waiting for input (via tell)"),
        "{lines:?}"
    );
    rig.hook("post-tool-use", json!({}));
    rig.hook("stop", json!({}));
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed (via tell)"), "{lines:?}");
    rig.turn();
    assert!(rig.drain().is_empty(), "완료는 tell 하나에 한 번");
}

/// 다음 tell 뒤에 늦게 실행된 옛 무장의 입력 대기·턴 종료 처리는 줄을 쓰지 않고 새 무장을 건드리지 않는다.
#[test]
fn late_runs_of_a_replaced_arm_stay_superseded() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    rig.hook("pre-tool-use", json!({ "tool_name": "AskUserQuestion" }));
    rig.hook("post-tool-use", json!({}));
    rig.hook("stop", json!({}));
    let late: Vec<String> = rig.host.queued.borrow_mut().drain(..).collect();
    assert_eq!(late.len(), 2, "{late:?}");
    rig.tell();
    let armed = rig.host.done_hooks();
    rig.host.queued.borrow_mut().extend(late);
    assert!(rig.drain().is_empty());
    assert_eq!(rig.host.done_hooks(), armed);
}

/// 권한 요청에 tell 로 답하면 대기하던 턴이 이어진다. 그 턴의 끝에 완료 줄 하나가 나온다.
#[test]
fn a_tell_that_answers_an_input_wait_notices_that_turn_end() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    rig.hook(
        "notification",
        json!({ "notification_type": "permission_prompt" }),
    );
    let first = rig.drain();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("waiting for input"), "{first:?}");
    rig.tell();
    rig.hook("stop", json!({}));
    let second = rig.drain();
    assert_eq!(second.len(), 1, "{second:?}");
    assert!(second[0].contains("state changed (via tell)"), "{second:?}");
}

/// 진행 중인 child 에 보낸 tell 이 다음 턴으로 넘어가 처리되면, 지금 턴의 끝과 처리한 턴의 끝에 한 줄씩 나온다.
/// 그 뒤 스스로 연 턴은 줄이 없다.
#[test]
fn a_tell_queued_for_the_next_turn_notices_both_turn_ends() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    rig.tell();
    rig.hook("stop", json!({}));
    let first = rig.drain();
    rig.turn();
    let second = rig.drain();
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(second.len(), 1, "{second:?}");
    rig.turn();
    assert!(rig.drain().is_empty());
}

/// 한계: 진행 중인 child 에 보낸 tell 이 지금 턴에 흡수되면, 그 뒤 스스로 연 턴 하나의 끝에도 줄이 나온다. 두 번째
/// 줄 뒤에는 더 나오지 않는다.
#[test]
fn a_tell_absorbed_by_the_running_turn_may_notice_one_more_turn() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    rig.tell();
    rig.hook("stop", json!({}));
    assert_eq!(rig.drain().len(), 1);
    rig.turn();
    assert_eq!(rig.drain().len(), 1, "스스로 연 턴 하나");
    rig.turn();
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
        rig.hook("prompt-submit", json!({}));
        let threshold =
            completion_gate::Threshold::of_reply(&reply, crate::handlers::DONE_FROM_KEY);
        super::register_notify_hooks(&rig.host, PARENT, CHILD, "tell", threshold);
        rig.hook("stop", json!({}));
        let mut counts = vec![rig.drain().len()];
        for _ in 0..2 {
            rig.turn();
            counts.push(rig.drain().len());
        }
        assert_eq!(counts, expected, "{reply}");
    }
}

/// 입력 대기에 tell 로 답하면 그 턴의 끝과 다음 턴의 끝에 한 줄씩 나오고, 그다음 턴은 줄이 없다.
#[test]
fn a_tell_that_answers_an_input_wait_notices_one_more_turn_end() {
    let rig = Rig::new();
    rig.host.set_state("idle");
    rig.tell();
    rig.hook("prompt-submit", json!({}));
    rig.hook(
        "notification",
        json!({ "notification_type": "permission_prompt" }),
    );
    assert_eq!(rig.drain().len(), 1);
    rig.tell();
    rig.hook("stop", json!({}));
    let mut counts = vec![rig.drain().len()];
    for _ in 0..2 {
        rig.turn();
        counts.push(rig.drain().len());
    }
    assert_eq!(counts, [1, 1, 0]);
}

/// Monitor 하나를 띄운다(Claude Code 2.1.319 실측 `PostToolUse` 모양).
fn start_monitor(rig: &Rig, id: &str) {
    rig.hook(
        crate::hook::BACKGROUND_START_EVENT,
        json!({ "tool_name": "Monitor", "tool_response": { "taskId": id, "timeoutMs": 1_500_000, "persistent": false } }),
    );
}

/// Monitor 하나만 남긴 Stop. Stop 목록에서 Monitor 는 `type: shell` 이다.
fn stop_leaving(rig: &Rig, ids: &[&str]) {
    let items: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "type": "shell", "status": "running" }))
        .collect();
    rig.hook("stop", json!({ "background_tasks": items }));
}

/// Monitor 의 중간 이벤트가 연 턴(`<status>` 없음).
fn monitor_event(rig: &Rig, id: &str) {
    rig.hook(
        "prompt-submit",
        json!({ "prompt": format!("<task-notification>\n<task-id>{id}</task-id>\n<summary>Monitor event: \"ticks\"</summary>\n<event>tick</event>\n</task-notification>") }),
    );
}

/// Monitor 만 남기고 턴을 끝낸 자식은 idle 이고 완료 줄 하나를 쓴다. 그 뒤 Monitor 이벤트가 연 턴이 다시 Monitor 만
/// 남기고 끝나도 새 spawn·tell 이 없으면 줄을 더 쓰지 않는다(spawn·tell 한 번에 완료 한 번).
#[test]
fn a_child_that_leaves_only_a_monitor_is_done_once_until_the_next_order() {
    let rig = Rig::new();
    rig.spawn();
    rig.hook("session-start", json!({ "source": "startup" }));
    rig.hook("prompt-submit", json!({}));
    start_monitor(&rig, "bym7ibssy");
    stop_leaving(&rig, &["bym7ibssy"]);
    assert_eq!(rig.host.states.borrow()[&CHILD], "idle");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("state changed (via spawn)"), "{lines:?}");

    for _ in 0..3 {
        monitor_event(&rig, "bym7ibssy");
        assert_eq!(rig.host.states.borrow()[&CHILD], "active");
        stop_leaving(&rig, &["bym7ibssy"]);
        assert_eq!(rig.host.states.borrow()[&CHILD], "idle");
        assert!(rig.drain().is_empty(), "이벤트 턴은 줄을 쓰지 않는다");
    }

    rig.tell();
    rig.hook("prompt-submit", json!({}));
    stop_leaving(&rig, &["bym7ibssy"]);
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "새 tell 은 다시 한 번: {lines:?}");
    assert!(lines[0].contains("(via tell)"), "{lines:?}");
}

/// Monitor 와 백그라운드 셸이 함께 남은 Stop 은 대기다. 줄이 없다. 셸이 끝나 열린 턴이 Monitor 만 남기고 끝나면
/// 그때 줄 하나를 쓴다.
#[test]
fn a_monitor_next_to_a_shell_waits_until_only_the_monitor_is_left() {
    let rig = Rig::new();
    rig.spawn();
    rig.hook("session-start", json!({ "source": "startup" }));
    rig.hook("prompt-submit", json!({}));
    start_monitor(&rig, "m1");
    stop_leaving(&rig, &["m1", "bwny9udnf"]);
    assert_eq!(rig.host.states.borrow()[&CHILD], "active");
    assert!(rig.drain().is_empty());

    rig.hook(
        "prompt-submit",
        json!({ "prompt": "<task-notification>\n<task-id>bwny9udnf</task-id>\n<status>completed</status>\n</task-notification>" }),
    );
    stop_leaving(&rig, &["m1"]);
    assert_eq!(rig.host.states.borrow()[&CHILD], "idle");
    let lines = rig.drain();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("(via spawn)"), "{lines:?}");
}
