//! attach 원격 출처 부모의 턴 결과. 이 Tasty 는 부모의 수신처를 모르므로 결과를 서버 host 에 맡기고
//! (`attach.child_notice`), client 의 같은 plugin 이 그 부모의 수신처로 기록한다([`super::Returns::remote_receive`]).
//! 수신처는 Codex 대화(`codex.deliver`)이거나 물리 surface 부모다. 물리 부모의 결과는 본문을 먼저 저장하고 그 부모의
//! 완료 로그에 결과 ID 와 조회 명령을 남긴다([`tasty_plugin_agent_common::remote_results`]).
//!
//! 영속 요청은 client 가 기록을 확인한(`delivered`) 뒤나 받을 수 없다고 한(`rejected`) 뒤에만 지운다.
//! `pending` 은 송신 실패와 같이 같은 키로 늘어나는 간격을 두고 다시 묻는다.

use std::time::Duration;

use serde_json::Value;
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_agent_common::remote_attach::{self, NoticeStatus};
use tasty_plugin_agent_common::remote_owner;
use tasty_plugin_agent_common::remote_results;
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use super::{DELIVER_CALL_TIMEOUT, Returns, ms, now_ms};

/// client 가 받을 결과 본문의 키. 나머지 형식은 이 plugin 이 정하며 host 는 해석하지 않는다.
pub(crate) const PAYLOAD_KEY: &str = "completion";

/// attach 원격 부모로 갈 요청인가.
pub(super) fn is_attach(request: &Value) -> bool {
    request.get("attach").and_then(Value::as_bool) == Some(true)
}

/// 보내지 못한 요청의 처리.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Failure {
    /// 일시 실패나 아직 확인 전이다. 늘어나는 간격으로 다시 보낸다(기한이 지나면 포기한다).
    Retry(String),
    /// 받는 쪽이 영구히 받을 수 없다고 했다. 다시 보내지 않고 사유와 함께 포기한 요청으로 남긴다.
    Refused(String),
}

/// 요청을 서버 host 에 맡기거나 상태를 묻는다. client 가 기록했으면 `Ok` 이고 그때만 지운다.
pub(super) fn hand_over<H: HostCall>(host: &H, mut request: Value) -> Result<Value, Failure> {
    let surface = request
        .get("child_surface")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| Failure::Refused("attach result without child_surface".into()))?;
    let key = super::key_of(&request)
        .ok_or_else(|| Failure::Refused("attach result without completion_key".into()))?
        .to_string();
    if let Some(map) = request.as_object_mut() {
        map.remove("attach");
    }
    let payload = serde_json::json!({ PAYLOAD_KEY: request });
    match remote_attach::child_notice(host, surface, &key, payload).map_err(Failure::Retry)? {
        NoticeStatus::Delivered => Ok(serde_json::json!({ "attach": "delivered" })),
        NoticeStatus::Rejected(reason) => Err(Failure::Refused(reason)),
        NoticeStatus::Pending => Err(Failure::Retry(
            "waiting for the attach parent to record it".into(),
        )),
    }
}

/// client 에서 받은 원격 결과를 그 턴의 route 토큰이 가리키는 수신처로 넘길 `codex.deliver` 요청. 수신처가 없거나
/// 본문이 틀리면 거절 사유다.
pub(super) fn local_request(
    payload: &Value,
    surface: u32,
    receiver_id: Option<String>,
) -> Result<Value, String> {
    let mut request = payload
        .get(PAYLOAD_KEY)
        .filter(|c| c.is_object())
        .cloned()
        .ok_or("payload without a completion")?;
    let key = super::key_of(&request)
        .ok_or("completion without completion_key")?
        .to_string();
    let receiver_id = receiver_id
        .filter(|r| !r.is_empty())
        .ok_or_else(|| format!("no Codex return route for the result of s{surface}"))?;
    let map = request
        .as_object_mut()
        .ok_or("completion is not an object")?;
    map.remove("attach");
    map.remove(remote_attach::ROUTE_FIELD);
    map.insert("receiver_id".into(), Value::from(receiver_id));
    map.insert(
        "completion_key".into(),
        Value::from(remote_attach::local_completion_key(&key)),
    );
    map.insert("child_surface".into(), Value::from(surface));
    Ok(request)
}

/// 원격 자식에게 보낸 지시 하나의 route 토큰. 서버가 그 지시로 시작한 턴의 결과에 이 토큰을 실어 돌려준다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttachRoute {
    pub(super) token: String,
    /// Codex 대화 수신처. 물리 surface 부모면 비어 있다.
    pub(super) receiver_id: String,
    /// 물리 surface 부모. 결과를 저장하고 이 surface 의 완료 로그에 알린다.
    pub(super) parent_surface: Option<u32>,
    /// 발급 때 그 부모 surface 의 Claude 세션. 결과가 올 때 세션이 바뀌었으면 알리지 않는다.
    pub(super) parent_session: Option<String>,
    pub(super) issued_at_ms: u64,
}

/// route 토큰이 가리키는 수신처.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouteTo {
    /// Codex 대화(receiver_id).
    Codex(String),
    /// 물리 surface 부모와 발급 때 그 surface 의 Claude 세션(셸 등이면 없다).
    Surface {
        surface: u32,
        session: Option<String>,
    },
}

impl RouteTo {
    /// 이 수신처의 원격 자식 소유자([`remote_owner`]).
    pub(crate) fn owner(&self) -> String {
        match self {
            Self::Codex(r) => remote_owner::codex_owner(r),
            Self::Surface { surface, session } => {
                remote_owner::surface_owner(*surface, session.as_deref())
            }
        }
    }
}

/// 부모의 완료 로그(notify mod·Monitor 가 읽는 곳)에 한 줄을 쓴다.
#[cfg(not(test))]
fn append_line(surface: u32, line: &str) -> std::io::Result<()> {
    crate::notice::append(surface, crate::notice::NoticeKind::RemoteResult, line)
}

/// 시험은 실제 완료 로그(`TASTY_PARENT_HOME`)에 쓰지 않고 이 스레드에 모은다.
#[cfg(test)]
fn append_line(surface: u32, line: &str) -> std::io::Result<()> {
    let tagged = tasty_utils::notify::notice_line(
        crate::notice::NoticeKind::RemoteResult.notice_type(),
        line,
    );
    test_log::push(surface, &tagged)
}

/// 물리 부모의 완료 로그에 남기는 줄. 조회 명령을 그대로 실어 부모가 바로 본문을 읽을 수 있게 한다.
pub(crate) fn result_line(tr: &Translator, stored: &remote_results::Stored) -> String {
    tr.t_args(
        "claude.notify.remote_result_message",
        &[&stored.child_surface.to_string(), &stored.id, stored.status],
    )
}

/// route 토큰 기록을 두는 기간. 서버는 결과를 확인받을 때까지 다시 보내며 host 통지 보관은 24시간이다.
const ATTACH_ROUTE_KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// route 토큰 기록 상한. 넘으면 오래된 것부터 지운다.
const ATTACH_ROUTES_MAX: usize = 4096;

impl Returns {
    /// client: 서버에서 온 원격 자식 결과를 그 턴을 지시한 부모의 수신처로 기록한다(`claude.remote_receive`).
    /// 같은 결과의 재전송은 같은 키라 Codex 원장이나 결과 보관이 중복으로 거른다. 기록하지 못하면 오류로 답해
    /// host 가 나중에 다시 보내게 한다. 토큰 기록은 mirror 가 사라져도 남아 있으므로 늦게 온 결과도 기록한다.
    pub(crate) fn remote_receive<H: HostCall>(
        &self,
        host: &H,
        params: &Value,
        tr: &Translator,
    ) -> Result<Value, IpcMethodError> {
        use tasty_plugin_agent_common::remote_attach::{Received, received};
        let Some((surface, payload)) = received(params) else {
            return Ok(Received::Rejected("missing surface_id or payload".into()).json());
        };
        // 수신처는 지금의 경로가 아니라 그 턴을 지시한 spawn·tell 때 발급한 토큰으로 찾는다. 그 사이 경로가
        // 풀리거나 다른 부모로 묶여도 그 턴의 결과는 지시한 부모에게 간다.
        let token = payload
            .get(PAYLOAD_KEY)
            .and_then(|c| c.get(remote_attach::ROUTE_FIELD))
            .and_then(Value::as_str)
            .filter(|t| remote_attach::is_route_token(t));
        let to = match token {
            Some(token) if self.is_available() => {
                self.route_to(token).map_err(IpcMethodError::new)?
            }
            _ => None,
        };
        if let Some(RouteTo::Surface {
            surface: parent,
            session,
        }) = to
        {
            return self.receive_for_surface(
                host,
                parent,
                session.as_deref(),
                surface,
                payload,
                tr,
            );
        }
        let receiver = match to {
            Some(RouteTo::Codex(r)) => Some(r),
            _ => None,
        };
        let request = match local_request(payload, surface, receiver) {
            Ok(r) => r,
            Err(reason) => return Ok(Received::Rejected(reason).json()),
        };
        let response = host
            .call_within("codex.deliver", request, DELIVER_CALL_TIMEOUT)
            .map_err(IpcMethodError::from)?;
        let status = if response.get("duplicate").and_then(Value::as_bool) == Some(true) {
            Received::Duplicate
        } else {
            Received::Recorded
        };
        Ok(status.json())
    }

    /// 토큰의 수신처. 기록이 없거나 지워졌으면 `None`.
    fn route_to(&self, token: &str) -> Result<Option<RouteTo>, String> {
        self.update(|book| {
            let to = book
                .attach_routes
                .iter()
                .find(|r| r.token == token)
                .map(|r| match r.parent_surface {
                    Some(parent) => RouteTo::Surface {
                        surface: parent,
                        session: r.parent_session.clone(),
                    },
                    None => RouteTo::Codex(r.receiver_id.clone()),
                });
            (to, false)
        })
    }

    /// 물리 surface 부모의 결과: 본문을 먼저 저장하고, 그 부모의 완료 로그에 결과 ID 와 조회 명령을 남긴 뒤 답한다.
    /// 부모 surface 가 없거나 지시 뒤 부모 세션이 바뀌었으면 저장만 하고 알리지 않는다.
    fn receive_for_surface<H: HostCall>(
        &self,
        host: &H,
        parent: u32,
        session: Option<&str>,
        surface: u32,
        payload: &Value,
        tr: &Translator,
    ) -> Result<Value, IpcMethodError> {
        use tasty_plugin_agent_common::remote_attach::Received;
        let Some(completion) = payload.get(PAYLOAD_KEY).filter(|c| c.is_object()) else {
            return Ok(Received::Rejected("payload without a completion".into()).json());
        };
        let dir = self
            .results_dir()
            .ok_or_else(|| IpcMethodError::new(tr.t("claude.codex.no_data_dir")))?;
        let suppressed =
            remote_results::suppression(host, parent, session).map_err(IpcMethodError::new)?;
        remote_results::receive(&dir, parent, surface, completion, suppressed, |stored| {
            append_line(stored.parent_surface, &result_line(tr, stored))
                .map_err(|e| format!("completion log of s{}: {e}", stored.parent_surface))
        })
        .map(|r| r.json())
        .map_err(IpcMethodError::new)
    }

    /// 원격 자식의 소유자 기록 폴더(plugin 데이터 폴더의 [`remote_owner::DIR`]).
    pub(crate) fn owners_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.dir.as_deref()?.parent()?.join(remote_owner::DIR))
    }

    /// 물리 부모에게 온 원격 결과를 두는 폴더(plugin 데이터 폴더의 [`remote_results::DIR`]).
    pub(crate) fn results_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.dir.as_deref()?.parent()?.join(remote_results::DIR))
    }

    /// client: 원격 자식에게 보내는 spawn·tell 하나의 route 토큰을 발급한다. 토큰→수신처 기록은 경로를 풀거나 다시
    /// 묶어도 남고 [`ATTACH_ROUTE_KEPT`] 뒤에 지운다. 서버에는 토큰만 보낸다.
    pub(crate) fn issue_attach_route(&self, to: &RouteTo) -> Result<String, String> {
        if !self.is_available() {
            return Err("no plugin data directory".into());
        }
        let token = remote_attach::new_route_token();
        let now = now_ms();
        self.update(|book| {
            book.attach_routes
                .retain(|r| now.saturating_sub(r.issued_at_ms) <= ms(ATTACH_ROUTE_KEPT));
            let (receiver_id, parent_surface, parent_session) = match to {
                RouteTo::Codex(r) => (r.clone(), None, None),
                RouteTo::Surface { surface, session } => {
                    (String::new(), Some(*surface), session.clone())
                }
            };
            book.attach_routes.push(AttachRoute {
                token: token.clone(),
                receiver_id,
                parent_surface,
                parent_session,
                issued_at_ms: now,
            });
            if book.attach_routes.len() > ATTACH_ROUTES_MAX {
                let extra = book.attach_routes.len() - ATTACH_ROUTES_MAX;
                book.attach_routes.drain(..extra);
            }
            ((), true)
        })?;
        Ok(token)
    }

    /// client: 원격 자식이 닫혔다. 서버에 남은 결과가 늦게 올 수 있으므로 경로를 바로 지우지 않고 끝난 경로로
    /// 두어 [`ENDED_ROUTE_GRACE`] 뒤 지운다.
    pub(crate) fn end_remote_child(&self, child_surface: u32) {
        if !self.is_available() {
            return;
        }
        let now = now_ms();
        let result = self.update(|book| {
            let mut changed = false;
            for r in book
                .routes
                .iter_mut()
                .filter(|r| r.child_surface == child_surface && r.ended_at_ms.is_none())
            {
                r.ended_at_ms = Some(now);
                changed = true;
            }
            ((), changed)
        });
        if let Err(e) = result {
            tracing::warn!(
                "claude codex return: could not end the remote child s{child_surface}: {e}"
            );
        }
    }
}

/// `claude.remote_results`: 물리 부모에게 온 원격 결과. `id` 가 있으면 본문까지 담은 한 건, 없으면 본문을 뺀 목록이다.
pub(crate) fn handle_remote_results(
    returns: &Returns,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let dir = returns
        .results_dir()
        .ok_or_else(|| IpcMethodError::new(tr.t("claude.codex.no_data_dir")))?;
    remote_results::query(&dir, params).map_err(|e| match e {
        remote_results::QueryError::BadId(id) => {
            IpcMethodError::invalid_params(&tr.t_fmt("claude.remote.result_bad_id", &id))
        }
        remote_results::QueryError::NotFound(id) => {
            IpcMethodError::invalid_params(&tr.t_fmt("claude.remote.result_not_found", &id))
        }
        remote_results::QueryError::Io(m) => IpcMethodError::new(m),
    })
}

#[cfg(test)]
pub(crate) mod test_log {
    use std::cell::RefCell;

    thread_local! {
        static LINES: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
        static FAIL: RefCell<bool> = const { RefCell::new(false) };
    }

    pub(super) fn push(surface: u32, line: &str) -> std::io::Result<()> {
        if FAIL.with(|f| *f.borrow()) {
            return Err(std::io::Error::other("injected"));
        }
        LINES.with(|l| l.borrow_mut().push((surface, line.to_string())));
        Ok(())
    }

    /// 이 스레드에서 쓴 줄을 꺼낸다.
    pub(crate) fn take() -> Vec<(u32, String)> {
        LINES.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    /// 쓰기를 실패하게 한다.
    pub(crate) fn fail(on: bool) {
        FAIL.with(|f| *f.borrow_mut() = on);
    }
}
