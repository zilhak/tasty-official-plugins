//! 대화 기록을 별도 스레드에서 읽는다. 동기 IPC dispatch가 파일 I/O를 기다리지 않게 한다.
//! 매 반복에서 파일을 읽고, VERIFY_EVERY 반복마다 호스트에 대상과 세션을 다시 확인한다.
//! 시험에는 환경 변수 대신 임시 기록 루트를 전달할 수 있다.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tasty_plugin_sdk::HostHandle;

use crate::record::{self, StreamEvent};
use crate::registry::StreamRegistry;
use crate::registry::TailCheckout;
use crate::resolve::{self, HostCall};
use crate::tail::TailPoll;

/// poison 경고는 처음 한 번만 남긴다.
static REGISTRY_POISON_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// poison 상태여도 기존 레지스트리를 사용해 수집을 계속한다.
/// 복구가 패닉 전의 부분 갱신을 되돌리지는 않는다. IPC 핸들러는 별도 오류 정책을 쓴다.
fn lock_registry(registry: &Shared) -> std::sync::MutexGuard<'_, StreamRegistry> {
    registry.lock().unwrap_or_else(|poisoned| {
        if !REGISTRY_POISON_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::error!(
                "agent-stream: the registry lock is poisoned (a thread panicked while holding \
                 it) — recovering so the tail loop keeps collecting; later occurrences are \
                 not logged"
            );
        }
        poisoned.into_inner()
    })
}

/// 파일 읽기 반복 사이의 대기 시간. I/O와 IPC에 걸리는 시간은 별도다.
pub const TICK: Duration = Duration::from_millis(300);

/// 대상과 세션을 다시 확인할 반복 횟수.
const VERIFY_EVERY: u64 = 10;

type Shared = Arc<Mutex<StreamRegistry>>;

pub(crate) fn tail_loop(registry: Shared, host: HostHandle) {
    let mut tick_count: u64 = 0;
    loop {
        std::thread::sleep(TICK);
        tick_count += 1;
        if tick(&registry, &host, tick_count).is_break() {
            return;
        }
    }
}

/// 수집 루프 한 번. Continue면 다음 반복을 진행한다.
fn tick<H: HostCall>(registry: &Shared, host: &H, tick_count: u64) -> std::ops::ControlFlow<()> {
    // 환경이 준비된 뒤 다시 찾을 수 있도록 루트를 매번 확인한다.
    let root = resolve::transcript_root();
    if tick_count.is_multiple_of(VERIFY_EVERY) {
        verify_targets(registry, host, root.as_deref());
    }
    pump_all(registry, root.as_deref());
    {
        let mut reg = lock_registry(registry);
        // 방금 수집한 이벤트의 활동 시각을 반영한 뒤 비활성 턴을 닫는다.
        reg.sweep_stale_turns(std::time::Instant::now());
        reg.save_if_dirty();
    }
    std::ops::ControlFlow::Continue(())
}

/// 호스트에 대상 생존과 세션 id 를 되묻는다. lock 은 IPC **바깥**에서만 잡는다.
pub(crate) fn verify_targets<H: HostCall>(registry: &Shared, host: &H, root: Option<&Path>) {
    let targets = lock_registry(registry).targets();
    for (surface_id, session_id, _) in targets {
        verify_one(registry, host, root, surface_id, &session_id);
    }
}

fn verify_one<H: HostCall>(
    registry: &Shared,
    host: &H,
    root: Option<&Path>,
    surface_id: u32,
    session_id: &str,
) {
    if !resolve::surface_exists(host, surface_id) {
        // 대상 자체가 사라졌다 — 소비자가 영원히 기다리지 않도록 턴을 닫고 해제한다.
        lock_registry(registry).remove(surface_id, record::REASON_SESSION_ENDED);
        return;
    }
    // meta 조회 실패(일시적 IPC 오류 포함)는 대상 유지 — 다음 verify 에서 재시도한다.
    let Ok(current) = resolve::session_id_for_surface(host, surface_id) else {
        return;
    };
    if current == session_id {
        return;
    }
    // 새 세션 파일이 아직 없으면 경로 미해결(빈 경로)로 바꿔 둔다 — 매 tick 재해석한다.
    let path = root
        .and_then(|r| resolve::find_transcript(r, &current))
        .unwrap_or_default();
    lock_registry(registry).switch_session(surface_id, current, path);
}

/// 등록된 모든 대상의 파일을 한 번씩 읽어 이벤트를 만든다.
pub(crate) fn pump_all(registry: &Shared, root: Option<&Path>) {
    let surfaces: Vec<u32> = lock_registry(registry)
        .targets()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    for surface_id in surfaces {
        pump_one(registry, surface_id, root);
    }
}

/// 대상 하나의 상태를 잠시 꺼내 파일을 읽고 돌려놓는다.
/// 파일 I/O 동안 레지스트리를 잠그지 않아 IPC·ping 처리의 잠금 대기를 줄인다.
fn pump_one(registry: &Shared, surface_id: u32, root: Option<&Path>) {
    resolve_pending_path(registry, surface_id, root);
    let Some(mut checkout) = check_out(registry, surface_id) else {
        return;
    };

    let poll = checkout.tail.poll(&checkout.transcript);

    apply_poll(registry, checkout, poll);
}

fn check_out(registry: &Shared, surface_id: u32) -> Option<TailCheckout> {
    lock_registry(registry).check_out(surface_id)
}

/// 읽어온 결과를 되돌리고 이벤트로 바꾼다.
fn apply_poll(registry: &Shared, checkout: TailCheckout, poll: std::io::Result<TailPoll>) {
    let surface_id = checkout.surface_id;
    let session_id = checkout.session_id.clone();
    let mut reg = lock_registry(registry);
    // 꺼내간 동안 대상이 바뀌었으면(unwatch · 세션 교체 · 재-watch) 읽어온 것은 옛 대상의
    // 진행 상태이므로 통째로 버린다.
    if !reg.check_in(checkout) {
        return;
    }
    let lines = match poll {
        Ok(TailPoll::Lines { lines, resynced }) => {
            if resynced {
                tracing::debug!(
                    "agent-stream: transcript for surface {surface_id} was truncated or replaced — re-read from the start, skipping UUIDs still in the dedupe cache"
                );
            }
            lines
        }
        Ok(TailPoll::Missing) => return,
        Err(e) => {
            tracing::warn!("agent-stream: reading transcript for surface {surface_id} failed: {e}");
            return;
        }
    };
    if lines.is_empty() {
        return;
    }
    let events = collect_events(&mut reg, surface_id, &lines);
    for event in events {
        reg.push_event(surface_id, &session_id, event);
    }
    reg.mark_dirty();
}

/// 아직 찾지 못한 기록 파일의 경로를 잠금 밖에서 다시 찾는다.
fn resolve_pending_path(registry: &Shared, surface_id: u32, root: Option<&Path>) {
    let Some(root) = root else {
        return;
    };
    let Some((session_id, transcript)) = target_of(registry, surface_id) else {
        return;
    };

    if transcript.is_file() {
        return;
    }
    let Some(path) = resolve::find_transcript(root, &session_id) else {
        return;
    };

    // 그 사이 세션이 바뀌었으면 반영하지 않는다 — 방금 찾은 경로는 옛 세션 것이다.
    lock_registry(registry).set_transcript(surface_id, &session_id, path);
}

/// 대상의 (session_id, transcript) 스냅샷. 락은 이 함수 안에서만 잡는다.
fn target_of(registry: &Shared, surface_id: u32) -> Option<(String, std::path::PathBuf)> {
    let reg = lock_registry(registry);
    reg.targets()
        .into_iter()
        .find(|(id, _, _)| *id == surface_id)
        .map(|(_, session, path)| (session, path))
}

/// 기록을 이벤트로 바꾸고 보관 중인 uuid와 중복되면 생략한다.
fn collect_events(reg: &mut StreamRegistry, surface_id: u32, lines: &[String]) -> Vec<StreamEvent> {
    let Some(watch) = reg.watch_mut(surface_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in lines {
        match record::parse_line(line) {
            Ok(parsed) => {
                if !watch.accept_record(parsed.uuid.as_deref()) {
                    continue;
                }
                out.extend(parsed.events);
            }
            Err(e) => {
                // 재동기화 직후의 반쪽 라인이거나 우리가 모르는 포맷이다. 그 한 줄만
                // 버리고 계속 간다 — 한 줄 때문에 스트림 전체를 끊지 않는다.
                tracing::debug!("agent-stream: skipping unparsable transcript line: {e}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::new_watch;
    use serde_json::{Value, json};
    use std::io::Write;
    use tasty_plugin_sdk::PluginError;

    struct StubHost {
        exists: bool,
        session: Option<String>,
    }

    /// 없는 대상을 호스트의 실제 대상 부재 오류로 거절하는 stub.
    struct RejectingHost;

    impl HostCall for RejectingHost {
        fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
            let sid = params
                .get("surface_id")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            match method {
                "surface.locate" => Err(PluginError::HostCall {
                    method: method.to_string(),
                    code: None,
                    message: format!(
                        "no live surface {sid} (named by 'surface.locate'); list the resource \
                         to get a live id — a named target is never resolved by focus"
                    ),
                }),
                other => panic!("unexpected host call {other}"),
            }
        }
    }

    impl HostCall for StubHost {
        fn call(&self, method: &str, _params: Value) -> Result<Value, PluginError> {
            match method {
                "surface.locate" => Ok(json!({ "exists": self.exists })),
                "surface.meta.get" => Ok(json!({ "value": self.session })),
                other => panic!("unexpected host call {other}"),
            }
        }
    }

    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open");
        f.write_all(text.as_bytes()).expect("write");
    }

    fn assistant_line(uuid: &str, text: &str, stop: &str) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"{uuid}","message":{{"stop_reason":"{stop}","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn shared_with(watch_path: &Path) -> Shared {
        let reg = Arc::new(Mutex::new(StreamRegistry::new(None)));
        reg.lock().expect("lock").insert(new_watch(
            1,
            "sess".into(),
            watch_path.to_path_buf(),
            false,
        ));
        reg
    }

    fn events(registry: &Shared) -> Vec<Value> {
        let reg = registry.lock().expect("lock");
        reg.poll_json(None, 0, 1000)["events"]
            .as_array()
            .expect("array")
            .clone()
    }

    /// 잠금이 poison 상태여도 기록 수집을 계속해야 한다.
    #[test]
    fn a_poisoned_registry_does_not_stop_the_tail_loop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        let poisoner = Arc::clone(&registry);
        // 의도한 패닉과 poison이 실제로 발생했는지 확인한다.
        std::thread::spawn(move || {
            let _guard = poisoner.lock().expect("fresh lock");
            panic!("poison the registry on purpose");
        })
        .join()
        .expect_err("패닉한 스레드는 Err 로 join 된다");
        assert!(registry.is_poisoned(), "전제: 락이 poison 이다");

        append(&path, &(assistant_line("u1", "hello", "end_turn") + "\n"));
        pump_all(&registry, None);

        // poison 상태이므로 제품과 같은 복구 함수로 결과를 읽는다.
        let collected = lock_registry(&registry).poll_json(None, 0, 1000)["events"]
            .as_array()
            .expect("array")
            .clone();
        assert!(
            !collected.is_empty(),
            "poison 이후에도 tail 이 이벤트를 만들어야 한다"
        );
    }

    /// 개별 함수뿐 아니라 전체 tick도 poison 상태에서 계속 실행돼야 한다.
    #[test]
    fn a_poisoned_registry_does_not_end_the_tail_thread() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        let poisoner = Arc::clone(&registry);
        std::thread::spawn(move || {
            let _guard = poisoner.lock().expect("fresh lock");
            panic!("poison the registry on purpose");
        })
        .join()
        .expect_err("패닉한 스레드는 Err 로 join 된다");
        assert!(registry.is_poisoned(), "전제: 락이 poison 이다");

        let host = StubHost {
            exists: true,
            session: Some("sess".into()),
        };
        append(&path, &(assistant_line("u1", "hello", "end_turn") + "\n"));

        // tick_count=1 은 verify 주기가 아니다(VERIFY_EVERY=10) — 이 단언이 보는 것은
        // pump 와 sweep 이지 호스트 왕복이 아니다.
        assert_eq!(
            tick(&registry, &host, 1),
            std::ops::ControlFlow::Continue(()),
            "poison 복구 뒤 수집 루프가 계속돼야 한다"
        );
        // poison 상태는 유지되므로 다음 반복에서도 계속 실행해야 한다.
        assert_eq!(
            tick(&registry, &host, 2),
            std::ops::ControlFlow::Continue(()),
            "poison 상태가 유지돼도 다음 tick을 계속해야 한다"
        );

        let collected = lock_registry(&registry).poll_json(None, 0, 1000)["events"]
            .as_array()
            .expect("array")
            .clone();
        assert!(
            !collected.is_empty(),
            "루프가 계속됐다면 그 사이 수집도 됐어야 한다"
        );
    }

    #[test]
    fn each_append_produces_its_events_exactly_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        append(
            &path,
            &format!("{}\n", assistant_line("u1", "one", "tool_use")),
        );
        pump_all(&registry, None);
        pump_all(&registry, None); // 두 번 돌려도 다시 방출되지 않는다.
        assert_eq!(events(&registry).len(), 1);

        append(
            &path,
            &format!("{}\n", assistant_line("u2", "two", "end_turn")),
        );
        pump_all(&registry, None);
        let collected = events(&registry);
        assert_eq!(collected.len(), 3, "text + text + turn_end");
        assert_eq!(collected[1]["text"], "two");
        assert_eq!(collected[2]["kind"], "turn_end");
        assert_eq!(collected[2]["reason"], "stop:end_turn");
    }

    #[test]
    fn a_partial_line_emits_once_when_it_completes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        let line = assistant_line("u1", "hello", "end_turn");
        let (head, tail) = line.split_at(line.len() / 2);
        append(&path, head);
        pump_all(&registry, None);
        assert!(events(&registry).is_empty(), "half a line emits nothing");

        append(&path, &format!("{tail}\n"));
        pump_all(&registry, None);
        assert_eq!(events(&registry).len(), 2, "text + turn_end, emitted once");
        pump_all(&registry, None);
        assert_eq!(events(&registry).len(), 2, "and not again");
    }

    #[test]
    fn a_rewritten_transcript_is_re_read_without_duplicating_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        append(
            &path,
            &format!("{}\n", assistant_line("u1", "a", "end_turn")),
        );
        pump_all(&registry, None);
        assert_eq!(events(&registry).len(), 2);

        // 파일이 통째로 짧게 다시 쓰였다 — 같은 레코드가 다시 관측된다.
        std::fs::write(
            &path,
            format!("{}\n", assistant_line("u1", "a", "end_turn")),
        )
        .expect("rewrite");
        pump_all(&registry, None);
        assert_eq!(
            events(&registry).len(),
            2,
            "uuid dedupe absorbs the re-read records"
        );
    }

    #[test]
    fn unparsable_lines_do_not_stop_the_stream() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        append(
            &path,
            &format!(
                "garbage not json\n{}\n",
                assistant_line("u1", "still here", "end_turn")
            ),
        );
        pump_all(&registry, None);
        let collected = events(&registry);
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0]["text"], "still here");
    }

    #[test]
    fn a_transcript_that_appears_later_is_picked_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 등록 시점에는 경로가 미해결(빈 경로)이다 — 세션 시작 직후 race.
        let registry = shared_with(Path::new(""));
        pump_all(&registry, Some(dir.path()));
        assert!(events(&registry).is_empty());

        let project = dir.path().join("-some-project");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::write(
            project.join("sess.jsonl"),
            format!("{}\n", assistant_line("u1", "late", "end_turn")),
        )
        .expect("write");

        pump_all(&registry, Some(dir.path()));
        let collected = events(&registry);
        assert_eq!(collected.len(), 2, "the late transcript is read in full");
        assert_eq!(collected[0]["text"], "late");
    }

    #[test]
    fn a_closed_surface_is_dropped_with_a_terminal_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        verify_targets(
            &registry,
            &StubHost {
                exists: false,
                session: Some("sess".into()),
            },
            None,
        );
        assert!(!registry.lock().expect("lock").is_watched(1));
        let collected = events(&registry);
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0]["kind"], "turn_end");
        assert_eq!(collected[0]["reason"], record::REASON_SESSION_ENDED);
    }

    /// 호스트가 대상 부재 오류로 거절해도 추적을 끝내야 한다.
    #[test]
    fn a_surface_the_host_rejects_as_absent_is_also_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);
        assert!(
            registry.lock().expect("lock").is_watched(1),
            "전제: 대상이 등록돼 있다"
        );

        verify_targets(&registry, &RejectingHost, None);

        assert!(
            !registry.lock().expect("lock").is_watched(1),
            "호스트가 대상 부재 오류를 반환하면 추적 목록에서 제거해야 한다"
        );
        let collected = events(&registry);
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0]["reason"], record::REASON_SESSION_ENDED);
    }

    #[test]
    fn a_new_session_on_the_same_surface_rebinds_the_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("old.jsonl");
        std::fs::write(&old, b"").expect("create");
        let project = dir.path().join("-proj");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::write(project.join("brand-new.jsonl"), b"").expect("write");
        let registry = shared_with(&old);

        verify_targets(
            &registry,
            &StubHost {
                exists: true,
                session: Some("brand-new".into()),
            },
            Some(dir.path()),
        );
        {
            let mut reg = registry.lock().expect("lock");
            let watch = reg.watch_mut(1).expect("still watched");
            assert_eq!(watch.session_id, "brand-new");
            assert_eq!(watch.transcript, project.join("brand-new.jsonl"));
        }
        let collected = events(&registry);
        assert_eq!(collected[0]["reason"], record::REASON_SESSION_ENDED);
        assert_eq!(collected[0]["session_id"], "sess", "the old turn is closed");
    }

    #[test]
    fn a_session_switch_whose_file_is_not_written_yet_leaves_the_path_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("old.jsonl");
        std::fs::write(&old, b"").expect("create");
        let registry = shared_with(&old);

        verify_targets(
            &registry,
            &StubHost {
                exists: true,
                session: Some("not-on-disk-yet".into()),
            },
            Some(dir.path()),
        );
        let mut reg = registry.lock().expect("lock");
        let watch = reg.watch_mut(1).expect("still watched");
        assert_eq!(watch.session_id, "not-on-disk-yet");
        assert_eq!(watch.transcript, std::path::PathBuf::new());
    }

    #[test]
    fn an_unchanged_session_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        verify_targets(
            &registry,
            &StubHost {
                exists: true,
                session: Some("sess".into()),
            },
            None,
        );
        assert!(registry.lock().expect("lock").is_watched(1));
        assert!(events(&registry).is_empty());
    }

    #[test]
    fn a_meta_lookup_failure_does_not_drop_the_watch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let registry = shared_with(&path);

        verify_targets(
            &registry,
            &StubHost {
                exists: true,
                session: None,
            },
            None,
        );
        assert!(
            registry.lock().expect("lock").is_watched(1),
            "a transient meta read failure must not tear the stream down"
        );
    }
}
