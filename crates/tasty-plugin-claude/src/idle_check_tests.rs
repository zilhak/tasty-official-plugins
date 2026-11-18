//! `claude.idle_check` 등록과 tell·spawn 자동 등록 시험.

use std::cell::RefCell;
use std::sync::Mutex;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_sdk::i18n::Translator;

use super::*;
use crate::error_scan::{ErrorScanner, ScanTarget, lock_scanner};

const CHILD: u32 = 7;
const PARENT: u32 = 3;

fn tr() -> Translator {
    Translator::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang"),
        "en",
    )
}

/// 상태와 Claude 세션 meta 를 정하는 호스트. 모든 호출을 기록한다.
struct TargetHost {
    state: Result<&'static str, &'static str>,
    session: bool,
    tell_fails: bool,
    calls: RefCell<Vec<(String, Value)>>,
}

impl TargetHost {
    fn new(state: &'static str) -> Self {
        Self {
            state: Ok(state),
            session: true,
            tell_fails: false,
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

impl HostCall for TargetHost {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.calls
            .borrow_mut()
            .push((method.to_string(), params.clone()));
        match method {
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
    let now = std::time::Instant::now();
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
