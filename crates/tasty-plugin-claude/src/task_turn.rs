//! Claude 훅을 agent task 턴 보고로 바꾼다. 턴을 끝내지 않는 Stop(백그라운드 대기·게이트
//! 보류)은 호출 전에 걸러진다. 게이트가 보류한 Stop 은 판정이 턴 종료로 확정될 때
//! [`report_ended`] 로 보고한다.
//!
//! 턴 종료가 확정되는 곳은 [`report_ended`] 와 `stop-failure` 보고뿐이다. Codex 부모에게 돌려보낼
//! 결과도 여기서만 만든다([`crate::codex_return`]).

use serde_json::Value;
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_agent_common::task_turn::{TurnReport, send};

use crate::codex_return::{ReturnStatus, Returns, TurnEnd};

/// 턴의 시작·끝에 해당하는 이벤트를 호스트에 보고한다. 턴을 끝내는 Stop 은 [`report_ended`] 를 거친다.
/// 결과 경로의 턴은 호스트에 보고하기 전에 정한다. 보고가 늦어지는 사이 새 턴이 열려도 바뀌지 않는다.
pub(crate) fn report<H: HostCall>(
    host: &H,
    returns: &Returns,
    event: &str,
    surface_id: u32,
    session: Option<&str>,
    params: &Value,
) {
    if event == "stop" {
        let final_answer = final_answer(params);
        let attempt = params
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        report_ended(
            host,
            returns,
            Ended {
                surface_id,
                session,
                final_answer: final_answer.as_deref(),
                attempt_id: attempt,
                turn: returns.turn_of(surface_id, session),
            },
        );
        return;
    }
    let Some(r) = turn_report(event, params) else {
        return;
    };
    let turn = matches!(r, TurnReport::Failed { .. })
        .then(|| returns.turn_of(surface_id, session))
        .flatten();
    send(host, "claude", surface_id, &r);
    if let TurnReport::Failed { error } = r
        && let Some(turn) = turn
    {
        returns.turn_end(TurnEnd {
            surface: surface_id,
            session,
            status: ReturnStatus::TurnFailed,
            body: Some(error),
            attempt_id: None,
            turn: Some(turn),
        });
    }
}

/// 확정된 턴 끝 한 건. `turn` 은 Stop 이 왔을 때 정한 결과 경로의 턴이며 경로가 없으면 없다.
pub(crate) struct Ended<'a> {
    pub surface_id: u32,
    pub session: Option<&'a str>,
    pub final_answer: Option<&'a str>,
    pub attempt_id: Option<&'a str>,
    pub turn: Option<crate::codex_return::TurnId>,
}

/// 턴 종료가 확정된 Stop 을 보고한다. 게이트가 보류한 Stop 은 보관한 최종 답변을 싣는다.
/// Codex 부모가 있는 자식이면 같은 시점에 그 결과를 부모에게 보낸다.
pub(crate) fn report_ended<H: HostCall>(host: &H, returns: &Returns, end: Ended<'_>) {
    send(
        host,
        "claude",
        end.surface_id,
        &TurnReport::Ended {
            final_answer: end.final_answer,
        },
    );
    if let Some(turn) = end.turn {
        returns.turn_end(TurnEnd {
            surface: end.surface_id,
            session: end.session,
            status: ReturnStatus::TurnEnded,
            body: end.final_answer,
            attempt_id: end.attempt_id,
            turn: Some(turn),
        });
    }
}

/// Stop 의 최종 답변. 게이트가 보류한 Stop 은 확정될 때까지 이 값을 보관한다.
pub(crate) fn final_answer(params: &Value) -> Option<String> {
    params
        .get("last_assistant_message")
        .and_then(Value::as_str)
        .map(String::from)
}

fn turn_report<'a>(event: &str, params: &'a Value) -> Option<TurnReport<'a>> {
    let text = |k: &str| params.get(k).and_then(|v| v.as_str());
    match event {
        "prompt-submit" => Some(TurnReport::Started {
            prompt: text("prompt"),
        }),
        "stop" => Some(TurnReport::Ended {
            final_answer: text("last_assistant_message"),
        }),
        "stop-failure" => Some(TurnReport::Failed {
            error: text("error").filter(|e| !e.is_empty()).unwrap_or("unknown"),
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
        let p = json!({ "last_assistant_message": "answer", "error": "rate_limit" });
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
            turn_report("stop-failure", &p),
            Some(TurnReport::Failed {
                error: "rate_limit"
            })
        );
        assert_eq!(
            turn_report("stop-failure", &json!({})),
            Some(TurnReport::Failed { error: "unknown" })
        );
        for other in [
            "notification",
            "session-end",
            "pre-tool-use",
            "subagent-stop",
        ] {
            assert_eq!(turn_report(other, &p), None, "{other}");
        }
    }
}
