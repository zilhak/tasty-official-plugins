//! Codex 훅을 agent task 턴 보고로 바꾼다. Codex 의 Interrupt 는 답변 없이 턴을 끝낸다.

use serde_json::Value;
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_agent_common::task_turn::{TurnReport, send};

/// 턴의 시작·끝에 해당하는 이벤트를 호스트에 보고한다.
pub(crate) fn report<H: HostCall>(host: &H, event: &str, surface_id: u32, params: &Value) {
    if let Some(r) = turn_report(event, params) {
        send(host, "codex", surface_id, &r);
    }
}

fn turn_report<'a>(event: &str, params: &'a Value) -> Option<TurnReport<'a>> {
    match event {
        "prompt-submit" => Some(TurnReport::Started {
            prompt: params.get("prompt").and_then(|v| v.as_str()),
        }),
        "stop" => Some(TurnReport::Ended {
            final_answer: params
                .get("last_assistant_message")
                .and_then(|v| v.as_str()),
        }),
        "interrupt" => Some(TurnReport::Failed {
            error: "interrupted",
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_turn_boundaries_become_reports() {
        let p = json!({ "last_assistant_message": "answer" });
        assert_eq!(
            turn_report("prompt-submit", &p),
            Some(TurnReport::Started { prompt: None })
        );
        assert_eq!(
            turn_report("prompt-submit", &json!({ "prompt": "hi" })),
            Some(TurnReport::Started { prompt: Some("hi") })
        );
        assert_eq!(
            turn_report("stop", &p),
            Some(TurnReport::Ended {
                final_answer: Some("answer")
            })
        );
        assert_eq!(
            turn_report("stop", &json!({})),
            Some(TurnReport::Ended { final_answer: None })
        );
        assert_eq!(
            turn_report("interrupt", &p),
            Some(TurnReport::Failed {
                error: "interrupted"
            })
        );
        for other in ["session-start", "permission-request", "post-tool-use"] {
            assert_eq!(turn_report(other, &p), None, "{other}");
        }
    }
}
