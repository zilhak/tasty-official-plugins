//! idle 체크의 시간·완료·재등록·실패 처리 시험. 가짜 호스트와 직접 정한 시각으로 판정한다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use super::*;
use crate::error_scan::{ErrorScanner, ScanTarget, lock_scanner};
use crate::notice::test_log;

const TARGET: u32 = 7;
const PARENT: u32 = 3;
const TEMPLATE: &str = "surface {0}: idle {1}s ({2})";

/// 상태 조회 결과를 시험이 바꾸는 호스트. meta 는 실제처럼 저장하고, 모든 호출을 기록한다.
struct FakeHost {
    /// `Ok(state)` 또는 조회 실패.
    state: RefCell<Result<&'static str, &'static str>>,
    /// 조회 실패 뒤 `surface.locate` 의 `exists`.
    exists: RefCell<bool>,
    meta: RefCell<HashMap<(u32, String), String>>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl FakeHost {
    fn new(state: &'static str) -> Self {
        Self {
            state: RefCell::new(Ok(state)),
            exists: RefCell::new(true),
            meta: RefCell::new(HashMap::new()),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn set_state(&self, state: &'static str) {
        *self.state.borrow_mut() = Ok(state);
    }

    fn fail_state(&self) {
        *self.state.borrow_mut() = Err("ipc timeout");
    }

    fn sent_input(&self) -> bool {
        self.calls
            .borrow()
            .iter()
            .any(|(m, _)| m == "terminal.tell" || m == "surface.send")
    }

    fn idle_meta(&self) -> Option<String> {
        self.meta
            .borrow()
            .get(&(TARGET, IDLE_CHECK_META_KEY.to_string()))
            .cloned()
    }
}

impl HostCall for FakeHost {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.calls
            .borrow_mut()
            .push((method.to_string(), params.clone()));
        let sid = params
            .get("surface_id")
            .or_else(|| params.get("surface"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let key = params
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(match method {
            "terminal.state" => match *self.state.borrow() {
                Ok(state) => json!({ "state": state, "evidence": "test" }),
                Err(e) => {
                    return Err(tasty_plugin_sdk::PluginError::HostCall {
                        method: method.to_string(),
                        message: e.to_string(),
                        code: None,
                    });
                }
            },
            "surface.locate" => json!({ "exists": *self.exists.borrow() }),
            "surface.meta.set" => {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                self.meta.borrow_mut().insert((sid, key), value);
                json!({})
            }
            "surface.meta.unset" => {
                self.meta.borrow_mut().remove(&(sid, key));
                json!({})
            }
            "surface.meta.get" => match self.meta.borrow().get(&(sid, key)) {
                Some(v) => json!({ "value": v }),
                None => json!({ "value": null }),
            },
            _ => json!({}),
        })
    }
}

/// 체크 하나를 t0 에 등록한 스캐너.
fn registered(host: &FakeHost, t0: Instant) -> Mutex<ErrorScanner> {
    let scanner = Mutex::new(ErrorScanner::new());
    lock_scanner(&scanner).idle_register(host, TARGET, PARENT, Origin::Manual, t0);
    scanner
}

/// 한 감시 회차를 `at` 시각으로 돌리고 이번 회차에 부모 로그에 쓴 줄을 돌려준다.
fn tick(scanner: &Mutex<ErrorScanner>, host: &FakeHost, at: Instant) -> Vec<(u32, String)> {
    run_idle_checks(scanner, host, TEMPLATE, at);
    test_log::take()
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn checks(scanner: &Mutex<ErrorScanner>) -> Vec<(u32, u64)> {
    lock_scanner(scanner).idle.of_target(TARGET)
}

/// 기대 결과 1: 메시지 없이 체크만 등록하고 active·완료가 없으면 119초까지 0건, 120초 뒤 첫 회차에 1건이다.
/// 등록은 대상에 입력을 보내지 않는다.
#[test]
fn a_check_alone_fires_once_at_120_seconds_and_not_at_119() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    assert!(!host.sent_input(), "등록은 입력을 보내지 않는다");
    for s in [1, 60, 119] {
        assert!(tick(&scanner, &host, t0 + secs(s)).is_empty(), "t={s}");
    }
    let lines = tick(&scanner, &host, t0 + secs(120));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].0, PARENT);
    assert_eq!(lines[0].1, "[warning] surface 7: idle 120s (idle)");
    assert!(checks(&scanner).is_empty(), "한 번 알리면 체크가 끝난다");
    assert_eq!(host.idle_meta(), None, "끝난 체크는 복원 근거도 지운다");
    assert!(!host.sent_input());
}

/// 기대 결과 2: t=90 에 active 를 본 뒤로 active·완료가 없으면 209초까지 0건, 210초 뒤 첫 회차에 1건이다.
#[test]
fn an_active_state_at_90_seconds_moves_the_deadline_to_210() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    assert!(tick(&scanner, &host, t0 + secs(30)).is_empty());
    host.set_state("active");
    assert!(tick(&scanner, &host, t0 + secs(90)).is_empty());
    host.set_state("idle");
    for s in [120, 150, 209] {
        assert!(tick(&scanner, &host, t0 + secs(s)).is_empty(), "t={s}");
    }
    assert_eq!(tick(&scanner, &host, t0 + secs(210)).len(), 1);
}

/// 기대 결과 3: 120초를 넘게 계속 active 면 0건이고, 마지막 active 뒤에 다시 120초를 기다린다.
#[test]
fn a_target_that_stays_active_never_fires_until_120_seconds_after_the_last_active() {
    test_log::take();
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    for s in (0..=600).step_by(20) {
        assert!(tick(&scanner, &host, t0 + secs(s)).is_empty(), "t={s}");
    }
    host.set_state("stale");
    assert!(tick(&scanner, &host, t0 + secs(719)).is_empty());
    assert_eq!(tick(&scanner, &host, t0 + secs(720)).len(), 1);
}

/// 기대 결과 4: 관측이 부족한 근거(pty_not_started·observation_unavailable)의 active 도 시간을 되돌린다.
/// 원격 mirror 의 active 도 같다. 근거를 골라 빼지 않는다.
#[test]
fn every_active_resets_the_timer_whatever_its_evidence() {
    test_log::take();
    struct EvidenceHost {
        evidence: &'static str,
    }
    impl HostCall for EvidenceHost {
        fn call(&self, method: &str, _: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
            Ok(match method {
                "terminal.state" => json!({
                    "state": "active",
                    "evidence": self.evidence,
                    "confidence": "low",
                    "remote_child": true,
                }),
                _ => json!({}),
            })
        }
    }
    for evidence in ["pty_not_started", "observation_unavailable"] {
        let host = EvidenceHost { evidence };
        let scanner = Mutex::new(ErrorScanner::new());
        let t0 = Instant::now();
        lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Manual, t0);
        for s in (0..=900).step_by(30) {
            run_idle_checks(&scanner, &host, TEMPLATE, t0 + secs(s));
        }
        assert!(test_log::take().is_empty(), "{evidence}");
        assert_eq!(checks(&scanner).len(), 1, "{evidence}: 체크는 남는다");
    }
}

/// 기대 결과 5: t=119 에 최종 완료가 확정되면 체크와 복원 근거가 지워지고, 이후 상태 변화·플러그인 재시작에도
/// 그 체크의 idle 은 0건이다.
#[test]
fn a_finished_turn_at_119_seconds_ends_the_check_for_good() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    assert!(host.idle_meta().is_some(), "등록은 복원 근거를 남긴다");
    lock_scanner(&scanner).idle_complete(&host, TARGET, t0 + secs(119));
    assert!(checks(&scanner).is_empty());
    assert_eq!(host.idle_meta(), None);
    for s in [120, 300, 1000] {
        host.set_state(if s == 300 { "active" } else { "stale" });
        assert!(tick(&scanner, &host, t0 + secs(s)).is_empty(), "t={s}");
    }
    // 플러그인 재시작: 새 스캐너가 meta 에서 되살릴 것이 없다.
    let restarted = Mutex::new(ErrorScanner::new());
    assert_eq!(
        lock_scanner(&restarted).idle_restore(&host, TARGET, t0 + secs(1000)),
        0
    );
    assert!(tick(&restarted, &host, t0 + secs(2000)).is_empty());
}

/// 만기 판정과 완료가 겹치면 먼저 잠금을 잡은 쪽이 끝낸다. 완료가 먼저면 이미 잡은 만기 판정은 버린다.
#[test]
fn an_expiry_probe_taken_before_a_finished_turn_is_dropped() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    let probe = lock_scanner(&scanner).idle.probes()[0];
    let observation = observe(&host, TARGET);
    lock_scanner(&scanner).idle_complete(&host, TARGET, t0 + secs(119));
    let step =
        lock_scanner(&scanner).idle_step(&host, probe, observation, t0 + secs(121), TEMPLATE);
    assert_eq!(step, Step::Stale);
    assert!(test_log::take().is_empty());
}

/// 기대 결과 8: idle 1건 뒤 상태·출력이 그대로여도 새 등록 전에는 추가 idle 이 없다.
#[test]
fn one_check_writes_one_line() {
    test_log::take();
    let host = FakeHost::new("stale");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    assert_eq!(tick(&scanner, &host, t0 + secs(120)).len(), 1);
    for s in [121, 300, 600, 3600] {
        assert!(tick(&scanner, &host, t0 + secs(s)).is_empty(), "t={s}");
    }
}

/// 기대 결과 9: idle 을 받은 부모가 메시지 없이 다시 등록하면 그 시각부터 120초를 센다. 앞 알림 때문에 밀리지 않는다.
#[test]
fn registering_again_after_a_line_starts_a_fresh_120_seconds() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    assert_eq!(tick(&scanner, &host, t0 + secs(120)).len(), 1);
    let t1 = t0 + secs(125);
    let again = lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Manual, t1);
    assert!(!again.completed);
    assert_eq!(again.replaced, None, "앞 체크는 이미 끝났다");
    assert!(tick(&scanner, &host, t1 + secs(119)).is_empty());
    assert_eq!(tick(&scanner, &host, t1 + secs(120)).len(), 1);
}

/// 과거 완료 이력이 있는 surface 에도 새로 등록할 수 있다. 등록 전의 완료는 새 체크를 끝내지 않는다.
#[test]
fn a_finished_turn_before_registering_does_not_end_the_new_check() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    lock_scanner(&scanner).idle_complete(&host, TARGET, t0);
    let r =
        lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Manual, t0 + secs(1));
    assert!(!r.completed);
    assert_eq!(tick(&scanner, &host, t0 + secs(121)).len(), 1);
}

/// tell 은 보내기 전 시각으로 등록한다. 보낸 뒤 등록 처리 전에 확정된 완료는 새 체크를 바로 끝낸다.
#[test]
fn a_finish_between_sending_and_registering_ends_the_check_at_once() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    lock_scanner(&scanner).idle_complete(&host, TARGET, t0 + secs(1));
    let r = lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Tell, t0);
    assert!(r.completed);
    assert!(checks(&scanner).is_empty());
    assert_eq!(host.idle_meta(), None);
    assert!(tick(&scanner, &host, t0 + secs(500)).is_empty());
}

/// 기대 결과 10: 앞 체크의 늦은 만료 판정과 늦게 확정된 앞 완료가 다시 등록한 체크를 끝내거나 줄을 더 만들지 않는다.
#[test]
fn late_events_of_an_earlier_check_leave_the_new_one_alone() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    // 앞 체크의 만기 판정이 잠금 밖에서 진행 중이다.
    let old = lock_scanner(&scanner).idle.probes()[0];
    let observation = observe(&host, TARGET);
    // 그 사이 부모가 다시 등록했다.
    let t1 = t0 + secs(130);
    let new = lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Manual, t1);
    assert_eq!(new.replaced, Some(old.id));
    let step = lock_scanner(&scanner).idle_step(&host, old, observation, t1, TEMPLATE);
    assert_eq!(step, Step::Stale, "앞 체크의 만기는 버린다");
    assert!(test_log::take().is_empty());
    // 다시 등록하기 전에 받은 Stop 이 늦게 확정됐다(게이트 시간 초과 등).
    lock_scanner(&scanner).idle_complete(&host, TARGET, t0 + secs(100));
    assert_eq!(checks(&scanner), vec![(PARENT, new.id)], "새 체크는 남는다");
    assert!(tick(&scanner, &host, t1 + secs(119)).is_empty());
    assert_eq!(
        tick(&scanner, &host, t1 + secs(120)).len(),
        1,
        "새 체크는 한 번 알린다"
    );
    assert!(tick(&scanner, &host, t1 + secs(400)).is_empty());
}

/// 같은 대상을 여러 부모가 감시하면 부모마다 따로 센다. 한 부모의 재등록은 다른 부모의 체크를 바꾸지 않는다.
#[test]
fn each_parent_owns_its_own_check() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    lock_scanner(&scanner).idle_register(&host, TARGET, 4, Origin::Manual, t0 + secs(60));
    let lines = tick(&scanner, &host, t0 + secs(120));
    assert_eq!(lines.iter().map(|l| l.0).collect::<Vec<_>>(), vec![PARENT]);
    let lines = tick(&scanner, &host, t0 + secs(180));
    assert_eq!(lines.iter().map(|l| l.0).collect::<Vec<_>>(), vec![4]);
    // 완료는 그 시각 이전에 등록한 모든 부모의 체크를 끝낸다.
    lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Manual, t0 + secs(200));
    lock_scanner(&scanner).idle_register(&host, TARGET, 4, Origin::Manual, t0 + secs(200));
    lock_scanner(&scanner).idle_complete(&host, TARGET, t0 + secs(210));
    assert!(checks(&scanner).is_empty());
}

/// 기대 결과 11(상태 조회 실패): 조회 실패는 비활동으로 세지 않는다. 실패가 이어지는 동안은 쓰지 않고, 조회가
/// 다시 되면 그 결과로 판정한다.
#[test]
fn a_failed_state_read_is_not_counted_as_inactivity() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    host.fail_state();
    for s in [120, 200, 400] {
        assert!(tick(&scanner, &host, t0 + secs(s)).is_empty(), "t={s}");
    }
    assert_eq!(
        checks(&scanner).len(),
        1,
        "조회 실패로 체크를 지우지 않는다"
    );
    host.set_state("active");
    assert!(tick(&scanner, &host, t0 + secs(401)).is_empty());
    host.set_state("idle");
    assert!(tick(&scanner, &host, t0 + secs(520)).is_empty());
    assert_eq!(tick(&scanner, &host, t0 + secs(521)).len(), 1);
}

/// 기대 결과 11(부모 로그 실패): 쓰기 실패는 전달로 확정하지 않는다. 체크를 남기고 30초 뒤 다시 쓴다.
/// 성공한 뒤에는 더 쓰지 않는다. 다시 쓰기 전에 active 가 오면 기한을 처음부터 센다.
#[test]
fn a_failed_parent_log_write_is_retried_and_then_written_once() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    test_log::fail_next(2);
    assert!(tick(&scanner, &host, t0 + secs(120)).is_empty());
    assert_eq!(checks(&scanner).len(), 1, "실패한 체크는 남는다");
    assert!(host.idle_meta().is_some());
    assert!(
        tick(&scanner, &host, t0 + secs(149)).is_empty(),
        "30초 안에는 다시 쓰지 않는다"
    );
    assert!(
        tick(&scanner, &host, t0 + secs(150)).is_empty(),
        "두 번째 실패"
    );
    let lines = tick(&scanner, &host, t0 + secs(180));
    assert_eq!(lines.len(), 1, "세 번째에 쓴다");
    assert!(checks(&scanner).is_empty());
    assert!(tick(&scanner, &host, t0 + secs(400)).is_empty());

    // 실패 뒤 active 가 오면 다시 기다린다.
    let scanner = registered(&host, t0);
    test_log::fail_next(1);
    assert!(tick(&scanner, &host, t0 + secs(120)).is_empty());
    host.set_state("active");
    assert!(tick(&scanner, &host, t0 + secs(140)).is_empty());
    host.set_state("idle");
    assert!(tick(&scanner, &host, t0 + secs(259)).is_empty());
    assert_eq!(tick(&scanner, &host, t0 + secs(260)).len(), 1);
}

/// 대상이 끝나거나(exited) 사라지면 체크를 남기지 않고 줄도 쓰지 않는다.
#[test]
fn an_exited_or_missing_target_drops_the_check_without_a_line() {
    test_log::take();
    let host = FakeHost::new("exited");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    assert!(tick(&scanner, &host, t0 + secs(200)).is_empty());
    assert!(checks(&scanner).is_empty());
    assert_eq!(host.idle_meta(), None);

    let host = FakeHost::new("idle");
    let scanner = registered(&host, t0);
    host.fail_state();
    *host.exists.borrow_mut() = false;
    assert!(tick(&scanner, &host, t0 + secs(200)).is_empty());
    assert!(checks(&scanner).is_empty());
}

/// 추적을 끝내는 경로(kill·release)는 그 대상의 체크도 지운다.
#[test]
fn stopping_the_tracking_drops_the_checks() {
    let host = FakeHost::new("idle");
    let scanner = registered(&host, Instant::now());
    lock_scanner(&scanner).enable(TARGET, ScanTarget::Child);
    lock_scanner(&scanner).stop_tracking(&host, TARGET);
    assert!(checks(&scanner).is_empty());
    assert_eq!(host.idle_meta(), None);
}

/// 백그라운드 작업 대기와 surface 전체 무출력 선언 동안은 시간을 세지 않는다. 그 조용함은 기존 무출력 알림 규칙이 맡는다.
#[test]
fn a_background_wait_or_a_surface_quiet_declaration_pauses_the_check() {
    test_log::take();
    let host = FakeHost::new("stale");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    lock_scanner(&scanner).mark_background_wait(TARGET, 1);
    assert!(tick(&scanner, &host, t0 + secs(600)).is_empty());
    lock_scanner(&scanner).clear_background_wait(TARGET);
    assert!(tick(&scanner, &host, t0 + secs(719)).is_empty());
    assert_eq!(tick(&scanner, &host, t0 + secs(720)).len(), 1);

    let scanner = registered(&host, t0);
    lock_scanner(&scanner).enable(TARGET, ScanTarget::Child);
    lock_scanner(&scanner).declare_quiet(&host, TARGET, None, -1);
    assert!(tick(&scanner, &host, t0 + secs(600)).is_empty());
    lock_scanner(&scanner).clear_quiet(&host, TARGET, None);
    assert!(tick(&scanner, &host, t0 + secs(719)).is_empty());
    assert_eq!(tick(&scanner, &host, t0 + secs(720)).len(), 1);

    // 작업 하나에 건 선언은 체크를 멈추지 않는다.
    let scanner = registered(&host, t0);
    lock_scanner(&scanner).enable(TARGET, ScanTarget::Child);
    lock_scanner(&scanner).declare_quiet(&host, TARGET, Some("task-1".into()), -1);
    assert_eq!(tick(&scanner, &host, t0 + secs(120)).len(), 1);
}

/// 재시작 복원: meta 에 남은 등록만 되살리고, 되살린 시각부터 120초를 다시 센다. 옛 형식이 아닌 meta 는 건너뛴다.
#[test]
fn a_restart_restores_only_the_live_registrations() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    lock_scanner(&scanner).idle_register(&host, TARGET, 4, Origin::Tell, t0);
    lock_scanner(&scanner).idle_complete(&host, 99, t0);
    // 부모 4 의 체크는 알림으로 끝났다고 하자.
    {
        let mut s = lock_scanner(&scanner);
        let probe = s.idle.probes().into_iter().find(|p| p.caller == 4).unwrap();
        s.idle.checks.remove(&(probe.target, probe.caller));
        s.save_idle(&host, TARGET);
    }
    let restarted = Mutex::new(ErrorScanner::new());
    let t1 = t0 + secs(500);
    assert_eq!(lock_scanner(&restarted).idle_restore(&host, TARGET, t1), 1);
    let restored = checks(&restarted);
    assert_eq!(
        restored.iter().map(|c| c.0).collect::<Vec<_>>(),
        vec![PARENT]
    );
    assert!(tick(&restarted, &host, t1 + secs(119)).is_empty());
    assert_eq!(tick(&restarted, &host, t1 + secs(120)).len(), 1);

    assert!(parse_meta("not json").is_empty());
    assert!(parse_meta(r#"{"checks":[{"caller":"x"}]}"#).is_empty());
}

/// 원격 자식의 턴 결과가 기록되면 그 자식의 체크를 끝낸다. 재전송과 거절은 끝내지 않는다.
#[test]
fn a_recorded_remote_turn_result_ends_the_check() {
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let params = |status: &str| {
        json!({
            "surface_id": TARGET,
            "payload": { "completion": { "status": status, "completion_key": "k" } },
        })
    };
    for (status, response, ended) in [
        ("turn_ended", "recorded", true),
        ("turn_failed", "recorded", true),
        ("exited", "recorded", true),
        ("turn_ended", "duplicate", false),
        ("turn_ended", "rejected", false),
    ] {
        let scanner = registered(&host, t0);
        crate::idle_check::after_remote_receive(
            &scanner,
            &host,
            &params(status),
            &json!({ "status": response }),
            t0 + secs(1),
        );
        assert_eq!(checks(&scanner).is_empty(), ended, "{status}/{response}");
    }
}
