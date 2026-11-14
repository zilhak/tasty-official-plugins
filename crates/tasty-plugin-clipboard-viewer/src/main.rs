#![forbid(unsafe_code)]

//! 현재 클립보드 내용을 한 번 읽어 보여주는 팝업.
//! 읽기에는 arboard와 플랫폼 API를, 그리기에는 egui-mesh를 사용한다.
//! 텍스트·파일·이미지 정보·HTML·기타 포맷을 표시하며 클립보드를 변경하지 않는다.
//! 배경·테두리·닫기 처리는 호스트가 맡고 플러그인은 내용 영역을 그린다.

// 이유: 시험의 let _는 제품 코드에서 반환값을 버리는 목록에 포함하지 않는다.
#![cfg_attr(test, allow(clippy::let_underscore_must_use))]

mod clipboard;
mod html_format;
mod raw_formats;
mod view;

use tasty_plugin_sdk::{
    Plugin, PluginEnv, PopupClosedCtx, PopupOpenCtx, PopupOpenResult, PopupSetContextCtx,
    SurfaceCreateCtx, SurfaceResult, Translator,
};

use crate::clipboard::{ClipboardType, ContentRepr};

#[cfg(any(unix, windows))]
use std::collections::{HashMap, HashSet};
#[cfg(any(unix, windows))]
#[cfg(any(unix, windows))]
use tasty_plugin_protocol::ThemeWire;
#[cfg(any(unix, windows))]
use tasty_plugin_sdk::EguiMeshPopup;
#[cfg(any(unix, windows))]
use tasty_type_appearance::theme::Theme;

const PLUGIN_ID: &str = "com.tasty.clipboard-viewer";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 팝업을 열 때 읽은 클립보드 내용과 선택 상태.
pub(crate) struct ViewerState {
    pub(crate) available: Vec<(ClipboardType, ContentRepr)>,
    pub(crate) read_error: Option<String>,
    pub(crate) selected: Option<ClipboardType>,
    /// HTML 정리 표시 여부. 팝업을 다시 열면 초기화한다.
    pub(crate) html_pretty: bool,
}

impl ViewerState {
    /// 현재 클립보드를 1회 읽어 스냅샷을 만든다. 첫 가용 타입을 기본 선택.
    fn load() -> Self {
        match clipboard::read_available() {
            Ok(available) => {
                let selected = available.first().map(|(ty, _)| *ty);
                Self {
                    available,
                    read_error: None,
                    selected,
                    html_pretty: false,
                }
            }
            Err(e) => {
                tracing::warn!("clipboard read failed: {e}");
                Self {
                    available: Vec::new(),
                    read_error: Some(e),
                    selected: None,
                    html_pretty: false,
                }
            }
        }
    }
}

struct ClipboardViewerPlugin {
    /// 주 인스턴스 ID. 추가 인스턴스에는 이미 열려 있다는 안내를 그린다.
    primary_instance: Option<u64>,
    /// 주 인스턴스의 클립보드 스냅샷 + 선택 상태.
    state: Option<ViewerState>,
    /// 인스턴스별 egui-mesh 렌더 상태와 공유 버퍼.
    #[cfg(any(unix, windows))]
    popups: HashMap<u64, EguiMeshPopup>,
    /// CJK fallback 폰트를 이미 설치한 instance_id — set_fonts 재업로드 방지.
    #[cfg(any(unix, windows))]
    fonts_installed: HashSet<u64>,
    tr: Translator,
}

impl ClipboardViewerPlugin {
    fn new(env: &PluginEnv) -> Self {
        Self {
            primary_instance: None,
            state: None,
            #[cfg(any(unix, windows))]
            popups: HashMap::new(),
            #[cfg(any(unix, windows))]
            fonts_installed: HashSet::new(),
            tr: Translator::from_plugin_env(env),
        }
    }
}

impl Plugin for ClipboardViewerPlugin {
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
        // UiNode 대신 mesh로 그린다. 첫 인스턴스만 클립보드 내용을 읽는다.
        if self.primary_instance.is_none() {
            self.primary_instance = Some(ctx.instance_id);
            self.state = Some(ViewerState::load());
        }
        PopupOpenResult::default()
    }

    fn paint_popup(&mut self, ctx: PopupSetContextCtx) {
        self.paint(ctx);
    }

    fn on_popup_closed(&mut self, ctx: PopupClosedCtx) {
        let iid = ctx.instance_id;
        #[cfg(any(unix, windows))]
        {
            self.popups.remove(&iid);
            self.fonts_installed.remove(&iid);
        }
        if self.primary_instance == Some(iid) {
            self.primary_instance = None;
            self.state = None;
        }
    }
}

impl ClipboardViewerPlugin {
    /// `popup.set_context` 한 frame 을 그려 host 에 popup mesh 를 회신한다.
    #[cfg(any(unix, windows))]
    fn paint(&mut self, ctx: PopupSetContextCtx) {
        let iid = ctx.params.instance_id;

        // 테마를 받기 전에는 그리지 않는다.
        let Some(theme) = ctx.params.theme.as_ref().map(theme_from_wire) else {
            tracing::debug!("clipboard popup {iid}: set_context without theme — skipping paint");
            return;
        };

        let Self {
            primary_instance,
            state,
            popups,
            fonts_installed,
            tr,
        } = self;
        let is_primary = *primary_instance == Some(iid);

        let popup = popups.entry(iid).or_insert_with(|| EguiMeshPopup::new(iid));
        // 인스턴스마다 CJK 대체 폰트를 한 번 설치한다.
        if fonts_installed.insert(iid) {
            install_fonts(popup.context());
        }

        // paint의 클로저는 반환값이 없어 닫기 요청을 밖에 기록해 두었다가 처리한다.
        let mut close_requested = false;
        let result = popup.paint(&ctx.host, &ctx.params, |egui_ctx| {
            close_requested = match (is_primary, state.as_mut()) {
                (true, Some(st)) => view::draw(egui_ctx, &theme, st, tr),
                // 주 인스턴스가 아니거나(중복 open) 스냅샷이 없으면 "이미 열림" placeholder.
                _ => view::draw_already_open(egui_ctx, &theme, tr),
            };
        });
        if let Err(e) = result {
            tracing::warn!("clipboard popup {iid} paint failed: {e}");
        }
        if close_requested {
            close_popup(&ctx.host, iid);
        }
    }

    /// 공유 버퍼 송신을 지원하지 않는 타깃에서는 그리기를 생략한다.
    #[cfg(not(any(unix, windows)))]
    fn paint(&mut self, _ctx: PopupSetContextCtx) {}
}

/// 전달받은 색·밝기·확대 비율로 테마를 만든다.
#[cfg(any(unix, windows))]
fn theme_from_wire(w: &ThemeWire) -> Theme {
    Theme::with_colors_and_zoom(w.colors.clone(), w.is_light, w.ui_zoom)
}

/// 호스트에 팝업 닫기를 요청한다.
#[cfg(any(unix, windows))]
fn close_popup(host: &tasty_plugin_sdk::HostHandle, instance_id: u64) {
    if let Err(e) = host.call(
        "popup.close",
        serde_json::json!({ "instance_id": instance_id }),
    ) {
        tracing::warn!("clipboard viewer popup close failed: {e}");
    }
}

/// 기본 폰트 뒤에 시스템 CJK 폰트를 추가한다. 구할 수 없으면 생략한다.
#[cfg(any(unix, windows))]
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
    let plugin = ClipboardViewerPlugin::new(&env);
    tasty_plugin_sdk::run(plugin)
}
