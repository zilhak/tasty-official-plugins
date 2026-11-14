//! CLI에 공통인 reboot 인자 처리와 화면 확인. 대기 시간·키 입력·안내문은 각 플러그인이 정한다.

use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tasty_plugin_sdk::HostHandle;

/// 명령 접수 → kill 시작까지 기본 대기 (초). `--delay` 로 오버라이드.
pub const DEFAULT_DELAY_SECS: u64 = 5;

/// 붙여넣기와 함께 전달된 Enter가 제출로 처리되지 않을 수 있어 추가 Enter 전에 기다린다.
const NOTICE_SUBMIT_DELAY: Duration = Duration::from_millis(500);

/// 기본 안내문과 추가 텍스트 사이에 빈 줄 하나를 넣는다. 번역된 문구는 호출자가 제공한다.
pub fn build_notice(base: &str, extra: Option<&str>) -> String {
    match extra {
        Some(t) => format!("{base}\n\n{t}"),
        None => base.to_string(),
    }
}

/// `surface.screen_text` 1 회 조회. 실패 → `None`(surface 소멸 등).
pub fn screen_text<H: crate::host_call::HostCall>(host: &H, surface_id: u32) -> Option<String> {
    host.call("surface.screen_text", json!({ "surface_id": surface_id }))
        .ok()
        .and_then(|r| r.get("text").and_then(|t| t.as_str().map(str::to_string)))
}

/// 화면에 문구가 있는지 확인한다. 조회 실패는 false로 처리한다.
pub fn screen_contains<H: crate::host_call::HostCall>(
    host: &H,
    surface_id: u32,
    needle: &str,
) -> bool {
    screen_text(host, surface_id)
        .map(|t| t.contains(needle))
        .unwrap_or(false)
}

/// delay와 prompt를 읽는다. 빈 prompt는 생략한 것으로 처리한다.
pub fn parse_options(params: &Value) -> (u64, Option<String>) {
    let delay = params
        .get("delay")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_DELAY_SECS);
    let extra = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    (delay, extra)
}

/// 셸에 그대로 넣을 session id를 영문·숫자·하이픈·밑줄로 제한한다.
pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 추가 Enter를 보낸다. 화면 확인은 호출자가 먼저 수행해야 한다.
pub fn ensure_submitted(host: &HostHandle, surface_id: u32, agent: &str) {
    thread::sleep(NOTICE_SUBMIT_DELAY);
    if let Err(e) = host.call(
        "surface.send_key",
        json!({ "surface_id": surface_id, "key": "enter" }),
    ) {
        tracing::warn!("{agent} reboot s{surface_id}: extra submit enter failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_delay_and_no_prompt() {
        let (delay, extra) = parse_options(&json!({ "surface_id": 1 }));
        assert_eq!(delay, DEFAULT_DELAY_SECS);
        assert!(extra.is_none());
    }

    #[test]
    fn parse_explicit_delay_and_prompt() {
        let (delay, extra) = parse_options(&json!({ "delay": 2, "prompt": "빌드부터 다시 확인" }));
        assert_eq!(delay, 2);
        assert_eq!(extra.as_deref(), Some("빌드부터 다시 확인"));
    }

    #[test]
    fn parse_empty_prompt_treated_as_none() {
        let (_, extra) = parse_options(&json!({ "prompt": "" }));
        assert!(extra.is_none());
    }

    #[test]
    fn safe_session_id_accepts_uuid() {
        assert!(is_safe_session_id("0e5cbdf4-32a1-4a5c-9c1d-8f2b3a4c5d6e"));
        assert!(is_safe_session_id("019f55e7-3dfa-7292-a8a9-9cf73a8b000b"));
        assert!(is_safe_session_id("abc_DEF-123"));
    }

    #[test]
    fn safe_session_id_rejects_shell_metachars() {
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("abc; rm -rf /"));
        assert!(!is_safe_session_id("a b"));
        assert!(!is_safe_session_id("a$(x)"));
        assert!(!is_safe_session_id("a&b"));
    }
}
