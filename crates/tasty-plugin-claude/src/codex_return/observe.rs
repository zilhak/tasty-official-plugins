//! hook 을 결과 경로에 반영하는 단계. [`super::Returns::observe`] 가 경로를 찾고, 세션·surface 를 따라가고,
//! 턴·세션 상태를 남기는 순서로 부른다.

use super::{Book, Route, WAITING_KEPT};

impl Route {
    /// SessionEnd 보다 먼저 온 새 세션일 수도, 늦게 온 예전 시작일 수도 있는 세션을 후보로 둔다. 먼저
    /// 새 프롬프트를 받은 후보가 잇는다. 후보끼리 서로 밀어내지 않는다. 새로 넣었으면 `true`.
    pub(super) fn wait_for(&mut self, surface: u32, session: &str) -> bool {
        if self.waiting.iter().any(|w| w == session) {
            return false;
        }
        tracing::info!(
            "claude codex return: s{surface} session {session} started before session {:?} ended; waiting for its new prompt",
            self.child_session
        );
        self.waiting.push(session.to_string());
        if self.waiting.len() > WAITING_KEPT {
            self.waiting.remove(0);
        }
        true
    }

    /// hook 의 세션·surface 를 경로에 반영한다. 옛 surface 에서 늦게 온 hook 이면 `false` 를 돌려주며
    /// 그 hook 은 더 반영하지 않는다.
    pub(super) fn follow_hook(
        &mut self,
        event: &str,
        surface: u32,
        session: &str,
        detail: Option<&str>,
        changed: &mut bool,
    ) -> bool {
        if self.follow_session(surface, session, detail) {
            *changed = true;
        }
        if self.child_surface != surface {
            if event != "session-start" {
                tracing::debug!(
                    "claude codex return: ignored a late {event} of session {session} from s{surface}; the route is on s{}",
                    self.child_surface
                );
                return false;
            }
            tracing::info!(
                "claude codex return: session moved from s{} to s{surface}",
                self.child_surface
            );
            self.child_surface = surface;
            *changed = true;
        }
        true
    }

    /// hook 의 세션이 이 경로의 세션이 아니면 그 세션에 묶는다. 묶었으면 `true`.
    fn follow_session(&mut self, surface: u32, session: &str, detail: Option<&str>) -> bool {
        if self.child_session.as_deref() == Some(session) {
            return false;
        }
        if self.child_session.is_some() {
            tracing::info!(
                "claude codex return: s{surface} continues its route in session {session} after {:?}",
                detail
            );
        }
        self.bind_session(session);
        true
    }

    /// hook 이 뜻하는 턴·세션 상태를 경로에 남긴다. 바뀌었으면 `true`.
    pub(super) fn record_hook(&mut self, event: &str, detail: Option<&str>, now: u64) -> bool {
        match event {
            "session-start" => {
                if self.ended_at_ms.take().is_some() || self.handoff {
                    self.handoff = false;
                    return true;
                }
                false
            }
            "prompt-submit" => {
                self.turn += 1;
                self.open = true;
                true
            }
            "session-end" if matches!(detail, Some("clear" | "resume")) => {
                self.handoff = true;
                true
            }
            "session-end" => {
                self.ended_at_ms = Some(now);
                true
            }
            _ => false,
        }
    }
}

impl Book {
    /// hook 이 닿는 경로. 기다리는 후보로만 남긴 세션이나 닿는 경로가 없으면 `None`.
    pub(super) fn route_for_hook(
        &mut self,
        event: &str,
        surface: u32,
        session: &str,
        detail: Option<&str>,
        changed: &mut bool,
    ) -> Option<usize> {
        let continuation = event == "session-start" && matches!(detail, Some("clear" | "resume"));
        let mut found = self
            .by_session(session)
            .or_else(|| self.adoptable(surface, event));
        if found.is_none() && continuation {
            if let Some(i) = self
                .handing_off(surface)
                .filter(|&i| !self.routes[i].is_stale_clear_start(session, detail))
            {
                found = Some(i);
            } else if let Some(i) = self
                .live_on(surface, session)
                .filter(|&i| !self.routes[i].is_stale_clear_start(session, detail))
            {
                if self.routes[i].wait_for(surface, session) {
                    *changed = true;
                }
                return None;
            }
        }
        // 기다리는 세션은 새 프롬프트로만 잇는다. 늦게 온 예전 세션의 Stop·SessionEnd 같은 hook 은
        // 새 턴의 근거가 아니므로 경로를 가져가지 못한다.
        if found.is_none() && event == "prompt-submit" {
            found = self
                .routes
                .iter()
                .position(|r| r.child_surface == surface && r.waiting.iter().any(|w| w == session));
        }
        found
    }
}
