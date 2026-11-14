//! 호스트 IPC 호출과 같은 명령으로 등록한 완료 알림 훅의 관리.

use serde_json::{Value, json};
use tasty_plugin_sdk::HostHandle;

/// 동기 호스트 호출. 시험에서는 mock으로 바꿔 응답과 실패를 재현한다.
pub trait HostCall {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError>;

    /// 이 호출 하나만 응답 상한을 바꾼다. 다른 호출과 핸들의 상한은 그대로다.
    /// 시험 mock 처럼 상한이 없는 구현은 [`Self::call`] 과 같다.
    fn call_within(
        &self,
        method: &str,
        params: Value,
        _timeout: std::time::Duration,
    ) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.call(method, params)
    }
}

impl HostCall for HostHandle {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        HostHandle::call(self, method, params)
    }

    fn call_within(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, tasty_plugin_sdk::PluginError> {
        let mut handle = self.clone();
        handle.timeout = timeout;
        HostHandle::call(&handle, method, params)
    }
}

/// surface.locate의 exists를 읽는다. 조회 실패 시 완료 알림 훅을 다시 등록하지 않도록 false로 처리한다.
pub fn surface_is_alive<H: HostCall>(host: &H, surface_id: u32) -> bool {
    host.call("surface.locate", json!({ "surface_id": surface_id }))
        .ok()
        .and_then(|r| r.get("exists").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// command가 정확히 같은 훅의 id만 고른다. 다른 명령으로 등록한 그룹은 건드리지 않는다.
pub fn siblings_to_unset(hooks: &[Value], expected_command: &str) -> Vec<u64> {
    hooks
        .iter()
        .filter(|h| h.get("command").and_then(|v| v.as_str()) == Some(expected_command))
        .filter_map(|h| h.get("id").and_then(|v| v.as_u64()))
        .collect()
}

/// 알림을 처리한 훅과 같은 surface·command의 남은 훅을 정리한다.
/// 정리 실패가 이미 전달한 알림의 결과를 바꾸지는 않는다.
pub fn cleanup_sibling_hooks<H: HostCall>(host: &H, target_surface: u32, expected_command: &str) {
    if let Ok(resp) = host.call("hook.list", json!({ "surface_id": target_surface }))
        && let Some(hooks) = resp.as_array()
    {
        for hook_id in siblings_to_unset(hooks, expected_command) {
            // 남은 once 훅은 다음에 중복 알림을 내므로 실패를 로그로 남긴다.
            if let Err(e) = host.call("hook.unset", json!({ "hook_id": hook_id })) {
                tracing::warn!("hook.unset {hook_id} failed during sibling cleanup: {e}");
            }
        }
    }
}

/// command가 `group_prefix`로 시작하는 훅의 id를 고른다. 같은 부모·대상의 완료 그룹은 명령 이름(spawn·tell 등)만
/// 다르므로 접두어로 함께 찾는다.
pub fn group_members_to_unset(hooks: &[Value], group_prefix: &str) -> Vec<u64> {
    hooks
        .iter()
        .filter(|h| {
            h.get("command")
                .and_then(|v| v.as_str())
                .is_some_and(|c| c.starts_with(group_prefix))
        })
        .filter_map(|h| h.get("id").and_then(|v| v.as_u64()))
        .collect()
}

/// 같은 부모·대상의 기존 완료 그룹을 지운다. 조회·해제 실패는 로그만 남기고 새 등록을 계속한다.
fn remove_completion_group<H: HostCall>(
    host: &H,
    target_surface: u32,
    group_prefix: &str,
    agent: &str,
) {
    match host.call("hook.list", json!({ "surface_id": target_surface })) {
        Ok(resp) => {
            for hook_id in
                group_members_to_unset(resp.as_array().map_or(&[][..], Vec::as_slice), group_prefix)
            {
                if let Err(e) = host.call("hook.unset", json!({ "hook_id": hook_id })) {
                    tracing::warn!(
                        "{agent} hook.unset {hook_id} failed while replacing the completion group: {e}"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!("{agent} hook.list failed before registering completion hooks: {e}")
        }
    }
}

/// 이벤트별로 같은 command의 once 훅을 등록한다.
/// 같은 부모·대상의 완료 그룹(`group_prefix`로 시작하는 명령)이 이미 있으면 먼저 지운다. 그대로 두면 tell 을
/// 반복할 때마다 그룹이 쌓여 한 번의 상태 변경에 완료 줄이 그룹 수만큼 남는다.
/// 이벤트 목록은 각 CLI의 매니페스트에 따라 호출자가 전달한다.
/// 등록 실패는 로그에 남기되 이미 성공한 spawn/tell 결과를 바꾸지 않는다.
pub fn register_completion_hooks<H: HostCall>(
    host: &H,
    target_surface: u32,
    command: &str,
    group_prefix: &str,
    events: &[&str],
    agent: &str,
) {
    debug_assert!(command.starts_with(group_prefix));
    remove_completion_group(host, target_surface, group_prefix, agent);
    for event in events {
        if let Err(e) = host.call(
            "hook.set",
            json!({
                "surface_id": target_surface,
                "event": event,
                "command": command,
                "once": true,
            }),
        ) {
            tracing::warn!("{agent} notify hook.set '{event}' failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 같은 target surface 에 두 그룹(예: spawn / tell)이 공존해도 서로의 정리
    /// 대상에 들어가지 않는다.
    #[test]
    fn siblings_to_unset_isolates_by_command() {
        let spawn_cmd = "tasty hook done --command spawn";
        let tell_cmd = "tasty hook done --command tell";
        let hooks = vec![
            json!({ "id": 1, "command": spawn_cmd, "event": "process-exit" }),
            json!({ "id": 2, "command": tell_cmd, "event": "process-exit" }),
            json!({ "id": 3, "command": spawn_cmd, "event": "agent-idle" }),
        ];
        assert_eq!(siblings_to_unset(&hooks, spawn_cmd), vec![1, 3]);
        assert_eq!(siblings_to_unset(&hooks, tell_cmd), vec![2]);
    }

    /// 같은 부모·대상의 완료 그룹은 명령 이름과 관계없이 모두 고르고, 다른 부모나 다른 명령 종류는 고르지 않는다.
    #[test]
    fn group_members_share_the_caller_and_target_prefix() {
        let prefix = "tasty x notify --caller 1 --target 5 --command ";
        let hooks = vec![
            json!({ "id": 1, "command": "tasty x notify --caller 1 --target 5 --command spawn" }),
            json!({ "id": 2, "command": "tasty x notify --caller 1 --target 5 --command tell" }),
            json!({ "id": 3, "command": "tasty x notify --caller 11 --target 5 --command tell" }),
            json!({ "id": 4, "command": "tasty x notify-error --caller 1 --target 5" }),
        ];
        assert_eq!(group_members_to_unset(&hooks, prefix), vec![1, 2]);
    }

    /// `command` 가 없거나 `id` 가 없는 항목은 조용히 건너뛴다 — 호스트 응답 스키마가
    /// 늘어나도 정리가 패닉하지 않는다.
    #[test]
    fn malformed_hook_entries_are_skipped() {
        let hooks = vec![
            json!({ "id": 1 }),
            json!({ "command": "c" }),
            json!({ "id": 2, "command": "c" }),
        ];
        assert_eq!(siblings_to_unset(&hooks, "c"), vec![2]);
    }
}
