//! 무출력 알림 선언의 IPC(`claude.quiet`)와 끝난 선언의 묶음을 부모 완료 로그에 쓰는 처리.
//! 선언의 기록과 보류는 [`crate::error_scan::quiet`] 가 맡는다.

use std::sync::Mutex;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::{HostCall, cleanup_sibling_hooks};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::error_scan::ErrorScanner;
use crate::error_scan::quiet::{QUIET_RELEASED_EVENT, Release, one_line};
use crate::notice::NoticeKind;

/// 묶음 한 줄에 적는 항목 수. 넘으면 남은 수만 적는다.
const ITEMS_SHOWN: usize = 5;

/// `claude.quiet`: 선언을 넣거나(`secs`) 끝낸다(`clear`). 거절하면 아무것도 기록하지 않는다.
pub(crate) fn handle_quiet<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let surface = crate::handlers::require_target_surface(params, tr)?;
    let task = params
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from);
    let clear = params
        .get("clear")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let secs = params.get("secs").filter(|v| !v.is_null());
    let secs = match (secs, clear) {
        (Some(_), true) | (None, false) => {
            return Err(IpcMethodError::invalid_params(
                tr.t("claude.quiet.secs_or_clear"),
            ));
        }
        (None, true) => None,
        (Some(raw), false) => {
            Some(raw.as_i64().filter(|s| *s > 0 || *s == -1).ok_or_else(|| {
                IpcMethodError::invalid_params(
                    &tr.t_fmt("claude.quiet.invalid_secs", &raw.to_string()),
                )
            })?)
        }
    };
    let mut scanner = crate::error_scan::lock_scanner(scanner);
    if !scanner.is_enabled(surface) {
        return Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.quiet.not_tracked", &surface.to_string()),
        ));
    }
    Ok(match secs {
        None => {
            let (cleared, released) = scanner.clear_quiet(host, surface, task.as_deref());
            json!({ "surface": surface, "task": task, "cleared": cleared, "released": released })
        }
        Some(secs) => {
            let d = scanner.declare_quiet(host, surface, task.clone(), secs);
            json!({
                "surface": surface,
                "task": task,
                "secs": secs,
                "mode": d.mode,
                "until_ms": d.until_ms,
            })
        }
    })
}

/// 묶음 훅 명령의 앞부분. 뒤에 부모 surface 가 온다.
pub(crate) const NOTIFY_QUIET_RELEASE_PREFIX: &str =
    "tasty claude notify-quiet-release --caller-surface ";

/// 묶음을 부모에게 알리는 훅 명령. 완료 그룹과 다른 명령을 쓴다.
pub(crate) fn notify_quiet_release_command(caller_surface: u32, target_surface: u32) -> String {
    format!("{NOTIFY_QUIET_RELEASE_PREFIX}{caller_surface} --target-surface {target_surface}")
}

/// [`QUIET_RELEASED_EVENT`] 를 반복해서 받는 훅을 등록한다. 같은 부모·대상의 기존 훅을 먼저 정리한다.
pub(crate) fn register_quiet_release_hook<H: HostCall>(
    host: &H,
    caller_surface: u32,
    target_surface: u32,
) {
    let command = notify_quiet_release_command(caller_surface, target_surface);
    cleanup_sibling_hooks(host, target_surface, &command);
    if let Err(error) = host.call(
        "hook.set",
        json!({
            "surface_id": target_surface,
            "event": QUIET_RELEASED_EVENT,
            "command": command,
            "once": false,
        }),
    ) {
        tracing::warn!("quiet release notification hook registration failed: {error}");
    }
}

/// `claude.notify_quiet_release`: 이 부모가 아직 받지 않은 묶음을 한 줄씩 쓴다.
pub(crate) fn handle_notify_quiet_release<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let (caller, target) = caller_and_target(params, tr)?;
    let releases =
        crate::error_scan::lock_scanner(scanner).take_quiet_releases(host, target, caller, false);
    for r in &releases {
        let line = release_line(tr, target, r);
        if let Err(e) = crate::notice::append(caller, NoticeKind::QuietReleased, &line) {
            tracing::warn!("claude notify-quiet-release completion-log append failed: {e}");
        }
    }
    Ok(json!({ "written": releases.len() }))
}

/// error 알림이 나간다. 대상의 선언을 모두 끝내고, 이 부모가 받지 않은 묶음을 error 줄 뒤에 붙일 글로 돌려준다.
pub(crate) fn error_bundle<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    caller: u32,
    target: u32,
    tr: &Translator,
) -> String {
    let mut scanner = crate::error_scan::lock_scanner(scanner);
    scanner.end_quiet_on_error(host, target);
    scanner
        .take_quiet_releases(host, target, caller, true)
        .iter()
        .map(|r| {
            tr.t_args(
                "claude.notify.quiet_held_suffix",
                &[
                    &r.held_count().to_string(),
                    &r.secs.to_string(),
                    &scope(tr, r),
                    &items_text(tr, r),
                ],
            )
        })
        .collect()
}

/// 끝난 선언의 묶음 한 줄. 줄바꿈이 없다.
pub(crate) fn release_line(tr: &Translator, target: u32, r: &Release) -> String {
    tr.t_args(
        "claude.notify.quiet_released_message",
        &[
            &target.to_string(),
            &r.held_count().to_string(),
            &r.secs.to_string(),
            &scope(tr, r),
            r.reason.token(),
            &items_text(tr, r),
        ],
    )
}

fn scope(tr: &Translator, r: &Release) -> String {
    match &r.task {
        Some(task) => tr.t_fmt("claude.notify.quiet_scope_task", &one_line(task)),
        None => tr.t("claude.notify.quiet_scope_surface").to_string(),
    }
}

/// 항목 목록. 시각은 선언 시작부터 지난 분이다. 로그 줄에는 시각이 없어 시간대 없이 바로 읽히게 한다.
/// 이름은 쌓을 때와 meta 에서 되살릴 때 한 줄로 잘랐지만 줄 하나가 알림 하나라는 계약을 여기서도 지킨다.
/// 적지 않은 항목과 항목 수 상한을 넘은 알림은 남은 알림 수로 적는다.
fn items_text(tr: &Translator, r: &Release) -> String {
    let minutes = |ms: u64| (ms.saturating_sub(r.set_at_ms) / 60_000).to_string();
    let mut parts: Vec<String> = r
        .items
        .iter()
        .take(ITEMS_SHOWN)
        .map(|i| {
            tr.t_args(
                "claude.notify.quiet_item",
                &[
                    &one_line(&i.label),
                    &i.count.to_string(),
                    i.kind.token(),
                    &minutes(i.first_ms),
                    &minutes(i.last_ms),
                    &(i.quiet_ms / 60_000).to_string(),
                ],
            )
        })
        .collect();
    let more = r
        .items
        .iter()
        .skip(ITEMS_SHOWN)
        .map(|i| i.count)
        .fold(r.overflow.map_or(0, |o| o.count), u32::saturating_add);
    if more > 0 {
        parts.push(tr.t_fmt("claude.notify.quiet_more", &more.to_string()));
    }
    parts.join("; ")
}

fn caller_and_target(params: &Value, tr: &Translator) -> Result<(u32, u32), IpcMethodError> {
    let read = |key: &str, missing: &str| {
        params
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| IpcMethodError::invalid_params(tr.t(missing)))
    };
    Ok((
        read("caller_surface", "claude.params.missing_caller_surface")?,
        read("target_surface", "claude.params.missing_target_surface")?,
    ))
}

#[cfg(test)]
#[path = "quiet_notice_tests.rs"]
mod tests;
