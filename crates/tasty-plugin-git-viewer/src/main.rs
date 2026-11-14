#![forbid(unsafe_code)]

//! Git 상태·이력·차이를 읽어 보여 주는 egui-mesh 팝업.
//! 로컬 저장소는 직접 읽고 원격 미러는 호스트 IPC로 조회한다.
//! 호스트가 팝업 외곽과 입력을 관리하며 플러그인은 받은 테마로 내용을 그린다.

mod render;

use std::path::PathBuf;

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tasty_git_core as git;
use tasty_plugin_protocol::ThemeWire;
use tasty_plugin_sdk::{
    BusHandle, EventDispatchCtx, HostHandle, Plugin, PluginEnv, PopupClosedCtx, PopupOpenCtx,
    PopupOpenResult, PopupSetContextCtx, SurfaceCreateCtx, SurfaceResult, Translator,
};
use tasty_type_appearance::theme::Theme;

use tasty_plugin_sdk::EguiMeshPopup;

const PLUGIN_ID: &str = "com.tasty.git-viewer";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
const LOG_LIMIT: usize = 200;
/// 호스트가 조회 결과를 전달할 이벤트 이름. 양쪽 정의를 함께 갱신해야 한다.
const GIT_VIEWER_QUERY_RESULT_EVENT: &str = "git_viewer.query_result";

#[derive(Default)]
pub(crate) struct ViewerState {
    /// 상태·이력·차이를 조회할 선택된 워크트리 경로.
    repo_path: Option<PathBuf>,
    /// 팝업을 처음 연 위치. is_current 표시의 기준으로 유지한다.
    current_workdir: Option<PathBuf>,
    /// main + 모든 linked worktree 종합 목록.
    worktrees: Vec<git::WorktreeEntry>,
    /// `worktrees` 내 활성 항목 인덱스.
    active_worktree: usize,
    error: Option<String>,
    status_entries: Vec<git::StatusEntry>,
    log_entries: Vec<git::LogEntry>,
    selected_file: Option<usize>,
    diff_content: Option<git::DiffData>,
    /// 글꼴 크기별 diff의 전체 너비 캐시. 보이는 줄만 재면 스크롤 범위가 흔들린다.
    /// set_diff가 내용을 바꿀 때 비우고 렌더링할 때 채운다.
    diff_width: Option<(f32, f32)>,
    /// 원격 미러면 호스트 IPC로 조회한다. 로컬 모드는 None이다.
    remote: Option<RemoteCtx>,
    /// 원격 응답을 기다리는 동안 loading을 표시한다.
    loading: bool,
    /// 선택한 로컬 저장소의 핸들. take_repo/put_repo로 사용하며 원격 모드에서는 없다.
    repo: Option<CachedRepo>,
}

/// 저장소 핸들과 경로. SDK의 단일 worker가 &mut self로 접근하므로 동시에 공유하지 않는다.
struct CachedRepo {
    /// 이 핸들이 가리키는 working dir. 요청 경로와 다르면 캐시 미스로 본다.
    workdir: PathBuf,
    handle: git2::Repository,
}

/// 원격 조회에 사용할 호스트와 대기 중 요청.
struct RemoteCtx {
    host: HostHandle,
    consumer: u64,
    /// 호스트가 원격 ID로 변환할 로컬 미러 surface ID.
    local_surface_id: u32,
    /// 마지막 요청 ID. 다른 ID의 응답은 버린다(연결 종료 알림의 0은 별도 처리).
    pending_request_id: Option<u64>,
    /// 대기 중 diff 요청의 파일 인덱스. 응답 kind가 diff일 때 사용한다.
    pending_diff_idx: Option<usize>,
}

/// 호스트의 원격 Git 조회 결과.
#[derive(serde::Deserialize)]
struct GitQueryReplyWire {
    request_id: u64,
    ok: bool,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    data: Option<Value>,
    /// 서버가 크기 제한으로 내용을 잘랐는지 여부. 현재 화면에는 별도 안내를 표시하지 않는다.
    #[serde(default)]
    #[allow(dead_code)]
    truncated: bool,
    #[serde(default)]
    reason: Option<String>,
}

/// 원격 스냅샷 데이터. 항목 타입은 tasty-git-core와 공유한다.
#[derive(serde::Deserialize)]
struct SnapshotWire {
    active_worktree_path: String,
    worktrees: Vec<git::WorktreeEntry>,
    status_entries: Vec<git::StatusEntry>,
    log_entries: Vec<git::LogEntry>,
}

impl ViewerState {
    /// 원격 스냅샷을 요청하고 응답을 기다리는 상태로 시작한다.
    fn new_remote(host: HostHandle, local_surface_id: u32, consumer: u64) -> Self {
        let mut s = ViewerState {
            remote: Some(RemoteCtx {
                host,
                consumer,
                local_surface_id,
                pending_request_id: None,
                pending_diff_idx: None,
            }),
            ..Default::default()
        };
        s.request_remote_snapshot(None);
        s
    }

    /// 원격 스냅샷을 요청한다. 받은 서버 경로는 로컬 경로로 해석하지 않고 그대로 돌려보낸다.
    /// 경로가 없으면 서버가 해당 surface의 원격 cwd에서 저장소를 찾는다.
    fn request_remote_snapshot(&mut self, worktree_path: Option<String>) {
        let Some(remote) = self.remote.as_mut() else {
            return;
        };
        let params = serde_json::json!({
            "kind": "snapshot",
            "local_surface_id": remote.local_surface_id,
            "consumer": remote.consumer,
            "worktree_path": worktree_path,
        });
        match remote.host.call("git_viewer.query", params) {
            Ok(v) => {
                remote.pending_request_id = v.get("request_id").and_then(Value::as_u64);
                remote.pending_diff_idx = None;
                self.loading = true;
                self.error = None;
            }
            Err(e) => {
                tracing::warn!("git_viewer.query(snapshot) failed: {e}");
                self.error = Some(e.to_string());
                self.loading = false;
            }
        }
    }

    /// 선택한 파일의 원격 diff를 요청한다. 응답 때도 파일 인덱스를 대조한다.
    fn request_remote_diff(&mut self, idx: usize, diff_path: String) {
        let worktree_path = self
            .worktrees
            .get(self.active_worktree)
            .map(|w| w.path.to_string_lossy().into_owned());
        let Some(remote) = self.remote.as_mut() else {
            return;
        };
        let params = serde_json::json!({
            "kind": "diff",
            "local_surface_id": remote.local_surface_id,
            "consumer": remote.consumer,
            "worktree_path": worktree_path,
            "diff_path": diff_path,
        });
        match remote.host.call("git_viewer.query", params) {
            Ok(v) => {
                remote.pending_request_id = v.get("request_id").and_then(Value::as_u64);
                remote.pending_diff_idx = Some(idx);
                self.error = None;
            }
            Err(e) => {
                tracing::warn!("git_viewer.query(diff) failed: {e}");
                self.error = Some(e.to_string());
            }
        }
    }

    /// 대기 중인 요청의 응답이나 연결 종료 알림만 반영한다.
    fn apply_remote_reply(&mut self, reply: GitQueryReplyWire) {
        let Some(remote) = self.remote.as_mut() else {
            return;
        };
        if !should_apply_remote_reply(remote.pending_request_id, reply.request_id) {
            return;
        }
        let pending_diff_idx = remote.pending_diff_idx.take();
        remote.pending_request_id = None;
        self.loading = false;

        if !reply.ok {
            self.error = Some(
                reply
                    .reason
                    .unwrap_or_else(|| "remote git query failed".to_string()),
            );
            return;
        }
        self.error = None;
        let Some(data) = reply.data else {
            self.error = Some("remote git query returned no data".to_string());
            return;
        };
        match reply.kind.as_str() {
            "snapshot" => self.apply_remote_snapshot(data),
            "diff" => {
                if let Some(idx) = pending_diff_idx {
                    self.apply_remote_diff(idx, data);
                }
            }
            other => tracing::warn!("git-viewer: unknown git_viewer.query_result kind '{other}'"),
        }
    }

    fn apply_remote_snapshot(&mut self, data: Value) {
        let wire: SnapshotWire = match serde_json::from_value(data) {
            Ok(w) => w,
            Err(e) => {
                self.error = Some(format!("malformed snapshot reply: {e}"));
                return;
            }
        };
        let active_path = PathBuf::from(&wire.active_worktree_path);
        // 현재 위치 배지는 처음 연 위치를 가리킨다. 워크트리 선택으로 바꾸지 않는다.
        if self.current_workdir.is_none() {
            self.current_workdir = Some(active_path.clone());
        }
        self.repo_path = Some(active_path.clone());
        let mut worktrees = wire.worktrees;
        if let Some(pinned) = &self.current_workdir {
            for w in &mut worktrees {
                w.is_current = &w.path == pinned;
            }
        }
        self.active_worktree = worktrees
            .iter()
            .position(|w| w.path == active_path)
            .unwrap_or(0);
        self.worktrees = worktrees;
        self.status_entries = wire.status_entries;
        self.log_entries = wire.log_entries;
        // 파일 목록이 바뀌었을 수 있으므로 기존 diff를 닫는다.
        self.selected_file = None;
        self.set_diff(None);
    }

    fn apply_remote_diff(&mut self, idx: usize, data: Value) {
        if self.selected_file != Some(idx) {
            // 사용자가 이미 다른 파일을 선택했거나 Back 으로 diff 를 닫음 — stale.
            return;
        }
        match serde_json::from_value::<git::DiffData>(data) {
            Ok(diff) => self.set_diff(Some(diff)),
            Err(e) => self.error = Some(format!("malformed diff reply: {e}")),
        }
    }

    fn load(cwd: Option<&std::path::Path>) -> Self {
        let mut s = ViewerState::default();
        let Some(cwd) = cwd else {
            return s;
        };
        let Some(repo) = git::discover_repo(cwd) else {
            return s;
        };
        let current_wd = repo
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| repo.path().to_path_buf());
        s.current_workdir = Some(current_wd.clone());
        s.worktrees = git::collect_worktrees(&repo, &current_wd).unwrap_or_default();
        s.active_worktree = s.worktrees.iter().position(|w| w.is_current).unwrap_or(0);

        // 처음 연 저장소가 선택된 워크트리면 핸들을 재사용한다.
        let active_is_current = s
            .worktrees
            .get(s.active_worktree)
            .is_some_and(|w| w.is_current);
        if s.worktrees.is_empty() || active_is_current {
            s.repo_path = Some(current_wd.clone());
            s.refresh_collections(&repo);
            s.put_repo(current_wd, repo);
        } else {
            s.bind_active();
        }
        s
    }

    /// 선택한 워크트리의 상태와 이력을 다시 읽는다.
    fn bind_active(&mut self) {
        let Some(path) = self
            .worktrees
            .get(self.active_worktree)
            .map(|e| e.path.clone())
            .or_else(|| self.repo_path.clone())
        else {
            return;
        };
        let Some(repo) = self.take_repo(&path) else {
            self.error = Some(format!("repo lost at {}", path.display()));
            return;
        };
        self.repo_path = repo
            .workdir()
            .map(|p| p.to_path_buf())
            .or(Some(path.clone()));
        self.refresh_collections(&repo);
        self.put_repo(self.repo_path.clone().unwrap_or(path), repo);
    }

    /// 조회 대상을 바꾼다. checkout이나 작업 경로 변경은 하지 않으며 유효하지 않은 항목은 거부한다.
    fn select_worktree(&mut self, idx: usize) {
        let Some(entry) = self.worktrees.get(idx) else {
            return;
        };
        if !entry.is_valid || idx == self.active_worktree {
            return;
        }
        let path = entry.path.to_string_lossy().into_owned();
        self.selected_file = None;
        self.set_diff(None);
        self.error = None;
        // 원격 모드에서도 이전 로컬 저장소 핸들이 남지 않게 비운다.
        self.repo = None;
        if self.remote.is_some() {
            self.active_worktree = idx;
            self.request_remote_snapshot(Some(path));
            return;
        }
        self.active_worktree = idx;
        self.bind_active();
    }

    fn refresh(&mut self) {
        if self.remote.is_some() {
            let worktree_path = self
                .worktrees
                .get(self.active_worktree)
                .map(|w| w.path.to_string_lossy().into_owned());
            self.request_remote_snapshot(worktree_path);
            return;
        }
        // 명시적인 새로고침에서는 저장소를 다시 연다.
        self.repo = None;

        // 외부에서 추가·삭제한 워크트리도 다시 수집한다.
        if let Some(current_wd) = self.current_workdir.clone() {
            let prev_active = self
                .worktrees
                .get(self.active_worktree)
                .map(|e| e.path.clone());
            if let Some(repo) = git::discover_repo(&current_wd) {
                if let Ok(v) = git::collect_worktrees(&repo, &current_wd)
                    && !v.is_empty()
                {
                    self.worktrees = v;
                    // 목록이 바뀌었을 수 있으니 이전 활성 경로로 인덱스 보정.
                    self.active_worktree = prev_active
                        .and_then(|p| self.worktrees.iter().position(|e| e.path == p))
                        .or_else(|| self.worktrees.iter().position(|w| w.is_current))
                        .unwrap_or(0);
                }
                // 이번에 연 저장소가 선택된 대상이면 다시 열지 않고 넘긴다.
                if self.repo_path.as_deref() == Some(current_wd.as_path()) {
                    self.put_repo(current_wd, repo);
                }
            }
        }

        let Some(path) = self.repo_path.clone() else {
            return;
        };
        let Some(repo) = self.take_repo(&path) else {
            self.error = Some(format!("repo lost at {}", path.display()));
            return;
        };
        self.error = None;
        self.refresh_collections(&repo);
        if let Some(idx) = self.selected_file {
            if let Some(entry) = self.status_entries.get(idx).cloned() {
                self.load_diff_for(&repo, &entry.path);
            } else {
                self.selected_file = None;
                self.set_diff(None);
            }
        }
        self.put_repo(path, repo);
    }

    fn refresh_collections(&mut self, repo: &git2::Repository) {
        match git::collect_status(repo) {
            Ok(v) => self.status_entries = v,
            Err(e) => {
                tracing::warn!("collect_status failed: {e}");
                self.error = Some(e.to_string());
            }
        }
        match git::collect_log(repo, LOG_LIMIT) {
            Ok(v) => self.log_entries = v,
            Err(e) => {
                tracing::warn!("collect_log failed: {e}");
                self.error = Some(e.to_string());
            }
        }
    }

    fn load_diff(&mut self, idx: usize) {
        let Some(entry) = self.status_entries.get(idx).cloned() else {
            return;
        };
        if self.remote.is_some() {
            self.selected_file = Some(idx);
            self.request_remote_diff(idx, entry.path);
            return;
        }
        let Some(path) = self.repo_path.clone() else {
            return;
        };
        let Some(repo) = self.take_repo(&path) else {
            return;
        };
        self.selected_file = Some(idx);
        self.load_diff_for(&repo, &entry.path);
        self.put_repo(path, repo);
    }

    fn load_diff_for(&mut self, repo: &git2::Repository, path: &str) {
        match git::collect_diff(repo, path) {
            Ok(d) => self.set_diff(Some(d)),
            Err(e) => {
                tracing::warn!("collect_diff failed: {e}");
                self.error = Some(e.to_string());
                self.set_diff(None);
            }
        }
    }

    fn close_diff(&mut self) {
        self.selected_file = None;
        self.set_diff(None);
    }

    /// diff를 바꿀 때 너비 캐시도 함께 비운다.
    fn set_diff(&mut self, diff: Option<git::DiffData>) {
        self.diff_content = diff;
        self.diff_width = None;
    }

    /// 경로가 같은 캐시 핸들을 꺼내거나 저장소를 새로 연다.
    /// 재사용하려면 호출자가 사용 후 put_repo로 돌려놓아야 한다.
    fn take_repo(&mut self, workdir: &std::path::Path) -> Option<git2::Repository> {
        if let Some(cached) = self.repo.take()
            && cached.workdir == workdir
        {
            return Some(cached.handle);
        }
        git::discover_repo(workdir)
    }

    /// [`Self::take_repo`] 로 꺼낸 핸들을 캐시에 돌려놓는다.
    fn put_repo(&mut self, workdir: PathBuf, handle: git2::Repository) {
        self.repo = Some(CachedRepo { workdir, handle });
    }
}

struct GitViewerPlugin {
    /// 첫 인스턴스만 내용을 표시하고 추가 인스턴스에는 이미 열림을 안내한다.
    primary: Option<u64>,
    /// primary 인스턴스의 상태.
    state: Option<ViewerState>,
    /// popup instance_id → egui-mesh 렌더 상태(폰트 atlas·shared buffer 소유).
    popups: HashMap<u64, EguiMeshPopup>,
    /// CJK fallback 폰트를 이미 설치한 popup instance_id.
    fonts_installed: HashSet<u64>,
    tr: Translator,
    /// 시작할 때 받은 호스트 핸들. 원격 조회 상태를 만들 때 복제해 넘긴다.
    host: Option<HostHandle>,
}

impl GitViewerPlugin {
    fn new(env: &PluginEnv) -> Self {
        Self {
            primary: None,
            state: None,
            popups: HashMap::new(),
            fonts_installed: HashSet::new(),
            tr: Translator::from_plugin_env(env),
            host: None,
        }
    }
}

fn cwd_from_context(context: &Value) -> Option<PathBuf> {
    context
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// 대기 중인 요청 ID와 일치하는 응답만 받는다.
/// 0은 연결 종료 알림이므로 어떤 요청이든 대기 중이면 적용해 대기를 끝낸다.
fn should_apply_remote_reply(pending_request_id: Option<u64>, reply_request_id: u64) -> bool {
    if reply_request_id == 0 {
        pending_request_id.is_some()
    } else {
        pending_request_id == Some(reply_request_id)
    }
}

impl Plugin for GitViewerPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn create_surface(&mut self, _ctx: SurfaceCreateCtx) -> SurfaceResult {
        SurfaceResult::default()
    }

    fn open_popup(&mut self, ctx: PopupOpenCtx) -> PopupOpenResult {
        // 첫 인스턴스만 상태를 읽는다. 추가 인스턴스에는 이미 열림을 안내한다.
        if self.primary.is_none() {
            self.primary = Some(ctx.instance_id);
            // 미러 surface면 로컬 저장소 대신 호스트를 통해 원격 저장소를 조회한다.
            let is_mirror = ctx
                .context
                .get("mirror")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let local_surface_id = ctx
                .context
                .get("local_surface_id")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            self.state = match (is_mirror, local_surface_id, self.host.clone()) {
                (true, Some(sid), Some(host)) => {
                    Some(ViewerState::new_remote(host, sid, ctx.instance_id))
                }
                _ => {
                    let cwd = cwd_from_context(&ctx.context);
                    Some(ViewerState::load(cwd.as_deref()))
                }
            };
        }
        PopupOpenResult::default()
    }

    fn paint_popup(&mut self, ctx: PopupSetContextCtx) {
        self.paint_popup_impl(ctx);
    }

    fn on_popup_closed(&mut self, ctx: PopupClosedCtx) {
        if self.primary == Some(ctx.instance_id) {
            self.primary = None;
            self.state = None;
        }
        self.popups.remove(&ctx.instance_id);
        self.fonts_installed.remove(&ctx.instance_id);
    }

    fn on_event(&mut self, ctx: EventDispatchCtx) {
        if ctx.envelope.key != GIT_VIEWER_QUERY_RESULT_EVENT {
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match serde_json::from_value::<GitQueryReplyWire>(ctx.envelope.payload) {
            Ok(reply) => state.apply_remote_reply(reply),
            Err(e) => tracing::warn!("git-viewer: malformed git_viewer.query_result event: {e}"),
        }
    }

    fn on_start(&mut self, host: HostHandle, _bus: BusHandle) {
        self.host = Some(host);
    }
}

impl GitViewerPlugin {
    /// `popup.set_context` 한 frame 을 그려 host 에 popup mesh 를 회신한다.
    fn paint_popup_impl(&mut self, ctx: PopupSetContextCtx) {
        let iid = ctx.params.instance_id;

        // host 가 Theme 을 아직 안 보냈으면 토큰을 풀 수 없으므로 이 frame 건너뜀.
        let Some(theme) = ctx.params.theme.as_ref().map(theme_from_wire) else {
            tracing::debug!("git-viewer popup {iid}: set_context without theme — skipping paint");
            return;
        };

        let is_primary = self.primary == Some(iid);
        let tr = &self.tr;
        let state = &mut self.state;
        let is_new = !self.popups.contains_key(&iid);
        let popup = self
            .popups
            .entry(iid)
            .or_insert_with(|| EguiMeshPopup::new(iid));
        if is_new {
            install_fonts(popup.context());
            self.fonts_installed.insert(iid);
        }

        let result = popup.paint(&ctx.host, &ctx.params, |egui_ctx| {
            if is_primary {
                if let Some(st) = state.as_mut() {
                    render::draw(egui_ctx, &theme, st, tr);
                }
            } else {
                render::draw_busy(egui_ctx, &theme, tr);
            }
        });
        if let Err(e) = result {
            tracing::warn!("git-viewer popup {iid} paint failed: {e}");
        }
    }
}

/// 전달받은 색·밝기·확대 비율로 테마를 만든다.
fn theme_from_wire(w: &ThemeWire) -> Theme {
    Theme::with_colors_and_zoom(w.colors.clone(), w.is_light, w.ui_zoom)
}

/// 구할 수 있는 시스템 CJK 폰트를 대체 폰트로 추가한다.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    tasty_egui_theme::install_system_cjk_fallback(&mut fonts);
    // 호스트와 같은 검사 함수로 언어팩 폰트를 대체 폰트 목록 끝에 추가한다.
    if let Some(path) = std::env::var_os("TASTY_LOCALE_FONT").filter(|v| !v.is_empty()) {
        let path = std::path::PathBuf::from(path);
        let face = tasty_egui_theme::locale_font_face_from_env();
        if let Err(e) = tasty_egui_theme::install_locale_font_fallback(&mut fonts, &path, face) {
            tracing::warn!(
                "locale font at {} could not be installed: {e}",
                path.display()
            );
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let env = PluginEnv::load()?;
    let plugin = GitViewerPlugin::new(&env);
    tasty_plugin_sdk::run(plugin)
}

#[cfg(test)]
mod remote_reply_tests {
    use super::*;

    #[test]
    fn sentinel_abandons_when_pending() {
        assert!(should_apply_remote_reply(Some(7), 0));
    }

    #[test]
    fn sentinel_ignored_when_idle() {
        assert!(!should_apply_remote_reply(None, 0));
    }

    #[test]
    fn normal_id_must_match_exactly() {
        assert!(should_apply_remote_reply(Some(7), 7));
        assert!(!should_apply_remote_reply(Some(7), 8));
        assert!(!should_apply_remote_reply(None, 7));
    }
}
