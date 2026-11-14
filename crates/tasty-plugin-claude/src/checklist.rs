//! Stop 훅에서 작업을 계속할지 판정한다. 본문·완료 표식·회차 상한은 gate 레지스트리에서 읽는다.
//! --gate를 생략하면 DEFAULT_GATE_NAME을 사용한다. 전역 훅에는 설치하지 않고 프로필로 적용한다.
//! block 응답의 reason은 Claude Code에 이어서 수행할 지시로 전달된다.
//!
//! prompt_id가 바뀌면 회차를 0부터 센다. 완료 표식이 있거나 상한에 도달하면 통과하고,
//! 그 외에는 회차를 올리고 block한다. prompt_id가 없으면 상태를 바꾸지 않고 통과한다.
//!
//! 게이트·세션별 회차는 checklist/gates/<gate>/rounds/<session_id>.json에 저장한다.
//! 같은 세션의 여러 게이트가 서로의 회차를 바꾸지 않도록 구분한다.
//! SessionEnd는 모든 게이트와 이전 버전 경로 checklist/rounds의 세션 상태를 정리한다.
//!
//! checklist/gates/<gate>/enabled.marker로 게이트를 재시작 없이 켜고 끈다.
//! 이전 버전의 checklist/enabled.marker는 기본 게이트로 한 번 옮긴다.
//! 이전 회차 상태는 옮기지 않지만 사용자가 켜 둔 설정은 유지하기 위한 차이다.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tasty_plugin_sdk::{HostHandle, IpcMethodError, i18n::Translator};
use tracing::warn;

/// 기본 완료 표식. 일반 문장과 겹치지 않는 문자열을 부분 일치로 찾는다.
/// 게이트 등록 시 별도 표식을 지정하지 않았을 때도 사용한다.
pub(crate) const SENTINEL: &str = "[[TASTY-CHECKLIST-DONE]]";

/// 상한 설정 항목의 storage key. `tasty-plugin.toml` 의
/// `[[contributes.settings_pages.items]]` 선언과 짝을 맞춘다.
const ROUND_LIMIT_STORAGE_KEY: &str = "continue_checklist_round_limit";

/// 게이트 자체 상한과 설정값이 없을 때 쓰는 최대 회차. 무한 연장을 막는다.
const DEFAULT_ROUND_LIMIT: u32 = 3;

/// 본문에 이 토큰이 있을 때만 목표 안내를 넣는다. 토큰 없는 사용자 본문은 그대로 둔다.
const GOAL_TOKEN: &str = "{{goal}}";

/// 목표가 있으면 토큰을 목표 안내로 바꾼다. 없으면 토큰과 단독 토큰 줄을 지운다.
/// 토큰이 없는 본문은 변경하지 않는다.
fn substitute_goal(body: &str, goal: Option<&str>, tr: &Translator) -> String {
    if !body.contains(GOAL_TOKEN) {
        return body.to_string();
    }
    match goal {
        Some(goal) => body.replace(GOAL_TOKEN, &tr.t_fmt("claude.checklist.goal_clause", goal)),
        // 토큰이 단독 줄이면 그 줄의 개행까지, 문장 중간이면 토큰만 걷어낸다.
        // 사용자가 등록한 본문 파일은 CRLF 일 수 있으므로 두 개행 형태를 모두 본다.
        None => body
            .replace(&format!("{GOAL_TOKEN}\r\n"), "")
            .replace(&format!("{GOAL_TOKEN}\n"), "")
            .replace(GOAL_TOKEN, ""),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoundState {
    prompt_id: String,
    rounds: u32,
}

impl RoundState {
    fn to_json(&self) -> Value {
        json!({ "prompt_id": self.prompt_id, "rounds": self.rounds })
    }

    fn from_json(v: &Value) -> Option<Self> {
        let prompt_id = v.get("prompt_id")?.as_str()?.to_string();
        let rounds = v.get("rounds")?.as_u64()?;
        Some(Self {
            prompt_id,
            rounds: rounds as u32,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Pass,
    Block { rounds: u32 },
}

/// prompt_id가 있는 요청의 완료 표식과 회차를 판정한다.
pub(crate) fn decide(
    stored: Option<&RoundState>,
    prompt_id: &str,
    last_assistant_message: &str,
    sentinel: &str,
    round_limit: u32,
) -> Decision {
    let current_rounds = match stored {
        Some(s) if s.prompt_id == prompt_id => s.rounds,
        _ => 0,
    };
    if last_assistant_message.contains(sentinel) {
        return Decision::Pass;
    }
    if current_rounds >= round_limit {
        return Decision::Pass;
    }
    Decision::Block {
        rounds: current_rounds + 1,
    }
}

fn checklist_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("checklist")
}

/// 게이트별 상태 디렉터리의 공통 루트. 세션 종료 시 이 아래를 순회한다.
fn gates_dir(data_dir: &Path) -> PathBuf {
    checklist_dir(data_dir).join("gates")
}

/// 게이트의 상태 디렉터리. 호출자는 is_valid_short_name으로 이름을 검증해야 한다.
fn gate_dir(data_dir: &Path, gate: &str) -> PathBuf {
    gates_dir(data_dir).join(gate)
}

fn rounds_dir(data_dir: &Path, gate: &str) -> PathBuf {
    gate_dir(data_dir, gate).join("rounds")
}

fn state_file(data_dir: &Path, gate: &str, session_id: &str) -> PathBuf {
    rounds_dir(data_dir, gate).join(format!("{session_id}.json"))
}

/// 게이트 축이 생기기 전 경로. 읽거나 쓰지 않고, session-end 정리에서만 지운다.
fn legacy_state_file(data_dir: &Path, session_id: &str) -> PathBuf {
    checklist_dir(data_dir)
        .join("rounds")
        .join(format!("{session_id}.json"))
}

fn marker_file(data_dir: &Path, gate: &str) -> PathBuf {
    gate_dir(data_dir, gate).join("enabled.marker")
}

/// 게이트 축이 생기기 전 경로. [`migrate_legacy_marker`] 만 건드린다.
fn legacy_marker_file(data_dir: &Path) -> PathBuf {
    checklist_dir(data_dir).join("enabled.marker")
}

/// 활성 파일이 있는지 확인한다. 데이터 디렉터리가 없거나 이름이 잘못되면 false.
/// gate 레지스트리를 읽기 전에 호출되므로 여기서도 경로에 넣을 이름을 검증한다.
pub(crate) fn marker_present(data_dir: Option<&Path>, gate: &str) -> bool {
    if !crate::gate::is_valid_short_name(gate) {
        return false;
    }
    data_dir
        .map(|d| marker_file(d, gate).is_file())
        .unwrap_or(false)
}

/// 이전 활성 파일을 기본 게이트로 옮긴다. enable·disable·status·hook에서 호출한다.
/// 실패는 경고만 남기며 이후 호출에서 다시 시도할 수 있다.
fn migrate_legacy_marker(data_dir: Option<&Path>) {
    let Some(dir) = data_dir else {
        return;
    };
    let legacy = legacy_marker_file(dir);
    if !legacy.is_file() {
        return;
    }
    if let Err(e) = move_marker_to_default_gate(dir, &legacy) {
        warn!(
            "checklist: failed to migrate the legacy marker {}: {e}",
            legacy.display()
        );
    }
}

/// 새 활성 파일을 만든 뒤 이전 파일을 지운다. 중간 실패 시 이전 파일을 남겨 재시도할 수 있게 한다.
fn move_marker_to_default_gate(dir: &Path, legacy: &Path) -> std::io::Result<()> {
    let gate = crate::gate::DEFAULT_GATE_NAME;
    std::fs::create_dir_all(gate_dir(dir, gate))?;
    std::fs::write(marker_file(dir, gate), "")?;
    match std::fs::remove_file(legacy) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn no_data_dir_err(tr: &Translator) -> IpcMethodError {
    IpcMethodError::new(tr.t("claude.checklist.no_data_dir"))
}

fn io_err(tr: &Translator, e: std::io::Error) -> IpcMethodError {
    IpcMethodError::new(tr.t_fmt("claude.checklist.io_error", &e.to_string()))
}

/// 대상 게이트 이름. IPC 직접 호출에도 CLI와 같은 기본값을 적용한다.
fn gate_param(params: &Value) -> &str {
    params
        .get("gate")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(crate::gate::DEFAULT_GATE_NAME)
}

/// 토글할 게이트는 등록 여부를 확인한다. 미등록 이름의 상태 디렉터리를 만들지 않기 위해서다.
/// 훅 실행은 등록이 제거된 세션도 종료할 수 있도록 미등록 게이트를 통과시킨다.
fn require_known_gate<'a>(
    data_dir: Option<&Path>,
    params: &'a Value,
    tr: &Translator,
) -> Result<&'a str, IpcMethodError> {
    let gate = gate_param(params);
    crate::gate::ensure_known(data_dir, gate, tr).map_err(|e| crate::gate::to_ipc_err(e, tr))?;
    Ok(gate)
}

/// 활성 파일을 만들어 게이트를 켠다.
pub(crate) fn handle_enable(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    migrate_legacy_marker(data_dir);
    let gate = require_known_gate(data_dir, params, tr)?;
    let dir = data_dir.ok_or_else(|| no_data_dir_err(tr))?;
    std::fs::create_dir_all(gate_dir(dir, gate)).map_err(|e| io_err(tr, e))?;
    std::fs::write(marker_file(dir, gate), "").map_err(|e| io_err(tr, e))?;
    Ok(json!({ "enabled": true }))
}

/// 활성 파일을 지워 게이트를 끈다. 파일이 없어도 성공한다.
pub(crate) fn handle_disable(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    migrate_legacy_marker(data_dir);
    let gate = require_known_gate(data_dir, params, tr)?;
    let dir = data_dir.ok_or_else(|| no_data_dir_err(tr))?;
    match std::fs::remove_file(marker_file(dir, gate)) {
        Ok(()) => Ok(json!({ "enabled": false })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({ "enabled": false })),
        Err(e) => Err(io_err(tr, e)),
    }
}

/// 활성 파일 존재 여부는 enabled, 등록 조회 성공 여부는 registered로 반환한다. 미등록 이름도 오류로 거절하지 않는다.
/// 전체 게이트의 상태는 gate-list로 조회한다.
pub(crate) fn handle_status(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    migrate_legacy_marker(data_dir);
    let gate = gate_param(params);
    Ok(json!({
        "enabled": marker_present(data_dir, gate),
        "registered": crate::gate::ensure_known(data_dir, gate, tr).is_ok(),
    }))
}

fn read_state(data_dir: &Path, gate: &str, session_id: &str) -> Option<RoundState> {
    let text = std::fs::read_to_string(state_file(data_dir, gate, session_id)).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    RoundState::from_json(&value)
}

fn write_state(data_dir: &Path, gate: &str, session_id: &str, state: &RoundState) {
    let dir = rounds_dir(data_dir, gate);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(
            "checklist: failed to create state dir {}: {e}",
            dir.display()
        );
        return;
    }
    let text = state.to_json().to_string();
    let path = state_file(data_dir, gate, session_id);
    if let Err(e) = std::fs::write(&path, text) {
        warn!(
            "checklist: failed to write round state {}: {e}",
            path.display()
        );
    }
}

fn remove_state(data_dir: &Path, gate: &str, session_id: &str) {
    remove_state_file(&state_file(data_dir, gate, session_id));
}

fn remove_state_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(
            "checklist: failed to remove round state {}: {e}",
            path.display()
        );
    }
}

/// 게이트 삭제 시 활성 파일과 회차 상태를 정리한다. 실패는 로그에 남긴다.
/// 등록이 제거된 게이트의 남은 상태는 훅에서 사용하지 않는다.
pub(crate) fn remove_gate_runtime_state(data_dir: Option<&Path>, gate: &str) {
    let Some(dir) = data_dir else {
        return;
    };
    // 디렉터리를 삭제하기 전에 경로에 넣을 이름을 다시 검증한다.
    if !crate::gate::is_valid_short_name(gate) {
        return;
    }
    let path = gate_dir(dir, gate);
    if let Err(e) = std::fs::remove_dir_all(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(
            "checklist: failed to remove gate runtime state {}: {e}",
            path.display()
        );
    }
}

/// SessionEnd에서 해당 세션의 회차 파일을 모든 게이트에서 지운다.
/// 호출자는 적용된 게이트 목록을 몰라도 된다.
pub(crate) fn remove_state_for_session(data_dir: Option<&Path>, session_id: &str) {
    let Some(dir) = data_dir else {
        return;
    };
    // 이전 버전 경로의 세션 파일도 정리한다.
    remove_state_file(&legacy_state_file(dir, session_id));
    let Ok(entries) = std::fs::read_dir(gates_dir(dir)) else {
        return; // 게이트가 하나도 붙지 않은 세션 — 디렉토리 자체가 없다.
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        remove_state_file(&path.join("rounds").join(format!("{session_id}.json")));
    }
}

/// 설정의 회차 상한을 조회한다. 기본값 선택은 resolve_round_limit이 처리한다.
fn fetch_settings_round_limit(host: &HostHandle) -> Option<u32> {
    host.call(
        "settings.get_plugin_setting",
        json!({ "storage_key": ROUND_LIMIT_STORAGE_KEY }),
    )
    .ok()
    .and_then(|v| v.get("value").and_then(|v| v.as_f64()))
    .map(|f| f.max(1.0).round() as u32)
}

/// surface의 목표를 조회한다. 없거나 조회에 실패하면 None으로 두고 게이트 판정은 계속한다.
fn fetch_goal(host: &HostHandle, surface_id: u32) -> Option<String> {
    host.call("memory.goal_get", json!({ "surface_id": surface_id }))
        .ok()
        // 미설정이면 응답이 `null` 이라 `value` 키 자체가 없다.
        .and_then(|v| v.get("value").and_then(|v| v.as_str()).map(str::to_string))
        .filter(|s| !s.trim().is_empty())
}

/// CLI가 채워 주는 surface·surface_id 두 키를 모두 받는다.
fn surface_id_param(params: &Value) -> Option<u32> {
    ["surface_id", "surface"]
        .iter()
        .find_map(|k| params.get(*k).and_then(|v| v.as_u64()))
        .and_then(|n| u32::try_from(n).ok())
}

/// 회차 상한은 게이트 정의, 공통 설정, DEFAULT_ROUND_LIMIT 순서로 선택한다.
/// 기본 게이트와 사용자 게이트 모두 같은 설정 폴백을 사용한다.
fn resolve_round_limit(gate_limit: Option<u32>, settings_limit: Option<u32>) -> u32 {
    gate_limit.or(settings_limit).unwrap_or(DEFAULT_ROUND_LIMIT)
}

/// CLI가 Stop 훅의 stdin JSON을 params로 옮겨 호출하는 진입점.
/// 활성 파일·session_id·prompt_id가 없거나 게이트를 읽지 못하면 빈 응답으로 통과시킨다.
/// 잘못된 훅 설정 때문에 세션 종료를 막지 않기 위한 처리다.
pub(crate) fn handle_checklist_hook(
    host: &HostHandle,
    data_dir: Option<&Path>,
    checklist_body: &str,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    hook_response(
        data_dir,
        checklist_body,
        tr,
        params,
        || fetch_settings_round_limit(host),
        || surface_id_param(params).and_then(|sid| fetch_goal(host, sid)),
    )
}

/// 호스트 없이 시험할 수 있도록 설정·목표 조회를 전달받는 판정 본체.
/// 설정은 게이트 자체 상한이 없을 때만 조회한다.
fn hook_response(
    data_dir: Option<&Path>,
    host_gate_body: &str,
    tr: &Translator,
    params: &Value,
    settings_round_limit: impl Fn() -> Option<u32>,
    session_goal: impl Fn() -> Option<String>,
) -> Result<Value, IpcMethodError> {
    migrate_legacy_marker(data_dir);
    let gate_name = gate_param(params);
    if !marker_present(data_dir, gate_name) {
        return Ok(json!({}));
    }
    let Some(data_dir) = data_dir else {
        return Ok(json!({}));
    };

    let Some(session_id) = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Ok(json!({}));
    };
    let Some(prompt_id) = params
        .get("prompt_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Ok(json!({}));
    };
    let last_message = params
        .get("last_assistant_message")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let Ok((owner, gate_def, gate_body)) = crate::gate::show(Some(data_dir), gate_name, tr) else {
        return Ok(json!({}));
    };
    // 기본 본문은 번역 캐시를 쓰고 사용자 본문은 매번 읽어 재등록한 내용을 반영한다.
    let body = if owner == "host" {
        host_gate_body
    } else {
        gate_body.as_str()
    };

    let stored = read_state(data_dir, gate_name, session_id);

    // stop_hook_active는 보조 확인용이며 판정을 바꾸지 않는다.
    // CLI의 bool 플래그 대신 stdin 값을 받을 수 있도록 문자열 인자로 선언한다.
    if let Some(active) = params.get("stop_hook_active").and_then(|v| v.as_str()) {
        let expected = stored
            .as_ref()
            .is_some_and(|s| s.prompt_id == prompt_id && s.rounds > 0);
        let active_bool = active == "true";
        if active_bool != expected {
            warn!(
                "checklist: stop_hook_active={active} disagrees with stored round state for session {session_id} (sanity check only, not acted on)"
            );
        }
    }

    let round_limit = resolve_round_limit(gate_def.round_limit, {
        // 게이트가 상한을 정했으면 공통 설정 조회는 생략한다.
        if gate_def.round_limit.is_some() {
            None
        } else {
            settings_round_limit()
        }
    });
    match decide(
        stored.as_ref(),
        prompt_id,
        last_message,
        &gate_def.sentinel,
        round_limit,
    ) {
        Decision::Pass => {
            remove_state(data_dir, gate_name, session_id);
            Ok(json!({}))
        }
        Decision::Block { rounds } => {
            write_state(
                data_dir,
                gate_name,
                session_id,
                &RoundState {
                    prompt_id: prompt_id.to_string(),
                    rounds,
                },
            );
            // 목표는 block할 본문에 토큰이 있을 때만 조회한다.
            let reason = if body.contains(GOAL_TOKEN) {
                substitute_goal(body, session_goal().as_deref(), tr)
            } else {
                body.to_string()
            };
            Ok(json!({ "decision": "block", "reason": reason }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_translator() -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, "en")
    }

    // 내장 언어의 본문에 실제 판정과 같은 완료 표식이 있는지 확인한다.

    #[test]
    fn checklist_body_contains_sentinel_in_every_locale() {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        for locale in tasty_i18n::BUILTIN_CODES {
            let translator = tasty_plugin_sdk::i18n::Translator::load(&lang_dir, locale);
            let body = translator.t("claude.checklist.body");
            assert!(
                body.contains(SENTINEL),
                "lang/{locale}.toml 의 claude.checklist.body 에 SENTINEL({SENTINEL}) 리터럴이 없음"
            );
        }
    }

    /// host 기본 본문과 같은 모양의 최소 본문 — 토큰이 단독 줄로 들어간 형태.
    const BODY_WITH_TOKEN: &str = "3. 후속 작업\n\n{{goal}}\n센티넬을 포함해라.\n";
    const BODY_WITHOUT_TOKEN: &str = "3. 후속 작업\n\n센티넬을 포함해라.\n";

    #[test]
    fn goal_present_is_substituted_into_the_body() {
        let out = substitute_goal(
            BODY_WITH_TOKEN,
            Some("게이트 배선 완료"),
            &test_translator(),
        );
        assert!(!out.contains(GOAL_TOKEN), "토큰이 남았다: {out}");
        assert!(
            out.contains("게이트 배선 완료"),
            "goal 텍스트가 없다: {out}"
        );
        // 3번 항목과 센티넬 지시는 그대로다.
        assert!(out.contains("3. 후속 작업"));
        assert!(out.contains("센티넬을 포함해라."));
    }

    #[test]
    fn goal_absent_removes_the_token_line_and_leaves_the_rest() {
        let out = substitute_goal(BODY_WITH_TOKEN, None, &test_translator());
        assert!(!out.contains(GOAL_TOKEN), "토큰이 남았다: {out}");
        // 토큰 줄만 사라지고 나머지는 토큰 없는 본문과 바이트 단위로 같다.
        assert_eq!(out, BODY_WITHOUT_TOKEN);
    }

    #[test]
    fn goal_absent_removes_the_token_line_in_crlf_bodies_too() {
        // CRLF 본문에서도 토큰 줄만 제거해야 한다.
        let crlf = BODY_WITH_TOKEN.replace('\n', "\r\n");
        let expected = BODY_WITHOUT_TOKEN.replace('\n', "\r\n");
        assert_eq!(substitute_goal(&crlf, None, &test_translator()), expected);
    }

    #[test]
    fn body_without_token_is_untouched_regardless_of_goal() {
        // 등록 게이트 하위호환 — goal 이 있어도 본문은 입력과 완전히 동일하다.
        let tr = test_translator();
        assert_eq!(
            substitute_goal(BODY_WITHOUT_TOKEN, Some("X"), &tr),
            BODY_WITHOUT_TOKEN
        );
        assert_eq!(
            substitute_goal(BODY_WITHOUT_TOKEN, None, &tr),
            BODY_WITHOUT_TOKEN
        );
    }

    // 내장 언어마다 목표 토큰과 치환 자리가 있는지 확인한다.

    #[test]
    fn checklist_body_contains_goal_token_in_every_locale() {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        for locale in tasty_i18n::BUILTIN_CODES {
            let translator = tasty_plugin_sdk::i18n::Translator::load(&lang_dir, locale);
            let body = translator.t("claude.checklist.body");
            assert!(
                body.contains(GOAL_TOKEN),
                "lang/{locale}.toml 의 claude.checklist.body 에 {GOAL_TOKEN} 토큰이 없음"
            );
        }
    }

    #[test]
    fn checklist_goal_clause_has_a_slot_in_every_locale() {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        for locale in tasty_i18n::BUILTIN_CODES {
            let translator = tasty_plugin_sdk::i18n::Translator::load(&lang_dir, locale);
            let clause = translator.t("claude.checklist.goal_clause");
            assert!(
                clause.contains("{}"),
                "lang/{locale}.toml 의 claude.checklist.goal_clause 에 goal 을 채울 {{}} 자리가 없음"
            );
        }
    }

    #[test]
    fn surface_id_param_accepts_either_key() {
        assert_eq!(surface_id_param(&json!({ "surface_id": 7 })), Some(7));
        assert_eq!(surface_id_param(&json!({ "surface": 9 })), Some(9));
        // 둘 다 있으면 CLI 층이 동기화한 값이라 어느 쪽을 봐도 같다.
        assert_eq!(
            surface_id_param(&json!({ "surface": 4, "surface_id": 4 })),
            Some(4)
        );
    }

    #[test]
    fn surface_id_param_rejects_missing_or_out_of_range() {
        assert_eq!(surface_id_param(&json!({})), None);
        assert_eq!(surface_id_param(&json!({ "surface_id": "3" })), None);
        assert_eq!(surface_id_param(&json!({ "surface_id": -1 })), None);
        assert_eq!(
            surface_id_param(&json!({ "surface_id": u64::from(u32::MAX) + 1 })),
            None
        );
    }

    fn state(prompt_id: &str, rounds: u32) -> RoundState {
        RoundState {
            prompt_id: prompt_id.to_string(),
            rounds,
        }
    }

    #[test]
    fn branch1_prompt_id_change_resets_counter_then_blocks() {
        // 이전 prompt_id 로 라운드 2까지 쌓여 있었지만, 새 prompt_id 가 들어오면
        // 0에서 다시 시작해 block(라운드 1)한다.
        let stored = state("old-prompt", 2);
        let d = decide(Some(&stored), "new-prompt", "아직 할 일 남음", SENTINEL, 3);
        assert_eq!(d, Decision::Block { rounds: 1 });
    }

    #[test]
    fn branch1_no_stored_state_treated_as_round_zero() {
        let d = decide(None, "p1", "진행 중", SENTINEL, 3);
        assert_eq!(d, Decision::Block { rounds: 1 });
    }

    #[test]
    fn branch2_sentinel_passes_regardless_of_round() {
        let stored = state("p1", 0);
        let msg = format!("작업을 다 마쳤습니다. {SENTINEL}");
        let d = decide(Some(&stored), "p1", &msg, SENTINEL, 3);
        assert_eq!(d, Decision::Pass);
    }

    #[test]
    fn branch2_sentinel_beats_backstop_even_at_limit() {
        // 완료 표식이 있으면 상한에 도달했어도 통과한다.
        let stored = state("p1", 3);
        let msg = format!("끝 {SENTINEL}");
        let d = decide(Some(&stored), "p1", &msg, SENTINEL, 3);
        assert_eq!(d, Decision::Pass);
    }

    #[test]
    fn branch3_round_limit_backstop_passes_without_sentinel() {
        let stored = state("p1", 3);
        let d = decide(Some(&stored), "p1", "아직도 하는 중", SENTINEL, 3);
        assert_eq!(d, Decision::Pass);
    }

    #[test]
    fn branch3_one_below_limit_still_blocks() {
        let stored = state("p1", 2);
        let d = decide(Some(&stored), "p1", "아직도 하는 중", SENTINEL, 3);
        assert_eq!(d, Decision::Block { rounds: 3 });
    }

    #[test]
    fn branch4_default_case_blocks_and_increments() {
        let stored = state("p1", 1);
        let d = decide(Some(&stored), "p1", "계속 작업 중", SENTINEL, 5);
        assert_eq!(d, Decision::Block { rounds: 2 });
    }

    /// 기본 표식이 아니라 이 게이트에 지정한 표식으로 판단한다.
    #[test]
    fn decide_uses_given_sentinel_not_the_default() {
        let msg = format!("끝났습니다 {SENTINEL}");
        assert_eq!(
            decide(None, "p1", &msg, "[[A-DONE]]", 3),
            Decision::Block { rounds: 1 }
        );
        assert_eq!(
            decide(None, "p1", "끝났습니다 [[A-DONE]]", "[[A-DONE]]", 3),
            Decision::Pass
        );
    }

    #[test]
    fn sentinel_substring_match_works_mid_message() {
        let msg = format!("전부 끝났습니다.\n\n{SENTINEL}\n\n추가로 궁금한 점 있으면 말씀하세요.");
        let d = decide(None, "p1", &msg, SENTINEL, 3);
        assert_eq!(d, Decision::Pass);
    }

    /// 마커 파일을 직접 만든다 — `handle_enable` 과 달리 게이트 등록 여부를 보지
    /// 않으므로, "켜 둔 뒤 등록이 지워진 게이트" 같은 상태도 만들 수 있다.
    fn setup_marker(dir: &Path, gate: &str) {
        std::fs::create_dir_all(gate_dir(dir, gate)).unwrap();
        std::fs::write(marker_file(dir, gate), "").unwrap();
    }

    const G: &str = crate::gate::DEFAULT_GATE_NAME;

    /// gate를 생략한 요청.
    fn no_gate() -> Value {
        json!({})
    }

    fn gate_params(gate: &str) -> Value {
        json!({ "gate": gate })
    }

    // 실제 호스트 IPC 없이 파일 상태와 전달받은 조회 결과로 판정한다.

    #[test]
    fn marker_absent_means_not_present() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!marker_present(Some(tmp.path()), G));
    }

    #[test]
    fn marker_present_after_touch() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), G);
        assert!(marker_present(Some(tmp.path()), G));
    }

    #[test]
    fn marker_present_false_without_data_dir() {
        assert!(!marker_present(None, G));
    }

    /// 레지스트리 조회 전에도 잘못된 경로 이름을 거절해야 한다.
    #[test]
    fn marker_present_rejects_names_that_are_not_valid_short_names() {
        let tmp = tempfile::tempdir().unwrap();
        // data_dir 바깥에 마커와 같은 이름의 파일을 두고 traversal 로 노려본다.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("enabled.marker"), "").unwrap();
        let inner = tmp.path().join("data");
        // 중간 디렉터리를 만들어 검증이 없으면 실제 바깥 파일에 닿는 입력을 준비한다.
        std::fs::create_dir_all(gates_dir(&inner)).unwrap();
        let escape = "../../../outside";
        assert!(
            gate_dir(&inner, escape).join("enabled.marker").exists(),
            "탈출 경로가 실제로 바깥 마커에 닿아야 이 테스트가 가드를 검증한다"
        );
        assert!(!marker_present(Some(&inner), escape));
        assert!(!marker_present(Some(&inner), "UPPER"));
    }

    #[test]
    fn markers_are_independent_per_gate() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", None);
        register_gate(tmp.path(), "gate-b", "[[B-DONE]]", None);

        handle_enable(Some(tmp.path()), &gate_params("gate-a"), &test_translator()).unwrap();
        assert!(marker_present(Some(tmp.path()), "gate-a"));
        assert!(!marker_present(Some(tmp.path()), "gate-b"));
        assert!(
            !marker_present(Some(tmp.path()), G),
            "host 기본 게이트까지 켜졌다"
        );

        // 한쪽을 꺼도 다른 쪽은 그대로다.
        handle_enable(Some(tmp.path()), &gate_params("gate-b"), &test_translator()).unwrap();
        handle_disable(Some(tmp.path()), &gate_params("gate-a"), &test_translator()).unwrap();
        assert!(!marker_present(Some(tmp.path()), "gate-a"));
        assert!(marker_present(Some(tmp.path()), "gate-b"));
    }

    #[test]
    fn enable_creates_marker_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!marker_present(Some(tmp.path()), G));
        // gate를 생략하면 기본 게이트를 켠다.
        let result = handle_enable(Some(tmp.path()), &no_gate(), &test_translator()).unwrap();
        assert_eq!(result, json!({ "enabled": true }));
        assert!(marker_present(Some(tmp.path()), G));
    }

    #[test]
    fn disable_removes_marker_file() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), G);
        assert!(marker_present(Some(tmp.path()), G));
        let result = handle_disable(Some(tmp.path()), &no_gate(), &test_translator()).unwrap();
        assert_eq!(result, json!({ "enabled": false }));
        assert!(!marker_present(Some(tmp.path()), G));
    }

    #[test]
    fn disable_is_idempotent_without_marker() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!marker_present(Some(tmp.path()), G));
        let result = handle_disable(Some(tmp.path()), &no_gate(), &test_translator()).unwrap();
        assert_eq!(result, json!({ "enabled": false }));
        // 두 번 더 꺼도 에러가 아니다.
        handle_disable(Some(tmp.path()), &no_gate(), &test_translator()).unwrap();
        handle_disable(Some(tmp.path()), &no_gate(), &test_translator()).unwrap();
    }

    #[test]
    fn status_round_trips_enable_disable() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        // enabled 필드는 상태 변경을 반영해야 한다.
        assert_eq!(
            handle_status(Some(tmp.path()), &no_gate(), &tr).unwrap()["enabled"],
            json!(false)
        );
        handle_enable(Some(tmp.path()), &no_gate(), &tr).unwrap();
        assert_eq!(
            handle_status(Some(tmp.path()), &no_gate(), &tr).unwrap()["enabled"],
            json!(true)
        );
        handle_disable(Some(tmp.path()), &no_gate(), &tr).unwrap();
        assert_eq!(
            handle_status(Some(tmp.path()), &no_gate(), &tr).unwrap()["enabled"],
            json!(false)
        );
    }

    #[test]
    fn enable_disable_status_error_without_data_dir() {
        assert!(handle_enable(None, &no_gate(), &test_translator()).is_err());
        assert!(handle_disable(None, &no_gate(), &test_translator()).is_err());
        // status 는 marker_present 와 동일하게 안전 폴백(false)이지 에러가 아니다.
        assert_eq!(
            handle_status(None, &no_gate(), &test_translator()).unwrap()["enabled"],
            json!(false)
        );
    }

    #[test]
    fn enable_and_disable_reject_unknown_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        let params = gate_params("no-such-gate");
        assert!(handle_enable(Some(tmp.path()), &params, &tr).is_err());
        assert!(handle_disable(Some(tmp.path()), &params, &tr).is_err());
        // 오타로 만든 게이트 디렉토리가 남지 않는다.
        assert!(!gate_dir(tmp.path(), "no-such-gate").exists());
        // 이름 규칙 위반도 같은 관문에서 걸린다.
        assert!(handle_enable(Some(tmp.path()), &gate_params("../evil"), &tr).is_err());
    }

    /// 조회는 관대하다 — 미등록 이름에도 에러가 아니라 `enabled: false`. 대신
    /// `registered: false` 로 오타를 알아볼 수 있게 한다.
    #[test]
    fn status_reports_disabled_for_unknown_gate_without_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            handle_status(
                Some(tmp.path()),
                &gate_params("no-such-gate"),
                &test_translator()
            )
            .unwrap(),
            json!({ "enabled": false, "registered": false })
        );
        // 이름 규칙 위반도 조회는 거부하지 않는다.
        assert_eq!(
            handle_status(
                Some(tmp.path()),
                &gate_params("../evil"),
                &test_translator()
            )
            .unwrap(),
            json!({ "enabled": false, "registered": false })
        );
        // host 기본 게이트는 실재하므로 registered 다.
        assert_eq!(
            handle_status(Some(tmp.path()), &no_gate(), &test_translator()).unwrap(),
            json!({ "enabled": false, "registered": true })
        );
    }

    #[test]
    fn enable_accepts_a_registered_gate() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", None);
        let params = gate_params("gate-a");
        handle_enable(Some(tmp.path()), &params, &test_translator()).unwrap();
        assert_eq!(
            handle_status(Some(tmp.path()), &params, &test_translator()).unwrap(),
            json!({ "enabled": true, "registered": true })
        );
    }

    #[test]
    fn legacy_marker_migrates_to_continue_checklist_gate() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checklist_dir(tmp.path())).unwrap();
        std::fs::write(legacy_marker_file(tmp.path()), "").unwrap();

        // 진입점 하나만 불러도 이관된다(여기서는 조회).
        assert_eq!(
            handle_status(Some(tmp.path()), &no_gate(), &test_translator()).unwrap()["enabled"],
            json!(true),
            "업그레이드 전에 켜 둔 마커가 조용히 꺼졌다"
        );
        assert!(marker_present(Some(tmp.path()), G));
        assert!(
            !legacy_marker_file(tmp.path()).exists(),
            "legacy 파일이 남아 다시 이관될 수 있다"
        );
    }

    #[test]
    fn legacy_migration_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        std::fs::create_dir_all(checklist_dir(tmp.path())).unwrap();
        std::fs::write(legacy_marker_file(tmp.path()), "").unwrap();

        migrate_legacy_marker(Some(tmp.path()));
        migrate_legacy_marker(Some(tmp.path()));
        assert!(marker_present(Some(tmp.path()), G));

        // 이관 후 비활성화한 게이트가 다음 조회에서 다시 켜져서는 안 된다.
        handle_disable(Some(tmp.path()), &no_gate(), &tr).unwrap();
        migrate_legacy_marker(Some(tmp.path()));
        handle_status(Some(tmp.path()), &no_gate(), &test_translator()).unwrap();
        assert!(
            !marker_present(Some(tmp.path()), G),
            "꺼 둔 게이트가 되살아났다"
        );
    }

    /// 등록을 제거하면 활성 파일과 회차 상태도 지운다.
    #[test]
    fn unregister_clears_gate_runtime_state() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(2));
        handle_enable(Some(tmp.path()), &gate_params("gate-a"), &tr).unwrap();
        write_state(tmp.path(), "gate-a", "sess-1", &state("p1", 1));
        assert!(marker_file(tmp.path(), "gate-a").is_file());
        assert!(state_file(tmp.path(), "gate-a", "sess-1").is_file());

        crate::gate::unregister(Some(tmp.path()), "gate-a").unwrap();

        assert!(
            !gate_dir(tmp.path(), "gate-a").exists(),
            "unregister 후에도 런타임 상태 디렉토리가 남았다"
        );
        assert!(!marker_present(Some(tmp.path()), "gate-a"));
        assert_eq!(read_state(tmp.path(), "gate-a", "sess-1"), None);
    }

    /// 같은 이름으로 다시 등록해도 이전 활성 상태와 회차를 물려받지 않는다.
    #[test]
    fn reregistered_gate_starts_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(3));
        handle_enable(Some(tmp.path()), &gate_params("gate-a"), &tr).unwrap();
        fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "진행 중"),
            None,
        );
        assert_eq!(
            read_state(tmp.path(), "gate-a", "sess-1").unwrap().rounds,
            1
        );

        crate::gate::unregister(Some(tmp.path()), "gate-a").unwrap();
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(3));

        assert_eq!(
            handle_status(Some(tmp.path()), &gate_params("gate-a"), &tr).unwrap(),
            json!({ "enabled": false, "registered": true }),
            "재등록한 게이트가 이전 켜짐 상태를 물려받았다"
        );
        assert_eq!(
            read_state(tmp.path(), "gate-a", "sess-1"),
            None,
            "재등록한 게이트가 이전 라운드 카운터를 물려받았다"
        );
        // 다시 켜고 실행하면 회차는 1부터다.
        handle_enable(Some(tmp.path()), &gate_params("gate-a"), &tr).unwrap();
        fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "진행 중"),
            None,
        );
        assert_eq!(
            read_state(tmp.path(), "gate-a", "sess-1").unwrap().rounds,
            1
        );
    }

    /// disable도 이전 활성 파일을 먼저 이관해야 이후 조회에서 다시 켜지지 않는다.
    #[test]
    fn legacy_migration_before_disable_does_not_resurrect() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        std::fs::create_dir_all(checklist_dir(tmp.path())).unwrap();
        std::fs::write(legacy_marker_file(tmp.path()), "").unwrap();

        handle_disable(Some(tmp.path()), &no_gate(), &tr).unwrap();
        assert!(!legacy_marker_file(tmp.path()).exists());
        assert_eq!(
            handle_status(Some(tmp.path()), &no_gate(), &tr).unwrap()["enabled"],
            json!(false),
            "끈 뒤에 legacy 마커가 다시 켜진 것으로 해석됐다"
        );
        let after_hook = fire(
            tmp.path(),
            &json!({
                "session_id": "sess-1",
                "prompt_id": "p1",
                "last_assistant_message": "진행 중",
            }),
            Some(3),
        );
        assert_eq!(after_hook, json!({}), "훅이 꺼진 게이트를 발동시켰다");
    }

    #[test]
    fn legacy_migration_is_noop_without_legacy_marker() {
        let tmp = tempfile::tempdir().unwrap();
        migrate_legacy_marker(Some(tmp.path()));
        migrate_legacy_marker(None);
        assert!(!marker_present(Some(tmp.path()), G));
    }

    #[test]
    fn legacy_marker_migration_does_not_touch_other_gates() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", None);
        std::fs::create_dir_all(checklist_dir(tmp.path())).unwrap();
        std::fs::write(legacy_marker_file(tmp.path()), "").unwrap();

        migrate_legacy_marker(Some(tmp.path()));
        assert!(marker_present(Some(tmp.path()), G));
        assert!(!marker_present(Some(tmp.path()), "gate-a"));
    }

    #[test]
    fn state_round_trip_and_removal() {
        let tmp = tempfile::tempdir().unwrap();
        let s = state("p1", 2);
        write_state(tmp.path(), "g", "sess-a", &s);
        assert_eq!(read_state(tmp.path(), "g", "sess-a"), Some(s));
        remove_state(tmp.path(), "g", "sess-a");
        assert_eq!(read_state(tmp.path(), "g", "sess-a"), None);
    }

    #[test]
    fn remove_state_for_session_is_noop_without_data_dir() {
        // panic 없이 조용히 넘어가야 한다.
        remove_state_for_session(None, "sess-a");
    }

    #[test]
    fn remove_state_for_session_noop_when_no_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        // 존재하지 않는 세션을 지우려 해도 에러/패닉 없음.
        remove_state_for_session(Some(tmp.path()), "never-existed");
    }

    #[test]
    fn concurrent_sessions_have_independent_state_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(tmp.path(), "g", "sess-a", &state("p1", 1));
        write_state(tmp.path(), "g", "sess-b", &state("p1", 5));
        assert_eq!(read_state(tmp.path(), "g", "sess-a").unwrap().rounds, 1);
        assert_eq!(read_state(tmp.path(), "g", "sess-b").unwrap().rounds, 5);
        remove_state(tmp.path(), "g", "sess-a");
        assert_eq!(read_state(tmp.path(), "g", "sess-a"), None);
        assert_eq!(read_state(tmp.path(), "g", "sess-b").unwrap().rounds, 5);
    }

    /// 같은 세션에서도 게이트별 회차는 서로 독립이다.
    #[test]
    fn gate_state_files_are_independent_per_gate() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(tmp.path(), "gate-a", "sess-1", &state("p1", 1));
        write_state(tmp.path(), "gate-b", "sess-1", &state("p1", 4));
        assert_eq!(
            read_state(tmp.path(), "gate-a", "sess-1").unwrap().rounds,
            1
        );
        assert_eq!(
            read_state(tmp.path(), "gate-b", "sess-1").unwrap().rounds,
            4
        );

        remove_state(tmp.path(), "gate-a", "sess-1");
        assert_eq!(read_state(tmp.path(), "gate-a", "sess-1"), None);
        assert_eq!(
            read_state(tmp.path(), "gate-b", "sess-1").unwrap().rounds,
            4
        );
    }

    #[test]
    fn remove_state_for_session_clears_all_gates() {
        let tmp = tempfile::tempdir().unwrap();
        for gate in ["gate-a", "gate-b", "gate-c"] {
            write_state(tmp.path(), gate, "sess-1", &state("p1", 2));
            // 지우면 안 되는 다른 세션도 함께 심어 둔다.
            write_state(tmp.path(), gate, "sess-2", &state("p1", 2));
        }

        remove_state_for_session(Some(tmp.path()), "sess-1");

        for gate in ["gate-a", "gate-b", "gate-c"] {
            assert_eq!(read_state(tmp.path(), gate, "sess-1"), None, "{gate}");
            assert!(read_state(tmp.path(), gate, "sess-2").is_some(), "{gate}");
        }
    }

    #[test]
    fn remove_state_for_session_noop_when_no_gates_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // gates/ 디렉토리가 아예 없는 상태 — 에러/패닉 없이 조용히 넘어가야 한다.
        assert!(!gates_dir(tmp.path()).exists());
        remove_state_for_session(Some(tmp.path()), "sess-1");
    }

    #[test]
    fn remove_state_for_session_also_clears_the_legacy_path() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = legacy_state_file(tmp.path(), "sess-1");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "{}").unwrap();

        remove_state_for_session(Some(tmp.path()), "sess-1");
        assert!(!legacy.exists(), "이전 버전 경로의 세션 상태 파일이 남았다");
    }

    #[test]
    fn round_limit_prefers_gate_value_over_settings() {
        assert_eq!(resolve_round_limit(Some(5), Some(2)), 5);
    }

    #[test]
    fn round_limit_falls_back_to_settings_then_default() {
        assert_eq!(resolve_round_limit(None, Some(7)), 7);
        assert_eq!(resolve_round_limit(None, None), DEFAULT_ROUND_LIMIT);
    }

    /// 센티넬을 포함한 본문 파일을 만들고 그 이름으로 게이트를 등록한다 —
    /// `gate::register` 가 본문의 센티넬 포함을 검증하므로 둘을 함께 준비해야 한다.
    fn register_gate(dir: &Path, name: &str, sentinel: &str, rounds: Option<u32>) {
        let body = dir.join(format!("{name}-body.md"));
        std::fs::write(&body, format!("게이트 {name} 본문\n{sentinel}\n")).unwrap();
        crate::gate::register(Some(dir), name, &body, Some(sentinel), rounds).unwrap();
    }

    fn hook_params(gate: &str, session: &str, prompt: &str, last: &str) -> Value {
        json!({
            "gate": gate,
            "session_id": session,
            "prompt_id": prompt,
            "last_assistant_message": last,
        })
    }

    fn fire(dir: &Path, params: &Value, settings_limit: Option<u32>) -> Value {
        fire_with_goal(dir, params, settings_limit, None)
    }

    /// 시험에서 목표 조회 결과를 직접 지정한다.
    fn fire_with_goal(
        dir: &Path,
        params: &Value,
        settings_limit: Option<u32>,
        goal: Option<&str>,
    ) -> Value {
        hook_response(
            Some(dir),
            "HOST-BODY",
            &test_translator(),
            params,
            || settings_limit,
            || goal.map(str::to_string),
        )
        .unwrap()
    }

    #[test]
    fn unknown_gate_passes_silently() {
        let tmp = tempfile::tempdir().unwrap();
        // 활성 파일만 남고 등록이 제거됐으면 통과해야 한다.
        setup_marker(tmp.path(), "no-such-gate");
        let params = hook_params("no-such-gate", "sess-1", "p1", "아직 작업 중");
        assert_eq!(fire(tmp.path(), &params, Some(3)), json!({}));
        // 라운드 상태 파일도 만들지 않는다.
        assert!(!rounds_dir(tmp.path(), "no-such-gate").exists());
    }

    #[test]
    fn hook_uses_the_gates_own_sentinel_and_body() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), "gate-a");
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(2));

        // 다른 게이트의 센티넬로는 통과하지 못한다.
        let blocked = fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "끝 [[B-DONE]]"),
            Some(9),
        );
        assert_eq!(blocked["decision"], "block");
        assert!(
            blocked["reason"]
                .as_str()
                .unwrap()
                .contains("게이트 gate-a"),
            "등록 게이트 본문이 주입되지 않았다: {blocked}"
        );

        // 자기 센티넬이면 통과.
        let passed = fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "끝 [[A-DONE]]"),
            Some(9),
        );
        assert_eq!(passed, json!({}));
    }

    /// 시험에서 기본 본문을 직접 지정한다.
    fn fire_host_body(dir: &Path, params: &Value, host_body: &str, goal: Option<&str>) -> Value {
        hook_response(
            Some(dir),
            host_body,
            &test_translator(),
            params,
            || Some(9),
            || goal.map(str::to_string),
        )
        .unwrap()
    }

    #[test]
    fn host_gate_reason_carries_the_goal_when_one_is_set() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), G);
        let out = fire_host_body(
            tmp.path(),
            &hook_params(G, "sess-1", "p1", "진행 중"),
            BODY_WITH_TOKEN,
            Some("게이트 배선 완료"),
        );
        assert_eq!(out["decision"], "block");
        let reason = out["reason"].as_str().unwrap();
        assert!(
            reason.contains("게이트 배선 완료"),
            "goal 이 안 실렸다: {reason}"
        );
        assert!(!reason.contains(GOAL_TOKEN));
    }

    #[test]
    fn host_gate_reason_matches_the_pre_token_body_without_a_goal() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), G);
        let out = fire_host_body(
            tmp.path(),
            &hook_params(G, "sess-1", "p1", "진행 중"),
            BODY_WITH_TOKEN,
            None,
        );
        assert_eq!(out["decision"], "block");
        // 목표가 없으면 토큰 줄만 제거한다.
        assert_eq!(out["reason"].as_str().unwrap(), BODY_WITHOUT_TOKEN);
    }

    #[test]
    fn registered_gate_body_is_byte_identical_even_when_a_goal_is_set() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), "gate-a");
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(9));
        let registered_body =
            std::fs::read_to_string(tmp.path().join("gates").join("bodies").join("gate-a.md"))
                .unwrap();

        let out = fire_with_goal(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "진행 중"),
            Some(9),
            Some("게이트 배선 완료"),
        );
        assert_eq!(out["decision"], "block");
        assert_eq!(
            out["reason"].as_str().unwrap(),
            registered_body,
            "토큰 없는 등록 게이트 본문이 goal 때문에 바뀌었다"
        );
    }

    #[test]
    fn hook_gate_round_limit_wins_over_settings() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), "gate-a");
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(1));

        // Settings 가 9 여도 게이트 상한 1 이 이긴다: 1 라운드 block 후 통과.
        let first = fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "진행 중"),
            Some(9),
        );
        assert_eq!(first["decision"], "block");
        assert_eq!(
            read_state(tmp.path(), "gate-a", "sess-1").unwrap().rounds,
            1
        );

        let second = fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "진행 중"),
            Some(9),
        );
        assert_eq!(second, json!({}));
    }

    #[test]
    fn two_gates_in_one_session_keep_independent_counters() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), "gate-a");
        setup_marker(tmp.path(), "gate-b");
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(2));
        register_gate(tmp.path(), "gate-b", "[[B-DONE]]", Some(5));

        for _ in 0..2 {
            fire(
                tmp.path(),
                &hook_params("gate-a", "sess-1", "p1", "진행 중"),
                None,
            );
            fire(
                tmp.path(),
                &hook_params("gate-b", "sess-1", "p1", "진행 중"),
                None,
            );
        }
        // gate-a 는 상한 2 에 도달, gate-b 는 아직 2/5.
        assert_eq!(
            read_state(tmp.path(), "gate-a", "sess-1").unwrap().rounds,
            2
        );
        assert_eq!(
            read_state(tmp.path(), "gate-b", "sess-1").unwrap().rounds,
            2
        );

        let a = fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "진행 중"),
            None,
        );
        let b = fire(
            tmp.path(),
            &hook_params("gate-b", "sess-1", "p1", "진행 중"),
            None,
        );
        assert_eq!(
            a,
            json!({}),
            "gate-a는 회차 상한에 도달했으므로 통과해야 한다"
        );
        assert_eq!(b["decision"], "block", "gate-b 는 아직 상한 전이다");
    }

    #[test]
    fn host_default_gate_is_used_when_no_gate_param_and_keeps_cached_body() {
        let tmp = tempfile::tempdir().unwrap();
        setup_marker(tmp.path(), G);
        // gate를 생략한 훅 요청.
        let params = json!({
            "session_id": "sess-1",
            "prompt_id": "p1",
            "last_assistant_message": "아직 작업 중",
        });
        let blocked = fire(tmp.path(), &params, Some(3));
        assert_eq!(blocked["decision"], "block");
        assert_eq!(
            blocked["reason"], "HOST-BODY",
            "host 기본 게이트는 캐시된 본문을 써야 한다"
        );
        // 상태를 기본 게이트 이름으로 저장한다.
        assert!(read_state(tmp.path(), crate::gate::DEFAULT_GATE_NAME, "sess-1").is_some());

        // 센티넬은 host 기본 센티넬.
        let passed = fire(
            tmp.path(),
            &json!({
                "session_id": "sess-1",
                "prompt_id": "p1",
                "last_assistant_message": format!("끝 {SENTINEL}"),
            }),
            Some(3),
        );
        assert_eq!(passed, json!({}));
    }

    /// 켜 둔 게이트에만 판정을 적용한다.
    #[test]
    fn only_the_enabled_gate_fires() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(3));
        register_gate(tmp.path(), "gate-b", "[[B-DONE]]", Some(3));
        handle_enable(Some(tmp.path()), &gate_params("gate-a"), &test_translator()).unwrap();

        let a = fire(
            tmp.path(),
            &hook_params("gate-a", "sess-1", "p1", "진행 중"),
            None,
        );
        let b = fire(
            tmp.path(),
            &hook_params("gate-b", "sess-1", "p1", "진행 중"),
            None,
        );
        assert_eq!(a["decision"], "block", "켜 둔 게이트가 발동하지 않았다");
        assert_eq!(b, json!({}), "꺼 둔 게이트가 발동했다");
        assert_eq!(read_state(tmp.path(), "gate-b", "sess-1"), None);
    }

    /// 훅 호출에서도 이전 활성 파일을 기본 게이트로 이관해야 한다.
    #[test]
    fn legacy_marker_keeps_the_hook_firing_without_any_command() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checklist_dir(tmp.path())).unwrap();
        std::fs::write(legacy_marker_file(tmp.path()), "").unwrap();

        let params = json!({
            "session_id": "sess-1",
            "prompt_id": "p1",
            "last_assistant_message": "아직 작업 중",
        });
        let blocked = fire(tmp.path(), &params, Some(3));
        assert_eq!(
            blocked["decision"], "block",
            "legacy 마커만 있는 인스턴스에서 훅이 조용히 꺼졌다"
        );
        assert!(!legacy_marker_file(tmp.path()).exists());
    }

    #[test]
    fn marker_off_passes_before_touching_the_gate_registry() {
        let tmp = tempfile::tempdir().unwrap();
        // 마커 없음 — 게이트를 등록해 두었어도 발동하지 않는다.
        register_gate(tmp.path(), "gate-a", "[[A-DONE]]", Some(2));
        let params = hook_params("gate-a", "sess-1", "p1", "진행 중");
        assert_eq!(fire(tmp.path(), &params, Some(3)), json!({}));
        assert_eq!(read_state(tmp.path(), "gate-a", "sess-1"), None);
    }

    /// 매니페스트의 --gate 기본값이 DEFAULT_GATE_NAME과 같아야 한다.
    #[test]
    fn manifest_gate_flag_default_matches_constant() {
        let manifest =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasty-plugin.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        let re = regex::Regex::new(r#"flag = "--gate".*?default = "([^"]+)""#).unwrap();
        // 선언이 여럿이다(훅 판정용 + enable/disable/status 토글용) — 하나라도
        // 어긋나면 같은 이름의 게이트를 가리키지 않게 되므로 전부 검사한다.
        let defaults: Vec<&str> = re
            .captures_iter(&text)
            .map(|c| c.extract::<1>().1[0])
            .collect();
        assert_eq!(
            defaults.len(),
            2,
            "tasty-plugin.toml 의 --gate 기본값 선언 수가 예상과 다르다: {defaults:?}"
        );
        for found in defaults {
            assert_eq!(found, crate::gate::DEFAULT_GATE_NAME);
        }
    }
}
