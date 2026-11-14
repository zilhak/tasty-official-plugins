use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use super::ledger::{self, DeliveryState};
use super::send::{self, JobKind, Outcome, fake};
use super::testing::{Fixture, TOKEN};
use super::*;

const PARENT: &str = "019f55e7-3dfa-7a00-8000-0000000000aa";
const OTHER: &str = "019f55e7-3dfa-7a00-8000-0000000000bb";
const CHILD: &str = "019f55e7-3dfa-7a00-8000-0000000000cc";

#[test]
fn execution_survives_reload_and_refresh_without_changing_receivers_or_claims() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let context = |thread: &str, name: &str| {
        let dir = fx.root().join(name);
        json!({"_tasty_context": {
            "provider": "codex", "thread_id": thread,
            "execution": {"executable": dir.join("codex"), "search_path": dir}
        }})
    };
    let prepare = |params| fx.with(|d| handle_parent(d, &params)).unwrap();
    let a = prepare(context(PARENT, "first"));
    let rid = a["receiver_id"].as_str().unwrap();
    let id = deliver_id(&fx, rid, "claude:one");
    fx.take_jobs();
    let other = prepare(context(OTHER, "other"));
    assert_ne!(a["receiver_id"], other["receiver_id"]);
    let updated = prepare(context(PARENT, "second"));
    assert_eq!(updated["receiver_id"], a["receiver_id"]);
    assert_eq!(deliver_id(&fx, rid, "claude:one"), id);

    // 디스크에서 다시 읽어 새 plugin 프로세스와 같은 입력으로 전송 경로를 고른다.
    let stored = ledger::read_receiver(fx.root(), rid).unwrap().unwrap();
    let expected = fx.root().join("second");
    let execution = stored.execution.as_ref().unwrap();
    assert_eq!(execution.executable, expected.join("codex"));
    assert_eq!(execution.search_path, expected.to_str().unwrap());
    let endpoint = fx.with(|d| target(d, &stored)).unwrap().0;
    assert!(matches!(endpoint, Endpoint::Proxy { execution: Some(e), .. } if e == *execution));
    let other = ledger::read_receiver(fx.root(), other["receiver_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        other.execution.unwrap().executable,
        fx.root().join("other/codex")
    );
    // 이전 CLI는 저장된 실행 정보를 지우지 않는다.
    parent(&fx, PARENT).unwrap();
    assert_eq!(
        ledger::read_receiver(fx.root(), rid).unwrap().unwrap(),
        stored
    );
}

fn ctx_of(thread: &str) -> Value {
    json!({ "provider": "codex", "thread_id": thread })
}

/// 모든 호출에 성공하는 호스트.
struct OkHost;

impl HostCall for OkHost {
    fn call(&self, _method: &str, _params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        Ok(json!({}))
    }
}

/// thread가 surface의 현재 세대에서 시작했다고 기록한다.
fn start_thread(fx: &Fixture, surface: u32, thread: &str) -> String {
    let generation = fx.open(surface, &[]);
    fx.with(|d| {
        crate::handlers::handle_hook(
            &OkHost,
            &json!({"surface": surface, "event": "session-start", "session": thread, "gen": generation}),
            &fx.tr,
            d,
        )
    })
    .unwrap();
    generation
}

fn parent(fx: &Fixture, thread: &str) -> Result<Value, IpcMethodError> {
    fx.with(|d| handle_parent(d, &json!({ "_tasty_context": ctx_of(thread) })))
}

fn deliver_params(receiver: &str, key: &str) -> Value {
    json!({
        "receiver_id": receiver,
        "completion_key": key,
        "provider": "claude",
        "child_surface": 9,
        "child_session": null,
        "status": "turn_ended",
        "body": "result text",
    })
}

fn deliver(fx: &Fixture, params: Value) -> Result<Value, IpcMethodError> {
    fx.with(|d| handle_deliver(d, &params))
}

fn event(fx: &Fixture, id: &str) -> ledger::Event {
    ledger::read_event(fx.root(), id).unwrap().unwrap()
}

fn outcome(fx: &Fixture, id: &str, kind: &str, result: Outcome) -> Value {
    fx.with(|d| {
        handle_outcome(
            d,
            &json!({ "event": id, "token": TOKEN, "kind": kind, "result": result }),
        )
    })
    .unwrap()
}

/// 재시도 시각을 지금으로 당긴다.
fn make_due(fx: &Fixture, id: &str) {
    let mut ev = event(fx, id);
    ev.delivery.next_attempt_at_ms = 0;
    ledger::write_event(fx.root(), &ev).unwrap();
}

fn wake(fx: &Fixture, thread: &str) {
    fx.with(|d| handle_wake(d, &json!({ "thread": thread, "token": TOKEN })))
        .unwrap();
}

fn rid_of(fx: &Fixture, thread: &str) -> String {
    parent(fx, thread).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn deliver_id(fx: &Fixture, rid: &str, key: &str) -> String {
    deliver(fx, deliver_params(rid, key)).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn an_owned_parent_receives_through_its_tasty_surface() {
    let fx = Fixture::new();
    start_thread(&fx, 5, PARENT);
    let out = parent(&fx, PARENT).unwrap();
    assert!(
        crate::ids::is_id(out["receiver_id"].as_str().unwrap()),
        "{out}"
    );
    // 소유 부모도 호스트에는 thread 키로 자식을 둔다. surface 5는 실행 위치일 뿐이다.
    assert_eq!(
        out["terminal_parent"],
        json!({ "parent_external": format!("codex:{PARENT}") })
    );
}

#[test]
fn an_external_parent_must_be_loaded_in_the_shared_server() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let out = parent(&fx, PARENT).unwrap();
    assert_eq!(
        out["terminal_parent"],
        json!({ "parent_external": format!("codex:{PARENT}") })
    );

    fx.loaded = Ok(false);
    let err = parent(&fx, PARENT).unwrap_err();
    assert!(err.message.contains(PARENT), "{}", err.message);
    fx.loaded = Err("proxy failed".into());
    let err = parent(&fx, PARENT).unwrap_err();
    assert!(err.message.contains("proxy failed"), "{}", err.message);

    let err = fx.with(|d| handle_parent(d, &json!({}))).unwrap_err();
    assert!(err.message.contains("Codex"), "{}", err.message);
    let bad = fx.with(|d| {
        handle_parent(
            d,
            &json!({ "_tasty_context": { "provider": "codex", "thread_id": "x" } }),
        )
    });
    assert!(bad.is_err());
}

#[test]
fn a_completion_is_captured_once_and_sent_in_the_background() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    let first = deliver(&fx, deliver_params(&rid, "claude:s:1")).unwrap();
    let id = first["event_id"].as_str().unwrap().to_string();
    assert!(crate::ids::is_id(&id));
    assert_eq!(first["delivery_state"], "sending");
    assert!(first.get("duplicate").is_none());
    let again = deliver(&fx, deliver_params(&rid, "claude:s:1")).unwrap();
    assert_eq!(again["event_id"], first["event_id"]);
    assert_eq!(again["duplicate"], true);

    let jobs = fx.take_jobs();
    assert_eq!(jobs.len(), 1, "중복 기록은 다시 보내지 않는다");
    assert_eq!(jobs[0].kind, JobKind::Send);
    assert!(
        jobs[0].check_loaded,
        "외부 부모는 보내기 전에 로드 여부를 확인한다"
    );
    assert!(matches!(
        jobs[0].endpoint,
        crate::rpc::Endpoint::Proxy { .. }
    ));
    assert!(
        !jobs[0].text.contains("result text"),
        "본문은 큐 메시지에 넣지 않는다"
    );
    assert!(jobs[0].text.contains(&id));
}

#[test]
fn the_contract_fields_are_validated_and_nullable_fields_survive() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    for (key, value) in [
        ("status", json!("completed")),
        ("provider", json!("gemini")),
        ("completion_key", json!("a/b")),
        ("completion_key", json!("k".repeat(161))),
        ("body", json!(3)),
        ("child_surface", json!(-1)),
        ("receiver_id", json!("0123")),
    ] {
        let mut p = deliver_params(&rid, "k:1");
        p[key] = value.clone();
        assert!(deliver(&fx, p).is_err(), "{key}={value}");
    }
    for status in ledger::STATUSES {
        let mut p = deliver_params(&rid, &format!("k:{status}"));
        p["status"] = json!(status);
        p["body"] = Value::Null;
        let id = deliver(&fx, p).unwrap()["event_id"]
            .as_str()
            .unwrap()
            .to_string();
        let ev = event(&fx, &id);
        assert_eq!(
            (
                ev.status.as_str(),
                ev.body.as_deref(),
                ev.child_session.as_deref()
            ),
            (*status, None, None)
        );
    }
    let mut p = deliver_params(&rid, "k:big");
    p["body"] = json!("x".repeat(ledger::BODY_MAX + 10));
    let id = deliver(&fx, p).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let read = fx
        .with(|d| handle_result(d, &json!({ "event": id, "_tasty_context": ctx_of(PARENT) })))
        .unwrap();
    assert_eq!(read["body_truncated"], true);
    assert_eq!(read["body"].as_str().unwrap().len(), ledger::BODY_MAX);
    assert_eq!(read["child_session"], Value::Null);
}

#[test]
fn an_owned_parent_without_a_ready_tui_holds_until_it_attaches() {
    let fx = Fixture::new();
    let generation = start_thread(&fx, 5, PARENT);
    let rid = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = deliver(&fx, deliver_params(&rid, "k:1")).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(event(&fx, &id).delivery.state, DeliveryState::Held);
    assert!(fx.take_jobs().is_empty());

    fx.ready(&generation, "unix:///owned/s.sock");
    fx.with(|d| flush_thread(d, PARENT));
    let jobs = fx.take_jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs[0].endpoint,
        crate::rpc::Endpoint::Unix("/owned/s.sock".into())
    );
    assert!(!jobs[0].check_loaded);
    assert_eq!(event(&fx, &id).delivery.state, DeliveryState::Sending);
}

#[test]
fn outcomes_apply_only_from_this_process_and_only_while_sending() {
    let clock = crate::store::test_clock::freeze();
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = deliver(&fx, deliver_params(&rid, "k:1")).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let forged = fx.with(|d| {
        handle_outcome(
            d,
            &json!({ "event": id, "token": "x", "kind": "send",
            "result": { "outcome": "not_loaded" } }),
        )
    });
    assert!(forged.is_err(), "다른 token의 결과는 받지 않는다");
    assert_eq!(fx.take_jobs().len(), 1);

    // 아무것도 보내지 못한 이벤트는 captured로 돌아가고 backoff 뒤 다시 보낸다.
    let out = outcome(
        &fx,
        &id,
        "send",
        Outcome::NotSent {
            detail: "refused".into(),
        },
    );
    assert_eq!(out["delivery_state"], "captured");
    assert!(
        fx.take_jobs().is_empty(),
        "재시도 시각 전에는 보내지 않는다"
    );
    assert_eq!(
        fx.rec.wakes.borrow().as_slice(),
        &[(PARENT.to_string(), backoff(1))]
    );
    clock.advance(backoff(1) - Duration::from_millis(1));
    wake(&fx, PARENT);
    assert!(
        fx.take_jobs().is_empty(),
        "재시도 시각 1ms 전에도 보내지 않는다"
    );
    clock.advance(Duration::from_millis(1));
    wake(&fx, PARENT);
    assert_eq!(fx.take_jobs().len(), 1, "재시도 시각이 되면 보낸다");
    let accepted = Outcome::Accepted {
        submission_id: Some("sub".into()),
        confirmed_by: "queue_add".into(),
    };
    assert_eq!(
        outcome(&fx, &id, "send", accepted.clone())["delivery_state"],
        "accepted"
    );
    assert_eq!(
        event(&fx, &id).delivery.submission_id.as_deref(),
        Some("sub")
    );
    assert_eq!(
        outcome(&fx, &id, "send", accepted)["applied"],
        false,
        "끝난 이벤트는 바꾸지 않는다"
    );
    fx.with(|d| flush_thread(d, PARENT));
    assert!(
        fx.take_jobs().is_empty(),
        "수락된 이벤트는 다시 보내지 않는다"
    );
}

#[test]
fn two_parents_receive_only_their_own_children() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let a = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = parent(&fx, OTHER).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    let ea = deliver(&fx, deliver_params(&a, "k:1")).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let eb = deliver(&fx, deliver_params(&b, "k:1")).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ea, eb, "같은 완료 키라도 수신자가 다르면 별도 이벤트");
    let jobs = fx.take_jobs();
    let target = |e: &str| {
        jobs.iter()
            .find(|j| j.event_id == e)
            .unwrap()
            .thread
            .clone()
    };
    assert_eq!(
        (target(&ea), target(&eb)),
        (PARENT.to_string(), OTHER.to_string())
    );
    let cross =
        fx.with(|d| handle_result(d, &json!({ "event": ea, "_tasty_context": ctx_of(OTHER) })));
    assert!(cross.is_err(), "다른 대화의 결과는 읽지 못한다");
}

#[test]
fn a_restart_verifies_sending_once_and_resends_captured() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    let sending = deliver_id(&fx, &rid, "k:1");
    // 같은 thread는 한 번에 한 건만 보내므로 두 번째는 기다린다.
    let captured = deliver_id(&fx, &rid, "k:2");
    assert_eq!(
        event(&fx, &captured).delivery.state,
        DeliveryState::Captured
    );
    assert_eq!(fx.take_jobs().len(), 1);

    // 다시 시작한 프로세스는 송신 중이던 상태를 모른다.
    let fx = fx.restarted();
    fx.with(recover);
    assert_eq!(event(&fx, &sending).delivery.state, DeliveryState::Unknown);
    let jobs = fx.take_jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        (jobs[0].event_id.as_str(), jobs[0].kind),
        (sending.as_str(), JobKind::Verify),
        "수락 여부를 모르면 다시 보내지 않고 확인만 한다"
    );

    let out = outcome(
        &fx,
        &sending,
        "verify",
        Outcome::StillUnknown {
            detail: "absent".into(),
        },
    );
    assert_eq!(out["delivery_state"], "unknown");
    let jobs = fx.take_jobs();
    assert_eq!(jobs.len(), 1, "확인이 끝나면 다음 이벤트를 보낸다");
    assert_eq!(
        (jobs[0].event_id.as_str(), jobs[0].kind),
        (captured.as_str(), JobKind::Send)
    );
    let fx = fx.restarted();
    fx.with(recover);
    assert!(
        fx.take_jobs().iter().all(|j| j.event_id != sending),
        "확인한 unknown은 다시 확인하지도 보내지도 않는다"
    );
}

/// 송신은 thread마다 한 건, 전체 [`MAX_INFLIGHT`]건까지다. 완료 하나를 기록할 때 지난 이벤트를
/// 다시 보내려고 원장 전체를 훑지 않는다.
#[test]
fn sending_is_bounded_per_thread_and_overall() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    for i in 0..50 {
        deliver_id(&fx, &rid, &format!("k:{i}"));
    }
    assert_eq!(fx.take_jobs().len(), 1, "같은 thread는 한 건씩");
    let threads: Vec<String> = (0..8)
        .map(|i| format!("019f55e7-3dfa-7a00-8000-0000000001{i:02x}"))
        .collect();
    for t in &threads {
        let r = rid_of(&fx, t);
        deliver_id(&fx, &r, "k:1");
    }
    assert_eq!(fx.pump.inflight(), MAX_INFLIGHT);
    assert_eq!(fx.take_jobs().len(), MAX_INFLIGHT - 1);
}

/// 일시적으로 보내지 못한 결과는 다른 spawn·tell이 없어도 backoff 뒤 다시 보낸다. 상한에
/// 닿으면 보류한다.
#[test]
fn an_unsent_reference_is_retried_with_backoff_until_the_limit() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    let id = deliver_id(&fx, &rid, "k:1");
    for attempt in 1..=MAX_AUTO_ATTEMPTS {
        assert_eq!(fx.take_jobs().len(), 1, "attempt {attempt}");
        outcome(
            &fx,
            &id,
            "send",
            Outcome::NotSent {
                detail: "daemon busy".into(),
            },
        );
        make_due(&fx, &id);
        wake(&fx, PARENT);
    }
    assert!(fx.take_jobs().is_empty());
    let ev = event(&fx, &id);
    assert_eq!(ev.delivery.state, DeliveryState::Held);
    assert!(ev.delivery.detail.unwrap().contains("retry limit"));
    let delays: Vec<_> = fx.rec.wakes.borrow().iter().map(|w| w.1).collect();
    assert_eq!(delays[0], backoff(1));
    assert!(delays.iter().all(|d| *d <= MAX_BACKOFF));
    // 재시작은 보류한 결과를 다시 보낸다.
    let fx = fx.restarted();
    fx.with(recover);
    assert_eq!(fx.take_jobs().len(), 1);
}

/// 응답을 잃은 송신은 대기열을 한 번 확인할 뿐 다시 보내지 않는다.
#[test]
fn an_unknown_send_is_verified_once_and_never_resent() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    let id = deliver_id(&fx, &rid, "k:1");
    fx.take_jobs();
    outcome(
        &fx,
        &id,
        "send",
        Outcome::Unknown {
            detail: "lost".into(),
        },
    );
    let jobs = fx.take_jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, JobKind::Verify);
    outcome(
        &fx,
        &id,
        "verify",
        Outcome::StillUnknown {
            detail: "absent".into(),
        },
    );
    deliver_id(&fx, &rid, "k:2");
    let jobs = fx.take_jobs();
    assert!(jobs.iter().all(|j| j.event_id != id), "{jobs:?}");
    assert!(
        ledger::pending_ids(fx.root(), PARENT)
            .iter()
            .all(|p| p != &id)
    );
}

#[test]
fn a_wake_needs_this_process_token() {
    let fx = Fixture::new();
    assert!(
        fx.with(|d| handle_wake(d, &json!({ "thread": PARENT, "token": "x" })))
            .is_err()
    );
}

#[test]
fn reading_a_result_is_repeatable_and_marks_it_read() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = deliver(&fx, deliver_params(&rid, "k:1")).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let read = |fx: &Fixture| {
        fx.with(|d| handle_result(d, &json!({ "event": id, "_tasty_context": ctx_of(PARENT) })))
            .unwrap()
    };
    let first = read(&fx);
    let second = read(&fx);
    assert_eq!(first["body"], "result text");
    assert_eq!(second["body"], "result text", "읽어도 본문은 남는다");
    assert_eq!(second["read"]["count"], 2);
    assert_eq!(first["read"]["first_at_ms"], second["read"]["first_at_ms"]);
    assert_eq!(first["status"], "turn_ended");
    let missing = fx.with(|d| {
        handle_result(d, &json!({ "event": "0123456789abcdef0123456789abcdef", "_tasty_context": ctx_of(PARENT) }))
    });
    assert!(missing.is_err());
}

fn completed(body: &str) -> super::turns::TurnOutcome {
    super::turns::TurnOutcome::Final {
        status: "completed".into(),
        body: Some(body.into()),
    }
}

/// Codex 자식 surface의 턴 경계가 부모 수신자로 한 번씩 기록된다. 본문은 Stop 입력이 아니라 소유
/// 서버가 보고한 끝난 턴의 마지막 메시지이고, 종료는 그 턴 뒤에 기록된다.
#[test]
fn a_codex_child_turn_and_exit_are_returned_to_the_parent() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let hook = |params: Value| {
        fx.with(|d| crate::handlers::handle_hook(&OkHost, &params, &fx.tr, d))
            .unwrap()
    };
    hook(
        json!({"surface":31,"event":"prompt-submit","session":CHILD,"gen":generation,"turn_id":"turn-1"}),
    );
    for _ in 0..2 {
        hook(
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"turn-1",
            "last_assistant_message":"stop text"}),
        );
    }
    hook(json!({"surface":31,"event":"session-end","session":CHILD,"gen":generation}));
    // TUI가 끝나면 helper가 Exited를 기록한다.
    fx.exited(&generation);
    assert!(
        ledger::all_events(fx.root()).is_empty(),
        "훅은 결과를 확정하지 않는다"
    );
    let jobs = fx.rec.turn_jobs.borrow().clone();
    assert_eq!(jobs.len(), 1, "같은 턴의 Stop은 확인 작업 하나");
    assert_eq!(
        (jobs[0].thread.as_str(), jobs[0].turn_id.as_str()),
        (CHILD, "turn-1")
    );
    fx.answer_turns(|_| completed("final text"));
    let events = ledger::all_events(fx.root());
    assert_eq!(events.len(), 2, "{events:?}");
    let turn = events.iter().find(|e| e.status == "turn_ended").unwrap();
    assert_eq!(turn.completion_key, format!("codex:{CHILD}:turn-1"));
    assert_eq!(
        (turn.body.as_deref(), turn.child_session.as_deref()),
        (Some("final text"), Some(CHILD))
    );
    assert_eq!((turn.provider.as_str(), turn.child_surface), ("codex", 31));
    let exit = events.iter().find(|e| e.status == "exited").unwrap();
    assert_eq!(exit.body, None);
}

/// 다른 Stop 훅이 턴을 이어 가게 하면 같은 turn_id의 Stop이 다시 온다. 실제 Codex 0.154에서
/// Stop#1(FINAL-1, stop_hook_active false)이 block된 뒤 같은 턴에 FINAL-2가 나오고 Stop#2(true)
/// 다음에 turn/completed가 왔다. 첫 Stop의 본문을 기록하지 않고, 서버가 그 턴을 끝냈다고 보고한
/// 뒤 마지막 메시지 FINAL-2로 한 번 기록한다.
#[test]
fn a_blocked_stop_returns_the_final_message_of_the_turn() {
    use super::turns::outcome_of;
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let stop = |active: bool, msg: &str| {
        hook(
            &fx,
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"T",
                "stop_hook_active":active,"last_assistant_message":msg}),
        )
    };
    let msg = |t: &str| json!({ "type": "agentMessage", "id": t, "text": t });
    let user = json!({ "type": "userMessage", "id": "u", "content": [] });
    stop(false, "FINAL-1");
    let running = json!({ "id": "T", "status": "inProgress", "items": [user, msg("FINAL-1")] });
    assert_eq!(fx.answer_turns(|_| outcome_of(&running)), 1);
    assert!(ledger::all_events(fx.root()).is_empty());
    assert_eq!(fx.rec.turn_wakes.borrow().len(), 1, "다시 확인을 예약한다");
    stop(true, "FINAL-2");
    let done = json!({ "id": "T", "status": "completed",
        "items": [user, msg("FINAL-1"), msg("FINAL-2")] });
    fx.answer_turns(|_| outcome_of(&done));
    let events = ledger::all_events(fx.root());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        (events[0].completion_key.as_str(), events[0].body.as_deref()),
        (format!("codex:{CHILD}:T").as_str(), Some("FINAL-2"))
    );
    // 확정 뒤 늦게 온 같은 턴의 Stop은 아무것도 다시 만들지 않는다.
    stop(true, "late");
    fx.answer_turns(|_| outcome_of(&done));
    assert_eq!(ledger::all_events(fx.root()).len(), 1);
}

/// 확인하지 못한 후보가 많아도 한 번의 처리는 깨우기를 하나만 요청한다(실제 스레드 수는
/// [`super::turn_scheduler`] 시험이 잰다). 50개 후보와 반복된 훅이 후보 수만큼 요청하지 않는다.
/// 다른 세대의 보류 후보는 이 세대의 훅으로 다시 열리지 않는다.
#[test]
fn many_unconfirmed_turns_request_one_wake_per_pass() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let old = start_thread(&fx, 31, CHILD);
    hook(
        &fx,
        json!({"surface":31,"event":"stop","session":CHILD,"gen":old,"turn_id":"old"}),
    );
    let old_name = crate::ids::completion_key_file_name(&format!("codex:{CHILD}:old")).unwrap();
    fx.pump.turn_hold(&old_name);
    // 새 세대의 helper는 아직 준비되지 않아 모든 후보가 확인에 실패한다.
    let generation = start_thread(&fx, 31, CHILD);
    let stop = |turn: String| {
        hook(
            &fx,
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":turn}),
        )
    };
    for i in 0..50 {
        stop(format!("t{i}"));
    }
    for _ in 0..20 {
        let before = fx.rec.turn_wakes.borrow().len();
        stop("t0".into());
        assert!(fx.rec.turn_wakes.borrow().len() <= before + 1);
    }
    assert!(fx.rec.turn_jobs.borrow().is_empty());
    assert!(
        fx.pump.turn_exhausted(&old_name),
        "다른 세대의 보류는 그대로다"
    );
    let before = fx.rec.turn_wakes.borrow().len();
    fx.with(|d| super::turns::handle_turn_wake(d, &json!({ "token": TOKEN, "seq": 1 })))
        .unwrap();
    assert!(fx.rec.turn_wakes.borrow().len() <= before + 1);
}

/// 일반 셸의 tell(Codex 대화가 아닌 호출자)
fn legacy_tell(fx: &Fixture, surface: u32) {
    fx.with(|d| {
        crate::handlers::handle_tell(
            &OkHost,
            &json!({ "surface": surface, "message": "other work" }),
            &fx.tr,
            d,
        )
    })
    .unwrap();
}

/// D 재현: 확인 중인 턴(inProgress)의 자식에게 일반 지시가 가면, 그 턴이 나중에 끝나도 옛
/// 수신자에게 기록하지 않는다. 같은 턴의 Stop이 다시 와도, 예약된 깨우기가 와도 그렇다.
/// 일반 지시 전에 이미 기록한 결과는 남는다.
#[test]
fn a_legacy_instruction_voids_unconfirmed_turns_but_keeps_captured_results() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let stop = |turn: &str, active: bool, msg: &str| {
        hook(
            &fx,
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":turn,
                "stop_hook_active":active,"last_assistant_message":msg}),
        )
    };
    // 이미 기록된 턴 T0.
    stop("T0", false, "done");
    fx.answer_turns(|_| completed("T0 final"));
    assert_eq!(keys(&fx), vec![format!("codex:{CHILD}:T0")]);
    // T1은 확인 중(inProgress)이다.
    stop("T1", false, "FINAL-1");
    fx.answer_turns(|_| super::turns::TurnOutcome::InProgress);
    legacy_tell(&fx, 31);
    assert_eq!(ledger::child_receiver(fx.root(), 31).unwrap(), None);
    assert_eq!(
        (candidate_count(&fx), void_count(&fx)),
        (0, 1),
        "무효화한 후보는 void에만 남는다"
    );
    stop("T1", true, "FINAL-2");
    assert!(fx.rec.turn_jobs.borrow().is_empty());
    fx.with(|d| super::turns::handle_turn_wake(d, &json!({ "token": TOKEN, "seq": 1 })))
        .unwrap();
    fx.answer_turns(|_| completed("FINAL-2"));
    assert_eq!(
        keys(&fx),
        vec![format!("codex:{CHILD}:T0")],
        "T1은 옛 수신자에게 가지 않는다"
    );
}

/// 확인이 진행 중인 사이에 일반 지시가 오면 늦게 온 확인 결과는 반영되지 않는다. 그 뒤 Codex
/// 부모가 다시 경로를 묶고 같은 턴 키로 새 후보가 생겨도, 옛 확인 결과는 새 후보를 끝내거나
/// 지우지 않는다. 새 후보는 자기 확인으로 새 수신자에게 기록된다. 종료 후보도 무효화된다.
#[test]
fn a_late_check_result_neither_revives_nor_settles_a_rebound_candidate() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let r1 = rid_of(&fx, PARENT);
    let r2 = rid_of(&fx, OTHER);
    ledger::set_child_receiver(fx.root(), 31, &r1, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let stop = || {
        hook(
            &fx,
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"T1"}),
        )
    };
    stop();
    let old_job = fx.rec.turn_jobs.borrow_mut().remove(0);
    legacy_tell(&fx, 31);
    // 다시 묶는다(다른 Codex 부모의 tell과 같다).
    ledger::set_child_receiver(fx.root(), 31, &r2, None).unwrap();
    stop();
    let new_job = fx.rec.turn_jobs.borrow_mut().remove(0);
    assert_eq!(old_job.name, new_job.name, "같은 턴 키");
    assert_ne!(old_job.candidate_id, new_job.candidate_id);
    let answer = |job: &super::turns::TurnJob, body: &str| {
        fx.with(|d| {
            super::turns::handle_turn_outcome(
                d,
                &json!({ "name": job.name, "candidate": job.candidate_id, "token": TOKEN,
                    "result": completed(body) }),
            )
        })
        .unwrap()
    };
    assert_eq!(answer(&old_job, "late")["applied"], false);
    assert!(ledger::all_events(fx.root()).is_empty());
    assert_eq!(candidate_count(&fx), 1, "새 후보는 그대로다");
    assert_eq!(answer(&new_job, "new")["applied"], "captured");
    let events = ledger::all_events(fx.root());
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].receiver_id.as_str(), events[0].body.as_deref()),
        (r2.as_str(), Some("new"))
    );

    // 종료 후보도 일반 지시가 무효화한다.
    hook(
        &fx,
        json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"T2"}),
    );
    hook(
        &fx,
        json!({"surface":31,"event":"session-end","session":CHILD,"gen":generation}),
    );
    legacy_tell(&fx, 31);
    fx.answer_turns(|_| completed("after"));
    assert_eq!(ledger::all_events(fx.root()).len(), 1);
}

/// 확인 대상인 후보 파일 수(무효화한 `void/`는 세지 않는다).
/// 무효화와 재bind가 있어도 실제로 실행 중인 확인은 끝날 때까지 자기 슬롯을 차지한다. 옛 확인의
/// 늦은 결과는 자기 슬롯만 비우고 새 후보의 확인 슬롯을 빼지 않는다. 동시 확인은 상한을 넘지
/// 않는다.
#[test]
fn an_old_check_keeps_its_slot_until_it_returns() {
    use super::turns::MAX_TURN_INFLIGHT;
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let r1 = rid_of(&fx, PARENT);
    let r2 = rid_of(&fx, OTHER);
    ledger::set_child_receiver(fx.root(), 31, &r1, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let stop = |turn: &str| {
        hook(
            &fx,
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":turn}),
        )
    };
    let take = || fx.rec.turn_jobs.borrow_mut().drain(..).collect::<Vec<_>>();
    stop("T1");
    let old = take().remove(0);
    // 무효화와 재bind를 거듭해도 실제 확인은 상한을 넘지 않는다.
    let mut outstanding = vec![old.clone()];
    for _ in 0..5 {
        legacy_tell(&fx, 31);
        ledger::set_child_receiver(fx.root(), 31, &r2, None).unwrap();
        stop("T1");
        outstanding.extend(take());
    }
    assert!(
        outstanding.len() <= MAX_TURN_INFLIGHT,
        "실행 중인 확인 {}개",
        outstanding.len()
    );
    assert_eq!(outstanding.len(), 2, "옛 확인 1 + 첫 재bind 후보 확인 1");
    let first = outstanding[1].clone();
    assert_eq!(old.name, first.name);
    // 슬롯이 가득 찼으므로 다른 턴은 기다린다.
    stop("T9");
    assert!(take().is_empty());
    let answer = |job: &super::turns::TurnJob, body: &str| {
        fx.with(|d| {
            super::turns::handle_turn_outcome(
                d,
                &json!({ "name": job.name, "candidate": job.candidate_id, "token": TOKEN,
                    "result": completed(body) }),
            )
        })
        .unwrap()
    };
    // 두 확인 모두 이후 무효화된 후보의 것이다. 반영되지 않고 각자 자기 슬롯만 비운다. 비운
    // 슬롯마다 기다리던 후보(현재 T1, T9) 하나가 확인을 시작한다.
    assert_eq!(answer(&old, "late")["applied"], false);
    let mut started = take();
    assert_eq!(started.len(), 1, "빈 슬롯 하나만큼만 시작한다");
    stop("T10");
    assert!(take().is_empty(), "슬롯 둘이 찼다");
    assert_eq!(answer(&first, "stale")["applied"], false);
    started.extend(take());
    assert_eq!(started.len(), 2);
    let mut turns: Vec<_> = started.iter().map(|j| j.turn_id.clone()).collect();
    turns.sort();
    assert_eq!(turns, vec!["T1".to_string(), "T9".to_string()]);
    assert!(ledger::all_events(fx.root()).is_empty());
    for job in &started {
        assert_eq!(answer(job, &job.turn_id)["applied"], "captured");
    }
    assert_eq!(take().len(), 1, "T10이 이어서 확인된다");
    let events = ledger::all_events(fx.root());
    assert_eq!(events.len(), 2);
    assert!(
        events.iter().all(|e| e.receiver_id == r2),
        "새 수신자에게만"
    );
}

/// G-1: 옛 확인의 늦은 결과는 같은 키로 다시 만든 후보의 확인 표시를 지우지 않는다. 새 후보의
/// 확인이 도는 동안 같은 턴 Stop이 다시 와도 확인이 겹치지 않는다.
#[test]
fn an_old_result_does_not_duplicate_the_new_candidates_check() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let r1 = rid_of(&fx, PARENT);
    let r2 = rid_of(&fx, OTHER);
    ledger::set_child_receiver(fx.root(), 31, &r1, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let stop = || {
        hook(
            &fx,
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"T1"}),
        )
    };
    let take = || fx.rec.turn_jobs.borrow_mut().drain(..).collect::<Vec<_>>();
    stop();
    let old = take().remove(0);
    legacy_tell(&fx, 31);
    ledger::set_child_receiver(fx.root(), 31, &r2, None).unwrap();
    stop();
    let new = take().remove(0);
    stop();
    assert!(take().is_empty());
    let answer = |job: &super::turns::TurnJob| {
        fx.with(|d| {
            super::turns::handle_turn_outcome(
                d,
                &json!({ "name": job.name, "candidate": job.candidate_id, "token": TOKEN,
                    "result": completed("body") }),
            )
        })
        .unwrap()
    };
    assert_eq!(answer(&old)["applied"], false);
    stop();
    assert!(take().is_empty(), "새 후보의 확인은 아직 돈다");
    assert_eq!(answer(&new)["applied"], "captured");
    assert_eq!(ledger::all_events(fx.root()).len(), 1);
}

/// G-3: 해석할 수 없는 후보 파일(손상 JSON, `id` 없는 이전 형식)이 다른 자식에 대한 일반 지시를
/// 막지 않는다. 그 파일은 `turns/corrupt/`로 격리되고 확인 대상에서 빠진다.
#[test]
fn an_unreadable_candidate_is_isolated_and_blocks_no_instruction() {
    let fx = Fixture::new();
    let turns = fx.root().join("turns");
    std::fs::create_dir_all(&turns).unwrap();
    let bad = ["a".repeat(64), "b".repeat(64)];
    std::fs::write(turns.join(format!("{}.json", bad[0])), "{not json").unwrap();
    std::fs::write(
        turns.join(format!("{}.json", bad[1])),
        json!({"completion_key":"k","receiver_id":"r","child_surface":99,"thread":"t",
            "generation":"g","turn_id":"x","attempt_id":null,"created_at_ms":1})
        .to_string(),
    )
    .unwrap();
    legacy_tell(&fx, 32);
    assert_eq!(candidate_count(&fx), 0);
    let corrupt = std::fs::read_dir(turns.join("corrupt")).unwrap().count();
    assert_eq!(corrupt, 2);
}

/// 세션 종료 후보의 생성 시각을 유예만큼 앞당긴다.
fn age_exit_candidates(fx: &Fixture) {
    let turns = fx.root().join("turns");
    for e in std::fs::read_dir(&turns).unwrap().filter_map(Result::ok) {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let mut c: super::turns::Candidate =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if c.turn_id.is_none() {
            c.created_at_ms -= super::turns::EXIT_GRACE.as_millis() as u64 + 1;
            std::fs::write(&path, serde_json::to_string(&c).unwrap()).unwrap();
        }
    }
}

fn wake_turns(fx: &Fixture) {
    fx.with(|d| super::turns::handle_turn_wake(d, &json!({ "token": TOKEN, "seq": 1 })))
        .unwrap();
}

/// Codex 0.154의 SessionEnd는 대화를 바꾼 뒤 구독자가 없는 이전 thread를 서버가 unload할 때도
/// 온다. 세대가 계속 실행 중이면 종료로 기록하지 않는다. 세대의 thread·경로·메타데이터는
/// 비우고, 다음 대화의 SessionStart(첫 턴 직전)는 비운 세대에 바로 묶이며 그 턴이 부모에게
/// 돌아간다. 이미 내려간 thread의 늦은 Stop은 결과를 만들지 않는다.
#[test]
fn an_unloaded_conversation_is_not_reported_as_an_exit() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let h = |ev: &str, thread: &str, turn: Option<&str>| {
        let mut p = json!({"surface":31,"event":ev,"session":thread,"gen":generation});
        if let Some(t) = turn {
            p["turn_id"] = json!(t);
        }
        hook(&fx, p)
    };
    // /new 뒤 60초가 지나 이전 thread가 unload된다. 서버는 그대로 실행 중이다.
    fx.set_live(&[]);
    assert!(h("session-end", CHILD, None).get("ignored").is_none());
    let session = crate::session::registry::current_session(fx.root(), 31, &generation)
        .unwrap()
        .unwrap();
    assert_eq!(session.thread, None, "세대의 thread를 비운다");
    assert!(
        fx.rec
            .turn_wakes
            .borrow()
            .iter()
            .all(|at| *at <= crate::store::now_ms() + super::turns::EXIT_POLL.as_millis() as u64),
        "세대 종료를 짧은 간격으로 다시 확인한다"
    );
    wake_turns(&fx);
    assert!(ledger::all_events(fx.root()).is_empty());
    age_exit_candidates(&fx);
    wake_turns(&fx);
    assert!(
        ledger::all_events(fx.root()).is_empty(),
        "종료로 기록하지 않는다"
    );
    assert_eq!((candidate_count(&fx), void_count(&fx)), (0, 1));
    // 내려간 thread의 늦은 Stop은 결과를 만들지 않는다.
    assert_eq!(
        h("stop", CHILD, Some("late"))["child_return_rejected"],
        "unknown_thread"
    );
    // 다음 대화의 SessionStart는 서버 확인 없이 비운 세대에 묶인다.
    *fx.live.borrow_mut() = None;
    assert!(h("session-start", OTHER, None).get("ignored").is_none());
    let session = crate::session::registry::current_session(fx.root(), 31, &generation)
        .unwrap()
        .unwrap();
    assert_eq!(session.thread.as_deref(), Some(OTHER));
    h("stop", OTHER, Some("n1"));
    fx.answer_turns(|_| completed("new conversation"));
    let events = ledger::all_events(fx.root());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_key, format!("codex:{OTHER}:n1"));
    assert_eq!(events[0].receiver_id, rid);
}

/// helper가 Exited를 기록하지 못하고 끝났으면(서버 연결 불가) 유예 뒤 종료로 기록한다.
#[test]
fn an_unreachable_session_is_reported_as_an_exit_after_the_grace() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    hook(
        &fx,
        json!({"surface":31,"event":"session-end","session":CHILD,"gen":generation}),
    );
    *fx.live.borrow_mut() = None;
    age_exit_candidates(&fx);
    wake_turns(&fx);
    let events = ledger::all_events(fx.root());
    assert_eq!(
        events.iter().map(|e| e.status.as_str()).collect::<Vec<_>>(),
        vec!["exited"]
    );
}

/// 새 대화가 먼저 묶인 뒤(첫 턴이 unload보다 먼저) 이전 thread의 SessionEnd가 와도 새 thread와
/// 메타데이터는 그대로이고 종료 후보도 생기지 않는다.
#[test]
fn a_late_unload_of_the_previous_thread_keeps_the_new_binding() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    fx.set_live(&[OTHER]);
    let start = hook(
        &fx,
        json!({"surface":31,"event":"session-start","session":OTHER,"gen":generation}),
    );
    assert!(start.get("ignored").is_none());
    let end = hook(
        &fx,
        json!({"surface":31,"event":"session-end","session":CHILD,"gen":generation}),
    );
    assert_eq!(end["ignored"], "other_thread");
    let session = crate::session::registry::current_session(fx.root(), 31, &generation)
        .unwrap()
        .unwrap();
    assert_eq!(session.thread.as_deref(), Some(OTHER));
    assert_eq!(candidate_count(&fx), 0);
}

/// thread가 빈 세대에서 SessionStart 없이 온 prompt(재개 등)는 소유 서버가 그 thread를 실행
/// 중으로 보일 때만 묶인다. 확인하지 못하면 묶지 않고 결과도 만들지 않는다.
#[test]
fn an_empty_session_adopts_a_running_thread_from_a_prompt() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    fx.set_live(&[]);
    hook(
        &fx,
        json!({"surface":31,"event":"session-end","session":CHILD,"gen":generation}),
    );
    let h = |ev: &str| {
        hook(
            &fx,
            json!({"surface":31,"event":ev,"session":OTHER,"gen":generation,"turn_id":"r1"}),
        )
    };
    // 실행 중으로 보이지 않으면 묶지 않는다.
    h("prompt-submit");
    assert_eq!(h("stop")["child_return_rejected"], "unknown_thread");
    // 실행 중이면 묶이고 그 턴이 돌아간다.
    fx.set_live(&[OTHER]);
    assert!(h("prompt-submit").get("ignored").is_none());
    let session = crate::session::registry::current_session(fx.root(), 31, &generation)
        .unwrap()
        .unwrap();
    assert_eq!(session.thread.as_deref(), Some(OTHER));
    h("stop");
    fx.answer_turns(|_| completed("resumed"));
    let events = ledger::all_events(fx.root());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_key, format!("codex:{OTHER}:r1"));
}

/// D 재현(N-1): A가 unload되어 SessionEnd가 오고, 유예 안에 B가 묶인 뒤 TUI가 끝나면 종료는 B의
/// 것 한 번뿐이다. B는 SessionStart로 묶이든(`by_prompt` 거짓), SessionStart 없이 확인된 prompt로
/// 묶이든 같다. A의 미확정 턴 후보는 남아 그대로 확인된다.
fn one_exit_after_an_unload_and_a_new_binding(by_prompt: bool) {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    let h = |ev: &str, thread: &str, turn: Option<&str>| {
        let mut p = json!({"surface":31,"event":ev,"session":thread,"gen":generation});
        if let Some(t) = turn {
            p["turn_id"] = json!(t);
        }
        hook(&fx, p)
    };
    // A의 턴은 아직 확인 중이다.
    fx.set_live(&[]);
    h("stop", CHILD, Some("a1"));
    let a_job = fx.rec.turn_jobs.borrow_mut().remove(0);
    h("session-end", CHILD, None);
    // 유예 안에 B가 묶인다.
    if by_prompt {
        fx.set_live(&[OTHER]);
        h("prompt-submit", OTHER, Some("b1"));
    } else {
        h("session-start", OTHER, None);
    }
    assert_eq!(
        crate::session::registry::current_session(fx.root(), 31, &generation)
            .unwrap()
            .unwrap()
            .thread
            .as_deref(),
        Some(OTHER)
    );
    assert_eq!(void_count(&fx), 1, "A의 종료 후보만 옮긴다");
    assert_eq!(candidate_count(&fx), 1, "A의 턴 후보는 남는다");
    // TUI가 끝난다.
    h("session-end", OTHER, None);
    fx.exited(&generation);
    wake_turns(&fx);
    fx.with(|d| {
        super::turns::handle_turn_outcome(
            d,
            &json!({ "name": a_job.name, "candidate": a_job.candidate_id, "token": TOKEN,
                "result": completed("a final") }),
        )
    })
    .unwrap();
    let mut got: Vec<(String, String)> = ledger::all_events(fx.root())
        .into_iter()
        .map(|e| (e.completion_key, e.status))
        .collect();
    got.sort();
    let mut want = vec![
        (format!("codex:{CHILD}:a1"), "turn_ended".to_string()),
        (
            format!("codex:{OTHER}:end.{generation}"),
            "exited".to_string(),
        ),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn an_exit_is_reported_once_after_an_unload_and_a_session_start() {
    one_exit_after_an_unload_and_a_new_binding(false);
}

#[test]
fn an_exit_is_reported_once_after_an_unload_and_an_adopting_prompt() {
    one_exit_after_an_unload_and_a_new_binding(true);
}

fn candidate_count(fx: &Fixture) -> usize {
    std::fs::read_dir(fx.root().join("turns"))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
        .unwrap_or(0)
}

fn void_count(fx: &Fixture) -> usize {
    std::fs::read_dir(fx.root().join("turns").join("void"))
        .map(|d| d.count())
        .unwrap_or(0)
}

/// 세션이 끝나 서버가 내려가면 확인하지 못한 턴은 결과로 추정하지 않고 보류하며, 종료 알림은
/// 기다리지 않고 기록한다. 후보 파일은 남는다.
#[test]
fn an_exit_after_the_server_stopped_is_not_held_by_unconfirmed_turns() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    hook(
        &fx,
        json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"t1"}),
    );
    fx.answer_turns(|_| super::turns::TurnOutcome::InProgress);
    fx.exited(&generation);
    hook(
        &fx,
        json!({"surface":31,"event":"session-end","session":CHILD,"gen":generation}),
    );
    let events = ledger::all_events(fx.root());
    assert_eq!(
        events.iter().map(|e| e.status.as_str()).collect::<Vec<_>>(),
        vec!["exited"]
    );
    // 서버가 끝났으므로 다시 묻지 않는다.
    assert!(fx.rec.turn_jobs.borrow().is_empty());
    assert_eq!(candidate_count(&fx), 1, "확인하지 못한 턴 후보는 남는다");
    let name = crate::ids::completion_key_file_name(&format!("codex:{CHILD}:t1")).unwrap();
    assert!(fx.pump.turn_exhausted(&name));
}

/// 서버에 닿지 못하는 동안 종료는 유예까지만 턴 확인을 기다린다. 유예가 지나면 종료를 기록하고
/// 턴은 추정하지 않는다. 유예 시각에 깨우기가 예약된다.
#[test]
fn an_exit_waits_for_turn_confirmation_only_for_the_grace_period() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    hook(
        &fx,
        json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"t1"}),
    );
    fx.answer_turns(|_| super::turns::TurnOutcome::Unreachable {
        detail: "refused".into(),
    });
    hook(
        &fx,
        json!({"surface":31,"event":"session-end","session":CHILD,"gen":generation}),
    );
    fx.answer_turns(|_| super::turns::TurnOutcome::Unreachable {
        detail: "refused".into(),
    });
    assert!(
        ledger::all_events(fx.root()).is_empty(),
        "유예 안에서는 기다린다"
    );
    assert!(
        fx.rec
            .turn_wakes
            .borrow()
            .iter()
            .all(|at| *at <= crate::store::now_ms() + super::turns::EXIT_GRACE.as_millis() as u64),
        "{:?}",
        fx.rec.turn_wakes.borrow()
    );
    // 유예가 지난 것으로 만든다(종료 후보의 생성 시각을 앞당긴다).
    let exit =
        crate::ids::completion_key_file_name(&format!("codex:{CHILD}:end.{generation}")).unwrap();
    let path = fx.root().join("turns").join(format!("{exit}.json"));
    let mut c: super::turns::Candidate =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    c.created_at_ms -= super::turns::EXIT_GRACE.as_millis() as u64 + 1;
    std::fs::write(&path, serde_json::to_string(&c).unwrap()).unwrap();
    fx.with(|d| super::turns::handle_turn_wake(d, &json!({ "token": TOKEN })))
        .unwrap();
    let events = ledger::all_events(fx.root());
    assert_eq!(
        events.iter().map(|e| e.status.as_str()).collect::<Vec<_>>(),
        vec!["exited"]
    );
    assert_eq!(candidate_count(&fx), 1, "확인하지 못한 턴 후보는 남는다");
}

/// 서버의 턴 상태가 반환 상태와 본문을 정한다.
#[test]
fn the_server_turn_state_decides_status_and_body() {
    use super::turns::{TurnOutcome, outcome_of};
    let msg = |t: &str| json!({ "type": "agentMessage", "id": "m", "text": t });
    assert_eq!(
        outcome_of(&json!({ "status": "inProgress", "items": [msg("x")] })),
        TurnOutcome::InProgress
    );
    assert_eq!(
        outcome_of(&json!({ "status": "completed", "items": [msg("a"),
            { "type": "userMessage", "id": "u", "content": [] }, msg("b"), msg("")] })),
        completed("b")
    );
    assert_eq!(
        outcome_of(&json!({ "status": "failed", "items": [], "error": { "message": "boom" } })),
        TurnOutcome::Final {
            status: "failed".into(),
            body: Some("boom".into())
        }
    );
    assert_eq!(
        outcome_of(&json!({ "status": "interrupted", "items": [] })),
        TurnOutcome::Final {
            status: "interrupted".into(),
            body: None
        }
    );
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    for (turn, ev) in [("t1", "interrupt"), ("t2", "stop")] {
        hook(
            &fx,
            json!({"surface":31,"event":ev,"session":CHILD,"gen":generation,"turn_id":turn}),
        );
    }
    fx.answer_turns(|job| match job.turn_id.as_str() {
        "t1" => outcome_of(&json!({ "status": "interrupted", "items": [] })),
        _ => {
            outcome_of(&json!({ "status": "failed", "items": [], "error": { "message": "boom" } }))
        }
    });
    let mut got: Vec<(String, String)> = ledger::all_events(fx.root())
        .into_iter()
        .map(|e| (e.completion_key, e.status))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (format!("codex:{CHILD}:t1"), "interrupted".into()),
            (format!("codex:{CHILD}:t2"), "turn_failed".into())
        ]
    );
}

/// 소유 서버에 닿지 못하면 확인을 상한까지만 예약하고 보류한다. 같은 세대의 새 훅이 다시 확인하게
/// 한다. 동시에 확인하는 후보 수에는 상한이 있다.
#[test]
fn turn_checks_are_bounded_and_resume_on_a_new_hook() {
    use super::turns::{MAX_TURN_CHECKS, MAX_TURN_INFLIGHT, TurnOutcome};
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    let stop = |turn: &str| {
        hook(
            &fx,
            json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":turn}),
        )
    };
    // helper가 준비되지 않았다: 서버에 묻지 않고 재확인만 예약한다.
    stop("t1");
    assert!(fx.rec.turn_jobs.borrow().is_empty());
    assert_eq!(fx.rec.turn_wakes.borrow().len(), 1);
    let name = crate::ids::completion_key_file_name(&format!("codex:{CHILD}:t1")).unwrap();
    // 예약 시각을 기다리지 않도록 매번 지금으로 당긴다.
    for _ in 0..MAX_TURN_CHECKS * 2 {
        fx.pump.turn_schedule(&name, 0);
        fx.with(|d| super::turns::handle_turn_wake(d, &json!({ "token": TOKEN })))
            .unwrap();
    }
    assert_eq!(
        fx.rec.turn_wakes.borrow().len(),
        MAX_TURN_CHECKS as usize - 1,
        "상한에 닿은 뒤에는 예약하지 않는다"
    );
    assert!(fx.pump.turn_exhausted(&name));
    assert!(ledger::all_events(fx.root()).is_empty());

    fx.ready(&generation, "unix:///owned/c.sock");
    for turn in ["t1", "t2", "t3"] {
        stop(turn);
    }
    assert_eq!(fx.rec.turn_jobs.borrow().len(), MAX_TURN_INFLIGHT);
    fx.answer_turns(|_| completed("ok"));
    assert_eq!(ledger::all_events(fx.root()).len(), 3);

    // 다른 프로세스의 결과는 받지 않는다.
    let err = fx
        .with(|d| {
            super::turns::handle_turn_outcome(
                d,
                &json!({ "name": "0".repeat(64), "token": "x", "result": TurnOutcome::InProgress }),
            )
        })
        .unwrap_err();
    assert!(err.message.contains("internal"), "{}", err.message);
}

/// 원장이 만든 작업을 시험 서버로 실제 송신하고 결과를 원장에 반영한다.
#[test]
fn a_reference_reaches_the_queue_of_a_fake_app_server() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = parent(&fx, PARENT).unwrap()["receiver_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = deliver(&fx, deliver_params(&rid, "k:1")).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let server = fake::start(fake::Rules {
        loaded: vec![PARENT.into()],
        queue_add: fake::QueueAdd::Accept,
        queued: vec![],
    });
    let mut job = fx.take_jobs().remove(0);
    job.endpoint = crate::rpc::Endpoint::Ws {
        port: server.port,
        token: "t".into(),
    };
    let result = send::run(&job);
    assert_eq!(
        outcome(&fx, &id, "send", result)["delivery_state"],
        "accepted"
    );
    assert_eq!(
        server.methods(),
        [
            "initialize",
            "initialized",
            "thread/loaded/list",
            "thread/queue/add"
        ]
        .map(String::from)
    );
}

fn hook(fx: &Fixture, params: Value) -> Value {
    fx.with(|d| crate::handlers::handle_hook(&OkHost, &params, &fx.tr, d))
        .unwrap()
}

fn keys(fx: &Fixture) -> Vec<String> {
    ledger::all_events(fx.root())
        .into_iter()
        .map(|e| e.completion_key)
        .collect()
}

/// 턴은 생산단의 turn_id로만 식별한다. 같은 turn_id의 Stop은 한 번, reboot 뒤의 새 턴은 새 결과다.
#[test]
fn turns_are_identified_by_the_producer_turn_id_across_reboots() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    for turn in ["turn-a", "turn-b"] {
        let generation = start_thread(&fx, 31, CHILD);
        fx.ready(&generation, "unix:///owned/c.sock");
        let p = |ev: &str| {
            json!({"surface":31,"event":ev,"session":CHILD,"gen":generation,"turn_id":turn,
            "last_assistant_message":"same answer"})
        };
        hook(&fx, p("prompt-submit"));
        hook(&fx, p("stop"));
        hook(&fx, p("stop"));
    }
    fx.answer_turns(|_| completed("same answer"));
    let mut k = keys(&fx);
    k.sort();
    assert_eq!(
        k,
        vec![
            format!("codex:{CHILD}:turn-a"),
            format!("codex:{CHILD}:turn-b")
        ],
        "같은 응답이어도 턴 ID가 다르면 다른 결과다"
    );
}

/// 턴 ID 없는 Stop·Interrupt(지원 버전에서는 생기지 않는다)는 지원하지 않는 입력으로 거절한다.
/// 다른 턴의 중복으로 추정하거나 새 키로 만들지 않는다.
#[test]
fn a_boundary_without_a_turn_id_is_rejected_not_guessed() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    for (ev, turn) in [
        ("stop", json!(null)),
        ("interrupt", json!(null)),
        ("stop", json!("a/b")),
    ] {
        let out = hook(
            &fx,
            json!({"surface":31,"event":ev,"session":CHILD,"gen":generation,"turn_id":turn,
            "last_assistant_message":"same answer"}),
        );
        assert_eq!(
            out["child_return_rejected"], "missing_turn_id",
            "{ev} {turn}"
        );
        assert_eq!(out["host_call_failures"], 1);
    }
    assert!(fx.rec.turn_jobs.borrow().is_empty());
    assert!(keys(&fx).is_empty());
}

/// 다른 thread의 훅은 세대의 소유 서버가 그 thread를 지금 실행 중이고 이전 thread는 아니라고
/// 확인할 때만 경로를 옮긴다. SessionStart 없이 이미 로드된 대화로 돌아가도 따라간다.
#[test]
fn a_thread_switch_follows_only_what_the_owned_server_shows_running() {
    let fx = Fixture::new();
    let generation = start_thread(&fx, 5, PARENT);
    let h = |ev: &str, thread: &str| {
        hook(
            &fx,
            json!({"surface":5,"event":ev,"session":thread,"gen":generation,"turn_id":"t"}),
        )
    };
    // helper가 준비되지 않았으면 확인할 수 없어 옮기지 않는다.
    fx.set_live(&[OTHER]);
    assert_eq!(h("session-start", OTHER)["ignored"], "unverified_session");
    fx.ready(&generation, "unix:///owned/s.sock");
    *fx.live.borrow_mut() = None;
    assert_eq!(h("session-start", OTHER)["ignored"], "unverified_session");
    assert!(parent(&fx, OTHER).is_err());

    // /new: 새 thread가 active, 이전 thread는 idle.
    fx.set_live(&[OTHER]);
    assert!(h("session-start", OTHER).get("ignored").is_none());
    // 공유 daemon 로드 확인이 거짓인 fixture에서 받을 수 있으면 소유 세션에 바인딩된 것이다.
    assert_eq!(
        parent(&fx, OTHER).unwrap()["terminal_parent"],
        json!({ "parent_external": format!("codex:{OTHER}") })
    );
    assert!(parent(&fx, PARENT).is_err());

    // 늦게 도착한 이전 대화의 훅(SessionStart 포함, source와 무관)은 되돌리지 못한다.
    for ev in ["session-start", "prompt-submit", "stop"] {
        assert_eq!(h(ev, PARENT)["ignored"], "stale_session", "{ev}");
    }
    // 두 thread가 모두 active로 보이면 정할 수 없어 옮기지 않는다.
    fx.set_live(&[OTHER, PARENT]);
    assert_eq!(h("session-start", PARENT)["ignored"], "stale_session");
    // 이전 thread의 SessionEnd는 세대를 끝내지 않는다.
    fx.set_live(&[PARENT]);
    assert_eq!(h("session-end", PARENT)["ignored"], "other_thread");

    // SessionStart 없이 로드된 A로 돌아가 prompt를 보내면 따라간다.
    assert!(h("prompt-submit", PARENT).get("ignored").is_none());
    assert_eq!(
        parent(&fx, PARENT).unwrap()["terminal_parent"],
        json!({ "parent_external": format!("codex:{PARENT}") })
    );
    assert!(parent(&fx, OTHER).is_err());
    // 이전 세대의 훅은 계속 거절한다.
    fx.open(5, &[]);
    assert_eq!(h("session-start", OTHER)["ignored"], "stale_generation");
}

/// 자식이 같은 TUI에서 이미 로드된 대화로 돌아가도 그 뒤 턴의 결과가 부모에게 돌아간다.
#[test]
fn a_child_returning_to_a_loaded_conversation_still_reports() {
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    let rid = rid_of(&fx, PARENT);
    ledger::set_child_receiver(fx.root(), 31, &rid, None).unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    fx.set_live(&[OTHER]);
    let p = |ev: &str| {
        json!({"surface":31,"event":ev,"session":OTHER,"gen":generation,"turn_id":"t2",
        "last_assistant_message":"from other"})
    };
    hook(&fx, p("prompt-submit"));
    hook(&fx, p("stop"));
    fx.answer_turns(|job| {
        assert_eq!(job.thread, OTHER);
        completed("from other")
    });
    let events = ledger::all_events(fx.root());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].completion_key, format!("codex:{OTHER}:t2"));
    assert_eq!(events[0].child_session.as_deref(), Some(OTHER));
}

/// 원장이 만든 송신·확인 작업을 실제 `codex app-server`로 실행한다. 모델은 호출하지 않으며
/// 인증이 필요 없다. 격리된 CODEX_HOME과 이 시험이 만든 unix 소켓만 쓰고, 서버는 별도 프로세스
/// 그룹으로 띄워 래퍼와 함께 정리한다.
#[cfg(unix)]
#[test]
#[ignore = "실제 codex CLI가 필요하다"]
fn the_real_app_server_receives_sends_and_verifies_from_the_ledger() {
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    use crate::rpc::{Client, Endpoint, queue_add};

    let home = tempfile::tempdir().unwrap();
    let sock_dir = tempfile::Builder::new()
        .prefix("tc")
        .tempdir_in("/tmp")
        .unwrap();
    let url = format!("unix://{}/s.sock", sock_dir.path().display());
    let log = std::fs::File::create(home.path().join("server.log")).unwrap();
    let mut server = std::process::Command::new("codex")
        .args(["app-server", "--listen", &url])
        .env("CODEX_HOME", home.path())
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .process_group(0)
        .spawn()
        .unwrap();
    let endpoint = Endpoint::Unix(sock_dir.path().join("s.sock"));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        if let Ok(c) = Client::connect(&endpoint, Duration::from_secs(5)) {
            break c;
        }
        assert!(Instant::now() < deadline, "server not ready");
        std::thread::sleep(Duration::from_millis(200));
    };
    let started = client
        .call("thread/start", json!({ "cwd": home.path() }))
        .unwrap();
    let thread = started["thread"]["id"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    // 턴이 없는 thread는 active가 아니다. 훅 실행 중 active는 소유 서버 probe로 측정했다.
    assert_eq!(owned_thread_active(&endpoint, &thread), Ok(false));

    let fx = Fixture::new();
    let generation = start_thread(&fx, 5, &thread);
    fx.ready(&generation, &url);
    let rid = rid_of(&fx, &thread);

    // 보낸다.
    let sent = deliver_id(&fx, &rid, "k:1");
    let job = fx.take_jobs().remove(0);
    assert_eq!(job.endpoint, endpoint);
    let result = send::run(&job);
    assert_eq!(
        outcome(&fx, &sent, "send", result)["delivery_state"],
        "accepted"
    );

    // 응답을 잃은 송신은 다시 보내지 않고 대기열만 확인한다. 실제로 가지 않았으면 unknown으로 남는다.
    let lost = deliver_id(&fx, &rid, "k:2");
    let job = fx.take_jobs().remove(0);
    outcome(
        &fx,
        &lost,
        "send",
        Outcome::Unknown {
            detail: "lost".into(),
        },
    );
    let verify = fx.take_jobs().remove(0);
    assert_eq!(verify.kind, JobKind::Verify);
    let result = send::run(&verify);
    assert!(matches!(result, Outcome::StillUnknown { .. }), "{result:?}");
    assert_eq!(
        outcome(&fx, &lost, "verify", result)["delivery_state"],
        "unknown"
    );

    // 실제로 들어간 송신은 확인으로 accepted가 된다.
    let lost2 = deliver_id(&fx, &rid, "k:3");
    let job3 = fx.take_jobs().remove(0);
    queue_add(&mut client, &thread, &job3.text, &job3.client_message_id()).unwrap();
    outcome(
        &fx,
        &lost2,
        "send",
        Outcome::Unknown {
            detail: "lost".into(),
        },
    );
    let verify = fx.take_jobs().remove(0);
    let result = send::run(&verify);
    assert_eq!(
        outcome(&fx, &lost2, "verify", result)["delivery_state"],
        "accepted"
    );
    drop(job);

    // 서버가 없으면 보내지 못한 것으로 기록하고 backoff 뒤 다시 보낸다.
    client.close();
    let status = tasty_reaper::terminate_process_group(&mut server, Duration::from_secs(5));
    assert!(status.is_ok(), "{status:?}");
    let gone = deliver_id(&fx, &rid, "k:4");
    let job = fx.take_jobs().remove(0);
    let result = send::run(&job);
    assert!(matches!(result, Outcome::NotSent { .. }), "{result:?}");
    assert_eq!(
        outcome(&fx, &gone, "send", result)["delivery_state"],
        "captured"
    );
    assert!(!fx.rec.wakes.borrow().is_empty());
}

/// 실제 턴이 실행되는 동안 소유 서버 probe가 그 thread를 active로, 끝난 뒤 active가 아니라고
/// 읽는다. 사용자의 기존 Codex 로그인을 런타임이 그대로 쓰며 인증 파일을 읽거나 복사하지 않는다.
/// 서버 환경에서 TASTY_* 를 지워 사용자 훅이 Tasty를 부르지 않게 하고, 만든 thread는 지운다.
#[cfg(unix)]
#[test]
#[ignore = "실제 codex와 사용자의 기존 로그인을 쓴다. TASTY_CODEX_NORMAL_HOME_E2E=1로만 실행한다"]
fn the_owned_server_reports_a_running_turn_as_active() {
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};

    use crate::rpc::{Client, Endpoint};

    assert_eq!(
        std::env::var("TASTY_CODEX_NORMAL_HOME_E2E").as_deref(),
        Ok("1")
    );
    let work = tempfile::tempdir().unwrap();
    let sock_dir = tempfile::Builder::new()
        .prefix("tc")
        .tempdir_in("/tmp")
        .unwrap();
    let url = format!("unix://{}/s.sock", sock_dir.path().display());
    let mut cmd = std::process::Command::new("codex");
    cmd.args(["app-server", "--listen", &url])
        .current_dir(work.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_THREAD_ID");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("TASTY_") {
            cmd.env_remove(k);
        }
    }
    let mut server = cmd.spawn().unwrap();
    let endpoint = Endpoint::Unix(sock_dir.path().join("s.sock"));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        if let Ok(c) = Client::connect(&endpoint, Duration::from_secs(5)) {
            break c;
        }
        assert!(Instant::now() < deadline, "server not ready");
        std::thread::sleep(Duration::from_millis(200));
    };
    let started = client
        .call("thread/start", json!({ "cwd": work.path() }))
        .unwrap();
    let thread = started["thread"]["id"].as_str().unwrap().to_string();
    let before = owned_thread_active(&endpoint, &thread);
    let started_turn = client
        .call(
            "turn/start",
            json!({ "threadId": thread, "input": [{ "type": "text",
                "text": "Reply with exactly ACK-LIVE and nothing else. Do not run any tool." }] }),
        )
        .unwrap();
    let mut seen_active = false;
    let mut seen_idle_after = false;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        match owned_thread_active(&endpoint, &thread) {
            Ok(true) => seen_active = true,
            Ok(false) if seen_active => {
                seen_idle_after = true;
                break;
            }
            Ok(false) => {}
            Err(e) => panic!("probe: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // 끝난 턴을 원장 확정 경로로 읽는다.
    let finished = super::turns::read_turn(&super::turns::TurnJob {
        name: "0".repeat(64),
        candidate_id: "0".repeat(32),
        endpoint: endpoint.clone(),
        thread: thread.clone(),
        turn_id: started_turn["turn"]["id"].as_str().unwrap().to_string(),
    });
    let archived = client.call("thread/archive", json!({ "threadId": thread }));
    let deleted = client.call("thread/delete", json!({ "threadId": thread }));
    client.close();
    let stopped = tasty_reaper::terminate_process_group(&mut server, Duration::from_secs(5));
    assert_eq!(before, Ok(false));
    assert!(
        seen_active && seen_idle_after,
        "active {seen_active} idle {seen_idle_after}"
    );
    assert!(
        archived.is_ok() && deleted.is_ok(),
        "{archived:?} {deleted:?}"
    );
    assert!(stopped.is_ok(), "{stopped:?}");
    assert_eq!(
        finished,
        super::turns::TurnOutcome::Final {
            status: "completed".into(),
            body: Some("ACK-LIVE".into())
        }
    );
}

#[test]
fn a_background_native_thread_cannot_replace_the_selected_conversation() {
    let fx = Fixture::new();
    let generation = start_thread(&fx, 7, PARENT);
    let mut session = crate::session::registry::read_session(fx.root(), &generation)
        .unwrap()
        .unwrap();
    session.native_args = Some(Vec::new());
    crate::session::registry::write_session(fx.root(), &session).unwrap();
    for event in ["session-start", "session-end", "stop", "prompt-submit"] {
        let result = fx
            .with(|d| {
                crate::handlers::handle_hook(
                    &OkHost,
                    &json!({"surface":7,"gen":generation,"session":OTHER,"event":event}),
                    &fx.tr,
                    d,
                )
            })
            .unwrap();
        assert_eq!(result["ignored"], "other_thread");
    }
    assert_eq!(
        crate::session::registry::read_session(fx.root(), &generation)
            .unwrap()
            .unwrap()
            .thread
            .as_deref(),
        Some(PARENT)
    );
}

/// attach 원격 출처 부모의 자식: 확정한 턴은 원장이 아니라 보낼 기록으로 옮겨 서버 host 에 맡긴다. client 가 확인할
/// 때까지 기록을 두고, 확인(delivered) 뒤에만 지운다.
#[test]
fn a_turn_of_an_attach_child_goes_to_the_attach_parent_not_the_ledger() {
    let clock = crate::store::test_clock::freeze();
    let mut fx = Fixture::new();
    fx.loaded = Ok(true);
    ledger::set_attach_child(
        fx.root(),
        31,
        "attach:o3",
        "0123456789abcdef0123456789abcdef",
        Some(1),
    )
    .unwrap();
    let generation = start_thread(&fx, 31, CHILD);
    fx.ready(&generation, "unix:///owned/c.sock");
    hook(
        &fx,
        json!({"surface":31,"event":"stop","session":CHILD,"gen":generation,"turn_id":"T1"}),
    );
    fx.answer_turns(|_| completed("FINAL"));
    assert!(
        ledger::all_events(fx.root()).is_empty(),
        "no local receiver"
    );
    assert_eq!(super::attach_out::pending(fx.root()), 1);
    let notices = fx.rec.notices.borrow().clone();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, 31);
    assert_eq!(notices[0].1, format!("codex:{CHILD}:T1"));
    assert_eq!(notices[0].2["completion"]["body"], json!("FINAL"));
    assert_eq!(
        notices[0].2["completion"]["route"],
        json!("0123456789abcdef0123456789abcdef"),
        "the result carries the route token of the instruction"
    );
    assert!(
        crate::delivery::parse_completion(&notices[0].2["completion"]).is_ok(),
        "the client reads it with the same parser"
    );

    *fx.rec.notice_status.borrow_mut() =
        Some(tasty_plugin_agent_common::remote_attach::NoticeStatus::Delivered);
    // 재시작은 남은 기록을 다시 묻는 계기다. 간격 전에는 묻지 않는다.
    fx.with(super::recover);
    assert_eq!(super::attach_out::pending(fx.root()), 1);
    clock.advance(backoff(1));
    fx.with(super::recover);
    assert_eq!(super::attach_out::pending(fx.root()), 0);
    assert_eq!(fx.rec.notices.borrow().len(), 2);
}
