//! Claude 터미널 출력의 오류 패턴과 오래 변하지 않는 출력을 감시한다.
//! surface.read_since_scan_mark의 전용 커서로 새 텍스트를 받아 매칭 창에 누적한다.
//! 에이전트가 쓰는 mark와 별개이므로 에이전트의 조회가 이 커서를 움직이지 않는다.
//!
//! launch·spawn·respawn에서 등록하고 폴링 중 대상 상태를 확인한다.
//! 자식은 surface 존재와 별도로 부모 관계를 확인해 release된 대상도 정리한다.
//! TopLevel 조회 오류는 대상 부재 오류까지 추적 유지로 처리하는 한계가 있다.
//! 새 턴에서는 중복 알림 기록과 정지 관측을 초기화하되 알림 간격 제한은 유지한다.

pub(crate) mod quiet;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::json;

use tasty_plugin_agent_common::host_call::HostCall;

use crate::task_watch::{FileActivity, TaskWatch};

/// 출력에서 찾는 오류 패턴.
const CLAUDE_ERROR_PATTERN: &str = r"(?i)(\bAPI Error\b|Output blocked by content filtering policy|\boverloaded_error\b|\brate_limit_error\b|\bInternal Server Error\b|\bnetwork error\b|\bBad Request\b)";

/// 누적 매칭 창의 바이트 상한. 호스트 OUTPUT_RETENTION_MAX_BYTES와 같은 값을 따로 선언한다.
/// 두 값이 달라지면 보관하는 출력 범위도 달라진다
/// (docs/features/terminal-output/index.md#출력-스캐너-전용-커서).
const SCAN_WINDOW_MAX: usize = 1_048_576;

/// 중복 알림 비교에 사용할 매칭 창 앞부분의 문자 수.
const DEDUPE_SNIPPET_CHARS: usize = 200;

static CLAUDE_ERROR_REGEX: OnceLock<Regex> = OnceLock::new();

fn claude_error_regex() -> &'static Regex {
    CLAUDE_ERROR_REGEX.get_or_init(|| {
        Regex::new(CLAUDE_ERROR_PATTERN).expect("ClaudeError catalog regex must compile")
    })
}

/// 오류 직후 출력 변화가 없을 때 정지 알림까지 기다리는 시간.
/// 재시도의 일시적인 대기를 정지로 오인하지 않도록 지연을 둔다.
const STALL_QUIET: Duration = Duration::from_secs(30);

/// 오류 뒤에 이 바이트 수보다 많은 출력이 이어졌으면 그 오류는 지금 조용한 이유로 보지 않는다.
/// 화면 한 장 정도의 분량이다. 알림 문구도 화면의 오류 줄로 고르므로 기준과 문구가 같은 오류를 가리킨다.
const RECENT_ERROR_BYTES: usize = 4096;

/// 오류 없이 조용한 경우의 대기 시간을 정하는 플러그인 설정 키. 알림을 끄는 값은 없다.
const NO_OUTPUT_NOTICE_KEY: &str = "no_output_notice_secs";

/// 설정이 없거나 숫자가 아닐 때의 기준. 긴 추론과 정지를 구분할 증거가 적어 오래 기다린다.
/// 호스트 CHILD_OUTPUT_SILENCE(자식의 stale 분류)와 별개의 값이다.
const NO_OUTPUT_NOTICE_DEFAULT: Duration = Duration::from_secs(600);

/// 설정값의 범위. 매니페스트 min·max와 같다. 설정 파일을 직접 고친 값에도 적용해, 오류 없는 기준이
/// 오류 직후 기준([`STALL_QUIET`])보다 짧아지지 않게 한다.
const NO_OUTPUT_NOTICE_MIN_SECS: f64 = 60.0;
const NO_OUTPUT_NOTICE_MAX_SECS: f64 = 86_400.0;

/// 설정을 다시 읽는 간격. 설정 변경 이벤트가 없어 다시 읽으며, surface 수와 무관하게 이 간격으로 한 번만 묻는다.
const NO_OUTPUT_NOTICE_REREAD: Duration = Duration::from_secs(30);

/// 출력 파일을 찾지 못했다는 진단 경고까지 기다리는 시간. 알림 기준과 무관하게 경로 규칙 변경을 일찍 알린다.
const LOCATE_WARN_AFTER: Duration = Duration::from_secs(120);

/// 출력 파일을 찾지 못한 대기의 화면 출력 기준. 설정값이 더 크면 설정값을 쓴다. Claude Code가 Stop payload로
/// 알려 준 대기이므로 일반 기준보다 짧아지지 않으며, 대기 한 번에 한 번만 알린다.
const STALL_QUIET_BACKGROUND_WAIT: Duration = Duration::from_secs(600);

/// 같은 surface에 정지 알림을 다시 보낼 때 지킬 최소 간격.
const STALL_NOTIFY_COOLDOWN: Duration = Duration::from_secs(300);

/// 정지 의심 알림용 이벤트. 매니페스트의 hook_events와 이름이 같아야 한다.
/// 오류 없는 정지도 알리지만 기존 구독과 호환되도록 이름은 유지한다.
pub(crate) const STALLED_EVENT: &str = "claude-error-stalled";

/// `text`(ANSI-stripped 권장)에 알려진 Claude 에러 패턴이 포함됐는지.
pub(crate) fn detect_claude_error(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    claude_error_regex().is_match(text)
}

/// `text` 에서 에러 패턴이 매치된 **첫 줄** 전체를 돌려준다 — 알림에 붙일 힌트용.
/// 매칭 규칙 자체는 [`detect_claude_error`] 와 같은 정규식을 그대로 쓴다.
pub(crate) fn first_error_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && claude_error_regex().is_match(line))
}

/// 독립 surface는 surface.locate, 자식은 terminal.parent로 추적 유지 여부를 판단한다.
/// release는 surface를 남기고 부모 관계만 해제하므로 자식은 존재 여부만 보면 안 된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanTarget {
    TopLevel,
    Child,
}

/// 스캐너의 모든 잠금에서 poison을 알리고 기존 상태를 재사용한다.
/// 복구가 패닉 전의 부분 변경을 되돌리는 것은 아니다.
pub(crate) fn lock_scanner(
    scanner: &std::sync::Mutex<ErrorScanner>,
) -> std::sync::MutexGuard<'_, ErrorScanner> {
    const WHAT: &str = "the claude error scanner";
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    tasty_utils::poison::recover_mutex(scanner.lock(), WHAT, &REPORTED)
}

#[derive(Default)]
pub struct ErrorScanner {
    /// 추적할 surface와 등록 경로. launch·spawn·respawn에서 넣고 kill·폴링 정리에서 뺀다.
    enabled: HashMap<u32, ScanTarget>,
    /// surface별로 마지막 알림에 사용한 창의 앞부분. 같은 값이면 중복 알림을 생략한다.
    last_fired: HashMap<u32, String>,
    /// 출력 변화가 멈춘 기간을 추정할 관측값.
    watch: HashMap<u32, OutputWatch>,
    /// 전용 커서로 받은 텍스트를 누적한 매칭 창. SCAN_WINDOW_MAX를 넘으면 앞에서 자른다.
    window: HashMap<u32, String>,
    /// 이번 턴의 마지막 출력. RECENT_ERROR_BYTES를 넘으면 앞에서 자르고 새 턴에서 비운다.
    recent_output: HashMap<u32, String>,
    /// surface별 마지막 정지 알림 시각.
    last_stall_notify: HashMap<u32, Instant>,
    /// 백그라운드 작업을 기다리는 surface. 대기 Stop에서 넣고 턴 종료·새 턴·세션 종료에서 뺀다.
    background_wait: HashMap<u32, BackgroundWait>,
    /// 정지 알림을 meta [`STALL_NOTICE_META_KEY`] 에 남긴 surface. 그 조용한 구간이 끝나면 meta 를 지운다.
    notice_recorded: std::collections::HashSet<u32>,
    /// 오류 없는 정지 알림 기준의 설정값.
    no_output_notice: NoOutputNotice,
    /// 무출력 알림 선언과 끝난 선언의 묶음. 처음 볼 때 meta 에서 되살린다.
    quiet: quiet::QuietTable,
}

/// 마지막으로 읽은 오류 없는 정지 알림 기준과 그 시각.
struct NoOutputNotice {
    threshold: Duration,
    read_at: Option<Instant>,
}

impl Default for NoOutputNotice {
    fn default() -> Self {
        Self {
            threshold: NO_OUTPUT_NOTICE_DEFAULT,
            read_at: None,
        }
    }
}

/// 설정 값을 기준 시간으로 바꾼다. 숫자가 아니면 기본값이고, 범위 밖이면 범위 안으로 조정한다.
fn parse_no_output_notice(value: Option<&serde_json::Value>) -> Duration {
    value
        .and_then(serde_json::Value::as_f64)
        .filter(|secs| secs.is_finite())
        .map(|secs| secs.clamp(NO_OUTPUT_NOTICE_MIN_SECS, NO_OUTPUT_NOTICE_MAX_SECS))
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
        .unwrap_or(NO_OUTPUT_NOTICE_DEFAULT)
}

/// 정지 알림을 보낸 시각(`at_ms`, Unix ms)을 남기는 surface meta. 플러그인이 다시 시작돼도 같은 조용한 구간을
/// 다시 알리지 않고 알림 간격도 이어 가려는 것이다. 출력이 바뀌거나 새 턴이 시작되면 지운다.
pub(crate) const STALL_NOTICE_META_KEY: &str = "claude-stall-notice";

/// 현재 시각(Unix ms).
pub(crate) fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 백그라운드 작업 대기 한 번의 기록.
struct BackgroundWait {
    /// 대기가 시작된 시각(Unix ms). 알림 문구의 경과 시간에 쓴다.
    since_ms: u64,
    /// 이 대기에서 정지 알림을 보냈는지.
    notified: bool,
    /// 기다리는 작업과 출력 파일을 찾을 단서. 없으면 화면 출력 기준(10분)만 쓴다.
    watch: Option<TaskWatch>,
    /// 찾은 출력 파일의 관측 상태.
    files: Option<FileActivity>,
    /// 마지막으로 출력 파일을 찾아본 시각. 못 찾으면 [`LOCATE_RETRY`] 뒤에 다시 찾는다.
    last_locate: Option<Instant>,
    /// 대상이 있는 출력 파일을 못 본 채 지난 구간의 시작. 링크 대상은 Stop 직후 잠시 없을 수 있어 바로 경고하지 않는다.
    unresolved_since: Option<Instant>,
    /// 못 찾았다는 경고를 이 대기에서 남겼는지.
    warned: bool,
    /// 기다리는 작업의 종류.
    kinds: WaitKinds,
}

/// 기다리는 작업의 종류. 대기 meta 의 `types`·`notice_types` 다.
#[derive(Default)]
pub(crate) struct WaitKinds {
    /// 모든 작업. `notify-stop-failure` 문구가 쓴다.
    pub all: Vec<String>,
    /// Monitor 를 뺀 작업. 정지 알림의 기준과 문구가 쓴다.
    pub noticed: Vec<String>,
}

/// surface meta `claude-background-wait` 에 남긴 대기. 플러그인이 다시 시작되면 이 값으로 대기를 되살린다.
pub(crate) struct SavedWait {
    pub since_ms: u64,
    pub notified: bool,
    pub kinds: WaitKinds,
    pub watch: Option<TaskWatch>,
}

impl SavedWait {
    /// meta 문자열을 읽는다. JSON 이 아니거나 시작 시각이 없으면 `None` 이다.
    pub fn parse(raw: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(raw)
            .inspect_err(|e| tracing::warn!("claude background wait meta is not JSON ({e}): {raw}"))
            .ok()?;
        let names = |key: &str| {
            v.get(key).and_then(serde_json::Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
        };
        let all = names("types").unwrap_or_default();
        // 이 필드가 없는 대기 meta 는 Monitor 를 가리지 않던 때의 것이라 모두 알림 대상으로 본다.
        let noticed = names("notice_types").unwrap_or_else(|| all.clone());
        Some(Self {
            since_ms: v.get("since_ms")?.as_u64()?,
            notified: v
                .get("notified")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            kinds: WaitKinds { all, noticed },
            watch: v.get("watch").and_then(TaskWatch::from_meta),
        })
    }
}

/// 출력 파일을 못 찾았을 때 다시 찾는 간격. Claude Code 가 폴더를 늦게 만들 수 있어 한 번에 포기하지 않는다.
const LOCATE_RETRY: Duration = Duration::from_secs(10);

impl BackgroundWait {
    fn new(since_ms: u64) -> Self {
        Self {
            since_ms,
            notified: false,
            watch: None,
            files: None,
            last_locate: None,
            unresolved_since: None,
            warned: false,
            kinds: WaitKinds::default(),
        }
    }

    fn restored(saved: SavedWait) -> Self {
        Self {
            notified: saved.notified,
            watch: saved.watch,
            kinds: saved.kinds,
            ..Self::new(saved.since_ms)
        }
    }

    /// 대기 meta 값. 알림 여부와 출력 파일 단서를 함께 남겨 플러그인이 다시 시작돼도 같은 대기로 이어 간다.
    fn meta_value(&self) -> String {
        let mut v = json!({
            "since_ms": self.since_ms,
            "tasks": self.kinds.all.len(),
            "types": self.kinds.all,
            "notice_types": self.kinds.noticed,
            "notified": self.notified,
        });
        if let Some(watch) = &self.watch {
            v["watch"] = watch.to_meta();
        }
        v.to_string()
    }

    /// 기다리는 작업이 모두 Monitor 인가. Monitor 는 조용한 것이 정상이라 이 대기에는 정지 알림을 보내지 않는다.
    /// 종류를 모르는 대기(`waiting_on_background_work` 만 있음)는 해당하지 않는다.
    fn monitors_only(&self) -> bool {
        !self.kinds.all.is_empty() && self.kinds.noticed.is_empty()
    }

    /// 기다리는 작업에 Monitor 가 있는가. Monitor 이벤트는 사람이나 부모 없이도 새 턴을 연다.
    fn has_monitors(&self) -> bool {
        self.kinds.all.len() > self.kinds.noticed.len()
    }

    /// 기다리는 작업의 출력 파일이 모두 활동 없이 지난 시간. 파일을 찾지 못했거나 대상이 있는 파일이 없으면
    /// (transcript 가 없는 서브에이전트 링크뿐이면) `None` 이며 화면 기준으로 돌아간다.
    fn file_quiet(&mut self, surface_id: u32, now: Instant) -> Option<Duration> {
        let watch = self.watch.as_ref()?;
        let due = self
            .last_locate
            .is_none_or(|t| now.saturating_duration_since(t) >= LOCATE_RETRY);
        if self.files.is_none() && due {
            self.last_locate = Some(now);
            self.files = crate::task_watch::tasks_dir(watch)
                .and_then(|dir| FileActivity::locate(watch, &dir));
        }
        let quiet = self
            .files
            .as_mut()
            .and_then(|f| f.poll(std::time::SystemTime::now()));
        if quiet.is_some() {
            self.unresolved_since = None;
            return quiet;
        }
        let since = *self.unresolved_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= LOCATE_WARN_AFTER && !self.warned {
            self.warned = true;
            tracing::warn!(
                "claude stall s{surface_id}: no output file found for background task(s) {:?} of session {} — falling back to the screen output rule",
                watch
                    .tasks
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect::<Vec<_>>(),
                watch.session_id
            );
        }
        None
    }
}

/// 누적 창의 해시 변화로 출력 변화를 추정한다. 실제 작업 진행 여부를 확정하지는 않는다.
struct OutputWatch {
    /// 마지막으로 본 텍스트의 해시.
    fingerprint: u64,
    /// 지문이 마지막으로 바뀐 시각.
    last_change: Instant,
    /// 현재 조용한 구간에서 알림을 보냈는지. 창의 해시가 바뀌면 초기화한다.
    stall_notified: bool,
    /// 이번 턴의 마지막 출력([`RECENT_ERROR_BYTES`])에 오류가 있었는지. 정지 알림까지 기다릴 시간을 고른다.
    saw_error: bool,
    /// 이 조용한 구간에서 상태가 정지 알림을 허용하지 않음(idle·needs_input·exited)을 확인했는지.
    /// 그 상태는 hook 이 보고한 값이라 출력 변화나 이 surface 의 hook 없이는 바뀌지 않으므로,
    /// 그때까지 대기 기록·상태 조회를 tick 마다 반복하지 않는다.
    settled: bool,
}

fn output_fingerprint(text: &str) -> u64 {
    // 앞부분이 같아도 뒤에 붙은 출력은 감지해야 하므로 창 전체의 해시를 쓴다.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// UTF-8 문자 경계를 지키며 앞에서 잘라 바이트 상한 이하로 줄인다.
fn trim_window(window: &mut String, max: usize) {
    if window.len() <= max {
        return;
    }
    let want = window.len() - max;
    let cut = (want..=window.len())
        .find(|i| window.is_char_boundary(*i))
        .unwrap_or(window.len());
    window.drain(..cut);
}

/// 백그라운드 대기 여부와 오류 유무에 따라 정지 알림까지 기다릴 시간을 고른다.
fn stall_threshold(saw_error: bool, background_wait: bool, no_output: Duration) -> Duration {
    if background_wait {
        STALL_QUIET_BACKGROUND_WAIT.max(no_output)
    } else if saw_error {
        STALL_QUIET
    } else {
        no_output
    }
}

/// 상태 조회(IPC) 전에 값싸게 거를 수 있는 조건 — 무출력 지속시간 · 중복 · 쿨다운.
fn stall_pre_gate(
    quiet: Duration,
    already_notified: bool,
    since_last_notify: Option<Duration>,
    threshold: Duration,
) -> bool {
    if already_notified || quiet < threshold {
        return false;
    }
    !matches!(since_last_notify, Some(d) if d < STALL_NOTIFY_COOLDOWN)
}

/// active·stale에서만 정지 의심 알림을 허용한다. 두 상태가 실제 정지를 확정하는 것은 아니다.
/// idle·needs_input·exited는 기존 완료 알림 대상이라 여기서 다시 알리지 않는다.
fn state_allows_stall_notice(child_state: &str) -> bool {
    matches!(child_state, "active" | "stale")
}

/// 정지 알림을 보낼지의 최종 판정(순수 함수).
fn should_notify_stall(
    quiet: Duration,
    child_state: &str,
    already_notified: bool,
    since_last_notify: Option<Duration>,
    threshold: Duration,
) -> bool {
    stall_pre_gate(quiet, already_notified, since_last_notify, threshold)
        && state_allows_stall_notice(child_state)
}

impl ErrorScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// 추적 대상을 등록한다. 같은 surface는 등록 경로만 바꾸고 중복 알림 기록은 유지한다.
    pub fn enable(&mut self, surface_id: u32, target: ScanTarget) {
        self.enabled.insert(surface_id, target);
    }

    /// 추적을 끝낸다. 무출력 선언도 끝내며 쌓인 것은 error 완료 줄을 잠시 기다린다.
    pub fn stop_tracking<H: HostCall>(&mut self, host: &H, surface_id: u32) {
        self.drop_quiet(host, surface_id);
        self.disable(surface_id);
    }

    pub fn disable(&mut self, surface_id: u32) {
        self.enabled.remove(&surface_id);
        self.last_fired.remove(&surface_id);
        self.watch.remove(&surface_id);
        self.last_stall_notify.remove(&surface_id);
        self.window.remove(&surface_id);
        self.recent_output.remove(&surface_id);
        self.background_wait.remove(&surface_id);
        self.notice_recorded.remove(&surface_id);
    }

    /// 오류 없는 정지 알림 기준을 정해진 간격으로 다시 읽는다. 조회에 실패하면 마지막 값을 유지한다.
    pub fn refresh_no_output_notice<H: HostCall>(&mut self, host: &H, now: Instant) {
        let setting = &mut self.no_output_notice;
        if setting
            .read_at
            .is_some_and(|t| now.saturating_duration_since(t) < NO_OUTPUT_NOTICE_REREAD)
        {
            return;
        }
        setting.read_at = Some(now);
        match host.call(
            "settings.get_plugin_setting",
            json!({ "storage_key": NO_OUTPUT_NOTICE_KEY }),
        ) {
            Ok(reply) => setting.threshold = parse_no_output_notice(reply.get("value")),
            Err(e) => {
                tracing::warn!("claude stall: reading {NO_OUTPUT_NOTICE_KEY} failed: {e}");
            }
        }
    }

    /// 재시작 전에 남긴 정지 알림을 되살린다. 누적 창이 빈 상태에서 시작하므로 빈 창의 해시를 기준으로 삼는다.
    /// 첫 스캔에서 새 출력이 없으면 같은 조용한 구간으로 보고 다시 알리지 않는다. 새 출력이 있으면 해시가 바뀌어
    /// 평소처럼 다시 센다. 알림 간격은 남긴 시각부터 잇는다.
    pub(crate) fn restore_stall_notice(&mut self, surface_id: u32, at_ms: u64, now: Instant) {
        if self.watch.contains_key(&surface_id) {
            return;
        }
        let elapsed = Duration::from_millis(unix_now_ms().saturating_sub(at_ms));
        self.watch.insert(
            surface_id,
            OutputWatch {
                fingerprint: output_fingerprint(""),
                last_change: now,
                stall_notified: true,
                saw_error: false,
                settled: false,
            },
        );
        self.last_stall_notify
            .insert(surface_id, now.checked_sub(elapsed).unwrap_or(now));
        self.notice_recorded.insert(surface_id);
    }

    /// 대기 Stop을 기록하고 대기 시작 시각(Unix ms)을 돌려준다. 이어진 대기 Stop은 처음 기록을 유지한다.
    pub fn mark_background_wait(&mut self, surface_id: u32, now_ms: u64) -> u64 {
        self.background_wait
            .entry(surface_id)
            .or_insert_with(|| BackgroundWait::new(now_ms))
            .since_ms
    }

    /// 대기에서 관측할 작업을 바꾼다. 작업 목록이 바뀌면 출력 파일을 다시 찾는다. 알림 여부는 유지한다.
    pub fn set_task_watch(&mut self, surface_id: u32, watch: Option<TaskWatch>) {
        if let Some(b) = self.background_wait.get_mut(&surface_id)
            && b.watch != watch
        {
            b.watch = watch;
            b.files = None;
            b.last_locate = None;
        }
    }

    /// 시험 전용 — 찾은 출력 파일을 직접 넣는다. 실제 임시 폴더를 쓰지 않으려는 것이다.
    #[cfg(test)]
    pub(crate) fn set_file_activity_for_test(
        &mut self,
        surface_id: u32,
        watch: TaskWatch,
        files: FileActivity,
    ) {
        if let Some(b) = self.background_wait.get_mut(&surface_id) {
            b.watch = Some(watch);
            b.files = Some(files);
        }
    }

    /// 대기 Stop 을 기록하고 대기 meta 값을 돌려준다. 이어진 대기 Stop 은 처음 시작 시각과 알림 여부를 유지한다.
    pub fn record_background_wait(
        &mut self,
        surface_id: u32,
        now_ms: u64,
        kinds: WaitKinds,
        watch: Option<TaskWatch>,
    ) -> String {
        self.mark_background_wait(surface_id, now_ms);
        self.set_task_watch(surface_id, watch);
        let b = self
            .background_wait
            .entry(surface_id)
            .or_insert_with(|| BackgroundWait::new(now_ms));
        b.kinds = kinds;
        b.meta_value()
    }

    /// meta 에 남은 대기를 되살린다. 이미 기록이 있으면(그사이 대기 Stop 이 먼저 왔으면) 그 기록을 둔다.
    pub(crate) fn restore_background_wait(&mut self, surface_id: u32, saved: SavedWait) {
        self.background_wait
            .entry(surface_id)
            .or_insert_with(|| BackgroundWait::restored(saved));
    }

    /// 백그라운드 대기 기록을 지운다.
    pub fn clear_background_wait(&mut self, surface_id: u32) {
        self.background_wait.remove(&surface_id);
    }

    /// 테스트 전용 — 대기 기록 존재 여부.
    #[cfg(test)]
    pub(crate) fn is_waiting_on_background_work(&self, surface_id: u32) -> bool {
        self.background_wait.contains_key(&surface_id)
    }

    pub fn is_enabled(&self, surface_id: u32) -> bool {
        self.enabled.contains_key(&surface_id)
    }

    /// 테스트 전용 — 등록 경로 확인.
    #[cfg(test)]
    pub(crate) fn target_of(&self, surface_id: u32) -> Option<ScanTarget> {
        self.enabled.get(&surface_id).copied()
    }

    /// 이 surface 의 hook 이 왔다. hook 은 상태를 바꿀 수 있으므로 조용한 구간의 상태 확인을 다시 하게 한다.
    pub fn note_hook(&mut self, surface_id: u32) {
        if let Some(w) = self.watch.get_mut(&surface_id) {
            w.settled = false;
        }
    }

    /// 시험 전용 — 조용한 구간의 상태 확인을 마친 것으로 둔다.
    #[cfg(test)]
    pub(crate) fn settle_for_test(&mut self, surface_id: u32) {
        self.watch.insert(
            surface_id,
            OutputWatch {
                fingerprint: 0,
                last_change: Instant::now(),
                stall_notified: false,
                saw_error: false,
                settled: true,
            },
        );
    }

    /// 시험 전용 — 정지 관측이 있는지.
    #[cfg(test)]
    pub(crate) fn is_watching(&self, surface_id: u32) -> bool {
        self.watch.contains_key(&surface_id)
    }

    /// 시험 전용 — 조용한 구간의 상태 확인을 마쳤는지.
    #[cfg(test)]
    pub(crate) fn is_settled(&self, surface_id: u32) -> bool {
        self.watch.get(&surface_id).is_some_and(|w| w.settled)
    }

    /// 새 턴을 위해 중복 알림 기록과 정지 관측을 초기화한다.
    /// 턴마다 오류가 반복돼도 알림이 너무 잦아지지 않도록 마지막 알림 시각은 유지한다.
    pub fn reset_dedupe(&mut self, surface_id: u32) {
        self.last_fired.remove(&surface_id);
        self.watch.remove(&surface_id);
        // 앞 턴의 오류가 이번 턴의 조용한 구간을 오류 직후로 만들지 않게 한다.
        self.recent_output.remove(&surface_id);
        // 호스트 출력 버퍼가 턴마다 비워지지 않으므로 누적 매칭 창도 유지한다.
    }

    /// enabled set의 snapshot. polling thread가 lock을 짧게 잡고 빠져나오도록.
    pub fn enabled_snapshot(&self) -> Vec<(u32, ScanTarget)> {
        self.enabled.iter().map(|(&sid, &t)| (sid, t)).collect()
    }

    /// 호스트 IPC 없이 중복 알림 상태를 준비하는 시험용 함수.
    #[cfg(test)]
    pub(crate) fn seed_dedupe_for_test(&mut self, surface_id: u32, snippet: &str) {
        self.last_fired.insert(surface_id, snippet.to_string());
    }

    /// 테스트 전용 — 현재 조용한 구간에서 정지 알림을 보낸 것으로 기록됐는지.
    #[cfg(test)]
    pub(crate) fn stall_notified_for_test(&self, surface_id: u32) -> bool {
        self.watch
            .get(&surface_id)
            .is_some_and(|w| w.stall_notified)
    }

    /// 테스트 전용 — dedupe 상태 존재 여부.
    #[cfg(test)]
    pub(crate) fn has_dedupe_state(&self, surface_id: u32) -> bool {
        self.last_fired.contains_key(&surface_id)
    }

    /// 새 출력을 누적해 오류를 찾고 정지 의심 여부도 확인한다.
    /// 오류 훅 요청이 성공하면 스니펫을 반환한다. 수신 훅이 실제 실행됐다는 보장은 아니다.
    pub fn scan_one<H: HostCall>(&mut self, host: &H, surface_id: u32) -> Option<String> {
        self.scan_one_at(host, surface_id, Instant::now())
    }

    /// 실제로 기다리지 않고 출력 정지 기간을 시험하도록 시각을 전달받는다.
    fn scan_one_at<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        now: Instant,
    ) -> Option<String> {
        let resp = host
            .call(
                "surface.read_since_scan_mark",
                json!({
                    "surface_id": surface_id,
                    "strip_ansi": true,
                }),
            )
            .ok()?;
        // 새로 받은 텍스트만으로는 분할된 패턴을 놓칠 수 있어 창에 누적한다.
        let delta = resp.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let window = self.window.entry(surface_id).or_default();
        window.push_str(delta);
        trim_window(window, SCAN_WINDOW_MAX);
        let has_error = detect_claude_error(window);
        let fingerprint = output_fingerprint(window);
        let snippet: String = window.chars().take(DEDUPE_SNIPPET_CHARS).collect();
        let recent = self.recent_output.entry(surface_id).or_default();
        recent.push_str(delta);
        trim_window(recent, RECENT_ERROR_BYTES);
        // 창에는 오래된 오류도 남으므로 정지 기준은 마지막 출력의 오류만 본다.
        let recent_error = detect_claude_error(recent);
        // 오류를 발견하기 전부터 창의 변화를 추적한다.
        self.track_output(surface_id, fingerprint, now, recent_error);
        self.forget_ended_notice(host, surface_id);

        let mut fired = None;
        if has_error {
            // 창의 앞부분이 마지막 알림과 같으면 오류 알림을 반복하지 않는다.
            let already_fired = self.last_fired.get(&surface_id) == Some(&snippet);
            if !already_fired {
                let fire_result = host.call(
                    "surface.fire_hook",
                    json!({
                        "surface_id": surface_id,
                        "event": "claude-error",
                    }),
                );
                match fire_result {
                    Ok(_) => {
                        self.last_fired.insert(surface_id, snippet.clone());
                        fired = Some(snippet);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "claude error fire_hook failed for surface {surface_id}: {e}"
                        );
                    }
                }
            }
        }

        // 오류가 없거나 중복 알림을 생략한 경우에도 정지 의심 여부는 확인한다.
        self.maybe_notify_stall(host, surface_id, now);
        fired
    }

    /// 알림을 남긴 조용한 구간이 끝났으면(출력 변화·새 턴) 알림 meta 를 지운다.
    fn forget_ended_notice<H: HostCall>(&mut self, host: &H, surface_id: u32) {
        if !self.notice_recorded.contains(&surface_id)
            || self
                .watch
                .get(&surface_id)
                .is_some_and(|w| w.stall_notified)
        {
            return;
        }
        self.notice_recorded.remove(&surface_id);
        if let Err(e) = host.call(
            "surface.meta.unset",
            json!({ "surface_id": surface_id, "key": STALL_NOTICE_META_KEY }),
        ) {
            tracing::warn!(
                "claude stall s{surface_id}: clearing the stall notice meta failed: {e}"
            );
        }
    }

    /// 창의 해시가 바뀌면 조용한 구간을 다시 센다. 같은 창을 다시 해시하지 않도록 값으로 받는다.
    fn track_output(
        &mut self,
        surface_id: u32,
        fingerprint: u64,
        now: Instant,
        recent_error: bool,
    ) {
        match self.watch.get_mut(&surface_id) {
            Some(w) if w.fingerprint == fingerprint => {}
            Some(w) => {
                w.fingerprint = fingerprint;
                w.last_change = now;
                w.stall_notified = false;
                w.saw_error = recent_error;
                w.settled = false;
            }
            None => {
                self.watch.insert(
                    surface_id,
                    OutputWatch {
                        fingerprint,
                        last_change: now,
                        stall_notified: false,
                        saw_error: recent_error,
                        settled: false,
                    },
                );
            }
        }
    }

    /// 출력 변화가 오래 없고 자식이 active·stale이면 정지 의심 훅을 요청한다.
    /// terminal.set_state는 호출하지 않아 자식 상태를 바꾸지는 않는다.
    fn maybe_notify_stall<H: HostCall>(&mut self, host: &H, surface_id: u32, now: Instant) {
        let Some(w) = self.watch.get(&surface_id) else {
            return;
        };
        if w.settled {
            return;
        }
        let quiet = now.saturating_duration_since(w.last_change);
        let no_output = self.no_output_notice.threshold;
        // 플러그인이 다시 시작되면 메모리의 대기 기록은 사라지고 surface meta 는 남는다. 알림 문구는 meta 를
        // 읽으므로, 일반 기준으로 알리기 전에 meta 에서 기록을 되살려 기준과 문구가 같은 대기를 가리키게 한다.
        if !self.background_wait.contains_key(&surface_id)
            && stall_pre_gate(
                quiet,
                w.stall_notified,
                self.last_stall_notify
                    .get(&surface_id)
                    .map(|t| now.saturating_duration_since(*t)),
                stall_threshold(w.saw_error, false, no_output),
            )
            && let Some(saved) = saved_wait_from_meta(host, surface_id)
        {
            tracing::info!(
                "claude stall s{surface_id}: restored the background wait from its surface meta"
            );
            self.restore_background_wait(surface_id, saved);
        }
        if self
            .background_wait
            .get(&surface_id)
            .is_some_and(BackgroundWait::monitors_only)
        {
            return;
        }
        let Some(w) = self.watch.get(&surface_id) else {
            return;
        };
        // 기다리는 작업의 출력 파일을 찾았으면 화면 대신 그 파일의 활동으로 일반 기준을 적용한다.
        let file_quiet = self
            .background_wait
            .get_mut(&surface_id)
            .and_then(|b| b.file_quiet(surface_id, now));
        let wait = self.background_wait.get(&surface_id);
        // 대기 한 번에 한 번만 알린다. 출력 변화로 stall_notified가 풀려도 다시 알리지 않는다.
        let already_notified = w.stall_notified || wait.is_some_and(|b| b.notified);
        let (quiet, threshold) = match file_quiet {
            Some(file_quiet) => (file_quiet, no_output),
            None => (
                quiet,
                stall_threshold(w.saw_error, wait.is_some(), no_output),
            ),
        };
        let since_last_notify = self
            .last_stall_notify
            .get(&surface_id)
            .map(|t| now.saturating_duration_since(*t));
        if !stall_pre_gate(quiet, already_notified, since_last_notify, threshold) {
            return;
        }

        // 시간·중복·알림 간격 조건을 통과했을 때만 호스트 상태를 조회한다.
        let child_state = host
            .call("terminal.state", json!({ "surface": surface_id }))
            .ok()
            .and_then(|r| {
                r.get("state")
                    .and_then(|v| v.as_str())
                    .map(str::to_ascii_lowercase)
            })
            .unwrap_or_default();
        if !should_notify_stall(
            quiet,
            &child_state,
            already_notified,
            since_last_notify,
            threshold,
        ) {
            // 조회에 실패했으면(빈 값) 다음 tick 에 다시 묻는다.
            if !child_state.is_empty()
                && let Some(w) = self.watch.get_mut(&surface_id)
            {
                w.settled = true;
            }
            return;
        }

        if let Some(b) = wait.filter(|_| file_quiet.is_some()) {
            // Monitor 이벤트가 새 턴을 열어 대기가 다시 시작돼도, 마지막 알림 뒤로 활동이 없는 같은 작업이면 같은 조용한
            // 구간이다. 이 대기를 알린 것으로 두어 tick 마다 다시 묻지 않는다. Monitor 가 없으면 부모의 지시 등으로 시작된
            // 새 대기이므로 지금처럼 다시 알려 턴을 깨운다.
            if b.has_monitors() && already_noticed_quiet_tasks(host, surface_id, b, quiet) {
                self.record_wait_notice(host, surface_id);
                return;
            }
        }
        // 대기가 아니면 오류 뒤 멈춤(error)과 오류 없는 멈춤(warning)을 여기서 가른다. 문구도 이 판정을 따른다.
        let wording = wait.is_none().then(|| StallWording::read(host, surface_id));
        if wording.as_ref().is_some_and(|w| w.error_line.is_some()) {
            // error 알림은 선언과 관계없이 바로 보내고 그 surface 의 선언을 모두 끝낸다. 쌓인 것은 error 줄이 가져간다.
            self.end_quiet_on_error(host, surface_id);
            self.fire_stall(host, surface_id, now, wording.as_ref());
            return;
        }
        let notice = match wait {
            Some(b) => match (&b.files, file_quiet) {
                (Some(files), Some(_)) => quiet::QuietNotice::Tasks(files.quiet_tasks()),
                _ => quiet::QuietNotice::Wait {
                    ids: b
                        .watch
                        .as_ref()
                        .map(TaskWatch::task_ids)
                        .unwrap_or_default(),
                    label: wait_label(&b.kinds),
                },
            },
            None => quiet::QuietNotice::Turn,
        };
        let task_quiet = matches!(notice, quiet::QuietNotice::Tasks(_));
        let labels = match &notice {
            quiet::QuietNotice::Tasks(t) => t.iter().map(|(_, l)| l.clone()).collect(),
            _ => Vec::new(),
        };
        let labels = match self.dispose_quiet(host, surface_id, notice, quiet.as_millis() as u64) {
            quiet::Send::All => labels,
            quiet::Send::Labels(rest) => rest,
            quiet::Send::Nothing => {
                // 보류나 버림도 알린 것과 같게 기록해 같은 조용한 구간·대기를 tick 마다 다시 판정하지 않는다.
                self.record_notice(host, surface_id, wording.as_ref());
                self.mark_stall_handled(host, surface_id, now);
                return;
            }
        };
        if task_quiet && let Some(b) = self.background_wait.get(&surface_id) {
            record_quiet_tasks(host, surface_id, b, quiet, labels);
        }
        self.fire_stall(host, surface_id, now, wording.as_ref());
    }

    /// 이 조용한 구간·대기를 알린 것으로 기록한다.
    fn mark_stall_handled<H: HostCall>(&mut self, host: &H, surface_id: u32, now: Instant) {
        if let Some(w) = self.watch.get_mut(&surface_id) {
            w.stall_notified = true;
        }
        self.record_wait_notice(host, surface_id);
        self.last_stall_notify.insert(surface_id, now);
    }

    /// 알림 기록을 남기고 정지 의심 훅을 요청한다. 성공하면 같은 정지 구간·대기에서 다시 알리지 않도록 기록한다.
    /// 기록을 먼저 쓰는 것은 훅이 실행한 문구 처리가 이 판정을 읽게 하려는 것이다. 요청이 실패하면 기록을 지운다.
    fn fire_stall<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        now: Instant,
        wording: Option<&StallWording>,
    ) {
        self.record_notice(host, surface_id, wording);
        if let Err(e) = host.call(
            "surface.fire_hook",
            json!({
                "surface_id": surface_id,
                "event": STALLED_EVENT,
            }),
        ) {
            tracing::warn!("claude stall fire_hook failed for surface {surface_id}: {e}");
            if self.notice_recorded.remove(&surface_id)
                && let Err(e) = host.call(
                    "surface.meta.unset",
                    json!({ "surface_id": surface_id, "key": STALL_NOTICE_META_KEY }),
                )
            {
                tracing::warn!(
                    "claude stall s{surface_id}: clearing the stall notice meta failed: {e}"
                );
            }
            return;
        }
        self.mark_stall_handled(host, surface_id, now);
    }

    /// 플러그인이 다시 시작돼도 같은 조용한 구간을 다시 알리지 않도록 알림 시각을 남긴다. 대기가 아닌 정지면 문구 종류와
    /// 오류 줄도 함께 남긴다.
    fn record_notice<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        wording: Option<&StallWording>,
    ) {
        let mut value = json!({ "at_ms": unix_now_ms() });
        if let Some(wording) = wording {
            value["kind"] = json!(wording.kind());
            if let Some(hint) = &wording.error_line {
                value["hint"] = json!(hint);
            }
        }
        match host.call(
            "surface.meta.set",
            json!({ "surface_id": surface_id, "key": STALL_NOTICE_META_KEY, "value": value.to_string() }),
        ) {
            Ok(_) => {
                self.notice_recorded.insert(surface_id);
            }
            Err(e) => tracing::warn!("claude stall s{surface_id}: recording the stall notice failed: {e}"),
        }
    }

    /// 대기 중이면 이 대기를 알린 것으로 기록하고 대기 meta 에도 남긴다. 플러그인이 다시 시작돼도 같은 대기를 다시 알리지 않는다.
    fn record_wait_notice<H: HostCall>(&mut self, host: &H, surface_id: u32) {
        if let Some(b) = self.background_wait.get_mut(&surface_id) {
            b.notified = true;
            if let Err(e) = host.call(
                "surface.meta.set",
                json!({ "surface_id": surface_id, "key": crate::hook::BACKGROUND_WAIT_META_KEY, "value": b.meta_value() }),
            ) {
                tracing::warn!("claude stall s{surface_id}: recording the notice in the wait meta failed: {e}");
            }
        }
    }
}

/// 대기가 아닌 정지의 문구 판정. 발화 직전 화면의 오류 줄로 정하고 meta [`STALL_NOTICE_META_KEY`] 에 남긴다.
pub(crate) struct StallWording {
    /// 화면의 첫 오류 줄(앞 [`STALL_HINT_CHARS`] 글자). 있으면 오류 뒤 멈춤이다.
    pub error_line: Option<String>,
}

/// 알림에 넣을 오류 줄의 최대 글자 수.
pub(crate) const STALL_HINT_CHARS: usize = 160;

/// 오류 뒤 멈춤과 오류 없는 멈춤의 meta 값.
pub(crate) const STALL_KIND_ERROR: &str = "stalled";
pub(crate) const STALL_KIND_NO_ERROR: &str = "stalled_no_error";

impl StallWording {
    /// 화면을 읽어 판정한다. 조회에 실패하면 오류 줄이 없는 것으로 본다.
    pub(crate) fn read<H: HostCall>(host: &H, surface_id: u32) -> Self {
        let screen = host
            .call("surface.screen_text", json!({ "surface_id": surface_id }))
            .ok()
            .and_then(|r| r.get("text").and_then(|t| t.as_str()).map(str::to_string));
        Self {
            error_line: screen
                .as_deref()
                .and_then(first_error_line)
                .map(|l| l.chars().take(STALL_HINT_CHARS).collect()),
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        if self.error_line.is_some() {
            STALL_KIND_ERROR
        } else {
            STALL_KIND_NO_ERROR
        }
    }
}

/// 대기 알림에 적는 작업 종류. 종류를 모르면 대기 판정에 쓴 필드 이름이다.
fn wait_label(kinds: &WaitKinds) -> String {
    if kinds.noticed.is_empty() {
        "waiting_on_background_work".to_string()
    } else {
        kinds.noticed.join(", ")
    }
}

/// 정지 알림 문구가 조용한 작업의 이름과 시간을 적도록 기록한다. 대기 시작 시각으로 같은 대기인지 가린다.
/// `labels` 는 알릴 작업이다. 선언으로 보류한 작업은 뺀다.
fn record_quiet_tasks<H: HostCall>(
    host: &H,
    surface_id: u32,
    wait: &BackgroundWait,
    quiet: Duration,
    labels: Vec<String>,
) {
    tracing::info!(
        "claude stall s{surface_id}: background task output quiet for {}s ({}) — notifying",
        quiet.as_secs(),
        labels.join(", ")
    );
    let tasks = wait.watch.as_ref().map(TaskWatch::task_ids);
    let value = json!({
        "since_ms": wait.since_ms,
        "labels": labels,
        "quiet_ms": quiet.as_millis() as u64,
        "tasks": tasks,
        "at_ms": unix_now_ms(),
    });
    if let Err(e) = host.call(
        "surface.meta.set",
        json!({ "surface_id": surface_id, "key": crate::task_watch::BACKGROUND_QUIET_META_KEY, "value": value.to_string() }),
    ) {
        tracing::warn!("claude stall s{surface_id}: recording the quiet background tasks failed: {e}");
    }
}

/// 지금 조용한 작업을 이번 조용한 구간에 이미 알렸는가. 마지막 기록의 작업이 같고 그 뒤로 파일 활동이 없으면 그렇다.
/// 대기와 플러그인 재시작을 넘어 이어지도록 메모리가 아니라 meta 로 판단한다. 파일 나이와 같은 실제 시계로 잰다.
fn already_noticed_quiet_tasks<H: HostCall>(
    host: &H,
    surface_id: u32,
    wait: &BackgroundWait,
    quiet: Duration,
) -> bool {
    let Some(raw) = crate::stop_pairing::meta_value(
        host,
        surface_id,
        crate::task_watch::BACKGROUND_QUIET_META_KEY,
    ) else {
        return false;
    };
    let Ok(last) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let same_tasks = wait
        .watch
        .as_ref()
        .is_some_and(|w| last.get("tasks") == Some(&json!(w.task_ids())));
    let at_ms = last.get("at_ms").and_then(serde_json::Value::as_u64);
    same_tasks
        && at_ms.is_some_and(|at| Duration::from_millis(unix_now_ms().saturating_sub(at)) <= quiet)
}

/// surface meta `claude-background-wait` 에 남은 대기. 없거나 읽을 수 없으면 `None` 이다.
pub(crate) fn saved_wait_from_meta<H: HostCall>(host: &H, surface_id: u32) -> Option<SavedWait> {
    let reply = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": surface_id, "key": crate::hook::BACKGROUND_WAIT_META_KEY }),
        )
        .inspect_err(|e| {
            tracing::warn!(
                "claude stall s{surface_id}: reading the background wait meta failed: {e}"
            )
        })
        .ok()?;
    SavedWait::parse(reply.get("value")?.as_str()?)
}

/// 폴링 중 추적 유지 여부를 확인한다. 조회 오류는 추적을 유지하도록 true로 처리한다.
/// TopLevel은 대상 부재를 오류로 돌려받아도 유지한다. Child는 정상 응답의 status=none이면 제거한다.
pub(crate) fn scan_target_is_alive<H: HostCall>(
    host: &H,
    surface_id: u32,
    target: ScanTarget,
) -> bool {
    match target {
        ScanTarget::TopLevel => host
            .call("surface.locate", json!({ "surface_id": surface_id }))
            .ok()
            .and_then(|r| r.get("exists").and_then(|v| v.as_bool()))
            .unwrap_or(true),
        // surface를 남기는 release도 감지하도록 부모 관계를 확인한다.
        ScanTarget::Child => host
            .call("terminal.parent", json!({ "surface": surface_id }))
            .ok()
            .map(|r| r.get("status").and_then(|v| v.as_str()) != Some("none"))
            .unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_api_error() {
        assert!(detect_claude_error("…\nAPI Error: Connection lost\n"));
    }

    #[test]
    fn detects_content_filter_block() {
        assert!(detect_claude_error(
            "Output blocked by content filtering policy"
        ));
    }

    #[test]
    fn detects_overloaded_and_rate_limit() {
        assert!(detect_claude_error("{\"type\":\"overloaded_error\"}"));
        assert!(detect_claude_error("got rate_limit_error from upstream"));
    }

    #[test]
    fn detects_case_insensitive() {
        assert!(detect_claude_error("api error: foo"));
    }

    #[test]
    fn detects_internal_server_error_and_network() {
        assert!(detect_claude_error("HTTP 500 Internal Server Error\n"));
        assert!(detect_claude_error("encountered network error: ECONNRESET"));
    }

    #[test]
    fn detects_bad_request() {
        assert!(detect_claude_error("400 Bad Request — model not allowed"));
    }

    #[test]
    fn ignores_unrelated_text() {
        assert!(!detect_claude_error("compile error fixed"));
        assert!(!detect_claude_error("no errors here"));
        assert!(!detect_claude_error(""));
    }

    #[test]
    fn enable_disable_tracking() {
        let mut s = ErrorScanner::new();
        assert!(!s.is_enabled(7));
        s.enable(7, ScanTarget::TopLevel);
        assert!(s.is_enabled(7));
        assert_eq!(s.enabled_snapshot(), vec![(7, ScanTarget::TopLevel)]);
        s.disable(7);
        assert!(!s.is_enabled(7));
    }

    #[test]
    fn enable_records_target_kind() {
        let mut s = ErrorScanner::new();
        s.enable(1, ScanTarget::TopLevel);
        s.enable(2, ScanTarget::Child);
        assert_eq!(s.target_of(1), Some(ScanTarget::TopLevel));
        assert_eq!(s.target_of(2), Some(ScanTarget::Child));
        assert_eq!(s.target_of(3), None);
    }

    #[test]
    fn re_enabling_same_surface_is_idempotent_and_keeps_dedupe() {
        // 같은 surface를 재등록해도 중복 항목을 만들지 않는다.
        let mut s = ErrorScanner::new();
        s.enable(5, ScanTarget::Child);
        s.seed_dedupe_for_test(5, "API Error: foo");
        s.enable(5, ScanTarget::Child);
        assert_eq!(s.enabled_snapshot(), vec![(5, ScanTarget::Child)]);
        assert!(s.has_dedupe_state(5));
    }

    #[test]
    fn disable_clears_dedupe_state() {
        // 같은 surface가 disable 후 다시 enable 되면 이전 fire 기록은
        // 사라져야 한다 (새 child 인스턴스로 가정).
        let mut s = ErrorScanner::new();
        s.enable(3, ScanTarget::Child);
        s.last_fired
            .insert(3, "API Error: foo".chars().take(200).collect());
        assert!(s.last_fired.contains_key(&3));
        s.disable(3);
        assert!(!s.last_fired.contains_key(&3));
        assert!(!s.has_dedupe_state(3));
    }

    #[test]
    fn reset_dedupe_clears_only_target_surface() {
        let mut s = ErrorScanner::new();
        s.last_fired.insert(1, "a".into());
        s.last_fired.insert(2, "b".into());
        s.reset_dedupe(1);
        assert!(!s.last_fired.contains_key(&1));
        assert!(s.last_fired.contains_key(&2));
    }

    #[test]
    fn stall_needs_sustained_silence() {
        // 정적이 짧으면 아직 판정하지 않는다 — 재시도 백오프 중일 수 있다.
        assert!(!should_notify_stall(
            STALL_QUIET - Duration::from_secs(1),
            "active",
            false,
            None,
            STALL_QUIET
        ));
        assert!(should_notify_stall(
            STALL_QUIET,
            "active",
            false,
            None,
            STALL_QUIET
        ));
    }

    #[test]
    fn stall_only_when_the_turn_has_not_ended() {
        // 기존 완료 알림 대상인 상태에서는 정지 알림을 추가하지 않는다.
        for state in ["idle", "needs_input", "exited", ""] {
            assert!(
                !should_notify_stall(STALL_QUIET * 2, state, false, None, STALL_QUIET),
                "state={state} 에서는 정지 알림이 나가면 안 된다"
            );
        }
        assert!(should_notify_stall(
            STALL_QUIET * 2,
            "active",
            false,
            None,
            STALL_QUIET
        ));
        // stale도 정지 의심 알림 대상이다.
        assert!(should_notify_stall(
            STALL_QUIET * 2,
            "stale",
            false,
            None,
            STALL_QUIET
        ));
    }

    #[test]
    fn stall_notifies_once_per_silence_and_respects_cooldown() {
        // 같은 정적 구간에서 반복 알림 금지.
        assert!(!should_notify_stall(
            STALL_QUIET * 3,
            "active",
            true,
            None,
            STALL_QUIET
        ));
        // 쿨다운 안이면 새 정적 구간이라도 억제 — 에러 반복 세션의 빈도 상한.
        assert!(!should_notify_stall(
            STALL_QUIET * 3,
            "active",
            false,
            Some(STALL_NOTIFY_COOLDOWN - Duration::from_secs(1)),
            STALL_QUIET
        ));
        assert!(should_notify_stall(
            STALL_QUIET * 3,
            "active",
            false,
            Some(STALL_NOTIFY_COOLDOWN),
            STALL_QUIET
        ));
    }

    #[test]
    fn the_threshold_depends_on_whether_an_error_is_on_screen() {
        let no_output = NO_OUTPUT_NOTICE_DEFAULT;
        assert_eq!(stall_threshold(true, false, no_output), STALL_QUIET);
        assert_eq!(stall_threshold(false, false, no_output), no_output);
        // 백그라운드 대기는 오류 유무와 무관하게 대기 기준을 쓴다.
        assert_eq!(
            stall_threshold(true, true, no_output),
            STALL_QUIET_BACKGROUND_WAIT
        );
        assert_eq!(
            stall_threshold(false, true, no_output),
            STALL_QUIET_BACKGROUND_WAIT
        );
        assert!(
            Duration::from_secs_f64(NO_OUTPUT_NOTICE_MIN_SECS) > STALL_QUIET,
            "보강 증거가 없으면 설정의 최소값으로도 더 길게 본다"
        );
        // 에러 없는 정적은 짧은 문턱으로는 정지가 아니다.
        assert!(!should_notify_stall(
            STALL_QUIET,
            "active",
            false,
            None,
            NO_OUTPUT_NOTICE_DEFAULT
        ));
        assert!(should_notify_stall(
            NO_OUTPUT_NOTICE_DEFAULT,
            "active",
            false,
            None,
            NO_OUTPUT_NOTICE_DEFAULT
        ));
    }

    #[test]
    fn output_fingerprint_tracks_appended_text() {
        // 창 앞부분이 같아도 뒤의 출력이 늘면 해시가 달라져야 한다.
        let head = "x".repeat(300);
        let grown = format!("{head}retrying…");
        assert_eq!(
            head.chars().take(200).collect::<String>(),
            grown.chars().take(200).collect::<String>(),
            "스니펫은 같다(전제)"
        );
        assert_ne!(output_fingerprint(&head), output_fingerprint(&grown));
    }

    #[test]
    fn first_error_line_extracts_matching_line() {
        let text = "building…\n  API Error: connection reset by peer\nnext line\n";
        assert_eq!(
            first_error_line(text),
            Some("API Error: connection reset by peer")
        );
        assert_eq!(first_error_line("all good\n"), None);
    }

    /// 새 출력·자식 상태를 제공하고 훅 요청을 기록하는 stub.
    /// set_text로 지정한 텍스트는 한 번만 읽히며 이후에는 빈 문자열을 반환한다.
    struct ScanHost {
        text: std::cell::RefCell<String>,
        state: std::cell::RefCell<&'static str>,
        fired: std::cell::RefCell<Vec<String>>,
        /// surface meta `claude-background-wait` 값. 없으면 meta 가 없는 것이다.
        wait_meta: Option<String>,
        /// surface.meta.set 으로 받은 (key, value).
        meta_sets: std::cell::RefCell<Vec<(String, String)>>,
        /// 받은 호출의 method 순서.
        calls: std::cell::RefCell<Vec<String>>,
        /// 재시작 전에 남은 `claude-background-quiet` 값. 없으면 이 호스트가 받은 마지막 기록을 돌려준다.
        quiet_meta: std::cell::RefCell<Option<String>>,
        /// 플러그인 설정 값.
        setting: std::cell::RefCell<Option<serde_json::Value>>,
        setting_fails: std::cell::RefCell<bool>,
        /// `surface.screen_text` 의 화면. 처음 출력과 같게 시작한다.
        screen: std::cell::RefCell<String>,
    }

    impl ScanHost {
        fn new(text: &str, state: &'static str) -> Self {
            Self {
                text: std::cell::RefCell::new(text.to_string()),
                state: std::cell::RefCell::new(state),
                fired: std::cell::RefCell::new(Vec::new()),
                wait_meta: None,
                meta_sets: std::cell::RefCell::new(Vec::new()),
                calls: std::cell::RefCell::new(Vec::new()),
                quiet_meta: std::cell::RefCell::new(None),
                setting: std::cell::RefCell::new(None),
                setting_fails: std::cell::RefCell::new(false),
                screen: std::cell::RefCell::new(text.to_string()),
            }
        }
        /// 마지막으로 남긴 정지 알림 meta.
        fn stall_notice(&self) -> Option<serde_json::Value> {
            self.meta_sets
                .borrow()
                .iter()
                .rev()
                .find(|(k, _)| k == STALL_NOTICE_META_KEY)
                .and_then(|(_, v)| serde_json::from_str(v).ok())
        }
        fn count(&self, method: &str) -> usize {
            self.calls.borrow().iter().filter(|m| *m == method).count()
        }
        fn set_text(&self, text: &str) {
            *self.text.borrow_mut() = text.to_string();
        }
        fn events(&self) -> Vec<String> {
            self.fired.borrow().clone()
        }
        fn stalled_count(&self) -> usize {
            self.events().iter().filter(|e| *e == STALLED_EVENT).count()
        }
    }

    impl HostCall for ScanHost {
        fn call(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, tasty_plugin_sdk::PluginError> {
            self.calls.borrow_mut().push(method.to_string());
            match method {
                "surface.read_since_scan_mark" => {
                    let delta = std::mem::take(&mut *self.text.borrow_mut());
                    Ok(json!({ "text": delta }))
                }
                "terminal.state" => Ok(json!({ "state": *self.state.borrow() })),
                "surface.screen_text" => Ok(json!({ "text": *self.screen.borrow() })),
                "settings.get_plugin_setting" if *self.setting_fails.borrow() => {
                    Err(tasty_plugin_sdk::PluginError::HostCall {
                        method: method.to_string(),
                        message: "host down".into(),
                        code: None,
                    })
                }
                "settings.get_plugin_setting" => {
                    assert_eq!(params["storage_key"], NO_OUTPUT_NOTICE_KEY);
                    Ok(json!({ "value": *self.setting.borrow() }))
                }
                "surface.meta.get"
                    if params["key"] == crate::task_watch::BACKGROUND_QUIET_META_KEY =>
                {
                    let last = self
                        .meta_sets
                        .borrow()
                        .iter()
                        .rev()
                        .find(|(k, _)| k == crate::task_watch::BACKGROUND_QUIET_META_KEY)
                        .map(|(_, v)| v.clone());
                    Ok(json!({ "value": self.quiet_meta.borrow().clone().or(last) }))
                }
                "surface.meta.get" if params["key"] == crate::hook::BACKGROUND_WAIT_META_KEY => {
                    Ok(match &self.wait_meta {
                        Some(v) => json!({ "value": v }),
                        None => json!({}),
                    })
                }
                "surface.meta.get"
                    if params["key"] == quiet::QUIET_META_KEY
                        || params["key"] == quiet::QUIET_RELEASE_META_KEY =>
                {
                    Ok(json!({}))
                }
                "surface.fire_hook" => {
                    let event = params["event"].as_str().unwrap_or_default().to_string();
                    self.fired.borrow_mut().push(event);
                    Ok(json!({ "fired": 1 }))
                }
                "surface.meta.set" => {
                    let key = params["key"].as_str().unwrap_or_default();
                    self.fired.borrow_mut().push(format!("meta-set:{key}"));
                    self.meta_sets.borrow_mut().push((
                        key.to_string(),
                        params["value"].as_str().unwrap_or_default().to_string(),
                    ));
                    Ok(json!({}))
                }
                "surface.meta.unset" => {
                    let key = params["key"].as_str().unwrap_or_default();
                    self.fired.borrow_mut().push(format!("meta-unset:{key}"));
                    Ok(json!({}))
                }
                other => panic!("unexpected host call: {other}"),
            }
        }
    }

    const ERR: &str = "⎿ API Error: Connection error\n";

    #[test]
    fn transient_error_that_keeps_producing_output_never_stalls() {
        // 반복해서 바뀌는 출력은 조용한 구간으로 판정하지 않는다.
        let host = ScanHost::new(ERR, "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        for i in 1..=6u32 {
            host.set_text(&format!("{ERR}Retrying in {i}s… (attempt {i}/10)"));
            s.scan_one_at(&host, 1, t0 + Duration::from_secs(10 * u64::from(i)));
        }
        assert_eq!(host.stalled_count(), 0, "재시도 중에는 알림이 없어야 한다");
        // 오류 감지 이벤트와 부모에게 전달할 정지 알림은 별개다.
        assert!(
            host.events().iter().all(|e| e == "claude-error"),
            "정지 이벤트가 섞이면 안 된다: {:?}",
            host.events()
        );
    }

    #[test]
    fn error_followed_by_silence_fires_stalled_once() {
        let host = ScanHost::new(ERR, "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        assert_eq!(host.stalled_count(), 0, "에러 직후엔 아직 정지가 아니다");

        // 오류 중복 알림을 생략해도 정지 판정은 계속한다.
        s.scan_one_at(&host, 1, t0 + STALL_QUIET);
        assert_eq!(
            host.stalled_count(),
            1,
            "출력 변화가 없는 시간이 기준에 도달하면 한 번 알린다"
        );
        let notice = host.stall_notice().expect("판정을 발화 전에 남긴다");
        assert_eq!(notice["kind"], STALL_KIND_ERROR);
        assert_eq!(notice["hint"], "⎿ API Error: Connection error");

        // 계속 조용해도 같은 정적 구간에서는 다시 알리지 않는다.
        s.scan_one_at(&host, 1, t0 + STALL_QUIET * 3);
        assert_eq!(
            host.stalled_count(),
            1,
            "같은 조용한 구간에서는 한 번만 알린다"
        );
    }

    #[test]
    fn silence_after_turn_ended_does_not_fire_stalled() {
        // idle은 정지 의심 알림에서 제외한다.
        let host = ScanHost::new(ERR, "idle");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + STALL_QUIET * 2);
        assert_eq!(host.stalled_count(), 0);
    }

    #[test]
    fn new_turn_resets_stall_watch() {
        let host = ScanHost::new(ERR, "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        // 새 턴 신호(prompt-submit 등)로 dedupe + 정지 관측이 초기화된다.
        s.reset_dedupe(1);
        assert!(!s.has_dedupe_state(1));
        // 이전 턴의 정적이 이월되지 않으므로 바로 다음 tick 에 정지로 판정되지 않는다.
        let t1 = t0 + STALL_QUIET;
        s.scan_one_at(&host, 1, t1);
        assert_eq!(host.stalled_count(), 0, "정적 구간은 새 턴부터 다시 잰다");
        // 앞 턴의 오류는 이번 턴의 조용한 구간에 쓰지 않는다.
        s.scan_one_at(&host, 1, t1 + STALL_QUIET);
        assert_eq!(
            host.stalled_count(),
            0,
            "앞 턴의 오류로 30초에 알리지 않는다"
        );
        s.scan_one_at(&host, 1, t1 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 1);
    }

    /// 창 앞쪽의 오래된 오류 뒤에 출력이 이어졌으면 오류 직후가 아니다. 오류 이벤트의 중복 생략은 그대로다.
    #[test]
    fn an_old_error_followed_by_more_output_uses_the_normal_threshold() {
        let host = ScanHost::new(ERR, "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        host.set_text(&"normal output\n".repeat(RECENT_ERROR_BYTES / 14 + 1));
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(1));
        let t1 = t0 + Duration::from_secs(1);
        s.scan_one_at(&host, 1, t1 + STALL_QUIET);
        assert_eq!(host.stalled_count(), 0, "30초에 알리지 않는다");
        s.scan_one_at(
            &host,
            1,
            t1 + NO_OUTPUT_NOTICE_DEFAULT - Duration::from_secs(1),
        );
        assert_eq!(host.stalled_count(), 0, "599초에는 알리지 않는다");
        s.scan_one_at(&host, 1, t1 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 1, "600초에 한 번 알린다");
        assert!(
            host.events().iter().any(|e| e == "claude-error"),
            "오류 이벤트는 창 전체로 계속 찾는다"
        );
    }

    /// 오류 뒤의 짧은 출력(상태줄 등)은 오류 직후로 본다.
    #[test]
    fn an_error_followed_by_a_short_redraw_still_uses_the_error_threshold() {
        let host = ScanHost::new(ERR, "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        host.set_text("\u{276f} \n");
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(1));
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(1) + STALL_QUIET);
        assert_eq!(host.stalled_count(), 1);
    }

    fn shell_kinds() -> WaitKinds {
        WaitKinds {
            all: vec!["shell".into()],
            noticed: vec!["shell".into()],
        }
    }

    /// Stop 이 Monitor 하나만 기다린다고 보고한 대기의 종류. Monitor 도 `shell` 로 온다.
    fn monitor_kinds() -> WaitKinds {
        WaitKinds {
            all: vec!["shell".into()],
            noticed: Vec::new(),
        }
    }

    /// Stop 이 Bash 하나와 Monitor 하나를 기다린다고 보고한 대기의 종류.
    fn mixed_kinds() -> WaitKinds {
        WaitKinds {
            all: vec!["shell".into(), "shell".into()],
            noticed: vec!["shell".into()],
        }
    }

    /// Monitor 만 기다리면 어떤 기준에도 알리지 않는다. 턴이 진행 중이면 일반 기준으로 돌아간다.
    #[test]
    fn a_wait_on_monitors_only_is_never_noticed() {
        let host = ScanHost::new("\u{276f} \n", "active");
        let mut s = scanner_with_notice(60);
        s.record_background_wait(1, 1_000, monitor_kinds(), None);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT * 100);
        assert_eq!(host.stalled_count(), 0);
        assert!(s.is_waiting_on_background_work(1), "대기 자체는 그대로다");

        // 새 턴: Monitor 가 떠 있어도 대기가 아니므로 조용한 턴은 알린다.
        s.clear_background_wait(1);
        s.reset_dedupe(1);
        let t1 = t0 + STALL_QUIET_BACKGROUND_WAIT * 101;
        s.scan_one_at(&host, 1, t1);
        s.scan_one_at(&host, 1, t1 + Duration::from_secs(60));
        assert_eq!(host.stalled_count(), 1);
    }

    /// 재시작 뒤 대기 meta 로 되살린 Monitor 대기도 알리지 않는다.
    #[test]
    fn a_restored_wait_on_monitors_only_is_not_noticed() {
        let mut s = ErrorScanner::new();
        let meta = s.record_background_wait(1, 1_000, monitor_kinds(), None);
        let mut host = ScanHost::new("\u{276f} \n", "active");
        host.wait_meta = Some(meta);
        let mut restarted = scanner_with_notice(60);
        let t0 = Instant::now();
        restarted.scan_one_at(&host, 1, t0);
        restarted.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT * 10);
        assert_eq!(host.stalled_count(), 0);
        assert!(restarted.is_waiting_on_background_work(1));
    }

    /// `notice_types` 가 없는 옛 대기 meta 는 지금처럼 알린다. 종류를 모르는 대기도 그렇다.
    #[test]
    fn a_wait_without_monitor_information_is_noticed_as_before() {
        for meta in [
            r#"{"since_ms":1000,"tasks":1,"types":["shell"]}"#,
            r#"{"since_ms":1000,"tasks":0,"types":[]}"#,
        ] {
            let mut host = ScanHost::new("\u{276f} \n", "active");
            host.wait_meta = Some(meta.to_string());
            let mut s = scanner_with_notice(60);
            let t0 = Instant::now();
            s.scan_one_at(&host, 1, t0);
            s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT);
            assert_eq!(host.stalled_count(), 1, "{meta}");
        }
    }

    /// 조용한 Bash 와 이벤트를 내는 Monitor: Monitor 이벤트가 새 턴을 열어 대기가 다시 시작돼도 같은 조용한 구간은
    /// 한 번만 알린다.
    #[test]
    fn a_quiet_task_is_noticed_once_across_waits_restarted_by_monitor_events() {
        let host = ScanHost::new("\u{276f} \n", "active");
        let mut s = scanner_with_notice(60);
        // 파일 나이는 실제 시계로 잰다. 시험의 쿨다운 경과보다 오래 조용하게 둔다.
        let (dir, watch, files) = quiet_task_files(STALL_NOTIFY_COOLDOWN + Duration::from_secs(60));
        s.record_background_wait(1, 1_000, mixed_kinds(), Some(watch.clone()));
        s.set_file_activity_for_test(1, watch.clone(), files);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        assert_eq!(host.stalled_count(), 1);

        // Monitor 이벤트: 새 턴 뒤 대기 Stop. 쿨다운이 지났지만 Bash 파일은 그대로다.
        s.clear_background_wait(1);
        s.reset_dedupe(1);
        s.record_background_wait(1, 2_000, mixed_kinds(), Some(watch.clone()));
        let refreshed = FileActivity::locate(&watch, dir.path()).unwrap();
        s.set_file_activity_for_test(1, watch.clone(), refreshed);
        s.scan_one_at(&host, 1, t0 + STALL_NOTIFY_COOLDOWN);
        assert_eq!(host.stalled_count(), 1, "같은 조용한 구간");
        let gets = host.count("surface.meta.get");
        s.scan_one_at(&host, 1, t0 + STALL_NOTIFY_COOLDOWN * 2);
        assert_eq!(
            host.count("surface.meta.get"),
            gets,
            "tick 마다 다시 묻지 않는다"
        );

        // 플러그인 재시작: 메모리 없이 meta 만 남았다.
        let saved = host
            .meta_sets
            .borrow()
            .iter()
            .rev()
            .find(|(k, _)| k == crate::task_watch::BACKGROUND_QUIET_META_KEY)
            .map(|(_, v)| v.clone());
        let again = |quiet_meta: Option<String>| {
            let host = ScanHost::new("\u{276f} \n", "active");
            *host.quiet_meta.borrow_mut() = quiet_meta;
            let mut restarted = scanner_with_notice(60);
            restarted.record_background_wait(1, 3_000, mixed_kinds(), Some(watch.clone()));
            let files = FileActivity::locate(&watch, dir.path()).unwrap();
            restarted.set_file_activity_for_test(1, watch.clone(), files);
            restarted.scan_one_at(&host, 1, Instant::now());
            host.stalled_count()
        };
        assert_eq!(again(saved.clone()), 0, "재시작 뒤에도 같은 조용한 구간");

        // 대조군: 다른 작업이나, 그 뒤로 파일 활동이 있었던 기록이면 새 사건으로 알린다.
        assert_eq!(again(None), 1);
        let edited = |key: &str, value: serde_json::Value| {
            let mut v: serde_json::Value = serde_json::from_str(saved.as_deref().unwrap()).unwrap();
            v[key] = value;
            Some(v.to_string())
        };
        assert_eq!(again(edited("tasks", json!(["other"]))), 1);
        let long_ago = super::unix_now_ms() - 1_000_000_000;
        assert_eq!(again(edited("at_ms", json!(long_ago))), 1);
    }

    /// Monitor 없는 Bash: 부모의 지시 등으로 새 턴이 지나 다시 대기하면, 파일이 그대로여도 새 대기마다 알려 턴을 깨운다.
    #[test]
    fn a_quiet_task_without_a_monitor_is_noticed_again_for_each_wait() {
        let host = ScanHost::new("\u{276f} \n", "active");
        let mut s = scanner_with_notice(60);
        let (dir, watch, files) = quiet_task_files(STALL_NOTIFY_COOLDOWN + Duration::from_secs(60));
        s.record_background_wait(1, 1_000, shell_kinds(), Some(watch.clone()));
        s.set_file_activity_for_test(1, watch.clone(), files);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        assert_eq!(host.stalled_count(), 1);
        // 같은 대기 안에서는 한 번이다.
        s.scan_one_at(&host, 1, t0 + STALL_NOTIFY_COOLDOWN);
        assert_eq!(host.stalled_count(), 1);

        s.clear_background_wait(1);
        s.reset_dedupe(1);
        s.record_background_wait(1, 2_000, shell_kinds(), Some(watch.clone()));
        let refreshed = FileActivity::locate(&watch, dir.path()).unwrap();
        s.set_file_activity_for_test(1, watch.clone(), refreshed);
        s.scan_one_at(&host, 1, t0 + STALL_NOTIFY_COOLDOWN);
        assert_eq!(host.stalled_count(), 2, "새 대기");
    }

    fn scanner_with_notice(secs: u64) -> ErrorScanner {
        let mut s = ErrorScanner::new();
        s.no_output_notice.threshold = Duration::from_secs(secs);
        s
    }

    /// 설정 값 해석. 범위 밖은 범위 안으로, 숫자가 아니면 기본값이다.
    #[test]
    fn the_no_output_notice_setting_is_clamped_and_falls_back() {
        let secs = |v: serde_json::Value| parse_no_output_notice(Some(&v)).as_secs();
        assert_eq!(secs(json!(5)), 60);
        assert_eq!(secs(json!(60)), 60);
        assert_eq!(secs(json!(1800.0)), 1800);
        assert_eq!(secs(json!(100_000)), 86_400);
        assert_eq!(secs(json!("NaN")), 600);
        assert_eq!(secs(json!(null)), 600);
        // 값을 JSON 으로 옮기면 NaN 은 null 이 된다. 그 앞 단계의 값도 같은 결과여야 한다.
        assert_eq!(secs(json!(f64::NAN)), 600);
        assert_eq!(parse_no_output_notice(None).as_secs(), 600);
    }

    /// 기본값 600초: 599초에는 알리지 않고 600초에 한 번 알린다.
    #[test]
    fn by_default_a_quiet_child_is_noticed_at_ten_minutes() {
        let host = ScanHost::new("thinking…\n", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(599));
        assert_eq!(host.stalled_count(), 0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(600));
        assert_eq!(host.stalled_count(), 1);
        assert_eq!(host.count("settings.get_plugin_setting"), 0);
    }

    /// 설정값이 일반 기준이 된다.
    #[test]
    fn a_configured_threshold_replaces_the_default() {
        let host = ScanHost::new("thinking…\n", "active");
        let mut s = scanner_with_notice(60);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(59));
        assert_eq!(host.stalled_count(), 0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(60));
        assert_eq!(host.stalled_count(), 1);
    }

    /// 출력 파일을 못 찾은 대기는 600초와 설정값 중 큰 값을 쓴다.
    #[test]
    fn the_wait_threshold_is_the_larger_of_ten_minutes_and_the_setting() {
        for (setting, due) in [(60, 600), (1800, 1800)] {
            let host = ScanHost::new("\u{276f} \n", "active");
            let mut s = scanner_with_notice(setting);
            s.mark_background_wait(1, 1_000);
            let t0 = Instant::now();
            s.scan_one_at(&host, 1, t0);
            s.scan_one_at(&host, 1, t0 + Duration::from_secs(due - 1));
            assert_eq!(host.stalled_count(), 0, "setting={setting}");
            s.scan_one_at(&host, 1, t0 + Duration::from_secs(due));
            assert_eq!(host.stalled_count(), 1, "setting={setting}");
        }
    }

    /// 출력 파일 기준은 설정값을 그대로 쓴다.
    #[test]
    fn quiet_task_output_files_use_the_configured_threshold() {
        for (quiet_secs, expected) in [(50, 0), (65, 1)] {
            let host = ScanHost::new("\u{276f} \n", "active");
            let mut s = scanner_with_notice(60);
            s.mark_background_wait(1, 1_000);
            let (_dir, watch, files) = quiet_task_files(Duration::from_secs(quiet_secs));
            s.set_file_activity_for_test(1, watch, files);
            s.scan_one_at(&host, 1, Instant::now());
            assert_eq!(host.stalled_count(), expected, "quiet={quiet_secs}");
        }
    }

    /// 설정은 재시작 없이 읽는 간격 안에 반영되고, 간격 안에서는 다시 묻지 않는다.
    #[test]
    fn the_setting_is_reread_at_its_interval_and_a_failed_read_keeps_the_value() {
        let host = ScanHost::new("", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        *host.setting.borrow_mut() = Some(json!(120));
        s.refresh_no_output_notice(&host, t0);
        assert_eq!(s.no_output_notice.threshold, Duration::from_secs(120));

        *host.setting.borrow_mut() = Some(json!(300));
        s.refresh_no_output_notice(&host, t0 + NO_OUTPUT_NOTICE_REREAD - Duration::from_secs(1));
        assert_eq!(host.count("settings.get_plugin_setting"), 1);
        assert_eq!(s.no_output_notice.threshold, Duration::from_secs(120));

        s.refresh_no_output_notice(&host, t0 + NO_OUTPUT_NOTICE_REREAD);
        assert_eq!(host.count("settings.get_plugin_setting"), 2);
        assert_eq!(s.no_output_notice.threshold, Duration::from_secs(300));

        // 설정을 지우면 기본값으로 돌아간다.
        *host.setting.borrow_mut() = None;
        s.refresh_no_output_notice(&host, t0 + NO_OUTPUT_NOTICE_REREAD * 2);
        assert_eq!(s.no_output_notice.threshold, NO_OUTPUT_NOTICE_DEFAULT);

        // 조회 실패는 마지막 값을 유지한다.
        s.no_output_notice.threshold = Duration::from_secs(90);
        *host.setting_fails.borrow_mut() = true;
        s.refresh_no_output_notice(&host, t0 + NO_OUTPUT_NOTICE_REREAD * 3);
        assert_eq!(s.no_output_notice.threshold, Duration::from_secs(90));
    }

    #[test]
    fn clean_output_never_fires_an_error_event() {
        // 오류 패턴이 없으면 claude-error는 보내지 않는다. 정지 의심 알림은 별개다.
        let host = ScanHost::new("running tests…\nall good\n", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        // 문턱 이전까지는 아무것도 안 나간다.
        s.scan_one_at(
            &host,
            1,
            t0 + NO_OUTPUT_NOTICE_DEFAULT - Duration::from_secs(1),
        );
        assert!(
            host.events().is_empty(),
            "이벤트가 없어야 한다: {:?}",
            host.events()
        );
        // 문턱을 넘겨도 에러 이벤트는 없다.
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT * 5);
        assert!(
            !host.events().iter().any(|e| e == "claude-error"),
            "에러가 없는데 claude-error 가 나갔다: {:?}",
            host.events()
        );
    }

    /// 오류 없이 오래 조용한 경우에도 정지 의심 알림을 보낸다.
    #[test]
    fn silence_without_error_fires_stall() {
        let host = ScanHost::new("waiting for your approval…\n", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        assert_eq!(host.stalled_count(), 0);
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 1, "에러가 없어도 정적이 길면 알린다");
        let notice = host.stall_notice().expect("판정을 발화 전에 남긴다");
        assert_eq!(notice["kind"], STALL_KIND_NO_ERROR);
        assert!(notice.get("hint").is_none(), "{notice}");
        assert!(
            host.events()
                .iter()
                .filter(|e| !e.starts_with("meta-set:"))
                .all(|e| e == STALLED_EVENT),
            "에러 이벤트가 섞이면 안 된다: {:?}",
            host.events()
        );
    }

    /// 호스트가 stale로 판정한 경우도 알린다.
    #[test]
    fn stale_state_fires_stall() {
        let host = ScanHost::new("…\n", "stale");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 1);
    }

    /// 기존 완료 알림 대상 상태에서는 정지 의심 알림을 보내지 않는다.
    #[test]
    fn idle_and_needs_input_still_suppressed() {
        for state in ["idle", "needs_input", "exited"] {
            let host = ScanHost::new("…\n", state);
            let mut s = ErrorScanner::new();
            let t0 = Instant::now();
            s.scan_one_at(&host, 1, t0);
            s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT * 2);
            assert_eq!(host.stalled_count(), 0, "state={state}");
        }
    }

    /// 쉬는 Claude 는 하단 상태줄(업데이트 확인 등)을 가끔 다시 그린다. 출력이 바뀔 때마다 새 조용한
    /// 구간이 시작되지만, idle 로 남아 있는 한 구간이 몇 번 반복돼도 정지 알림은 없다.
    /// 같은 출력 변화가 active 대상에는 구간마다 알림을 만든다(대조군).
    #[test]
    fn status_line_redraws_of_an_idle_target_never_fire_a_stall() {
        for (state, expected) in [("idle", 0), ("active", 4)] {
            let host = ScanHost::new("TASK COMPLETE\n", state);
            let mut s = ErrorScanner::new();
            let mut t = Instant::now();
            s.scan_one_at(&host, 1, t);
            for redraw in [
                "\rChecking for updates",
                "\rRestart to update",
                "\r/restart to apply",
            ] {
                t += STALL_NOTIFY_COOLDOWN + NO_OUTPUT_NOTICE_DEFAULT;
                s.scan_one_at(&host, 1, t);
                host.set_text(redraw);
                s.scan_one_at(&host, 1, t);
            }
            t += NO_OUTPUT_NOTICE_DEFAULT;
            s.scan_one_at(&host, 1, t);
            assert_eq!(host.stalled_count(), expected, "state={state}");
        }
    }

    /// 800ms tick 을 `n` 번 돌린다.
    fn ticks(s: &mut ErrorScanner, host: &ScanHost, from: Instant, n: u32) -> Instant {
        let mut t = from;
        for _ in 0..n {
            t += Duration::from_millis(800);
            s.scan_one_at(host, 1, t);
        }
        t
    }

    /// idle 로 조용한 대상은 조용한 구간마다 대기 기록·상태를 한 번만 묻는다.
    #[test]
    fn a_quiet_idle_target_is_checked_once_per_quiet_stretch() {
        for state in ["idle", "needs_input", "exited"] {
            let host = ScanHost::new("…\n", state);
            let mut s = ErrorScanner::new();
            let t0 = Instant::now();
            s.scan_one_at(&host, 1, t0);
            ticks(&mut s, &host, t0 + NO_OUTPUT_NOTICE_DEFAULT, 100);
            assert_eq!(host.count("terminal.state"), 1, "state={state}");
            assert_eq!(host.count("surface.meta.get"), 1, "state={state}");
            assert_eq!(host.stalled_count(), 0, "state={state}");
        }
    }

    /// 상태를 읽지 못했으면 확인한 것으로 치지 않고 다음 tick 에 다시 묻는다.
    #[test]
    fn a_failed_state_read_is_retried_next_tick() {
        let host = ScanHost::new("…\n", "");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        ticks(&mut s, &host, t0 + NO_OUTPUT_NOTICE_DEFAULT, 5);
        assert_eq!(host.count("terminal.state"), 5);
    }

    /// hook 이 상태를 active 로 되돌리면 다음 tick 에 다시 물어 이전과 같은 시점에 알린다.
    #[test]
    fn a_hook_rearms_the_state_check_without_delaying_the_stall() {
        let host = ScanHost::new("…\n", "idle");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        let t = ticks(&mut s, &host, t0 + NO_OUTPUT_NOTICE_DEFAULT, 10);
        assert_eq!(host.count("terminal.state"), 1);
        *host.state.borrow_mut() = "active";
        s.note_hook(1);
        s.scan_one_at(&host, 1, t + Duration::from_millis(800));
        assert_eq!(host.count("terminal.state"), 2);
        assert_eq!(host.stalled_count(), 1, "hook 다음 tick 에 알린다");
    }

    /// 출력이 바뀌면 새 조용한 구간이라 기준 시간이 지난 뒤 다시 묻는다.
    #[test]
    fn new_output_starts_a_new_quiet_stretch_for_the_state_check() {
        let host = ScanHost::new("…\n", "idle");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        let t = ticks(&mut s, &host, t0 + NO_OUTPUT_NOTICE_DEFAULT, 10);
        assert_eq!(host.count("terminal.state"), 1);
        host.set_text("more\n");
        s.scan_one_at(&host, 1, t);
        ticks(&mut s, &host, t, 10);
        assert_eq!(
            host.count("terminal.state"),
            1,
            "새 구간은 기준 시간 전에는 묻지 않는다"
        );
        ticks(&mut s, &host, t + NO_OUTPUT_NOTICE_DEFAULT, 10);
        assert_eq!(host.count("terminal.state"), 2);
    }

    /// 출력이 재개되면 dedupe 가 풀리고 다음 정적 구간은 새 사건으로 센다.
    #[test]
    fn output_resumption_rearms_and_does_not_double_fire() {
        let host = ScanHost::new("thinking…\n", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT * 2);
        assert_eq!(host.stalled_count(), 1, "같은 정적 구간에서는 1 회");

        // 출력 재개 → 다시 정적. 쿨다운을 넘긴 시점이라 두 번째 사건이 나간다.
        let t1 = t0 + STALL_NOTIFY_COOLDOWN + NO_OUTPUT_NOTICE_DEFAULT;
        host.set_text("thinking…\nstill here\n");
        s.scan_one_at(&host, 1, t1);
        assert_eq!(host.stalled_count(), 1, "재개 직후는 정적이 아니다");
        s.scan_one_at(&host, 1, t1 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 2);
    }

    /// 백그라운드 작업을 기다리는 자식은 일반 기준에 정지 알림을 받지 않는다.
    #[test]
    fn a_surface_waiting_on_background_work_gets_no_notice_at_the_normal_threshold() {
        let host = ScanHost::new("❯ \n⏵⏵ 1 shell\n", "active");
        let mut s = scanner_with_notice(60);
        s.mark_background_wait(1, 1_000);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(61));
        assert_eq!(host.stalled_count(), 0);
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT);
        assert_eq!(host.stalled_count(), 1, "대기 기준에 도달하면 한 번 알린다");
    }

    /// 플러그인이 다시 시작돼 메모리 기록이 없어도 대기 meta 가 남아 있으면 대기 기준을 쓴다.
    /// 알림 문구도 meta 를 읽으므로 기준과 문구가 같은 대기를 가리킨다.
    #[test]
    fn a_wait_left_in_the_surface_meta_is_restored_before_the_normal_notice() {
        let mut host = ScanHost::new("❯ \n⏵⏵ 1 shell\n", "active");
        host.wait_meta = Some(r#"{"since_ms":1000,"tasks":1,"types":["shell"]}"#.to_string());
        let mut s = scanner_with_notice(60);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(61));
        assert_eq!(host.stalled_count(), 0, "일반 기준에서는 알리지 않는다");
        assert!(s.is_waiting_on_background_work(1));
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT);
        assert_eq!(host.stalled_count(), 1, "대기 기준에 도달하면 한 번 알린다");
    }

    /// 화면 기준 정지 알림은 meta 에 시각을 남긴다. 플러그인이 다시 시작돼 그 시각을 되살리면, 새 출력이 없는 한
    /// 같은 조용한 구간을 다시 알리지 않는다. 출력이 다시 바뀌면 meta 를 지우고 다음 조용한 구간은 평소처럼 알린다.
    #[test]
    fn a_screen_notice_before_a_restart_is_not_repeated_while_the_output_stays() {
        let host = ScanHost::new("thinking…\n", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 1);
        let saved = host
            .meta_sets
            .borrow()
            .iter()
            .find(|(k, _)| k == STALL_NOTICE_META_KEY)
            .map(|(_, v)| v.clone())
            .expect("알림 시각을 남긴다");
        let at_ms = serde_json::from_str::<serde_json::Value>(&saved).unwrap()["at_ms"]
            .as_u64()
            .unwrap();

        // 재시작: 메모리 기록 없이 meta 의 시각만 되살린다. 그사이 새 출력은 없다.
        let restarted_host = ScanHost::new("", "active");
        let mut restarted = ErrorScanner::new();
        restarted.restore_stall_notice(1, at_ms, t0);
        for k in 0..6 {
            restarted.scan_one_at(&restarted_host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT * k);
        }
        assert_eq!(restarted_host.stalled_count(), 0, "같은 조용한 구간");
        assert!(
            !restarted_host
                .events()
                .contains(&format!("meta-unset:{STALL_NOTICE_META_KEY}"))
        );

        // 출력이 바뀌면 그 구간은 끝났다. meta 를 지우고 다음 구간은 알린다(알림 간격은 지났다).
        let t1 = t0 + NO_OUTPUT_NOTICE_DEFAULT * 6;
        restarted_host.set_text("next step\n");
        restarted.scan_one_at(&restarted_host, 1, t1);
        assert!(
            restarted_host
                .events()
                .contains(&format!("meta-unset:{STALL_NOTICE_META_KEY}"))
        );
        restarted.scan_one_at(&restarted_host, 1, t1 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(restarted_host.stalled_count(), 1);
    }

    /// 되살린 알림 뒤 첫 스캔에 새 출력이 있으면 재시작 사이에 구간이 끝난 것이다. 그 뒤 조용해지면 알림 간격을
    /// 지킨 채 다시 알린다.
    #[test]
    fn a_restored_notice_is_dropped_when_output_arrived_during_the_restart() {
        let host = ScanHost::new("resumed output\n", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        let long_ago = super::unix_now_ms() - STALL_NOTIFY_COOLDOWN.as_millis() as u64;
        s.restore_stall_notice(1, long_ago, t0);
        assert!(s.stall_notified_for_test(1));
        s.scan_one_at(&host, 1, t0);
        assert!(!s.stall_notified_for_test(1));
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 1);
    }

    /// 대기 알림은 대기 meta 에도 남는다. 플러그인이 다시 시작돼 그 meta 로 대기를 되살리면 같은 대기를 다시 알리지 않는다.
    #[test]
    fn a_wait_notified_before_a_restart_is_not_notified_again() {
        let host = ScanHost::new("❯ \n⏵⏵ 1 shell\n", "active");
        let mut s = ErrorScanner::new();
        let first = s.record_background_wait(1, 1_000, shell_kinds(), None);
        assert!(!SavedWait::parse(&first).unwrap().notified);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT);
        assert_eq!(host.stalled_count(), 1);
        let saved = host
            .meta_sets
            .borrow()
            .iter()
            .rev()
            .find(|(k, _)| k == crate::hook::BACKGROUND_WAIT_META_KEY)
            .map(|(_, v)| v.clone())
            .expect("알림 뒤 대기 meta 를 다시 쓴다");
        let parsed = SavedWait::parse(&saved).unwrap();
        assert!(parsed.notified, "{saved}");
        assert_eq!(
            (parsed.since_ms, parsed.kinds.noticed),
            (1_000, vec!["shell".to_string()])
        );

        let mut restarted_host = ScanHost::new("❯ \n⏵⏵ 1 shell\n", "active");
        restarted_host.wait_meta = Some(saved);
        let mut restarted = ErrorScanner::new();
        restarted.scan_one_at(&restarted_host, 1, t0);
        restarted.scan_one_at(&restarted_host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT * 2);
        assert_eq!(restarted_host.stalled_count(), 0, "이미 알린 대기");
    }

    /// 대기 meta 는 출력 파일 단서를 싣고, 되살린 대기는 그 단서로 파일을 다시 찾는다.
    #[test]
    fn a_restored_wait_keeps_the_task_watch() {
        let watch = TaskWatch {
            session_id: "s-1".into(),
            cwd: "/w".into(),
            tasks: vec![crate::task_watch::WatchedTask {
                id: "bq1".into(),
                label: "sleep 300".into(),
            }],
        };
        let mut s = ErrorScanner::new();
        let meta = s.record_background_wait(1, 1_000, shell_kinds(), Some(watch.clone()));
        let mut restarted = ErrorScanner::new();
        restarted.restore_background_wait(1, SavedWait::parse(&meta).unwrap());
        assert_eq!(restarted.background_wait[&1].watch, Some(watch));
        // 그사이 대기 Stop 이 먼저 기록됐으면 되살린 값이 덮어쓰지 않는다.
        restarted.restore_background_wait(
            1,
            SavedWait::parse(r#"{"since_ms":5,"notified":true}"#).unwrap(),
        );
        assert_eq!(restarted.background_wait[&1].since_ms, 1_000);
        assert!(!restarted.background_wait[&1].notified);
    }

    fn quiet_task_files(age: Duration) -> (tempfile::TempDir, TaskWatch, FileActivity) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bq1.output");
        std::fs::write(&path, "").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() - age)
            .unwrap();
        let watch = TaskWatch {
            session_id: "s-quiet".into(),
            cwd: "/w".into(),
            tasks: vec![crate::task_watch::WatchedTask {
                id: "bq1".into(),
                label: "sleep 300".into(),
            }],
        };
        let files = FileActivity::locate(&watch, dir.path()).unwrap();
        (dir, watch, files)
    }

    /// 기다리는 작업의 출력 파일이 일반 기준 동안 그대로면 화면 출력과 관계없이 한 번 알린다.
    /// 알림 문구가 쓸 조용한 작업 기록을 이벤트보다 먼저 남긴다.
    #[test]
    fn quiet_task_output_files_are_noticed_at_the_normal_threshold() {
        let host = ScanHost::new("❯ \n⏵⏵ 1 shell\n", "active");
        let mut s = ErrorScanner::new();
        s.mark_background_wait(1, 1_000);
        let (_dir, watch, files) =
            quiet_task_files(NO_OUTPUT_NOTICE_DEFAULT + Duration::from_secs(5));
        s.set_file_activity_for_test(1, watch, files);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        assert_eq!(
            host.events(),
            [
                format!("meta-set:{}", crate::task_watch::BACKGROUND_QUIET_META_KEY),
                format!("meta-set:{STALL_NOTICE_META_KEY}"),
                STALLED_EVENT.to_string(),
                format!("meta-set:{}", crate::hook::BACKGROUND_WAIT_META_KEY),
            ]
        );
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT * 2);
        assert_eq!(host.stalled_count(), 1, "대기 한 번에 한 번만 알린다");
    }

    /// 출력 파일이 계속 바뀌면 화면이 대기 기준(10분)보다 오래 조용해도 알리지 않는다.
    #[test]
    fn growing_task_output_files_keep_a_quiet_screen_from_being_noticed() {
        let host = ScanHost::new("❯ \n⏵⏵ 1 shell\n", "active");
        let mut s = ErrorScanner::new();
        s.mark_background_wait(1, 1_000);
        let (dir, watch, files) = quiet_task_files(Duration::from_secs(1));
        s.set_file_activity_for_test(1, watch, files);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        for i in 1..=3u32 {
            std::fs::write(dir.path().join("bq1.output"), "tick\n".repeat(i as usize)).unwrap();
            s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT * i);
        }
        assert_eq!(host.stalled_count(), 0);
    }

    /// transcript 가 없는 서브에이전트 링크뿐이면 일반 기준에 알리지 않고 대기 기준(10분)으로 돌아간다.
    #[cfg(unix)]
    #[test]
    fn a_wait_with_only_a_dangling_link_uses_the_wait_threshold() {
        let host = ScanHost::new("❯ \n⏵⏵ 1 local agent\n", "active");
        let mut s = scanner_with_notice(60);
        s.mark_background_wait(1, 1_000);
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("agent-a1.jsonl"),
            dir.path().join("a1.output"),
        )
        .unwrap();
        let watch = TaskWatch {
            session_id: "s-dangling".into(),
            cwd: "/w".into(),
            tasks: vec![crate::task_watch::WatchedTask {
                id: "a1".into(),
                label: "subagent".into(),
            }],
        };
        let files = FileActivity::locate(&watch, dir.path()).unwrap();
        s.set_file_activity_for_test(1, watch, files);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(61));
        assert_eq!(host.stalled_count(), 0);
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT);
        assert_eq!(host.stalled_count(), 1);
        let quiet = format!("meta-set:{}", crate::task_watch::BACKGROUND_QUIET_META_KEY);
        assert!(!host.events().contains(&quiet));
    }

    /// 출력 파일을 찾지 못하면 대기 기준(10분)으로 돌아간다.
    #[test]
    fn without_task_output_files_the_wait_threshold_stays() {
        let host = ScanHost::new("❯ \n⏵⏵ 1 shell\n", "active");
        let mut s = scanner_with_notice(60);
        s.mark_background_wait(1, 1_000);
        let (_dir, mut watch, _files) = quiet_task_files(Duration::from_secs(1));
        watch.session_id = "no-such-session-7f3c1e".into();
        s.set_task_watch(1, Some(watch));
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + Duration::from_secs(61));
        assert_eq!(host.stalled_count(), 0);
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT);
        assert_eq!(host.stalled_count(), 1);
        let quiet = format!("meta-set:{}", crate::task_watch::BACKGROUND_QUIET_META_KEY);
        assert!(!host.events().contains(&quiet));
    }

    /// 대기 meta 가 없거나 읽을 수 없으면 일반 기준으로 알린다.
    #[test]
    fn without_a_readable_wait_meta_the_normal_notice_stays() {
        for meta in [None, Some("not json".to_string()), Some("{}".to_string())] {
            let mut host = ScanHost::new("thinking…\n", "active");
            host.wait_meta = meta.clone();
            let mut s = ErrorScanner::new();
            let t0 = Instant::now();
            s.scan_one_at(&host, 1, t0);
            s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
            assert_eq!(host.stalled_count(), 1, "{meta:?}");
            assert!(!s.is_waiting_on_background_work(1));
        }
    }

    /// 대기 한 번에 한 번만 알린다. 출력이 바뀌고 쿨다운이 지나도 다시 알리지 않는다.
    #[test]
    fn a_background_wait_is_noticed_once() {
        let host = ScanHost::new("❯ \n", "active");
        let mut s = ErrorScanner::new();
        s.mark_background_wait(1, 1_000);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + STALL_QUIET_BACKGROUND_WAIT);
        assert_eq!(host.stalled_count(), 1);
        let t1 = t0 + STALL_QUIET_BACKGROUND_WAIT + STALL_NOTIFY_COOLDOWN;
        host.set_text("❯ \n⏵⏵ 2 shells\n");
        s.scan_one_at(&host, 1, t1);
        s.scan_one_at(&host, 1, t1 + STALL_QUIET_BACKGROUND_WAIT);
        assert_eq!(
            host.stalled_count(),
            1,
            "같은 대기에서는 다시 알리지 않는다"
        );
        // 이어진 대기 Stop은 기록을 새로 만들지 않는다.
        assert_eq!(s.mark_background_wait(1, 9_000), 1_000);
        s.scan_one_at(&host, 1, t1 + STALL_QUIET_BACKGROUND_WAIT * 2);
        assert_eq!(host.stalled_count(), 1);
    }

    /// 대기가 끝난 뒤의 조용한 턴은 다시 일반 기준으로 알린다.
    #[test]
    fn a_quiet_turn_after_the_wait_is_cleared_uses_the_normal_threshold() {
        let host = ScanHost::new("thinking…\n", "active");
        let mut s = ErrorScanner::new();
        s.mark_background_wait(1, 1_000);
        s.clear_background_wait(1);
        assert!(!s.is_waiting_on_background_work(1));
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
        assert_eq!(host.stalled_count(), 1);
    }

    #[test]
    fn disable_clears_the_background_wait() {
        let mut s = ErrorScanner::new();
        s.enable(1, ScanTarget::Child);
        s.mark_background_wait(1, 1_000);
        s.disable(1);
        assert!(!s.is_waiting_on_background_work(1));
    }

    /// 두 번의 읽기에 나뉜 오류 문자열도 누적해서 찾아야 한다.
    #[test]
    fn an_error_split_across_two_polls_is_still_detected() {
        let host = ScanHost::new("\u{23bf} API Er", "active");
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        assert!(
            host.events().is_empty(),
            "반쪽으로는 아직 매치가 아니다: {:?}",
            host.events()
        );

        host.set_text("ror: Connection error\n");
        s.scan_one_at(&host, 1, t0 + Duration::from_millis(800));
        assert_eq!(
            host.events(),
            vec!["claude-error".to_string()],
            "두 델타를 이어 붙이면 매치여야 한다"
        );
    }

    /// 창 상한과 자르는 자리. 바이트로 자르면 UTF-8 중간을 갈라 패닉한다.
    #[test]
    fn the_window_is_capped_and_cut_on_a_char_boundary() {
        // 세 바이트짜리 문자로만 채워 상한을 조금 넘긴다 — 잘라야 할 바이트 수가
        // char 경계에 안 떨어지는 배치다.
        let mut w = "\u{ac00}".repeat(SCAN_WINDOW_MAX / 3 + 10);
        assert!(
            w.len() > SCAN_WINDOW_MAX,
            "시험 입력이 바이트 상한을 넘지 않았다"
        );
        trim_window(&mut w, SCAN_WINDOW_MAX);
        assert!(w.len() <= SCAN_WINDOW_MAX, "상한을 안 지켰다: {}", w.len());
        assert!(
            w.starts_with('\u{ac00}'),
            "char 경계가 아닌 자리에서 잘렸다"
        );
        assert!(
            w.len() + 3 > SCAN_WINDOW_MAX,
            "필요 이상으로 잘랐다: {}",
            w.len()
        );
    }

    /// 한 surface를 해제해도 다른 surface의 상태는 유지돼야 한다.
    #[test]
    fn disable_clears_stall_state() {
        let host = ScanHost::new(ERR, "active");
        let mut s = ErrorScanner::new();
        s.enable(1, ScanTarget::Child);
        s.enable(2, ScanTarget::Child);
        let t0 = Instant::now();
        s.scan_one_at(&host, 1, t0);
        s.scan_one_at(&host, 2, t0);
        s.disable(1);
        assert!(!s.watch.contains_key(&1));
        assert!(!s.last_stall_notify.contains_key(&1));
        assert!(!s.window.contains_key(&1));
        // 끄라고 하지 않은 surface 의 감시는 그대로다.
        assert!(s.watch.contains_key(&2));
        assert!(s.window.contains_key(&2));
        assert!(s.is_enabled(2));
    }

    struct StubHost {
        locate_exists: Option<bool>,
        parent_status: Option<&'static str>,
    }

    impl HostCall for StubHost {
        fn call(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value, tasty_plugin_sdk::PluginError> {
            match method {
                "surface.locate" => match self.locate_exists {
                    Some(e) => Ok(json!({ "exists": e })),
                    None => Err(tasty_plugin_sdk::PluginError::HostCall {
                        method: method.to_string(),
                        message: "host down".into(),
                        code: None,
                    }),
                },
                "terminal.parent" => match self.parent_status {
                    Some("none") => Ok(json!({ "parent_surface_id": null, "status": "none" })),
                    Some(st) => Ok(json!({ "parent_surface_id": 1, "status": st })),
                    None => Err(tasty_plugin_sdk::PluginError::HostCall {
                        method: method.to_string(),
                        message: "host down".into(),
                        code: None,
                    }),
                },
                other => panic!("unexpected host call: {other}"),
            }
        }
    }

    #[test]
    fn top_level_liveness_uses_surface_locate() {
        let alive = StubHost {
            locate_exists: Some(true),
            parent_status: None,
        };
        let dead = StubHost {
            locate_exists: Some(false),
            parent_status: None,
        };
        assert!(scan_target_is_alive(&alive, 4, ScanTarget::TopLevel));
        assert!(!scan_target_is_alive(&dead, 4, ScanTarget::TopLevel));
    }

    #[test]
    fn child_liveness_uses_parent_relation_not_surface_existence() {
        // release 재현: surface 는 살아있지만(locate 는 아예 호출되지 않아야 한다)
        // 부모-자식 관계가 끊겼으므로 스캔 대상에서 빠져야 한다.
        let released = StubHost {
            locate_exists: None,
            parent_status: Some("none"),
        };
        assert!(!scan_target_is_alive(&released, 4, ScanTarget::Child));

        let attached = StubHost {
            locate_exists: None,
            parent_status: Some("active"),
        };
        assert!(scan_target_is_alive(&attached, 4, ScanTarget::Child));
    }

    #[test]
    fn liveness_lookup_failure_keeps_target_enabled() {
        // 일시적 IPC 오류로 감시를 영구히 끄지 않는다 (재활성화 경로가 없다).
        let broken = StubHost {
            locate_exists: None,
            parent_status: None,
        };
        assert!(scan_target_is_alive(&broken, 4, ScanTarget::TopLevel));
        assert!(scan_target_is_alive(&broken, 4, ScanTarget::Child));
    }

    /// 폴링 루프(`main.rs::error_scan_loop`)의 생존 대조 tick 을 재현한다.
    fn liveness_tick<H: HostCall>(scanner: &mut ErrorScanner, host: &H) {
        for (sid, target) in scanner.enabled_snapshot() {
            if !scan_target_is_alive(host, sid, target) {
                scanner.disable(sid);
            }
        }
    }

    #[test]
    fn spawned_child_survives_ticks_while_relation_holds() {
        let mut s = ErrorScanner::new();
        s.enable(42, ScanTarget::Child); // handle_spawn
        let attached = StubHost {
            locate_exists: None,
            parent_status: Some("active"),
        };
        liveness_tick(&mut s, &attached);
        assert!(s.is_enabled(42));
    }

    #[test]
    fn released_child_is_dropped_with_its_dedupe_state() {
        // release는 surface를 남기므로 부모 관계 해제를 기준으로 정리해야 한다.
        let mut s = ErrorScanner::new();
        s.enable(42, ScanTarget::Child);
        s.seed_dedupe_for_test(42, "API Error: boom");
        let released = StubHost {
            locate_exists: Some(true),
            parent_status: Some("none"),
        };
        liveness_tick(&mut s, &released);
        assert!(!s.is_enabled(42), "release 후에는 스캔 대상에서 빠져야 함");
        assert!(!s.has_dedupe_state(42), "dedupe 상태도 함께 정리돼야 함");
    }

    #[test]
    fn top_level_launch_surface_is_not_judged_by_child_relation() {
        // launch surface 는 child registry 에 없다 — 관계로 판정하면 즉시 꺼진다.
        let mut s = ErrorScanner::new();
        s.enable(9, ScanTarget::TopLevel);
        let host = StubHost {
            locate_exists: Some(true),
            parent_status: Some("none"),
        };
        liveness_tick(&mut s, &host);
        assert!(s.is_enabled(9));
    }
}

#[cfg(test)]
mod poison_tests {
    use super::*;
    use std::sync::Mutex;

    /// poison 상태에서도 등록·조회·해제가 가능하고 폴링 대상 목록을 유지해야 한다.
    #[test]
    fn a_poisoned_scanner_keeps_the_scan_loop_alive() {
        let scanner = Mutex::new(ErrorScanner::default());
        lock_scanner(&scanner).enable(7, ScanTarget::TopLevel);

        let panicked = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _held = scanner.lock().expect("not poisoned yet");
                    panic!("poison the error scanner");
                })
                .join()
        });
        assert!(panicked.is_err(), "the helper thread must have panicked");
        assert!(
            scanner.lock().is_err(),
            "the scanner lock must actually be poisoned now"
        );

        assert_eq!(
            lock_scanner(&scanner).enabled_snapshot().len(),
            1,
            "a poisoned scanner must still hand the polling loop its work list — giving up \
             here kills error scanning for the rest of the process"
        );
        assert!(
            lock_scanner(&scanner).is_enabled(7),
            "reads must survive the poison"
        );

        lock_scanner(&scanner).enable(9, ScanTarget::Child);
        assert!(
            lock_scanner(&scanner).is_enabled(9),
            "registration must land on a poisoned scanner, or that surface is never scanned"
        );
        lock_scanner(&scanner).disable(7);
        assert!(
            !lock_scanner(&scanner).is_enabled(7),
            "deregistration must land too, or a dead surface is scanned forever"
        );
    }
}
