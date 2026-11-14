use super::*;
use crate::error_scan::ScanTarget;
use crate::error_scan::quiet::QUIET_RELEASED_EVENT;
use crate::error_scan::quiet::{EndReason, Held, HeldKind, Overflow, QUIET_META_KEY, QuietNotice};
use std::cell::RefCell;
use std::collections::HashMap;

fn tr() -> Translator {
    Translator::load(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang"),
        "en",
    )
}

/// meta 를 실제처럼 저장하고 대상 상태를 정할 수 있는 호스트.
struct Host {
    meta: RefCell<HashMap<String, String>>,
    state: RefCell<&'static str>,
    fired: RefCell<Vec<String>>,
    /// 묶음 훅을 건 부모.
    parents: RefCell<Vec<u32>>,
    /// 훅 목록 조회를 실패시킨다(surface 가 이미 없는 경우).
    hooks_fail: std::cell::Cell<bool>,
}

impl Host {
    fn new() -> Self {
        Self {
            meta: RefCell::new(HashMap::new()),
            state: RefCell::new("active"),
            fired: RefCell::new(Vec::new()),
            parents: RefCell::new(Vec::new()),
            hooks_fail: std::cell::Cell::new(false),
        }
    }
    fn meta_sets(&self) -> usize {
        self.meta.borrow().len()
    }
}

impl HostCall for Host {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        if method == "hook.list" && self.hooks_fail.get() {
            return Err(tasty_plugin_sdk::PluginError::HostCall {
                method: method.into(),
                message: "surface not found".into(),
                code: Some(-32602),
            });
        }
        let key = params["key"].as_str().unwrap_or_default().to_string();
        Ok(match method {
            "terminal.state" => json!({ "state": *self.state.borrow() }),
            "surface.meta.get" => json!({ "value": self.meta.borrow().get(&key) }),
            "surface.meta.set" => {
                self.meta
                    .borrow_mut()
                    .insert(key, params["value"].as_str().unwrap_or_default().into());
                json!({})
            }
            "surface.meta.unset" => {
                self.meta.borrow_mut().remove(&key);
                json!({})
            }
            "surface.fire_hook" => {
                self.fired
                    .borrow_mut()
                    .push(params["event"].as_str().unwrap_or_default().into());
                json!({})
            }
            "hook.list" => {
                let target = params["surface_id"].as_u64().unwrap_or_default() as u32;
                let hooks: Vec<Value> = self
                    .parents
                    .borrow()
                    .iter()
                    .flat_map(|&p| {
                        [
                            json!({ "event": QUIET_RELEASED_EVENT, "command": notify_quiet_release_command(p, target) }),
                            json!({ "event": "process-exit", "command": format!("tasty claude notify-done --caller-surface {p}") }),
                        ]
                    })
                    .collect();
                json!(hooks)
            }
            _ => json!({}),
        })
    }
}

fn tracked() -> Mutex<ErrorScanner> {
    let mut s = ErrorScanner::new();
    s.enable(5, ScanTarget::Child);
    Mutex::new(s)
}

fn quiet(scanner: &Mutex<ErrorScanner>, host: &Host, params: Value) -> Result<Value, i32> {
    handle_quiet(scanner, host, &params, &tr()).map_err(|e| e.code)
}

/// 0·-1 이 아닌 음수·`--secs` 와 `--clear` 동시·둘 다 없음·추적하지 않는 surface 는 -32602 로 거절하고 meta 를 쓰지 않는다.
/// `-1` 과 상한 없는 큰 값은 받는다.
#[test]
fn bad_declarations_are_refused_without_writing_anything() {
    let scanner = tracked();
    let host = Host::new();
    for params in [
        json!({ "surface": 5, "secs": 0, "clear": false }),
        json!({ "surface": 5, "secs": -2, "clear": false }),
        json!({ "surface": 5, "secs": 60, "clear": true }),
        json!({ "surface": 5, "clear": false }),
        json!({ "surface": 5, "secs": "60", "clear": false }),
        json!({ "surface": 9, "secs": 60, "clear": false }),
    ] {
        assert_eq!(
            quiet(&scanner, &host, params.clone()),
            Err(-32602),
            "{params}"
        );
    }
    assert_eq!(host.meta_sets(), 0);
    let ok = quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "secs": -1, "clear": false }),
    )
    .expect("-1");
    assert_eq!(
        (ok["mode"].as_str(), ok["until_ms"].is_null()),
        (Some("discard"), true)
    );
    let ok = quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "task": "b1", "secs": 172_800, "clear": false }),
    )
    .expect("상한 없음");
    assert_eq!(ok["mode"], "hold");
    let ok = quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "task": "b2", "secs": i64::MAX, "clear": false }),
    )
    .expect("i64 최댓값");
    assert_eq!(ok["until_ms"], u64::MAX, "감기지 않고 가장 먼 끝 시각");
    assert_eq!(
        quiet(&scanner, &host, json!({ "surface": 5, "clear": true })).unwrap()["cleared"],
        3,
        "--task 없는 --clear 는 모든 선언을 끝낸다"
    );
}

/// 쌓인 것이 있는 양수 선언에서 `--clear` 하면 묶음을 남기고 부모 훅을 한 번 부른다.
#[test]
fn clearing_a_declaration_with_held_notices_releases_them() {
    let scanner = tracked();
    let host = Host::new();
    quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "task": "T", "secs": 600, "clear": false }),
    )
    .unwrap();
    hold_one(&scanner, &host, "T");
    let r = quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "task": "T", "clear": true }),
    )
    .unwrap();
    assert_eq!(
        (r["cleared"].as_u64(), r["released"].as_bool()),
        (Some(1), Some(true))
    );
    assert_eq!(*host.fired.borrow(), vec![QUIET_RELEASED_EVENT.to_string()]);
}

fn hold_one(scanner: &Mutex<ErrorScanner>, host: &Host, task: &str) {
    let notice = QuietNotice::Tasks(vec![(task.to_string(), "sleep 600".to_string())]);
    crate::error_scan::lock_scanner(scanner).dispose_quiet(host, 5, notice, 600_000);
}

/// 묶음은 부모마다 `[warning]` 한 줄로 한 번 쓴다.
#[test]
fn a_release_is_written_once_to_each_parent() {
    let scanner = tracked();
    let host = Host::new();
    quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "secs": 600, "clear": false }),
    )
    .unwrap();
    hold_one(&scanner, &host, "T");
    hold_one(&scanner, &host, "T");
    quiet(&scanner, &host, json!({ "surface": 5, "clear": true })).unwrap();
    crate::notice::test_log::take();
    let params = |caller| json!({ "caller_surface": caller, "target_surface": 5 });
    for caller in [1, 2, 1] {
        handle_notify_quiet_release(&scanner, &host, &params(caller), &tr()).unwrap();
    }
    let lines = crate::notice::test_log::take();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!((lines[0].0, lines[1].0), (1, 2));
    let line = &lines[0].1;
    assert!(line.starts_with("[warning] surface 5: 2 held"), "{line}");
    assert!(
        line.contains("(cleared)") && line.contains("sleep 600 ×2"),
        "{line}"
    );
    assert!(!line.contains("has written no output"), "{line}");
}

/// error 완료 줄은 그 surface 의 선언을 모두 끝내고 쌓인 것을 같은 줄에 붙인다. 부모 둘이 각각 한 번씩 받는다.
/// `-1` 선언이면 error 줄만 쓰고, info 줄은 선언을 그대로 둔다.
#[test]
fn an_error_line_ends_the_declarations_and_carries_what_they_held() {
    let scanner = tracked();
    let host = Host::new();
    quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "task": "T", "secs": 600, "clear": false }),
    )
    .unwrap();
    hold_one(&scanner, &host, "T");
    let done = |caller: u32| {
        crate::notifications::handle_notify_done(
            &scanner,
            &host,
            &json!({ "caller_surface": caller, "target_surface": 5, "command": "tell" }),
            &tr(),
        )
        .unwrap();
    };
    *host.state.borrow_mut() = "idle";
    crate::notice::test_log::take();
    done(1);
    assert!(
        host.meta.borrow().contains_key(QUIET_META_KEY),
        "info 는 선언을 둔다"
    );
    *host.state.borrow_mut() = "exited";
    done(1);
    done(2);
    let lines = crate::notice::test_log::take();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].1.starts_with("[info] "), "{lines:?}");
    for (caller, line) in &lines[1..] {
        assert!(line.starts_with("[error] surface 5: "), "{caller}: {line}");
        assert!(line.contains("; held 1 no-output"), "{caller}: {line}");
        assert!(!line.contains('\n'), "{line}");
    }
    assert!(!host.meta.borrow().contains_key(QUIET_META_KEY));

    let scanner = tracked();
    let host = Host::new();
    quiet(
        &scanner,
        &host,
        json!({ "surface": 5, "secs": -1, "clear": false }),
    )
    .unwrap();
    hold_one(&scanner, &host, "T");
    *host.state.borrow_mut() = "exited";
    crate::notice::test_log::take();
    crate::notifications::handle_notify_done(
        &scanner,
        &host,
        &json!({ "caller_surface": 1, "target_surface": 5, "command": "tell" }),
        &tr(),
    )
    .unwrap();
    let lines = crate::notice::test_log::take();
    assert!(!lines[0].1.contains("held"), "{lines:?}");
    assert!(!host.meta.borrow().contains_key(QUIET_META_KEY));
}

fn item(label: &str, count: u32) -> Held {
    Held {
        key: label.into(),
        kind: HeldKind::Task,
        label: label.into(),
        count,
        first_ms: 600_000,
        last_ms: 3_540_000,
        quiet_ms: 3_540_000,
    }
}

/// 묶음 줄은 한 줄이다. 잘리지 않은 이름이 와도 줄로 쓸 때 한 줄로 자른다. 항목이 많거나 상한을 넘으면 남은 알림 수만 적는다.
#[test]
fn a_release_line_is_one_line_with_capped_items() {
    let long = "x".repeat(200);
    let r = Release {
        id: "r".into(),
        reason: EndReason::Expired,
        task: None,
        secs: 5400,
        set_at_ms: 0,
        at_ms: 5_400_000,
        items: vec![
            item("first\nsecond\rthird", 2),
            item(&long, 1),
            item("c", 1),
            item("d", 1),
            item("e", 1),
            item("f", 1),
            item("g", 1),
        ],
        overflow: Some(Overflow {
            count: 3,
            first_ms: 3_600_000,
            last_ms: 3_700_000,
        }),
        delivered_to: Vec::new(),
        recipients: Vec::new(),
    };
    let line = release_line(&tr(), 14937, &r);
    assert!(!line.contains('\n') && !line.contains('\r'), "{line}");
    assert!(
        line.contains("first second third ×2 (task quiet, +10–59 min, quiet up to 59 min)"),
        "{line}"
    );
    assert!(line.contains(&format!("{}…", "x".repeat(80))), "{line}");
    assert!(!line.contains(&"x".repeat(81)), "{line}");
    assert!(
        line.contains("+5 more"),
        "적지 않은 2건과 상한을 넘은 3건: {line}"
    );
    assert!(
        line.contains("11 held") && line.contains("5400s") && line.contains("(expired)"),
        "{line}"
    );
}

/// 기다리는 작업 둘 모두에 작업 선언이 있을 때 화면 기준 대기 알림 1건은 묶음 줄 1개로 나간다.
#[test]
fn a_wait_held_by_two_task_declarations_is_released_once() {
    let scanner = tracked();
    let host = Host::new();
    for task in ["A", "B"] {
        quiet(
            &scanner,
            &host,
            json!({ "surface": 5, "task": task, "secs": 600, "clear": false }),
        )
        .unwrap();
    }
    let notice = QuietNotice::Wait {
        ids: vec!["A".into(), "B".into()],
        label: "shell".into(),
    };
    crate::error_scan::lock_scanner(&scanner).dispose_quiet(&host, 5, notice, 600_000);
    quiet(&scanner, &host, json!({ "surface": 5, "clear": true })).unwrap();
    crate::notice::test_log::take();
    handle_notify_quiet_release(
        &scanner,
        &host,
        &json!({ "caller_surface": 1, "target_surface": 5 }),
        &tr(),
    )
    .unwrap();
    let lines = crate::notice::test_log::take();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].1.contains("1 held"), "{lines:?}");
}

/// 선언 여럿이 한꺼번에 끝나 묶음이 남기는 개수보다 많아도, 묶음 훅을 건 부모 둘이 모두 받는다.
/// 일반 묶음 줄과 error 줄에 붙는 묶음 모두 같다.
#[test]
fn every_parent_gets_every_release_when_many_end_at_once() {
    let many = 20;
    for error in [false, true] {
        let scanner = tracked();
        let host = Host::new();
        *host.parents.borrow_mut() = vec![1, 2];
        for i in 0..many {
            let task = format!("T{i}");
            quiet(
                &scanner,
                &host,
                json!({ "surface": 5, "task": task, "secs": 600, "clear": false }),
            )
            .unwrap();
            hold_one(&scanner, &host, &task);
        }
        crate::notice::test_log::take();
        let mut held = Vec::new();
        if error {
            *host.state.borrow_mut() = "exited";
            for caller in [1, 2] {
                crate::notifications::handle_notify_done(
                    &scanner,
                    &host,
                    &json!({ "caller_surface": caller, "target_surface": 5, "command": "tell" }),
                    &tr(),
                )
                .unwrap();
            }
            for (caller, line) in crate::notice::test_log::take() {
                held.push((caller, line.matches("; held 1 no-output").count()));
            }
        } else {
            quiet(&scanner, &host, json!({ "surface": 5, "clear": true })).unwrap();
            for caller in [1, 2] {
                handle_notify_quiet_release(
                    &scanner,
                    &host,
                    &json!({ "caller_surface": caller, "target_surface": 5 }),
                    &tr(),
                )
                .unwrap();
            }
            for (caller, _) in crate::notice::test_log::take() {
                match held.iter_mut().find(|(c, _)| *c == caller) {
                    Some((_, n)) => *n += 1,
                    None => held.push((caller, 1)),
                }
            }
        }
        assert_eq!(held, vec![(1, many), (2, many)], "error {error}");
    }
}

/// meta 는 밖에서 바뀔 수 있다. 줄바꿈·긴 이름·상한보다 많은 항목을 담은 선언과 묶음을 되살려 보내도 줄은 한 줄이고
/// 상한을 넘은 항목은 남은 알림 수로 적는다.
#[test]
fn restored_meta_is_cut_to_one_line_and_capped() {
    let now = crate::error_scan::unix_now_ms();
    let held: Vec<Value> = (0..70)
        .map(|i| {
            let label = match i {
                0 => "first\nsecond".to_string(),
                1 => "x".repeat(200),
                _ => format!("cmd {i}"),
            };
            json!({ "key": format!("T{i}"), "kind": "task quiet", "label": label, "count": 1,
                    "first_ms": now, "last_ms": now, "quiet_ms": 600_000 })
        })
        .collect();
    let host = Host::new();
    host.meta.borrow_mut().insert(
        QUIET_META_KEY.into(),
        json!({ "decls": [{ "task": null, "mode": "hold", "secs": 600, "set_at_ms": now,
                            "until_ms": now + 600_000, "held": held }] })
        .to_string(),
    );
    host.meta.borrow_mut().insert(
        crate::error_scan::quiet::QUIET_RELEASE_META_KEY.into(),
        json!({ "releases": [{ "id": "r", "reason": "expired", "task": null, "secs": 600,
                               "set_at_ms": now, "at_ms": now, "items": held }] })
        .to_string(),
    );
    let scanner = tracked();
    let notify = || {
        handle_notify_quiet_release(
            &scanner,
            &host,
            &json!({ "caller_surface": 1, "target_surface": 5 }),
            &tr(),
        )
        .unwrap();
    };
    crate::notice::test_log::take();
    notify();
    quiet(&scanner, &host, json!({ "surface": 5, "clear": true })).unwrap();
    notify();
    let lines = crate::notice::test_log::take();
    assert_eq!(
        lines.len(),
        2,
        "되살린 묶음과 되살린 선언의 묶음: {lines:?}"
    );
    for (_, line) in &lines {
        assert!(!line.contains('\n') && !line.contains('\r'), "{line}");
        assert!(line.contains("first second ×1"), "{line}");
        assert!(line.contains(&format!("{}…", "x".repeat(80))), "{line}");
        assert!(!line.contains(&"x".repeat(81)), "{line}");
        assert!(
            line.contains("70 held") && line.contains("+65 more"),
            "64개 뒤 6건은 남은 수로: {line}"
        );
    }
    let stored: Value =
        serde_json::from_str(&host.meta.borrow()[crate::error_scan::quiet::QUIET_RELEASE_META_KEY])
            .unwrap();
    let releases = stored["releases"].as_array().unwrap();
    assert_eq!(releases.len(), 2);
    for r in releases {
        assert_eq!(r["items"].as_array().unwrap().len(), 64, "{r}");
        assert_eq!(r["overflow"]["count"], 6, "{r}");
        assert_eq!(r["items"][0]["label"], "first second", "{r}");
    }
}

/// 완료 줄 없이 추적이 끝난 surface 의 묶음도 부모 둘이 각각 모두 받는다. 훅 조회가 되면 받을 부모를 기록하고,
/// 조회가 안 돼 받을 부모를 모르더라도 개수 상한으로 지우지 않는다.
#[test]
fn every_parent_gets_every_release_of_a_surface_that_went_away() {
    let many = 20;
    for hooks_fail in [false, true] {
        let scanner = tracked();
        let host = Host::new();
        *host.parents.borrow_mut() = vec![1, 2];
        for i in 0..many {
            let task = format!("T{i}");
            quiet(
                &scanner,
                &host,
                json!({ "surface": 5, "task": task, "secs": 600, "clear": false }),
            )
            .unwrap();
            hold_one(&scanner, &host, &task);
        }
        host.hooks_fail.set(hooks_fail);
        {
            let mut s = crate::error_scan::lock_scanner(&scanner);
            s.drop_quiet(&host, 5);
            let recipients: Vec<Vec<u32>> = s
                .quiet_state(&host, 5)
                .releases
                .iter()
                .map(|r| r.recipients.clone())
                .collect();
            let expected: Vec<u32> = if hooks_fail { vec![] } else { vec![1, 2] };
            assert_eq!(recipients, vec![expected; many], "hooks_fail {hooks_fail}");
        }
        *host.state.borrow_mut() = "exited";
        crate::notice::test_log::take();
        for caller in [1, 2] {
            crate::notifications::handle_notify_done(
                &scanner,
                &host,
                &json!({ "caller_surface": caller, "target_surface": 5, "command": "tell" }),
                &tr(),
            )
            .unwrap();
        }
        let held: Vec<(u32, usize)> = crate::notice::test_log::take()
            .into_iter()
            .map(|(caller, line)| (caller, line.matches("; held 1 no-output").count()))
            .collect();
        assert_eq!(held, vec![(1, many), (2, many)], "hooks_fail {hooks_fail}");
    }
}
