//! Claude 훅을 받아 자식 상태·세션 메타데이터·사용량 기록·화면 알림을 호스트에 요청한다.
//! 상태는 terminal.set_state로 호스트에 전달하고, 완료 알림은 별도로 등록한 훅이 처리한다.
//! 이 모듈은 부모 관계를 조회하지 않는다. 전송할 호출 목록은 apply_hook에서 만든다.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};
// 지역 HostCall enum과 이름을 구분하기 위한 호스트 호출 trait 별칭.
use tasty_plugin_agent_common::host_call::HostCall as HostCallSink;

use crate::checklist;
use crate::error_scan::ErrorScanner;
use crate::profile_attach;
use crate::session_start::{session_lifecycle_calls, session_start_state};
use crate::state::{ClaudeState, lock_state};
use crate::stop_pairing::{Expired, Settled, StopKind, StopPairing, lock_pairing};

/// surface meta 키 — 복원이 셸에 그대로 타이핑하는 재기동 명령
/// (`src/core/layout_persistence/restore.rs`). 값 포맷의 소유자는
/// [`crate::reboot::resume_command_line`] 다.
pub(crate) const RESTORE_COMMAND_META_KEY: &str = "restore.command";

/// surface meta 키 — 직전 턴이 API 에러로 끝났을 때 그 에러 종류(Claude Code
/// `StopFailure` payload 의 `error`: `overloaded` · `rate_limit` · `server_error` …).
/// `stop-failure` 가 쓰고 새 턴(`prompt-submit`/`session-start`/`active`)과
/// `session-end` 가 지운다. 부모 완료 알림 문구가 읽는다.
pub(crate) const STOP_FAILURE_META_KEY: &str = "claude-last-stop-failure";

/// surface meta 키 — 자식이 백그라운드 작업을 기다리는 중일 때 그 대기의 요약(JSON:
/// `since_ms` 대기 시작 Unix ms · `tasks` 끝나지 않은 작업 수 · `types` 작업 종류 목록).
/// 대기 Stop 이 쓰고 턴 종료(`stop`·`stop-failure`)·새 턴·`session-end` 가 지운다.
/// 부모 정지 알림 문구가 읽는다.
pub(crate) const BACKGROUND_WAIT_META_KEY: &str = "claude-background-wait";

/// 백그라운드 작업을 띄운 도구 호출을 알리는 훅 이벤트. 설치가 `PostToolUse` 에 matcher
/// [`crate::background_tasks::BACKGROUND_TOOLS_MATCHER`] 로 등록한다. 상태는 바꾸지 않고 작업만 기록한다.
pub(crate) const BACKGROUND_START_EVENT: &str = "background-start";

/// API 에러로 턴이 끝났을 때 `claude-idle` 과 함께 쏘는 surface hook 이벤트
/// (매니페스트 `contributes.hook_events` 에 선언).
pub(crate) const STOP_FAILURE_EVENT: &str = "claude-stop-failure";

/// hook 처리 후 plugin이 호스트에 보낼 IPC 호출 1건.
#[derive(Debug, Clone, PartialEq)]
pub enum HostCall {
    /// `terminal.set_state { surface, state }` — 호스트 registry 의 자식 상태 갱신.
    /// state ∈ {"idle", "needs_input", "active"}.
    SetState {
        surface_id: u32,
        state: &'static str,
    },
    /// `surface.fire_hook { surface_id, event }`
    FireHook {
        surface_id: u32,
        event: &'static str,
    },
    /// `surface.meta.set { surface_id, key, value }`
    MetaSet {
        surface_id: u32,
        key: &'static str,
        value: String,
    },
    /// `surface.meta.unset { surface_id, key }`
    MetaUnset { surface_id: u32, key: &'static str },
    /// `telemetry.record { metric, value, tags }`
    TelemetryRecord {
        metric: &'static str,
        value: f64,
        surface_id: u32,
    },
    /// surface.completion으로 completion 또는 needs_input 화면 알림을 요청한다.
    /// 자식 상태를 갱신하는 SetState와는 별도 호출이다.
    SurfaceCompletion { surface_id: u32, kind: &'static str },
}

/// 보류한 Stop 을 확정할 때 쓰는 공유 상태. 훅·게이트 처리기와 시간 초과 스레드가 함께 쓴다.
pub(crate) struct StopTail<'a, H> {
    pub state: &'a Mutex<ClaudeState>,
    pub scanner: &'a Arc<Mutex<ErrorScanner>>,
    pub resume: &'a Mutex<crate::auto_resume::ResumeTable>,
    pub pairing: &'a Mutex<StopPairing>,
    /// Codex 부모에게 돌려보낼 결과의 경로. 턴 종료를 확정할 때 쓴다.
    pub returns: &'a crate::codex_return::Returns,
    pub host: &'a H,
}

impl<H: HostCallSink> StopTail<'_, H> {
    /// 게이트가 붙은 세션의 Stop 을 보류 표에 넣는다. 게이트나 세션 id 가 없으면 아무것도 하지 않는다.
    fn pair_stop(
        &self,
        session: Option<&str>,
        surface_id: u32,
        prompt_id: Option<String>,
        kind: StopKind,
        final_answer: Option<String>,
        gates: usize,
    ) -> usize {
        let Some(session_id) = session.filter(|_| gates > 0) else {
            return 0;
        };
        // 결과 경로의 턴은 Stop 이 온 지금 정한다. 판정이 늦게 와도 그 사이 열린 새 턴으로 바뀌지 않는다.
        let turn = self.returns.turn_of(surface_id, Some(session_id));
        let held = crate::stop_pairing::HeldStop {
            surface_id,
            prompt_id,
            kind,
            final_answer,
            turn,
        };
        let settled =
            lock_pairing(self.pairing).stop(session_id, held, gates, std::time::Instant::now());
        self.finish(settled)
    }

    /// 새 턴이 오면 보류 중인 Stop 에서 턴이 끝난 것이므로 idle 로 확정한다. 세션 종료는 보류를 버린다.
    fn settle_at_turn_boundary(&self, event: &str, session: Option<&str>) -> usize {
        let Some(session_id) = session else {
            return 0;
        };
        if is_new_turn_event(event) {
            let settled = lock_pairing(self.pairing).new_turn(session_id);
            return self.finish(settled);
        }
        if event == "session-end" {
            lock_pairing(self.pairing).end_session(session_id);
        }
        0
    }

    /// 확정된 Stop 을 처리한다. 턴이 끝났으면 agent task 턴 끝을 보고하고 보류했던 idle 처리를
    /// 실행한다. 게이트가 턴을 이어 가게 했으면 보류 때 보낸 active 를 그대로 두고 보고하지 않는다.
    pub(crate) fn finish(&self, settled: impl IntoIterator<Item = Settled>) -> usize {
        let mut failures = 0;
        for s in settled {
            if s.runs_idle() {
                tracing::info!(
                    "claude stop s{}: every gate let the turn end — reporting idle",
                    s.surface_id
                );
                crate::task_turn::report_ended(
                    self.host,
                    self.returns,
                    crate::task_turn::Ended {
                        surface_id: s.surface_id,
                        session: Some(&s.session),
                        final_answer: s.final_answer.as_deref(),
                        attempt_id: None,
                        turn: s.turn.clone(),
                    },
                );
                failures += self.run_idle_stop(s.surface_id);
            } else if s.kind == StopKind::Idle {
                tracing::info!(
                    "claude stop s{}: a gate blocked the stop — turn continues, stays active",
                    s.surface_id
                );
                failures += self.end_background_wait(s.surface_id);
            }
        }
        failures
    }

    /// 시간 초과로 확정한 Stop 을 처리한다. 확정한 뒤 같은 세션에 새 턴이 시작됐으면 버린다.
    /// 판정 표를 잠근 채 보내므로, 워커의 새 턴 처리는 이 처리가 끝난 뒤에 active 를 보낸다.
    pub(crate) fn finish_expired(&self, expired: Vec<Expired>) -> usize {
        let mut failures = 0;
        for e in expired {
            let table = lock_pairing(self.pairing);
            if !table.is_current_turn(&e.session, e.turn) {
                tracing::info!(
                    "claude stop s{}: a new turn started before the timed-out stop was reported — dropped",
                    e.settled.surface_id
                );
                // 상태 보고는 새 턴을 덮지 않도록 버리지만, Codex 부모에게는 Stop 이 온 턴의 결과로 보낸다.
                if e.settled.runs_idle()
                    && let Some(turn) = e.settled.turn.clone()
                {
                    self.returns.turn_end(crate::codex_return::TurnEnd {
                        surface: e.settled.surface_id,
                        session: Some(&e.settled.session),
                        status: crate::codex_return::ReturnStatus::TurnEnded,
                        body: e.settled.final_answer.as_deref(),
                        attempt_id: None,
                        turn: Some(turn),
                    });
                }
                continue;
            }
            failures += self.finish([e.settled]);
            drop(table);
        }
        failures
    }

    /// 대기가 아닌 Stop 이 왔으므로 앞선 백그라운드 대기는 끝났다. 게이트가 턴을 이어 가도 대기 기록을 지운다.
    fn end_background_wait(&self, surface_id: u32) -> usize {
        crate::error_scan::lock_scanner(self.scanner).clear_background_wait(surface_id);
        deliver_all(
            self.host,
            &[HostCall::MetaUnset {
                surface_id,
                key: BACKGROUND_WAIT_META_KEY,
            }],
        )
    }

    /// 턴 종료 Stop 의 처리. 보류하지 않은 Stop 과 같은 호출·기록을 만든다.
    fn run_idle_stop(&self, surface_id: u32) -> usize {
        // stop 의 호출 목록은 번역 문구를 쓰지 않는다.
        let mut calls = apply_hook("stop", surface_id, None, None, None, &Translator::default())
            .unwrap_or_else(|e| {
                tracing::warn!("claude stop s{surface_id}: building the idle calls failed: {e:?}");
                Vec::new()
            });
        calls.extend(telemetry_for_hook(
            &mut lock_state(self.state),
            "stop",
            surface_id,
            None,
            now_ms(),
        ));
        let failures = deliver_all(self.host, &calls);
        crate::auto_resume::observe_hook(
            self.resume,
            self.host,
            "stop",
            surface_id,
            None,
            std::time::Instant::now(),
        );
        crate::error_scan::lock_scanner(self.scanner).clear_background_wait(surface_id);
        failures
    }
}

#[allow(clippy::too_many_arguments)] // reason: 훅 처리에 필요한 플러그인 상태를 각각 빌려 받는다.
pub(crate) fn handle_claude_hook<H: HostCallSink>(
    state: &Mutex<ClaudeState>,
    scanner: &Arc<Mutex<ErrorScanner>>,
    resume: &Mutex<crate::auto_resume::ResumeTable>,
    pairing: &Mutex<StopPairing>,
    returns: &crate::codex_return::Returns,
    host: &H,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let event = params
        .get("event")
        .and_then(|v| v.as_str())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.hook.missing_event")))?;

    let surface_id = resolve_surface_id(params, tr)?;
    crate::error_scan::lock_scanner(scanner).note_hook(surface_id);
    let session = params
        .get("session")
        .and_then(|v| v.as_str())
        .map(String::from);
    if event == "session-start"
        && let Some(session) = session.as_deref()
        && crate::session_options::stale_start(host, surface_id, session, tr)?
    {
        return Ok(json!({"ignored": "stale_session_start"}));
    }
    let message = params.get("message").and_then(|v| v.as_str());
    let notification_type = params.get("notification_type").and_then(|v| v.as_str());
    let error = params.get("error").and_then(|v| v.as_str());
    let agent_id = params.get("agent_id").and_then(|v| v.as_str());
    let now_ms = now_ms();
    let tail = StopTail {
        state,
        scanner,
        resume,
        pairing,
        returns,
        host,
    };
    // 결과 경로의 세션을 묶는다. 턴 종료를 판정하기 전에 해야 한다. 새 프롬프트의 턴 순번은 보류 중인
    // Stop 을 앞 턴으로 확정한 뒤에 올린다(아래). 먼저 올리면 앞 턴 결과가 새 턴 키로 claim 된다.
    let detail = params
        .get(if event == "session-end" {
            "reason"
        } else {
            "source"
        })
        .and_then(Value::as_str);
    if event != "prompt-submit" {
        returns.observe(event, surface_id, session.as_deref(), detail);
    }

    if let Some(response) =
        hook_outside_the_turn(state, host, event, surface_id, agent_id, error, params)
    {
        return Ok(response);
    }
    if crate::session_start::is_compaction(event, params) {
        return compaction_start(host, params, surface_id, session.as_deref(), data_dir, tr);
    }
    lock_state(state)
        .background
        .track(event, surface_id, params);
    // 끝난 작업에 건 무출력 선언을 끝낸다. 양수 선언에 쌓인 것은 묶어 부모에게 보낸다.
    if let Some(ended) = crate::background_tasks::ended_tasks(event, params) {
        crate::error_scan::lock_scanner(scanner).end_quiet_tasks(host, surface_id, ended);
    }
    if event == "prompt-submit" {
        crate::monitor_tasks::forget_notified(state, host, surface_id, params);
    }

    if event == "stop-failure"
        && let Some(response) = stop_failure_with_background_work(&tail, surface_id, params, now_ms)
    {
        return Ok(response);
    }

    if event == "stop"
        && let Some(response) =
            stop_that_does_not_end_the_turn(&tail, params, surface_id, session.as_deref(), now_ms)
    {
        return Ok(response);
    }

    // 새 프롬프트가 왔다면 보류 중인 Stop 에서 턴이 끝났다. 새 턴의 active 보다 먼저 idle 을 보고하고,
    // agent task 에도 앞 턴의 끝을 새 턴의 시작보다 먼저 보고한다.
    let settled_failures = tail.settle_at_turn_boundary(event, session.as_deref());
    if event == "prompt-submit" {
        returns.observe(event, surface_id, session.as_deref(), detail);
    }
    crate::task_turn::report(host, returns, event, surface_id, session.as_deref(), params);

    let mut calls = apply_hook(
        event,
        surface_id,
        session.as_deref(),
        notification_type,
        error,
        tr,
    )?;
    if event == "session-start" {
        session_start_state(
            &mut calls,
            host,
            surface_id,
            params.get("source").and_then(Value::as_str),
        );
    }

    session_lifecycle_calls(
        &mut calls,
        host,
        event,
        surface_id,
        session.as_deref(),
        params,
        data_dir,
        tr,
    );

    calls.extend(telemetry_for_hook(
        &mut lock_state(state),
        event,
        surface_id,
        message,
        now_ms,
    ));

    // 새 턴의 active 를 보내기 전에 정지 관측을 지운다. 순서가 반대면 그 사이 스캔이
    // 이전 턴의 조용한 구간과 새 상태 active 를 함께 보고 거짓 정지 알림을 낼 수 있다.
    if is_new_turn_event(event) {
        reset_dedupe_if_enabled(scanner, surface_id);
    }
    // 호스트 호출의 실패 횟수를 항상 응답에 포함한다.
    let host_call_failures = settled_failures + deliver_all(host, &calls);
    // 상태(`idle`)를 먼저 쓴 뒤에 예약한다 — 만기 처리가 그 상태를 확인한다.
    crate::auto_resume::observe_hook(
        resume,
        host,
        event,
        surface_id,
        error,
        std::time::Instant::now(),
    );

    if ends_background_wait(event) {
        crate::error_scan::lock_scanner(scanner).clear_background_wait(surface_id);
    }

    // 게이트 판정은 별도 경로지만 세션 종료 시 모든 게이트의 회차 상태를 정리한다.
    if event == "session-end" {
        end_session(returns, data_dir, surface_id, session.as_deref());
    }

    // ok는 핸들러 완료를 뜻한다. 호스트 호출의 성공 여부는 실패 횟수로 구분한다.
    Ok(json!({
        "ok": true,
        "surface_id": surface_id,
        "event": event,
        "host_call_failures": host_call_failures,
    }))
}

/// 메인 턴에 속하지 않는 hook 의 응답. 실패한 subagent 의 StopFailure 와 백그라운드 작업 시작이다.
/// 둘 다 아니면 `None` 이며 호출자가 턴의 hook 으로 처리한다.
fn hook_outside_the_turn<H: HostCallSink>(
    state: &Mutex<ClaudeState>,
    host: &H,
    event: &str,
    surface_id: u32,
    agent_id: Option<&str>,
    error: Option<&str>,
    params: &Value,
) -> Option<Value> {
    if is_subagent_stop_failure(event, agent_id) {
        // 메인 턴은 끝나지 않았다 — 상태·알림·meta 어느 것도 건드리지 않는다.
        tracing::info!(
            "claude hook stop-failure s{surface_id}: subagent {agent_id:?} failed ({error:?}) — main turn continues, ignored"
        );
        return Some(json!({
            "ok": true,
            "surface_id": surface_id,
            "event": event,
            "ignored": "subagent",
            "host_call_failures": 0,
        }));
    }

    if event == BACKGROUND_START_EVENT {
        let started = crate::monitor_tasks::record_start(state, host, surface_id, params);
        if let Some(id) = &started {
            tracing::info!("claude hook s{surface_id}: background task {id} started");
        }
        return Some(json!({
            "ok": true,
            "surface_id": surface_id,
            "event": event,
            "background_task": started,
            "host_call_failures": 0,
        }));
    }
    None
}

/// 압축 뒤의 SessionStart. 같은 세션의 기록(세션 id·복원 명령·프로필)만 갱신하고, 상태 보고와
/// 턴 경계 처리(보류 Stop 확정·백그라운드 작업 기록·자동 재개·wall_time·정지 관측 초기화)는 하지 않는다.
/// 턴 도중의 압축이면 앞선 prompt-submit 의 active 가, 쉬는 중의 압축이면 Stop 의 idle 이 그대로 남는다.
fn compaction_start<H: HostCallSink>(
    host: &H,
    params: &Value,
    surface_id: u32,
    session: Option<&str>,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let mut calls = apply_hook("session-start", surface_id, session, None, None, tr)?;
    crate::session_start::keep_turn_state_across_compaction(&mut calls);
    session_lifecycle_calls(
        &mut calls,
        host,
        "session-start",
        surface_id,
        session,
        params,
        data_dir,
        tr,
    );
    tracing::info!(
        "claude hook session-start s{surface_id}: compaction — state and turn left unchanged"
    );
    Ok(json!({
        "ok": true,
        "surface_id": surface_id,
        "event": "session-start",
        "compaction": true,
        "host_call_failures": deliver_all(host, &calls),
    }))
}

/// 세션 종료 시 모든 게이트의 회차 상태와 결과 경로를 정리한다.
fn end_session(
    returns: &crate::codex_return::Returns,
    data_dir: Option<&Path>,
    surface_id: u32,
    session: Option<&str>,
) {
    // 확정되지 않은 채 끝난 턴은 Codex 부모에게 exited 로 알린다. 확정된 턴이면 보내지 않는다.
    returns.turn_end(crate::codex_return::TurnEnd {
        surface: surface_id,
        session,
        status: crate::codex_return::ReturnStatus::Exited,
        body: None,
        attempt_id: None,
        turn: None,
    });
    checklist::remove_state_for_session(data_dir, session.unwrap_or(""));
    // 닫은 탭을 곧바로 복원할 수 있도록 즉시 지우지 않고 종료 표시만 남긴다.
    // reboot 뒤 session-start가 오면 새 기록으로 갱신한다.
    profile_attach::mark_ended(data_dir, session.unwrap_or(""));
}

/// 턴을 끝내지 않는 Stop 을 처리한다. 백그라운드 작업을 기다리는 Stop 과, 게이트 판정을 기다리며
/// idle 처리를 보류하는 Stop 이다. 둘 다 아니면 `None` 이며 호출자가 턴 종료로 처리한다.
fn stop_that_does_not_end_the_turn<H: HostCallSink>(
    tail: &StopTail<'_, H>,
    params: &Value,
    surface_id: u32,
    session: Option<&str>,
    now_ms: u64,
) -> Option<Value> {
    let prompt_id = params
        .get("prompt_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    // 게이트가 붙은 세션의 Stop 은 게이트 판정과 짝지어 확정한다. 판정을 가릴 세션 id 가 필요하다.
    let gates = match session {
        Some(_) => crate::stop_pairing::attached_gate_count(tail.host, surface_id),
        None => 0,
    };

    if let Some(pending) = background_work_pending(params) {
        // 메인 응답만 끝났고 백그라운드 작업이 남아 있다. 작업이 끝나면 Claude Code 가
        // 새 턴(UserPromptSubmit)을 열고 그 턴의 Stop 이 진짜 종료가 된다.
        // idle·완료 알림·화면 알림·wall_time 기록·자동 재개 성공 처리를 하지 않고,
        // 턴이 이어진다는 사실만 active 로 보고한다.
        tracing::info!(
            "claude hook stop s{surface_id}: waiting on background work ({pending}) — main turn continues, stays active"
        );
        let monitors = crate::monitor_tasks::recorded(tail.host, surface_id);
        let watch = crate::task_watch::TaskWatch::from_stop(params, &monitors);
        let kinds = crate::monitor_tasks::wait_kinds(
            &params
                .get("background_tasks")
                .and_then(pending_background_tasks)
                .unwrap_or_default(),
            &monitors,
        );
        let calls = background_wait_calls(tail, surface_id, now_ms, kinds, watch);
        // 게이트는 대기 Stop 도 판정한다. 그 판정이 다음 Stop 과 섞이지 않도록 이 Stop 과 짝짓는다.
        let host_call_failures = deliver_all(tail.host, &calls)
            + tail.pair_stop(
                session,
                surface_id,
                prompt_id,
                StopKind::Waiting,
                None,
                gates,
            );
        return Some(json!({
            "ok": true,
            "surface_id": surface_id,
            "event": "stop",
            "waiting": "background_work",
            "host_call_failures": host_call_failures,
        }));
    }

    if gates > 0 && session.is_some() {
        // 게이트가 block 하면 턴이 이어진다. 판정이 모일 때까지 idle 처리를 보류하고 active 로 둔다.
        tracing::info!(
            "claude hook stop s{surface_id}: {gates} gate(s) attached — idle deferred until their decisions"
        );
        let active = [HostCall::SetState {
            surface_id,
            state: "active",
        }];
        let host_call_failures = deliver_all(tail.host, &active)
            + tail.pair_stop(
                session,
                surface_id,
                prompt_id,
                StopKind::Idle,
                crate::task_turn::final_answer(params),
                gates,
            );
        return Some(json!({
            "ok": true,
            "surface_id": surface_id,
            "event": "stop",
            "deferred": "gate",
            "host_call_failures": host_call_failures,
        }));
    }

    None
}

/// 대기를 `active` 로 보고하고 정지 감시가 대기 기준과 문구를 쓰도록 기록하는 호출.
/// 같은 대기에서 이어진 기록은 처음 시작 시각을 유지한다.
fn background_wait_calls<H>(
    tail: &StopTail<'_, H>,
    surface_id: u32,
    now_ms: u64,
    kinds: crate::error_scan::WaitKinds,
    watch: Option<crate::task_watch::TaskWatch>,
) -> [HostCall; 2] {
    let value = crate::error_scan::lock_scanner(tail.scanner)
        .record_background_wait(surface_id, now_ms, kinds, watch);
    [
        HostCall::SetState {
            surface_id,
            state: "active",
        },
        HostCall::MetaSet {
            surface_id,
            key: BACKGROUND_WAIT_META_KEY,
            value,
        },
    ]
}

/// 백그라운드 작업이 남은 채 API 오류로 끝난 턴. 작업이 끝나면 Claude Code 가 새 턴을 열므로
/// 대기 Stop 과 같이 `active` 로 보고하고 idle·완료 알림·자동 재개를 만들지 않는다.
/// 오류는 meta 와 `claude-stop-failure` 로 남겨, 부모의 notify-stop-failure 훅이 한 줄로 알린다.
/// 그 훅은 대기 meta 를 읽으므로 두 meta 를 이벤트보다 먼저 기록한다.
/// 남은 작업이 없으면 `None` 이며 호출자가 턴 종료로 처리한다.
fn stop_failure_with_background_work<H: HostCallSink>(
    tail: &StopTail<'_, H>,
    surface_id: u32,
    params: &Value,
    now_ms: u64,
) -> Option<Value> {
    let error = params.get("error").and_then(Value::as_str);
    let (ids, types) = {
        let state = lock_state(tail.state);
        (
            state.background.pending_ids(surface_id),
            state.background.pending_types(surface_id),
        )
    };
    if types.is_empty() {
        return None;
    }
    tracing::info!(
        "claude hook stop-failure s{surface_id}: API error {error:?} while {} background task(s) ({}) still run — the session continues when they finish, stays active",
        types.len(),
        types.join(",")
    );
    let mut calls = vec![HostCall::MetaSet {
        surface_id,
        key: STOP_FAILURE_META_KEY,
        value: error
            .filter(|e| !e.is_empty())
            .unwrap_or("unknown")
            .to_string(),
    }];
    // 상태와 notify-stop-failure 문구는 Monitor 도 그대로 센다. 정지 알림의 대상에서만 뺀다.
    let monitors = crate::monitor_tasks::recorded(tail.host, surface_id);
    let (noticed_ids, kinds) = crate::monitor_tasks::split_recorded(ids, types, &monitors);
    let watch = crate::task_watch::TaskWatch::from_ids(params, noticed_ids);
    calls.extend(background_wait_calls(
        tail, surface_id, now_ms, kinds, watch,
    ));
    calls.push(HostCall::FireHook {
        surface_id,
        event: STOP_FAILURE_EVENT,
    });
    Some(json!({
        "ok": true,
        "surface_id": surface_id,
        "event": "stop-failure",
        "waiting": "background_work",
        "host_call_failures": deliver_all(tail.host, &calls),
    }))
}

/// 백그라운드 대기 기록을 지우는 이벤트. 대기가 아닌 Stop 과 StopFailure 는 턴이 끝났고,
/// 새 턴과 세션 종료 뒤에는 이전 대기가 이어지지 않는다.
fn ends_background_wait(event: &str) -> bool {
    is_new_turn_event(event) || matches!(event, "stop" | "stop-failure" | "session-end")
}

/// agent_id가 있는 stop-failure는 서브에이전트 오류로 처리한다.
/// 이 오류만으로 메인 세션을 idle로 바꾸거나 완료 알림을 보내지 않는다.
pub(crate) fn is_subagent_stop_failure(event: &str, agent_id: Option<&str>) -> bool {
    event == "stop-failure" && agent_id.is_some_and(|a| !a.is_empty())
}

/// Stop payload 가 아직 끝나지 않은 백그라운드 작업을 보고하면 그 요약을 돌려준다.
///
/// `waiting_on_background_work` 가 있으면 그 값을 따른다. 없으면 `background_tasks` 에서
/// 끝을 뜻하는 `status`([`FINISHED_TASK_STATUSES`])가 아닌 항목을 센다. 항목의 종류(subagent·shell 등)는
/// 가리지 않는다.
/// 두 필드가 모두 없거나 해석할 수 없으면 `None` 이며, 이전처럼 Stop 을 턴 종료로 처리한다.
/// 두 필드는 CLI 가 stdin JSON 값을 그대로 넘기므로 JSON 값이고, 직접 플래그로 주면 문자열이다.
pub(crate) fn background_work_pending(params: &Value) -> Option<String> {
    if let Some(waiting) = params
        .get("waiting_on_background_work")
        .and_then(read_waiting_flag)
    {
        return waiting.then(|| "waiting_on_background_work".to_string());
    }
    let pending = pending_background_task_types(params.get("background_tasks")?)?;
    (!pending.is_empty()).then(|| {
        format!(
            "{} background task(s): {}",
            pending.len(),
            pending.join(",")
        )
    })
}

/// `waiting_on_background_work` 값을 읽는다. 해석할 수 없으면 `None` 이며 background_tasks 로 넘어간다.
fn read_waiting_flag(value: &Value) -> Option<bool> {
    let flag = match value {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s == "true" => Some(true),
        Value::String(s) if s == "false" => Some(false),
        _ => None,
    };
    if flag.is_none() {
        tracing::warn!(
            "claude hook: unreadable waiting_on_background_work {value} — falling back to background_tasks"
        );
    }
    flag
}

/// 끝난 작업을 뜻하는 `status` 값. 대소문자는 가리지 않는다.
///
/// 공식 hooks 문서(2026-09-28 확인)는 `background_tasks` 항목을 진행 중인 작업(in-flight)이라 하고
/// 진행 중인 작업이 없으면 배열이 비어 있다고 설명하며, `status` 값 목록은 주지 않는다.
/// 2.1.283 실측 값은 `running` 뿐이었다. 그래서 목록에 없는 값과 `status` 가 없는 항목은 모두 대기로 본다.
/// `completed`·`failed` 는 이 모듈의 시험이 쓰는 값이고, 나머지는 흔한 종료어다.
const FINISHED_TASK_STATUSES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "canceled",
    "killed",
    "stopped",
    "error",
    "done",
];

pub(crate) fn is_finished_task(task: &Value) -> bool {
    task.get("status").and_then(Value::as_str).is_some_and(|s| {
        FINISHED_TASK_STATUSES
            .iter()
            .any(|f| s.eq_ignore_ascii_case(f))
    })
}

/// `background_tasks` 에서 끝나지 않은 항목의 종류를 모은다. 배열이 아니면 `None` 이다.
fn pending_background_task_types(value: &Value) -> Option<Vec<String>> {
    Some(
        pending_background_tasks(value)?
            .iter()
            .map(crate::monitor_tasks::task_kind)
            .collect(),
    )
}

/// `background_tasks` 에서 끝나지 않은 항목을 모은다. 배열이 아니면 `None` 이다.
fn pending_background_tasks(value: &Value) -> Option<Vec<Value>> {
    let parsed;
    let tasks = match value {
        Value::String(raw) => {
            parsed = serde_json::from_str::<Value>(raw)
                .inspect_err(|e| {
                    tracing::warn!(
                        "claude hook: background_tasks is not JSON ({e}) — treating the stop as a turn end"
                    )
                })
                .ok()?;
            &parsed
        }
        other => other,
    };
    let Some(items) = tasks.as_array() else {
        tracing::warn!(
            "claude hook: background_tasks is not an array — treating the stop as a turn end"
        );
        return None;
    };
    Some(
        items
            .iter()
            .filter(|t| !is_finished_task(t))
            .cloned()
            .collect(),
    )
}

/// 새 턴으로 처리할 이벤트. 이전 턴의 오류가 새 알림을 막지 않도록 중복 기록을 초기화한다.
fn is_new_turn_event(event: &str) -> bool {
    matches!(event, "prompt-submit" | "session-start" | "active")
}

/// 오류 감시 중인 surface에만 중복 기록 초기화를 적용한다.
fn reset_dedupe_if_enabled(scanner: &Arc<Mutex<ErrorScanner>>, surface_id: u32) {
    let mut s = crate::error_scan::lock_scanner(scanner);
    if s.is_enabled(surface_id) {
        s.reset_dedupe(surface_id);
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// session-start에서 시각을 기록하고 종료 이벤트에서 경과 시간을 계산한다.
/// notification에 토큰 사용량이 있으면 input_tokens 기록도 만든다.
pub(crate) fn telemetry_for_hook(
    state: &mut ClaudeState,
    event: &str,
    surface_id: u32,
    message: Option<&str>,
    now_ms: u64,
) -> Vec<HostCall> {
    let mut out = Vec::new();
    match event {
        "session-start" => {
            state.mark_session_start(surface_id, now_ms);
        }
        // subagent-stop 은 메인 턴의 끝이 아니므로 경과 시간을 닫지 않는다.
        "stop" | "stop-failure" | "session-end" => {
            if let Some(elapsed) = state.take_wall_time(surface_id, now_ms) {
                out.push(HostCall::TelemetryRecord {
                    metric: "wall_time_ms",
                    value: elapsed as f64,
                    surface_id,
                });
            }
        }
        "notification" => {
            if let Some(text) = message
                && let Some(n) = extract_tokens(text)
            {
                out.push(HostCall::TelemetryRecord {
                    metric: "input_tokens",
                    value: n as f64,
                    surface_id,
                });
            }
        }
        _ => {}
    }
    out
}

/// token/Token에 선택적인 s/S, 콜론, 공백·탭, 숫자가 이어지는 표현을 찾는다.
/// 앞뒤는 ASCII 영숫자가 아니어야 하며 정규식의 Unicode 단어 경계와는 다르다.
fn extract_tokens(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 시작 위치이거나 앞 바이트가 ASCII 영숫자가 아니어야 한다.
        let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if prev_ok && (bytes[i..].starts_with(b"token") || bytes[i..].starts_with(b"Token")) {
            let mut j = i + 5;
            if j < bytes.len() && (bytes[j] == b's' || bytes[j] == b'S') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' {
                j += 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                let start = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > start {
                    let after_ok = j == bytes.len() || !bytes[j].is_ascii_alphanumeric();
                    if after_ok
                        && let Ok(n) = std::str::from_utf8(&bytes[start..j])
                            .unwrap()
                            .parse::<u64>()
                    {
                        return Some(n);
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// `Notification` 한 건이 자식 상태에 주는 효과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotificationEffect {
    /// 이 세션이 사용자 입력을 기다린다.
    NeedsInput,
    /// 멈췄던 작업이 이어진다.
    Active,
    /// 상태를 바꾸지 않는다.
    Unchanged,
}

/// Claude Code `Notification` 의 `notification_type` 별 효과. 공식 hooks 문서의
/// Notification matcher 값 목록을 모두 적는다. `docs/plugins/claude/index.md` 의
/// "Notification 유형별 상태" 표가 같은 목록이며 테스트가 두 목록을 비교한다.
pub(crate) const NOTIFICATION_EFFECTS: &[(&str, NotificationEffect)] = &[
    ("permission_prompt", NotificationEffect::NeedsInput),
    ("elicitation_dialog", NotificationEffect::NeedsInput),
    ("elicitation_url_dialog", NotificationEffect::NeedsInput),
    ("quota_auto_resume_stale", NotificationEffect::NeedsInput),
    // agent view 의 다른 백그라운드 세션 대기나 auto mode 의 classifier 요금 안내(약 6초 무입력)일 수도 있지만
    // payload 로 teammate 설정 질문과 구분할 수 없어 이 세션의 대기로 본다.
    ("agent_needs_input", NotificationEffect::NeedsInput),
    ("quota_auto_resume_fired", NotificationEffect::Active),
    // 턴이 끝난 지 약 60초 뒤의 재알림이다. 턴 종료는 Stop 이 이미 기록했다.
    ("idle_prompt", NotificationEffect::Unchanged),
    ("auth_success", NotificationEffect::Unchanged),
    ("agent_completed", NotificationEffect::Unchanged),
    // 대기가 풀렸다는 알림이다. needs_input 의 원인을 기록하지 않으므로 active 로 바꾸면 겹친 다른 대기까지 지운다.
    ("elicitation_complete", NotificationEffect::Unchanged),
    ("elicitation_response", NotificationEffect::Unchanged),
    ("quota_auto_resume_disabled", NotificationEffect::Unchanged),
];

/// `notification_type` 의 효과를 정한다. 필드가 없으면 유형이 생기기 전 버전과 같이 입력 대기로 본다.
/// 목록에 없는 값은 입력 대기로 보면 spawn/tell 대기 노드가 조기에 끝나므로 상태를 바꾸지 않고 기록만 남긴다.
pub(crate) fn notification_effect(
    surface_id: u32,
    notification_type: Option<&str>,
) -> NotificationEffect {
    let Some(ty) = notification_type else {
        return NotificationEffect::NeedsInput;
    };
    match NOTIFICATION_EFFECTS.iter().find(|(name, _)| *name == ty) {
        Some((_, effect)) => *effect,
        None => {
            tracing::warn!(
                "claude hook notification s{surface_id}: unknown notification_type {ty:?} — state left unchanged"
            );
            NotificationEffect::Unchanged
        }
    }
}

/// 턴·세션 경계에서 지울 meta 의 호출.
fn turn_boundary_meta_unsets(event: &str, surface_id: u32) -> Vec<HostCall> {
    // 새 턴·세션 종료에서는 이전 API 오류 기록을 지워 다음 완료 알림에 남지 않게 한다.
    let stop_failure = is_new_turn_event(event) || event == "session-end";
    // 새 세션과 세션 종료 뒤에는 이전 Monitor 가 이어지지 않는다. 압축의 SessionStart 는 호출자가 이 호출을 뺀다.
    let monitors = matches!(event, "session-start" | "session-end");
    [
        (stop_failure, STOP_FAILURE_META_KEY),
        (ends_background_wait(event), BACKGROUND_WAIT_META_KEY),
        (monitors, crate::monitor_tasks::MONITOR_TASKS_META_KEY),
    ]
    .into_iter()
    .filter(|(applies, _)| *applies)
    .map(|(_, key)| HostCall::MetaUnset { surface_id, key })
    .collect()
}

/// 훅 이벤트를 호스트 호출 목록으로 바꾼다.
/// notification은 [`NOTIFICATION_EFFECTS`] 에 따라 처리한다.
pub(crate) fn apply_hook(
    event: &str,
    surface_id: u32,
    session: Option<&str>,
    notification_type: Option<&str>,
    error: Option<&str>,
    tr: &Translator,
) -> Result<Vec<HostCall>, IpcMethodError> {
    let mut calls = turn_boundary_meta_unsets(event, surface_id);
    match event {
        "stop-failure" => {
            // 오류로 끝난 턴도 idle과 완료 이벤트로 알린다.
            // 알림 명령이 오류 종류를 읽을 수 있도록 메타데이터를 먼저 기록한다.
            calls.push(HostCall::SetState {
                surface_id,
                state: "idle",
            });
            calls.push(HostCall::MetaSet {
                surface_id,
                key: STOP_FAILURE_META_KEY,
                // 오류 종류가 없으면 unknown으로 기록한다.
                value: error
                    .filter(|e| !e.is_empty())
                    .unwrap_or("unknown")
                    .to_string(),
            });
            calls.push(HostCall::FireHook {
                surface_id,
                event: "claude-idle",
            });
            calls.push(HostCall::FireHook {
                surface_id,
                event: STOP_FAILURE_EVENT,
            });
            calls.push(HostCall::SurfaceCompletion {
                surface_id,
                kind: "completion",
            });
        }
        // 서브에이전트가 끝났을 뿐 메인 턴은 이어진다. 상태·완료 알림·화면 알림을 만들지 않는다.
        // Claude Code 는 내부 에이전트(agent_type 이 빈 값)의 종료도 이 훅으로 보내며, 턴 도중에도 온다.
        "subagent-stop" => log_subagent_stop(surface_id),
        "stop" => {
            calls.push(HostCall::SetState {
                surface_id,
                state: "idle",
            });
            calls.push(HostCall::FireHook {
                surface_id,
                event: "claude-idle",
            });
            calls.push(HostCall::SurfaceCompletion {
                surface_id,
                kind: "completion",
            });
        }
        "session-end" => {
            // session-start와 순서를 대조할 수 있도록 메타데이터 정리를 기록한다.
            tracing::info!(
                "claude hook session-end s{surface_id}: clearing claude-session-id/restore.command meta"
            );
            calls.push(HostCall::SetState {
                surface_id,
                state: "idle",
            });
            calls.push(HostCall::MetaUnset {
                surface_id,
                key: "claude-session-id",
            });
            calls.push(HostCall::MetaUnset {
                surface_id,
                key: RESTORE_COMMAND_META_KEY,
            });
            calls.push(HostCall::FireHook {
                surface_id,
                event: "claude-idle",
            });
            calls.push(HostCall::SurfaceCompletion {
                surface_id,
                kind: "completion",
            });
        }
        "notification" => match notification_effect(surface_id, notification_type) {
            NotificationEffect::NeedsInput => {
                calls.push(HostCall::SetState {
                    surface_id,
                    state: "needs_input",
                });
                calls.push(HostCall::FireHook {
                    surface_id,
                    event: "needs-input",
                });
                calls.push(HostCall::SurfaceCompletion {
                    surface_id,
                    kind: "needs_input",
                });
            }
            NotificationEffect::Active => calls.push(HostCall::SetState {
                surface_id,
                state: "active",
            }),
            NotificationEffect::Unchanged => {}
        },
        "pre-tool-use" => {
            // 설치된 훅의 matcher는 AskUserQuestion이다. 질문 전 입력 대기로 표시한다.
            calls.push(HostCall::SetState {
                surface_id,
                state: "needs_input",
            });
            calls.push(HostCall::FireHook {
                surface_id,
                event: "needs-input",
            });
            calls.push(HostCall::SurfaceCompletion {
                surface_id,
                kind: "needs_input",
            });
        }
        "post-tool-use" => {
            // AskUserQuestion 응답 뒤 자식 상태를 active로 바꾼다.
            // 화면 알림을 지우는 호출은 여기서 하지 않는다.
            calls.push(HostCall::SetState {
                surface_id,
                state: "active",
            });
        }
        "prompt-submit" | "session-start" | "active" => {
            calls.push(HostCall::SetState {
                surface_id,
                state: "active",
            });
            if event == "session-start" {
                match session {
                    Some(session_id) => {
                        calls.push(HostCall::MetaSet {
                            surface_id,
                            key: "claude-session-id",
                            value: session_id.to_string(),
                        });
                        calls.push(HostCall::MetaSet {
                            surface_id,
                            key: RESTORE_COMMAND_META_KEY,
                            // 호출자가 프로필을 확인하면 settings를 포함한 명령으로 바꾼다.
                            value: crate::reboot::resume_command_line(session_id, None),
                        });
                    }
                    None => {
                        // 세션 id가 없으면 이후 reboot에 필요한 메타데이터를 기록할 수 없다.
                        tracing::warn!(
                            "claude hook session-start s{surface_id}: no session_id in payload — claude-session-id meta NOT set (tasty claude reboot will fail until the next session-start carries a session_id)"
                        );
                    }
                }
            }
        }
        other => {
            return Err(IpcMethodError::invalid_params(
                &tr.t_fmt("claude.hook.unknown_event", other),
            ));
        }
    }
    Ok(calls)
}

fn log_subagent_stop(surface_id: u32) {
    tracing::info!(
        "claude hook subagent-stop s{surface_id}: subagent finished, main turn continues — state unchanged"
    );
}

/// 계획한 호스트 호출을 모두 시도하고 실패한 수를 반환한다.
/// 실패를 전파해 뒤의 로컬 파일 정리를 건너뛰지 않도록 한다.
/// 종료 훅은 surface가 이미 사라진 뒤에도 도착할 수 있지만 세션 파일 정리는 여전히 필요하다.
/// 정책은 docs/dev-guide/plugin-development.md#실패-후-로컬-정리 참고.
fn deliver_all<H: HostCallSink>(host: &H, calls: &[HostCall]) -> usize {
    calls.iter().filter(|call| !deliver(host, call)).count()
}

fn deliver<H: HostCallSink>(host: &H, call: &HostCall) -> bool {
    let (method, params) = match call {
        HostCall::SetState { surface_id, state } => (
            "terminal.set_state",
            json!({ "surface": surface_id, "state": state }),
        ),
        HostCall::FireHook { surface_id, event } => (
            "surface.fire_hook",
            json!({ "surface_id": surface_id, "event": event }),
        ),
        HostCall::MetaSet {
            surface_id,
            key,
            value,
        } => (
            "surface.meta.set",
            json!({ "surface_id": surface_id, "key": key, "value": value }),
        ),
        HostCall::MetaUnset { surface_id, key } => (
            "surface.meta.unset",
            json!({ "surface_id": surface_id, "key": key }),
        ),
        HostCall::TelemetryRecord {
            metric,
            value,
            surface_id,
        } => (
            "telemetry.record",
            json!({
                "metric": metric,
                "value": value,
                "tags": { "surface_id": surface_id.to_string() },
            }),
        ),
        HostCall::SurfaceCompletion { surface_id, kind } => (
            "surface.completion",
            json!({ "surface_id": surface_id, "kind": kind }),
        ),
    };
    if let Err(e) = host.call(method, params) {
        tracing::warn!("claude hook host call '{method}' failed: {e}");
        return false;
    }
    true
}

/// surface·surface_id를 공용 판정으로 읽고 생략한 경우에만 전달받은 환경 값으로 대신한다.
/// 환경을 직접 읽지 않아 시험에서 값을 제어할 수 있다.
fn resolve_surface_id_from(
    params: &Value,
    env_surface: Option<&str>,
    tr: &Translator,
) -> Result<u32, IpcMethodError> {
    // 잘못 지정한 값은 오류로 반환하고, 값이 없을 때만 환경 값으로 대신한다.
    if let Some(sid) = crate::handlers::optional_target_surface(params, tr)? {
        return Ok(sid);
    }
    if let Some(sid) = env_surface.and_then(|e| e.parse::<u32>().ok()) {
        return Ok(sid);
    }
    Err(IpcMethodError::invalid_params(
        tr.t("claude.params.missing_surface_no_env"),
    ))
}

/// 실제 TASTY_SURFACE_ID를 읽어 대상 판정에 전달한다.
fn resolve_surface_id(params: &Value, tr: &Translator) -> Result<u32, IpcMethodError> {
    let env = std::env::var("TASTY_SURFACE_ID").ok();
    resolve_surface_id_from(params, env.as_deref(), tr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_attach::AttachRecord;
    use crate::session_start::{
        LAUNCH_PROMPT_META_KEY, SessionStartProfile, apply_session_start_profile,
        plan_session_start_profile,
    };

    /// 실제 카탈로그로 오류 안내를 확인한다.
    fn test_translator() -> Translator {
        test_translator_for("en")
    }

    fn test_translator_for(code: &str) -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, code)
    }

    /// 지정한 메서드만 실패시키는 mock 호스트.
    struct FlakyHost {
        /// 실패시킬 method 이름. 비면 전부 성공.
        fail: Vec<&'static str>,
        seen: std::cell::RefCell<Vec<String>>,
    }

    impl FlakyHost {
        fn failing_everything() -> Self {
            Self {
                fail: vec![
                    "terminal.set_state",
                    "surface.fire_hook",
                    "surface.meta.set",
                    "surface.meta.unset",
                    "telemetry.record",
                    "surface.completion",
                ],
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn healthy() -> Self {
            Self {
                fail: Vec::new(),
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl HostCallSink for FlakyHost {
        fn call(
            &self,
            method: &str,
            _params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            self.seen.borrow_mut().push(method.to_string());
            if self.fail.contains(&method) {
                // 호스트의 대상 부재 오류를 모의한다.
                Err(tasty_plugin_sdk::PluginError::HostCall {
                    method: method.to_string(),
                    message: "no live surface 999".to_string(),
                    code: Some(-32602),
                })
            } else {
                Ok(json!({}))
            }
        }
    }

    /// 호출한 메서드와 인자를 모두 기록하는 mock 호스트. 모든 호출이 성공한다.
    /// `settings_file` 이 있으면 부착 settings 경로 meta 조회에 그 값을 돌려준다.
    /// `settings_session` 은 그 경로를 소유한 세션 id meta 다.
    #[derive(Default)]
    struct RecordingHost {
        seen: std::cell::RefCell<Vec<(String, Value)>>,
        settings_file: Option<String>,
        settings_session: Option<String>,
        /// 프롬프트를 실어 실행했다는 meta 가 있는가.
        launch_prompt: bool,
        /// Monitor 기록 meta. 플러그인 재시작에도 남는 값이라 실제처럼 저장한다.
        monitor_meta: std::cell::RefCell<Option<String>>,
    }

    impl HostCallSink for RecordingHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            if params["key"] == crate::monitor_tasks::MONITOR_TASKS_META_KEY {
                match method {
                    "surface.meta.set" => {
                        *self.monitor_meta.borrow_mut() =
                            params["value"].as_str().map(String::from);
                    }
                    "surface.meta.unset" => *self.monitor_meta.borrow_mut() = None,
                    _ => {}
                }
            }
            let reply = match (&self.settings_file, method, params["key"].as_str()) {
                _ if method == "surface.meta.get"
                    && params["key"] == crate::monitor_tasks::MONITOR_TASKS_META_KEY =>
                {
                    json!({ "value": *self.monitor_meta.borrow() })
                }
                (
                    Some(path),
                    "surface.meta.get",
                    Some(crate::stop_pairing::SETTINGS_FILE_META_KEY),
                ) => {
                    json!({ "value": path })
                }
                _ if self.launch_prompt
                    && method == "surface.meta.get"
                    && params["key"] == LAUNCH_PROMPT_META_KEY =>
                {
                    json!({ "value": "1" })
                }
                _ if method == "surface.meta.get"
                    && params["key"] == crate::stop_pairing::SETTINGS_SESSION_META_KEY =>
                {
                    match &self.settings_session {
                        Some(id) => json!({ "value": id }),
                        None => json!({"value": null}),
                    }
                }
                _ if method == "surface.meta.get" => json!({"value": null}),
                _ => json!({}),
            };
            self.seen.borrow_mut().push((method.to_string(), params));
            Ok(reply)
        }
    }

    impl RecordingHost {
        fn methods(&self) -> Vec<String> {
            self.seen.borrow().iter().map(|(m, _)| m.clone()).collect()
        }

        fn states(&self) -> Vec<String> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == "terminal.set_state")
                .map(|(_, p)| p["state"].as_str().unwrap_or("").to_string())
                .collect()
        }

        fn fired(&self) -> Vec<String> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == "surface.fire_hook")
                .map(|(_, p)| p["event"].as_str().unwrap_or("").to_string())
                .collect()
        }

        /// agent task 턴 보고를 (event, final_answer) 로 차례대로 꺼낸다. 시작은 답이 없다.
        fn turn_reports(&self) -> Vec<(String, Option<String>)> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == "agent.task_turn_report")
                .map(|(_, p)| {
                    (
                        p["event"].as_str().unwrap_or("").to_string(),
                        p["final_answer"].as_str().map(String::from),
                    )
                })
                .collect()
        }

        /// Codex 부모에게 보낸 결과를 (status, body) 로 차례대로 꺼낸다.
        fn codex_returns(&self) -> Vec<(String, Option<String>)> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == "codex.deliver")
                .map(|(_, p)| {
                    (
                        p["status"].as_str().unwrap_or("").to_string(),
                        p["body"].as_str().map(String::from),
                    )
                })
                .collect()
        }

        fn delivered_keys(&self) -> Vec<String> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == "codex.deliver")
                .map(|(_, p)| p["completion_key"].as_str().unwrap_or("").to_string())
                .collect()
        }

        fn unset_keys(&self) -> Vec<String> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == "surface.meta.unset")
                .map(|(_, p)| p["key"].as_str().unwrap_or("").to_string())
                .collect()
        }
    }

    /// 훅 처리기 전체를 한 번 돌리는 준비물. wall_time 시작과 자동 재개 시도 1회를 미리 남긴다.
    struct HookRig {
        state: Mutex<ClaudeState>,
        scanner: Arc<Mutex<ErrorScanner>>,
        resume: Mutex<crate::auto_resume::ResumeTable>,
        pairing: Mutex<StopPairing>,
        returns: crate::codex_return::Returns,
        /// 결과 경로 파일을 두는 임시 폴더. 시험이 끝날 때까지 유지한다.
        _returns_dir: tempfile::TempDir,
        host: RecordingHost,
    }

    impl HookRig {
        const SURFACE: u32 = 7;

        fn new() -> Self {
            let mut state = ClaudeState::default();
            state.mark_session_start(Self::SURFACE, 1_000);
            let resume = Mutex::new(crate::auto_resume::ResumeTable::default());
            crate::auto_resume::lock_table(&resume).record_attempt(Self::SURFACE);
            let returns_dir = tempfile::tempdir().expect("returns dir");
            Self {
                state: Mutex::new(state),
                scanner: Arc::new(Mutex::new(ErrorScanner::new())),
                resume,
                pairing: Mutex::new(StopPairing::default()),
                returns: crate::codex_return::Returns::new(Some(returns_dir.path())),
                _returns_dir: returns_dir,
                host: RecordingHost::default(),
            }
        }

        /// 이 surface 의 Claude 가 `gates` 개의 Stop 게이트를 붙인 settings 로 실행된 준비물.
        /// 파일은 게이트 부착과 같은 명령 문자열로 만든다.
        fn with_gates(dir: &Path, gates: usize) -> Self {
            let hooks: Vec<Value> = (0..gates)
                .map(|i| {
                    let command = crate::install::tasty_guarded_command(
                        &crate::gate::gate_hook_argv(&format!("gate-{i}")),
                    );
                    json!({ "type": "command", "command": command })
                })
                .collect();
            let path = dir.join("settings.json");
            std::fs::write(
                &path,
                json!({ "hooks": { "Stop": [{ "matcher": "", "hooks": hooks }] } }).to_string(),
            )
            .unwrap();
            let mut rig = Self::new();
            rig.host.settings_file = Some(path.to_string_lossy().into_owned());
            rig
        }

        fn tail(&self) -> StopTail<'_, RecordingHost> {
            StopTail {
                state: &self.state,
                scanner: &self.scanner,
                resume: &self.resume,
                pairing: &self.pairing,
                returns: &self.returns,
                host: &self.host,
            }
        }

        /// 시간 초과로 확정할 Stop 을 꺼낸다. 확정 스레드가 잠금을 풀고 보내기 전의 상태다.
        fn take_expired(&self) -> Vec<crate::stop_pairing::Expired> {
            lock_pairing(&self.pairing)
                .expire(std::time::Instant::now() + crate::stop_pairing::GATE_DECISION_TIMEOUT)
        }

        fn is_waiting(&self) -> bool {
            crate::error_scan::lock_scanner(&self.scanner)
                .is_waiting_on_background_work(Self::SURFACE)
        }

        /// 게이트 한 건의 판정이 도착한 것처럼 처리한다.
        fn gate(&self, decision: Value, prompt_id: &str) {
            let settled = lock_pairing(&self.pairing).verdict(
                "sess-1",
                Some(prompt_id.to_string()),
                crate::stop_pairing::verdict_of(&decision),
                None,
                std::time::Instant::now(),
            );
            StopTail {
                state: &self.state,
                scanner: &self.scanner,
                resume: &self.resume,
                pairing: &self.pairing,
                returns: &self.returns,
                host: &self.host,
            }
            .finish(settled);
        }

        fn run(&mut self, params: Value) -> Value {
            handle_claude_hook(
                &self.state,
                &self.scanner,
                &self.resume,
                &self.pairing,
                &self.returns,
                &self.host,
                &params,
                None,
                &test_translator(),
            )
            .expect("hook handled")
        }

        /// 자동 재개 시도 기록이 아직 남아 있는가(성공 처리로 지워지지 않았는가).
        fn resume_attempts_kept(&self) -> bool {
            crate::auto_resume::lock_table(&self.resume).on_success(Self::SURFACE) > 0
        }

        fn wall_time_open(&mut self) -> bool {
            lock_state(&self.state)
                .take_wall_time(Self::SURFACE, u64::MAX)
                .is_some()
        }
    }

    /// Claude Code 2.1.283 이 백그라운드 서브에이전트를 남긴 채 보낸 Stop 의 background_tasks.
    fn running_subagent() -> Value {
        json!([{
            "id": "a05303b937811e7fc",
            "type": "subagent",
            "status": "running",
            "description": "README first line",
            "agent_type": "Explore",
        }])
    }

    fn gated_stop(prompt_id: &str) -> Value {
        json!({ "event": "stop", "surface": HookRig::SURFACE, "session": "sess-1", "prompt_id": prompt_id })
    }

    #[test]
    fn a_stop_the_gate_blocks_does_not_report_idle_in_either_arrival_order() {
        let block = json!({ "decision": "block", "reason": "checklist" });
        for gate_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut rig = HookRig::with_gates(dir.path(), 1);
            if gate_first {
                rig.gate(block.clone(), "p1");
            }
            let resp = rig.run(gated_stop("p1"));
            assert_eq!(resp["deferred"], "gate");
            if !gate_first {
                rig.gate(block.clone(), "p1");
            }
            assert_eq!(rig.host.states(), vec!["active"], "gate_first={gate_first}");
            assert!(rig.host.fired().is_empty(), "claude-idle must not fire");
            assert!(!rig.host.methods().iter().any(|m| m == "surface.completion"));
            assert!(!rig.host.methods().iter().any(|m| m == "telemetry.record"));
            assert!(rig.resume_attempts_kept());
            assert!(rig.wall_time_open());
        }
    }

    #[test]
    fn a_stop_every_gate_passes_reports_idle_once_the_decisions_arrive() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 2);
        rig.run(gated_stop("p1"));
        rig.gate(json!({}), "p1");
        // 두 번째 판정 전에는 idle 을 보고하지 않는다.
        assert_eq!(rig.host.states(), vec!["active"]);
        rig.gate(json!({}), "p1");
        assert_eq!(rig.host.states(), vec!["active", "idle"]);
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
        assert!(rig.host.methods().iter().any(|m| m == "surface.completion"));
        assert!(!rig.resume_attempts_kept());
        assert!(!rig.wall_time_open());
    }

    #[test]
    fn a_stop_without_an_attached_gate_reports_idle_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 0);
        let resp = rig.run(gated_stop("p1"));
        assert!(resp.get("deferred").is_none());
        assert_eq!(rig.host.states(), vec!["idle"]);
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
    }

    #[test]
    fn a_blocked_stop_is_followed_by_the_next_stop_of_the_same_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_stop("p1"));
        rig.gate(json!({ "decision": "block" }), "p1");
        rig.run(gated_stop("p1"));
        rig.gate(json!({}), "p1");
        assert_eq!(rig.host.states(), vec!["active", "active", "idle"]);
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
    }

    fn gated_stop_answering(prompt_id: &str, answer: &str) -> Value {
        let mut stop = gated_stop(prompt_id);
        stop["last_assistant_message"] = json!(answer);
        stop
    }

    fn ended(answer: &str) -> (String, Option<String>) {
        ("turn_ended".to_string(), Some(answer.to_string()))
    }

    /// 게이트가 두 번 이어 간 턴은 통과한 마지막 Stop 에서 한 번만 끝을 보고한다. 이어 간 Stop 의 답은 싣지 않는다.
    #[test]
    fn a_gated_turn_reports_its_end_once_at_the_stop_the_gates_let_through() {
        for gate_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut rig = HookRig::with_gates(dir.path(), 1);
            for (answer, decision) in [
                ("draft 1", json!({ "decision": "block" })),
                ("draft 2", json!({ "decision": "block" })),
                ("final", json!({})),
            ] {
                if gate_first {
                    rig.gate(decision.clone(), "p1");
                }
                rig.run(gated_stop_answering("p1", answer));
                if !gate_first {
                    rig.gate(decision, "p1");
                }
            }
            assert_eq!(
                rig.host.turn_reports(),
                vec![ended("final")],
                "gate_first={gate_first}"
            );
        }
    }

    /// 판정이 다 오지 않아 시간 초과로 끝을 확정한 Stop 도 한 번 보고한다.
    #[test]
    fn a_gated_stop_settled_by_the_timeout_reports_the_turn_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_stop_answering("p1", "late gate"));
        assert!(rig.host.turn_reports().is_empty());
        rig.tail().finish_expired(rig.take_expired());
        assert_eq!(rig.host.turn_reports(), vec![ended("late gate")]);
    }

    /// 판정 전에 새 프롬프트가 오면 보류한 Stop 의 끝을 새 턴의 시작보다 먼저 보고한다.
    #[test]
    fn a_new_prompt_reports_the_pending_stop_end_before_the_new_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_stop_answering("p1", "answer"));
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        assert_eq!(
            rig.host.turn_reports(),
            vec![ended("answer"), ("turn_started".to_string(), None)]
        );
    }

    /// 백그라운드 작업을 기다리는 Stop 은 게이트가 통과시켜도 턴 끝이 아니다.
    #[test]
    fn a_gated_waiting_stop_does_not_report_the_turn_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_waiting_stop("p1"));
        rig.gate(json!({}), "p1");
        assert!(rig.host.turn_reports().is_empty());
    }

    impl HookRig {
        /// 송신 스레드가 할 한 차례 송신을 하고 Codex 부모에게 보낸 결과를 (status, body) 로 꺼낸다.
        fn codex_returns(&self) -> Vec<(String, Option<String>)> {
            self.returns
                .pump_once(&self.host, crate::codex_return::now_ms());
            self.host.codex_returns()
        }

        /// 이 surface 의 Claude 를 Codex 부모가 spawn 했고 세션이 시작됐다.
        fn codex_child(&mut self) {
            self.returns
                .bind_new_child(
                    Self::SURFACE,
                    &crate::codex_return::Binding {
                        receiver_id: "receiver-1".into(),
                        parent_external: None,
                        attach: false,
                    },
                    true,
                )
                .expect("route stored");
            self.run(
                json!({ "event": "session-start", "surface": Self::SURFACE, "session": "sess-1" }),
            );
        }
    }

    fn returned(status: &str, body: Option<&str>) -> (String, Option<String>) {
        (status.to_string(), body.map(String::from))
    }

    /// 게이트가 이어 간 Stop 은 Codex 부모에게 보내지 않는다. 통과한 마지막 Stop 만 한 번 보낸다.
    #[test]
    fn a_gated_turn_returns_only_its_final_stop_to_the_codex_parent() {
        for gate_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut rig = HookRig::with_gates(dir.path(), 1);
            rig.codex_child();
            for (answer, decision) in [
                ("draft 1", json!({ "decision": "block" })),
                ("draft 2", json!({ "decision": "block" })),
                ("final", json!({})),
            ] {
                if gate_first {
                    rig.gate(decision.clone(), "p1");
                }
                rig.run(gated_stop_answering("p1", answer));
                if !gate_first {
                    rig.gate(decision, "p1");
                }
            }
            assert_eq!(
                rig.codex_returns(),
                vec![returned("turn_ended", Some("final"))],
                "gate_first={gate_first}"
            );
        }
    }

    /// 게이트 판정을 기다리는 Stop 이 있는 동안 새 프롬프트가 오면, 보류된 Stop 은 앞 턴의 결과로 확정되고
    /// 새 턴의 결과는 따로 한 번 나간다. 새 턴 순번을 먼저 올리면 앞 턴 본문이 새 턴 키로 나가고 새 턴
    /// 결과가 중복으로 버려진다.
    #[test]
    fn a_pending_gated_stop_is_settled_as_the_previous_turn_before_a_new_prompt_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.codex_child();
        let prompt = || json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" });
        rig.run(prompt());
        rig.run(gated_stop_answering("p1", "first"));
        rig.run(prompt());
        rig.gate(json!({}), "p2");
        rig.run(gated_stop_answering("p2", "second"));
        assert_eq!(
            rig.codex_returns(),
            vec![ended("first"), ended("second")],
            "each turn returns its own answer once"
        );
        let keys = rig.host.delivered_keys();
        assert_eq!(keys.len(), 2);
        assert!(keys[0].ends_with(":t1"), "{keys:?}");
        assert!(keys[1].ends_with(":t2"), "{keys:?}");
    }

    /// 확정 스레드가 시간 초과 Stop 을 꺼낸 뒤 보고가 늦어지는 사이 새 프롬프트가 두 번 오거나 새 턴의 최종
    /// Stop 까지 끝나도, 늦은 Stop 의 결과는 그 Stop 이 온 턴의 키로 한 번만 나간다. 새 턴 결과도 제 키로 한 번이다.
    #[test]
    fn a_delayed_timeout_report_keeps_the_key_of_the_turn_its_stop_came_in() {
        for newer_turn_finished in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut rig = HookRig::with_gates(dir.path(), 1);
            rig.codex_child();
            let prompt = || json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" });
            rig.run(prompt());
            rig.run(gated_stop_answering("p1", "first"));
            let expired = rig.take_expired();
            rig.run(prompt());
            rig.run(prompt());
            if newer_turn_finished {
                rig.gate(json!({}), "p3");
                rig.run(gated_stop_answering("p3", "third"));
            }
            rig.tail().finish_expired(expired);
            let mut got: Vec<(String, Option<String>)> = rig
                .codex_returns()
                .into_iter()
                .zip(rig.host.delivered_keys())
                .map(|((_, body), key)| (key[key.rfind(':').unwrap() + 1..].to_string(), body))
                .collect();
            got.sort();
            let mut want = vec![("t1".to_string(), Some("first".to_string()))];
            if newer_turn_finished {
                want.push(("t3".to_string(), Some("third".to_string())));
            }
            assert_eq!(got, want, "newer_turn_finished={newer_turn_finished}");
        }
    }

    /// 게이트 없는 Stop 도 턴을 Stop 이 온 때 정한다. 호스트 보고가 끝나기 전에 새 턴이 열려도 키가 바뀌지 않는다.
    #[test]
    fn an_ungated_stop_claims_the_turn_it_came_in_even_if_a_new_turn_opens_during_its_report() {
        struct OpensATurn<'a> {
            inner: &'a RecordingHost,
            returns: &'a crate::codex_return::Returns,
        }
        impl tasty_plugin_agent_common::host_call::HostCall for OpensATurn<'_> {
            fn call(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                if method == "agent.task_turn" || method.starts_with("agent.") {
                    // 보고가 오가는 사이 다른 워커가 새 프롬프트를 처리했다.
                    self.returns
                        .observe("prompt-submit", HookRig::SURFACE, Some("sess-1"), None);
                }
                self.inner.call(method, params)
            }
        }
        let mut rig = HookRig::new();
        rig.codex_child();
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        let host = OpensATurn {
            inner: &rig.host,
            returns: &rig.returns,
        };
        crate::task_turn::report(
            &host,
            &rig.returns,
            "stop",
            HookRig::SURFACE,
            Some("sess-1"),
            &json!({ "last_assistant_message": "one" }),
        );
        let keys = {
            rig.returns
                .pump_once(&rig.host, crate::codex_return::now_ms());
            rig.host.delivered_keys()
        };
        assert_eq!(keys.len(), 1, "{keys:?}");
        assert!(keys[0].ends_with(":t1"), "{keys:?}");
    }

    /// 시간 초과로 확정한 게이트 Stop 도 결과를 한 번 보낸다.
    #[test]
    fn a_gated_stop_settled_by_the_timeout_returns_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.codex_child();
        rig.run(gated_stop_answering("p1", "late gate"));
        assert!(rig.codex_returns().is_empty());
        rig.tail().finish_expired(rig.take_expired());
        assert_eq!(
            rig.codex_returns(),
            vec![returned("turn_ended", Some("late gate"))]
        );
    }

    /// 백그라운드 작업을 기다리는 Stop 은 결과가 아니다. 작업이 끝난 뒤 새 턴의 Stop 이 결과다.
    #[test]
    fn a_background_wait_stop_is_not_returned() {
        let mut rig = HookRig::new();
        rig.codex_child();
        rig.run(json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "session": "sess-1",
            "last_assistant_message": "waiting",
            "background_tasks": running_subagent(),
        }));
        assert!(rig.codex_returns().is_empty());
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        rig.run(json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "session": "sess-1",
            "last_assistant_message": "all done",
        }));
        assert_eq!(
            rig.codex_returns(),
            vec![returned("turn_ended", Some("all done"))]
        );
    }

    /// prompt_id 가 없어도 턴마다 한 번 보낸다. 같은 턴의 Stop 이 다시 와도 한 번이다.
    #[test]
    fn stops_without_a_prompt_id_return_once_per_turn() {
        let mut rig = HookRig::new();
        rig.codex_child();
        let stop = |answer: &str| json!({ "event": "stop", "surface": HookRig::SURFACE, "session": "sess-1", "last_assistant_message": answer });
        rig.run(stop("first"));
        rig.run(stop("first"));
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        rig.run(stop("second"));
        assert_eq!(
            rig.codex_returns(),
            vec![
                returned("turn_ended", Some("first")),
                returned("turn_ended", Some("second"))
            ]
        );
        let keys: Vec<Value> = rig
            .host
            .seen
            .borrow()
            .iter()
            .filter(|(m, _)| m == "codex.deliver")
            .map(|(_, p)| p["completion_key"].clone())
            .collect();
        assert_ne!(keys[0], keys[1]);
    }

    /// API 오류로 끝난 턴과 확정 전에 끝난 세션은 품질 판정이 아닌 각자의 상태로 보낸다.
    #[test]
    fn a_failed_turn_and_an_exit_keep_their_own_status() {
        let mut rig = HookRig::new();
        rig.codex_child();
        rig.run(json!({ "event": "stop-failure", "surface": HookRig::SURFACE, "session": "sess-1", "error": "rate_limit" }));
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        rig.run(
            json!({ "event": "session-end", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        assert_eq!(
            rig.codex_returns(),
            vec![
                returned("turn_failed", Some("rate_limit")),
                returned("exited", None)
            ]
        );
    }

    /// `/clear` 는 SessionEnd(reason `clear`) 뒤 SessionStart(source `clear`)로 새 세션을 연다.
    /// 그 SessionEnd 는 exited 가 아니고, 새 세션의 턴 결과는 같은 Codex 부모에게 간다.
    #[test]
    fn a_clear_keeps_returning_to_the_codex_parent() {
        let mut rig = HookRig::new();
        rig.codex_child();
        rig.run(json!({ "event": "session-end", "surface": HookRig::SURFACE,
            "session": "sess-1", "reason": "clear" }));
        rig.run(
            json!({ "event": "session-start", "surface": HookRig::SURFACE,
            "session": "sess-2", "source": "clear" }),
        );
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-2" }),
        );
        rig.run(
            json!({ "event": "stop", "surface": HookRig::SURFACE, "session": "sess-2",
            "last_assistant_message": "after clear" }),
        );
        assert_eq!(
            rig.codex_returns(),
            vec![returned("turn_ended", Some("after clear"))]
        );
    }

    /// Codex 부모가 없는 자식(일반 Claude 부모)은 Codex 로 아무것도 보내지 않는다.
    #[test]
    fn a_child_of_a_claude_parent_returns_nothing_to_codex() {
        let mut rig = HookRig::new();
        rig.run(
            json!({ "event": "session-start", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        rig.run(json!({ "event": "stop", "surface": HookRig::SURFACE, "session": "sess-1", "last_assistant_message": "x" }));
        assert!(rig.codex_returns().is_empty());
        assert_eq!(rig.host.turn_reports(), vec![ended("x")]);
    }

    fn gated_waiting_stop(prompt_id: &str) -> Value {
        json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "session": "sess-1",
            "prompt_id": prompt_id,
            "background_tasks": running_subagent(),
        })
    }

    #[test]
    fn a_timed_out_stop_is_not_reported_after_a_new_turn_started() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_stop("p1"));
        let expired = rig.take_expired();
        // 확정 스레드가 보내기 전에 워커가 새 턴을 처리했다.
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        rig.tail().finish_expired(expired);
        assert_eq!(rig.host.states(), vec!["active", "active"]);
        assert!(rig.host.fired().is_empty());
        // 새 턴이 없었으면 보낸다.
        rig.run(gated_stop("p2"));
        rig.tail().finish_expired(rig.take_expired());
        assert_eq!(
            rig.host.states(),
            vec!["active", "active", "active", "idle"]
        );
    }

    #[test]
    fn a_blocked_stop_after_a_background_wait_clears_the_wait_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_waiting_stop("p1"));
        rig.gate(json!({}), "p1");
        assert!(rig.is_waiting());
        rig.run(gated_stop("p1"));
        rig.gate(json!({ "decision": "block" }), "p1");
        assert!(!rig.is_waiting(), "대기가 아닌 Stop 은 대기 기록을 지운다");
        assert!(
            rig.host
                .unset_keys()
                .iter()
                .any(|k| k == BACKGROUND_WAIT_META_KEY)
        );
        assert!(rig.host.fired().is_empty());
    }

    /// 대기 Stop 의 판정은 그 Stop 과 짝지어진다. 짝짓지 않으면 다음 Stop 이 그 block 을 가져간다.
    #[test]
    fn a_waiting_stop_takes_its_own_gate_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_waiting_stop("p1"));
        rig.gate(json!({ "decision": "block" }), "p1");
        rig.run(gated_stop("p1"));
        rig.gate(json!({}), "p1");
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
    }

    #[test]
    fn the_deferred_idle_clears_the_background_wait_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_waiting_stop("p1"));
        rig.gate(json!({}), "p1");
        rig.run(gated_stop("p1"));
        rig.gate(json!({}), "p1");
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
        assert!(!rig.is_waiting());
    }

    /// 시간 초과로 확정한 Stop 의 판정이 다음 Stop 전에 늦게 오면 버린다. 그 Stop 과 짝짓지 않는다.
    #[test]
    fn a_late_decision_of_a_timed_out_stop_is_not_given_to_the_next_stop() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_stop("p1"));
        rig.tail().finish_expired(rig.take_expired());
        rig.gate(json!({ "decision": "block" }), "p1");
        rig.run(gated_stop("p1"));
        rig.gate(json!({}), "p1");
        assert_eq!(rig.host.states(), vec!["active", "idle", "active", "idle"]);
    }

    #[test]
    fn a_new_prompt_reports_the_pending_stop_idle_before_the_new_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_stop("p1"));
        rig.run(
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "sess-1" }),
        );
        assert_eq!(rig.host.states(), vec!["active", "idle", "active"]);
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
    }

    #[test]
    fn background_work_is_not_pending_without_either_field() {
        assert_eq!(background_work_pending(&json!({ "event": "stop" })), None);
    }

    #[test]
    fn background_work_is_not_pending_for_an_empty_list() {
        assert_eq!(
            background_work_pending(&json!({ "background_tasks": [] })),
            None
        );
    }

    #[test]
    fn background_work_is_not_pending_when_every_item_finished() {
        let done = json!([
            { "id": "a", "type": "shell", "status": "completed" },
            { "id": "b", "type": "subagent", "status": "failed" },
            { "id": "c", "type": "shell", "status": "Cancelled" },
            { "id": "d", "type": "monitor", "status": "killed" },
        ]);
        assert_eq!(
            background_work_pending(&json!({ "background_tasks": done })),
            None
        );
    }

    #[test]
    fn a_running_subagent_or_shell_or_pending_item_is_pending_work() {
        assert!(
            background_work_pending(&json!({ "background_tasks": running_subagent() })).is_some()
        );
        let shell =
            json!([{ "id": "b1", "type": "shell", "status": "running", "command": "sleep 15" }]);
        assert!(background_work_pending(&json!({ "background_tasks": shell })).is_some());
        // 방어적으로 읽는 모양: 문서·실측에 없는 status "pending" 과 type 이 없는 항목도 대기로 본다.
        let defensive =
            json!([{ "id": "t1", "name": "build", "status": "pending", "created_at": "x" }]);
        assert!(background_work_pending(&json!({ "background_tasks": defensive })).is_some());
    }

    /// 문서가 값 목록을 주지 않으므로 끝을 뜻하지 않는 모르는 값과 status 가 없는 항목은 대기다.
    #[test]
    fn an_unknown_or_missing_status_is_pending_work() {
        for item in [
            json!({ "id": "q", "type": "shell", "status": "queued" }),
            json!({ "id": "s", "type": "workflow", "status": "starting" }),
            json!({ "id": "n", "type": "subagent" }),
        ] {
            assert!(
                background_work_pending(&json!({ "background_tasks": [item] })).is_some(),
                "{item} 는 대기로 본다"
            );
        }
    }

    /// 플래그로 직접 준 값은 JSON 문자열로 온다.
    #[test]
    fn background_tasks_given_as_a_json_string_is_read_too() {
        let raw = running_subagent().to_string();
        assert!(background_work_pending(&json!({ "background_tasks": raw })).is_some());
        assert_eq!(
            background_work_pending(&json!({ "background_tasks": "not json" })),
            None,
            "해석할 수 없으면 이전처럼 턴 종료로 본다"
        );
    }

    #[test]
    fn waiting_on_background_work_decides_when_present() {
        let both =
            json!({ "waiting_on_background_work": false, "background_tasks": running_subagent() });
        assert_eq!(background_work_pending(&both), None);
        let waiting = json!({ "waiting_on_background_work": true, "background_tasks": [] });
        assert!(background_work_pending(&waiting).is_some());
        let as_text = json!({ "waiting_on_background_work": "true" });
        assert!(background_work_pending(&as_text).is_some());
    }

    /// 백그라운드 작업을 기다리는 Stop 은 턴 종료가 아니다. idle·완료 알림·화면 알림을 만들지 않고,
    /// wall_time 을 닫지 않으며 자동 재개의 성공 처리(시도 기록 삭제)도 하지 않는다.
    #[test]
    fn stop_with_running_background_tasks_keeps_the_child_active() {
        let mut rig = HookRig::new();
        let out = rig.run(json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "background_tasks": running_subagent(),
        }));
        assert_eq!(out["waiting"], "background_work");
        // 조회 둘은 끝난 작업의 선언을 찾으려고 무출력 선언·묶음 meta 를 되살리는 것이다.
        assert_eq!(
            rig.host.methods(),
            vec![
                "surface.meta.get",
                "surface.meta.get",
                "surface.meta.get",
                "terminal.set_state",
                "surface.meta.set"
            ]
        );
        assert_eq!(rig.host.states(), vec!["active"]);
        assert!(
            rig.resume_attempts_kept(),
            "자동 재개 시도 기록이 남아야 한다"
        );
        assert!(rig.wall_time_open(), "wall_time 이 열려 있어야 한다");
    }

    /// active 를 받는 순간 스캐너에 이전 턴의 정지 관측이 남아 있었는지 기록하는 호스트.
    struct TurnOrderProbe {
        scanner: Arc<Mutex<ErrorScanner>>,
        watched_at_active: std::cell::RefCell<Vec<bool>>,
    }

    impl HostCallSink for TurnOrderProbe {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            if method == "terminal.set_state" && params["state"] == "active" {
                let watching =
                    crate::error_scan::lock_scanner(&self.scanner).is_watching(HookRig::SURFACE);
                self.watched_at_active.borrow_mut().push(watching);
            }
            Ok(json!({}))
        }
    }

    /// 새 턴 hook 은 active 를 보내기 전에 정지 관측을 지운다. 순서가 반대면 그 사이의 스캔
    /// tick 이 이전 턴의 조용한 구간으로 거짓 정지 알림을 낼 수 있다.
    #[test]
    fn a_new_turn_clears_the_stall_watch_before_reporting_active() {
        for event in ["prompt-submit", "session-start", "active"] {
            let rig = HookRig::new();
            {
                let mut s = crate::error_scan::lock_scanner(&rig.scanner);
                s.enable(HookRig::SURFACE, crate::error_scan::ScanTarget::Child);
                s.settle_for_test(HookRig::SURFACE);
            }
            let probe = TurnOrderProbe {
                scanner: Arc::clone(&rig.scanner),
                watched_at_active: std::cell::RefCell::new(Vec::new()),
            };
            handle_claude_hook(
                &rig.state,
                &rig.scanner,
                &rig.resume,
                &rig.pairing,
                &rig.returns,
                &probe,
                &json!({ "event": event, "surface": HookRig::SURFACE }),
                None,
                &test_translator(),
            )
            .expect("hook handled");
            assert_eq!(
                *probe.watched_at_active.borrow(),
                vec![false],
                "{event}: active 를 보낼 때 이전 정지 관측이 없어야 한다"
            );
        }
    }

    /// hook 은 상태를 바꿀 수 있으므로, 조용한 구간의 상태 확인을 마친 대상도 다시 묻게 한다.
    /// 상태를 건드리지 않고 일찍 돌아가는 경로(하위 에이전트 실패)도 마찬가지다.
    #[test]
    fn every_hook_rearms_the_quiet_stretch_state_check() {
        for params in [
            json!({ "event": "prompt-submit", "surface": HookRig::SURFACE }),
            json!({ "event": "notification", "surface": HookRig::SURFACE }),
            json!({ "event": "stop-failure", "surface": HookRig::SURFACE, "agent_id": "sub-1" }),
        ] {
            let mut rig = HookRig::new();
            crate::error_scan::lock_scanner(&rig.scanner).settle_for_test(HookRig::SURFACE);
            rig.run(params.clone());
            assert!(
                !crate::error_scan::lock_scanner(&rig.scanner).is_settled(HookRig::SURFACE),
                "{params}"
            );
        }
    }

    /// 대기 Stop 은 정지 감시와 부모 알림 문구가 쓰도록 대기를 기록한다.
    #[test]
    fn a_waiting_stop_records_the_background_wait() {
        let mut rig = HookRig::new();
        rig.run(json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "background_tasks": running_subagent(),
        }));
        assert!(
            crate::error_scan::lock_scanner(&rig.scanner)
                .is_waiting_on_background_work(HookRig::SURFACE)
        );
        let seen = rig.host.seen.borrow();
        let (_, set) = seen
            .iter()
            .find(|(m, _)| m == "surface.meta.set")
            .expect("대기 meta");
        assert_eq!(set["key"], BACKGROUND_WAIT_META_KEY);
        let value: Value = serde_json::from_str(set["value"].as_str().unwrap()).unwrap();
        assert_eq!(value["tasks"], 1);
        assert_eq!(value["types"], json!(["subagent"]));
        assert!(value["since_ms"].as_u64().is_some_and(|ms| ms > 0));
    }

    /// Claude 가 끝난 SessionEnd 는 그 프로세스의 settings meta 를 지운다. `/clear`·`/resume` 은 같은
    /// 프로세스가 이어지므로 남긴다.
    #[test]
    fn a_session_end_that_ends_the_process_clears_the_settings_meta() {
        let key = crate::stop_pairing::SETTINGS_FILE_META_KEY.to_string();
        for (reason, cleared) in [
            (Some("prompt_input_exit"), true),
            (Some("other"), true),
            (Some("logout"), true),
            (None, true),
            (Some("clear"), false),
            (Some("resume"), false),
        ] {
            let mut rig = HookRig::new();
            rig.host.settings_session = Some("s-1".to_string());
            let mut params =
                json!({ "event": "session-end", "surface": HookRig::SURFACE, "session": "s-1" });
            if let Some(r) = reason {
                params["reason"] = json!(r);
            }
            rig.run(params);
            assert_eq!(rig.host.unset_keys().contains(&key), cleared, "{reason:?}");
        }
    }

    /// respawn·reboot 는 새 settings 를 기록한 뒤 이전 Claude 를 끝낸다. 이전 세션의 SessionEnd 는
    /// 경로를 소유하지 않으므로 새 경로를 지우지 않는다(2.1.290 에서 reason `other` 로 도착했다).
    #[test]
    fn a_session_end_of_a_session_that_does_not_own_the_settings_keeps_them() {
        let key = crate::stop_pairing::SETTINGS_FILE_META_KEY.to_string();
        for owner in [None, Some("s-new")] {
            let mut rig = HookRig::new();
            rig.host.settings_session = owner.map(String::from);
            rig.run(json!({ "event": "session-end", "surface": HookRig::SURFACE,
                "session": "s-old", "reason": "other" }));
            assert!(!rig.host.unset_keys().contains(&key), "{owner:?}");
        }
        // 세션 id 가 없는 SessionEnd 는 소유를 가릴 수 없어 남긴다.
        let mut rig = HookRig::new();
        rig.host.settings_session = Some("s-1".to_string());
        rig.run(json!({ "event": "session-end", "surface": HookRig::SURFACE }));
        assert!(!rig.host.unset_keys().contains(&key));
    }

    /// SessionStart 는 그 세션을 settings 경로의 소유 세션으로 기록한다.
    #[test]
    fn a_session_start_claims_the_settings_for_its_session() {
        let mut rig = HookRig::new();
        rig.run(
            json!({ "event": "session-start", "surface": HookRig::SURFACE,
            "session": "s-9", "source": "startup" }),
        );
        let owner = rig.host.seen.borrow().iter().any(|(m, p)| {
            m == "surface.meta.set"
                && p["key"] == crate::stop_pairing::SETTINGS_SESSION_META_KEY
                && p["value"] == "s-9"
        });
        assert!(owner);
    }

    fn session_start(source: Option<&str>) -> Value {
        let mut p =
            json!({ "event": "session-start", "surface": HookRig::SURFACE, "session": "s-9" });
        if let Some(source) = source {
            p["source"] = json!(source);
        }
        p
    }

    /// 입력 대기 화면으로 시작한 세션은 idle 이다. 턴이 끝난 것이 아니므로 완료 훅·화면 알림·
    /// wall_time 은 만들지 않는다(spawn·tell 완료 알림은 `claude-idle` 훅으로 나간다).
    #[test]
    fn a_session_that_starts_waiting_for_input_is_idle_without_a_completion() {
        for source in ["startup", "resume", "clear", "fork"] {
            let mut rig = HookRig::new();
            rig.run(session_start(Some(source)));
            assert_eq!(rig.host.states(), vec!["idle"], "{source}");
            assert!(rig.host.fired().is_empty(), "{source}");
            assert!(
                !rig.host
                    .methods()
                    .iter()
                    .any(|m| m == "surface.completion" || m == "telemetry.record"),
                "{source}"
            );
            assert!(rig.wall_time_open(), "{source}");
        }
    }

    /// source 가 없거나 모르는 값이면 판단하지 않고 active 로 둔다.
    #[test]
    fn an_unknown_start_stays_active() {
        for source in [None, Some("future")] {
            let mut rig = HookRig::new();
            rig.run(session_start(source));
            assert_eq!(rig.host.states(), vec!["active"], "{source:?}");
        }
    }

    fn compaction(session: &str) -> Value {
        json!({ "event": "session-start", "surface": HookRig::SURFACE, "session": session, "source": "compact" })
    }

    /// 턴이 끝나 쉬는 중의 압축(Claude Code idle compaction)은 Stop 의 idle 을 바꾸지 않는다.
    /// 그 뒤 Stop 이 오지 않으므로 active 로 바꾸면 끝난 자식이 계속 active·stale 로 남는다.
    /// 완료 훅·화면 알림·wall_time 시작도 새로 만들지 않고, 세션 기록은 그대로 갱신한다.
    #[test]
    fn a_compaction_while_idle_keeps_the_idle_state() {
        let mut rig = HookRig::new();
        rig.run(json!({ "event": "stop", "surface": HookRig::SURFACE, "session": "s-9" }));
        assert_eq!(rig.host.states(), vec!["idle"]);
        let fired = rig.host.fired().len();
        let completions = rig
            .host
            .methods()
            .iter()
            .filter(|m| *m == "surface.completion")
            .count();

        let resp = rig.run(compaction("s-9"));
        assert_eq!(
            rig.host.states(),
            vec!["idle"],
            "압축은 상태를 보고하지 않는다"
        );
        assert_eq!(resp["compaction"], true);
        assert_eq!(rig.host.fired().len(), fired, "claude-idle 등 새 훅 없음");
        assert_eq!(
            rig.host
                .methods()
                .iter()
                .filter(|m| *m == "surface.completion")
                .count(),
            completions,
            "새 화면 알림 없음"
        );
        assert!(!rig.wall_time_open(), "wall_time 을 다시 열지 않는다");
        assert!(
            rig.host
                .seen
                .borrow()
                .iter()
                .any(|(m, p)| m == "surface.meta.set"
                    && p["key"] == "claude-session-id"
                    && p["value"] == "s-9"),
            "세션 기록은 갱신한다"
        );
    }

    /// 턴 도중의 압축은 prompt-submit 의 active 를 그대로 두고 턴 경계 처리도 하지 않는다.
    /// 새 세션처럼 다루면 자동 재개의 연속 실패 수와 직전 턴 오류 기록이 턴 중간에 지워진다.
    #[test]
    fn a_compaction_during_a_turn_keeps_the_turn_running() {
        let mut rig = HookRig::new();
        rig.run(json!({ "event": "prompt-submit", "surface": HookRig::SURFACE, "session": "s-9" }));
        let unsets = rig.host.unset_keys().len();
        rig.run(compaction("s-9"));
        assert!(
            rig.resume_attempts_kept(),
            "자동 재개 계수를 새 세션처럼 지우지 않는다"
        );
        assert_eq!(
            rig.host.unset_keys().len(),
            unsets,
            "턴 meta 를 지우지 않는다"
        );
        assert_eq!(rig.host.states(), vec!["active"]);
    }

    /// 백그라운드 작업을 기다리는 중의 압축은 대기를 끝내지 않는다.
    #[test]
    fn a_compaction_while_waiting_on_background_work_keeps_the_wait() {
        let mut rig = HookRig::new();
        rig.run(json!({
            "event": "stop", "surface": HookRig::SURFACE, "session": "s-9",
            "background_tasks": running_subagent(),
        }));
        assert!(rig.is_waiting());
        rig.run(compaction("s-9"));
        assert!(rig.is_waiting(), "대기 기록 유지");
        assert!(
            !rig.host
                .unset_keys()
                .iter()
                .any(|k| k == BACKGROUND_WAIT_META_KEY),
            "대기 meta 유지"
        );
        assert_eq!(rig.host.states(), vec!["active"]);
    }

    /// 게이트 판정을 기다리는 Stop 은 압축으로 턴 종료가 확정되지 않는다. 판정이 와야 확정된다.
    #[test]
    fn a_compaction_does_not_settle_a_stop_waiting_for_its_gate() {
        let dir = tempfile::tempdir().unwrap();
        let mut rig = HookRig::with_gates(dir.path(), 1);
        rig.run(gated_stop("p1"));
        assert_eq!(rig.host.states(), vec!["active"]);
        rig.run(compaction("sess-1"));
        assert_eq!(
            rig.host.states(),
            vec!["active"],
            "압축은 보류 Stop 을 확정하지 않는다"
        );
        rig.gate(json!({}), "p1");
        assert_eq!(rig.host.states(), vec!["active", "idle"]);
    }

    /// 프롬프트를 실어 실행한 세션은 곧 그 턴을 시작하므로 active 로 두고 표시를 지운다.
    #[test]
    fn a_session_launched_with_a_prompt_stays_active_and_drops_the_mark() {
        let mut rig = HookRig::new();
        rig.host.launch_prompt = true;
        rig.run(session_start(Some("startup")));
        assert_eq!(rig.host.states(), vec!["active"]);
        assert!(
            rig.host
                .unset_keys()
                .iter()
                .any(|k| k == LAUNCH_PROMPT_META_KEY)
        );
    }

    /// 2.1.290 실측 `PostToolUse` 처럼 Bash 를 백그라운드로 띄운 도구 호출을 알린다.
    fn start_background_shell(rig: &mut HookRig) {
        rig.run(json!({
            "event": BACKGROUND_START_EVENT,
            "surface": HookRig::SURFACE,
            "tool_name": "Bash",
            "tool_response": { "stdout": "", "stderr": "", "interrupted": false, "backgroundTaskId": "bwny9udnf" },
        }));
    }

    /// Claude Code 2.1.319 실측 `PostToolUse`(Monitor)의 알림.
    fn start_monitor(rig: &mut HookRig, id: &str, agent_id: Option<&str>) {
        let mut params = json!({
            "event": BACKGROUND_START_EVENT,
            "surface": HookRig::SURFACE,
            "tool_name": "Monitor",
            "tool_response": { "taskId": id, "timeoutMs": 1500000, "persistent": false },
        });
        if let Some(agent) = agent_id {
            params["agent_id"] = json!(agent);
        }
        let reply = rig.run(params);
        assert_eq!(reply["background_task"], id);
    }

    /// Claude Code 2.1.319 실측 Stop 항목. Monitor 도 `type: shell` 이다.
    fn shell_item(id: &str, description: &str) -> Value {
        json!({ "id": id, "type": "shell", "status": "running", "description": description, "command": "x" })
    }

    fn stop_with(rig: &mut HookRig, items: Value) -> Value {
        rig.run(json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "session": "s-monitor",
            "cwd": "/w",
            "background_tasks": items,
        }))
    }

    /// 마지막 대기 meta.
    fn last_wait_meta(rig: &HookRig) -> Value {
        let seen = rig.host.seen.borrow();
        let (_, set) = seen
            .iter()
            .rev()
            .find(|(m, p)| m == "surface.meta.set" && p["key"] == BACKGROUND_WAIT_META_KEY)
            .expect("대기 meta");
        serde_json::from_str(set["value"].as_str().unwrap()).unwrap()
    }

    /// Monitor 만 남은 Stop: 상태·대기 기록은 그대로이고 정지 알림 대상에서만 빠진다.
    #[test]
    fn a_stop_waiting_only_on_a_monitor_stays_active_without_a_notice_target() {
        let mut rig = HookRig::new();
        start_monitor(&mut rig, "bym7ibssy", None);
        let reply = stop_with(&mut rig, json!([shell_item("bym7ibssy", "quiet monitor")]));
        assert_eq!(reply["waiting"], "background_work");
        assert_eq!(rig.host.states(), vec!["active"]);
        assert!(!rig.host.fired().contains(&"claude-idle".to_string()));
        assert!(rig.is_waiting());
        let meta = last_wait_meta(&rig);
        assert_eq!(meta["types"], json!(["shell"]));
        assert_eq!(meta["notice_types"], json!([]));
        assert!(meta.get("watch").is_none(), "{meta}");
    }

    /// Monitor 와 셸이 함께면 셸만으로 판정한다.
    #[test]
    fn a_monitor_next_to_a_shell_is_left_out_of_the_notice() {
        let mut rig = HookRig::new();
        start_monitor(&mut rig, "bym7ibssy", None);
        stop_with(
            &mut rig,
            json!([
                shell_item("bym7ibssy", "quiet monitor"),
                shell_item("baaw0pnlp", "probe silent bash")
            ]),
        );
        let meta = last_wait_meta(&rig);
        assert_eq!(meta["types"], json!(["shell", "shell"]));
        assert_eq!(meta["notice_types"], json!(["shell"]));
        let tasks = meta["watch"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1, "{meta}");
        assert_eq!(tasks[0]["id"], "baaw0pnlp");
        assert_eq!(tasks[0]["label"], "probe silent bash");
    }

    /// 기록이 없으면(옛 훅 설치) 셸로 본다. `type: monitor` 는 기록 없이도 Monitor 다.
    #[test]
    fn an_unrecorded_shell_is_noticed_and_a_monitor_type_is_not() {
        let mut rig = HookRig::new();
        stop_with(&mut rig, json!([shell_item("bym7ibssy", "unknown")]));
        assert_eq!(last_wait_meta(&rig)["notice_types"], json!(["shell"]));
        stop_with(
            &mut rig,
            json!([{ "id": "m1", "type": "monitor", "status": "running" }]),
        );
        assert_eq!(last_wait_meta(&rig)["notice_types"], json!([]));
    }

    /// 플러그인이 다시 시작돼도 기록은 surface meta 에 남는다. 첫 Stop 전에 재시작해도 같다.
    /// 중간 이벤트의 새 턴은 기록을 지우지 않고, `<status>` 가 있는 끝 알림만 지운다.
    #[test]
    fn the_monitor_record_survives_restarts_and_events_until_the_end_notice() {
        let mut rig = HookRig::new();
        start_monitor(&mut rig, "bkmcn04ns", None);
        rig.state = Mutex::new(ClaudeState::default());
        rig.scanner = Arc::new(Mutex::new(ErrorScanner::new()));
        stop_with(&mut rig, json!([shell_item("bkmcn04ns", "ticks")]));
        assert_eq!(last_wait_meta(&rig)["notice_types"], json!([]));

        rig.run(json!({
            "event": "prompt-submit",
            "surface": HookRig::SURFACE,
            "prompt": "<task-notification>\n<task-id>bkmcn04ns</task-id>\n<summary>Monitor event: \"ticks\"</summary>\n<event>tick 1</event>\n</task-notification>",
        }));
        assert!(!rig.is_waiting(), "새 턴은 대기를 끝낸다");
        rig.state = Mutex::new(ClaudeState::default());
        rig.scanner = Arc::new(Mutex::new(ErrorScanner::new()));
        stop_with(&mut rig, json!([shell_item("bkmcn04ns", "ticks")]));
        assert_eq!(
            last_wait_meta(&rig)["notice_types"],
            json!([]),
            "이벤트 뒤 재시작해도 Monitor 다"
        );

        rig.run(json!({
            "event": "prompt-submit",
            "surface": HookRig::SURFACE,
            "prompt": "<task-notification>\n<task-id>bkmcn04ns</task-id>\n<output-file>/t</output-file>\n<status>completed</status>\n</task-notification>",
        }));
        assert_eq!(*rig.host.monitor_meta.borrow(), None, "끝 알림이 지운다");
    }

    /// 서브에이전트가 띄운 Monitor. 그 전의 Stop 목록에 없어도 기록을 지우지 않고, 뒤에 나타나면 Monitor 다.
    #[test]
    fn a_subagent_monitor_missing_from_a_stop_list_stays_recorded() {
        let mut rig = HookRig::new();
        start_monitor(&mut rig, "bqmug8c54", Some("a22d7fc9077e1353c"));
        stop_with(
            &mut rig,
            json!([{ "id": "a22d7fc9077e1353c", "type": "subagent", "status": "running" }]),
        );
        assert_eq!(last_wait_meta(&rig)["notice_types"], json!(["subagent"]));
        stop_with(&mut rig, json!([shell_item("bqmug8c54", "sub monitor")]));
        assert_eq!(last_wait_meta(&rig)["notice_types"], json!([]));
    }

    /// 첫 Stop 전의 Monitor 는 StopFailure 판정의 기록에 들어가지 않는다. 지금처럼 턴이 끝난다.
    #[test]
    fn a_stop_failure_with_only_a_monitor_before_any_stop_goes_idle_as_before() {
        let mut rig = HookRig::new();
        start_monitor(&mut rig, "bym7ibssy", None);
        rig.run(
            json!({ "event": "stop-failure", "surface": HookRig::SURFACE, "error": "overloaded" }),
        );
        assert_stop_failure_ended_the_turn(&mut rig);
    }

    /// Stop 으로 기록된 Monitor 가 남은 StopFailure: 상태와 `types` 는 그대로이고 정지 알림 대상만 빠진다.
    #[test]
    fn a_stop_failure_while_a_monitor_runs_keeps_its_state_and_wording() {
        let mut rig = HookRig::new();
        start_monitor(&mut rig, "bym7ibssy", None);
        stop_with(&mut rig, json!([shell_item("bym7ibssy", "quiet monitor")]));
        let reply = rig.run(json!({
            "event": "stop-failure",
            "surface": HookRig::SURFACE,
            "session": "s-monitor",
            "cwd": "/w",
            "error": "overloaded",
        }));
        assert_eq!(reply["waiting"], "background_work");
        assert_eq!(rig.host.states(), vec!["active", "active"]);
        assert_eq!(rig.host.fired(), vec![STOP_FAILURE_EVENT]);
        let meta = last_wait_meta(&rig);
        assert_eq!(meta["types"], json!(["shell"]), "문구가 읽는 값");
        assert_eq!(meta["notice_types"], json!([]));
        assert!(meta.get("watch").is_none(), "{meta}");
    }

    /// 새 세션과 세션 종료는 기록을 지우고, 압축은 같은 세션이라 남긴다.
    #[test]
    fn session_boundaries_clear_the_monitor_record_but_compaction_keeps_it() {
        let mut rig = HookRig::new();
        start_monitor(&mut rig, "bym7ibssy", None);
        rig.run(json!({
            "event": "session-start",
            "surface": HookRig::SURFACE,
            "session": "s-monitor",
            "source": "compact",
        }));
        assert!(rig.host.monitor_meta.borrow().is_some(), "압축");
        rig.run(json!({
            "event": "session-start",
            "surface": HookRig::SURFACE,
            "session": "s-new",
            "source": "startup",
        }));
        assert_eq!(*rig.host.monitor_meta.borrow(), None, "새 세션");

        start_monitor(&mut rig, "bym7ibssy", None);
        rig.run(json!({ "event": "session-end", "surface": HookRig::SURFACE, "session": "s-new" }));
        assert_eq!(*rig.host.monitor_meta.borrow(), None, "세션 종료");
    }

    /// 턴 종료로 처리한 StopFailure 의 흔적(idle·오류 이벤트·완료 알림·wall_time 소비·자동 재개 성공 처리).
    fn assert_stop_failure_ended_the_turn(rig: &mut HookRig) {
        assert_eq!(rig.host.states().last().map(String::as_str), Some("idle"));
        assert!(rig.host.fired().contains(&"claude-idle".to_string()));
        assert!(rig.host.fired().contains(&STOP_FAILURE_EVENT.to_string()));
        assert!(
            rig.host
                .methods()
                .contains(&"surface.completion".to_string())
        );
        assert!(!rig.is_waiting());
    }

    /// 백그라운드 작업이 남은 채 API 오류로 끝난 턴은 대기다. active 를 보내고 대기와 오류를 기록한 뒤
    /// 부모 알림용 `claude-stop-failure` 만 fire 한다. idle·`claude-idle`·완료 알림·wall_time·자동 재개
    /// 성공 처리는 만들지 않는다. 알림 훅이 두 meta 를 읽으므로 meta 기록이 이벤트보다 먼저다.
    #[test]
    fn a_stop_failure_while_background_work_runs_keeps_the_child_active() {
        let mut rig = HookRig::new();
        start_background_shell(&mut rig);
        assert!(
            rig.host.seen.borrow().is_empty(),
            "작업 시작은 호스트를 부르지 않는다"
        );
        let reply = rig.run(json!({
            "event": "stop-failure",
            "surface": HookRig::SURFACE,
            "error": "server_error",
        }));
        assert_eq!(reply["waiting"], "background_work");
        assert_eq!(rig.host.states(), vec!["active"]);
        assert_eq!(rig.host.fired(), vec![STOP_FAILURE_EVENT]);
        assert!(
            !rig.host
                .methods()
                .contains(&"surface.completion".to_string())
        );
        assert!(rig.is_waiting());
        let seen = rig.host.seen.borrow();
        let meta = |key: &str| {
            seen.iter()
                .position(|(m, p)| m == "surface.meta.set" && p["key"] == key)
                .expect(key)
        };
        let fire = seen
            .iter()
            .position(|(m, _)| m == "surface.fire_hook")
            .expect("fire");
        assert!(meta(STOP_FAILURE_META_KEY) < fire && meta(BACKGROUND_WAIT_META_KEY) < fire);
        assert_eq!(seen[meta(STOP_FAILURE_META_KEY)].1["value"], "server_error");
        let value: Value = serde_json::from_str(
            seen[meta(BACKGROUND_WAIT_META_KEY)].1["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["types"], json!(["shell"]));
        drop(seen);
        assert!(rig.wall_time_open());
        assert!(rig.resume_attempts_kept());
    }

    /// 대기 Stop 뒤에 작업이 끝났다는 알림 없이 온 StopFailure 도 대기다.
    #[test]
    fn a_stop_failure_after_a_waiting_stop_keeps_waiting() {
        let mut rig = HookRig::new();
        rig.run(json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "background_tasks": running_subagent(),
        }));
        rig.run(
            json!({ "event": "stop-failure", "surface": HookRig::SURFACE, "error": "overloaded" }),
        );
        assert_eq!(rig.host.states(), vec!["active", "active"]);
        assert_eq!(rig.host.fired(), vec![STOP_FAILURE_EVENT]);
        assert!(rig.is_waiting());
    }

    /// 작업이 끝났다고 알려졌거나 Stop 이 남은 작업이 없다고 하면 StopFailure 는 전처럼 턴을 끝낸다.
    #[test]
    fn a_stop_failure_after_the_background_work_ended_goes_idle() {
        let finished = [
            json!({
                "event": "prompt-submit",
                "prompt": "<task-notification>\n<task-id>bwny9udnf</task-id>\n<status>completed</status>\n</task-notification>",
            }),
            json!({ "event": "stop", "background_tasks": [] }),
            json!({ "event": "session-start", "session": "s-2" }),
        ];
        for mut ended in finished {
            let mut rig = HookRig::new();
            start_background_shell(&mut rig);
            ended["surface"] = json!(HookRig::SURFACE);
            rig.run(ended.clone());
            rig.host.seen.borrow_mut().clear();
            rig.run(json!({ "event": "stop-failure", "surface": HookRig::SURFACE, "error": "overloaded" }));
            assert_stop_failure_ended_the_turn(&mut rig);
        }
    }

    /// 작업 선언은 그 작업이 끝났다는 `<task-notification>` 이나 그 작업이 빠진 Stop 에서 끝난다.
    /// 쌓인 것이 있는 양수 선언이면 부모 훅을 부르고, `-1` 이면 부르지 않는다.
    #[test]
    fn a_task_declaration_ends_with_its_task() {
        let notice =
            || crate::error_scan::quiet::QuietNotice::Tasks(vec![("T".into(), "sleep 600".into())]);
        let released = |rig: &HookRig| {
            rig.host
                .fired()
                .iter()
                .filter(|e| *e == crate::error_scan::quiet::QUIET_RELEASED_EVENT)
                .count()
        };
        for (secs, expected) in [(600, 1), (-1, 0)] {
            let mut rig = HookRig::new();
            {
                let mut s = crate::error_scan::lock_scanner(&rig.scanner);
                s.declare_quiet(&rig.host, HookRig::SURFACE, Some("T".into()), secs);
                s.dispose_quiet(&rig.host, HookRig::SURFACE, notice(), 1);
            }
            rig.run(json!({
                "event": "prompt-submit",
                "surface": HookRig::SURFACE,
                "prompt": "<task-notification>\n<task-id>T</task-id>\n</task-notification>",
            }));
            assert_eq!(released(&rig), expected, "secs {secs}");

            let mut rig = HookRig::new();
            {
                let mut s = crate::error_scan::lock_scanner(&rig.scanner);
                s.declare_quiet(&rig.host, HookRig::SURFACE, Some("T".into()), secs);
                s.dispose_quiet(&rig.host, HookRig::SURFACE, notice(), 1);
            }
            stop_with(&mut rig, json!([shell_item("U", "other")]));
            assert_eq!(released(&rig), expected, "secs {secs}");
        }
    }

    /// 백그라운드 작업을 띄운 적 없는 StopFailure 는 지금처럼 idle 이다.
    #[test]
    fn a_stop_failure_without_background_work_goes_idle() {
        let mut rig = HookRig::new();
        rig.run(
            json!({ "event": "stop-failure", "surface": HookRig::SURFACE, "error": "overloaded" }),
        );
        assert_stop_failure_ended_the_turn(&mut rig);
    }

    /// 대기 Stop 뒤 남은 작업이 없는 StopFailure 는 턴 종료이므로 대기 기록을 지운다.
    #[test]
    fn a_stop_failure_with_nothing_left_clears_the_wait_record() {
        let mut rig = HookRig::new();
        rig.run(json!({
            "event": "stop",
            "surface": HookRig::SURFACE,
            "background_tasks": running_subagent(),
        }));
        rig.run(json!({
            "event": "prompt-submit",
            "surface": HookRig::SURFACE,
            "prompt": "<task-notification><task-id>a05303b937811e7fc</task-id></task-notification>",
        }));
        rig.run(
            json!({ "event": "stop-failure", "surface": HookRig::SURFACE, "error": "overloaded" }),
        );
        assert!(!rig.is_waiting());
        assert!(
            rig.host
                .unset_keys()
                .contains(&BACKGROUND_WAIT_META_KEY.to_string())
        );
    }

    /// 대기 기록은 턴 종료·새 턴·세션 종료에서 지운다. 서브에이전트 종료와 알림은 지우지 않는다.
    #[test]
    fn the_background_wait_record_is_cleared_by_turn_end_new_turn_and_session_end() {
        let clears = [
            json!({ "event": "stop", "background_tasks": [] }),
            json!({ "event": "prompt-submit" }),
            json!({ "event": "session-start", "session": "s-1" }),
            json!({ "event": "session-end" }),
            json!({ "event": "active" }),
        ];
        for mut params in clears {
            let mut rig = HookRig::new();
            rig.run(json!({
                "event": "stop",
                "surface": HookRig::SURFACE,
                "background_tasks": running_subagent(),
            }));
            params["surface"] = json!(HookRig::SURFACE);
            rig.run(params.clone());
            assert!(
                !crate::error_scan::lock_scanner(&rig.scanner)
                    .is_waiting_on_background_work(HookRig::SURFACE),
                "{params}"
            );
            assert!(
                rig.host
                    .unset_keys()
                    .contains(&BACKGROUND_WAIT_META_KEY.to_string()),
                "{params}"
            );
        }
        for mut params in [
            json!({ "event": "subagent-stop", "agent_id": "a1" }),
            json!({ "event": "notification", "notification_type": "idle_prompt" }),
        ] {
            let mut rig = HookRig::new();
            rig.run(json!({
                "event": "stop",
                "surface": HookRig::SURFACE,
                "background_tasks": running_subagent(),
            }));
            params["surface"] = json!(HookRig::SURFACE);
            rig.run(params.clone());
            assert!(
                crate::error_scan::lock_scanner(&rig.scanner)
                    .is_waiting_on_background_work(HookRig::SURFACE),
                "{params}"
            );
        }
    }

    #[test]
    fn stop_with_empty_background_tasks_is_idle_as_before() {
        let mut rig = HookRig::new();
        rig.run(json!({ "event": "stop", "surface": HookRig::SURFACE, "background_tasks": [] }));
        assert_eq!(rig.host.states(), vec!["idle"]);
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
        assert!(
            rig.host
                .methods()
                .contains(&"surface.completion".to_string())
        );
        assert!(rig.host.methods().contains(&"telemetry.record".to_string()));
        assert!(
            rig.host
                .unset_keys()
                .contains(&crate::auto_resume::COUNT_META_KEY.to_string()),
            "성공한 턴은 자동 재개 시도 기록을 지운다"
        );
        assert!(!rig.resume_attempts_kept());
    }

    /// 필드를 보내지 않는 옛 Claude Code 에서도 동작은 그대로다.
    #[test]
    fn stop_without_background_fields_is_idle_as_before() {
        let mut rig = HookRig::new();
        rig.run(json!({ "event": "stop", "surface": HookRig::SURFACE }));
        assert_eq!(rig.host.states(), vec!["idle"]);
        assert_eq!(rig.host.fired(), vec!["claude-idle"]);
    }

    #[test]
    fn stop_with_only_finished_background_tasks_is_idle() {
        let mut rig = HookRig::new();
        let done = json!([{ "id": "b1", "type": "shell", "status": "completed" }]);
        rig.run(json!({ "event": "stop", "surface": HookRig::SURFACE, "background_tasks": done }));
        assert_eq!(rig.host.states(), vec!["idle"]);
    }

    /// 서브에이전트 종료는 처리기 전체를 거쳐도 호스트 호출·wall_time·자동 재개에 흔적을 남기지 않는다.
    #[test]
    fn subagent_stop_through_the_handler_leaves_no_trace() {
        let mut rig = HookRig::new();
        rig.run(json!({
            "event": "subagent-stop",
            "surface": HookRig::SURFACE,
            "agent_id": "a8724a00da74ad1c7",
            "background_tasks": running_subagent(),
        }));
        assert!(rig.host.methods().is_empty(), "{:?}", rig.host.methods());
        assert!(rig.resume_attempts_kept());
        assert!(rig.wall_time_open());
    }

    /// 시작·종료의 호스트 호출이 모두 실패해도 열네 개를 모두 시도하고 실패 횟수를 반환해야 한다.
    #[test]
    fn a_dead_surface_makes_every_host_call_fail_and_the_count_is_fourteen() {
        let tr = test_translator();
        let host = FlakyHost::failing_everything();

        let start = apply_hook("session-start", 100, Some("sess-1"), None, None, &tr).unwrap();
        let end = apply_hook("session-end", 100, None, None, None, &tr).unwrap();
        let failures = deliver_all(&host, &start) + deliver_all(&host, &end);

        assert_eq!(failures, 14, "시도한 호출: {:?}", host.seen.borrow());
        assert_eq!(
            host.seen.borrow().len(),
            14,
            "실패 횟수와 시도한 호출 수가 같아야 한다"
        );
    }

    /// 같은 호출이 모두 성공하면 실패 횟수는 0이어야 한다.
    #[test]
    fn a_live_host_makes_the_same_two_events_report_zero_failures() {
        let tr = test_translator();
        let host = FlakyHost::healthy();

        let start = apply_hook("session-start", 100, Some("sess-1"), None, None, &tr).unwrap();
        let end = apply_hook("session-end", 100, None, None, None, &tr).unwrap();
        let failures = deliver_all(&host, &start) + deliver_all(&host, &end);

        assert_eq!(failures, 0);
        assert_eq!(
            host.seen.borrow().len(),
            14,
            "실패가 없어도 같은 호출을 모두 시도해야 한다"
        );
    }

    /// 일부 호출이 실패해도 나머지를 계속 시도하고 실패한 수만 센다.
    #[test]
    fn only_the_failing_calls_are_counted() {
        let tr = test_translator();
        let host = FlakyHost {
            fail: vec!["surface.meta.unset"],
            seen: std::cell::RefCell::new(Vec::new()),
        };

        let end = apply_hook("session-end", 100, None, None, None, &tr).unwrap();
        assert_eq!(
            deliver_all(&host, &end),
            5,
            "meta.unset 은 이 계획에 다섯이다"
        );
        assert_eq!(
            host.seen.borrow().len(),
            8,
            "실패 뒤에도 나머지 호출을 시도해야 한다"
        );
    }

    #[test]
    fn stop_sets_idle_and_emits_fire_hook() {
        let calls = apply_hook("stop", 100, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: BACKGROUND_WAIT_META_KEY,
                },
                HostCall::SetState {
                    surface_id: 100,
                    state: "idle",
                },
                HostCall::FireHook {
                    surface_id: 100,
                    event: "claude-idle",
                },
                HostCall::SurfaceCompletion {
                    surface_id: 100,
                    kind: "completion",
                },
            ]
        );
    }

    /// 서브에이전트 종료는 메인 턴의 끝이 아니다. 턴 도중에 와도 자식을 idle 로 만들거나
    /// 완료 알림(claude-idle)·화면 알림을 내면 spawn/tell 대기 노드가 조기에 끝난다.
    #[test]
    fn subagent_stop_does_not_mark_the_main_session_idle() {
        let calls = apply_hook("subagent-stop", 7, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            Vec::new(),
            "subagent-stop 은 호스트 호출을 만들지 않는다"
        );
    }

    #[test]
    fn notification_sets_needs_input_and_fires_needs_input() {
        let calls = apply_hook("notification", 100, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::SetState {
                    surface_id: 100,
                    state: "needs_input",
                },
                HostCall::FireHook {
                    surface_id: 100,
                    event: "needs-input",
                },
                HostCall::SurfaceCompletion {
                    surface_id: 100,
                    kind: "needs_input",
                },
            ]
        );
    }

    #[test]
    fn notification_permission_prompt_sets_needs_input() {
        let calls = apply_hook(
            "notification",
            100,
            None,
            Some("permission_prompt"),
            None,
            &test_translator(),
        )
        .unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::SetState {
                    surface_id: 100,
                    state: "needs_input",
                },
                HostCall::FireHook {
                    surface_id: 100,
                    event: "needs-input",
                },
                HostCall::SurfaceCompletion {
                    surface_id: 100,
                    kind: "needs_input",
                },
            ]
        );
    }

    #[test]
    fn notification_idle_prompt_does_not_set_needs_input() {
        // idle_prompt는 입력 대기 상태를 만들지 않는다.
        let calls = apply_hook(
            "notification",
            100,
            None,
            Some("idle_prompt"),
            None,
            &test_translator(),
        )
        .unwrap();
        assert!(calls.is_empty());
    }

    fn notification_calls(ty: &str) -> Vec<HostCall> {
        apply_hook(
            "notification",
            100,
            None,
            Some(ty),
            None,
            &test_translator(),
        )
        .unwrap()
    }

    #[test]
    fn notifications_that_wait_for_input_set_needs_input() {
        for ty in [
            "permission_prompt",
            "elicitation_dialog",
            "elicitation_url_dialog",
            "quota_auto_resume_stale",
            "agent_needs_input",
        ] {
            assert_eq!(
                notification_calls(ty),
                vec![
                    HostCall::SetState {
                        surface_id: 100,
                        state: "needs_input",
                    },
                    HostCall::FireHook {
                        surface_id: 100,
                        event: "needs-input",
                    },
                    HostCall::SurfaceCompletion {
                        surface_id: 100,
                        kind: "needs_input",
                    },
                ],
                "{ty}"
            );
        }
    }

    /// 입력 대기가 아닌 알림이 턴 도중에 오면 needs_input 이 spawn/tell 대기 노드를 조기에 끝낸다.
    #[test]
    fn notifications_that_do_not_wait_for_input_leave_the_state_alone() {
        for ty in [
            "idle_prompt",
            "auth_success",
            "agent_completed",
            "elicitation_complete",
            "elicitation_response",
            "quota_auto_resume_disabled",
            "future_type",
        ] {
            assert_eq!(notification_calls(ty), Vec::new(), "{ty}");
        }
    }

    #[test]
    fn quota_auto_resume_fired_marks_the_child_active() {
        assert_eq!(
            notification_calls("quota_auto_resume_fired"),
            vec![HostCall::SetState {
                surface_id: 100,
                state: "active",
            }]
        );
        // 작업 재개 알림은 새 턴이 아니므로 claude-error 중복 기록을 초기화하지 않는다.
        assert!(!is_new_turn_event("notification"));
    }

    #[test]
    fn every_listed_notification_type_is_covered_by_the_tests() {
        let tested = [
            "permission_prompt",
            "elicitation_dialog",
            "elicitation_url_dialog",
            "quota_auto_resume_stale",
            "agent_needs_input",
            "idle_prompt",
            "auth_success",
            "agent_completed",
            "elicitation_complete",
            "elicitation_response",
            "quota_auto_resume_disabled",
            "quota_auto_resume_fired",
        ];
        let listed: std::collections::BTreeSet<&str> =
            NOTIFICATION_EFFECTS.iter().map(|(name, _)| *name).collect();
        assert_eq!(listed, tested.into_iter().collect());
        assert_eq!(listed.len(), NOTIFICATION_EFFECTS.len(), "중복 항목");
    }

    /// 문서 표 "Notification 유형별 상태" 는 [`NOTIFICATION_EFFECTS`] 의 사본이다.
    /// 유형 이름과 효과를 양방향으로 비교한다.
    #[test]
    fn the_docs_notification_table_matches_the_effect_list() {
        let doc = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/plugins/claude/index.md"),
        )
        .unwrap();
        let rows = notification_doc_rows(&doc);
        let code: std::collections::BTreeMap<String, &str> = NOTIFICATION_EFFECTS
            .iter()
            .map(|(name, effect)| (name.to_string(), effect_doc_label(*effect)))
            .collect();
        assert_eq!(rows, code);
    }

    fn effect_doc_label(effect: NotificationEffect) -> &'static str {
        match effect {
            NotificationEffect::NeedsInput => "needs_input",
            NotificationEffect::Active => "active",
            NotificationEffect::Unchanged => "변경 없음",
        }
    }

    /// `#### Notification 유형별 상태` 제목 아래 첫 표에서 (유형, 상태) 행을 읽는다.
    fn notification_doc_rows(doc: &str) -> std::collections::BTreeMap<String, &str> {
        let start = doc
            .find("#### Notification 유형별 상태")
            .expect("docs 표 제목");
        let mut rows = std::collections::BTreeMap::new();
        let mut in_table = false;
        for line in doc[start..].lines().skip(1) {
            if line.starts_with('|') {
                in_table = true;
                let cells: Vec<&str> = line.split('|').map(str::trim).collect();
                let (Some(ty), Some(state)) = (cells.get(1), cells.get(2)) else {
                    continue;
                };
                if let Some(name) = ty.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
                    let state = state.trim_matches('`');
                    assert!(
                        rows.insert(name.to_string(), state).is_none(),
                        "docs 표 중복 {name}"
                    );
                }
            } else if in_table {
                break;
            }
        }
        rows
    }

    #[test]
    fn the_docs_table_reader_sees_a_changed_row() {
        let doc = "#### Notification 유형별 상태\n\n| 유형 | 상태 |\n|---|---|\n| `a` | needs_input |\n| `b` | 변경 없음 |\n\n| `c` | active |\n";
        let rows = notification_doc_rows(doc);
        assert_eq!(rows.len(), 2, "표 뒤의 다른 표는 읽지 않는다");
        assert_eq!(rows["b"], "변경 없음");
    }

    #[test]
    fn notification_missing_type_still_sets_needs_input() {
        // notification_type 이 생기기 전 버전과 같이 필드가 없으면 입력 대기로 처리한다.
        let calls = apply_hook("notification", 100, None, None, None, &test_translator()).unwrap();
        assert!(calls.iter().any(|c| matches!(
            c,
            HostCall::SetState {
                state: "needs_input",
                ..
            }
        )));
    }

    #[test]
    fn pre_tool_use_sets_needs_input_and_fires_needs_input() {
        // AskUserQuestion 전에 상태·훅·화면 알림을 요청한다.
        let calls = apply_hook("pre-tool-use", 100, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::SetState {
                    surface_id: 100,
                    state: "needs_input",
                },
                HostCall::FireHook {
                    surface_id: 100,
                    event: "needs-input",
                },
                HostCall::SurfaceCompletion {
                    surface_id: 100,
                    kind: "needs_input",
                },
            ]
        );
    }

    #[test]
    fn post_tool_use_sets_active() {
        // 질문 응답 뒤 상태만 active로 바꾸고 새 화면 알림은 만들지 않는다.
        let calls = apply_hook("post-tool-use", 100, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            vec![HostCall::SetState {
                surface_id: 100,
                state: "active",
            }]
        );
    }

    #[test]
    fn session_end_clears_session_meta_and_fires_idle() {
        let calls = apply_hook("session-end", 100, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: STOP_FAILURE_META_KEY,
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: BACKGROUND_WAIT_META_KEY,
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: crate::monitor_tasks::MONITOR_TASKS_META_KEY,
                },
                HostCall::SetState {
                    surface_id: 100,
                    state: "idle",
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: "claude-session-id",
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: "restore.command",
                },
                HostCall::FireHook {
                    surface_id: 100,
                    event: "claude-idle",
                },
                HostCall::SurfaceCompletion {
                    surface_id: 100,
                    kind: "completion",
                },
            ]
        );
    }

    // 완료·입력 대기 이벤트의 화면 알림 호출을 각각 확인한다.

    #[test]
    fn stop_also_raises_surface_completion_highlight() {
        let calls = apply_hook("stop", 100, None, None, None, &test_translator()).unwrap();
        assert!(calls.iter().any(|c| matches!(
            c,
            HostCall::SurfaceCompletion {
                surface_id: 100,
                kind: "completion",
            }
        )));
    }

    #[test]
    fn subagent_stop_raises_no_surface_completion_highlight() {
        let calls = apply_hook("subagent-stop", 7, None, None, None, &test_translator()).unwrap();
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, HostCall::SurfaceCompletion { .. }))
        );
    }

    #[test]
    fn session_end_also_raises_surface_completion_highlight() {
        let calls = apply_hook("session-end", 100, None, None, None, &test_translator()).unwrap();
        assert!(calls.iter().any(|c| matches!(
            c,
            HostCall::SurfaceCompletion {
                surface_id: 100,
                kind: "completion",
            }
        )));
    }

    #[test]
    fn notification_also_raises_surface_completion_highlight() {
        let calls = apply_hook("notification", 100, None, None, None, &test_translator()).unwrap();
        assert!(calls.iter().any(|c| matches!(
            c,
            HostCall::SurfaceCompletion {
                surface_id: 100,
                kind: "needs_input",
            }
        )));
    }

    /// 질문 전에는 needs_input 화면 알림을 요청한다.
    #[test]
    fn pre_tool_use_also_raises_surface_completion_with_needs_input_kind() {
        let calls = apply_hook("pre-tool-use", 100, None, None, None, &test_translator()).unwrap();
        assert!(calls.iter().any(|c| matches!(
            c,
            HostCall::SurfaceCompletion {
                surface_id: 100,
                kind: "needs_input",
            }
        )));
    }

    #[test]
    fn prompt_submit_does_not_raise_surface_completion_highlight() {
        // 새 턴 시작 자체는 완료·입력 대기 화면 알림 대상이 아니다.
        let calls = apply_hook("prompt-submit", 100, None, None, None, &test_translator()).unwrap();
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, HostCall::SurfaceCompletion { .. }))
        );
    }

    #[test]
    fn prompt_submit_sets_active_and_clears_the_stop_failure_and_wait_records() {
        let calls = apply_hook("prompt-submit", 100, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: STOP_FAILURE_META_KEY,
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: BACKGROUND_WAIT_META_KEY,
                },
                HostCall::SetState {
                    surface_id: 100,
                    state: "active",
                },
            ]
        );
    }

    #[test]
    fn session_start_without_session_id_just_sets_active() {
        let calls = apply_hook("session-start", 100, None, None, None, &test_translator()).unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: STOP_FAILURE_META_KEY,
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: BACKGROUND_WAIT_META_KEY,
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: crate::monitor_tasks::MONITOR_TASKS_META_KEY,
                },
                HostCall::SetState {
                    surface_id: 100,
                    state: "active",
                },
            ]
        );
    }

    #[test]
    fn only_a_stop_failure_carrying_an_agent_id_is_a_subagent_failure() {
        assert!(is_subagent_stop_failure("stop-failure", Some("a1b2")));
        assert!(!is_subagent_stop_failure("stop-failure", None));
        assert!(!is_subagent_stop_failure("stop-failure", Some("")));
        // 다른 이벤트는 agent_id 가 있어도 이 판정과 무관하다.
        assert!(!is_subagent_stop_failure("stop", Some("a1b2")));
    }

    #[test]
    fn stop_failure_sets_idle_and_fires_idle_and_stop_failure() {
        let calls = apply_hook(
            "stop-failure",
            100,
            None,
            None,
            Some("overloaded"),
            &test_translator(),
        )
        .unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: BACKGROUND_WAIT_META_KEY,
                },
                HostCall::SetState {
                    surface_id: 100,
                    state: "idle",
                },
                // meta 가 fire 보다 먼저다 — 완료 알림 command 가 fire 뒤에 이것을 읽는다.
                HostCall::MetaSet {
                    surface_id: 100,
                    key: STOP_FAILURE_META_KEY,
                    value: "overloaded".into(),
                },
                HostCall::FireHook {
                    surface_id: 100,
                    event: "claude-idle",
                },
                HostCall::FireHook {
                    surface_id: 100,
                    event: STOP_FAILURE_EVENT,
                },
                HostCall::SurfaceCompletion {
                    surface_id: 100,
                    kind: "completion",
                },
            ]
        );
    }

    #[test]
    fn stop_failure_without_error_field_records_unknown() {
        for error in [None, Some("")] {
            let calls =
                apply_hook("stop-failure", 5, None, None, error, &test_translator()).unwrap();
            assert!(calls.contains(&HostCall::MetaSet {
                surface_id: 5,
                key: STOP_FAILURE_META_KEY,
                value: "unknown".into(),
            }));
        }
    }

    #[test]
    fn stop_failure_record_is_cleared_by_new_turn_and_session_end_only() {
        let clears = |event: &str| {
            apply_hook(event, 9, None, None, None, &test_translator())
                .unwrap()
                .contains(&HostCall::MetaUnset {
                    surface_id: 9,
                    key: STOP_FAILURE_META_KEY,
                })
        };
        for event in ["prompt-submit", "session-start", "active", "session-end"] {
            assert!(clears(event), "{event} 는 기록을 지워야 한다");
        }
        // 에러로 끝난 턴 뒤의 알림·질문이 기록을 지우면 부모 알림이 사유를 잃는다.
        for event in ["stop", "subagent-stop", "notification", "pre-tool-use"] {
            assert!(!clears(event), "{event} 는 기록을 건드리지 않는다");
        }
    }

    #[test]
    fn stop_failure_closes_wall_time_like_stop() {
        let mut state = ClaudeState::default();
        telemetry_for_hook(&mut state, "session-start", 3, None, 1_000);
        let calls = telemetry_for_hook(&mut state, "stop-failure", 3, None, 4_000);
        assert_eq!(
            calls,
            vec![HostCall::TelemetryRecord {
                metric: "wall_time_ms",
                value: 3_000.0,
                surface_id: 3,
            }]
        );
    }

    #[test]
    fn session_start_with_session_id_emits_meta_set() {
        let calls = apply_hook(
            "session-start",
            100,
            Some("sess-abc"),
            None,
            None,
            &test_translator(),
        )
        .unwrap();
        assert_eq!(
            calls,
            vec![
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: STOP_FAILURE_META_KEY,
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: BACKGROUND_WAIT_META_KEY,
                },
                HostCall::MetaUnset {
                    surface_id: 100,
                    key: crate::monitor_tasks::MONITOR_TASKS_META_KEY,
                },
                HostCall::SetState {
                    surface_id: 100,
                    state: "active",
                },
                HostCall::MetaSet {
                    surface_id: 100,
                    key: "claude-session-id",
                    value: "sess-abc".into(),
                },
                HostCall::MetaSet {
                    surface_id: 100,
                    key: "restore.command",
                    value: "claude -r sess-abc".into(),
                },
            ]
        );
    }

    #[test]
    fn session_options_follow_the_hook_lifecycle_without_leaking_to_an_unrelated_process() {
        struct ModelHost;
        impl HostCallSink for ModelHost {
            fn call(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                assert_eq!(method, "surface.meta.get");
                Ok(match params["key"].as_str().unwrap() {
                    crate::session_options::META_KEY => {
                        json!({"value": "{\"model\":\"opus\",\"effort\":\"high\"}"})
                    }
                    crate::stop_pairing::SETTINGS_SESSION_META_KEY => {
                        json!({"value": "sess-model"})
                    }
                    _ => json!({"value": null}),
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let tr = test_translator();
        let mut calls =
            apply_hook("session-start", 42, Some("sess-model"), None, None, &tr).unwrap();
        session_lifecycle_calls(
            &mut calls,
            &ModelHost,
            "session-start",
            42,
            Some("sess-model"),
            &json!({"source": "startup"}),
            Some(dir.path()),
            &tr,
        );
        assert_eq!(
            restore_command(&calls).unwrap(),
            "claude -r sess-model --model opus --effort high"
        );
        for (ended_session, reason, should_clear) in [
            ("sess-model", "prompt_input_exit", true),
            ("old-session", "prompt_input_exit", false),
            ("sess-model", "clear", false),
            ("sess-model", "resume", false),
        ] {
            let mut calls = Vec::new();
            session_lifecycle_calls(
                &mut calls,
                &ModelHost,
                "session-end",
                42,
                Some(ended_session),
                &json!({"reason": reason}),
                Some(dir.path()),
                &tr,
            );
            assert_eq!(
                calls
                    .iter()
                    .any(|call| matches!(call, HostCall::MetaUnset { key, .. }
                if *key == crate::session_options::META_KEY)),
                should_clear
            );
        }
        // The session file survives live metadata cleanup for a later closed-tab restore.
        assert!(dir.path().join("session-options/sess-model.json").is_file());
    }

    fn register_profile(dir: &std::path::Path, name: &str, body: &str) {
        let src = dir.join(format!("{name}-src.json"));
        std::fs::write(&src, body).unwrap();
        crate::profile::register(Some(dir), name, &src).unwrap();
    }

    fn register_gate(dir: &std::path::Path, name: &str) {
        let body = dir.join(format!("{name}-body.md"));
        std::fs::write(&body, format!("{name} 본문\n[[{name}-DONE]]\n")).unwrap();
        crate::gate::register(
            Some(dir),
            name,
            &body,
            Some(&format!("[[{name}-DONE]]")),
            Some(2),
        )
        .unwrap();
    }

    fn no_meta() -> crate::reboot::AttachedProfile {
        crate::reboot::AttachedProfile {
            names: None,
            path: None,
        }
    }

    fn names_meta(names: &str) -> crate::reboot::AttachedProfile {
        crate::reboot::AttachedProfile {
            names: Some(names.to_string()),
            path: None,
        }
    }

    /// session-start 호출 목록에서 `restore.command` 값을 뽑는다.
    fn restore_command(calls: &[HostCall]) -> Option<String> {
        calls.iter().find_map(|c| match c {
            HostCall::MetaSet { key, value, .. } if *key == RESTORE_COMMAND_META_KEY => {
                Some(value.clone())
            }
            _ => None,
        })
    }

    /// apply_hook의 호출 목록에 프로필 복원 계획을 반영한다.
    fn session_start_calls(
        surface_id: u32,
        session_id: &str,
        meta: &crate::reboot::AttachedProfile,
        data_dir: &std::path::Path,
    ) -> (Vec<HostCall>, SessionStartProfile) {
        let tr = test_translator();
        let mut calls = apply_hook(
            "session-start",
            surface_id,
            Some(session_id),
            None,
            None,
            &tr,
        )
        .unwrap();
        let plan = plan_session_start_profile(session_id, meta, Some(data_dir), &tr);
        apply_session_start_profile(&mut calls, surface_id, session_id, &plan);
        (calls, plan)
    }

    /// 메타데이터가 없으면 저장 기록으로 프로필을 복원하고 명령에 settings를 넣는다.
    #[test]
    fn session_start_restores_the_profile_from_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        register_profile(tmp.path(), "reviewer", r#"{"env":{"A":"1"}}"#);
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("reviewer".into()),
        );

        let (calls, plan) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());

        let generated = plan.profile_file.expect("프로필이 해석돼야 한다");
        // OS별 경로 구분자 차이는 Path로 비교한다.
        let expected = "profiles/generated/reviewer.json";
        assert!(
            std::path::Path::new(&generated).ends_with(expected),
            "생성 프로필 경로가 `{expected}` 로 끝나야 하는데 실제로는 `{generated}` 다"
        );
        assert_eq!(
            restore_command(&calls).unwrap(),
            // The temporary path may hold any character; the quoting rule itself is pinned by
            // the literal session_options tests, so this only checks the wiring.
            format!(
                "claude -r sess-1 --settings {}",
                crate::session_options::quote_path(&generated)
            )
        );
        // meta 도 함께 되살아난다 — 이후 `profile-current` / 무인자 reboot 가 승계한다.
        assert!(calls.contains(&HostCall::MetaSet {
            surface_id: 7,
            key: crate::reboot::PROFILE_NAMES_META_KEY,
            value: "reviewer".into(),
        }));
    }

    /// 저장 기록과 달라도 현재 메타데이터를 우선한다.
    #[test]
    fn session_start_prefers_meta_over_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        register_profile(tmp.path(), "reviewer", r#"{"env":{"A":"1"}}"#);
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("ghost".into()),
        );

        let (calls, plan) = session_start_calls(7, "sess-1", &names_meta("reviewer"), tmp.path());

        assert!(plan.profile_file.unwrap().ends_with("reviewer.json"));
        assert!(restore_command(&calls).unwrap().contains("--settings"));
        // meta 가 이미 있으므로 meta 복구 호출은 없다.
        assert!(plan.meta_calls.is_empty());
        // 현재 메타데이터로 저장 기록도 갱신한다.
        assert_eq!(plan.restamp, Some(AttachRecord::Names("reviewer".into())));
    }

    /// 등록이 제거된 프로필은 복원에서 생략하고 세션 명령은 유지한다.
    #[test]
    fn session_start_degrades_when_the_recorded_name_no_longer_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("ghost".into()),
        );

        let (calls, plan) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());

        assert_eq!(plan, SessionStartProfile::default());
        assert_eq!(restore_command(&calls).unwrap(), "claude -r sess-1");
    }

    /// 경로로 지정한 파일이 사라져도 프로필 없이 복원한다.
    #[test]
    fn session_start_degrades_when_the_recorded_path_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Path(tmp.path().join("gone.json").display().to_string()),
        );

        let (calls, _) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());

        assert_eq!(restore_command(&calls).unwrap(), "claude -r sess-1");
    }

    /// 종료 표시 직후에는 프로필 기록을 남겨 닫은 탭 복원에 사용할 수 있어야 한다.
    #[test]
    fn session_end_marks_the_record_ended_but_keeps_it_for_a_closed_tab_restore() {
        let tmp = tempfile::tempdir().unwrap();
        register_profile(tmp.path(), "probe", r#"{"env":{"A":"1"}}"#);
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("probe".into()),
        );
        profile_attach::mark_ended(Some(tmp.path()), "sess-1");

        // 되살아난 세션의 session-start 가 기록으로 프로필을 복구할 수 있어야 한다.
        let (calls, plan) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());
        assert!(plan.profile_file.is_some());
        assert!(restore_command(&calls).unwrap().contains("--settings"));
    }

    /// 재등록한 프로필은 다음 session-start의 생성 파일에 반영돼야 한다.
    #[test]
    fn session_start_reresolves_names_against_the_current_registry() {
        let tmp = tempfile::tempdir().unwrap();
        register_profile(tmp.path(), "reviewer", r#"{"env":{"VERSION":"1"}}"#);
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("reviewer".into()),
        );

        let (_, plan) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());
        let generated = std::path::PathBuf::from(plan.profile_file.unwrap());
        let v1: Value =
            serde_json::from_str(&std::fs::read_to_string(&generated).unwrap()).unwrap();
        assert_eq!(v1["env"]["VERSION"], "1");

        register_profile(tmp.path(), "reviewer", r#"{"env":{"VERSION":"2"}}"#);
        let (_, plan) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());
        let generated = std::path::PathBuf::from(plan.profile_file.unwrap());
        let v2: Value =
            serde_json::from_str(&std::fs::read_to_string(&generated).unwrap()).unwrap();
        assert_eq!(v2["env"]["VERSION"], "2");
    }

    /// reboot 중 종료 표시가 생겨도 새 session-start에서 프로필 기록을 갱신해야 한다.
    #[test]
    fn reboot_then_session_end_then_session_start_keeps_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        register_profile(tmp.path(), "reviewer", r#"{"env":{"A":"1"}}"#);

        // reboot가 프로필 메타데이터와 기록을 남긴다.
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("reviewer".into()),
        );

        // session-end는 종료 표시를 남기고 프로필 메타데이터는 유지한다.
        let end_calls = apply_hook(
            "session-end",
            7,
            Some("sess-1"),
            None,
            None,
            &test_translator(),
        )
        .unwrap();
        assert!(!end_calls.iter().any(|c| matches!(
            c,
            HostCall::MetaUnset { key, .. }
                if *key == crate::reboot::PROFILE_NAMES_META_KEY
                    || *key == crate::reboot::PROFILE_META_KEY
        )));
        profile_attach::mark_ended(Some(tmp.path()), "sess-1");

        // 다음 session-start는 남은 메타데이터로 프로필을 다시 저장한다.
        let (calls, plan) = session_start_calls(7, "sess-1", &names_meta("reviewer"), tmp.path());
        if let Some(record) = &plan.restamp {
            profile_attach::store(Some(tmp.path()), "sess-1", record);
        }

        assert_eq!(
            profile_attach::load(Some(tmp.path()), "sess-1"),
            Some(AttachRecord::Names("reviewer".into())),
            "session-start 뒤 프로필 기록이 다시 저장돼야 한다"
        );
        assert!(restore_command(&calls).unwrap().contains("--settings"));
    }

    /// 같은 이름을 프로필에서 게이트로 다시 등록해도 최신 정의로 복원해야 한다.
    #[test]
    fn session_start_reinterprets_a_name_that_changed_from_profile_to_gate() {
        let tmp = tempfile::tempdir().unwrap();
        register_profile(tmp.path(), "shared", r#"{"env":{"A":"1"}}"#);
        profile_attach::store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("shared".into()),
        );

        let (_, plan) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());
        let as_profile: Value =
            serde_json::from_str(&std::fs::read_to_string(plan.profile_file.unwrap()).unwrap())
                .unwrap();
        assert_eq!(as_profile["env"]["A"], "1");
        assert!(as_profile.get("hooks").is_none());

        crate::profile::unregister(Some(tmp.path()), "shared").unwrap();
        register_gate(tmp.path(), "shared");

        let (calls, plan) = session_start_calls(7, "sess-1", &no_meta(), tmp.path());
        let as_gate: Value =
            serde_json::from_str(&std::fs::read_to_string(plan.profile_file.unwrap()).unwrap())
                .unwrap();
        assert!(
            as_gate["hooks"]["Stop"].is_array(),
            "게이트로 재등록됐으면 Stop 훅 조각으로 해석돼야 한다: {as_gate}"
        );
        // 게이트 이름으로 부착한 것도 같은 경로로 복원된다.
        assert!(restore_command(&calls).unwrap().contains("--settings"));
    }

    #[test]
    fn unknown_event_returns_invalid_params() {
        let err = apply_hook("bogus", 100, None, None, None, &test_translator()).unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("bogus"));
    }

    #[test]
    fn resolve_surface_id_prefers_explicit_param() {
        let tr = test_translator();
        assert_eq!(
            resolve_surface_id(&json!({ "surface": 42 }), &tr).unwrap(),
            42
        );
        assert_eq!(
            resolve_surface_id(&json!({ "surface_id": 7 }), &tr).unwrap(),
            7
        );
    }

    #[test]
    fn telemetry_session_start_marks_then_stop_emits_wall_time() {
        let mut state = ClaudeState::default();
        let started = telemetry_for_hook(&mut state, "session-start", 7, None, 1_000);
        assert!(started.is_empty(), "session-start emits no host calls");
        let stopped = telemetry_for_hook(&mut state, "stop", 7, None, 5_000);
        assert_eq!(
            stopped,
            vec![HostCall::TelemetryRecord {
                metric: "wall_time_ms",
                value: 4_000.0,
                surface_id: 7,
            }]
        );
        // 두번째 stop 은 start 가 없으므로 발행 안 함.
        let again = telemetry_for_hook(&mut state, "stop", 7, None, 9_000);
        assert!(again.is_empty());
    }

    /// 서브에이전트 종료가 메인 턴의 경과 시간을 닫으면 뒤의 진짜 stop 이 기록을 잃는다.
    #[test]
    fn telemetry_subagent_stop_keeps_the_wall_time_open_for_the_main_stop() {
        let mut state = ClaudeState::default();
        telemetry_for_hook(&mut state, "session-start", 7, None, 1_000);
        let sub = telemetry_for_hook(&mut state, "subagent-stop", 7, None, 3_000);
        assert!(
            sub.is_empty(),
            "subagent-stop 은 wall_time 을 기록하지 않는다"
        );
        let stopped = telemetry_for_hook(&mut state, "stop", 7, None, 5_000);
        assert_eq!(
            stopped,
            vec![HostCall::TelemetryRecord {
                metric: "wall_time_ms",
                value: 4_000.0,
                surface_id: 7,
            }]
        );
    }

    #[test]
    fn telemetry_session_end_also_emits_wall_time() {
        let mut state = ClaudeState::default();
        telemetry_for_hook(&mut state, "session-start", 1, None, 100);
        let calls = telemetry_for_hook(&mut state, "session-end", 1, None, 250);
        assert_eq!(
            calls,
            vec![HostCall::TelemetryRecord {
                metric: "wall_time_ms",
                value: 150.0,
                surface_id: 1,
            }]
        );
    }

    #[test]
    fn telemetry_notification_extracts_tokens() {
        let mut state = ClaudeState::default();
        let calls = telemetry_for_hook(
            &mut state,
            "notification",
            42,
            Some("Claude used tokens: 12345 in this turn"),
            0,
        );
        assert_eq!(
            calls,
            vec![HostCall::TelemetryRecord {
                metric: "input_tokens",
                value: 12345.0,
                surface_id: 42,
            }]
        );
    }

    #[test]
    fn telemetry_notification_no_match_no_record() {
        let mut state = ClaudeState::default();
        let calls = telemetry_for_hook(&mut state, "notification", 1, Some("approval needed"), 0);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_tokens_variants() {
        assert_eq!(extract_tokens("tokens: 99"), Some(99));
        assert_eq!(extract_tokens("token:5"), Some(5));
        assert_eq!(extract_tokens("Tokens:   1000  used"), Some(1000));
        assert_eq!(extract_tokens("notokens: 5"), None);
        assert_eq!(extract_tokens("tokens 5"), None); // 콜론 없음
        assert_eq!(extract_tokens("xtoken: 5"), None); // 워드 경계 위반
    }

    /// 환경 값은 인자로 전달해 실제 실행 환경과 관계없이 검사한다.
    #[test]
    fn resolve_surface_id_missing_and_no_env_is_invalid_params() {
        let err = resolve_surface_id_from(&json!({}), None, &test_translator()).unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn resolve_surface_id_falls_back_to_env_when_the_param_is_absent() {
        let sid = resolve_surface_id_from(&json!({}), Some("42"), &test_translator()).unwrap();
        assert_eq!(sid, 42);
    }

    /// u32 범위를 넘는 값은 다른 id로 자르거나 환경 값으로 대신하지 않고 거절한다.
    #[test]
    fn a_surface_beyond_u32_is_rejected_not_truncated() {
        let big = serde_json::json!({ "surface": 5_000_000_000u64 });
        let err = resolve_surface_id_from(&big, Some("7"), &test_translator())
            .expect_err("u32 를 넘는 값은 오류여야 한다");
        assert_eq!(err.code, -32602);
        // 범위 안의 값은 그대로 받아야 한다.
        let ok = resolve_surface_id_from(
            &serde_json::json!({ "surface": 4_294_967_295u64 }),
            None,
            &test_translator(),
        )
        .expect("u32::MAX 는 유효한 값이다");
        assert_eq!(ok, u32::MAX);
    }

    /// 형식이 잘못된 값은 환경 값으로 대신하지 않고 거절한다.
    #[test]
    fn a_non_numeric_surface_is_rejected_and_does_not_fall_back_to_env() {
        for bad in [
            json!({ "surface": "conductor" }),
            json!({ "surface_id": "7x" }),
        ] {
            let err = resolve_surface_id_from(&bad, Some("42"), &test_translator())
                .expect_err("숫자가 아닌 surface 는 거부해야 한다");
            assert_eq!(err.code, -32602, "params={bad}");
        }
    }

    /// 내장 언어 모두 잘못된 surface 인자 이름을 오류에 포함해야 한다.
    #[test]
    fn a_malformed_surface_in_a_hook_names_which_of_the_two_keys_was_wrong() {
        for locale in tasty_i18n::BUILTIN_CODES {
            let tr = test_translator_for(locale);
            // env 는 채워둔다 — 폴백으로 넘어가면 문구 자체가 안 나온다.
            let by_surface = resolve_surface_id_from(&json!({ "surface": "x" }), Some("42"), &tr)
                .expect_err("문자열은 surface id 가 아니다")
                .message;
            let by_surface_id =
                resolve_surface_id_from(&json!({ "surface_id": "x" }), Some("42"), &tr)
                    .expect_err("문자열은 surface id 가 아니다")
                    .message;
            assert!(
                by_surface_id.contains("surface_id"),
                "{locale}: 'surface_id' 가 틀렸는데 그 이름을 안 댄다 — {by_surface_id}"
            );
            assert!(
                !by_surface.contains("surface_id"),
                "{locale}: 'surface' 가 틀렸는데 'surface_id' 를 댄다 — {by_surface}"
            );
            assert_ne!(
                by_surface, by_surface_id,
                "{locale}: 두 키가 같은 문구를 받는다 — 어느 쪽이 틀렸는지 못 가른다"
            );
        }
    }

    /// 두 대상 키가 서로 다르면 훅도 거절한다.
    #[test]
    fn two_names_disagreeing_is_refused_in_a_hook_too() {
        let err = resolve_surface_id_from(
            &json!({ "surface": 1, "surface_id": 2 }),
            Some("42"),
            &test_translator(),
        )
        .expect_err("두 이름이 다른 대상을 가리키면 고를 수 없다");
        assert_eq!(err.code, -32602);
        assert!(
            err.message.contains('1') && err.message.contains('2'),
            "충돌한 두 대상 값을 오류에 포함해야 한다: {}",
            err.message
        );
    }

    // 새 턴의 오류 중복 알림 초기화.

    #[test]
    fn is_new_turn_event_matches_active_transition_only() {
        for event in ["prompt-submit", "session-start", "active"] {
            assert!(is_new_turn_event(event), "{event} 은 새 턴 신호여야 함");
        }
        for event in [
            "stop",
            "subagent-stop",
            "notification",
            "session-end",
            "pre-tool-use",
            "post-tool-use",
        ] {
            assert!(!is_new_turn_event(event), "{event} 은 새 턴 신호가 아님");
        }
    }

    #[test]
    fn reset_dedupe_if_enabled_clears_tracked_surface() {
        let scanner = Arc::new(Mutex::new(ErrorScanner::new()));
        scanner
            .lock()
            .unwrap()
            .enable(7, crate::error_scan::ScanTarget::TopLevel);
        scanner
            .lock()
            .unwrap()
            .seed_dedupe_for_test(7, "API Error: boom");

        reset_dedupe_if_enabled(&scanner, 7);

        assert!(
            !scanner.lock().unwrap().has_dedupe_state(7),
            "추적 중인 surface 는 dedupe 가 초기화돼야 함"
        );
    }

    #[test]
    fn reset_dedupe_if_enabled_skips_untracked_surface() {
        // 감시 대상이 아니면 남아 있는 중복 기록도 건드리지 않아야 한다.
        let scanner = Arc::new(Mutex::new(ErrorScanner::new()));
        scanner
            .lock()
            .unwrap()
            .seed_dedupe_for_test(99, "unrelated");

        reset_dedupe_if_enabled(&scanner, 99);

        assert!(!scanner.lock().unwrap().is_enabled(99));
        assert!(
            scanner.lock().unwrap().has_dedupe_state(99),
            "추적 대상이 아니면 dedupe 상태를 건드리지 않아야 함"
        );
    }
    #[derive(Default)]
    struct R1ModelHost {
        meta: std::cell::RefCell<std::collections::HashMap<String, String>>,
    }
    impl HostCallSink for R1ModelHost {
        fn call(&self, method: &str, p: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
            let k = p["key"].as_str().unwrap_or("").to_owned();
            match method {
                "surface.meta.get" => Ok(json!({"value": self.meta.borrow().get(&k)})),
                "surface.meta.set" => {
                    self.meta
                        .borrow_mut()
                        .insert(k, p["value"].as_str().unwrap().into());
                    Ok(json!({}))
                }
                "surface.meta.unset" => {
                    self.meta.borrow_mut().remove(&k);
                    Ok(json!({}))
                }
                _ => Ok(json!({})),
            }
        }
    }
    fn r1_event(h: &R1ModelHost, dir: &Path, event: &str, sess: &str, detail: &str) {
        let tr = test_translator();
        let mut calls = apply_hook(event, 42, Some(sess), None, None, &tr).unwrap();
        let p = if event == "session-end" {
            json!({"reason": detail})
        } else {
            json!({"source": detail})
        };
        session_lifecycle_calls(&mut calls, h, event, 42, Some(sess), &p, Some(dir), &tr);
        assert_eq!(deliver_all(h, &calls), 0);
    }
    fn r1_options(model: &str) -> crate::session_options::SessionOptions {
        crate::session_options::SessionOptions::parse(
            &json!({"model": model, "effort": "high"}),
            &test_translator(),
        )
        .unwrap()
    }
    #[test]
    fn r1_resume_must_not_overwrite_other_session_selection() {
        let dir = tempfile::tempdir().unwrap();
        let h = R1ModelHost::default();
        let a = r1_options("opus");
        let b = r1_options("sonnet");
        crate::session_options::store(Some(dir.path()), "session-b", &b);
        a.record(&h, 42, "session-a").unwrap();
        r1_event(&h, dir.path(), "session-start", "session-a", "startup");
        r1_event(&h, dir.path(), "session-end", "session-a", "resume");
        r1_event(&h, dir.path(), "session-start", "session-b", "resume");
        let saved =
            std::fs::read_to_string(dir.path().join("session-options/session-b.json")).unwrap();
        assert_eq!(
            saved,
            b.json().to_string(),
            "restore={:?}",
            h.meta.borrow().get(RESTORE_COMMAND_META_KEY)
        );
    }
    #[test]
    fn r1_delayed_start_after_fresh_respawn_must_not_inherit_new_options() {
        let dir = tempfile::tempdir().unwrap();
        let h = R1ModelHost::default();
        let a = r1_options("opus");
        a.record(&h, 42, "session-a").unwrap();
        // A has durable options from a prior run; its resume SessionStart is still in flight.
        crate::session_options::store(Some(dir.path()), "session-a", &a);
        // start_claude_with_options for an omitted-option respawn records {} and resets owner.
        crate::session_options::SessionOptions::default()
            .record(&h, 42, "session-new")
            .unwrap();
        crate::stop_pairing::record_settings_file(&h, 42, None);
        // Background resume SessionStart of the terminated process arrives after the fresh launch.
        r1_event(&h, dir.path(), "session-start", "session-a", "resume");
        let saved =
            std::fs::read_to_string(dir.path().join("session-options/session-a.json")).unwrap();
        assert_eq!(saved, a.json().to_string());
        let pending: Value =
            serde_json::from_str(&h.meta.borrow()[crate::session_options::META_KEY]).unwrap();
        assert_eq!(pending["session"], "session-new");
        assert_eq!(pending["pending"], true);
        assert!(!h.meta.borrow().contains_key(RESTORE_COMMAND_META_KEY));
        r1_event(&h, dir.path(), "session-start", "session-new", "startup");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("session-options/session-new.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("session-options/session-a.json")).unwrap(),
            a.json().to_string()
        );
    }
    #[test]
    fn r1_restore_without_meta_and_corrupt_record() {
        let dir = tempfile::tempdir().unwrap();
        let h = R1ModelHost::default();
        let a = r1_options("opus");
        crate::session_options::store(Some(dir.path()), "session-a", &a);
        r1_event(&h, dir.path(), "session-start", "session-a", "resume");
        assert_eq!(
            h.meta.borrow()[RESTORE_COMMAND_META_KEY],
            "claude -r session-a --model opus --effort high"
        );
        let empty = R1ModelHost::default();
        std::fs::write(
            dir.path().join("session-options/session-a.json"),
            "bad JSON",
        )
        .unwrap();
        r1_event(&empty, dir.path(), "session-start", "session-a", "resume");
        assert!(!empty.meta.borrow().contains_key(RESTORE_COMMAND_META_KEY));
        assert!(
            crate::session_options::load(
                &empty,
                42,
                Some(dir.path()),
                "session-a",
                &test_translator()
            )
            .is_err()
        );
    }

    #[test]
    fn r1_unrelated_startup_after_missing_end_must_not_inherit_old_selection() {
        let dir = tempfile::tempdir().unwrap();
        let h = R1ModelHost::default();
        r1_options("opus").record(&h, 42, "session-a").unwrap();
        r1_event(&h, dir.path(), "session-start", "session-a", "startup");
        // SIGKILL/crash cannot send SessionEnd. A manual `claude` starts B with default argv.
        r1_event(&h, dir.path(), "session-start", "session-b", "startup");
        let saved =
            std::fs::read_to_string(dir.path().join("session-options/session-b.json")).unwrap();
        assert_eq!(
            saved,
            "{}",
            "restore={:?}",
            h.meta.borrow().get(RESTORE_COMMAND_META_KEY)
        );
    }
}
