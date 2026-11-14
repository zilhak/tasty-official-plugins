//! 메인 턴이 띄운 백그라운드 작업을 surface 별로 추적한다.
//!
//! `StopFailure` payload 에는 `background_tasks` 가 없어, 백그라운드 작업이 남은 채 API 오류로 끝난 턴을
//! 턴 종료와 구분할 입력이 없다. 그래서 작업의 시작과 끝을 다른 훅에서 모은다.
//! - 시작: `PostToolUse` 의 `tool_response`. Bash 는 `backgroundTaskId`, Agent 는 `isAsync`·`status: async_launched`
//!   와 함께 `agentId` 가 온다(Claude Code 2.1.290 실측).
//! - 끝: 작업이 끝나면 Claude Code 가 `<task-notification>` 으로 시작하는 prompt 의 `UserPromptSubmit` 으로
//!   새 턴을 연다. 그 prompt 의 `<task-id>` 가 끝난 작업이다.
//! - `Stop` 의 `background_tasks` 는 그 시점에 남은 작업 목록이므로 기록을 그 목록으로 바꾼다.
//!
//! 기록은 메모리에만 있다. 플러그인이 다시 시작되면 사라지고, 그 뒤의 `StopFailure` 는 전처럼 idle 이 된다.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

/// 백그라운드 작업을 띄울 수 있는 도구. 설치 훅의 matcher 와 같은 목록이다(`Task` 는 Agent 의 옛 이름).
/// Monitor 는 이 모듈의 기록에 넣지 않고 [`crate::monitor_tasks`] 가 따로 기록한다. 이 기록이 StopFailure 를
/// `active` 로 둘지 정하는데, Monitor 를 넣으면 그 판정이 달라진다.
pub(crate) const BACKGROUND_TOOLS_MATCHER: &str = "Bash|Agent|Task|Monitor";

/// 작업 id → 종류(`shell`·`subagent`). 종류는 `Stop` 의 `background_tasks[].type` 과 같은 이름이다.
type Tasks = BTreeMap<String, String>;

#[derive(Debug, Default)]
pub(crate) struct BackgroundTasks {
    by_surface: HashMap<u32, Tasks>,
}

impl BackgroundTasks {
    /// 도구 결과가 백그라운드 작업을 띄웠으면 기록하고 그 id 를 돌려준다.
    pub fn started(
        &mut self,
        surface_id: u32,
        tool_name: Option<&str>,
        tool_response: Option<&Value>,
    ) -> Option<String> {
        let (id, kind) = started_task(tool_name?, tool_response?)?;
        self.by_surface
            .entry(surface_id)
            .or_default()
            .insert(id.clone(), kind.to_string());
        Some(id)
    }

    /// `<task-notification>` prompt 가 알린 작업을 지운다. 지운 수를 돌려준다.
    pub fn notified(&mut self, surface_id: u32, prompt: Option<&str>) -> usize {
        let ids = notified_task_ids(prompt.unwrap_or(""));
        let Some(tasks) = self.by_surface.get_mut(&surface_id) else {
            return 0;
        };
        let removed = ids.iter().filter(|id| tasks.remove(*id).is_some()).count();
        if tasks.is_empty() {
            self.by_surface.remove(&surface_id);
        }
        removed
    }

    /// `Stop` 이 알려 준 남은 작업 목록으로 기록을 바꾼다. 목록을 읽지 못하면 그대로 둔다.
    pub fn sync_with_stop(&mut self, surface_id: u32, background_tasks: Option<&Value>) {
        let Some(tasks) = background_tasks.and_then(unfinished_tasks) else {
            return;
        };
        if tasks.is_empty() {
            self.by_surface.remove(&surface_id);
        } else {
            self.by_surface.insert(surface_id, tasks);
        }
    }

    /// 훅 이벤트에 맞춰 기록을 갱신한다. `Stop` 은 남은 작업 목록을 주고, `<task-notification>` prompt 는
    /// 끝난 작업을 알린다. 세션이 바뀌거나 끝나면 이전 작업은 이어지지 않는다.
    pub fn track(&mut self, event: &str, surface_id: u32, params: &Value) {
        match event {
            "stop" => self.sync_with_stop(surface_id, params.get("background_tasks")),
            "prompt-submit" => {
                let ended = self.notified(surface_id, params.get("prompt").and_then(Value::as_str));
                if ended > 0 {
                    tracing::info!(
                        "claude hook s{surface_id}: {ended} background task(s) reported finished"
                    );
                }
            }
            "session-start" | "session-end" => self.clear(surface_id),
            _ => {}
        }
    }

    /// 새 세션이나 세션 종료에서 기록을 버린다.
    pub fn clear(&mut self, surface_id: u32) {
        self.by_surface.remove(&surface_id);
    }

    /// 아직 끝나지 않은 작업의 id.
    pub fn pending_ids(&self, surface_id: u32) -> Vec<String> {
        self.by_surface
            .get(&surface_id)
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 아직 끝나지 않은 작업의 종류. 비어 있으면 남은 작업이 없다.
    pub fn pending_types(&self, surface_id: u32) -> Vec<String> {
        self.by_surface
            .get(&surface_id)
            .map(|t| t.values().cloned().collect())
            .unwrap_or_default()
    }
}

/// 작업 id 를 받아 끝난 작업인지 답하는 판정.
pub(crate) type EndedTask = Box<dyn Fn(&str) -> bool>;

/// 이 훅이 끝났다고 알려 준 작업을 가리는 판정. 끝난 작업이 없으면 `None` 이다.
/// `<task-notification>` prompt 의 `<task-id>`, Stop 의 남은 목록에 없는 작업, 세션이 바뀌거나 끝나면 모든 작업이다.
pub(crate) fn ended_tasks(event: &str, params: &Value) -> Option<EndedTask> {
    match event {
        "prompt-submit" => {
            let ids = notified_task_ids(params.get("prompt").and_then(Value::as_str).unwrap_or(""));
            (!ids.is_empty()).then(|| Box::new(move |id: &str| ids.iter().any(|i| i == id)) as _)
        }
        "stop" => {
            let left = params.get("background_tasks").and_then(unfinished_tasks)?;
            Some(Box::new(move |id: &str| !left.contains_key(id)))
        }
        "session-start" | "session-end" => Some(Box::new(|_: &str| true)),
        _ => None,
    }
}

/// 도구 결과에서 백그라운드 작업의 (id, 종류) 를 읽는다. 백그라운드로 띄운 것이 아니면 `None` 이다.
fn started_task(tool_name: &str, tool_response: &Value) -> Option<(String, &'static str)> {
    let parsed;
    let response = match tool_response {
        Value::String(raw) => {
            parsed = serde_json::from_str::<Value>(raw).ok()?;
            &parsed
        }
        other => other,
    };
    let text = |key: &str| {
        response
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    match tool_name {
        "Bash" => text("backgroundTaskId").map(|id| (id, "shell")),
        "Agent" | "Task" => {
            let launched = response.get("isAsync").and_then(Value::as_bool) == Some(true)
                || response.get("status").and_then(Value::as_str) == Some("async_launched");
            if launched {
                text("agentId").map(|id| (id, "subagent"))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `<task-notification>` prompt 에서 `<task-id>` 값을 모은다. 알림 prompt 가 아니면 비어 있다.
pub(crate) fn notified_task_ids(prompt: &str) -> Vec<String> {
    const OPEN: &str = "<task-id>";
    const CLOSE: &str = "</task-id>";
    if !prompt.trim_start().starts_with("<task-notification>") {
        return Vec::new();
    }
    let mut ids = Vec::new();
    let mut rest = prompt;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(CLOSE) else {
            break;
        };
        let id = after[..end].trim();
        if !id.is_empty() {
            ids.push(id.to_string());
        }
        rest = &after[end + CLOSE.len()..];
    }
    ids
}

/// `background_tasks` 에서 끝나지 않은 항목의 (id, 종류) 를 모은다. 배열이 아니면 `None` 이다.
/// 끝남 판정은 대기 Stop 과 같은 규칙([`crate::hook::is_finished_task`])을 쓴다.
fn unfinished_tasks(value: &Value) -> Option<Tasks> {
    let parsed;
    let tasks = match value {
        Value::String(raw) => {
            parsed = serde_json::from_str::<Value>(raw).ok()?;
            &parsed
        }
        other => other,
    };
    Some(
        tasks
            .as_array()?
            .iter()
            .filter(|t| !crate::hook::is_finished_task(t))
            .filter_map(|t| {
                let id = t.get("id").and_then(Value::as_str)?.to_string();
                let kind = t.get("type").and_then(Value::as_str).unwrap_or("unknown");
                Some((id, kind.to_string()))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Claude Code 2.1.290 실측 `PostToolUse` 의 `tool_response` 모양.
    fn bash_response() -> Value {
        json!({"stdout":"","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false,"backgroundTaskId":"bwny9udnf"})
    }

    fn agent_response() -> Value {
        json!({"isAsync":true,"status":"async_launched","agentId":"a485249fcec58ca82","description":"Reply sub-ok"})
    }

    #[test]
    fn background_bash_and_agent_launches_are_recorded() {
        let mut t = BackgroundTasks::default();
        assert_eq!(
            t.started(3, Some("Bash"), Some(&bash_response())),
            Some("bwny9udnf".into())
        );
        assert_eq!(
            t.started(3, Some("Agent"), Some(&agent_response())),
            Some("a485249fcec58ca82".into())
        );
        assert_eq!(t.pending_types(3), vec!["subagent", "shell"]);
    }

    #[test]
    fn a_response_given_as_a_json_string_is_read_too() {
        let mut t = BackgroundTasks::default();
        let raw = Value::String(bash_response().to_string());
        assert_eq!(
            t.started(3, Some("Bash"), Some(&raw)),
            Some("bwny9udnf".into())
        );
    }

    #[test]
    fn foreground_tool_calls_are_not_recorded() {
        let mut t = BackgroundTasks::default();
        let foreground_bash = json!({"stdout":"ok","stderr":"","interrupted":false});
        let foreground_agent = json!({"status":"completed","agentId":"a1","content":[]});
        assert_eq!(t.started(3, Some("Bash"), Some(&foreground_bash)), None);
        assert_eq!(t.started(3, Some("Agent"), Some(&foreground_agent)), None);
        assert_eq!(t.started(3, Some("Read"), Some(&bash_response())), None);
        assert_eq!(t.started(3, None, Some(&bash_response())), None);
        assert!(t.pending_types(3).is_empty());
    }

    /// 끝난 작업: 알림 prompt 의 id, Stop 의 남은 목록에 없는 id, 세션 경계의 모든 작업. 읽지 못하는 Stop 은 아무것도 끝내지 않는다.
    #[test]
    fn ended_tasks_come_from_the_notification_the_stop_list_and_session_bounds() {
        let ended = |event: &str, params: Value| ended_tasks(event, &params);
        let prompt =
            json!({ "prompt": "<task-notification>\n<task-id>T</task-id>\n</task-notification>" });
        let f = ended("prompt-submit", prompt).expect("알림 prompt");
        assert!(f("T") && !f("U"));
        assert!(ended("prompt-submit", json!({ "prompt": "hello" })).is_none());
        let stop =
            json!({ "background_tasks": [{ "id": "U", "type": "shell", "status": "running" }] });
        let f = ended("stop", stop).expect("남은 목록");
        assert!(f("T") && !f("U"));
        assert!(ended("stop", json!({})).is_none());
        assert!(ended("session-end", json!({})).expect("세션 종료")("T"));
        assert!(ended("notification", json!({})).is_none());
    }

    #[test]
    fn a_task_notification_prompt_ends_the_task_it_names() {
        let mut t = BackgroundTasks::default();
        t.started(3, Some("Bash"), Some(&bash_response()));
        t.started(3, Some("Agent"), Some(&agent_response()));
        let prompt = "<task-notification>\n<task-id>a485249fcec58ca82</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>completed</status>\n</task-notification>";
        assert_eq!(t.notified(3, Some(prompt)), 1);
        assert_eq!(t.pending_types(3), vec!["shell"]);
    }

    #[test]
    fn an_ordinary_prompt_that_mentions_a_task_id_ends_nothing() {
        let mut t = BackgroundTasks::default();
        t.started(3, Some("Bash"), Some(&bash_response()));
        assert_eq!(
            t.notified(3, Some("look at <task-id>bwny9udnf</task-id>")),
            0
        );
        assert_eq!(t.notified(3, None), 0);
        assert_eq!(t.pending_types(3), vec!["shell"]);
    }

    #[test]
    fn a_stop_replaces_the_record_with_the_tasks_it_reports() {
        let mut t = BackgroundTasks::default();
        t.started(3, Some("Bash"), Some(&bash_response()));
        t.started(3, Some("Agent"), Some(&agent_response()));
        let list = json!([{"id":"bwny9udnf","type":"shell","status":"running"},{"id":"x","type":"shell","status":"completed"}]);
        t.sync_with_stop(3, Some(&list));
        assert_eq!(t.pending_types(3), vec!["shell"]);
        t.sync_with_stop(3, Some(&json!([])));
        assert!(t.pending_types(3).is_empty());
    }

    #[test]
    fn a_stop_without_a_readable_list_keeps_the_record() {
        let mut t = BackgroundTasks::default();
        t.started(3, Some("Bash"), Some(&bash_response()));
        t.sync_with_stop(3, None);
        t.sync_with_stop(3, Some(&json!("not json")));
        t.sync_with_stop(3, Some(&json!({"id":"x"})));
        assert_eq!(t.pending_types(3), vec!["shell"]);
    }

    #[test]
    fn records_are_kept_per_surface_and_cleared() {
        let mut t = BackgroundTasks::default();
        t.started(3, Some("Bash"), Some(&bash_response()));
        t.started(4, Some("Agent"), Some(&agent_response()));
        t.clear(3);
        assert!(t.pending_types(3).is_empty());
        assert_eq!(t.pending_types(4), vec!["subagent"]);
    }
}
