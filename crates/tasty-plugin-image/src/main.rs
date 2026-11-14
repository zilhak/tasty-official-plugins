#![forbid(unsafe_code)]

//! 이미지 픽셀과 편집 상태를 플러그인에서 관리하고 egui-mesh로 그린다.
//! 저장·붙여넣기·파일 탐색은 문서 상태를 바꾸며, 화면 변환·목록 조회는 호스트에 맡긴다.

// 이유: 시험의 let _는 제품 코드에서 반환값을 버리는 목록에 포함하지 않는다.
#![cfg_attr(test, allow(clippy::let_underscore_must_use))]

mod doc;
mod picker;
#[cfg(any(unix, windows))]
mod render;
mod tiled;

use std::collections::HashMap;
use std::sync::mpsc;

use doc::{ImageDoc, SaveFailure, SaveTarget};
use serde_json::{Value, json};
use tasty_plugin_protocol::{RawInputEventWire, ThemeWire};
use tasty_plugin_sdk::file_watch::{self, StatGatedDigest, WatchCmd};
use tasty_plugin_sdk::{
    IpcMethodCtx, IpcMethodError, Plugin, PluginEnv, SurfaceCreateCtx, SurfaceResult,
    SurfaceSetContextCtx, Translator, host::HostHandle,
};
use tasty_type_appearance::theme::Theme;

#[cfg(any(unix, windows))]
use tasty_plugin_sdk::EguiMeshSurface;

const PLUGIN_ID: &str = "com.tasty.image";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

struct ImagePlugin {
    /// 화면별 egui 렌더 상태와 공유 버퍼.
    #[cfg(any(unix, windows))]
    meshes: HashMap<u32, EguiMeshSurface>,
    /// surface_id 들 중 폰트(CJK fallback)를 이미 설치한 것 — set_fonts 재업로드 방지.
    #[cfg(any(unix, windows))]
    fonts_installed: std::collections::HashSet<u32>,
    /// surface_id → image document state.
    docs: HashMap<u32, ImageDoc>,
    /// plugin lang 카탈로그 (UI 문자열).
    tr: Translator,
    /// 외부 변경 감시 스레드에 파일 등록·해제를 보내는 채널.
    watch_tx: Option<mpsc::Sender<WatchCmd>>,
}

impl ImagePlugin {
    fn new(tr: Translator) -> Self {
        Self {
            #[cfg(any(unix, windows))]
            meshes: HashMap::new(),
            #[cfg(any(unix, windows))]
            fonts_installed: std::collections::HashSet::new(),
            docs: HashMap::new(),
            tr,
            watch_tx: None,
        }
    }
}

impl Plugin for ImagePlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn on_start(&mut self, host: HostHandle, _bus: tasty_plugin_sdk::BusHandle) {
        // 입력이 없을 때도 외부 저장을 반영하도록 별도 스레드에서 감시한다.
        // 메타데이터가 바뀔 때만 내용을 비교해 불필요한 읽기·디코딩을 줄인다.
        let (tx, rx) = mpsc::channel();
        self.watch_tx = Some(tx);
        if let Err(e) = std::thread::Builder::new()
            .name("image-watch".to_string())
            .spawn(move || file_watch::run::<StatGatedDigest>(host, rx, "image.reload"))
        {
            tracing::warn!("image watch worker spawn failed — idle auto-reload disabled: {e}");
        }
    }

    fn on_event(&mut self, ctx: tasty_plugin_sdk::EventDispatchCtx) {
        if ctx.envelope.key == "file_picker.result" {
            for doc in self.docs.values_mut() {
                picker::receive(doc, &ctx.envelope.payload);
            }
        }
    }

    fn create_surface(&mut self, ctx: SurfaceCreateCtx) -> SurfaceResult {
        // SDK가 전달한 전체 요청에서 중첩된 파일 파라미터를 읽는다.
        let file = surface_param_file(&ctx.params);
        self.watch_register(ctx.surface_id, file.clone());
        self.docs.insert(ctx.surface_id, ImageDoc::new(file));
        SurfaceResult::default()
    }

    fn destroy_surface(&mut self, surface_id: u32) {
        #[cfg(any(unix, windows))]
        {
            self.meshes.remove(&surface_id);
            self.fonts_installed.remove(&surface_id);
        }
        self.docs.remove(&surface_id);
        self.watch_unregister(surface_id);
    }

    fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> Result<Value, IpcMethodError> {
        match ctx.method.as_str() {
            // 화면 변환과 전체 목록 조회는 호스트에 맡긴다.
            "image.open" | "image.list" => trampoline(&ctx.host, &ctx.method, ctx.params),
            // 사용자 입력 없이 문서가 바뀌므로 마지막 컨텍스트로 다시 그린다.
            "image.save" | "image.export_png" => {
                let out = self.image_save(&ctx.params)?;
                self.repaint_after_edit(&ctx.host, &ctx.params);
                self.announce_pending_path(&ctx.host, &ctx.params);
                Ok(out)
            }
            // 파일 읽기와 문서 변경은 플러그인 처리 스레드에서 수행한다.
            "image.reload" => {
                let out = self.image_reload(&ctx.params)?;
                self.repaint_after_edit(&ctx.host, &ctx.params);
                Ok(out)
            }
            "image.paste" => {
                let out = self.image_paste(&ctx.params)?;
                self.repaint_after_edit(&ctx.host, &ctx.params);
                Ok(out)
            }
            "image.next" => {
                let out = self.image_step(&ctx.params, true)?;
                self.repaint_after_edit(&ctx.host, &ctx.params);
                self.announce_pending_path(&ctx.host, &ctx.params);
                Ok(out)
            }
            "image.prev" => {
                let out = self.image_step(&ctx.params, false)?;
                self.repaint_after_edit(&ctx.host, &ctx.params);
                self.announce_pending_path(&ctx.host, &ctx.params);
                Ok(out)
            }
            other => Err(IpcMethodError::not_found(other)),
        }
    }

    fn paint_surface(&mut self, ctx: SurfaceSetContextCtx) {
        self.paint(ctx);
    }
}

impl ImagePlugin {
    /// IPC 처리로 문서 경로가 바뀌었으면 호스트에 알린다.
    fn announce_pending_path(&mut self, host: &HostHandle, params: &Value) {
        if let Ok(sid) = require_surface(params)
            && let Some(path) = self
                .docs
                .get_mut(&sid)
                .and_then(ImageDoc::take_path_for_host)
        {
            announce_surface_path(host, sid, path);
        }
    }

    fn image_save(&mut self, params: &Value) -> Result<Value, IpcMethodError> {
        let sid = require_surface(params)?;
        let explicit = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let doc = self.docs.get_mut(&sid).ok_or_else(|| {
            IpcMethodError::invalid_params(&format!("Surface {sid} is not an image"))
        })?;
        let explicit_given = explicit.is_some();
        let final_path = match explicit {
            Some(p) => p,
            None => match doc.save_target() {
                SaveTarget::Write(p) => p,
                SaveTarget::NoPath => {
                    return Err(IpcMethodError::invalid_params(
                        "No save path: provide 'path' or open a file first",
                    ));
                }
                SaveTarget::Exists(p) => {
                    return Err(IpcMethodError::invalid_params(&format!(
                        "Save target already exists: {p}. Provide 'path' to save elsewhere"
                    )));
                }
            },
        };
        // 경로 없는 저장은 옆 `.png` 를 새 파일로만 만든다. 명시한 경로는 덮어쓴다.
        let written = if explicit_given {
            doc.save_png(&final_path).map_err(SaveFailure::Failed)
        } else {
            doc.write_save_target(&final_path)
        };
        match written {
            Ok(()) => {
                if doc.is_blank() {
                    doc.adopt_saved_path(final_path.clone());
                } else if !explicit_given {
                    doc.adopt_if_saved_elsewhere(&final_path);
                }
                // 직접 저장한 내용을 외부 변경으로 다시 읽지 않도록 감시 기준을 갱신한다.
                let watched = doc.file_path.clone();
                self.watch_register(sid, watched);
                Ok(json!({ "ok": true, "path": final_path }))
            }
            Err(SaveFailure::Exists(p)) => Err(IpcMethodError::invalid_params(&format!(
                "Save target already exists: {p}. Provide 'path' to save elsewhere"
            ))),
            Err(SaveFailure::Failed(e)) => Err(IpcMethodError::new(format!("save failed: {e}"))),
        }
    }

    /// 파일을 다시 읽어 문서에 반영한다. 편집 중이면 미뤄 둔다(`apply_external_change`).
    fn image_reload(&mut self, params: &Value) -> Result<Value, IpcMethodError> {
        let sid = require_surface(params)?;
        let doc = self.docs.get_mut(&sid).ok_or_else(|| {
            IpcMethodError::invalid_params(&format!("Surface {sid} is not an image"))
        })?;
        let applied = doc.apply_external_change();
        Ok(json!({ "ok": true, "surface_id": sid, "applied": applied }))
    }

    /// 감시 파일과 비교 기준을 등록·갱신한다.
    fn watch_register(&self, surface_id: u32, path: Option<String>) {
        let Some(tx) = &self.watch_tx else { return };
        if tx.send(WatchCmd::Register { surface_id, path }).is_err() {
            tracing::warn!(
                "image watch: register send failed for surface {surface_id} (worker gone)"
            );
        }
    }

    fn watch_unregister(&self, surface_id: u32) {
        let Some(tx) = &self.watch_tx else { return };
        if tx.send(WatchCmd::Unregister { surface_id }).is_err() {
            tracing::warn!(
                "image watch: unregister send failed for surface {surface_id} (worker gone)"
            );
        }
    }

    fn image_paste(&mut self, params: &Value) -> Result<Value, IpcMethodError> {
        let sid = require_surface(params)?;
        let color_image = read_clipboard_image()?;
        let doc = self.docs.get_mut(&sid).ok_or_else(|| {
            IpcMethodError::invalid_params(&format!("Surface {sid} is not an image"))
        })?;
        doc.ensure_loaded();
        doc.paste_image(color_image);
        Ok(json!({ "ok": true, "surface_id": sid }))
    }

    fn image_step(&mut self, params: &Value, forward: bool) -> Result<Value, IpcMethodError> {
        let sid = require_surface(params)?;
        let doc = self.docs.get_mut(&sid).ok_or_else(|| {
            IpcMethodError::invalid_params(&format!("Surface {sid} is not an image"))
        })?;
        doc.ensure_loaded();
        // 편집 중 이동하면 화면은 그대로인데 저장·감시 경로만 바뀐다. 편집을 끝낸 뒤 이동하게 한다.
        if doc.is_editing() {
            return Err(IpcMethodError::invalid_params(
                "Image is being edited: save or cancel the edit before moving to another image",
            ));
        }
        let new_path = if forward {
            doc.step_next()
        } else {
            doc.step_prev()
        };
        match new_path {
            Some(path) => {
                doc.load_after_navigation();
                // 선택한 파일로 감시 대상도 바꾼다.
                self.watch_register(sid, Some(path.clone()));
                Ok(json!({ "ok": true, "path": path }))
            }
            None => Err(IpcMethodError::invalid_params(
                "No sibling images available",
            )),
        }
    }

    /// `set_context` 한 frame 을 그려 host 에 mesh 를 회신한다.
    #[cfg(any(unix, windows))]
    fn paint(&mut self, ctx: SurfaceSetContextCtx) {
        let sid = ctx.params.surface_id;

        // host 가 Theme 을 아직 안 보냈으면 토큰을 풀 수 없으므로 이 frame 은 건너뛴다.
        let Some(theme) = ctx.params.theme.as_ref().map(theme_from_wire) else {
            tracing::debug!("image surface {sid}: set_context without theme — skipping paint");
            return;
        };

        let tr = &self.tr;

        let doc = self.docs.entry(sid).or_insert_with(|| ImageDoc::new(None));
        doc.ensure_loaded();
        doc.ensure_brush_themed(theme.accent_danger().to_egui());
        apply_paste_events(doc, &ctx.params.raw_input.events, || {
            read_clipboard_image().map_err(|e| e.message)
        });

        let is_new = !self.meshes.contains_key(&sid);
        let mesh = self
            .meshes
            .entry(sid)
            .or_insert_with(|| EguiMeshSurface::new(sid));
        if is_new {
            // 인스턴스마다 CJK 대체 폰트를 한 번 설치한다.
            install_fonts(mesh.context());
            self.fonts_installed.insert(sid);
        }

        let result = mesh.paint(&ctx.host, &ctx.params, |egui_ctx| {
            render::draw(egui_ctx, &theme, tr, doc);
        });
        if let Err(e) = result {
            tracing::warn!("image surface {sid} paint failed: {e}");
        }
        picker::dispatch(doc, sid, |method, params| {
            ctx.host.call(method, params).map_err(|e| e.to_string())
        });
        if let Some(path) = doc.take_path_for_host() {
            announce_surface_path(&ctx.host, sid, path);
        }
    }

    /// IPC로 바뀐 문서를 마지막 컨텍스트와 빈 입력으로 다시 그린다. 테마가 없으면 생략한다.
    #[cfg(any(unix, windows))]
    fn repaint_after_edit(&mut self, host: &HostHandle, params: &Value) {
        let Ok(sid) = require_surface(params) else {
            return;
        };
        let Some(theme) = self
            .meshes
            .get(&sid)
            .and_then(|m| m.last_theme())
            .map(theme_from_wire)
        else {
            return;
        };
        let tr = &self.tr;
        let Some(doc) = self.docs.get_mut(&sid) else {
            return;
        };
        doc.ensure_loaded();
        doc.ensure_brush_themed(theme.accent_danger().to_egui());
        let Some(mesh) = self.meshes.get_mut(&sid) else {
            return;
        };
        let result = mesh.repaint_last(host, |egui_ctx| {
            render::draw(egui_ctx, &theme, tr, doc);
        });
        if let Err(e) = result {
            tracing::warn!("image surface {sid} repaint failed: {e}");
        }
    }

    /// Unix·Windows 외에는 공유 버퍼 그리기를 생략한다.
    #[cfg(not(any(unix, windows)))]
    fn paint(&mut self, _ctx: SurfaceSetContextCtx) {}

    /// Unix·Windows 외에는 다시 그리기도 생략한다.
    #[cfg(not(any(unix, windows)))]
    fn repaint_after_edit(&mut self, _host: &HostHandle, _params: &Value) {}
}

/// 새 경로로 저장하거나 이동한 이미지를 호스트의 `image.open`으로 같은 surface에서 다시 연다.
/// 호스트가 탭 제목과 복원 경로를 바꾸고 surface를 다시 만들어, 새 문서가 그 파일을 감시한다.
/// 호스트는 다시 만들 때 이 플러그인의 응답을 기다리므로 플러그인 처리 스레드에서 호출하면
/// 서로 기다리다 호출 시한(60초)까지 멈춘다. 그래서 별도 스레드에서 호출한다.
fn announce_surface_path(host: &HostHandle, surface_id: u32, path: String) {
    let host = host.clone();
    let spawned = std::thread::Builder::new()
        .name("image-path-announce".to_string())
        .spawn(move || {
            let params = json!({ "surface_id": surface_id, "path": path });
            if let Err(e) = host.call("image.open", params) {
                tracing::warn!(
                    "image surface {surface_id}: image.open after path change failed: {e}"
                );
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("image surface {surface_id}: path announce thread spawn failed: {e}");
    }
}

/// 같은 이름의 호스트 메서드를 호출한다. 호스트가 이 자기 호출을 내부 처리기로 보낸다.
fn trampoline(host: &HostHandle, method: &str, params: Value) -> Result<Value, IpcMethodError> {
    Ok(host.call(method, params)?)
}

/// IPC의 surface 인자를 읽는다.
fn require_surface(params: &Value) -> Result<u32, IpcMethodError> {
    params
        .get("surface")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| IpcMethodError::invalid_params("missing 'surface'"))
}

/// 호스트가 붙여넣기 단축키를 Paste 이벤트로 보낸다. 이번 입력에 Paste 가 있을 때만 클립보드를
/// 읽어 이미지를 붙인다(이미지는 이 프로세스가 직접 읽는다). 붙였으면 true 다.
#[cfg(any(unix, windows))]
fn apply_paste_events(
    doc: &mut ImageDoc,
    events: &[RawInputEventWire],
    read: impl FnOnce() -> Result<egui::ColorImage, String>,
) -> bool {
    events.contains(&RawInputEventWire::Paste) && doc.paste_from_clipboard(read)
}

/// Read the system clipboard image into a `ColorImage` (paste → floating selection).
fn read_clipboard_image() -> Result<egui::ColorImage, IpcMethodError> {
    let mut cb = arboard::Clipboard::new()
        .map_err(|e| IpcMethodError::new(format!("clipboard open failed: {e}")))?;
    let image = cb
        .get_image()
        .map_err(|e| IpcMethodError::invalid_params(&format!("no image on clipboard: {e}")))?;
    // 외부 입력 (클립보드 이미지 바이트) → ColorImage 픽셀.
    #[allow(clippy::disallowed_methods)]
    let pixels: Vec<egui::Color32> = image
        .bytes
        .chunks_exact(4)
        .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
        .collect();
    Ok(egui::ColorImage {
        size: [image.width, image.height],
        pixels,
    })
}

/// surface.create envelope 에서 `file` 을 꺼낸다 (nested `params.file`, flat fallback).
fn surface_param_file(envelope: &Value) -> Option<String> {
    envelope
        .get("params")
        .and_then(|p| p.get("file"))
        .or_else(|| envelope.get("file"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 전달받은 색·밝기·확대 비율로 테마를 만든다.
fn theme_from_wire(w: &ThemeWire) -> Theme {
    Theme::with_colors_and_zoom(w.colors.clone(), w.is_light, w.ui_zoom)
}

/// 구할 수 있는 시스템 CJK 폰트를 대체 폰트로 추가한다.
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
    let tr = Translator::from_plugin_env(&env);
    tasty_plugin_sdk::run(ImagePlugin::new(tr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_param_file_reads_nested_and_flat() {
        assert_eq!(
            surface_param_file(&json!({ "params": { "file": "/a/b.png" } })).as_deref(),
            Some("/a/b.png")
        );
        assert_eq!(
            surface_param_file(&json!({ "file": "/c/d.png" })).as_deref(),
            Some("/c/d.png")
        );
        assert_eq!(surface_param_file(&json!({ "params": {} })), None);
    }

    #[test]
    fn create_surface_inserts_doc() {
        let mut p = ImagePlugin::new(Translator::default());
        p.create_surface(SurfaceCreateCtx {
            surface_id: 1,
            kind: "image".into(),
            cwd: None,
            params: json!({ "surface_id": 1, "kind": "image", "params": { "file": "/x/y.png" } }),
        });
        assert_eq!(
            p.docs.get(&1).unwrap().file_path.as_deref(),
            Some("/x/y.png")
        );
    }

    #[test]
    fn save_without_surface_is_invalid_params() {
        let mut p = ImagePlugin::new(Translator::default());
        let err = p.image_save(&json!({})).unwrap_err();
        assert_eq!(err.code, -32602);
    }

    /// 같은 프로세스의 재호출도 구분하는 시험용 임시 PNG 경로.
    fn probe_path(what: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "tasty-image-{what}-{}-{}.png",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    /// 새 캔버스를 경로를 주어 저장하면 그 경로를 호스트에 한 번 알릴 경로로 남겨야 한다.
    #[test]
    fn saving_a_new_canvas_to_a_path_queues_that_path_for_the_host_once() {
        let mut p = ImagePlugin::new(Translator::default());
        p.create_surface(SurfaceCreateCtx {
            surface_id: 1,
            kind: "image".into(),
            cwd: None,
            params: json!({ "surface_id": 1, "kind": "image", "params": {} }),
        });
        p.docs
            .get_mut(&1)
            .expect("문서가 있어야 한다")
            .ensure_loaded();
        let path = probe_path("saveas-host");
        let file = path.to_string_lossy().into_owned();

        p.image_save(&json!({ "surface": 1, "path": file }))
            .expect("새 캔버스 저장이 성공해야 한다");
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        assert_eq!(doc.file_path.as_deref(), Some(file.as_str()));
        assert_eq!(
            doc.take_path_for_host().as_deref(),
            Some(file.as_str()),
            "저장한 경로를 호스트에 알려 탭 제목·복원 경로·감시 대상을 바꾸게 해야 한다"
        );
        assert_eq!(
            doc.take_path_for_host(),
            None,
            "같은 경로를 두 번 알리지 않아야 한다"
        );
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    /// 같은 폴더에 PNG 두 장을 둔 시험용 폴더와 첫 파일 경로.
    fn two_image_dir(what: &str) -> (std::path::PathBuf, String, String) {
        let dir = probe_path(what).with_extension("");
        std::fs::create_dir_all(&dir).expect("시험 폴더 생성 실패");
        let first = dir.join("a.png");
        let second = dir.join("b.png");
        for (path, rgb) in [(&first, [255, 0, 0]), (&second, [0, 0, 255])] {
            image::RgbImage::from_pixel(4, 4, image::Rgb(rgb))
                .save(path)
                .expect("probe png 저장 실패");
        }
        (
            dir,
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        )
    }

    fn plugin_with_file(file: &str) -> ImagePlugin {
        let mut p = ImagePlugin::new(Translator::default());
        p.create_surface(SurfaceCreateCtx {
            surface_id: 1,
            kind: "image".into(),
            cwd: None,
            params: json!({ "surface_id": 1, "kind": "image", "params": { "file": file } }),
        });
        p.docs
            .get_mut(&1)
            .expect("문서가 있어야 한다")
            .ensure_loaded();
        p
    }

    /// 다음 이미지로 넘어가면 그 경로를 호스트에 알려 탭 제목·복원 경로·감시 대상을 바꾸게 해야 한다.
    #[test]
    fn stepping_to_a_sibling_queues_its_path_for_the_host() {
        let (dir, first, second) = two_image_dir("step-host");
        let mut p = plugin_with_file(&first);

        p.image_step(&json!({ "surface": 1 }), true)
            .expect("다음 이미지로 넘어가야 한다");
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        assert_eq!(doc.file_path.as_deref(), Some(second.as_str()));
        assert_eq!(doc.take_path_for_host().as_deref(), Some(second.as_str()));
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// 편집 중 next/prev 는 거절한다. 문서 경로·위치가 바뀌면 이후 저장이 다른 파일에 쓰고,
    /// 호스트에 알리면 다시 만들면서 편집을 잃는다.
    #[test]
    fn stepping_during_an_edit_is_refused_and_keeps_the_document_path() {
        let (dir, first, _second) = two_image_dir("step-edit");
        let mut p = plugin_with_file(&first);
        p.docs
            .get_mut(&1)
            .expect("문서가 있어야 한다")
            .enter_edit_mode();

        for forward in [true, false] {
            let err = p
                .image_step(&json!({ "surface": 1 }), forward)
                .expect_err("편집 중 이동은 거절해야 한다");
            assert_eq!(err.code, -32602);
        }
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        assert!(doc.is_editing(), "편집 세션이 유지되어야 한다");
        assert_eq!(doc.file_path.as_deref(), Some(first.as_str()));
        assert_eq!(doc.current_index, 0);
        assert_eq!(doc.take_path_for_host(), None);
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// 파일을 연 문서를 다른 경로로 내보내는 저장은 문서 경로를 바꾸지 않으므로 호스트에 알리지 않는다.
    #[test]
    fn exporting_an_opened_file_to_another_path_does_not_move_the_surface() {
        let src = probe_path("export-src");
        image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
            .save(&src)
            .expect("probe png 저장 실패");
        let dst = probe_path("export-dst");
        let mut p = ImagePlugin::new(Translator::default());
        p.create_surface(SurfaceCreateCtx {
            surface_id: 1,
            kind: "image".into(),
            cwd: None,
            params: json!({ "surface_id": 1, "kind": "image", "params": { "file": src.to_string_lossy() } }),
        });
        p.docs
            .get_mut(&1)
            .expect("문서가 있어야 한다")
            .ensure_loaded();

        p.image_save(&json!({ "surface": 1, "path": dst.to_string_lossy() }))
            .expect("내보내기 저장이 성공해야 한다");
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        assert_eq!(
            doc.file_path.as_deref(),
            Some(src.to_string_lossy().as_ref())
        );
        assert_eq!(doc.take_path_for_host(), None);
        let _ = std::fs::remove_file(&src); // best-effort 정리 — 실패 무시.
        let _ = std::fs::remove_file(&dst); // best-effort 정리 — 실패 무시.
    }

    /// 파일을 연 서피스에서 새 캔버스를 만든 뒤 경로 없이 저장하면 원래 파일을 덮어쓰지 않아야 한다.
    #[test]
    fn saving_a_new_canvas_without_a_path_leaves_the_opened_file_intact() {
        // 같은 프로세스의 재호출도 구분하도록 단조 카운터를 붙인다.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "tasty-image-newcanvas-save-{}-{}.png",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
            .save(&path)
            .expect("probe png 저장 실패");
        let file = path.to_string_lossy().into_owned();
        let mut p = ImagePlugin::new(Translator::default());
        p.create_surface(SurfaceCreateCtx {
            surface_id: 1,
            kind: "image".into(),
            cwd: None,
            params: json!({ "surface_id": 1, "kind": "image", "params": { "file": file } }),
        });
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        doc.ensure_loaded();
        doc.create_blank_canvas(8, 8);

        let err = p.image_save(&json!({ "surface": 1 })).unwrap_err();
        assert_eq!(err.code, -32602, "경로가 없으면 저장을 거절해야 한다");
        let on_disk = image::open(&path).expect("원래 파일을 읽을 수 있어야 한다");
        assert_eq!(
            (on_disk.width(), on_disk.height()),
            (4, 4),
            "원래 파일이 새 캔버스로 덮어써지지 않아야 한다"
        );
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    /// 경로 없는 `image.save` 도 비-PNG 문서를 옆 `.png` 로 쓰고 문서를 옮기며, 그 파일이 이미
    /// 있으면 -32602 로 거절하고 아무 파일도 바꾸지 않는다.
    #[test]
    fn saving_a_jpg_without_a_path_follows_the_png_beside_it_rule() {
        let png = probe_path("ipc-save-jpg");
        let jpg = png.with_extension("jpg");
        image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
            .save(&jpg)
            .expect("probe jpg 저장 실패");
        let jpg_bytes = std::fs::read(&jpg).expect("jpg 읽기");
        let (jpg_s, png_s) = (
            jpg.to_string_lossy().into_owned(),
            png.to_string_lossy().into_owned(),
        );

        let mut p = plugin_with_file(&jpg_s);
        let out = p
            .image_save(&json!({ "surface": 1 }))
            .expect("옆 .png 가 없으면 저장해야 한다");
        assert_eq!(out["path"], png_s.as_str());
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        assert_eq!(doc.file_path.as_deref(), Some(png_s.as_str()));
        assert_eq!(doc.take_path_for_host().as_deref(), Some(png_s.as_str()));

        let png_bytes = std::fs::read(&png).expect("png 읽기");
        let mut p = plugin_with_file(&jpg_s);
        let err = p.image_save(&json!({ "surface": 1 })).unwrap_err();
        assert_eq!(err.code, -32602, "이미 있는 .png 는 덮어쓰지 않는다");
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        assert_eq!(doc.file_path.as_deref(), Some(jpg_s.as_str()));
        assert_eq!(doc.take_path_for_host(), None);
        assert_eq!(std::fs::read(&png).expect("png 읽기"), png_bytes);
        assert_eq!(std::fs::read(&jpg).expect("jpg 읽기"), jpg_bytes);
        let _ = std::fs::remove_file(&jpg); // best-effort 정리 — 실패 무시.
        let _ = std::fs::remove_file(&png); // best-effort 정리 — 실패 무시.
    }

    /// 이번 입력에 Paste 가 있을 때만 클립보드를 읽어 붙이고, 없으면 읽지도 않는다.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_paste_event_in_the_frame_input_pastes_the_clipboard_image() {
        let (dir, first, _second) = two_image_dir("paste-event");
        let mut p = plugin_with_file(&first);
        let doc = p.docs.get_mut(&1).expect("문서가 있어야 한다");
        let key = RawInputEventWire::Key {
            key: "V".into(),
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };

        let pasted = apply_paste_events(doc, std::slice::from_ref(&key), || {
            panic!("Paste 가 없으면 클립보드를 읽지 않아야 한다")
        });
        assert!(!pasted);
        assert!(!doc.is_editing());

        let pasted = apply_paste_events(doc, &[key, RawInputEventWire::Paste], || {
            Ok(egui::ColorImage::new([2, 2], egui::Color32::YELLOW))
        });
        assert!(pasted);
        assert!(
            doc.is_editing(),
            "떠 있는 선택으로 편집 모드에 들어가야 한다"
        );
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    #[test]
    fn step_on_missing_surface_is_invalid_params() {
        let mut p = ImagePlugin::new(Translator::default());
        let err = p.image_step(&json!({ "surface": 7 }), true).unwrap_err();
        assert_eq!(err.code, -32602);
    }

    /// 너무 큼 문구는 내장 언어 모두 숫자와 단위를 NBSP 로 붙여 그 사이에서 줄이 바뀌지 않는다.
    #[test]
    fn too_large_lines_keep_each_number_with_its_unit() {
        let lang = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lang");
        for locale in tasty_i18n::BUILTIN_CODES {
            let tr = Translator::load(&lang, locale);
            let sub = tr.t_args("image.state.too_large_sub", &["16384", "512\u{a0}MiB"]);
            assert!(sub.contains("16384\u{a0}px"), "{locale}: {sub}");
            assert!(!sub.contains("16384 px"), "{locale}: {sub}");
            let size = tr.t_args("image.state.too_large_size", &["20000", "300"]);
            assert_eq!(size, "20000 × 300\u{a0}px", "{locale}");
        }
    }
}
