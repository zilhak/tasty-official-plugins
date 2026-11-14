//! Codex 자식 턴의 결과를 소유 app-server의 턴 상태로 확정한다.
//!
//! Stop 훅은 턴이 끝났다는 신호가 아니다. 훅이 실행되는 동안 그 턴은 `inProgress`이고, 다른 Stop
//! 훅이 턴을 이어 가게 하면 같은 턴의 Stop이 다시 온다. 그래서 Stop·Interrupt 훅은 바뀌지 않는
//! 후보(수신자, thread, 세대, turn_id)만 남기고 바로 돌아간다. 확인은 별도 스레드가 세대의 소유
//! 서버에 `thread/turns/list`로 그 turn_id의 상태를 묻고, `completed`·`interrupted`·`failed`가
//! 되면 그 턴의 마지막 agentMessage로 한 번 기록한다. 확인하지 못한 턴 후보는 결과로 추정하지
//! 않고 파일로 남긴다.
//!
//! SessionEnd 훅(Codex 0.154의 `reason`은 늘 `other`)은 thread가 내려갔다는 신호다. TUI가 끝날
//! 때와, 대화를 바꾼 뒤 구독자가 없는 이전 thread를 서버가 unload할 때 모두 온다. 그래서 종료는
//! 세대가 끝났다고 확인될 때(helper의 Exited·Failed, 세션 기록 없음, [`EXIT_GRACE`] 뒤 소유 서버
//! 연결 불가)만 기록하고, 유예 뒤에도 서버가 살아 있으면 기록하지 않는다.
//!
//! 재확인 예약은 후보마다 만들지 않는다. 한 번의 펌프가 남은 후보의 가장 이른 시각 하나를
//! [`super::turn_scheduler`]에 예약하고, 스케줄러 스레드 하나가 그중 가장 이른 시각에 깨운다.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tasty_plugin_sdk::IpcMethodError;

use super::ledger::{self, Completion};
use super::{Ctx, backoff};
use crate::rpc::{Client, Endpoint, RpcError};
use crate::store;

/// 동시에 확인하는 후보 수.
pub(crate) const MAX_TURN_INFLIGHT: usize = 2;
/// 한 프로세스에서 후보 하나를 자동으로 확인하는 횟수 상한. 넘으면 보류하고 같은 세대의 다음
/// 훅이나 플러그인 재시작 때 다시 확인한다.
pub(crate) const MAX_TURN_CHECKS: u32 = 24;
/// 세션 종료 기록이 같은 세대의 턴 확인을 기다리는 최대 시간. 정상 실행에서 턴은 Stop 직후
/// 끝나고 확인은 수십 ms 안에 끝난다.
pub(crate) const EXIT_GRACE: Duration = Duration::from_secs(10);
/// 세션 종료 후보가 세대 종료를 다시 확인하는 간격.
pub(crate) const EXIT_POLL: Duration = Duration::from_secs(1);
/// 한 번 확인에서 읽는 턴 페이지 크기와 페이지 수.
const PAGE: u32 = 16;
const MAX_PAGES: u32 = 4;

/// 확인할 턴 경계. 만든 뒤 바꾸지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Candidate {
    /// 후보 하나의 정체(32자리 16진수 난수). 같은 완료 키로 다시 만든 후보와 구분한다.
    pub id: String,
    pub completion_key: String,
    pub receiver_id: String,
    pub child_surface: u32,
    pub thread: String,
    pub generation: String,
    /// 없으면 세션 종료다.
    pub turn_id: Option<String>,
    pub attempt_id: Option<String>,
    pub created_at_ms: u64,
    /// attach 원격 출처 부모. 있으면 확정한 결과를 원장 대신 그 부모에게 보낼 기록으로 옮긴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_parent: Option<String>,
}

/// 소유 서버가 보고한 턴 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum TurnOutcome {
    Final {
        status: String,
        body: Option<String>,
    },
    InProgress,
    NotFound,
    Unreachable {
        detail: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct TurnJob {
    pub name: String,
    /// 확인을 시작한 후보의 [`Candidate::id`]. 결과는 같은 정체의 후보에만 반영한다.
    pub candidate_id: String,
    pub endpoint: Endpoint,
    pub thread: String,
    pub turn_id: String,
}

fn dir(root: &Path) -> PathBuf {
    root.join("turns")
}

fn file(root: &Path, name: &str) -> PathBuf {
    dir(root).join(format!("{name}.json"))
}

fn name_of(key: &str) -> Option<String> {
    crate::ids::completion_key_file_name(key)
}

/// 후보를 처음 한 번만 만든다. 같은 키가 이미 있으면(이어진 Stop) 바꾸지 않는다.
pub(crate) fn note(ctx: &Ctx<'_>, candidate: &Candidate) -> std::io::Result<bool> {
    let name = name_of(&candidate.completion_key)
        .ok_or_else(|| std::io::Error::other("invalid completion key"))?;
    let text = serde_json::to_string(candidate).map_err(std::io::Error::other)?;
    let created = store::create_claim(&file(ctx.root, &name), &text)?;
    pump_turns(ctx, Fresh::Generation(&candidate.generation));
    Ok(created)
}

fn read(root: &Path, name: &str) -> std::io::Result<Option<Candidate>> {
    match store::read_text(&file(root, name))? {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
    }
}

/// 후보 하나를 읽는다. 내용을 해석할 수 없는 파일은 `turns/corrupt/`로 옮겨 격리하고, 다른 읽기
/// 오류는 경고만 남긴다. 어느 쪽이든 그 후보는 건너뛰며 다른 후보와 다른 자식의 처리를 막지 않는다.
fn read_or_isolate(root: &Path, name: &str) -> Option<Candidate> {
    match read(root, name) {
        Ok(found) => found,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            isolate(root, name, &e);
            None
        }
        Err(e) => {
            tracing::warn!("codex turn {name}: unreadable candidate: {e}");
            None
        }
    }
}

/// 해석할 수 없는 후보 파일을 `turns/corrupt/`로 옮긴다. `e`는 해석 오류다.
fn isolate(root: &Path, name: &str, e: &std::io::Error) {
    let corrupt = dir(root).join("corrupt");
    let to = corrupt.join(format!("{name}.{}.json", store::now_ms()));
    match store::private_dir(&corrupt).and_then(|()| std::fs::rename(file(root, name), &to)) {
        Ok(()) => tracing::warn!("codex turn {name}: isolated unreadable candidate: {e}"),
        Err(move_err) => tracing::warn!(
            "codex turn {name}: unreadable candidate ({e}) could not be isolated: {move_err}"
        ),
    }
}

fn names(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir(root)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|n| n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_string)
        })
        .collect();
    out.sort_unstable();
    out
}

/// 일반(Codex 대화가 아닌) 지시가 그 자식에게 가기 전에, 아직 기록하지 않은 그 자식의 턴·종료
/// 후보를 모두 무효화한다. 이미 원장에 기록한 결과는 건드리지 않는다. 후보는 `turns/void/`로
/// 옮겨 진단용으로만 남기며, 확인 중이던 결과가 늦게 와도 되살리지 않는다. 무효화한 수를
/// 돌려준다.
pub(crate) fn invalidate_surface(ctx: &Ctx<'_>, surface: u32) -> std::io::Result<usize> {
    let mut count = 0;
    for name in names(ctx.root) {
        let Some(candidate) = read_or_isolate(ctx.root, &name) else {
            continue;
        };
        if candidate.child_surface != surface {
            continue;
        }
        retire(ctx, &name, &candidate)?;
        count += 1;
    }
    Ok(count)
}

/// 세대에 `thread`가 새로 묶였다. 그 세대의 다른 thread가 남긴 미확정 종료 후보는 대화 unload에서
/// 온 것이다(새 바인딩이 세대가 살아 있다는 증거다). 종료로 기록하지 않고 `turns/void/`로 옮긴다.
/// 턴 후보와 이미 기록한 결과는 건드리지 않는다. 옮긴 수를 돌려준다.
pub(crate) fn retire_unloaded_exits(
    ctx: &Ctx<'_>,
    generation: &str,
    thread: &str,
) -> std::io::Result<usize> {
    let mut count = 0;
    for name in names(ctx.root) {
        let Some(candidate) = read_or_isolate(ctx.root, &name) else {
            continue;
        };
        if candidate.turn_id.is_some()
            || candidate.generation != generation
            || candidate.thread == thread
        {
            continue;
        }
        retire(ctx, &name, &candidate)?;
        count += 1;
    }
    Ok(count)
}

/// 후보를 기록하지 않고 `turns/void/`로 옮긴다. 예약 정보만 지우며, 이미 실행 중인 확인은
/// 돌아올 때까지 슬롯을 차지한다.
fn retire(ctx: &Ctx<'_>, name: &str, candidate: &Candidate) -> std::io::Result<()> {
    let void = dir(ctx.root).join("void");
    store::private_dir(&void)?;
    let to = void.join(format!("{name}.{}.json", candidate.id));
    match std::fs::rename(file(ctx.root, name), &to) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    ctx.pump.turn_forget(name);
    Ok(())
}

/// 세대의 소유 서버에 아직 연결되는지. 세대가 끝났다고 기록되지 않은 채 연결도 안 되면 helper가
/// 비정상으로 끝난 것이다.
fn server_reachable(ctx: &Ctx<'_>, candidate: &Candidate) -> bool {
    let Ok(Some(session)) = crate::session::registry::read_session(ctx.root, &candidate.generation)
    else {
        return false;
    };
    match crate::session::registry::ready_endpoint(ctx.root, &session) {
        Ok(endpoint) => (ctx.live)(&endpoint, &candidate.thread).is_ok(),
        Err(_) => false,
    }
}

fn remove(root: &Path, name: &str) {
    if let Err(e) = store::remove_file(&file(root, name)) {
        tracing::warn!("codex turn {name}: remove candidate: {e}");
    }
}

/// 펌프를 부른 계기. 새 훅과 재시작은 보류했거나 예약을 기다리는 후보를 지금 확인하게 한다.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Fresh<'a> {
    /// 예약·결과 반영. 시각이 된 후보만 확인한다.
    No,
    /// 그 세대의 새 턴 경계 훅. 그 세대의 후보만 다시 연다.
    Generation(&'a str),
    /// 플러그인 재시작.
    All,
}

/// 세대의 소유 서버가 끝났는지. helper가 Exited·Failed를 기록했거나 세션 기록이 없으면 끝난
/// 것이다. 끝난 서버의 턴은 더 확인할 수 없다.
fn server_gone(root: &Path, generation: &str) -> bool {
    use crate::session::registry::{HelperPhase, read_helper, read_session};
    if matches!(read_session(root, generation), Ok(None)) {
        return true;
    }
    matches!(read_helper(root, generation),
        Ok(Some(h)) if matches!(h.phase, HelperPhase::Exited | HelperPhase::Failed))
}

/// 남은 후보를 확인하고, 다음 확인이 필요하면 깨우기 하나를 예약한다.
pub(crate) fn pump_turns(ctx: &Ctx<'_>, fresh: Fresh<'_>) {
    let now = store::now_ms();
    let all: Vec<(String, Candidate)> = names(ctx.root)
        .into_iter()
        .filter_map(|n| read_or_isolate(ctx.root, &n).map(|c| (n, c)))
        .collect();
    let mut next: Option<u64> = None;
    for (name, candidate) in all.iter().filter(|(_, c)| c.turn_id.is_some()) {
        let reopen = match fresh {
            Fresh::No => false,
            Fresh::Generation(g) => g == candidate.generation,
            Fresh::All => true,
        };
        if reopen {
            ctx.pump.turn_reset(name);
        }
        if ctx.pump.turn_due(name, &candidate.id, now) {
            start_check(ctx, name, candidate);
        }
    }
    for (name, candidate) in all.iter().filter(|(_, c)| c.turn_id.is_none()) {
        if let Some(at) = settle_exit(ctx, name, candidate, now) {
            next = Some(next.map_or(at, |t| t.min(at)));
        }
    }
    for (name, candidate) in all.iter().filter(|(_, c)| c.turn_id.is_some()) {
        if let Some(at) = ctx.pump.turn_next(name, &candidate.id) {
            next = Some(next.map_or(at, |t| t.min(at)));
        }
    }
    if let Some(at) = super::attach_out::pump(ctx) {
        next = Some(next.map_or(at, |t| t.min(at)));
    }
    if let Some(at) = next {
        arm_wake(ctx, at);
    }
}

/// 종료 후보 하나를 처리한다. 아직 유예 중이면 다시 확인할 시각을 돌려준다.
fn settle_exit(ctx: &Ctx<'_>, name: &str, candidate: &Candidate, now: u64) -> Option<u64> {
    // SessionEnd는 thread가 내려갔다는 뜻이다. 세대(TUI)가 끝났을 때와 대화만 바뀌어 이전
    // thread가 unload될 때 모두 온다. 세대가 끝났다고 확인될 때만 종료로 기록하고, 유예가
    // 지나도 소유 서버가 살아 있으면 대화가 바뀐 것이므로 기록하지 않는다. 세대가 끝나면 남은
    // 턴은 확인할 수 없으므로 기다리지 않는다.
    let deadline = candidate.created_at_ms + EXIT_GRACE.as_millis() as u64;
    if server_gone(ctx.root, &candidate.generation) {
        capture(ctx, name, candidate, "exited", None);
    } else if now < deadline {
        return Some(deadline.min(now + EXIT_POLL.as_millis() as u64));
    } else if server_reachable(ctx, candidate) {
        tracing::info!(
            "codex turn {name}: thread {} ended while its session kept running; not an exit",
            candidate.thread
        );
        if let Err(e) = retire(ctx, name, candidate) {
            tracing::warn!("codex turn {name}: retire a conversation end: {e}");
        }
    } else {
        capture(ctx, name, candidate, "exited", None);
    }
    None
}

/// `at`에 깨우기를 예약한다. 스케줄러가 가장 이른 예약 하나로 합친다.
fn arm_wake(ctx: &Ctx<'_>, at: u64) {
    if let Err(e) = ctx.dispatch.wake_turns_at(at) {
        tracing::warn!("codex turn: retry timer: {e}");
    }
}

fn start_check(ctx: &Ctx<'_>, name: &str, candidate: &Candidate) {
    let Some(turn_id) = candidate.turn_id.clone() else {
        return;
    };
    let endpoint = match crate::session::registry::read_session(ctx.root, &candidate.generation) {
        Ok(Some(session)) => crate::session::registry::ready_endpoint(ctx.root, &session),
        Ok(None) => Err("session record missing".to_string()),
        Err(e) => Err(format!("session record unreadable: {e}")),
    };
    let endpoint = match endpoint {
        Ok(e) => e,
        Err(detail) => {
            retry(ctx, name, candidate, &detail);
            return;
        }
    };
    if !ctx.pump.turn_begin(&candidate.id) {
        return;
    }
    let job = TurnJob {
        name: name.to_string(),
        candidate_id: candidate.id.clone(),
        endpoint,
        thread: candidate.thread.clone(),
        turn_id,
    };
    if let Err(e) = ctx.dispatch.check_turn(job) {
        ctx.pump.turn_finish(&candidate.id);
        retry(ctx, name, candidate, &format!("check thread: {e}"));
    }
}

/// 확인하지 못했다. 서버가 끝났으면 바로 보류하고, 상한 안이면 다음 시각을 정하고, 넘으면
/// 보류한다. 깨우기는 펌프가 묶어서 예약한다.
fn retry(ctx: &Ctx<'_>, name: &str, candidate: &Candidate, detail: &str) {
    if server_gone(ctx.root, &candidate.generation) {
        ctx.pump.turn_hold(name);
        tracing::warn!("codex turn {name}: server of the session has exited; not confirmed, held");
        return;
    }
    let checks = ctx.pump.turn_failed(name);
    if checks >= MAX_TURN_CHECKS {
        tracing::warn!("codex turn {name}: not confirmed after {checks} checks, held: {detail}");
        return;
    }
    let delay = backoff(checks);
    ctx.pump
        .turn_schedule(name, store::now_ms() + delay.as_millis() as u64);
}

fn capture(ctx: &Ctx<'_>, name: &str, candidate: &Candidate, status: &str, body: Option<String>) {
    if candidate.attach_parent.is_some() {
        let completion = completion_of(candidate, status, body);
        match super::attach_out::queue(ctx.root, name, &completion, &candidate.receiver_id) {
            Ok(()) => {
                remove(ctx.root, name);
                ctx.pump.turn_forget(name);
                if let Some(at) = super::attach_out::pump(ctx) {
                    arm_wake(ctx, at);
                }
            }
            Err(e) => tracing::warn!("codex turn {name}: attach result not kept: {e}"),
        }
        return;
    }
    let Some(receiver) = receiver_of(ctx, name, candidate) else {
        return;
    };
    match super::deliver(ctx, &receiver, completion_of(candidate, status, body)) {
        Ok(_) => {
            remove(ctx.root, name);
            ctx.pump.turn_forget(name);
        }
        Err(e) => tracing::warn!("codex turn {name}: capture failed: {e}"),
    }
}

fn completion_of(candidate: &Candidate, status: &str, body: Option<String>) -> Completion {
    Completion {
        completion_key: candidate.completion_key.clone(),
        provider: "codex".into(),
        child_surface: candidate.child_surface,
        child_session: Some(candidate.thread.clone()),
        status: status.into(),
        body,
        attempt_id: candidate.attempt_id.clone(),
    }
}

/// 후보의 수신자. 수신자가 없어졌으면 후보를 지운다. 읽지 못했으면 후보를 남긴다.
fn receiver_of(ctx: &Ctx<'_>, name: &str, candidate: &Candidate) -> Option<ledger::Receiver> {
    match ledger::read_receiver(ctx.root, &candidate.receiver_id) {
        Ok(Some(r)) => Some(r),
        Ok(None) => {
            tracing::warn!(
                "codex turn {name}: receiver {} missing",
                candidate.receiver_id
            );
            remove(ctx.root, name);
            None
        }
        Err(e) => {
            tracing::warn!("codex turn {name}: receiver unreadable: {e}");
            None
        }
    }
}

/// 서버의 턴 상태를 Tasty의 반환 상태로 바꾼다.
fn status_of(server: &str) -> Option<&'static str> {
    match server {
        "completed" => Some("turn_ended"),
        "interrupted" => Some("interrupted"),
        "failed" => Some("turn_failed"),
        _ => None,
    }
}

/// 확인 결과를 반영한다. 이 프로세스의 token이 아니면 무시한다.
pub(crate) fn handle_turn_outcome(ctx: &Ctx<'_>, params: &Value) -> Result<Value, IpcMethodError> {
    if params.get("token").and_then(Value::as_str) != Some(ctx.token) {
        return Err(IpcMethodError::invalid_params(
            "codex.turn_outcome is internal",
        ));
    }
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| IpcMethodError::invalid_params("name"))?;
    let outcome: TurnOutcome = serde_json::from_value(params["result"].clone())
        .map_err(|e| IpcMethodError::invalid_params(&e.to_string()))?;
    // 이 결과를 낸 확인 작업의 슬롯만 비운다. 후보가 무효화됐거나 다시 만들어졌어도 같다.
    let job = params
        .get("candidate")
        .and_then(Value::as_str)
        .filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| IpcMethodError::invalid_params("candidate"))?;
    ctx.pump.turn_finish(job);
    let candidate = read_or_isolate(ctx.root, name);
    // 무효화됐거나(일반 지시) 같은 키로 다시 만든 후보에는 늦게 온 결과를 반영하지 않는다.
    let Some(candidate) = candidate.filter(|c| c.id == job) else {
        // 비운 슬롯으로 기다리던 후보를 확인한다.
        pump_turns(ctx, Fresh::No);
        return Ok(json!({ "applied": false }));
    };
    let applied = match outcome {
        TurnOutcome::Final { status, body } => match status_of(&status) {
            Some(s) => {
                capture(ctx, name, &candidate, s, body);
                "captured"
            }
            None => {
                retry(
                    ctx,
                    name,
                    &candidate,
                    &format!("unexpected turn status {status}"),
                );
                "retry"
            }
        },
        TurnOutcome::InProgress => {
            retry(ctx, name, &candidate, "turn in progress");
            "in_progress"
        }
        TurnOutcome::NotFound => {
            retry(ctx, name, &candidate, "turn not found");
            "not_found"
        }
        TurnOutcome::Unreachable { detail } => {
            retry(ctx, name, &candidate, &detail);
            "unreachable"
        }
    };
    // 이 후보가 끝나 기다리던 종료나 다른 후보를 이어서 확인한다.
    pump_turns(ctx, Fresh::No);
    Ok(json!({ "applied": applied }))
}

/// 예약한 확인 시각이 됐다.
pub(crate) fn handle_turn_wake(ctx: &Ctx<'_>, params: &Value) -> Result<Value, IpcMethodError> {
    if params.get("token").and_then(Value::as_str) != Some(ctx.token) {
        return Err(IpcMethodError::invalid_params(
            "codex.turn_wake is internal",
        ));
    }
    pump_turns(ctx, Fresh::No);
    Ok(json!({}))
}

/// 소유 서버에서 그 턴의 상태와 마지막 agentMessage를 읽는다. 최근 턴부터 상한 페이지까지 본다.
pub(crate) fn read_turn(job: &TurnJob) -> TurnOutcome {
    let mut client = match Client::connect(&job.endpoint, super::send::RPC_TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            return TurnOutcome::Unreachable {
                detail: e.to_string(),
            };
        }
    };
    let found = find_turn(&mut client, &job.thread, &job.turn_id);
    client.close();
    match found {
        Ok(Some(turn)) => outcome_of(&turn),
        Ok(None) => TurnOutcome::NotFound,
        Err(e) => TurnOutcome::Unreachable {
            detail: e.to_string(),
        },
    }
}

fn find_turn(client: &mut Client, thread: &str, turn_id: &str) -> Result<Option<Value>, RpcError> {
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params = json!({ "threadId": thread, "limit": PAGE, "itemsView": "full" });
        if let Some(c) = &cursor {
            params["cursor"] = json!(c);
        }
        let page = client.call("thread/turns/list", params)?;
        let turns = page.get("data").and_then(Value::as_array);
        if let Some(turn) = turns
            .into_iter()
            .flatten()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(turn_id))
        {
            return Ok(Some(turn.clone()));
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(None)
}

/// 턴 하나의 상태. 끝난 턴의 본문은 마지막 agentMessage이고, 실패한 턴에 메시지가 없으면 오류
/// 문구다.
pub(crate) fn outcome_of(turn: &Value) -> TurnOutcome {
    let status = turn.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "inProgress" {
        return TurnOutcome::InProgress;
    }
    let last_message = turn
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("agentMessage"))
        .filter_map(|i| i.get("text").and_then(Value::as_str))
        .rfind(|t| !t.is_empty())
        .map(str::to_string);
    let body = last_message.or_else(|| {
        (status == "failed")
            .then(|| turn.pointer("/error/message").and_then(Value::as_str))
            .flatten()
            .map(str::to_string)
    });
    TurnOutcome::Final {
        status: status.to_string(),
        body,
    }
}

/// 스레드 하나에서 [`read_turn`]을 실행하고 결과를 worker로 돌려준다.
pub(crate) fn spawn_check(
    host: tasty_plugin_sdk::HostHandle,
    token: String,
    job: TurnJob,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name(format!("codex-turn-{}", &job.name[..8]))
        .spawn(move || {
            let outcome = read_turn(&job);
            let params = json!({ "name": job.name, "candidate": job.candidate_id,
                "token": token, "result": outcome });
            // 잃으면 다음 훅이나 재시작 때 다시 확인한다.
            if let Err(e) = host.self_invoke("codex.turn_outcome", params) {
                tracing::warn!("codex turn {}: outcome lost: {e}", job.name);
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}
