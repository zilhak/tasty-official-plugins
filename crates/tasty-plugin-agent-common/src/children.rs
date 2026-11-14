//! 호스트 `terminal.children` 응답을 읽는 헬퍼.

use serde_json::{Value, json};

use crate::host_call::HostCall;

/// 자식 상태를 읽는다. 상태가 없으면 None으로 남겨 알려진 상태와 구분한다.
pub fn state_of(child: &Value) -> Option<&str> {
    child.get("state").and_then(|s| s.as_str())
}

/// 조건을 만족하는 자식들의 `index` 목록.
pub fn indices_with(children: &[Value], pred: impl Fn(&Value) -> bool) -> Vec<u64> {
    children
        .iter()
        .filter(|c| pred(c))
        .filter_map(|c| c.get("index").and_then(|i| i.as_u64()))
        .collect()
}

/// index 목록을 사람이 읽는 한 줄로.
pub fn join_indices(indices: &[u64]) -> String {
    indices
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// spawn_child_warn_threshold 설정을 읽지 못했을 때 쓸 기본 경고 기준.
pub const DEFAULT_SPAWN_CHILD_WARN_THRESHOLD: f64 = 6.0;

/// spawn 경고에 필요한 자식 수와 상태. 안내문은 각 플러그인의 번역으로 만든다.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnCensus {
    pub total: usize,
    pub idle: Vec<u64>,
    /// confirmed 상태인 stale 자식만 포함한다.
    pub stale: Vec<u64>,
    pub threshold: f64,
}

/// 자식 상태와 경고 기준을 읽는다. 목록 조회에 실패하면 None을 반환한다.
/// heuristic stale은 긴 작업·출력 중단과 구분하기 어려워 재시작 후보에서 제외한다.
pub fn spawn_census<H: HostCall>(host: &H, parent_surface_id: u32) -> Option<SpawnCensus> {
    spawn_census_for(host, json!({ "surface": parent_surface_id }))
}

/// [`spawn_census`]와 같지만 부모를 `terminal.children` 인자(`surface` 또는
/// `parent_external`·`workspace`)로 받는다.
pub fn spawn_census_for<H: HostCall>(host: &H, parent: Value) -> Option<SpawnCensus> {
    let resp = host.call("terminal.children", parent).ok()?;
    let children = resp.get("children")?.as_array()?;
    let threshold = host
        .call(
            "settings.get_plugin_setting",
            json!({ "storage_key": "spawn_child_warn_threshold" }),
        )
        .ok()
        .and_then(|v| v.get("value").and_then(|v| v.as_f64()))
        .unwrap_or(DEFAULT_SPAWN_CHILD_WARN_THRESHOLD);
    Some(SpawnCensus {
        total: children.len(),
        idle: indices_with(children, |c| state_of(c) == Some("idle")),
        stale: indices_with(children, |c| {
            state_of(c) == Some("stale")
                && c.get("confidence").and_then(|v| v.as_str()) == Some("confirmed")
        }),
        threshold,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn index_less_children_are_dropped_not_defaulted() {
        let children = vec![
            json!({ "index": 1, "state": "idle" }),
            json!({ "state": "idle" }),
            json!({ "index": 3, "state": "running" }),
        ];
        let idle = indices_with(&children, |c| state_of(c) == Some("idle"));
        assert_eq!(idle, vec![1], "index가 없는 항목은 목록에서 제외해야 한다");
        assert_eq!(join_indices(&[1, 3]), "1, 3");
        assert_eq!(join_indices(&[]), "");
    }

    struct FakeHost(Vec<(String, Value)>);
    impl HostCall for FakeHost {
        fn call(
            &self,
            method: &str,
            _params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            self.0
                .iter()
                .find(|(m, _)| m == method)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| tasty_plugin_sdk::PluginError::HostCall {
                    method: method.to_string(),
                    message: "mock: no response set".to_string(),
                    code: None,
                })
        }
    }

    /// heuristic stale은 재시작 후보에 넣지 않는다.
    #[test]
    fn only_confirmed_stale_children_count_as_respawn_candidates() {
        let host = FakeHost(vec![(
            "terminal.children".into(),
            json!({ "children": [
                { "index": 0, "state": "idle" },
                { "index": 1, "state": "stale", "confidence": "confirmed" },
                { "index": 2, "state": "stale", "confidence": "heuristic" },
                // confidence가 없는 옛 응답도 재시작 후보에서는 제외한다.
                { "index": 4, "state": "stale" },
                { "index": 3, "state": "running" },
            ]}),
        )]);
        let c = spawn_census(&host, 1).expect("자식 목록을 읽었다");
        assert_eq!(c.total, 5);
        assert_eq!(c.idle, vec![0]);
        assert_eq!(c.stale, vec![1], "휴리스틱 stale 을 respawn 후보로 셌다");
        assert_eq!(
            c.threshold, DEFAULT_SPAWN_CHILD_WARN_THRESHOLD,
            "설정을 못 읽으면 기본 문턱값이다"
        );
    }

    /// 조회 실패를 자식이 없는 상태로 처리하지 않는다.
    #[test]
    fn a_failed_children_call_is_none_not_zero() {
        assert_eq!(spawn_census(&FakeHost(vec![]), 1), None);
    }
}
