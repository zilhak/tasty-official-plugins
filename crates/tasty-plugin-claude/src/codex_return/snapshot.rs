//! 지시 전 자식 경로의 저장과 복구. 확정 실패한 tell 이 바꾼 경로를 이전 상태로 되돌린다.

use super::{Returns, Route};

/// 지시 전에 저장한 자식의 경로들.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildRoutes(Vec<Route>);

impl Returns {
    /// 자식의 지금 경로들. 확정 실패한 지시 뒤 [`Self::restore_child`] 로 되돌린다.
    pub(crate) fn child_routes(&self, child_surface: u32) -> Result<ChildRoutes, String> {
        if !self.is_available() {
            return Ok(ChildRoutes(Vec::new()));
        }
        self.update(|book| {
            let saved = book
                .routes
                .iter()
                .filter(|r| r.child_surface == child_surface)
                .cloned()
                .collect();
            (ChildRoutes(saved), false)
        })
    }

    /// 자식의 경로를 [`Self::child_routes`] 로 저장한 상태로 되돌린다.
    pub(crate) fn restore_child(
        &self,
        child_surface: u32,
        saved: ChildRoutes,
    ) -> Result<(), String> {
        if !self.is_available() {
            return Ok(());
        }
        self.update(|book| {
            book.routes.retain(|r| r.child_surface != child_surface);
            book.routes.extend(saved.0);
            ((), true)
        })
    }
}
