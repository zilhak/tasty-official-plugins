//! agent task 의 턴 보고. provider 훅이 턴의 시작과 끝을 호스트(`agent.task_turn_report`)에
//! 알린다. 호스트는 surface 에 묶인 agent task 회차가 있을 때만 적용하고, 없으면 무시한다.
//! 결과를 확정하거나 task 를 끝내는 것은 호스트다.

use serde_json::{Value, json};

use crate::host_call::HostCall;

/// 호스트가 agent task 지시 끝에 붙이는 회차 표지의 앞부분. 계약은 protocol 크레이트에 있다.
pub use tasty_plugin_protocol::task_turn::ATTEMPT_MARKER_PREFIX;

/// 프롬프트에 실린 회차 표지의 토큰. 마지막 표지를 쓴다.
pub fn attempt_marker(prompt: &str) -> Option<&str> {
    let start = prompt.rfind(ATTEMPT_MARKER_PREFIX)? + ATTEMPT_MARKER_PREFIX.len();
    let rest = &prompt[start..];
    let token = &rest[..rest.find(']')?];
    (!token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric())).then_some(token)
}

/// 훅 이벤트가 뜻하는 턴 보고.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnReport<'a> {
    /// 턴 시작. `prompt` 는 훅이 넘겨 준 프롬프트이고, 받지 못했으면 없다.
    Started {
        prompt: Option<&'a str>,
    },
    Ended {
        final_answer: Option<&'a str>,
    },
    Failed {
        error: &'a str,
    },
}

/// 보고 params.
pub fn report_params(provider: &str, surface_id: u32, report: &TurnReport<'_>) -> Value {
    match report {
        TurnReport::Started { prompt } => {
            let mut p = json!({
                "provider": provider,
                "surface_id": surface_id,
                "event": "turn_started",
            });
            // 프롬프트를 받았으면 표지 유무를 알린다. 프롬프트 본문은 보내지 않는다.
            if let Some(prompt) = prompt {
                p["prompt_seen"] = json!(true);
                p["attempt_marker"] = json!(attempt_marker(prompt));
            }
            p
        }
        TurnReport::Ended { final_answer } => json!({
            "provider": provider,
            "surface_id": surface_id,
            "event": "turn_ended",
            "final_answer": final_answer,
        }),
        TurnReport::Failed { error } => json!({
            "provider": provider,
            "surface_id": surface_id,
            "event": "turn_ended",
            "error": error,
        }),
    }
}

/// 보고를 보낸다. 실패는 로그만 남긴다(훅의 다른 처리를 막지 않는다).
pub fn send<H: HostCall>(host: &H, provider: &str, surface_id: u32, report: &TurnReport<'_>) {
    if let Err(e) = host.call(
        "agent.task_turn_report",
        report_params(provider, surface_id, report),
    ) {
        tracing::warn!("{provider} hook s{surface_id}: agent.task_turn_report failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_end_carries_the_final_answer_and_a_failure_carries_the_error() {
        let p = report_params(
            "claude",
            3,
            &TurnReport::Ended {
                final_answer: Some("done"),
            },
        );
        assert_eq!(p["event"], "turn_ended");
        assert_eq!(p["final_answer"], "done");
        assert!(p.get("error").is_none());
        let p = report_params(
            "codex",
            3,
            &TurnReport::Failed {
                error: "overloaded",
            },
        );
        assert_eq!(p["error"], "overloaded");
        let p = report_params("claude", 3, &TurnReport::Started { prompt: None });
        assert_eq!(p["event"], "turn_started");
        assert!(p.get("prompt_seen").is_none() && p.get("attempt_marker").is_none());
    }

    #[test]
    fn a_start_names_the_attempt_marker_of_its_prompt() {
        let started = |prompt| report_params("codex", 3, &TurnReport::Started { prompt });
        let p = started(Some("review\n\nmarker: [tasty-task-attempt:ab12]"));
        assert_eq!(p["prompt_seen"], true);
        assert_eq!(p["attempt_marker"], "ab12");
        assert!(p.get("prompt").is_none(), "본문은 보내지 않는다");
        let p = started(Some("just a user prompt"));
        assert_eq!(p["prompt_seen"], true);
        assert!(p["attempt_marker"].is_null());
    }

    #[test]
    fn only_a_closed_marker_with_a_plain_token_counts() {
        assert_eq!(
            attempt_marker("x [tasty-task-attempt:aa] y [tasty-task-attempt:bb]"),
            Some("bb")
        );
        assert_eq!(attempt_marker("[tasty-task-attempt:open"), None);
        assert_eq!(attempt_marker("[tasty-task-attempt:]"), None);
        assert_eq!(attempt_marker("[tasty-task-attempt:a b]"), None);
    }
}
