//! 부모 완료 로그에 줄을 남길지 정한다. spawn·tell 하나에 턴 종료는 한 번만 알리고, 그 뒤에는 입력 대기와
//! 종료만 알린다.
//!
//! - 자식의 훅 처리기는 새 활동(턴 시작 등)마다 surface meta 의 활동 순번을 1 올린다.
//! - spawn·tell 은 입력을 보내기 전에 문턱 `N`([`threshold`])을 정하고, 이벤트마다 다른 명령으로 once 훅을
//!   건다([`arm`]). 명령 꼬리에 이벤트·문턱·무장 id 가 들어간다.
//! - 훅 명령(notify-done 등)은 [`evaluate`] 로 줄의 종류와 다시 무장할지를 정하고, 줄을 쓴 뒤 [`finish`] 로
//!   자기 무장의 남은 훅을 지우고 필요하면 다시 무장한다.
//!
//! 꼬리가 없는 명령은 업그레이드 전에 등록한 훅이다. 호출자는 그 경우 예전 처리를 따른다.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::host_call::{HostCall, group_members_to_unset, surface_is_alive};

/// 완료 훅이 받는 사건.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeEvent {
    /// 턴이 끝났다(`claude-idle`·`codex-idle`).
    Idle,
    /// 입력·승인을 기다린다(`needs-input`).
    Input,
    /// 프로세스가 끝났다(`process-exit`).
    Exit,
}

impl NoticeEvent {
    /// 명령 꼬리의 `--event` 값.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Input => "input",
            Self::Exit => "exit",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "input" => Some(Self::Input),
            "exit" => Some(Self::Exit),
            _ => None,
        }
    }
}

/// 이벤트별 호스트 훅 이름. 각 플러그인의 매니페스트에 선언한 이름이다.
#[derive(Debug, Clone, Copy)]
pub struct HookEvents {
    pub idle: &'static str,
    pub input: &'static str,
    pub exit: &'static str,
}

impl HookEvents {
    fn name(&self, event: NoticeEvent) -> &'static str {
        match event {
            NoticeEvent::Idle => self.idle,
            NoticeEvent::Input => self.input,
            NoticeEvent::Exit => self.exit,
        }
    }
}

/// 훅 명령 꼬리에 실린 값.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tail {
    pub event: NoticeEvent,
    /// 이 순번 이상의 활동 뒤에 온 턴 종료만 완료로 센다.
    pub done_from: u64,
    /// 무장 id. 무장할 때마다 새로 받는다.
    pub arm: u64,
}

impl Tail {
    /// 명령 뒤에 붙이는 꼬리(앞에 공백 포함).
    pub fn render(&self) -> String {
        format!(
            " --event {} --done-from {} --arm {}",
            self.event.as_str(),
            self.done_from,
            self.arm
        )
    }

    /// 그룹 접두어 뒤의 명령 꼬리를 읽는다. `--event` 가 없으면(옛 형식) `None` 이다.
    pub fn parse(command: &str, group_prefix: &str) -> Option<Self> {
        let rest = command.strip_prefix(group_prefix)?;
        let mut words = rest.split_whitespace();
        // 첫 단어는 명령 종류(spawn·tell 등)다.
        words.next()?;
        let (mut event, mut done_from, mut arm) = (None, None, None);
        while let Some(flag) = words.next() {
            let value = words.next()?;
            match flag {
                "--event" => event = NoticeEvent::parse(value),
                "--done-from" => done_from = value.parse().ok(),
                "--arm" => arm = value.parse().ok(),
                _ => return None,
            }
        }
        Some(Self {
            event: event?,
            done_from: done_from?,
            arm: arm?,
        })
    }

    /// 요청 인자(`event`·`done_from`·`arm`)에서 읽는다. 하나라도 없거나 틀리면 옛 형식으로 본다.
    pub fn of_params(params: &Value) -> Option<Self> {
        let number = |key: &str| {
            params.get(key).and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
            })
        };
        Some(Self {
            event: NoticeEvent::parse(params.get("event")?.as_str()?)?,
            done_from: number("done_from")?,
            arm: number("arm")?,
        })
    }
}

/// 새 무장 id. 등록 시각(ms)과 프로세스 안 순번을 합친다. CLI 정수 인자(i64)에 들어가는 크기다.
pub fn new_arm_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ms * 1000 + NEXT.fetch_add(1, Ordering::Relaxed) % 1000
}

/// 입력을 보내기 전의 대상 상태와 활동 순번으로 정하는 문턱. 대상이 턴을 진행 중이고 끝난 턴의 보류도 없으면
/// 메시지가 그 턴에 흡수될 수 있으므로 지금 턴의 끝도 완료로 센다. 그 밖에는 새 활동이 시작된 뒤의 끝만 센다.
pub fn threshold(state: Option<&str>, seq: u64, held_stop: bool) -> u64 {
    if state == Some("active") && !held_stop {
        seq
    } else {
        seq + 1
    }
}

/// 대상의 현재 상태(`idle`·`active`·`needs_input`·`exited` 등). 조회에 실패하면 `None` 이다.
pub fn target_state<H: HostCall>(host: &H, target: u32) -> Option<String> {
    host.call("terminal.state", json!({ "surface": target }))
        .ok()
        .and_then(|r| r.get("state").and_then(|v| v.as_str()).map(str::to_string))
}

/// surface meta 의 활동 순번. 없거나 읽지 못하면 0 이다.
pub fn activity_seq<H: HostCall>(host: &H, target: u32, key: &str) -> u64 {
    host.call(
        "surface.meta.get",
        json!({ "surface_id": target, "key": key }),
    )
    .ok()
    .and_then(|r| {
        r.get("value")
            .and_then(Value::as_str)
            .and_then(|s| s.trim().parse().ok())
    })
    .unwrap_or(0)
}

/// 같은 부모·대상의 완료 그룹 하나. 명령은 `{prefix}{kind}{tail}` 이다.
#[derive(Debug, Clone, Copy)]
pub struct Group<'a> {
    pub target: u32,
    /// 같은 부모·대상이 공유하는 명령 앞부분. 끝이 종류 플래그(`--command `·`--kind `)다.
    pub prefix: &'a str,
    /// spawn·tell 등 명령 종류.
    pub kind: &'a str,
    pub events: HookEvents,
    /// 활동 순번 meta 키.
    pub seq_key: &'a str,
    /// 로그에 적는 플러그인 이름.
    pub agent: &'a str,
}

impl Group<'_> {
    fn command(&self, tail: Tail) -> String {
        format!("{}{}{}", self.prefix, self.kind, tail.render())
    }
}

/// 같은 접두어에서 무장 id 가 `arm` 인 훅의 id.
pub fn arm_members(hooks: &[Value], group_prefix: &str, arm: u64) -> Vec<u64> {
    hooks
        .iter()
        .filter(|h| {
            h.get("command")
                .and_then(Value::as_str)
                .and_then(|c| Tail::parse(c, group_prefix))
                .is_some_and(|t| t.arm == arm)
        })
        .filter_map(|h| h.get("id").and_then(Value::as_u64))
        .collect()
}

/// 같은 접두어에 다른 무장(옛 형식 포함)의 훅이 있는가. 있으면 새 tell 이나 다른 처리가 이미 다시 무장했다.
pub fn other_arm_present(hooks: &[Value], group_prefix: &str, arm: u64) -> bool {
    hooks.iter().any(|h| {
        h.get("command")
            .and_then(Value::as_str)
            .filter(|c| c.starts_with(group_prefix))
            .is_some_and(|c| Tail::parse(c, group_prefix).is_none_or(|t| t.arm != arm))
    })
}

/// 같은 무장에 턴 종료 훅이 남아 있는가.
fn idle_in_arm(hooks: &[Value], group_prefix: &str, arm: u64) -> bool {
    hooks.iter().any(|h| {
        h.get("command")
            .and_then(Value::as_str)
            .and_then(|c| Tail::parse(c, group_prefix))
            .is_some_and(|t| t.arm == arm && t.event == NoticeEvent::Idle)
    })
}

/// 같은 부모·대상의 그룹을 지우고 새 무장으로 이벤트별 once 훅을 건다. `with_idle` 이 거짓이면 턴 종료 훅은
/// 걸지 않는다(이 무장에서 완료를 이미 알렸다). 등록 실패는 로그만 남긴다. 새 무장 id 를 돌려준다.
pub fn arm<H: HostCall>(host: &H, group: &Group<'_>, done_from: u64, with_idle: bool) -> u64 {
    let arm = new_arm_id();
    match host.call("hook.list", json!({ "surface_id": group.target })) {
        Ok(resp) => {
            for hook_id in
                group_members_to_unset(resp.as_array().map_or(&[][..], Vec::as_slice), group.prefix)
            {
                if let Err(e) = host.call("hook.unset", json!({ "hook_id": hook_id })) {
                    tracing::warn!(
                        "{} hook.unset {hook_id} failed while replacing the completion group: {e}",
                        group.agent
                    );
                }
            }
        }
        Err(e) => tracing::warn!(
            "{} hook.list failed before registering completion hooks: {e}",
            group.agent
        ),
    }
    let events = [NoticeEvent::Idle, NoticeEvent::Input, NoticeEvent::Exit];
    for event in events
        .into_iter()
        .filter(|e| with_idle || *e != NoticeEvent::Idle)
    {
        let name = group.events.name(event);
        let command = group.command(Tail {
            event,
            done_from,
            arm,
        });
        if let Err(e) = host.call(
            "hook.set",
            json!({
                "surface_id": group.target,
                "event": name,
                "command": command,
                "once": true,
            }),
        ) {
            tracing::warn!("{} notify hook.set '{name}' failed: {e}", group.agent);
        }
    }
    arm
}

/// 부모 로그에 쓸 줄의 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 일을 시작한 뒤 턴이 끝났다.
    Done,
    /// 입력·승인을 기다린다.
    NeedsInput,
    /// 프로세스가 끝났다. 활동 여부와 관계없이 알린다.
    Exited,
    /// 줄을 쓰지 않는다.
    Skip,
}

/// 다시 무장할 내용.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rearm {
    pub done_from: u64,
    /// 턴 종료 훅도 거는가.
    pub with_idle: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    /// `None` 이면 다시 무장하지 않는다.
    pub rearm: Option<Rearm>,
}

/// 판정. `idle_armed` 는 이 무장에 턴 종료 훅이 있었는가, `superseded` 는 그룹에 다른 무장이 있는가다.
/// 턴 종료를 알린 뒤에는 턴 종료 훅을 다시 걸지 않는다. 다음 spawn·tell 이 다시 건다.
pub fn decide(
    tail: Tail,
    idle_armed: bool,
    state: Option<&str>,
    seq: u64,
    superseded: bool,
) -> Verdict {
    if superseded {
        return Verdict {
            decision: Decision::Skip,
            rearm: None,
        };
    }
    let same = Rearm {
        done_from: tail.done_from,
        with_idle: idle_armed,
    };
    match tail.event {
        NoticeEvent::Idle if state == Some("idle") && seq >= tail.done_from => Verdict {
            decision: Decision::Done,
            rearm: Some(Rearm {
                done_from: tail.done_from,
                with_idle: false,
            }),
        },
        NoticeEvent::Idle => Verdict {
            decision: Decision::Skip,
            rearm: Some(Rearm {
                with_idle: true,
                ..same
            }),
        },
        NoticeEvent::Input if state == Some("needs_input") => Verdict {
            decision: Decision::NeedsInput,
            rearm: Some(same),
        },
        NoticeEvent::Input => Verdict {
            decision: Decision::Skip,
            rearm: Some(same),
        },
        NoticeEvent::Exit => Verdict {
            decision: Decision::Exited,
            rearm: Some(same),
        },
    }
}

/// 훅 명령 처리를 한 번에 하나씩 하게 한다. 같은 부모·대상의 늦은 처리가 새 무장을 덮거나 같은 무장의 두
/// 처리가 줄을 두 번 쓰지 않게 한다. 원격 대상의 다시 읽기 동안에도 잡고 있다.
pub fn serialize() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// relay 가 늦을 수 있는 원격 대상에서 활동 순번을 다시 읽는 횟수와 간격.
pub const REMOTE_SEQ_RETRIES: u32 = 4;
pub const REMOTE_SEQ_PAUSE: Duration = Duration::from_millis(250);

/// 순번이 `need` 에 닿을 때까지 `retries` 번 더 읽는다. 마지막으로 읽은 값을 돌려준다.
pub fn settle_seq(
    mut read: impl FnMut() -> u64,
    need: u64,
    retries: u32,
    mut pause: impl FnMut(),
) -> u64 {
    let mut seq = read();
    for _ in 0..retries {
        if seq >= need {
            break;
        }
        pause();
        seq = read();
    }
    seq
}

/// 훅 명령 하나의 판정과 그때 본 대상 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub verdict: Verdict,
    pub state: Option<String>,
}

/// 그룹을 조회하고 대상 상태와 활동 순번을 읽어 판정한다. 원격 대상이면 relay 가 늦은 순번을 잠깐 다시 읽는다.
pub fn evaluate<H: HostCall>(
    host: &H,
    group: &Group<'_>,
    tail: Tail,
    remote: bool,
    pause: impl FnMut(),
) -> Evaluation {
    let hooks = host
        .call("hook.list", json!({ "surface_id": group.target }))
        .ok()
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default();
    let superseded = other_arm_present(&hooks, group.prefix, tail.arm);
    let idle_armed = tail.event == NoticeEvent::Idle || idle_in_arm(&hooks, group.prefix, tail.arm);
    let state = target_state(host, group.target);
    let read = || activity_seq(host, group.target, group.seq_key);
    let seq = if tail.event == NoticeEvent::Idle && state.as_deref() == Some("idle") && remote {
        let seq = settle_seq(read, tail.done_from, REMOTE_SEQ_RETRIES, pause);
        if seq < tail.done_from {
            tracing::warn!(
                "{} s{}: idle without new activity (seq {seq} < {}); the relay may be late",
                group.agent,
                group.target,
                tail.done_from
            );
        }
        seq
    } else {
        read()
    };
    Evaluation {
        verdict: decide(tail, idle_armed, state.as_deref(), seq, superseded),
        state,
    }
}

/// 자기 무장의 남은 훅을 지우고, 판정이 정하면 대상이 조회될 때 다시 무장한다.
pub fn finish<H: HostCall>(host: &H, group: &Group<'_>, tail: Tail, verdict: Verdict) {
    if let Ok(resp) = host.call("hook.list", json!({ "surface_id": group.target }))
        && let Some(hooks) = resp.as_array()
    {
        for hook_id in arm_members(hooks, group.prefix, tail.arm) {
            if let Err(e) = host.call("hook.unset", json!({ "hook_id": hook_id })) {
                tracing::warn!(
                    "{} hook.unset {hook_id} failed during arm cleanup: {e}",
                    group.agent
                );
            }
        }
    }
    if let Some(rearm) = verdict.rearm
        && surface_is_alive(host, group.target)
    {
        arm(host, group, rearm.done_from, rearm.with_idle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn tail(event: NoticeEvent, done_from: u64) -> Tail {
        Tail {
            event,
            done_from,
            arm: 1,
        }
    }

    fn skip_rearm(done_from: u64, with_idle: bool) -> Verdict {
        Verdict {
            decision: Decision::Skip,
            rearm: Some(Rearm {
                done_from,
                with_idle,
            }),
        }
    }

    /// A1: 새 활동 없이 온 턴 종료는 줄을 쓰지 않고 같은 문턱으로 다시 건다.
    #[test]
    fn an_idle_without_new_activity_is_skipped() {
        let v = decide(tail(NoticeEvent::Idle, 5), true, Some("idle"), 4, false);
        assert_eq!(v, skip_rearm(5, true));
    }

    /// A2: 문턱에 닿은 턴 종료는 완료다. 그 뒤에는 턴 종료 훅을 다시 걸지 않는다.
    #[test]
    fn an_idle_after_new_activity_is_done_once() {
        let v = decide(tail(NoticeEvent::Idle, 5), true, Some("idle"), 5, false);
        assert_eq!(v.decision, Decision::Done);
        assert_eq!(
            v.rearm,
            Some(Rearm {
                done_from: 5,
                with_idle: false
            })
        );
    }

    /// A3: 처리할 때 이미 새 턴이 시작됐으면 줄 없이 다음 턴 끝을 기다린다.
    #[test]
    fn an_idle_seen_after_the_next_turn_started_is_skipped() {
        let v = decide(tail(NoticeEvent::Idle, 5), true, Some("active"), 5, false);
        assert_eq!(v, skip_rearm(5, true));
    }

    /// A4: 입력 대기는 문턱과 관계없이 알리고 같은 문턱으로 다시 건다.
    #[test]
    fn an_input_wait_is_noticed_and_keeps_the_threshold() {
        for idle_armed in [true, false] {
            let v = decide(
                tail(NoticeEvent::Input, 5),
                idle_armed,
                Some("needs_input"),
                0,
                false,
            );
            assert_eq!(v.decision, Decision::NeedsInput);
            assert_eq!(
                v.rearm,
                Some(Rearm {
                    done_from: 5,
                    with_idle: idle_armed
                })
            );
        }
    }

    /// A5: 처리할 때 대기가 이미 끝났으면 줄을 쓰지 않는다.
    #[test]
    fn an_input_wait_that_already_ended_is_skipped() {
        let v = decide(tail(NoticeEvent::Input, 5), true, Some("idle"), 9, false);
        assert_eq!(v, skip_rearm(5, true));
    }

    /// A6: 종료는 활동과 문턱에 관계없이 알린다.
    #[test]
    fn an_exit_is_always_noticed() {
        for (state, seq) in [(Some("exited"), 0), (None, 0), (Some("idle"), 99)] {
            let v = decide(tail(NoticeEvent::Exit, 5), false, state, seq, false);
            assert_eq!(v.decision, Decision::Exited);
        }
    }

    /// A7: 그룹에 다른 무장이 있으면 어떤 사건이든 줄을 쓰지 않고 다시 걸지 않는다.
    #[test]
    fn a_superseded_arm_writes_nothing() {
        for event in [NoticeEvent::Idle, NoticeEvent::Input, NoticeEvent::Exit] {
            let v = decide(tail(event, 5), true, Some("idle"), 9, true);
            assert_eq!(
                v,
                Verdict {
                    decision: Decision::Skip,
                    rearm: None
                }
            );
        }
    }

    const PREFIX: &str = "tasty x notify-done --caller-surface 1 --target-surface 5 --command ";

    /// A8: 명령 꼬리는 만들고 읽으면 같은 값이다. 꼬리가 없으면 옛 형식이다.
    #[test]
    fn a_tail_round_trips_and_an_old_command_has_none() {
        let t = Tail {
            event: NoticeEvent::Idle,
            done_from: 7,
            arm: 1_700_000_000_000_001,
        };
        let command = format!("{PREFIX}tell{}", t.render());
        assert!(command.ends_with("tell --event idle --done-from 7 --arm 1700000000000001"));
        assert_eq!(Tail::parse(&command, PREFIX), Some(t));
        assert_eq!(Tail::parse(&format!("{PREFIX}tell"), PREFIX), None);
        assert_eq!(Tail::parse("tasty other --command tell", PREFIX), None);
        let params = json!({ "event": "idle", "done_from": 7, "arm": 1_700_000_000_000_001_i64 });
        assert_eq!(Tail::of_params(&params), Some(t));
        assert_eq!(Tail::of_params(&json!({ "command": "tell" })), None);
    }

    /// A9: 무장 단위 고르기는 다른 무장과 다른 부모의 훅을 고르지 않는다.
    #[test]
    fn arm_members_isolate_by_arm_and_caller() {
        let cmd = |prefix: &str, arm: u64| {
            format!(
                "{prefix}tell{}",
                Tail {
                    event: NoticeEvent::Idle,
                    done_from: 1,
                    arm
                }
                .render()
            )
        };
        let other_caller = "tasty x notify-done --caller-surface 11 --target-surface 5 --command ";
        let hooks = vec![
            json!({ "id": 1, "command": cmd(PREFIX, 10) }),
            json!({ "id": 2, "command": cmd(PREFIX, 20) }),
            json!({ "id": 3, "command": cmd(other_caller, 10) }),
            json!({ "id": 4, "command": format!("{PREFIX}tell") }),
            json!({ "id": 5, "command": "tasty x notify-error --caller-surface 1 --target-surface 5" }),
        ];
        assert_eq!(arm_members(&hooks, PREFIX, 10), vec![1]);
        assert!(other_arm_present(&hooks, PREFIX, 10));
        assert!(other_arm_present(&hooks[..1], PREFIX, 20));
        assert!(!other_arm_present(&hooks[..1], PREFIX, 10));
        // 옛 형식 훅도 다른 무장으로 본다.
        assert!(other_arm_present(&hooks[3..], PREFIX, 10));
    }

    /// 문턱: 진행 중인 턴(보류 없음)은 지금 순번, 나머지는 다음 순번.
    #[test]
    fn the_threshold_counts_the_running_turn_only() {
        assert_eq!(threshold(Some("active"), 4, false), 4);
        assert_eq!(threshold(Some("active"), 4, true), 5);
        for state in [Some("idle"), Some("needs_input"), Some("stale"), None] {
            assert_eq!(threshold(state, 4, false), 5);
        }
    }

    /// 순번을 n 번째 조회부터 올리는 mock 호스트.
    struct SeqHost {
        reads: Cell<u32>,
        rises_at: u32,
        hooks: RefCell<Vec<Value>>,
    }

    impl HostCall for SeqHost {
        fn call(
            &self,
            method: &str,
            _params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            Ok(match method {
                "terminal.state" => json!({ "state": "idle" }),
                "surface.meta.get" => {
                    let n = self.reads.get() + 1;
                    self.reads.set(n);
                    let seq = if n >= self.rises_at { 5 } else { 4 };
                    json!({ "value": seq.to_string() })
                }
                "hook.list" => Value::Array(self.hooks.borrow().clone()),
                _ => json!({}),
            })
        }
    }

    fn group() -> Group<'static> {
        Group {
            target: 5,
            prefix: PREFIX,
            kind: "tell",
            events: HookEvents {
                idle: "x-idle",
                input: "needs-input",
                exit: "process-exit",
            },
            seq_key: "x-activity-seq",
            agent: "x",
        }
    }

    /// A10: 원격 대상에서 순번이 모자라면 정해진 횟수만큼 다시 읽는다. 끝까지 모자라면 Skip, 닿으면 Done.
    #[test]
    fn a_remote_idle_rereads_a_late_sequence() {
        let host = |rises_at| SeqHost {
            reads: Cell::new(0),
            rises_at,
            hooks: RefCell::new(Vec::new()),
        };
        let never = host(u32::MAX);
        let pauses = Cell::new(0);
        let e = evaluate(&never, &group(), tail(NoticeEvent::Idle, 5), true, || {
            pauses.set(pauses.get() + 1)
        });
        assert_eq!(e.verdict, skip_rearm(5, true));
        assert_eq!(never.reads.get(), REMOTE_SEQ_RETRIES + 1);
        assert_eq!(pauses.get(), REMOTE_SEQ_RETRIES);

        let late = host(3);
        let e = evaluate(&late, &group(), tail(NoticeEvent::Idle, 5), true, || {});
        assert_eq!(e.verdict.decision, Decision::Done);
        assert_eq!(late.reads.get(), 3);

        // 로컬 대상은 다시 읽지 않는다.
        let local = host(3);
        let e = evaluate(&local, &group(), tail(NoticeEvent::Idle, 5), false, || {});
        assert_eq!(e.verdict.decision, Decision::Skip);
        assert_eq!(local.reads.get(), 1);
    }

    /// 판정은 그룹의 무장을 보고 덮인 무장과 턴 종료 훅 유무를 정한다.
    #[test]
    fn evaluation_reads_the_arm_from_the_hook_list() {
        let g = group();
        let cmd = |event, arm| {
            g.command(Tail {
                event,
                done_from: 5,
                arm,
            })
        };
        let host = SeqHost {
            reads: Cell::new(0),
            rises_at: 1,
            hooks: RefCell::new(vec![
                json!({ "id": 1, "command": cmd(NoticeEvent::Input, 1) }),
                json!({ "id": 2, "command": cmd(NoticeEvent::Idle, 1) }),
            ]),
        };
        let e = evaluate(&host, &g, tail(NoticeEvent::Exit, 5), false, || {});
        assert_eq!(e.verdict.rearm.map(|r| r.with_idle), Some(true));
        host.hooks.borrow_mut().pop();
        let e = evaluate(&host, &g, tail(NoticeEvent::Exit, 5), false, || {});
        assert_eq!(e.verdict.rearm.map(|r| r.with_idle), Some(false));
        host.hooks
            .borrow_mut()
            .push(json!({ "id": 3, "command": cmd(NoticeEvent::Idle, 2) }));
        let e = evaluate(&host, &g, tail(NoticeEvent::Exit, 5), false, || {});
        assert_eq!(e.verdict.rearm, None);
    }
}
