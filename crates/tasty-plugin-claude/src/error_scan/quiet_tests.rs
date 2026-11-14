use super::*;
use crate::error_scan::{NO_OUTPUT_NOTICE_DEFAULT, STALL_KIND_ERROR, STALLED_EVENT};
use crate::task_watch::{FileActivity, TaskWatch, WatchedTask};
use std::cell::RefCell;
use std::time::{Duration, Instant};

const NOW: u64 = 10_000_000;

fn decl(task: Option<&str>, secs: i64) -> Decl {
    Decl {
        task: task.map(String::from),
        secs,
        set_at_ms: NOW,
        until_ms: (secs > 0).then(|| NOW + secs as u64 * 1000),
        held: Vec::new(),
        overflow: None,
    }
}

fn state(decls: Vec<Decl>) -> QuietState {
    QuietState {
        decls,
        releases: Vec::new(),
    }
}

fn tasks(ids: &[&str]) -> QuietNotice {
    QuietNotice::Tasks(
        ids.iter()
            .map(|id| (id.to_string(), format!("cmd {id}")))
            .collect(),
    )
}

/// 선언(작업 id)마다 쌓인 항목의 (키, 횟수).
type HeldCounts = Vec<(Option<String>, Vec<(String, u32)>)>;

fn held_counts(s: &QuietState) -> HeldCounts {
    s.decls
        .iter()
        .map(|d| {
            (
                d.task.clone(),
                d.held.iter().map(|h| (h.key.clone(), h.count)).collect(),
            )
        })
        .collect()
}

/// 작업 선언은 그 작업만, surface 전체 선언은 나머지 모두를 맡는다. 맡지 않은 작업만 바로 보낸다.
#[test]
fn a_task_declaration_holds_its_task_and_lets_the_others_through() {
    let mut s = state(vec![decl(Some("T"), 600)]);
    assert_eq!(
        s.dispose(tasks(&["T", "U"]), 700_000, NOW + 1),
        Send::Labels(vec!["cmd U".into()])
    );
    assert_eq!(s.dispose(tasks(&["U"]), 1, NOW + 2), Send::All);
    assert_eq!(s.dispose(tasks(&["T"]), 1, NOW + 3), Send::Nothing);
    assert_eq!(
        held_counts(&s),
        vec![(Some("T".into()), vec![("T".into(), 2)])]
    );

    let mut s = state(vec![decl(Some("T"), 600), decl(None, 600)]);
    assert_eq!(s.dispose(tasks(&["T", "U"]), 1, NOW), Send::Nothing);
    assert_eq!(
        held_counts(&s),
        vec![
            (Some("T".into()), vec![("T".into(), 1)]),
            (None, vec![("U".into(), 1)]),
        ]
    );
}

/// 끝 시각이 지난 양수 선언은 더 이상 보류하지 않는다. `-1` 은 시간이 얼마나 지나도 버린다.
#[test]
fn an_expired_declaration_no_longer_holds_and_minus_one_discards_forever() {
    let mut s = state(vec![decl(Some("T"), 60)]);
    assert_eq!(s.dispose(tasks(&["T"]), 1, NOW + 60_000), Send::All);
    let mut s = state(vec![decl(None, -1)]);
    assert_eq!(s.dispose(tasks(&["T"]), 1, u64::MAX - 1), Send::Nothing);
    assert_eq!(s.dispose(QuietNotice::Turn, 1, NOW), Send::Nothing);
    assert!(s.decls[0].held.is_empty(), "-1 은 쌓지 않는다");
}

/// 대기 문구는 surface 선언이 맡거나, 기다리는 작업 모두가 작업 선언에 걸려야 보류한다. 턴 정지는 surface 선언만 맡는다.
#[test]
fn a_wait_or_turn_notice_is_held_only_when_fully_covered() {
    let wait = |ids: &[&str]| QuietNotice::Wait {
        ids: ids.iter().map(|s| s.to_string()).collect(),
        label: "shell".into(),
    };
    let mut s = state(vec![decl(Some("T"), 600)]);
    assert_eq!(s.dispose(wait(&["T", "U"]), 1, NOW), Send::All);
    assert_eq!(s.dispose(wait(&[]), 1, NOW), Send::All);
    assert_eq!(s.dispose(QuietNotice::Turn, 1, NOW), Send::All);
    assert_eq!(s.dispose(wait(&["T"]), 1, NOW), Send::Nothing);
    let mut s = state(vec![decl(None, 600)]);
    assert_eq!(s.dispose(wait(&[]), 1, NOW), Send::Nothing);
    assert_eq!(s.dispose(QuietNotice::Turn, 1, NOW), Send::Nothing);
    assert_eq!(
        held_counts(&s),
        vec![(None, vec![("wait:".into(), 1), ("turn".into(), 1)])]
    );
}

/// 같은 대상의 반복은 한 항목으로 합쳐 횟수·첫·마지막 시각·가장 긴 조용한 시간을 남긴다.
#[test]
fn repeats_of_one_task_merge_into_one_item() {
    let mut s = state(vec![decl(Some("T"), 6000)]);
    s.dispose(tasks(&["T"]), 600_000, NOW + 600_000);
    s.dispose(tasks(&["T"]), 3_540_000, NOW + 3_540_000);
    let h = &s.decls[0].held[0];
    assert_eq!(
        (h.count, h.first_ms, h.last_ms, h.quiet_ms),
        (2, NOW + 600_000, NOW + 3_540_000, 3_540_000)
    );
}

/// 재선언: 양수→양수는 쌓인 것을 두고 끝 시각만 바꾼다. 양수→-1 은 묶음을 낸다. -1→양수는 새로 쌓는다.
#[test]
fn redeclaring_follows_the_three_rules() {
    let mut s = state(Vec::new());
    assert!(s.declare(Some("T".into()), 60, NOW).is_empty());
    s.dispose(tasks(&["T"]), 1, NOW + 1);
    assert!(s.declare(Some("T".into()), 600, NOW + 10).is_empty());
    assert_eq!(s.decls.len(), 1);
    assert_eq!(s.decls[0].until_ms, Some(NOW + 10 + 600_000));
    assert_eq!(
        s.decls[0].set_at_ms, NOW,
        "연장은 시작 시각을 바꾸지 않는다"
    );
    assert_eq!(s.decls[0].held.len(), 1, "연장은 쌓인 것을 둔다");

    let released = s.declare(Some("T".into()), -1, NOW + 20);
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].reason, EndReason::Replaced);
    assert_eq!(released[0].items.len(), 1);
    assert_eq!((s.decls[0].secs, s.decls[0].until_ms), (-1, None));

    assert!(s.declare(Some("T".into()), 60, NOW + 30).is_empty());
    assert!(s.decls[0].held.is_empty());
}

/// 끝난 선언은 양수이고 쌓인 것이 있을 때만 묶음을 낸다.
#[test]
fn only_a_holding_declaration_with_items_makes_a_release() {
    let mut s = state(vec![
        decl(Some("A"), 60),
        decl(Some("B"), 60),
        decl(None, -1),
    ]);
    s.dispose(tasks(&["A"]), 1, NOW);
    s.dispose(QuietNotice::Turn, 1, NOW);
    let (count, released) = s.end(|_| true, EndReason::Cleared, NOW + 5);
    assert_eq!(count, 3);
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].task.as_deref(), Some("A"));
    assert!(s.decls.is_empty());
}

/// 묶음은 부모마다 한 번씩 받는다. 추적이 끝난 surface 의 묶음은 error 완료 줄만 받는다.
#[test]
fn a_release_is_taken_once_per_parent() {
    let mut s = state(vec![decl(None, 60)]);
    s.dispose(QuietNotice::Turn, 1, NOW);
    let (_, released) = s.end(|_| true, EndReason::Expired, NOW);
    s.releases = released;
    assert_eq!(s.take_for(1, false).len(), 1);
    assert_eq!(s.take_for(1, true).len(), 0);
    assert_eq!(s.take_for(2, false).len(), 1);

    let mut s = state(vec![decl(None, 60)]);
    s.dispose(QuietNotice::Turn, 1, NOW);
    let (_, released) = s.end(|_| true, EndReason::SurfaceGone, NOW);
    s.releases = released;
    assert!(s.take_for(1, false).is_empty());
    assert_eq!(s.take_for(1, true).len(), 1);
    s.prune(NOW + ORPHAN_KEPT_MS);
    assert!(s.releases.is_empty(), "error 줄이 오지 않으면 버린다");
}

/// meta 에 쓴 값을 다시 읽으면 같은 선언·묶음이다.
#[test]
fn declarations_and_releases_survive_their_meta() {
    let mut d = decl(Some("T"), 60);
    d.hold(Held {
        key: "T".into(),
        kind: HeldKind::Task,
        label: "sleep 600".into(),
        count: 2,
        first_ms: 1,
        last_ms: 2,
        quiet_ms: 3,
    });
    let raw = json!({ "decls": [d.to_json(), decl(None, -1).to_json()] }).to_string();
    assert_eq!(parse_decls(&raw), vec![d.clone(), decl(None, -1)]);
    let r = release_of(d, EndReason::TaskEnded, 9).expect("release");
    let raw = json!({ "releases": [r.to_json()] }).to_string();
    assert_eq!(parse_releases(&raw), vec![r]);
}

/// 스캐너 시험용 호스트. meta 를 실제처럼 저장하고, 발화한 이벤트와 남긴 meta 를 기록한다.
#[derive(Default)]
struct MetaHost {
    meta: RefCell<HashMap<String, String>>,
    fired: RefCell<Vec<String>>,
    screen: RefCell<String>,
}

impl MetaHost {
    fn json(&self, key: &str) -> Option<Value> {
        self.meta
            .borrow()
            .get(key)
            .and_then(|v| serde_json::from_str(v).ok())
    }
    fn count(&self, event: &str) -> usize {
        self.fired.borrow().iter().filter(|e| *e == event).count()
    }
    fn decls(&self) -> Vec<Value> {
        self.json(QUIET_META_KEY)
            .and_then(|v| v["decls"].as_array().cloned())
            .unwrap_or_default()
    }
    fn releases(&self) -> Vec<Value> {
        self.json(QUIET_RELEASE_META_KEY)
            .and_then(|v| v["releases"].as_array().cloned())
            .unwrap_or_default()
    }
}

impl HostCall for MetaHost {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        let key = params["key"].as_str().unwrap_or_default().to_string();
        Ok(match method {
            "surface.read_since_scan_mark" => json!({ "text": "" }),
            "terminal.state" => json!({ "state": "active" }),
            "surface.screen_text" => json!({ "text": *self.screen.borrow() }),
            "settings.get_plugin_setting" => json!({ "value": null }),
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
            "hook.list" => json!([]),
            other => panic!("unexpected host call: {other}"),
        })
    }
}

/// 조용한 출력 파일을 가진 작업들. 수정 시각이 `age` 전이다.
fn quiet_files(ids: &[&str], age: Duration) -> (tempfile::TempDir, TaskWatch, FileActivity) {
    let dir = tempfile::tempdir().unwrap();
    for id in ids {
        let path = dir.path().join(format!("{id}.output"));
        std::fs::write(&path, "").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }
    let watch = TaskWatch {
        session_id: "s-quiet".into(),
        cwd: "/w".into(),
        tasks: ids
            .iter()
            .map(|id| WatchedTask {
                id: id.to_string(),
                label: format!("cmd {id}"),
            })
            .collect(),
    };
    let files = FileActivity::locate(&watch, dir.path()).unwrap();
    (dir, watch, files)
}

/// 새 대기를 열고 조용한 출력 파일을 붙인 뒤 정지 판정을 한 번 돌린다(59분 경로처럼 대기가 다시 시작된 경우).
fn scan_new_wait(s: &mut ErrorScanner, host: &MetaHost, ids: &[&str]) -> tempfile::TempDir {
    s.clear_background_wait(1);
    s.reset_dedupe(1);
    s.mark_background_wait(1, unix_now_ms());
    let (dir, watch, files) = quiet_files(ids, NO_OUTPUT_NOTICE_DEFAULT + Duration::from_secs(5));
    s.set_file_activity_for_test(1, watch, files);
    let t0 = Instant::now();
    s.scan_one_at(host, 1, t0);
    s.last_stall_notify.clear();
    dir
}

/// 양수 작업 선언 중에는 무출력 알림을 보내지 않고 쌓는다. 새 대기의 같은 작업은 같은 항목의 횟수가 된다.
/// 끝 시각이 지나면 묶음 하나를 남기고 부모 훅을 한 번 부른다.
#[test]
fn a_positive_declaration_holds_the_notices_and_releases_them_when_it_expires() {
    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.enable(1, crate::error_scan::ScanTarget::Child);
    s.declare_quiet(&host, 1, Some("T".into()), 5400);
    let _a = scan_new_wait(&mut s, &host, &["T"]);
    let _b = scan_new_wait(&mut s, &host, &["T"]);
    assert_eq!(host.count(STALLED_EVENT), 0, "선언 중에는 보내지 않는다");
    let decls = host.decls();
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0]["held"][0]["count"], 2, "{decls:?}");

    s.release_expired(&host, 1, unix_now_ms() + 5_400_000);
    assert_eq!(host.count(QUIET_RELEASED_EVENT), 1);
    assert!(host.decls().is_empty());
    let releases = host.releases();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0]["reason"], "expired");
    assert_eq!(releases[0]["items"][0]["count"], 2);
}

/// 쌓인 것이 없는 양수 선언은 어느 경로로 끝나도 묶음을 내지 않는다.
#[test]
fn an_empty_positive_declaration_ends_without_a_release() {
    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.declare_quiet(&host, 1, Some("A".into()), 60);
    s.declare_quiet(&host, 1, Some("B".into()), 60);
    s.declare_quiet(&host, 1, None, 60);
    s.release_expired(&host, 1, unix_now_ms() + 60_000 + 1);
    assert!(host.decls().is_empty());
    s.declare_quiet(&host, 1, Some("A".into()), 60);
    s.end_quiet_tasks(&host, 1, |id| id == "A");
    s.declare_quiet(&host, 1, None, 60);
    assert_eq!(s.clear_quiet(&host, 1, None), (1, false));
    assert_eq!(host.count(QUIET_RELEASED_EVENT), 0);
    assert!(host.releases().is_empty());
}

/// `-1` 선언은 알림을 보내지도 쌓지도 않고, 작업이 끝나도 아무것도 보내지 않는다.
#[test]
fn a_minus_one_declaration_drops_the_notices_and_sends_nothing_at_its_end() {
    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.enable(1, crate::error_scan::ScanTarget::Child);
    s.declare_quiet(&host, 1, Some("T".into()), -1);
    let _a = scan_new_wait(&mut s, &host, &["T"]);
    let _b = scan_new_wait(&mut s, &host, &["T"]);
    assert_eq!(host.count(STALLED_EVENT), 0);
    assert!(host.decls()[0]["held"].as_array().unwrap().is_empty());
    s.release_expired(&host, 1, u64::MAX - 1);
    assert_eq!(host.decls().len(), 1, "-1 은 시간으로 끝나지 않는다");
    s.end_quiet_tasks(&host, 1, |id| id == "T");
    assert!(host.decls().is_empty());
    assert_eq!(host.count(QUIET_RELEASED_EVENT), 0);
}

/// 선언된 작업 T 와 선언되지 않은 작업 U 가 함께 조용하면 U 이름만 바로 보내고 T 는 쌓는다.
#[test]
fn an_undeclared_task_is_noticed_at_once_beside_a_held_one() {
    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.enable(1, crate::error_scan::ScanTarget::Child);
    s.declare_quiet(&host, 1, Some("T".into()), 600);
    let _a = scan_new_wait(&mut s, &host, &["T", "U"]);
    assert_eq!(host.count(STALLED_EVENT), 1);
    let quiet = host
        .json(crate::task_watch::BACKGROUND_QUIET_META_KEY)
        .unwrap();
    assert_eq!(quiet["labels"], json!(["cmd U"]));
    assert_eq!(host.decls()[0]["held"][0]["key"], "T");
}

/// 대기가 아닌 정지: surface 선언은 오류 없는 정지를 쌓는다. 화면에 오류가 있으면 바로 error 로 보내고 선언을 모두 끝낸다.
/// 작업 선언만 있으면 오류 없는 정지도 지금처럼 보낸다.
#[test]
fn a_turn_stall_is_held_by_a_surface_declaration_unless_it_follows_an_error() {
    let quiet_turn = |s: &mut ErrorScanner, host: &MetaHost| {
        s.reset_dedupe(1);
        s.last_stall_notify.clear();
        let t0 = Instant::now();
        s.scan_one_at(host, 1, t0);
        s.scan_one_at(host, 1, t0 + NO_OUTPUT_NOTICE_DEFAULT);
    };
    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.declare_quiet(&host, 1, None, 600);
    quiet_turn(&mut s, &host);
    assert_eq!(host.count(STALLED_EVENT), 0);
    assert_eq!(host.decls()[0]["held"][0]["kind"], "turn quiet");

    *host.screen.borrow_mut() = "⎿ API Error: overloaded\n".into();
    quiet_turn(&mut s, &host);
    assert_eq!(host.count(STALLED_EVENT), 1);
    assert_eq!(
        host.json(crate::error_scan::STALL_NOTICE_META_KEY).unwrap()["kind"],
        STALL_KIND_ERROR
    );
    assert!(host.decls().is_empty(), "error 가 선언을 끝낸다");
    assert_eq!(host.releases()[0]["reason"], "error");
    assert_eq!(
        host.count(QUIET_RELEASED_EVENT),
        0,
        "묶음은 error 줄이 가져간다"
    );

    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.declare_quiet(&host, 1, Some("T".into()), 600);
    quiet_turn(&mut s, &host);
    assert_eq!(host.count(STALLED_EVENT), 1);
}

/// 플러그인이 다시 시작돼도 meta 의 선언과 쌓인 것으로 이어 간다. 꺼진 동안 끝 시각이 지났으면 첫 처리에서 묶음을 낸다.
#[test]
fn a_restarted_scanner_keeps_holding_and_releases_what_expired_meanwhile() {
    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.enable(1, crate::error_scan::ScanTarget::Child);
    s.declare_quiet(&host, 1, Some("T".into()), 600);
    let _a = scan_new_wait(&mut s, &host, &["T"]);

    let mut restarted = ErrorScanner::new();
    restarted.enable(1, crate::error_scan::ScanTarget::Child);
    let _b = scan_new_wait(&mut restarted, &host, &["T"]);
    assert_eq!(host.count(STALLED_EVENT), 0);
    assert_eq!(host.decls()[0]["held"][0]["count"], 2);

    let mut again = ErrorScanner::new();
    again.release_expired(&host, 1, unix_now_ms() + 600_000);
    assert_eq!(host.count(QUIET_RELEASED_EVENT), 1);
}

/// 완료 줄 없이 추적이 끝나면 선언을 지운다. 쌓인 것은 error 완료 줄만 잠시 가져갈 수 있다.
#[test]
fn stopping_the_tracking_ends_the_declarations_without_a_release_event() {
    let host = MetaHost::default();
    let mut s = ErrorScanner::new();
    s.enable(1, crate::error_scan::ScanTarget::Child);
    s.declare_quiet(&host, 1, None, 600);
    let _a = scan_new_wait(&mut s, &host, &["T"]);
    s.stop_tracking(&host, 1);
    assert!(host.decls().is_empty());
    assert_eq!(host.count(QUIET_RELEASED_EVENT), 0);
    assert!(s.take_quiet_releases(&host, 1, 3, false).is_empty());
    assert_eq!(s.take_quiet_releases(&host, 1, 3, true).len(), 1);
}

fn wait(ids: &[&str]) -> QuietNotice {
    QuietNotice::Wait {
        ids: ids.iter().map(|i| i.to_string()).collect(),
        label: "shell".into(),
    }
}

/// 아주 큰 `secs` 는 밀리초로 바꿀 때 감기지 않고 가장 먼 끝 시각에 머문다. 경계 바로 아래는 정확히 계산한다.
#[test]
fn a_huge_secs_saturates_instead_of_wrapping() {
    let edge = (u64::MAX - NOW) / 1000;
    let as_secs = |v: u64| i64::try_from(v).unwrap();
    assert_eq!(until_of(as_secs(edge), NOW), Some(NOW + edge * 1000));
    assert_eq!(until_of(as_secs(edge + 1), NOW), Some(u64::MAX));
    assert_eq!(until_of(2_305_843_009_213_693_952, NOW), Some(u64::MAX));
    assert_eq!(until_of(i64::MAX, NOW), Some(u64::MAX));
    assert_eq!(until_of(-1, NOW), None);
    let mut s = state(Vec::new());
    s.declare(None, i64::MAX, NOW);
    assert!(s.decls[0].active_at(NOW + 1_000_000_000_000));
    assert_eq!(s.until_for(None), Some(u64::MAX));
}

/// error 로 끝난 선언의 묶음은 일반 묶음 처리자가 가져가지 못한다.
/// 순서: A 의 묶음이 생겨 처리자가 예약됨 → B 에 누적 → B 가 error 로 끝남 → A 의 처리자가 먼저 돈다.
#[test]
fn an_error_release_is_left_for_the_error_line() {
    let mut s = state(vec![decl(Some("A"), 600), decl(Some("B"), 600)]);
    s.dispose(tasks(&["A"]), 1, NOW + 1);
    let (_, a) = s.end(
        |d| d.task.as_deref() == Some("A"),
        EndReason::Cleared,
        NOW + 2,
    );
    s.releases.extend(a);
    s.dispose(tasks(&["B"]), 1, NOW + 3);
    let (_, b) = s.end(|_| true, EndReason::Error, NOW + 4);
    s.releases.extend(b);
    let general = s.take_for(7, false);
    assert_eq!(
        general
            .iter()
            .map(|r| r.task.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("A")]
    );
    let on_error = s.take_for(7, true);
    assert_eq!(
        on_error
            .iter()
            .map(|r| (r.task.as_deref(), r.reason))
            .collect::<Vec<_>>(),
        vec![(Some("B"), EndReason::Error)]
    );
}

/// 한 번에 상한보다 많이 끝나도 받을 부모가 모두 가져가기 전에는 묶음을 지우지 않는다. 개수 상한은 모두 가져간 묶음에만 건다.
/// 부모 A 가 가져간 뒤에도 부모 B 는 묶음을 모두 받는다. error 로 끝난 묶음도 같다.
#[test]
fn ending_many_declarations_at_once_keeps_every_release_until_taken() {
    for (reason, with_error) in [(EndReason::Cleared, false), (EndReason::Error, true)] {
        let ids: Vec<String> = (0..RELEASES_KEPT + 4).map(|i| format!("T{i}")).collect();
        let mut s = state(ids.iter().map(|id| decl(Some(id), 600)).collect());
        for id in &ids {
            s.dispose(tasks(&[id.as_str()]), 1, NOW + 1);
        }
        let (count, released) = s.end(|_| true, reason, NOW + 2);
        assert_eq!((count, released.len()), (ids.len(), ids.len()));
        s.releases.extend(released.into_iter().map(|r| Release {
            recipients: vec![1, 2],
            ..r
        }));
        s.prune(NOW + 3);
        assert_eq!(s.releases.len(), ids.len(), "받기 전에는 지우지 않는다");
        assert_eq!(s.take_for(1, with_error).len(), ids.len(), "{reason:?}");
        s.prune(NOW + 4);
        assert_eq!(
            s.releases.len(),
            ids.len(),
            "B 가 받기 전에는 지우지 않는다: {reason:?}"
        );
        assert_eq!(s.take_for(2, with_error).len(), ids.len(), "{reason:?}");
        s.prune(NOW + 5);
        assert_eq!(s.releases.len(), RELEASES_KEPT, "{reason:?}");
    }
}

/// 받을 부모가 다 가져가지 않은 묶음도 보관 시간이 지나면 지운다.
#[test]
fn a_release_not_taken_by_every_parent_goes_after_the_keep_time() {
    let mut s = state(vec![decl(Some("T"), 600)]);
    s.dispose(tasks(&["T"]), 1, NOW + 1);
    let (_, released) = s.end(|_| true, EndReason::Cleared, NOW + 2);
    s.releases.extend(released.into_iter().map(|r| Release {
        recipients: vec![1, 2],
        ..r
    }));
    s.take_for(1, false);
    s.prune(NOW + 2 + RELEASE_KEPT_MS - 1);
    assert_eq!(s.releases.len(), 1);
    s.prune(NOW + 2 + RELEASE_KEPT_MS);
    assert!(s.releases.is_empty());
}

/// 화면 기준 대기는 Stop 이 알려 준 작업 순서와 관계없이 같은 작업 집합이면 한 항목으로 합친다.
#[test]
fn a_wait_on_the_same_tasks_in_another_order_is_one_item() {
    let mut s = state(vec![decl(None, 600)]);
    s.dispose(wait(&["T", "U"]), 1, NOW + 1);
    s.dispose(wait(&["U", "T", "T"]), 1, NOW + 2);
    assert_eq!(
        held_counts(&s),
        vec![(None, vec![("wait:T,U".to_string(), 2)])]
    );
}

/// 여러 작업 선언에 걸린 화면 기준 대기 알림은 한 선언에만 쌓는다(끝 시각이 가장 늦은 양수 선언).
/// 걸린 선언이 모두 -1 이면 버린다.
#[test]
fn a_wait_covered_by_several_task_declarations_is_held_once() {
    let mut s = state(vec![decl(Some("A"), 900), decl(Some("B"), 600)]);
    assert_eq!(s.dispose(wait(&["A", "B"]), 1, NOW + 1), Send::Nothing);
    let (_, released) = s.end(|_| true, EndReason::Cleared, NOW + 2);
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].task.as_deref(), Some("A"));

    let mut s = state(vec![decl(Some("A"), -1), decl(Some("B"), 600)]);
    s.dispose(wait(&["A", "B"]), 1, NOW + 1);
    assert_eq!(
        held_counts(&s),
        vec![
            (Some("A".to_string()), vec![]),
            (Some("B".to_string()), vec![("wait:A,B".to_string(), 1)])
        ]
    );

    let mut s = state(vec![decl(Some("A"), -1), decl(Some("B"), -1)]);
    assert_eq!(s.dispose(wait(&["A", "B"]), 1, NOW + 1), Send::Nothing);
    assert!(s.decls.iter().all(|d| d.held.is_empty()));
}

/// 양수→양수 재선언은 끝 시각을 늦출 때만 바꾼다. 짧은 값으로 다시 선언해도 앞당기지 않는다.
#[test]
fn a_shorter_redeclaration_does_not_pull_the_end_in() {
    let mut s = state(Vec::new());
    s.declare(Some("T".into()), 5400, NOW);
    s.declare(Some("T".into()), 60, NOW + 10);
    assert_eq!(
        (s.decls[0].secs, s.decls[0].until_ms),
        (5400, Some(NOW + 5_400_000))
    );
    s.declare(Some("T".into()), 9000, NOW + 20);
    assert_eq!(
        (s.decls[0].secs, s.decls[0].until_ms),
        (9000, Some(NOW + 20 + 9_000_000))
    );
}

/// 선언 하나가 따로 적는 항목은 상한까지다. 넘는 알림은 버리지 않고 수와 시각만 센다.
/// 이름은 묶음 줄과 같은 길이로 잘라 저장한다.
#[test]
fn items_past_the_cap_are_counted_not_dropped() {
    let mut s = state(vec![decl(None, 600)]);
    let long = "x".repeat(200);
    s.dispose(
        QuietNotice::Tasks(vec![("L".into(), format!("a\n{long}"))]),
        1,
        NOW,
    );
    for i in 0..HELD_KEYS_MAX + 5 {
        s.dispose(tasks(&[format!("T{i}").as_str()]), 1, NOW + 1 + i as u64);
    }
    let d = s.decls[0].clone();
    assert_eq!(d.held.len(), HELD_KEYS_MAX);
    let overflow = d.overflow.expect("넘은 알림");
    assert_eq!(
        (overflow.count, overflow.first_ms, overflow.last_ms),
        (6, NOW + 64, NOW + 69)
    );
    assert_eq!(d.held[0].label.chars().count(), LABEL_CHARS + 1);
    assert!(!d.held[0].label.contains('\n'));
    assert_eq!(Decl::from_json(&d.to_json()), Some(d.clone()));
    let (_, released) = s.end(|_| true, EndReason::Cleared, NOW + 100);
    assert_eq!(released[0].held_count(), 70);
    assert_eq!(released[0].overflow, Some(overflow));
}
