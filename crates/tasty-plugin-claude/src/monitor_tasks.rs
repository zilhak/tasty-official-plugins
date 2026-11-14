//! 정지 알림에서 뺄 Monitor 작업을 기록한다.
//!
//! Monitor 는 이벤트가 있을 때만 출력하므로 조용한 것이 정상이다. 그런데 `Stop` 의 `background_tasks` 는 Monitor
//! 작업을 백그라운드 Bash 와 같은 `type: shell` 로 적고 id 접두도 같아 구별되지 않는다(Claude Code 2.1.319 실측).
//! `PostToolUse` 의 `tool_name: Monitor` 와 `tool_response.taskId` 만 구별해 주므로 그 id 를 모아 Stop 항목과 맞춘다.
//!
//! 기록은 surface meta 에 둔다. `PostToolUse` 는 다시 오지 않으므로 플러그인이 다시 시작돼도 남아야 하고, 중간 이벤트가
//! 새 턴을 열어도 Monitor 는 이어지므로 새 턴이 지우는 대기 meta 와 따로 둔다. `<status>` 가 있는 끝 알림과 세션 경계에서
//! 지운다. Stop 목록에 없다는 이유로는 지우지 않는다. 실행 중인 서브에이전트의 Monitor 가 목록에 나오는지 확인하지
//! 못했기 때문이다. 끝난 id 가 남아도 작업 id 가 달라 다른 작업과 겹치지 않는다.
//! 기록이 없으면 셸로 본다. 옛 훅 설치에서는 지금처럼 알린다.

use std::collections::BTreeSet;
use std::sync::Mutex;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use crate::error_scan::WaitKinds;
use crate::state::{ClaudeState, lock_state};

/// 기록한 Monitor 작업 id(JSON 배열)를 남기는 surface meta.
pub(crate) const MONITOR_TASKS_META_KEY: &str = "claude-monitor-tasks";

/// `Stop` 항목의 종류가 이 값이면 기록 없이도 Monitor 로 본다. MCP·WebSocket 감시의 값이다(실제 payload 미측정).
const MONITOR_TYPE: &str = "monitor";

pub(crate) type MonitorIds = BTreeSet<String>;

/// `background-start` 훅의 도구 결과를 기록하고 띄운 작업의 id 를 돌려준다. Monitor 는 이 모듈의 기록에, 나머지는
/// [`crate::background_tasks`] 에 둔다. 서브에이전트가 띄운 작업도 메인 턴이 기다리므로 `agent_id` 를 거르지 않는다.
pub(crate) fn record_start<H: HostCall>(
    state: &Mutex<ClaudeState>,
    host: &H,
    surface_id: u32,
    params: &Value,
) -> Option<String> {
    let tool_name = params.get("tool_name").and_then(Value::as_str);
    let tool_response = params.get("tool_response");
    // 잠근 채 읽고 써서 동시에 띄운 Monitor 를 잃지 않는다.
    let mut state = lock_state(state);
    match started_id(tool_name, tool_response) {
        Some(id) => {
            record_started(host, surface_id, id.clone());
            Some(id)
        }
        None => state
            .background
            .started(surface_id, tool_name, tool_response),
    }
}

/// `prompt-submit` 훅의 prompt 가 끝났다고 알린 Monitor 를 기록에서 지운다.
pub(crate) fn forget_notified<H: HostCall>(
    state: &Mutex<ClaudeState>,
    host: &H,
    surface_id: u32,
    params: &Value,
) {
    let prompt = params.get("prompt").and_then(Value::as_str).unwrap_or("");
    let finished = finished_ids(prompt);
    if !finished.is_empty() {
        // 읽고 쓰는 사이에 다른 훅이 끼지 않게 잠근다.
        let _serial = lock_state(state);
        forget_finished(host, surface_id, &finished);
    }
}

/// `Stop` 항목의 종류. 없으면 `unknown` 이다.
pub(crate) fn task_kind(task: &Value) -> String {
    task.get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// 대기 Stop 의 끝나지 않은 항목에서 대기 기록의 종류를 만든다.
pub(crate) fn wait_kinds(items: &[Value], monitors: &MonitorIds) -> WaitKinds {
    WaitKinds {
        all: items.iter().map(task_kind).collect(),
        noticed: items
            .iter()
            .filter(|t| !is_monitor_item(t, monitors))
            .map(task_kind)
            .collect(),
    }
}

/// 플러그인이 기록한 작업(StopFailure 경로)에서 정지 알림 대상의 id 와 대기 기록의 종류를 만든다.
pub(crate) fn split_recorded(
    ids: Vec<String>,
    kinds: Vec<String>,
    monitors: &MonitorIds,
) -> (Vec<String>, WaitKinds) {
    let (noticed_ids, noticed): (Vec<String>, Vec<String>) = ids
        .into_iter()
        .zip(kinds.iter().cloned())
        .filter(|(id, kind)| !is_monitor(Some(id), Some(kind), monitors))
        .unzip();
    (
        noticed_ids,
        WaitKinds {
            all: kinds,
            noticed,
        },
    )
}

/// 도구 결과가 Monitor 를 띄웠으면 그 작업 id 를 돌려준다.
fn started_id(tool_name: Option<&str>, tool_response: Option<&Value>) -> Option<String> {
    if tool_name? != "Monitor" {
        return None;
    }
    let id = |response: &Value| {
        response
            .get("taskId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    match tool_response? {
        Value::String(raw) => id(&serde_json::from_str(raw).ok()?),
        other => id(other),
    }
}

/// 기록한 Monitor 작업 id. 없거나 읽을 수 없으면 비어 있다.
pub(crate) fn recorded<H: HostCall>(host: &H, surface_id: u32) -> MonitorIds {
    let Some(raw) = crate::stop_pairing::meta_value(host, surface_id, MONITOR_TASKS_META_KEY)
    else {
        return MonitorIds::new();
    };
    serde_json::from_str(&raw)
        .inspect_err(|e| tracing::warn!("claude monitor tasks meta is not JSON ({e}): {raw}"))
        .unwrap_or_default()
}

/// Monitor 작업 id 를 기록에 더한다. 호출자가 읽고 쓰는 사이를 잠가 동시에 띄운 작업을 잃지 않게 한다.
fn record_started<H: HostCall>(host: &H, surface_id: u32, id: String) {
    let mut ids = recorded(host, surface_id);
    if ids.insert(id) {
        write(host, surface_id, &ids);
    }
}

/// `<status>` 가 있는 `<task-notification>` 이 알린 Monitor 를 기록에서 지운다. 호출자가 잠근다.
fn forget_finished<H: HostCall>(host: &H, surface_id: u32, finished: &[String]) {
    let mut ids = recorded(host, surface_id);
    let before = ids.len();
    ids.retain(|id| !finished.contains(id));
    if ids.len() != before {
        write(host, surface_id, &ids);
    }
}

fn write<H: HostCall>(host: &H, surface_id: u32, ids: &MonitorIds) {
    let result = if ids.is_empty() {
        host.call(
            "surface.meta.unset",
            json!({ "surface_id": surface_id, "key": MONITOR_TASKS_META_KEY }),
        )
    } else {
        host.call(
            "surface.meta.set",
            json!({ "surface_id": surface_id, "key": MONITOR_TASKS_META_KEY, "value": json!(ids).to_string() }),
        )
    };
    if let Err(e) = result {
        tracing::warn!("claude monitor s{surface_id}: recording the monitor tasks failed: {e}");
    }
}

/// 작업 하나가 Monitor 인가.
fn is_monitor(id: Option<&str>, kind: Option<&str>, recorded: &MonitorIds) -> bool {
    kind == Some(MONITOR_TYPE) || id.is_some_and(|id| recorded.contains(id))
}

/// `Stop` 의 `background_tasks` 항목이 Monitor 인가.
pub(crate) fn is_monitor_item(item: &Value, recorded: &MonitorIds) -> bool {
    is_monitor(
        item.get("id").and_then(Value::as_str),
        item.get("type").and_then(Value::as_str),
        recorded,
    )
}

/// prompt 의 `<task-notification>` 가운데 작업이 끝났다고 알린 것(`<status>` 가 있음)의 `<task-id>`.
/// Monitor 의 중간 이벤트에는 `<status>` 가 없다.
fn finished_ids(prompt: &str) -> Vec<String> {
    const BLOCK: &str = "<task-notification>";
    if !prompt.trim_start().starts_with(BLOCK) {
        return Vec::new();
    }
    prompt
        .split(BLOCK)
        .skip(1)
        .filter(|block| block.contains("<status>"))
        .filter_map(|block| {
            let after = block.split_once("<task-id>")?.1;
            let id = after.split_once("</task-id>")?.0.trim();
            (!id.is_empty()).then(|| id.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MetaHost {
        meta: RefCell<HashMap<String, String>>,
    }

    impl HostCall for MetaHost {
        fn call(
            &self,
            method: &str,
            params: Value,
        ) -> Result<Value, tasty_plugin_sdk::PluginError> {
            let key = params["key"].as_str().unwrap().to_string();
            Ok(match method {
                "surface.meta.get" => match self.meta.borrow().get(&key) {
                    Some(v) => json!({ "value": v }),
                    None => json!({}),
                },
                "surface.meta.set" => {
                    let value = params["value"].as_str().unwrap().to_string();
                    self.meta.borrow_mut().insert(key, value);
                    json!({})
                }
                "surface.meta.unset" => {
                    self.meta.borrow_mut().remove(&key);
                    json!({})
                }
                other => panic!("unexpected host call: {other}"),
            })
        }
    }

    /// Claude Code 2.1.319 실측 `PostToolUse` 의 `tool_response`.
    fn probe_response() -> Value {
        json!({"taskId":"bym7ibssy","timeoutMs":1500000,"persistent":false})
    }

    #[test]
    fn only_the_monitor_tool_gives_an_id() {
        assert_eq!(
            started_id(Some("Monitor"), Some(&probe_response())),
            Some("bym7ibssy".to_string())
        );
        let raw = Value::String(probe_response().to_string());
        assert_eq!(
            started_id(Some("Monitor"), Some(&raw)),
            Some("bym7ibssy".to_string())
        );
        assert_eq!(started_id(Some("Bash"), Some(&probe_response())), None);
        assert_eq!(started_id(None, Some(&probe_response())), None);
        assert_eq!(started_id(Some("Monitor"), Some(&json!({}))), None);
        assert_eq!(started_id(Some("Monitor"), None), None);
    }

    #[test]
    fn the_record_lives_in_the_meta_and_loses_only_finished_ids() {
        let host = MetaHost::default();
        record_started(&host, 3, "a".into());
        record_started(&host, 3, "b".into());
        record_started(&host, 3, "a".into());
        // 플러그인 재시작: 메모리 상태 없이 같은 meta 를 읽는다.
        assert_eq!(recorded(&host, 3).len(), 2);
        forget_finished(&host, 3, &["a".to_string(), "other".to_string()]);
        assert_eq!(recorded(&host, 3).into_iter().collect::<Vec<_>>(), ["b"]);
        forget_finished(&host, 3, &["b".to_string()]);
        assert!(host.meta.borrow().is_empty(), "빈 기록은 meta 를 지운다");
    }

    #[test]
    fn an_unreadable_record_counts_as_no_monitor() {
        let host = MetaHost::default();
        host.meta
            .borrow_mut()
            .insert(MONITOR_TASKS_META_KEY.to_string(), "not json".to_string());
        assert!(recorded(&host, 3).is_empty());
    }

    #[test]
    fn a_stop_item_is_a_monitor_by_record_or_by_type() {
        let ids = MonitorIds::from(["bym7ibssy".to_string()]);
        // Stop 항목은 Monitor 도 `type: shell` 이다(실측).
        let shell = json!({"id":"bym7ibssy","type":"shell","status":"running"});
        assert!(is_monitor_item(&shell, &ids));
        let bash = json!({"id":"baaw0pnlp","type":"shell","status":"running"});
        assert!(!is_monitor_item(&bash, &ids));
        let typed = json!({"id":"m1","type":"monitor","status":"running"});
        assert!(is_monitor_item(&typed, &MonitorIds::new()));
    }

    /// 중간 이벤트(`<status>` 없음)는 끝이 아니다. 실측 모양이다.
    #[test]
    fn a_monitor_event_is_not_an_end_but_a_status_is() {
        let event = "<task-notification>\n<task-id>bkmcn04ns</task-id>\n<summary>Monitor event: \"x\"</summary>\n<event>tick</event>\n</task-notification>";
        assert!(finished_ids(event).is_empty());
        let end = "<task-notification>\n<task-id>bkmcn04ns</task-id>\n<output-file>/t</output-file>\n<status>completed</status>\n</task-notification>";
        assert_eq!(finished_ids(end), ["bkmcn04ns"]);
        assert!(finished_ids("look at <task-id>a</task-id> <status>x</status>").is_empty());
        assert_eq!(finished_ids(&format!("{event}\n{end}")), ["bkmcn04ns"]);
    }
}
