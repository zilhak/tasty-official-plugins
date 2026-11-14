//! Tasty helper로 띄운 Codex의 재시작을 helper가 기록한 세대 상태로 판정한다.
//!
//! 화면 문구로는 종료를 확정할 수 없다. Codex 0.154는 `/new` 때도 TUI를 끝내지 않은 채 이전
//! 세션의 "run codex resume" 안내와 새 시작 배너를 찍고, 보이는 화면의 문구 수는 스크롤로 줄어든다.
//! helper는 TUI가 끝나자마자 세대의 `exited`를 기록하고, 새 helper는 서버가 준비되면 surface의
//! 새 세대를 `ready`로 기록한다. 그래서 helper 세션에서는 이 기록으로 종료와 복귀를 판정한다.
//!
//! Ctrl+C는 한 번 보낼 때마다 종료를 확인하고, 종료되면 더 보내지 않는다. 끝난 TUI 뒤의 셸에
//! Ctrl+C가 들어가면 셸이 프롬프트를 다시 그리는 동안 이어 보낸 입력의 첫 글자를 잃을 수 있다.
//! helper는 `exited`를 기록한 뒤 서버를 정리하고 끝나므로, `exited`만 보고 보내면 helper가 아직
//! 전경에 있을 수 있다. resume 명령은 호스트가 전경을 셸로 분류(`is_shell: true`)하고 그 pid·이름이
//! 요청 때의 helper와 다를 때만 보낸다. 셸 분류의 근거는 호스트 판정이다. pid·이름이 달라도 셸이
//! 아닌 프로그램(예: vim)일 수 있기 때문이다. `false`·`null`이거나 조회되지 않으면 보내지 않고 실패로
//! 남긴다. 요청 때 기준 전경을 읽을 수 없으면 요청 단계에서 거절한다.
//!
//! 기다리는 동안 같은 surface에 다른 helper 세대가 열릴 수 있다(사용자의 helper 실행, respawn, 탭
//! 복원). 그 세대가 등록되면 이전 세대 기록은 정리될 수 있다. 그래서 Ctrl+C와 resume을 보내기 직전마다
//! surface의 현재 세대가 대상 세대인지 확인하고, 바뀌었으면 아무것도 보내지 않고 멈춘다. 등록 전의 새
//! helper는 셸 분류가 아니며, 이름도 요청 때 helper와 같아 추가로 걸러진다. 재개는 resume 전송 뒤 만들어지고 같은
//! thread를 기록한 새 세대가 준비된 것으로만 확인하며, 다른 세대가 나타나면 실패로 끝낸다.
//!
//! 전경 pid의 의미는 OS마다 다르다. Linux·macOS는 터미널의 전경 프로세스 그룹 ID이고, helper는
//! TUI를 자기 그룹에서 실행하므로(서버만 별도 그룹) helper가 끝날 때까지 helper의 그룹이다.
//! Windows는 ConPTY에 전경 그룹이 없어 셸의 가장 얕은 셸 아닌 자손(helper)을 고르고, 자손이 없으면
//! 셸 자신을 돌려준다. 결과는 로그로 남고, 완료 여부는 surface의 새 세대가 준비됐는지로 판정한다.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::session::registry::{HelperPhase, current_gen, read_helper, read_session};

/// Ctrl+C 전송 횟수와 간격.
pub(crate) const CTRL_C_COUNT: u32 = 4;
pub(crate) const CTRL_C_INTERVAL: Duration = Duration::from_millis(500);
/// 마지막 Ctrl+C 뒤 종료를 기다리는 한도.
pub(crate) const EXIT_WAIT: Duration = Duration::from_secs(8);
/// 종료 뒤 셸 복귀(전경 프로세스 변경)를 기다리는 한도.
pub(crate) const SHELL_WAIT: Duration = Duration::from_secs(5);
/// resume 명령 뒤 새 세대가 준비되기를 기다리는 한도.
pub(crate) const RETURN_WAIT: Duration = Duration::from_secs(30);
/// 상태 확인 간격.
pub(crate) const POLL: Duration = Duration::from_millis(100);

/// 기다리는 한도.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub exit: Duration,
    pub shell: Duration,
    pub ready: Duration,
}

impl Limits {
    pub(crate) const DEFAULT: Self = Self {
        exit: EXIT_WAIT,
        shell: SHELL_WAIT,
        ready: RETURN_WAIT,
    };
}

/// 호스트가 보고한 전경 프로세스.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Foreground {
    pub pid: u32,
    pub name: String,
    /// 호스트의 셸 분류. 판정할 수 없으면 None이다.
    pub is_shell: Option<bool>,
}

impl Foreground {
    /// 요청 때의 helper가 끝나고 셸이 전경으로 돌아왔는지. 호스트가 셸로 분류해야 하고, 추가로
    /// 요청 때의 helper와 pid·이름이 달라야 한다(등록 전의 새 helper 방어).
    fn is_shell_after(&self, before: &Foreground) -> bool {
        self.is_shell == Some(true) && self.pid != before.pid && self.name != before.name
    }
}

/// 재시작할 helper 세션.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HelperWatch {
    pub root: PathBuf,
    pub surface: u32,
    pub generation: String,
}

/// resume 뒤 surface의 세대 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resumed {
    /// 아직 새 세대가 없거나 자기 새 세대가 준비 중이다.
    Waiting,
    /// 자기 resume으로 만든 새 세대가 준비됐다.
    Ready(String),
    /// 다른 세대가 surface를 차지했다.
    Foreign,
    /// 자기 새 세대가 재개 확인 전에 끝났다(재개 거부·정책 미적용·TUI 시작 실패).
    Failed,
}

impl HelperWatch {
    /// surface의 현재 세대가 실행 중인 helper 세션이면 그 세대를 돌려준다.
    pub(crate) fn find(root: &Path, surface: u32) -> Option<Self> {
        let generation = current_gen(root, surface).ok()??;
        let session = read_session(root, &generation).ok()??;
        if session.surface != surface {
            return None;
        }
        let helper = read_helper(root, &generation).ok()??;
        (helper.phase == HelperPhase::Ready).then(|| Self {
            root: root.to_path_buf(),
            surface,
            generation,
        })
    }

    /// surface의 현재 세대가 아직 대상 세대인지. 조회에 실패하면 아니라고 본다.
    pub(crate) fn is_current(&self) -> bool {
        matches!(current_gen(&self.root, self.surface), Ok(Some(g)) if g == self.generation)
    }

    /// 세대의 TUI가 끝났는지.
    pub(crate) fn ended(&self) -> bool {
        matches!(read_helper(&self.root, &self.generation),
            Ok(Some(h)) if matches!(h.phase, HelperPhase::Exited | HelperPhase::Failed))
    }

    /// resume 뒤의 상태. 자기 새 세대는 `sent_at_ms` 이후에 만들어지고 `thread`를 재개한 세대다.
    fn resumed(&self, thread: &str, sent_at_ms: u64) -> Resumed {
        let Ok(Some(generation)) = current_gen(&self.root, self.surface) else {
            return Resumed::Waiting;
        };
        if generation == self.generation {
            return Resumed::Waiting;
        }
        let Ok(Some(session)) = read_session(&self.root, &generation) else {
            return Resumed::Waiting;
        };
        let own = session.surface == self.surface
            && session.created_at_ms >= sent_at_ms
            && session
                .thread
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case(thread));
        if !own {
            return Resumed::Foreign;
        }
        // `ready`는 서버 준비와 helper의 재개 확인(`resumed`) 뒤에 기록된다. 재개를 확인하지 않은
        // `ready`는 완료가 아니다.
        match read_helper(&self.root, &generation) {
            Ok(Some(h))
                if h.phase == HelperPhase::Ready
                    && h.resumed
                        .as_deref()
                        .is_some_and(|t| t.eq_ignore_ascii_case(thread)) =>
            {
                Resumed::Ready(generation)
            }
            Ok(Some(h)) if matches!(h.phase, HelperPhase::Exited | HelperPhase::Failed) => {
                Resumed::Failed
            }
            _ => Resumed::Waiting,
        }
    }
}

/// 재시작에 필요한 터미널 조작.
pub(crate) trait RebootIo {
    fn ctrl_c(&self) -> Result<(), String>;
    /// 전경 프로세스. 확인할 수 없으면 None.
    fn foreground(&self) -> Option<Foreground>;
    fn send(&self, text: &str) -> Result<(), String>;
    fn sleep(&self, d: Duration);
    /// Codex 입력란에 안내를 넣는다.
    fn tell(&self, text: &str) -> Result<(), String>;
    /// 화면에 `snippet`이 보이는지.
    fn visible(&self, snippet: &str) -> bool;
    /// 별도 Enter.
    fn enter(&self) -> Result<(), String>;
    /// 세대 기록과 같은 시계의 현재 시각(ms).
    fn now_ms(&self) -> u64 {
        crate::store::now_ms()
    }
}

/// 재시작 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 자기 resume으로 만든 새 세대 `generation`이 준비됐다. 보낸 Ctrl+C 수.
    Restarted { ctrl_c: u32, generation: String },
    /// Ctrl+C 뒤에도 TUI가 끝나지 않았다. resume을 보내지 않았다.
    NotExited,
    /// TUI는 끝났지만 셸이 전경으로 돌아온 것을 확인하지 못했다. resume을 보내지 않았다.
    ShellNotReady,
    /// resume을 보냈지만 새 세대가 준비되지 않았다.
    NotReturned,
    /// surface가 다른 세대로 바뀌었다. 그 뒤로는 아무것도 보내지 않았다.
    TargetChanged,
    /// resume 명령으로 만든 새 세대가 재개를 확인하기 전에 끝났다. 안내를 보내지 않았다.
    ResumeFailed,
    /// 터미널 조작이 실패했다.
    Failed,
}

/// `pred`가 참이 되거나 `timeout`이 지날 때까지 확인한다.
fn wait_until(io: &dyn RebootIo, timeout: Duration, pred: &dyn Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        io.sleep(POLL);
    }
}

fn target_changed(watch: &HelperWatch, stage: &str) -> Outcome {
    tracing::warn!(
        "codex reboot s{}: surface no longer runs session {} ({stage}); nothing more is sent",
        watch.surface,
        watch.generation
    );
    Outcome::TargetChanged
}

/// 종료부터 복귀까지 실행한다. `fg0`은 요청 때의 전경 프로세스, `thread`는 재개할 thread다.
pub(crate) fn restart(
    io: &dyn RebootIo,
    watch: &HelperWatch,
    fg0: &Foreground,
    thread: &str,
    resume: &str,
    limits: Limits,
) -> Outcome {
    let sent = match interrupt(io, watch, limits) {
        Ok(sent) => sent,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = return_to_shell(io, watch, fg0, limits) {
        return outcome;
    }
    resume_session(io, watch, thread, resume, limits, sent)
}

/// Ctrl+C 로 세션을 끝내고 끝날 때까지 기다린다. 보낸 Ctrl+C 수를 돌려준다.
fn interrupt(io: &dyn RebootIo, watch: &HelperWatch, limits: Limits) -> Result<u32, Outcome> {
    let surface = watch.surface;
    let mut sent = 0;
    while !watch.ended() && sent < CTRL_C_COUNT {
        if !watch.is_current() {
            return Err(target_changed(watch, "before Ctrl+C"));
        }
        if let Err(e) = io.ctrl_c() {
            tracing::warn!("codex reboot s{surface}: Ctrl+C failed: {e} — aborting");
            return Err(Outcome::Failed);
        }
        sent += 1;
        if wait_until(io, CTRL_C_INTERVAL, &|| {
            watch.ended() || !watch.is_current()
        }) {
            break;
        }
    }
    if !wait_until(io, limits.exit, &|| watch.ended() || !watch.is_current()) {
        tracing::warn!(
            "codex reboot s{surface}: session {} did not end after {sent}x Ctrl+C; resume command not sent",
            watch.generation
        );
        return Err(Outcome::NotExited);
    }
    if !watch.is_current() {
        return Err(target_changed(watch, "while waiting for exit"));
    }
    Ok(sent)
}

/// 세션이 끝난 뒤 전경이 셸로 돌아올 때까지 기다린다.
fn return_to_shell(
    io: &dyn RebootIo,
    watch: &HelperWatch,
    fg0: &Foreground,
    limits: Limits,
) -> Result<(), Outcome> {
    let surface = watch.surface;
    // helper가 서버를 정리하고 끝나면 전경이 셸로 돌아온다.
    let returned = wait_until(io, limits.shell, &|| {
        io.foreground().is_some_and(|fg| fg.is_shell_after(fg0))
    });
    if !returned {
        tracing::warn!(
            "codex reboot s{surface}: session ended but the shell did not return to the foreground \
             (foreground at request: {} {}); resume command not sent",
            fg0.name,
            fg0.pid
        );
        return Err(Outcome::ShellNotReady);
    }
    if !watch.is_current() {
        return Err(target_changed(watch, "before resume"));
    }
    Ok(())
}

/// 재개 명령을 보내고 새 세션이 준비될 때까지 기다린다. `sent`는 보낸 Ctrl+C 수다.
fn resume_session(
    io: &dyn RebootIo,
    watch: &HelperWatch,
    thread: &str,
    resume: &str,
    limits: Limits,
    sent: u32,
) -> Outcome {
    let surface = watch.surface;
    let sent_at_ms = io.now_ms();
    if let Err(e) = io.send(resume) {
        tracing::warn!("codex reboot s{surface}: resume send failed: {e}");
        return Outcome::Failed;
    }
    let state = RefCell::new(Resumed::Waiting);
    wait_until(io, limits.ready, &|| {
        let now = watch.resumed(thread, sent_at_ms);
        let settled = now != Resumed::Waiting;
        *state.borrow_mut() = now;
        settled
    });
    resumed_outcome(state.into_inner(), watch, limits, sent)
}

/// 재개를 기다린 결과를 재시작 결과로 바꾼다. `sent`는 보낸 Ctrl+C 수다.
fn resumed_outcome(resumed: Resumed, watch: &HelperWatch, limits: Limits, sent: u32) -> Outcome {
    let surface = watch.surface;
    match resumed {
        Resumed::Ready(generation) => Outcome::Restarted {
            ctrl_c: sent,
            generation,
        },
        Resumed::Foreign => target_changed(watch, "after resume"),
        Resumed::Failed => {
            tracing::warn!(
                "codex reboot s{surface}: the resumed session ended before it was confirmed; see its helper detail — notice skipped"
            );
            Outcome::ResumeFailed
        }
        Resumed::Waiting => {
            tracing::warn!(
                "codex reboot s{surface}: no new session became ready within {}s — resume sent but notice skipped",
                limits.ready.as_secs()
            );
            Outcome::NotReturned
        }
    }
}

/// 재시작 뒤 안내 전달 방법.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Notice<'a> {
    pub text: &'a str,
    /// 화면에서 안내를 확인할 선두 문구.
    pub snippet: &'a str,
    pub attempts: u32,
    /// 새 TUI가 입력을 받을 때까지 기다리는 시간.
    pub grace: Duration,
    /// 넣은 뒤 화면을 확인하기까지의 시간.
    pub verify: Duration,
    /// 보이지 않을 때 다시 넣기까지의 시간.
    pub retry: Duration,
    /// 별도 Enter 전 대기.
    pub submit: Duration,
}

/// 안내 전달 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeResult {
    Delivered,
    /// 정해진 횟수 안에 화면에서 확인하지 못했다.
    NotVisible,
    /// surface가 `generation`을 떠났거나 그 세대의 TUI가 끝났다. 그 뒤로는 보내지 않았다.
    Left,
    Failed,
}

/// 확인한 새 세대 `generation`에만 안내와 별도 Enter를 보낸다. 대기 뒤 매 입력 직전에 surface가
/// 아직 그 세대이고 그 helper가 `ready`(TUI 실행 중)인지 확인한다. 다시 조회한 현재 세대를 대상으로 삼지 않는다. 확인 사이에 사용자가 연
/// 다른 세대를 안내 대상으로 오인할 수 있기 때문이다.
pub(crate) fn deliver_notice(
    io: &dyn RebootIo,
    watch: &HelperWatch,
    generation: &str,
    notice: &Notice<'_>,
) -> NoticeResult {
    let surface = watch.surface;
    // 세대가 같아도 helper가 `exited`·`failed`를 기록했다면 TUI가 끝나 셸이 입력을 받는다.
    let on = || {
        matches!(current_gen(&watch.root, surface), Ok(Some(g)) if g == generation)
            && matches!(read_helper(&watch.root, generation),
                Ok(Some(h)) if h.phase == HelperPhase::Ready)
    };
    let left = |stage: &str| {
        tracing::warn!(
            "codex reboot s{surface}: surface left session {generation} ({stage}); notice stopped"
        );
        NoticeResult::Left
    };
    let submit = || {
        io.sleep(notice.submit);
        if !on() {
            return left("before Enter");
        }
        match io.enter() {
            Ok(()) => NoticeResult::Delivered,
            Err(e) => {
                tracing::warn!("codex reboot s{surface}: extra submit enter failed: {e}");
                NoticeResult::Failed
            }
        }
    };
    io.sleep(notice.grace);
    for attempt in 1..=notice.attempts {
        if !on() {
            return left("before notice");
        }
        if let Err(e) = io.tell(notice.text) {
            tracing::warn!("codex reboot s{surface}: notice tell failed: {e}");
            return NoticeResult::Failed;
        }
        io.sleep(notice.verify);
        if io.visible(notice.snippet) {
            return submit();
        }
        tracing::info!(
            "codex reboot s{surface}: notice attempt {attempt}/{} not visible yet — retrying",
            notice.attempts
        );
        io.sleep(notice.retry);
    }
    if io.visible(notice.snippet) {
        return submit();
    }
    NoticeResult::NotVisible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::registry::{HelperState, Session, helper_file, session_dir, write_session};
    use std::cell::{Cell, RefCell};

    const OLD: &str = "ffd4ec8d832e184e";
    const NEW: &str = "0123456789abcdef";
    const USER: &str = "fedcba9876543210";
    const THREAD: &str = "019f55e7-3dfa-7a00-8000-0123456789ab";
    const OTHER_THREAD: &str = "019f55e7-3dfa-7a00-8000-ba9876543210";
    const HELPER_NAME: &str = "tasty-plugin-co";
    /// 요청 때의 helper, 셸, 사용자가 새로 띄운 helper.
    const HELPER_PID: u32 = 100;
    const SHELL_PID: u32 = 200;
    const USER_HELPER_PID: u32 = 300;
    /// 셸 위에서 사용자가 띄운 일반 프로그램.
    const VIM_PID: u32 = 400;
    const FAST: Limits = Limits {
        exit: Duration::from_millis(200),
        shell: Duration::from_millis(200),
        ready: Duration::from_millis(200),
    };

    fn fg0() -> Foreground {
        Foreground {
            pid: HELPER_PID,
            name: HELPER_NAME.into(),
            is_shell: Some(false),
        }
    }

    fn helper(root: &Path, generation: &str, phase: HelperPhase) {
        helper_resumed(root, generation, phase, None);
    }

    /// `resumed`는 helper가 서버 응답으로 확인한 재개 thread다.
    fn helper_resumed(root: &Path, generation: &str, phase: HelperPhase, resumed: Option<&str>) {
        crate::store::write_json(
            &helper_file(&session_dir(root, generation)),
            &HelperState {
                phase,
                url: Some("unix:///x/s.sock".into()),
                detail: None,
                resumed: resumed.map(str::to_string),
                updated_at_ms: 0,
            },
        )
        .unwrap();
    }

    fn register_with(
        root: &Path,
        generation: &str,
        thread: Option<&str>,
        created_at_ms: u64,
        phase: HelperPhase,
    ) {
        let session = Session {
            generation: generation.into(),
            surface: 522,
            thread: thread.map(str::to_string),
            tui_args: Vec::new(),
            native_args: None,
            model: None,
            turn_attempts: Vec::new(),
            listen: "unix:///x/s.sock".into(),
            token_file: None,
            socket_dir: None,
            created_at_ms,
        };
        write_session(root, &session).unwrap();
        crate::store::write_json(
            &root.join("surfaces").join("522.json"),
            &serde_json::json!({ "generation": generation }),
        )
        .unwrap();
        helper_resumed(root, generation, phase, thread);
    }

    fn register(root: &Path, generation: &str) {
        register_with(root, generation, None, 0, HelperPhase::Ready);
    }

    /// 사용자가 같은 surface에서 옛 Codex를 끝내고 새 helper 세대를 연다. `retire`면 새 세대 등록이
    /// 끝난 옛 세대 파일을 지운다(`registry::open_session`).
    fn user_reopens(root: &Path, retire: bool) {
        helper(root, OLD, HelperPhase::Exited);
        register_with(root, USER, None, crate::store::now_ms(), HelperPhase::Ready);
        if retire {
            std::fs::remove_dir_all(session_dir(root, OLD)).unwrap();
        }
    }

    /// resume 뒤 surface에 나타날 세대.
    #[derive(Clone, Copy, PartialEq)]
    enum AfterResume {
        /// 자기 resume으로 만든 새 세대.
        Own,
        /// resume 명령이 실행되지 않음(예: 첫 글자를 잃음).
        Nothing,
        /// 다른 thread를 연 세대.
        OtherThread,
        /// resume 전송 전에 만들어진 세대.
        Older,
        /// 서버는 준비됐지만 helper가 재개를 확인하지 않았다(`resumed` 없음).
        Unconfirmed,
        /// helper가 재개를 확인하지 못하고 실패했다(예: 원격 재개 거부, 정책 미적용).
        Rejected,
    }

    /// 실행 중인 helper 세션을 흉내 낸다. Ctrl+C `quit_at`번째에 TUI가 끝나고, 그 뒤 전경이 셸이
    /// 된다.
    struct Fake {
        root: PathBuf,
        quit_at: u32,
        ctrl_c: Cell<u32>,
        fg: Cell<u32>,
        /// 참이면 TUI가 끝나도 전경이 바뀌지 않는다(helper가 아직 실행 중).
        keep_fg: Cell<bool>,
        /// 참이면 전경 조회가 실패한다.
        fg_unreadable: Cell<bool>,
        /// 참이면 호스트가 셸 여부를 판정하지 못한다(`is_shell: null`).
        shell_unknown: Cell<bool>,
        /// 0이 아니면 TUI가 끝날 때 전경이 이 pid가 된다.
        fg_at_quit: Cell<u32>,
        /// 0이 아니면 TUI가 끝날 때 사용자가 새 helper를 띄운다. 1은 등록 전, 2는 등록 후,
        /// 3은 셸이 돌아온 뒤 전경을 확인하는 동안 새 세대가 등록된다.
        user_at_quit: Cell<u8>,
        after_resume: Cell<AfterResume>,
        sent: RefCell<Vec<String>>,
        /// 안내 tell과 별도 Enter 수.
        told: Cell<u32>,
        entered: Cell<u32>,
        /// 몇 번째 tell부터 화면에 안내가 보이는지. 0이면 보이지 않는다.
        visible_from: Cell<u32>,
        /// sleep 호출 수와, 0이 아니면 그 번째 sleep에서 사용자가 새 세대를 연다.
        sleeps: Cell<u32>,
        user_at_sleep: Cell<u32>,
        /// 0이 아니면 그 번째 sleep에서 재개한 TUI가 끝난다(helper `exited`).
        tui_exit_at_sleep: Cell<u32>,
        /// resume을 보낼 때의 Ctrl+C 수와 전경 pid.
        sent_state: RefCell<Vec<(u32, u32)>>,
    }

    impl RebootIo for Fake {
        fn ctrl_c(&self) -> Result<(), String> {
            let n = self.ctrl_c.get() + 1;
            self.ctrl_c.set(n);
            if n == self.quit_at {
                helper(&self.root, OLD, HelperPhase::Exited);
                match self.user_at_quit.get() {
                    0 if self.fg_at_quit.get() != 0 => self.fg.set(self.fg_at_quit.get()),
                    0 if !self.keep_fg.get() => self.fg.set(SHELL_PID),
                    0 => {}
                    1 => self.fg.set(USER_HELPER_PID),
                    3 => self.fg.set(SHELL_PID),
                    _ => {
                        self.fg.set(USER_HELPER_PID);
                        user_reopens(&self.root, true);
                    }
                }
            }
            Ok(())
        }
        fn foreground(&self) -> Option<Foreground> {
            if self.fg_unreadable.get() {
                return None;
            }
            if self.user_at_quit.get() == 3 && self.ctrl_c.get() >= self.quit_at {
                user_reopens(&self.root, false);
            }
            let pid = self.fg.get();
            let (name, shell) = match pid {
                SHELL_PID => ("zsh", true),
                VIM_PID => ("vim", false),
                _ => (HELPER_NAME, false),
            };
            Some(Foreground {
                pid,
                name: name.into(),
                is_shell: (!self.shell_unknown.get()).then_some(shell),
            })
        }
        fn send(&self, text: &str) -> Result<(), String> {
            self.sent.borrow_mut().push(text.into());
            self.sent_state
                .borrow_mut()
                .push((self.ctrl_c.get(), self.fg.get()));
            let now = crate::store::now_ms();
            let (thread, created) = match self.after_resume.get() {
                AfterResume::Nothing => return Ok(()),
                AfterResume::Own => (THREAD, now),
                AfterResume::OtherThread => (OTHER_THREAD, now),
                AfterResume::Older => (THREAD, now - 1_000),
                AfterResume::Unconfirmed => {
                    register_with(&self.root, NEW, Some(THREAD), now, HelperPhase::Ready);
                    helper(&self.root, NEW, HelperPhase::Ready);
                    return Ok(());
                }
                AfterResume::Rejected => {
                    register_with(&self.root, NEW, Some(THREAD), now, HelperPhase::Failed);
                    return Ok(());
                }
            };
            register_with(&self.root, NEW, Some(thread), created, HelperPhase::Ready);
            Ok(())
        }
        fn sleep(&self, _d: Duration) {
            let n = self.sleeps.get() + 1;
            self.sleeps.set(n);
            if n == self.tui_exit_at_sleep.get() {
                helper(&self.root, NEW, HelperPhase::Exited);
            }
            if n == self.user_at_sleep.get() {
                register_with(
                    &self.root,
                    USER,
                    None,
                    crate::store::now_ms(),
                    HelperPhase::Ready,
                );
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        fn tell(&self, _text: &str) -> Result<(), String> {
            self.told.set(self.told.get() + 1);
            Ok(())
        }
        fn visible(&self, _snippet: &str) -> bool {
            self.visible_from.get() != 0 && self.told.get() >= self.visible_from.get()
        }
        fn enter(&self) -> Result<(), String> {
            self.entered.set(self.entered.get() + 1);
            Ok(())
        }
    }

    fn fake(root: &Path, quit_at: u32) -> Fake {
        Fake {
            root: root.to_path_buf(),
            quit_at,
            ctrl_c: Cell::new(0),
            fg: Cell::new(HELPER_PID),
            keep_fg: Cell::new(false),
            fg_unreadable: Cell::new(false),
            shell_unknown: Cell::new(false),
            fg_at_quit: Cell::new(0),
            user_at_quit: Cell::new(0),
            after_resume: Cell::new(AfterResume::Own),
            sent: RefCell::new(Vec::new()),
            told: Cell::new(0),
            entered: Cell::new(0),
            visible_from: Cell::new(1),
            sleeps: Cell::new(0),
            user_at_sleep: Cell::new(0),
            tui_exit_at_sleep: Cell::new(0),
            sent_state: RefCell::new(Vec::new()),
        }
    }

    fn setup() -> (tempfile::TempDir, HelperWatch) {
        let dir = tempfile::tempdir().unwrap();
        register(dir.path(), OLD);
        let watch = HelperWatch::find(dir.path(), 522).unwrap();
        (dir, watch)
    }

    fn restarted(ctrl_c: u32) -> Outcome {
        Outcome::Restarted {
            ctrl_c,
            generation: NEW.into(),
        }
    }

    const NOTICE: Notice<'static> = Notice {
        text: "tasty codex reboot: notice",
        snippet: "tasty codex reboot",
        attempts: 3,
        grace: Duration::ZERO,
        verify: Duration::ZERO,
        retry: Duration::ZERO,
        submit: Duration::ZERO,
    };

    /// 재시작을 확인한 뒤 안내를 보낸다. `Restarted`가 실은 세대만 대상으로 쓴다.
    fn restart_and_notify(io: &Fake, watch: &HelperWatch) -> NoticeResult {
        let Outcome::Restarted { generation, .. } = run(io, watch) else {
            panic!("restart did not complete");
        };
        io.sleeps.set(0);
        deliver_notice(io, watch, &generation, &NOTICE)
    }

    fn run(io: &Fake, watch: &HelperWatch) -> Outcome {
        restart(io, watch, &fg0(), THREAD, "resume\r", FAST)
    }

    /// E8#2: 첫 Ctrl+C에 TUI가 끝났다(helper exited). 화면 문구와 관계없이 종료를 확인하고, 남은
    /// Ctrl+C를 셸에 보내지 않으며, 셸 복귀 뒤 resume을 보내 새 세대 준비까지 확인한다.
    #[test]
    fn a_helper_session_restarts_from_its_recorded_state() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 1);
        assert_eq!(run(&io, &watch), restarted(1));
        assert_eq!(io.ctrl_c.get(), 1, "끝난 뒤 Ctrl+C를 더 보내지 않는다");
        assert_eq!(*io.sent.borrow(), vec!["resume\r".to_string()]);
        assert_eq!(
            *io.sent_state.borrow(),
            vec![(1, SHELL_PID)],
            "셸 복귀 뒤에 보낸다"
        );
    }

    /// 진행 중인 턴이 있으면 첫 Ctrl+C는 턴을 멈추고 다음 Ctrl+C가 끝낸다.
    #[test]
    fn ctrl_c_stops_as_soon_as_the_session_ends() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 2);
        assert_eq!(run(&io, &watch), restarted(2));
        assert_eq!(io.ctrl_c.get(), 2);
    }

    /// 끝나지 않으면 resume을 보내지 않는다.
    #[test]
    fn no_resume_is_sent_while_the_session_runs() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 99);
        let started = Instant::now();
        assert_eq!(run(&io, &watch), Outcome::NotExited);
        assert!(started.elapsed() >= FAST.exit);
        assert_eq!(io.ctrl_c.get(), CTRL_C_COUNT);
        assert!(io.sent.borrow().is_empty());
    }

    /// 전경이 helper에서 셸로 돌아오지 않거나(helper가 아직 서버를 정리 중) 조회되지 않으면
    /// `exited`만으로 resume을 보내지 않는다.
    #[test]
    fn no_resume_is_sent_before_the_shell_is_back() {
        for unreadable in [false, true] {
            let (dir, watch) = setup();
            let io = fake(dir.path(), 1);
            io.keep_fg.set(true);
            io.fg_unreadable.set(unreadable);
            assert_eq!(
                run(&io, &watch),
                Outcome::ShellNotReady,
                "unreadable={unreadable}"
            );
            assert!(io.sent.borrow().is_empty());
        }
    }

    /// resume 뒤 자기 새 세대가 준비되지 않으면 성공으로 보지 않는다. 다른 thread를 연 세대나
    /// 전송 전에 만들어진 세대는 자기 재개가 아니다.
    #[test]
    fn only_its_own_new_session_counts_as_a_restart() {
        for (after, want) in [
            (AfterResume::Nothing, Outcome::NotReturned),
            (AfterResume::OtherThread, Outcome::TargetChanged),
            (AfterResume::Older, Outcome::TargetChanged),
            (AfterResume::Unconfirmed, Outcome::NotReturned),
            (AfterResume::Rejected, Outcome::ResumeFailed),
        ] {
            let (dir, watch) = setup();
            let io = fake(dir.path(), 1);
            io.after_resume.set(after);
            assert_eq!(run(&io, &watch), want);
            assert_eq!(io.sent.borrow().len(), 1);
        }
    }

    /// delay 중 사용자가 옛 Codex를 끝내고 같은 surface에 새 helper 세대를 열었다. 옛 세대 기록이
    /// 정리됐든 남았든 새 TUI에 Ctrl+C·resume을 넣지 않고 성공으로도 기록하지 않는다.
    #[test]
    fn a_session_reopened_before_ctrl_c_gets_no_input() {
        for retire in [true, false] {
            let (dir, watch) = setup();
            user_reopens(dir.path(), retire);
            let io = fake(dir.path(), 99);
            io.fg.set(USER_HELPER_PID);
            assert_eq!(
                (io.ctrl_c.get(), io.sent.borrow().len(), run(&io, &watch)),
                (0, 0, Outcome::TargetChanged),
                "retire={retire}"
            );
            assert_eq!(io.ctrl_c.get(), 0, "retire={retire}");
        }
    }

    /// 종료를 기다리는 동안 새 helper 세대가 등록됐다. 남은 Ctrl+C와 resume을 보내지 않는다.
    #[test]
    fn a_session_reopened_during_the_exit_wait_gets_no_input() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 1);
        io.user_at_quit.set(2);
        assert_eq!(run(&io, &watch), Outcome::TargetChanged);
        assert_eq!(io.ctrl_c.get(), 1);
        assert!(io.sent.borrow().is_empty());
    }

    /// 셸 복귀를 확인하는 사이 새 세대가 등록됐다. resume을 보내지 않는다.
    #[test]
    fn a_session_reopened_during_the_shell_wait_gets_no_resume() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 1);
        io.user_at_quit.set(3);
        assert_eq!(run(&io, &watch), Outcome::TargetChanged);
        assert!(io.sent.borrow().is_empty());
    }

    /// helper가 끝난 뒤 전경이 셸이 아닌 일반 프로그램(vim)이면 pid·이름이 모두 달라도 셸 복귀가
    /// 아니다. 호스트가 셸 여부를 판정하지 못해도(`null`) 보내지 않는다.
    #[test]
    fn only_a_host_classified_shell_receives_the_resume() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 1);
        io.fg_at_quit.set(VIM_PID);
        assert_eq!(run(&io, &watch), Outcome::ShellNotReady);
        assert!(io.sent.borrow().is_empty());

        let (dir, watch) = setup();
        let io = fake(dir.path(), 1);
        io.shell_unknown.set(true);
        assert_eq!(run(&io, &watch), Outcome::ShellNotReady);
        assert!(io.sent.borrow().is_empty());
    }

    /// 아직 세대를 등록하지 않은 새 helper가 전경이면 셸 복귀로 보지 않는다.
    #[test]
    fn an_unregistered_new_helper_is_not_the_shell() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 1);
        io.user_at_quit.set(1);
        assert_eq!(run(&io, &watch), Outcome::ShellNotReady);
        assert!(io.sent.borrow().is_empty());
    }

    /// rt-f25-1 E8: 재개한 TUI가 시작 직후 끝나 셸로 돌아왔다(helper `exited`). 세대는 그대로지만
    /// 안내와 Enter를 셸에 넣지 않는다. 대기 중, 첫 확인 대기 중(안내가 셸에 보여도), Enter 대기 중
    /// 모두 멈춘다.
    #[test]
    fn a_tui_that_ended_after_the_restart_gets_no_notice() {
        // (TUI가 끝나는 sleep 번호, 기대 tell 수, 기대 Enter 수)
        for (at, told, entered) in [(1, 0, 0), (2, 1, 0), (3, 1, 0)] {
            let (dir, watch) = setup();
            let io = fake(dir.path(), 1);
            io.tui_exit_at_sleep.set(at);
            if at == 2 {
                io.visible_from.set(2);
            }
            assert_eq!(
                restart_and_notify(&io, &watch),
                NoticeResult::Left,
                "at={at}"
            );
            assert_eq!(
                (io.told.get(), io.entered.get()),
                (told, entered),
                "at={at}"
            );
        }
    }

    /// 확인한 새 세대에 안내와 별도 Enter를 한 번씩 보낸다.
    #[test]
    fn the_notice_goes_to_the_verified_generation() {
        let (dir, watch) = setup();
        let io = fake(dir.path(), 1);
        assert_eq!(restart_and_notify(&io, &watch), NoticeResult::Delivered);
        assert_eq!((io.told.get(), io.entered.get()), (1, 1));
    }

    /// 재개를 확인한 직후(TUI 준비 대기 중), 첫 안내 확인 대기 중, 별도 Enter 대기 중에 사용자가
    /// 새 세대를 열면 그 세대에 안내나 Enter를 보내지 않는다. sleep 순서는 grace, verify, retry, ...,
    /// submit이다.
    #[test]
    fn a_session_opened_after_the_restart_gets_no_notice() {
        // (새 세대가 열리는 sleep 번호, 안내가 보이는 tell 번호, 기대 tell 수, 기대 Enter 수)
        for (at, visible_from, told, entered) in [(1, 1, 0, 0), (2, 2, 1, 0), (3, 1, 1, 0)] {
            let (dir, watch) = setup();
            let io = fake(dir.path(), 1);
            io.visible_from.set(visible_from);
            io.user_at_sleep.set(at);
            assert_eq!(
                restart_and_notify(&io, &watch),
                NoticeResult::Left,
                "at={at} visible_from={visible_from}"
            );
            assert_eq!(
                (io.told.get(), io.entered.get()),
                (told, entered),
                "at={at} visible_from={visible_from}"
            );
        }
    }

    /// helper 세션이 아니거나 이미 끝난 세대는 이 경로를 쓰지 않는다.
    #[test]
    fn only_a_running_helper_session_is_watched() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(HelperWatch::find(dir.path(), 522), None);
        register(dir.path(), OLD);
        assert!(HelperWatch::find(dir.path(), 523).is_none());
        helper(dir.path(), OLD, HelperPhase::Exited);
        assert_eq!(HelperWatch::find(dir.path(), 522), None);
    }
}
