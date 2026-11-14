//! 같은 터미널에서 Claude를 종료하고 저장된 세션 ID로 재시작한다.
//!
//! 기본 5초 대기 후 Ctrl+C를 0.5초 간격으로 4번 보내고 `claude -r`을 전송한다.
//! 요청 때 읽은 전경 프로세스 이름을 기준으로 이탈·복귀를 확인한다.
//! 이 확인은 프로세스 이름 비교이며 프로세스 신원이나 셸 입력의 안전을 보장하지 않는다.
//! 안내문은 셸 인자로 넣지 않고 TUI 복귀를 확인한 뒤 terminal.tell로 보낸다.
//!
//! 세션 ID는 종료 훅이 메타데이터를 지우기 전에 읽어 둔다.
//! 프로필 파일은 종료 절차 전에 읽기·JSON 해석을 검사한다. 부착 정보는 터미널
//! 메타데이터와 복원용 파일에 기록하며, 무인자 reboot에서 승계하고 --clear-profile로 뗀다.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_agent_common::reboot::{
    build_notice, ensure_submitted, is_safe_session_id, parse_options, screen_contains,
};
use tasty_plugin_sdk::{HostHandle, IpcMethodError, i18n::Translator};

use crate::handlers::require_target_surface;

/// 진행 중 집합에 넣을 때 poison이면 요청을 거부한다.
/// 제거할 때는 내부 값을 사용해 ID가 남아 이후 요청을 계속 막는 일을 피한다.
const INFLIGHT_WHAT: &str = "the claude reboot in-flight set";
static INFLIGHT_POISON_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ctrl+C 전송 횟수 / 간격.
const CTRL_C_COUNT: u32 = 4;
const CTRL_C_INTERVAL: Duration = Duration::from_millis(500);
/// 전경 프로세스 폴링 간격.
const FG_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Ctrl+C 후 claude 종료(전경 이탈) 대기 한도.
const EXIT_WAIT: Duration = Duration::from_secs(5);
/// resume 명령 후 claude 복귀(전경 재진입) 대기 한도.
const RETURN_WAIT: Duration = Duration::from_secs(15);
/// 전경 복귀 후 TUI 초기화를 기다리는 시간.
const TUI_READY_GRACE: Duration = Duration::from_secs(3);
/// 안내문 제출 횟수·재시도 간격·화면 확인 대기. 초기화 중 입력 유실에 대비한다.
const NOTICE_ATTEMPTS: u32 = 4;
const NOTICE_RETRY_INTERVAL: Duration = Duration::from_secs(3);
const NOTICE_VERIFY_DELAY: Duration = Duration::from_millis(1500);
/// 안내문이 화면에 보이는지 확인할 때 찾는 선두 문구.
const NOTICE_SNIPPET: &str = "tasty claude reboot";

/// 경로로 부착한 프로필. 다음 무인자 reboot에서 승계한다.
pub(crate) const PROFILE_META_KEY: &str = "claude-session-profile";
/// 이름으로 부착한 프로필. 경로 메타데이터와 둘 중 하나만 기록한다.
/// 다음 무인자 reboot 때 이름을 다시 해석해 등록 내용의 변경을 반영한다.
pub(crate) const PROFILE_NAMES_META_KEY: &str = "claude-session-profile-names";

/// 검증과 상태 조회를 마친 뒤 재시작 절차를 별도 스레드에 맡기고 응답한다.
pub(crate) fn handle_reboot(
    inflight: &Arc<Mutex<HashSet<u32>>>,
    host: &HostHandle,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let surface_id = require_target_surface(params, tr)?;
    reboot_surface(inflight, host, surface_id, params, data_dir, tr)
}

/// reboot과 child-profile이 대상 터미널을 정한 뒤 사용하는 공통 처리기.
/// 같은 inflight 집합으로 대상별 중복 실행을 거부한다.
pub(crate) fn reboot_surface(
    inflight: &Arc<Mutex<HashSet<u32>>>,
    host: &HostHandle,
    surface_id: u32,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let (delay_secs, extra_prompt) = parse_options(params);
    // 잘못된 프로필은 호스트 조회·메타데이터 변경·Ctrl+C 전송 전에 거부한다.
    // preflight_profile_runs_before_any_host_roundtrip이 이 호출 순서를 검사한다.
    let (profile_action, preresolved) = preflight_profile(params, data_dir, tr)?;

    // 요청 시점 캡처 (session-end 가 meta 를 지우기 전).
    let session_id = fetch_session_id(host, surface_id, tr)?;
    if !is_safe_session_id(&session_id) {
        return Err(IpcMethodError::new(tr.t_args(
            "claude.reboot.malformed_session_id",
            &[&surface_id.to_string(), &format!("{session_id:?}")],
        )));
    }

    if crate::session_options::stale_start(host, surface_id, &session_id, tr)? {
        return Err(IpcMethodError::invalid_params(&tr.t_fmt(
            "claude.params.invalid_session_option",
            crate::session_options::META_KEY,
        )));
    }

    // 승계할 프로필도 검증에 실패하면 종료 절차를 시작하지 않는다.
    let profile_file = resolve_and_apply_profile(
        host,
        surface_id,
        &session_id,
        &profile_action,
        preresolved,
        data_dir,
        tr,
    )?;

    // 이번 호출의 승인 정책은 restore.command에 저장하지 않는다.
    // docs/plugins/claude/index.md#승인-정책---permission-mode 참고.
    let permission_mode =
        crate::handlers::resolve_permission_mode(host, params, profile_file.as_deref(), tr)?;

    let options = crate::session_options::load(host, surface_id, data_dir, &session_id, tr)?;
    let command = resume_with_options(
        &session_id,
        profile_file.as_deref(),
        permission_mode.as_deref(),
        &options,
    );

    let Some(baseline) = query_foreground(host, surface_id) else {
        return Err(IpcMethodError::new(tr.t_fmt(
            "claude.reboot.no_foreground_process",
            &surface_id.to_string(),
        )));
    };

    {
        let mut set = inflight.lock().map_err(|e| {
            IpcMethodError::new(tr.t_fmt("claude.reboot.lock_poisoned", &e.to_string()))
        })?;
        if !set.insert(surface_id) {
            return Err(IpcMethodError::new(tr.t_fmt(
                "claude.reboot.already_in_progress",
                &surface_id.to_string(),
            )));
        }
    }

    // 스레드에 번역기를 빌려줄 수 없으므로 해석한 문자열을 넘긴다.
    let notice_base = tr.t("claude.reboot.notice").to_string();

    let thread_host = host.clone();
    let thread_inflight = inflight.clone();
    let spawned = thread::Builder::new()
        .name(format!("claude-reboot-s{surface_id}"))
        .spawn(move || {
            run_reboot_sequence(
                &thread_host,
                surface_id,
                delay_secs,
                &baseline,
                &command,
                extra_prompt.as_deref(),
                profile_file.as_deref(),
                &notice_base,
            );
            tasty_utils::poison::recover_mutex(
                thread_inflight.lock(),
                INFLIGHT_WHAT,
                &INFLIGHT_POISON_REPORTED,
            )
            .remove(&surface_id);
        });
    if let Err(e) = spawned {
        tasty_utils::poison::recover_mutex(
            inflight.lock(),
            INFLIGHT_WHAT,
            &INFLIGHT_POISON_REPORTED,
        )
        .remove(&surface_id);
        return Err(IpcMethodError::new(
            tr.t_fmt("claude.reboot.spawn_thread_failed", &e.to_string()),
        ));
    }

    Ok(json!({
        "surface_id": surface_id,
        "session_id": session_id,
        "reboot_in_secs": delay_secs,
    }))
}

/// `--profile-file` / `--profile` / `--clear-profile` 로 요청된 부착 상태 변경.
/// `Keep` 은 셋 다 없는 기본 호출 — 기존에 부착된 프로필(있으면)을 그대로 승계한다.
#[derive(Debug, PartialEq)]
pub(crate) enum ProfileOption {
    /// 인자 없음 — surface meta 에 부착된 값을 그대로 승계(없으면 프로필 없음).
    Keep,
    /// --profile-file 경로. CLI에서는 path_kind="file"로 정규화한다.
    AttachPath(String),
    /// --profile 이름 목록. 여러 이름이면 병합한다.
    AttachNames(String),
    /// `--clear-profile` — 부착된 프로필을 뗀다(meta 삭제).
    Clear,
}

/// clear-profile을 우선하며, profile-file과 profile의 동시 지정은 거부한다.
pub(crate) fn parse_profile_option(
    params: &Value,
    tr: &Translator,
) -> Result<ProfileOption, IpcMethodError> {
    let clear = params
        .get("clear_profile")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if clear {
        return Ok(ProfileOption::Clear);
    }
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
        (Some(p), None) => Ok(ProfileOption::AttachPath(p.to_string())),
        (None, Some(n)) => Ok(ProfileOption::AttachNames(n.to_string())),
        (None, None) => Ok(ProfileOption::Keep),
    }
}

/// 호스트 호출 전에 명시한 프로필 인자의 충돌·이름·파일을 검증한다.
/// 무인자 승계는 메타데이터 조회가 필요하므로 여기서는 경로를 정하지 않는다.
pub(crate) fn preflight_profile(
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<(ProfileOption, Option<String>), IpcMethodError> {
    let action = parse_profile_option(params, tr)?;
    let candidate = match &action {
        ProfileOption::AttachPath(path) => Some(path.clone()),
        ProfileOption::AttachNames(names) => Some(resolve_names_to_path(data_dir, names, tr)?),
        ProfileOption::Clear | ProfileOption::Keep => None,
    };
    if let Some(path) = &candidate {
        validate_profile_file(path, tr)?;
    }
    Ok((action, candidate))
}

/// 이번 재시작에 사용할 프로필 경로를 정하고 부착 기록을 갱신한다.
/// 명시한 프로필은 preflight에서 검증했다. 승계하는 프로필은 여기서 검증한다.
/// 이름 메타데이터가 있으면 다시 해석하고, 없으면 경로 메타데이터를 읽는다.
/// 검증이 끝난 뒤에만 메타데이터 쓰기를 시도한다.
fn resolve_and_apply_profile(
    host: &HostHandle,
    surface_id: u32,
    session_id: &str,
    action: &ProfileOption,
    preresolved: Option<String>,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Option<String>, IpcMethodError> {
    let candidate = match action {
        ProfileOption::Clear => None,
        ProfileOption::AttachPath(_) | ProfileOption::AttachNames(_) => preresolved,
        ProfileOption::Keep => {
            let candidate = match fetch_profile_names_meta(host, surface_id) {
                Some(names) => Some(resolve_names_to_path(data_dir, &names, tr)?),
                None => fetch_profile_meta(host, surface_id),
            };
            if let Some(path) = &candidate {
                validate_profile_file(path, tr)?;
            }
            candidate
        }
    };

    // 메타데이터와 복원용 파일에 같은 변경을 적용한다. 각각의 쓰기는 실패할 수 있다.
    match action {
        ProfileOption::AttachPath(path) => {
            set_profile_meta(host, surface_id, path);
            unset_profile_names_meta(host, surface_id);
        }
        ProfileOption::AttachNames(names) => {
            set_profile_names_meta(host, surface_id, names);
            unset_profile_meta(host, surface_id);
        }
        ProfileOption::Clear => {
            unset_profile_meta(host, surface_id);
            unset_profile_names_meta(host, surface_id);
        }
        ProfileOption::Keep => {}
    }
    apply_record_update(data_dir, session_id, &record_update_for(action));

    Ok(candidate)
}

/// 메타데이터 변경에 대응하는 복원용 기록 변경.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RecordUpdate {
    Store(crate::profile_attach::AttachRecord),
    Remove,
    /// 승계(`Keep`)는 부착 상태를 바꾸지 않으므로 기록도 그대로 둔다.
    Keep,
}

pub(crate) fn record_update_for(action: &ProfileOption) -> RecordUpdate {
    use crate::profile_attach::AttachRecord;
    match action {
        // 복원 때 다시 해석하도록 이름 자체를 남긴다.
        ProfileOption::AttachNames(names) => {
            RecordUpdate::Store(AttachRecord::Names(names.clone()))
        }
        ProfileOption::AttachPath(path) => RecordUpdate::Store(AttachRecord::Path(path.clone())),
        ProfileOption::Clear => RecordUpdate::Remove,
        ProfileOption::Keep => RecordUpdate::Keep,
    }
}

fn apply_record_update(data_dir: Option<&Path>, session_id: &str, update: &RecordUpdate) {
    match update {
        RecordUpdate::Store(record) => crate::profile_attach::store(data_dir, session_id, record),
        RecordUpdate::Remove => crate::profile_attach::remove(data_dir, session_id),
        RecordUpdate::Keep => {}
    }
}

fn resolve_names_to_path(
    data_dir: Option<&Path>,
    names: &str,
    tr: &Translator,
) -> Result<String, IpcMethodError> {
    crate::profile::resolve_names(data_dir, names, tr)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| crate::profile::to_ipc_err(e, tr))
}

/// 종료 절차를 시작하기 전에 파일을 읽고 JSON으로 해석할 수 있는지 확인한다.
pub(crate) fn validate_profile_file(path: &str, tr: &Translator) -> Result<(), IpcMethodError> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        IpcMethodError::new(tr.t_args(
            "claude.reboot.profile_file_not_readable",
            &[path, &e.to_string()],
        ))
    })?;
    serde_json::from_str::<Value>(&contents).map_err(|e| {
        IpcMethodError::new(tr.t_args(
            "claude.reboot.profile_file_not_json",
            &[path, &e.to_string()],
        ))
    })?;
    Ok(())
}

/// 부착된 프로필 경로를 읽는다. 없거나 조회에 실패하면 None을 반환한다.
fn fetch_profile_meta<H: HostCall>(host: &H, surface_id: u32) -> Option<String> {
    host.call(
        "surface.meta.get",
        json!({ "surface_id": surface_id, "key": PROFILE_META_KEY }),
    )
    .ok()
    .and_then(|r| r.get("value").and_then(|v| v.as_str()).map(String::from))
    .filter(|s| !s.is_empty())
}

fn set_profile_meta(host: &HostHandle, surface_id: u32, path: &str) {
    if let Err(e) = host.call(
        "surface.meta.set",
        json!({ "surface_id": surface_id, "key": PROFILE_META_KEY, "value": path }),
    ) {
        tracing::warn!("claude reboot s{surface_id}: failed to record attached profile: {e}");
    }
}

fn unset_profile_meta(host: &HostHandle, surface_id: u32) {
    if let Err(e) = host.call(
        "surface.meta.unset",
        json!({ "surface_id": surface_id, "key": PROFILE_META_KEY }),
    ) {
        tracing::warn!("claude reboot s{surface_id}: failed to clear attached profile: {e}");
    }
}

fn fetch_profile_names_meta<H: HostCall>(host: &H, surface_id: u32) -> Option<String> {
    host.call(
        "surface.meta.get",
        json!({ "surface_id": surface_id, "key": PROFILE_NAMES_META_KEY }),
    )
    .ok()
    .and_then(|r| r.get("value").and_then(|v| v.as_str()).map(String::from))
    .filter(|s| !s.is_empty())
}

fn set_profile_names_meta(host: &HostHandle, surface_id: u32, names: &str) {
    if let Err(e) = host.call(
        "surface.meta.set",
        json!({ "surface_id": surface_id, "key": PROFILE_NAMES_META_KEY, "value": names }),
    ) {
        tracing::warn!("claude reboot s{surface_id}: failed to record attached profile names: {e}");
    }
}

fn unset_profile_names_meta(host: &HostHandle, surface_id: u32) {
    if let Err(e) = host.call(
        "surface.meta.unset",
        json!({ "surface_id": surface_id, "key": PROFILE_NAMES_META_KEY }),
    ) {
        tracing::warn!("claude reboot s{surface_id}: failed to clear attached profile names: {e}");
    }
}

/// profile_current에 쓸 부착 기록을 읽는다. 이름을 우선하고 없으면 경로를 사용한다.
pub(crate) struct AttachedProfile {
    pub names: Option<String>,
    pub path: Option<String>,
}

pub(crate) fn attached_profile_summary<H: HostCall>(host: &H, surface_id: u32) -> AttachedProfile {
    let names = fetch_profile_names_meta(host, surface_id);
    let path = if names.is_none() {
        fetch_profile_meta(host, surface_id)
    } else {
        None
    };
    AttachedProfile { names, path }
}

/// 세션 ID 메타데이터를 읽는다. 조회에 실패하거나 값이 없으면 오류를 반환한다.
fn fetch_session_id(
    host: &HostHandle,
    surface_id: u32,
    tr: &Translator,
) -> Result<String, IpcMethodError> {
    let resp = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": surface_id, "key": "claude-session-id" }),
        )
        .map_err(IpcMethodError::from)?;
    let session_id = resp
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return Err(IpcMethodError::new(
            tr.t_fmt("claude.reboot.no_active_session", &surface_id.to_string()),
        ));
    }
    Ok(session_id)
}

/// 셸에 보낼 resume 명령과 제출 문자(\r)를 만든다.
/// 환경 변수는 PTY에 이미 주입돼 있으므로 POSIX 전용 인라인 환경 문법을 쓰지 않는다.
/// 프로필은 인라인 JSON 대신 OS별 규칙으로 인용한 파일 경로로 전달한다
/// (Linux·macOS는 큰따옴표, Windows는 Git Bash 작은따옴표 규칙).
pub(crate) fn resume_command(
    session_id: &str,
    profile_file: Option<&str>,
    permission_mode: Option<&str>,
) -> String {
    let mode = match permission_mode {
        Some(m) => format!(" --permission-mode {m}"),
        None => String::new(),
    };
    format!("{}{mode}\r", resume_command_line(session_id, profile_file))
}

/// Reboot captures these validated options before SessionEnd can clear the live metadata.
pub(crate) fn resume_with_options(
    session_id: &str,
    profile_file: Option<&str>,
    permission_mode: Option<&str>,
    options: &crate::session_options::SessionOptions,
) -> String {
    format!(
        "{}{}\r",
        resume_command(session_id, profile_file, permission_mode).trim_end_matches('\r'),
        options.flags(),
    )
}

/// 복원에도 사용하는 resume 명령 본문. 제출 문자는 포함하지 않는다.
/// 승인 정책은 이번 호출에만 적용하므로 이 문자열에는 저장하지 않는다.
pub(crate) fn resume_command_line(session_id: &str, profile_file: Option<&str>) -> String {
    match profile_file {
        Some(path) => format!(
            "claude -r {session_id} --settings {}",
            crate::session_options::quote_path(path)
        ),
        None => format!("claude -r {session_id}"),
    }
}

/// 지연 뒤 전경 이름에 따라 Ctrl+C 전송 여부를 정한다.
#[derive(Debug, PartialEq)]
pub(crate) enum AfterDelay {
    /// 요청 때의 전경 이름과 같으면 Ctrl+C를 보낸다.
    SendCtrlC,
    /// 전경 이름이 바뀌었으면 resume으로 넘어간다.
    SkipToResume,
}

pub(crate) fn after_delay_action(current: &str, baseline: &str) -> AfterDelay {
    if current == baseline {
        AfterDelay::SendCtrlC
    } else {
        AfterDelay::SkipToResume
    }
}

/// 별도 스레드에서 재시작 절차를 수행한다. 각 단계가 실패하면 경고 후 중단한다.
#[allow(clippy::too_many_arguments)]
fn run_reboot_sequence(
    host: &HostHandle,
    surface_id: u32,
    delay_secs: u64,
    baseline: &str,
    command: &str,
    extra_prompt: Option<&str>,
    profile_file: Option<&str>,
    notice_base: &str,
) {
    thread::sleep(Duration::from_secs(delay_secs));

    let Some(current) = query_foreground(host, surface_id) else {
        tracing::warn!(
            "claude reboot s{surface_id}: foreground unavailable before Ctrl+C; aborting"
        );
        return;
    };

    if !kill_or_skip(host, surface_id, baseline, &current) {
        return;
    }

    if !resume_and_wait(host, surface_id, baseline, command, profile_file) {
        return;
    }
    thread::sleep(TUI_READY_GRACE);

    if !deliver_notice(
        host,
        surface_id,
        baseline,
        &build_notice(notice_base, extra_prompt),
    ) {
        tracing::warn!(
            "claude reboot s{surface_id}: notice not confirmed on screen after {NOTICE_ATTEMPTS} attempts"
        );
    }
}

/// 현재 전경 이름이 기준과 같을 때만 Ctrl+C 종료 절차를 실행한다.
fn kill_or_skip(host: &HostHandle, surface_id: u32, baseline: &str, current: &str) -> bool {
    match after_delay_action(current, baseline) {
        AfterDelay::SendCtrlC => kill_claude_via_ctrlc(host, surface_id, baseline),
        AfterDelay::SkipToResume => {
            tracing::info!(
                "claude reboot s{surface_id}: foreground already '{current}' (was '{baseline}') — skipping Ctrl+C"
            );
            true
        }
    }
}

/// Ctrl+C를 보낸 뒤 전경 이름이 기준에서 바뀌는지 확인한다.
fn kill_claude_via_ctrlc(host: &HostHandle, surface_id: u32, baseline: &str) -> bool {
    for _ in 0..CTRL_C_COUNT {
        if let Err(e) = host.call(
            "surface.send_combo",
            json!({ "surface_id": surface_id, "key": "c", "modifiers": ["ctrl"] }),
        ) {
            tracing::warn!("claude reboot s{surface_id}: send_combo failed: {e} — aborting");
            return false;
        }
        thread::sleep(CTRL_C_INTERVAL);
    }
    if !poll_foreground(host, surface_id, EXIT_WAIT, |name| name != baseline) {
        tracing::warn!(
            "claude reboot s{surface_id}: foreground name unchanged after {CTRL_C_COUNT}x Ctrl+C; resume command not sent"
        );
        return false;
    }
    true
}

/// resume 명령을 보낸 뒤 전경 이름이 기준으로 돌아오는지 확인한다. 실패하면 중단한다.
fn resume_and_wait(
    host: &HostHandle,
    surface_id: u32,
    baseline: &str,
    command: &str,
    profile_file: Option<&str>,
) -> bool {
    crate::stop_pairing::record_settings_file(host, surface_id, profile_file);
    if let Err(e) = host.call(
        "surface.send",
        json!({
            "surface_id": surface_id,
            "text": command,
        }),
    ) {
        tracing::warn!("claude reboot s{surface_id}: resume send failed: {e}");
        return false;
    }

    if !poll_foreground(host, surface_id, RETURN_WAIT, |name| name == baseline) {
        tracing::warn!(
            "claude reboot s{surface_id}: claude did not return to foreground within {}s — resume sent but notice skipped",
            RETURN_WAIT.as_secs()
        );
        return false;
    }
    true
}

/// 매 시도 전 전경 이름을 확인하고 안내문이 화면에 보일 때까지 제한된 횟수로 재시도한다.
fn deliver_notice(host: &HostHandle, surface_id: u32, baseline: &str, notice: &str) -> bool {
    for attempt in 1..=NOTICE_ATTEMPTS {
        match try_deliver_notice_once(host, surface_id, baseline, notice) {
            NoticeAttempt::Confirmed => return true,
            NoticeAttempt::Aborted => return false,
            NoticeAttempt::NotYetVisible => {
                tracing::info!(
                    "claude reboot s{surface_id}: notice attempt {attempt}/{NOTICE_ATTEMPTS} not visible yet — retrying"
                );
                thread::sleep(NOTICE_RETRY_INTERVAL);
            }
        }
    }
    // 마지막 시도 후 늦게 표시됐을 수 있어 한 번 더 확인한다.
    if screen_contains(host, surface_id, NOTICE_SNIPPET) {
        ensure_submitted(host, surface_id, "claude");
        return true;
    }
    false
}

/// 안내 프롬프트 제출 1회 시도 결과.
enum NoticeAttempt {
    /// 화면에서 안내문을 확인하고 제출 보완을 시도했다.
    Confirmed,
    /// 전경 변경/tell 실패 — 시퀀스 전체를 중단해야 함.
    Aborted,
    /// 제출은 했으나 아직 화면에 안 보임 — 재시도 대상.
    NotYetVisible,
}

/// 전경이 여전히 baseline(claude) 인지 확인 후 안내 프롬프트를 `terminal.tell` 로
/// 제출하고 화면에 나타났는지 검사한다.
fn try_deliver_notice_once(
    host: &HostHandle,
    surface_id: u32,
    baseline: &str,
    notice: &str,
) -> NoticeAttempt {
    match query_foreground(host, surface_id) {
        Some(name) if name == baseline => {}
        other => {
            tracing::warn!(
                "claude reboot s{surface_id}: foreground changed to {other:?} before notice — aborting"
            );
            return NoticeAttempt::Aborted;
        }
    }
    if let Err(e) = host.call(
        "terminal.tell",
        json!({ "surface": surface_id, "text": notice }),
    ) {
        tracing::warn!("claude reboot s{surface_id}: notice tell failed: {e}");
        return NoticeAttempt::Aborted;
    }
    thread::sleep(NOTICE_VERIFY_DELAY);
    if screen_contains(host, surface_id, NOTICE_SNIPPET) {
        ensure_submitted(host, surface_id, "claude");
        NoticeAttempt::Confirmed
    } else {
        NoticeAttempt::NotYetVisible
    }
}

/// `surface.foreground_process` 1회 조회. 실패/이름 없음 → None.
fn query_foreground(host: &HostHandle, surface_id: u32) -> Option<String> {
    let resp = host
        .call(
            "surface.foreground_process",
            json!({ "surface_id": surface_id }),
        )
        .ok()?;
    resp.get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 전경 이름을 폴링한다. 조회 실패 시 중단하고, 조건 불일치 시 경과 시간을 확인한다.
fn poll_foreground(
    host: &HostHandle,
    surface_id: u32,
    timeout: Duration,
    pred: impl Fn(&str) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match query_foreground(host, surface_id) {
            Some(name) if pred(&name) => return true,
            Some(_) => {}
            None => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(FG_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_translator() -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, "en")
    }

    /// 잘못된 프로필이 첫 호스트 조회 전에 거부되는지 호출 순서를 검사한다.
    /// 이 시험은 실행 결과가 아니라 소스의 두 호출 위치를 비교한다.
    #[test]
    fn preflight_profile_runs_before_any_host_roundtrip() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/reboot.rs"),
        )
        .expect("자기 소스를 못 읽었다")
        .replace("\r\n", "\n");
        let sig = "pub(crate) fn reboot_surface(";
        let at = src
            .find(sig)
            .expect("`reboot_surface` 가 없다 — 이름이 바뀌었다");
        // 중괄호 균형으로 본문을 자른다(들여쓰기·rustfmt 스타일에 안 기댄다).
        let open = src[at..].find('{').expect("본문 여는 괄호가 없다") + at;
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in src[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[open..end.expect("본문 닫는 괄호를 못 찾았다")];
        let pre = body
            .find("preflight_profile(params")
            .expect("`reboot_surface` 가 `preflight_profile` 을 부르지 않는다 — 검증이 사라졌다");
        let host_call = body
            .find("fetch_session_id(host")
            .expect("`reboot_surface` 에서 첫 host 왕복(`fetch_session_id`)을 못 찾았다");
        assert!(
            pre < host_call,
            "프로필 사전 검증은 첫 호스트 조회보다 앞에 있어야 한다: preflight {pre}, host {host_call}"
        );
    }

    #[test]
    fn preflight_rejects_profile_and_profile_file_together() {
        let tr = test_translator();
        let dir = tempfile::tempdir().unwrap();
        let err = preflight_profile(
            &json!({ "profile": "probe", "profile_file": "/tmp/x.json" }),
            Some(dir.path()),
            &tr,
        )
        .unwrap_err();
        assert!(
            err.message.contains("mutually exclusive"),
            "{}",
            err.message
        );
    }

    #[test]
    fn preflight_rejects_an_unregistered_profile_name() {
        let tr = test_translator();
        let dir = tempfile::tempdir().unwrap();
        let err =
            preflight_profile(&json!({ "profile": "nosuch" }), Some(dir.path()), &tr).unwrap_err();
        assert!(err.message.contains("nosuch"), "{}", err.message);
    }

    #[test]
    fn preflight_resolves_a_registered_profile_name_to_a_real_file() {
        let tr = test_translator();
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.json");
        std::fs::write(&src, r#"{"hooks":{}}"#).unwrap();
        crate::profile::register(Some(dir.path()), "probe", &src).unwrap();

        let (action, resolved) =
            preflight_profile(&json!({ "profile": "probe" }), Some(dir.path()), &tr).unwrap();
        assert!(matches!(action, ProfileOption::AttachNames(ref n) if n == "probe"));
        let resolved = resolved.expect("registered name resolves to a merged file");
        assert!(Path::new(&resolved).is_file(), "{resolved}");
    }

    #[test]
    fn preflight_leaves_keep_unresolved_for_the_meta_lookup() {
        // 무인자 승계는 surface meta 를 읽어야 하므로 여기서 후보를 못 정한다.
        let tr = test_translator();
        let dir = tempfile::tempdir().unwrap();
        let (action, resolved) = preflight_profile(&json!({}), Some(dir.path()), &tr).unwrap();
        assert!(matches!(action, ProfileOption::Keep));
        assert_eq!(resolved, None);
    }

    /// 이름 부착은 복원 때 다시 해석할 이름을 기록해야 한다.
    #[test]
    fn attach_names_records_the_names_not_a_path() {
        assert_eq!(
            record_update_for(&ProfileOption::AttachNames("reviewer".into())),
            RecordUpdate::Store(crate::profile_attach::AttachRecord::Names(
                "reviewer".into()
            ))
        );
    }

    #[test]
    fn attach_path_records_the_path() {
        assert_eq!(
            record_update_for(&ProfileOption::AttachPath("/abs/p.json".into())),
            RecordUpdate::Store(crate::profile_attach::AttachRecord::Path(
                "/abs/p.json".into()
            ))
        );
    }

    /// clear-profile이 복원용 기록도 삭제하도록 지시하는지 확인한다.
    /// 이 시험은 호스트 메타데이터 삭제까지 실행하지 않는다.
    #[test]
    fn clear_removes_the_record() {
        assert_eq!(
            record_update_for(&ProfileOption::Clear),
            RecordUpdate::Remove
        );
    }

    #[test]
    fn keep_leaves_the_record_untouched() {
        assert_eq!(record_update_for(&ProfileOption::Keep), RecordUpdate::Keep);
    }

    /// 기록 갱신이 실제 파일에 반영되는지 — 부착 → 조회 왕복.
    #[test]
    fn applying_an_attach_update_round_trips_through_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        apply_record_update(
            Some(tmp.path()),
            "sess-1",
            &record_update_for(&ProfileOption::AttachNames("reviewer".into())),
        );
        assert_eq!(
            crate::profile_attach::load(Some(tmp.path()), "sess-1"),
            Some(crate::profile_attach::AttachRecord::Names(
                "reviewer".into()
            ))
        );
        apply_record_update(
            Some(tmp.path()),
            "sess-1",
            &record_update_for(&ProfileOption::Clear),
        );
        assert_eq!(
            crate::profile_attach::load(Some(tmp.path()), "sess-1"),
            None
        );
    }

    #[test]
    fn resume_command_is_plain_and_submits() {
        assert_eq!(
            resume_command("0e5cbdf4-32a1", None, None),
            "claude -r 0e5cbdf4-32a1\r"
        );
    }

    #[test]
    fn resume_command_with_profile_appends_quoted_settings_path() {
        assert_eq!(
            resume_command("0e5cbdf4-32a1", Some("/home/user/profile.json"), None),
            if cfg!(windows) {
                "claude -r 0e5cbdf4-32a1 --settings /home/user/profile.json\r"
            } else {
                "claude -r 0e5cbdf4-32a1 --settings \"/home/user/profile.json\"\r"
            }
        );
    }

    #[test]
    fn parse_profile_option_defaults_to_keep() {
        assert_eq!(
            parse_profile_option(&json!({ "surface_id": 1 }), &test_translator()).unwrap(),
            ProfileOption::Keep
        );
    }

    #[test]
    fn parse_profile_option_attach_path() {
        assert_eq!(
            parse_profile_option(&json!({ "profile_file": "/a/b.json" }), &test_translator())
                .unwrap(),
            ProfileOption::AttachPath("/a/b.json".to_string())
        );
    }

    #[test]
    fn parse_profile_option_attach_names() {
        assert_eq!(
            parse_profile_option(
                &json!({ "profile": "reviewer,sandbox" }),
                &test_translator()
            )
            .unwrap(),
            ProfileOption::AttachNames("reviewer,sandbox".to_string())
        );
    }

    #[test]
    fn parse_profile_option_path_and_names_together_is_rejected() {
        assert!(
            parse_profile_option(
                &json!({ "profile_file": "/a/b.json", "profile": "reviewer" }),
                &test_translator()
            )
            .is_err()
        );
    }

    #[test]
    fn parse_profile_option_clear_wins_over_attach() {
        assert_eq!(
            parse_profile_option(
                &json!({ "profile_file": "/a/b.json", "clear_profile": true }),
                &test_translator()
            )
            .unwrap(),
            ProfileOption::Clear
        );
    }

    #[test]
    fn parse_profile_option_clear_wins_over_names() {
        assert_eq!(
            parse_profile_option(
                &json!({ "profile": "reviewer", "clear_profile": true }),
                &test_translator()
            )
            .unwrap(),
            ProfileOption::Clear
        );
    }

    #[test]
    fn parse_profile_option_empty_profile_file_treated_as_keep() {
        assert_eq!(
            parse_profile_option(&json!({ "profile_file": "" }), &test_translator()).unwrap(),
            ProfileOption::Keep
        );
    }

    #[test]
    fn validate_profile_file_accepts_valid_json() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), r#"{"hooks":{}}"#).unwrap();
        assert!(validate_profile_file(tmp.path().to_str().unwrap(), &test_translator()).is_ok());
    }

    #[test]
    fn validate_profile_file_rejects_missing_file() {
        assert!(
            validate_profile_file("/no/such/tasty-profile-test.json", &test_translator()).is_err()
        );
    }

    #[test]
    fn validate_profile_file_rejects_broken_json() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), "{not valid json").unwrap();
        assert!(validate_profile_file(tmp.path().to_str().unwrap(), &test_translator()).is_err());
    }

    #[test]
    fn notice_without_extra_is_fixed_text() {
        let base = test_translator().t("claude.reboot.notice").to_string();
        assert_eq!(build_notice(&base, None), base);
    }

    #[test]
    fn notice_with_extra_appends_after_blank_line() {
        let base = test_translator().t("claude.reboot.notice").to_string();
        let n = build_notice(&base, Some("이어서 soak 돌려"));
        assert!(n.starts_with(&base));
        assert!(n.ends_with("\n\n이어서 soak 돌려"));
    }

    #[test]
    fn after_delay_same_foreground_sends_ctrl_c() {
        assert_eq!(after_delay_action("node", "node"), AfterDelay::SendCtrlC);
    }

    #[test]
    fn after_delay_changed_foreground_skips_to_resume() {
        assert_eq!(
            after_delay_action("cmd.exe", "node"),
            AfterDelay::SkipToResume
        );
    }
}
