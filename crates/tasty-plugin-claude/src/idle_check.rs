//! `claude.idle_check`: 메시지를 보내지 않고 idle 체크만 등록한다. tell·spawn·respawn 의 자동 등록과
//! 원격 턴 결과로 끝내는 경로도 여기서 체크 표([`crate::error_scan::idle`])로 잇는다.

use std::sync::Mutex;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_agent_common::remote_attach::{self, AttachOrigin};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::codex_return::{ParentTarget, Returns};
use crate::error_scan::idle::{At, Finish, IDLE_CHECK_WAIT, Origin, Owner};
use crate::error_scan::{ErrorScanner, lock_scanner};
use crate::stop_pairing::StopPairing;

/// Claude 세션 meta. SessionStart 훅이 남긴다.
const SESSION_META_KEY: &str = "claude-session-id";

/// 대상이 idle 체크를 걸 수 있는 Claude surface 인지 확인하고 지금 상태를 돌려준다.
/// 끝난 터미널(`exited`)과 Claude 를 감시하지 않는 surface 는 거절한다.
fn check_target<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    target: u32,
    tr: &Translator,
) -> Result<String, IpcMethodError> {
    let state = host
        .call("terminal.state", json!({ "surface": target }))
        .map_err(|e| {
            IpcMethodError::new(tr.t_args(
                "claude.idle_check.state_failed",
                &[&target.to_string(), &e.to_string()],
            ))
        })?
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if state.is_empty() || state == "exited" {
        return Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.idle_check.not_running", &target.to_string()),
        ));
    }
    // 잠금은 이 줄에서 푼다. 아래 meta·anchor 조회 동안 감시 루프를 막지 않는다.
    let tracked = lock_scanner(scanner).is_enabled(target);
    let claude = tracked
        || crate::stop_pairing::meta_value(host, target, SESSION_META_KEY).is_some()
        || crate::remote::child_anchor(host, target, tr)
            .ok()
            .flatten()
            .is_some();
    if !claude {
        return Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.idle_check.not_claude", &target.to_string()),
        ));
    }
    Ok(state)
}

/// `claude.idle_check`. `surface` 는 감시할 자식, `caller_surface` 는 idle 줄을 받을 부모다.
/// 대상에 입력을 보내지 않는다. 같은 부모·대상의 앞 체크는 새 체크로 바꾼다.
pub(crate) fn handle_idle_check<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let target = crate::handlers::require_target_surface(params, tr)?;
    let caller = params
        .get("caller_surface")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(tr.t("claude.params.missing_caller_surface"))
        })?;
    if caller == target {
        return Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.idle_check.self_target", &target.to_string()),
        ));
    }
    let registered_at = At::now();
    let state = check_target(scanner, host, target, tr)?;
    let registered = lock_scanner(scanner).idle_register(
        host,
        target,
        caller,
        Origin::Manual,
        Owner::at(registered_at),
    );
    Ok(json!({
        "registered": !registered.completed,
        "id": registered.id,
        "replaced": registered.replaced,
        "target_surface": target,
        "caller_surface": caller,
        "wait_secs": IDLE_CHECK_WAIT.as_secs(),
        "state": state,
    }))
}

/// `claude.tell` 에 idle 체크 등록을 잇는다. 보내기 전의 시각과, 로컬 대상이면 보내기 전에 정한 완료 문턱으로 등록한다.
/// 그래서 보낸 뒤 등록 전에 끝난 이 메시지의 턴은 이 체크를 끝내고, 실행 중인 대상에서 진행 중이던 앞 턴의 끝은 끝내지
/// 않는다. 보내기에 실패하면(오류 응답) 체크를 걸지 않는다. 받을 부모가 없으면 걸지 않는다.
pub(crate) fn handle_tell_with_idle_check<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    returns: &Returns,
    pairing: &Mutex<StopPairing>,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let registered_at = At::now();
    let (resp, threshold) =
        crate::handlers::handle_tell_with_threshold(returns, pairing, host, params, tr)?;
    let (threshold, running) = match threshold {
        Some((threshold, running)) => (Some(threshold), running),
        None => (None, false),
    };
    // 원격 출처(서버 쪽) 호출의 부모는 client 에 있다. 체크는 client 가 건다.
    if AttachOrigin::of(params).is_some() {
        return Ok(resp);
    }
    let caller = params
        .get("caller_surface")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    let target = crate::handlers::require_target_surface(params, tr).ok();
    if let (Some(caller), Some(target)) = (caller, target)
        && caller != target
    {
        lock_scanner(scanner).idle_register(
            host,
            target,
            caller,
            Origin::Tell,
            Owner {
                at: registered_at,
                threshold,
                running,
            },
        );
    }
    Ok(resp)
}

/// spawn·respawn 뒤 물리 부모의 체크를 건다. 일을 준 경우(첫 프롬프트)에만 건다. 프롬프트 없이 띄운 자식은
/// 할 일이 없어 조용한 것이 정상이다.
pub(crate) fn register_for_child<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    target: &ParentTarget,
    child: u32,
    with_prompt: bool,
    origin: Origin,
    registered_at: At,
) {
    if let ParentTarget::Legacy(Some(parent)) = target
        && with_prompt
        && *parent != child
    {
        lock_scanner(scanner).idle_register(host, child, *parent, origin, Owner::at(registered_at));
    }
}

/// respawn 뒤: 앞 세션에 걸린 체크는 새 세션의 것이 아니므로 모두 지우고, 프롬프트를 줬으면 부모의 새 체크를 건다.
pub(crate) fn after_respawn<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    target: &ParentTarget,
    child: u32,
    with_prompt: bool,
    registered_at: At,
) {
    lock_scanner(scanner).idle_drop(host, child);
    register_for_child(
        scanner,
        host,
        target,
        child,
        with_prompt,
        Origin::Respawn,
        registered_at,
    );
}

/// `claude.remote_receive` 로 원격 자식의 턴 결과가 기록됐으면 그 결과의 작업을 기다리던 체크를 끝낸다. 서버가 확정한
/// 턴 끝(`turn_ended`·`turn_failed`)은 그 결과의 route 토큰을 발급한 시각이 등록 뒤인 체크만 끝낸다. 늦게 온 앞 지시의
/// 결과가 다시 건 체크를 끝내지 않게 하려는 것이다. 종료(`exited`)는 모두 끝낸다. 재전송(`duplicate`)과 거절은 보지 않는다.
pub(crate) fn after_remote_receive<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    returns: &Returns,
    host: &H,
    params: &Value,
    response: &Value,
) {
    if response.get("status").and_then(Value::as_str) != Some("recorded") {
        return;
    }
    let Some((surface, payload)) = remote_attach::received(params) else {
        return;
    };
    let completion = payload.get(crate::codex_return::PAYLOAD_KEY);
    let finish = match completion
        .and_then(|c| c.get("status"))
        .and_then(Value::as_str)
    {
        Some("turn_ended" | "turn_failed") => Finish::Remote {
            issued_ms: completion
                .and_then(|c| c.get(remote_attach::ROUTE_FIELD))
                .and_then(Value::as_str)
                .filter(|t| remote_attach::is_route_token(t))
                .and_then(|token| returns.attach_route_issued_ms(token)),
        },
        Some("exited") => Finish::RemoteExit,
        _ => return,
    };
    lock_scanner(scanner).idle_complete(host, surface, finish);
}

/// 재시작 뒤 모든 surface 의 meta 에서 남은 체크를 되살린다. 끝난 터미널의 체크는 버린다.
pub(crate) fn restore_all<H: HostCall>(scanner: &Mutex<ErrorScanner>, host: &H) -> usize {
    let ids: Vec<u32> = match host.call("surface.list", json!({})) {
        Ok(list) => list
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.get("id").and_then(Value::as_u64))
                    .filter_map(|id| u32::try_from(id).ok())
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!("claude idle check restore: surface.list failed: {e}");
            return 0;
        }
    };
    let now = std::time::Instant::now();
    ids.into_iter()
        .map(|id| {
            let n = lock_scanner(scanner).idle_restore(host, id, now);
            if n > 0
                && crate::error_scan::idle::observe(host, id)
                    == crate::error_scan::idle::Observation::Exited
            {
                lock_scanner(scanner).idle_drop(host, id);
                return 0;
            }
            n
        })
        .sum()
}

#[cfg(test)]
#[path = "idle_check_tests.rs"]
mod tests;
