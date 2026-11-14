//! 일시적인 API 오류 뒤 설정한 시간이 지나면 재개 문구를 제출한다. 기본값은 꺼짐이다.
//! hook 처리에서는 예약만 갱신하고 전용 스레드가 시간이 된 예약을 처리한다.
//! 전송 전 설정·턴·전경 프로세스·사용자 입력을 다시 확인하지만, 조회와 전송은 원자적이지 않다.
//! 정책은 docs/plugins/claude/index.md#api-에러-뒤-자동-재개-auto_resumers 참고.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tasty_plugin_agent_common::host_call::HostCall;

/// 설정 키 — 매니페스트 `[[contributes.settings_pages.items]]` 의 `storage_key` 와 같다.
pub(crate) const ENABLED_KEY: &str = "auto_resume_enabled";
pub(crate) const DELAY_KEY: &str = "auto_resume_delay_secs";
pub(crate) const MAX_ATTEMPTS_KEY: &str = "auto_resume_max_attempts";

/// 미설정 시 적용할 기본값. 호스트는 없는 설정에 null을 반환하므로 여기서 채운다.
const DEFAULT_DELAY_SECS: f64 = 10.0;
/// 지연 상한. 직접 수정한 설정도 Duration과 Instant의 표현 범위를 넘지 않도록 제한한다.
const MAX_DELAY_SECS: f64 = 86_400.0;
const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// 자동 재개 횟수를 표시할 surface 메타데이터. 성공 턴·새 세션·종료 시 지운다.
pub(crate) const COUNT_META_KEY: &str = "claude-auto-resume-count";

/// 재개 예약을 확인하는 주기. 실제 전송까지의 총 대기 시간 상한은 아니다.
const TICK: Duration = Duration::from_millis(500);

/// 만기 시점에 사용자가 타이핑 중이면 이만큼 미룬다. 호스트의 타이핑 창(5 초)과 같다.
const TYPING_DEFER: Duration = Duration::from_secs(5);

/// 일시적인 서버 오류인 overloaded·server_error만 재개한다.
/// rate_limit은 수초 만에 해소되지 않을 수 있고, 인증·청구·요청 오류 등은 사용자 조치가 필요하다.
/// 2026-09-23의 Claude Code 2.1.280에서는 게이트웨이의 529 overloaded_error가 server_error로 전달됐다.
pub(crate) fn is_resumable_error(error: &str) -> bool {
    matches!(error, "overloaded" | "server_error")
}

/// plugin 설정. 켜짐·상한은 만기 시점에 다시 읽고, 지연은 예약 때 한 번 쓴다.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub delay: Duration,
    pub max_attempts: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            delay: Duration::from_secs_f64(DEFAULT_DELAY_SECS),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }
}

/// 설정을 읽는다. 조회 실패·미설정은 기본값을 적용하며 활성 여부의 기본값은 false다.
pub(crate) fn read_settings<H: HostCall>(host: &H) -> Settings {
    let get = |key: &str| {
        host.call("settings.get_plugin_setting", json!({ "storage_key": key }))
            .ok()
            .and_then(|v| v.get("value").cloned())
            .filter(|v| !v.is_null())
    };
    let d = Settings::default();
    Settings {
        enabled: get(ENABLED_KEY)
            .and_then(|v| v.as_bool())
            .unwrap_or(d.enabled),
        delay: get(DELAY_KEY)
            .and_then(|v| v.as_f64())
            .and_then(|s| Duration::try_from_secs_f64(s.clamp(1.0, MAX_DELAY_SECS)).ok())
            .unwrap_or(d.delay),
        max_attempts: get(MAX_ATTEMPTS_KEY)
            .and_then(|v| v.as_f64())
            .map(|n| n.max(1.0).round() as u32)
            .unwrap_or(d.max_attempts),
    }
}

/// 걸려 있는 재개 예약 하나.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pending {
    pub due: Instant,
    pub error: String,
    /// 예약한 턴의 번호 — 보내기 직전에 그 뒤로 새 턴이 없었는지 이것으로 본다.
    pub turn: u64,
    /// 예약 시점의 전경 pid — 그 사이 Claude 가 다시 떴으면 다른 세션이다.
    pub foreground_pid: Option<u64>,
    /// 이 시각 이후의 사용자 입력은 미제출 초안이 있을 수 있어 자동 재개를 취소하는 기준이다.
    /// 실패한 턴 시작 시각을 사용하고, 모르면 예약 시각을 쓴다.
    pub input_since: Instant,
}

#[derive(Debug, Default)]
struct SurfaceResume {
    /// 연속으로 보낸 재개 문구 수. 성공 턴(`stop`)과 새 세션에서 0.
    attempts: u32,
    /// 턴 경계마다 올라가는 번호.
    turn: u64,
    /// 마지막 `prompt-submit` 시각.
    turn_started: Option<Instant>,
    pending: Option<Pending>,
    /// 상한 도달 알림을 이미 냈는가 — 연속 실패 구간당 1 회.
    limit_notified: bool,
}

/// surface → 재개 상태. hook 스레드와 만기 스레드가 함께 쓴다(`Arc<Mutex<_>>`).
#[derive(Debug, Default)]
pub(crate) struct ResumeTable {
    surfaces: HashMap<u32, SurfaceResume>,
}

impl ResumeTable {
    /// 새 턴이 시작됐다(사람 입력 · 재개 문구 제출 · `active`) — 걸린 예약은 취소한다.
    pub fn on_new_turn(&mut self, surface: u32, now: Instant) {
        let e = self.surfaces.entry(surface).or_default();
        e.turn += 1;
        e.turn_started = Some(now);
        e.pending = None;
    }

    /// 새 세션 — 턴 경계이면서 연속 실패 계수도 새로 센다.
    pub fn on_session_start(&mut self, surface: u32, now: Instant) {
        self.on_new_turn(surface, now);
        let e = self.surfaces.entry(surface).or_default();
        e.attempts = 0;
        e.limit_notified = false;
    }

    /// 턴이 성공으로 끝났다 — 연속 실패가 끊겼다. 끊기 전의 연속 재개 수를 돌려준다.
    pub fn on_success(&mut self, surface: u32) -> u32 {
        let Some(e) = self.surfaces.get_mut(&surface) else {
            return 0;
        };
        e.turn += 1;
        e.limit_notified = false;
        e.pending = None;
        std::mem::take(&mut e.attempts)
    }

    /// 세션이 끝났다 — 이 surface 의 상태를 버린다.
    pub fn on_session_end(&mut self, surface: u32) {
        self.surfaces.remove(&surface);
    }

    /// API 에러로 끝난 턴에 재개를 예약한다. 이미 걸린 예약은 덮어쓴다(중복 전송 금지).
    pub fn schedule(
        &mut self,
        surface: u32,
        error: &str,
        now: Instant,
        delay: Duration,
        foreground_pid: Option<u64>,
    ) {
        let e = self.surfaces.entry(surface).or_default();
        e.turn += 1;
        // 지연이 시계 표현 범위를 넘으면 새 예약을 만들지 않는다. 이전 예약도 무효화돼 있다.
        let Some(due) = now.checked_add(delay) else {
            e.pending = None;
            tracing::warn!(
                "claude auto-resume s{surface}: delay {delay:?} overflows the clock, not scheduling"
            );
            return;
        };
        e.pending = Some(Pending {
            due,
            error: error.to_string(),
            turn: e.turn,
            foreground_pid,
            input_since: e.turn_started.unwrap_or(now),
        });
    }

    /// `surface` 의 예약이 `now` 에 만기인가(꺼내지 않고 본다). 시험 전용 — 만기 스레드는
    /// [`Self::take_due`] 로 꺼낸다.
    #[cfg(test)]
    pub fn due(&self, surface: u32, now: Instant) -> Option<&Pending> {
        self.surfaces
            .get(&surface)
            .and_then(|e| e.pending.as_ref())
            .filter(|p| p.due <= now)
    }

    /// 만기된 예약을 전부 꺼낸다 — 꺼낸 뒤에는 표에 없으므로 두 번 처리되지 않는다.
    pub fn take_due(&mut self, now: Instant) -> Vec<(u32, Pending, u32)> {
        let mut out = Vec::new();
        for (sid, e) in self.surfaces.iter_mut() {
            if e.pending.as_ref().is_some_and(|p| p.due <= now)
                && let Some(p) = e.pending.take()
            {
                out.push((*sid, p, e.attempts));
            }
        }
        out
    }

    /// 예약한 뒤로 턴 경계가 없었는가 — 보내기 직전의 마지막 확인.
    pub fn still_current(&self, surface: u32, turn: u64) -> bool {
        self.surfaces
            .get(&surface)
            .is_some_and(|e| e.turn == turn && e.pending.is_none())
    }

    /// 꺼낸 예약을 `by` 만큼 미뤄 되돌린다 — 그 사이 턴 경계가 있었으면 버린다.
    pub fn defer(&mut self, surface: u32, mut pending: Pending, now: Instant, by: Duration) {
        if self.still_current(surface, pending.turn)
            && let Some(e) = self.surfaces.get_mut(&surface)
        {
            pending.due = now + by;
            e.pending = Some(pending);
        }
    }

    /// 재개 문구를 하나 보냈다. 보낸 뒤의 누적 수를 돌려준다.
    pub fn record_attempt(&mut self, surface: u32) -> u32 {
        let e = self.surfaces.entry(surface).or_default();
        e.attempts += 1;
        e.attempts
    }

    /// 시험 전용 — 만기 처리는 꺼낸 시도 수를 [`judge`] 에 넘겨 같은 비교를 한다.
    #[cfg(test)]
    pub fn limit_reached(&self, surface: u32, max_attempts: u32) -> bool {
        self.surfaces
            .get(&surface)
            .is_some_and(|e| e.attempts >= max_attempts)
    }

    /// 상한 도달 알림을 낼 차례인가 — 연속 실패 구간당 처음 한 번만 `true`.
    pub fn take_limit_notice(&mut self, surface: u32) -> bool {
        let e = self.surfaces.entry(surface).or_default();
        !std::mem::replace(&mut e.limit_notified, true)
    }
}

/// poison을 알리고 기존 표를 재사용한다. 패닉 전 부분 갱신을 되돌리지는 않는다.
pub(crate) fn lock_table(table: &Mutex<ResumeTable>) -> std::sync::MutexGuard<'_, ResumeTable> {
    const WHAT: &str = "the claude auto-resume table";
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    tasty_utils::poison::recover_mutex(table.lock(), WHAT, &REPORTED)
}

/// hook 이벤트 하나를 표에 반영한다. `stop-failure` 이면 설정을 읽어 예약을 건다.
/// 서브에이전트의 `stop-failure` 는 호출부(`hook.rs`)가 이미 걸러 이 함수에 오지 않는다.
/// 백그라운드 작업을 기다리는 `stop` 도 턴 종료가 아니므로 호출부가 걸러 이 함수에 오지 않는다.
pub(crate) fn observe_hook<H: HostCall>(
    table: &Mutex<ResumeTable>,
    host: &H,
    event: &str,
    surface_id: u32,
    error: Option<&str>,
    now: Instant,
) {
    match event {
        "prompt-submit" | "active" => lock_table(table).on_new_turn(surface_id, now),
        // 새 세션·종료 때는 plugin 재시작 전의 메타데이터도 지우도록 표가 비어 있어도 호출한다.
        "session-start" => {
            lock_table(table).on_session_start(surface_id, now);
            clear_count_meta(host, surface_id);
        }
        // `subagent-stop` 은 서브에이전트가 끝난 것이지 메인 턴이 끝난 것이 아니다.
        "stop" => {
            // 재개 뒤의 턴이 성공했으면 계수 기록을 지운다. 매 턴 오는 신호라 보낸 적이
            // 있을 때만 부른다.
            if lock_table(table).on_success(surface_id) > 0 {
                clear_count_meta(host, surface_id);
            }
        }
        "session-end" => {
            lock_table(table).on_session_end(surface_id);
            clear_count_meta(host, surface_id);
        }
        "stop-failure" => {
            let error = error.filter(|e| !e.is_empty()).unwrap_or("unknown");
            if !is_resumable_error(error) {
                return;
            }
            let settings = read_settings(host);
            if !settings.enabled {
                return;
            }
            let foreground_pid = foreground(host, surface_id).and_then(|(_, pid)| pid);
            lock_table(table).schedule(surface_id, error, now, settings.delay, foreground_pid);
            tracing::info!(
                "claude auto-resume s{surface_id}: turn ended on {error}, resume scheduled in {:?}",
                settings.delay
            );
        }
        _ => {}
    }
}

fn clear_count_meta<H: HostCall>(host: &H, surface_id: u32) {
    if let Err(e) = host.call(
        "surface.meta.unset",
        json!({ "surface_id": surface_id, "key": COUNT_META_KEY }),
    ) {
        tracing::warn!("claude auto-resume s{surface_id}: clearing the attempt count failed: {e}");
    }
}

/// 만기 시점에 모은 사실. [`judge`] 의 입력 — host 없이 테스트한다.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DueFacts {
    pub settings: Settings,
    pub attempts: u32,
    /// `terminal.state` 의 값(조회 실패면 `None`).
    pub state: Option<String>,
    /// 전경 프로세스 이름·pid(조회 실패 — surface 소멸 — 면 `None`).
    pub foreground_name: Option<String>,
    pub foreground_pid: Option<u64>,
    pub expected_pid: Option<u64>,
    /// `surface.is_typing` 의 `typing`.
    pub typing: bool,
    /// 마지막 사용자 입력(키보드 · IME · 붙여넣기) 뒤 경과 — `surface.is_typing` 의
    /// `idle_seconds`. 입력이 한 번도 없었으면 `None`.
    pub key_idle: Option<Duration>,
    /// [`Pending::input_since`] 뒤로 지난 시간.
    pub since_input_window: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Send,
    /// 지금은 보내지 않고 미룬다(시도 수를 쓰지 않는다).
    Defer,
    /// 보내지 않고 예약을 버린다.
    Cancel(&'static str),
    /// 연속 상한에 닿았다 — 보내지 않고 한 번 알린다.
    LimitReached,
}

/// 취소 조건을 상한보다 먼저 확인한다. 이미 사용자가 개입했거나 Claude가 종료됐으면
/// 재개 횟수 상한 알림을 보내지 않는다.
pub(crate) fn judge(f: &DueFacts) -> Verdict {
    if !f.settings.enabled {
        return Verdict::Cancel("disabled");
    }
    if f.state.as_deref() != Some("idle") {
        return Verdict::Cancel("not idle");
    }
    if !f
        .foreground_name
        .as_deref()
        .is_some_and(is_claude_process_name)
    {
        return Verdict::Cancel("claude is not in the foreground");
    }
    if f.expected_pid.is_some() && f.foreground_pid != f.expected_pid {
        return Verdict::Cancel("claude was restarted");
    }
    // 최근 타이핑 여부뿐 아니라 턴 시작 뒤의 마지막 입력도 확인해 남아 있을 초안을 보호한다.
    if f.key_idle.is_some_and(|idle| idle < f.since_input_window) {
        return Verdict::Cancel("the user gave input after the turn began");
    }
    if f.attempts >= f.settings.max_attempts {
        return Verdict::LimitReached;
    }
    if f.typing {
        return Verdict::Defer;
    }
    Verdict::Send
}

/// Windows의 .exe 접미사와 대소문자 차이를 무시하고 claude 프로세스 이름인지 확인한다.
fn is_claude_process_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower) == "claude"
}

/// 만기 처리 스레드가 쓰는 문구 — `main()` 이 활성 locale 로 한 번 해석해 넘긴다.
#[derive(Debug, Clone)]
pub(crate) struct Texts {
    /// 제출할 재개 문구.
    pub message: String,
    pub limit_title: String,
    /// `{}` 세 개: surface id, 시도 수, 에러 종류.
    pub limit_body: String,
    /// 시도 수가 1일 때의 `limit_body`(단수 변형이 없는 언어는 같은 문구).
    pub limit_body_one: String,
}

pub(crate) fn run_loop<H: HostCall>(table: Arc<Mutex<ResumeTable>>, host: H, texts: Texts) {
    loop {
        std::thread::sleep(TICK);
        let due = lock_table(&table).take_due(Instant::now());
        for (surface_id, pending, attempts) in due {
            process_due(&table, &host, &texts, surface_id, pending, attempts);
        }
    }
}

fn process_due<H: HostCall>(
    table: &Mutex<ResumeTable>,
    host: &H,
    texts: &Texts,
    surface_id: u32,
    pending: Pending,
    attempts: u32,
) {
    let now = Instant::now();
    let facts = gather_facts(host, surface_id, &pending, attempts, now);
    match judge(&facts) {
        Verdict::Send => send(table, host, texts, surface_id, &pending),
        Verdict::Defer => {
            tracing::info!("claude auto-resume s{surface_id}: user is typing, deferring");
            lock_table(table).defer(surface_id, pending, now, TYPING_DEFER);
        }
        Verdict::Cancel(reason) => {
            tracing::info!("claude auto-resume s{surface_id}: not resuming — {reason}");
        }
        Verdict::LimitReached => {
            if lock_table(table).take_limit_notice(surface_id) {
                notify_limit(host, texts, surface_id, attempts, &pending.error);
            }
        }
    }
}

fn gather_facts<H: HostCall>(
    host: &H,
    surface_id: u32,
    pending: &Pending,
    attempts: u32,
    now: Instant,
) -> DueFacts {
    let state = host
        .call("terminal.state", json!({ "surface": surface_id }))
        .ok()
        .and_then(|r| r.get("state").and_then(|v| v.as_str()).map(str::to_string));
    let (foreground_name, foreground_pid) = match foreground(host, surface_id) {
        Some((name, pid)) => (Some(name), pid),
        None => (None, None),
    };
    let typing = host
        .call("surface.is_typing", json!({ "surface_id": surface_id }))
        .ok();
    // 조회가 실패하면 "입력 있음" 으로 본다 — 모르는 채로 보내면 초안에 붙을 수 있다.
    let (typing_now, key_idle) = match &typing {
        Some(r) => (
            r.get("typing").and_then(|v| v.as_bool()).unwrap_or(true),
            r.get("idle_seconds")
                .and_then(|v| v.as_f64())
                .filter(|s| *s >= 0.0)
                // 표현 범위를 넘는 경과는 "아주 오래전" 이다 — 패닉 대신 최댓값으로.
                .map(|s| Duration::try_from_secs_f64(s).unwrap_or(Duration::MAX)),
        ),
        None => (true, Some(Duration::ZERO)),
    };
    DueFacts {
        settings: read_settings(host),
        attempts,
        state,
        foreground_name,
        foreground_pid,
        expected_pid: pending.foreground_pid,
        typing: typing_now,
        key_idle,
        since_input_window: now.saturating_duration_since(pending.input_since),
    }
}

fn foreground<H: HostCall>(host: &H, surface_id: u32) -> Option<(String, Option<u64>)> {
    let r = host
        .call(
            "surface.foreground_process",
            json!({ "surface_id": surface_id }),
        )
        .ok()?;
    let name = r.get("name").and_then(|v| v.as_str())?.to_string();
    Some((name, r.get("pid").and_then(|v| v.as_u64())))
}

fn send<H: HostCall>(
    table: &Mutex<ResumeTable>,
    host: &H,
    texts: &Texts,
    surface_id: u32,
    pending: &Pending,
) {
    // 조회 중 새 턴이 시작됐는지 다시 확인한다. 이 확인과 실제 전송 사이에도 상태가 바뀔 수 있다.
    if !lock_table(table).still_current(surface_id, pending.turn) {
        tracing::info!("claude auto-resume s{surface_id}: a new turn began, not resuming");
        return;
    }
    // terminal.tell은 본문과 Enter를 나눠 보내며 원격 점유 대상은 거절한다. 실패하면 재예약하지 않는다.
    if let Err(e) = host.call(
        "terminal.tell",
        json!({ "surface": surface_id, "text": texts.message }),
    ) {
        tracing::warn!("claude auto-resume s{surface_id}: tell failed, not retrying: {e}");
        return;
    }
    record_resume(table, host, surface_id, pending);
}

/// 보낸 재개 한 번을 시도 수로 센다 — 표에 올리고, 그 수를 surface 메타데이터에도 적는다.
fn record_resume<H: HostCall>(
    table: &Mutex<ResumeTable>,
    host: &H,
    surface_id: u32,
    pending: &Pending,
) {
    let n = lock_table(table).record_attempt(surface_id);
    tracing::info!(
        "claude auto-resume s{surface_id}: resumed after {} (attempt {n})",
        pending.error
    );
    if let Err(e) = host.call(
        "surface.meta.set",
        json!({ "surface_id": surface_id, "key": COUNT_META_KEY, "value": n.to_string() }),
    ) {
        tracing::warn!("claude auto-resume s{surface_id}: recording the attempt count failed: {e}");
    }
}

fn notify_limit<H: HostCall>(host: &H, texts: &Texts, surface_id: u32, attempts: u32, error: &str) {
    tracing::info!(
        "claude auto-resume s{surface_id}: stopped after {attempts} consecutive API errors ({error})"
    );
    let template = if attempts == 1 {
        &texts.limit_body_one
    } else {
        &texts.limit_body
    };
    let body = tasty_plugin_sdk::i18n::fill_args(
        template,
        &[&surface_id.to_string(), &attempts.to_string(), error],
    );
    if let Err(e) = host.call(
        "notification.create",
        json!({ "title": texts.limit_title, "body": body, "surface_id": surface_id }),
    ) {
        tracing::warn!("claude auto-resume s{surface_id}: limit notification failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Settings {
        Settings {
            enabled: true,
            ..Settings::default()
        }
    }

    /// 전송 가능한 기본 상태. 각 시험에서 한 조건씩 바꾼다.
    fn sendable() -> DueFacts {
        DueFacts {
            settings: on(),
            attempts: 0,
            state: Some("idle".into()),
            foreground_name: Some("claude".into()),
            foreground_pid: Some(42),
            expected_pid: Some(42),
            typing: false,
            key_idle: None,
            since_input_window: Duration::from_secs(12),
        }
    }

    #[test]
    fn overloaded_is_resumable_auth_failure_is_not() {
        assert!(is_resumable_error("overloaded"));
        assert!(is_resumable_error("server_error"));
        for e in [
            "authentication_failed",
            "billing_error",
            "invalid_request",
            "max_output_tokens",
            "rate_limit",
            "model_not_found",
            "unknown",
        ] {
            assert!(!is_resumable_error(e), "{e}");
        }
    }

    #[test]
    fn the_default_is_off() {
        assert!(!Settings::default().enabled);
        let mut f = sendable();
        f.settings = Settings::default();
        assert_eq!(judge(&f), Verdict::Cancel("disabled"));
    }

    #[test]
    fn all_conditions_met_sends() {
        assert_eq!(judge(&sendable()), Verdict::Send);
    }

    #[test]
    fn a_turn_that_is_no_longer_idle_is_not_resumed() {
        let mut f = sendable();
        f.state = Some("active".into());
        assert!(matches!(judge(&f), Verdict::Cancel(_)));
        f.state = None;
        assert!(matches!(judge(&f), Verdict::Cancel(_)));
    }

    #[test]
    fn claude_gone_or_restarted_is_not_resumed() {
        let mut f = sendable();
        f.foreground_name = Some("zsh".into());
        assert!(matches!(judge(&f), Verdict::Cancel(_)));
        let mut f = sendable();
        f.foreground_name = None;
        assert!(matches!(judge(&f), Verdict::Cancel(_)));
        let mut f = sendable();
        f.foreground_pid = Some(43);
        assert_eq!(judge(&f), Verdict::Cancel("claude was restarted"));
    }

    /// Windows의 claude.exe 이름과 대소문자 차이도 허용한다.
    #[test]
    fn the_foreground_name_matches_claude_on_every_platform() {
        for name in ["claude", "claude.exe", "Claude.exe", "CLAUDE.EXE"] {
            let mut f = sendable();
            f.foreground_name = Some(name.into());
            assert_eq!(judge(&f), Verdict::Send, "{name}");
        }
        for name in [
            "claudex",
            "claudex.exe",
            "claude.exe.bak",
            "node",
            "exe",
            "",
        ] {
            let mut f = sendable();
            f.foreground_name = Some(name.into());
            assert_eq!(
                judge(&f),
                Verdict::Cancel("claude is not in the foreground"),
                "{name}"
            );
        }
    }

    /// 최근 타이핑은 멈췄어도 턴 시작 뒤 입력한 초안이 있을 수 있다.
    #[test]
    fn a_draft_left_for_more_than_five_seconds_still_cancels() {
        let mut f = sendable();
        f.typing = false;
        f.key_idle = Some(Duration::from_secs(8));
        f.since_input_window = Duration::from_secs(12);
        assert_eq!(
            judge(&f),
            Verdict::Cancel("the user gave input after the turn began")
        );
    }

    /// 확인 기간보다 앞선 입력은 개입으로 보지 않는다.
    #[test]
    fn input_before_the_turn_began_does_not_cancel() {
        let mut f = sendable();
        f.key_idle = Some(Duration::from_secs(13));
        f.since_input_window = Duration::from_secs(12);
        assert_eq!(judge(&f), Verdict::Send);
    }

    #[test]
    fn typing_right_now_defers_without_spending_an_attempt() {
        let mut f = sendable();
        f.typing = true;
        f.key_idle = Some(Duration::from_secs(2));
        f.since_input_window = Duration::from_secs(1);
        assert_eq!(judge(&f), Verdict::Defer);
    }

    #[test]
    fn the_limit_stops_sending() {
        let mut f = sendable();
        f.attempts = f.settings.max_attempts;
        assert_eq!(judge(&f), Verdict::LimitReached);
    }

    #[test]
    fn new_turn_cancels_pending_resume() {
        let now = Instant::now();
        let mut t = ResumeTable::default();
        t.schedule(7, "overloaded", now, Duration::from_secs(10), Some(1));
        assert!(t.due(7, now + Duration::from_secs(11)).is_some());
        t.on_new_turn(7, now + Duration::from_secs(2));
        assert!(t.due(7, now + Duration::from_secs(11)).is_none());
        assert!(t.take_due(now + Duration::from_secs(11)).is_empty());
    }

    #[test]
    fn a_pending_resume_is_not_due_before_the_delay() {
        let now = Instant::now();
        let mut t = ResumeTable::default();
        t.schedule(7, "overloaded", now, Duration::from_secs(10), None);
        assert!(t.take_due(now + Duration::from_secs(9)).is_empty());
        let due = t.take_due(now + Duration::from_secs(10));
        assert_eq!(due.len(), 1);
        // 한 번 꺼낸 예약은 다시 나오지 않는다 — 두 번 보내지 않는다.
        assert!(t.take_due(now + Duration::from_secs(20)).is_empty());
    }

    #[test]
    fn rescheduling_overwrites_instead_of_queueing_a_second_send() {
        let now = Instant::now();
        let mut t = ResumeTable::default();
        t.schedule(7, "overloaded", now, Duration::from_secs(10), None);
        t.schedule(7, "server_error", now, Duration::from_secs(10), None);
        let due = t.take_due(now + Duration::from_secs(10));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].1.error, "server_error");
    }

    #[test]
    fn the_input_window_opens_at_the_turn_start_when_known() {
        let t0 = Instant::now();
        let mut t = ResumeTable::default();
        t.on_new_turn(7, t0);
        t.schedule(
            7,
            "overloaded",
            t0 + Duration::from_secs(30),
            Duration::from_secs(10),
            None,
        );
        assert_eq!(
            t.due(7, t0 + Duration::from_secs(40)).unwrap().input_since,
            t0
        );
        // 턴 시작을 모르면(plugin 재시작 직후) 예약 시각부터.
        let mut t = ResumeTable::default();
        let at = t0 + Duration::from_secs(5);
        t.schedule(8, "overloaded", at, Duration::from_secs(10), None);
        assert_eq!(
            t.due(8, at + Duration::from_secs(10)).unwrap().input_since,
            at
        );
    }

    #[test]
    fn a_turn_boundary_during_the_checks_makes_the_taken_resume_stale() {
        let now = Instant::now();
        let mut t = ResumeTable::default();
        t.schedule(7, "overloaded", now, Duration::from_secs(1), None);
        let (_, p, _) = t.take_due(now + Duration::from_secs(1)).remove(0);
        assert!(t.still_current(7, p.turn));
        t.on_new_turn(7, now + Duration::from_secs(2));
        assert!(!t.still_current(7, p.turn));
        // 미루려 해도 되살아나지 않는다.
        t.defer(7, p, now, Duration::from_secs(5));
        assert!(t.take_due(now + Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn a_deferred_resume_comes_back_later() {
        let now = Instant::now();
        let mut t = ResumeTable::default();
        t.schedule(7, "overloaded", now, Duration::from_secs(1), None);
        let (_, p, _) = t.take_due(now + Duration::from_secs(1)).remove(0);
        t.defer(7, p, now + Duration::from_secs(1), Duration::from_secs(5));
        assert!(t.take_due(now + Duration::from_secs(5)).is_empty());
        assert_eq!(t.take_due(now + Duration::from_secs(6)).len(), 1);
    }

    #[test]
    fn attempts_stop_at_limit_and_reset_on_success() {
        let mut t = ResumeTable::default();
        for _ in 0..5 {
            t.record_attempt(7);
        }
        assert!(t.limit_reached(7, 5));
        t.on_success(7);
        assert!(!t.limit_reached(7, 5));
    }

    #[test]
    fn a_new_turn_does_not_reset_the_consecutive_count() {
        // 자동 재개도 prompt-submit을 발생시키므로 이때 시도 수를 지우면 상한에 도달하지 않는다.
        let mut t = ResumeTable::default();
        t.record_attempt(7);
        t.on_new_turn(7, Instant::now());
        assert_eq!(t.record_attempt(7), 2);
        t.on_session_start(7, Instant::now());
        assert_eq!(t.record_attempt(7), 1, "새 세션은 새로 센다");
    }

    #[test]
    fn the_limit_notice_goes_out_once_per_failure_streak() {
        let mut t = ResumeTable::default();
        assert!(t.take_limit_notice(7));
        assert!(!t.take_limit_notice(7));
        t.on_success(7);
        assert!(t.take_limit_notice(7));
    }

    #[test]
    fn session_end_forgets_the_surface() {
        let now = Instant::now();
        let mut t = ResumeTable::default();
        t.schedule(7, "overloaded", now, Duration::from_secs(1), None);
        t.record_attempt(7);
        t.on_session_end(7);
        assert!(t.take_due(now + Duration::from_secs(2)).is_empty());
        assert!(!t.limit_reached(7, 1));
    }

    /// 번역 키가 내장 언어마다 있고 횟수 안내에 placeholder 세 개가 있는지 확인한다.
    #[test]
    fn every_locale_has_the_resume_texts() {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        for code in tasty_i18n::BUILTIN_CODES {
            let tr = tasty_plugin_sdk::i18n::Translator::load(&lang_dir, code);
            for key in [
                "claude.auto_resume.message",
                "claude.auto_resume.limit_title",
                "claude.auto_resume.limit_body",
                "claude.settings.auto_resume_enabled",
                "claude.settings.auto_resume_delay_secs",
                "claude.settings.auto_resume_max_attempts",
            ] {
                assert_ne!(tr.t(key), key, "{code}: {key} 누락");
            }
            assert_eq!(
                tr.t("claude.auto_resume.limit_body").matches("{}").count(),
                3,
                "{code}"
            );
        }
    }

    // 설정과 오류 종류에 따른 예약 처리.

    struct SettingsHost {
        enabled: Option<bool>,
        unsets: Mutex<Vec<String>>,
    }

    impl SettingsHost {
        fn new(enabled: Option<bool>) -> Self {
            Self {
                enabled,
                unsets: Mutex::new(Vec::new()),
            }
        }
    }

    impl HostCall for SettingsHost {
        fn call(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, tasty_plugin_sdk::PluginError> {
            match (method, params["storage_key"].as_str()) {
                ("settings.get_plugin_setting", Some(ENABLED_KEY)) => {
                    Ok(json!({ "value": self.enabled }))
                }
                ("settings.get_plugin_setting", Some(DELAY_KEY)) => Ok(json!({ "value": 3.0 })),
                ("settings.get_plugin_setting", _) => Ok(json!({ "value": null })),
                ("surface.foreground_process", _) => Ok(json!({ "name": "claude", "pid": 9 })),
                ("surface.meta.unset", _) => {
                    let key = params["key"].as_str().unwrap_or_default().to_string();
                    self.unsets.lock().expect("unsets").push(key);
                    Ok(json!({}))
                }
                _ => Ok(json!({})),
            }
        }
    }

    fn scheduled_after(host: &SettingsHost, error: Option<&str>) -> Option<Pending> {
        let t = Mutex::new(ResumeTable::default());
        let now = Instant::now();
        observe_hook(&t, host, "stop-failure", 7, error, now);
        lock_table(&t).due(7, now + Duration::from_secs(3)).cloned()
    }

    struct DelayHost(serde_json::Value);

    impl HostCall for DelayHost {
        fn call(
            &self,
            _method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, tasty_plugin_sdk::PluginError> {
            match params["storage_key"].as_str() {
                Some(DELAY_KEY) => Ok(json!({ "value": self.0 })),
                _ => Ok(json!({ "value": null })),
            }
        }
    }

    /// 손으로 고친 설정 파일은 매니페스트의 `max` 를 안 거친다 — 코드가 자른다.
    #[test]
    fn a_huge_delay_is_clamped_instead_of_panicking() {
        for v in [json!(1e300), json!(f64::MAX), json!(1e20)] {
            let s = read_settings(&DelayHost(v.clone()));
            assert_eq!(s.delay, Duration::from_secs_f64(MAX_DELAY_SECS), "{v}");
        }
        assert_eq!(
            read_settings(&DelayHost(json!(0.2))).delay,
            Duration::from_secs(1)
        );
        assert_eq!(
            read_settings(&DelayHost(json!(30.0))).delay,
            Duration::from_secs(30)
        );
    }

    /// 표에 직접 넘친 지연이 와도 패닉하지 않고, 예약하지 않는다(앞선 예약도 버린다).
    #[test]
    fn an_overflowing_delay_is_not_scheduled() {
        let mut t = ResumeTable::default();
        let now = Instant::now();
        t.schedule(7, "overloaded", now, Duration::from_secs(1), None);
        t.schedule(7, "overloaded", now, Duration::MAX, None);
        assert!(t.due(7, now + Duration::from_secs(5)).is_none());
        assert!(t.take_due(now + Duration::from_secs(5)).is_empty());
    }

    #[test]
    fn stop_failure_schedules_only_when_enabled_and_resumable() {
        let on = SettingsHost::new(Some(true));
        let p = scheduled_after(&on, Some("overloaded")).expect("scheduled");
        assert_eq!(p.foreground_pid, Some(9));
        assert!(scheduled_after(&on, Some("authentication_failed")).is_none());
        assert!(
            scheduled_after(&on, None).is_none(),
            "unknown 은 재개 대상이 아니다"
        );
        assert!(scheduled_after(&SettingsHost::new(Some(false)), Some("overloaded")).is_none());
        assert!(
            scheduled_after(&SettingsHost::new(None), Some("overloaded")).is_none(),
            "미설정은 기본값(꺼짐)"
        );
    }

    #[test]
    fn subagent_stop_does_not_end_the_failure_streak() {
        let t = Mutex::new(ResumeTable::default());
        let host = SettingsHost::new(Some(true));
        lock_table(&t).record_attempt(7);
        observe_hook(&t, &host, "subagent-stop", 7, None, Instant::now());
        assert!(lock_table(&t).limit_reached(7, 1));
        observe_hook(&t, &host, "stop", 7, None, Instant::now());
        assert!(!lock_table(&t).limit_reached(7, 1));
    }

    #[test]
    fn the_attempt_count_record_is_cleared_whenever_the_streak_resets() {
        let t = Mutex::new(ResumeTable::default());
        let host = SettingsHost::new(Some(true));
        let unsets = |h: &SettingsHost| std::mem::take(&mut *h.unsets.lock().expect("unsets"));

        // 보낸 적 없는 성공 턴은 IPC 를 쓰지 않는다 — 매 턴 오는 신호다.
        observe_hook(&t, &host, "stop", 7, None, Instant::now());
        assert!(unsets(&host).is_empty());

        lock_table(&t).record_attempt(7);
        observe_hook(&t, &host, "stop", 7, None, Instant::now());
        assert_eq!(unsets(&host), [COUNT_META_KEY]);

        // 새 세션·세션 종료는 표가 비어 있어도 지운다(plugin 재시작 뒤 남은 기록).
        observe_hook(&t, &host, "session-start", 7, None, Instant::now());
        assert_eq!(unsets(&host), [COUNT_META_KEY]);
        observe_hook(&t, &host, "session-end", 7, None, Instant::now());
        assert_eq!(unsets(&host), [COUNT_META_KEY]);

        // 재개 문구 자신이 부르는 새 턴은 계수를 지우지 않는다.
        lock_table(&t).record_attempt(7);
        observe_hook(&t, &host, "prompt-submit", 7, None, Instant::now());
        assert!(unsets(&host).is_empty());
    }

    // 전송과 횟수 기록의 순서 및 실패 처리.

    /// host 호출을 순서대로 적는다. 호출마다 그 순간 표의 시도 수를 함께 적어, 표 갱신이
    /// 어느 호출 앞뒤에 일어났는지 본다. `fail` 에 든 메서드는 에러를 돌려준다.
    struct RecordingHost<'a> {
        table: &'a Mutex<ResumeTable>,
        fail: Option<&'static str>,
        calls: Mutex<Vec<(String, serde_json::Value, u32)>>,
    }

    impl<'a> RecordingHost<'a> {
        fn new(table: &'a Mutex<ResumeTable>, fail: Option<&'static str>) -> Self {
            Self {
                table,
                fail,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn take_calls(&self) -> Vec<(String, serde_json::Value, u32)> {
            std::mem::take(&mut *self.calls.lock().expect("calls"))
        }
    }

    impl HostCall for RecordingHost<'_> {
        fn call(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, tasty_plugin_sdk::PluginError> {
            let attempts = attempts_of(self.table, 7);
            self.calls
                .lock()
                .expect("calls")
                .push((method.to_string(), params, attempts));
            if self.fail == Some(method) {
                return Err(tasty_plugin_sdk::PluginError::HostCall {
                    method: method.to_string(),
                    message: "mock: refused".to_string(),
                    code: None,
                });
            }
            Ok(json!({}))
        }
    }

    fn attempts_of(table: &Mutex<ResumeTable>, surface: u32) -> u32 {
        lock_table(table)
            .surfaces
            .get(&surface)
            .map_or(0, |e| e.attempts)
    }

    fn texts() -> Texts {
        Texts {
            message: "resume please".into(),
            limit_title: "limit".into(),
            limit_body: "{} {} {}".into(),
            limit_body_one: "{} {} {}".into(),
        }
    }

    /// 예약을 걸고 만기로 꺼낸 상태 — 만기 스레드가 `send` 에 넘기는 그 모양이다.
    fn taken_pending(table: &Mutex<ResumeTable>) -> Pending {
        let now = Instant::now();
        let mut t = lock_table(table);
        t.schedule(7, "overloaded", now, Duration::from_secs(1), Some(42));
        let mut due = t.take_due(now + Duration::from_secs(1));
        assert_eq!(due.len(), 1);
        due.remove(0).1
    }

    #[test]
    fn send_tells_first_then_counts_the_attempt_and_records_it() {
        let table = Mutex::new(ResumeTable::default());
        let host = RecordingHost::new(&table, None);
        let pending = taken_pending(&table);

        send(&table, &host, &texts(), 7, &pending);
        let calls = host.take_calls();
        let methods: Vec<&str> = calls.iter().map(|(m, _, _)| m.as_str()).collect();
        assert_eq!(methods, ["terminal.tell", "surface.meta.set"]);
        let (_, tell, before) = &calls[0];
        assert_eq!(tell, &json!({ "surface": 7, "text": "resume please" }));
        assert_eq!(*before, 0, "시도는 제출이 성공한 뒤에 센다");
        let (_, set, after) = &calls[1];
        assert_eq!(*after, 1, "메타데이터는 표를 올린 뒤에 적는다");
        assert_eq!(
            set,
            &json!({ "surface_id": 7, "key": COUNT_META_KEY, "value": "1" })
        );
        assert_eq!(attempts_of(&table, 7), 1);

        // 두 번째 재개는 누적 수를 적는다 — 적는 값은 표가 돌려준 수 그대로다.
        send(&table, &host, &texts(), 7, &pending);
        let calls = host.take_calls();
        assert_eq!(
            calls.last().map(|(m, p, _)| (m.as_str(), p)),
            Some((
                "surface.meta.set",
                &json!({ "surface_id": 7, "key": COUNT_META_KEY, "value": "2" })
            ))
        );
        assert_eq!(attempts_of(&table, 7), 2);
    }

    #[test]
    fn send_does_nothing_when_a_new_turn_began_after_the_take() {
        let table = Mutex::new(ResumeTable::default());
        let host = RecordingHost::new(&table, None);
        let pending = taken_pending(&table);
        lock_table(&table).on_new_turn(7, Instant::now());

        send(&table, &host, &texts(), 7, &pending);
        assert!(host.take_calls().is_empty(), "제출도 기록도 없다");
        assert_eq!(attempts_of(&table, 7), 0);
    }

    #[test]
    fn a_refused_tell_is_not_counted_or_recorded() {
        let table = Mutex::new(ResumeTable::default());
        let host = RecordingHost::new(&table, Some("terminal.tell"));
        let pending = taken_pending(&table);

        send(&table, &host, &texts(), 7, &pending);
        let methods: Vec<String> = host.take_calls().into_iter().map(|(m, _, _)| m).collect();
        assert_eq!(methods, ["terminal.tell"]);
        assert_eq!(attempts_of(&table, 7), 0);
    }

    /// 메타데이터 기록이 실패해도 보낸 사실은 표에 남는다 — 상한 판정은 표가 한다.
    #[test]
    fn a_failed_count_record_still_counts_the_attempt() {
        let table = Mutex::new(ResumeTable::default());
        let host = RecordingHost::new(&table, Some("surface.meta.set"));
        let pending = taken_pending(&table);

        send(&table, &host, &texts(), 7, &pending);
        let methods: Vec<String> = host.take_calls().into_iter().map(|(m, _, _)| m).collect();
        assert_eq!(methods, ["terminal.tell", "surface.meta.set"]);
        assert_eq!(attempts_of(&table, 7), 1);
    }
}
