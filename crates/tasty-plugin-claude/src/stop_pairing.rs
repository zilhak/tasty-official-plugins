//! 같은 Stop 의 상태 훅(`claude hook stop`)과 Stop 게이트 판정(`claude checklist-hook`)을 짝짓는다.
//!
//! Claude Code 는 한 Stop 의 훅을 병렬로 실행하므로 두 요청의 도착 순서가 정해지지 않는다.
//! 플러그인은 요청을 한 워커에서 차례로 처리하므로 한 요청이 다른 요청을 기다릴 수 없다.
//! 그래서 게이트가 붙은 세션의 Stop 은 idle 처리를 보류해 이 표에 넣고 곧바로 반환한다.
//! 게이트 판정이 모두 모이면 확정한다. 하나라도 block 이면 턴이 이어지므로 idle 을 보고하지 않고,
//! 모두 pass 이면 보류한 idle 처리를 실행한다. 판정이 [`GATE_DECISION_TIMEOUT`] 안에 오지 않으면
//! 백그라운드 스레드가 온 판정만으로 확정하며, 오지 않은 판정은 pass 로 본다.
//! 표는 메모리에만 있어 플러그인이 재시작되면 보류 중인 Stop 이 사라진다.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use crate::hook::HostCall as PlannedCall;

/// 게이트 판정을 기다리는 상한. 격리 debug 인스턴스(부하 평균 18~24, 20코어)에서
/// `tasty claude checklist-hook` 한 번의 CLI 왕복은 80회 중 최대 115ms(중앙값 99ms)였다.
/// 게이트가 없어 판정이 오지 않는 경우에만 이 시간만큼 idle 이 늦어지므로 수십 배 여유를 둔다.
pub(crate) const GATE_DECISION_TIMEOUT: Duration = Duration::from_secs(5);

/// Claude Code 가 Stop 훅의 block 을 연속으로 받아들이는 기본 횟수. 이 횟수만큼 이어진 뒤의
/// block 은 무시하고 턴을 끝낸다. 사용자는 `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` 로 바꿀 수 있고,
/// 게이트 명령이 그 값을 `--block-cap` 으로 넘긴다.
pub(crate) const DEFAULT_BLOCK_CAP: u32 = 8;

/// surface meta 키 — 플러그인이 Claude 를 실행할 때 붙인 `--settings` 파일 경로.
/// spawn·respawn·launch·reboot 가 쓰고, 파일 없이 실행하면 지운다. Stop 이 게이트 수를 셀 때 읽는다.
pub(crate) const SETTINGS_FILE_META_KEY: &str = "claude-settings-file";

/// surface meta 키 — settings 경로를 기록한 뒤 처음 시작한 Claude 세션의 id. SessionStart 가 쓰고,
/// 실행 기록([`record_settings_file`])이 지운다. 이 세션의 SessionEnd 만 settings 경로를 지운다.
/// respawn·reboot 는 새 경로를 기록한 뒤 이전 Claude 를 끝내므로, 이전 세션의 SessionEnd 가
/// 새 경로를 지우지 않게 한다.
pub(crate) const SETTINGS_SESSION_META_KEY: &str = "claude-settings-session";

/// 게이트 명령을 알아보는 문자열. 게이트 부착은 이 명령을 Stop 훅으로 넣는다.
const GATE_COMMAND: &str = "tasty claude checklist-hook";

/// 훅·게이트 처리기와 확정 스레드가 함께 잠근다. poison 은 한 번 알리고 기존 표를 계속 쓴다.
pub(crate) fn lock_pairing(
    pairing: &std::sync::Mutex<StopPairing>,
) -> std::sync::MutexGuard<'_, StopPairing> {
    const WHAT: &str = "the claude stop pairing table";
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    tasty_utils::poison::recover_mutex(pairing.lock(), WHAT, &REPORTED)
}

/// 게이트 한 건의 판정.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Pass,
    Block,
}

/// 보류한 Stop 의 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopKind {
    /// 턴 종료 후보. 모두 pass 로 확정되면 idle 처리를 실행한다.
    Idle,
    /// 백그라운드 작업을 기다리는 Stop. 이미 active 로 보고했으므로 판정은 연속 block 수에만 쓴다.
    Waiting,
}

/// 확정된 Stop 한 건.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settled {
    pub surface_id: u32,
    /// 이 Stop 의 세션. 턴 종료를 보고할 때 같은 세션의 결과 경로만 고르는 데 쓴다.
    pub session: String,
    pub kind: StopKind,
    /// 턴이 끝났는가. 게이트가 block 했고 Claude Code 가 그 block 을 받아들이면 false 다.
    pub turn_ended: bool,
    /// 이 Stop 의 최종 답변(`last_assistant_message`). 턴이 끝나면 agent task 턴 끝 보고에 싣는다.
    pub final_answer: Option<String>,
    /// Stop 이 왔을 때의 Codex 결과 경로 턴. 확정이 늦어도 이 턴의 결과로 claim 한다.
    pub turn: Option<crate::codex_return::TurnId>,
}

impl Settled {
    /// 보류한 idle 처리를 실행해야 하는가.
    pub fn runs_idle(&self) -> bool {
        self.kind == StopKind::Idle && self.turn_ended
    }
}

/// 시간 초과로 확정된 Stop. 확정 스레드는 idle 을 보내기 직전에 그 사이 새 턴이 시작됐는지 확인한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Expired {
    pub settled: Settled,
    pub session: String,
    /// 확정할 때의 턴 세대. [`StopPairing::is_current_turn`] 으로 비교한다.
    pub turn: u64,
}

/// 보류 표에 넣는 Stop 한 건.
pub(crate) struct HeldStop {
    pub surface_id: u32,
    pub prompt_id: Option<String>,
    pub kind: StopKind,
    /// 이 Stop 의 최종 답변. 턴 종료로 확정되면 agent task 턴 끝 보고에 싣는다.
    pub final_answer: Option<String>,
    /// Stop 이 왔을 때의 Codex 결과 경로 턴.
    pub turn: Option<crate::codex_return::TurnId>,
}

struct PendingStop {
    surface_id: u32,
    prompt_id: Option<String>,
    kind: StopKind,
    final_answer: Option<String>,
    turn: Option<crate::codex_return::TurnId>,
    expected: usize,
    verdicts: Vec<Verdict>,
    block_cap: Option<u32>,
    since: Instant,
}

struct Orphan {
    prompt_id: Option<String>,
    verdict: Verdict,
    block_cap: Option<u32>,
    at: Instant,
}

/// 세션별 보류 Stop 과 아직 짝이 없는 판정, 연속 block 수.
#[derive(Default)]
pub(crate) struct StopPairing {
    pending: HashMap<String, PendingStop>,
    orphans: HashMap<String, VecDeque<Orphan>>,
    /// 시간 초과로 확정된 Stop 에 늦게 올 판정 수. 다음 Stop 이 오기 전까지만 버린다.
    skip: HashMap<String, usize>,
    /// (prompt_id, 연속 block 수).
    blocks: HashMap<String, (Option<String>, u32)>,
    /// 세션별 턴 세대. 새 턴과 세션 종료에서 올린다.
    turns: HashMap<String, u64>,
}

fn same_prompt(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

impl StopPairing {
    /// 게이트가 붙은 세션의 Stop 을 넣는다. 판정이 이미 모였으면 확정 결과를 돌려준다.
    /// 같은 세션에 보류 중인 Stop 이 있으면 그 Stop 은 block 됐던 것이다. 턴이 끝났다면 새 Stop 이
    /// 오기 전에 UserPromptSubmit 이 왔을 것이기 때문이다.
    /// Claude Code 는 한 Stop 의 훅이 모두 끝난 뒤에 턴을 잇고 플러그인은 요청을 차례로 처리하므로,
    /// 새 Stop 이 오면 앞 Stop 의 오지 않은 판정은 오지 않는다. 그 빈자리를 남기면 이 Stop 의 판정을 삼킨다.
    pub fn stop(
        &mut self,
        session: &str,
        held: HeldStop,
        expected: usize,
        now: Instant,
    ) -> Vec<Settled> {
        let mut out = Vec::new();
        self.skip.remove(session);
        if let Some(mut previous) = self.pending.remove(session) {
            if !previous.verdicts.contains(&Verdict::Block) {
                previous.verdicts.push(Verdict::Block);
            }
            out.push(self.settle(session, previous));
        }
        let HeldStop {
            surface_id,
            prompt_id,
            kind,
            final_answer,
            turn,
        } = held;
        let mut entry = PendingStop {
            surface_id,
            prompt_id,
            kind,
            final_answer,
            turn,
            expected,
            verdicts: Vec::new(),
            block_cap: None,
            since: now,
        };
        if let Some(queue) = self.orphans.get_mut(session) {
            while entry.verdicts.len() < entry.expected {
                let Some(orphan) = queue.pop_front() else {
                    break;
                };
                let fresh = now.saturating_duration_since(orphan.at) < GATE_DECISION_TIMEOUT;
                if fresh && same_prompt(&orphan.prompt_id, &entry.prompt_id) {
                    entry.verdicts.push(orphan.verdict);
                    entry.block_cap = entry.block_cap.or(orphan.block_cap);
                }
            }
            if queue.is_empty() {
                self.orphans.remove(session);
            }
        }
        if entry.verdicts.len() >= entry.expected {
            out.push(self.settle(session, entry));
        } else {
            self.pending.insert(session.to_string(), entry);
        }
        out
    }

    /// 게이트 판정 한 건을 넣는다. 보류 중인 Stop 의 판정이 모두 모이면 확정 결과를 돌려준다.
    pub fn verdict(
        &mut self,
        session: &str,
        prompt_id: Option<String>,
        verdict: Verdict,
        block_cap: Option<u32>,
        now: Instant,
    ) -> Option<Settled> {
        if let Some(skip) = self.skip.get_mut(session)
            && *skip > 0
        {
            *skip -= 1;
            if *skip == 0 {
                self.skip.remove(session);
            }
            return None;
        }
        match self.pending.get_mut(session) {
            Some(entry) if same_prompt(&entry.prompt_id, &prompt_id) => {
                entry.verdicts.push(verdict);
                entry.block_cap = entry.block_cap.or(block_cap);
                if entry.verdicts.len() >= entry.expected {
                    let entry = self.pending.remove(session)?;
                    return Some(self.settle(session, entry));
                }
                None
            }
            _ => {
                self.orphans
                    .entry(session.to_string())
                    .or_default()
                    .push_back(Orphan {
                        prompt_id,
                        verdict,
                        block_cap,
                        at: now,
                    });
                None
            }
        }
    }

    /// 판정을 다 받지 못한 채 상한을 넘긴 Stop 을 확정하고 오래된 판정을 버린다.
    pub fn expire(&mut self, now: Instant) -> Vec<Expired> {
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| now.saturating_duration_since(p.since) >= GATE_DECISION_TIMEOUT)
            .map(|(s, _)| s.clone())
            .collect();
        let mut out = Vec::new();
        for session in due {
            if let Some(entry) = self.pending.remove(&session) {
                let missing = entry.expected.saturating_sub(entry.verdicts.len());
                tracing::warn!(
                    "claude stop s{}: {missing} of {} gate decision(s) did not arrive within {}s — treating them as pass",
                    entry.surface_id,
                    entry.expected,
                    GATE_DECISION_TIMEOUT.as_secs()
                );
                // 다음 Stop 이 오기 전에 늦게 오는 판정은 그 Stop 과 짝짓지 않는다.
                *self.skip.entry(session.clone()).or_default() += missing;
                let settled = self.settle(&session, entry);
                let turn = self.turn(&session);
                out.push(Expired {
                    settled,
                    session,
                    turn,
                });
            }
        }
        self.orphans.retain(|_, queue| {
            queue.retain(|o| now.saturating_duration_since(o.at) < GATE_DECISION_TIMEOUT);
            !queue.is_empty()
        });
        out
    }

    /// 새 턴 이벤트에서 보류 중인 Stop 을 확정한다. 새 프롬프트가 왔다면 그 Stop 에서 턴이 끝났다.
    pub fn new_turn(&mut self, session: &str) -> Option<Settled> {
        let entry = self.pending.remove(session);
        self.forget(session);
        entry.map(|e| Settled {
            surface_id: e.surface_id,
            session: session.to_string(),
            kind: e.kind,
            turn_ended: true,
            final_answer: e.final_answer,
            turn: e.turn,
        })
    }

    /// 세션 종료에서 보류 기록을 버린다. 종료 처리가 idle·완료 알림을 따로 보낸다.
    pub fn end_session(&mut self, session: &str) {
        self.pending.remove(session);
        self.forget(session);
    }

    /// 확정한 뒤로 이 세션에 새 턴이나 세션 종료가 없었는가.
    pub fn is_current_turn(&self, session: &str, turn: u64) -> bool {
        self.turn(session) == turn
    }

    fn turn(&self, session: &str) -> u64 {
        self.turns.get(session).copied().unwrap_or(0)
    }

    fn forget(&mut self, session: &str) {
        self.orphans.remove(session);
        self.skip.remove(session);
        self.blocks.remove(session);
        *self.turns.entry(session.to_string()).or_default() += 1;
    }

    /// 판정을 확정한다. block 이 있어도 Claude Code 의 연속 block 상한에 닿았으면 턴이 끝난다.
    fn settle(&mut self, session: &str, entry: PendingStop) -> Settled {
        let blocked = entry.verdicts.contains(&Verdict::Block);
        let counter = self
            .blocks
            .entry(session.to_string())
            .or_insert((entry.prompt_id.clone(), 0));
        if !same_prompt(&counter.0, &entry.prompt_id) {
            *counter = (entry.prompt_id.clone(), 0);
        }
        let cap = entry.block_cap.unwrap_or(DEFAULT_BLOCK_CAP);
        let turn_ended = if !blocked {
            counter.1 = 0;
            true
        } else if counter.1 >= cap {
            tracing::info!(
                "claude stop s{}: gate blocked after {} consecutive continuation(s) — Claude Code ignores blocks past its cap ({cap}), so the turn ends",
                entry.surface_id,
                counter.1
            );
            counter.1 = 0;
            true
        } else {
            counter.1 += 1;
            false
        };
        Settled {
            surface_id: entry.surface_id,
            session: session.to_string(),
            kind: entry.kind,
            turn_ended,
            final_answer: entry.final_answer,
            turn: entry.turn,
        }
    }
}

/// 게이트 응답에서 판정을 읽는다. `decision: "block"` 이 아니면 pass 다.
pub(crate) fn verdict_of(response: &Value) -> Verdict {
    if response.get("decision").and_then(Value::as_str) == Some("block") {
        Verdict::Block
    } else {
        Verdict::Pass
    }
}

/// `--block-cap` 값을 읽는다. 비었거나 숫자가 아니면 None 이며 기본값을 쓴다.
pub(crate) fn block_cap_param(params: &Value) -> Option<u32> {
    let raw = params.get("block_cap")?;
    match raw {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) if s.trim().is_empty() => None,
        Value::String(s) => s
            .trim()
            .parse()
            .inspect_err(|e| {
                tracing::warn!("claude checklist-hook: unreadable --block-cap {s:?} ({e})")
            })
            .ok(),
        _ => None,
    }
}

/// settings JSON 의 `hooks.Stop` 에서 게이트 명령 수를 센다. Claude Code 는 같은 명령을 한 번만
/// 실행하므로(공식 hooks 문서 "same handler … runs once") 같은 명령 문자열은 한 번만 센다.
pub(crate) fn count_gate_commands(settings: &Value) -> usize {
    settings
        .pointer("/hooks/Stop")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
        .filter(|command| command.contains(GATE_COMMAND))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// settings 파일을 읽어 게이트 명령 수를 센다. 읽지 못하면 0 이며 Stop 을 보류하지 않는다.
pub(crate) fn count_gate_commands_in_file(path: &std::path::Path) -> usize {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "claude stop: cannot read attached settings {} ({e}) — no gate assumed",
                path.display()
            );
            return 0;
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => count_gate_commands(&v),
        Err(e) => {
            tracing::warn!(
                "claude stop: attached settings {} is not JSON ({e}) — no gate assumed",
                path.display()
            );
            0
        }
    }
}

/// 플러그인이 Claude 를 실행하며 붙인 settings 경로를 기록한다. 없으면 지운다.
pub(crate) fn record_settings_file<H: HostCall>(host: &H, surface_id: u32, path: Option<&str>) {
    let result = match path {
        Some(p) => host.call(
            "surface.meta.set",
            json!({ "surface_id": surface_id, "key": SETTINGS_FILE_META_KEY, "value": p }),
        ),
        None => host.call(
            "surface.meta.unset",
            json!({ "surface_id": surface_id, "key": SETTINGS_FILE_META_KEY }),
        ),
    };
    if let Err(e) = result {
        tracing::warn!("claude s{surface_id}: failed to record the attached settings file: {e}");
    }
    // 새 경로는 아직 어느 세션의 것도 아니다. 다음 SessionStart 가 소유 세션을 기록한다.
    if let Err(e) = host.call(
        "surface.meta.unset",
        json!({ "surface_id": surface_id, "key": SETTINGS_SESSION_META_KEY }),
    ) {
        tracing::warn!("claude s{surface_id}: failed to reset the settings owner session: {e}");
    }
}

/// settings 경로를 소유한 세션 id. 없으면 `None` 이다.
pub(crate) fn settings_owner_session<H: HostCall>(host: &H, surface_id: u32) -> Option<String> {
    meta_value(host, surface_id, SETTINGS_SESSION_META_KEY)
}

/// `--resume` 으로 다시 연 세션은 복원 명령이 붙인 `--settings` 로 실행됐다고 보고 그 경로를
/// 게이트 수를 셀 settings meta 로 기록한다. 플러그인이 실행하지 않은 Claude 에는 다른 기록 경로가 없다.
/// 새로 시작한 세션(`startup`)은 플러그인이 실행할 때 기록한 meta 를 그대로 쓴다.
pub(crate) fn settings_file_for_resumed_session(
    surface_id: u32,
    source: Option<&str>,
    profile_file: Option<&str>,
) -> Option<PlannedCall> {
    let path = profile_file.filter(|_| source == Some("resume"))?;
    Some(PlannedCall::MetaSet {
        surface_id,
        key: SETTINGS_FILE_META_KEY,
        value: path.to_string(),
    })
}

/// Claude 프로세스가 끝나면 그 프로세스의 settings meta 를 지운다. 같은 surface 에서 다음에 실행한
/// Claude 의 게이트로 세지 않기 위해서다. `/clear`·`/resume` 의 SessionEnd(`clear`·`resume`)는
/// 같은 프로세스가 이어지므로 남긴다. 값이 없거나 다른 값이면 종료로 본다(공식 hooks 문서의 SessionEnd reason).
/// 경로를 소유한 세션(`owner`)이 끝날 때만 지운다. respawn·reboot 가 새 경로를 기록한 뒤 도착한
/// 이전 세션의 SessionEnd 는 소유 세션이 아니므로 새 경로를 남긴다.
pub(crate) fn settings_file_after_session_end(
    surface_id: u32,
    reason: Option<&str>,
    session: Option<&str>,
    owner: Option<&str>,
) -> Vec<PlannedCall> {
    if matches!(reason, Some("clear" | "resume")) || session.is_none() || session != owner {
        return Vec::new();
    }
    vec![
        PlannedCall::MetaUnset {
            surface_id,
            key: SETTINGS_FILE_META_KEY,
        },
        PlannedCall::MetaUnset {
            surface_id,
            key: SETTINGS_SESSION_META_KEY,
        },
    ]
}

pub(crate) fn meta_value<H: HostCall>(host: &H, surface_id: u32, key: &str) -> Option<String> {
    host.call(
        "surface.meta.get",
        json!({ "surface_id": surface_id, "key": key }),
    )
    .ok()
    .and_then(|r| r.get("value").and_then(Value::as_str).map(String::from))
    .filter(|s| !s.is_empty())
}

/// Stop 을 보낸 세션에 부착된 게이트 수. 지금 실행 중인 Claude 의 `--settings` 경로(meta)에서만 센다.
/// 플러그인이 실행·재시작한 Claude 와 `--settings` 를 붙여 복원한 세션만 이 meta 를 가진다.
/// Claude 가 끝나면 meta 를 지우므로, 같은 surface 에서 사용자가 직접 실행한 Claude 는 0 으로 센다.
/// 프로필 meta 는 surface 에 남아 다음 실행에 다시 붙이는 용도라, 지금 프로세스의 게이트를 뜻하지 않는다.
pub(crate) fn attached_gate_count<H: HostCall>(host: &H, surface_id: u32) -> usize {
    meta_value(host, surface_id, SETTINGS_FILE_META_KEY)
        .map(|path| count_gate_commands_in_file(std::path::Path::new(&path)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "sess-1";

    fn p(id: &str) -> Option<String> {
        Some(id.to_string())
    }

    fn at(surface_id: u32, prompt_id: Option<String>, kind: StopKind) -> HeldStop {
        HeldStop {
            surface_id,
            prompt_id,
            kind,
            final_answer: None,
            turn: None,
        }
    }

    fn idle(surface_id: u32) -> Settled {
        Settled {
            surface_id,
            session: S.to_string(),
            kind: StopKind::Idle,
            turn_ended: true,
            final_answer: None,
            turn: None,
        }
    }

    fn settled(expired: Vec<Expired>) -> Vec<Settled> {
        expired.into_iter().map(|e| e.settled).collect()
    }

    fn blocked(surface_id: u32) -> Settled {
        Settled {
            surface_id,
            session: S.to_string(),
            kind: StopKind::Idle,
            turn_ended: false,
            final_answer: None,
            turn: None,
        }
    }

    #[test]
    fn a_blocked_stop_keeps_the_turn_in_either_arrival_order() {
        let t = Instant::now();
        // Stop 먼저.
        let mut a = StopPairing::default();
        assert!(a.stop(S, at(3, p("x"), StopKind::Idle), 1, t).is_empty());
        assert_eq!(
            a.verdict(S, p("x"), Verdict::Block, None, t),
            Some(blocked(3))
        );
        // 판정 먼저.
        let mut b = StopPairing::default();
        assert_eq!(b.verdict(S, p("x"), Verdict::Block, None, t), None);
        assert_eq!(
            b.stop(S, at(3, p("x"), StopKind::Idle), 1, t),
            vec![blocked(3)]
        );
    }

    #[test]
    fn a_stop_every_gate_passed_ends_the_turn() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        assert!(a.stop(S, at(3, p("x"), StopKind::Idle), 2, t).is_empty());
        assert_eq!(a.verdict(S, p("x"), Verdict::Pass, None, t), None);
        let settled = a.verdict(S, p("x"), Verdict::Pass, None, t).unwrap();
        assert_eq!(settled, idle(3));
        assert!(settled.runs_idle());
    }

    #[test]
    fn one_block_among_several_gates_keeps_the_turn() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Idle), 2, t);
        a.verdict(S, p("x"), Verdict::Pass, None, t);
        assert_eq!(
            a.verdict(S, p("x"), Verdict::Block, None, t),
            Some(blocked(3))
        );
    }

    #[test]
    fn a_missing_decision_ends_the_turn_after_the_timeout_once() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        assert!(a.expire(t + GATE_DECISION_TIMEOUT / 2).is_empty());
        assert_eq!(settled(a.expire(t + GATE_DECISION_TIMEOUT)), vec![idle(3)]);
        assert!(
            a.expire(t + GATE_DECISION_TIMEOUT * 2).is_empty(),
            "한 번만"
        );
        // 늦게 온 판정은 확정된 Stop 을 다시 바꾸지 않고 다음 Stop 과도 짝짓지 않는다.
        assert_eq!(a.verdict(S, p("x"), Verdict::Block, None, t), None);
        let later = t + GATE_DECISION_TIMEOUT * 3;
        a.stop(S, at(3, p("y"), StopKind::Idle), 1, later);
        assert_eq!(
            a.verdict(S, p("y"), Verdict::Pass, None, later),
            Some(idle(3))
        );
    }

    #[test]
    fn a_block_at_the_continuation_cap_counts_as_the_end_of_the_turn() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        for _ in 0..DEFAULT_BLOCK_CAP {
            a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
            assert_eq!(
                a.verdict(S, p("x"), Verdict::Block, None, t),
                Some(blocked(3))
            );
        }
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        assert_eq!(
            a.verdict(S, p("x"), Verdict::Block, None, t),
            Some(idle(3)),
            "상한만큼 이어진 뒤의 block 은 Claude Code 가 무시한다"
        );
    }

    #[test]
    fn the_block_cap_from_the_gate_command_replaces_the_default() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        assert_eq!(
            a.verdict(S, p("x"), Verdict::Block, Some(1), t),
            Some(blocked(3))
        );
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        assert_eq!(
            a.verdict(S, p("x"), Verdict::Block, Some(1), t),
            Some(idle(3))
        );
    }

    #[test]
    fn a_new_prompt_resets_the_consecutive_blocks() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        a.verdict(S, p("x"), Verdict::Block, Some(1), t);
        a.stop(S, at(3, p("y"), StopKind::Idle), 1, t);
        assert_eq!(
            a.verdict(S, p("y"), Verdict::Block, Some(1), t),
            Some(blocked(3))
        );
    }

    #[test]
    fn a_waiting_stop_consumes_its_gate_decision() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Waiting), 1, t);
        let settled = a.verdict(S, p("x"), Verdict::Block, None, t).unwrap();
        assert_eq!(settled.kind, StopKind::Waiting);
        assert!(!settled.runs_idle());
        // 대기 Stop 의 판정이 다음 Stop 에 섞이지 않는다.
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        assert_eq!(a.verdict(S, p("x"), Verdict::Pass, None, t), Some(idle(3)));
    }

    #[test]
    fn a_second_stop_before_the_first_is_decided_settles_the_first_as_blocked() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        assert_eq!(
            a.stop(S, at(3, p("x"), StopKind::Idle), 1, t),
            vec![blocked(3)]
        );
        // 둘째 Stop 이 왔으면 첫 Stop 의 판정은 더 오지 않는다. 이어서 오는 판정은 둘째 Stop 의 것이다.
        assert_eq!(a.verdict(S, p("x"), Verdict::Pass, None, t), Some(idle(3)));
    }

    /// 한 Stop 의 훅이 모두 끝나야 턴이 이어지므로, 다음 Stop 이 오면 앞 Stop 의 오지 않은 판정은 오지 않는다.
    /// 그 빈자리가 다음 Stop 의 실제 판정을 삼키면 block 이 idle 로 확정된다.
    #[test]
    fn a_lost_decision_does_not_swallow_the_next_stop_of_the_same_turn() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        // 게이트 둘. Stop1 에서 A 는 block, B 의 판정은 오지 않는다.
        assert!(a.stop(S, at(3, p("x"), StopKind::Idle), 2, t).is_empty());
        assert_eq!(a.verdict(S, p("x"), Verdict::Block, None, t), None);
        assert_eq!(
            settled(a.expire(t + GATE_DECISION_TIMEOUT)),
            vec![blocked(3)]
        );
        // 턴이 이어져 같은 prompt 의 Stop2 가 오고 A 가 다시 block 한다.
        let t2 = t + GATE_DECISION_TIMEOUT * 2;
        assert!(a.stop(S, at(3, p("x"), StopKind::Idle), 2, t2).is_empty());
        assert_eq!(a.verdict(S, p("x"), Verdict::Block, None, t2), None);
        assert_eq!(
            settled(a.expire(t2 + GATE_DECISION_TIMEOUT)),
            vec![blocked(3)],
            "Stop2 의 block 이 Stop1 의 빈자리에 삼켜지면 안 된다"
        );
    }

    #[test]
    fn stale_or_foreign_decisions_do_not_pair_with_a_new_stop() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.verdict(S, p("old"), Verdict::Block, None, t);
        a.verdict(S, p("x"), Verdict::Block, None, t);
        let late = t + GATE_DECISION_TIMEOUT;
        assert!(a.stop(S, at(3, p("x"), StopKind::Idle), 1, late).is_empty());
        assert_eq!(
            a.verdict(S, p("x"), Verdict::Pass, None, late),
            Some(idle(3))
        );
        // 다른 세션의 판정과도 섞이지 않는다.
        let mut b = StopPairing::default();
        b.verdict("other", p("x"), Verdict::Block, None, t);
        assert!(b.stop(S, at(3, p("x"), StopKind::Idle), 1, t).is_empty());
    }

    #[test]
    fn a_new_turn_settles_the_pending_stop_as_ended_and_session_end_drops_it() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        assert_eq!(a.new_turn(S), Some(idle(3)));
        assert_eq!(a.new_turn(S), None);
        a.stop(S, at(3, p("y"), StopKind::Idle), 1, t);
        a.end_session(S);
        assert!(a.expire(t + GATE_DECISION_TIMEOUT).is_empty());
    }

    #[test]
    fn a_timed_out_stop_knows_whether_a_new_turn_started_since() {
        let t = Instant::now();
        let mut a = StopPairing::default();
        a.stop(S, at(3, p("x"), StopKind::Idle), 1, t);
        let expired = a.expire(t + GATE_DECISION_TIMEOUT);
        assert_eq!(expired.len(), 1);
        let e = &expired[0];
        assert!(a.is_current_turn(&e.session, e.turn));
        assert_eq!(a.new_turn(S), None, "이미 확정돼 보류가 없다");
        assert!(
            !a.is_current_turn(&e.session, e.turn),
            "새 턴이 세대를 올린다"
        );
        a.end_session(S);
        assert!(
            !a.is_current_turn(&e.session, e.turn + 1),
            "세션 종료도 세대를 올린다"
        );
        assert!(a.is_current_turn("other", 0));
    }

    /// `--resume` 으로 다시 연 세션은 복원 명령이 붙인 settings 를 게이트 수를 셀 meta 로 기록한다.
    /// 새로 시작한 세션은 플러그인이 실행할 때 기록한 meta 를 건드리지 않는다.
    #[test]
    fn a_resumed_session_records_the_settings_it_was_restored_with() {
        let plan = Some("/p/merged.json");
        assert_eq!(
            settings_file_for_resumed_session(7, Some("resume"), plan),
            Some(PlannedCall::MetaSet {
                surface_id: 7,
                key: SETTINGS_FILE_META_KEY,
                value: "/p/merged.json".to_string(),
            })
        );
        for source in [Some("startup"), Some("clear"), Some("compact"), None] {
            assert_eq!(
                settings_file_for_resumed_session(7, source, plan),
                None,
                "{source:?}"
            );
        }
        assert_eq!(
            settings_file_for_resumed_session(7, Some("resume"), None),
            None
        );
    }

    /// 실행 기록은 settings 경로와 함께 소유 세션을 지운다. 다음 SessionStart 가 새로 기록한다.
    #[test]
    fn recording_the_settings_resets_the_owner_session() {
        struct Calls(std::cell::RefCell<Vec<(String, String)>>);
        impl HostCall for Calls {
            fn call(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                let key = params["key"].as_str().unwrap_or("").to_string();
                self.0.borrow_mut().push((method.to_string(), key));
                Ok(json!({}))
            }
        }
        for path in [Some("/p/s.json"), None] {
            let host = Calls(Default::default());
            record_settings_file(&host, 3, path);
            assert!(host.0.borrow().contains(&(
                "surface.meta.unset".to_string(),
                SETTINGS_SESSION_META_KEY.to_string()
            )));
        }
    }

    /// surface meta 조회만 답하는 mock 호스트.
    struct MetaHost(Vec<(&'static str, String)>);

    impl HostCall for MetaHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            let key = params["key"].as_str().unwrap_or("");
            match self
                .0
                .iter()
                .find(|(k, _)| method == "surface.meta.get" && *k == key)
            {
                Some((_, v)) => Ok(json!({ "value": v })),
                None => Ok(json!({})),
            }
        }
    }

    fn settings_with_gates(dir: &std::path::Path, name: &str, gates: &[&str]) -> String {
        let hooks: Vec<Value> = gates
            .iter()
            .map(|g| json!({ "type": "command", "command": format!("tasty claude checklist-hook --gate {g}") }))
            .collect();
        let path = dir.join(name);
        std::fs::write(
            &path,
            json!({ "hooks": { "Stop": [{ "hooks": hooks }] } }).to_string(),
        )
        .unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn the_gate_count_reads_only_the_settings_meta_of_the_running_claude() {
        let dir = tempfile::tempdir().unwrap();
        let one = settings_with_gates(dir.path(), "one.json", &["a"]);
        let two = settings_with_gates(dir.path(), "two.json", &["a", "b"]);
        let count = |host: MetaHost| attached_gate_count(&host, 3);
        assert_eq!(count(MetaHost(vec![])), 0);
        assert_eq!(
            count(MetaHost(vec![(SETTINGS_FILE_META_KEY, one.clone())])),
            1
        );
        // 프로필 meta 는 다음 실행에 다시 붙일 프로필이다. 지금 실행 중인 Claude 의 게이트로 세지 않는다.
        assert_eq!(
            count(MetaHost(vec![
                (crate::reboot::PROFILE_META_KEY, two.clone()),
                (
                    crate::reboot::PROFILE_NAMES_META_KEY,
                    "continue-checklist".to_string()
                )
            ])),
            0
        );
        assert_eq!(
            count(MetaHost(vec![
                (SETTINGS_FILE_META_KEY, one),
                (crate::reboot::PROFILE_META_KEY, two)
            ])),
            1
        );
    }

    #[test]
    fn the_same_gate_command_is_counted_once() {
        let command = "tasty claude checklist-hook --gate a";
        let settings = json!({ "hooks": { "Stop": [
            { "matcher": "", "hooks": [{ "type": "command", "command": command }] },
            { "hooks": [{ "type": "command", "command": command, "timeout": 30 }] },
        ]}});
        assert_eq!(count_gate_commands(&settings), 1);
    }

    #[test]
    fn gate_commands_are_counted_in_the_stop_hooks() {
        let settings = json!({ "hooks": {
            "Stop": [
                { "matcher": "", "hooks": [
                    { "type": "command", "command": "if [ -n \"$TASTY_SURFACE_ID\" ]; then tasty claude checklist-hook --gate a || true; fi" },
                    { "type": "command", "command": "echo other" },
                ]},
                { "matcher": "", "hooks": [
                    { "type": "command", "command": "tasty claude checklist-hook --gate b" },
                ]},
            ],
            "SessionStart": [
                { "matcher": "", "hooks": [
                    { "type": "command", "command": "tasty claude checklist-hook --gate c" },
                ]},
            ],
        }});
        assert_eq!(count_gate_commands(&settings), 2);
        assert_eq!(count_gate_commands(&json!({})), 0);
    }

    #[test]
    fn verdict_and_block_cap_are_read_from_the_gate() {
        assert_eq!(
            verdict_of(&json!({ "decision": "block", "reason": "r" })),
            Verdict::Block
        );
        assert_eq!(verdict_of(&json!({})), Verdict::Pass);
        assert_eq!(block_cap_param(&json!({ "block_cap": "12" })), Some(12));
        assert_eq!(block_cap_param(&json!({ "block_cap": "" })), None);
        assert_eq!(block_cap_param(&json!({ "block_cap": "x" })), None);
        assert_eq!(block_cap_param(&json!({})), None);
    }
}
