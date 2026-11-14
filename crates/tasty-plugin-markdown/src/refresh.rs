//! 공통 새로고침 신호(`surface.refresh_requested`) 처리. 신호는 구독한 plugin 모두에게 가므로
//! 이 plugin 이 연 markdown 문서일 때만 갱신한다.

use serde_json::Value;

use crate::{MarkdownPlugin, MdDoc, REFRESH_REQUESTED_EVENT, ReloadOrigin, RemoteRequester};

/// 새로고침 신호가 가리키는 surface 와 그 kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RefreshTarget<'a> {
    pub(crate) surface_id: u32,
    pub(crate) kind: &'a str,
}

fn refresh_target(payload: &Value) -> Option<RefreshTarget<'_>> {
    Some(RefreshTarget {
        surface_id: u32::try_from(payload.get("surface_id")?.as_u64()?).ok()?,
        kind: payload.get("kind")?.as_str()?,
    })
}

/// 새로고침 신호에 대한 이 plugin 의 처리.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshAction {
    /// 다른 kind 거나 이 plugin 이 연 문서가 아니다.
    Ignore,
    /// mirror 문서의 원문을 다시 요청한다.
    Remote,
    /// 로컬 파일을 다시 읽는다.
    Local,
}

pub(crate) fn refresh_action(target: RefreshTarget<'_>, doc: Option<&MdDoc>) -> RefreshAction {
    match doc {
        _ if target.kind != "markdown" => RefreshAction::Ignore,
        None => RefreshAction::Ignore,
        Some(doc) if doc.remote.is_some() => RefreshAction::Remote,
        Some(_) => RefreshAction::Local,
    }
}

impl MarkdownPlugin {
    /// `surface.refresh_requested` — 사용자의 공통 새로고침. 이 plugin 이 연 markdown 문서만 갱신한다.
    /// 원격 문서는 서버 원문을 사용자 출처로 다시 요청하고, 로컬 문서는 문서 안 새로고침과 같은
    /// 대용량 확인을 거쳐 파일을 다시 읽는다.
    pub(crate) fn on_refresh_requested(&mut self, payload: &Value) {
        let Some(target) = refresh_target(payload) else {
            tracing::warn!("markdown: malformed {REFRESH_REQUESTED_EVENT} event");
            return;
        };
        match refresh_action(target, self.docs.get(&target.surface_id)) {
            RefreshAction::Ignore => {}
            RefreshAction::Remote => {
                self.request_remote_content(target.surface_id, RemoteRequester::User)
            }
            RefreshAction::Local => self.reload_local(target.surface_id, ReloadOrigin::Explicit),
        }
    }
}
