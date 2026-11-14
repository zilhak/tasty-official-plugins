//! 주소창 이동 신호의 해석과 원격 주소창 이동 결과의 처리.
//! 원격 이동의 최종 결과는 `run.result` 로 읽고, 실패는 주소창 오류 상태로 보인다
//! (docs/surfaces/markdown/index.md 의 attach mirror 문서 절).

use serde_json::{Value, json};
use tasty_plugin_sdk::HostHandle;

use crate::{MarkdownPlugin, MdDoc, RemoteRequester, render};

/// 원격 주소창 이동의 최종 결과를 워커에 돌려주는 자기 호출 메서드. 외부 호출자는 쓸 수 없다.
pub(crate) const ADDR_RESULT_METHOD: &str = "markdown.address_result";

/// 원격 주소창 이동의 `run.result` 를 묻는 첫 간격·최대 간격과 대기 행을 보이기까지의 시간.
/// 간격은 두 배씩 늘린다. host 의 slow_loop 탐지는 같은 run 을 5분에 20번 물으면 사용자에게 경고하므로,
/// 어느 5분 구간에서도 그 수에 닿지 않게 한다([`poll_delays`] 시험).
const ADDR_RESULT_POLL: std::time::Duration = std::time::Duration::from_millis(200);
const ADDR_RESULT_POLL_MAX: std::time::Duration = std::time::Duration::from_secs(3);
pub(crate) const ADDR_WAITING_AFTER: std::time::Duration =
    std::time::Duration::from_millis(render::ADDR_WAITING_AFTER_MS);
/// 대기 행을 보인 뒤에도 늦은 결과를 반영하려고 이 간격으로 계속 묻는다. 시간 상한은 두지 않는다.
/// run 이 끝나거나(연결이 끝나면 host 가 `unknown` 으로 끝낸다) host 가 그 run 을 더 모르면 그친다.
const ADDR_RESULT_LATE_POLL: std::time::Duration = std::time::Duration::from_secs(60);

/// `run.result` 를 다시 묻기 전 기다릴 간격들. 대기 행 시점까지는 짧게, 그 뒤로는 길게 묻는다. 끝이 없다.
pub(crate) fn poll_delays() -> impl Iterator<Item = std::time::Duration> {
    let mut next = ADDR_RESULT_POLL;
    let mut total = std::time::Duration::ZERO;
    std::iter::from_fn(move || {
        let d = if total >= ADDR_WAITING_AFTER {
            ADDR_RESULT_LATE_POLL
        } else {
            next
        };
        total += d;
        next = (next * 2).min(ADDR_RESULT_POLL_MAX);
        Some(d)
    })
}

/// 원격 이동 결과를 반영한 뒤 host 에 보낼 일.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddrEffect {
    /// 원격이 바꾼 문서의 원문과 경로를 다시 받는다.
    Refetch,
    /// 오류 행이나 대기 행이 바뀌어 문서를 다시 그린다.
    Redraw,
    Nothing,
}

/// 원격 주소창 이동의 최종 결과(`run.result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AddrOutcome {
    Succeeded,
    Failed(render::AddrFailureKind),
}

impl AddrOutcome {
    /// 자기 호출에 싣는 (결과, 원격 사유). 사유는 원격 거절에만 있다.
    pub(crate) fn as_wire(&self) -> (&'static str, Option<&str>) {
        use render::AddrFailureKind as K;
        match self {
            AddrOutcome::Succeeded => ("succeeded", None),
            AddrOutcome::Failed(K::NotFound) => ("not_found", None),
            AddrOutcome::Failed(K::Rejected(reason)) => ("rejected", Some(reason)),
            AddrOutcome::Failed(K::ConnectionLost) => ("connection_lost", None),
            AddrOutcome::Failed(K::Cancelled) => ("cancelled", None),
            AddrOutcome::Failed(K::Internal) => ("internal", None),
        }
    }

    pub(crate) fn from_wire(s: &str, reason: Option<&str>) -> Option<Self> {
        use render::AddrFailureKind as K;
        Some(match s {
            "succeeded" => AddrOutcome::Succeeded,
            "not_found" => AddrOutcome::Failed(K::NotFound),
            "rejected" => AddrOutcome::Failed(K::rejected(reason.unwrap_or_default())),
            "connection_lost" => AddrOutcome::Failed(K::ConnectionLost),
            "cancelled" => AddrOutcome::Failed(K::Cancelled),
            "internal" => AddrOutcome::Failed(K::Internal),
            _ => return None,
        })
    }
}

/// `run.result` 응답을 분류한다. 끝나지 않았으면 None이다. 파일 없음은 `reason_code` 로만 정하고
/// 결과 문장을 해석하지 않는다. 원격이 사유를 주고 거절했으면 그 사유 원문(`remote_reason`)을,
/// 결과 전에 연결이 끝났으면(`unknown`) 연결 끊김을, 보내기 전에 취소됐으면 취소를 고른다.
/// 그 밖의 실패는 요청이 원격에 닿지 않은 내부 실패다.
pub(crate) fn classify_navigate_run(v: &Value) -> Option<AddrOutcome> {
    use render::AddrFailureKind as K;
    if v.get("done").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let remote_reason = v.get("remote_reason").and_then(Value::as_str);
    Some(match v.get("state").and_then(Value::as_str) {
        Some("succeeded") => AddrOutcome::Succeeded,
        Some("failed") if v.get("reason_code").and_then(Value::as_str) == Some("not_found") => {
            AddrOutcome::Failed(K::NotFound)
        }
        Some("failed") => AddrOutcome::Failed(K::rejected(remote_reason.unwrap_or_default())),
        Some("unknown") => AddrOutcome::Failed(K::ConnectionLost),
        Some("cancelled") => AddrOutcome::Failed(K::Cancelled),
        _ => AddrOutcome::Failed(K::Internal),
    })
}

/// WebView 이동 요청 하나에 plugin 이 할 일.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NavAction {
    Ignore,
    /// mirror 문서의 원문을 다시 요청한다.
    RefetchRemote,
    /// 대용량 대기 상태의 로컬 문서를 다시 연다.
    OpenDeferred,
    /// mirror 문서의 외부 링크.
    External(String),
    /// 로컬 문서의 링크.
    Link(String),
    /// 같은 surface 를 원격 경로로 바꾼다. 실패는 주소창 오류 상태로 보인다.
    NavigateRemote(String),
    /// 같은 surface 를 로컬 경로로 바꾼다.
    NavigateLocal(String),
    /// 주소창 오류 행이 지워졌다.
    DismissAddr,
    /// 주소창 폴더 버튼. 이 폴더(없으면 host 기본 위치)에서 파일 피커를 연다.
    Browse(Option<String>),
}

/// 이동 요청을 문서 종류에 맞는 동작으로 바꾼다. mirror 문서의 파일 링크는 원격 호스트의 경로라
/// 이 머신에서 열지 않는다. 주소창 경로는 원격으로 보내 원격이 존재를 판정한다.
pub(crate) fn nav_action(intent: render::NavIntent, doc: Option<&MdDoc>) -> NavAction {
    let remote = doc.and_then(|d| d.remote.as_ref());
    match (intent, remote) {
        (render::NavIntent::Refresh, Some(_)) => NavAction::RefetchRemote,
        // 로컬 문서에서는 대용량 대기 상태의 "Open file…" 버튼만 이 요청을 낸다.
        (render::NavIntent::Refresh, None) => NavAction::OpenDeferred,
        (render::NavIntent::Link(dest), Some(_)) => match render::classify_link(&dest, None) {
            Some(render::LinkClick::External(url)) => NavAction::External(url),
            _ => NavAction::Ignore,
        },
        (render::NavIntent::Link(dest), None) => NavAction::Link(dest),
        // 끊긴 문서의 주소창은 읽기 전용이다. 늦게 온 신호도 보내지 않는다.
        (render::NavIntent::Addr(_), Some(r)) if r.disconnected => NavAction::Ignore,
        (render::NavIntent::Addr(path), Some(_)) => NavAction::NavigateRemote(path),
        (render::NavIntent::Addr(path), None) => NavAction::NavigateLocal(path),
        (render::NavIntent::AddrDismiss, _) => NavAction::DismissAddr,
        (render::NavIntent::Browse, Some(r)) if r.disconnected => NavAction::Ignore,
        (render::NavIntent::Browse, _) => match doc {
            Some(d) => NavAction::Browse(crate::browse::start_dir(d)),
            None => NavAction::Ignore,
        },
    }
}

/// 원격 주소창 이동의 `run.result` 를 기다려 결과를 워커에 자기 호출로 돌려준다. 대기 행을 보인
/// 뒤에도 늦은 결과를 반영하려고 run 이 끝날 때까지 묻는다. 결과를 알 수 없게 되면 실패로 바꾸지 않고
/// 기록만 남긴다.
pub(crate) fn spawn_addr_watch(
    host: HostHandle,
    sid: u32,
    ticket: u64,
    typed: String,
    run: u64,
    browse: bool,
) {
    let spawned = std::thread::Builder::new()
        .name("markdown-addr-result".to_string())
        .spawn(move || {
            let Some(outcome) = wait_navigate_run(&host, sid, run) else {
                return;
            };
            let (wire, reason) = outcome.as_wire();
            let params = json!({
                "surface_id": sid,
                "ticket": ticket,
                "path": typed,
                "outcome": wire,
                "reason": reason,
                "browse": browse,
            });
            if let Err(e) = host.self_invoke(ADDR_RESULT_METHOD, params) {
                tracing::warn!(
                    "markdown surface {sid}: could not hand back the navigate result: {e}"
                );
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("markdown surface {sid}: navigate result watcher spawn failed: {e}");
    }
}

/// `run.result` 를 끝날 때까지 묻는다. 첫 조회가 실패하면 host 가 요청을 받지 못한 내부 실패다.
/// 진행 중이라는 답을 받은 뒤의 조회 실패는 host 가 그 run 을 더 모르는 것이라(오래된 run 정리)
/// 결과를 알 수 없어 None 이다.
fn wait_navigate_run(host: &HostHandle, sid: u32, run: u64) -> Option<AddrOutcome> {
    let mut delays = poll_delays();
    let mut seen = false;
    loop {
        match host.call("run.result", json!({ "run": run })) {
            Ok(v) => {
                if let Some(outcome) = classify_navigate_run(&v) {
                    return Some(outcome);
                }
                seen = true;
            }
            Err(e) if seen => {
                tracing::warn!(
                    "markdown surface {sid}: run.result {run} is no longer known — the result cannot be shown: {e}"
                );
                return None;
            }
            Err(e) => {
                tracing::warn!("markdown surface {sid}: run.result {run} failed: {e}");
                return Some(AddrOutcome::Failed(render::AddrFailureKind::Internal));
            }
        }
        let delay = delays.next().unwrap_or(ADDR_RESULT_LATE_POLL);
        std::thread::sleep(delay);
    }
}

impl MarkdownPlugin {
    /// [`ADDR_RESULT_METHOD`] 자기 호출. 닫혔거나 바뀐 문서, 더 새 이동이 있는 문서면 버린다.
    pub(crate) fn on_addr_result(&mut self, params: &Value) {
        let reason = params.get("reason").and_then(Value::as_str);
        let parsed = (
            params.get("surface_id").and_then(Value::as_u64),
            params.get("ticket").and_then(Value::as_u64),
            params.get("path").and_then(Value::as_str),
            params
                .get("outcome")
                .and_then(Value::as_str)
                .and_then(|o| AddrOutcome::from_wire(o, reason)),
        );
        let (Some(sid), Some(ticket), Some(path), Some(outcome)) = parsed else {
            tracing::warn!("markdown: malformed {ADDR_RESULT_METHOD} params: {params}");
            return;
        };
        let browse = params.get("browse").and_then(Value::as_bool) == Some(true);
        self.apply_addr_outcome(sid as u32, ticket, path.to_string(), outcome, browse);
    }

    /// 원격 이동 결과를 반영한다. 성공하면 원격이 바꾼 문서의 원문과 경로를 다시 받는다.
    /// 폴더 버튼(`browse`)의 실패는 host 가 사용자 toast 로 알리므로 주소창 오류로 만들지 않는다.
    pub(crate) fn apply_addr_outcome(
        &mut self,
        sid: u32,
        ticket: u64,
        typed: String,
        outcome: AddrOutcome,
        browse: bool,
    ) {
        match self.settle_addr_outcome(sid, ticket, typed, outcome, browse) {
            AddrEffect::Refetch => self.request_remote_content(sid, RemoteRequester::User),
            AddrEffect::Redraw => self.reload_webview(sid),
            AddrEffect::Nothing => {}
        }
    }

    /// [`Self::apply_addr_outcome`] 의 상태 반영. host 에 보낼 일은 돌려주기만 한다.
    pub(crate) fn settle_addr_outcome(
        &mut self,
        sid: u32,
        ticket: u64,
        typed: String,
        outcome: AddrOutcome,
        browse: bool,
    ) -> AddrEffect {
        let Some(doc) = self.docs.get_mut(&sid) else {
            return AddrEffect::Nothing;
        };
        if doc.remote.as_ref().map(|r| r.addr_ticket) != Some(ticket) {
            return AddrEffect::Nothing;
        }
        let succeeded = outcome == AddrOutcome::Succeeded;
        let redraw = match (browse, outcome) {
            (true, AddrOutcome::Failed(_)) => false,
            (_, outcome) => doc.apply_addr_outcome(ticket, typed, outcome),
        };
        if succeeded {
            AddrEffect::Refetch
        } else if redraw {
            AddrEffect::Redraw
        } else {
            AddrEffect::Nothing
        }
    }

    /// 원격 주소창 직접 이동을 보냈다. 결과가 오기 전에 문서를 다시 그려도 입력값과 대기 행을 잇는다.
    pub(crate) fn begin_addr_waiting(&mut self, sid: u32, ticket: u64, typed: &str) {
        if let Some(remote) = self.docs.get_mut(&sid).and_then(|d| d.remote.as_mut()) {
            remote.addr_waiting = Some(AddrWaiting {
                ticket,
                typed: typed.to_string(),
                since: std::time::Instant::now(),
            });
        }
    }
}

impl crate::RemoteDoc {
    /// 페이지가 오류 행이나 대기 행을 지웠다(다음 입력·Esc). 보관한 실패와 대기 상태를 버린다.
    /// 순번은 그대로라 늦게 온 성공은 계속 반영하고, 늦게 온 실패는 다시 보이지 않는다.
    pub(crate) fn dismiss_addr(&mut self) {
        self.addr_failure = None;
        self.addr_waiting = None;
        self.addr_dismissed = Some(self.addr_ticket);
    }

    /// 폴더 버튼으로 고른 이동이 마지막 이동이 된다. 새 순번과, 결과를 기다리던 직접 이동을 대신했는지를
    /// 돌려준다. 대신했으면 페이지의 대기 행 타이머와 이미 보인 대기 행이 남지 않게 다시 그려야 한다.
    pub(crate) fn replace_with_picked(&mut self) -> (u64, bool) {
        self.addr_ticket += 1;
        (self.addr_ticket, self.addr_waiting.take().is_some())
    }
}

/// 원격 주소창 직접 이동이 결과를 기다리는 중이다. 페이지가 [`ADDR_WAITING_AFTER`] 뒤 대기 행을 스스로
/// 띄우므로, plugin 은 그사이 문서를 다시 그릴 때만 이 값으로 입력값과 남은 시간을 넘긴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddrWaiting {
    pub(crate) ticket: u64,
    pub(crate) typed: String,
    pub(crate) since: std::time::Instant,
}

impl AddrWaiting {
    /// 다시 그릴 문서에 실을 대기 상태. 대기 행까지 남은 시간이 0이면 행을 바로 그린다.
    pub(crate) fn view(&self) -> render::AddrWaitingView {
        let left = ADDR_WAITING_AFTER.saturating_sub(self.since.elapsed());
        render::AddrWaitingView {
            typed: self.typed.clone(),
            after_ms: u64::try_from(left.as_millis()).unwrap_or(u64::MAX),
        }
    }
}
