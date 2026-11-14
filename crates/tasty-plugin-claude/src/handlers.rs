//! Claude IPC 처리. 자식 터미널의 생성·조회·입력·종료는 호스트 terminal.*에 위임한다.
//! 여기서는 Claude 실행 명령·세션 토큰·오류 감시·알림과 공개 응답 형식을 처리한다.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tasty_plugin_agent_common::children::{join_indices, spawn_census};
use tasty_plugin_agent_common::host_call::HostCall;
#[cfg(test)]
use tasty_plugin_agent_common::host_call::cleanup_sibling_hooks;
use tasty_plugin_agent_common::params::{
    TargetSurfaceError, U32FieldError, forward, target_surface,
};
use tasty_plugin_agent_common::prompt_file;
use tasty_plugin_agent_common::remote_attach::AttachOrigin;
use tasty_plugin_agent_common::remote_owner::{self, Admit};
use tasty_plugin_sdk::{HostHandle, IpcMethodError, i18n::Translator};

use crate::codex_return::{Binding, ParentTarget, Returns, RouteTo, TerminalParent, parent_target};
use crate::error_scan::{ErrorScanner, ScanTarget};
use crate::reboot::reboot_surface;

/// 경로나 등록 이름 목록을 --settings 파일 하나로 해석한다.
/// profile_file과 profile을 함께 지정하면 우선순위를 정하지 않고 거절한다.
pub(crate) fn resolve_profile_file_param(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Option<String>, IpcMethodError> {
    let path = params
        .get("profile_file")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let names = params
        .get("profile")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    match (path, names) {
        (Some(_), Some(_)) => Err(IpcMethodError::new(
            tr.t("claude.profile.mutually_exclusive_file_and_profile"),
        )),
        (Some(p), None) => Ok(Some(p.to_string())),
        (None, Some(n)) => crate::profile::resolve_names(data_dir, n, tr)
            .map(|p| Some(p.to_string_lossy().into_owned()))
            .map_err(|e| crate::profile::to_ipc_err(e, tr)),
        (None, None) => Ok(None),
    }
}

/// 이 플러그인이 허용하는 Claude Code 권한 모드. 의미는 Claude Code가 정하며 값만 전달한다.
/// 2026-09-12의 claude --help로 확인한 목록이다.
pub(crate) const VALID_PERMISSION_MODES: &[&str] = &[
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

/// 승인 정책 기본값을 담는 plugin 설정 키. 매니페스트
/// `[[contributes.settings_pages.items]]` 의 `storage_key` 와 같은 문자열이어야 한다.
const PERMISSION_MODE_SETTING_KEY: &str = "default_permission_mode";

/// `--permission-mode` 조각(또는 빈 문자열). 앞에 공백을 포함해 기존 명령 문자열
/// 뒤에 그대로 이어 붙인다.
fn permission_mode_flag(mode: Option<&str>) -> String {
    match mode {
        Some(m) => format!(" --permission-mode {m}"),
        None => String::new(),
    }
}

/// permissions.defaultMode를 지정했는지 확인한다. allow·deny 규칙 목록은 모드와 별개다.
fn settings_json_sets_default_mode(settings: &Value) -> bool {
    settings
        .get("permissions")
        .and_then(|p| p.get("defaultMode"))
        .is_some()
}

/// 프로필이 권한 모드를 지정했는지 확인한다. 읽기·파싱에 실패하면 false로 처리한다.
fn profile_file_sets_default_mode(path: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|v| settings_json_sets_default_mode(&v))
}

/// 기본 권한 모드. 미설정·inherit이면 플래그를 붙이지 않는다.
/// 알 수 없는 설정값은 경고하고 무시한다.
fn default_permission_mode<H: HostCall>(host: &H) -> Option<String> {
    let value = host
        .call(
            "settings.get_plugin_setting",
            json!({ "storage_key": PERMISSION_MODE_SETTING_KEY }),
        )
        .ok()
        .and_then(|v| v.get("value").and_then(|v| v.as_str()).map(String::from))
        .filter(|s| s != "inherit" && !s.is_empty())?;
    if !VALID_PERMISSION_MODES.contains(&value.as_str()) {
        tracing::warn!("claude: ignoring unknown {PERMISSION_MODE_SETTING_KEY} setting '{value}'");
        return None;
    }
    Some(value)
}

/// 명시한 인자, 플러그인 설정, 플래그 생략 순서로 권한 모드를 고른다.
/// 명시한 인자와 프로필의 permissions.defaultMode가 함께 있으면 거절한다.
/// 명시하지 않은 설정 폴백에는 이 충돌 검사를 적용하지 않는다.
pub(crate) fn resolve_permission_mode<H: HostCall>(
    host: &H,
    params: &Value,
    profile_file: Option<&str>,
    tr: &Translator,
) -> Result<Option<String>, IpcMethodError> {
    let explicit = params
        .get("permission_mode")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let Some(mode) = explicit else {
        return Ok(default_permission_mode(host));
    };
    if !VALID_PERMISSION_MODES.contains(&mode) {
        return Err(IpcMethodError::invalid_params(&tr.t_fmt(
            "claude.params.invalid_permission_mode",
            &format!("{mode} (valid: {})", VALID_PERMISSION_MODES.join(", ")),
        )));
    }
    if profile_file.is_some_and(profile_file_sets_default_mode) {
        return Err(IpcMethodError::invalid_params(
            tr.t("claude.params.permission_mode_conflicts_with_profile"),
        ));
    }
    Ok(Some(mode.to_string()))
}

/// 공용 u32 판정으로 누락·잘못된 값·범위 초과를 구분하고 Claude 번역문으로 안내한다.
/// 큰 값을 잘라 다른 대상 id로 해석하지 않는다.
fn require_u32(
    params: &Value,
    key: &str,
    missing_key: &str,
    malformed_key: &str,
    tr: &Translator,
) -> Result<u32, IpcMethodError> {
    tasty_plugin_agent_common::params::u32_field(params, key).map_err(|e| match e {
        U32FieldError::Missing => IpcMethodError::invalid_params(tr.t(missing_key)),
        U32FieldError::Malformed { raw } => {
            IpcMethodError::invalid_params(&tr.t_fmt(malformed_key, &raw))
        }
    })
}

/// 공용 판정으로 surface·surface_id를 읽고 오류를 Claude 번역문으로 안내한다.
pub(crate) fn optional_target_surface(
    params: &Value,
    tr: &Translator,
) -> Result<Option<u32>, IpcMethodError> {
    target_surface(params).map_err(|e| match e {
        // 두 인자 중 잘못된 키 이름도 안내에 포함한다.
        TargetSurfaceError::Malformed { key, raw } => IpcMethodError::invalid_params(&tr.t_fmt(
            "claude.params.target_surface_not_a_number",
            &format!("'{key}' = {raw}"),
        )),
        TargetSurfaceError::Conflict {
            surface,
            surface_id,
        } => IpcMethodError::invalid_params(&tr.t_fmt(
            "claude.params.surface_conflict",
            &format!("surface={surface}, surface_id={surface_id}"),
        )),
    })
}

/// surface·surface_id를 같은 대상 필드로 읽되 둘 다 없으면 거절한다.
pub(crate) fn require_target_surface(
    params: &Value,
    tr: &Translator,
) -> Result<u32, IpcMethodError> {
    optional_target_surface(params, tr)?
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_surface_id")))
}

/// 호스트로 넘길 params 에 대상 surface 를 싣는다 — 실패 문구만 claude 것으로 옮긴다.
#[cfg(test)]
fn put_target_surface(
    dst: &mut serde_json::Map<String, Value>,
    params: &Value,
    tr: &Translator,
) -> Result<(), IpcMethodError> {
    if let Some(surface) = optional_target_surface(params, tr)? {
        dst.insert("surface".into(), json!(surface));
    }
    Ok(())
}

pub(crate) fn require_child_index(params: &Value, tr: &Translator) -> Result<u32, IpcMethodError> {
    require_u32(
        params,
        "child_index",
        "claude.params.missing_child_index",
        "claude.params.child_index_not_a_number",
        tr,
    )
}

/// 부모의 자식 인덱스를 surface id로 바꾼다.
/// terminal.children 원본의 surface_id를 읽으며 Claude 응답의 child_surface_id와 구분한다.
pub(crate) fn resolve_child_surface_id<H: HostCall>(
    host: &H,
    parent_surface_id: u32,
    child_index: u32,
    tr: &Translator,
) -> Result<u32, IpcMethodError> {
    let resp = host
        .call("terminal.children", json!({ "surface": parent_surface_id }))
        .map_err(IpcMethodError::from)?;
    let children = resp
        .get("children")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let found = children
        .iter()
        .find(|c| c.get("index").and_then(|v| v.as_u64()) == Some(child_index as u64))
        .and_then(|c| c.get("surface_id").and_then(|v| v.as_u64()))
        .map(|v| v as u32);
    found.ok_or_else(|| {
        // 대상 인덱스를 찾지 못하면 현재 사용할 수 있는 인덱스도 안내한다.
        let available: Vec<String> = children
            .iter()
            .filter_map(|c| c.get("index").and_then(|v| v.as_u64()))
            .map(|i| i.to_string())
            .collect();
        let available = if available.is_empty() {
            "-".to_string()
        } else {
            available.join(", ")
        };
        IpcMethodError::invalid_params(&tr.t_args(
            "claude.child.unknown_index",
            &[
                &child_index.to_string(),
                &parent_surface_id.to_string(),
                &available,
            ],
        ))
    })
}

fn host_call<H: HostCall>(host: &H, method: &str, params: Value) -> Result<Value, IpcMethodError> {
    host.call(method, params).map_err(IpcMethodError::from)
}

/// 호스트 자식 목록에 전경 프로세스 정보를 추가한다.
/// 공개 응답은 배열이며 호스트의 surface_id를 child_surface_id로 바꾼다.
/// 호스트 객체를 그대로 반환하는 Codex와 형식이 다르다.
pub(crate) fn handle_children<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let target = parent_target(host, params, tr)?;
    let mut cp = serde_json::Map::new();
    returns.insert_parent(host, &target, params, &mut cp, "surface", None, tr)?;
    let resp = host_call(host, "terminal.children", Value::Object(cp))?;
    let list = resp
        .get("children")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut entries: Vec<Value> = list
        .iter()
        .map(|c| {
            json!({
                "child_surface_id": c.get("surface_id").cloned().unwrap_or(Value::Null),
                "index": c.get("index").cloned().unwrap_or(Value::Null),
                "cwd": c.get("cwd").cloned().unwrap_or(Value::Null),
                "role": c.get("role").cloned().unwrap_or(Value::Null),
                "nickname": c.get("nickname").cloned().unwrap_or(Value::Null),
                "state": c.get("state").cloned().unwrap_or(Value::Null),
                // 상태뿐 아니라 판단 근거와 확실성도 함께 전달한다.
                "evidence": c.get("evidence").cloned().unwrap_or(Value::Null),
                "confidence": c.get("confidence").cloned().unwrap_or(Value::Null),
                // attach mirror 의 원격 자식 표지와, 연결이 끊긴 동안의 확인 불가 상태.
                "remote": c.get("remote").cloned().unwrap_or(Value::Null),
                "liveness": c.get("liveness").cloned().unwrap_or(Value::Null),
                "parent": c.get("parent").cloned().unwrap_or(Value::Null),
                "last_report_at": c.get("last_report_at").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    for entry in &mut entries {
        let Some(sid) = entry
            .get("child_surface_id")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
        else {
            continue;
        };
        if let Ok(resp) = host.call("surface.foreground_process", json!({ "surface_id": sid })) {
            if let Some(name) = resp.get("name").and_then(|v| v.as_str()) {
                entry["foreground_process"] = json!(name);
            }
            if let Some(pid) = resp.get("pid").and_then(|v| v.as_u64()) {
                entry["foreground_pid"] = json!(pid);
            }
        }
    }
    Ok(json!(entries))
}

/// terminal.kill에 종료를 위임하고 호스트 응답에 `killed`를 더해 반환한다.
/// 실제로 닫힌 경우(killed_surface_id)에만 `killed: true`로 답하고 오류 감시에서 제거한다.
/// 원격으로 넘긴 kill은 forwarded 응답에 `killed: false`를 붙인다.
pub(crate) fn handle_kill<H: HostCall>(
    scanner: &Arc<Mutex<ErrorScanner>>,
    returns: &Returns,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    if let Some(origin) = AttachOrigin::of(params) {
        return crate::remote::serve_kill(scanner, returns, host, params, &origin, tr);
    }
    let child_index = require_child_index(params, tr)?;
    let target = parent_target(host, params, tr)?;
    let mut kp = serde_json::Map::new();
    returns.insert_parent(host, &target, params, &mut kp, "surface", None, tr)?;
    kp.insert("child".into(), json!(child_index));
    // attach mirror 의 원격 자식은 서버의 같은 메서드로 닫는다.
    if let Some(child) = crate::remote::child_by_index(host, &kp, child_index)
        && let Some(anchor) = crate::remote::child_anchor(host, child, tr)?
    {
        return crate::remote::kill(returns, host, anchor, child, child_index, tr);
    }
    let resp = host_call(host, "terminal.kill", Value::Object(kp))?;
    let killed_id = resp
        .get("killed_surface_id")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    // 다음 폴링을 기다리지 않고 오류 감시에서 제거한다.
    if let Some(killed) = killed_id {
        crate::error_scan::lock_scanner(scanner).stop_tracking(host, killed);
        // 닫힌 자식의 결과 경로를 지운다. 보내지 못한 결과는 남는다.
        returns.forget_child(killed);
    }
    // 기존 호출자가 읽던 `killed`를 유지하되, 실제로 닫힌 경우에만 true로 둔다.
    let mut out = resp;
    if let Value::Object(m) = &mut out {
        m.insert("killed".into(), json!(killed_id.is_some()));
    }
    Ok(out)
}

/// 부모의 자식에게 텍스트를 보내는 terminal.broadcast에 위임한다. role 필터도 전달한다.
pub(crate) fn handle_broadcast<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let text = params
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_text")))?;
    let target = parent_target(host, params, tr)?;
    let mut bp = forward(params, &["role"]);
    returns.insert_parent(host, &target, params, &mut bp, "surface", None, tr)?;
    bp.insert("text".into(), json!(text));
    host_call(host, "terminal.broadcast", Value::Object(bp))
}

/// terminal.tell에 메시지를 전달한다. 줄바꿈과 제출 처리는 호스트가 담당한다.
/// Codex 호출이면 명시한 대상이 그 대화의 자식인지 확인하고, 이 턴의 결과를 그 대화로 돌려보내도록
/// 경로를 먼저 저장한 뒤 보낸다.
pub(crate) fn handle_tell<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    if let Some(origin) = AttachOrigin::of(params) {
        return crate::remote::serve_tell(returns, host, params, &origin, tr);
    }
    let surface_id = require_target_surface(params, tr)?;
    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_message")))?;
    let remote = crate::remote::child_anchor(host, surface_id, tr)?;
    let resolved = match crate::codex_return::codex_caller(params, tr)? {
        Some(caller) => Some(crate::codex_return::resolve_parent(host, &caller, tr)?),
        None => None,
    };
    // 조회와 검사는 경로를 바꾸기 전에 끝낸다. 여기서 실패하면 아무 기록도 바뀌지 않는다.
    let to = tell_target(
        returns,
        host,
        params,
        surface_id,
        remote.is_some(),
        &resolved,
        tr,
    )?;
    let saved = returns.child_routes(surface_id).map_err(|e| {
        IpcMethodError::new(tr.t_args(
            "claude.codex.route_not_released",
            &[&surface_id.to_string(), &e],
        ))
    })?;
    // 원격 자식의 결과 소유자는 만든 부모로 고정한다. 다른 부모의 tell 은 경로를 바꾸기 전에 거절한다.
    let owner = remote.map(|_| {
        to.as_ref()
            .map_or_else(|| remote_owner::NOBODY.to_string(), RouteTo::owner)
    });
    let claimed = match &owner {
        Some(owner) => crate::remote::admit(returns, surface_id, owner, tr)? == Admit::Claimed,
        None => false,
    };
    // 확정된 실패는 경로와 소유를 이전 상태로 되돌린다. 결과불명이면 서버에서 실행됐을 수 있으므로 그대로 둔다.
    let rollback = |e: IpcMethodError| {
        if let Err(r) = returns.restore_child(surface_id, saved.clone()) {
            tracing::warn!("claude tell: could not restore the route of s{surface_id}: {r}");
        }
        if claimed && let Some(owner) = &owner {
            crate::remote::unclaim(returns, surface_id, owner);
        }
        e
    };
    let route = tell_route(
        returns,
        surface_id,
        resolved.as_ref(),
        remote.and(to.as_ref()),
        tr,
    )
    .map_err(rollback)?;
    let resp = match remote {
        Some(anchor) => match crate::remote::tell(host, anchor, surface_id, message, route, tr) {
            Ok(resp) => resp,
            Err(e) if e.unknown => return Err(e.error),
            Err(e) => return Err(rollback(e.error)),
        },
        None => host_call(
            host,
            "terminal.tell",
            json!({ "surface": surface_id, "text": message }),
        )
        .map_err(rollback)?,
    };

    // 알림을 받을 caller_surface가 없으면 훅 등록만 생략하고 tell 결과는 유지한다.
    if let Some(caller_surface) = params.get("caller_surface").and_then(|v| v.as_u64()) {
        register_notify_hooks(host, caller_surface as u32, surface_id, "tell");
    }

    Ok(resp)
}

/// 완료 알림을 받을 호출 surface.
fn caller_surface_of(params: &Value) -> Option<u32> {
    params
        .get("caller_surface")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// tell 의 수신처를 정한다. 기록은 바꾸지 않는다. Codex 호출이면 대상이 그 대화의 자식인지 확인한다. 원격 자식에게
/// 일반 호출이 보내면 호출 surface 가 수신처다. 호출 surface 가 없거나 로컬 자식의 일반 호출이면 수신처가 없다.
fn tell_target<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    surface_id: u32,
    remote: bool,
    resolved: &Option<crate::codex_return::ResolvedParent>,
    tr: &Translator,
) -> Result<Option<RouteTo>, IpcMethodError> {
    if let Some(resolved) = resolved {
        require_returns(returns, tr)?;
        let target = ParentTarget::Codex(resolved.clone());
        let mut parent = serde_json::Map::new();
        returns.insert_parent(
            host,
            &target,
            params,
            &mut parent,
            "surface",
            Some(surface_id),
            tr,
        )?;
        if !crate::codex_return::is_child_of(host, parent, surface_id)? {
            return Err(IpcMethodError::invalid_params(
                &tr.t_fmt("claude.codex.not_a_child", &surface_id.to_string()),
            ));
        }
        return Ok(Some(RouteTo::Codex(resolved.receiver_id.clone())));
    }
    if !remote {
        return Ok(None);
    }
    // 원격 자식의 본문은 서버에만 있으므로 호출 surface 에 결과를 돌려받을 토큰을 보낸다.
    let Some(caller) = caller_surface_of(params) else {
        return Ok(None);
    };
    require_returns(returns, tr)?;
    Ok(Some(RouteTo::Surface {
        surface: caller,
        // 세션 조회 실패를 세션 없음으로 낮추면 세션 보호가 빠진 토큰이 나간다. 보내기 전에 실패한다.
        session: tasty_plugin_agent_common::remote_results::parent_session(host, caller)
            .map_err(|e| IpcMethodError::new(tr.t_fmt("claude.codex.route_not_saved", &e)))?,
    }))
}

/// 정한 수신처로 경로를 바꾼다. Codex 호출이면 경로를 묶고, 일반 호출이면 이전 Codex 경로를 끊는다. 원격 자식이면
/// 이 지시의 route 토큰을 준다. 서버는 이 지시로 시작한 턴의 결과에 토큰을 실어 돌려준다.
fn tell_route(
    returns: &Returns,
    surface_id: u32,
    resolved: Option<&crate::codex_return::ResolvedParent>,
    remote_to: Option<&RouteTo>,
    tr: &Translator,
) -> Result<Option<String>, IpcMethodError> {
    let route_err = |e: String| IpcMethodError::new(tr.t_fmt("claude.codex.route_not_saved", &e));
    match resolved {
        Some(resolved) => returns
            .bind_tell(surface_id, &Binding::new(resolved))
            .map_err(route_err)?,
        // 일반 호출이 새 지시를 보내면 이 자식의 다음 결과는 그 호출자의 것이다. 보내기 전에 Codex 경로를 끊는다.
        None => release_codex_route(returns, surface_id, tr)?,
    }
    remote_to
        .map(|to| returns.issue_attach_route(to).map_err(route_err))
        .transpose()
}

/// 일반 호출(tell·respawn)이 자식에게 새 지시를 보내기 전에 이전 Codex 부모의 결과 경로를 끊는다.
/// 이미 claim 한 결과는 그대로 보낸다. 끊지 못하면 결과가 엉뚱한 대화로 갈 수 있으므로 지시를 보내지 않는다.
pub(crate) fn release_codex_route(
    returns: &Returns,
    child_surface: u32,
    tr: &Translator,
) -> Result<(), IpcMethodError> {
    match returns.release_child(child_surface) {
        Ok(true) => {
            tracing::info!(
                "claude codex return: s{child_surface} got a non-Codex instruction; its Codex return route was released"
            );
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(e) => Err(IpcMethodError::new(tr.t_args(
            "claude.codex.route_not_released",
            &[&child_surface.to_string(), &e],
        ))),
    }
}

/// Codex 부모의 결과 경로를 저장할 수 있어야 한다. 저장할 수 없으면 자식을 만들거나 메시지를 보내기 전에 거절한다.
fn require_returns(returns: &Returns, tr: &Translator) -> Result<(), IpcMethodError> {
    if returns.is_available() {
        Ok(())
    } else {
        Err(IpcMethodError::new(tr.t("claude.codex.no_data_dir")))
    }
}

/// 부모를 정하고, 일반 호출이면 기존처럼 부모 surface 를 요구한다.
fn require_parent<H: HostCall>(
    host: &H,
    returns: &Returns,
    params: &Value,
    tr: &Translator,
) -> Result<ParentTarget, IpcMethodError> {
    let target = parent_target(host, params, tr)?;
    match &target {
        ParentTarget::Legacy(None) => Err(IpcMethodError::invalid_params(
            tr.t("claude.params.missing_surface_id"),
        )),
        ParentTarget::Codex(_) => require_returns(returns, tr).map(|()| target),
        ParentTarget::Legacy(Some(_)) => Ok(target),
    }
}

/// 응답에 싣는 부모. 물리 부모는 기존 `parent_surface_id`, 외부 부모는 `parent_external` 이다.
pub(crate) fn describe_parent(target: &ParentTarget, out: &mut serde_json::Map<String, Value>) {
    match target {
        ParentTarget::Legacy(Some(id)) => {
            out.insert("parent_surface_id".into(), json!(id));
        }
        ParentTarget::Legacy(None) => {}
        ParentTarget::Codex(resolved) => resolved.parent.describe(out),
    }
}

/// 물리 부모 surface. 외부 부모에는 없다.
fn parent_surface(target: &ParentTarget) -> Option<u32> {
    match target {
        ParentTarget::Legacy(id) => *id,
        ParentTarget::Codex(resolved) => match resolved.parent {
            TerminalParent::Surface(id) => Some(id),
            TerminalParent::External(_) => None,
        },
    }
}

/// 호스트에 자식 생성(또는 재실행)을 맡기고, Codex 부모면 Claude 를 띄우기 전에 결과 경로를 저장한다.
/// 같은 워커가 처리할 자식의 첫 hook 보다 경로가 먼저 있어야 한다. 저장하지 못하면 만든 자식 id 와 함께 실패한다.
fn create_child<H: HostCall>(
    returns: &Returns,
    host: &H,
    method: &str,
    request: serde_json::Map<String, Value>,
    target: &ParentTarget,
    with_prompt: bool,
    tr: &Translator,
) -> Result<(Value, u32), IpcMethodError> {
    let missing_key = if method == "terminal.respawn" {
        "claude.respawn.missing_child_surface_id"
    } else {
        "claude.spawn.missing_child_surface_id"
    };
    let resp = host_call(host, method, Value::Object(request))?;
    let child_surface_id = resp
        .get("child_surface_id")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| IpcMethodError::new(tr.t_fmt(missing_key, &resp.to_string())))?;
    match target {
        ParentTarget::Codex(resolved) => {
            returns
                .bind_new_child(child_surface_id, &Binding::new(resolved), with_prompt)
                .map_err(|e| {
                    IpcMethodError::new(tr.t_args(
                        "claude.codex.child_route_not_saved",
                        &[&child_surface_id.to_string(), &e],
                    ))
                })?;
        }
        // 일반 호출의 respawn 은 Claude 를 띄우기 전에 이전 Codex 경로를 끊는다. 새 자식(spawn)에는 끊을 경로가 없다.
        ParentTarget::Legacy(_) if method == "terminal.respawn" => {
            release_codex_route(returns, child_surface_id, tr)?;
        }
        ParentTarget::Legacy(_) => {}
    }
    Ok((resp, child_surface_id))
}

#[cfg(test)]
use crate::notifications::*;
pub(crate) use crate::notifications::{
    handle_notify_done, handle_notify_error, handle_notify_stop_failure, register_notify_hooks,
};

/// 새 워크스페이스에 Claude를 실행하고 독립 surface로 오류 감시에 등록한다.
pub(crate) fn handle_launch(
    scanner: &Arc<Mutex<ErrorScanner>>,
    host: &HostHandle,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let workspace_name = params
        .get("workspace")
        .and_then(|v| v.as_str())
        .unwrap_or("claude")
        .to_string();
    let directory = params
        .get("directory")
        .and_then(|v| v.as_str())
        .map(String::from);
    let task = params
        .get("task")
        .and_then(|v| v.as_str())
        .map(String::from);
    let profile_file = resolve_profile_file_param(data_dir, params, tr)?;
    let permission_mode = resolve_permission_mode(host, params, profile_file.as_deref(), tr)?;

    // CLI에서 정규화한 작업 디렉터리를 PTY 생성에 전달한다.
    let mut ws_params = json!({
        "type": "terminal",
        "name": workspace_name,
    });
    if let Some(dir) = directory.as_deref() {
        ws_params["cwd"] = Value::String(dir.to_string());
    }
    let ws_resp = host
        .call("workspace.create", ws_params)
        .map_err(IpcMethodError::from)?;

    let workspace_id = ws_resp
        .get("id")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| IpcMethodError::new(tr.t("claude.launch.workspace_create_missing_id")))?;
    let surface_id = ws_resp
        .get("surface_id")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    if let Some(sid) = surface_id {
        let cmd = build_launch_command(
            task.as_deref(),
            profile_file.as_deref(),
            permission_mode.as_deref(),
        );
        if let Err(e) = host.call(
            "surface.send",
            json!({ "surface_id": sid, "text": format!("{cmd}\r") }),
        ) {
            tracing::warn!("surface.send (launch) failed: {e}");
        }

        crate::retrack::enable_and_record(host, scanner, sid, ScanTarget::TopLevel);
        crate::stop_pairing::record_settings_file(host, sid, profile_file.as_deref());
    }

    Ok(json!({
        "workspace_id": workspace_id,
        "workspace_name": workspace_name,
        "surface_id": surface_id,
    }))
}

/// 작업 인자를 이스케이프하고 settings 경로·권한 모드를 붙인 Claude 실행 명령을 만든다.
pub(crate) fn build_launch_command(
    task: Option<&str>,
    profile_file: Option<&str>,
    permission_mode: Option<&str>,
) -> String {
    let mut cmd = "claude".to_string();
    if let Some(t) = task {
        let escaped = crate::session_options::posix_single_quote(t);
        cmd.push_str(&format!(" --task {escaped}"));
    }
    if let Some(path) = profile_file {
        cmd.push_str(&format!(
            " --settings {}",
            crate::session_options::quote_path(path)
        ));
    }
    cmd.push_str(&permission_mode_flag(permission_mode));
    cmd
}

/// 호스트 terminal.respawn으로 자식을 준비한 뒤 Claude 실행 명령을 보낸다.
/// 같은 surface id를 다시 쓰므로 오류 감시의 중복 알림 상태도 초기화한다.
pub(crate) fn handle_respawn(
    scanner: &Arc<Mutex<ErrorScanner>>,
    returns: &Returns,
    host: &HostHandle,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let target = require_parent(host, returns, params, tr)?;
    let child_index = require_child_index(params, tr)?;
    let prompt = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(String::from);
    let profile_file = resolve_profile_file_param(data_dir, params, tr)?;
    let permission_mode = resolve_permission_mode(host, params, profile_file.as_deref(), tr)?;

    // 호스트가 cwd에 따른 PTY 교체와 자식 메타데이터·idle 상태를 처리한다.
    let mut rp = forward(params, &["cwd", "role", "nickname"]);
    returns.insert_parent(host, &target, params, &mut rp, "surface", None, tr)?;
    rp.insert("child".into(), json!(child_index));
    let (_, child_surface_id) = create_child(
        returns,
        host,
        "terminal.respawn",
        rp,
        &target,
        prompt.is_some(),
        tr,
    )?;

    start_claude_in_surface(
        host,
        child_surface_id,
        prompt.as_deref(),
        profile_file.as_deref(),
        permission_mode.as_deref(),
    );

    // 재실행 뒤 같은 오류도 알릴 수 있도록 중복 기록을 지운다.
    crate::retrack::enable_and_record(host, scanner, child_surface_id, ScanTarget::Child);
    crate::error_scan::lock_scanner(scanner).reset_dedupe(child_surface_id);

    let mut out = serde_json::Map::new();
    out.insert("child_surface_id".into(), json!(child_surface_id));
    out.insert("child_index".into(), json!(child_index));
    describe_parent(&target, &mut out);
    Ok(Value::Object(out))
}

/// 자식 인덱스를 surface id로 바꿔 같은 reboot 경로로 프로필을 적용한다.
/// 재시작 중복 검사도 공유한다. 부모가 아닌 자식을 재시작하고 부모에게 완료 알림을 등록한다.
pub(crate) fn handle_child_profile(
    inflight: &Arc<Mutex<HashSet<u32>>>,
    host: &HostHandle,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let parent_surface_id = require_target_surface(params, tr)?;
    // child는 필수이며 생략을 호출자 자신으로 해석하지 않는다.
    let child_index = require_child_index(params, tr)?;
    let child_surface_id = resolve_child_surface_id(host, parent_surface_id, child_index, tr)?;

    let resp = reboot_surface(inflight, host, child_surface_id, params, data_dir, tr)?;

    register_notify_hooks(host, parent_surface_id, child_surface_id, "child-profile");

    Ok(json!({
        "child_surface_id": child_surface_id,
        "child_index": child_index,
        "parent_surface_id": parent_surface_id,
        "session_id": resp.get("session_id").cloned().unwrap_or(Value::Null),
        "reboot_in_secs": resp.get("reboot_in_secs").cloned().unwrap_or(Value::Null),
    }))
}

/// 자식 surface에서 Claude를 실행한다. 위치와 관측용 agent id를 환경 변수로 전달하고,
/// 세션 토큰 발급이 성공했으면 인증 토큰도 붙인다.
pub(crate) fn start_claude_in_surface(
    host: &HostHandle,
    surface_id: u32,
    prompt: Option<&str>,
    profile_file: Option<&str>,
    permission_mode: Option<&str>,
) {
    if let Err(e) = start_claude_with_options(
        host,
        surface_id,
        prompt,
        profile_file,
        permission_mode,
        &crate::session_options::SessionOptions::default(),
    ) {
        tracing::warn!("claude start failed: {}", e.message);
    }
}

pub(crate) fn start_claude_with_options<H: HostCall>(
    host: &H,
    surface_id: u32,
    prompt: Option<&str>,
    profile_file: Option<&str>,
    permission_mode: Option<&str>,
    options: &crate::session_options::SessionOptions,
) -> Result<(), IpcMethodError> {
    let session_id = uuid::Uuid::new_v4().to_string();
    options.record(host, surface_id, &session_id)?;
    // Stop 이 부착 게이트 수를 셀 수 있도록 붙인 settings 경로를 남긴다.
    crate::stop_pairing::record_settings_file(host, surface_id, profile_file);
    record_launch_prompt(host, surface_id, prompt.is_some());
    let agent_id = format!("claude_s{surface_id}");
    let session_token = issue_session_token(host, &agent_id);
    let agent_prefix = match session_token {
        Some(tok) => format!(
            "TASTY_SURFACE_ID={surface_id} TASTY_AGENT_ID={agent_id} TASTY_SESSION_TOKEN={tok} "
        ),
        None => format!("TASTY_SURFACE_ID={surface_id} TASTY_AGENT_ID={agent_id} "),
    };
    let mut text = match prompt {
        Some(p) => claude_launch_command_with_prompt(
            surface_id,
            &agent_prefix,
            p,
            profile_file,
            permission_mode,
        ),
        None => {
            let settings_flag = match profile_file {
                Some(path) => format!(" --settings {}", crate::session_options::quote_path(path)),
                None => String::new(),
            };
            format!(
                "{agent_prefix}claude{settings_flag}{}\r",
                permission_mode_flag(permission_mode)
            )
        }
    };

    // Add invocation flags before the positional prompt (which may itself start with '-').
    let flags = format!(" --session-id {session_id}{}", options.flags());
    if !flags.is_empty() {
        let at = agent_prefix.len() + "claude".len();
        text.insert_str(at, &flags);
    }
    host.call(
        "surface.send",
        json!({ "surface_id": surface_id, "text": text }),
    )
    .map_err(IpcMethodError::from)?;
    Ok(())
}

/// 프롬프트를 실어 실행하는지 기록한다. SessionStart 가 이 표시로 시작 상태를 정한다.
fn record_launch_prompt<H: HostCall>(host: &H, surface_id: u32, with_prompt: bool) {
    let key = crate::session_start::LAUNCH_PROMPT_META_KEY;
    let result = if with_prompt {
        host.call(
            "surface.meta.set",
            json!({ "surface_id": surface_id, "key": key, "value": "1" }),
        )
    } else {
        host.call(
            "surface.meta.unset",
            json!({ "surface_id": surface_id, "key": key }),
        )
    };
    if let Err(e) = result {
        tracing::warn!("claude s{surface_id}: failed to record the launch prompt mark: {e}");
    }
}

/// 다른 플러그인의 프롬프트 파일과 구분할 접두어. 저장·정리 규칙은 공용 헬퍼를 사용한다.
const PROMPT_FILE_PREFIX: &str = "tasty-prompt-";
/// 프롬프트를 임시 파일에 쓰고 POSIX 셸의 cat 치환으로 전달할 명령을 만든다.
/// 파일 쓰기 실패도 기록 후 명령 생성을 계속한다. settings·권한 모드는 본문 앞에 둔다.
/// 자식이 읽었는지 확인할 수 없어 즉시 삭제하지 않고 다음 생성 때 TTL로 정리한다.
/// 공용 write는 Unix에서 새 파일에 0600 모드를 지정한다.
fn claude_launch_command_with_prompt(
    surface_id: u32,
    agent_prefix: &str,
    prompt: &str,
    profile_file: Option<&str>,
    permission_mode: Option<&str>,
) -> String {
    let temp_dir = std::env::temp_dir();
    prompt_file::sweep_stale(&temp_dir, PROMPT_FILE_PREFIX);
    let prompt_path = prompt_file::path_for(&temp_dir, PROMPT_FILE_PREFIX, surface_id);
    if let Err(e) = prompt_file::write(&prompt_path, prompt) {
        tracing::warn!("Failed to write prompt file: {e}");
    }
    let settings_flag = match profile_file {
        Some(path) => format!("--settings {} ", crate::session_options::quote_path(path)),
        None => String::new(),
    };
    let mode_flag = match permission_mode {
        Some(m) => format!("--permission-mode {m} "),
        None => String::new(),
    };
    format!(
        "{agent_prefix}claude {settings_flag}{mode_flag}\"$(cat '{}')\"\r",
        prompt_path.display()
    )
}

/// 자식의 세션 토큰을 발급한다. 실패하면 토큰 없이 실행하도록 None을 반환한다.
/// 일반 권한은 플러그인의 허용 범위에서 발급한다. 자기 namespace인 claude는 별도 보유 없이,
/// codex 호출 권한은 매니페스트로 받은 것을 전달한다.
pub(crate) fn issue_session_token<H: HostCall>(host: &H, agent_id: &str) -> Option<String> {
    let resp = match host.call(
        "session.issue",
        json!({
            "agent_id": agent_id,
            "permissions": [
                "surface.read",
                "surface.write",
                "terminal.write",
                "terminal.read",
                "notification",
                "telemetry",
                "agent",
                "ipc.invoke:claude",
                "ipc.invoke:codex",
            ],
        }),
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("session.issue failed for child {agent_id}: {e}");
            return None;
        }
    };
    let token = resp.get("token").and_then(|v| v.as_str()).map(String::from);
    if token.is_none() {
        tracing::warn!("session.issue returned no 'token' field for child {agent_id}");
    }
    token
}

/// terminal.spawn으로 자식 surface를 만든 뒤 Claude 명령을 보내고 오류 감시에 등록한다.
/// 인덱스·탭·페인·점유 상태는 호스트가 관리한다.
pub(crate) fn handle_spawn<H: HostCall>(
    scanner: &Arc<Mutex<ErrorScanner>>,
    returns: &Returns,
    host: &H,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    if let Some(origin) = AttachOrigin::of(params) {
        return crate::remote::serve_spawn(scanner, returns, host, params, &origin, data_dir, tr);
    }
    let options = crate::session_options::SessionOptions::parse(params, tr)?;
    let target = require_parent(host, returns, params, tr)?;
    if let Some(anchor) = crate::remote::spawn_anchor(host, params, tr)? {
        return crate::remote::spawn(returns, host, params, &target, anchor, tr);
    }
    let prompt = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(String::from);
    let profile_file = resolve_profile_file_param(data_dir, params, tr)?;
    let permission_mode = resolve_permission_mode(host, params, profile_file.as_deref(), tr)?;

    let mut sp = forward(params, &["workspace", "pane", "cwd", "role", "nickname"]);
    target.insert(&mut sp, "parent");
    let (resp, child_surface_id) = create_child(
        returns,
        host,
        "terminal.spawn",
        sp,
        &target,
        prompt.is_some(),
        tr,
    )?;

    start_claude_with_options(
        host,
        child_surface_id,
        prompt.as_deref(),
        profile_file.as_deref(),
        permission_mode.as_deref(),
        &options,
    )?;

    // 자식 관계로 추적해 surface가 남는 release도 정리할 수 있게 한다.
    crate::retrack::enable_and_record(host, scanner, child_surface_id, ScanTarget::Child);

    // 호스트 응답에 부모를 추가하고 나머지 필드는 그대로 전달한다.
    let mut out = resp;
    if let Some(obj) = out.as_object_mut() {
        describe_parent(&target, obj);
        obj.insert("requested_session_options".into(), options.json());
    }

    // 자식 수 경고는 생성 성공을 바꾸지 않는다. 자식 수 집계는 물리 부모 surface 로만 조회한다.
    if let Some(parent_surface_id) = parent_surface(&target)
        && let Some(warning) = compute_spawn_warning(host, parent_surface_id, tr)
        && let Some(obj) = out.as_object_mut()
    {
        obj.insert("warning".into(), json!(warning));
    }

    // 부모가 자식 상태 전환을 알 수 있도록 완료 훅을 등록한다. Codex 부모는 확정된 턴 결과를 받으므로 등록하지 않는다.
    if let ParentTarget::Legacy(Some(parent_surface_id)) = target {
        register_notify_hooks(host, parent_surface_id, child_surface_id, "spawn");
    }

    Ok(out)
}

/// 공용 자식 상태 집계로 생성 후 경고를 만든다. 조회 실패는 경고 생략으로 처리한다.
fn compute_spawn_warning<H: HostCall>(
    host: &H,
    parent_surface_id: u32,
    tr: &Translator,
) -> Option<String> {
    let c = spawn_census(host, parent_surface_id)?;
    build_spawn_warning(tr, c.total, &c.idle, &c.stale, c.threshold)
}

/// idle과 confirmed stale을 별도로 안내한다.
/// 전자는 완료 보고가 있고 후자는 보고 없이 전경이 셸로 돌아온 상태다.
fn build_spawn_warning(
    tr: &Translator,
    total: usize,
    idle_indices: &[u64],
    stale_indices: &[u64],
    threshold: f64,
) -> Option<String> {
    if (total as f64) <= threshold {
        return None;
    }
    let mut msg = tr.t_args(
        "claude.spawn.warning_threshold",
        &[&total.to_string(), &threshold.to_string()],
    );
    if !idle_indices.is_empty() {
        msg.push_str(&tr.t_fmt(
            "claude.spawn.warning_idle_children",
            &join_indices(idle_indices),
        ));
    }
    if !stale_indices.is_empty() {
        msg.push_str(&tr.t_fmt(
            "claude.spawn.warning_stale_children",
            &join_indices(stale_indices),
        ));
    }
    Some(msg)
}

/// 자식 surface 의 parent 를 조회 — 호스트 `terminal.parent` 로 위임.
pub(crate) fn handle_parent(
    host: &HostHandle,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let surface = require_target_surface(params, tr)?;
    host_call(host, "terminal.parent", json!({ "surface": surface }))
}

/// 완료 전략은 자기 namespace의 메서드를 사용해야 하므로 terminal.state를 claude.state로 제공한다.
pub(crate) fn handle_state(
    host: &HostHandle,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let surface = require_target_surface(params, tr)?;
    host_call(host, "terminal.state", json!({ "surface": surface }))
}

#[cfg(test)]
// 테스트의 정리 작업에서 결과를 의도적으로 무시한다.
#[allow(clippy::let_underscore_must_use)]
mod tests {
    use super::*;

    /// 어느 대상 키로 입력해도 호스트 params의 surface에 전달돼야 한다.
    #[test]
    fn the_target_surface_reaches_the_host_under_either_name() {
        let tr = test_translator();
        for params in [json!({ "surface": 7 }), json!({ "surface_id": 7 })] {
            let mut out = serde_json::Map::new();
            put_target_surface(&mut out, &params, &tr).expect("두 대상 키를 모두 받아야 한다");
            assert_eq!(
                out.get("surface"),
                Some(&json!(7)),
                "{params} 에서 대상이 호스트로 안 실렸다 — 유일-parent 폴백에 떨어진다"
            );
        }
    }

    /// 대상을 생략한 요청에는 값을 넣지 않아 호스트의 기본 대상 선택을 유지한다.
    #[test]
    fn no_target_named_stays_no_target_sent() {
        let tr = test_translator();
        let mut out = serde_json::Map::new();
        put_target_surface(&mut out, &json!({ "child_index": 0 }), &tr).expect("없어도 성공");
        assert!(
            out.is_empty(),
            "대상을 안 준 호출에 값을 지어냈다 — 폴백이 사라진다: {out:?}"
        );
    }

    /// 두 대상 키가 서로 다르면 거절한다.
    #[test]
    fn two_names_with_different_values_are_refused_not_picked() {
        let tr = test_translator();
        let e = optional_target_surface(&json!({ "surface": 1, "surface_id": 2 }), &tr)
            .expect_err("서로 다른 두 대상을 조용히 하나로 고르면 안 된다");
        let msg = format!("{e:?}");
        assert!(
            msg.contains('1') && msg.contains('2'),
            "충돌한 두 값이 오류 안내에 없다: {msg}"
        );
        // 같은 값이면 부딪힌 것이 아니다.
        assert_eq!(
            optional_target_surface(&json!({ "surface": 3, "surface_id": 3 }), &tr).unwrap(),
            Some(3),
            "CLI 가 두 키를 같은 값으로 채워 보내는 형태를 막으면 안 된다"
        );
    }

    /// 실제 자원을 만들지 않고 누락·정상·형식 오류·범위 초과를 검사한다.
    #[test]
    fn required_u32_params_separate_absent_from_malformed_and_refuse_to_truncate() {
        let tr = test_translator();

        // ① 키 없음.
        assert!(require_target_surface(&json!({}), &tr).is_err());
        assert!(require_child_index(&json!({}), &tr).is_err());

        // ② 정상 — 경계값이 그대로 통과한다.
        assert_eq!(
            require_target_surface(&json!({ "surface_id": 0 }), &tr).unwrap(),
            0
        );
        assert_eq!(
            require_target_surface(&json!({ "surface_id": u32::MAX }), &tr).unwrap(),
            u32::MAX
        );

        // ③ 숫자가 아니다 — 거부하고, "missing" 이라고 답하지 않는다.
        let e = require_target_surface(&json!({ "surface_id": "conductor" }), &tr).unwrap_err();
        let m = format!("{e:?}");
        assert!(m.contains("32 bits"), "{m}");
        assert!(!m.contains("Missing"), "값이 왔는데 없다고 답한다: {m}");

        // 범위를 넘는 값을 잘라 다른 id로 해석해서는 안 된다.
        for over in [
            u64::from(u32::MAX) + 1,
            u64::from(u32::MAX) + 2,
            5_000_000_000,
        ] {
            assert!(
                require_target_surface(&json!({ "surface_id": over }), &tr).is_err(),
                "범위를 넘는 {over}를 거절해야 한다"
            );
            assert!(require_child_index(&json!({ "child_index": over }), &tr).is_err());
        }

        assert!(require_target_surface(&json!({ "surface_id": -1 }), &tr).is_err());
    }

    /// null도 인자 부재로 처리한다.
    #[test]
    fn a_null_slot_reads_as_absent_not_as_a_malformed_value() {
        let tr = test_translator();
        let e = require_target_surface(&json!({ "surface_id": Value::Null }), &tr).unwrap_err();
        assert!(format!("{e:?}").contains("Missing"), "{e:?}");
    }

    /// 실제 영어 카탈로그로 오류 안내를 검사한다.
    fn test_translator() -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, "en")
    }

    fn test_translator_for(code: &str) -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, code)
    }

    /// 잘못된 인자 이름을 내장 언어 모두에서 알려야 한다.
    #[test]
    fn a_malformed_target_surface_names_which_of_the_two_keys_was_wrong() {
        for locale in tasty_i18n::BUILTIN_CODES {
            let tr = test_translator_for(locale);
            let by_surface = optional_target_surface(&json!({ "surface": "x" }), &tr)
                .expect_err("문자열은 surface id 가 아니다")
                .message;
            let by_surface_id = optional_target_surface(&json!({ "surface_id": "x" }), &tr)
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

    #[test]
    fn build_spawn_warning_none_below_threshold() {
        let tr = test_translator();
        assert_eq!(build_spawn_warning(&tr, 3, &[], &[], 6.0), None);
    }

    #[test]
    fn build_spawn_warning_above_threshold_lists_idle_and_mentions_respawn() {
        let tr = test_translator();
        let w = build_spawn_warning(&tr, 7, &[2, 5], &[], 6.0).unwrap();
        assert!(w.contains("respawn"));
        assert!(w.contains('2') && w.contains('5'));
    }

    #[test]
    fn build_spawn_warning_above_threshold_no_idle_has_no_respawn_word() {
        let tr = test_translator();
        let w = build_spawn_warning(&tr, 7, &[], &[], 6.0).unwrap();
        assert!(!w.contains("respawn"));
    }

    #[test]
    fn build_spawn_warning_respects_custom_threshold() {
        let tr = test_translator();
        assert_eq!(build_spawn_warning(&tr, 3, &[], &[], 6.0), None);
        assert!(build_spawn_warning(&tr, 4, &[], &[], 3.0).is_some());
    }

    /// confirmed stale도 재실행 후보로 안내한다.
    #[test]
    fn build_spawn_warning_lists_stale_children_as_respawn_candidates() {
        let tr = test_translator();
        let w = build_spawn_warning(&tr, 7, &[], &[3], 6.0).unwrap();
        assert!(w.contains("respawn"), "{w}");
        assert!(w.contains('3'), "{w}");
    }

    /// 완료 보고가 없었던 stale을 idle과 같은 문구로 안내하지 않는다.
    #[test]
    fn build_spawn_warning_separates_stale_wording_from_idle() {
        let tr = test_translator();
        let idle_only = build_spawn_warning(&tr, 7, &[2], &[], 6.0).unwrap();
        let stale_only = build_spawn_warning(&tr, 7, &[], &[3], 6.0).unwrap();
        assert!(idle_only.contains("Idle children"), "{idle_only}");
        assert!(!stale_only.contains("Idle children"), "{stale_only}");
        assert!(
            stale_only.contains("never reported completion"),
            "{stale_only}"
        );

        let both = build_spawn_warning(&tr, 7, &[2], &[3], 6.0).unwrap();
        assert!(both.contains("Idle children"), "{both}");
        assert!(both.contains("never reported completion"), "{both}");
    }

    #[test]
    fn build_launch_command_no_task() {
        assert_eq!(build_launch_command(None, None, None), "claude");
    }

    #[test]
    fn build_launch_command_with_simple_task() {
        assert_eq!(
            build_launch_command(Some("fix"), None, None),
            "claude --task fix"
        );
    }

    #[test]
    fn build_launch_command_with_spaces_gets_escaped() {
        let out = build_launch_command(Some("fix the bug"), None, None);
        assert!(out.starts_with("claude --task "), "prefix wrong: {out}");
        assert!(out.contains("fix the bug"), "task body missing: {out}");
        assert_ne!(out, "claude --task fix the bug", "must be escaped");
    }

    /// Every OS gets the POSIX single-quote rule, whatever `MSYSTEM` the plugin inherited.
    #[test]
    fn build_launch_command_quotes_the_task_for_posix_shells() {
        assert_eq!(
            build_launch_command(Some("it's $HOME & !x; a|b"), None, None),
            r"claude --task 'it'\''s $HOME & '\!'x; a|b'"
        );
    }

    /// Linux·macOS keep the double-quoted path; Git Bash on Windows gets the POSIX single-quote
    /// rule, which leaves a path of plain characters bare.
    const PROFILE_ARG: &str = if cfg!(windows) {
        "/home/user/profile.json"
    } else {
        "\"/home/user/profile.json\""
    };

    #[test]
    fn build_launch_command_with_profile_appends_quoted_settings_path() {
        assert_eq!(
            build_launch_command(None, Some("/home/user/profile.json"), None),
            format!("claude --settings {PROFILE_ARG}")
        );
    }

    #[test]
    fn build_launch_command_with_task_and_profile_appends_both() {
        assert_eq!(
            build_launch_command(Some("fix"), Some("/home/user/profile.json"), None),
            format!("claude --task fix --settings {PROFILE_ARG}")
        );
    }

    /// 권한 기본 설정만 반환하는 시험용 호스트.
    struct SettingHost(Option<&'static str>);

    impl HostCall for SettingHost {
        fn call(
            &self,
            method: &str,
            _params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            match (method, self.0) {
                ("settings.get_plugin_setting", Some(v)) => Ok(json!({ "value": v })),
                ("settings.get_plugin_setting", None) => Ok(json!({})),
                _ => Ok(json!({})),
            }
        }
    }

    #[test]
    fn build_launch_command_with_permission_mode_appends_flag() {
        assert_eq!(
            build_launch_command(None, None, Some("acceptEdits")),
            "claude --permission-mode acceptEdits"
        );
    }

    /// 두 플래그가 함께 오면 순서가 고정된다 — `--settings` 먼저, 정책이 뒤.
    #[test]
    fn build_launch_command_with_profile_and_permission_mode_fixes_order() {
        assert_eq!(
            build_launch_command(None, Some("/home/u/p.json"), Some("plan")),
            if cfg!(windows) {
                "claude --settings /home/u/p.json --permission-mode plan"
            } else {
                "claude --settings \"/home/u/p.json\" --permission-mode plan"
            }
        );
    }

    #[test]
    fn claude_launch_command_with_prompt_puts_mode_before_the_positional_prompt() {
        let out = claude_launch_command_with_prompt(
            3,
            "TASTY_SURFACE_ID=3 ",
            "hello",
            None,
            Some("dontAsk"),
        );
        assert!(
            out.starts_with("TASTY_SURFACE_ID=3 claude --permission-mode dontAsk \"$(cat '"),
            "got {out}"
        );
    }

    #[test]
    fn resolve_permission_mode_defaults_to_no_flag() {
        // 인자와 플러그인 설정이 모두 없으면 Claude Code의 설정을 따른다.
        let host = SettingHost(None);
        assert_eq!(
            resolve_permission_mode(&host, &json!({}), None, &test_translator()).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_permission_mode_takes_the_explicit_param() {
        let host = SettingHost(None);
        assert_eq!(
            resolve_permission_mode(
                &host,
                &json!({ "permission_mode": "plan" }),
                None,
                &test_translator()
            )
            .unwrap(),
            Some("plan".to_string())
        );
    }

    #[test]
    fn resolve_permission_mode_falls_back_to_the_plugin_setting() {
        let host = SettingHost(Some("acceptEdits"));
        assert_eq!(
            resolve_permission_mode(&host, &json!({}), None, &test_translator()).unwrap(),
            Some("acceptEdits".to_string())
        );
    }

    /// 호출별 params 가 설정 기본값을 이 호출에 한해 덮는다(docs/plugins/claude/index.md#승인-정책---permission-mode).
    #[test]
    fn explicit_param_overrides_the_plugin_setting() {
        let host = SettingHost(Some("acceptEdits"));
        assert_eq!(
            resolve_permission_mode(
                &host,
                &json!({ "permission_mode": "plan" }),
                None,
                &test_translator()
            )
            .unwrap(),
            Some("plan".to_string())
        );
    }

    /// `inherit` 은 "플래그를 안 붙인다" 와 같은 뜻이다.
    #[test]
    fn inherit_setting_means_no_flag() {
        let host = SettingHost(Some("inherit"));
        assert_eq!(
            resolve_permission_mode(&host, &json!({}), None, &test_translator()).unwrap(),
            None
        );
    }

    /// 설정에 모르는 값이 들어 있어도 기동을 막지 않는다 — 무시하고 미부착.
    #[test]
    fn unknown_setting_value_is_ignored_not_fatal() {
        let host = SettingHost(Some("yolo"));
        assert_eq!(
            resolve_permission_mode(&host, &json!({}), None, &test_translator()).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_permission_mode_rejects_an_unknown_value() {
        let host = SettingHost(None);
        let err = resolve_permission_mode(
            &host,
            &json!({ "permission_mode": "yolo" }),
            None,
            &test_translator(),
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("yolo"), "got {err:?}");
    }

    #[test]
    fn resolve_permission_mode_rejects_a_profile_that_sets_the_same_axis() {
        // docs/plugins/claude/index.md#승인-정책---permission-mode — 어느 쪽이 이기는지 조용히 정하지 않는다.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        std::fs::write(&path, r#"{"permissions":{"defaultMode":"plan"}}"#).unwrap();
        let host = SettingHost(None);
        let err = resolve_permission_mode(
            &host,
            &json!({ "permission_mode": "acceptEdits" }),
            path.to_str(),
            &test_translator(),
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("defaultMode"), "got {err:?}");
    }

    /// allow·deny는 모드가 아닌 규칙이므로 권한 모드와 함께 지정할 수 있다.
    #[test]
    fn resolve_permission_mode_allows_a_profile_that_only_lists_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        std::fs::write(&path, r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#).unwrap();
        let host = SettingHost(None);
        assert_eq!(
            resolve_permission_mode(
                &host,
                &json!({ "permission_mode": "acceptEdits" }),
                path.to_str(),
                &test_translator()
            )
            .unwrap(),
            Some("acceptEdits".to_string())
        );
    }

    #[test]
    fn claude_launch_command_with_prompt_no_profile_unchanged() {
        let out = claude_launch_command_with_prompt(1, "TASTY_SURFACE_ID=1 ", "hello", None, None);
        assert!(
            out.starts_with("TASTY_SURFACE_ID=1 claude \"$(cat '"),
            "got {out}"
        );
        assert!(!out.contains("--settings"), "got {out}");
    }

    #[test]
    fn claude_launch_command_with_prompt_and_profile_prepends_settings() {
        let out = claude_launch_command_with_prompt(
            2,
            "TASTY_SURFACE_ID=2 ",
            "hello",
            Some("/home/user/profile.json"),
            None,
        );
        assert!(
            out.starts_with(&format!(
                "TASTY_SURFACE_ID=2 claude --settings {PROFILE_ARG} \"$(cat '"
            )),
            "got {out}"
        );
    }

    // 실제 한국어 번역으로 상태 변경 안내를 검사한다.
    fn test_translator_ko() -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, "ko")
    }

    #[test]
    fn notify_done_message_describes_state_change() {
        let tr = test_translator_ko();
        let msg = notify_done_message(&tr, "spawn", 42);
        assert!(
            msg.contains("상태 변경"),
            "상태 변경을 알리는 문구여야 한다: {msg}"
        );
        assert!(msg.contains("42"), "target surface 번호 누락: {msg}");
        assert!(msg.contains("spawn"), "호출 방식 정보 누락: {msg}");
    }

    #[test]
    fn notify_done_message_does_not_read_as_command_itself_completing() {
        // spawn/tell 명령 자체가 완료된 것으로 읽히는 문장으로 시작하지 않아야 한다.
        let tr = test_translator_ko();
        for command_name in ["spawn", "tell"] {
            let msg = notify_done_message(&tr, command_name, 7);
            assert!(
                !msg.starts_with(&format!("{command_name} 완료")),
                "명령 자체의 완료로 읽히는 안내다: {msg}"
            );
        }
    }

    #[test]
    fn stop_failure_hint_is_appended_after_the_completion_text() {
        let tr = test_translator_ko();
        let base = notify_done_message(&tr, "spawn", 42);
        let msg =
            crate::notifications::with_stop_failure_hint(&tr, base.clone(), Some("overloaded"));
        assert!(
            msg.starts_with(&base),
            "완료 문구 앞부분은 그대로여야 함: {msg}"
        );
        assert!(msg.contains("overloaded"), "에러 종류 누락: {msg}");
        assert!(
            msg.contains("API 에러"),
            "에러로 끝났음이 드러나야 함: {msg}"
        );
    }

    #[test]
    fn no_stop_failure_record_leaves_the_completion_text_unchanged() {
        let tr = test_translator();
        let base = notify_done_message(&tr, "tell", 7);
        for error in [None, Some("")] {
            assert_eq!(
                crate::notifications::with_stop_failure_hint(&tr, base.clone(), error),
                base
            );
        }
    }

    #[test]
    fn require_child_index_missing_is_invalid_params() {
        let tr = test_translator();
        let err = require_child_index(&json!({ "surface_id": 1 }), &tr).unwrap_err();
        assert_eq!(err.code, -32602);
    }

    // 같은 명령으로 등록한 완료 훅의 정리와 재등록.

    use std::cell::RefCell;

    struct MockHook {
        id: u64,
        surface_id: u32,
        command: String,
        event: String,
        /// once=false 인 상시 hook 은 발화해도 남는다(에러 정지 알림 hook).
        once: bool,
    }

    /// 훅 등록·정리와 메시지 전달을 모의 실행한다.
    /// surface.locate는 기본적으로 exists=false이며 mark_alive/mark_dead로 바꾼다.
    struct MockHost {
        hooks: RefCell<Vec<MockHook>>,
        next_id: RefCell<u64>,
        alive: RefCell<std::collections::HashSet<u32>>,
        /// terminal.children 원본 배열. 변환 전이므로 surface_id를 사용한다.
        children: RefCell<Vec<Value>>,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                hooks: RefCell::new(Vec::new()),
                next_id: RefCell::new(1),
                alive: RefCell::new(std::collections::HashSet::new()),
                children: RefCell::new(Vec::new()),
            }
        }

        fn set_children(&self, children: Vec<Value>) {
            *self.children.borrow_mut() = children;
        }

        /// 매칭되는 once 훅을 제거하고 해당 개수를 반환한다. 상시 훅은 남긴다.
        fn fire(&self, surface_id: u32, event: &str) -> usize {
            let mut hooks = self.hooks.borrow_mut();
            let fired = hooks
                .iter()
                .filter(|h| h.surface_id == surface_id && h.event == event)
                .count();
            hooks.retain(|h| !(h.surface_id == surface_id && h.event == event && h.once));
            fired
        }

        /// 완료 훅의 command만 모은다. 상시 오류 알림은 수명이 달라 제외한다.
        fn done_commands_on(&self, surface_id: u32) -> Vec<String> {
            self.commands_on(surface_id)
                .into_iter()
                .filter(|c| c.contains("notify-done"))
                .collect()
        }

        /// 특정 event 로 등록된 hook 개수.
        fn hooks_for_event(&self, surface_id: u32, event: &str) -> usize {
            self.hooks
                .borrow()
                .iter()
                .filter(|h| h.surface_id == surface_id && h.event == event)
                .count()
        }

        fn commands_on(&self, surface_id: u32) -> Vec<String> {
            self.hooks
                .borrow()
                .iter()
                .filter(|h| h.surface_id == surface_id)
                .map(|h| h.command.clone())
                .collect()
        }

        /// surface.locate가 exists=true를 반환하도록 한다.
        fn mark_alive(&self, surface_id: u32) {
            self.alive.borrow_mut().insert(surface_id);
        }

        /// surface.locate가 exists=false를 반환하도록 한다.
        fn mark_dead(&self, surface_id: u32) {
            self.alive.borrow_mut().remove(&surface_id);
        }
    }

    impl HostCall for MockHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            match method {
                "hook.set" => {
                    let mut id = self.next_id.borrow_mut();
                    let hid = *id;
                    *id += 1;
                    self.hooks.borrow_mut().push(MockHook {
                        id: hid,
                        surface_id: params["surface_id"].as_u64().unwrap() as u32,
                        command: params["command"].as_str().unwrap().to_string(),
                        event: params["event"].as_str().unwrap().to_string(),
                        once: params["once"].as_bool().unwrap_or(false),
                    });
                    Ok(json!({ "hook_id": hid }))
                }
                "hook.list" => {
                    let sid = params["surface_id"].as_u64().map(|v| v as u32);
                    let arr: Vec<Value> = self
                        .hooks
                        .borrow()
                        .iter()
                        .filter(|h| sid.is_none_or(|s| h.surface_id == s))
                        .map(|h| {
                            json!({ "id": h.id, "surface_id": h.surface_id, "command": h.command, "event": h.event })
                        })
                        .collect();
                    Ok(json!(arr))
                }
                "hook.unset" => {
                    let hid = params["hook_id"].as_u64().unwrap();
                    self.hooks.borrow_mut().retain(|h| h.id != hid);
                    Ok(json!({ "removed": true }))
                }
                "surface.locate" => {
                    let sid = params["surface_id"].as_u64().unwrap() as u32;
                    let exists = self.alive.borrow().contains(&sid);
                    Ok(json!({ "surface_id": sid, "exists": exists }))
                }
                "terminal.children" => Ok(json!({ "children": self.children.borrow().clone() })),
                _ => Ok(json!({})),
            }
        }
    }

    // 호스트 자식 인덱스를 surface id로 해석한다.

    #[test]
    fn child_index_resolves_to_the_hosts_surface_id() {
        let tr = test_translator();
        let host = MockHost::new();
        host.set_children(vec![
            json!({ "index": 0, "surface_id": 7 }),
            json!({ "index": 1, "surface_id": 9 }),
        ]);
        assert_eq!(resolve_child_surface_id(&host, 3, 1, &tr).unwrap(), 9);
        assert_eq!(resolve_child_surface_id(&host, 3, 0, &tr).unwrap(), 7);
    }

    #[test]
    fn child_index_out_of_range_is_invalid_params_and_lists_what_exists() {
        let tr = test_translator();
        let host = MockHost::new();
        host.set_children(vec![
            json!({ "index": 0, "surface_id": 7 }),
            json!({ "index": 1, "surface_id": 9 }),
        ]);
        let err = resolve_child_surface_id(&host, 3, 99, &tr).unwrap_err();
        assert_eq!(err.code, -32602);
        // 오타인지 자식이 죽은 것인지 구분할 수 있게 실제 index 목록이 실린다.
        assert!(err.message.contains("99"), "{}", err.message);
        assert!(err.message.contains("0, 1"), "{}", err.message);
    }

    #[test]
    fn child_index_with_no_children_at_all_is_rejected() {
        let tr = test_translator();
        let host = MockHost::new();
        let err = resolve_child_surface_id(&host, 3, 0, &tr).unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn child_index_does_not_read_the_claude_remapped_field() {
        // 변환된 Claude 응답의 필드명으로 원본 호스트 응답을 읽어서는 안 된다.
        let tr = test_translator();
        let host = MockHost::new();
        host.set_children(vec![json!({ "index": 0, "child_surface_id": 7 })]);
        assert!(resolve_child_surface_id(&host, 3, 0, &tr).is_err());
    }

    #[test]
    fn sibling_cleanup_removes_all_after_one_fires() {
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        register_notify_hooks(&host, caller, target, "tell");
        assert_eq!(host.done_commands_on(target).len(), 3, "3 형제 등록");

        // needs-input 이 fire(once 제거) → 나머지 형제(claude-idle, process-exit) 정리.
        assert_eq!(host.fire(target, "needs-input"), 1);
        let expected = notify_done_command(caller, target, "tell");
        cleanup_sibling_hooks(&host, target, &expected);

        assert!(
            host.done_commands_on(target).is_empty(),
            "같은 완료 그룹의 훅이 모두 제거돼야 한다: {:?}",
            host.done_commands_on(target)
        );
    }

    #[test]
    fn a_new_registration_replaces_the_completion_group_of_the_same_caller() {
        // 같은 부모가 spawn 뒤 tell 하면 tell 그룹이 spawn 그룹을 대신한다. 둘 다 두면 한 번의 상태 변경에
        // 완료 줄이 두 줄 남는다.
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        register_notify_hooks(&host, caller, target, "spawn");
        register_notify_hooks(&host, caller, target, "tell");
        let tell_cmd = notify_done_command(caller, target, "tell");
        assert_eq!(host.done_commands_on(target), vec![tell_cmd.clone(); 3]);
        // 에러 정지 hook 은 명령 이름이 없어 두 번 등록해도 1 개만 남는다.
        assert_eq!(
            host.hooks_for_event(target, crate::error_scan::STALLED_EVENT),
            1,
            "에러 정지 hook 은 중복 등록되지 않아야 함"
        );

        assert_eq!(host.fire(target, "claude-idle"), 1, "완료 줄은 한 줄");
        cleanup_sibling_hooks(&host, target, &tell_cmd);
        assert!(
            host.done_commands_on(target).is_empty(),
            "형제 hook 이 전부 사라져야 함: {:?}",
            host.done_commands_on(target)
        );
    }

    /// tell 을 반복해도 같은 부모·대상의 완료 그룹은 하나다. 다른 부모의 그룹은 건드리지 않는다.
    #[test]
    fn repeated_tells_keep_one_completion_group_per_caller() {
        let host = MockHost::new();
        let (caller, other, target) = (7u32, 8u32, 1650u32);
        for _ in 0..3 {
            register_notify_hooks(&host, caller, target, "tell");
        }
        assert_eq!(host.done_commands_on(target).len(), 3);
        assert_eq!(host.fire(target, "claude-idle"), 1);
        register_notify_hooks(&host, other, target, "tell");
        register_notify_hooks(&host, caller, target, "tell");
        let commands = host.done_commands_on(target);
        assert_eq!(commands.len(), 6, "{commands:?}");
        assert_eq!(host.fire(target, "needs-input"), 2, "부모마다 한 줄");
    }

    // 대상 surface가 남아 있으면 완료 훅을 다시 등록해 이후 상태 전환도 알린다.

    #[test]
    fn handle_notify_done_rearms_when_target_still_alive() {
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        host.mark_alive(target);
        register_notify_hooks(&host, caller, target, "tell");
        assert_eq!(host.done_commands_on(target).len(), 3, "최초 3 형제 등록");

        let tr = test_translator();

        // 1번째 전환: needs-input(되묻기) — child 는 여전히 살아있다.
        assert_eq!(host.fire(target, "needs-input"), 1);
        handle_notify_done(
            &std::sync::Mutex::new(crate::error_scan::ErrorScanner::new()),
            &host,
            &json!({ "caller_surface": caller, "target_surface": target, "command": "tell" }),
            &tr,
        )
        .unwrap();
        assert_eq!(
            host.done_commands_on(target).len(),
            3,
            "대상 surface가 남아 있으면 완료 훅 3개를 다시 등록해야 한다"
        );

        // surface가 남아 있는 두 번째 상태 전환에서도 재등록해야 한다.
        assert_eq!(host.fire(target, "claude-idle"), 1);
        handle_notify_done(
            &std::sync::Mutex::new(crate::error_scan::ErrorScanner::new()),
            &host,
            &json!({ "caller_surface": caller, "target_surface": target, "command": "tell" }),
            &tr,
        )
        .unwrap();
        assert_eq!(
            host.done_commands_on(target).len(),
            3,
            "두 번째 상태 전환에도 완료 훅을 다시 등록해야 한다"
        );
    }

    #[test]
    fn spawn_tell_wiring_subscribes_the_error_axis() {
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        register_notify_hooks(&host, caller, target, "spawn");
        assert_eq!(
            host.hooks_for_event(target, crate::error_scan::STALLED_EVENT),
            1,
            "완료 3종과 함께 에러 정지 hook 도 등록돼야 함"
        );
        assert!(
            host.commands_on(target)
                .iter()
                .any(|c| c == &notify_error_command(caller, target))
        );
    }

    /// spawn·tell 은 백그라운드 대기 중 API 오류를 알릴 상시 훅도 하나 등록한다.
    /// 완료 훅 정리·재등록을 거쳐도 하나로 남는다.
    #[test]
    fn spawn_tell_wiring_subscribes_the_stop_failure_notice() {
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        host.mark_alive(target);
        register_notify_hooks(&host, caller, target, "spawn");
        let tr = test_translator();
        for _ in 0..2 {
            assert_eq!(
                host.hooks_for_event(target, crate::hook::STOP_FAILURE_EVENT),
                1
            );
            assert!(
                host.commands_on(target)
                    .iter()
                    .any(|c| c == &notify_stop_failure_command(caller, target))
            );
            assert_eq!(host.fire(target, crate::hook::STOP_FAILURE_EVENT), 1);
            assert_eq!(host.fire(target, "claude-idle"), 1);
            handle_notify_done(
                &std::sync::Mutex::new(crate::error_scan::ErrorScanner::new()),
                &host,
                &json!({ "caller_surface": caller, "target_surface": target, "command": "spawn" }),
                &tr,
            )
            .unwrap();
        }
    }

    #[test]
    fn error_hook_survives_the_sibling_fire_cleanup_rearm_cycle() {
        // 형제 그룹은 fire → 정리 → 재무장 사이클을 도는데, 에러 정지 hook 은 그
        // 사이클에 휘말리지 않아야 한다(수명이 다르다).
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        host.mark_alive(target);
        register_notify_hooks(&host, caller, target, "tell");
        let tr = test_translator();

        for _ in 0..3 {
            assert_eq!(host.fire(target, "needs-input"), 1);
            handle_notify_done(
                &std::sync::Mutex::new(crate::error_scan::ErrorScanner::new()),
                &host,
                &json!({ "caller_surface": caller, "target_surface": target, "command": "tell" }),
                &tr,
            )
            .unwrap();
            assert_eq!(
                host.hooks_for_event(target, crate::error_scan::STALLED_EVENT),
                1,
                "완료 훅을 다시 등록해도 오류 훅은 하나만 유지해야 한다"
            );
            assert_eq!(
                host.done_commands_on(target).len(),
                3,
                "완료 훅 3개도 다시 등록해야 한다"
            );
        }
    }

    #[test]
    fn error_hook_is_standing_so_repeated_stalls_keep_notifying() {
        // once=true 였다면 첫 발화 후 사라져 두 번째 정지를 놓친다.
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        register_notify_hooks(&host, caller, target, "spawn");
        assert_eq!(host.fire(target, crate::error_scan::STALLED_EVENT), 1);
        assert_eq!(
            host.hooks_for_event(target, crate::error_scan::STALLED_EVENT),
            1,
            "상시 hook 이라 발화해도 남아야 한다"
        );
        assert_eq!(host.fire(target, crate::error_scan::STALLED_EVENT), 1);
    }

    #[test]
    fn handle_notify_error_requires_both_surfaces() {
        let host = MockHost::new();
        let tr = test_translator();
        assert_eq!(
            handle_notify_error(
                &std::sync::Mutex::new(crate::error_scan::ErrorScanner::new()),
                &host,
                &json!({ "target_surface": 5 }),
                &tr
            )
            .unwrap_err()
            .code,
            -32602
        );
        assert_eq!(
            handle_notify_error(
                &std::sync::Mutex::new(crate::error_scan::ErrorScanner::new()),
                &host,
                &json!({ "caller_surface": 5 }),
                &tr
            )
            .unwrap_err()
            .code,
            -32602
        );
    }

    #[test]
    fn notify_error_message_appends_error_line_hint() {
        // 화면에 오류 줄이 있으면 알림에 그 내용을 덧붙인다.
        struct ScreenHost(&'static str);
        impl HostCall for ScreenHost {
            fn call(
                &self,
                method: &str,
                _params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                match method {
                    "surface.screen_text" => Ok(json!({ "text": self.0 })),
                    // 백그라운드 대기 기록이 없다.
                    "surface.meta.get" => Ok(json!({ "value": null })),
                    other => panic!("unexpected host call: {other}"),
                }
            }
        }
        let tr = test_translator();
        let with_hint = notify_error_message(
            &tr,
            &ScreenHost("working…\n  API Error: Connection error\n"),
            42,
        );
        assert_eq!(with_hint.0, crate::notice::NoticeKind::Stalled);
        let with_hint = with_hint.1;
        assert!(with_hint.contains("42"), "대상 surface 가 문구에 있어야 함");
        assert!(
            with_hint.contains("API Error: Connection error"),
            "에러 줄 힌트: {with_hint}"
        );

        // 화면에 에러 줄이 없으면 힌트 없이 본문만.
        let (kind, plain) = notify_error_message(&tr, &ScreenHost("all good\n"), 42);
        assert_eq!(kind, crate::notice::NoticeKind::StalledNoError);
        assert!(!plain.contains("Last error"), "{plain}");
    }

    /// 백그라운드 작업을 기다리는 자식에는 "looks stuck" 대신 대기 문구를 쓴다.
    #[test]
    fn notify_error_message_describes_a_background_wait() {
        struct WaitHost(String);
        impl HostCall for WaitHost {
            fn call(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                match method {
                    "surface.meta.get"
                        if params["key"] == crate::hook::BACKGROUND_WAIT_META_KEY =>
                    {
                        Ok(json!({ "value": self.0 }))
                    }
                    "surface.meta.get"
                        if params["key"] == crate::task_watch::BACKGROUND_QUIET_META_KEY =>
                    {
                        Ok(json!({}))
                    }
                    other => panic!("unexpected host call: {other} {params}"),
                }
            }
        }
        let since_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - 11 * 60_000;
        let wait = json!({ "since_ms": since_ms, "tasks": 1, "types": ["shell"] }).to_string();
        let tr = test_translator();
        let (kind, message) = notify_error_message(&tr, &WaitHost(wait.clone()), 42);
        assert_eq!(kind, crate::notice::NoticeKind::StalledBackgroundWait);
        assert_eq!(
            message,
            "surface 42: waiting on background work (shell) for 11 min, with no output meanwhile"
        );
        let (_, ko) = notify_error_message(&test_translator_ko(), &WaitHost(wait), 42);
        assert!(
            ko.contains("백그라운드 작업(shell)") && ko.contains("11분째"),
            "{ko}"
        );
    }

    /// 출력 파일 감시가 같은 대기에 남긴 기록이 있으면 조용한 작업의 이름과 분을 적는다.
    /// 시작 시각이 다른 대기의 기록은 쓰지 않는다.
    #[test]
    fn notify_error_message_names_the_quiet_background_tasks() {
        struct QuietHost(String, String);
        impl HostCall for QuietHost {
            fn call(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                assert_eq!(method, "surface.meta.get");
                if params["key"] == crate::hook::BACKGROUND_WAIT_META_KEY {
                    Ok(json!({ "value": self.0 }))
                } else {
                    assert_eq!(params["key"], crate::task_watch::BACKGROUND_QUIET_META_KEY);
                    Ok(json!({ "value": self.1 }))
                }
            }
        }
        let wait = json!({ "since_ms": 5_000, "tasks": 1, "types": ["shell"] }).to_string();
        let long = "x".repeat(100);
        let quiet = |since: u64| {
            json!({ "since_ms": since, "labels": ["sleep 300", long], "quiet_ms": 125_000 })
                .to_string()
        };
        let tr = test_translator();
        let (kind, line) = notify_error_message(&tr, &QuietHost(wait.clone(), quiet(5_000)), 42);
        assert_eq!(kind, crate::notice::NoticeKind::StalledBackgroundTaskQuiet);
        assert_eq!(
            line,
            format!(
                "surface 42: background work sleep 300, {}… has written no output for 2 min",
                "x".repeat(80)
            )
        );
        let (_, other) = notify_error_message(&tr, &QuietHost(wait, quiet(4_000)), 42);
        assert!(
            other.contains("waiting on background work (shell)"),
            "{other}"
        );
    }

    #[test]
    fn the_background_wait_message_names_the_flag_when_no_task_types_are_known() {
        let tr = test_translator();
        let message = crate::notifications::background_wait_message(
            &tr,
            3,
            &json!({ "since_ms": 0, "tasks": 0, "types": [] }),
            600_000,
        );
        assert_eq!(
            message,
            "surface 3: waiting on background work (waiting_on_background_work) for 10 min, with no output meanwhile"
        );
    }

    #[test]
    fn handle_notify_done_does_not_rearm_when_target_exited() {
        let host = MockHost::new();
        let (caller, target) = (7u32, 1650u32);
        host.mark_alive(target);
        register_notify_hooks(&host, caller, target, "spawn");

        // surface가 없다고 응답하는 상태에서 process-exit 훅을 처리한다.
        assert_eq!(host.fire(target, "process-exit"), 1);
        host.mark_dead(target);
        let tr = test_translator();
        handle_notify_done(
            &std::sync::Mutex::new(crate::error_scan::ErrorScanner::new()),
            &host,
            &json!({ "caller_surface": caller, "target_surface": target, "command": "spawn" }),
            &tr,
        )
        .unwrap();

        assert!(
            host.done_commands_on(target).is_empty(),
            "없는 surface에 완료 훅을 다시 등록해서는 안 된다: {:?}",
            host.done_commands_on(target)
        );
    }

    /// 호스트 자식 목록·전경 프로세스·종료 응답을 반환해 Claude 응답 변환을 검사하는 mock.
    struct ShapeHost;

    impl HostCall for ShapeHost {
        fn call(
            &self,
            method: &str,
            _params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            match method {
                "terminal.children" => Ok(json!({ "children": [{
                    "surface_id": 42,
                    "index": 0,
                    "cwd": "/w",
                    "role": "reviewer",
                    "nickname": "nick",
                    "state": "idle",
                    "evidence": "prompt",
                    "confidence": "certain",
                    "parent": 1,
                    "last_report_at": 1_767_613_200_000_u64,
                }]})),
                "surface.foreground_process" => Ok(json!({ "name": "claude", "pid": 4242 })),
                "terminal.kill" => Ok(json!({ "killed_surface_id": 42, "child_index": 0 })),
                // kill 은 대상이 attach mirror 의 원격 자식인지 먼저 묻는다.
                "attach.agent_target" => Ok(json!({ "remote": false })),
                other => panic!("unexpected host call: {other}"),
            }
        }
    }

    /// 배열 응답과 child_surface_id 변환, 전경 프로세스 정보 추가를 유지해야 한다.
    #[test]
    fn children_response_is_a_bare_remapped_array() {
        let out = handle_children(
            &Returns::new(None),
            &ShapeHost,
            &json!({ "surface_id": 1 }),
            &test_translator(),
        )
        .expect("handle_children");
        let arr = out
            .as_array()
            .unwrap_or_else(|| panic!("응답이 배열이 아니다: {out}"));
        assert_eq!(arr.len(), 1);
        let e = &arr[0];
        assert_eq!(
            e["child_surface_id"],
            json!(42),
            "child_surface_id 필드로 변환되지 않았다: {e}"
        );
        assert!(
            e.get("surface_id").is_none(),
            "호스트 필드명이 그대로 남았다: {e}"
        );
        // 상태와 판단 근거·확실성을 모두 전달한다.
        assert_eq!(e["state"], json!("idle"));
        assert_eq!(e["evidence"], json!("prompt"));
        assert_eq!(e["confidence"], json!("certain"));
        assert_eq!(e["foreground_process"], json!("claude"));
        assert_eq!(e["foreground_pid"], json!(4242));
        // 표의 parents·last_report_at 열이 읽는 값이다.
        assert_eq!(e["parent"], json!(1));
        assert_eq!(e["last_report_at"], json!(1_767_613_200_000_u64));
    }

    /// CLI 의 --format 은 출력 방식만 고른다. 플러그인 응답은 바뀌지 않는다.
    #[test]
    fn children_response_ignores_the_cli_format_param() {
        let call = |params: Value| {
            handle_children(&Returns::new(None), &ShapeHost, &params, &test_translator())
                .expect("handle_children")
        };
        assert_eq!(
            call(json!({ "surface_id": 1, "format": "table" })),
            call(json!({ "surface_id": 1 }))
        );
    }

    /// 실제로 닫힌 kill은 호스트 응답에 `killed: true`를 더해 돌려주고 닫힌 surface의 오류 감시를 끈다.
    #[test]
    fn kill_response_is_the_host_response() {
        let scanner = Arc::new(Mutex::new(ErrorScanner::new()));
        crate::error_scan::lock_scanner(&scanner).enable(42, ScanTarget::Child);
        let out = handle_kill(
            &scanner,
            &Returns::new(None),
            &ShapeHost,
            &json!({ "surface_id": 1, "child_index": 0 }),
            &test_translator(),
        )
        .expect("handle_kill");
        assert_eq!(
            out,
            json!({ "killed_surface_id": 42, "child_index": 0, "killed": true })
        );
        assert!(!crate::error_scan::lock_scanner(&scanner).is_enabled(42));
    }

    /// 원격으로 넘긴 kill은 `killed: false`로 답하고 surface의 오류 감시도 유지한다.
    #[test]
    fn a_forwarded_kill_is_not_reported_as_killed() {
        struct ForwardHost;
        impl HostCall for ForwardHost {
            fn call(
                &self,
                method: &str,
                _params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                match method {
                    // 원격 자식 판별 조회. 이 부모 아래 자식 목록이 비어 있으면 로컬 kill 이다.
                    "terminal.children" => Ok(json!({ "children": [] })),
                    other => {
                        assert_eq!(other, "terminal.kill");
                        Ok(json!({ "forwarded": true, "surface_id": 42, "child_index": 0 }))
                    }
                }
            }
        }
        let scanner = Arc::new(Mutex::new(ErrorScanner::new()));
        crate::error_scan::lock_scanner(&scanner).enable(42, ScanTarget::Child);
        let out = handle_kill(
            &scanner,
            &Returns::new(None),
            &ForwardHost,
            &json!({ "surface_id": 1, "child_index": 0 }),
            &test_translator(),
        )
        .expect("handle_kill");
        assert_eq!(
            out,
            json!({ "forwarded": true, "surface_id": 42, "child_index": 0, "killed": false })
        );
        assert!(crate::error_scan::lock_scanner(&scanner).is_enabled(42));
    }

    /// Codex 부모의 자식 관리 시험. `codex.parent`·`terminal.*` 을 흉내 내고 호출을 기록한다.
    struct CodexParentHost {
        terminal_parent: Value,
        children: Vec<u32>,
        seen: std::cell::RefCell<Vec<(String, Value)>>,
    }

    const RECEIVER: &str = "0123456789abcdef0123456789abcdef";
    const EXTERNAL: &str = "codex:0193a6f2-7c1e-7b3d-9a40-1f2e3d4c5b6a";

    impl CodexParentHost {
        fn external(children: Vec<u32>) -> Self {
            Self {
                terminal_parent: json!({ "parent_external": EXTERNAL }),
                children,
                seen: Default::default(),
            }
        }

        fn calls(&self, method: &str) -> Vec<Value> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == method)
                .map(|(_, p)| p.clone())
                .collect()
        }
    }

    impl HostCall for CodexParentHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            self.seen.borrow_mut().push((method.to_string(), params));
            Ok(match method {
                "codex.parent" => {
                    json!({ "receiver_id": RECEIVER, "terminal_parent": self.terminal_parent })
                }
                "terminal.children" => json!({ "children": self.children.iter().enumerate()
                    .map(|(i, id)| json!({ "index": i, "surface_id": id })).collect::<Vec<_>>() }),
                "terminal.spawn" | "terminal.respawn" => json!({ "child_surface_id": 55 }),
                "codex.deliver" => json!({ "event_id": "e", "delivery_state": "captured" }),
                // 자식은 모두 workspace 9 에 있다.
                "surface.list" => json!(
                    self.children
                        .iter()
                        .map(|id| json!({ "id": id, "workspace_id": 9 }))
                        .collect::<Vec<_>>()
                ),
                _ => json!({}),
            })
        }
    }

    fn codex_call(extra: Value) -> Value {
        let mut v = json!({ "_tasty_context": {
            "provider": "codex",
            "thread_id": "0193a6f2-7c1e-7b3d-9a40-1f2e3d4c5b6a",
        } });
        if let (Some(dst), Some(src)) = (v.as_object_mut(), extra.as_object()) {
            dst.extend(src.clone());
        }
        v
    }

    /// 외부 부모의 index 명령은 호스트가 지금 그 자식들을 둔 workspace 로 보낸다. spawn 때 준 이름을
    /// 다시 쓰지 않는다. 자기 surface id 를 요구하지 않는다.
    #[test]
    fn index_commands_of_an_external_parent_use_the_workspace_that_holds_the_children() {
        let dir = tempfile::tempdir().unwrap();
        let returns = Returns::new(Some(dir.path()));
        let host = CodexParentHost::external(vec![55]);
        let target = parent_target(&host, &codex_call(json!({})), &test_translator()).unwrap();
        let mut sp = serde_json::Map::new();
        target.insert(&mut sp, "parent");
        sp.insert("workspace".into(), json!("lane"));
        create_child(
            &returns,
            &host,
            "terminal.spawn",
            sp,
            &target,
            true,
            &test_translator(),
        )
        .unwrap();
        assert_eq!(
            host.calls("terminal.spawn"),
            vec![json!({ "parent_external": EXTERNAL, "workspace": "lane" })]
        );

        let tr = test_translator();
        handle_children(&returns, &host, &codex_call(json!({})), &tr).unwrap();
        handle_kill(
            &Arc::new(Mutex::new(ErrorScanner::new())),
            &returns,
            &host,
            &codex_call(json!({ "child_index": 0 })),
            &tr,
        )
        .unwrap();
        handle_broadcast(&returns, &host, &codex_call(json!({ "text": "hi" })), &tr).unwrap();
        let routed = json!({ "parent_external": EXTERNAL, "workspace": "9" });
        // children 과, kill 이 원격 자식인지 확인한 조회.
        assert_eq!(
            host.calls("terminal.children"),
            vec![routed.clone(), routed.clone()]
        );
        assert_eq!(
            host.calls("terminal.kill"),
            vec![json!({ "parent_external": EXTERNAL, "workspace": "9", "child": 0 })]
        );
        assert_eq!(
            host.calls("terminal.broadcast"),
            vec![json!({ "parent_external": EXTERNAL, "workspace": "9", "text": "hi" })]
        );
    }

    /// 결과 경로는 Claude 를 띄우기 전에, 자식 생성 직후 저장된다. 첫 hook 이 바로 와도 경로가 있다.
    #[test]
    fn a_spawned_child_has_its_route_before_its_first_hook() {
        let dir = tempfile::tempdir().unwrap();
        let returns = Returns::new(Some(dir.path()));
        let host = CodexParentHost::external(vec![]);
        let target = parent_target(&host, &codex_call(json!({})), &test_translator()).unwrap();
        let (_, child) = create_child(
            &returns,
            &host,
            "terminal.spawn",
            serde_json::Map::new(),
            &target,
            true,
            &test_translator(),
        )
        .unwrap();
        returns.observe("session-start", child, Some("s"), None);
        returns.turn_end(crate::codex_return::TurnEnd {
            surface: child,
            session: Some("s"),
            status: crate::codex_return::ReturnStatus::TurnEnded,
            body: Some("done"),
            attempt_id: None,
            turn: None,
        });
        returns.pump_once(&host, crate::codex_return::now_ms());
        let sent = host.calls("codex.deliver");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["receiver_id"], RECEIVER);
        assert_eq!(sent[0]["child_surface"], 55);
    }

    /// 경로를 저장하지 못하면 만든 자식 id 를 알리며 실패한다. 결과를 잃은 채 성공으로 끝나지 않는다.
    #[test]
    fn a_route_that_cannot_be_saved_fails_the_spawn_with_the_child_id() {
        let dir = tempfile::tempdir().unwrap();
        // 저장 파일 자리에 폴더를 두어 쓰기를 실패시킨다.
        std::fs::create_dir_all(dir.path().join("codex-return/routes.json")).unwrap();
        let returns = Returns::new(Some(dir.path()));
        let host = CodexParentHost::external(vec![]);
        let target = parent_target(&host, &codex_call(json!({})), &test_translator()).unwrap();
        let err = create_child(
            &returns,
            &host,
            "terminal.spawn",
            serde_json::Map::new(),
            &target,
            false,
            &test_translator(),
        )
        .unwrap_err();
        assert!(err.message.contains("55"), "{}", err.message);
    }

    /// Codex 대화는 자기 자식에게만 tell 할 수 있다. 다른 부모의 자식은 경로를 만들지 않고 거절한다.
    #[test]
    fn a_codex_tell_reaches_only_its_own_children() {
        let dir = tempfile::tempdir().unwrap();
        let returns = Returns::new(Some(dir.path()));
        let host = CodexParentHost::external(vec![55]);
        let tr = test_translator();
        let err = handle_tell(
            &returns,
            &host,
            &codex_call(json!({ "surface": 77, "message": "hi" })),
            &tr,
        )
        .unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(host.calls("terminal.tell").is_empty());

        handle_tell(
            &returns,
            &host,
            &codex_call(json!({ "surface": 55, "message": "go" })),
            &tr,
        )
        .unwrap();
        assert_eq!(
            host.calls("terminal.tell"),
            vec![json!({ "surface": 55, "text": "go" })],
            "the explicit child stays the target"
        );
        returns.observe("prompt-submit", 55, Some("s"), None);
        returns.turn_end(crate::codex_return::TurnEnd {
            surface: 55,
            session: Some("s"),
            status: crate::codex_return::ReturnStatus::TurnEnded,
            body: Some("answer"),
            attempt_id: None,
            turn: None,
        });
        returns.pump_once(&host, crate::codex_return::now_ms());
        assert_eq!(host.calls("codex.deliver")[0]["body"], "answer");
    }

    /// Codex 경로가 있는 자식에게 일반 호출자가 tell·respawn 으로 새 지시를 보내면, 지시를 보내기 전에 경로를
    /// 끊는다. 그 뒤 결과는 Codex 대화로 가지 않고, 이미 claim 한 결과는 그대로 보낸다.
    #[test]
    fn a_legacy_instruction_releases_the_codex_route_before_it_is_sent() {
        for via in ["tell", "respawn"] {
            let dir = tempfile::tempdir().unwrap();
            let returns = Returns::new(Some(dir.path()));
            let host = CodexParentHost::external(vec![55]);
            let tr = test_translator();
            let target = parent_target(&host, &codex_call(json!({})), &tr).unwrap();
            create_child(
                &returns,
                &host,
                "terminal.spawn",
                serde_json::Map::new(),
                &target,
                true,
                &tr,
            )
            .unwrap();
            returns.observe("session-start", 55, Some("s"), None);
            returns.turn_end(crate::codex_return::TurnEnd {
                surface: 55,
                session: Some("s"),
                status: crate::codex_return::ReturnStatus::TurnEnded,
                body: Some("claimed before"),
                attempt_id: None,
                turn: None,
            });
            match via {
                "tell" => {
                    handle_tell(
                        &returns,
                        &host,
                        &json!({ "surface": 55, "message": "plain" }),
                        &tr,
                    )
                    .unwrap();
                }
                _ => {
                    create_child(
                        &returns,
                        &host,
                        "terminal.respawn",
                        serde_json::Map::new(),
                        &ParentTarget::Legacy(Some(3)),
                        true,
                        &tr,
                    )
                    .unwrap();
                }
            }
            returns.observe("prompt-submit", 55, Some("s"), None);
            returns.turn_end(crate::codex_return::TurnEnd {
                surface: 55,
                session: Some("s"),
                status: crate::codex_return::ReturnStatus::TurnEnded,
                body: Some("for the plain caller"),
                attempt_id: None,
                turn: None,
            });
            returns.pump_once(&host, crate::codex_return::now_ms());
            let sent = host.calls("codex.deliver");
            assert_eq!(sent.len(), 1, "{via}: {sent:?}");
            assert_eq!(sent[0]["body"], "claimed before", "{via}");
        }
    }

    /// 경로를 끊지 못하면 일반 tell 도 보내지 않는다. 결과가 이전 Codex 대화로 갈 수 있기 때문이다.
    #[test]
    fn a_legacy_tell_is_not_sent_when_the_route_cannot_be_released() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("codex-return")).unwrap();
        std::fs::write(dir.path().join("codex-return/routes.json"), "not json").unwrap();
        let returns = Returns::new(Some(dir.path()));
        let host = CodexParentHost::external(vec![55]);
        let err = handle_tell(
            &returns,
            &host,
            &json!({ "surface": 55, "message": "plain" }),
            &test_translator(),
        )
        .unwrap_err();
        assert!(err.message.contains("55"), "{}", err.message);
        assert!(host.calls("terminal.tell").is_empty());
    }

    /// 닫힌 자식의 결과 경로는 지운다. 그 surface 의 늦은 hook 은 결과를 만들지 않는다.
    #[test]
    fn killing_a_codex_child_forgets_its_route() {
        struct KillHost(CodexParentHost);
        impl HostCall for KillHost {
            fn call(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, tasty_plugin_sdk::PluginError> {
                if method == "terminal.kill" {
                    return Ok(json!({ "killed_surface_id": 55, "child_index": 0 }));
                }
                self.0.call(method, params)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let returns = Returns::new(Some(dir.path()));
        let host = KillHost(CodexParentHost::external(vec![55]));
        let tr = test_translator();
        let target = parent_target(&host, &codex_call(json!({})), &tr).unwrap();
        create_child(
            &returns,
            &host,
            "terminal.spawn",
            serde_json::Map::new(),
            &target,
            true,
            &tr,
        )
        .unwrap();
        returns.observe("session-start", 55, Some("s"), None);
        handle_kill(
            &Arc::new(Mutex::new(ErrorScanner::new())),
            &returns,
            &host,
            &codex_call(json!({ "child_index": 0 })),
            &tr,
        )
        .unwrap();
        returns.turn_end(crate::codex_return::TurnEnd {
            surface: 55,
            session: Some("s"),
            status: crate::codex_return::ReturnStatus::Exited,
            body: None,
            attempt_id: None,
            turn: None,
        });
        assert!(returns.pending().unwrap().is_empty());
    }

    /// 일반 호출은 Codex 를 부르지 않고 기존 surface 인자와 응답을 그대로 쓴다.
    #[test]
    fn a_legacy_call_never_asks_codex() {
        let returns = Returns::new(None);
        let host = CodexParentHost::external(vec![55]);
        let tr = test_translator();
        handle_children(&returns, &host, &json!({ "surface": 3 }), &tr).unwrap();
        handle_tell(
            &returns,
            &host,
            &json!({ "surface": 55, "message": "x" }),
            &tr,
        )
        .unwrap();
        assert!(host.calls("codex.parent").is_empty());
        assert_eq!(
            host.calls("terminal.children"),
            vec![json!({ "surface": 3 })]
        );
    }
}
