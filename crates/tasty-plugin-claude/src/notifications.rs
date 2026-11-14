//! 부모 터미널에 완료·진행 정체 알림을 남긴다.
use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::{HostCall, cleanup_sibling_hooks, surface_is_alive};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use std::sync::Mutex;

use crate::error_scan::ErrorScanner;
use crate::notice::NoticeKind;
use tasty_utils::notify::NoticeType;

/// 완료 훅 등록과 정리에 같은 명령 문자열을 사용한다.
/// 대상 터미널과 이 문자열이 모두 같은 훅을 한 그룹으로 찾는다.
pub(crate) fn notify_done_command(
    caller_surface: u32,
    target_surface: u32,
    command_name: &str,
) -> String {
    format!(
        "{}{command_name}",
        notify_done_group_prefix(caller_surface, target_surface)
    )
}

/// 같은 부모·대상의 완료 그룹이 공유하는 명령 앞부분. 등록할 때 이 앞부분으로 기존 그룹을 찾아 바꾼다.
fn notify_done_group_prefix(caller_surface: u32, target_surface: u32) -> String {
    format!(
        "tasty claude notify-done --caller-surface {caller_surface} --target-surface {target_surface} --command "
    )
}

/// 자식의 상태 변경을 알린다. 입력 대기·종료도 포함하므로 작업 완료를 뜻하지는 않는다.
pub(crate) fn notify_done_message(
    tr: &Translator,
    command_name: &str,
    target_surface: u32,
) -> String {
    tr.t_args(
        "claude.notify.done_message",
        &[&target_surface.to_string(), command_name],
    )
}

/// 마지막 턴의 API 오류가 있으면 완료 문구 뒤에 덧붙인다.
pub(crate) fn with_stop_failure_hint(
    tr: &Translator,
    mut message: String,
    error: Option<&str>,
) -> String {
    if let Some(error) = error.filter(|e| !e.is_empty()) {
        message.push_str(&tr.t_args("claude.notify.stop_failure_hint", &[error]));
    }
    message
}

/// 마지막 턴의 API 오류를 읽는다. 조회에 실패하면 힌트 없이 알린다.
fn last_stop_failure<H: HostCall>(host: &H, target_surface: u32) -> Option<String> {
    host.call(
        "surface.meta.get",
        json!({ "surface_id": target_surface, "key": crate::hook::STOP_FAILURE_META_KEY }),
    )
    .ok()
    .and_then(|r| r.get("value").and_then(|v| v.as_str()).map(String::from))
    .filter(|s| !s.is_empty())
}

/// claude-idle / needs-input / process-exit 완료 훅을 등록한다.
/// 등록 실패는 spawn/tell의 성공을 취소하지 않는다.
/// 반복해서 받는 오류 알림 훅은 완료 훅과 수명이 달라 이 그룹에 넣지 않는다.
pub(crate) fn register_notify_hooks<H: HostCall>(
    host: &H,
    caller_surface: u32,
    target_surface: u32,
    command_name: &str,
) {
    let command = notify_done_command(caller_surface, target_surface, command_name);
    // 플러그인마다 매니페스트에 등록한 이벤트 목록을 넘긴다.
    tasty_plugin_agent_common::host_call::register_completion_hooks(
        host,
        target_surface,
        &command,
        &notify_done_group_prefix(caller_surface, target_surface),
        &["claude-idle", "needs-input", "process-exit"],
        "claude",
    );
    register_error_notify_hook(host, caller_surface, target_surface);
    register_stop_failure_notify_hook(host, caller_surface, target_surface);
    crate::quiet_notice::register_quiet_release_hook(host, caller_surface, target_surface);
}

/// 백그라운드 작업이 남은 StopFailure 를 부모에게 알리는 훅 명령. 완료 그룹과 다른 명령을 쓴다.
pub(crate) fn notify_stop_failure_command(caller_surface: u32, target_surface: u32) -> String {
    format!(
        "tasty claude notify-stop-failure --caller-surface {caller_surface} --target-surface {target_surface}"
    )
}

/// `claude-stop-failure` 를 반복해서 받는 훅을 등록한다. 같은 부모·대상의 기존 훅을 먼저 정리한다.
/// 턴이 끝난 StopFailure 는 완료 줄이 오류를 적으므로 이 훅은 줄을 남기지 않는다.
pub(crate) fn register_stop_failure_notify_hook<H: HostCall>(
    host: &H,
    caller_surface: u32,
    target_surface: u32,
) {
    let command = notify_stop_failure_command(caller_surface, target_surface);
    cleanup_sibling_hooks(host, target_surface, &command);
    if let Err(error) = host.call(
        "hook.set",
        json!({
            "surface_id": target_surface,
            "event": crate::hook::STOP_FAILURE_EVENT,
            "command": command,
            "once": false,
        }),
    ) {
        tracing::warn!("stop-failure notification hook registration failed: {error}");
    }
}

/// 완료 훅과 다른 명령을 써서 완료 그룹을 정리할 때 제거되지 않게 한다.
pub(crate) fn notify_error_command(caller_surface: u32, target_surface: u32) -> String {
    format!(
        "tasty claude notify-error --caller-surface {caller_surface} --target-surface {target_surface}"
    )
}

/// 진행 정체 알림을 반복해서 받는 훅을 등록한다.
/// 발신 측 오류 감시기가 알림 빈도를 제한하므로 once를 사용하지 않는다.
/// 같은 부모·대상의 기존 훅을 정리한 뒤 새 훅 등록을 시도한다.
pub(crate) fn register_error_notify_hook<H: HostCall>(
    host: &H,
    caller_surface: u32,
    target_surface: u32,
) {
    let command = notify_error_command(caller_surface, target_surface);
    cleanup_sibling_hooks(host, target_surface, &command);
    // 등록에 실패해도 spawn/tell은 유지하고 경고만 남긴다.
    if let Err(error) = host.call(
        "hook.set",
        json!({
            "surface_id": target_surface,
            "event": crate::error_scan::STALLED_EVENT,
            "command": command,
            "once": false,
        }),
    ) {
        tracing::warn!("completion error observation hook registration failed: {error}");
    }
}

/// 완료 로그를 남기고 같은 대상·명령의 완료 훅을 정리한다.
/// 대상이 여전히 조회되면 다음 알림을 받도록 다시 등록한다.
pub(crate) fn handle_notify_done<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let caller_surface = params
        .get("caller_surface")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(tr.t("claude.params.missing_caller_surface"))
        })? as u32;
    let target_surface = params
        .get("target_surface")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(tr.t("claude.params.missing_target_surface"))
        })? as u32;
    let command_name = params
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_command")))?;

    let (kind, mut message) = notify_done_line(tr, host, command_name, target_surface);
    if kind.notice_type() == NoticeType::Error {
        message.push_str(&crate::quiet_notice::error_bundle(
            scanner,
            host,
            caller_surface,
            target_surface,
            tr,
        ));
    }
    if let Err(e) = crate::notice::append(caller_surface, kind, &message) {
        tracing::warn!("claude notify-done completion-log append failed: {e}");
    }

    // 다른 자식의 훅을 지우지 않도록 대상과 명령 문자열을 함께 비교한다.
    let expected_command = notify_done_command(caller_surface, target_surface, command_name);
    cleanup_sibling_hooks(host, target_surface, &expected_command);

    // 다음 상태 전환도 받을 수 있도록 대상이 조회되면 다시 등록한다.
    rearm_if_still_alive(host, caller_surface, target_surface, command_name);

    Ok(json!({}))
}

/// 진행 정체 알림을 부모 로그에 남긴다.
/// 이 알림만으로 자식 상태를 바꾸거나 완료 훅을 정리하지 않는다.
pub(crate) fn handle_notify_error<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let caller_surface = params
        .get("caller_surface")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(tr.t("claude.params.missing_caller_surface"))
        })? as u32;
    let target_surface = params
        .get("target_surface")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(tr.t("claude.params.missing_target_surface"))
        })? as u32;

    let (kind, mut message) = notify_error_message(tr, host, target_surface);
    if kind.notice_type() == NoticeType::Error {
        message.push_str(&crate::quiet_notice::error_bundle(
            scanner,
            host,
            caller_surface,
            target_surface,
            tr,
        ));
    }
    if let Err(e) = crate::notice::append(caller_surface, kind, &message) {
        tracing::warn!("claude notify-error completion-log append failed: {e}");
    }
    Ok(json!({}))
}

/// 완료 줄과 그 종류. 오류 힌트("입력을 기다린다")는 턴이 끝난 idle 줄에만 붙인다. 백그라운드 작업이 남아
/// 대기 중에 기록된 오류가 needs-input·process-exit 줄에 붙지 않게 한다.
/// 세 완료 훅이 같은 명령을 쓰므로 어느 사건인지 모른다. 대상 상태로 가른다: `exited` 와 힌트가 붙은 idle 은 error 다.
pub(crate) fn notify_done_line<H: HostCall>(
    tr: &Translator,
    host: &H,
    command_name: &str,
    target_surface: u32,
) -> (NoticeKind, String) {
    let state = target_state(host, target_surface);
    let error = if state.as_deref() == Some("idle") {
        last_stop_failure(host, target_surface)
    } else {
        None
    };
    let kind = if error.is_some() {
        NoticeKind::StopFailure
    } else if state.as_deref() == Some("exited") {
        NoticeKind::Exited
    } else {
        NoticeKind::StateChanged
    };
    let line = with_stop_failure_hint(
        tr,
        notify_done_message(tr, command_name, target_surface),
        error.as_deref(),
    );
    (kind, line)
}

/// 대상의 현재 상태(`idle`·`active`·`needs_input` 등). 조회에 실패하면 `None` 이다.
fn target_state<H: HostCall>(host: &H, target_surface: u32) -> Option<String> {
    host.call("terminal.state", json!({ "surface": target_surface }))
        .ok()
        .and_then(|r| r.get("state").and_then(|v| v.as_str()).map(str::to_string))
}

/// 백그라운드 작업이 남은 채 API 오류로 끝난 턴을 부모 로그에 한 번 남긴다.
/// 대기 기록이 없으면 턴이 끝난 StopFailure 이고 완료 줄이 오류를 적으므로 줄을 남기지 않는다.
pub(crate) fn handle_notify_stop_failure<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let caller_surface = params
        .get("caller_surface")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(tr.t("claude.params.missing_caller_surface"))
        })? as u32;
    let target_surface = params
        .get("target_surface")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(tr.t("claude.params.missing_target_surface"))
        })? as u32;
    let Some(mut message) = stop_failure_notice(tr, host, target_surface) else {
        return Ok(json!({ "written": false }));
    };
    message.push_str(&crate::quiet_notice::error_bundle(
        scanner,
        host,
        caller_surface,
        target_surface,
        tr,
    ));
    if let Err(e) =
        crate::notice::append(caller_surface, NoticeKind::StopFailureBackground, &message)
    {
        tracing::warn!("claude notify-stop-failure completion-log append failed: {e}");
    }
    Ok(json!({ "written": true }))
}

/// 백그라운드 대기 중인 StopFailure 의 부모 로그 문구. 대기 기록이 없으면 `None` 이다.
pub(crate) fn stop_failure_notice<H: HostCall>(
    tr: &Translator,
    host: &H,
    target_surface: u32,
) -> Option<String> {
    let wait = background_wait(host, target_surface)?;
    let error = last_stop_failure(host, target_surface).unwrap_or_else(|| "unknown".to_string());
    let types: Vec<&str> = wait
        .get("types")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let detail = if types.is_empty() {
        "waiting_on_background_work".to_string()
    } else {
        types.join(", ")
    };
    Some(tr.t_args(
        "claude.notify.stop_failure_background_message",
        &[&target_surface.to_string(), &error, &detail],
    ))
}

/// 백그라운드 대기 기록을 읽는다. 없거나 해석할 수 없으면 `None` 이다.
fn background_wait<H: HostCall>(host: &H, target_surface: u32) -> Option<Value> {
    let raw = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": target_surface, "key": crate::hook::BACKGROUND_WAIT_META_KEY }),
        )
        .ok()?
        .get("value")?
        .as_str()?
        .to_string();
    serde_json::from_str(&raw)
        .inspect_err(|e| tracing::warn!("claude background wait meta is not JSON ({e}): {raw}"))
        .ok()
}

/// 백그라운드 대기 중인 자식의 정지 알림 문구. 작업 종류와 대기 시작부터 지난 분을 적는다.
pub(crate) fn background_wait_message(
    tr: &Translator,
    target_surface: u32,
    wait: &Value,
    now_ms: u64,
) -> String {
    // `notice_types` 는 Monitor 를 뺀 종류다. 없으면 Monitor 를 가리지 않던 때의 대기 meta 다.
    let types: Vec<&str> = wait
        .get("notice_types")
        .or_else(|| wait.get("types"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // 종류가 없으면 대기 판정에 쓴 필드 이름을 적는다.
    let detail = if types.is_empty() {
        "waiting_on_background_work".to_string()
    } else {
        types.join(", ")
    };
    let since_ms = wait
        .get("since_ms")
        .and_then(Value::as_u64)
        .unwrap_or(now_ms);
    let minutes = now_ms.saturating_sub(since_ms) / 60_000;
    tr.t_args(
        "claude.notify.stalled_background_wait_message",
        &[&target_surface.to_string(), &detail, &minutes.to_string()],
    )
}

/// 출력 파일 감시가 기록한 조용한 작업의 문구. 같은 대기(시작 시각이 같음)의 기록이 없으면 `None` 이다.
pub(crate) fn quiet_task_message<H: HostCall>(
    tr: &Translator,
    host: &H,
    target_surface: u32,
    wait: &Value,
) -> Option<String> {
    let raw = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": target_surface, "key": crate::task_watch::BACKGROUND_QUIET_META_KEY }),
        )
        .ok()?
        .get("value")?
        .as_str()?
        .to_string();
    let quiet: Value = serde_json::from_str(&raw).ok()?;
    if quiet.get("since_ms")? != wait.get("since_ms")? {
        return None;
    }
    // 명령이 긴 셸 작업도 한 줄에 들어가도록 이름마다 앞부분만 적는다.
    let labels: Vec<String> = quiet
        .get("labels")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(|l| {
            let head: String = l.chars().take(QUIET_LABEL_CHARS).collect();
            if head.len() < l.len() {
                format!("{head}…")
            } else {
                head
            }
        })
        .collect();
    if labels.is_empty() {
        return None;
    }
    let minutes = quiet.get("quiet_ms")?.as_u64()? / 60_000;
    Some(tr.t_args(
        "claude.notify.stalled_background_task_quiet_message",
        &[
            &target_surface.to_string(),
            &labels.join(", "),
            &minutes.to_string(),
        ],
    ))
}

/// 조용한 작업 이름 하나에 적는 최대 글자 수.
const QUIET_LABEL_CHARS: usize = 80;

/// 정지 알림 문구와 그 종류. 화면의 오류 줄을 알림에 덧붙이며 조회에 실패하면 힌트를 생략한다.
/// 자식이 백그라운드 작업을 기다리는 중이면 대기 문구를 쓴다.
pub(crate) fn notify_error_message<H: HostCall>(
    tr: &Translator,
    host: &H,
    target_surface: u32,
) -> (NoticeKind, String) {
    if let Some(wait) = background_wait(host, target_surface) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if let Some(line) = quiet_task_message(tr, host, target_surface, &wait) {
            return (NoticeKind::StalledBackgroundTaskQuiet, line);
        }
        return (
            NoticeKind::StalledBackgroundWait,
            background_wait_message(tr, target_surface, &wait, now_ms),
        );
    }
    // 같은 이벤트로 들어오는 두 경우를 스캐너가 발화 직전에 정한 판정으로 구분한다. 판정이 없으면(이전 플러그인이
    // 남긴 기록) 지금 화면의 오류 줄로 정한다.
    let wording = stall_wording(host, target_surface)
        .unwrap_or_else(|| crate::error_scan::StallWording::read(host, target_surface));
    let error_line = wording.error_line;
    let (kind, key) = if error_line.is_some() {
        (NoticeKind::Stalled, "claude.notify.stalled_message")
    } else {
        (
            NoticeKind::StalledNoError,
            "claude.notify.stalled_no_error_message",
        )
    };
    let mut message = tr.t_args(key, &[&target_surface.to_string()]);
    if let Some(line) = error_line {
        message.push_str(&tr.t_args("claude.notify.stalled_hint", &[&line]));
    }
    (kind, message)
}

/// 스캐너가 남긴 정지 문구 판정. 종류가 없으면 `None` 이다.
fn stall_wording<H: HostCall>(
    host: &H,
    target_surface: u32,
) -> Option<crate::error_scan::StallWording> {
    let raw = crate::stop_pairing::meta_value(
        host,
        target_surface,
        crate::error_scan::STALL_NOTICE_META_KEY,
    )?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let error_line = match v.get("kind")?.as_str()? {
        crate::error_scan::STALL_KIND_ERROR => Some(
            v.get("hint")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        crate::error_scan::STALL_KIND_NO_ERROR => None,
        _ => return None,
    };
    Some(crate::error_scan::StallWording { error_line })
}

/// 대상 터미널이 조회되면 완료 훅 3개를 다시 등록한다. 조회 실패 시 생략한다.
/// 터미널의 존재 여부만 확인하므로 자식 프로세스가 실행 중이라는 뜻은 아니다.
pub(crate) fn rearm_if_still_alive<H: HostCall>(
    host: &H,
    caller_surface: u32,
    target_surface: u32,
    command_name: &str,
) {
    if surface_is_alive(host, target_surface) {
        register_notify_hooks(host, caller_surface, target_surface, command_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr() -> Translator {
        Translator::load(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang"),
            "en",
        )
    }

    /// surface meta 와 상태 조회만 답하는 mock 호스트.
    struct StateHost {
        state: &'static str,
        meta: Vec<(&'static str, String)>,
    }

    impl HostCall for StateHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            Ok(match method {
                "terminal.state" => json!({ "state": self.state }),
                "surface.meta.get" => self
                    .meta
                    .iter()
                    .find(|(k, _)| params["key"] == *k)
                    .map(|(_, v)| json!({ "value": v }))
                    .unwrap_or_else(|| json!({})),
                _ => json!({}),
            })
        }
    }

    fn waiting_host(state: &'static str) -> StateHost {
        StateHost {
            state,
            meta: vec![
                (crate::hook::STOP_FAILURE_META_KEY, "overloaded".to_string()),
                (
                    crate::hook::BACKGROUND_WAIT_META_KEY,
                    r#"{"since_ms":1,"tasks":1,"types":["shell"]}"#.to_string(),
                ),
            ],
        }
    }

    /// 오류 힌트는 idle 완료 줄에만 붙는다. 대기 중 기록된 오류가 다른 상태의 줄에 붙지 않는다.
    #[test]
    fn the_stop_failure_hint_is_added_to_idle_lines_only() {
        let tr = tr();
        let hint = "waiting for input";
        let (kind, line) = notify_done_line(&tr, &waiting_host("idle"), "spawn", 5);
        assert!(line.contains(hint), "{line}");
        assert_eq!(kind, NoticeKind::StopFailure);
        for state in ["active", "needs_input", "exited", "stale"] {
            let (_, line) = notify_done_line(&tr, &waiting_host(state), "spawn", 5);
            assert!(!line.contains(hint), "{state}: {line}");
            assert!(!line.contains("overloaded"), "{state}: {line}");
        }
    }

    /// 완료 줄의 타입은 대상 상태로 정한다. `exited` 는 kill 로 끝난 경우도 error 이고, 힌트 없는 idle·needs_input 은 info 다.
    #[test]
    fn a_done_line_is_an_error_for_an_exit_or_an_api_error_and_info_otherwise() {
        let tr = tr();
        let plain = |state| StateHost {
            state,
            meta: Vec::new(),
        };
        let kind = |host: &StateHost| notify_done_line(&tr, host, "tell", 5).0;
        assert_eq!(kind(&plain("idle")), NoticeKind::StateChanged);
        assert_eq!(kind(&plain("needs_input")), NoticeKind::StateChanged);
        assert_eq!(kind(&plain("exited")), NoticeKind::Exited);
        assert_eq!(kind(&waiting_host("exited")), NoticeKind::Exited);
        assert_eq!(kind(&waiting_host("idle")), NoticeKind::StopFailure);
    }

    /// 쓰기 함수는 종류의 타입 표지로 줄을 시작한다.
    #[test]
    fn the_written_line_starts_with_the_type_tag() {
        let tr = tr();
        crate::notice::test_log::take();
        handle_notify_done(
            &Mutex::new(ErrorScanner::new()),
            &plain_exited(),
            &json!({ "caller_surface": 3, "target_surface": 5, "command": "tell" }),
            &tr,
        )
        .expect("notify-done");
        let lines = crate::notice::test_log::take();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].1.starts_with("[error] surface 5: "), "{lines:?}");
    }

    /// 대기가 아닌 정지는 스캐너가 발화 직전에 정한 종류와 오류 줄을 쓴다. 그 뒤 화면이 바뀌어도 판정이 같다.
    #[test]
    fn a_stall_uses_the_kind_the_scanner_recorded() {
        let tr = tr();
        let recorded = |value: &str| StateHost {
            state: "active",
            meta: vec![(crate::error_scan::STALL_NOTICE_META_KEY, value.to_string())],
        };
        let (kind, line) = notify_error_message(
            &tr,
            &recorded(r#"{"at_ms":1,"kind":"stalled","hint":"API Error: overloaded"}"#),
            5,
        );
        assert_eq!(kind, NoticeKind::Stalled);
        assert!(line.contains("API Error: overloaded"), "{line}");
        let (kind, _) = notify_error_message(
            &tr,
            &recorded(r#"{"at_ms":1,"kind":"stalled_no_error"}"#),
            5,
        );
        assert_eq!(kind, NoticeKind::StalledNoError);
    }

    fn plain_exited() -> StateHost {
        StateHost {
            state: "exited",
            meta: Vec::new(),
        }
    }

    /// 정지 알림의 종류는 Monitor 를 뺀 `notice_types` 다. 옛 대기 meta 는 `types` 를 읽는다.
    #[test]
    fn the_wait_notice_names_only_the_tasks_it_is_about() {
        let tr = tr();
        let line = background_wait_message(
            &tr,
            5,
            &json!({"since_ms":0,"types":["shell","subagent"],"notice_types":["subagent"]}),
            600_000,
        );
        assert!(line.contains("(subagent)"), "{line}");
        let old =
            background_wait_message(&tr, 5, &json!({"since_ms":0,"types":["shell"]}), 600_000);
        assert!(old.contains("(shell)"), "{old}");
    }

    /// 백그라운드 대기 중인 StopFailure 는 오류와 작업 종류를 적은 줄 하나를 만든다.
    /// 대기 기록이 없으면 턴이 끝난 것이므로 줄을 만들지 않는다.
    #[test]
    fn a_stop_failure_notice_is_written_only_while_waiting_on_background_work() {
        let tr = tr();
        let line = stop_failure_notice(&tr, &waiting_host("active"), 5).expect("대기 중");
        assert!(line.starts_with("surface 5:"), "{line}");
        assert!(
            line.contains("overloaded") && line.contains("shell"),
            "{line}"
        );
        assert!(!line.contains("waiting for input"), "{line}");
        let ended = StateHost {
            state: "idle",
            meta: vec![(crate::hook::STOP_FAILURE_META_KEY, "overloaded".to_string())],
        };
        assert_eq!(stop_failure_notice(&tr, &ended, 5), None);
    }
}
