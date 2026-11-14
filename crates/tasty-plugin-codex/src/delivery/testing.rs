//! 원장·세션 시험의 공용 준비물. 임시 폴더를 원장으로 쓰고 송신 작업은 실행하지 않고 기록한다.

use std::cell::RefCell;

use tasty_plugin_sdk::i18n::Translator;

use super::Ctx;
use super::send::{Dispatch, Job};
use crate::caller::CallerContext;

#[derive(Default)]
pub(crate) struct Recorder {
    pub jobs: RefCell<Vec<Job>>,
    /// 예약한 재시도(thread, 간격).
    pub wakes: RefCell<Vec<(String, std::time::Duration)>>,
    /// 턴 상태 확인 작업과 예약한 재확인 간격.
    pub turn_jobs: RefCell<Vec<super::turns::TurnJob>>,
    /// 턴 스케줄러에 요청한 깨우기 시각(ms).
    pub turn_wakes: RefCell<Vec<u64>>,
    /// `attach.child_notice` 로 맡긴 (surface, key, payload).
    pub notices: RefCell<Vec<(u32, String, serde_json::Value)>>,
    /// `attach.child_notice` 의 답. 없으면 pending 이다.
    pub notice_status: RefCell<Option<tasty_plugin_agent_common::remote_attach::NoticeStatus>>,
    /// 참이면 `attach.child_notice` 호출이 실패한다(상한 초과 등).
    pub notice_fails: std::cell::Cell<bool>,
}

impl Dispatch for Recorder {
    fn dispatch(&self, job: Job) -> Result<(), String> {
        self.jobs.borrow_mut().push(job);
        Ok(())
    }

    fn wake_after(&self, thread: &str, delay: std::time::Duration) -> Result<(), String> {
        self.wakes.borrow_mut().push((thread.to_string(), delay));
        Ok(())
    }

    fn check_turn(&self, job: super::turns::TurnJob) -> Result<(), String> {
        self.turn_jobs.borrow_mut().push(job);
        Ok(())
    }

    fn wake_turns_at(&self, at_ms: u64) -> Result<(), String> {
        self.turn_wakes.borrow_mut().push(at_ms);
        Ok(())
    }

    fn child_notice(
        &self,
        surface: u32,
        key: &str,
        payload: serde_json::Value,
    ) -> Result<tasty_plugin_agent_common::remote_attach::NoticeStatus, String> {
        self.notices
            .borrow_mut()
            .push((surface, key.to_string(), payload));
        if self.notice_fails.get() {
            return Err("attach.child_notice timed out".into());
        }
        Ok(self
            .notice_status
            .borrow()
            .clone()
            .unwrap_or(tasty_plugin_agent_common::remote_attach::NoticeStatus::Pending))
    }
}

pub(crate) const TOKEN: &str = "0123456789abcdef0123456789abcdef";

pub(crate) struct Fixture {
    pub dir: tempfile::TempDir,
    pub tr: Translator,
    pub rec: Recorder,
    /// 공유 daemon 조회 결과.
    pub loaded: Result<bool, String>,
    /// 소유 서버가 active로 보고하는 thread. 없으면 서버에 닿지 못한 것으로 본다.
    pub live: RefCell<Option<std::collections::HashSet<String>>>,
    pub pump: super::Pump,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let lang = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Self {
            dir: tempfile::tempdir().unwrap(),
            tr: Translator::load(&lang, "en"),
            rec: Recorder::default(),
            loaded: Ok(false),
            live: RefCell::new(None),
            pump: super::Pump::default(),
        }
    }

    pub(crate) fn root(&self) -> &std::path::Path {
        self.dir.path()
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&Ctx<'_>) -> R) -> R {
        let loaded = |_: &CallerContext| self.loaded.clone();
        let live = |_: &crate::rpc::Endpoint, thread: &str| match &*self.live.borrow() {
            Some(active) => Ok(active.contains(thread)),
            None => Err("owned server unreachable".to_string()),
        };
        let ctx = Ctx {
            root: self.dir.path(),
            dispatch: &self.rec,
            loaded: &loaded,
            live: &live,
            tr: &self.tr,
            token: TOKEN,
            pump: &self.pump,
        };
        f(&ctx)
    }

    /// surface에 새 세대를 열고 세대 값을 돌려준다.
    pub(crate) fn open(&self, surface: u32, tui_args: &[&str]) -> String {
        crate::session::registry::open_session(
            self.root(),
            surface,
            None,
            tui_args.iter().map(|s| s.to_string()).collect(),
        )
        .unwrap()
        .generation
    }

    /// 세대의 helper가 준비됐다고 기록한다.
    /// helper가 서버를 끝냈다고 기록한다.
    pub(crate) fn exited(&self, generation: &str) {
        let dir = crate::session::registry::session_dir(self.root(), generation);
        crate::store::write_json(
            &crate::session::registry::helper_file(&dir),
            &crate::session::registry::HelperState {
                phase: crate::session::registry::HelperPhase::Exited,
                url: None,
                detail: None,
                resumed: None,
                updated_at_ms: 0,
            },
        )
        .unwrap();
    }

    pub(crate) fn ready(&self, generation: &str, url: &str) {
        let dir = crate::session::registry::session_dir(self.root(), generation);
        crate::store::write_json(
            &crate::session::registry::helper_file(&dir),
            &crate::session::registry::HelperState {
                phase: crate::session::registry::HelperPhase::Ready,
                url: Some(url.into()),
                detail: None,
                resumed: None,
                updated_at_ms: 0,
            },
        )
        .unwrap();
    }

    /// 같은 원장으로 다시 시작한 플러그인. 진행 중 송신 기록은 사라진다.
    pub(crate) fn restarted(self) -> Self {
        Self {
            dir: self.dir,
            tr: self.tr,
            rec: Recorder::default(),
            loaded: self.loaded,
            live: self.live,
            pump: super::Pump::default(),
        }
    }

    /// 기다리는 턴 확인 작업에 `answer`로 답한다. 답이 새 작업을 만들면 그것도 처리한다.
    /// 답한 작업 수를 돌려준다.
    pub(crate) fn answer_turns(
        &self,
        answer: impl Fn(&super::turns::TurnJob) -> super::turns::TurnOutcome,
    ) -> usize {
        let mut answered = 0;
        for _ in 0..16 {
            let jobs: Vec<_> = self.rec.turn_jobs.borrow_mut().drain(..).collect();
            if jobs.is_empty() {
                break;
            }
            for job in jobs {
                let params = serde_json::json!({
                    "name": job.name, "candidate": job.candidate_id, "token": TOKEN,
                    "result": answer(&job),
                });
                self.with(|d| super::turns::handle_turn_outcome(d, &params))
                    .unwrap();
                answered += 1;
            }
        }
        answered
    }

    /// 소유 서버가 이 thread들만 active로 보고하게 한다.
    pub(crate) fn set_live(&self, threads: &[&str]) {
        *self.live.borrow_mut() = Some(threads.iter().map(|t| t.to_string()).collect());
    }

    pub(crate) fn take_jobs(&self) -> Vec<Job> {
        std::mem::take(&mut *self.rec.jobs.borrow_mut())
    }
}
