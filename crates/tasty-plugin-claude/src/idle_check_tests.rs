//! `claude.idle_check` 등록과 tell·spawn 자동 등록 시험.

use std::cell::RefCell;
use std::sync::Mutex;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_sdk::i18n::Translator;

use super::*;
use crate::error_scan::idle::IDLE_CHECK_META_KEY;
use crate::error_scan::{ErrorScanner, ScanTarget, lock_scanner};

const CHILD: u32 = 7;
const PARENT: u32 = 3;

fn tr() -> Translator {
    Translator::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang"),
        "en",
    )
}

/// 상태와 Claude 세션 meta 를 정하는 호스트. idle meta 는 실제처럼 저장하고, 모든 호출을 기록한다.
struct TargetHost<'a> {
    state: Result<&'static str, &'static str>,
    session: bool,
    tell_fails: bool,
    /// `terminal.tell` 을 받는 동안 할 일(보낸 직후 처리 전에 온 Stop 등).
    on_tell: Option<Box<dyn Fn() + 'a>>,
    idle_meta: RefCell<Option<String>>,
    /// `surface.list` 을 실패시킨다.
    list_fails: RefCell<bool>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl TargetHost<'_> {
    fn new(state: &'static str) -> Self {
        Self {
            state: Ok(state),
            session: true,
            tell_fails: false,
            on_tell: None,
            idle_meta: RefCell::new(None),
            list_fails: RefCell::new(false),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn called(&self, method: &str) -> bool {
        self.calls.borrow().iter().any(|(m, _)| m == method)
    }
}

fn host_error(method: &str, message: &str) -> tasty_plugin_sdk::PluginError {
    tasty_plugin_sdk::PluginError::HostCall {
        method: method.to_string(),
        message: message.to_string(),
        code: None,
    }
}

impl HostCall for TargetHost<'_> {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.calls
            .borrow_mut()
            .push((method.to_string(), params.clone()));
        let idle_key = params["key"] == IDLE_CHECK_META_KEY;
        match method {
            "terminal.tell" if !self.tell_fails => {
                if let Some(f) = &self.on_tell {
                    f();
                }
                Ok(json!({}))
            }
            "surface.list" if *self.list_fails.borrow() => Err(host_error(method, "list failed")),
            "surface.list" => Ok(json!([{ "id": CHILD }])),
            "surface.meta.set" if idle_key => {
                *self.idle_meta.borrow_mut() = params["value"].as_str().map(String::from);
                Ok(json!({}))
            }
            "surface.meta.unset" if idle_key => {
                *self.idle_meta.borrow_mut() = None;
                Ok(json!({}))
            }
            "surface.meta.get" if idle_key => Ok(json!({ "value": *self.idle_meta.borrow() })),
            "terminal.state" => self
                .state
                .map(|s| json!({ "state": s }))
                .map_err(|e| host_error(method, e)),
            "terminal.tell" if self.tell_fails => Err(host_error(method, "pty closed")),
            "surface.meta.get" if params["key"] == "claude-session-id" && self.session => {
                Ok(json!({ "value": "sess-1" }))
            }
            "surface.meta.get" => Ok(json!({ "value": null })),
            _ => Ok(json!({})),
        }
    }
}

fn checks(scanner: &Mutex<ErrorScanner>) -> Vec<(u32, u64)> {
    lock_scanner(scanner).idle.of_target(CHILD)
}

/// 단독 등록은 대상에 아무것도 보내지 않고 체크 하나를 건다. 응답에 id·기한·지금 상태를 싣는다.
#[test]
fn a_standalone_registration_sends_nothing_and_answers_the_check() {
    let host = TargetHost::new("idle");
    let scanner = Mutex::new(ErrorScanner::new());
    let resp = handle_idle_check(
        &scanner,
        &host,
        &json!({ "surface": CHILD, "caller_surface": PARENT }),
        &tr(),
    )
    .expect("registered");
    assert_eq!(resp["registered"], true);
    assert_eq!(resp["target_surface"], CHILD);
    assert_eq!(resp["caller_surface"], PARENT);
    assert_eq!(resp["wait_secs"], 120);
    assert_eq!(resp["state"], "idle");
    assert_eq!(resp["replaced"], Value::Null);
    assert!(!host.called("terminal.tell") && !host.called("surface.send"));
    let id = resp["id"].as_u64().unwrap();
    assert_eq!(checks(&scanner), vec![(PARENT, id)]);

    let again = handle_idle_check(
        &scanner,
        &host,
        &json!({ "surface": CHILD, "caller_surface": PARENT }),
        &tr(),
    )
    .unwrap();
    assert_eq!(again["replaced"], id, "같은 부모·대상은 새 체크로 바꾼다");
    assert_eq!(checks(&scanner).len(), 1);
}

/// 대상 검증에 실패하면 체크를 남기지 않는다.
#[test]
fn a_rejected_registration_leaves_no_check() {
    let tr = tr();
    let cases: Vec<(TargetHost, Value, &str)> = vec![
        (
            TargetHost::new("idle"),
            json!({ "surface": CHILD }),
            "caller_surface",
        ),
        (
            TargetHost::new("idle"),
            json!({ "surface": CHILD, "caller_surface": CHILD }),
            "itself",
        ),
        (
            TargetHost::new("exited"),
            json!({ "surface": CHILD, "caller_surface": PARENT }),
            "no running terminal",
        ),
        (
            TargetHost {
                state: Err("ipc timeout"),
                ..TargetHost::new("idle")
            },
            json!({ "surface": CHILD, "caller_surface": PARENT }),
            "could not read the state",
        ),
        (
            TargetHost {
                session: false,
                ..TargetHost::new("idle")
            },
            json!({ "surface": CHILD, "caller_surface": PARENT }),
            "not a Claude surface",
        ),
    ];
    for (host, params, expected) in cases {
        let scanner = Mutex::new(ErrorScanner::new());
        let err = handle_idle_check(&scanner, &host, &params, &tr).unwrap_err();
        assert!(
            err.message.contains(expected),
            "{expected}: {}",
            err.message
        );
        assert!(checks(&scanner).is_empty(), "{expected}");
        assert!(!host.called("surface.meta.set"), "{expected}");
    }
    // 감시 중인 Claude surface 는 세션 meta 가 없어도 받는다.
    let host = TargetHost {
        session: false,
        ..TargetHost::new("active")
    };
    let scanner = Mutex::new(ErrorScanner::new());
    lock_scanner(&scanner).enable(CHILD, ScanTarget::Child);
    handle_idle_check(
        &scanner,
        &host,
        &json!({ "surface": CHILD, "caller_surface": PARENT }),
        &tr,
    )
    .expect("tracked surface");
}

/// tell 은 보낸 뒤 체크를 건다. 보내기에 실패하면 걸지 않는다. 받을 부모가 없으면 걸지 않는다.
#[test]
fn a_tell_registers_a_check_only_when_the_message_was_sent() {
    let tr = tr();
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    let pairing = Mutex::new(StopPairing::default());

    let host = TargetHost::new("idle");
    let scanner = Mutex::new(ErrorScanner::new());
    handle_tell_with_idle_check(
        &scanner,
        &returns,
        &pairing,
        &host,
        &json!({ "surface": CHILD, "caller_surface": PARENT, "message": "work" }),
        &tr,
    )
    .expect("sent");
    assert!(host.called("terminal.tell"));
    assert_eq!(checks(&scanner).len(), 1);

    let host = TargetHost {
        tell_fails: true,
        ..TargetHost::new("idle")
    };
    let scanner = Mutex::new(ErrorScanner::new());
    assert!(
        handle_tell_with_idle_check(
            &scanner,
            &returns,
            &pairing,
            &host,
            &json!({ "surface": CHILD, "caller_surface": PARENT, "message": "work" }),
            &tr,
        )
        .is_err()
    );
    assert!(
        checks(&scanner).is_empty(),
        "보내지 못한 tell 은 체크를 남기지 않는다"
    );

    let host = TargetHost::new("idle");
    let scanner = Mutex::new(ErrorScanner::new());
    handle_tell_with_idle_check(
        &scanner,
        &returns,
        &pairing,
        &host,
        &json!({ "surface": CHILD, "message": "work" }),
        &tr,
    )
    .unwrap();
    assert!(checks(&scanner).is_empty());
}

/// spawn·respawn 은 일을 준(프롬프트) 물리 부모에게만 체크를 건다.
#[test]
fn a_child_gets_a_check_only_from_a_surface_parent_that_gave_it_work() {
    let host = TargetHost::new("active");
    let now = At::now();
    for (target, with_prompt, expected) in [
        (ParentTarget::Legacy(Some(PARENT)), true, 1),
        (ParentTarget::Legacy(Some(PARENT)), false, 0),
        (ParentTarget::Legacy(None), true, 0),
    ] {
        let scanner = Mutex::new(ErrorScanner::new());
        register_for_child(
            &scanner,
            &host,
            &target,
            CHILD,
            with_prompt,
            Origin::Spawn,
            now,
        );
        assert_eq!(checks(&scanner).len(), expected, "{target:?} {with_prompt}");
    }
}

/// tell 은 보내기 전 시각과 문턱으로 등록한다. 보낸 직후 등록 전에 그 메시지의 턴이 끝나 Stop 이 확정되면 체크는
/// 바로 끝난다(거짓 idle 이 없다).
#[test]
fn a_turn_that_ends_before_the_tell_registers_ends_its_check() {
    let tr = tr();
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    let pairing = Mutex::new(StopPairing::default());
    let scanner = Mutex::new(ErrorScanner::new());
    let quiet_host = TargetHost::new("idle");
    let host = TargetHost {
        on_tell: Some(Box::new(|| {
            // 보내기 전 순번 0 → 메시지가 연 턴(순번 1)이 곧바로 끝났다.
            lock_scanner(&scanner).idle_complete(
                &quiet_host,
                CHILD,
                Finish::Turn {
                    at: std::time::Instant::now(),
                    seq: 1,
                    turn: None,
                },
            );
        })),
        ..TargetHost::new("idle")
    };
    let resp = handle_tell_with_idle_check(
        &scanner,
        &returns,
        &pairing,
        &host,
        &json!({ "surface": CHILD, "caller_surface": PARENT, "message": "work" }),
        &tr,
    )
    .expect("sent");
    assert!(resp.is_object());
    assert!(
        checks(&scanner).is_empty(),
        "이미 끝난 턴의 체크는 남지 않는다"
    );
}

/// tell 을 보내고 등록하기 전까지는 그 대상의 이력을 대상당 상한으로 버리지 않는다. 그 사이 메시지의 턴 끝 뒤에
/// 상한보다 많은 다른 완료가 와도 메시지의 턴 끝이 체크를 끝낸다.
#[test]
fn finishes_between_the_tell_and_its_registration_are_all_kept() {
    let tr = tr();
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    let pairing = Mutex::new(StopPairing::default());
    let scanner = Mutex::new(ErrorScanner::new());
    let quiet_host = TargetHost::new("idle");
    let host = TargetHost {
        on_tell: Some(Box::new(|| {
            let mut s = lock_scanner(&scanner);
            s.idle_complete(
                &quiet_host,
                CHILD,
                Finish::Turn {
                    at: std::time::Instant::now(),
                    seq: 1,
                    turn: None,
                },
            );
            // 이 체크를 끝내지 않는 완료(지시를 모르는 원격 결과)가 상한보다 많이 온다.
            for _ in 0..20 {
                s.idle_complete(&quiet_host, CHILD, Finish::Remote { issued_ms: None });
            }
        })),
        ..TargetHost::new("idle")
    };
    handle_tell_with_idle_check(
        &scanner,
        &returns,
        &pairing,
        &host,
        &json!({ "surface": CHILD, "caller_surface": PARENT, "message": "work" }),
        &tr,
    )
    .expect("sent");
    assert!(checks(&scanner).is_empty(), "메시지의 턴 끝이 남았다");
}

/// 자기 자신에게 보낸 tell 에는 체크를 걸지 않는다.
#[test]
fn a_tell_to_itself_registers_no_check() {
    let tr = tr();
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    let pairing = Mutex::new(StopPairing::default());
    let host = TargetHost::new("idle");
    let scanner = Mutex::new(ErrorScanner::new());
    handle_tell_with_idle_check(
        &scanner,
        &returns,
        &pairing,
        &host,
        &json!({ "surface": CHILD, "caller_surface": CHILD, "message": "note" }),
        &tr,
    )
    .expect("a tell to itself is sent");
    assert!(host.called("terminal.tell"));
    assert!(checks(&scanner).is_empty());
}

/// respawn 은 앞 세션의 체크를 지운다. 프롬프트 없이 다시 띄우면 체크가 없으므로 옛 체크의 거짓 idle 줄도 없다.
#[test]
fn a_respawn_drops_the_checks_of_the_old_session() {
    crate::notice::test_log::take();
    let host = TargetHost::new("idle");
    let scanner = Mutex::new(ErrorScanner::new());
    let t0 = std::time::Instant::now();
    lock_scanner(&scanner).idle_register(
        &host,
        CHILD,
        PARENT,
        Origin::Tell,
        Owner::at(At { instant: t0, ms: 0 }),
    );
    after_respawn(
        &scanner,
        &host,
        &ParentTarget::Legacy(Some(PARENT)),
        CHILD,
        false,
        At::now(),
    );
    assert!(checks(&scanner).is_empty());
    assert_eq!(*host.idle_meta.borrow(), None);
    crate::error_scan::idle::run_idle_checks(
        &scanner,
        &host,
        "{0} {1} {2}",
        t0 + std::time::Duration::from_secs(600),
    );
    assert!(crate::notice::test_log::take().is_empty());

    // 프롬프트를 주면 새 세션의 체크 하나만 남는다.
    lock_scanner(&scanner).idle_register(
        &host,
        CHILD,
        4,
        Origin::Manual,
        Owner::at(At { instant: t0, ms: 0 }),
    );
    after_respawn(
        &scanner,
        &host,
        &ParentTarget::Legacy(Some(PARENT)),
        CHILD,
        true,
        At::now(),
    );
    assert_eq!(
        checks(&scanner).iter().map(|c| c.0).collect::<Vec<_>>(),
        vec![PARENT]
    );
}

/// 시작 복원은 끝난 터미널(`exited`)의 남은 체크를 되살리지 않고 meta 도 지운다.
#[test]
fn the_start_restore_skips_exited_targets() {
    let host = TargetHost::new("exited");
    *host.idle_meta.borrow_mut() =
        Some(r#"{"checks":[{"caller":3,"id":1,"origin":"tell","registered_ms":1}]}"#.to_string());
    let scanner = Mutex::new(ErrorScanner::new());
    assert_eq!(restore_all(&scanner, &host), 0);
    assert!(checks(&scanner).is_empty());
    assert_eq!(*host.idle_meta.borrow(), None);

    let host = TargetHost::new("idle");
    *host.idle_meta.borrow_mut() =
        Some(r#"{"checks":[{"caller":3,"id":1,"origin":"tell","registered_ms":1}]}"#.to_string());
    let scanner = Mutex::new(ErrorScanner::new());
    assert_eq!(restore_all(&scanner, &host), 1);
}

/// 시작 복원을 마치면 이력 보호를 풀어 일반 상한으로 돌아간다. 복원 중 surface 목록 조회가 실패해도 같다.
#[test]
fn the_start_restore_returns_the_history_to_its_limits() {
    for fails in [false, true] {
        let host = TargetHost::new("idle");
        let scanner = Mutex::new({
            let mut s = ErrorScanner::new();
            s.idle.begin_restore(std::time::Instant::now());
            s
        });
        if fails {
            *host.list_fails.borrow_mut() = true;
        }
        restore_all(&scanner, &host);
        let now = std::time::Instant::now();
        let mut s = lock_scanner(&scanner);
        for target in 1000..2000 {
            s.idle.complete(
                target,
                Finish::Turn {
                    at: now,
                    seq: 1,
                    turn: None,
                },
                now,
            );
        }
        assert!(
            s.idle.history_targets() <= crate::error_scan::idle::HISTORY_TARGETS_MAX,
            "fails={fails}"
        );
    }
}

/// 원격 자식: 기록된 결과는 그 결과의 route 토큰을 발급한 시각이 체크 등록 뒤일 때만 체크를 끝낸다. t=0 에 등록하고
/// t=130 에 다시 등록한 뒤 t=131 에 앞 지시의 결과가 기록돼도 새 체크는 남는다. 재전송·거절은 끝내지 않고 종료는 끝낸다.
#[test]
fn a_late_result_of_an_earlier_remote_instruction_keeps_the_new_check() {
    let host = TargetHost::new("active");
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    let to = crate::codex_return::RouteTo::Surface {
        surface: PARENT,
        session: None,
    };
    let token = returns.issue_attach_route(&to).expect("token");
    let issued = returns.attach_route_issued_ms(&token).expect("issued");
    let params = |status: &str| {
        json!({
            "surface_id": CHILD,
            "payload": { "completion": {
                "status": status,
                "completion_key": "k",
                "route": token,
            } },
        })
    };
    let t0 = std::time::Instant::now();
    let register = |scanner: &Mutex<ErrorScanner>, secs: u64| {
        lock_scanner(scanner).idle_register(
            &host,
            CHILD,
            PARENT,
            Origin::Manual,
            Owner::at(At {
                instant: t0 + std::time::Duration::from_secs(secs),
                ms: issued + secs * 1000,
            }),
        )
    };
    // 지시와 함께 건 체크(t=0)는 그 지시의 결과로 끝난다.
    for (status, response, ended) in [
        ("turn_ended", "recorded", true),
        ("turn_failed", "recorded", true),
        ("exited", "recorded", true),
        ("turn_ended", "duplicate", false),
        ("turn_ended", "rejected", false),
    ] {
        let scanner = Mutex::new(ErrorScanner::new());
        register(&scanner, 0);
        after_remote_receive(
            &scanner,
            &returns,
            &host,
            &params(status),
            &json!({ "status": response }),
        );
        assert_eq!(checks(&scanner).is_empty(), ended, "{status}/{response}");
    }
    // t=130 에 다시 건 체크는 t=0 지시의 늦은 결과로 끝나지 않는다.
    let scanner = Mutex::new(ErrorScanner::new());
    register(&scanner, 0);
    let newer = register(&scanner, 130);
    after_remote_receive(
        &scanner,
        &returns,
        &host,
        &params("turn_ended"),
        &json!({ "status": "recorded" }),
    );
    assert_eq!(checks(&scanner), vec![(PARENT, newer.id)]);
    // 종료는 지시와 관계없이 끝낸다.
    after_remote_receive(
        &scanner,
        &returns,
        &host,
        &params("exited"),
        &json!({ "status": "recorded" }),
    );
    assert!(checks(&scanner).is_empty());
}

/// 실행 중인 대상에 보낸 tell 은 보내기 전 상태·순번으로 정한 문턱으로 건다. 진행 중이던 턴의 끝(같은 순번)은 이
/// 체크를 끝내지 않는다.
#[test]
fn a_tell_to_a_running_target_keeps_its_check_past_the_running_turn() {
    let tr = tr();
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    let pairing = Mutex::new(StopPairing::default());
    let host = TargetHost::new("active");
    let scanner = Mutex::new(ErrorScanner::new());
    handle_tell_with_idle_check(
        &scanner,
        &returns,
        &pairing,
        &host,
        &json!({ "surface": CHILD, "caller_surface": PARENT, "message": "more" }),
        &tr,
    )
    .expect("sent");
    lock_scanner(&scanner).idle_complete(
        &host,
        CHILD,
        Finish::Turn {
            at: std::time::Instant::now(),
            seq: 0,
            turn: None,
        },
    );
    assert_eq!(checks(&scanner).len(), 1, "진행 중이던 턴의 끝");
    lock_scanner(&scanner).idle_complete(
        &host,
        CHILD,
        Finish::Turn {
            at: std::time::Instant::now(),
            seq: 1,
            turn: None,
        },
    );
    assert!(checks(&scanner).is_empty(), "다음 턴의 끝");
}

/// tell 경로가 끝나면(보내기 성공·실패·패닉) 그 대상의 이력 보호가 풀려 대상당 상한이 다시 적용된다.
#[test]
fn the_tell_path_returns_the_target_history_to_its_limits() {
    let tr = tr();
    let dir = tempfile::tempdir().unwrap();
    let returns = Returns::new(Some(dir.path()));
    let pairing = Mutex::new(StopPairing::default());
    let params = json!({ "surface": CHILD, "caller_surface": PARENT, "message": "work" });
    for case in ["sent", "failed", "panicked"] {
        let host = TargetHost {
            tell_fails: case == "failed",
            on_tell: (case == "panicked")
                .then(|| -> Box<dyn Fn()> { Box::new(|| panic!("tell panicked in the test")) }),
            ..TargetHost::new("idle")
        };
        let scanner = Mutex::new(ErrorScanner::new());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_tell_with_idle_check(&scanner, &returns, &pairing, &host, &params, &tr)
        }));
        let mut s = lock_scanner(&scanner);
        for _ in 0..20 {
            s.idle_complete(&host, CHILD, Finish::Remote { issued_ms: None });
        }
        assert!(
            s.idle.history_len(CHILD) <= crate::error_scan::idle::HISTORY_PER_TARGET,
            "{case}: {}",
            s.idle.history_len(CHILD)
        );
    }
}
