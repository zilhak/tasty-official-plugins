//! Codex 부모가 spawn·tell 한 Claude 자식의 확정된 턴 결과를 원래 Codex 대화로 돌려보낸다.
//!
//! - 부모 해석과 수신처 준비는 Codex 플러그인의 `codex.parent` 가, 전달 원장과 실제 송신은
//!   `codex.deliver` 가 맡는다. 이 모듈은 자식 → 수신처 경로와 세션별 턴 순번만 저장한다.
//!   자식 목록·index·상태는 호스트 child registry 가 소유한다.
//! - 결과를 만드는 곳은 턴 종료가 확정된 지점 하나다([`crate::task_turn::report_ended`] 와
//!   `stop-failure` 보고). 게이트가 이어 가게 한 Stop 과 백그라운드 작업을 기다리는 Stop 은
//!   그 지점에 오지 않으므로 결과가 되지 않는다.
//! - 같은 턴의 결과는 한 번만 claim 한다. 키는 경로 id 와 세션별 턴 순번이며 prompt_id 에 기대지 않는다.
//!   claim 과 보낼 요청을 같은 파일에 함께 쓰고 송신 스레드([`ReturnPump`])를 깨운다. 훅 처리는
//!   `codex.deliver` 를 기다리지 않는다. 송신 스레드는 실패한 요청을 같은 키로 늘어나는 간격을 두고
//!   다시 보내고, 플러그인이 다시 시작하면 남은 요청부터 보낸다. 요청은 `claude.codex_returns` 로 볼 수 있다.

mod attach;
mod observe;
mod parent;
mod snapshot;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_agent_common::remote_attach;
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

#[cfg(test)]
pub(crate) use attach::test_log;
pub(crate) use attach::{RouteTo, handle_remote_results};
pub(crate) use parent::*;

const STORE_DIR: &str = "codex-return";
const STORE_FILE: &str = "routes.json";
/// 저장 파일 갱신을 프로세스 사이에서 직렬화하는 잠금 파일. 업그레이드 중 두 플러그인 프로세스가
/// 같은 데이터 폴더를 쓸 수 있다.
const LOCK_FILE: &str = "routes.lock";

/// 부모에게 보내는 결과 상태. 작업 품질 판정이 아니라 턴이 어떻게 끝났는지만 뜻한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReturnStatus {
    /// 턴이 정상적으로 끝났다(Stop 확정). 작업이 맞았다는 뜻은 아니다.
    TurnEnded,
    /// 턴이 API 오류로 끝났다(StopFailure).
    TurnFailed,
    /// 진행 중이던 턴이 확정 전에 세션 종료로 끝났다.
    Exited,
}

impl ReturnStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::TurnEnded => "turn_ended",
            Self::TurnFailed => "turn_failed",
            Self::Exited => "exited",
        }
    }
}

/// 자식 하나의 결과 경로. 세션은 첫 hook 에서 정해지고, 복원으로 surface 가 바뀌면 세션으로 다시 찾는다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    route_id: String,
    child_surface: u32,
    child_session: Option<String>,
    receiver_id: String,
    /// 이 경로에서 시작된 턴 수. prompt-submit 마다 늘어난다. 진행 중 턴에 합쳐진 입력은 한 턴이다.
    turn: u64,
    /// 시작된 턴이 아직 확정되지 않았는가. 세션 종료를 `exited` 로 보낼지 정한다.
    open: bool,
    /// 결과를 claim 한 턴들. 늦게 확정된 앞 턴도 제 턴으로 claim 하므로 마지막 하나가 아니라 모두 기억한다.
    /// 오래된 것은 [`CLAIMED_KEPT`] 개만 남긴다.
    claimed: BTreeSet<u64>,
    /// 세션을 아무 hook 으로나 묶어도 되는가. 이미 실행 중인 자식에게 tell 로 만든 경로만 그렇다.
    /// spawn·respawn 경로는 새 실행의 `session-start` 로만 묶어 이전 실행의 늦은 hook 을 받지 않는다.
    adopt_any: bool,
    /// surface 가 없는 외부 부모의 키. 물리 부모면 없다.
    parent_external: Option<String>,
    /// 묶인 세션이 `/clear`·`/resume` 으로 끝났다(SessionEnd reason `clear`·`resume`). 같은 프로세스가
    /// 이어지므로, 같은 surface 에 오는 다음 SessionStart(source `clear`·`resume`)의 세션이 이 경로를 잇는다.
    handoff: bool,
    /// SessionEnd 보다 먼저 온 `/clear`·`/resume` 의 새 세션 후보들(최근 [`WAITING_KEPT`] 개). 먼저
    /// `prompt-submit` 을 보낸 후보가 경로를 잇고 나머지는 버린다. 늦게 온 예전 시작도 후보가 되지만
    /// 새 프롬프트를 보내지 않으므로 경로를 가져가지 못한다.
    waiting: Vec<String>,
    /// 이 경로에 묶였다가 넘어간 세션들(최근 [`RETIRED_KEPT`] 개). 이 세션의 늦은 `clear` 시작은 받지 않는다.
    retired: Vec<String>,
    /// 묶인 세션의 프로세스가 끝난 시각(유닉스 ms). 복원된 같은 세션이 다시 이을 수 있도록
    /// [`ENDED_ROUTE_GRACE`] 동안 남겨 두고 그 뒤 지운다.
    ended_at_ms: Option<u64>,
    /// 결과를 `codex.deliver` 가 아니라 attach 원격 부모(`attach.child_notice`)로 보낸다. 이때 `receiver_id` 는 비어 있다.
    attach: bool,
}

/// 결과 경로의 한 턴. Stop 이 온 때 정하고 확정·보고가 늦어도 그대로 들고 가 그 턴의 키로 claim 한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnId {
    route_id: String,
    turn: u64,
    /// Stop 이 온 세션과 surface. `/clear` 나 복원으로 경로가 다른 세션·surface 로 넘어간 뒤 claim 해도
    /// 결과에는 그 턴을 실행한 세션과 surface 를 싣는다.
    child_session: Option<String>,
    child_surface: u32,
    /// Stop 이 온 때의 수신처. 확정을 기다리는 동안 다른 부모가 tell 해 경로의 수신처가 바뀌어도 그 턴의
    /// 결과는 그 턴을 지시한 부모에게 간다.
    receiver_id: String,
    /// Stop 이 온 때 결과가 attach 원격 부모로 가는 경로였는가.
    attach: bool,
}

/// 한 자식의 결과를 받을 곳과 그 자식을 관리하는 부모.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub receiver_id: String,
    pub parent_external: Option<String>,
    /// 결과를 attach 원격 부모에게 돌려보낸다. 이때 `receiver_id` 자리에는 client 가 이 지시에 준 route 토큰을 둔다.
    pub attach: bool,
}

impl Binding {
    pub(crate) fn new(resolved: &ResolvedParent) -> Self {
        Self {
            receiver_id: resolved.receiver_id.clone(),
            parent_external: match &resolved.parent {
                TerminalParent::External(key) => Some(key.clone()),
                TerminalParent::Surface(_) => None,
            },
            attach: false,
        }
    }

    /// attach 원격 출처 부모. 결과는 서버 host 를 거쳐 client 의 같은 plugin 이 그 부모의 수신처로 기록한다.
    /// `route` 는 client 가 이 지시에 준 토큰이며, 그 지시로 시작한 턴의 결과에 그대로 실린다.
    pub(crate) fn attach(parent_key: &str, route: &str) -> Self {
        Self {
            receiver_id: route.to_string(),
            parent_external: Some(parent_key.to_string()),
            attach: true,
        }
    }
}

/// 보낼 요청과 송신 상태. 송신이 확인되면 지운다.
#[derive(Debug, Clone, PartialEq)]
struct Outgoing {
    request: Value,
    error: Option<String>,
    /// 실패한 송신 수.
    attempts: u32,
    /// 첫 실패 시각(유닉스 ms). 자동 재시도 기간을 이 시각부터 센다.
    first_failed_ms: Option<u64>,
    /// 다음 자동 송신을 해도 되는 시각(유닉스 ms).
    next_at_ms: u64,
    /// 자동 재시도 기간이 지났다. 목록에는 남고 `codex-returns --retry` 로만 다시 보낸다.
    gave_up: bool,
}

impl Outgoing {
    fn new(request: Value) -> Self {
        Self {
            request,
            error: None,
            attempts: 0,
            first_failed_ms: None,
            next_at_ms: 0,
            gave_up: false,
        }
    }

    fn is_due(&self, now_ms: u64) -> bool {
        !self.gave_up && self.next_at_ms <= now_ms
    }
}

/// 실패한 송신의 다음 시도까지 기다리는 시간. 2초에서 두 배씩 늘고 5분에서 멈춘다.
fn backoff(attempts: u32) -> Duration {
    let secs = 2u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(16));
    Duration::from_secs(secs.min(300))
}

/// 첫 실패부터 이 기간이 지나면 자동 재시도를 멈춘다. 요청은 지우지 않는다.
const AUTO_RETRY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// 한 번에 보내는 요청 수. 남은 요청은 곧바로 다음 차례에 보낸다.
const DELIVER_BATCH: usize = 4;
/// 보낼 것이 없을 때 송신 스레드가 깨어나는 간격. 다른 프로세스가 남긴 요청도 이 간격으로 확인한다.
const PUMP_IDLE_WAIT: Duration = Duration::from_secs(60);
/// 경로마다 기억하는 넘어간 세션 수.
const RETIRED_KEPT: usize = 16;
/// 경로 하나가 기다리는 새 세션 후보 수.
const WAITING_KEPT: usize = 4;
/// 경로마다 기억하는 claim 한 턴 수. 이보다 오래된 턴의 확정은 늦게 와도 claim 하지 않는다.
const CLAIMED_KEPT: u64 = 64;
/// 끝난 세션의 경로를 복원에 대비해 남겨 두는 기간.
const ENDED_ROUTE_GRACE: Duration = Duration::from_secs(24 * 60 * 60);
/// `codex.deliver` 한 번의 응답 상한. 호스트 호출 기본 상한(60초)보다 짧게 둔다.
pub(crate) const DELIVER_CALL_TIMEOUT: Duration = Duration::from_secs(15);

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(ms)
        .unwrap_or(0)
}

#[derive(Debug, Default, Clone, PartialEq)]
struct Book {
    routes: Vec<Route>,
    outbox: Vec<Outgoing>,
    /// client: 원격 자식에게 보낸 spawn·tell 의 route 토큰과 그때의 수신처(Codex 대화나 물리 surface 부모).
    attach_routes: Vec<attach::AttachRoute>,
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(String::from)
}

impl Route {
    /// 경로를 새 세션에 묶는다. 앞 세션은 넘어간 세션으로 기억한다.
    fn bind_session(&mut self, session: &str) {
        if let Some(old) = self.child_session.replace(session.to_string()) {
            self.retired.retain(|s| *s != old && s != session);
            self.retired.push(old);
            if self.retired.len() > RETIRED_KEPT {
                self.retired.remove(0);
            }
        }
        self.handoff = false;
        self.waiting.clear();
    }

    /// 이 경로에 묶였다가 넘어간 세션의 `clear` 시작이다. 그 세션의 시작은 이미 지나갔으므로 늦게 온 훅이다.
    fn is_stale_clear_start(&self, session: &str, detail: Option<&str>) -> bool {
        detail == Some("clear") && self.retired.iter().any(|s| s == session)
    }
}

impl Book {
    fn from_json(v: &Value) -> Option<Self> {
        let routes = v
            .get("routes")?
            .as_array()?
            .iter()
            .map(|r| {
                Some(Route {
                    route_id: r.get("route_id")?.as_str()?.to_string(),
                    child_surface: u32::try_from(r.get("child_surface")?.as_u64()?).ok()?,
                    child_session: opt_str(r, "child_session"),
                    receiver_id: r.get("receiver_id")?.as_str()?.to_string(),
                    turn: r.get("turn")?.as_u64()?,
                    open: r.get("open")?.as_bool()?,
                    claimed: r
                        .get("claimed")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_u64).collect())
                        .or_else(|| {
                            r.get("delivered")
                                .and_then(Value::as_u64)
                                .map(|d| BTreeSet::from([d]))
                        })
                        .unwrap_or_default(),
                    adopt_any: r.get("adopt_any")?.as_bool()?,
                    parent_external: opt_str(r, "parent_external"),
                    handoff: r.get("handoff").and_then(Value::as_bool).unwrap_or(false),
                    // 이전 형식은 후보 하나를 문자열로 저장했다.
                    waiting: match r.get("waiting") {
                        Some(Value::Array(a)) => a
                            .iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect(),
                        _ => opt_str(r, "waiting").into_iter().collect(),
                    },
                    retired: r
                        .get("retired")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default(),
                    ended_at_ms: r.get("ended_at_ms").and_then(Value::as_u64),
                    attach: r.get("attach").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let outbox = v
            .get("outbox")?
            .as_array()?
            .iter()
            .map(|o| {
                Some(Outgoing {
                    request: o.get("request")?.clone(),
                    error: opt_str(o, "error"),
                    attempts: o
                        .get("attempts")
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0),
                    first_failed_ms: o.get("first_failed_ms").and_then(Value::as_u64),
                    next_at_ms: o.get("next_at_ms").and_then(Value::as_u64).unwrap_or(0),
                    gave_up: o.get("gave_up").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let attach_routes = v
            .get("attach_routes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some(attach::AttachRoute {
                            token: r.get("token")?.as_str()?.to_string(),
                            receiver_id: r.get("receiver_id")?.as_str()?.to_string(),
                            parent_surface: r
                                .get("parent_surface")
                                .and_then(Value::as_u64)
                                .and_then(|n| u32::try_from(n).ok()),
                            parent_session: opt_str(r, "parent_session"),
                            issued_at_ms: r.get("issued_at_ms")?.as_u64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            routes,
            outbox,
            attach_routes,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "routes": self.routes.iter().map(|r| json!({
                "route_id": r.route_id,
                "child_surface": r.child_surface,
                "child_session": r.child_session,
                "receiver_id": r.receiver_id,
                "turn": r.turn,
                "open": r.open,
                "claimed": r.claimed,
                "adopt_any": r.adopt_any,
                "parent_external": r.parent_external,
                "handoff": r.handoff,
                "waiting": r.waiting,
                "retired": r.retired,
                "ended_at_ms": r.ended_at_ms,
                "attach": r.attach,
            })).collect::<Vec<_>>(),
            "outbox": self.outbox.iter().map(outgoing_json).collect::<Vec<_>>(),
            "attach_routes": self.attach_routes.iter().map(|r| json!({
                "token": r.token,
                "receiver_id": r.receiver_id,
                "parent_surface": r.parent_surface,
                "parent_session": r.parent_session,
                "issued_at_ms": r.issued_at_ms,
            })).collect::<Vec<_>>(),
        })
    }

    /// 이 세션에 묶인 경로.
    fn by_session(&self, session: &str) -> Option<usize> {
        self.routes
            .iter()
            .position(|r| r.child_session.as_deref() == Some(session))
    }

    /// 이 hook 으로 세션을 묶을 수 있는 같은 surface 의 경로. 같은 surface 에 다른 세션이 이미 묶여 있으면
    /// 그 surface 를 다른 실행이 쓰고 있으므로 찾지 않는다.
    fn adoptable(&self, surface: u32, event: &str) -> Option<usize> {
        self.routes.iter().position(|r| {
            r.child_surface == surface
                && r.child_session.is_none()
                && (r.adopt_any || event == "session-start")
        })
    }

    /// `/clear`·`/resume` 으로 세션을 넘기는 중인 같은 surface 의 경로.
    fn handing_off(&self, surface: u32) -> Option<usize> {
        self.routes
            .iter()
            .position(|r| r.child_surface == surface && r.handoff)
    }

    /// 확정된 턴의 경로. 세션과 surface 가 모두 그 경로의 것이어야 한다. 복원으로 옮겨 간 세션의
    /// 옛 surface 에서 늦게 온 보고는 받지 않는다.
    fn for_turn_end(&self, surface: u32, session: Option<&str>) -> Option<usize> {
        match session {
            Some(s) => self
                .by_session(s)
                .filter(|&i| self.routes[i].child_surface == surface),
            None => self
                .routes
                .iter()
                .position(|r| r.child_surface == surface && r.child_session.is_none()),
        }
    }

    /// 복원을 기다릴 기간이 지난 끝난 경로를 지운다. 보낼 요청은 경로와 따로 남으므로 잃지 않는다.
    fn sweep(&mut self, now_ms: u64) -> bool {
        let before = self.routes.len();
        self.routes.retain(|r| {
            r.ended_at_ms
                .is_none_or(|t| now_ms.saturating_sub(t) < ms(ENDED_ROUTE_GRACE))
        });
        self.routes.len() != before
    }

    /// 이 외부 부모의 자식 surface 들.
    fn children_of(&self, parent_external: &str) -> Vec<u32> {
        self.routes
            .iter()
            .filter(|r| r.parent_external.as_deref() == Some(parent_external))
            .map(|r| r.child_surface)
            .collect()
    }

    /// 같은 surface 에서 아직 끝나지 않은 다른 세션에 묶인 경로. 그 세션의 `/clear`·`/resume` 이 시작한
    /// 새 세션이 SessionEnd 보다 먼저 왔을 때 그 새 세션을 기다리게 한다.
    fn live_on(&self, surface: u32, session: &str) -> Option<usize> {
        self.routes.iter().position(|r| {
            r.child_surface == surface
                && r.ended_at_ms.is_none()
                && r.child_session.as_deref().is_some_and(|s| s != session)
        })
    }
}

fn outgoing_json(o: &Outgoing) -> Value {
    json!({
        "request": o.request,
        "error": o.error,
        "attempts": o.attempts,
        "first_failed_ms": o.first_failed_ms,
        "next_at_ms": o.next_at_ms,
        "gave_up": o.gave_up,
    })
}

/// 요청 하나의 completion_key.
fn key_of(request: &Value) -> Option<&str> {
    request.get("completion_key").and_then(Value::as_str)
}

static ROUTE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 경로 id. 재시작 뒤 다시 만든 경로와 키가 겹치지 않으면 된다(인증 값이 아니다).
fn new_route_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = ROUTE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{:x}-{seq:x}", std::process::id())
}

/// 송신 스레드를 깨우거나 멈추는 신호.
#[derive(Default)]
struct Signal {
    woken: bool,
    stop: bool,
}

/// 한 번의 송신 차례 결과.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PumpRound {
    /// 이번에 `codex.deliver` 를 부른 수.
    pub attempted: usize,
    /// 이번 차례에 다 보내지 못해 곧바로 이어 보낼 요청이 있다.
    pub more: bool,
    /// 다음 자동 송신 시각(유닉스 ms). 기다리는 요청이 없으면 없다.
    pub next_due_ms: Option<u64>,
}

/// 결과 경로와 보낼 요청의 저장소. 훅 워커·Stop 확정 스레드·송신 스레드와 같은 데이터 폴더를 쓰는 다른
/// 플러그인 프로세스가 함께 쓰므로 파일 갱신을 프로세스 안 잠금과 잠금 파일로 직렬화한다. 훅 처리 쪽은 claim 을 파일에 남기고 송신 스레드를 깨우기만 한다.
pub(crate) struct Returns {
    dir: Option<PathBuf>,
    lock: Mutex<()>,
    signal: Mutex<Signal>,
    wake: Condvar,
}

impl Returns {
    pub(crate) fn new(data_dir: Option<&Path>) -> Self {
        Self {
            dir: data_dir.map(|d| d.join(STORE_DIR)),
            lock: Mutex::new(()),
            signal: Mutex::new(Signal::default()),
            wake: Condvar::new(),
        }
    }

    /// 경로를 저장할 수 있는가. Codex 호출은 자식을 만들기 전에 이것을 확인한다.
    pub(crate) fn is_available(&self) -> bool {
        self.dir.is_some()
    }

    fn update<R>(&self, f: impl FnOnce(&mut Book) -> (R, bool)) -> Result<R, String> {
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        // 잠금은 파일 갱신만 직렬화한다. 보호하는 메모리 상태가 없으므로 poison 뒤에도 이어 쓴다.
        let _guard = tasty_utils::poison::recover_mutex(
            self.lock.lock(),
            "claude codex return store",
            &REPORTED,
        );
        let dir = self
            .dir
            .as_deref()
            .ok_or("plugin data directory is unavailable")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let lock_path = dir.join(LOCK_FILE);
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        // 파일이 닫히면 잠금도 풀린다.
        lock.lock()
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        let path = dir.join(STORE_FILE);
        let mut book = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| Book::from_json(&v))
                .ok_or_else(|| format!("{} is not a valid return store", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Book::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let (out, changed) = f(&mut book);
        if changed {
            write_durably(dir, &path, &book.to_json())?;
        }
        Ok(out)
    }

    fn lock_signal(&self) -> std::sync::MutexGuard<'_, Signal> {
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        tasty_utils::poison::recover_mutex(
            self.signal.lock(),
            "claude codex return signal",
            &REPORTED,
        )
    }

    /// 송신 스레드를 깨운다. 송신은 그 스레드가 한다.
    fn wake(&self) {
        self.lock_signal().woken = true;
        self.wake.notify_all();
    }

    /// 송신 스레드를 멈추게 한다.
    fn stop(&self) {
        self.lock_signal().stop = true;
        self.wake.notify_all();
    }

    fn stopping(&self) -> bool {
        self.lock_signal().stop
    }

    /// 깨우거나 멈추라는 신호가 오거나 `timeout` 이 지날 때까지 기다린다. 멈춰야 하면 `false`.
    fn wait(&self, timeout: Duration) -> bool {
        let mut signal = self.lock_signal();
        if !signal.woken && !signal.stop && !timeout.is_zero() {
            static REPORTED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            signal = match self.wake.wait_timeout(signal, timeout) {
                Ok((guard, _)) => guard,
                Err(poisoned) => {
                    tasty_utils::poison::recover_poisoned(
                        poisoned,
                        "claude codex return signal",
                        &REPORTED,
                    )
                    .0
                }
            };
        }
        signal.woken = false;
        !signal.stop
    }

    /// spawn·respawn 이 만든 자식의 경로를 새로 둔다. 같은 surface 의 예전 경로는 버린다.
    /// `open` 은 첫 프롬프트를 실어 실행했는가다.
    pub(crate) fn bind_new_child(
        &self,
        child_surface: u32,
        binding: &Binding,
        open: bool,
    ) -> Result<(), String> {
        self.update(|book| {
            book.sweep(now_ms());
            book.routes.retain(|r| r.child_surface != child_surface);
            book.routes.push(Route {
                route_id: new_route_id(),
                child_surface,
                child_session: None,
                receiver_id: binding.receiver_id.clone(),
                turn: 0,
                open,
                claimed: BTreeSet::new(),
                adopt_any: false,
                parent_external: binding.parent_external.clone(),
                handoff: false,
                waiting: Vec::new(),
                retired: Vec::new(),
                ended_at_ms: None,
                attach: binding.attach,
            });
            ((), true)
        })
    }

    /// tell 의 결과를 이 수신처로 보낸다. 이미 경로가 있으면 수신처만 바꾸고 턴 순번은 이어 간다.
    pub(crate) fn bind_tell(&self, child_surface: u32, binding: &Binding) -> Result<(), String> {
        self.update(|book| {
            let current = book
                .routes
                .iter()
                .rposition(|r| r.child_surface == child_surface);
            match current {
                Some(i) => {
                    let route = &mut book.routes[i];
                    route.receiver_id = binding.receiver_id.clone();
                    route.parent_external = binding.parent_external.clone();
                    route.attach = binding.attach;
                    // 묶인 세션이 끝났거나 넘기다 멈췄으면 지금 그 surface 에서 tell 을 받는 실행은 다른 세션이다.
                    // 다음 hook 의 세션으로 다시 묶는다. 경로 id 와 턴 순번은 이어 가므로 키가 겹치지 않는다.
                    if route.ended_at_ms.take().is_some() || route.handoff {
                        if let Some(old) = route.child_session.take() {
                            route.retired.retain(|s| *s != old);
                            route.retired.push(old);
                        }
                        route.waiting.clear();
                        route.adopt_any = true;
                        route.handoff = false;
                        route.open = false;
                    }
                }
                None => book.routes.push(Route {
                    route_id: new_route_id(),
                    child_surface,
                    child_session: None,
                    receiver_id: binding.receiver_id.clone(),
                    turn: 0,
                    open: false,
                    claimed: BTreeSet::new(),
                    adopt_any: true,
                    parent_external: binding.parent_external.clone(),
                    handoff: false,
                    waiting: Vec::new(),
                    retired: Vec::new(),
                    ended_at_ms: None,
                    attach: binding.attach,
                }),
            }
            ((), true)
        })
    }

    /// 자식의 경로를 지운다. 이미 claim 해 보낼 요청은 경로와 따로 남는다. 지웠으면 `true`.
    pub(crate) fn release_child(&self, child_surface: u32) -> Result<bool, String> {
        if !self.is_available() {
            return Ok(false);
        }
        self.update(|book| {
            let before = book.routes.len();
            book.routes.retain(|r| r.child_surface != child_surface);
            let removed = book.routes.len() != before;
            (removed, removed)
        })
    }

    /// 닫힌 자식의 경로를 지운다. 아직 보내지 못한 결과는 경로와 따로 남는다.
    pub(crate) fn forget_child(&self, child_surface: u32) {
        if let Err(e) = self.release_child(child_surface) {
            tracing::warn!("claude codex return: could not forget s{child_surface}: {e}");
        }
    }

    /// 외부 부모의 요청을 보낼 workspace. 명시값이 먼저다. 그다음 대상 자식이 정해져 있으면 그 자식이
    /// 지금 있는 workspace 를, 아니면 이 부모의 자식들이 지금 한 workspace 에만 있을 때 그것을 쓴다.
    /// 복원으로 옮겨 간 자식도 호스트가 지금 그 surface 를 둔 곳으로 찾는다. 여러 workspace 에 있으면
    /// 어느 창의 자식인지 정할 수 없으므로 거절한다. 사용자 포커스나 마지막으로 쓴 workspace 로 정하지 않는다.
    fn workspace_for<H: HostCall>(
        &self,
        host: &H,
        parent_external: &str,
        explicit: Option<&str>,
        child: Option<u32>,
    ) -> Result<Option<String>, Vec<String>> {
        if let Some(w) = explicit {
            return Ok(Some(w.to_string()));
        }
        if let Some(c) = child
            && let Some(w) = current_workspace(host, c)
        {
            return Ok(Some(w));
        }
        if !self.is_available() {
            return Ok(None);
        }
        let children = match self.update(|book| (book.children_of(parent_external), false)) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("claude codex return: cannot read the stored children: {e}");
                return Ok(None);
            }
        };
        let all: BTreeSet<String> = children
            .into_iter()
            .filter_map(|c| current_workspace(host, c))
            .collect();
        match all.len() {
            0 => Ok(None),
            1 => Ok(all.into_iter().next()),
            _ => Err(all.into_iter().collect()),
        }
    }

    /// 부모와 workspace 를 호스트 요청에 싣는다. 물리 부모는 workspace 를 덧붙이지 않는다.
    /// `child` 는 tell 처럼 대상 자식이 정해진 요청의 자식 surface 다.
    #[allow(clippy::too_many_arguments)] // 이유: 요청 하나의 부모·인자·대상 자식과 호스트를 각각 받는다.
    pub(crate) fn insert_parent<H: HostCall>(
        &self,
        host: &H,
        target: &ParentTarget,
        params: &Value,
        map: &mut Map<String, Value>,
        physical_key: &str,
        child: Option<u32>,
        tr: &Translator,
    ) -> Result<(), IpcMethodError> {
        target.insert(map, physical_key);
        let ParentTarget::Codex(ResolvedParent {
            parent: TerminalParent::External(key),
            ..
        }) = target
        else {
            return Ok(());
        };
        let explicit = params.get("workspace").and_then(Value::as_str);
        match self.workspace_for(host, key, explicit, child) {
            Ok(Some(workspace)) => {
                map.insert("workspace".into(), json!(workspace));
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(candidates) => Err(IpcMethodError::invalid_params(
                &tr.t_fmt("claude.codex.ambiguous_workspace", &candidates.join(", ")),
            )),
        }
    }

    /// hook 을 경로에 반영한다. 경로가 없는 자식(일반 부모)에는 아무것도 하지 않는다.
    /// `detail` 은 session-start 의 `source`, session-end 의 `reason` 이다.
    ///
    /// - 세션이 아직 없는 경로는 그 surface 의 hook 으로 세션을 묶는다([`Book::adoptable`]).
    /// - 묶인 세션의 hook 이 다른 surface 에서 오면 그것이 `session-start` 일 때만(복원된 실행) 옮긴다.
    ///   옛 surface 에서 늦게 온 다른 hook 은 경로를 되돌리지 않는다.
    /// - `/clear`·`/resume` 의 SessionEnd 는 경로를 넘길 준비만 하고, 같은 surface 의 다음
    ///   SessionStart(`clear`·`resume`)가 새 세션으로 잇는다. Claude Code 는 SessionEnd 훅을 짧은 상한까지만
    ///   기다리므로 SessionStart 가 먼저 올 수 있다. 그때는 그 세션을 기다리게 두고, 그 세션의 다음
    ///   `prompt-submit` 이 와야 잇는다. 기다리는 세션이 여럿이면 먼저 새 프롬프트를 보낸 세션이 잇고 나머지
    ///   후보는 버린다. 늦게 온 예전 세션의 시작 뒤에 같은 세션의 늦은 Stop·SessionEnd 가 와도 경로를 가져가지
    ///   못한다. 이 경로에 묶였던 세션의 `clear` 시작은 받지 않는다. 경로가 넘어간 뒤 옛 세션의 hook 은
    ///   어느 경로에도 닿지 않는다.
    ///   `startup` 시작은 잇지 않는다.
    /// - 그 밖의 SessionEnd 는 끝난 시각을 남긴다. 복원된 같은 세션이 오면 다시 이어진다.
    pub(crate) fn observe(
        &self,
        event: &str,
        surface: u32,
        session: Option<&str>,
        detail: Option<&str>,
    ) {
        let (true, Some(session)) = (self.is_available(), session) else {
            return;
        };
        let now = now_ms();
        let result = self.update(|book| {
            let mut changed = event == "session-start" && book.sweep(now);
            let Some(i) = book.route_for_hook(event, surface, session, detail, &mut changed) else {
                return ((), changed);
            };
            let route = &mut book.routes[i];
            if !route.follow_hook(event, surface, session, detail, &mut changed) {
                return ((), changed);
            }
            if route.record_hook(event, detail, now) {
                changed = true;
            }
            if changed {
                let id = book.routes[i].route_id.clone();
                // 같은 surface 에 남은 다른 세션의 경로는 이전 실행의 것이다.
                book.routes.retain(|r| {
                    r.route_id == id || r.child_surface != surface || r.child_session.is_none()
                });
            }
            ((), changed)
        });
        if let Err(e) = result {
            tracing::error!("claude codex return: could not record {event} for s{surface}: {e}");
        }
    }

    /// 이 surface·세션의 경로가 지금 있는 턴. Stop 이 온 때 불러 확정까지 들고 간다. 경로가 없으면 `None`.
    pub(crate) fn turn_of(&self, surface: u32, session: Option<&str>) -> Option<TurnId> {
        if !self.is_available() {
            return None;
        }
        let found = self.update(|book| {
            let id = book.for_turn_end(surface, session).map(|i| TurnId {
                route_id: book.routes[i].route_id.clone(),
                turn: book.routes[i].turn,
                child_session: book.routes[i].child_session.clone(),
                child_surface: book.routes[i].child_surface,
                receiver_id: book.routes[i].receiver_id.clone(),
                attach: book.routes[i].attach,
            });
            (id, false)
        });
        match found {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("claude codex return: cannot read the route of s{surface}: {e}");
                None
            }
        }
    }

    /// 확정된 턴 결과를 claim 하고 보낼 요청을 남긴다. 같은 턴을 이미 claim 했거나 경로가 없으면 `None`.
    /// `turn` 이 있으면 그 경로의 그 턴이다. 없으면 이 surface·세션 경로의 지금 턴이며, 세션 종료의
    /// `exited` 는 그 턴이 열려 있을 때만 claim 한다.
    fn claim(&self, end: &TurnEnd<'_>) -> Result<Option<Value>, String> {
        let TurnEnd {
            surface,
            session,
            status,
            body,
            attempt_id,
            turn: ref turn_id,
        } = *end;
        self.update(|book| {
            let found = match turn_id {
                Some(id) => book
                    .routes
                    .iter()
                    .position(|r| r.route_id == id.route_id)
                    .map(|i| (i, id.turn)),
                None => book
                    .for_turn_end(surface, session)
                    .map(|i| (i, book.routes[i].turn)),
            };
            let Some((i, turn)) = found else {
                return (None, false);
            };
            let route = &mut book.routes[i];
            let too_old = route
                .claimed
                .last()
                .is_some_and(|&last| turn.saturating_add(CLAIMED_KEPT) <= last);
            if route.claimed.contains(&turn)
                || too_old
                || (status == ReturnStatus::Exited
                    && (turn != route.turn || !route.open || route.handoff))
            {
                return (None, false);
            }
            route.claimed.insert(turn);
            while route.claimed.len() as u64 > CLAIMED_KEPT {
                route.claimed.pop_first();
            }
            if turn == route.turn {
                route.open = false;
            }
            // 결과에 싣는 수신처·세션·surface 는 그 턴의 것이다. 경로가 그 뒤 다른 세션이나 수신처로
            // 넘어갔어도 바꾸지 않는다.
            let (receiver_id, child_surface, child_session, attach) = match turn_id {
                Some(id) => (
                    id.receiver_id.clone(),
                    id.child_surface,
                    id.child_session.clone(),
                    id.attach,
                ),
                None => (
                    route.receiver_id.clone(),
                    route.child_surface,
                    route.child_session.clone(),
                    route.attach,
                ),
            };
            let mut request = json!({
                "completion_key": format!("claude:{}:t{turn}", route.route_id),
                "provider": "claude",
                "child_surface": child_surface,
                "child_session": child_session,
                "status": status.as_str(),
                "body": body,
            });
            if let Some(attempt) = attempt_id {
                request["attempt_id"] = json!(attempt);
            }
            if attach {
                request["attach"] = json!(true);
                request[remote_attach::ROUTE_FIELD] = json!(receiver_id);
            } else {
                request["receiver_id"] = json!(receiver_id);
            }
            book.outbox.push(Outgoing::new(request.clone()));
            (Some(request), true)
        })
    }

    /// 확정된 턴 결과를 claim 해 파일에 남기고 송신 스레드를 깨운다. 여기서는 Codex 를 부르지 않는다.
    pub(crate) fn turn_end(&self, end: TurnEnd<'_>) {
        if !self.is_available() {
            return;
        }
        match self.claim(&end) {
            Ok(Some(_)) => self.wake(),
            Ok(None) => {}
            Err(e) => tracing::error!(
                "claude codex return: s{} {} was not claimed: {e}",
                end.surface,
                end.status.as_str()
            ),
        }
    }

    /// 보낼 때가 된 요청을 최대 [`DELIVER_BATCH`] 개 보낸다. 같은 요청은 같은 completion_key 로
    /// 다시 보내며, 받는 쪽이 (receiver_id, completion_key) 로 중복을 거른다. 성공한 것만 지운다.
    pub(crate) fn pump_once<H: HostCall>(&self, host: &H, now: u64) -> PumpRound {
        let due = match self.update(|book| {
            let due: Vec<Value> = book
                .outbox
                .iter()
                .filter(|o| o.is_due(now))
                .map(|o| o.request.clone())
                .collect();
            (due, false)
        }) {
            Ok(due) => due,
            Err(e) => {
                tracing::error!("claude codex return: cannot read pending results: {e}");
                return PumpRound::default();
            }
        };
        let mut round = PumpRound {
            more: due.len() > DELIVER_BATCH,
            ..PumpRound::default()
        };
        for request in due.into_iter().take(DELIVER_BATCH) {
            if self.stopping() {
                round.more = false;
                break;
            }
            if self.deliver_one(host, request, now) {
                round.attempted += 1;
            }
        }
        round.next_due_ms = self.next_due_ms();
        round
    }

    /// 요청 하나를 `codex.deliver` 로 넘기고 결과를 남긴다. completion_key 가 없어 부르지 않았으면 `false`.
    fn deliver_one<H: HostCall>(&self, host: &H, request: Value, now: u64) -> bool {
        let Some(key) = key_of(&request).map(String::from) else {
            return false;
        };
        let outcome = if attach::is_attach(&request) {
            attach::hand_over(host, request)
        } else {
            host.call("codex.deliver", request)
                .map_err(|e| attach::Failure::Retry(e.to_string()))
        };
        match &outcome {
            Ok(response) => {
                tracing::info!("claude codex return: {key} handed to codex ({response})")
            }
            Err(attach::Failure::Retry(message)) => tracing::warn!(
                "claude codex return: {key} was not handed to codex, kept for retry: {message}"
            ),
            Err(attach::Failure::Refused(_)) => {}
        }
        self.settle(&key, outcome.map(|_| ()), now_ms().max(now));
        true
    }

    /// 자동 재시도를 멈추지 않은 요청 중 가장 이른 송신 시각.
    fn next_due_ms(&self) -> Option<u64> {
        self.update(|book| {
            let next = book
                .outbox
                .iter()
                .filter(|o| !o.gave_up)
                .map(|o| o.next_at_ms)
                .min();
            (next, false)
        })
        .unwrap_or(None)
    }

    fn settle(&self, key: &str, outcome: Result<(), attach::Failure>, now: u64) {
        let result = self.update(|book| {
            match &outcome {
                Ok(()) => book.outbox.retain(|o| key_of(&o.request) != Some(key)),
                // 영구 거절은 지우지 않고 사유와 함께 포기한 요청으로 남긴다(`codex-returns` 로 본다).
                Err(attach::Failure::Refused(reason)) => {
                    for o in book
                        .outbox
                        .iter_mut()
                        .filter(|o| key_of(&o.request) == Some(key))
                    {
                        o.error = Some(format!("refused by the attach parent: {reason}"));
                        o.attempts = o.attempts.saturating_add(1);
                        o.first_failed_ms.get_or_insert(now);
                        o.gave_up = true;
                    }
                    tracing::warn!(
                        "claude codex return: {key} was refused by the attach parent; kept as given up: {reason}"
                    );
                }
                Err(attach::Failure::Retry(e)) => {
                    for o in book
                        .outbox
                        .iter_mut()
                        .filter(|o| key_of(&o.request) == Some(key))
                    {
                        o.error = Some(e.clone());
                        o.attempts = o.attempts.saturating_add(1);
                        let first = *o.first_failed_ms.get_or_insert(now);
                        if now.saturating_sub(first) >= ms(AUTO_RETRY_WINDOW) {
                            o.gave_up = true;
                            tracing::warn!(
                                "claude codex return: {key} stopped retrying after {} attempts; use claude codex-returns --retry",
                                o.attempts
                            );
                        } else {
                            o.next_at_ms = now.saturating_add(ms(backoff(o.attempts)));
                        }
                    }
                }
            }
            ((), true)
        });
        if let Err(e) = result {
            tracing::error!("claude codex return: could not record the delivery of {key}: {e}");
        }
    }

    /// 모든 요청을 지금 다시 보낼 차례로 돌리고 송신 스레드를 깨운다. 자동 재시도를 멈춘 요청도
    /// 새 기간으로 다시 시작한다. 돌린 요청 수를 돌려준다.
    fn requeue_all(&self) -> Result<usize, String> {
        let n = self.update(|book| {
            for o in &mut book.outbox {
                o.next_at_ms = 0;
                o.gave_up = false;
                o.first_failed_ms = None;
            }
            let n = book.outbox.len();
            (n, n > 0)
        })?;
        if n > 0 {
            self.wake();
        }
        Ok(n)
    }

    /// 보내지 못한 요청 목록. 본문·수신처·오류·재시도 상태를 그대로 보여 준다.
    pub(crate) fn pending(&self) -> Result<Vec<Value>, String> {
        self.update(|book| (book.outbox.iter().map(outgoing_json).collect(), false))
    }
}

/// 확정된 턴 종료 한 건.
pub(crate) struct TurnEnd<'a> {
    pub surface: u32,
    pub session: Option<&'a str>,
    pub status: ReturnStatus,
    pub body: Option<&'a str>,
    pub attempt_id: Option<&'a str>,
    /// Stop 이 온 때 정한 턴([`Returns::turn_of`]). 세션 종료의 `exited` 는 지금 턴이므로 없다.
    pub turn: Option<TurnId>,
}

/// 결과를 Codex 플러그인에 넘기는 송신 스레드. 시작하면 이전 실행이 남긴 요청부터 보낸다.
/// 훅 워커는 이 스레드를 깨우기만 하므로 `codex.deliver` 가 늦어도 훅 처리가 막히지 않는다.
/// 값이 사라질 때(플러그인 종료) 스레드를 멈추고 회수한다.
pub(crate) struct ReturnPump {
    returns: Arc<Returns>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ReturnPump {
    pub(crate) fn start<H: HostCall + Send + 'static>(
        returns: Arc<Returns>,
        host: H,
    ) -> Option<Self> {
        if !returns.is_available() {
            return None;
        }
        let worker = Arc::clone(&returns);
        let thread = std::thread::Builder::new()
            .name("claude-codex-return".into())
            .spawn(move || run_pump(&worker, &host));
        match thread {
            Ok(thread) => Some(Self {
                returns,
                thread: Some(thread),
            }),
            Err(e) => {
                tracing::error!(
                    "claude codex return: could not start the delivery thread, results stay pending: {e}"
                );
                None
            }
        }
    }
}

impl Drop for ReturnPump {
    fn drop(&mut self) {
        self.returns.stop();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("claude codex return: the delivery thread panicked");
        }
    }
}

fn run_pump<H: HostCall>(returns: &Returns, host: &H) {
    loop {
        if returns.stopping() {
            return;
        }
        let now = now_ms();
        let round = returns.pump_once(host, now);
        let wait = if round.more {
            Duration::ZERO
        } else {
            round
                .next_due_ms
                .map(|due| Duration::from_millis(due.saturating_sub(now_ms())))
                .map_or(PUMP_IDLE_WAIT, |d| d.min(PUMP_IDLE_WAIT))
        };
        if !returns.wait(wait) {
            return;
        }
    }
}

/// tmp 파일에 쓰고 디스크에 내린 뒤 이름을 바꾼다. 중간에 끊겨도 이전 내용이나 새 내용 중 하나가 남는다.
fn write_durably(dir: &Path, path: &Path, value: &Value) -> Result<(), String> {
    use std::io::Write;
    let tmp = dir.join(format!("{STORE_FILE}.{}.tmp", std::process::id()));
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    file.write_all(value.to_string().as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        tracing::warn!("claude codex return: could not sync {}: {e}", dir.display());
    }
    Ok(())
}

/// `claude.codex_returns`: 보내지 못한 결과를 보여 준다. `retry` 면 모두 지금 다시 보낼 차례로 돌리고
/// 송신 스레드를 깨운다. 송신은 그 스레드가 하므로 이 요청은 Codex 응답을 기다리지 않는다.
pub(crate) fn handle_returns(
    returns: &Returns,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    if !returns.is_available() {
        return Err(IpcMethodError::new(tr.t("claude.codex.no_data_dir")));
    }
    let requeued = if params.get("retry").and_then(Value::as_bool) == Some(true) {
        returns.requeue_all().map_err(IpcMethodError::new)?
    } else {
        0
    };
    let pending = returns.pending().map_err(IpcMethodError::new)?;
    Ok(json!({ "pending": pending, "requeued": requeued }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const THREAD: &str = "0193a6f2-7c1e-7b3d-9a40-1f2e3d4c5b6a";
    const RECEIVER: &str = "0123456789abcdef0123456789abcdef";

    fn tr() -> Translator {
        Translator::load(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang"),
            "en",
        )
    }

    /// `codex.parent`·`codex.deliver`·`terminal.children` 를 흉내 내는 호스트. deliver 실패를 켤 수 있다.
    #[derive(Default)]
    pub(crate) struct CodexHost {
        pub parent: Option<Value>,
        pub parent_error: Option<i32>,
        pub deliver_fails: RefCell<bool>,
        pub children: Value,
        /// surface → 호스트가 지금 그 surface 를 둔 workspace id. `surface.list` 로 답한다.
        pub workspace_of: RefCell<std::collections::BTreeMap<u32, u32>>,
        pub seen: RefCell<Vec<(String, Value)>>,
    }

    impl HostCall for CodexHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            self.seen.borrow_mut().push((method.to_string(), params));
            let fail = |code: i32, message: &str| {
                Err(tasty_plugin_sdk::PluginError::HostCall {
                    method: method.to_string(),
                    message: message.to_string(),
                    code: Some(code),
                })
            };
            match method {
                "codex.parent" => match (self.parent_error, &self.parent) {
                    (Some(code), _) => fail(code, "namespace 'codex' is not available"),
                    (None, Some(v)) => Ok(v.clone()),
                    (None, None) => fail(-32601, "method not found"),
                },
                "codex.deliver" if *self.deliver_fails.borrow() => fail(-32000, "receiver offline"),
                "codex.deliver" => Ok(json!({ "event_id": "ev", "delivery_state": "captured" })),
                "terminal.children" => Ok(self.children.clone()),
                "surface.list" => Ok(json!(
                    self.workspace_of
                        .borrow()
                        .iter()
                        .map(|(s, w)| json!({ "id": s, "workspace_id": w }))
                        .collect::<Vec<_>>()
                )),
                _ => Ok(json!({})),
            }
        }
    }

    impl CodexHost {
        pub(crate) fn delivered(&self) -> Vec<Value> {
            self.seen
                .borrow()
                .iter()
                .filter(|(m, _)| m == "codex.deliver")
                .map(|(_, p)| p.clone())
                .collect()
        }
    }

    fn codex_params(extra: Value) -> Value {
        let mut v = json!({ CONTEXT_KEY: { "provider": "codex", "thread_id": THREAD } });
        if let (Some(dst), Some(src)) = (v.as_object_mut(), extra.as_object()) {
            dst.extend(src.clone());
        }
        v
    }

    fn end(surface: u32, session: &str, status: ReturnStatus, body: &str) -> TurnEnd<'static> {
        TurnEnd {
            surface,
            session: Some(Box::leak(session.to_string().into_boxed_str())),
            status,
            body: Some(Box::leak(body.to_string().into_boxed_str())),
            attempt_id: None,
            turn: None,
        }
    }

    #[test]
    fn only_a_codex_context_with_a_thread_uuid_makes_a_codex_caller() {
        let tr = tr();
        assert!(codex_caller(&json!({}), &tr).unwrap().is_none());
        assert!(
            codex_caller(&json!({ CONTEXT_KEY: { "provider": "other" } }), &tr)
                .unwrap()
                .is_none()
        );
        assert!(
            codex_caller(&codex_params(json!({})), &tr)
                .unwrap()
                .is_some()
        );
        let bad = json!({ CONTEXT_KEY: { "provider": "codex", "thread_id": "not-a-uuid" } });
        assert_eq!(codex_caller(&bad, &tr).unwrap_err().code, -32602);
    }

    /// 일반 호출은 `codex.parent` 를 부르지 않고 기존 surface 를 그대로 쓴다.
    #[test]
    fn a_legacy_caller_keeps_its_surface_and_never_asks_codex() {
        let host = CodexHost::default();
        let target = parent_target(&host, &json!({ "surface": 7 }), &tr()).unwrap();
        assert_eq!(target, ParentTarget::Legacy(Some(7)));
        assert!(host.seen.borrow().is_empty());
    }

    #[test]
    fn a_codex_caller_gets_its_parent_from_codex_parent() {
        for (terminal_parent, expected) in [
            (json!({ "parent": 12 }), TerminalParent::Surface(12)),
            (
                json!({ "parent_external": format!("codex:{THREAD}") }),
                TerminalParent::External(format!("codex:{THREAD}")),
            ),
        ] {
            let host = CodexHost {
                parent: Some(
                    json!({ "receiver_id": RECEIVER, "terminal_parent": terminal_parent }),
                ),
                ..Default::default()
            };
            let target = parent_target(&host, &codex_params(json!({})), &tr()).unwrap();
            assert_eq!(
                target,
                ParentTarget::Codex(ResolvedParent {
                    receiver_id: RECEIVER.into(),
                    parent: expected.clone()
                })
            );
            assert_eq!(
                host.seen.borrow()[0].1,
                json!({ CONTEXT_KEY: { "provider": "codex", "thread_id": THREAD } })
            );
            let mut spawn = Map::new();
            target.insert(&mut spawn, "parent");
            match expected {
                TerminalParent::Surface(_) => assert_eq!(spawn["parent"], json!(12)),
                TerminalParent::External(k) => {
                    assert_eq!(spawn["parent_external"], json!(k));
                    assert!(!spawn.contains_key("parent"));
                }
            }
        }
    }

    /// Codex 플러그인이 없거나 꺼졌으면 오류 코드를 그대로 알리고 다른 부모로 대신하지 않는다.
    #[test]
    fn a_missing_codex_plugin_is_an_error_not_a_fallback() {
        for code in [-32601, -32001, -32002] {
            let host = CodexHost {
                parent_error: Some(code),
                ..Default::default()
            };
            let err =
                parent_target(&host, &codex_params(json!({ "surface": 3 })), &tr()).unwrap_err();
            assert_eq!(err.code, code);
        }
        let malformed = CodexHost {
            parent: Some(
                json!({ "receiver_id": RECEIVER, "terminal_parent": { "parent": 1, "parent_external": "x" } }),
            ),
            ..Default::default()
        };
        assert!(parent_target(&malformed, &codex_params(json!({})), &tr()).is_err());
    }

    /// receiver_id 는 32자리 16진수여야 한다.
    #[test]
    fn a_receiver_that_is_not_32_hex_is_refused() {
        for receiver in ["r1", "0123456789abcdef0123456789abcdeg", ""] {
            let host = CodexHost {
                parent: Some(
                    json!({ "receiver_id": receiver, "terminal_parent": { "parent": 1 } }),
                ),
                ..Default::default()
            };
            assert!(
                parent_target(&host, &codex_params(json!({})), &tr()).is_err(),
                "{receiver}"
            );
        }
    }

    /// completion_key 는 계약의 길이·문자 제한 안에 있다.
    #[test]
    fn the_completion_key_fits_the_contract() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b(RECEIVER), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        send(&returns, &host, end(40, "sA", ReturnStatus::TurnEnded, "x"));
        let key = host.delivered()[0]["completion_key"]
            .as_str()
            .unwrap()
            .to_string();
        assert!((1..=160).contains(&key.len()), "{key}");
        assert!(
            key.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)),
            "{key}"
        );
    }

    /// 명시한 surface 가 Codex 대화의 부모와 다르면 다른 부모의 자식을 건드리지 않도록 거절한다.
    #[test]
    fn an_explicit_surface_of_another_parent_is_refused() {
        let host = CodexHost {
            parent: Some(json!({ "receiver_id": RECEIVER, "terminal_parent": { "parent": 12 } })),
            ..Default::default()
        };
        assert!(parent_target(&host, &codex_params(json!({ "surface": 12 })), &tr()).is_ok());
        let err = parent_target(&host, &codex_params(json!({ "surface": 13 })), &tr()).unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn a_child_check_asks_the_host_registry_with_the_same_parent_key() {
        let host = CodexHost {
            children: json!({ "children": [{ "index": 1, "surface_id": 40 }] }),
            workspace_of: RefCell::new([(40, 5)].into()),
            ..Default::default()
        };
        let (_d, returns) = store();
        let external = Binding {
            receiver_id: RECEIVER.into(),
            parent_external: Some("codex:x".into()),
            attach: false,
        };
        returns.bind_new_child(40, &external, true).unwrap();
        let target = ParentTarget::Codex(ResolvedParent {
            receiver_id: RECEIVER.into(),
            parent: TerminalParent::External("codex:x".into()),
        });
        let parent = || {
            let mut m = Map::new();
            returns
                .insert_parent(&host, &target, &json!({}), &mut m, "surface", None, &tr())
                .unwrap();
            m
        };
        assert!(is_child_of(&host, parent(), 40).unwrap());
        assert!(!is_child_of(&host, parent(), 41).unwrap());
        let children_call = host
            .seen
            .borrow()
            .iter()
            .find(|(m, _)| m == "terminal.children")
            .map(|(_, p)| p.clone());
        assert_eq!(
            children_call,
            Some(json!({ "parent_external": "codex:x", "workspace": "5" })),
            "the workspace that holds the child now routes the lookup, not the focused window"
        );
        // 명시한 workspace 가 있으면 그것을 쓴다.
        let mut m = Map::new();
        returns
            .insert_parent(
                &host,
                &target,
                &json!({ "workspace": "other" }),
                &mut m,
                "surface",
                None,
                &tr(),
            )
            .unwrap();
        assert_eq!(m["workspace"], "other");
        // 물리 부모는 workspace 를 덧붙이지 않는다.
        let mut m = Map::new();
        returns
            .insert_parent(
                &host,
                &ParentTarget::Legacy(Some(3)),
                &json!({}),
                &mut m,
                "surface",
                None,
                &tr(),
            )
            .unwrap();
        assert_eq!(Value::Object(m), json!({ "surface": 3 }));
    }

    /// 물리 부모 자식의 결과 수신처.
    pub(crate) fn b(receiver: &str) -> Binding {
        Binding {
            receiver_id: receiver.to_string(),
            parent_external: None,
            attach: false,
        }
    }

    /// 확정한 결과를 claim 하고, 송신 스레드가 할 한 차례 송신을 지금 한다.
    fn send(returns: &Returns, host: &CodexHost, end: TurnEnd<'_>) {
        returns.turn_end(end);
        returns.pump_once(host, now_ms());
    }

    fn store() -> (tempfile::TempDir, Returns) {
        let dir = tempfile::tempdir().unwrap();
        let returns = Returns::new(Some(dir.path()));
        (dir, returns)
    }

    /// 같은 턴의 Stop 이 두 번 와도 결과는 한 번이다. prompt_id 가 없어도 턴 순번으로 구분한다.
    #[test]
    fn a_turn_is_claimed_once_and_the_next_prompt_opens_a_new_key() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "one"),
        );
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "one again"),
        );
        returns.observe("prompt-submit", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "two"),
        );
        let sent = host.delivered();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["body"], "one");
        assert_eq!(sent[0]["status"], "turn_ended");
        assert_eq!(sent[0]["receiver_id"], "r1");
        assert_eq!(sent[0]["child_session"], "sA");
        assert_eq!(sent[1]["body"], "two");
        assert_ne!(sent[0]["completion_key"], sent[1]["completion_key"]);
        assert!(sent[0].get("attempt_id").is_none());
    }

    /// 진행 중 턴에 tell 이 합쳐져 Stop 이 한 번이면 결과도 한 번이다.
    #[test]
    fn inputs_merged_into_one_turn_give_one_result() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.observe("prompt-submit", 40, Some("sA"), None);
        returns.bind_tell(40, &b("r1")).unwrap();
        returns.observe("prompt-submit", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "merged"),
        );
        assert_eq!(host.delivered().len(), 1);
    }

    /// 재시작해 다시 읽어도 이미 claim 한 턴은 다시 보내지 않는다.
    #[test]
    fn a_restart_does_not_resend_a_claimed_turn() {
        let (dir, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "one"),
        );
        let restarted = Returns::new(Some(dir.path()));
        send(
            &restarted,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "one"),
        );
        assert_eq!(host.delivered().len(), 1);
    }

    /// 두 부모의 자식은 각자의 수신처로 간다.
    #[test]
    fn two_parents_keep_their_own_receivers() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("parent-a"), true).unwrap();
        returns.bind_new_child(41, &b("parent-b"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.observe("session-start", 41, Some("sB"), None);
        send(&returns, &host, end(41, "sB", ReturnStatus::TurnEnded, "b"));
        send(&returns, &host, end(40, "sA", ReturnStatus::TurnEnded, "a"));
        let sent = host.delivered();
        assert_eq!(
            (sent[0]["receiver_id"].clone(), sent[0]["body"].clone()),
            (json!("parent-b"), json!("b"))
        );
        assert_eq!(
            (sent[1]["receiver_id"].clone(), sent[1]["body"].clone()),
            (json!("parent-a"), json!("a"))
        );
    }

    /// 같은 surface 를 다른 세션이 쓰면 이전 자식의 경로로 보내지 않는다.
    #[test]
    fn a_reused_surface_with_another_session_is_ignored() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.observe("session-start", 40, Some("other"), None);
        returns.observe("prompt-submit", 40, Some("other"), None);
        send(
            &returns,
            &host,
            end(40, "other", ReturnStatus::TurnEnded, "not mine"),
        );
        assert!(host.delivered().is_empty());
    }

    /// 복원으로 surface 가 바뀌어도 세션으로 경로를 다시 잇는다.
    #[test]
    fn a_restored_session_on_a_new_surface_keeps_its_route() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.observe("session-start", 77, Some("sA"), None);
        returns.observe("prompt-submit", 77, Some("sA"), None);
        send(
            &returns,
            &host,
            end(77, "sA", ReturnStatus::TurnEnded, "after restore"),
        );
        let sent = host.delivered();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["child_surface"], 77);
    }

    /// 경로가 없는 자식(일반 Claude 부모)의 턴은 Codex 로 보내지 않는다.
    #[test]
    fn a_child_without_a_route_sends_nothing() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.observe("prompt-submit", 40, Some("sA"), None);
        send(&returns, &host, end(40, "sA", ReturnStatus::TurnEnded, "x"));
        assert!(host.seen.borrow().is_empty());
    }

    /// 보내지 못한 결과는 본문·수신처째 남고, 늘어나는 간격을 두고 같은 키로 자동으로 다시 보낸다.
    #[test]
    fn a_failed_delivery_is_retried_automatically_with_the_same_key() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        *host.deliver_fails.borrow_mut() = true;
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.turn_end(end(40, "sA", ReturnStatus::TurnFailed, "rate_limit"));
        let t0 = now_ms();
        let first = returns.pump_once(&host, t0);
        assert_eq!(first.attempted, 1);
        let pending = returns.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["request"]["body"], "rate_limit");
        assert_eq!(pending[0]["request"]["receiver_id"], "r1");
        assert_eq!(pending[0]["request"]["status"], "turn_failed");
        assert_eq!(pending[0]["attempts"], 1);
        assert!(
            pending[0]["error"]
                .as_str()
                .unwrap()
                .contains("receiver offline")
        );
        let due = first.next_due_ms.expect("a retry is scheduled");
        assert!(due >= t0 + 2_000, "the first retry waits the backoff");

        // 기다리는 동안에는 보내지 않는다.
        assert_eq!(returns.pump_once(&host, due - 1).attempted, 0);
        // 두 번째 실패 뒤 간격은 늘어난다.
        let second = returns.pump_once(&host, due);
        assert_eq!(second.attempted, 1);
        assert!(second.next_due_ms.unwrap() >= due + 4_000);

        *host.deliver_fails.borrow_mut() = false;
        let third = returns.pump_once(&host, second.next_due_ms.unwrap());
        assert_eq!(third.attempted, 1);
        assert_eq!(third.next_due_ms, None);
        assert!(returns.pending().unwrap().is_empty());
        let keys: Vec<Value> = host
            .delivered()
            .iter()
            .map(|r| r["completion_key"].clone())
            .collect();
        assert_eq!(keys.len(), 3);
        assert!(
            keys.iter().all(|k| *k == keys[0]),
            "every retry keeps the key"
        );
    }

    /// 자동 재시도 기간이 지나면 멈추지만 요청은 남고, 수동 retry 가 다시 보낼 차례로 돌린다.
    #[test]
    fn retries_stop_after_the_window_and_a_manual_retry_requeues_them() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        *host.deliver_fails.borrow_mut() = true;
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.turn_end(end(40, "sA", ReturnStatus::TurnEnded, "kept"));
        // 실패 기록은 호출이 끝난 뒤의 벽시계와 넘긴 시각 중 늦은 쪽이다. 벽시계가 따라잡지 못할
        // 미래 시각을 넘겨 첫 실패 시각을 t0 으로 고정한다.
        let t0 = now_ms() + 3_600_000;
        returns.pump_once(&host, t0);
        let late = t0 + ms(AUTO_RETRY_WINDOW) + 1;
        let round = returns.pump_once(&host, late);
        assert_eq!(round.attempted, 1);
        assert_eq!(round.next_due_ms, None, "no automatic retry is scheduled");
        assert_eq!(returns.pump_once(&host, late + 3_600_000).attempted, 0);
        let pending = returns.pending().unwrap();
        assert_eq!(pending[0]["gave_up"], true);
        assert_eq!(pending[0]["request"]["body"], "kept");

        *host.deliver_fails.borrow_mut() = false;
        let out = handle_returns(&returns, &json!({ "retry": true }), &tr()).unwrap();
        assert_eq!(out["requeued"], 1);
        assert_eq!(
            host.delivered().len(),
            2,
            "the manual retry does not send on the worker"
        );
        assert_eq!(returns.pump_once(&host, late + 1).attempted, 1);
        assert!(returns.pending().unwrap().is_empty());
    }

    /// 한 차례에는 작은 묶음만 보내고, 남은 것은 곧바로 다음 차례로 넘긴다.
    #[test]
    fn a_round_sends_a_small_batch_and_reports_the_rest() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        for surface in 0..(DELIVER_BATCH as u32 + 2) {
            returns.bind_new_child(surface, &b("r1"), true).unwrap();
            let session = format!("s{surface}");
            returns.observe("session-start", surface, Some(&session), None);
            returns.turn_end(TurnEnd {
                surface,
                session: Some(&session),
                status: ReturnStatus::TurnEnded,
                body: Some("x"),
                attempt_id: None,
                turn: None,
            });
        }
        let first = returns.pump_once(&host, now_ms());
        assert_eq!((first.attempted, first.more), (DELIVER_BATCH, true));
        let second = returns.pump_once(&host, now_ms());
        assert_eq!((second.attempted, second.more), (2, false));
    }

    /// 확정은 파일에 claim 을 남기기만 하고 Codex 를 부르지 않는다. 다시 시작한 플러그인의 송신이 보낸다.
    #[test]
    fn a_claim_survives_a_restart_and_is_sent_by_the_next_pump() {
        let (dir, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.turn_end(end(40, "sA", ReturnStatus::TurnEnded, "before restart"));
        assert!(
            host.seen.borrow().is_empty(),
            "turn_end never calls the host"
        );
        let restarted = Returns::new(Some(dir.path()));
        assert_eq!(restarted.pump_once(&host, now_ms()).attempted, 1);
        assert_eq!(host.delivered()[0]["body"], "before restart");
    }

    /// 확정 전에 세션이 끝나면 그 턴을 exited 로 한 번 보낸다. 이미 끝난 턴이면 보내지 않는다.
    #[test]
    fn a_session_end_reports_only_an_open_turn() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "done"),
        );
        send(&returns, &host, end(40, "sA", ReturnStatus::Exited, ""));
        assert_eq!(host.delivered().len(), 1);
        returns.observe("prompt-submit", 40, Some("sA"), None);
        send(&returns, &host, end(40, "sA", ReturnStatus::Exited, ""));
        let sent = host.delivered();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["status"], "exited");
    }

    /// respawn 은 같은 surface 에 새 경로를 둔다. 이전 세션의 늦은 hook 은 새 경로를 건드리지 않는다.
    #[test]
    fn a_respawn_starts_a_fresh_route() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("old"), None);
        returns.bind_new_child(40, &b("r2"), true).unwrap();
        returns.observe("stop", 40, Some("old"), None);
        send(
            &returns,
            &host,
            end(40, "old", ReturnStatus::TurnEnded, "late"),
        );
        assert!(host.delivered().is_empty());
        returns.observe("session-start", 40, Some("new"), None);
        send(
            &returns,
            &host,
            end(40, "new", ReturnStatus::TurnEnded, "fresh"),
        );
        assert_eq!(host.delivered()[0]["receiver_id"], "r2");
    }

    /// 이미 실행 중인 자식에게 tell 로 만든 경로는 다음 hook 의 세션으로 묶인다.
    #[test]
    fn a_tell_to_a_running_child_adopts_its_next_hook() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_tell(50, &b("r9")).unwrap();
        returns.observe("prompt-submit", 50, Some("sX"), None);
        send(
            &returns,
            &host,
            end(50, "sX", ReturnStatus::TurnEnded, "answer"),
        );
        let sent = host.delivered();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["receiver_id"], "r9");
        assert_eq!(sent[0]["child_session"], "sX");
    }

    /// 데이터 경로가 없으면 Codex 경로를 만들지 못하고 그 사실을 알린다.
    #[test]
    fn without_a_data_dir_routes_cannot_be_stored() {
        let returns = Returns::new(None);
        assert!(!returns.is_available());
        assert!(returns.bind_new_child(1, &b("r"), true).is_err());
        assert!(handle_returns(&returns, &json!({}), &tr()).is_err());
    }

    /// `/clear` 는 같은 프로세스가 새 세션으로 이어 간다(SessionEnd `clear` 를 마친 뒤 SessionStart `clear`).
    /// 경로는 새 세션으로 이어지고, 그 SessionEnd 는 `exited` 가 아니다.
    #[test]
    fn a_clear_continues_the_route_in_the_new_session() {
        for kind in ["clear", "resume"] {
            let (_d, returns) = store();
            let host = CodexHost::default();
            returns.bind_new_child(40, &b("r1"), true).unwrap();
            returns.observe("session-start", 40, Some("sA"), Some("startup"));
            send(
                &returns,
                &host,
                end(40, "sA", ReturnStatus::TurnEnded, "before"),
            );
            returns.observe("session-end", 40, Some("sA"), Some(kind));
            send(&returns, &host, end(40, "sA", ReturnStatus::Exited, ""));
            returns.observe("session-start", 40, Some("sB"), Some(kind));
            returns.observe("prompt-submit", 40, Some("sB"), None);
            send(
                &returns,
                &host,
                end(40, "sB", ReturnStatus::TurnEnded, "after"),
            );
            let sent = host.delivered();
            let bodies: Vec<&Value> = sent.iter().map(|r| &r["body"]).collect();
            assert_eq!(bodies, [&json!("before"), &json!("after")], "{kind}");
            assert_eq!(sent[1]["child_session"], "sB", "{kind}");
            assert_ne!(
                sent[0]["completion_key"], sent[1]["completion_key"],
                "{kind}"
            );
        }
    }

    /// `/clear`·`/resume` 이 아닌 다른 세션의 시작(새 실행, 같은 surface 안의 다른 Claude)은 경로를 가져가지 못한다.
    #[test]
    fn a_start_that_is_not_a_continuation_cannot_take_the_route() {
        for source in [Some("startup"), Some("compact"), None] {
            let (_d, returns) = store();
            let host = CodexHost::default();
            returns.bind_new_child(40, &b("r1"), true).unwrap();
            returns.observe("session-start", 40, Some("sA"), Some("startup"));
            returns.observe("session-start", 40, Some("stale"), source);
            returns.observe("prompt-submit", 40, Some("stale"), None);
            send(
                &returns,
                &host,
                end(40, "stale", ReturnStatus::TurnEnded, "x"),
            );
            assert!(host.delivered().is_empty(), "{source:?}");
            // 원래 세션은 그대로 이어진다.
            send(
                &returns,
                &host,
                end(40, "sA", ReturnStatus::TurnEnded, "mine"),
            );
            assert_eq!(host.delivered()[0]["body"], "mine", "{source:?}");
        }
    }

    /// Claude Code 는 `/clear`·`/resume` 의 SessionEnd 훅을 짧은 상한까지만 기다린다. 넘으면 새 세션의
    /// SessionStart 가 먼저 온다. 그래도 경로는 새 세션으로 이어지고, 뒤늦은 옛 SessionEnd 는 아무것도 바꾸지 않는다.
    #[test]
    fn a_continuation_start_that_beats_its_session_end_still_continues_the_route() {
        for kind in ["clear", "resume"] {
            let (_d, returns) = store();
            let host = CodexHost::default();
            returns.bind_new_child(40, &b("r1"), true).unwrap();
            returns.observe("session-start", 40, Some("sA"), Some("startup"));
            send(&returns, &host, end(40, "sA", ReturnStatus::TurnEnded, "a"));
            returns.observe("session-start", 40, Some("sB"), Some(kind));
            returns.observe("session-end", 40, Some("sA"), Some(kind));
            send(&returns, &host, end(40, "sA", ReturnStatus::Exited, ""));
            returns.observe("prompt-submit", 40, Some("sB"), None);
            send(&returns, &host, end(40, "sB", ReturnStatus::TurnEnded, "b"));
            let sent = host.delivered();
            let bodies: Vec<&Value> = sent.iter().map(|r| &r["body"]).collect();
            assert_eq!(bodies, [&json!("a"), &json!("b")], "{kind}");
            assert_eq!(sent[1]["child_session"], "sB", "{kind}");
        }
    }

    /// 늦게 온 예전 SessionStart(`clear`·`resume`)는 경로를 되돌리지 못한다. 이 경로에 묶였던 세션의 `clear`
    /// 시작은 받지 않고, 묶인 적 없는 세션의 시작은 그 세션의 다음 hook 이 올 때까지 기다리기만 한다.
    #[test]
    fn a_late_old_continuation_start_does_not_take_the_route_back() {
        for (late, kind) in [("sB", "clear"), ("sX", "clear"), ("sX", "resume")] {
            let (_d, returns) = store();
            let host = CodexHost::default();
            returns.bind_new_child(40, &b("r1"), false).unwrap();
            returns.observe("session-start", 40, Some("sA"), Some("startup"));
            returns.observe("session-end", 40, Some("sA"), Some("clear"));
            returns.observe("session-start", 40, Some("sB"), Some("clear"));
            returns.observe("session-end", 40, Some("sB"), Some("clear"));
            returns.observe("session-start", 40, Some("sC"), Some("clear"));
            // 늦은 시작. 그 세션의 hook 은 더 오지 않는다.
            returns.observe("session-start", 40, Some(late), Some(kind));
            returns.observe("prompt-submit", 40, Some("sC"), None);
            send(&returns, &host, end(40, "sC", ReturnStatus::TurnEnded, "c"));
            let sent = host.delivered();
            assert_eq!(sent.len(), 1, "{late} {kind}: {sent:?}");
            assert_eq!(sent[0]["child_session"], "sC", "{late} {kind}");
        }
    }

    /// hook 처리 순서대로 Stop 을 보낸다. observe 를 먼저 하고 그 뒤 잡은 턴으로 claim 한다.
    fn stop_in_order(returns: &Returns, host: &CodexHost, session: &str, body: &str) {
        returns.observe("stop", 40, Some(session), None);
        if let Some(turn) = returns.turn_of(40, Some(session)) {
            send(
                returns,
                host,
                TurnEnd {
                    turn: Some(turn),
                    ..end(40, session, ReturnStatus::TurnEnded, body)
                },
            );
        }
    }

    /// (턴 키 끝, 상태, 본문, 세션) 목록.
    fn delivered_turns(host: &CodexHost) -> Vec<(String, String, String, String)> {
        host.delivered()
            .iter()
            .map(|d| {
                let key = d["completion_key"].as_str().unwrap_or("");
                (
                    key[key.rfind(':').map_or(0, |i| i + 1)..].to_string(),
                    d["status"].as_str().unwrap_or("").to_string(),
                    d["body"].as_str().unwrap_or("-").to_string(),
                    d["child_session"].as_str().unwrap_or("-").to_string(),
                )
            })
            .collect()
    }

    /// `/clear` 로 B 에 넘어간 뒤 넘어간 세션 A 의 늦은 `resume` 시작과 그 뒤 A 의 늦은 Stop·SessionEnd 가 와도
    /// 경로는 B 에 남는다. B 의 결과는 B 의 턴 키로 가고 A 의 늦은 hook 은 결과나 exited 를 만들지 않는다.
    #[test]
    fn late_hooks_after_a_late_resume_start_of_a_retired_session_do_not_take_the_route() {
        for late in ["stop", "end-clear", "end-exit"] {
            let (_d, returns) = store();
            let host = CodexHost::default();
            returns.bind_new_child(40, &b("r1"), true).unwrap();
            returns.observe("session-start", 40, Some("sA"), Some("startup"));
            returns.observe("prompt-submit", 40, Some("sA"), None);
            stop_in_order(&returns, &host, "sA", "a1");
            returns.observe("session-end", 40, Some("sA"), Some("clear"));
            returns.observe("session-start", 40, Some("sB"), Some("clear"));
            returns.observe("prompt-submit", 40, Some("sB"), None);
            returns.observe("session-start", 40, Some("sA"), Some("resume"));
            match late {
                "stop" => stop_in_order(&returns, &host, "sA", "a-late"),
                "end-clear" => returns.observe("session-end", 40, Some("sA"), Some("clear")),
                _ => {
                    returns.observe("session-end", 40, Some("sA"), Some("prompt_input_exit"));
                    send(
                        &returns,
                        &host,
                        TurnEnd {
                            body: None,
                            ..end(40, "sA", ReturnStatus::Exited, "")
                        },
                    );
                }
            }
            stop_in_order(&returns, &host, "sB", "b2");
            returns.observe("prompt-submit", 40, Some("sB"), None);
            stop_in_order(&returns, &host, "sB", "b3");
            let want = [("t1", "a1", "sA"), ("t2", "b2", "sB"), ("t3", "b3", "sB")]
                .map(|(t, b, s)| (t.into(), "turn_ended".into(), b.into(), s.into()));
            assert_eq!(delivered_turns(&host), want, "{late}");
        }
    }

    /// 넘어간 세션의 늦은 시작은 SessionEnd 보다 먼저 와 기다리는 새 세션을 밀어내지 않는다. 먼저 새 프롬프트를
    /// 보낸 후보가 잇고, 잇고 나면 다른 후보는 버리므로 그 뒤 그 후보의 프롬프트도 경로를 가져가지 못한다.
    #[test]
    fn a_late_start_of_a_retired_session_does_not_displace_a_waiting_session() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("session-end", 40, Some("sA"), Some("clear"));
        returns.observe("session-start", 40, Some("sB"), Some("clear"));
        returns.observe("session-start", 40, Some("sC"), Some("clear"));
        returns.observe("session-start", 40, Some("sA"), Some("resume"));
        returns.observe("session-end", 40, Some("sB"), Some("clear"));
        returns.observe("prompt-submit", 40, Some("sC"), None);
        stop_in_order(&returns, &host, "sC", "c");
        returns.observe("prompt-submit", 40, Some("sA"), None);
        stop_in_order(&returns, &host, "sA", "a-late");
        stop_in_order(&returns, &host, "sC", "c-again");
        assert_eq!(
            delivered_turns(&host),
            [("t1".into(), "turn_ended".into(), "c".into(), "sC".into())]
        );
    }

    /// 묶인 세션의 SessionEnd 가 끊겨 오지 않은 채 `/clear` 로 시작한 세션이 프롬프트 없이 넘어간 세션으로
    /// `/resume` 하면, 두 시작이 모두 후보가 되고 새 프롬프트를 보낸 resume 세션이 잇는다.
    #[test]
    fn a_resume_to_a_retired_session_continues_while_a_promptless_start_waits() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("prompt-submit", 40, Some("sA"), None);
        stop_in_order(&returns, &host, "sA", "a1");
        returns.observe("session-end", 40, Some("sA"), Some("clear"));
        returns.observe("session-start", 40, Some("sB"), Some("clear"));
        returns.observe("prompt-submit", 40, Some("sB"), None);
        stop_in_order(&returns, &host, "sB", "b2");
        // B 의 SessionEnd(clear) 와 C 의 SessionEnd(resume) 는 오지 않는다.
        returns.observe("session-start", 40, Some("sC"), Some("clear"));
        returns.observe("session-start", 40, Some("sA"), Some("resume"));
        returns.observe("prompt-submit", 40, Some("sA"), None);
        stop_in_order(&returns, &host, "sA", "a-resumed");
        assert_eq!(
            delivered_turns(&host),
            [
                ("t1".into(), "turn_ended".into(), "a1".into(), "sA".into()),
                ("t2".into(), "turn_ended".into(), "b2".into(), "sB".into()),
                (
                    "t3".into(),
                    "turn_ended".into(),
                    "a-resumed".into(),
                    "sA".into()
                ),
            ]
        );
    }

    /// 후보는 [`WAITING_KEPT`] 개까지만 남는다. 이전 형식의 문자열 후보 하나도 읽는다.
    #[test]
    fn waiting_candidates_are_bounded_and_read_from_the_old_format() {
        let (dir, returns) = store();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        for i in 0..WAITING_KEPT + 2 {
            returns.observe("session-start", 40, Some(&format!("s{i}")), Some("clear"));
        }
        let path = dir.path().join(STORE_DIR).join(STORE_FILE);
        let mut saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let waiting = saved["routes"][0]["waiting"].as_array().unwrap().clone();
        assert_eq!(waiting.len(), WAITING_KEPT);
        assert_eq!(waiting[0], "s2");
        saved["routes"][0]["waiting"] = json!("sOld");
        std::fs::write(&path, saved.to_string()).unwrap();
        let restarted = Returns::new(Some(dir.path()));
        let host = CodexHost::default();
        restarted.observe("prompt-submit", 40, Some("sOld"), None);
        stop_in_order(&restarted, &host, "sOld", "old");
        assert_eq!(
            delivered_turns(&host),
            [(
                "t1".into(),
                "turn_ended".into(),
                "old".into(),
                "sOld".into()
            )]
        );
    }

    /// 기다리는 세션은 묶인 세션이 그 뒤 프롬프트를 받아도 버리지 않는다. 묶인 세션의 hook 이 늦게 왔을 수
    /// 있으므로 기다리는 세션이 새 프롬프트를 받으면 잇는다.
    #[test]
    fn a_waiting_session_still_continues_after_a_late_prompt_of_the_bound_session() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("session-start", 40, Some("sB"), Some("resume"));
        returns.observe("prompt-submit", 40, Some("sA"), None);
        stop_in_order(&returns, &host, "sA", "a");
        returns.observe("prompt-submit", 40, Some("sB"), None);
        stop_in_order(&returns, &host, "sB", "b");
        assert_eq!(
            delivered_turns(&host),
            [
                ("t1".into(), "turn_ended".into(), "a".into(), "sA".into()),
                ("t2".into(), "turn_ended".into(), "b".into(), "sB".into()),
            ]
        );
    }

    /// 판정을 기다리는 Stop 뒤 다른 부모의 tell 로 수신처가 바뀌어도 그 턴의 결과는 Stop 이 온 때의
    /// 수신처·세션·surface·키로 간다. 새 부모는 그 뒤 턴의 결과를 받는다.
    #[test]
    fn a_pending_stop_goes_to_the_receiver_of_its_turn_after_another_parent_tells() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("prompt-submit", 40, Some("sA"), None);
        returns.observe("stop", 40, Some("sA"), None);
        let held = returns.turn_of(40, Some("sA")).expect("turn of sA");
        returns.bind_tell(40, &b("r2")).unwrap();
        returns.observe("prompt-submit", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            TurnEnd {
                turn: Some(held),
                ..end(40, "sA", ReturnStatus::TurnEnded, "for-r1")
            },
        );
        stop_in_order(&returns, &host, "sA", "for-r2");
        let sent = host.delivered();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(
            (
                &sent[0]["receiver_id"],
                &sent[0]["body"],
                &sent[0]["child_session"],
                &sent[0]["child_surface"]
            ),
            (&json!("r1"), &json!("for-r1"), &json!("sA"), &json!(40))
        );
        assert!(sent[0]["completion_key"].as_str().unwrap().ends_with(":t1"));
        assert_eq!(
            (&sent[1]["receiver_id"], &sent[1]["body"]),
            (&json!("r2"), &json!("for-r2"))
        );
        assert!(sent[1]["completion_key"].as_str().unwrap().ends_with(":t2"));
    }

    /// 묶였던 세션의 늦은 `clear` 시작은 넘길 준비 중이어도 경로를 가져가지 못한다. 다음 새 세션이 잇는다.
    #[test]
    fn a_stale_clear_start_of_a_retired_session_does_not_take_a_handoff() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("session-end", 40, Some("sA"), Some("clear"));
        returns.observe("session-start", 40, Some("sB"), Some("clear"));
        returns.observe("session-end", 40, Some("sB"), Some("clear"));
        returns.observe("session-start", 40, Some("sA"), Some("clear"));
        assert_eq!(
            returns.turn_of(40, Some("sA")),
            None,
            "the stale start of a retired session did not take the handoff"
        );
        returns.observe("session-start", 40, Some("sC"), Some("clear"));
        returns.observe("prompt-submit", 40, Some("sC"), None);
        send(&returns, &host, end(40, "sC", ReturnStatus::TurnEnded, "c"));
        assert_eq!(host.delivered()[0]["child_session"], "sC");
    }

    /// 끝난 세션 뒤 같은 surface 에서 새로 시작한 Claude 에 tell 하면 그 실행의 다음 hook 으로 다시 묶는다.
    /// 경로 id 와 턴 순번은 이어 가므로 이전 결과와 키가 겹치지 않는다.
    #[test]
    fn a_tell_after_a_fresh_start_on_the_same_surface_rebinds_the_route() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "old"),
        );
        returns.observe("session-end", 40, Some("sA"), Some("prompt_input_exit"));
        returns.observe("session-start", 40, Some("sB"), Some("startup"));
        returns.bind_tell(40, &b("r2")).unwrap();
        returns.observe("prompt-submit", 40, Some("sB"), None);
        send(
            &returns,
            &host,
            end(40, "sB", ReturnStatus::TurnEnded, "new"),
        );
        let sent = host.delivered();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(sent[1]["body"], "new");
        assert_eq!(sent[1]["receiver_id"], "r2");
        assert_eq!(sent[1]["child_session"], "sB");
        assert_ne!(sent[0]["completion_key"], sent[1]["completion_key"]);
    }

    /// 넘길 준비만 하고 새 세션 시작이 오지 않은 경로도 tell 이 다시 묶는다.
    #[test]
    fn a_tell_rebinds_a_route_whose_handoff_never_completed() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("session-end", 40, Some("sA"), Some("clear"));
        returns.bind_tell(40, &b("r1")).unwrap();
        returns.observe("prompt-submit", 40, Some("sB"), None);
        send(&returns, &host, end(40, "sB", ReturnStatus::TurnEnded, "x"));
        assert_eq!(host.delivered()[0]["child_session"], "sB");
    }

    /// 실행 중인 세션에 다시 tell 하면 세션을 바꾸지 않는다. 같은 surface 의 다른 Claude 가 경로를 가져가지 못한다.
    #[test]
    fn a_tell_to_a_live_session_keeps_its_binding() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.bind_tell(40, &b("r2")).unwrap();
        returns.observe("prompt-submit", 40, Some("nested"), None);
        send(
            &returns,
            &host,
            end(40, "nested", ReturnStatus::TurnEnded, "no"),
        );
        returns.observe("prompt-submit", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "yes"),
        );
        let sent = host.delivered();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["body"], "yes");
        assert_eq!(sent[0]["receiver_id"], "r2");
    }

    /// Stop 이 온 때 정한 턴으로 claim 한다. 그 뒤 새 턴이 몇 번 열리거나 새 턴 결과가 먼저 나가도 늦게
    /// 확정된 앞 턴 결과는 원래 키로 한 번 나간다.
    #[test]
    fn a_late_settled_turn_keeps_the_key_of_the_turn_its_stop_came_in() {
        for newer_first in [false, true] {
            let (_d, returns) = store();
            let host = CodexHost::default();
            returns.bind_new_child(40, &b("r1"), false).unwrap();
            returns.observe("session-start", 40, Some("sA"), Some("startup"));
            returns.observe("prompt-submit", 40, Some("sA"), None);
            let first = returns.turn_of(40, Some("sA")).expect("turn 1");
            returns.observe("prompt-submit", 40, Some("sA"), None);
            returns.observe("prompt-submit", 40, Some("sA"), None);
            let third = returns.turn_of(40, Some("sA")).expect("turn 3");
            let late = |body: &'static str| TurnEnd {
                turn: Some(first.clone()),
                ..end(40, "sA", ReturnStatus::TurnEnded, body)
            };
            let newer = TurnEnd {
                turn: Some(third.clone()),
                ..end(40, "sA", ReturnStatus::TurnEnded, "three")
            };
            if newer_first {
                send(&returns, &host, newer);
                send(&returns, &host, late("one"));
            } else {
                send(&returns, &host, late("one"));
                send(&returns, &host, newer);
            }
            send(&returns, &host, late("one again"));
            let sent = host.delivered();
            let mut got: Vec<(String, String)> = sent
                .iter()
                .map(|r| {
                    let key = r["completion_key"].as_str().unwrap();
                    (
                        key[key.rfind(':').unwrap() + 1..].to_string(),
                        r["body"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            got.sort();
            assert_eq!(
                got,
                [("t1".into(), "one".into()), ("t3".into(), "three".into())],
                "newer_first={newer_first}"
            );
        }
    }

    /// `/clear` 로 경로가 새 세션으로 넘어간 뒤 늦게 claim 한 앞 턴 결과에는 그 턴을 실행한 세션을 싣는다.
    #[test]
    fn a_late_result_after_a_clear_names_the_session_that_ran_the_turn() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("prompt-submit", 40, Some("sA"), None);
        let first = returns.turn_of(40, Some("sA")).expect("turn of sA");
        returns.observe("session-end", 40, Some("sA"), Some("clear"));
        returns.observe("session-start", 40, Some("sB"), Some("clear"));
        send(
            &returns,
            &host,
            TurnEnd {
                turn: Some(first),
                ..end(40, "sA", ReturnStatus::TurnEnded, "from sA")
            },
        );
        let sent = host.delivered();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["child_session"], "sA");
        assert_eq!(sent[0]["child_surface"], 40);
    }

    /// 같은 데이터 폴더를 쓰는 두 저장소(두 플러그인 프로세스)가 동시에 갱신해도 서로의 경로를 잃지 않는다.
    #[test]
    fn two_stores_on_one_data_dir_do_not_lose_each_others_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let workers: Vec<_> = (0..2u32)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let returns = Returns::new(Some(&path));
                    for i in 0..40u32 {
                        returns
                            .bind_new_child(w * 1000 + i, &b("r1"), false)
                            .unwrap();
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        let returns = Returns::new(Some(&path));
        let count = returns.update(|book| (book.routes.len(), false)).unwrap();
        assert_eq!(count, 80);
    }

    /// 넘긴 뒤 옛 세션의 늦은 SessionEnd 는 새 세션의 경로를 끝내거나 exited 를 만들지 않는다.
    #[test]
    fn a_late_session_end_of_the_old_session_changes_nothing() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("session-end", 40, Some("sA"), Some("clear"));
        returns.observe("session-start", 40, Some("sB"), Some("clear"));
        returns.observe("prompt-submit", 40, Some("sB"), None);
        returns.observe("session-end", 40, Some("sA"), Some("other"));
        send(&returns, &host, end(40, "sA", ReturnStatus::Exited, ""));
        assert!(host.delivered().is_empty());
        send(
            &returns,
            &host,
            end(40, "sB", ReturnStatus::TurnEnded, "still mine"),
        );
        assert_eq!(host.delivered()[0]["body"], "still mine");
    }

    /// 복원으로 새 surface 로 옮긴 경로를 옛 surface 의 늦은 hook 이 되돌리거나 끝내지 못한다.
    #[test]
    fn a_late_hook_from_the_old_surface_does_not_move_a_restored_route_back() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        returns.bind_new_child(40, &b("r1"), false).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("session-start", 77, Some("sA"), Some("resume"));
        returns.observe("prompt-submit", 77, Some("sA"), None);
        for event in ["stop", "prompt-submit", "session-end"] {
            returns.observe(event, 40, Some("sA"), Some("other"));
        }
        send(&returns, &host, end(40, "sA", ReturnStatus::Exited, ""));
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "old surface"),
        );
        assert!(host.delivered().is_empty());
        send(
            &returns,
            &host,
            end(77, "sA", ReturnStatus::TurnEnded, "restored"),
        );
        let sent = host.delivered();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["child_surface"], 77);
        assert_eq!(
            sent[0]["completion_key"]
                .as_str()
                .unwrap()
                .rsplit(':')
                .next(),
            Some("t1")
        );
    }

    /// 프로세스가 끝난 경로는 복원을 기다리는 동안 남고 같은 세션의 복원으로 이어진다. 기간이 지나면
    /// 지우지만 아직 보내지 못한 결과는 남는다.
    #[test]
    fn an_ended_route_waits_for_restore_and_is_then_swept_without_losing_results() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        *host.deliver_fails.borrow_mut() = true;
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), Some("startup"));
        returns.observe("session-end", 40, Some("sA"), Some("other"));
        send(&returns, &host, end(40, "sA", ReturnStatus::Exited, ""));
        returns.observe("session-start", 77, Some("sA"), Some("resume"));
        returns.observe("prompt-submit", 77, Some("sA"), None);
        send(
            &returns,
            &host,
            end(77, "sA", ReturnStatus::TurnEnded, "restored"),
        );
        assert_eq!(
            returns.pending().unwrap().len(),
            2,
            "exited + restored turn kept"
        );

        returns.observe("session-end", 77, Some("sA"), Some("other"));
        let later = now_ms() + ms(ENDED_ROUTE_GRACE) + 1;
        returns.update(|book| ((), book.sweep(later))).unwrap();
        assert!(
            returns
                .update(|book| (book.routes.is_empty(), false))
                .unwrap()
        );
        assert_eq!(
            returns.pending().unwrap().len(),
            2,
            "sweeping a route keeps its results"
        );
    }

    /// 닫힌 자식의 경로는 바로 지우되 보내지 못한 결과는 남긴다.
    #[test]
    fn forgetting_a_killed_child_keeps_its_pending_results() {
        let (_d, returns) = store();
        let host = CodexHost::default();
        *host.deliver_fails.borrow_mut() = true;
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        send(
            &returns,
            &host,
            end(40, "sA", ReturnStatus::TurnEnded, "last words"),
        );
        returns.forget_child(40);
        assert!(
            returns
                .update(|book| (book.routes.is_empty(), false))
                .unwrap()
        );
        assert_eq!(
            returns.pending().unwrap()[0]["request"]["body"],
            "last words"
        );
    }

    fn external() -> Binding {
        Binding {
            receiver_id: RECEIVER.into(),
            parent_external: Some("codex:x".into()),
            attach: false,
        }
    }

    /// 외부 부모의 자식이 여러 workspace 에 있으면 대상 자식이 지금 있는 workspace 를 쓰고, 대상이 없는
    /// index 명령은 명시하지 않으면 거절한다. 복원으로 자식이 옮겨 가면 spawn 때의 workspace 가 아니라
    /// 호스트가 지금 그 surface 를 둔 workspace 를 쓴다. 마지막으로 쓴 workspace 로 정하지 않는다.
    #[test]
    fn the_workspace_comes_from_where_the_host_holds_the_child_now() {
        let host = CodexHost {
            workspace_of: RefCell::new([(40, 1), (41, 2)].into()),
            ..Default::default()
        };
        let (_d, returns) = store();
        returns.bind_new_child(40, &external(), true).unwrap();
        returns.bind_new_child(41, &external(), true).unwrap();
        let target = ParentTarget::Codex(ResolvedParent {
            receiver_id: RECEIVER.into(),
            parent: TerminalParent::External("codex:x".into()),
        });
        let routed = |params: Value, child: Option<u32>| {
            let mut m = Map::new();
            returns
                .insert_parent(&host, &target, &params, &mut m, "surface", child, &tr())
                .map(|()| m.get("workspace").cloned())
        };
        assert_eq!(routed(json!({}), Some(40)).unwrap(), Some(json!("1")));
        assert_eq!(routed(json!({}), Some(41)).unwrap(), Some(json!("2")));
        let err = routed(json!({}), None).unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(
            err.message.contains('1') && err.message.contains('2'),
            "{}",
            err.message
        );
        assert_eq!(
            routed(json!({ "workspace": "right" }), None).unwrap(),
            Some(json!("right"))
        );
        // 41 이 복원되어 40 과 같은 workspace 의 새 surface 로 옮겨 갔다.
        returns.observe("session-start", 41, Some("s41"), Some("startup"));
        returns.observe("session-start", 77, Some("s41"), Some("resume"));
        {
            let mut w = host.workspace_of.borrow_mut();
            w.remove(&41);
            w.insert(77, 1);
        }
        assert_eq!(routed(json!({}), Some(77)).unwrap(), Some(json!("1")));
        assert_eq!(
            routed(json!({}), None).unwrap(),
            Some(json!("1")),
            "both children are in workspace 1 now"
        );
        // 호스트에 없는 surface 는 후보가 아니다.
        returns.forget_child(40);
        host.workspace_of.borrow_mut().clear();
        assert_eq!(routed(json!({}), None).unwrap(), None);
    }

    /// 다른 스레드에서 쓰는 호스트. `codex.deliver` 는 들어왔다고 알린 뒤 시험이 풀어 줄 때까지 멈춘다.
    struct GatedHost {
        entered: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        delivered: Mutex<Vec<Value>>,
    }

    /// 시험이 쥐는 쪽. `entered` 로 deliver 진입을 보고 `release` 로 하나씩 풀어 준다.
    struct Gate {
        entered: std::sync::mpsc::Receiver<()>,
        release: std::sync::mpsc::SyncSender<()>,
    }

    /// 순서 단언이 기다리는 상한. 넘으면 그 순서가 오지 않은 것이다. 디스크 동기화가 느린 부하에서도
    /// 정상 진행이 이 안에 끝나도록 넉넉히 둔다.
    const ORDER_WAIT: Duration = Duration::from_secs(30);

    fn gated_host() -> (Arc<GatedHost>, Gate) {
        let (entered_tx, entered) = std::sync::mpsc::sync_channel(8);
        let (release, release_rx) = std::sync::mpsc::sync_channel(8);
        let host = Arc::new(GatedHost {
            entered: entered_tx,
            release: Mutex::new(release_rx),
            delivered: Mutex::new(Vec::new()),
        });
        (host, Gate { entered, release })
    }

    impl HostCall for GatedHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            if method == "codex.deliver" {
                self.entered.send(()).expect("the test watches deliveries");
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(ORDER_WAIT)
                    .expect("the test releases codex.deliver");
                self.delivered.lock().unwrap().push(params);
            }
            Ok(json!({ "event_id": "e", "delivery_state": "captured" }))
        }
    }

    /// 송신 스레드로 옮기는 공유 핸들.
    struct Shared(Arc<GatedHost>);

    impl HostCall for Shared {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            self.0.call(method, params)
        }
    }

    /// 송신 스레드가 `codex.deliver` 에 막혀 있는 동안에도 확정은 끝난다. 스레드는 시작할 때 남은
    /// 요청을 보내고, 플러그인이 끝나면(값이 사라지면) 멈추고 회수된다.
    ///
    /// 걸린 시간이 아니라 순서를 본다. 확정은 파일을 동기화해 기록하므로 디스크가 바쁘면 그 자체로
    /// 수백 ms~초가 걸리고, 그 시간은 deliver 를 기다렸는지와 관계없다.
    #[test]
    fn the_delivery_thread_keeps_hooks_unblocked_and_is_joined_on_drop() {
        let (dir, returns) = store();
        returns.bind_new_child(40, &b("r1"), true).unwrap();
        returns.observe("session-start", 40, Some("sA"), None);
        returns.turn_end(end(40, "sA", ReturnStatus::TurnEnded, "left from before"));
        drop(returns);

        let returns = Arc::new(Returns::new(Some(dir.path())));
        let (host, gate) = gated_host();
        let pump = ReturnPump::start(Arc::clone(&returns), Shared(Arc::clone(&host))).unwrap();
        // 시작한 송신이 남은 요청을 deliver 하다 멈췄다.
        gate.entered
            .recv_timeout(ORDER_WAIT)
            .expect("the pump delivers the result left from before");

        returns.observe("prompt-submit", 40, Some("sA"), None);
        let (done_tx, done) = std::sync::mpsc::sync_channel(1);
        let hook = {
            let returns = Arc::clone(&returns);
            std::thread::spawn(move || {
                returns.turn_end(end(40, "sA", ReturnStatus::TurnEnded, "new"));
                done_tx.send(()).expect("the test waits for turn_end");
            })
        };
        // deliver 를 풀기 전에 확정이 끝나야 한다. 확정이 deliver 를 기다리면 여기서 멈춘다.
        done.recv_timeout(ORDER_WAIT)
            .expect("turn_end finished while codex.deliver was blocked");
        assert!(
            host.delivered.lock().unwrap().is_empty(),
            "the first delivery is still blocked"
        );
        hook.join().expect("the hook thread");

        for _ in 0..2 {
            gate.release
                .send(())
                .expect("the pump waits in codex.deliver");
        }
        let deadline = std::time::Instant::now() + ORDER_WAIT;
        while host.delivered.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let bodies: Vec<Value> = host
            .delivered
            .lock()
            .unwrap()
            .iter()
            .map(|r| r["body"].clone())
            .collect();
        assert_eq!(bodies, [json!("left from before"), json!("new")]);

        // 쉬는 송신 스레드는 다음 차례까지 PUMP_IDLE_WAIT 만큼 기다린다. 멈춤 신호가 깨우지 않으면
        // 그만큼 걸리므로 그 절반을 상한으로 둔다.
        let stopping = std::time::Instant::now();
        drop(pump);
        assert!(
            stopping.elapsed() < PUMP_IDLE_WAIT / 2,
            "an idle delivery thread stops when the plugin ends: {:?}",
            stopping.elapsed()
        );
    }
}
