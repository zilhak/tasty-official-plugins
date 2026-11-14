//! Codex 자식의 턴 경계를 부모 대화로 보낼 후보로 남긴다.
//!
//! 자식 surface에 수신자가 기록돼 있을 때만 동작한다. 완료 키는 thread와 턴 ID로 만든다. 턴 ID는
//! 생산단(Codex)이 주는 값만 쓰며 순번이나 본문으로 턴을 추정하지 않는다. 결과는 훅이 아니라
//! [`crate::delivery::turns`]가 소유 서버의 턴 상태로 확정한다.

use serde_json::Value;

use crate::delivery::turns::{self, Candidate};
use crate::delivery::{Ctx, ledger};
use crate::session::registry::{Session, TurnAttempt};
use crate::store;

/// 턴 경계를 남기는 훅.
fn is_boundary(event: &str) -> bool {
    matches!(event, "stop" | "interrupt" | "session-end")
}

/// 회차 표지를 기억하는 턴 수. 넘으면 가장 오래된 것을 버린다.
const MAX_TURN_ATTEMPTS: usize = 64;

/// 훅이 준 턴 ID. Codex 0.154의 UserPromptSubmit·Stop·Interrupt 입력은 turn_id를 반드시 준다.
fn turn_id_of(params: &Value) -> Option<&str> {
    params
        .get("turn_id")
        .and_then(Value::as_str)
        .filter(|t| crate::ids::is_plain_token(t, 64))
}

/// prompt-submit의 agent task 회차 표지를 그 턴 ID에 기록한다.
pub(crate) fn note_prompt(session: &mut Session, params: &Value) {
    let Some(turn_id) = turn_id_of(params) else {
        return;
    };
    let Some(attempt) = params
        .get("prompt")
        .and_then(Value::as_str)
        .and_then(tasty_plugin_agent_common::task_turn::attempt_marker)
    else {
        return;
    };
    if session.turn_attempts.iter().any(|t| t.turn_id == turn_id) {
        return;
    }
    session.turn_attempts.push(TurnAttempt {
        turn_id: turn_id.to_string(),
        attempt: attempt.to_string(),
    });
    if session.turn_attempts.len() > MAX_TURN_ATTEMPTS {
        session.turn_attempts.remove(0);
    }
}

/// 완료 키. 턴은 생산단이 준 turn_id로만 식별한다. turn_id가 없는 턴 경계는 키가 없다.
pub(crate) fn completion_key(session: &Session, event: &str, params: &Value) -> Option<String> {
    let who = session
        .thread
        .clone()
        .unwrap_or_else(|| format!("g{}", session.generation));
    if event == "session-end" {
        return Some(format!("codex:{who}:end.{}", session.generation));
    }
    turn_id_of(params).map(|turn| format!("codex:{who}:{turn}"))
}

fn attempt_of(session: &Session, params: &Value) -> Option<String> {
    let turn_id = turn_id_of(params)?;
    session
        .turn_attempts
        .iter()
        .find(|t| t.turn_id == turn_id)
        .map(|t| t.attempt.clone())
}

/// 턴 경계를 기록하고 실패한 처리 수를 돌려준다. 수신자가 없으면 아무것도 하지 않는다.
/// 턴 경계 훅의 반환 처리 결과.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub failures: usize,
    /// 지원하지 않는 입력이라 반환하지 않은 이유.
    pub rejected: Option<&'static str>,
}

fn failed() -> Report {
    Report {
        failures: 1,
        rejected: None,
    }
}

/// 턴 경계를 확인할 후보로 남긴다. 확인을 기다리지 않는다.
pub(crate) fn report(ctx: &Ctx<'_>, session: &Session, event: &str, params: &Value) -> Report {
    if !is_boundary(event) {
        return Report::default();
    }
    let (receiver_id, attach_parent) = match ledger::child_receiver(ctx.root, session.surface) {
        Ok(Some(r)) => (r.receiver_id, r.attach_parent),
        Ok(None) => return Report::default(),
        Err(e) => {
            tracing::warn!(
                "codex child return {}: record unreadable: {e}",
                session.surface
            );
            return failed();
        }
    };
    let candidate = match candidate(session, event, params, receiver_id, attach_parent) {
        Ok(candidate) => candidate,
        Err(report) => return report,
    };
    match turns::note(ctx, &candidate) {
        Ok(_) => Report::default(),
        Err(e) => {
            tracing::warn!(
                "codex child return {}: turn candidate: {e}",
                session.surface
            );
            failed()
        }
    }
}

/// 이 턴 경계의 반환 후보. 반환할 수 없으면 그 보고를 돌려준다.
fn candidate(
    session: &Session,
    event: &str,
    params: &Value,
    receiver_id: String,
    attach_parent: Option<String>,
) -> Result<Candidate, Report> {
    let Some(thread) = session.thread.clone() else {
        tracing::warn!(
            "codex child return {}: {event} before the session thread is known",
            session.surface
        );
        return Err(Report {
            failures: 1,
            rejected: Some("unknown_thread"),
        });
    };
    // 지원 버전(Codex 0.154)은 Stop·Interrupt에 turn_id를 반드시 준다. 없으면 다른 턴과 구분할
    // 수 없으므로 반환하지 않는다.
    let Some(completion_key) = completion_key(session, event, params) else {
        tracing::warn!(
            "codex child return {}: {event} without a valid turn_id is not supported; not returned",
            session.surface
        );
        return Err(Report {
            failures: 1,
            rejected: Some("missing_turn_id"),
        });
    };
    let id = crate::ids::new_id().map_err(|e| {
        tracing::warn!("codex child return {}: candidate id: {e}", session.surface);
        failed()
    })?;
    Ok(Candidate {
        id,
        completion_key,
        receiver_id,
        child_surface: session.surface,
        thread,
        generation: session.generation.clone(),
        turn_id: turn_id_of(params)
            .filter(|_| event != "session-end")
            .map(str::to_string),
        attempt_id: if event == "session-end" {
            None
        } else {
            attempt_of(session, params)
        },
        created_at_ms: store::now_ms(),
        attach_parent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T: &str = "01a12f74-dab9-7a00-8000-0123456789ab";

    fn session(thread: Option<&str>) -> Session {
        Session {
            generation: "0123456789abcdef".into(),
            surface: 7,
            thread: thread.map(str::to_string),
            tui_args: vec![],
            native_args: None,
            model: None,
            turn_attempts: vec![],
            listen: String::new(),
            token_file: None,
            socket_dir: None,
            created_at_ms: 0,
        }
    }

    #[test]
    fn the_key_is_the_producer_turn_id() {
        let s = session(Some(T));
        let k = |event: &str, p: Value| completion_key(&s, event, &p);
        assert_eq!(
            k("stop", json!({ "turn_id": "turn-9" })),
            Some(format!("codex:{T}:turn-9"))
        );
        assert_eq!(
            k("interrupt", json!({ "turn_id": "turn-9" })),
            Some(format!("codex:{T}:turn-9"))
        );
        assert_eq!(
            k("session-end", json!({ "turn_id": "x" })),
            Some(format!("codex:{T}:end.0123456789abcdef"))
        );
        assert_eq!(
            completion_key(&session(None), "stop", &json!({ "turn_id": "t" })),
            Some("codex:g0123456789abcdef:t".into())
        );
        // 턴 ID가 없거나 형식이 틀리면 키를 만들지 않는다(추정·난수 키 없음).
        assert_eq!(k("stop", json!({})), None);
        assert_eq!(k("stop", json!({ "turn_id": "a/b" })), None);
    }

    /// 회차 표지는 그 턴 ID의 프롬프트 것이다. 다음 턴의 프롬프트가 먼저 와도 바뀌지 않는다.
    #[test]
    fn the_attempt_marker_follows_its_turn_id() {
        let mut s = session(Some(T));
        note_prompt(
            &mut s,
            &json!({ "turn_id": "t1", "prompt": "[tasty-task-attempt:a1] one" }),
        );
        note_prompt(
            &mut s,
            &json!({ "turn_id": "t2", "prompt": "[tasty-task-attempt:a2] two" }),
        );
        note_prompt(&mut s, &json!({ "turn_id": "t3", "prompt": "no marker" }));
        let of = |t: &str| attempt_of(&s, &json!({ "turn_id": t }));
        assert_eq!(
            (of("t1"), of("t2"), of("t3")),
            (Some("a1".into()), Some("a2".into()), None)
        );
    }

    #[test]
    fn only_turn_boundaries_are_reported() {
        for e in ["stop", "interrupt", "session-end"] {
            assert!(is_boundary(e), "{e}");
        }
        assert!(!is_boundary("prompt-submit"));
        assert!(!is_boundary("notification"));
    }
}
