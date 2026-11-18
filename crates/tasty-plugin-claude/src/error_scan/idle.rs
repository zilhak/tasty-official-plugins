//! 부모가 등록하는 Claude 자식의 idle 체크.
//!
//! 체크 하나는 (대상 surface, 받을 부모 surface) 한 쌍에 하나다. 등록 시각부터 대상의 `terminal.state` 를 감시 회차마다
//! 묻고, `active` 를 볼 때마다 기준 시각을 그때로 옮긴다. 근거(evidence)는 가리지 않는다. 마지막 `active` 뒤
//! [`IDLE_CHECK_WAIT`] 가 지나도록 확정된 완료가 없으면 부모 완료 로그에 idle 줄 하나를 쓰고 체크를 끝낸다.
//!
//! - 확정된 완료(턴 종료 Stop·StopFailure, 게이트 확정, 세션 종료, 원격 턴 결과)는 그 Stop 을 받은 시각 이전에 등록한
//!   체크를 지운다. 그 뒤에 등록한 체크는 남는다. 늦게 확정된 앞 턴의 완료가 새 등록을 지우지 않게 하려는 것이다.
//! - 같은 부모·대상을 다시 등록하면 새 id 의 체크로 바꾼다. 앞 체크의 진행 중인 판정은 id 가 달라 버린다.
//! - 상태 조회 실패는 비활동으로 세지 않는다. 그 회차에는 줄을 쓰지 않는다.
//! - 부모 로그 쓰기에 실패하면 체크를 남기고 [`DELIVERY_RETRY`] 뒤에 조건을 다시 확인해 쓴다. 쓰기에 성공한 체크만 끝낸다.
//! - 백그라운드 작업 대기(대기 Stop 기록)와 surface 전체 무출력 선언(`claude quiet`)이 있는 동안은 `active` 처럼
//!   기준 시각을 옮긴다. 그 동안의 조용함은 기존 무출력 알림 규칙이 맡는다.
//! - 등록은 대상 surface meta [`IDLE_CHECK_META_KEY`] 에 남긴다. 플러그인이 다시 시작되면 남은 등록만 되살리고
//!   그 시각부터 [`IDLE_CHECK_WAIT`] 를 다시 센다. 끝난 체크는 meta 에서 지우므로 되살아나지 않는다.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use super::{ErrorScanner, lock_scanner, unix_now_ms};
use crate::notice::NoticeKind;

/// 마지막 `active` 관측 뒤 idle 줄을 쓰기까지 기다리는 시간.
pub(crate) const IDLE_CHECK_WAIT: Duration = Duration::from_secs(120);

/// 부모 로그 쓰기에 실패한 체크를 다시 시도하기까지 기다리는 시간.
pub(crate) const DELIVERY_RETRY: Duration = Duration::from_secs(30);

/// 상태 조회 실패를 경고로 남기는 간격(회차 수). 감시 회차는 약 0.8초다.
const STATE_ERROR_WARN_EVERY: u32 = 75;

/// 등록한 체크를 남기는 대상 surface meta. 재시작 뒤 복원에 쓴다.
pub(crate) const IDLE_CHECK_META_KEY: &str = "claude-idle-checks";

/// 체크를 건 경로. 응답·meta·로그에 적는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// `claude idle-check` 로 따로 등록했다.
    Manual,
    Tell,
    Spawn,
    Respawn,
}

impl Origin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "idle-check",
            Self::Tell => "tell",
            Self::Spawn => "spawn",
            Self::Respawn => "respawn",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "tell" => Self::Tell,
            "spawn" => Self::Spawn,
            "respawn" => Self::Respawn,
            _ => Self::Manual,
        }
    }
}

struct Check {
    id: u64,
    origin: Origin,
    /// 소유 판정에 쓰는 등록 시각. 이 시각 이후에 받은 Stop 의 확정만 이 체크를 끝낸다.
    registered_at: Instant,
    registered_ms: u64,
    /// 마지막으로 `active` 를 본 시각(처음에는 등록 시각).
    last_active: Instant,
    /// 마지막으로 읽은 상태. 줄에 적는다.
    last_state: Option<String>,
    /// 부모 로그 쓰기 실패 뒤 다시 시도할 수 있는 시각.
    retry_at: Option<Instant>,
    delivery_failures: u32,
    state_errors: u32,
}

/// idle 체크 표. [`ErrorScanner`] 잠금 아래에 둔다.
#[derive(Default)]
pub(crate) struct IdleChecks {
    checks: HashMap<(u32, u32), Check>,
    /// 대상별 마지막 확정 완료의 Stop 수신 시각. 그 뒤에 넣는 등록이 이미 지난 완료와 겹치는지 가린다.
    completed_at: HashMap<u32, Instant>,
    next_id: u64,
}

/// 등록 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registered {
    pub id: u64,
    /// 같은 부모·대상의 앞 체크를 바꿨으면 그 id.
    pub replaced: Option<u64>,
    /// 등록 시각 뒤의 완료를 이미 받아 체크를 바로 끝냈다.
    pub completed: bool,
}

/// 감시 회차에 판정할 체크 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Probe {
    pub target: u32,
    pub caller: u32,
    pub id: u64,
}

/// 대상 상태 조회 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observation {
    Active,
    /// `active`·`exited` 가 아닌 상태(idle·needs_input·stale 등).
    NotActive(String),
    /// 프로세스가 끝났다.
    Exited,
    /// surface 가 없다.
    Gone,
    /// 조회에 실패했다. 비활동으로 세지 않는다.
    Error(String),
}

/// 한 회차 판정의 결과. 시험이 읽는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// 체크가 바뀌었거나 끝났다. 이 판정은 버린다.
    Stale,
    Waiting,
    Delivered,
    DeliveryFailed,
    Dropped,
}

impl IdleChecks {
    fn new_id(&mut self) -> u64 {
        if self.next_id == 0 {
            // 재시작 전 id 와 겹치지 않도록 시작 시각에서 센다.
            self.next_id = unix_now_ms().max(1);
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 체크를 넣는다. 같은 부모·대상의 앞 체크는 바꾼다. 등록 시각 이후의 완료를 이미 받았으면 넣지 않는다.
    pub(crate) fn register(
        &mut self,
        target: u32,
        caller: u32,
        origin: Origin,
        registered_at: Instant,
        registered_ms: u64,
    ) -> Registered {
        let id = self.new_id();
        if self
            .completed_at
            .get(&target)
            .is_some_and(|done| *done >= registered_at)
        {
            let replaced = self.checks.remove(&(target, caller)).map(|c| c.id);
            return Registered {
                id,
                replaced,
                completed: true,
            };
        }
        let replaced = self
            .checks
            .insert(
                (target, caller),
                Check {
                    id,
                    origin,
                    registered_at,
                    registered_ms,
                    last_active: registered_at,
                    last_state: None,
                    retry_at: None,
                    delivery_failures: 0,
                    state_errors: 0,
                },
            )
            .map(|c| c.id);
        Registered {
            id,
            replaced,
            completed: false,
        }
    }

    /// 확정된 완료. `stopped_at` 이전에 등록한 체크를 지우고 지운 부모를 돌려준다.
    pub(crate) fn complete(&mut self, target: u32, stopped_at: Instant) -> Vec<u32> {
        let done = self.completed_at.entry(target).or_insert(stopped_at);
        if stopped_at > *done {
            *done = stopped_at;
        }
        let ended: Vec<(u32, u32)> = self
            .checks
            .iter()
            .filter(|((t, _), c)| *t == target && c.registered_at <= stopped_at)
            .map(|(k, _)| *k)
            .collect();
        ended
            .into_iter()
            .filter_map(|k| self.checks.remove(&k).map(|_| k.1))
            .collect()
    }

    /// 대상의 체크를 모두 지운다(세션 교체·추적 종료). 완료 기록도 지운다.
    pub(crate) fn drop_target(&mut self, target: u32) -> usize {
        self.completed_at.remove(&target);
        let before = self.checks.len();
        self.checks.retain(|(t, _), _| *t != target);
        before - self.checks.len()
    }

    pub(crate) fn probes(&self) -> Vec<Probe> {
        let mut out: Vec<Probe> = self
            .checks
            .iter()
            .map(|(&(target, caller), c)| Probe {
                target,
                caller,
                id: c.id,
            })
            .collect();
        out.sort_by_key(|p| (p.target, p.caller));
        out
    }

    fn current(&mut self, probe: Probe) -> Option<&mut Check> {
        self.checks
            .get_mut(&(probe.target, probe.caller))
            .filter(|c| c.id == probe.id)
    }

    /// 시험용 — 대상의 체크 (부모, id) 목록.
    #[cfg(test)]
    pub(crate) fn of_target(&self, target: u32) -> Vec<(u32, u64)> {
        let mut out: Vec<(u32, u64)> = self
            .checks
            .iter()
            .filter(|((t, _), _)| *t == target)
            .map(|((_, caller), c)| (*caller, c.id))
            .collect();
        out.sort_unstable();
        out
    }

    /// meta 에 남길 값. 체크가 없으면 `None`.
    fn meta_value(&self, target: u32) -> Option<String> {
        let mut checks: Vec<Value> = self
            .checks
            .iter()
            .filter(|((t, _), _)| *t == target)
            .map(|((_, caller), c)| {
                json!({
                    "caller": caller,
                    "id": c.id,
                    "origin": c.origin.as_str(),
                    "registered_ms": c.registered_ms,
                })
            })
            .collect();
        if checks.is_empty() {
            return None;
        }
        checks.sort_by_key(|v| v["caller"].as_u64());
        Some(json!({ "checks": checks }).to_string())
    }
}

/// meta 에 남은 등록(부모, 경로, 등록 Unix ms). 읽을 수 없는 항목은 건너뛴다.
pub(crate) fn parse_meta(raw: &str) -> Vec<(u32, Origin, u64)> {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        tracing::warn!("claude idle check meta is not JSON: {raw}");
        return Vec::new();
    };
    v.get("checks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let caller = u32::try_from(c.get("caller")?.as_u64()?).ok()?;
                    let origin = Origin::parse(c.get("origin").and_then(Value::as_str)?);
                    let ms = c.get("registered_ms").and_then(Value::as_u64).unwrap_or(0);
                    Some((caller, origin, ms))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 대상의 지금 상태를 읽는다. 조회에 실패하면 surface 가 남았는지 확인해 없는 surface 와 조회 오류를 가른다.
pub(crate) fn observe<H: HostCall>(host: &H, target: u32) -> Observation {
    match host.call("terminal.state", json!({ "surface": target })) {
        Ok(reply) => match reply.get("state").and_then(Value::as_str) {
            Some(s) if s.eq_ignore_ascii_case("active") => Observation::Active,
            Some(s) if s.eq_ignore_ascii_case("exited") => Observation::Exited,
            Some(s) => Observation::NotActive(s.to_ascii_lowercase()),
            None => Observation::Error(format!("terminal.state reply without state: {reply}")),
        },
        Err(e) => match host.call("surface.locate", json!({ "surface_id": target })) {
            Ok(r) if r.get("exists").and_then(Value::as_bool) == Some(false) => Observation::Gone,
            _ => Observation::Error(e.to_string()),
        },
    }
}

/// 부모 로그에 쓸 idle 줄. `template` 은 `claude.notify.idle_check_message` 다.
pub(crate) fn idle_line(template: &str, target: u32, quiet: Duration, state: &str) -> String {
    tasty_plugin_sdk::i18n::fill_args(
        template,
        &[&target.to_string(), &quiet.as_secs().to_string(), state],
    )
}

impl ErrorScanner {
    /// 체크를 등록하고 meta 에 남긴다.
    pub(crate) fn idle_register<H: HostCall>(
        &mut self,
        host: &H,
        target: u32,
        caller: u32,
        origin: Origin,
        registered_at: Instant,
    ) -> Registered {
        let registered = self
            .idle
            .register(target, caller, origin, registered_at, unix_now_ms());
        tracing::info!(
            "claude idle check s{target}→s{caller}: registered #{} ({}){}{}",
            registered.id,
            origin.as_str(),
            registered
                .replaced
                .map(|id| format!(", replacing #{id}"))
                .unwrap_or_default(),
            if registered.completed {
                ", already finished"
            } else {
                ""
            }
        );
        self.save_idle(host, target);
        registered
    }

    /// 확정된 완료로 체크를 끝낸다. 남은 체크가 바뀌었으면 meta 를 다시 쓴다.
    pub(crate) fn idle_complete<H: HostCall>(
        &mut self,
        host: &H,
        target: u32,
        stopped_at: Instant,
    ) {
        let ended = self.idle.complete(target, stopped_at);
        if !ended.is_empty() {
            tracing::info!(
                "claude idle check s{target}: finished turn confirmed — ended the check(s) of parent(s) {ended:?}"
            );
            self.save_idle(host, target);
        }
    }

    /// 대상의 체크를 모두 지운다(kill·release·respawn·세션 교체).
    pub(crate) fn idle_drop<H: HostCall>(&mut self, host: &H, target: u32) {
        if self.idle.drop_target(target) > 0 {
            tracing::info!("claude idle check s{target}: target ended — check(s) dropped");
            self.save_idle(host, target);
        }
    }

    fn save_idle<H: HostCall>(&self, host: &H, target: u32) {
        let result = match self.idle.meta_value(target) {
            Some(value) => host.call(
                "surface.meta.set",
                json!({ "surface_id": target, "key": IDLE_CHECK_META_KEY, "value": value }),
            ),
            None => host.call(
                "surface.meta.unset",
                json!({ "surface_id": target, "key": IDLE_CHECK_META_KEY }),
            ),
        };
        if let Err(e) = result {
            tracing::warn!(
                "claude idle check s{target}: writing {IDLE_CHECK_META_KEY} failed: {e}"
            );
        }
    }

    /// 재시작 전 meta 의 등록을 되살린다. 이미 있는 체크는 두고, 되살린 수를 돌려준다.
    pub(crate) fn idle_restore<H: HostCall>(
        &mut self,
        host: &H,
        target: u32,
        now: Instant,
    ) -> usize {
        let Some(raw) = crate::stop_pairing::meta_value(host, target, IDLE_CHECK_META_KEY) else {
            return 0;
        };
        let mut restored = 0;
        for (caller, origin, ms) in parse_meta(&raw) {
            if self.idle.checks.contains_key(&(target, caller)) {
                continue;
            }
            let id = self.idle.new_id();
            self.idle.checks.insert(
                (target, caller),
                Check {
                    id,
                    origin,
                    registered_at: now,
                    registered_ms: ms,
                    last_active: now,
                    last_state: None,
                    retry_at: None,
                    delivery_failures: 0,
                    state_errors: 0,
                },
            );
            restored += 1;
        }
        if restored > 0 {
            tracing::info!(
                "claude idle check s{target}: restored {restored} check(s) — counting {}s again from now",
                IDLE_CHECK_WAIT.as_secs()
            );
            self.save_idle(host, target);
        }
        restored
    }

    /// 대상이 기다림을 선언했는가. 백그라운드 작업 대기나 surface 전체 무출력 선언이 있으면 시간을 세지 않는다.
    fn idle_paused<H: HostCall>(&mut self, host: &H, target: u32, now_ms: u64) -> bool {
        if self.background_wait.contains_key(&target) {
            return true;
        }
        if !self.enabled.contains_key(&target) && !self.quiet.contains_key(&target) {
            // 오류 감시 대상이 아니면(원격 mirror 등) 선언도 없다.
            return false;
        }
        self.quiet_state(host, target)
            .decls
            .iter()
            .any(|d| d.task.is_none() && d.until_ms.is_none_or(|u| now_ms < u))
    }

    /// 체크 하나의 한 회차. 조회 결과를 반영하고, 기한이 지났으면 부모 로그에 쓴다.
    pub(crate) fn idle_step<H: HostCall>(
        &mut self,
        host: &H,
        probe: Probe,
        observation: Observation,
        now: Instant,
        template: &str,
    ) -> Step {
        let paused = matches!(observation, Observation::NotActive(_))
            && self.idle_paused(host, probe.target, unix_now_ms());
        let gone = observation == Observation::Gone;
        let Some(check) = self.idle.current(probe) else {
            return Step::Stale;
        };
        let state = match observation {
            Observation::Active => {
                check.last_active = now;
                check.last_state = Some("active".into());
                check.state_errors = 0;
                return Step::Waiting;
            }
            Observation::Exited | Observation::Gone => {
                tracing::info!(
                    "claude idle check s{}→s{}: target {} — check #{} dropped",
                    probe.target,
                    probe.caller,
                    if gone { "surface is gone" } else { "exited" },
                    probe.id
                );
                self.idle.checks.remove(&(probe.target, probe.caller));
                self.save_idle(host, probe.target);
                return Step::Dropped;
            }
            Observation::Error(e) => {
                if check.state_errors % STATE_ERROR_WARN_EVERY == 0 {
                    tracing::warn!(
                        "claude idle check s{}→s{}: reading the state failed ({} time(s)) — not counted as inactivity: {e}",
                        probe.target,
                        probe.caller,
                        check.state_errors + 1
                    );
                }
                check.state_errors += 1;
                return Step::Waiting;
            }
            Observation::NotActive(state) => state,
        };
        check.state_errors = 0;
        check.last_state = Some(state.clone());
        if paused {
            check.last_active = now;
            return Step::Waiting;
        }
        let quiet = now.saturating_duration_since(check.last_active);
        if quiet < IDLE_CHECK_WAIT || check.retry_at.is_some_and(|r| now < r) {
            return Step::Waiting;
        }
        let line = idle_line(template, probe.target, quiet, &state);
        match crate::notice::append(probe.caller, NoticeKind::IdleCheck, &line) {
            Ok(()) => {
                tracing::info!(
                    "claude idle check s{}→s{}: no active state for {}s (state {state}) — notified, check #{} ended",
                    probe.target,
                    probe.caller,
                    quiet.as_secs(),
                    probe.id
                );
                self.idle.checks.remove(&(probe.target, probe.caller));
                self.save_idle(host, probe.target);
                Step::Delivered
            }
            Err(e) => {
                check.delivery_failures += 1;
                check.retry_at = Some(now + DELIVERY_RETRY);
                tracing::warn!(
                    "claude idle check s{}→s{}: writing the idle line to the parent log failed ({} time(s)), retrying in {}s while the target stays inactive: {e}",
                    probe.target,
                    probe.caller,
                    check.delivery_failures,
                    DELIVERY_RETRY.as_secs()
                );
                Step::DeliveryFailed
            }
        }
    }
}

/// 감시 회차 하나. 체크마다 잠금을 풀고 상태를 물은 뒤 다시 잠가 반영한다.
pub(crate) fn run_idle_checks<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    template: &str,
    now: Instant,
) -> Vec<(Probe, Step)> {
    let probes = lock_scanner(scanner).idle.probes();
    probes
        .into_iter()
        .map(|probe| {
            let observation = observe(host, probe.target);
            let step = lock_scanner(scanner).idle_step(host, probe, observation, now, template);
            (probe, step)
        })
        .collect()
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod tests;
