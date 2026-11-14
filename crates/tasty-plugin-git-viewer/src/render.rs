//! Git 팝업의 내용 영역을 그린다.
//! 워크트리·파일 선택이 같은 프레임에 반영되도록 선택 버튼 뒤에 상세 내용을 그린다.

mod diff;

use egui::{Align, Align2, Color32, FontId, Layout, Rect, Sense, Stroke, UiBuilder, vec2};
use tasty_plugin_sdk::Translator;
use tasty_type_appearance::theme::Theme;
use tasty_ui_widgets::{
    Button, ButtonVariant, ControlSize, TagVariant, tag, tag_mono, tag_mono_width,
};
use tasty_ui_widgets::{RowParent, RowState};

use diff::{diff_toolbar, draw_diff};

use crate::ViewerState;
use tasty_git_core::{FileStatus, LogEntry, StatusEntry, WorktreeEntry};

// 공용 토큰에 대응하지 않는 화면 전용 치수.
/// 워크트리 목록 너비.
const RAIL_W: f32 = 232.0;
/// 섹션 헤더 strip 높이(jsx `gvHeadStrip height: 28`).
const SECTION_H: f32 = 28.0;
/// context strip 높이(jsx `height: 30`).
const CTX_H: f32 = 30.0;
/// Changes 행 높이(jsx `ChRow height: 26`).
const CH_ROW_H: f32 = 26.0;
/// Commits 행 높이(jsx `CmRow height: 28`).
const CM_ROW_H: f32 = 28.0;
/// 상태 pill 고정 폭(jsx `GBadge width: 18`).
const STATUS_BADGE_W: f32 = 18.0;

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
fn prop(size: f32) -> FontId {
    FontId::proportional(size)
}

/// 팝업의 헤더·저장소 정보·본문을 그린다.
pub(crate) fn draw(ctx: &egui::Context, theme: &Theme, state: &mut ViewerState, tr: &Translator) {
    let frame = egui::Frame::new()
        .fill(theme.bg_panel().to_egui())
        .inner_margin(egui::Margin::ZERO);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        header(ui, theme, state, tr);
        context_strip(ui, theme, state, tr);
        if let Some(err) = state.error.clone() {
            error_line(ui, theme, &err);
        }
        if state.repo_path.is_none() {
            if state.loading {
                loading(ui, theme, tr);
            } else {
                nonrepo(ui, theme, tr);
            }
            return;
        }
        body(ui, theme, state, tr);
    });
}

/// 추가 인스턴스에 이미 열려 있다는 안내를 표시한다.
pub(crate) fn draw_busy(ctx: &egui::Context, theme: &Theme, tr: &Translator) {
    let frame = egui::Frame::new().fill(theme.bg_panel().to_egui());
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        let h = ui.available_height().max(1.0);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), h),
            Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(tr.t("git_viewer.already_open"))
                            .size(theme.font_size_body.value())
                            .strong()
                            .color(theme.text_secondary().to_egui()),
                    );
                    ui.add_space(theme.spacing_xs.value());
                    ui.label(
                        egui::RichText::new(tr.t("git_viewer.already_open_hint"))
                            .size(theme.font_size_caption.value())
                            .color(theme.text_muted().to_egui()),
                    );
                });
            },
        );
    });
}

fn header(ui: &mut egui::Ui, theme: &Theme, state: &mut ViewerState, tr: &Translator) {
    let full_w = ui.available_width();
    let pad_x = theme.spacing_md.value();
    let pad_y = theme.spacing_md.value();
    let btn_h = ControlSize::Sm.height(theme);
    let h = pad_y * 2.0 + btn_h;
    let (rect, _) = ui.allocate_exact_size(vec2(full_w, h), Sense::hover());

    ui.painter().text(
        egui::pos2(rect.left() + pad_x, rect.center().y),
        Align2::LEFT_CENTER,
        tr.t("git_viewer.heading"),
        prop(theme.font_size_max.value()),
        theme.text_primary().to_egui(),
    );

    let ctrl_rect = Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + pad_y),
        egui::pos2(rect.right() - pad_x, rect.top() + pad_y + btn_h),
    );
    let mut cui = ui.new_child(
        UiBuilder::new()
            .max_rect(ctrl_rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if Button::new(tr.t("git_viewer.refresh"))
        .variant(ButtonVariant::Secondary)
        .size(ControlSize::Sm)
        .show(&mut cui, theme)
        .clicked()
    {
        state.refresh();
    }
    bottom_separator(ui, theme, rect);
}

fn context_strip(ui: &mut egui::Ui, theme: &Theme, state: &ViewerState, tr: &Translator) {
    let full_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(full_w, CTX_H), Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, theme.bg_sidebar().to_egui());
    bottom_separator(ui, theme, rect);

    let active = state.worktrees.get(state.active_worktree);
    let name = active
        .map(|w| w.name.clone())
        .unwrap_or_else(|| "—".to_string());
    let branch = active
        .and_then(|w| w.branch.clone())
        .unwrap_or_else(|| tr.t("git_viewer.detached").to_string());
    let oid = active.and_then(|w| w.oid.clone());

    let pad_x = theme.spacing_md.value();
    let content = rect.shrink2(vec2(pad_x, 0.0));
    let mut cui = ui.new_child(
        UiBuilder::new()
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Center)),
    );
    cui.spacing_mut().item_spacing.x = theme.spacing_sm.value();
    cui.label(
        egui::RichText::new(name)
            .font(mono(theme.font_size_term_sm.value()))
            .color(theme.text_primary().to_egui()),
    );
    cui.label(
        egui::RichText::new("·")
            .size(theme.font_size_term_sm.value())
            .color(theme.text_disabled().to_egui()),
    );
    cui.label(
        egui::RichText::new(branch)
            .font(mono(theme.font_size_term_sm.value()))
            .color(theme.text_secondary().to_egui()),
    );
    if let Some(oid) = oid {
        tag_mono(&mut cui, theme, &oid, TagVariant::Info, false);
    }

    if let Some(path) = state
        .repo_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
    {
        let clip = Rect::from_min_max(
            egui::pos2(content.left() + content.width() * 0.4, rect.top()),
            egui::pos2(content.right(), rect.bottom()),
        );
        ui.painter().with_clip_rect(clip).text(
            egui::pos2(content.right(), rect.center().y),
            Align2::RIGHT_CENTER,
            path,
            mono(theme.font_size_caption.value()),
            theme.text_muted().to_egui(),
        );
    }
}

fn error_line(ui: &mut egui::Ui, theme: &Theme, err: &str) {
    let full_w = ui.available_width();
    let pad_x = theme.spacing_md.value();
    let h = theme.spacing_sm.value() * 2.0 + theme.font_size_caption.value();
    let (rect, _) = ui.allocate_exact_size(vec2(full_w, h), Sense::hover());
    let danger = theme.accent_danger().to_egui();
    // 오류 요약 줄 배경. 대응 토큰 없음 — 값에 이름만 둔다.
    const ERROR_ROW_BG_OPACITY: f32 = 0.1;
    ui.painter()
        .rect_filled(rect, 0.0, danger.gamma_multiply(ERROR_ROW_BG_OPACITY));
    bottom_separator(ui, theme, rect);
    ui.painter()
        .with_clip_rect(rect.shrink2(vec2(pad_x, 0.0)))
        .text(
            egui::pos2(rect.left() + pad_x, rect.center().y),
            Align2::LEFT_CENTER,
            err,
            prop(theme.font_size_caption.value()),
            danger,
        );
}

/// 원격 스냅샷 응답을 기다리는 동안 표시한다.
fn loading(ui: &mut egui::Ui, theme: &Theme, tr: &Translator) {
    let h = ui.available_height().max(1.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), h),
        Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.label(
                egui::RichText::new(tr.t("git_viewer.loading"))
                    .size(theme.font_size_body.value())
                    .color(theme.text_muted().to_egui()),
            );
        },
    );
}

fn nonrepo(ui: &mut egui::Ui, theme: &Theme, tr: &Translator) {
    let h = ui.available_height().max(1.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), h),
        Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(tr.t("git_viewer.no_repo_title"))
                        .size(theme.font_size_body.value())
                        .strong()
                        .color(theme.text_secondary().to_egui()),
                );
                ui.add_space(theme.spacing_sm.value());
                ui.label(
                    egui::RichText::new(tr.t("git_viewer.no_repo"))
                        .size(theme.font_size_caption.value())
                        .color(theme.text_muted().to_egui()),
                );
            });
        },
    );
}

fn body(ui: &mut egui::Ui, theme: &Theme, state: &mut ViewerState, tr: &Translator) {
    let avail = ui.available_rect_before_wrap();
    let rail_rect = Rect::from_min_size(avail.min, vec2(RAIL_W, avail.height()));
    let right_rect = Rect::from_min_max(egui::pos2(avail.min.x + RAIL_W, avail.min.y), avail.max);

    ui.painter().vline(
        avail.min.x + RAIL_W,
        rail_rect.y_range(),
        Stroke::new(
            theme.border_width.value(),
            theme.separator.to_egui_premultiplied(),
        ),
    );

    draw_rail(ui, theme, state, tr, rail_rect);
    draw_right(ui, theme, state, tr, right_rect);
    ui.allocate_rect(avail, Sense::hover());
}

fn draw_rail(
    ui: &mut egui::Ui,
    theme: &Theme,
    state: &mut ViewerState,
    tr: &Translator,
    area: Rect,
) {
    let mut pane = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::top_down(Align::Min)),
    );
    pane.spacing_mut().item_spacing = vec2(0.0, 0.0);
    pane_head(
        &mut pane,
        theme,
        &format!(
            "{} ({})",
            tr.t("git_viewer.worktrees_heading"),
            state.worktrees.len()
        ),
    );
    if state.worktrees.is_empty() {
        empty_line(&mut pane, theme, tr.t("git_viewer.no_worktrees"));
        return;
    }
    let mut clicked: Option<usize> = None;
    // 보이는 행만 그리되 인덱스는 전체 목록 기준으로 전달한다.
    egui::ScrollArea::vertical()
        .id_salt("gv_rail")
        .auto_shrink([false, false])
        .drag_to_scroll(false)
        .show_rows(
            &mut pane,
            wt_row_h(theme),
            state.worktrees.len(),
            |ui, row_range| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for idx in row_range {
                    let Some(wt) = state.worktrees.get(idx) else {
                        continue;
                    };
                    let selected = idx == state.active_worktree;
                    if ui
                        .push_id(idx, |ui| wt_row(ui, theme, tr, wt, selected))
                        .inner
                    {
                        clicked = Some(idx);
                    }
                }
            },
        );
    // 오른쪽 내용을 그리기 전에 선택한 워크트리를 반영한다.
    if let Some(idx) = clicked {
        state.select_worktree(idx);
    }
}

/// 모든 워크트리 행에 같은 높이를 사용해야 show_rows가 올바른 행을 고른다.
fn wt_row_h(theme: &Theme) -> f32 {
    let pad_y = theme.spacing_sm.value();
    let line_gap = theme.spacing_xs.value();
    let l1_h = theme.font_size_term_sm.value() + 4.0;
    let l2_h = theme.font_size_caption.value() + 4.0;
    pad_y * 2.0 + l1_h + line_gap + l2_h
}

/// 첫 줄은 이름·타입, 둘째 줄은 짧은 커밋 ID·상태를 표시한다.
fn wt_row(
    ui: &mut egui::Ui,
    theme: &Theme,
    tr: &Translator,
    wt: &WorktreeEntry,
    selected: bool,
) -> bool {
    let full_w = ui.available_width();
    let pad_x = theme.spacing_md.value();
    let pad_y = theme.spacing_sm.value();
    let line_gap = theme.spacing_xs.value();
    let l1_h = theme.font_size_term_sm.value() + 4.0;
    let l2_h = theme.font_size_caption.value() + 4.0;
    let h = wt_row_h(theme);
    let (rect, resp) = ui.allocate_exact_size(vec2(full_w, h), Sense::click());

    let state = RowState::from_response(selected, &resp);
    state.paint_fill(ui.painter(), rect, 0.0, theme);

    let name_color = if !wt.is_valid {
        state.meta(theme, RowParent::Mantle)
    } else {
        state.label(theme, RowParent::Mantle, theme.text_secondary())
    };

    let l1 = Rect::from_min_size(
        egui::pos2(rect.left() + pad_x, rect.top() + pad_y),
        vec2(full_w - pad_x * 2.0, l1_h),
    );
    let (type_key, type_variant) = if wt.is_main {
        ("git_viewer.wt_main", TagVariant::Info)
    } else {
        ("git_viewer.wt_linked", TagVariant::Default)
    };
    let mut t1 = ui.new_child(
        UiBuilder::new()
            .max_rect(l1)
            .layout(Layout::right_to_left(Align::Center)),
    );
    let type_resp = tag(&mut t1, theme, tr.t(type_key), type_variant, false);
    let name_right = type_resp.rect.left() - theme.spacing_sm.value();
    let name_clip = Rect::from_min_max(l1.min, egui::pos2(name_right, l1.max.y));
    ui.painter().with_clip_rect(name_clip).text(
        egui::pos2(l1.left(), l1.center().y),
        Align2::LEFT_CENTER,
        &wt.name,
        mono(theme.font_size_term_sm.value()),
        name_color.to_egui(),
    );

    let l2 = Rect::from_min_size(
        egui::pos2(rect.left() + pad_x, l1.max.y + line_gap),
        vec2(full_w - pad_x * 2.0, l2_h),
    );
    if let Some((label_key, variant)) = wt_state_pill(wt) {
        let mut t2 = ui.new_child(
            UiBuilder::new()
                .max_rect(l2)
                .layout(Layout::right_to_left(Align::Center)),
        );
        let resp = tag(&mut t2, theme, tr.t(label_key), variant, true);
        // 잠금·무효 상태의 이유를 툴팁으로 보여 준다.
        if let Some(reason) = &wt.lock_reason
            && !reason.is_empty()
        {
            resp.on_hover_text(reason);
        }
    }
    if let Some(oid) = &wt.oid {
        ui.painter().text(
            egui::pos2(l2.left(), l2.center().y),
            Align2::LEFT_CENTER,
            oid,
            mono(theme.font_size_caption.value()),
            if selected {
                state.meta(theme, RowParent::Mantle)
            } else {
                theme.accent_info_raised()
            }
            .to_egui(),
        );
    }

    if selected {
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.min,
                vec2(theme.listctrl_selected_bar_width().value(), rect.height()),
            ),
            0.0,
            theme.accent_primary_on_active().to_egui(),
        );
    }
    bottom_separator(ui, theme, rect);
    resp.clicked() && wt.is_valid
}

/// 워크트리 상태 태그. 무효·잠김·현재 위치 순으로 우선한다.
fn wt_state_pill(wt: &WorktreeEntry) -> Option<(&'static str, TagVariant)> {
    if !wt.is_valid {
        Some(("git_viewer.wt_invalid", TagVariant::Danger))
    } else if wt.locked {
        Some(("git_viewer.wt_locked", TagVariant::Warning))
    } else if wt.is_current {
        Some(("git_viewer.wt_current", TagVariant::Success))
    } else {
        None
    }
}

fn draw_right(
    ui: &mut egui::Ui,
    theme: &Theme,
    state: &mut ViewerState,
    tr: &Translator,
    area: Rect,
) {
    let half = (area.height() * 0.5).round();
    let top = Rect::from_min_size(area.min, vec2(area.width(), half));
    let bottom = Rect::from_min_max(egui::pos2(area.left(), area.top() + half), area.max);

    ui.painter().hline(
        area.x_range(),
        area.top() + half,
        Stroke::new(
            theme.border_width.value(),
            theme.separator.to_egui_premultiplied(),
        ),
    );

    draw_changes(ui, theme, state, tr, top);
    draw_bottom(ui, theme, state, tr, bottom);
}

fn draw_changes(
    ui: &mut egui::Ui,
    theme: &Theme,
    state: &mut ViewerState,
    tr: &Translator,
    area: Rect,
) {
    let mut pane = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::top_down(Align::Min)),
    );
    pane.spacing_mut().item_spacing = vec2(0.0, 0.0);
    pane_head(
        &mut pane,
        theme,
        &format!(
            "{} ({})",
            tr.t("git_viewer.status_heading"),
            state.status_entries.len()
        ),
    );
    if state.status_entries.is_empty() {
        empty_line(&mut pane, theme, tr.t("git_viewer.no_changes"));
        return;
    }
    let mut clicked: Option<usize> = None;
    // 화면에 보이는 행의 전체 목록 인덱스로 diff를 연다.
    egui::ScrollArea::vertical()
        .id_salt("gv_changes")
        .auto_shrink([false, false])
        .drag_to_scroll(false)
        .show_rows(
            &mut pane,
            CH_ROW_H,
            state.status_entries.len(),
            |ui, row_range| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for idx in row_range {
                    let Some(entry) = state.status_entries.get(idx) else {
                        continue;
                    };
                    let selected = state.selected_file == Some(idx);
                    if ui
                        .push_id(idx, |ui| ch_row(ui, theme, entry, selected))
                        .inner
                    {
                        clicked = Some(idx);
                    }
                }
            },
        );
    if let Some(idx) = clicked {
        state.load_diff(idx);
    }
}

/// 상태 태그와 파일 경로를 그린다.
fn ch_row(ui: &mut egui::Ui, theme: &Theme, entry: &StatusEntry, selected: bool) -> bool {
    let full_w = ui.available_width();
    let pad_x = theme.spacing_md.value();
    let (rect, resp) = ui.allocate_exact_size(vec2(full_w, CH_ROW_H), Sense::click());
    let state = RowState::from_response(selected, &resp);
    state.paint_fill(ui.painter(), rect, 0.0, theme);

    let (glyph, variant) = status_pill(entry.status);
    let content = Rect::from_min_size(
        egui::pos2(rect.left() + pad_x, rect.top()),
        vec2(full_w - pad_x * 2.0, CH_ROW_H),
    );
    let mut cui = ui.new_child(
        UiBuilder::new()
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Center)),
    );
    cui.spacing_mut().item_spacing.x = theme.spacing_sm.value();
    cui.allocate_ui_with_layout(
        vec2(STATUS_BADGE_W, CH_ROW_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            tag_mono(ui, theme, glyph, variant, false);
        },
    );
    let (dir, file) = split_path(&entry.path);
    let path_clip = Rect::from_min_max(
        egui::pos2(cui.cursor().left(), rect.top()),
        egui::pos2(content.right(), rect.bottom()),
    );
    let p = ui.painter().with_clip_rect(path_clip);
    let cy = rect.center().y;
    let mut x = cui.cursor().left();
    let (dir_fg, file_fg) = if selected {
        let ink = state.meta(theme, RowParent::Mantle).to_egui();
        (ink, ink)
    } else {
        (
            state.meta(theme, RowParent::Mantle).to_egui(),
            theme.text_primary().to_egui(),
        )
    };
    if !dir.is_empty() {
        let g = p.layout_no_wrap(dir, mono(theme.font_size_term_sm.value()), dir_fg);
        p.galley(egui::pos2(x, cy - g.rect.height() * 0.5), g.clone(), dir_fg);
        x += g.rect.width();
    }
    let gf = p.layout_no_wrap(file, mono(theme.font_size_term_sm.value()), file_fg);
    p.galley(egui::pos2(x, cy - gf.rect.height() * 0.5), gf, file_fg);

    if selected {
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.min,
                vec2(theme.listctrl_selected_bar_width().value(), rect.height()),
            ),
            0.0,
            theme.accent_primary_on_active().to_egui(),
        );
    }
    resp.clicked()
}

fn draw_bottom(
    ui: &mut egui::Ui,
    theme: &Theme,
    state: &mut ViewerState,
    tr: &Translator,
    area: Rect,
) {
    // 뒤로 가기를 먼저 처리해 같은 프레임에 커밋 목록으로 돌아간다.
    let showing_diff = state.selected_file.is_some() && state.diff_content.is_some();
    let toolbar_h = if showing_diff {
        theme.git_toolbar_height().value()
    } else {
        0.0
    };
    if showing_diff {
        let toolbar = Rect::from_min_size(area.min, vec2(area.width(), toolbar_h));
        if diff_toolbar(ui, theme, tr, toolbar, state.diff_content.as_ref()) {
            state.close_diff();
        }
    }
    let content = Rect::from_min_max(egui::pos2(area.left(), area.top() + toolbar_h), area.max);
    if state.selected_file.is_some()
        && let Some(diff) = state.diff_content.as_ref()
    {
        draw_diff(ui, theme, tr, diff, &mut state.diff_width, content);
    } else {
        draw_commits(ui, theme, tr, &state.log_entries, content);
    }
}

fn draw_commits(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, log: &[LogEntry], area: Rect) {
    let mut pane = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::top_down(Align::Min)),
    );
    pane.spacing_mut().item_spacing = vec2(0.0, 0.0);
    pane_head(
        &mut pane,
        theme,
        &format!("{} ({})", tr.t("git_viewer.log_heading"), log.len()),
    );
    if log.is_empty() {
        empty_line(&mut pane, theme, tr.t("git_viewer.no_commits"));
        return;
    }
    // 보이는 커밋 행만 그린다.
    egui::ScrollArea::vertical()
        .id_salt("gv_commits")
        .auto_shrink([false, false])
        .drag_to_scroll(false)
        .show_rows(&mut pane, CM_ROW_H, log.len(), |ui, row_range| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            for idx in row_range {
                let Some(entry) = log.get(idx) else {
                    continue;
                };
                ui.push_id(idx, |ui| cm_row(ui, theme, tr, entry));
            }
        });
}

/// 비어 있는 커밋 제목은 플러그인의 언어로 안내한다. Git 데이터 자체는 번역하지 않는다.
fn summary_text<'a>(tr: &'a Translator, entry: &'a LogEntry) -> &'a str {
    if entry.summary.is_empty() {
        tr.t("git_viewer.no_message")
    } else {
        &entry.summary
    }
}

/// 비어 있는 작성자 이름을 번역 문구로 대체한다.
fn author_text<'a>(tr: &'a Translator, entry: &'a LogEntry) -> &'a str {
    if entry.author.is_empty() {
        tr.t("git_viewer.unknown_author")
    } else {
        &entry.author
    }
}

/// 커밋 ID·참조 태그·제목·작성자·시각을 그린다.
fn cm_row(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, entry: &LogEntry) {
    let full_w = ui.available_width();
    let pad_x = theme.spacing_md.value();
    let (rect, resp) = ui.allocate_exact_size(vec2(full_w, CM_ROW_H), Sense::hover());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, theme.overlay_hover().to_egui_premultiplied());
    }
    let content = Rect::from_min_size(
        egui::pos2(rect.left() + pad_x, rect.top()),
        vec2(full_w - pad_x * 2.0, CM_ROW_H),
    );
    let gap = theme.spacing_sm.value();

    // 작성자 영역이 커밋 ID를 가리지 않도록 ID 너비를 먼저 구한다.
    let oid_w = ui
        .painter()
        .layout_no_wrap(
            entry.oid_short.clone(),
            mono(theme.font_size_caption.value()),
            Color32::PLACEHOLDER,
        )
        .rect
        .width();
    let right_clip_left = content.left() + oid_w + gap;

    // 긴 작성자 이름을 자르되 스크롤 영역 밖으로 그리지 않도록 부모 clip과 교차한다.
    let mut right = ui.new_child(
        UiBuilder::new()
            .max_rect(content)
            .layout(Layout::right_to_left(Align::Center)),
    );
    let right_clip = Rect::from_min_max(
        egui::pos2(right_clip_left, rect.top()),
        egui::pos2(content.right(), rect.bottom()),
    );
    right.shrink_clip_rect(right_clip);
    right.spacing_mut().item_spacing.x = gap;
    right.label(
        egui::RichText::new(&entry.time)
            .font(mono(theme.font_size_caption.value()))
            .color(theme.text_muted().to_egui()),
    );
    right.label(
        egui::RichText::new(author_text(tr, entry))
            .size(theme.font_size_term_sm.value())
            .color(theme.text_muted().to_egui()),
    );
    // 논리적 전체 너비 대신 실제로 보이는 작성자 영역을 태그 배치의 경계로 쓴다.
    let right_start = right.min_rect().left().max(right_clip_left);

    // 남은 너비만큼 참조 태그를 표시하고 나머지 개수는 +N으로 줄인다.
    let mut left = ui.new_child(
        UiBuilder::new()
            .max_rect(content)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.spacing_mut().item_spacing.x = gap;
    left.label(
        egui::RichText::new(&entry.oid_short)
            .font(mono(theme.font_size_caption.value()))
            .color(theme.accent_info_raised().to_egui()),
    );
    let pill_limit = (right_start - gap).max(left.min_rect().right());
    let mut cursor_x = left.min_rect().right();
    let mut shown = 0usize;
    for r in &entry.refs {
        let remaining_after = entry.refs.len() - shown - 1;
        // 이 pill을 그린 뒤에도 남는 게 있으면 "+N" pill 자리를 반드시 남겨둔다.
        let reserve = if remaining_after > 0 {
            gap + tag_mono_width(&left, theme, &format!("+{remaining_after}"))
        } else {
            0.0
        };
        let w = tag_mono_width(&left, theme, r);
        if cursor_x + gap + w + reserve > pill_limit {
            break;
        }
        tag_mono(&mut left, theme, r, TagVariant::Info, false);
        cursor_x += gap + w;
        shown += 1;
    }
    if shown < entry.refs.len() {
        let remaining = entry.refs.len() - shown;
        tag_mono(
            &mut left,
            theme,
            &format!("+{remaining}"),
            TagVariant::Info,
            false,
        );
    }
    let left_end = left.min_rect().right();

    // summary — 가운데 남는 폭에 clip(truncate).
    let sx = left_end + gap;
    let clip = Rect::from_min_max(
        egui::pos2(sx, rect.top()),
        egui::pos2((right_start - gap).max(sx), rect.bottom()),
    );
    ui.painter().with_clip_rect(clip).text(
        egui::pos2(sx, rect.center().y),
        Align2::LEFT_CENTER,
        summary_text(tr, entry),
        prop(theme.font_size_body.value()),
        theme.text_primary().to_egui(),
    );
}

/// 섹션 헤더와 아래 구분선을 그린다.
fn pane_head(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    let full_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(full_w, SECTION_H), Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, theme.bg_sidebar().to_egui());
    bottom_separator(ui, theme, rect);
    ui.painter().text(
        egui::pos2(rect.left() + theme.spacing_md.value(), rect.center().y),
        Align2::LEFT_CENTER,
        text.to_uppercase(),
        tasty_ui_widgets::label_font(theme.font_size_micro.value()),
        theme.text_muted().to_egui(),
    );
}

/// 비어 있는 목록의 안내를 중앙에 표시한다.
fn empty_line(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    let full_w = ui.available_width();
    let h = ui
        .available_height()
        .max(theme.item_height_interactive.value());
    let (rect, _) = ui.allocate_exact_size(vec2(full_w, h), Sense::hover());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        prop(theme.font_size_term_sm.value()),
        theme.text_muted().to_egui(),
    );
}

/// rect 하단에 1px separator.
fn bottom_separator(ui: &mut egui::Ui, theme: &Theme, rect: Rect) {
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - theme.border_width.value() * 0.5,
        Stroke::new(
            theme.border_width.value(),
            theme.separator.to_egui_premultiplied(),
        ),
    );
}

/// 경로를 (dir, file) 로 분리 — 마지막 `/` 기준. dir 은 trailing `/` 포함.
fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(i) => (path[..=i].to_string(), path[i + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// status → (표시 글리프, Tag variant). (jsx `ST` 매핑)
fn status_pill(s: FileStatus) -> (&'static str, TagVariant) {
    match s {
        FileStatus::Modified => ("M", TagVariant::Warning),
        FileStatus::Added => ("A", TagVariant::Success),
        FileStatus::Deleted => ("D", TagVariant::Danger),
        FileStatus::Renamed => ("R", TagVariant::Accent),
        FileStatus::Untracked => ("?", TagVariant::Default),
        FileStatus::Conflicted => ("U", TagVariant::Danger),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(summary: &str, author: &str) -> LogEntry {
        LogEntry {
            oid_short: "0123abc".to_string(),
            summary: summary.to_string(),
            author: author.to_string(),
            time: String::new(),
            refs: Vec::new(),
        }
    }

    fn translator(locale: &str) -> Translator {
        let lang_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, locale)
    }

    /// 빈 값은 번역 문구로 대체하고 값이 있으면 그대로 표시한다.
    #[test]
    fn empty_summary_and_author_fall_back_to_lang_keys() {
        let tr = translator("en");
        let e = entry("", "");
        assert_eq!(summary_text(&tr, &e), "(no message)");
        assert_eq!(author_text(&tr, &e), "(unknown)");
        let e = entry("feat: x", "alice");
        assert_eq!(summary_text(&tr, &e), "feat: x");
        assert_eq!(author_text(&tr, &e), "alice");
    }

    /// 받은 언어 설정에 따라 빈 값 안내를 번역해야 한다.
    #[test]
    fn fallback_follows_plugin_locale() {
        let e = entry("", "");
        for locale in tasty_i18n::BUILTIN_CODES
            .iter()
            .filter(|code| **code != "en")
        {
            let tr = translator(locale);
            let summary = summary_text(&tr, &e);
            let author = author_text(&tr, &e);
            assert_ne!(summary, "(no message)", "{locale}: summary 가 번역돼야 함");
            assert_ne!(author, "(unknown)", "{locale}: author 가 번역돼야 함");
            assert_ne!(summary, "git_viewer.no_message", "{locale}: 키 누락");
            assert_ne!(author, "git_viewer.unknown_author", "{locale}: 키 누락");
        }
    }
}
