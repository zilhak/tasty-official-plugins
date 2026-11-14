//! surface의 claude-session-id 메타데이터로 기록 파일을 찾는다.
//! Claude 플러그인을 직접 호출하지 않고 호스트 IPC로 메타데이터를 읽는다.
//! 외부 도구의 프로젝트 디렉터리 이름 규칙을 추측하지 않고 루트와 바로 아래 디렉터리를 탐색한다.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tasty_plugin_sdk::{HostHandle, PluginError};

/// claude plugin 이 `session-start` 훅에서 기록하는 surface meta 키.
pub const CLAUDE_SESSION_META_KEY: &str = "claude-session-id";

/// 호스트 IPC 호출 추상화 — 단위 테스트가 스텁을 끼울 수 있게 trait 뒤에 둔다.
pub trait HostCall {
    fn call(&self, method: &str, params: Value) -> Result<Value, PluginError>;
}

impl HostCall for HostHandle {
    fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        HostHandle::call(self, method, params)
    }
}

/// 세션과 기록 경로를 찾지 못한 이유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// 대상 surface 에 세션 id meta 가 없다.
    NoSessionMeta { surface_id: u32 },
    /// transcript 루트 디렉토리를 특정할 수 없다(홈 디렉토리 미확인 등).
    TranscriptRootMissing,
    /// 루트는 있으나 그 세션의 파일이 아직 없다.
    TranscriptNotFound { session_id: String },
    /// host IPC 호출 자체가 실패했다.
    HostCall { message: String },
}

/// 세션 id를 읽는다. 없으면 어떤 기록을 읽을지 정할 수 없어 NoSessionMeta를 반환한다.
pub(crate) fn session_id_for_surface<H: HostCall>(
    host: &H,
    surface_id: u32,
) -> Result<String, ResolveError> {
    let resp = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": surface_id, "key": CLAUDE_SESSION_META_KEY }),
        )
        .map_err(|e| ResolveError::HostCall {
            message: e.to_string(),
        })?;
    resp.get("value")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or(ResolveError::NoSessionMeta { surface_id })
}

/// surface 존재 여부를 조회한다. 공용 대상 부재 오류는 false로 해석한다.
/// 그 외의 IPC 실패는 일시적일 수 있어 true로 처리하고 추적을 유지한다.
/// 오류 코드 -32602만으로는 다른 인자 오류와 구분할 수 없어
/// tasty_utils::target의 공용 메시지 판정을 사용한다.
pub(crate) fn surface_exists<H: HostCall>(host: &H, surface_id: u32) -> bool {
    match host.call("surface.locate", json!({ "surface_id": surface_id })) {
        Ok(r) => r.get("exists").and_then(Value::as_bool).unwrap_or(true),
        Err(e) => !rejects_as_no_live_surface(&e.to_string(), surface_id),
    }
}

/// 공용 대상 부재 오류인지 확인한다.
fn rejects_as_no_live_surface(message: &str, surface_id: u32) -> bool {
    tasty_utils::target::says_no_live_target(message, "surface", u64::from(surface_id))
}

/// transcript 루트 디렉토리. `CLAUDE_CONFIG_DIR` 이 설정돼 있으면 그 아래 `projects`,
/// 아니면 홈의 `.claude/projects`.
pub(crate) fn transcript_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("projects"));
    }
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.join(".claude").join("projects"))
}

/// 루트와 바로 아래 프로젝트 디렉터리에서 처음 발견한 <session_id>.jsonl을 반환한다.
pub(crate) fn find_transcript(root: &Path, session_id: &str) -> Option<PathBuf> {
    let file_name = format!("{session_id}.jsonl");
    let direct = root.join(&file_name);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let candidate = entry.path().join(&file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 루트 탐색까지 묶은 편의 함수.
pub(crate) fn transcript_path(session_id: &str) -> Result<PathBuf, ResolveError> {
    let root = transcript_root().ok_or(ResolveError::TranscriptRootMissing)?;
    find_transcript(&root, session_id).ok_or_else(|| ResolveError::TranscriptNotFound {
        session_id: session_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 호출 기록을 남기는 host 스텁.
    struct StubHost {
        meta_value: Option<Value>,
        locate_exists: Option<bool>,
        fail: bool,
        calls: RefCell<Vec<String>>,
    }

    impl StubHost {
        fn with_meta(value: Option<&str>) -> Self {
            Self {
                meta_value: Some(json!({ "value": value })),
                locate_exists: None,
                fail: false,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl HostCall for StubHost {
        fn call(&self, method: &str, _params: Value) -> Result<Value, PluginError> {
            self.calls.borrow_mut().push(method.to_string());
            if self.fail {
                return Err(PluginError::HostClosed);
            }
            match method {
                "surface.meta.get" => Ok(self.meta_value.clone().unwrap_or(json!({}))),
                "surface.locate" => Ok(json!({ "exists": self.locate_exists.unwrap_or(true) })),
                other => panic!("unexpected host call {other}"),
            }
        }
    }

    #[test]
    fn reads_the_session_id_from_surface_meta() {
        let host = StubHost::with_meta(Some("5ff72b70-4a0a-4530-b3ca-ab4159b3ca24"));
        let id = session_id_for_surface(&host, 657).expect("session id");
        assert_eq!(id, "5ff72b70-4a0a-4530-b3ca-ab4159b3ca24");
        assert_eq!(host.calls.borrow().as_slice(), ["surface.meta.get"]);
    }

    #[test]
    fn missing_meta_is_an_explicit_error() {
        let host = StubHost::with_meta(None);
        assert_eq!(
            session_id_for_surface(&host, 42),
            Err(ResolveError::NoSessionMeta { surface_id: 42 })
        );
    }

    #[test]
    fn empty_meta_is_treated_as_missing() {
        let host = StubHost::with_meta(Some(""));
        assert_eq!(
            session_id_for_surface(&host, 42),
            Err(ResolveError::NoSessionMeta { surface_id: 42 })
        );
    }

    #[test]
    fn host_failure_is_reported_as_such_not_as_missing_meta() {
        let mut host = StubHost::with_meta(Some("s"));
        host.fail = true;
        match session_id_for_surface(&host, 1) {
            Err(ResolveError::HostCall { .. }) => {}
            other => panic!("expected HostCall error, got {other:?}"),
        }
    }

    #[test]
    fn liveness_defaults_to_alive_when_the_host_cannot_answer() {
        let mut host = StubHost::with_meta(None);
        host.fail = true;
        assert!(surface_exists(&host, 1));
    }

    #[test]
    fn liveness_reports_a_closed_surface() {
        let mut host = StubHost::with_meta(None);
        host.locate_exists = Some(false);
        assert!(!surface_exists(&host, 1));
    }

    #[test]
    fn finds_the_transcript_by_scanning_project_dirs_without_computing_the_slug() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // slug 규칙을 흉내 낼 수 없는 이름들 — 탐색 방식이라 상관없다.
        std::fs::create_dir_all(root.join("-home-user-a")).expect("mkdir");
        std::fs::create_dir_all(root.join("-home-user-b-worktree-wt-1")).expect("mkdir");
        let wanted = root.join("-home-user-b-worktree-wt-1").join("sess-1.jsonl");
        std::fs::write(&wanted, b"{}\n").expect("write");

        assert_eq!(find_transcript(root, "sess-1"), Some(wanted));
    }

    #[test]
    fn finds_a_transcript_placed_directly_under_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wanted = dir.path().join("sess-2.jsonl");
        std::fs::write(&wanted, b"{}\n").expect("write");
        assert_eq!(find_transcript(dir.path(), "sess-2"), Some(wanted));
    }

    #[test]
    fn returns_none_when_the_session_file_does_not_exist_yet() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("-proj")).expect("mkdir");
        assert_eq!(find_transcript(dir.path(), "not-written-yet"), None);
    }

    #[test]
    fn a_directory_named_like_the_transcript_is_not_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("-proj").join("sess-3.jsonl")).expect("mkdir");
        assert_eq!(find_transcript(dir.path(), "sess-3"), None);
    }

    /// 호스트의 대상 부재 오류 예시. 오류 문구 판정에 사용하는 fixture다.
    const HOST_REJECTION: &str = "host call 'surface.locate' failed: no live surface 424242 \
         (named by 'surface.locate'); list the resource to get a live id — a named target is \
         never resolved by focus";

    /// 대상 부재 오류는 조회 실패와 구분해야 한다.
    #[test]
    fn a_no_live_surface_rejection_is_read_as_absence() {
        assert!(
            rejects_as_no_live_surface(HOST_REJECTION, 424_242),
            "호스트의 대상 부재 오류를 인식해야 한다"
        );
    }

    /// 그 외 실패는 대상 부재로 단정하지 않는다.
    #[test]
    fn other_host_failures_are_not_read_as_absence() {
        for msg in [
            "host call 'surface.locate' timed out after 5s",
            "io: connection reset by peer",
            "host call 'surface.locate' failed: internal error",
            // 다른 surface 를 지목한 거절은 이 surface 의 답이 아니다.
            "host call 'surface.locate' failed: no live surface 999 (named by 'surface.locate')",
        ] {
            assert!(
                !rejects_as_no_live_surface(msg, 424_242),
                "다른 조회 실패를 대상 부재로 해석해서는 안 된다: {msg}"
            );
        }
    }
}
