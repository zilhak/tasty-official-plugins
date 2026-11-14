//! 플러그인이 다시 시작된 뒤 정지 감시 대상을 다시 채운다.
//!
//! 감시 대상과 대기 기록은 플러그인 메모리에만 있어 재시작(업데이트·disable/enable)으로 사라진다. launch·spawn·respawn
//! 은 등록 경로를 surface meta 에 남기고, 시작할 때 그 meta 와 대기 meta 로 감시 대상과 대기 기록을 되살린다.

use std::sync::Mutex;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use crate::error_scan::{ErrorScanner, ScanTarget, lock_scanner, saved_wait_from_meta};

/// 감시 등록 경로를 남기는 surface meta.
pub(crate) const SCAN_TARGET_META_KEY: &str = "claude-scan-target";

/// SessionStart 훅이 남기는 Claude 세션 meta.
const SESSION_META_KEY: &str = "claude-session-id";

fn target_name(target: ScanTarget) -> &'static str {
    match target {
        ScanTarget::TopLevel => "top-level",
        ScanTarget::Child => "child",
    }
}

fn parse_target(name: &str) -> Option<ScanTarget> {
    match name {
        "top-level" => Some(ScanTarget::TopLevel),
        "child" => Some(ScanTarget::Child),
        _ => None,
    }
}

/// 감시 대상으로 등록하고 등록 경로를 meta 에 남긴다. meta 기록 실패는 재시작 뒤 복원만 놓치므로 경고로 둔다.
pub(crate) fn enable_and_record<H: HostCall>(
    host: &H,
    scanner: &Mutex<ErrorScanner>,
    surface_id: u32,
    target: ScanTarget,
) {
    lock_scanner(scanner).enable(surface_id, target);
    if let Err(e) = host.call(
        "surface.meta.set",
        json!({ "surface_id": surface_id, "key": SCAN_TARGET_META_KEY, "value": target_name(target) }),
    ) {
        tracing::warn!("claude scan s{surface_id}: recording the scan target failed: {e}");
    }
}

fn meta_text<H: HostCall>(host: &H, surface_id: u32, key: &str) -> Option<String> {
    host.call(
        "surface.meta.get",
        json!({ "surface_id": surface_id, "key": key }),
    )
    .ok()?
    .get("value")?
    .as_str()
    .map(String::from)
}

/// 남은 등록 경로. meta 가 없어도 Claude 세션 meta 와 부모 관계가 있으면 자식으로 본다. 등록 경로를 남기기 전
/// 버전의 플러그인이 spawn 한 자식도 업데이트 뒤 다시 감시하려는 것이다.
fn saved_target<H: HostCall>(host: &H, surface_id: u32) -> Option<ScanTarget> {
    if let Some(name) = meta_text(host, surface_id, SCAN_TARGET_META_KEY) {
        return parse_target(&name);
    }
    meta_text(host, surface_id, SESSION_META_KEY)?;
    let parent = host
        .call("terminal.parent", json!({ "surface": surface_id }))
        .ok()?;
    let has_parent = parent.get("status").and_then(Value::as_str) != Some("none");
    has_parent.then_some(ScanTarget::Child)
}

/// 재시작 전에 남긴 정지 알림 시각(Unix ms). 없거나 읽을 수 없으면 `None` 이다.
fn stall_notice_at<H: HostCall>(host: &H, surface_id: u32) -> Option<u64> {
    let raw = meta_text(host, surface_id, crate::error_scan::STALL_NOTICE_META_KEY)?;
    serde_json::from_str::<Value>(&raw)
        .inspect_err(|e| tracing::warn!("claude stall notice meta is not JSON ({e}): {raw}"))
        .ok()?
        .get("at_ms")?
        .as_u64()
}

/// 모든 터미널을 훑어 감시 대상과 대기 기록을 되살린다. 되살린 surface 수를 돌려준다.
/// 이미 등록된 대상과 이미 있는 대기 기록은 덮어쓰지 않는다.
pub(crate) fn retrack<H: HostCall>(host: &H, scanner: &Mutex<ErrorScanner>) -> usize {
    let surfaces = match host.call("surface.list", json!({})) {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("claude retrack: surface.list failed: {e}");
            return 0;
        }
    };
    let ids: Vec<u32> = surfaces
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.get("id").and_then(Value::as_u64))
                .map(|id| id as u32)
                .collect()
        })
        .unwrap_or_default();
    ids.into_iter()
        .filter(|&surface_id| retrack_one(host, scanner, surface_id))
        .count()
}

/// surface 하나를 되살린다. 감시 대상이 아니거나 추적 유지 판단을 통과하지 못하면 `false` 다.
fn retrack_one<H: HostCall>(host: &H, scanner: &Mutex<ErrorScanner>, surface_id: u32) -> bool {
    let Some(target) = saved_target(host, surface_id) else {
        return false;
    };
    if !crate::error_scan::scan_target_is_alive(host, surface_id, target) {
        return false;
    }
    let saved = saved_wait_from_meta(host, surface_id);
    let waiting = saved.as_ref().map(|w| (w.notified, w.watch.is_some()));
    let notice = stall_notice_at(host, surface_id);
    {
        let mut s = lock_scanner(scanner);
        if !s.is_enabled(surface_id) {
            s.enable(surface_id, target);
        }
        if let Some(saved) = saved {
            s.restore_background_wait(surface_id, saved);
        }
        if let Some(at_ms) = notice {
            s.restore_stall_notice(surface_id, at_ms, std::time::Instant::now());
        }
    }
    tracing::info!(
        "claude retrack s{surface_id}: watching again as {} (background wait: {waiting:?}, stall notice at: {notice:?})",
        target_name(target)
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// surface 목록·meta·부모 관계만 답하는 호스트.
    #[derive(Default)]
    struct ListHost {
        surfaces: Vec<u32>,
        meta: RefCell<HashMap<(u32, String), String>>,
        /// 부모가 있는 surface.
        children: Vec<u32>,
        /// 터미널이 남아 있는 surface.
        alive: Vec<u32>,
    }

    impl ListHost {
        fn with_meta(mut self, surface_id: u32, key: &str, value: &str) -> Self {
            self.meta
                .get_mut()
                .insert((surface_id, key.to_string()), value.to_string());
            self
        }
    }

    impl HostCall for ListHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            let sid = params["surface_id"]
                .as_u64()
                .or(params["surface"].as_u64())
                .unwrap_or_default() as u32;
            Ok(match method {
                "surface.list" => json!(
                    self.surfaces
                        .iter()
                        .map(|id| json!({ "id": id }))
                        .collect::<Vec<_>>()
                ),
                "surface.meta.get" => self
                    .meta
                    .borrow()
                    .get(&(sid, params["key"].as_str().unwrap().to_string()))
                    .map(|v| json!({ "value": v }))
                    .unwrap_or_else(|| json!({})),
                "surface.meta.set" => {
                    self.meta.borrow_mut().insert(
                        (sid, params["key"].as_str().unwrap().to_string()),
                        params["value"].as_str().unwrap().to_string(),
                    );
                    json!({})
                }
                "terminal.parent" if self.children.contains(&sid) => {
                    json!({ "status": "ok", "parent": 1 })
                }
                "terminal.parent" => json!({ "status": "none" }),
                "surface.locate" => json!({ "exists": self.alive.contains(&sid) }),
                other => panic!("unexpected host call: {other}"),
            })
        }
    }

    /// 등록 경로를 남긴 터미널과 그 대기 기록을 되살린다. 이미 알린 대기는 알린 상태로 돌아온다.
    #[test]
    fn surfaces_with_a_recorded_target_are_watched_again() {
        let recorder = ListHost::default();
        let first = Mutex::new(ErrorScanner::new());
        enable_and_record(&recorder, &first, 5, ScanTarget::Child);
        enable_and_record(&recorder, &first, 6, ScanTarget::TopLevel);
        let host = ListHost {
            surfaces: vec![4, 5, 6],
            children: vec![5],
            alive: vec![4, 5, 6],
            meta: recorder.meta,
        }
        .with_meta(
            6,
            crate::error_scan::STALL_NOTICE_META_KEY,
            r#"{"at_ms":1000}"#,
        )
        .with_meta(
            5,
            crate::hook::BACKGROUND_WAIT_META_KEY,
            r#"{"since_ms":1000,"types":["shell"],"notified":true}"#,
        );
        let scanner = Mutex::new(ErrorScanner::new());
        assert_eq!(retrack(&host, &scanner), 2);
        let s = lock_scanner(&scanner);
        assert_eq!(s.target_of(5), Some(ScanTarget::Child));
        assert_eq!(s.target_of(6), Some(ScanTarget::TopLevel));
        assert!(!s.is_enabled(4), "Claude 기록이 없는 터미널");
        assert!(s.is_waiting_on_background_work(5));
        assert!(!s.is_waiting_on_background_work(6));
        assert!(s.stall_notified_for_test(6), "남긴 정지 알림");
        assert!(!s.stall_notified_for_test(5));
    }

    /// 등록 경로를 남기기 전 버전이 spawn 한 자식은 세션 meta 와 부모 관계로 찾는다. 부모가 없는 Claude 는
    /// 사용자가 직접 실행한 것일 수 있어 감시하지 않는다.
    #[test]
    fn a_child_spawned_before_the_target_meta_is_found_by_its_session_and_parent() {
        let host = ListHost {
            surfaces: vec![7, 8],
            children: vec![7],
            alive: vec![7, 8],
            ..Default::default()
        }
        .with_meta(7, SESSION_META_KEY, "uuid-7")
        .with_meta(8, SESSION_META_KEY, "uuid-8");
        let scanner = Mutex::new(ErrorScanner::new());
        assert_eq!(retrack(&host, &scanner), 1);
        assert_eq!(lock_scanner(&scanner).target_of(7), Some(ScanTarget::Child));
        assert!(!lock_scanner(&scanner).is_enabled(8));
    }

    /// 부모 관계가 풀린 자식(release)은 되살리지 않는다. 폴링이 곧바로 빼는 대상이다.
    #[test]
    fn a_released_child_is_not_watched_again() {
        let host = ListHost {
            surfaces: vec![9],
            alive: vec![9],
            ..Default::default()
        }
        .with_meta(9, SCAN_TARGET_META_KEY, "child");
        let scanner = Mutex::new(ErrorScanner::new());
        assert_eq!(retrack(&host, &scanner), 0);
        assert!(!lock_scanner(&scanner).is_enabled(9));
    }
}
