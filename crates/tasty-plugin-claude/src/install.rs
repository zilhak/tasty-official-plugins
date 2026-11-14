//! ~/.claude/settings.json에 Tasty 훅을 추가·갱신·제거한다.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};
use tasty_plugin_sdk::i18n::Translator;

/// 자동 등록할 (Claude 이벤트, Tasty 훅 이름, matcher) 목록.
/// UserPromptSubmit은 다음 턴의 상태를 active로 바꾼다.
/// AskUserQuestion의 전후 훅은 입력 대기와 응답 이후 상태를 구분한다.
/// StopFailure는 API 오류로 끝난 턴도 상태·알림에 반영하기 위해 등록한다.
/// Bash·Agent의 PostToolUse는 백그라운드 작업 시작을 기록해 StopFailure가 대기인지 가린다.
/// SubagentStop은 상태를 바꾸지 않고 로그만 남긴다. 이미 설치된 항목과 호환되도록 등록은 유지한다.
pub const MANAGED_HOOKS: &[(&str, &str, &str)] = &[
    ("Stop", "stop", ""),
    ("Notification", "notification", ""),
    ("SessionEnd", "session-end", ""),
    ("SubagentStop", "subagent-stop", ""),
    ("SessionStart", "session-start", ""),
    ("UserPromptSubmit", "prompt-submit", ""),
    ("PreToolUse", "pre-tool-use", "AskUserQuestion"),
    ("PostToolUse", "post-tool-use", "AskUserQuestion"),
    (
        "PostToolUse",
        crate::hook::BACKGROUND_START_EVENT,
        crate::background_tasks::BACKGROUND_TOOLS_MATCHER,
    ),
    ("StopFailure", "stop-failure", ""),
];

/// `entry_matches_marker`가 식별자로 사용하는 substring.
fn tasty_hook_marker(event_token: &str) -> String {
    format!("tasty claude hook {}", event_token)
}

/// settings.json과 세션 프로필에서 공유하는 훅 명령.
/// TASTY_SURFACE_ID가 없으면 호출하지 않고 성공으로 끝낸다.
/// 값이 있으면 호출하되 실패 코드가 세션을 막지 않도록 || true를 붙인다.
/// IPC 실패 기록은 CLI의 hook_failure가 담당한다.
/// 기존 설치 항목을 찾아 갱신할 수 있도록 tasty_hook_marker 문자열을 유지해야 한다.
pub(crate) fn tasty_guarded_command(argv: &str) -> String {
    format!("if [ -n \"$TASTY_SURFACE_ID\" ]; then {argv} || true; fi")
}

/// 이벤트 이름만 넣은 명령을 만든다. 세션 id·메시지 등은 CLI가 stdin JSON에서 읽는다.
fn tasty_hook_command(event_token: &str) -> String {
    if event_token == crate::hook::BACKGROUND_START_EVENT {
        return background_start_command();
    }
    tasty_guarded_command(&format!("tasty claude hook {event_token}"))
}

/// `background-start` 훅 명령. 이 훅은 Bash·Agent·Monitor 호출마다 실행되고, 끝날 때까지 Claude Code 가 다음
/// 모델 요청을 보내지 않는다. 그래서 백그라운드로 띄운 호출의 `tool_response` 에만 있는 필드
/// (`"backgroundTaskId":"`, `"isAsync":true`)와 Monitor 의 도구 이름을 셸에서 먼저 보고, 없으면 tasty CLI 를 띄우지
/// 않고 끝낸다. Claude Code 2.1.291·2.1.319 의 payload 는 공백 없는 JSON 이다. 형식이 바뀌어 맞지 않으면 기록이 빠질 뿐
/// 세션은 막지 않는다. 플러그인은 CLI 로 받은 payload 를 다시 해석해 판정한다.
fn background_start_command() -> String {
    format!(
        "if [ -n \"$TASTY_SURFACE_ID\" ]; then p=$(cat); case \"$p\" in {}) printf '%s' \"$p\" | tasty claude hook {} || true ;; esac; fi",
        BACKGROUND_PAYLOAD_PATTERNS,
        crate::hook::BACKGROUND_START_EVENT
    )
}

/// 백그라운드 호출 payload 에만 있는 문자열을 찾는 셸 `case` 패턴.
const BACKGROUND_PAYLOAD_PATTERNS: &str =
    r#"*'"backgroundTaskId":"'*|*'"isAsync":true'*|*'"tool_name":"Monitor"'*"#;

pub(crate) fn claude_settings_path(tr: &Translator) -> Result<PathBuf> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!(tr.t("claude.install.no_home_dir").to_string()))?;
    Ok(base.home_dir().join(".claude").join("settings.json"))
}

fn entry_matches_marker(entry: &Value, marker: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(marker))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// 현재 제품 호출은 설치 확인 함수뿐이며, 단위 시험에서도 직접 사용한다.
#[allow(dead_code)]
pub(crate) fn is_marker_installed_in_value(root: &Value, event_name: &str, marker: &str) -> bool {
    let Some(hooks) = root.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    let Some(arr) = hooks.get(event_name).and_then(|v| v.as_array()) else {
        return false;
    };
    arr.iter().any(|entry| entry_matches_marker(entry, marker))
}

/// 턴 확정을 보고하는 Stop·StopFailure 훅이 이 컴퓨터의 Claude Code 설정에 모두 있는가. 파일이 없으면 false다.
/// attach 원격 자식은 자식이 도는 서버의 설정을 쓰므로 서버 plugin 이 spawn 응답 경고에 쓴다.
pub(crate) fn completion_hooks_installed(tr: &Translator) -> Result<bool> {
    let path = claude_settings_path(tr)?;
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&path)?;
    let root: Value = serde_json::from_str(&content)?;
    Ok(
        is_marker_installed_in_value(&root, "Stop", &tasty_hook_marker("stop"))
            && is_marker_installed_in_value(
                &root,
                "StopFailure",
                &tasty_hook_marker("stop-failure"),
            ),
    )
}

/// install 이 바꾼 이벤트. `added` 는 새로 넣은 항목, `updated` 는 기존 Tasty 항목의 matcher 나 명령을
/// 현재 값으로 바꾼 항목이다. 둘 다 비면 설정 파일을 다시 쓰지 않는다.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct InstallChanges {
    pub added: Vec<&'static str>,
    pub updated: Vec<&'static str>,
    pub mod_updated: bool,
}

impl InstallChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && !self.mod_updated
    }
}

/// settings.json 루트 값에 hook을 idempotent하게 추가.
pub(crate) fn install_hooks_in_value(root: &mut Value, tr: &Translator) -> Result<InstallChanges> {
    let root_obj = root.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!(tr.t("claude.install.settings_root_not_object").to_string())
    })?;

    let hooks_obj = root_obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(tr.t("claude.install.hooks_not_object").to_string()))?;

    let mut changes = InstallChanges::default();

    for (event_name, event_token, matcher) in MANAGED_HOOKS {
        let marker = tasty_hook_marker(event_token);
        let command = tasty_hook_command(event_token);

        let arr = hooks_obj
            .entry((*event_name).to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                anyhow::anyhow!(tr.t_fmt("claude.install.hooks_event_not_array", event_name))
            })?;

        // 기존 Tasty 항목은 중복 추가하지 않고 matcher와 명령을 현재 값으로 갱신한다.
        let mut upgraded = false;
        let mut changed = false;
        for entry in arr.iter_mut() {
            if !entry_matches_marker(entry, &marker) {
                continue;
            }
            upgraded = true;
            if let Some(obj) = entry.as_object_mut() {
                let matcher_needs_update = obj
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(|m| m != *matcher)
                    .unwrap_or(true);
                if matcher_needs_update {
                    obj.insert("matcher".into(), Value::String((*matcher).to_string()));
                    changed = true;
                }
            }
            if let Some(hooks) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                for h in hooks.iter_mut() {
                    let needs_update = h
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains(&marker) && c != command)
                        .unwrap_or(false);
                    if needs_update && let Some(obj) = h.as_object_mut() {
                        obj.insert("command".into(), Value::String(command.clone()));
                        changed = true;
                    }
                }
            }
        }
        if upgraded {
            if changed {
                changes.updated.push(*event_name);
            }
            continue;
        }

        arr.push(json!({
            "matcher": matcher,
            "hooks": [
                {
                    "type": "command",
                    "command": command
                }
            ]
        }));
        changes.added.push(*event_name);
    }

    Ok(changes)
}

/// settings.json 루트 값에서 tasty hook entry를 제거.
pub(crate) fn uninstall_hooks_from_value(root: &mut Value) -> Vec<&'static str> {
    let Some(root_obj) = root.as_object_mut() else {
        return Vec::new();
    };
    let Some(hooks) = root_obj.get_mut("hooks") else {
        return Vec::new();
    };
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return Vec::new();
    };

    let mut removed: Vec<&'static str> = Vec::new();

    for (event_name, event_token, _matcher) in MANAGED_HOOKS {
        let marker = tasty_hook_marker(event_token);

        let Some(arr) = hooks_obj
            .get_mut(*event_name)
            .and_then(|v| v.as_array_mut())
        else {
            continue;
        };

        let before_len = arr.len();
        arr.retain(|entry| !entry_matches_marker(entry, &marker));
        let changed = arr.len() != before_len;

        if changed {
            removed.push(*event_name);
        }

        if arr.is_empty() {
            hooks_obj.remove(*event_name);
        }
    }

    if hooks_obj.is_empty() {
        root_obj.remove("hooks");
    }

    removed
}

/// `claude.install` IPC 핸들러. ~/.claude/settings.json을 idempotent하게 갱신.
pub(crate) fn run_install(tr: &Translator) -> Result<InstallChanges> {
    install_into_file(&claude_settings_path(tr)?, tr)
}

/// 설정 파일에 hook 을 넣거나 기존 Tasty 항목을 현재 형식으로 바꾼다. 바뀐 것이 없으면 파일을 쓰지 않는다.
fn install_into_file(path: &std::path::Path, tr: &Translator) -> Result<InstallChanges> {
    let mut root: Value = if path.exists() {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    let original = root.clone();
    let mut changes = install_hooks_in_value(&mut root, tr)?;
    changes.mod_updated = crate::notify_mod::install(&mut root, path)
        .map_err(|e| anyhow::anyhow!(tr.t_fmt("claude.install.mod_failed", &e.to_string())))?;
    if changes.is_empty() || root == original {
        return Ok(changes);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let output = serde_json::to_string_pretty(&root)?;
    std::fs::write(path, output)?;
    Ok(changes)
}

/// `claude.uninstall` IPC 핸들러. settings가 없어도 남은 관리 mod 파일을 제거한다.
pub(crate) fn run_uninstall(tr: &Translator) -> Result<Vec<&'static str>> {
    uninstall_from_file(&claude_settings_path(tr)?)
}

fn uninstall_from_file(path: &std::path::Path) -> Result<Vec<&'static str>> {
    let mut root: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else {
        json!({})
    };
    let original = root.clone();
    let mut removed = uninstall_hooks_from_value(&mut root);
    if crate::notify_mod::uninstall(&mut root, path)? {
        removed.push("notify-mod");
    }

    if root == original {
        return Ok(removed);
    }
    let output = serde_json::to_string_pretty(&root)?;
    std::fs::write(path, output)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_translator() -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, "en")
    }

    #[test]
    fn full_file_install_and_uninstall_preserve_user_hooks_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let user = json!({"env":{"OTHER":"keep"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"user-stop"}]}]}});
        std::fs::write(&path, serde_json::to_string(&user).unwrap()).unwrap();
        let changes = install_into_file(&path, &test_translator()).unwrap();
        assert!(changes.mod_updated);
        assert!(!changes.added.is_empty());
        assert!(
            install_into_file(&path, &test_translator())
                .unwrap()
                .is_empty()
        );
        let removed = uninstall_from_file(&path).unwrap();
        assert!(removed.contains(&"notify-mod"));
        let final_value: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(final_value, user);
        assert!(uninstall_from_file(&path).unwrap().is_empty());
    }

    #[test]
    fn asset_repair_preserves_unchanged_settings_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let tr = test_translator();
        install_into_file(&path, &tr).unwrap();
        let content = format!("{}\n\n", std::fs::read_to_string(&path).unwrap());
        std::fs::write(&path, &content).unwrap();
        let asset = crate::notify_mod::directory(&path)
            .unwrap()
            .join("hooks/reader.ts");
        std::fs::write(&asset, "old asset").unwrap();
        assert!(install_into_file(&path, &tr).unwrap().mod_updated);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        assert_ne!(std::fs::read_to_string(asset).unwrap(), "old asset");
    }

    #[test]
    fn uninstall_cleans_assets_without_recreating_missing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_into_file(&path, &test_translator()).unwrap();
        let asset_dir = crate::notify_mod::directory(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(uninstall_from_file(&path).unwrap(), vec!["notify-mod"]);
        assert!(!path.exists());
        assert!(!asset_dir.exists());
    }

    fn count_managed_entries(root: &Value, event_name: &str, marker: &str) -> usize {
        root.get("hooks")
            .and_then(|h| h.as_object())
            .and_then(|h| h.get(event_name))
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|e| entry_matches_marker(e, marker))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn install_in_empty_value_adds_all_events() {
        let mut root = json!({});
        let added = install_hooks_in_value(&mut root, &test_translator())
            .expect("install")
            .added;
        assert_eq!(added.len(), MANAGED_HOOKS.len());
        for (event_name, token, _matcher) in MANAGED_HOOKS {
            let marker = tasty_hook_marker(token);
            assert_eq!(
                count_managed_entries(&root, event_name, &marker),
                1,
                "missing event {} after install",
                event_name
            );
        }
    }

    /// MANAGED_HOOKS 순회로 만든 기대값과 별도로 StopFailure 등록을 확인한다.
    #[test]
    fn install_adds_stop_failure_entry_exactly_once() {
        let mut root = json!({});
        install_hooks_in_value(&mut root, &test_translator()).expect("install 1");
        install_hooks_in_value(&mut root, &test_translator()).expect("install 2");
        assert_eq!(
            count_managed_entries(&root, "StopFailure", &tasty_hook_marker("stop-failure")),
            1
        );
    }

    #[test]
    fn install_upgrades_stale_command() {
        // 이전 명령을 재설치하면 기존 항목의 명령만 갱신해야 한다.
        let stale_command = "[ -n \"$TASTY_SURFACE_ID\" ] && tasty claude hook session-start --session ${CLAUDE_SESSION_ID} || true";
        let mut root = json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": "",
                        "hooks": [{ "type": "command", "command": stale_command }]
                    }
                ]
            }
        });
        let added = install_hooks_in_value(&mut root, &test_translator())
            .expect("install")
            .added;
        // 신규 추가가 아니라 in-place upgrade 라서 added 에 SessionStart 는 없다.
        assert!(!added.contains(&"SessionStart"));
        let arr = root["hooks"]["SessionStart"].as_array().unwrap();
        // 중복 entry 가 추가되지 않고 한 개만 남는다.
        let tasty_count = arr
            .iter()
            .filter(|e| {
                e["hooks"][0]["command"]
                    .as_str()
                    .map(|c| c.contains("tasty claude hook session-start"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(
            tasty_count, 1,
            "stale entry should be upgraded, not duplicated"
        );
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, tasty_hook_command("session-start"));
        assert!(!cmd.contains("${CLAUDE_SESSION_ID}"));
    }

    /// 모든 항목이 이미 있어도 명령이 옛 형식이면 바꾼 이벤트를 `updated` 로 알린다. 다시 설치하면 둘 다 비어 있다.
    /// 옛 형식은 셸 판정 이전의 background-start 명령과 `[ … ] && … || true` 꼴의 명령이다.
    #[test]
    fn existing_entries_with_an_old_command_are_reported_as_updated() {
        let mut root = json!({});
        install_hooks_in_value(&mut root, &test_translator()).expect("install");
        let old_background =
            r#"if [ -n "$TASTY_SURFACE_ID" ]; then tasty claude hook background-start || true; fi"#;
        let old_stop = r#"[ -n "$TASTY_SURFACE_ID" ] && tasty claude hook stop || true"#;
        for (event, old) in [("Stop", old_stop), ("PostToolUse", old_background)] {
            for entry in root["hooks"][event].as_array_mut().unwrap() {
                let cmd = entry["hooks"][0]["command"].as_str().unwrap().to_string();
                let token = if event == "Stop" {
                    "hook stop"
                } else {
                    "hook background-start"
                };
                if cmd.contains(token) {
                    entry["hooks"][0]["command"] = json!(old);
                }
            }
        }
        let changes = install_hooks_in_value(&mut root, &test_translator()).expect("reinstall");
        assert!(changes.added.is_empty(), "{changes:?}");
        assert_eq!(changes.updated, vec!["Stop", "PostToolUse"]);
        let commands: Vec<&str> = root["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["hooks"][0]["command"].as_str())
            .collect();
        assert!(commands.contains(&tasty_hook_command("background-start").as_str()));
        let again = install_hooks_in_value(&mut root, &test_translator()).expect("again");
        assert!(again.is_empty(), "{again:?}");
    }

    /// 설정 파일은 바뀐 것이 있을 때만 다시 쓴다. 옛 명령만 바뀐 경우에도 쓴다.
    #[test]
    fn the_settings_file_is_rewritten_only_when_something_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let tr = test_translator();
        assert!(!install_into_file(&path, &tr).unwrap().added.is_empty());
        let installed = std::fs::read_to_string(&path).unwrap();
        // 사용자가 손댄 공백은 아무것도 바뀌지 않으면 그대로 남는다.
        let hand_edited = format!("{installed}\n\n");
        std::fs::write(&path, &hand_edited).unwrap();
        assert!(install_into_file(&path, &tr).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), hand_edited);
        let old =
            r#"if [ -n "$TASTY_SURFACE_ID" ]; then tasty claude hook background-start || true; fi"#;
        let current = serde_json::to_string(&tasty_hook_command("background-start")).unwrap();
        std::fs::write(
            &path,
            installed.replace(&current, &serde_json::to_string(old).unwrap()),
        )
        .unwrap();
        let changes = install_into_file(&path, &tr).unwrap();
        assert_eq!(changes.updated, vec!["PostToolUse"]);
        assert!(std::fs::read_to_string(&path).unwrap().contains(&current));
    }

    #[test]
    fn install_sets_pre_post_tool_use_matcher_to_ask_user_question() {
        let mut root = json!({});
        install_hooks_in_value(&mut root, &test_translator()).expect("install");
        assert_eq!(root["hooks"]["PreToolUse"][0]["matcher"], "AskUserQuestion");
        assert_eq!(
            root["hooks"]["PostToolUse"][0]["matcher"],
            "AskUserQuestion"
        );
        // 나머지는 matcher `""` 로 전부 받는다(`StopFailure` 는 에러 종류 전부).
        for event_name in [
            "Stop",
            "Notification",
            "SessionEnd",
            "SubagentStop",
            "SessionStart",
            "UserPromptSubmit",
            "StopFailure",
        ] {
            assert_eq!(
                root["hooks"][event_name][0]["matcher"], "",
                "{event_name} matcher should remain empty"
            );
        }
    }

    #[test]
    fn install_upgrades_stale_matcher() {
        // 이전 matcher도 재설치로 갱신해야 한다.
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "",
                        "hooks": [{ "type": "command", "command": tasty_hook_command("pre-tool-use") }]
                    }
                ]
            }
        });
        let added = install_hooks_in_value(&mut root, &test_translator())
            .expect("install")
            .added;
        assert!(
            !added.contains(&"PreToolUse"),
            "in-place upgrade, not a fresh add"
        );
        let arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "no duplicate entry");
        assert_eq!(arr[0]["matcher"], "AskUserQuestion");
    }

    /// `Monitor` 를 받기 전 설치의 matcher·명령은 재설치로 갱신한다. 재설치 전에는 Monitor 기록이 없다.
    #[test]
    fn install_upgrades_the_background_start_hook_to_monitor() {
        let old = "if [ -n \"$TASTY_SURFACE_ID\" ]; then p=$(cat); case \"$p\" in *'\"backgroundTaskId\":\"'*|*'\"isAsync\":true'*) printf '%s' \"$p\" | tasty claude hook background-start || true ;; esac; fi";
        let mut root = json!({
            "hooks": {
                "PostToolUse": [
                    { "matcher": "Bash|Agent|Task", "hooks": [{ "type": "command", "command": old }] }
                ]
            }
        });
        install_hooks_in_value(&mut root, &test_translator()).expect("install");
        let marker = tasty_hook_marker(crate::hook::BACKGROUND_START_EVENT);
        assert_eq!(count_managed_entries(&root, "PostToolUse", &marker), 1);
        let entry = root["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["matcher"] == "Bash|Agent|Task|Monitor")
            .expect("갱신한 matcher");
        let command = entry["hooks"][0]["command"].as_str().unwrap();
        assert!(command.contains("\"tool_name\":\"Monitor\""), "{command}");
    }

    #[test]
    fn install_is_idempotent() {
        let mut root = json!({});
        install_hooks_in_value(&mut root, &test_translator()).expect("install 1");
        let added2 = install_hooks_in_value(&mut root, &test_translator())
            .expect("install 2")
            .added;
        assert!(added2.is_empty(), "second install should add nothing");
        for (event_name, token, _matcher) in MANAGED_HOOKS {
            let marker = tasty_hook_marker(token);
            assert_eq!(count_managed_entries(&root, event_name, &marker), 1);
        }
    }

    #[test]
    fn install_preserves_other_hooks() {
        // 같은 이벤트 아래의 사용자 matcher·명령은 그대로 유지한다.
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo user" }] }
                ],
                "Stop": [
                    { "matcher": "", "hooks": [{ "type": "command", "command": "echo user-stop" }] }
                ]
            }
        });
        install_hooks_in_value(&mut root, &test_translator()).expect("install");

        let pretool_arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            pretool_arr.len(),
            2,
            "user's Bash-matcher entry preserved + tasty's AskUserQuestion-matcher entry added"
        );
        assert_eq!(pretool_arr[0]["matcher"], "Bash");
        assert_eq!(pretool_arr[0]["hooks"][0]["command"], "echo user");
        let stop_arr = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_arr.len(), 2);
    }

    #[test]
    fn uninstall_removes_all() {
        let mut root = json!({});
        install_hooks_in_value(&mut root, &test_translator()).expect("install");
        let removed = uninstall_hooks_from_value(&mut root);
        assert_eq!(removed.len(), MANAGED_HOOKS.len());
        assert!(root.get("hooks").is_none(), "empty hooks should be removed");
    }

    #[test]
    fn uninstall_preserves_user_entries() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    { "matcher": "", "hooks": [{ "type": "command", "command": "echo user-stop" }] }
                ]
            }
        });
        install_hooks_in_value(&mut root, &test_translator()).expect("install");
        // 반환값(제거된 항목 목록)은 다음 assert가 root 구조로 검증하므로 무시.
        uninstall_hooks_from_value(&mut root);
        let stop_arr = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_arr.len(), 1);
        let cmd = stop_arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("user-stop"));
    }

    #[test]
    fn uninstall_on_empty_settings_returns_empty() {
        let mut root = json!({});
        let removed = uninstall_hooks_from_value(&mut root);
        assert!(removed.is_empty());
    }

    #[test]
    fn is_marker_installed_in_value_works() {
        let mut root = json!({});
        let marker = tasty_hook_marker("stop");
        assert!(!is_marker_installed_in_value(&root, "Stop", &marker));
        install_hooks_in_value(&mut root, &test_translator()).expect("install");
        assert!(is_marker_installed_in_value(&root, "Stop", &marker));
    }

    /// 가변 데이터는 stdin으로 받으므로 명령에는 이벤트 이름만 달라야 한다.
    #[test]
    fn hook_command_matches_host_format() {
        assert_eq!(
            tasty_hook_command("stop"),
            "if [ -n \"$TASTY_SURFACE_ID\" ]; then tasty claude hook stop || true; fi"
        );
        assert_eq!(
            tasty_hook_command("session-start"),
            "if [ -n \"$TASTY_SURFACE_ID\" ]; then tasty claude hook session-start || true; fi"
        );
        assert_eq!(
            tasty_hook_command("notification"),
            "if [ -n \"$TASTY_SURFACE_ID\" ]; then tasty claude hook notification || true; fi"
        );
    }

    /// 환경 값이 없는 경우와 호출 실패 처리를 구분하도록 if 블록을 유지한다.
    #[test]
    fn hook_command_separates_guard_from_failure_handling() {
        for (_, token, _) in MANAGED_HOOKS {
            let cmd = tasty_hook_command(token);
            assert!(
                cmd.starts_with("if [ -n \"$TASTY_SURFACE_ID\" ]; then "),
                "가드가 if 블록이 아니다: {cmd}"
            );
            assert!(cmd.ends_with("; fi"), "블록이 닫히지 않았다: {cmd}");
            assert!(
                !cmd.contains("] && "),
                "옛 `A && B || true` 형태로 회귀했다: {cmd}"
            );
        }
    }

    /// 명령 형식이 바뀌어도 기존 항목을 찾는 marker는 유지해야 한다.
    #[test]
    fn new_command_still_contains_marker() {
        for (_, token, _) in MANAGED_HOOKS {
            let marker = tasty_hook_marker(token);
            assert!(
                tasty_hook_command(token).contains(&marker),
                "marker '{marker}' 를 잃었다"
            );
        }
    }

    /// 이전 버전 명령도 중복 추가 없이 현재 명령으로 갱신해야 한다.
    #[test]
    fn install_upgrades_previous_production_command_in_place() {
        let legacy = "[ -n \"$TASTY_SURFACE_ID\" ] && tasty claude hook stop || true";
        let mut root = json!({
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [{ "type": "command", "command": legacy }]
                }]
            }
        });
        let added = install_hooks_in_value(&mut root, &test_translator())
            .expect("install")
            .added;
        assert!(
            !added.contains(&"Stop"),
            "신규 추가가 아니라 upgrade 여야 한다"
        );

        let arr = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "중복 entry 가 생기면 hook 이 두 번 발화한다");
        assert_eq!(
            arr[0]["hooks"][0]["command"].as_str().unwrap(),
            tasty_hook_command("stop")
        );

        // 두 번째 install 도 멱등.
        install_hooks_in_value(&mut root, &test_translator()).expect("install again");
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    /// 실제 셸에서 TASTY_SURFACE_ID가 없으면 호출 없이 성공하는지 확인한다.
    #[cfg(unix)]
    #[test]
    fn guard_exits_silently_without_surface_id() {
        // `tasty` 가 PATH 에 없어도(=실행되면 반드시 실패) 가드가 막아 exit 0.
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(tasty_hook_command("stop"))
            .env_remove("TASTY_SURFACE_ID")
            .env("PATH", "/nonexistent")
            .output()
            .expect("/bin/sh");
        assert!(out.status.success(), "가드 통과 시 exit 0 이어야 한다");
        assert!(out.stdout.is_empty(), "stdout 무소음");
        assert!(
            out.stderr.is_empty(),
            "stderr 무소음: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 대상 id가 있으면 명령을 시도하고 실패해도 최종 종료 코드는 0이어야 한다.
    #[cfg(unix)]
    #[test]
    fn guard_runs_command_when_surface_id_present() {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(tasty_hook_command("stop"))
            .env("TASTY_SURFACE_ID", "999")
            .env("PATH", "/nonexistent")
            .output()
            .expect("/bin/sh");
        assert!(out.status.success(), "hook 실패는 턴을 막지 않는다(exit 0)");
        assert!(
            !out.stderr.is_empty(),
            "블록에 진입해 명령을 시도했어야 한다"
        );
    }

    /// SessionStart hook 도 다른 hook 과 동일한 단순 명령으로 설치되어야 한다.
    #[test]
    fn session_start_hook_uses_simple_command() {
        let mut root = json!({});
        install_hooks_in_value(&mut root, &test_translator()).expect("install");
        let arr = root["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            !cmd.contains("${CLAUDE_SESSION_ID}"),
            "SessionStart hook must not contain shell-expansion placeholder anymore (session_id arrives via stdin JSON), got: {}",
            cmd
        );
        assert!(
            !cmd.contains("--session"),
            "SessionStart hook must not pass --session via CLI (session_id is read from stdin), got: {}",
            cmd
        );
        assert!(cmd.contains("tasty claude hook session-start"));
    }

    /// background-start 명령은 백그라운드 호출 payload 만 CLI 에 넘기고, 넘길 때 stdin 을 그대로 전달한다.
    /// 포그라운드 호출과 Tasty 밖 셸에서는 CLI 를 실행하지 않는다. payload 는 Claude Code 2.1.291 실측이다.
    #[cfg(unix)]
    #[test]
    fn the_background_start_command_runs_the_cli_only_for_background_calls() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls.log");
        let fake = dir.path().join("tasty");
        tasty_test_support::write_executable(
            &fake,
            format!(
                "#!/bin/sh\n{{ echo \"$*\"; cat; echo; }} >> '{}'\n",
                log.display()
            ),
        )
        .unwrap();
        let path = format!(
            "{}:{}",
            dir.path().display(),
            std::env::var("PATH").unwrap()
        );
        let run = |payload: &str, surface: Option<&str>| {
            use std::io::Write;
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c")
                .arg(tasty_hook_command(crate::hook::BACKGROUND_START_EVENT))
                .env("PATH", &path)
                .stdin(std::process::Stdio::piped());
            match surface {
                Some(s) => cmd.env("TASTY_SURFACE_ID", s),
                None => cmd.env_remove("TASTY_SURFACE_ID"),
            };
            let mut child = cmd.spawn().unwrap();
            // Tasty 밖 셸에서는 명령이 stdin 을 읽지 않고 끝나므로 쓰기가 BrokenPipe 로 끝날 수 있다.
            let written = child.stdin.take().unwrap().write_all(payload.as_bytes());
            if let Err(e) = written {
                assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
            }
            assert!(child.wait().unwrap().success());
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        let fg_bash = r#"{"tool_name":"Bash","tool_input":{"command":"echo fg"},"tool_response":{"stdout":"fg","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false}}"#;
        let fg_agent = r#"{"tool_name":"Agent","tool_input":{"run_in_background":false},"tool_response":{"status":"completed","agentId":"a242555e6059db038","content":[{"type":"text","text":"say \"isAsync\":true"}]}}"#;
        let bg_bash = r#"{"tool_name":"Bash","tool_input":{"command":"sleep 2","run_in_background":true},"tool_response":{"stdout":"","backgroundTaskId":"bd0w3381s"}}"#;
        let bg_agent = r#"{"tool_name":"Agent","tool_input":{"run_in_background":true},"tool_response":{"isAsync":true,"status":"async_launched","agentId":"a22d7fc9077e1353c"}}"#;
        // Claude Code 2.1.319 실측 모양. 다른 도구의 값에 같은 글자가 있어도 JSON 이스케이프 때문에 맞지 않는다.
        let monitor = r#"{"hook_event_name":"PostToolUse","tool_name":"Monitor","tool_input":{"description":"quiet","timeout_ms":1500000,"command":"sleep 1500"},"tool_response":{"taskId":"bym7ibssy","timeoutMs":1500000,"persistent":false}}"#;
        let fg_bash_quoting = r#"{"tool_name":"Bash","tool_input":{"command":"echo \"tool_name\":\"Monitor\""},"tool_response":{"stdout":"\"tool_name\":\"Monitor\""}}"#;
        assert_eq!(run(fg_bash, Some("7")), "");
        assert_eq!(run(fg_agent, Some("7")), "");
        assert_eq!(run(fg_bash_quoting, Some("7")), "");
        assert_eq!(run(monitor, None), "");
        assert_eq!(run(bg_bash, None), "");
        let after_bash = run(bg_bash, Some("7"));
        assert_eq!(
            after_bash,
            format!("claude hook background-start\n{bg_bash}\n")
        );
        let after_agent = run(bg_agent, Some("7"));
        assert_eq!(
            after_agent,
            format!("{after_bash}claude hook background-start\n{bg_agent}\n")
        );
        assert_eq!(
            run(monitor, Some("7")),
            format!("{after_agent}claude hook background-start\n{monitor}\n")
        );
    }
}
