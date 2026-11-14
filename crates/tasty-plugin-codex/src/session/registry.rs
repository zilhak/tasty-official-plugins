//! Tasty가 실행한 Codex 세션의 세대 등록부.
//!
//! surface에서 세션 helper가 시작될 때마다 새 세대(generation)를 발급한다. 세대 디렉터리에는 등록
//! 정보(`session.json`, 플러그인만 씀)와 helper 상태(`helper.json`, helper만 씀)가 있다.
//! `surfaces/<surface>.json`이 그 surface의 현재 세대를, `threads/<thread>.json`이 thread를
//! 실행 중인 세대를 가리킨다. 훅과 전달은 현재 세대만 신뢰한다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rpc::Endpoint;
use crate::store;

/// 턴 ID와 그 턴 프롬프트의 agent task 회차 표지.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TurnAttempt {
    pub turn_id: String,
    pub attempt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Session {
    pub generation: String,
    pub surface: u32,
    /// 이 세대 서버에서 마지막으로 시작된 thread.
    pub thread: Option<String>,
    /// TUI에 넘긴 정책·모델 인자. 복원 명령이 같은 값을 다시 쓴다.
    pub tui_args: Vec<String>,
    #[serde(default)]
    pub native_args: Option<Vec<String>>,
    /// SessionStart 훅이 알려 준 모델.
    #[serde(default)]
    pub model: Option<String>,
    /// 회차 표지가 있는 프롬프트의 턴. Stop이 자기 턴의 표지를 찾는다.
    #[serde(default)]
    pub turn_attempts: Vec<TurnAttempt>,
    /// helper가 서버에 넘길 listen URL.
    pub listen: String,
    /// loopback WebSocket의 capability token 파일.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// 경로 길이 때문에 세대 디렉터리 밖에 둔 socket 디렉터리.
    #[serde(default)]
    pub socket_dir: Option<PathBuf>,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HelperPhase {
    Starting,
    Ready,
    Failed,
    Exited,
}

/// helper가 쓰는 실행 상태.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct HelperState {
    pub phase: HelperPhase,
    /// 준비된 서버의 실제 URL(포트 0으로 띄운 WebSocket은 실제 포트).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    /// 서버가 `thread/resume`으로 다시 연 thread. 요청한 승인·sandbox·모델을 서버 응답으로
    /// 확인한 뒤에만 기록한다. 새 세션이거나 아직 확인 전이면 None이다.
    #[serde(default)]
    pub resumed: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pointer {
    generation: String,
}

pub(crate) fn session_dir(root: &Path, generation: &str) -> PathBuf {
    root.join("sessions").join(generation)
}

pub(crate) fn helper_file(state_dir: &Path) -> PathBuf {
    state_dir.join("helper.json")
}

fn session_file(root: &Path, generation: &str) -> PathBuf {
    session_dir(root, generation).join("session.json")
}

fn surface_file(root: &Path, surface: u32) -> PathBuf {
    root.join("surfaces").join(format!("{surface}.json"))
}

fn thread_file(root: &Path, thread: &str) -> PathBuf {
    root.join("threads")
        .join(format!("{}.json", thread.to_ascii_lowercase()))
}

pub(crate) fn read_session(root: &Path, generation: &str) -> std::io::Result<Option<Session>> {
    if !crate::ids::is_generation(generation) {
        return Ok(None);
    }
    store::read_json(&session_file(root, generation))
}

pub(crate) fn write_session(root: &Path, session: &Session) -> std::io::Result<()> {
    store::write_json(&session_file(root, &session.generation), session)
}

pub(crate) fn read_helper(root: &Path, generation: &str) -> std::io::Result<Option<HelperState>> {
    store::read_json(&helper_file(&session_dir(root, generation)))
}

pub(crate) fn current_gen(root: &Path, surface: u32) -> std::io::Result<Option<String>> {
    Ok(store::read_json::<Pointer>(&surface_file(root, surface))?.map(|p| p.generation))
}

/// surface의 현재 세대가 gen이면 그 등록을 돌려준다.
pub(crate) fn current_session(
    root: &Path,
    surface: u32,
    generation: &str,
) -> std::io::Result<Option<Session>> {
    if current_gen(root, surface)?.as_deref() != Some(generation) {
        return Ok(None);
    }
    Ok(read_session(root, generation)?.filter(|s| s.surface == surface))
}

/// thread를 실행 중인 현재 세대 등록. 다른 세대로 바뀌었거나 없으면 None이다.
pub(crate) fn thread_session(root: &Path, thread: &str) -> std::io::Result<Option<Session>> {
    let Some(pointer) = store::read_json::<Pointer>(&thread_file(root, thread))? else {
        return Ok(None);
    };
    let Some(session) = read_session(root, &pointer.generation)? else {
        return Ok(None);
    };
    if current_gen(root, session.surface)?.as_deref() != Some(pointer.generation.as_str()) {
        return Ok(None);
    }
    Ok(Some(session))
}

pub(crate) fn bind_thread(root: &Path, thread: &str, generation: &str) -> std::io::Result<()> {
    store::write_json(
        &thread_file(root, thread),
        &Pointer {
            generation: generation.into(),
        },
    )
}

/// thread 색인이 gen을 가리킬 때만 지운다. 이미 다른 세대가 이어받았으면 그대로 둔다.
pub(crate) fn unbind_thread(root: &Path, thread: &str, generation: &str) -> std::io::Result<()> {
    let path = thread_file(root, thread);
    if store::read_json::<Pointer>(&path)?.is_some_and(|p| p.generation == generation) {
        store::remove_file(&path)?;
    }
    Ok(())
}

/// unix socket 경로의 안전한 최대 길이(sun_path 104~108바이트보다 짧게 둔다).
#[cfg(unix)]
const SOCKET_PATH_MAX: usize = 100;

/// 새 세대를 등록한다. 세대 디렉터리와 listen 주소는 Tasty가 만든 것만 쓴다.
pub(crate) fn open_session(
    root: &Path,
    surface: u32,
    resume: Option<&str>,
    tui_args: Vec<String>,
) -> std::io::Result<Session> {
    let generation = crate::ids::new_generation()?;
    let dir = session_dir(root, &generation);
    store::private_dir(&root.join("sessions"))?;
    store::private_dir_new(&dir)?;
    let (listen, token_file, socket_dir) = listen_address(&dir, &generation)?;
    let previous = current_gen(root, surface)?;
    let session = Session {
        generation: generation.clone(),
        surface,
        thread: resume.map(str::to_ascii_lowercase),
        tui_args,
        native_args: None,
        model: None,
        turn_attempts: Vec::new(),
        listen,
        token_file,
        socket_dir,
        created_at_ms: store::now_ms(),
    };
    write_session(root, &session)?;
    store::write_json(
        &surface_file(root, surface),
        &Pointer {
            generation: generation.clone(),
        },
    )?;
    if let Some(thread) = &session.thread {
        bind_thread(root, thread, &generation)?;
    }
    if let Some(old) = previous.filter(|old| old != &generation) {
        retire(root, &old);
    }
    Ok(session)
}

/// 대체된 세대의 파일을 지운다. helper가 아직 준비·실행 상태라고 기록했다면 그 서버가
/// 남아 있을 수 있어 지우지 않는다.
fn retire(root: &Path, generation: &str) {
    if !helper_stopped(root, generation) {
        return;
    }
    remove_socket_dir(root, generation);
    if let Err(e) = std::fs::remove_dir_all(session_dir(root, generation))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("codex session {generation}: cleanup: {e}");
    }
}

/// helper가 준비·실행 상태라고 기록하지 않았는가. 기록을 읽지 못하면 남아 있다고 본다.
fn helper_stopped(root: &Path, generation: &str) -> bool {
    match read_helper(root, generation) {
        Ok(Some(h)) if matches!(h.phase, HelperPhase::Starting | HelperPhase::Ready) => false,
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("codex session {generation}: helper state unreadable, kept: {e}");
            false
        }
    }
}

/// 세대가 사용자 전용 런타임 폴더에 따로 만든 socket 디렉터리를 지운다.
fn remove_socket_dir(root: &Path, generation: &str) {
    if let Ok(Some(session)) = read_session(root, generation)
        && let Some(dir) = session.socket_dir
        && let Err(e) = std::fs::remove_dir_all(&dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("codex session {generation}: socket dir cleanup: {e}");
    }
}

#[cfg(unix)]
fn listen_address(
    dir: &Path,
    generation: &str,
) -> std::io::Result<(String, Option<PathBuf>, Option<PathBuf>)> {
    let in_dir = dir.join("s.sock");
    if in_dir.as_os_str().len() <= SOCKET_PATH_MAX {
        return Ok((format!("unix://{}", in_dir.display()), None, None));
    }
    // 데이터 경로가 길면 사용자 전용 런타임 폴더에 세대별 디렉터리를 새로 만든다.
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir);
    let socket_dir = base.join(format!("tcx-{generation}"));
    store::private_dir_new(&socket_dir)?;
    let path = socket_dir.join("s.sock");
    if path.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(std::io::Error::other(format!(
            "no socket path shorter than {SOCKET_PATH_MAX} bytes"
        )));
    }
    Ok((format!("unix://{}", path.display()), None, Some(socket_dir)))
}

/// Windows는 unix socket 대신 loopback WebSocket에 capability token을 요구한다. 포트는
/// 서버가 고르고 helper가 실제 포트를 기록한다.
#[cfg(not(unix))]
fn listen_address(
    dir: &Path,
    _gen: &str,
) -> std::io::Result<(String, Option<PathBuf>, Option<PathBuf>)> {
    let token_file = dir.join("token");
    let token = crate::ids::random_hex(32)?;
    if !store::create_claim(&token_file, &token)? {
        return Err(std::io::Error::other("token file already exists"));
    }
    Ok(("ws://127.0.0.1:0".into(), Some(token_file), None))
}

/// 준비된 helper의 서버 endpoint. 준비 전이거나 종료했으면 그 사유를 돌려준다.
pub(crate) fn ready_endpoint(root: &Path, session: &Session) -> Result<Endpoint, String> {
    let helper = match read_helper(root, &session.generation) {
        Ok(Some(h)) => h,
        Ok(None) => return Err("session helper has not started".into()),
        Err(e) => return Err(format!("session helper state unreadable: {e}")),
    };
    if helper.phase != HelperPhase::Ready {
        return Err(format!("session helper is {:?}", helper.phase).to_ascii_lowercase());
    }
    let url = helper.url.unwrap_or_else(|| session.listen.clone());
    parse_endpoint(&url, session.token_file.as_deref())
}

pub(crate) fn parse_endpoint(url: &str, token_file: Option<&Path>) -> Result<Endpoint, String> {
    if let Some(path) = url.strip_prefix("unix://") {
        return Ok(Endpoint::Unix(PathBuf::from(path)));
    }
    if let Some(rest) = url.strip_prefix("ws://127.0.0.1:") {
        let port: u16 = rest
            .parse()
            .map_err(|_| format!("unexpected endpoint {url}"))?;
        let file = token_file.ok_or("websocket endpoint without a token file")?;
        let token = std::fs::read_to_string(file)
            .map_err(|e| format!("token file unreadable: {e}"))?
            .trim()
            .to_string();
        return Ok(Endpoint::Ws { port, token });
    }
    Err(format!("unexpected endpoint {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(root: &Path, s: &Session) {
        store::write_json(
            &helper_file(&session_dir(root, &s.generation)),
            &HelperState {
                phase: HelperPhase::Ready,
                url: Some(s.listen.clone()),
                detail: None,
                resumed: None,
                updated_at_ms: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn a_new_generation_replaces_the_surface_and_thread_routes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let thread = "01a12f74-dab9-7a00-8000-0123456789ab";
        let first = open_session(root, 7, Some(thread), vec!["-a".into(), "never".into()]).unwrap();
        assert_eq!(
            current_gen(root, 7).unwrap().as_deref(),
            Some(first.generation.as_str())
        );
        assert_eq!(
            thread_session(root, thread).unwrap().unwrap().generation,
            first.generation
        );
        assert!(
            current_session(root, 7, &first.generation)
                .unwrap()
                .is_some()
        );
        assert!(
            current_session(root, 8, &first.generation)
                .unwrap()
                .is_none()
        );

        let second = open_session(root, 7, Some(thread), vec![]).unwrap();
        assert_ne!(first.generation, second.generation);
        assert!(
            current_session(root, 7, &first.generation)
                .unwrap()
                .is_none(),
            "이전 세대는 무효"
        );
        assert_eq!(
            thread_session(root, thread).unwrap().unwrap().generation,
            second.generation
        );
        // 이전 helper가 시작 기록을 남기지 않았으므로 정리된다.
        assert!(!session_dir(root, &first.generation).exists());

        unbind_thread(root, thread, &first.generation).unwrap();
        assert!(
            thread_session(root, thread).unwrap().is_some(),
            "다른 세대 색인은 유지"
        );
        unbind_thread(root, thread, &second.generation).unwrap();
        assert!(thread_session(root, thread).unwrap().is_none());
    }

    #[test]
    fn a_running_previous_generation_is_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let first = open_session(root, 9, None, vec![]).unwrap();
        ready(root, &first);
        open_session(root, 9, None, vec![]).unwrap();
        assert!(session_dir(root, &first.generation).exists());
    }

    #[test]
    fn only_a_ready_helper_yields_an_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let s = open_session(root, 3, None, vec![]).unwrap();
        assert!(
            ready_endpoint(root, &s)
                .unwrap_err()
                .contains("not started")
        );
        store::write_json(
            &helper_file(&session_dir(root, &s.generation)),
            &HelperState {
                phase: HelperPhase::Exited,
                url: None,
                detail: None,
                resumed: None,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        assert!(ready_endpoint(root, &s).unwrap_err().contains("exited"));
        ready(root, &s);
        assert!(ready_endpoint(root, &s).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn unix_listen_paths_stay_short_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let long = dir.path().join("x".repeat(90));
        let s = open_session(&long, 1, None, vec![]).unwrap();
        let path = s.listen.strip_prefix("unix://").unwrap();
        assert!(path.len() <= SOCKET_PATH_MAX, "{path}");
        let sock_dir = s
            .socket_dir
            .clone()
            .expect("긴 데이터 경로는 별도 socket 디렉터리");
        assert_eq!(
            std::fs::metadata(&sock_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir_all(sock_dir).unwrap();
    }

    #[test]
    fn endpoints_parse_only_tasty_forms() {
        assert_eq!(
            parse_endpoint("unix:///a/s.sock", None),
            Ok(Endpoint::Unix("/a/s.sock".into()))
        );
        assert!(parse_endpoint("ws://10.0.0.1:80", None).is_err());
        assert!(
            parse_endpoint("ws://127.0.0.1:80", None).is_err(),
            "token 파일 필수"
        );
    }
}
