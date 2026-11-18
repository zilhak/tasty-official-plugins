//! 부모가 등록하는 Claude 자식의 idle 체크.
//!
//! 체크 하나는 (대상 surface, 받을 부모 surface) 한 쌍에 하나다. 등록 시각부터 대상의 `terminal.state` 를 감시 회차마다
//! 묻고, `active` 를 볼 때마다 기준 시각을 그때로 옮긴다. 근거(evidence)는 가리지 않는다. 마지막 `active` 뒤
//! [`IDLE_CHECK_WAIT`] 가 지나도록 그 체크의 작업이 끝났다는 확정 완료가 없으면 부모 완료 로그에 idle 줄 하나를 쓰고
//! 체크를 끝낸다.
//!
//! - 확정 완료([`Finish`])가 어느 체크를 끝내는지는 체크의 소유([`Owner`])로 가른다. 로컬 턴 종료는 등록 뒤에 받은 Stop
//!   이어야 하고, tell 이 정한 완료 문턱(활동 순번)이 있으면 완료 알림과 같은 판정([`completion_gate::decide`])을
//!   따른다. 실행 중인 대상에 보낸 tell 은 진행 중이던 앞 턴의 끝으로 끝나지 않는다. 원격 자식의 결과는 그 결과를
//!   낳은 지시(route 토큰)의 발급 시각이 등록 뒤일 때만 체크를 끝낸다.
//! - 같은 부모·대상을 다시 등록하면 새 id 의 체크로 바꾼다. 앞 체크의 진행 중인 판정은 id 가 달라 버린다.
//! - 상태 조회 실패는 비활동으로 세지 않는다. 그 회차에는 줄을 쓰지 않는다.
//! - 줄을 쓰기 전에 meta 에서 그 체크를 먼저 지운다. 지우지 못하면 쓰지 않고 다음 회차에 다시 한다. 그래서 쓴 줄의
//!   등록은 재시작 뒤 되살아나지 않는다. 부모 로그 쓰기에 실패하면 체크와 meta 를 되돌리고 [`DELIVERY_RETRY`] 뒤에
//!   조건을 다시 확인해 쓴다.
//! - 백그라운드 작업 대기(대기 Stop 기록)와 surface 전체 무출력 선언(`claude quiet`)이 있는 동안은 `active` 처럼
//!   기준 시각을 옮긴다. 그 동안의 조용함은 기존 무출력 알림 규칙이 맡는다.
//! - 등록은 대상 surface meta [`IDLE_CHECK_META_KEY`] 에 남긴다. 쓰지 못한 meta 는 다음 회차에 다시 쓴다. 플러그인이
//!   다시 시작되면 남은 등록만 되살리고 그 시각부터 [`IDLE_CHECK_WAIT`] 를 다시 센다. 되살리기 전에 받은 확정 완료는
//!   되살릴 등록에 먼저 적용한다.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tasty_plugin_agent_common::completion_gate::{self, Threshold};
use tasty_plugin_agent_common::host_call::HostCall;

use super::{ErrorScanner, lock_scanner, unix_now_ms};
use crate::notice::NoticeKind;

/// 마지막 `active` 관측 뒤 idle 줄을 쓰기까지 기다리는 시간.
pub(crate) const IDLE_CHECK_WAIT: Duration = Duration::from_secs(120);

/// 부모 로그 쓰기에 실패한 체크를 다시 시도하기까지 기다리는 시간.
pub(crate) const DELIVERY_RETRY: Duration = Duration::from_secs(30);

/// 상태 조회 실패를 경고로 남기는 간격(회차 수). 감시 회차는 약 0.8초다.
const STATE_ERROR_WARN_EVERY: u32 = 75;
/// meta 쓰기 실패를 경고로 남기는 간격(대상별 연속 실패 수). 실패한 meta 는 회차마다 다시 쓴다.
const META_ERROR_WARN_EVERY: u32 = 75;

/// 등록한 체크를 남기는 대상 surface meta. 재시작 뒤 복원에 쓴다.
pub(crate) const IDLE_CHECK_META_KEY: &str = "claude-idle-checks";

/// 완료 이력을 두는 시간. 이력은 보내기와 등록 사이, 시작과 복원 사이에 온 완료를 가리는 데만 쓴다. 아래 세 상한은
/// 그 두 구간을 지나는 중인 대상(보호 대상)에는 쓰지 않는다([`IdleChecks::protects`]).
const HISTORY_KEPT: Duration = Duration::from_secs(5 * 60);
/// 완료 이력을 두는 대상 수 상한. 넘으면 가장 오래된 대상부터 지운다.
pub(crate) const HISTORY_TARGETS_MAX: usize = 256;
/// 대상 하나에 두는 완료 이력 수.
const HISTORY_PER_TARGET: usize = 8;
/// idle 줄을 쓴 기록 상한. 오류 없는 정지 알림과 겹치는지 가리는 데 쓴다.
const DELIVERED_MAX: usize = 1024;

/// 체크를 건 경로. 응답·meta·로그에 적는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// `claude idle-check` 로 따로 등록했다.
    Manual,
    Tell,
    Spawn,
    Respawn,
}

impl Origin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "idle-check",
            Self::Tell => "tell",
            Self::Spawn => "spawn",
            Self::Respawn => "respawn",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "tell" => Self::Tell,
            "spawn" => Self::Spawn,
            "respawn" => Self::Respawn,
            _ => Self::Manual,
        }
    }
}

/// 한 시점. 로컬 완료는 [`Instant`] 로, 원격 지시의 발급 시각은 Unix ms 로 비교한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct At {
    pub instant: Instant,
    pub ms: u64,
}

impl At {
    pub(crate) fn now() -> Self {
        Self {
            instant: Instant::now(),
            ms: unix_now_ms(),
        }
    }
}

/// 체크가 기다리는 작업. 어떤 확정 완료가 이 체크를 끝내는지 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Owner {
    /// 등록 시각. tell 은 보내기 전에 잰다.
    pub at: At,
    /// tell 이 보내기 전에 정한 완료 문턱. 없으면 등록 뒤의 턴 종료면 끝난다.
    pub threshold: Option<Threshold>,
    /// tell 을 보낼 때 대상이 턴을 진행 중이었다(`active`). 그때만 문턱을 넘은 첫 끝을 진행 중이던 앞 턴의 끝으로
    /// 보고 건너뛴다. 입력 대기(`needs_input`)에 보낸 답은 대기하던 턴을 이어 가므로 그 턴의 끝이 이 체크를 끝낸다.
    pub running: bool,
}

impl Owner {
    pub(crate) fn at(at: At) -> Self {
        Self {
            at,
            threshold: None,
            running: false,
        }
    }
}

/// 확정된 완료.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Finish {
    /// 로컬 Claude 의 턴 종료 확정(Stop·StopFailure·세션 종료). Stop 을 받은 시각과 그때의 활동 순번, 그 턴의
    /// 식별자(Stop 의 `prompt_id`, 없으면 `None`). 같은 턴의 Stop 이 다시 와도 한 턴으로 센다([`Finish::same_turn`]).
    Turn {
        at: Instant,
        seq: u64,
        turn: Option<TurnKey>,
    },
    /// 원격 자식의 기록된 턴 결과. 그 결과를 낳은 지시의 route 토큰 발급 시각(Unix ms). 모르면 `None`.
    Remote { issued_ms: Option<u64> },
    /// 원격 자식이 끝났다.
    RemoteExit,
}

/// 로컬 턴의 식별자. Stop 의 `prompt_id` 를 줄인 값이다. meta 에 남겨 재시작 뒤에도 비교하므로 빌드와 상관없이
/// 같은 값이 나오는 FNV-1a 로 줄인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TurnKey(u64);

impl TurnKey {
    /// `prompt_id` 의 식별자. 비었거나 없으면 `None`.
    pub(crate) fn of(prompt_id: Option<&str>) -> Option<Self> {
        let id = prompt_id.filter(|s| !s.is_empty())?;
        let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        Some(Self(hash))
    }
}

impl Finish {
    /// 로컬 턴 종료. `prompt_id` 는 그 Stop 의 것이다.
    pub(crate) fn turn(at: Instant, seq: u64, prompt_id: Option<&str>) -> Self {
        Self::Turn {
            at,
            seq,
            turn: TurnKey::of(prompt_id),
        }
    }

    /// 같은 턴의 끝인가. 둘 다 턴 식별자가 있으면 그것으로, 없으면 활동 순번으로 가른다. 받은 시각은 같은 Stop 을
    /// 다시 받으면 달라지므로 쓰지 않는다. 순번이 같으면 같은 턴으로 본다 — 실행 중에 줄에 넣은 메시지의 다음 턴은
    /// 순번을 더 올리지 못할 수 있어 그 끝도 같은 턴으로 보지만, 거짓 해제 대신 늦은 idle 줄 쪽을 택한다.
    fn same_turn(&self, other: &Finish) -> bool {
        match (self, other) {
            (
                Finish::Turn {
                    seq: sa, turn: ta, ..
                },
                Finish::Turn {
                    seq: sb, turn: tb, ..
                },
            ) => match (ta, tb) {
                (Some(x), Some(y)) => x == y,
                _ => sa == sb,
            },
            _ => false,
        }
    }
}

/// 확정 완료가 체크 하나에 주는 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    End,
    Keep,
    /// 진행 중이던 앞 턴의 끝이다. 이 문턱부터 세는 다음 턴의 끝을 기다린다.
    Advance(Threshold),
}

/// `finish` 가 `owner` 의 체크에 주는 결과. `before_restore` 는 이전 프로세스의 등록을 되살리기 전에 받은 완료라
/// 등록 시각과 비교하지 않는다(등록은 그 전 프로세스에서 했다).
fn effect(owner: &Owner, finish: Finish, before_restore: bool) -> Effect {
    match finish {
        Finish::Turn { at, seq, .. } => {
            if !before_restore && at < owner.at.instant {
                return Effect::Keep;
            }
            let Some(threshold) = owner.threshold else {
                return Effect::End;
            };
            // 완료 알림과 같은 판정(`completion_gate::decide`)으로 이 끝이 문턱을 넘었는지 본다. 보낼 때 실행 중이던
            // 대상이면 두 번 세는 문턱의 첫 끝은 진행 중이던 앞 턴의 끝이므로 체크를 남기고, 그 뒤의 다음 턴 끝을
            // 기다린다. 입력 대기에 보낸 답은 첫 끝에서 끝난다.
            let tail = completion_gate::Tail {
                event: completion_gate::NoticeEvent::Idle,
                done_from: threshold.done_from,
                arm: 0,
                twice: threshold.twice,
            };
            let verdict = completion_gate::decide(tail, Some("idle"), seq, false);
            match (verdict.decision, verdict.next) {
                // 다음 문턱은 이 끝의 순번이다(`decide` 의 `seq + 1` 이 아니다). Claude Code 는 실행 중에 받은 메시지의
                // prompt-submit 을 줄에 넣는 즉시 보내므로, 앞 턴의 Stop 이 이미 그 순번을 보고 다음 턴의 Stop 은
                // 순번을 더 올리지 못할 수 있다. 앞 턴의 끝 뒤에 오는 확정 완료는 다음 턴의 끝이다.
                (completion_gate::Decision::Done, completion_gate::Next::Keep(_))
                    if owner.running =>
                {
                    Effect::Advance(Threshold::once(seq))
                }
                (completion_gate::Decision::Done, _) => Effect::End,
                _ => Effect::Keep,
            }
        }
        Finish::Remote {
            issued_ms: Some(issued),
        } if issued >= owner.at.ms => Effect::End,
        Finish::Remote { .. } => Effect::Keep,
        Finish::RemoteExit => Effect::End,
    }
}

struct Check {
    id: u64,
    origin: Origin,
    owner: Owner,
    /// 진행 중이던 앞 턴의 끝으로 보고 건너뛴 첫 끝. 같은 턴의 끝이 다시 와도 다음 턴의 끝으로 세지 않는다.
    skipped: Option<Finish>,
    /// 마지막으로 `active` 를 본 시각(처음에는 등록 시각).
    last_active: Instant,
    /// 마지막으로 읽은 상태. 줄에 적는다.
    last_state: Option<String>,
    /// 부모 로그 쓰기 실패 뒤 다시 시도할 수 있는 시각.
    retry_at: Option<Instant>,
    delivery_failures: u32,
    state_errors: u32,
}

impl Check {
    fn new(id: u64, origin: Origin, owner: Owner, now: Instant) -> Self {
        Self {
            id,
            origin,
            owner,
            skipped: None,
            last_active: now,
            last_state: None,
            retry_at: None,
            delivery_failures: 0,
            state_errors: 0,
        }
    }

    /// 완료를 적용한다. 체크가 끝났으면 `true`.
    fn apply(&mut self, finish: Finish, before_restore: bool) -> bool {
        if self.skipped.is_some_and(|s| s.same_turn(&finish)) {
            // 건너뛴 앞 턴의 끝을 다시 받았다(같은 Stop 의 재전달). 다음 턴의 끝이 아니다.
            return false;
        }
        match effect(&self.owner, finish, before_restore) {
            Effect::End => true,
            Effect::Keep => false,
            Effect::Advance(threshold) => {
                self.owner.threshold = Some(threshold);
                self.skipped = Some(finish);
                false
            }
        }
    }
}

/// idle 체크 표. [`ErrorScanner`] 잠금 아래에 둔다.
#[derive(Default)]
pub(crate) struct IdleChecks {
    checks: HashMap<(u32, u32), Check>,
    /// 대상별 최근 확정 완료(받은 시각, 완료). 보내기와 등록 사이, 시작과 복원 사이에 온 완료를 새 등록에 적용한다.
    history: HashMap<u32, VecDeque<(Instant, Finish)>>,
    /// 쓰지 못한 meta 의 대상. 다음 회차에 다시 쓴다.
    unsaved: HashSet<u32>,
    /// 대상별 연속 meta 쓰기 실패 수. 경고를 [`META_ERROR_WARN_EVERY`] 번마다 한 줄로 줄인다.
    meta_errors: HashMap<u32, u32>,
    /// idle 줄을 쓴 (대상, 부모)와 그 시각.
    delivered: HashMap<(u32, u32), Instant>,
    /// 플러그인 프로세스가 시작한 시각. 되살린 체크의 등록 시각으로 쓴다. 그래서 시작 뒤에 받은 완료는 되살리기
    /// 전에 받았어도(게이트 판정을 기다리다 되살린 뒤 확정된 Stop 등) 되살린 체크를 끝낸다.
    started: Option<Instant>,
    /// 시작 복원을 아직 마치지 않았다. 그동안은 모든 대상의 이력을 상한으로 버리지 않는다.
    restoring: bool,
    /// tell 을 보내고 아직 등록하지 않은 대상(진행 중인 수). 그동안은 그 대상의 이력을 상한으로 버리지 않는다.
    expecting: HashMap<u32, u32>,
    next_id: u64,
}

/// 등록 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registered {
    pub id: u64,
    /// 같은 부모·대상의 앞 체크를 바꿨으면 그 id.
    pub replaced: Option<u64>,
    /// 등록 시각 뒤의 완료를 이미 받아 체크를 바로 끝냈다.
    pub completed: bool,
}

/// 감시 회차에 판정할 체크 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Probe {
    pub target: u32,
    pub caller: u32,
    pub id: u64,
}

/// 대상 상태 조회 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observation {
    Active,
    /// `active`·`exited` 가 아닌 상태(idle·needs_input·stale 등).
    NotActive(String),
    /// 프로세스가 끝났다.
    Exited,
    /// surface 가 없다.
    Gone,
    /// 조회에 실패했다. 비활동으로 세지 않는다.
    Error(String),
}

/// 한 회차 판정의 결과. 시험이 읽는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// 체크가 바뀌었거나 끝났다. 이 판정은 버린다.
    Stale,
    Waiting,
    Delivered,
    DeliveryFailed,
    Dropped,
}

/// meta 에 남은 등록 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stored {
    pub caller: u32,
    pub origin: Origin,
    /// 등록 Unix ms.
    pub ms: u64,
    pub threshold: Option<Threshold>,
    /// 보낼 때 대상이 턴을 진행 중이었다([`Owner::running`]).
    pub running: bool,
    /// 앞 턴의 끝으로 보고 건너뛴 첫 끝의 순번과 턴 식별자([`Check::skipped`]).
    pub skipped: Option<(u64, Option<TurnKey>)>,
}

impl IdleChecks {
    fn new_id(&mut self) -> u64 {
        if self.next_id == 0 {
            // 재시작 전 id 와 겹치지 않도록 시작 시각에서 센다.
            self.next_id = unix_now_ms().max(1);
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 체크를 넣는다. 같은 부모·대상의 앞 체크는 바꾼다. 등록 뒤의 완료를 이미 받았으면 그 완료를 적용한다.
    pub(crate) fn register(
        &mut self,
        target: u32,
        caller: u32,
        origin: Origin,
        owner: Owner,
    ) -> Registered {
        let id = self.new_id();
        let mut check = Check::new(id, origin, owner, owner.at.instant);
        let finished = self
            .history
            .get(&target)
            .is_some_and(|h| h.iter().any(|(_, f)| check.apply(*f, false)));
        if finished {
            let replaced = self.checks.remove(&(target, caller)).map(|c| c.id);
            return Registered {
                id,
                replaced,
                completed: true,
            };
        }
        let replaced = self.checks.insert((target, caller), check).map(|c| c.id);
        Registered {
            id,
            replaced,
            completed: false,
        }
    }

    /// 확정된 완료를 이력에 남기고 대상의 체크에 적용한다. 끝난 부모를 돌려준다.
    pub(crate) fn complete(&mut self, target: u32, finish: Finish, now: Instant) -> Vec<u32> {
        self.remember(target, finish, now);
        let mut ended: Vec<u32> = self
            .checks
            .iter_mut()
            .filter(|((t, _), _)| *t == target)
            .filter_map(|((_, caller), c)| c.apply(finish, false).then_some(*caller))
            .collect();
        ended.sort_unstable();
        for caller in &ended {
            self.checks.remove(&(target, *caller));
        }
        ended
    }

    /// 플러그인 시작에 부른다. 이 시각을 되살릴 체크의 등록 시각으로 쓰고, 복원을 마칠 때까지 이력을 지킨다.
    pub(crate) fn begin_restore(&mut self, started: Instant) {
        self.started = Some(started);
        self.restoring = true;
    }

    /// 시작 복원을 마쳤다. 이력은 다음 완료부터 일반 상한을 따른다.
    pub(crate) fn end_restore(&mut self) {
        self.restoring = false;
    }

    /// tell 을 보내기 전에 부른다. 등록(또는 포기)할 때까지 그 대상의 이력을 지킨다.
    pub(crate) fn expect(&mut self, target: u32) {
        *self.expecting.entry(target).or_default() += 1;
    }

    /// [`Self::expect`] 를 푼다.
    pub(crate) fn unexpect(&mut self, target: u32) {
        if let Some(n) = self.expecting.get_mut(&target) {
            *n -= 1;
            if *n == 0 {
                self.expecting.remove(&target);
            }
        }
    }

    /// 이 대상의 이력을 상한으로 버리면 안 되는가. 복원 전이거나 tell 을 보내고 등록하기 전이다.
    fn protects(&self, target: u32) -> bool {
        self.restoring || self.expecting.contains_key(&target)
    }

    /// 완료 이력에 넣는다. 오래된 이력과 상한을 넘는 대상을 함께 지운다. 보호 대상의 이력은 지우지 않는다.
    fn remember(&mut self, target: u32, finish: Finish, now: Instant) {
        let restoring = self.restoring;
        let expecting = &self.expecting;
        self.history.retain(|t, h| {
            if restoring || expecting.contains_key(t) {
                return true;
            }
            h.retain(|(at, _)| now.saturating_duration_since(*at) < HISTORY_KEPT);
            !h.is_empty()
        });
        let protected = self.protects(target);
        let entries = self.history.entry(target).or_default();
        entries.push_back((now, finish));
        while !protected && entries.len() > HISTORY_PER_TARGET {
            entries.pop_front();
        }
        while self.history.len() > HISTORY_TARGETS_MAX {
            let oldest = self
                .history
                .iter()
                .filter(|(t, _)| **t != target && !self.protects(**t))
                .min_by_key(|(_, h)| h.back().map(|(at, _)| *at))
                .map(|(t, _)| *t);
            match oldest {
                Some(t) => {
                    self.history.remove(&t);
                }
                None => break,
            }
        }
    }

    /// 대상의 체크를 모두 지운다(세션 교체·추적 종료). 완료·전달 기록도 지운다.
    pub(crate) fn drop_target(&mut self, target: u32) -> usize {
        self.history.remove(&target);
        self.delivered.retain(|(t, _), _| *t != target);
        self.meta_errors.remove(&target);
        let before = self.checks.len();
        self.checks.retain(|(t, _), _| *t != target);
        before - self.checks.len()
    }

    pub(crate) fn probes(&self) -> Vec<Probe> {
        let mut out: Vec<Probe> = self
            .checks
            .iter()
            .map(|(&(target, caller), c)| Probe {
                target,
                caller,
                id: c.id,
            })
            .collect();
        out.sort_by_key(|p| (p.target, p.caller));
        out
    }

    fn current(&mut self, probe: Probe) -> Option<&mut Check> {
        self.checks
            .get_mut(&(probe.target, probe.caller))
            .filter(|c| c.id == probe.id)
    }

    /// 부모가 대상에 체크를 걸어 두었는가.
    pub(crate) fn has_check(&self, target: u32, caller: u32) -> bool {
        self.checks.contains_key(&(target, caller))
    }

    /// 대상에 걸린 체크가 있는가.
    pub(crate) fn has_any(&self, target: u32) -> bool {
        self.checks.keys().any(|(t, _)| *t == target)
    }

    /// 부모가 `since` 뒤에 이 대상의 idle 줄을 받았는가.
    pub(crate) fn delivered_since(&self, target: u32, caller: u32, since: Instant) -> bool {
        self.delivered
            .get(&(target, caller))
            .is_some_and(|at| *at >= since)
    }

    fn record_delivered(&mut self, target: u32, caller: u32, now: Instant) {
        self.delivered.insert((target, caller), now);
        while self.delivered.len() > DELIVERED_MAX {
            let Some(oldest) = self
                .delivered
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(k, _)| *k)
            else {
                break;
            };
            self.delivered.remove(&oldest);
        }
    }

    /// 시험용 — 대상의 체크 (부모, id) 목록.
    #[cfg(test)]
    pub(crate) fn of_target(&self, target: u32) -> Vec<(u32, u64)> {
        let mut out: Vec<(u32, u64)> = self
            .checks
            .iter()
            .filter(|((t, _), _)| *t == target)
            .map(|((_, caller), c)| (*caller, c.id))
            .collect();
        out.sort_unstable();
        out
    }

    /// 시험용 — 완료 이력을 둔 대상 수.
    #[cfg(test)]
    pub(crate) fn history_targets(&self) -> usize {
        self.history.len()
    }

    /// meta 에 남길 값. `skip` 부모의 체크는 뺀다. 남는 체크가 없으면 `None`.
    fn meta_value(&self, target: u32, skip: Option<u32>) -> Option<String> {
        let mut checks: Vec<Value> = self
            .checks
            .iter()
            .filter(|((t, caller), _)| *t == target && Some(*caller) != skip)
            .map(|((_, caller), c)| {
                let mut v = json!({
                    "caller": caller,
                    "id": c.id,
                    "origin": c.origin.as_str(),
                    "registered_ms": c.owner.at.ms,
                });
                if let Some(th) = c.owner.threshold {
                    v["done_from"] = json!(th.done_from);
                    v["twice"] = json!(th.twice);
                    if c.owner.running {
                        v["running"] = json!(true);
                    }
                    if let Some(Finish::Turn { seq, turn, .. }) = c.skipped {
                        v["skipped_seq"] = json!(seq);
                        if let Some(TurnKey(key)) = turn {
                            v["skipped_turn"] = json!(key);
                        }
                    }
                }
                v
            })
            .collect();
        if checks.is_empty() {
            return None;
        }
        checks.sort_by_key(|v| v["caller"].as_u64());
        Some(json!({ "checks": checks }).to_string())
    }
}

/// meta 에 남은 등록. 읽을 수 없는 항목은 건너뛴다.
pub(crate) fn parse_meta(raw: &str) -> Vec<Stored> {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        tracing::warn!("claude idle check meta is not JSON: {raw}");
        return Vec::new();
    };
    v.get("checks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let caller = u32::try_from(c.get("caller")?.as_u64()?).ok()?;
                    let origin = Origin::parse(c.get("origin").and_then(Value::as_str)?);
                    let ms = c.get("registered_ms").and_then(Value::as_u64).unwrap_or(0);
                    let threshold =
                        c.get("done_from")
                            .and_then(Value::as_u64)
                            .map(|done_from| Threshold {
                                done_from,
                                twice: c.get("twice").and_then(Value::as_bool).unwrap_or(false),
                            });
                    let running = c.get("running").and_then(Value::as_bool).unwrap_or(false);
                    let skipped = c.get("skipped_seq").and_then(Value::as_u64).map(|seq| {
                        let turn = c.get("skipped_turn").and_then(Value::as_u64).map(TurnKey);
                        (seq, turn)
                    });
                    Some(Stored {
                        caller,
                        origin,
                        ms,
                        threshold,
                        running,
                        skipped,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 대상의 지금 상태를 읽는다. 조회에 실패하면 surface 가 남았는지 확인해 없는 surface 와 조회 오류를 가른다.
pub(crate) fn observe<H: HostCall>(host: &H, target: u32) -> Observation {
    match host.call("terminal.state", json!({ "surface": target })) {
        Ok(reply) => match reply.get("state").and_then(Value::as_str) {
            Some(s) if s.eq_ignore_ascii_case("active") => Observation::Active,
            Some(s) if s.eq_ignore_ascii_case("exited") => Observation::Exited,
            Some(s) => Observation::NotActive(s.to_ascii_lowercase()),
            None => Observation::Error(format!("terminal.state reply without state: {reply}")),
        },
        Err(e) => match host.call("surface.locate", json!({ "surface_id": target })) {
            Ok(r) if r.get("exists").and_then(Value::as_bool) == Some(false) => Observation::Gone,
            _ => Observation::Error(e.to_string()),
        },
    }
}

/// 부모 로그에 쓸 idle 줄. `template` 은 `claude.notify.idle_check_message` 다.
pub(crate) fn idle_line(template: &str, target: u32, quiet: Duration, state: &str) -> String {
    tasty_plugin_sdk::i18n::fill_args(
        template,
        &[&target.to_string(), &quiet.as_secs().to_string(), state],
    )
}

/// 기한이 지난 체크의 처리.
enum Due {
    Step(Step),
    Deliver { quiet: Duration, state: String },
}

impl ErrorScanner {
    /// 체크를 등록하고 meta 에 남긴다.
    pub(crate) fn idle_register<H: HostCall>(
        &mut self,
        host: &H,
        target: u32,
        caller: u32,
        origin: Origin,
        owner: Owner,
    ) -> Registered {
        let registered = self.idle.register(target, caller, origin, owner);
        tracing::info!(
            "claude idle check s{target}→s{caller}: registered #{} ({}){}{}",
            registered.id,
            origin.as_str(),
            registered
                .replaced
                .map(|id| format!(", replacing #{id}"))
                .unwrap_or_default(),
            if registered.completed {
                ", already finished"
            } else {
                ""
            }
        );
        self.save_idle(host, target);
        registered
    }

    /// 확정된 완료를 적용한다. 끝난 체크가 있으면 meta 를 다시 쓴다.
    pub(crate) fn idle_complete<H: HostCall>(&mut self, host: &H, target: u32, finish: Finish) {
        let ended = self.idle.complete(target, finish, Instant::now());
        if !ended.is_empty() {
            tracing::info!(
                "claude idle check s{target}: finished turn confirmed — ended the check(s) of parent(s) {ended:?}"
            );
            self.save_idle(host, target);
        } else if self.idle.has_any(target) {
            // 앞 턴의 끝으로 문턱만 옮겼을 수 있다. 재시작 뒤에도 그 문턱으로 판정하도록 남긴다.
            self.save_idle(host, target);
        }
    }

    /// 대상의 체크를 모두 지운다(kill·release·respawn·세션 교체).
    pub(crate) fn idle_drop<H: HostCall>(&mut self, host: &H, target: u32) {
        if self.idle.drop_target(target) > 0 {
            tracing::info!("claude idle check s{target}: target ended — check(s) dropped");
            self.save_idle(host, target);
        }
    }

    /// meta 를 쓴다. 실패하면 `false`.
    fn write_idle_meta<H: HostCall>(
        &mut self,
        host: &H,
        target: u32,
        value: Option<String>,
    ) -> bool {
        let result = match value {
            Some(value) => host.call(
                "surface.meta.set",
                json!({ "surface_id": target, "key": IDLE_CHECK_META_KEY, "value": value }),
            ),
            None => host.call(
                "surface.meta.unset",
                json!({ "surface_id": target, "key": IDLE_CHECK_META_KEY }),
            ),
        };
        match result {
            Ok(_) => {
                if let Some(failures) = self.idle.meta_errors.remove(&target) {
                    tracing::info!(
                        "claude idle check s{target}: {IDLE_CHECK_META_KEY} written after {failures} failed attempt(s)"
                    );
                }
                true
            }
            Err(e) => {
                let failures = self.idle.meta_errors.entry(target).or_default();
                if failures.is_multiple_of(META_ERROR_WARN_EVERY) {
                    tracing::warn!(
                        "claude idle check s{target}: writing {IDLE_CHECK_META_KEY} failed ({} time(s)), retrying every round: {e}",
                        *failures + 1
                    );
                }
                *failures += 1;
                false
            }
        }
    }

    /// 시험용 — 대상의 연속 meta 쓰기 실패 수.
    #[cfg(test)]
    pub(crate) fn idle_meta_errors(&self, target: u32) -> u32 {
        self.idle.meta_errors.get(&target).copied().unwrap_or(0)
    }

    /// 대상의 체크를 meta 에 남긴다. 실패하면 다음 회차에 다시 쓴다.
    fn save_idle<H: HostCall>(&mut self, host: &H, target: u32) {
        let value = self.idle.meta_value(target, None);
        if self.write_idle_meta(host, target, value) {
            self.idle.unsaved.remove(&target);
        } else {
            self.idle.unsaved.insert(target);
        }
    }

    /// 쓰지 못한 meta 를 다시 쓴다.
    pub(crate) fn idle_retry_saves<H: HostCall>(&mut self, host: &H) {
        let mut targets: Vec<u32> = self.idle.unsaved.iter().copied().collect();
        targets.sort_unstable();
        for target in targets {
            self.save_idle(host, target);
        }
    }

    /// 재시작 전 meta 의 등록을 되살린다. 되살리기 전에 받은 확정 완료를 먼저 적용해 끝난 등록은 버리고 meta 에서도
    /// 지운다. 이미 있는 체크는 둔다. 되살린 수를 돌려준다.
    pub(crate) fn idle_restore<H: HostCall>(
        &mut self,
        host: &H,
        target: u32,
        now: Instant,
    ) -> usize {
        let Some(raw) = crate::stop_pairing::meta_value(host, target, IDLE_CHECK_META_KEY) else {
            return 0;
        };
        let history: Vec<Finish> = self
            .idle
            .history
            .get(&target)
            .map(|h| h.iter().map(|(_, f)| *f).collect())
            .unwrap_or_default();
        let (mut restored, mut ended) = (0, 0);
        for stored in parse_meta(&raw) {
            if self.idle.checks.contains_key(&(target, stored.caller)) {
                continue;
            }
            let owner = Owner {
                at: At {
                    instant: self.idle.started.unwrap_or(now),
                    ms: stored.ms,
                },
                threshold: stored.threshold,
                running: stored.running,
            };
            let id = self.idle.new_id();
            let mut check = Check::new(id, stored.origin, owner, now);
            // 건너뛴 앞 턴의 끝을 되살린다. 재시작 뒤 같은 턴의 Stop 이 다시 와도 다음 턴의 끝으로 세지 않는다. 받은
            // 시각은 이전 프로세스의 것이라 남기지 않는다.
            check.skipped = stored
                .skipped
                .map(|(seq, turn)| Finish::Turn { at: now, seq, turn });
            if history.iter().any(|f| check.apply(*f, true)) {
                ended += 1;
                continue;
            }
            self.idle.checks.insert((target, stored.caller), check);
            restored += 1;
        }
        if restored > 0 {
            tracing::info!(
                "claude idle check s{target}: restored {restored} check(s) — counting {}s again from now",
                IDLE_CHECK_WAIT.as_secs()
            );
        }
        if ended > 0 {
            tracing::info!(
                "claude idle check s{target}: {ended} stored check(s) ended by a finished turn before the restore"
            );
        }
        if restored > 0 || ended > 0 {
            self.save_idle(host, target);
        }
        restored
    }

    /// 대상이 기다림을 선언했는가. 백그라운드 작업 대기나 surface 전체 무출력 선언이 있으면 시간을 세지 않는다.
    fn idle_paused<H: HostCall>(&mut self, host: &H, target: u32, now_ms: u64) -> bool {
        if self.background_wait.contains_key(&target) {
            return true;
        }
        if !self.enabled.contains_key(&target) && !self.quiet.contains_key(&target) {
            // 오류 감시 대상이 아니면(원격 mirror 등) 선언도 없다.
            return false;
        }
        self.quiet_state(host, target)
            .decls
            .iter()
            .any(|d| d.task.is_none() && d.until_ms.is_none_or(|u| now_ms < u))
    }

    /// 체크 하나의 한 회차. 조회 결과를 반영하고, 기한이 지났으면 부모 로그에 쓴다.
    pub(crate) fn idle_step<H: HostCall>(
        &mut self,
        host: &H,
        probe: Probe,
        observation: Observation,
        now: Instant,
        template: &str,
    ) -> Step {
        let paused = matches!(observation, Observation::NotActive(_))
            && self.idle_paused(host, probe.target, unix_now_ms());
        match self.idle_due(probe, observation, paused, now) {
            Due::Step(Step::Dropped) => {
                self.idle.checks.remove(&(probe.target, probe.caller));
                self.save_idle(host, probe.target);
                Step::Dropped
            }
            Due::Step(step) => step,
            Due::Deliver { quiet, state } => {
                self.idle_deliver(host, probe, quiet, &state, now, template)
            }
        }
    }

    /// 조회 결과를 체크에 반영하고 할 일을 정한다.
    fn idle_due(
        &mut self,
        probe: Probe,
        observation: Observation,
        paused: bool,
        now: Instant,
    ) -> Due {
        let Some(check) = self.idle.current(probe) else {
            return Due::Step(Step::Stale);
        };
        let state = match observation {
            Observation::Active => {
                check.last_active = now;
                check.last_state = Some("active".into());
                check.state_errors = 0;
                return Due::Step(Step::Waiting);
            }
            Observation::Exited | Observation::Gone => {
                tracing::info!(
                    "claude idle check s{}→s{}: target {} — check #{} dropped",
                    probe.target,
                    probe.caller,
                    if observation == Observation::Gone {
                        "surface is gone"
                    } else {
                        "exited"
                    },
                    probe.id
                );
                return Due::Step(Step::Dropped);
            }
            Observation::Error(e) => {
                if check.state_errors % STATE_ERROR_WARN_EVERY == 0 {
                    tracing::warn!(
                        "claude idle check s{}→s{}: reading the state failed ({} time(s)) — not counted as inactivity: {e}",
                        probe.target,
                        probe.caller,
                        check.state_errors + 1
                    );
                }
                check.state_errors += 1;
                return Due::Step(Step::Waiting);
            }
            Observation::NotActive(state) => state,
        };
        check.state_errors = 0;
        check.last_state = Some(state.clone());
        if paused {
            check.last_active = now;
            return Due::Step(Step::Waiting);
        }
        let quiet = now.saturating_duration_since(check.last_active);
        if quiet < IDLE_CHECK_WAIT || check.retry_at.is_some_and(|r| now < r) {
            return Due::Step(Step::Waiting);
        }
        Due::Deliver { quiet, state }
    }

    /// 기한이 지난 체크의 줄을 쓴다. meta 에서 먼저 지워야 쓴다. 쓰기에 실패하면 meta 를 되돌리고 나중에 다시 쓴다.
    fn idle_deliver<H: HostCall>(
        &mut self,
        host: &H,
        probe: Probe,
        quiet: Duration,
        state: &str,
        now: Instant,
        template: &str,
    ) -> Step {
        let rest = self.idle.meta_value(probe.target, Some(probe.caller));
        if !self.write_idle_meta(host, probe.target, rest) {
            // 지우지 못한 채 쓰면 재시작 뒤 같은 등록이 다시 알린다. 다음 회차에 다시 한다.
            return Step::DeliveryFailed;
        }
        let line = idle_line(template, probe.target, quiet, state);
        match crate::notice::append(probe.caller, NoticeKind::IdleCheck, &line) {
            Ok(()) => {
                tracing::info!(
                    "claude idle check s{}→s{}: no active state for {}s (state {state}) — notified, check #{} ended",
                    probe.target,
                    probe.caller,
                    quiet.as_secs(),
                    probe.id
                );
                self.idle.checks.remove(&(probe.target, probe.caller));
                self.idle.unsaved.remove(&probe.target);
                self.idle.record_delivered(probe.target, probe.caller, now);
                Step::Delivered
            }
            Err(e) => {
                if let Some(check) = self.idle.current(probe) {
                    check.delivery_failures += 1;
                    check.retry_at = Some(now + DELIVERY_RETRY);
                    tracing::warn!(
                        "claude idle check s{}→s{}: writing the idle line to the parent log failed ({} time(s)), retrying in {}s while the target stays inactive: {e}",
                        probe.target,
                        probe.caller,
                        check.delivery_failures,
                        DELIVERY_RETRY.as_secs()
                    );
                }
                // 체크가 남았으므로 meta 에도 다시 남긴다.
                self.save_idle(host, probe.target);
                Step::DeliveryFailed
            }
        }
    }
}

/// 감시 회차 하나. 쓰지 못한 meta 를 다시 쓰고, 체크마다 잠금을 풀고 상태를 물은 뒤 다시 잠가 반영한다.
pub(crate) fn run_idle_checks<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    template: &str,
    now: Instant,
) -> Vec<(Probe, Step)> {
    let probes = {
        let mut guard = lock_scanner(scanner);
        guard.idle_retry_saves(host);
        guard.idle.probes()
    };
    probes
        .into_iter()
        .map(|probe| {
            let observation = observe(host, probe.target);
            let step = lock_scanner(scanner).idle_step(host, probe, observation, now, template);
            (probe, step)
        })
        .collect()
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod tests;
