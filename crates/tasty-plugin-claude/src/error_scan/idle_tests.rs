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
    /// 다음 idle meta 쓰기(set·unset) 몇 번을 실패시킨다.
    meta_fails: RefCell<u32>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl FakeHost {
    fn new(state: &'static str) -> Self {
        Self {
            state: RefCell::new(Ok(state)),
            exists: RefCell::new(true),
            meta: RefCell::new(HashMap::new()),
            meta_fails: RefCell::new(0),
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
        if matches!(method, "surface.meta.set" | "surface.meta.unset")
            && key == IDLE_CHECK_META_KEY
            && *self.meta_fails.borrow() > 0
        {
            *self.meta_fails.borrow_mut() -= 1;
            return Err(tasty_plugin_sdk::PluginError::HostCall {
                method: method.to_string(),
                message: "meta store busy".into(),
                code: None,
            });
        }
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

/// `t` 에 등록한 소유. 원격 비교에 쓰는 Unix ms 는 `ms` 다.
fn owner_at(t: Instant, ms: u64) -> Owner {
    Owner::at(At { instant: t, ms })
}

/// `t` 에 받은 로컬 턴 종료. 활동 순번은 `seq`.
fn turn(t: Instant, seq: u64) -> Finish {
    Finish::Turn {
        at: t,
        seq,
        turn: None,
    }
}

/// 부모 `caller` 의 체크를 `t` 에 따로 등록한다.
fn register<H: HostCall>(
    scanner: &Mutex<ErrorScanner>,
    host: &H,
    caller: u32,
    t: Instant,
) -> Registered {
    lock_scanner(scanner).idle_register(host, TARGET, caller, Origin::Manual, owner_at(t, 0))
}

/// 체크 하나를 t0 에 등록한 스캐너.
fn registered(host: &FakeHost, t0: Instant) -> Mutex<ErrorScanner> {
    let scanner = Mutex::new(ErrorScanner::new());
    register(&scanner, host, PARENT, t0);
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
        register(&scanner, &host, PARENT, t0);
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
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(119), 1));
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
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(119), 1));
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
    let again = register(&scanner, &host, PARENT, t1);
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
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0, 1));
    let r = register(&scanner, &host, PARENT, t0 + secs(1));
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
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(1), 1));
    let r =
        lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Tell, owner_at(t0, 0));
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
    let new = register(&scanner, &host, PARENT, t1);
    assert_eq!(new.replaced, Some(old.id));
    let step = lock_scanner(&scanner).idle_step(&host, old, observation, t1, TEMPLATE);
    assert_eq!(step, Step::Stale, "앞 체크의 만기는 버린다");
    assert!(test_log::take().is_empty());
    // 다시 등록하기 전에 받은 Stop 이 늦게 확정됐다(게이트 시간 초과 등).
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(100), 1));
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
    register(&scanner, &host, 4, t0 + secs(60));
    let lines = tick(&scanner, &host, t0 + secs(120));
    assert_eq!(lines.iter().map(|l| l.0).collect::<Vec<_>>(), vec![PARENT]);
    let lines = tick(&scanner, &host, t0 + secs(180));
    assert_eq!(lines.iter().map(|l| l.0).collect::<Vec<_>>(), vec![4]);
    // 완료는 그 시각 이전에 등록한 모든 부모의 체크를 끝낸다.
    register(&scanner, &host, PARENT, t0 + secs(200));
    register(&scanner, &host, 4, t0 + secs(200));
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(210), 1));
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
    register(&scanner, &host, 4, t0);
    lock_scanner(&scanner).idle_complete(&host, 99, turn(t0, 1));
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

/// 기대 결과 4 의 시간 비교: 관측이 부족한 근거의 active 를 본 시각이 새 기준이다. active 를 본 뒤 비활동으로 바뀌면
/// 그 관측 + 119초까지 0건, + 120초에 1건이다. 근거를 골라 갱신을 건너뛰면 등록 + 120초에 쓰게 된다.
#[test]
fn an_active_with_little_evidence_moves_the_deadline_like_any_active() {
    test_log::take();
    struct EvidenceHost {
        state: RefCell<&'static str>,
        evidence: &'static str,
    }
    impl HostCall for EvidenceHost {
        fn call(&self, method: &str, _: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
            Ok(match method {
                "terminal.state" => json!({
                    "state": *self.state.borrow(),
                    "evidence": self.evidence,
                    "confidence": "low",
                }),
                _ => json!({}),
            })
        }
    }
    for evidence in ["pty_not_started", "observation_unavailable"] {
        let host = EvidenceHost {
            state: RefCell::new("idle"),
            evidence,
        };
        let scanner = Mutex::new(ErrorScanner::new());
        let t0 = Instant::now();
        register(&scanner, &host, PARENT, t0);
        run_idle_checks(&scanner, &host, TEMPLATE, t0 + secs(30));
        *host.state.borrow_mut() = "active";
        run_idle_checks(&scanner, &host, TEMPLATE, t0 + secs(60));
        *host.state.borrow_mut() = "idle";
        for s in [120, 150, 179] {
            run_idle_checks(&scanner, &host, TEMPLATE, t0 + secs(s));
            assert!(test_log::take().is_empty(), "{evidence} t={s}");
        }
        run_idle_checks(&scanner, &host, TEMPLATE, t0 + secs(180));
        assert_eq!(test_log::take().len(), 1, "{evidence}");
    }
}

/// tell 이 정한 완료 문턱을 가진 체크. `state` 는 보내기 전 대상 상태, `seq` 는 그때의 활동 순번이다.
fn tell_check(
    scanner: &Mutex<ErrorScanner>,
    host: &FakeHost,
    t: Instant,
    state: &str,
    seq: u64,
) -> Registered {
    let threshold = completion_gate::threshold(Some(state), seq, false);
    lock_scanner(scanner).idle_register(
        host,
        TARGET,
        PARENT,
        Origin::Tell,
        Owner {
            at: At { instant: t, ms: 0 },
            threshold: Some(threshold),
            running: state == "active",
        },
    )
}

/// 실행 중인 대상에 보낸 tell: 진행 중이던 앞 턴의 Stop 은 이 메시지의 끝이 아니므로 체크가 남는다. 그 뒤 새 활동이
/// 열린 다음 턴의 Stop 이 체크를 끝낸다.
#[test]
fn a_tell_to_a_running_target_waits_for_the_turn_that_handles_the_message() {
    test_log::take();
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t0, "active", 5);
    // 앞 턴이 도구를 더 쓰고(순번 7) 끝났다.
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(10), 7));
    assert_eq!(
        checks(&scanner).len(),
        1,
        "앞 턴의 끝은 이 체크를 끝내지 않는다"
    );
    assert!(
        host.idle_meta().unwrap().contains("\"done_from\":7"),
        "이 끝 뒤의 다음 끝을 기다린다: {:?}",
        host.idle_meta()
    );
    // 메시지를 처리하는 다음 턴(순번 8)이 끝났다.
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(40), 8));
    assert!(checks(&scanner).is_empty());
    assert_eq!(host.idle_meta(), None);
    assert!(tick(&scanner, &host, t0 + secs(500)).is_empty());
}

/// 실행 중인 대상에 보낸 tell 의 prompt-submit 이 앞 턴이 끝나기 전에 온 경우(Claude Code 는 실행 중에 받은 메시지를
/// 줄에 넣을 때 보낸다): 앞 턴의 Stop 과 다음 턴의 Stop 이 같은 순번이다. 두 Stop 은 prompt_id 로 가른다. 앞 턴의
/// Stop 은 체크를 남기고, 다음 턴의 Stop 이 끝낸다. 다음 턴의 Stop 이 오지 않으면 120초 뒤 idle 1건이다.
#[test]
fn a_tell_queued_behind_a_running_turn_ends_at_the_turn_after_it() {
    test_log::take();
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t0, "active", 1);
    // 메시지를 줄에 넣을 때 온 prompt-submit 이 순번을 2로 올린 뒤 앞 턴이 끝났다.
    lock_scanner(&scanner).idle_complete(&host, TARGET, Finish::turn(t0 + secs(1), 2, Some("p1")));
    assert_eq!(
        checks(&scanner).len(),
        1,
        "앞 턴의 끝은 이 체크를 끝내지 않는다"
    );
    // 메시지를 처리한 다음 턴의 Stop 도 순번 2다.
    lock_scanner(&scanner).idle_complete(&host, TARGET, Finish::turn(t0 + secs(2), 2, Some("p2")));
    assert!(checks(&scanner).is_empty(), "다음 턴의 끝이 체크를 끝낸다");
    assert_eq!(host.idle_meta(), None);
    host.set_state("idle");
    assert!(tick(&scanner, &host, t0 + secs(500)).is_empty());

    // 같은 순서에서 다음 턴의 Stop 이 오지 않으면 마지막 active 뒤 120초에 1건이다.
    let scanner = Mutex::new(ErrorScanner::new());
    host.set_state("active");
    tell_check(&scanner, &host, t0, "active", 1);
    assert!(tick(&scanner, &host, t0 + secs(1)).is_empty());
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(1), 2));
    host.set_state("idle");
    assert!(tick(&scanner, &host, t0 + secs(120)).is_empty());
    assert_eq!(tick(&scanner, &host, t0 + secs(121)).len(), 1);
}

/// 앞 턴의 끝으로 건너뛴 확정 완료를 다시 적용해도(같은 수신, 또는 같은 prompt 의 Stop) 다음 턴의 끝으로 세지 않는다.
/// prompt_id 가 없으면 순번이 오른 끝이 다음 턴의 끝이다.
#[test]
fn the_same_finish_applied_again_does_not_end_a_tell_check() {
    test_log::take();
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t0, "active", 1);
    let first = turn(t0 + secs(1), 2);
    lock_scanner(&scanner).idle_complete(&host, TARGET, first);
    lock_scanner(&scanner).idle_complete(&host, TARGET, first);
    assert_eq!(checks(&scanner).len(), 1, "같은 완료의 재적용");
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(2), 3));
    assert!(checks(&scanner).is_empty(), "다음 턴의 끝");

    // 받은 시각이 달라도 같은 prompt 의 Stop 이면 같은 턴이다.
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t0, "active", 1);
    let stop = |t, p| Finish::turn(t, 2, Some(p));
    lock_scanner(&scanner).idle_complete(&host, TARGET, stop(t0 + secs(1), "p1"));
    lock_scanner(&scanner).idle_complete(&host, TARGET, stop(t0 + secs(3), "p1"));
    assert_eq!(checks(&scanner).len(), 1, "같은 prompt 의 Stop 재전달");
    lock_scanner(&scanner).idle_complete(&host, TARGET, stop(t0 + secs(4), "p2"));
    assert!(checks(&scanner).is_empty(), "다른 prompt 의 끝");
}

/// 입력을 기다리던 대상에 보낸 tell: 답은 대기하던 턴을 이어 가므로 그 턴의 Stop 하나가 체크를 끝낸다. 그 뒤 쉬어도
/// idle 줄이 없다.
#[test]
fn a_tell_answering_a_wait_for_input_ends_at_the_single_stop() {
    test_log::take();
    let host = FakeHost::new("needs_input");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t0, "needs_input", 4);
    assert!(host.idle_meta().unwrap().contains("\"twice\":true"));
    assert!(!host.idle_meta().unwrap().contains("running"));
    host.set_state("active");
    assert!(tick(&scanner, &host, t0 + secs(5)).is_empty());
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(10), 5));
    assert!(checks(&scanner).is_empty(), "답을 처리한 턴의 끝");
    host.set_state("idle");
    assert!(tick(&scanner, &host, t0 + secs(200)).is_empty());
    assert!(test_log::take().is_empty());
}

/// 실행 중인 대상에 보낸 tell 의 앞 턴이 끝난 뒤 다음 턴이 시작되지 않거나 멈추면, 마지막 active 뒤 120초에 idle 1건이다.
#[test]
fn a_tell_whose_next_turn_never_ends_writes_one_idle_line() {
    test_log::take();
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t0, "active", 5);
    assert!(tick(&scanner, &host, t0 + secs(10)).is_empty());
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(10), 7));
    host.set_state("idle");
    assert!(tick(&scanner, &host, t0 + secs(129)).is_empty());
    assert_eq!(tick(&scanner, &host, t0 + secs(130)).len(), 1);
    assert!(tick(&scanner, &host, t0 + secs(400)).is_empty());
}

/// 쉬고 있는 대상에 보낸 tell 은 메시지가 연 턴(보내기 전 순번 + 1 이상)의 끝에서만 끝난다. 그 전에 온 앞 턴의 늦은
/// Stop 은 끝내지 않는다.
#[test]
fn a_tell_to_an_idle_target_ends_only_at_the_turn_the_message_opened() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t0, "idle", 5);
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(1), 5));
    assert_eq!(checks(&scanner).len(), 1);
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(20), 6));
    assert!(checks(&scanner).is_empty());
}

/// 시작 복원 경합: 이전 프로세스가 meta 에 남긴 체크의 완료가 되살리기 전에 오면, 복원은 그 등록을 되살리지 않고
/// meta 도 지운다.
#[test]
fn a_finish_before_the_restore_keeps_the_stored_check_from_coming_back() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let _old = registered(&host, t0);
    assert!(host.idle_meta().is_some());
    let restarted = Mutex::new(ErrorScanner::new());
    lock_scanner(&restarted).idle_complete(&host, TARGET, turn(t0 + secs(119), 3));
    assert_eq!(
        lock_scanner(&restarted).idle_restore(&host, TARGET, t0 + secs(120)),
        0
    );
    assert_eq!(host.idle_meta(), None, "끝난 등록은 meta 에서도 지운다");
    assert!(tick(&restarted, &host, t0 + secs(400)).is_empty());
}

/// 되살리기 전에 받았지만 되살린 뒤 적용된 완료(게이트 판정을 기다리다 확정된 Stop 등)도 되살린 체크를 끝낸다.
/// 되살린 체크의 등록 시각은 프로세스 시작 시각이다.
#[test]
fn a_stop_received_before_the_restore_but_confirmed_after_ends_the_restored_check() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let _old = registered(&host, t0);
    let restarted = Mutex::new(ErrorScanner::new());
    lock_scanner(&restarted).idle.begin_restore(t0 + secs(1));
    assert_eq!(
        lock_scanner(&restarted).idle_restore(&host, TARGET, t0 + secs(120)),
        1
    );
    lock_scanner(&restarted).idle_complete(&host, TARGET, turn(t0 + secs(119), 3));
    assert!(checks(&restarted).is_empty());
    assert_eq!(host.idle_meta(), None);
    assert!(tick(&restarted, &host, t0 + secs(400)).is_empty());
}

/// 복원을 마치기 전에는 다른 대상의 완료가 많아도(대상 수 상한) 되살릴 대상의 완료를 버리지 않는다. 복원을 마치면
/// 다시 일반 상한을 따른다.
#[test]
fn finishes_before_the_restore_survive_the_target_cap() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let _old = registered(&host, t0);
    let restarted = Mutex::new(ErrorScanner::new());
    let mut s = lock_scanner(&restarted);
    s.idle.begin_restore(t0 + secs(1));
    s.idle
        .complete(TARGET, turn(t0 + secs(119), 3), t0 + secs(119));
    let later = t0 + secs(119) + Duration::from_millis(1);
    for target in 1000..1000 + HISTORY_TARGETS_MAX as u32 {
        s.idle.complete(target, turn(later, 1), later);
    }
    assert_eq!(s.idle_restore(&host, TARGET, t0 + secs(120)), 0);
    assert_eq!(host.idle_meta(), None);
    s.idle.end_restore();
    for target in 5000..6000 {
        s.idle.complete(target, turn(later, 1), later);
    }
    assert!(s.idle.history_targets() <= HISTORY_TARGETS_MAX);
}

/// 복원이 5분 넘게 늦어져도 복원 전에 받은 완료는 시간 상한으로 버리지 않는다.
#[test]
fn finishes_before_a_late_restore_survive_the_age_limit() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let _old = registered(&host, t0);
    let restarted = Mutex::new(ErrorScanner::new());
    let mut s = lock_scanner(&restarted);
    s.idle.begin_restore(t0 + secs(1));
    s.idle.complete(TARGET, turn(t0 + secs(2), 3), t0 + secs(2));
    s.idle.complete(99, turn(t0 + secs(420), 1), t0 + secs(420));
    assert_eq!(s.idle_restore(&host, TARGET, t0 + secs(421)), 0);
    assert_eq!(host.idle_meta(), None);
}

/// tell 을 보내고 등록하기 전에는 대상당 상한을 넘게 완료가 와도 버리지 않는다. 등록 뒤 보낸 지시의 원격 결과가
/// 먼저 오고 앞 지시의 결과 여덟 개가 늦게 와도 체크는 바로 끝난다.
#[test]
fn finishes_before_a_tell_registers_survive_the_per_target_cap() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    let mut s = lock_scanner(&scanner);
    s.idle.expect(TARGET);
    s.idle.complete(
        TARGET,
        Finish::Remote {
            issued_ms: Some(2000),
        },
        t0,
    );
    for ms in 100..108 {
        s.idle.complete(
            TARGET,
            Finish::Remote {
                issued_ms: Some(ms),
            },
            t0,
        );
    }
    let registered = s.idle_register(&host, TARGET, PARENT, Origin::Tell, owner_at(t0, 1000));
    s.idle.unexpect(TARGET);
    assert!(registered.completed);
}

/// 일반 규칙의 이력(보호 없음)도 등록 전에 온 두 완료를 순서대로 적용한다. 실행 중인 대상에 보낸 tell 의 앞 턴 끝과
/// 메시지 턴의 끝이 모두 등록 전에 왔으면 체크는 바로 끝난다.
#[test]
fn two_finishes_before_a_tell_registers_both_count() {
    let mut idle = IdleChecks::default();
    let t = Instant::now();
    idle.complete(TARGET, turn(t + secs(1), 5), t + secs(1));
    idle.complete(TARGET, turn(t + secs(2), 6), t + secs(2));
    let registered = idle.register(
        TARGET,
        PARENT,
        Origin::Tell,
        Owner {
            at: At { instant: t, ms: 0 },
            threshold: Some(completion_gate::threshold(Some("active"), 5, false)),
            running: true,
        },
    );
    assert!(registered.completed);
}

/// 쓰지 못한 meta 는 회차마다 다시 쓰지만 경고는 대상별 연속 실패 75번마다 한 줄이다. 다시 쓰면 수를 지운다.
#[test]
fn a_failing_meta_write_warns_once_every_75_attempts() {
    #[derive(Clone, Default)]
    struct Buf(std::sync::Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    test_log::take();
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    *host.meta_fails.borrow_mut() = 151;
    register(&scanner, &host, PARENT, t0);
    for round in 1..=150 {
        tick(&scanner, &host, t0 + Duration::from_millis(800 * round));
    }
    assert_eq!(lock_scanner(&scanner).idle_meta_errors(TARGET), 151);
    let warned = |text: &str| {
        text.lines()
            .filter(|l| l.contains("WARN") && l.contains("writing claude-idle-checks failed"))
            .count()
    };
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert_eq!(warned(&text), 3, "1·76·151번째 실패만");
    tick(&scanner, &host, t0 + secs(200));
    assert_eq!(lock_scanner(&scanner).idle_meta_errors(TARGET), 0);
    assert!(host.idle_meta().is_some(), "다시 썼다");
}

/// meta 쓰기 실패: 줄을 쓰기 전에 meta 에서 체크를 먼저 지우고, 지우지 못하면 쓰지 않고 다음 회차에 다시 한다.
/// 쓴 등록은 재시작 뒤 되살아나지 않는다.
#[test]
fn a_delivered_check_does_not_come_back_after_a_failed_meta_write() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    *host.meta_fails.borrow_mut() = 1;
    assert!(
        tick(&scanner, &host, t0 + secs(120)).is_empty(),
        "meta 를 못 지웠으면 쓰지 않는다"
    );
    assert!(host.idle_meta().is_some());
    assert_eq!(tick(&scanner, &host, t0 + secs(121)).len(), 1);
    assert_eq!(host.idle_meta(), None);
    let restarted = Mutex::new(ErrorScanner::new());
    assert_eq!(
        lock_scanner(&restarted).idle_restore(&host, TARGET, t0 + secs(130)),
        0
    );
    assert!(tick(&restarted, &host, t0 + secs(250)).is_empty());
    assert!(tick(&restarted, &host, t0 + secs(1000)).is_empty());
}

/// 완료 경로의 meta 쓰기 실패도 다음 회차에 다시 써서, 재시작 뒤 끝난 체크가 되살아나지 않는다.
#[test]
fn a_finished_check_does_not_come_back_after_a_failed_meta_write() {
    test_log::take();
    let host = FakeHost::new("idle");
    let t0 = Instant::now();
    let scanner = registered(&host, t0);
    *host.meta_fails.borrow_mut() = 1;
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t0 + secs(30), 2));
    assert!(checks(&scanner).is_empty());
    assert!(host.idle_meta().is_some(), "쓰기에 실패해 meta 가 남았다");
    assert!(tick(&scanner, &host, t0 + secs(31)).is_empty());
    assert_eq!(host.idle_meta(), None, "다음 회차에 다시 썼다");
    let restarted = Mutex::new(ErrorScanner::new());
    assert_eq!(
        lock_scanner(&restarted).idle_restore(&host, TARGET, t0 + secs(40)),
        0
    );
    assert!(tick(&restarted, &host, t0 + secs(200)).is_empty());
}

/// 체크가 없는 대상(원격 mirror 등)의 완료 이력은 상한 안에서만 남는다.
#[test]
fn finished_turns_of_targets_without_checks_do_not_pile_up() {
    let mut idle = IdleChecks::default();
    let now = Instant::now();
    for target in 1..=1000 {
        idle.complete(target, turn(now, 1), now);
    }
    assert!(
        idle.history_targets() <= HISTORY_TARGETS_MAX,
        "{}",
        idle.history_targets()
    );
    for target in 2000..2010 {
        idle.complete(target, Finish::RemoteExit, now);
    }
    assert!(idle.history_targets() <= HISTORY_TARGETS_MAX);
}

/// 원격 결과는 그 결과를 낳은 지시를 체크 등록 뒤에 보냈을 때만 체크를 끝낸다. 종료는 모두 끝낸다.
#[test]
fn a_remote_result_ends_only_checks_registered_before_its_instruction() {
    test_log::take();
    let host = FakeHost::new("active");
    let t0 = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    lock_scanner(&scanner).idle_register(&host, TARGET, PARENT, Origin::Tell, owner_at(t0, 1_000));
    // 다시 건 체크(t=130).
    lock_scanner(&scanner).idle_register(
        &host,
        TARGET,
        PARENT,
        Origin::Manual,
        owner_at(t0 + secs(130), 131_000),
    );
    lock_scanner(&scanner).idle_complete(
        &host,
        TARGET,
        Finish::Remote {
            issued_ms: Some(1_000),
        },
    );
    assert_eq!(
        checks(&scanner).len(),
        1,
        "앞 지시의 결과는 새 체크를 끝내지 않는다"
    );
    lock_scanner(&scanner).idle_complete(&host, TARGET, Finish::Remote { issued_ms: None });
    assert_eq!(
        checks(&scanner).len(),
        1,
        "지시를 모르는 결과도 끝내지 않는다"
    );
    lock_scanner(&scanner).idle_complete(
        &host,
        TARGET,
        Finish::Remote {
            issued_ms: Some(140_000),
        },
    );
    assert!(checks(&scanner).is_empty());
    register(&scanner, &host, PARENT, t0 + secs(200));
    lock_scanner(&scanner).idle_complete(&host, TARGET, Finish::RemoteExit);
    assert!(checks(&scanner).is_empty());
}

/// 건너뛴 앞 턴의 끝은 meta 에 남아 정상 재시작 뒤에도 되살아난다. 재시작 뒤 같은 턴의 Stop 을 다시 받아도 체크가
/// 남고, 다른 턴의 Stop 이 끝낸다.
#[test]
fn a_skipped_finish_survives_a_restart() {
    test_log::take();
    let host = FakeHost::new("active");
    let t = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t, "active", 1);
    lock_scanner(&scanner).idle_complete(&host, TARGET, Finish::turn(t + secs(1), 2, Some("old")));
    assert_eq!(checks(&scanner).len(), 1, "앞 턴의 끝은 건너뛴다");
    let meta = host.idle_meta().unwrap();
    assert!(
        meta.contains("\"skipped_seq\":2"),
        "건너뛴 끝을 남긴다: {meta}"
    );
    assert!(
        meta.contains("\"skipped_turn\":"),
        "그 턴의 식별자도 남긴다: {meta}"
    );

    let restarted = Mutex::new(ErrorScanner::new());
    lock_scanner(&restarted).idle.begin_restore(t + secs(2));
    assert_eq!(
        lock_scanner(&restarted).idle_restore(&host, TARGET, t + secs(3)),
        1
    );
    lock_scanner(&restarted).idle.end_restore();
    lock_scanner(&restarted).idle_complete(
        &host,
        TARGET,
        Finish::turn(t + secs(4), 2, Some("old")),
    );
    assert_eq!(
        checks(&restarted).len(),
        1,
        "건너뛴 턴의 Stop 재수신은 체크를 끝내지 않는다"
    );
    assert!(
        host.idle_meta().unwrap().contains("\"skipped_seq\":2"),
        "되살린 건너뛴 끝을 다시 남긴다: {:?}",
        host.idle_meta()
    );
    lock_scanner(&restarted).idle_complete(
        &host,
        TARGET,
        Finish::turn(t + secs(5), 2, Some("new")),
    );
    assert!(checks(&restarted).is_empty(), "다른 턴의 Stop 이 끝낸다");
    assert_eq!(host.idle_meta(), None);
}

/// prompt_id 가 없는 Stop 은 활동 순번으로 턴을 가른다. 실행 중인 대상에 보낸 tell 의 다음 턴이 순번을 더 올리지 못하고
/// prompt_id 없이 끝나면 건너뛴 앞 턴의 끝과 구별할 수 없어 체크가 남는다. 거짓 해제 대신 마지막 active 에서 120초 뒤
/// idle 줄 하나를 쓴다.
#[test]
fn a_next_turn_stop_without_prompt_at_the_same_sequence_keeps_the_check() {
    test_log::take();
    let host = FakeHost::new("active");
    let t = Instant::now();
    let scanner = Mutex::new(ErrorScanner::new());
    tell_check(&scanner, &host, t, "active", 1);
    // 앞 턴의 끝(줄에 넣은 메시지의 prompt-submit 이 순번을 2로 올린 뒤).
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t + secs(1), 2));
    assert_eq!(checks(&scanner).len(), 1, "앞 턴의 끝은 건너뛴다");
    // 메시지를 처리한 다음 턴이 순번을 올리지 못하고 끝났다.
    assert!(tick(&scanner, &host, t + secs(30)).is_empty());
    host.set_state("idle");
    lock_scanner(&scanner).idle_complete(&host, TARGET, turn(t + secs(31), 2));
    assert_eq!(checks(&scanner).len(), 1, "같은 순번의 끝은 끝내지 않는다");
    assert!(tick(&scanner, &host, t + secs(149)).is_empty());
    assert_eq!(
        tick(&scanner, &host, t + secs(150)).len(),
        1,
        "마지막 active 에서 120초 뒤 1줄"
    );
    assert!(tick(&scanner, &host, t + secs(400)).is_empty());
}
