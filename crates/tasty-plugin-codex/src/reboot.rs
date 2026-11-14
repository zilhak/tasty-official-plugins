//! 같은 터미널의 Codex를 재시작하고 저장된 세션 ID로 재개한다.
//!
//! Windows의 npm 실행 체인에서는 전경 이름을 읽기 어려워 화면 문구로 진행을 추정한다.
//! Ctrl+C 뒤에는 run codex resume, 재기동 뒤에는 OpenAI Codex의 등장 횟수가
//! 요청 시점보다 늘어나야 한다. 임의 출력도 일치할 수 있고 화면에서 지워질 수도 있어
//! 프로세스 종료·복귀를 확정하거나 입력의 안전을 보장하는 검사는 아니다.
//!
//! Tasty helper로 띄운 세션은 화면 문구 대신 helper가 기록한 세대 상태와 전경 프로세스로 종료·
//! 복귀를 판정한다([`crate::reboot_watch`]). 아래 화면 판정은 helper 세션이 아닐 때만 쓴다.
//! 응답은 접수 결과이며, 재시작의 성공·실패는 로그와 surface의 새 세대로 확인한다.
//!
//! 세션 ID와 화면의 기준 횟수는 종료 전에 읽는다.
//! 재시작 안내의 Enter가 업데이트 메뉴를 선택하지 않도록 업데이트 확인을 끈다.
//! 각 단계에서 조회나 시간 제한 검사가 실패하면 이후 명령 전송을 중단한다.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tasty_plugin_agent_common::reboot::{
    build_notice, ensure_submitted, is_safe_session_id, parse_options, screen_contains, screen_text,
};
use tasty_plugin_sdk::{HostHandle, IpcMethodError, i18n::Translator};

use crate::handlers::resolve_policy_args;

/// 진행 중 집합에 넣을 때 poison이면 요청을 거부한다.
/// 제거할 때는 내부 값을 사용해 ID가 남아 이후 요청을 계속 막는 일을 피한다.
const INFLIGHT_WHAT: &str = "the codex reboot in-flight set";
static INFLIGHT_POISON_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ctrl+C 전송 횟수 / 간격.
const CTRL_C_COUNT: u32 = 4;
const CTRL_C_INTERVAL: Duration = Duration::from_millis(500);
/// 화면 마커 폴링 간격.
const SCREEN_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Ctrl+C 후 codex 종료(exit 마커 증가) 대기 한도.
const EXIT_WAIT: Duration = Duration::from_secs(8);
/// resume 명령 후 codex 복귀(배너 마커 증가) 대기 한도.
const RETURN_WAIT: Duration = Duration::from_secs(20);
/// 복귀 문구를 확인한 뒤 TUI 초기화를 기다리는 시간.
const TUI_READY_GRACE: Duration = Duration::from_secs(3);
/// 안내 프롬프트 제출 시도 횟수 / 재시도 간격 / 제출→화면 검증 대기.
const NOTICE_ATTEMPTS: u32 = 4;
const NOTICE_RETRY_INTERVAL: Duration = Duration::from_secs(3);
const NOTICE_VERIFY_DELAY: Duration = Duration::from_millis(1500);
/// 안내를 확인한 뒤 별도 Enter 전 대기. agent-common의 `ensure_submitted`와 같은 값이다.
const NOTICE_SUBMIT_DELAY: Duration = Duration::from_millis(500);
/// 종료 안내에서 찾는 부분 문자열.
const EXIT_MARKER: &str = "run codex resume";
/// 시작 배너에서 찾는 부분 문자열.
const BANNER_MARKER: &str = "OpenAI Codex";
/// 화면 검증에 쓰는 안내문 선두 조각.
const NOTICE_SNIPPET: &str = "tasty codex reboot";

/// 안내문의 번역 키. 화면 확인에 쓰므로 모든 언어에서 NOTICE_SNIPPET으로 시작해야 한다.
const REBOOT_NOTICE_KEY: &str = "codex.reboot.notice";

/// 검증과 상태 조회를 마친 뒤 재시작 절차를 별도 스레드에 맡기고 응답한다.
pub(crate) fn handle_reboot(
    inflight: &Arc<Mutex<HashSet<u32>>>,
    host: &HostHandle,
    params: &Value,
    tr: &Translator,
    root: Option<&std::path::Path>,
) -> Result<Value, IpcMethodError> {
    let surface_id = crate::handlers::require_target_surface(params, tr)?;
    let (delay_secs, extra_prompt) = parse_options(params);
    // 기동과 같은 우선순위로 정책을 정한다. 최종 승인 기본값은 never다.
    let policy_args = resolve_policy_args(host, params, tr)?;

    let session_id = fetch_session_id(host, surface_id, tr)?;
    // helper는 thread UUID만 재개한다.
    if !is_safe_session_id(&session_id) || !crate::ids::is_uuid(&session_id) {
        return Err(IpcMethodError::new(crate::handlers::t_args(
            tr,
            "codex.reboot.malformed_session_id",
            &[
                ("{surface}", &surface_id.to_string()),
                ("{value}", &format!("{session_id:?}")),
            ],
        )));
    }

    let watch = root.and_then(|r| crate::reboot_watch::HelperWatch::find(r, surface_id));
    // helper 세션은 셸 복귀를 전경 pid 변화로 확인하므로, 기준값을 읽을 수 없으면 Codex를 끝내기
    // 전에 거절한다.
    let watch = match watch {
        Some(watch) => match foreground(host, surface_id) {
            Some(fg0) => Some((watch, fg0)),
            None => {
                return Err(IpcMethodError::new(tr.t_replace(
                    "codex.reboot.foreground_unreadable",
                    "{surface}",
                    &surface_id.to_string(),
                )));
            }
        },
        None => None,
    };
    // 이미 화면에 남은 문구와 구분하도록 현재 횟수를 기록한다.
    let Some(screen) = screen_text(host, surface_id) else {
        return Err(IpcMethodError::new(tr.t_replace(
            "codex.reboot.screen_unreadable",
            "{surface}",
            &surface_id.to_string(),
        )));
    };
    let exit_c0 = exit_marker_count(&screen);
    let banner_c0 = count_occurrences(&screen, BANNER_MARKER);

    {
        let mut set = inflight.lock().map_err(|e| {
            IpcMethodError::new(tr.t_replace(
                "codex.reboot.lock_poisoned",
                "{detail}",
                &e.to_string(),
            ))
        })?;
        if !set.insert(surface_id) {
            return Err(IpcMethodError::new(tr.t_replace(
                "codex.reboot.already_in_progress",
                "{surface}",
                &surface_id.to_string(),
            )));
        }
    }

    let thread_host = host.clone();
    let thread_inflight = inflight.clone();
    let thread_session = session_id.clone();
    let mut thread_policy_args = policy_args.clone();
    if let Some(root) = root
        && let Ok(Some(generation)) = crate::session::registry::current_gen(root, surface_id)
        && let Ok(Some(session)) = crate::session::registry::read_session(root, &generation)
    {
        let saved = crate::spawn_options::saved_tokens(&session);
        if !saved.is_empty() {
            thread_policy_args.push(' ');
            thread_policy_args.push_str(&saved.join(" "));
        }
    }
    // 스레드에는 번역기를 빌려주지 않고 완성된 안내문을 넘긴다.
    let thread_notice = build_notice(tr.t(REBOOT_NOTICE_KEY), extra_prompt.as_deref());
    let spawned = thread::Builder::new()
        .name(format!("codex-reboot-s{surface_id}"))
        .spawn(move || {
            match watch {
                Some((watch, fg0)) => run_helper_reboot(
                    &thread_host,
                    &watch,
                    delay_secs,
                    fg0,
                    &thread_session,
                    &thread_notice,
                    &thread_policy_args,
                ),
                None => run_reboot_sequence(
                    &thread_host,
                    surface_id,
                    delay_secs,
                    &thread_session,
                    exit_c0,
                    banner_c0,
                    &thread_notice,
                    &thread_policy_args,
                ),
            }
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
        return Err(IpcMethodError::new(tr.t_replace(
            "codex.reboot.spawn_thread_failed",
            "{detail}",
            &e.to_string(),
        )));
    }

    Ok(json!({
        "surface_id": surface_id,
        "session_id": session_id,
        "reboot_in_secs": delay_secs,
    }))
}

/// 세션 ID 메타데이터를 읽는다. 조회 실패나 값 부재는 오류로 반환한다.
fn fetch_session_id(
    host: &HostHandle,
    surface_id: u32,
    tr: &Translator,
) -> Result<String, IpcMethodError> {
    // 호스트 오류에 이미 있는 접두어를 다시 붙이지 않는다.
    let resp = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": surface_id, "key": "codex-session-id" }),
        )
        .map_err(IpcMethodError::from)?;
    let session_id = resp
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return Err(IpcMethodError::new(tr.t_replace(
            "codex.reboot.no_active_session",
            "{surface}",
            &surface_id.to_string(),
        )));
    }
    Ok(session_id)
}

/// 제출 문자(\r)를 포함한 resume 명령을 만든다. 새 세션과 같은 helper가 전용 서버를 다시
/// 띄우고 같은 thread를 연다. 호출자가 정한 정책을 TUI에 넘긴다.
pub(crate) fn resume_command(session_id: &str, policy_args: &str) -> std::io::Result<String> {
    crate::session::submit_line(
        Some(session_id),
        None,
        &crate::session::command::policy_tokens(policy_args),
    )
}

fn exit_marker_count(screen: &str) -> usize {
    count_occurrences(screen, EXIT_MARKER)
}

/// 겹치지 않는 부분 문자열의 등장 횟수를 센다.
pub(crate) fn count_occurrences(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    hay.matches(needle).count()
}

/// 호스트로 터미널을 조작한다.
struct HostIo<'a> {
    host: &'a HostHandle,
    surface_id: u32,
}

impl crate::reboot_watch::RebootIo for HostIo<'_> {
    fn ctrl_c(&self) -> Result<(), String> {
        self.host
            .call(
                "surface.send_combo",
                json!({ "surface_id": self.surface_id, "key": "c", "modifiers": ["ctrl"] }),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn foreground(&self) -> Option<crate::reboot_watch::Foreground> {
        foreground(self.host, self.surface_id)
    }
    fn send(&self, text: &str) -> Result<(), String> {
        self.host
            .call(
                "surface.send",
                json!({ "surface_id": self.surface_id, "text": text }),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn sleep(&self, d: Duration) {
        thread::sleep(d);
    }
    fn tell(&self, text: &str) -> Result<(), String> {
        self.host
            .call(
                "terminal.tell",
                json!({ "surface": self.surface_id, "text": text }),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn visible(&self, snippet: &str) -> bool {
        screen_contains(self.host, self.surface_id, snippet)
    }
    fn enter(&self) -> Result<(), String> {
        self.host
            .call(
                "surface.send_key",
                json!({ "surface_id": self.surface_id, "key": "enter" }),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// 호스트가 보고하는 전경 프로세스. 조회 실패나 pid·이름 부재는 None이다.
/// `is_shell`은 같은 조회 시점의 호스트 셸 분류다(판정 불가면 null).
fn foreground(host: &HostHandle, surface_id: u32) -> Option<crate::reboot_watch::Foreground> {
    let v = host
        .call(
            "surface.foreground_process",
            json!({ "surface_id": surface_id }),
        )
        .ok()?;
    parse_foreground(&v)
}

fn parse_foreground(v: &Value) -> Option<crate::reboot_watch::Foreground> {
    Some(crate::reboot_watch::Foreground {
        pid: v.get("pid")?.as_u64().and_then(|p| u32::try_from(p).ok())?,
        name: v.get("name")?.as_str()?.to_string(),
        // 셸 분류를 주지 않는 호스트는 판정 불가와 같다.
        is_shell: v.get("is_shell").and_then(Value::as_bool),
    })
}

/// helper 세션을 재시작한다. 결과는 로그로 남긴다.
fn run_helper_reboot(
    host: &HostHandle,
    watch: &crate::reboot_watch::HelperWatch,
    delay_secs: u64,
    fg0: crate::reboot_watch::Foreground,
    session_id: &str,
    notice: &str,
    policy_args: &str,
) {
    use crate::reboot_watch::{Outcome, restart};
    let surface_id = watch.surface;
    thread::sleep(Duration::from_secs(delay_secs));
    let command = match resume_command(session_id, policy_args) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("codex reboot s{surface_id}: resume command: {e}");
            return;
        }
    };
    let io = HostIo { host, surface_id };
    let outcome = restart(
        &io,
        watch,
        &fg0,
        session_id,
        &command,
        crate::reboot_watch::Limits::DEFAULT,
    );
    let Outcome::Restarted { ctrl_c, generation } = outcome else {
        // 각 실패는 restart가 경고로 남겼다.
        return;
    };
    tracing::info!(
        "codex reboot s{surface_id}: session {} ended after {ctrl_c}x Ctrl+C and session {generation} is ready",
        watch.generation
    );
    // 안내 대상은 restart가 확인한 세대뿐이다. 현재 세대를 다시 읽어 대상으로 삼지 않는다.
    notify_restarted(&io, watch, &generation, notice);
}

/// restart가 확인한 새 세대 `generation`에 재시작 안내를 보낸다.
fn notify_restarted(
    io: &HostIo<'_>,
    watch: &crate::reboot_watch::HelperWatch,
    generation: &str,
    notice: &str,
) {
    let surface_id = watch.surface;
    let delivered = crate::reboot_watch::deliver_notice(
        io,
        watch,
        generation,
        &crate::reboot_watch::Notice {
            text: notice,
            snippet: NOTICE_SNIPPET,
            attempts: NOTICE_ATTEMPTS,
            grace: TUI_READY_GRACE,
            verify: NOTICE_VERIFY_DELAY,
            retry: NOTICE_RETRY_INTERVAL,
            submit: NOTICE_SUBMIT_DELAY,
        },
    );
    if delivered == crate::reboot_watch::NoticeResult::NotVisible {
        tracing::warn!(
            "codex reboot s{surface_id}: notice not confirmed on screen after {NOTICE_ATTEMPTS} attempts"
        );
    }
}

/// 별도 스레드에서 각 단계를 실행한다. 실패하면 경고 후 중단한다.
#[allow(clippy::too_many_arguments)]
fn run_reboot_sequence(
    host: &HostHandle,
    surface_id: u32,
    delay_secs: u64,
    session_id: &str,
    exit_c0: usize,
    banner_c0: usize,
    notice: &str,
    policy_args: &str,
) {
    thread::sleep(Duration::from_secs(delay_secs));

    if screen_text(host, surface_id).is_none() {
        tracing::warn!("codex reboot s{surface_id}: screen unavailable before Ctrl+C; aborting");
        return;
    }

    if !kill_codex_via_ctrlc(host, surface_id, exit_c0) {
        return;
    }

    if !resume_and_wait(host, surface_id, session_id, policy_args, banner_c0) {
        return;
    }
    thread::sleep(TUI_READY_GRACE);

    if !deliver_notice(host, surface_id, notice) {
        tracing::warn!(
            "codex reboot s{surface_id}: notice not confirmed on screen after {NOTICE_ATTEMPTS} attempts"
        );
    }
}

/// Ctrl+C를 보낸 뒤 종료 안내 문구가 요청 때보다 늘었는지 확인한다.
fn kill_codex_via_ctrlc(host: &HostHandle, surface_id: u32, exit_c0: usize) -> bool {
    for _ in 0..CTRL_C_COUNT {
        if let Err(e) = host.call(
            "surface.send_combo",
            json!({ "surface_id": surface_id, "key": "c", "modifiers": ["ctrl"] }),
        ) {
            tracing::warn!("codex reboot s{surface_id}: send_combo failed: {e} — aborting");
            return false;
        }
        thread::sleep(CTRL_C_INTERVAL);
    }

    if !poll_screen(host, surface_id, EXIT_WAIT, |s| {
        exit_marker_count(s) > exit_c0
    }) {
        tracing::warn!(
            "codex reboot s{surface_id}: exit marker count did not increase after {CTRL_C_COUNT}x Ctrl+C; resume command not sent"
        );
        return false;
    }
    true
}

/// resume 명령을 보내고 시작 배너가 요청 때보다 늘었는지 확인한다.
fn resume_and_wait(
    host: &HostHandle,
    surface_id: u32,
    session_id: &str,
    policy_args: &str,
    banner_c0: usize,
) -> bool {
    if !send_resume(host, surface_id, session_id, policy_args) {
        return false;
    }

    if !poll_screen(host, surface_id, RETURN_WAIT, |s| {
        count_occurrences(s, BANNER_MARKER) > banner_c0
    }) {
        tracing::warn!(
            "codex reboot s{surface_id}: codex banner did not reappear within {}s — resume sent but notice skipped",
            RETURN_WAIT.as_secs()
        );
        return false;
    }
    true
}

/// resume 명령을 surface에 보낸다.
fn send_resume(host: &HostHandle, surface_id: u32, session_id: &str, policy_args: &str) -> bool {
    let command = match resume_command(session_id, policy_args) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("codex reboot s{surface_id}: resume command: {e}");
            return false;
        }
    };
    if let Err(e) = host.call(
        "surface.send",
        json!({ "surface_id": surface_id, "text": command }),
    ) {
        tracing::warn!("codex reboot s{surface_id}: resume send failed: {e}");
        return false;
    }
    true
}

/// 안내문을 보내고 화면에서 선두 문구를 찾는다. 제한된 횟수만큼 재시도한다.
fn deliver_notice(host: &HostHandle, surface_id: u32, notice: &str) -> bool {
    for attempt in 1..=NOTICE_ATTEMPTS {
        if let Err(e) = host.call(
            "terminal.tell",
            json!({ "surface": surface_id, "text": notice }),
        ) {
            tracing::warn!("codex reboot s{surface_id}: notice tell failed: {e}");
            return false;
        }
        thread::sleep(NOTICE_VERIFY_DELAY);
        if screen_contains(host, surface_id, NOTICE_SNIPPET) {
            ensure_submitted(host, surface_id, "codex");
            return true;
        }
        tracing::info!(
            "codex reboot s{surface_id}: notice attempt {attempt}/{NOTICE_ATTEMPTS} not visible yet — retrying"
        );
        thread::sleep(NOTICE_RETRY_INTERVAL);
    }
    if screen_contains(host, surface_id, NOTICE_SNIPPET) {
        ensure_submitted(host, surface_id, "codex");
        return true;
    }
    false
}

/// 화면을 폴링한다. 조회 실패 시 중단하고, 조건 불일치 시 경과 시간을 확인한다.
fn poll_screen(
    host: &HostHandle,
    surface_id: u32,
    timeout: Duration,
    pred: impl Fn(&str) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match screen_text(host, surface_id) {
            Some(text) if pred(&text) => return true,
            Some(_) => {}
            None => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(SCREEN_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_command_reopens_the_thread_through_the_session_helper() {
        let thread = "019f55e7-3dfa-7a00-8000-0123456789ab";
        let line = resume_command(thread, "-a never -s read-only").unwrap();
        let program = crate::session::command::helper_program().unwrap();
        assert_eq!(
            line,
            format!(
                "{} session --resume '{thread}' -- '-a' 'never' '-s' 'read-only'\r",
                crate::session::command::sh_quote(&program)
            )
        );
        assert!(!line.contains("bypass-hook-trust"));
    }

    fn test_translator_for(code: &str) -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, code)
    }

    #[test]
    fn notice_without_extra_is_the_translated_text() {
        let tr = test_translator_for("ko");
        assert_eq!(
            build_notice(tr.t(REBOOT_NOTICE_KEY), None),
            tr.t(REBOOT_NOTICE_KEY)
        );
    }

    #[test]
    fn notice_with_extra_appends_after_blank_line() {
        let tr = test_translator_for("ko");
        let n = build_notice(tr.t(REBOOT_NOTICE_KEY), Some("soak 이어서"));
        assert!(n.starts_with(tr.t(REBOOT_NOTICE_KEY)));
        assert!(n.ends_with("\n\nsoak 이어서"));
    }

    /// 모든 언어에서 화면 확인에 사용할 선두 문구를 유지해야 한다.
    #[test]
    fn notice_starts_with_the_snippet_in_every_locale() {
        for code in tasty_i18n::BUILTIN_CODES {
            let tr = test_translator_for(code);
            let notice = build_notice(tr.t(REBOOT_NOTICE_KEY), None);
            assert!(
                notice.starts_with(NOTICE_SNIPPET),
                "[{code}] 안내문이 화면 검증 조각(`{NOTICE_SNIPPET}`)으로 시작하지 않는다: {notice}"
            );
        }
    }

    /// 언어별 카탈로그를 사용하는지 확인한다.
    #[test]
    fn notice_changes_with_the_locale() {
        let en = build_notice(test_translator_for("en").t(REBOOT_NOTICE_KEY), None);
        let ko = build_notice(test_translator_for("ko").t(REBOOT_NOTICE_KEY), None);
        assert_ne!(en, ko, "언어가 달라도 같은 문구를 반환했다");
    }

    /// 셸 분류가 없거나 null인 응답은 판정 불가로 읽는다.
    #[test]
    fn the_shell_classification_is_read_only_when_present() {
        let read = |v: Value| parse_foreground(&v).map(|f| f.is_shell);
        assert_eq!(
            read(json!({ "pid": 7, "name": "zsh", "is_shell": true })),
            Some(Some(true))
        );
        assert_eq!(
            read(json!({ "pid": 7, "name": "vim", "is_shell": false })),
            Some(Some(false))
        );
        assert_eq!(
            read(json!({ "pid": 7, "name": "zsh", "is_shell": null })),
            Some(None)
        );
        assert_eq!(read(json!({ "pid": 7, "name": "zsh" })), Some(None));
        assert_eq!(read(json!({ "pid": null, "name": null })), None);
    }

    #[test]
    fn count_occurrences_counts_and_handles_empty() {
        assert_eq!(count_occurrences("a b a b a", "a"), 3);
        assert_eq!(
            count_occurrences("run codex resume x\nrun codex resume y", "run codex resume"),
            2
        );
        assert_eq!(count_occurrences("anything", ""), 0);
        assert_eq!(count_occurrences("", "x"), 0);
    }

    /// 종료 안내와 시작 배너를 구분해 세는지 확인한다.
    #[test]
    fn exit_marker_count_counts_only_the_quit_hint() {
        assert_eq!(
            exit_marker_count("To continue this session, run codex resume thread"),
            1
        );
        assert_eq!(exit_marker_count("\u{2502} >_ OpenAI Codex (v0.142.2)"), 0);
    }

    #[test]
    fn exit_marker_matches_observed_quit_hint() {
        // v0.142 실측 종료 출력에서 마커가 잡히는지 회귀 가드.
        let observed = "Token usage: total=509 input=504\nTo continue this session, run codex resume 019f55f1-cd32-7790-9459-ee7f5488051e";
        assert_eq!(count_occurrences(observed, EXIT_MARKER), 1);
    }

    #[test]
    fn banner_marker_matches_observed_banner() {
        let observed =
            "\u{256d}\u{2500}\u{2500}\u{256e}\n\u{2502} >_ OpenAI Codex (v0.142.2) \u{2502}";
        assert_eq!(count_occurrences(observed, BANNER_MARKER), 1);
    }

    #[test]
    fn require_target_surface_accepts_both_keys() {
        assert_eq!(
            crate::handlers::require_target_surface(
                &json!({ "surface": 7 }),
                &test_translator_for("en")
            )
            .unwrap(),
            7
        );
        assert_eq!(
            crate::handlers::require_target_surface(
                &json!({ "surface_id": 9 }),
                &test_translator_for("en")
            )
            .unwrap(),
            9
        );
        assert!(
            crate::handlers::require_target_surface(&json!({}), &test_translator_for("en"))
                .is_err()
        );

        // 범위를 넘는 ID를 잘라 다른 대상을 선택하지 않아야 한다.
        assert!(
            crate::handlers::require_target_surface(
                &json!({ "surface": u64::from(u32::MAX) + 2 }),
                &test_translator_for("en")
            )
            .is_err()
        );
        assert!(
            crate::handlers::require_target_surface(
                &json!({ "surface": "conductor" }),
                &test_translator_for("en")
            )
            .is_err()
        );
        assert_eq!(
            crate::handlers::require_target_surface(
                &json!({ "surface": u32::MAX }),
                &test_translator_for("en")
            )
            .unwrap(),
            u32::MAX
        );
    }
}
