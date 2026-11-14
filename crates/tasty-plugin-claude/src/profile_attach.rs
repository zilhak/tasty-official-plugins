//! 세션 ID별 프로필 부착 기록을 파일로 보관한다.
//!
//! 터미널 메타데이터는 앱·탭 복원 시 사라질 수 있어 별도 기록이 필요하다.
//! `TASTY_PLUGIN_DATA_DIR/profiles/attachments/<session_id>.json`에 저장하며,
//! 이름으로 부착한 프로필은 복원 때 다시 해석하고 경로로 부착한 것은 그대로 쓴다.
//!
//! session-start에서 기록을 다시 쓰고 session-end에서 종료 시각을 남긴다.
//! 닫은 탭을 복원할 수 있도록 종료 때 바로 지우지 않는다.
//! 이후 sweep이 종료 유예나 TTL을 지난 기록의 삭제를 시도한다.
//! 명시적인 --clear-profile은 유예 없이 삭제한다.
//! 세션 ID의 파일명 검증은 세션 사이의 접근 권한을 분리하지 않는다.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tracing::warn;

/// 복원 시 다시 해석할 이름 또는 그대로 사용할 경로.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttachRecord {
    /// `--profile <names>` — 쉼표 구분 이름 목록(등록 프로필 또는 게이트).
    Names(String),
    /// `--profile-file <path>` — 절대 경로.
    Path(String),
}

impl AttachRecord {
    fn kind(&self) -> &'static str {
        match self {
            AttachRecord::Names(_) => "names",
            AttachRecord::Path(_) => "path",
        }
    }

    fn value(&self) -> &str {
        match self {
            AttachRecord::Names(v) | AttachRecord::Path(v) => v,
        }
    }
}

/// 종료 표시가 없는 기록의 TTL. 재시작 없이 오래 실행되는 세션을 고려해 길게 둔다.
/// session-start에서 다시 저장하면 수정 시각이 갱신된다.
const RECORD_TTL: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// 종료 표시 후 닫은 탭을 복원할 수 있도록 기록을 남기는 기간.
const ENDED_GRACE: Duration = Duration::from_secs(24 * 60 * 60);

fn attachments_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("profiles").join("attachments")
}

/// 세션 ID로 기록 파일 경로를 만든다. 각 진입점에서 영숫자·하이픈·밑줄만 허용한다.
fn record_file(data_dir: &Path, session_id: &str) -> PathBuf {
    attachments_dir(data_dir).join(format!("{session_id}.json"))
}

/// 데이터 경로나 유효한 세션 ID가 없으면 기록을 생략한다.
fn resolve(data_dir: Option<&Path>, session_id: &str) -> Option<PathBuf> {
    let dir = data_dir?;
    if !tasty_plugin_agent_common::reboot::is_safe_session_id(session_id) {
        return None;
    }
    Some(record_file(dir, session_id))
}

/// 부착 기록을 쓴다(있으면 덮어쓴다). 실패는 warn 로그 후 무시.
pub(crate) fn store(data_dir: Option<&Path>, session_id: &str, record: &AttachRecord) {
    let Some(path) = resolve(data_dir, session_id) else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(
            "claude profile attach: failed to create {}: {e}",
            parent.display()
        );
        return;
    }
    // 다시 저장하면 이전 종료 표시를 지운다.
    let text = json!({ "kind": record.kind(), "value": record.value() }).to_string();
    if let Err(e) = std::fs::write(&path, text) {
        warn!(
            "claude profile attach: failed to write {}: {e}",
            path.display()
        );
    }
}

/// 부착 기록을 읽는다. 파일을 읽거나 해석하지 못하면 None을 반환한다.
pub(crate) fn load(data_dir: Option<&Path>, session_id: &str) -> Option<AttachRecord> {
    let path = resolve(data_dir, session_id)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "claude profile attach: malformed record {}: {e}",
                path.display()
            );
            return None;
        }
    };
    let kind = value.get("kind").and_then(|v| v.as_str());
    let inner = value
        .get("value")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    match (kind, inner) {
        (Some("names"), Some(v)) => Some(AttachRecord::Names(v.to_string())),
        (Some("path"), Some(v)) => Some(AttachRecord::Path(v.to_string())),
        _ => {
            warn!(
                "claude profile attach: unusable record {} (kind={kind:?})",
                path.display()
            );
            None
        }
    }
}

/// 종료 시각을 남긴다. 닫은 탭 복원을 위해 파일은 보존하며, 없으면 새로 만들지 않는다.
pub(crate) fn mark_ended(data_dir: Option<&Path>, session_id: &str) {
    let Some(path) = resolve(data_dir, session_id) else {
        return;
    };
    let Some(record) = load(data_dir, session_id) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let text =
        json!({ "kind": record.kind(), "value": record.value(), "ended_at": now }).to_string();
    if let Err(e) = std::fs::write(&path, text) {
        warn!(
            "claude profile attach: failed to mark {} ended: {e}",
            path.display()
        );
    }
}

/// 부착 기록을 지운다(`--clear-profile` 처럼 명시적 해제). 없는 파일은 성공으로 본다.
pub(crate) fn remove(data_dir: Option<&Path>, session_id: &str) {
    let Some(path) = resolve(data_dir, session_id) else {
        return;
    };
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(
            "claude profile attach: failed to remove {}: {e}",
            path.display()
        );
    }
}

/// session-start 때 오래된 기록의 삭제를 시도한다. 실패해도 훅 처리는 계속한다.
pub(crate) fn sweep(data_dir: Option<&Path>) {
    sweep_at(data_dir, std::time::SystemTime::now());
}

/// 시험에서 현재 시각을 지정해 파일 수정 시각을 조작하지 않고 만료를 확인한다.
fn sweep_at(data_dir: Option<&Path>, now: std::time::SystemTime) {
    let Some(dir) = data_dir.map(attachments_dir) else {
        return;
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            // 기록을 만든 적이 없으면 디렉터리가 없을 수 있다.
            tracing::debug!(
                "claude profile attach sweep: read_dir({}) failed: {e}",
                dir.display()
            );
            return;
        }
    };
    let now_epoch = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        // 종료 시각이 있으면 짧은 유예를, 없으면 파일 수정 시각과 TTL을 적용한다.
        let ended = ended_at(&entry.path());
        let stale = match ended {
            Some(at) => now_epoch.saturating_sub(at) >= ENDED_GRACE.as_secs(),
            None => entry
                .metadata()
                .and_then(|m| m.modified())
                .and_then(|modified| {
                    now.duration_since(modified)
                        .map_err(|e| std::io::Error::other(e.to_string()))
                })
                .is_ok_and(|age| age >= RECORD_TTL),
        };
        if stale && let Err(e) = std::fs::remove_file(entry.path()) {
            tracing::debug!(
                "claude profile attach sweep: remove({:?}) failed: {e}",
                entry.path()
            );
        }
    }
}

/// 종료 시각(Unix 초)을 읽는다. 없거나 읽지 못하면 None을 반환한다.
fn ended_at(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("ended_at").and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_load_round_trips_names() {
        let tmp = tempfile::tempdir().unwrap();
        store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Names("reviewer".into()),
        );
        assert_eq!(
            load(Some(tmp.path()), "sess-1"),
            Some(AttachRecord::Names("reviewer".into()))
        );
    }

    #[test]
    fn store_and_load_round_trips_path() {
        let tmp = tempfile::tempdir().unwrap();
        store(
            Some(tmp.path()),
            "sess-1",
            &AttachRecord::Path("/abs/p.json".into()),
        );
        assert_eq!(
            load(Some(tmp.path()), "sess-1"),
            Some(AttachRecord::Path("/abs/p.json".into()))
        );
    }

    #[test]
    fn store_overwrites_previous_record() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "s", &AttachRecord::Names("a".into()));
        store(Some(tmp.path()), "s", &AttachRecord::Names("b".into()));
        assert_eq!(
            load(Some(tmp.path()), "s"),
            Some(AttachRecord::Names("b".into()))
        );
    }

    #[test]
    fn remove_deletes_the_record_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "s", &AttachRecord::Names("a".into()));
        remove(Some(tmp.path()), "s");
        assert_eq!(load(Some(tmp.path()), "s"), None);
        remove(Some(tmp.path()), "s");
    }

    /// 훅 payload 의 session id 는 외부 입력이라 경로 조각으로 쓰기 전에 걸러야 한다.
    #[test]
    fn unsafe_session_id_never_touches_the_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        store(
            Some(tmp.path()),
            "../escape",
            &AttachRecord::Names("a".into()),
        );
        assert!(!tmp.path().join("profiles").exists());
        assert_eq!(load(Some(tmp.path()), "../escape"), None);
    }

    #[test]
    fn no_data_dir_is_a_silent_no_op() {
        assert_eq!(load(None, "s"), None);
        store(None, "s", &AttachRecord::Names("a".into()));
        remove(None, "s");
        sweep(None);
    }

    #[test]
    fn malformed_record_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = attachments_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s.json"), "{not json").unwrap();
        assert_eq!(load(Some(tmp.path()), "s"), None);
        std::fs::write(dir.join("s.json"), r#"{"kind":"bogus","value":"x"}"#).unwrap();
        assert_eq!(load(Some(tmp.path()), "s"), None);
    }

    /// session-end 표시는 기록을 지우지 않는다 — 닫은 탭 복원이 다시 읽어야 한다.
    #[test]
    fn mark_ended_keeps_the_record_readable() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "s", &AttachRecord::Names("probe".into()));
        mark_ended(Some(tmp.path()), "s");
        assert_eq!(
            load(Some(tmp.path()), "s"),
            Some(AttachRecord::Names("probe".into()))
        );
        assert!(ended_at(&record_file(tmp.path(), "s")).is_some());
    }

    /// 다시 저장하면 이전 종료 표시가 사라진다.
    #[test]
    fn re_stamping_clears_the_ended_mark() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "s", &AttachRecord::Names("probe".into()));
        mark_ended(Some(tmp.path()), "s");
        store(Some(tmp.path()), "s", &AttachRecord::Names("probe".into()));
        assert!(ended_at(&record_file(tmp.path(), "s")).is_none());
    }

    /// 기록이 없으면 종료 표시는 아무것도 만들지 않는다.
    #[test]
    fn mark_ended_on_a_missing_record_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        mark_ended(Some(tmp.path()), "s");
        assert_eq!(load(Some(tmp.path()), "s"), None);
    }

    /// 종료 표시된 기록은 유예 안에서는 살아남고(복원 가능), 유예를 넘기면 회수된다.
    #[test]
    fn sweep_respects_the_ended_grace() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "s", &AttachRecord::Names("probe".into()));
        mark_ended(Some(tmp.path()), "s");

        sweep(Some(tmp.path()));
        assert!(record_file(tmp.path(), "s").exists(), "유예 안에서는 보존");

        let future = std::time::SystemTime::now() + ENDED_GRACE + Duration::from_secs(60);
        sweep_at(Some(tmp.path()), future);
        assert!(!record_file(tmp.path(), "s").exists(), "유예를 넘기면 회수");
    }

    /// 종료 표시가 없는 기록에는 긴 TTL을 적용한다.
    #[test]
    fn sweep_does_not_apply_the_ended_grace_to_live_records() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "s", &AttachRecord::Names("probe".into()));
        let future = std::time::SystemTime::now() + ENDED_GRACE + Duration::from_secs(60);
        sweep_at(Some(tmp.path()), future);
        assert!(record_file(tmp.path(), "s").exists());
    }

    /// TTL 안쪽 기록은 건드리지 않는다.
    #[test]
    fn sweep_keeps_records_within_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "fresh", &AttachRecord::Names("a".into()));
        sweep(Some(tmp.path()));
        assert!(record_file(tmp.path(), "fresh").exists());
    }

    /// 파일 수정 시각 대신 비교 시각을 미래로 지정해 TTL 경과를 확인한다.
    #[test]
    fn sweep_removes_records_past_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        store(Some(tmp.path()), "old", &AttachRecord::Names("a".into()));
        let future = std::time::SystemTime::now() + RECORD_TTL + Duration::from_secs(60);
        sweep_at(Some(tmp.path()), future);
        assert!(!record_file(tmp.path(), "old").exists());
    }
}
