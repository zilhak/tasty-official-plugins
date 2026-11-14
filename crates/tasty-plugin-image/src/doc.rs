//! 이미지별 픽셀·편집·실행 취소·확대·탐색 상태.
//! 픽셀을 플러그인의 egui 텍스처로 올려 mesh 채널을 통해 호스트에 전달한다.

use std::path::{Path, PathBuf};

use egui::{Color32, ColorImage, Pos2, Rect, Vec2};

use crate::tiled::TiledTexture;

/// Default blank-canvas dimensions when an image surface is created without a file.
pub const DEFAULT_BLANK_CANVAS_WIDTH: usize = 800;
pub const DEFAULT_BLANK_CANVAS_HEIGHT: usize = 600;

/// A single undoable drawing action.
#[derive(Clone)]
pub enum DrawAction {
    Stroke {
        points: Vec<(Pos2, Pos2)>,
        brush_size: f32,
        color: Color32,
    },
    PasteImage {
        image: ColorImage,
        position: Vec2,
        size: [usize; 2],
    },
}

/// Tracks drawing actions for undo/redo.
pub struct ActionHistory {
    actions: Vec<DrawAction>,
    redo_stack: Vec<DrawAction>,
}

impl ActionHistory {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn push(&mut self, action: DrawAction) {
        self.actions.push(action);
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) -> Option<DrawAction> {
        let a = self.actions.pop()?;
        self.redo_stack.push(a.clone());
        Some(a)
    }

    pub fn redo(&mut self) -> Option<DrawAction> {
        let a = self.redo_stack.pop()?;
        self.actions.push(a.clone());
        Some(a)
    }

    pub fn can_undo(&self) -> bool {
        !self.actions.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Replay all actions onto a fresh transparent layer.
    pub fn replay(&self, base_size: [usize; 2]) -> ColorImage {
        let mut layer = ColorImage::new(base_size, Color32::TRANSPARENT);
        let [w, h] = base_size;
        for action in &self.actions {
            match action {
                DrawAction::Stroke {
                    points,
                    brush_size,
                    color,
                } => {
                    let radius = (*brush_size / 2.0).max(0.5);
                    for &(from, to) in points {
                        bresenham_thick_line(&mut layer, from, to, radius, *color, w, h);
                    }
                }
                DrawAction::PasteImage {
                    image: paste_img,
                    position,
                    size,
                } => {
                    blit_image(&mut layer, paste_img, *position, *size, w, h);
                }
            }
        }
        layer
    }
}

/// In-progress stroke being built during a mouse drag.
pub struct StrokeBuilder {
    pub points: Vec<(Pos2, Pos2)>,
    pub brush_size: f32,
    pub color: Color32,
}

/// Resize handle position on the floating selection border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

/// Drag interaction state for a floating selection.
#[derive(Debug, Clone)]
pub enum DragState {
    Idle,
    Moving {
        drag_start_pos: Pos2,
        initial_position: Vec2,
    },
    Resizing {
        #[allow(dead_code)]
        // 크기 조절 시작 위치를 기록하지만 현재 처리에서는 읽지 않는다.
        handle: ResizeHandle,
        drag_start_pos: Pos2,
        initial_rect: Rect,
    },
}

/// A pasted image floating over the canvas, waiting to be committed.
pub struct FloatingSelection {
    pub image: ColorImage,
    pub texture: Option<TiledTexture>,
    pub position: Vec2,
    pub size: [usize; 2],
    pub drag_state: DragState,
}

/// Edit session state for the image document.
pub enum EditState {
    Inactive,
    Drawing {
        history: ActionHistory,
        current_stroke: Option<StrokeBuilder>,
    },
    FloatingSelection {
        selection: FloatingSelection,
        history: ActionHistory,
    },
}

/// 경로 없는 저장의 쓰기 실패.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveFailure {
    /// 새 파일로 만들 경로가 이미 있다. `SaveTarget::Exists` 와 같이 처리한다.
    Exists(String),
    Failed(String),
}

/// 확장자가 대소문자와 관계없이 `png` 인지.
fn is_png_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

/// 경로를 주지 않은 저장(도구 모음 Save, 경로 없는 `image.save`)이 쓸 곳.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveTarget {
    /// 이 경로에 PNG 로 쓴다.
    Write(String),
    /// 경로가 없다(새 캔버스).
    NoPath,
    /// 비-PNG 문서의 같은 이름 `.png` 가 이미 있다. 덮어쓰지 않는다.
    Exists(String),
}

/// Per-surface image document state owned by the plugin.
pub struct ImageDoc {
    /// `None` = blank canvas not yet saved to disk.
    pub file_path: Option<String>,
    /// Sibling images in the same directory (sorted), used for prev/next navigation.
    pub dir_images: Vec<String>,
    /// Index into `dir_images` for the currently displayed file.
    pub current_index: usize,

    pub original_image: Option<ColorImage>,
    /// 마지막으로 파일을 읽지 못한 이유. 읽기에 성공하면 비운다.
    pub load_failure: Option<LoadFailure>,
    pub texture: Option<TiledTexture>,
    pub zoom: f32,
    pub pan_offset: Vec2,

    pub edit_state: EditState,
    pub draw_layer: Option<ColorImage>,
    pub draw_texture: Option<TiledTexture>,
    pub brush_size: f32,
    pub brush_color: Color32,
    pub last_draw_pos: Option<Pos2>,
    pub draw_texture_dirty: bool,

    pub new_image_popup: bool,
    pub new_image_width: String,
    pub new_image_height: String,

    pub save_path_popup: bool,
    pub(crate) browse: crate::picker::BrowseState,
    pub save_path_buffer: String,
    /// 같은 이름의 `.png` 가 있어 Save As 를 열었으면 그 파일 이름. 입력칸 위 안내에 쓴다.
    pub save_path_clash: Option<String>,
    /// 다음 프레임에 입력칸에 포커스를 주고 파일 이름의 stem 을 선택한다.
    pub save_path_select_stem: bool,
    /// 새 경로로 저장한 뒤 호스트에 아직 알리지 않은 경로.
    /// 호스트가 탭 제목·복원 경로·감시 대상을 이 경로로 바꾸도록 `image.open`으로 보낸다.
    path_for_host: Option<String>,

    /// 편집 중 받은 외부 변경을 기억했다가 편집이 끝날 때 다시 읽는다.
    /// 변경 감시기가 같은 변경을 다시 알리지 않으므로 여기서 버리지 않는다.
    pending_external_reload: bool,
    /// True until pixels are first loaded — the plugin lazily loads on first paint.
    loaded: bool,
    /// True once the brush color has been seeded from the theme (accent-danger). The
    /// default lives in the `Theme` (delivered via set_context), not hardcoded here.
    themed_brush: bool,
}

impl ImageDoc {
    /// Create a document from an optional file path (None = blank canvas surface).
    pub fn new(file: Option<String>) -> Self {
        let (dir_images, current_index) = match &file {
            Some(f) => {
                let dir = scan_directory_images(f);
                let idx = dir.iter().position(|p| p == f).unwrap_or(0);
                (dir, idx)
            }
            None => (Vec::new(), 0),
        };
        Self {
            file_path: file,
            dir_images,
            current_index,
            original_image: None,
            load_failure: None,
            texture: None,
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            edit_state: EditState::Inactive,
            draw_layer: None,
            draw_texture: None,
            brush_size: 2.0,
            brush_color: Color32::TRANSPARENT,
            last_draw_pos: None,
            draw_texture_dirty: false,
            new_image_popup: false,
            new_image_width: DEFAULT_BLANK_CANVAS_WIDTH.to_string(),
            new_image_height: DEFAULT_BLANK_CANVAS_HEIGHT.to_string(),
            save_path_popup: false,
            browse: Default::default(),
            save_path_buffer: String::new(),
            save_path_clash: None,
            save_path_select_stem: false,
            path_for_host: None,
            pending_external_reload: false,
            loaded: false,
            themed_brush: false,
        }
    }

    /// Seed the brush color from the theme (accent-danger) on the first themed frame.
    /// Keeps the paint default in the `Theme` rather than hardcoded.
    pub fn ensure_brush_themed(&mut self, accent_danger: Color32) {
        if !self.themed_brush {
            self.brush_color = accent_danger;
            self.themed_brush = true;
        }
    }

    /// True when an editing session is active (drawing or floating selection).
    pub fn is_editing(&self) -> bool {
        !matches!(self.edit_state, EditState::Inactive)
    }

    /// Lazy-load pixel data on first paint. Cheap to call repeatedly.
    pub fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Some(p) = self.file_path.clone() {
            self.load_from(&p);
        } else {
            // Blank canvas — start in edit mode so the user/agent can draw immediately.
            self.original_image = Some(ColorImage::new(
                [DEFAULT_BLANK_CANVAS_WIDTH, DEFAULT_BLANK_CANVAS_HEIGHT],
                Color32::WHITE,
            ));
            self.enter_edit_mode();
        }
    }

    /// Reload from `file_path` regardless of mtime, keeping any edit session cleared.
    pub fn reload_from_disk(&mut self) {
        if let Some(path) = self.file_path.clone() {
            self.load_from(&path);
            self.texture = None;
        }
    }

    /// 파일을 읽어 원본을 바꾼다. 읽지 못했으면 원본을 비우고 이유를 남긴다.
    fn load_from(&mut self, path: &str) {
        match load_image_from_path(path) {
            Ok(img) => {
                self.original_image = Some(img);
                self.load_failure = None;
            }
            Err(failure) => {
                self.original_image = None;
                self.load_failure = Some(failure);
            }
        }
    }

    /// 외부 변경을 반영하되 편집 중에는 미룬다. 화면 내용이 바뀌었으면 true다.
    pub fn apply_external_change(&mut self) -> bool {
        if self.is_editing() {
            self.pending_external_reload = true;
            return false;
        }
        self.reload_from_disk();
        true
    }

    /// Step one image backward in the directory. Returns the new path on success.
    pub fn step_prev(&mut self) -> Option<String> {
        if self.dir_images.is_empty() {
            return None;
        }
        if self.current_index > 0 {
            self.current_index -= 1;
        } else {
            self.current_index = self.dir_images.len() - 1;
        }
        let path = self.dir_images.get(self.current_index)?.clone();
        self.file_path = Some(path.clone());
        Some(path)
    }

    /// Step one image forward in the directory. Returns the new path on success.
    pub fn step_next(&mut self) -> Option<String> {
        if self.dir_images.is_empty() {
            return None;
        }
        self.current_index = (self.current_index + 1) % self.dir_images.len();
        let path = self.dir_images.get(self.current_index)?.clone();
        self.file_path = Some(path.clone());
        Some(path)
    }

    /// After navigation updated `file_path`, load the new file and reset zoom/pan/edit.
    /// 이동을 적용한 경로는 호스트에 알릴 경로로 남겨 탭 제목·복원 경로·감시 대상이 따라오게 한다.
    /// 편집 중에는 이동을 적용하지 않으므로 알리지도 않는다.
    pub fn load_after_navigation(&mut self) {
        if self.is_editing() {
            return;
        }
        if let Some(path) = self.file_path.clone() {
            self.path_for_host = Some(path.clone());
            self.load_from(&path);
            self.texture = None;
            self.zoom = 1.0;
            self.pan_offset = Vec2::ZERO;
            self.exit_edit_mode();
        }
    }

    /// 새 경로로 저장한 문서가 그 파일을 가리키게 하고, 호스트에 알릴 경로로 남긴다.
    pub fn adopt_saved_path(&mut self, path: String) {
        self.file_path = Some(path.clone());
        self.path_for_host = Some(path);
    }

    /// 호스트에 알릴 경로를 꺼낸다. 한 번만 보내도록 비운다.
    pub fn take_path_for_host(&mut self) -> Option<String> {
        self.path_for_host.take()
    }

    /// 경로 없는 저장의 기본 대상(항상 PNG). 확장자가 대소문자와 관계없이 `png` 이면 문서 파일
    /// 자체다. 그 밖에는 같은 폴더의 같은 이름 `.png` 다.
    pub fn save_path(&self) -> Option<String> {
        self.file_path.as_ref().map(|p| {
            let path = Path::new(p);
            if is_png_path(path) {
                p.clone()
            } else {
                path.with_extension("png").to_string_lossy().to_string()
            }
        })
    }

    /// 경로 없는 저장의 대상. PNG 문서는 자기 파일에 쓴다. 비-PNG 문서는 같은 폴더의 같은 이름
    /// `.png` 에 쓰되, 그 파일이 이미 있으면 덮어쓰지 않는다. 원본 파일은 건드리지 않는다.
    pub fn save_target(&self) -> SaveTarget {
        let Some(target) = self.save_path() else {
            return SaveTarget::NoPath;
        };
        let own_file = self.file_path.as_deref() == Some(target.as_str());
        if !own_file && Path::new(&target).exists() {
            SaveTarget::Exists(target)
        } else {
            SaveTarget::Write(target)
        }
    }

    /// 경로 없는 저장이 문서와 다른 파일(비-PNG 의 `.png`)에 썼으면 문서를 그 파일로 옮긴다.
    /// 옮긴 경로는 호스트에 알려 탭 제목·복원 경로·감시 대상이 따라오게 한다.
    pub fn adopt_if_saved_elsewhere(&mut self, path: &str) {
        if self.file_path.as_deref() != Some(path) {
            self.adopt_saved_path(path.to_string());
        }
    }

    /// `save_target` 이 고른 경로에 쓴다. 문서 자기 파일은 덮어쓰고, 옆 `.png` 는 새 파일로만
    /// 만든다 — 판단과 쓰기 사이에 다른 프로세스가 같은 이름을 만들었어도 덮어쓰지 않는다.
    pub fn write_save_target(&self, path: &str) -> Result<(), SaveFailure> {
        if self.file_path.as_deref() == Some(path) {
            self.save_png(path).map_err(SaveFailure::Failed)
        } else {
            self.save_png_new(path)
        }
    }

    /// 도구 모음 Save. 대상에 쓰고 편집을 끝내거나, 쓸 곳이 없으면 경로 입력 팝업을 연다.
    pub fn save_from_toolbar(&mut self) {
        match self.save_target() {
            SaveTarget::Write(path) => match self.write_save_target(&path) {
                Err(SaveFailure::Exists(taken)) => self.open_save_as_for_clash(&taken),
                Err(SaveFailure::Failed(e)) => tracing::warn!("failed to save image: {e}"),
                Ok(()) => {
                    self.adopt_if_saved_elsewhere(&path);
                    self.exit_edit_mode();
                    self.reload_from_disk();
                }
            },
            SaveTarget::NoPath => self.open_save_as(),
            SaveTarget::Exists(taken) => self.open_save_as_for_clash(&taken),
        }
    }

    /// 경로를 묻는 Save As 를 연다.
    pub fn open_save_as(&mut self) {
        self.browse.invalidate();
        self.save_path_popup = true;
        self.save_path_clash = None;
        self.save_path_select_stem = false;
    }

    /// 같은 이름의 `.png` 가 있어 Save As 를 연다. 입력칸에는 다음 빈 이름(`이름-1.png`)을 넣고
    /// stem 을 선택해 둔다.
    pub fn open_save_as_for_clash(&mut self, taken: &str) {
        self.browse.invalidate();
        self.save_path_popup = true;
        self.save_path_clash = Some(
            Path::new(taken)
                .file_name()
                .map_or_else(|| taken.to_string(), |n| n.to_string_lossy().into_owned()),
        );
        self.save_path_buffer = next_free_png_path(taken);
        self.save_path_select_stem = true;
    }

    /// Save As 를 닫는다.
    pub fn close_save_as(&mut self) {
        self.browse.invalidate();
        self.save_path_popup = false;
        self.save_path_clash = None;
        self.save_path_select_stem = false;
    }

    pub fn is_blank(&self) -> bool {
        self.file_path.is_none()
    }

    /// Enter edit mode by allocating a transparent overlay matching the original size.
    pub fn enter_edit_mode(&mut self) {
        if let Some(ref img) = self.original_image {
            let [w, h] = img.size;
            self.draw_layer = Some(ColorImage::new([w, h], Color32::TRANSPARENT));
            self.draw_texture = None;
            self.edit_state = EditState::Drawing {
                history: ActionHistory::new(),
                current_stroke: None,
            };
            self.last_draw_pos = None;
            self.draw_texture_dirty = true;
        }
    }

    /// Exit edit mode, discarding the draw layer.
    pub fn exit_edit_mode(&mut self) {
        self.edit_state = EditState::Inactive;
        self.draw_layer = None;
        self.draw_texture = None;
        self.last_draw_pos = None;
        self.draw_texture_dirty = false;
        if self.pending_external_reload {
            self.pending_external_reload = false;
            self.reload_from_disk();
        }
    }

    /// Replace the original image with a fresh blank canvas and enter edit mode.
    /// 새 캔버스는 열려 있던 파일과 별개 문서다. 경로와 폴더 목록을 버려
    /// Save가 원래 파일을 덮어쓰지 않고 저장할 경로를 묻게 한다.
    pub fn create_blank_canvas(&mut self, width: usize, height: usize) {
        self.file_path = None;
        self.dir_images.clear();
        self.current_index = 0;
        self.pending_external_reload = false;
        self.original_image = Some(ColorImage::new([width, height], Color32::WHITE));
        self.load_failure = None;
        self.texture = None;
        self.zoom = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.enter_edit_mode();
        self.new_image_popup = false;
    }

    /// 붙여넣기 단축키. 클립보드에 이미지가 있으면 떠 있는 선택으로 붙인다. 이미지가 없으면
    /// (텍스트 등) 문서를 바꾸지 않는다. 붙였으면 true 다.
    pub fn paste_from_clipboard(
        &mut self,
        read: impl FnOnce() -> Result<ColorImage, String>,
    ) -> bool {
        match read() {
            Ok(image) => {
                self.paste_image(image);
                true
            }
            Err(e) => {
                tracing::debug!("image: paste ignored, no image on the clipboard: {e}");
                false
            }
        }
    }

    /// Paste an image as a floating selection.
    pub fn paste_image(&mut self, image: ColorImage) {
        let size = image.size;

        let make_selection = |img: ColorImage, sz: [usize; 2]| FloatingSelection {
            image: img,
            texture: None,
            position: Vec2::ZERO,
            size: sz,
            drag_state: DragState::Idle,
        };

        match std::mem::replace(&mut self.edit_state, EditState::Inactive) {
            EditState::Inactive => {
                if let Some(ref orig) = self.original_image {
                    let [w, h] = orig.size;
                    self.draw_layer = Some(ColorImage::new([w, h], Color32::TRANSPARENT));
                    self.draw_texture = None;
                    self.draw_texture_dirty = true;
                }
                self.edit_state = EditState::FloatingSelection {
                    selection: make_selection(image, size),
                    history: ActionHistory::new(),
                };
            }
            EditState::Drawing {
                history,
                current_stroke: _,
            } => {
                self.edit_state = EditState::FloatingSelection {
                    selection: make_selection(image, size),
                    history,
                };
            }
            EditState::FloatingSelection {
                selection: old_sel,
                mut history,
            } => {
                Self::do_commit(
                    &mut self.draw_layer,
                    &mut self.draw_texture_dirty,
                    &old_sel,
                    &mut history,
                );
                self.edit_state = EditState::FloatingSelection {
                    selection: make_selection(image, size),
                    history,
                };
            }
        }
    }

    pub fn commit_floating(&mut self) {
        if let EditState::FloatingSelection {
            selection,
            mut history,
        } = std::mem::replace(&mut self.edit_state, EditState::Inactive)
        {
            Self::do_commit(
                &mut self.draw_layer,
                &mut self.draw_texture_dirty,
                &selection,
                &mut history,
            );
            self.edit_state = EditState::Drawing {
                history,
                current_stroke: None,
            };
        }
    }

    pub fn cancel_floating(&mut self) {
        if let EditState::FloatingSelection { history, .. } =
            std::mem::replace(&mut self.edit_state, EditState::Inactive)
        {
            self.edit_state = EditState::Drawing {
                history,
                current_stroke: None,
            };
        }
    }

    fn do_commit(
        draw_layer: &mut Option<ColorImage>,
        dirty: &mut bool,
        selection: &FloatingSelection,
        history: &mut ActionHistory,
    ) {
        if let Some(layer) = draw_layer {
            let [w, h] = layer.size;
            blit_image(
                layer,
                &selection.image,
                selection.position,
                selection.size,
                w,
                h,
            );
            *dirty = true;
        }
        history.push(DrawAction::PasteImage {
            image: selection.image.clone(),
            position: selection.position,
            size: selection.size,
        });
    }

    pub fn start_stroke(&mut self) {
        if let EditState::Drawing { current_stroke, .. } = &mut self.edit_state {
            *current_stroke = Some(StrokeBuilder {
                points: Vec::new(),
                brush_size: self.brush_size,
                color: self.brush_color,
            });
        }
    }

    pub fn finish_stroke(&mut self) {
        if let EditState::Drawing {
            history,
            current_stroke,
        } = &mut self.edit_state
            && let Some(stroke) = current_stroke.take()
            && !stroke.points.is_empty()
        {
            history.push(DrawAction::Stroke {
                points: stroke.points,
                brush_size: stroke.brush_size,
                color: stroke.color,
            });
        }
    }

    pub fn draw_line(&mut self, from: Pos2, to: Pos2) {
        let layer = match self.draw_layer.as_mut() {
            Some(l) => l,
            None => return,
        };
        let [w, h] = layer.size;
        let radius = (self.brush_size / 2.0).max(0.5);
        let color = self.brush_color;

        bresenham_thick_line(layer, from, to, radius, color, w, h);

        if let EditState::Drawing { current_stroke, .. } = &mut self.edit_state
            && let Some(stroke) = current_stroke
        {
            stroke.points.push((from, to));
        }

        self.draw_texture_dirty = true;
    }

    pub fn undo(&mut self) {
        if matches!(self.edit_state, EditState::FloatingSelection { .. }) {
            self.commit_floating();
        }
        if let EditState::Drawing { history, .. } = &mut self.edit_state
            && history.undo().is_some()
            && let Some(ref original) = self.original_image
        {
            self.draw_layer = Some(history.replay(original.size));
            self.draw_texture_dirty = true;
        }
    }

    pub fn redo(&mut self) {
        if matches!(self.edit_state, EditState::FloatingSelection { .. }) {
            self.commit_floating();
        }
        if let EditState::Drawing { history, .. } = &mut self.edit_state
            && history.redo().is_some()
            && let Some(ref original) = self.original_image
        {
            self.draw_layer = Some(history.replay(original.size));
            self.draw_texture_dirty = true;
        }
    }

    pub fn can_undo(&self) -> bool {
        match &self.edit_state {
            EditState::Drawing { history, .. } => history.can_undo(),
            EditState::FloatingSelection { .. } => true, // commit + undo
            _ => false,
        }
    }

    pub fn can_redo(&self) -> bool {
        match &self.edit_state {
            EditState::Drawing { history, .. } => history.can_redo(),
            EditState::FloatingSelection { .. } => false,
            _ => false,
        }
    }

    /// Save the composited image (original + overlay + active floating selection) as PNG.
    pub fn save_png(&self, path: &str) -> Result<(), String> {
        self.composited_rgba()?
            .save(path)
            .map_err(|e| format!("Failed to save PNG: {}", e))
    }

    /// 합성 이미지를 PNG 로 쓰되 파일이 없을 때만 만든다. 이미 있으면 `Exists` 다.
    pub fn save_png_new(&self, path: &str) -> Result<(), SaveFailure> {
        let img = self.composited_rgba().map_err(SaveFailure::Failed)?;
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(SaveFailure::Exists(path.to_string()));
            }
            Err(e) => return Err(SaveFailure::Failed(format!("Failed to create {path}: {e}"))),
        };
        let mut writer = std::io::BufWriter::new(file);
        let written = img
            .write_to(&mut writer, image::ImageFormat::Png)
            .map_err(|e| e.to_string())
            .and_then(|()| std::io::Write::flush(&mut writer).map_err(|e| e.to_string()));
        if let Err(e) = written {
            // 만든 파일이 반쯤 쓰인 채 남지 않게 지운다.
            if let Err(rm) = std::fs::remove_file(path) {
                tracing::warn!("image: failed to remove partial {path}: {rm}");
            }
            return Err(SaveFailure::Failed(format!("Failed to save PNG: {e}")));
        }
        Ok(())
    }

    /// 원본 + 그리기 층 + 떠 있는 선택을 합친 RGBA 이미지.
    fn composited_rgba(&self) -> Result<image::RgbaImage, String> {
        let original = self.original_image.as_ref().ok_or("No image to save")?;
        let [w, h] = original.size;

        let mut composited = original.clone();
        if let Some(ref layer) = self.draw_layer {
            for i in 0..(w * h) {
                let bg = composited.pixels[i];
                let fg = layer.pixels[i];
                composited.pixels[i] = alpha_blend(bg, fg);
            }
        }

        if let EditState::FloatingSelection { ref selection, .. } = self.edit_state {
            blit_image(
                &mut composited,
                &selection.image,
                selection.position,
                selection.size,
                w,
                h,
            );
        }

        let mut rgba_data = Vec::with_capacity(w * h * 4);
        for pixel in &composited.pixels {
            rgba_data.push(pixel.r());
            rgba_data.push(pixel.g());
            rgba_data.push(pixel.b());
            rgba_data.push(pixel.a());
        }

        image::RgbaImage::from_raw(w as u32, h as u32, rgba_data)
            .ok_or_else(|| "Failed to create image buffer".to_string())
    }
}

/// `taken` 과 같은 폴더에서 `<stem>-<n>.png` 중 아직 없는 첫 이름. n 은 1 부터다.
pub(crate) fn next_free_png_path(taken: &str) -> String {
    let path = Path::new(taken);
    let stem = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    (1u32..)
        .map(|n| dir.join(format!("{stem}-{n}.png")))
        .find(|p| !p.exists())
        .map_or_else(|| taken.to_string(), |p| p.to_string_lossy().into_owned())
}

/// 경로 문자열에서 파일 이름 stem 의 글자 범위(시작, 끝). 확장자와 폴더 부분은 빠진다.
pub(crate) fn stem_char_range(path: &str) -> (usize, usize) {
    let name_start = path
        .rfind(['/', '\\'])
        .map_or(0, |i| path[..=i].chars().count());
    let total = path.chars().count();
    let name: String = path.chars().skip(name_start).collect();
    let stem_len = match name.rfind('.') {
        Some(dot) if dot > 0 => name[..dot].chars().count(),
        _ => name.chars().count(),
    };
    (name_start, (name_start + stem_len).min(total))
}

/// 파일을 읽지 못한 이유. 캔버스 상태 화면의 제목·글리프·이유 줄을 정한다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadFailure {
    /// 경로에 파일이 없다(옮겨졌거나 지워짐).
    Missing,
    /// 파일을 읽을 권한이 없다.
    Permission,
    /// 손상됐거나 지원하지 않는 포맷이다. 디코더 문구를 번역하지 않고 담는다.
    Decode(String),
    /// 한 변이 [`MAX_IMAGE_SIDE`] 를 넘거나 디코딩에 [`MAX_DECODE_ALLOC`] 보다 많은 메모리가
    /// 필요해 디코드 전에 거절했다. 헤더에서 읽은 그림 크기(px)를 담고, 헤더를 읽지 못했으면 None 이다.
    TooLarge(Option<[u32; 2]>),
}

impl LoadFailure {
    fn from_image_error(e: &image::ImageError) -> Self {
        match e {
            image::ImageError::IoError(io) => match io.kind() {
                std::io::ErrorKind::NotFound => Self::Missing,
                std::io::ErrorKind::PermissionDenied => Self::Permission,
                _ => Self::Decode(e.to_string()),
            },
            image::ImageError::Limits(_) => Self::TooLarge(None),
            _ => Self::Decode(e.to_string()),
        }
    }
}

/// 뷰어가 여는 그림의 한 변 상한(px). Explorer 미리보기의 상한과 같다.
pub(crate) const MAX_IMAGE_SIDE: u32 = 16384;

/// 디코더가 한 그림에 쓸 수 있는 메모리 상한. image 크레이트의 기본값을 명시한 것이다.
/// 뷰어는 원본을 줄이지 않고 올리므로 디코드 뒤에 RGBA 사본·텍스처 타일이 더해진다.
pub(crate) const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;

/// Load an image from a file path.
pub(crate) fn load_image_from_path(path: &str) -> Result<ColorImage, LoadFailure> {
    let decoded = image::ImageReader::open(path)
        .map_err(image::ImageError::IoError)
        .and_then(|mut reader| {
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_IMAGE_SIDE);
            limits.max_image_height = Some(MAX_IMAGE_SIDE);
            limits.max_alloc = Some(MAX_DECODE_ALLOC);
            reader.limits(limits);
            reader.decode()
        });
    let img = match decoded {
        Ok(img) => img,
        // 읽지 못한 이유를 로그에도 남긴다. 화면은 원인별 상태 화면을 보인다.
        Err(e) => {
            tracing::warn!("image: failed to decode {path}: {e}");
            return Err(match LoadFailure::from_image_error(&e) {
                LoadFailure::TooLarge(_) => LoadFailure::TooLarge(header_size(path)),
                other => other,
            });
        }
    };

    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    // 이미지 픽셀에서 읽은 외부 색상이다.
    #[allow(clippy::disallowed_methods)]
    let pixels: Vec<Color32> = rgba
        .pixels()
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();

    Ok(ColorImage {
        size: [w as usize, h as usize],
        pixels,
    })
}

/// 픽셀을 펼치지 않고 헤더에서 그림 크기를 읽는다. 상한에 걸린 그림의 실제 크기를 보이는 데 쓴다.
fn header_size(path: &str) -> Option<[u32; 2]> {
    let reader = image::ImageReader::open(path).ok()?;
    reader.into_dimensions().ok().map(|(w, h)| [w, h])
}

/// 확장자에 해당하는 디코더가 이 빌드에 포함됐는지 확인한다.
/// 지원 목록을 따로 복제하지 않고 image 크레이트의 reading_enabled를 사용한다.
pub(crate) fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(image::ImageFormat::from_extension)
        .is_some_and(|f| f.reading_enabled())
}

/// Scan the directory of the given file path for image files (sorted).
pub(crate) fn scan_directory_images(file_path: &str) -> Vec<String> {
    let path = Path::new(file_path);
    let parent = match path.parent() {
        Some(p) => p,
        None => return vec![file_path.to_string()],
    };

    let mut images: Vec<PathBuf> = match std::fs::read_dir(parent) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_image_file(p))
            .collect(),
        Err(_) => return vec![file_path.to_string()],
    };

    images.sort();
    images
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect()
}

/// Alpha blend foreground over background.
pub(crate) fn alpha_blend(bg: Color32, fg: Color32) -> Color32 {
    let fa = fg.a() as f32 / 255.0;
    if fa < 0.001 {
        return bg;
    }
    let ba = bg.a() as f32 / 255.0;
    let out_a = fa + ba * (1.0 - fa);
    if out_a < 0.001 {
        return Color32::TRANSPARENT;
    }
    let r = (fg.r() as f32 * fa + bg.r() as f32 * ba * (1.0 - fa)) / out_a;
    let g = (fg.g() as f32 * fa + bg.g() as f32 * ba * (1.0 - fa)) / out_a;
    let b = (fg.b() as f32 * fa + bg.b() as f32 * ba * (1.0 - fa)) / out_a;
    // 두 입력 색상을 합성한 결과다.
    #[allow(clippy::disallowed_methods)]
    {
        Color32::from_rgba_unmultiplied(r as u8, g as u8, b as u8, (out_a * 255.0) as u8)
    }
}

/// Draw a thick line using Bresenham's algorithm with a circle brush.
pub(crate) fn bresenham_thick_line(
    layer: &mut ColorImage,
    from: Pos2,
    to: Pos2,
    radius: f32,
    color: Color32,
    w: usize,
    h: usize,
) {
    let dx = (to.x - from.x).abs();
    let dy = (to.y - from.y).abs();
    let steps = dx.max(dy).ceil() as i32;
    let steps = steps.max(1);

    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = from.x + (to.x - from.x) * t;
        let y = from.y + (to.y - from.y) * t;
        fill_circle(layer, x, y, radius, color, w, h);
    }
}

/// Blit a source image onto a target layer at the given position, scaled to `dest_size`.
pub(crate) fn blit_image(
    layer: &mut ColorImage,
    src: &ColorImage,
    position: Vec2,
    dest_size: [usize; 2],
    layer_w: usize,
    layer_h: usize,
) {
    let [src_w, src_h] = src.size;
    let [dst_w, dst_h] = dest_size;
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }
    let ox = position.x as i32;
    let oy = position.y as i32;
    for dy in 0..dst_h as i32 {
        let py = oy + dy;
        if py < 0 || py >= layer_h as i32 {
            continue;
        }
        for dx in 0..dst_w as i32 {
            let px = ox + dx;
            if px < 0 || px >= layer_w as i32 {
                continue;
            }
            let sx = (dx as usize * src_w) / dst_w;
            let sy = (dy as usize * src_h) / dst_h;
            let fg = src.pixels[sy * src_w + sx];
            if fg.a() == 0 {
                continue;
            }
            let idx = py as usize * layer_w + px as usize;
            let bg = layer.pixels[idx];
            layer.pixels[idx] = alpha_blend(bg, fg);
        }
    }
}

/// Fill a circle of pixels at (cx, cy) with the given radius.
pub(crate) fn fill_circle(
    layer: &mut ColorImage,
    cx: f32,
    cy: f32,
    radius: f32,
    color: Color32,
    w: usize,
    h: usize,
) {
    let r = radius.ceil() as i32;
    let cx_i = cx as i32;
    let cy_i = cy as i32;
    let r_sq = radius * radius;

    for dy in -r..=r {
        for dx in -r..=r {
            if (dx * dx + dy * dy) as f32 <= r_sq {
                let px = cx_i + dx;
                let py = cy_i + dy;
                if px >= 0 && px < w as i32 && py >= 0 && py < h as i32 {
                    layer.pixels[py as usize * w + px as usize] = color;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_doc_starts_in_edit_mode_after_load() {
        let mut doc = ImageDoc::new(None);
        doc.ensure_loaded();
        assert!(doc.is_editing());
        assert!(doc.original_image.is_some());
    }

    #[test]
    fn every_extension_the_navigator_accepts_can_actually_be_decoded() {
        // 지원하는 확장자인지 확인하고 인코더도 있는 포맷은 실제 왕복을 검사한다.
        for ext in [
            "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tiff", "tif",
        ] {
            let path = Path::new("x").with_extension(ext);
            assert!(
                is_image_file(&path),
                "매니페스트가 선언한 {ext}를 파일 목록에 포함해야 한다"
            );

            let fmt = image::ImageFormat::from_extension(ext).expect("확장자→포맷");
            assert!(fmt.reading_enabled(), "{ext}: 디코더가 안 켜져 있다");

            // 인코더가 없는 포맷은 읽기 지원 여부까지만 확인한다.
            if !fmt.writing_enabled() {
                continue;
            }
            let src = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
            let mut buf = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(src)
                .write_to(&mut buf, fmt)
                .unwrap_or_else(|e| panic!("{ext} 인코드 실패: {e}"));
            let decoded = image::load_from_memory_with_format(buf.get_ref(), fmt)
                .unwrap_or_else(|e| panic!("{ext} 디코드 실패: {e}"));
            assert_eq!(
                decoded.width(),
                1,
                "{ext}: 저장 후 다시 읽은 이미지 너비가 달라졌다"
            );
        }
    }

    #[test]
    fn an_extension_with_no_decoder_is_not_offered() {
        // SVG 디코더는 이 빌드에 없다.
        assert!(!is_image_file(Path::new("x.svg")));
    }

    #[test]
    fn missing_file_loads_as_none() {
        let mut doc = ImageDoc::new(Some("\0nonexistent".into()));
        doc.ensure_loaded();
        assert!(doc.original_image.is_none());
        assert!(!doc.is_editing());
    }

    /// 첫 픽셀로 읽은 파일을 구분할 수 있도록 단색 PNG를 만든다.
    fn write_probe_png(path: &std::path::Path, rgb: [u8; 3]) {
        let img = image::RgbImage::from_pixel(4, 4, image::Rgb(rgb));
        img.save(path).expect("probe png 저장 실패");
    }

    fn probe_png_path(what: &str) -> PathBuf {
        // 시간 해상도에 의존하지 않도록 PID와 단조 카운터로 시험 파일명을 구분한다.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "tasty-image-{what}-{}-{:?}.png",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn first_pixel(doc: &ImageDoc) -> Color32 {
        doc.original_image
            .as_ref()
            .expect("픽셀이 있어야 한다")
            .pixels[0]
    }

    /// 편집 중이 아니면 외부 변경을 바로 반영해야 한다.
    #[test]
    fn an_external_change_is_applied_at_once_when_not_editing() {
        let path = probe_png_path("apply");
        write_probe_png(&path, [255, 0, 0]);
        let mut doc = ImageDoc::new(Some(path.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        assert_eq!(first_pixel(&doc).r(), 255, "전제: 첫 세대를 읽었다");

        write_probe_png(&path, [0, 255, 0]);
        assert!(
            doc.apply_external_change(),
            "편집 중이 아니면 곧바로 반영한다"
        );
        assert_eq!(first_pixel(&doc).g(), 255, "두 번째 세대가 보여야 한다");
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    /// 편집 중에는 기존 이미지를 유지하고, 편집이 끝나면 미룬 변경을 읽어야 한다.
    #[test]
    fn an_external_change_during_an_edit_is_deferred_not_lost() {
        let path = probe_png_path("defer");
        write_probe_png(&path, [255, 0, 0]);
        let mut doc = ImageDoc::new(Some(path.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        doc.enter_edit_mode();
        assert!(doc.is_editing(), "전제: 편집 세션이 활성이어야 한다");

        write_probe_png(&path, [0, 255, 0]);
        assert!(
            !doc.apply_external_change(),
            "편집 중에는 원본 이미지를 바꾸지 않아야 한다"
        );
        assert_eq!(
            first_pixel(&doc).r(),
            255,
            "편집 중에는 첫 세대가 그대로 보여야 한다"
        );

        doc.exit_edit_mode();
        assert_eq!(
            first_pixel(&doc).g(),
            255,
            "편집을 끝내면 미뤄 둔 파일 변경을 반영해야 한다"
        );
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    /// 외부 변경을 받은 적이 없으면 편집 종료 때 다시 읽지 않는다.
    #[test]
    fn leaving_an_edit_without_a_pending_change_does_not_reload() {
        let path = probe_png_path("nopending");
        write_probe_png(&path, [255, 0, 0]);
        let mut doc = ImageDoc::new(Some(path.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        doc.enter_edit_mode();

        // 감시자가 아무것도 안 알린 채로 파일만 바뀐 상태를 만든다.
        write_probe_png(&path, [0, 255, 0]);
        doc.exit_edit_mode();
        assert_eq!(
            first_pixel(&doc).r(),
            255,
            "외부 변경 기록이 없으면 편집 종료 때 다시 읽지 않아야 한다"
        );
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    /// 파일을 연 문서에서 새 캔버스를 만들면 원래 파일 경로를 저장 대상으로 쓰지 않아야 한다.
    #[test]
    fn a_new_canvas_does_not_keep_the_open_file_as_its_save_path() {
        let path = probe_png_path("newcanvas");
        write_probe_png(&path, [255, 0, 0]);
        let mut doc = ImageDoc::new(Some(path.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        assert!(doc.save_path().is_some(), "전제: 연 파일이 저장 대상이다");

        doc.create_blank_canvas(8, 8);
        assert_eq!(
            doc.save_path(),
            None,
            "새 캔버스는 저장할 경로를 물어야 한다"
        );
        assert!(doc.is_blank());
        assert!(
            doc.dir_images.is_empty(),
            "새 캔버스에서 원래 폴더의 이전·다음 이미지로 넘어가지 않아야 한다"
        );
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    /// 같은 이름 `.png` 가 없는 JPG 문서의 쓸 경로를 만든다. 반환: (jpg, 옆 png).
    fn probe_jpg(what: &str) -> (PathBuf, PathBuf) {
        let png = probe_png_path(what);
        let jpg = png.with_extension("jpg");
        image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
            .save(&jpg)
            .expect("probe jpg 저장 실패");
        (jpg, png)
    }

    /// 비-PNG 문서의 Save 는 옆의 같은 이름 `.png` 에 쓰고 문서를 그 파일로 옮긴다.
    /// 원본 JPG 는 그대로이고, 화면은 편집이 반영된 `.png` 를 보여 준다.
    #[test]
    fn toolbar_save_of_a_jpg_writes_a_png_beside_it_and_moves_the_document() {
        let (jpg, png) = probe_jpg("save-jpg");
        let jpg_bytes = std::fs::read(&jpg).expect("jpg 읽기");
        let mut doc = ImageDoc::new(Some(jpg.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        doc.enter_edit_mode();
        doc.brush_color = Color32::BLUE;
        doc.start_stroke();
        doc.draw_line(Pos2::new(1.0, 1.0), Pos2::new(1.0, 1.0));
        doc.finish_stroke();

        doc.save_from_toolbar();
        let png_s = png.to_string_lossy().into_owned();
        assert!(png.exists(), "옆에 .png 를 써야 한다");
        assert_eq!(doc.file_path.as_deref(), Some(png_s.as_str()));
        assert_eq!(doc.take_path_for_host().as_deref(), Some(png_s.as_str()));
        assert!(!doc.is_editing());
        assert!(!doc.save_path_popup);
        let shown = doc
            .original_image
            .as_ref()
            .expect("저장한 파일을 다시 읽어야 한다");
        assert_eq!(
            shown.pixels[4 + 1],
            Color32::BLUE,
            "다시 읽은 화면에 편집이 남아 있어야 한다"
        );
        assert_eq!(
            std::fs::read(&jpg).expect("jpg 읽기"),
            jpg_bytes,
            "원본 JPG 는 바뀌지 않아야 한다"
        );
        let _ = std::fs::remove_file(&jpg); // best-effort 정리 — 실패 무시.
        let _ = std::fs::remove_file(&png); // best-effort 정리 — 실패 무시.
    }

    /// 같은 이름 `.png` 가 이미 있으면 덮어쓰지 않고 경로 입력 팝업을 연다. 편집은 그대로다.
    #[test]
    fn toolbar_save_of_a_jpg_does_not_overwrite_an_existing_png() {
        let (jpg, png) = probe_jpg("save-jpg-exists");
        write_probe_png(&png, [0, 255, 0]);
        let png_bytes = std::fs::read(&png).expect("png 읽기");
        let jpg_s = jpg.to_string_lossy().into_owned();
        let mut doc = ImageDoc::new(Some(jpg_s.clone()));
        doc.ensure_loaded();
        doc.enter_edit_mode();
        assert_eq!(
            doc.save_target(),
            SaveTarget::Exists(png.to_string_lossy().into_owned())
        );

        doc.save_from_toolbar();
        assert!(doc.save_path_popup, "경로 입력 팝업을 열어야 한다");
        let png_name = png.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(doc.save_path_clash.as_deref(), Some(png_name.as_str()));
        let stem = png.file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            doc.save_path_buffer,
            png.with_file_name(format!("{stem}-1.png"))
                .to_string_lossy()
                .into_owned(),
            "입력칸에 다음 빈 이름을 넣어야 한다"
        );
        assert!(doc.save_path_select_stem);
        doc.close_save_as();
        assert_eq!(doc.save_path_clash, None);
        assert!(doc.is_editing(), "편집 세션은 그대로 남아야 한다");
        assert_eq!(doc.file_path.as_deref(), Some(jpg_s.as_str()));
        assert_eq!(doc.take_path_for_host(), None);
        assert_eq!(std::fs::read(&png).expect("png 읽기"), png_bytes);
        let _ = std::fs::remove_file(&jpg); // best-effort 정리 — 실패 무시.
        let _ = std::fs::remove_file(&png); // best-effort 정리 — 실패 무시.
    }

    /// 다음 빈 이름은 `-1` 부터 이미 있는 이름을 건너뛴다.
    #[test]
    fn the_next_free_png_name_skips_taken_numbers() {
        let dir = probe_dir("next-free");
        let taken = dir.join("diagram.png");
        write_probe_png(&taken, [0, 0, 0]);
        write_probe_png(&dir.join("diagram-1.png"), [0, 0, 0]);
        assert_eq!(
            next_free_png_path(&taken.to_string_lossy()),
            dir.join("diagram-2.png").to_string_lossy()
        );
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// stem 범위는 폴더와 확장자를 뺀 파일 이름 부분이고 글자 단위다.
    #[test]
    fn the_stem_range_covers_only_the_file_name_stem() {
        assert_eq!(stem_char_range("/x/y/diagram-1.png"), (5, 14));
        assert_eq!(stem_char_range("그림-1.png"), (0, 4));
        assert_eq!(stem_char_range("C:\\pics\\a.png"), (8, 9));
        assert_eq!(stem_char_range("noext"), (0, 5));
    }

    /// 읽지 못한 원인을 없음 · 디코드로 나누고, 다시 읽어 성공하면 원인을 지운다.
    #[test]
    fn a_load_failure_records_its_cause_and_clears_on_success() {
        let dir = probe_dir("load-failure");
        let path = dir.join("pic.png");
        let file = path.to_string_lossy().into_owned();
        let mut doc = ImageDoc::new(Some(file.clone()));
        doc.ensure_loaded();
        assert!(doc.original_image.is_none());
        assert_eq!(doc.load_failure, Some(LoadFailure::Missing));

        std::fs::write(&path, b"not a png").expect("쓰기");
        doc.reload_from_disk();
        match &doc.load_failure {
            Some(LoadFailure::Decode(msg)) => assert!(!msg.is_empty()),
            other => panic!("디코드 실패여야 한다: {other:?}"),
        }

        write_probe_png(&path, [0, 255, 0]);
        doc.reload_from_disk();
        assert!(doc.original_image.is_some());
        assert_eq!(doc.load_failure, None);
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// 픽셀이 없는 PNG. 상한 판정은 IHDR 의 크기로 픽셀을 펼치기 전에 끝난다.
    fn write_png_header(path: &std::path::Path, w: u32, h: u32) {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let crc = crc32(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
        }
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for b in bytes {
                crc ^= u32::from(*b);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        // 8비트 RGBA, 압축·필터·인터레이스 기본값.
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut out, b"IHDR", &ihdr);
        // 디코더는 첫 IDAT 까지 읽어야 머리를 끝낸다. 내용은 빈 zlib 스트림이다.
        chunk(
            &mut out,
            b"IDAT",
            &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        );
        chunk(&mut out, b"IEND", &[]);
        std::fs::write(path, out).expect("쓰기");
    }

    /// 한 변이 상한을 넘거나 디코딩 메모리가 상한을 넘으면 펼치기 전에 '너무 큼'으로 거절하고,
    /// 상한과 같은 변은 연다.
    #[test]
    fn an_image_over_the_limits_is_refused_as_too_large() {
        let dir = probe_dir("load-too-large");
        let wide = dir.join("wide.png");
        write_png_header(&wide, MAX_IMAGE_SIDE + 1, 1);
        assert_eq!(
            load_image_from_path(&wide.to_string_lossy()).err(),
            Some(LoadFailure::TooLarge(Some([MAX_IMAGE_SIDE + 1, 1])))
        );
        let tall = dir.join("tall.png");
        write_png_header(&tall, 1, MAX_IMAGE_SIDE + 1);
        assert_eq!(
            load_image_from_path(&tall.to_string_lossy()).err(),
            Some(LoadFailure::TooLarge(Some([1, MAX_IMAGE_SIDE + 1])))
        );
        // 한 변은 상한 안이지만 RGBA 로 펼치면 MAX_DECODE_ALLOC 을 넘는다.
        let side = 12_000u32;
        assert!(u64::from(side) * u64::from(side) * 4 > MAX_DECODE_ALLOC);
        let heavy = dir.join("heavy.png");
        write_png_header(&heavy, side, side);
        assert_eq!(
            load_image_from_path(&heavy.to_string_lossy()).err(),
            Some(LoadFailure::TooLarge(Some([side, side])))
        );

        let edge = dir.join("edge.png");
        image::RgbaImage::from_pixel(MAX_IMAGE_SIDE, 1, image::Rgba([1, 2, 3, 255]))
            .save(&edge)
            .expect("PNG 저장");
        let loaded = load_image_from_path(&edge.to_string_lossy()).expect("상한과 같은 변은 연다");
        assert_eq!(loaded.size, [MAX_IMAGE_SIDE as usize, 1]);

        let mut doc = ImageDoc::new(Some(wide.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        assert!(doc.original_image.is_none());
        assert_eq!(
            doc.load_failure,
            Some(LoadFailure::TooLarge(Some([MAX_IMAGE_SIDE + 1, 1])))
        );
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// 읽기 권한이 없는 파일은 권한 실패로 나눈다.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_a_permission_failure() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = probe_dir("load-permission");
        let path = dir.join("locked.png");
        write_probe_png(&path, [0, 0, 255]);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // root 는 권한과 관계없이 읽으므로 이 판정을 할 수 없다.
        if std::fs::File::open(&path).is_ok() {
            let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
            return;
        }
        let mut doc = ImageDoc::new(Some(path.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        assert_eq!(doc.load_failure, Some(LoadFailure::Permission));
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// 이 시험만 쓰는 빈 폴더. 대소문자만 다른 파일명이 다른 시험과 겹치지 않게 한다.
    fn probe_dir(what: &str) -> PathBuf {
        let dir = probe_png_path(what).with_extension("");
        std::fs::create_dir_all(&dir).expect("시험 폴더 생성");
        dir
    }

    /// 확장자가 대문자인 PNG 문서도 자기 파일에 쓰고, 소문자 사본을 만들거나 문서를 옮기지 않는다.
    #[test]
    fn an_upper_case_png_document_saves_over_its_own_file() {
        let dir = probe_dir("save-upper-png");
        let path = dir.join("IMG.PNG");
        write_probe_png(&path, [255, 0, 0]);
        let before = std::fs::read(&path).expect("png 읽기");
        let file = path.to_string_lossy().into_owned();
        let mut doc = ImageDoc::new(Some(file.clone()));
        doc.ensure_loaded();
        assert_eq!(doc.save_target(), SaveTarget::Write(file.clone()));

        doc.paste_image(ColorImage::new([2, 2], Color32::BLUE));
        doc.save_from_toolbar();
        assert!(!doc.save_path_popup);
        assert_eq!(doc.file_path.as_deref(), Some(file.as_str()));
        assert_eq!(doc.take_path_for_host(), None);
        assert_ne!(
            std::fs::read(&path).expect("png 읽기"),
            before,
            "자기 파일에 편집을 써야 한다"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .expect("폴더 읽기")
            .map(|e| e.expect("항목").file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("IMG.PNG")]);
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// 확장자가 대문자인 비-PNG 문서는 같은 이름의 소문자 `.png` 를 대상으로 한다.
    #[test]
    fn an_upper_case_jpg_document_targets_the_png_beside_it() {
        let dir = probe_dir("save-upper-jpg");
        let jpg = dir.join("IMG.JPG");
        image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
            .save_with_format(&jpg, image::ImageFormat::Jpeg)
            .expect("probe jpg 저장 실패");
        let doc = ImageDoc::new(Some(jpg.to_string_lossy().into_owned()));
        assert_eq!(
            doc.save_target(),
            SaveTarget::Write(dir.join("IMG.png").to_string_lossy().into_owned())
        );
        let _ = std::fs::remove_dir_all(&dir); // best-effort 정리 — 실패 무시.
    }

    /// 판단 뒤 쓰기 전에 옆 `.png` 가 생겼으면 덮어쓰지 않고 Exists 로 끝난다.
    #[test]
    fn a_png_created_after_the_decision_is_not_overwritten() {
        let (jpg, png) = probe_jpg("save-race");
        let mut doc = ImageDoc::new(Some(jpg.to_string_lossy().into_owned()));
        doc.ensure_loaded();
        let png_s = png.to_string_lossy().into_owned();
        assert_eq!(doc.save_target(), SaveTarget::Write(png_s.clone()));

        write_probe_png(&png, [0, 255, 0]);
        let other = std::fs::read(&png).expect("png 읽기");
        assert_eq!(
            doc.write_save_target(&png_s),
            Err(SaveFailure::Exists(png_s.clone()))
        );
        assert_eq!(std::fs::read(&png).expect("png 읽기"), other);
        let _ = std::fs::remove_file(&jpg); // best-effort 정리 — 실패 무시.
        let _ = std::fs::remove_file(&png); // best-effort 정리 — 실패 무시.
    }

    /// PNG 문서는 자기 파일에 쓴다. 파일이 이미 있는 것이 정상이다.
    #[test]
    fn a_png_document_saves_over_its_own_file() {
        let path = probe_png_path("save-own");
        write_probe_png(&path, [255, 0, 0]);
        let file = path.to_string_lossy().into_owned();
        let doc = ImageDoc::new(Some(file.clone()));
        assert_eq!(doc.save_target(), SaveTarget::Write(file));
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    /// 붙여넣기 단축키는 클립보드 이미지를 떠 있는 선택으로 붙이고, 이미지가 아니면 무시한다.
    #[test]
    fn paste_shortcut_floats_a_clipboard_image_and_ignores_other_content() {
        let path = probe_png_path("paste");
        write_probe_png(&path, [255, 0, 0]);
        let mut doc = ImageDoc::new(Some(path.to_string_lossy().into_owned()));
        doc.ensure_loaded();

        assert!(!doc.paste_from_clipboard(|| Err("text only".into())));
        assert!(
            !doc.is_editing(),
            "이미지가 아니면 문서를 바꾸지 않아야 한다"
        );

        let clip = ColorImage::new([2, 3], Color32::YELLOW);
        assert!(doc.paste_from_clipboard(|| Ok(clip)));
        match &doc.edit_state {
            EditState::FloatingSelection { selection, .. } => {
                assert_eq!(selection.size, [2, 3]);
            }
            _ => panic!("떠 있는 선택이어야 한다"),
        }
        let _ = std::fs::remove_file(&path); // best-effort 정리 — 실패 무시.
    }

    #[test]
    fn undo_redo_roundtrip_on_blank() {
        let mut doc = ImageDoc::new(None);
        doc.ensure_loaded();
        doc.start_stroke();
        doc.draw_line(Pos2::new(1.0, 1.0), Pos2::new(5.0, 5.0));
        doc.finish_stroke();
        assert!(doc.can_undo());
        doc.undo();
        assert!(doc.can_redo());
        doc.redo();
        assert!(doc.can_undo());
    }
}
