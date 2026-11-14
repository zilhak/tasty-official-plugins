//! Codex 부모 대화로 자식 결과를 돌려보내는 `codex.parent`·`codex.deliver`·`codex.result`.
//!
//! - parent: 호출자 thread를 받을 수 있는지 확인하고 수신자를 만든다. Tasty 세션이면 그
//!   surface가, 아니면 공유 daemon에 로드된 thread가 수신 대상이다. 둘 다 아니면 실패한다.
//! - deliver: 원장에 기록(동기)하고 송신은 백그라운드에 맡긴다.
//! - 송신 펌프: thread마다 한 건씩 기록 순서대로, 전체 동시 송신은 [`MAX_INFLIGHT`]건까지
//!   보낸다. 보내지 못한 이벤트는 지수 backoff로 다시 보내고, 응답을 잃은 unknown은 대기열을
//!   한 번 확인할 뿐 다시 보내지 않는다.
//! - result: 부모가 본문을 도구 결과로 읽는다. 읽어도 본문은 남는다.
//!
//! 큐에 넣는 메시지는 고정 템플릿과 이벤트 ID뿐이다. 자식 본문은 userMessage가 되지 않는다.

pub(crate) mod attach_out;
pub(crate) mod ledger;
pub(crate) mod send;
pub(crate) mod turn_scheduler;
pub(crate) mod turns;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::caller::{CallerContext, codex_caller};
use crate::rpc::Endpoint;
use crate::session::registry;
use crate::store;
use ledger::{Completion, DeliveryState, Event, Receiver, ReceiverKind};
use send::{Dispatch, Job, JobKind, Outcome};

/// 외부 부모 thread가 공유 daemon에 로드돼 있는지 확인하는 함수.
pub(crate) type LoadedProbe<'a> = &'a dyn Fn(&CallerContext) -> Result<bool, String>;
/// 세대의 소유 서버에서 thread가 지금 턴을 실행 중(`active`)인지 묻는다.
pub(crate) type LiveProbe<'a> = &'a dyn Fn(&Endpoint, &str) -> Result<bool, String>;

pub(crate) struct Ctx<'a> {
    pub root: &'a Path,
    pub dispatch: &'a dyn Dispatch,
    pub loaded: LoadedProbe<'a>,
    pub live: LiveProbe<'a>,
    pub tr: &'a Translator,
    /// 이 프로세스의 송신 결과 확인 값.
    pub token: &'a str,
    pub pump: &'a Pump,
}

/// 전체 동시 송신 상한. 외부 부모는 송신마다 proxy 프로세스를 띄우므로 작게 둔다.
pub(crate) const MAX_INFLIGHT: usize = 4;
/// 자동 재송신 횟수 상한. 넘으면 보류하고 다음 계기(같은 thread의 완료, 부모 TUI 연결,
/// 플러그인 재시작)를 기다린다.
pub(crate) const MAX_AUTO_ATTEMPTS: u32 = 12;
/// 재송신 간격 상한.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// n번째 실패 뒤 다시 보낼 때까지의 간격(1초부터 두 배씩, 상한 [`MAX_BACKOFF`]).
pub(crate) fn backoff(attempts: u32) -> Duration {
    let secs = 1u64 << attempts.saturating_sub(1).min(6);
    Duration::from_secs(secs).min(MAX_BACKOFF)
}

/// 이 프로세스에서 진행 중인 송신과 예약한 재시도. 원장에 쓰지 않는 실행 상태다.
#[derive(Default)]
pub(crate) struct Pump {
    state: Mutex<PumpState>,
    poison_reported: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct PumpState {
    /// 송신·확인 중인 thread.
    inflight: HashSet<String>,
    /// 재시도를 예약한 thread와 그 시각(ms).
    timers: HashMap<String, u64>,
    /// 실행 중인 턴 확인 작업. 후보 정체([`turns::Candidate::id`])로 센다. 확인 스레드가 결과를
    /// 돌려줄 때까지 슬롯을 차지하며 후보 무효화로는 비지 않는다.
    turn_inflight: HashSet<String>,
    /// 후보 이름(완료 키)별 예약 정보: 확인 실패 수와 다음 확인 시각(ms).
    turn_checks: HashMap<String, u32>,
    turn_due: HashMap<String, u64>,
}

impl Pump {
    fn lock(&self) -> std::sync::MutexGuard<'_, PumpState> {
        tasty_utils::poison::recover_mutex(
            self.state.lock(),
            "codex delivery pump",
            &self.poison_reported,
        )
    }

    fn try_begin(&self, thread: &str) -> bool {
        let mut s = self.lock();
        if s.inflight.len() >= MAX_INFLIGHT || s.inflight.contains(thread) {
            return false;
        }
        s.inflight.insert(thread.to_string());
        true
    }

    fn finish(&self, thread: &str) {
        self.lock().inflight.remove(thread);
    }

    fn is_busy(&self, thread: &str) -> bool {
        self.lock().inflight.contains(thread)
    }

    /// 후보 `id`의 확인을 시작한다. 전체 상한에 닿았거나 그 후보를 이미 확인 중이면 거짓이다.
    fn turn_begin(&self, id: &str) -> bool {
        let mut s = self.lock();
        if s.turn_inflight.len() >= turns::MAX_TURN_INFLIGHT || s.turn_inflight.contains(id) {
            return false;
        }
        s.turn_inflight.insert(id.to_string());
        true
    }

    /// 후보 `id`의 확인 작업이 끝났다. 그 작업의 슬롯만 비운다.
    fn turn_finish(&self, id: &str) {
        self.lock().turn_inflight.remove(id);
    }

    /// 확인 실패를 하나 더하고 지금까지의 수를 돌려준다.
    fn turn_failed(&self, name: &str) -> u32 {
        let mut s = self.lock();
        let n = s.turn_checks.entry(name.to_string()).or_default();
        *n += 1;
        *n
    }

    fn turn_schedule(&self, name: &str, at_ms: u64) {
        self.lock().turn_due.insert(name.to_string(), at_ms);
    }

    /// 보류한 후보의 다음 확인 시각. 확인 중이거나 보류했으면 없다.
    fn turn_next(&self, name: &str, id: &str) -> Option<u64> {
        let s = self.lock();
        if s.turn_inflight.contains(id)
            || s.turn_checks
                .get(name)
                .is_some_and(|n| *n >= turns::MAX_TURN_CHECKS)
        {
            return None;
        }
        s.turn_due.get(name).copied()
    }

    /// 확인할 수 없는 후보를 보류한다.
    fn turn_hold(&self, name: &str) {
        let mut s = self.lock();
        s.turn_checks
            .insert(name.to_string(), turns::MAX_TURN_CHECKS);
        s.turn_due.remove(name);
    }

    #[cfg(test)]
    pub(crate) fn turn_exhausted(&self, name: &str) -> bool {
        self.lock()
            .turn_checks
            .get(name)
            .is_some_and(|n| *n >= turns::MAX_TURN_CHECKS)
    }

    /// 지금 확인을 시작해도 되는지. 보류했거나 예약 시각 전이면 거짓이다.
    fn turn_due(&self, name: &str, id: &str, now_ms: u64) -> bool {
        let s = self.lock();
        !s.turn_inflight.contains(id)
            && s.turn_checks
                .get(name)
                .is_none_or(|n| *n < turns::MAX_TURN_CHECKS)
            && s.turn_due.get(name).is_none_or(|at| *at <= now_ms)
    }

    /// 새 훅이 왔다. 보류와 예약을 풀어 바로 확인하게 한다.
    fn turn_reset(&self, name: &str) {
        let mut s = self.lock();
        s.turn_checks.remove(name);
        s.turn_due.remove(name);
    }

    /// 후보 이름의 예약 정보만 지운다. 실행 중인 확인의 슬롯은 그 확인이 돌아올 때 비운다.
    fn turn_forget(&self, name: &str) {
        let mut s = self.lock();
        s.turn_checks.remove(name);
        s.turn_due.remove(name);
    }

    #[cfg(test)]
    pub(crate) fn inflight(&self) -> usize {
        self.lock().inflight.len()
    }
}

fn io_err(e: std::io::Error) -> IpcMethodError {
    IpcMethodError::new(format!("codex delivery store: {e}"))
}

/// 공식 proxy로 공유 daemon의 로드 목록을 확인한다.
pub(crate) fn proxy_loaded(caller: &CallerContext) -> Result<bool, String> {
    let endpoint = Endpoint::Proxy {
        codex_home: caller.codex_home.clone(),
        execution: caller.execution.clone(),
    };
    let mut client =
        crate::rpc::Client::connect(&endpoint, send::RPC_TIMEOUT).map_err(|e| e.to_string())?;
    let loaded = crate::rpc::thread_loaded(&mut client, &caller.thread).map_err(|e| e.to_string());
    client.close();
    loaded
}

/// 훅이 기다리는 동안 답해야 하므로 짧게 둔다. 실측 응답은 20ms 안팎이었다.
const LIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// 소유 서버의 `thread/read` 상태가 `active`인지. 훅은 동기로 실행되므로 훅이 실행되는 동안 그
/// 훅의 thread는 `active`다.
pub(crate) fn owned_thread_active(endpoint: &Endpoint, thread: &str) -> Result<bool, String> {
    let mut client =
        crate::rpc::Client::connect(endpoint, LIVE_TIMEOUT).map_err(|e| e.to_string())?;
    let read = client.call("thread/read", json!({ "threadId": thread }));
    client.close();
    let read = read.map_err(|e| e.to_string())?;
    Ok(read
        .get("thread")
        .and_then(|t| t.get("status"))
        .and_then(|st| st.get("type"))
        .and_then(Value::as_str)
        == Some("active"))
}

fn require_caller(params: &Value, tr: &Translator) -> Result<CallerContext, IpcMethodError> {
    match codex_caller(params) {
        Ok(Some(c)) => Ok(c),
        Ok(None) => Err(IpcMethodError::invalid_params(
            tr.t("codex.parent.requires_codex_caller"),
        )),
        Err(e) => Err(IpcMethodError::invalid_params(&e)),
    }
}

/// 호출자 thread의 수신자를 만들고 호스트 부모 키([`caller_parent_key`])를 돌려준다.
pub(crate) fn prepare_receiver(
    ctx: &Ctx<'_>,
    caller: &CallerContext,
) -> Result<(Receiver, Value), IpcMethodError> {
    if registry::thread_session(ctx.root, &caller.thread)
        .map_err(io_err)?
        .is_some()
    {
        let receiver = ledger::receiver_for(ctx.root, &caller.thread, ReceiverKind::Owned, None)
            .map_err(io_err)?;
        return Ok((receiver, caller_parent_key(caller)));
    }
    match (ctx.loaded)(caller) {
        Ok(true) => {
            let mut receiver = ledger::receiver_for(
                ctx.root,
                &caller.thread,
                ReceiverKind::External,
                caller.codex_home.clone(),
            )
            .map_err(io_err)?;
            // 실행 환경이 바뀌어도 같은 수신자이며 완료 claim은 유지한다.
            if caller.execution.is_some() && receiver.execution != caller.execution {
                receiver.execution = caller.execution.clone();
                store::write_json(
                    &ledger::receiver_file(ctx.root, &receiver.receiver_id),
                    &receiver,
                )
                .map_err(io_err)?;
            }
            Ok((receiver, caller_parent_key(caller)))
        }
        Ok(false) => Err(IpcMethodError::new(ctx.tr.t_replace(
            "codex.parent.not_receivable",
            "{thread}",
            &caller.thread,
        ))),
        Err(detail) => Err(IpcMethodError::new(
            ctx.tr
                .t_replace("codex.parent.check_failed", "{thread}", &caller.thread)
                .replace("{detail}", &detail),
        )),
    }
}

/// 물리 surface가 없는 부모의 호스트 키.
pub(crate) fn external_parent_key(thread: &str) -> String {
    format!("codex:{}", thread.to_ascii_lowercase())
}

/// 호출자의 호스트 부모 키. Codex 부모는 실행 위치(소유 helper surface 또는 공유 daemon)와
/// 관계없이 자기 thread로 자식을 소유한다. 같은 surface에서 `/new`로 바뀐 다른 대화가 이전 대화의
/// 자식을 관리하지 않게 하려는 것이다.
pub(crate) fn caller_parent_key(caller: &CallerContext) -> Value {
    json!({ "parent_external": external_parent_key(&caller.thread) })
}

/// 호출자 thread가 지금 바인딩된 Tasty helper surface. 부모 키가 아니라 실행 위치다.
pub(crate) fn caller_surface(root: &Path, caller: &CallerContext) -> std::io::Result<Option<u32>> {
    Ok(registry::thread_session(root, &caller.thread)?.map(|s| s.surface))
}

pub(crate) fn handle_parent(ctx: &Ctx<'_>, params: &Value) -> Result<Value, IpcMethodError> {
    let caller = require_caller(params, ctx.tr)?;
    let (receiver, terminal_parent) = prepare_receiver(ctx, &caller)?;
    Ok(json!({ "receiver_id": receiver.receiver_id, "terminal_parent": terminal_parent }))
}

fn invalid(ctx: &Ctx<'_>, detail: &str) -> IpcMethodError {
    IpcMethodError::invalid_params(
        &ctx.tr
            .t_replace("codex.deliver.invalid", "{detail}", detail),
    )
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{key} must be a string or null")),
    }
}

pub(crate) fn parse_completion(params: &Value) -> Result<Completion, String> {
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(format!("missing {key}"))
    };
    let completion_key = text("completion_key")?;
    if crate::ids::completion_key_file_name(&completion_key).is_none() {
        return Err("completion_key must be 1..160 bytes of [A-Za-z0-9._:-]".into());
    }
    let provider = text("provider")?;
    if !ledger::PROVIDERS.contains(&provider.as_str()) {
        return Err(format!(
            "provider must be one of {}",
            ledger::PROVIDERS.join("|")
        ));
    }
    let status = text("status")?;
    if !ledger::STATUSES.contains(&status.as_str()) {
        return Err(format!(
            "status must be one of {}",
            ledger::STATUSES.join("|")
        ));
    }
    let child_surface = params
        .get("child_surface")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or("child_surface must be a surface id")?;
    let attempt_id = optional_string(params, "attempt_id")?;
    if let Some(a) = &attempt_id
        && !crate::ids::is_plain_token(a, 128)
    {
        return Err("attempt_id must be a short plain token".into());
    }
    Ok(Completion {
        completion_key,
        provider,
        child_surface,
        child_session: optional_string(params, "child_session")?,
        status,
        body: optional_string(params, "body")?,
        attempt_id,
    })
}

/// 원장에 기록하고 그 thread의 미송신 이벤트를 보낸다.
pub(crate) fn deliver(
    ctx: &Ctx<'_>,
    receiver: &Receiver,
    completion: Completion,
) -> std::io::Result<(Event, bool)> {
    let (event, duplicate) = ledger::capture(ctx.root, receiver, completion)?;
    pump_thread(ctx, &receiver.thread);
    let current = ledger::read_event(ctx.root, &event.event_id)?.unwrap_or(event);
    Ok((current, duplicate))
}

pub(crate) fn handle_deliver(ctx: &Ctx<'_>, params: &Value) -> Result<Value, IpcMethodError> {
    let receiver_id = params
        .get("receiver_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(ctx, "missing receiver_id"))?;
    let receiver = ledger::read_receiver(ctx.root, receiver_id)
        .map_err(io_err)?
        .ok_or_else(|| invalid(ctx, "unknown receiver_id"))?;
    let completion = parse_completion(params).map_err(|e| invalid(ctx, &e))?;
    let (event, duplicate) = deliver(ctx, &receiver, completion).map_err(io_err)?;
    let mut out = json!({
        "event_id": event.event_id,
        "delivery_state": event.delivery.state.as_str(),
    });
    if duplicate {
        out["duplicate"] = json!(true);
    }
    Ok(out)
}

pub(crate) fn handle_result(ctx: &Ctx<'_>, params: &Value) -> Result<Value, IpcMethodError> {
    let caller = require_caller(params, ctx.tr)?;
    let id = params
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let not_found = || {
        IpcMethodError::invalid_params(&ctx.tr.t_replace("codex.result.not_found", "{event}", id))
    };
    let mut event = ledger::read_event(ctx.root, id)
        .map_err(io_err)?
        .ok_or_else(not_found)?;
    if !event.thread.eq_ignore_ascii_case(&caller.thread) {
        return Err(IpcMethodError::invalid_params(&ctx.tr.t_replace(
            "codex.result.other_thread",
            "{event}",
            id,
        )));
    }
    let now = store::now_ms();
    event.read_count += 1;
    event.first_read_at_ms.get_or_insert(now);
    ledger::write_event(ctx.root, &event).map_err(io_err)?;
    Ok(result_view(&event))
}

fn result_view(event: &Event) -> Value {
    json!({
        "event_id": event.event_id,
        "provider": event.provider,
        "status": event.status,
        "status_meaning": "turn boundary reported by the child agent; not a quality verdict",
        "body": event.body,
        "body_truncated": event.body_truncated,
        "child_surface": event.child_surface,
        "child_session": event.child_session,
        "completion_key": event.completion_key,
        "attempt_id": event.attempt_id,
        "captured_at_ms": event.captured_at_ms,
        "delivery": {
            "state": event.delivery.state.as_str(),
            "detail": event.delivery.detail,
            "submission_id": event.delivery.submission_id,
            "error_code": event.delivery.error_code,
            "attempts": event.delivery.attempts,
        },
        "read": { "count": event.read_count, "first_at_ms": event.first_read_at_ms },
    })
}

fn set_state(event: &mut Event, state: DeliveryState, detail: Option<String>) {
    event.delivery.state = state;
    event.delivery.detail = detail;
    event.delivery.updated_at_ms = store::now_ms();
}

/// 이벤트의 송신 대상. Tasty 세션 thread가 지금 붙어 있지 않으면 Err(보류 사유)다.
fn target(ctx: &Ctx<'_>, receiver: &Receiver) -> Result<(Endpoint, bool), String> {
    match receiver.kind {
        ReceiverKind::External => Ok((
            Endpoint::Proxy {
                codex_home: receiver.codex_home.clone(),
                execution: receiver.execution.clone(),
            },
            true,
        )),
        ReceiverKind::Owned => {
            let session = registry::thread_session(ctx.root, &receiver.thread)
                .map_err(|e| e.to_string())?
                .ok_or("the parent thread is not running in a Tasty session")?;
            registry::ready_endpoint(ctx.root, &session).map(|ep| (ep, false))
        }
    }
}

fn write_or_warn(ctx: &Ctx<'_>, event: &Event, what: &str) -> bool {
    match ledger::write_event(ctx.root, event) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("codex delivery {}: {what}: {e}", event.event_id);
            false
        }
    }
}

/// 부모 TUI가 붙었을 때 보류한 결과를 보낸다.
pub(crate) fn flush_thread(ctx: &Ctx<'_>, thread: &str) {
    pump_thread(ctx, thread);
}

fn drop_pending(ctx: &Ctx<'_>, event: &Event) {
    if let Err(e) = ledger::remove_pending(ctx.root, event) {
        tracing::warn!("codex delivery {}: pending index: {e}", event.event_id);
    }
}

/// 이 thread의 다음 할 일을 시작한다. 기록 순서를 지키려고 맨 앞 이벤트가 기다리는 중이면
/// 뒤 이벤트도 기다린다. 자동 재시도 상한에 닿아 보류한 이벤트는 새 계기(같은 thread의
/// 완료, 부모 TUI 연결)에서만 다시 시도한다.
pub(crate) fn pump_thread(ctx: &Ctx<'_>, thread: &str) {
    pump(ctx, thread, true);
}

fn pump(ctx: &Ctx<'_>, thread: &str, resume: bool) {
    if ctx.pump.is_busy(thread) {
        return;
    }
    for id in ledger::pending_ids(ctx.root, thread) {
        let event = match ledger::read_event(ctx.root, &id) {
            Ok(Some(e)) => e,
            Ok(None) => {
                // 이벤트가 없는 표지는 기록 중 중단된 흔적이다.
                if let Err(e) = remove_marker_by_id(ctx.root, thread, &id) {
                    tracing::warn!("codex delivery {id}: pending index: {e}");
                }
                continue;
            }
            Err(e) => {
                tracing::warn!("codex delivery {id}: unreadable: {e}");
                return;
            }
        };
        if !advance(ctx, event, resume) {
            return;
        }
    }
}

/// 맨 앞 이벤트 하나를 진행한다. 이미 끝난 이벤트라 색인에서 지웠고 다음 이벤트로 넘어가도 되면
/// `true`.
fn advance(ctx: &Ctx<'_>, event: Event, resume: bool) -> bool {
    match event.delivery.state {
        DeliveryState::Held if event.delivery.attempts >= MAX_AUTO_ATTEMPTS => {
            if !resume {
                return false;
            }
            let mut event = event;
            event.delivery.attempts = 0;
            if write_or_warn(ctx, &event, "retry reset") {
                start_send(ctx, event);
            }
            false
        }
        DeliveryState::Captured | DeliveryState::Held => {
            start_send(ctx, event);
            false
        }
        DeliveryState::Unknown if !event.delivery.unknown_checked => {
            verify_unknown(ctx, &event);
            false
        }
        // 다른 프로세스에서 시작한 송신은 복구가 unknown으로 바꾼다.
        DeliveryState::Sending => false,
        _ => {
            drop_pending(ctx, &event);
            true
        }
    }
}

fn remove_marker_by_id(root: &Path, thread: &str, id: &str) -> std::io::Result<()> {
    let dir = root.join("pending").join(thread.to_ascii_lowercase());
    for entry in std::fs::read_dir(&dir)?.filter_map(Result::ok) {
        if entry.file_name().to_string_lossy().ends_with(id) {
            store::remove_file(&entry.path())?;
        }
    }
    Ok(())
}

/// 색인에 남은 모든 thread를 진행한다. 상한에 닿아 보류한 이벤트는 건드리지 않는다.
fn pump_all(ctx: &Ctx<'_>) {
    for thread in ledger::pending_threads(ctx.root) {
        pump(ctx, &thread, false);
    }
}

/// captured·held 이벤트를 보낸다. 대상이 없으면 held로 두고, 재시도 시각 전이면 예약한다.
fn start_send(ctx: &Ctx<'_>, mut event: Event) {
    let receiver = match ledger::read_receiver(ctx.root, &event.receiver_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            tracing::warn!("codex delivery {}: receiver missing", event.event_id);
            set_state(
                &mut event,
                DeliveryState::Rejected,
                Some("receiver missing".into()),
            );
            if write_or_warn(ctx, &event, "receiver missing state") {
                drop_pending(ctx, &event);
            }
            return;
        }
        Err(e) => {
            tracing::warn!(
                "codex delivery {}: receiver unreadable: {e}",
                event.event_id
            );
            return;
        }
    };
    let (endpoint, check_loaded) = match target(ctx, &receiver) {
        Ok(t) => t,
        Err(reason) => {
            if event.delivery.state != DeliveryState::Held
                || event.delivery.detail.as_deref() != Some(&reason)
            {
                set_state(&mut event, DeliveryState::Held, Some(reason));
                write_or_warn(ctx, &event, "held state write");
            }
            return;
        }
    };
    let now = store::now_ms();
    if event.delivery.state == DeliveryState::Captured && event.delivery.next_attempt_at_ms > now {
        arm_timer(
            ctx,
            &event.thread,
            Duration::from_millis(event.delivery.next_attempt_at_ms - now),
        );
        return;
    }
    if !ctx.pump.try_begin(&event.thread) {
        // 상한에 걸렸다. 다른 송신이 끝나면 pump_all이 다시 시작한다.
        return;
    }
    // 보내기 전에 sending을 기록해야 재시작 뒤 중복 송신을 막을 수 있다.
    set_state(&mut event, DeliveryState::Sending, None);
    event.delivery.attempts += 1;
    if !write_or_warn(ctx, &event, "sending state write failed, not sent") {
        ctx.pump.finish(&event.thread);
        return;
    }
    let job = Job {
        kind: JobKind::Send,
        event_id: event.event_id.clone(),
        thread: event.thread.clone(),
        endpoint,
        check_loaded,
        text: send::reference_text(&event.provider, &event.status, &event.event_id),
    };
    if let Err(e) = ctx.dispatch.dispatch(job) {
        ctx.pump.finish(&event.thread);
        schedule_retry(ctx, &mut event, format!("send thread: {e}"));
        write_or_warn(ctx, &event, "revert to captured");
    }
}

/// 보내지 못한 이벤트의 다음 시도를 정한다. 상한을 넘으면 보류한다.
fn schedule_retry(ctx: &Ctx<'_>, event: &mut Event, detail: String) {
    if event.delivery.attempts >= MAX_AUTO_ATTEMPTS {
        set_state(
            event,
            DeliveryState::Held,
            Some(format!("automatic retry limit reached: {detail}")),
        );
        return;
    }
    let delay = backoff(event.delivery.attempts);
    set_state(event, DeliveryState::Captured, Some(detail));
    event.delivery.next_attempt_at_ms = store::now_ms() + delay.as_millis() as u64;
    arm_timer(ctx, &event.thread, delay);
}

/// 예약 시각이 이만큼 지나도 깨우지 않았으면 예약을 잃은 것으로 보고 다시 예약한다.
const LOST_WAKE: u64 = 5_000;

fn arm_timer(ctx: &Ctx<'_>, thread: &str, delay: Duration) {
    let now = store::now_ms();
    {
        let mut s = ctx.pump.lock();
        if s.timers.get(thread).is_some_and(|at| *at + LOST_WAKE > now) {
            return;
        }
        s.timers
            .insert(thread.to_string(), now + delay.as_millis() as u64);
    }
    if let Err(e) = ctx.dispatch.wake_after(thread, delay) {
        ctx.pump.lock().timers.remove(thread);
        tracing::warn!("codex delivery: retry timer for {thread}: {e}");
    }
}

/// 예약한 재시도 시각이 됐다. 이 프로세스의 token이 아니면 무시한다.
pub(crate) fn handle_wake(ctx: &Ctx<'_>, params: &Value) -> Result<Value, IpcMethodError> {
    if params.get("token").and_then(Value::as_str) != Some(ctx.token) {
        return Err(IpcMethodError::invalid_params(
            "codex.delivery_wake is internal",
        ));
    }
    let thread = params
        .get("thread")
        .and_then(Value::as_str)
        .filter(|t| crate::ids::is_uuid(t))
        .ok_or_else(|| IpcMethodError::invalid_params("thread"))?;
    ctx.pump.lock().timers.remove(thread);
    pump(ctx, thread, false);
    Ok(json!({}))
}

/// 백그라운드 송신 결과를 원장에 반영한다. 이 프로세스의 token이 아니면 무시한다.
pub(crate) fn handle_outcome(ctx: &Ctx<'_>, params: &Value) -> Result<Value, IpcMethodError> {
    if params.get("token").and_then(Value::as_str) != Some(ctx.token) {
        return Err(IpcMethodError::invalid_params(
            "codex.delivery_outcome is internal",
        ));
    }
    let id = params
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let outcome: Outcome = serde_json::from_value(params["result"].clone())
        .map_err(|e| IpcMethodError::invalid_params(&e.to_string()))?;
    let verify = params.get("kind").and_then(Value::as_str) == Some("verify");
    let Some(mut event) = ledger::read_event(ctx.root, id).map_err(io_err)? else {
        return Ok(json!({ "applied": false }));
    };
    let expected = if verify {
        DeliveryState::Unknown
    } else {
        DeliveryState::Sending
    };
    if event.delivery.state != expected {
        return Ok(json!({ "applied": false }));
    }
    ctx.pump.finish(&event.thread);
    let mut done = true;
    match outcome {
        Outcome::Accepted {
            submission_id,
            confirmed_by,
        } => {
            set_state(
                &mut event,
                DeliveryState::Accepted,
                Some(format!("confirmed by {confirmed_by}")),
            );
            event.delivery.submission_id = submission_id;
        }
        Outcome::Rejected { code, message } => {
            set_state(&mut event, DeliveryState::Rejected, Some(message));
            event.delivery.error_code = Some(code);
        }
        Outcome::NotLoaded => set_state(&mut event, DeliveryState::NotLoaded, None),
        // 아무것도 보내지 못했으므로 다시 보내도 중복이 아니다.
        Outcome::NotSent { detail } => {
            schedule_retry(ctx, &mut event, detail);
            done = false;
        }
        // 대기열 확인 한 번은 펌프가 이어서 한다.
        Outcome::Unknown { detail } => {
            set_state(&mut event, DeliveryState::Unknown, Some(detail));
            done = false;
        }
        Outcome::StillUnknown { detail } => {
            event.delivery.unknown_checked = true;
            event.delivery.detail = Some(detail);
            event.delivery.updated_at_ms = store::now_ms();
        }
    }
    ledger::write_event(ctx.root, &event).map_err(io_err)?;
    if done {
        drop_pending(ctx, &event);
    }
    pump_all(ctx);
    Ok(json!({ "applied": true, "delivery_state": event.delivery.state.as_str() }))
}

/// 플러그인 시작 때 원장을 복구한다. 색인된 이벤트만 읽는다. sending은 수락 여부를 모르므로
/// unknown이 되고 한 번만 대기열을 확인한다. 보존 기간이 지난 종료 이벤트는 지운다.
pub(crate) fn recover(ctx: &Ctx<'_>) {
    for thread in ledger::pending_threads(ctx.root) {
        for id in ledger::pending_ids(ctx.root, &thread) {
            recover_event(ctx, &thread, &id);
        }
    }
    ledger::prune(ctx.root, std::time::SystemTime::now());
    // 재시작은 보류한 결과를 다시 시도하는 계기다.
    turns::pump_turns(ctx, turns::Fresh::All);
    for thread in ledger::pending_threads(ctx.root) {
        pump_thread(ctx, &thread);
    }
}

/// 색인된 이벤트 하나를 복구한다.
fn recover_event(ctx: &Ctx<'_>, thread: &str, id: &str) {
    let Some(mut event) = read_indexed(ctx, thread, id) else {
        return;
    };
    match ledger::repair_claim(ctx.root, &event) {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => tracing::warn!("codex delivery {id}: claim repair: {e}"),
    }
    if event.delivery.state == DeliveryState::Sending {
        set_state(
            &mut event,
            DeliveryState::Unknown,
            Some("the plugin restarted while sending".into()),
        );
        write_or_warn(ctx, &event, "recovery write");
    }
}

/// 색인된 이벤트를 읽는다. 이벤트가 없는 표지는 지운다. 없거나 읽지 못하면 `None`.
fn read_indexed(ctx: &Ctx<'_>, thread: &str, id: &str) -> Option<Event> {
    match ledger::read_event(ctx.root, id) {
        Ok(Some(e)) => Some(e),
        Ok(None) => {
            if let Err(e) = remove_marker_by_id(ctx.root, thread, id) {
                tracing::warn!("codex delivery {id}: pending index: {e}");
            }
            None
        }
        Err(e) => {
            tracing::warn!("codex delivery {id}: unreadable: {e}");
            None
        }
    }
}

fn verify_unknown(ctx: &Ctx<'_>, event: &Event) {
    let Ok(Some(receiver)) = ledger::read_receiver(ctx.root, &event.receiver_id) else {
        return;
    };
    let Ok((endpoint, _)) = target(ctx, &receiver) else {
        return;
    };
    if !ctx.pump.try_begin(&event.thread) {
        return;
    }
    let job = Job {
        kind: JobKind::Verify,
        event_id: event.event_id.clone(),
        thread: event.thread.clone(),
        endpoint,
        check_loaded: false,
        text: String::new(),
    };
    if let Err(e) = ctx.dispatch.dispatch(job) {
        ctx.pump.finish(&event.thread);
        tracing::warn!("codex delivery {}: verify thread: {e}", event.event_id);
    }
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
