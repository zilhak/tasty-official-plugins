//! 이미지의 도구 모음과 캔버스를 플러그인에서 그린다.
//! 문서의 보기·편집 상태에 따라 버튼과 그리기 영역을 구성한다.

/// 빌드 때 만든 아이콘 폴리라인. 런타임에 크기를 맞춰 선으로 그린다.
mod baked_icons {
    include!(concat!(env!("OUT_DIR"), "/plugin_icons.rs"));
}

use egui::emath::GuiRounding as _;
use tasty_plugin_sdk::Translator;
use tasty_type_appearance::theme::Theme;
use tasty_ui_widgets::{
    Button, ButtonVariant, ControlSize, IconButton, Input, StateGlyph, StateScreenView,
    state_screen,
};

use crate::doc::{
    DragState, EditState, ImageDoc, LoadFailure, MAX_DECODE_ALLOC, MAX_IMAGE_SIDE, ResizeHandle,
};
use crate::tiled::TiledTexture;

/// Render one frame of the image surface into `ctx`.
pub(crate) fn draw(ctx: &egui::Context, theme: &Theme, tr: &Translator, doc: &mut ImageDoc) {
    let frame = egui::Frame::new().fill(theme.bg_panel().to_egui());
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        if doc.new_image_popup {
            draw_new_image_popup(ui, theme, tr, doc);
            return;
        }
        if doc.save_path_popup {
            draw_save_path_popup(ui, theme, tr, doc);
            return;
        }

        let pad = theme.spacing_sm.value();

        ui.add_space(pad);
        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.spacing_mut().item_spacing.x = theme.spacing_xs.value();
            if doc.is_editing() {
                draw_edit_controls(ui, theme, tr, doc);
            } else {
                draw_viewer_controls(ui, theme, tr, doc);
            }
            ui.add_space(pad);
        });
        ui.add_space(pad);

        let sep_y = ui.min_rect().bottom();
        ui.painter().hline(
            ui.max_rect().x_range(),
            sep_y,
            egui::Stroke::new(
                theme.border_width.value(),
                theme.separator.to_egui_premultiplied(),
            ),
        );

        draw_canvas(ui, theme, tr, doc);
    });
}

fn draw_viewer_controls(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, doc: &mut ImageDoc) {
    let has_dir = doc.dir_images.len() > 1;

    if has_dir {
        if icon_button(
            ui,
            theme,
            baked_icons::CHEVRON_LEFT,
            tr.t("image_viewer.prev"),
            true,
        )
        .clicked()
            && !doc.is_editing()
            && doc.step_prev().is_some()
        {
            doc.load_after_navigation();
        }
        if icon_button(
            ui,
            theme,
            baked_icons::CHEVRON_RIGHT,
            tr.t("image_viewer.next"),
            true,
        )
        .clicked()
            && !doc.is_editing()
            && doc.step_next().is_some()
        {
            doc.load_after_navigation();
        }
    }

    if icon_button(
        ui,
        theme,
        baked_icons::REFRESH,
        tr.t("image_viewer.refresh"),
        true,
    )
    .clicked()
    {
        doc.reload_from_disk();
    }

    if doc.original_image.is_some()
        && icon_button(
            ui,
            theme,
            baked_icons::EDIT,
            tr.t("image_viewer.edit"),
            true,
        )
        .clicked()
    {
        doc.enter_edit_mode();
    }

    if icon_button(
        ui,
        theme,
        baked_icons::PLUS,
        tr.t("image_viewer.new_image"),
        true,
    )
    .clicked()
    {
        doc.new_image_popup = true;
    }

    ui.add_space(theme.spacing_sm.value());

    if let Some(ref path) = doc.file_path {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let info = if doc.dir_images.len() > 1 {
            format!(
                "{} ({}/{})",
                name,
                doc.current_index + 1,
                doc.dir_images.len()
            )
        } else {
            name
        };
        ui.label(caption(theme, &info));
    }

    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        draw_zoom_controls(ui, theme, tr, doc);
    });
}

fn draw_edit_controls(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, doc: &mut ImageDoc) {
    if Button::new(tr.t("image_viewer.save"))
        .variant(ButtonVariant::Secondary)
        .size(ControlSize::Sm)
        .show(ui, theme)
        .clicked()
    {
        doc.save_from_toolbar();
    }

    if Button::new(tr.t("image_viewer.cancel"))
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Sm)
        .show(ui, theme)
        .clicked()
    {
        doc.exit_edit_mode();
    }

    let undo_enabled = doc.can_undo();
    if icon_button(
        ui,
        theme,
        baked_icons::UNDO,
        tr.t("image_viewer.undo"),
        undo_enabled,
    )
    .clicked()
    {
        doc.undo();
    }
    let redo_enabled = doc.can_redo();
    if icon_button(
        ui,
        theme,
        baked_icons::REDO,
        tr.t("image_viewer.redo"),
        redo_enabled,
    )
    .clicked()
    {
        doc.redo();
    }

    ui.separator();

    ui.label(caption(theme, tr.t("image_viewer.brush_size")));
    ui.add(egui::Slider::new(&mut doc.brush_size, 1.0..=20.0).show_value(false));

    ui.label(caption(theme, tr.t("image_viewer.color")));
    let mut color_arr = [
        doc.brush_color.r(),
        doc.brush_color.g(),
        doc.brush_color.b(),
    ];
    if ui.color_edit_button_srgb(&mut color_arr).changed() {
        // 사용자가 색상 선택기에서 고른 값이다.
        #[allow(clippy::disallowed_methods)]
        let new_color = egui::Color32::from_rgb(color_arr[0], color_arr[1], color_arr[2]);
        doc.brush_color = new_color;
    }

    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        draw_zoom_controls(ui, theme, tr, doc);
    });
}

/// 시안 ZoomGroup: Fit은 Secondary sm Button, +/−는 sm IconButton이다.
fn draw_zoom_controls(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, doc: &mut ImageDoc) {
    // right_to_left layout: add in reverse visual order (-, %, +, Fit).
    if icon_button(
        ui,
        theme,
        baked_icons::MINUS,
        tr.t("image_viewer.zoom_out"),
        true,
    )
    .clicked()
    {
        doc.zoom = (doc.zoom / 1.25).max(0.1);
    }
    let zoom_pct = format!("{}%", (doc.zoom * 100.0).round_ui() as i32);
    zoom_label(ui, theme, &zoom_pct);
    if icon_button(
        ui,
        theme,
        baked_icons::PLUS,
        tr.t("image_viewer.zoom_in"),
        true,
    )
    .clicked()
    {
        doc.zoom = (doc.zoom * 1.25).min(20.0);
    }
    if Button::new(tr.t("image_viewer.fit"))
        .variant(ButtonVariant::Secondary)
        .size(ControlSize::Sm)
        .show(ui, theme)
        .clicked()
    {
        doc.zoom = 1.0;
        doc.pan_offset = egui::Vec2::ZERO;
    }
}

/// 원본 이미지와 편집 레이어 텍스처를 필요할 때만 올린다.
fn ensure_textures(ui: &egui::Ui, doc: &mut ImageDoc) {
    if doc.texture.is_none()
        && let Some(img) = doc.original_image.as_ref()
    {
        doc.texture = Some(TiledTexture::load(
            ui.ctx(),
            "image_original",
            img,
            egui::TextureOptions::LINEAR,
        ));
    }
    if doc.is_editing()
        && let Some(layer) = doc.draw_layer.as_ref()
        && (doc.draw_texture.is_none() || doc.draw_texture_dirty)
    {
        doc.draw_texture = Some(TiledTexture::load(
            ui.ctx(),
            "image_draw",
            layer,
            egui::TextureOptions::LINEAR,
        ));
        doc.draw_texture_dirty = false;
    }
}

fn draw_canvas(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, doc: &mut ImageDoc) {
    let available = ui.available_rect_before_wrap();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(available.width(), available.height()),
        egui::Sense::click_and_drag(),
    );

    ui.painter()
        .rect_filled(rect, 0.0, theme.bg_sidebar().to_egui());

    let Some(img) = doc.original_image.as_ref() else {
        if canvas_state(ui, theme, tr, rect, doc) {
            doc.reload_from_disk();
        }
        return;
    };
    let [img_w, img_h] = img.size;
    ensure_textures(ui, doc);

    // Compute display size with zoom; fit-to-window when zoom <= 1.0.
    let zoom = doc.zoom;
    let (final_w, final_h, effective_zoom) = if zoom <= 1.0 {
        let scale_x = rect.width() / img_w as f32;
        let scale_y = rect.height() / img_h as f32;
        let fit = scale_x.min(scale_y).min(1.0);
        (img_w as f32 * fit, img_h as f32 * fit, fit)
    } else {
        (img_w as f32 * zoom, img_h as f32 * zoom, zoom)
    };

    let center = rect.center() + doc.pan_offset;
    let img_rect = egui::Rect::from_center_size(center, egui::vec2(final_w, final_h));

    if let Some(ref tex) = doc.texture {
        tex.paint(ui.painter(), img_rect, egui::Color32::WHITE);
    }
    if doc.is_editing()
        && let Some(ref tex) = doc.draw_texture
    {
        tex.paint(ui.painter(), img_rect, egui::Color32::WHITE);
    }

    // Zoom with mouse wheel (only over the canvas).
    let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
    if scroll_delta != 0.0
        && rect.contains(ui.input(|i| i.pointer.latest_pos().unwrap_or_default()))
    {
        let factor = if scroll_delta > 0.0 { 1.1 } else { 1.0 / 1.1 };
        doc.zoom = (doc.zoom * factor).clamp(0.1, 20.0);
    }

    // Double click resets zoom / pan.
    if response.double_clicked() {
        doc.zoom = 1.0;
        doc.pan_offset = egui::Vec2::ZERO;
    }

    // Esc cancels a floating selection.
    let esc_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if esc_pressed && matches!(doc.edit_state, EditState::FloatingSelection { .. }) {
        doc.cancel_floating();
    }

    if matches!(doc.edit_state, EditState::FloatingSelection { .. }) {
        draw_floating_selection(ui, theme, doc, img_rect, effective_zoom, &response);
    } else if doc.is_editing() {
        // Drawing mode: drag to draw.
        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let img_x = (pos.x - img_rect.min.x) / effective_zoom;
                let img_y = (pos.y - img_rect.min.y) / effective_zoom;
                let img_pos = egui::pos2(img_x, img_y);
                if doc.last_draw_pos.is_none() {
                    doc.start_stroke();
                }
                if let Some(last) = doc.last_draw_pos {
                    doc.draw_line(last, img_pos);
                } else {
                    doc.draw_line(img_pos, img_pos);
                }
                doc.last_draw_pos = Some(img_pos);
            }
        } else {
            if doc.last_draw_pos.is_some() {
                doc.finish_stroke();
            }
            doc.last_draw_pos = None;
        }
    } else if response.dragged() && doc.zoom > 1.0 {
        // Viewer mode: drag to pan (only when zoomed in).
        doc.pan_offset += response.drag_delta();
    }
}

fn draw_floating_selection(
    ui: &mut egui::Ui,
    theme: &Theme,
    doc: &mut ImageDoc,
    img_rect: egui::Rect,
    effective_zoom: f32,
    response: &egui::Response,
) {
    let handle_size = theme.image_handle_size().value();

    let sel_screen_rect = if let EditState::FloatingSelection {
        ref mut selection, ..
    } = doc.edit_state
    {
        if selection.texture.is_none() {
            selection.texture = Some(TiledTexture::load(
                ui.ctx(),
                "image_float",
                &selection.image,
                egui::TextureOptions::LINEAR,
            ));
        }
        let sel_x = img_rect.min.x + selection.position.x * effective_zoom;
        let sel_y = img_rect.min.y + selection.position.y * effective_zoom;
        let sel_w = selection.size[0] as f32 * effective_zoom;
        let sel_h = selection.size[1] as f32 * effective_zoom;
        let sel_rect =
            egui::Rect::from_min_size(egui::pos2(sel_x, sel_y), egui::vec2(sel_w, sel_h));

        if let Some(ref tex) = selection.texture {
            tex.paint(ui.painter(), sel_rect, egui::Color32::WHITE);
        }

        let stroke =
            egui::Stroke::new(theme.border_width.value(), theme.accent_primary().to_egui());
        ui.painter()
            .rect_stroke(sel_rect, 0.0, stroke, egui::StrokeKind::Outside);

        for (_handle, handle_rect) in resize_handle_rects(sel_rect, handle_size) {
            ui.painter()
                .rect_filled(handle_rect, 0.0, theme.accent_primary().to_egui());
        }
        sel_rect
    } else {
        return;
    };

    let handles = resize_handle_rects(sel_screen_rect, handle_size);

    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let found_handle = handles
                .iter()
                .find(|(_, r)| r.contains(pos))
                .map(|(h, _)| *h);

            let should_commit = if let EditState::FloatingSelection {
                ref mut selection, ..
            } = doc.edit_state
            {
                if let Some(handle) = found_handle {
                    selection.drag_state = DragState::Resizing {
                        handle,
                        drag_start_pos: pos,
                        initial_rect: sel_screen_rect,
                    };
                    false
                } else if sel_screen_rect.contains(pos) {
                    selection.drag_state = DragState::Moving {
                        drag_start_pos: pos,
                        initial_position: selection.position,
                    };
                    false
                } else {
                    true
                }
            } else {
                false
            };
            if should_commit {
                doc.commit_floating();
            }
        }
    } else if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos()
            && let EditState::FloatingSelection {
                ref mut selection, ..
            } = doc.edit_state
        {
            match selection.drag_state.clone() {
                DragState::Moving {
                    drag_start_pos,
                    initial_position,
                } => {
                    let delta = pos - drag_start_pos;
                    selection.position = initial_position + delta / effective_zoom;
                }
                DragState::Resizing {
                    handle: _,
                    drag_start_pos,
                    initial_rect,
                } => {
                    let delta = pos - drag_start_pos;
                    let new_w =
                        ((initial_rect.width() + delta.x).max(10.0) / effective_zoom) as usize;
                    let new_h =
                        ((initial_rect.height() + delta.y).max(10.0) / effective_zoom) as usize;
                    selection.size = [new_w.max(1), new_h.max(1)];
                }
                DragState::Idle => {}
            }
        }
    } else if response.drag_stopped() {
        if let EditState::FloatingSelection {
            ref mut selection, ..
        } = doc.edit_state
        {
            selection.drag_state = DragState::Idle;
        }
    } else if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
        && !sel_screen_rect.contains(pos)
    {
        doc.commit_floating();
    }
}

fn resize_handle_rects(sel_rect: egui::Rect, handle_size: f32) -> Vec<(ResizeHandle, egui::Rect)> {
    let hs = handle_size;
    let mid_x = sel_rect.center().x;
    let mid_y = sel_rect.center().y;
    let l = sel_rect.left();
    let r = sel_rect.right();
    let t = sel_rect.top();
    let b = sel_rect.bottom();
    let sq = |x: f32, y: f32| egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(hs, hs));
    vec![
        (ResizeHandle::TopLeft, sq(l, t)),
        (ResizeHandle::Top, sq(mid_x, t)),
        (ResizeHandle::TopRight, sq(r, t)),
        (ResizeHandle::Right, sq(r, mid_y)),
        (ResizeHandle::BottomRight, sq(r, b)),
        (ResizeHandle::Bottom, sq(mid_x, b)),
        (ResizeHandle::BottomLeft, sq(l, b)),
        (ResizeHandle::Left, sq(l, mid_y)),
    ]
}

/// 확대 비율 — 시안 ZoomGroup 의 mono caption. 최소 폭 칸 가운데에 두어 자릿수가 바뀌어도 버튼이 흔들리지 않는다.
fn zoom_label(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::monospace(theme.image_zoom_font_size().value()),
        theme.text_muted().to_egui(),
    );
    let w = galley.size().x.max(theme.image_zoom_min_width().value());
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(w, ControlSize::Sm.height(theme)),
        egui::Sense::hover(),
    );
    let at = rect.center() - galley.size() * 0.5;
    ui.painter()
        .galley(at, galley, theme.text_muted().to_egui());
}

/// New Image · Save As 가 공유하는 카드 — 시안 `image-popup-*`. 제목 · 본문 줄 · 오른쪽 정렬 Cancel + 확인.
/// 확인 버튼을 눌렀으면 참을 돌려준다. Cancel 은 `cancel` 로 처리한다.
fn popup_card(
    ui: &mut egui::Ui,
    theme: &Theme,
    title: &str,
    confirm: &str,
    cancel: &str,
    body: impl FnOnce(&mut egui::Ui),
) -> (bool, bool) {
    let width = theme.image_popup_width().value();
    let pad_x = theme.image_popup_pad_x().value() as i8;
    let pad_top = theme.image_popup_pad_top().value() as i8;
    let gap = theme.image_popup_gap().value();
    let btn_gap = theme.image_popup_btn_gap().value();
    let (mut confirmed, mut cancelled) = (false, false);
    ui.add_space(theme.spacing_lg.value());
    ui.vertical_centered(|ui| {
        egui::Frame::new()
            .fill(theme.bg_panel().to_egui())
            .stroke(egui::Stroke::new(
                theme.border_width.value(),
                theme.border_strong().to_egui(),
            ))
            .corner_radius(theme.corner_radius.value())
            .shadow(theme.shadow_modal().to_egui())
            .show(ui, |ui| {
                ui.set_width(width);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: pad_x,
                            right: pad_x,
                            top: pad_top,
                            bottom: gap as i8,
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            // 시안 제목은 14 / semibold 다. egui 에 semibold 글꼴이 없어 굵기는 재현하지 않는다.
                            ui.label(
                                egui::RichText::new(title)
                                    .size(theme.image_popup_title_font_size().value())
                                    .color(theme.text_primary().to_egui()),
                            );
                            ui.add_space(gap);
                            body(ui);
                        });
                    egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: pad_x,
                            right: pad_x,
                            top: 0,
                            bottom: pad_top,
                        })
                        .show(ui, |ui| {
                            let row =
                                egui::vec2(ui.available_width(), ControlSize::Sm.height(theme));
                            ui.allocate_ui_with_layout(
                                row,
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = btn_gap;
                                    confirmed = Button::new(confirm)
                                        .variant(ButtonVariant::Primary)
                                        .size(ControlSize::Sm)
                                        .show(ui, theme)
                                        .clicked();
                                    cancelled = Button::new(cancel)
                                        .variant(ButtonVariant::Ghost)
                                        .size(ControlSize::Sm)
                                        .show(ui, theme)
                                        .clicked();
                                },
                            );
                        });
                });
            });
    });
    (confirmed, cancelled)
}

fn draw_new_image_popup(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, doc: &mut ImageDoc) {
    let (ok, cancel) = popup_card(
        ui,
        theme,
        tr.t("image_viewer.new_image_title"),
        tr.t("button.ok"),
        tr.t("button.cancel"),
        |ui| {
            ui.horizontal(|ui| {
                // 라벨을 입력칸 높이의 가운데에 맞추려면 줄 높이를 먼저 정한다.
                ui.set_min_height(theme.input_height().value());
                ui.spacing_mut().item_spacing.x = theme.image_popup_btn_gap().value();
                let input_w = theme.image_size_input_width().value();
                ui.label(body(theme, tr.t("image_viewer.width")));
                Input::new()
                    .width(input_w)
                    .show(ui, theme, &mut doc.new_image_width);
                ui.label(caption(theme, "×"));
                ui.label(body(theme, tr.t("image_viewer.height")));
                Input::new()
                    .width(input_w)
                    .show(ui, theme, &mut doc.new_image_height);
            });
        },
    );
    if cancel {
        doc.new_image_popup = false;
    }
    if ok {
        let w = doc
            .new_image_width
            .parse::<usize>()
            .unwrap_or(800)
            .clamp(1, 8192);
        let h = doc
            .new_image_height
            .parse::<usize>()
            .unwrap_or(600)
            .clamp(1, 8192);
        doc.create_blank_canvas(w, h);
    }
}

fn draw_save_path_popup(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, doc: &mut ImageDoc) {
    let (save, cancel) = popup_card(
        ui,
        theme,
        tr.t("image_viewer.save_path_title"),
        tr.t("button.save"),
        tr.t("button.cancel"),
        |ui| {
            // 같은 이름의 .png 가 있어 열렸으면 입력칸 위에 caption accent-warning 으로 알린다.
            if let Some(taken) = doc.save_path_clash.as_deref() {
                ui.label(
                    egui::RichText::new(tr.t_fmt("image.save_as.exists", taken))
                        .size(theme.font_size_caption.value())
                        .color(theme.accent_warning_raised().to_egui()),
                );
                ui.add_space(theme.spacing_xs.value());
            }
            // 찾아보기 IconButton을 오른쪽 끝에 먼저 놓고 남은 폭을 경로 입력칸이 채운다.
            let row = egui::vec2(ui.available_width(), ControlSize::Sm.height(theme));
            ui.allocate_ui_with_layout(
                row,
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = theme.image_path_row_gap().value();
                    if icon_button(
                        ui,
                        theme,
                        baked_icons::FOLDER_OPEN,
                        tr.t("image_viewer.browse"),
                        true,
                    )
                    .clicked()
                    {
                        doc.browse.queue();
                    }
                    let resp = Input::new()
                        .placeholder(tr.t("image_viewer.save_path_placeholder"))
                        .show(ui, theme, &mut doc.save_path_buffer);
                    if !resp.has_focus() && doc.save_path_buffer.is_empty() {
                        resp.request_focus();
                    }
                    // 제안한 다음 빈 이름은 stem 만 선택해 바로 고쳐 쓸 수 있게 한다.
                    if doc.save_path_select_stem {
                        resp.request_focus();
                        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                            let (start, end) = crate::doc::stem_char_range(&doc.save_path_buffer);
                            state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(start),
                                    egui::text::CCursor::new(end),
                                )));
                            state.store(ui.ctx(), resp.id);
                            doc.save_path_select_stem = false;
                        }
                    }
                },
            );
        },
    );
    if cancel {
        doc.close_save_as();
    }
    if save && !doc.save_path_buffer.is_empty() {
        let mut path = doc.save_path_buffer.clone();
        if !path.ends_with(".png") {
            path.push_str(".png");
        }
        if let Err(e) = doc.save_png(&path) {
            tracing::warn!("failed to save image: {e}");
        } else {
            doc.adopt_saved_path(path);
            doc.close_save_as();
            doc.exit_edit_mode();
            doc.reload_from_disk();
        }
    }
}

/// 이미지가 없을 때의 캔버스 상태 화면. 파일 없음 · 권한 없음 · 디코드 실패는 Retry 를 두고 상한 초과는 두지 않으며
/// 원인을 읽지 못한 빈 캔버스는 한 줄만 보인다. Retry 를 눌렀으면 true 다.
fn canvas_state(
    ui: &mut egui::Ui,
    theme: &Theme,
    tr: &Translator,
    rect: egui::Rect,
    doc: &ImageDoc,
) -> bool {
    let danger = theme.image_error_fg().to_egui();
    let warning = theme.accent_warning().to_egui();
    let muted = theme.text_muted().to_egui();
    let secondary = theme.text_secondary().to_egui();
    let too_large_sub = tr.t_args(
        "image.state.too_large_sub",
        &[
            &MAX_IMAGE_SIDE.to_string(),
            &format!("{}\u{a0}MiB", MAX_DECODE_ALLOC >> 20),
        ],
    );
    let too_large_size = match &doc.load_failure {
        Some(LoadFailure::TooLarge(Some([w, h]))) => Some(tr.t_args(
            "image.state.too_large_size",
            &[&w.to_string(), &h.to_string()],
        )),
        _ => None,
    };
    let (icon, glyph_color, title, title_color, sub, reason): (
        &'static [&'static [[f32; 2]]],
        _,
        _,
        _,
        _,
        Option<&str>,
    ) = match &doc.load_failure {
        None => (
            baked_icons::IMAGE,
            muted,
            tr.t("image_viewer.no_image"),
            secondary,
            None,
            None,
        ),
        Some(LoadFailure::Missing) => (
            baked_icons::ALERT_TRIANGLE,
            danger,
            tr.t("image.state.missing"),
            danger,
            Some(tr.t("image.state.missing_sub")),
            doc.file_path.as_deref(),
        ),
        Some(LoadFailure::Permission) => (
            baked_icons::LOCK,
            warning,
            tr.t("image.state.permission"),
            warning,
            Some(tr.t("image.state.permission_sub")),
            None,
        ),
        Some(LoadFailure::Decode(msg)) => (
            baked_icons::ALERT_TRIANGLE,
            danger,
            tr.t("image.state.decode"),
            danger,
            Some(tr.t("image.state.decode_sub")),
            Some(msg.as_str()),
        ),
        // 파일은 정상이고 상한으로 거절했으므로 디코드 실패 톤을 쓰지 않는다.
        Some(LoadFailure::TooLarge(_)) => (
            baked_icons::IMAGE,
            theme.image_too_large_fg().to_egui(),
            tr.t("image.state.too_large"),
            theme.text_primary().to_egui(),
            Some(too_large_sub.as_str()),
            too_large_size.as_deref(),
        ),
    };
    let retry = [(tr.t("image.state.retry"), ButtonVariant::Secondary)];
    // 너무 큼은 같은 파일을 다시 읽어도 결과가 같아 Retry 를 두지 않는다. 파일이 바뀌면 감시가 다시 읽는다.
    let retryable = doc
        .load_failure
        .as_ref()
        .is_some_and(|f| !matches!(f, LoadFailure::TooLarge(_)));
    let actions: &[(&str, ButtonVariant)] = if retryable { &retry } else { &[] };
    let paint = |ui: &mut egui::Ui, r: egui::Rect, c: egui::Color32| {
        tasty_plugin_sdk::baked_icon::draw(ui.painter(), icon, r.center(), r.height(), c);
    };
    let view = StateScreenView {
        glyph: StateGlyph::Paint(&paint),
        glyph_color,
        title,
        title_color,
        sub,
        reason,
        actions,
    };
    state_screen(ui, theme, rect, &view).is_some()
}

/// A caption-sized muted label (filename / zoom % / field labels).
fn caption(theme: &Theme, text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .size(theme.font_size_caption.value())
        .color(theme.text_muted().to_egui())
}

/// New Image 의 Width · Height 라벨 — 시안 13 · text-secondary.
fn body(theme: &Theme, text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .size(theme.font_size_body.value())
        .color(theme.text_secondary().to_egui())
}

/// 시안 ImgBtn: 도구 모음·zoom 그룹·찾아보기의 sm IconButton.
/// 위젯이 정한 글리프 칸과 상태별 색으로 폴리라인 아이콘을 그린다.
fn icon_button(
    ui: &mut egui::Ui,
    theme: &Theme,
    icon: &'static [&'static [[f32; 2]]],
    tooltip: &str,
    enabled: bool,
) -> egui::Response {
    IconButton::new()
        .size(ControlSize::Sm)
        .enabled(enabled)
        .show(ui, theme, &|ui, rect, color| {
            tasty_plugin_sdk::baked_icon::draw(
                ui.painter(),
                icon,
                rect.center(),
                rect.height(),
                color,
            );
        })
        .on_hover_text(tooltip)
}
