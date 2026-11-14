//! 클립보드 팝업의 헤더·타입 선택·본문·푸터를 그린다.
//! 타입이 하나면 배지를, 여러 개면 가로 선택 버튼을 표시한다.
//! 내용 영역만 그리며 닫기 버튼을 누르면 호출자가 호스트에 닫기를 요청한다.

mod baked_icons {
    include!(concat!(env!("OUT_DIR"), "/plugin_icons.rs"));
}

use tasty_plugin_sdk::{Translator, baked_icon};
use tasty_type_appearance::theme::Theme;
use tasty_ui_widgets::{
    Button, ButtonVariant, ControlSize, IconButton, TagVariant, TypeSegment, checkbox,
    draw_type_segments, tag,
};

use crate::ViewerState;
use crate::clipboard::{ClipboardType, ContentRepr, OtherFormatEntry, format_bytes};
use crate::html_format::prettify;

/// 헤더·타입바·푸터의 좌우 여백. spacing_md 토큰을 사용한다.
fn row_pad_x(theme: &Theme) -> f32 {
    theme.spacing_md.value()
}

/// 버튼 높이에 대한 아이콘 크기 비율.
const ICON_DRAW_RATIO: f32 = 0.7;

// 빈 상태 아이콘 크기는 갤러리와 같은 공용 토큰을 사용한다.
use tasty_ui_widgets::tokens::CLIPBOARD_CENTER_ICON_SIZE as CENTER_ICON_SIZE;

/// 기타 포맷별 미리보기의 줄 수 상한. 포맷 목록 자체는 줄이지 않는다.
const OTHER_PREVIEW_MAX_LINES: usize = 20;

/// 주 팝업의 오류·빈 상태·내용을 그린다. 닫기 버튼을 누르면 true를 반환한다.
pub(crate) fn draw(
    ctx: &egui::Context,
    theme: &Theme,
    state: &mut ViewerState,
    tr: &Translator,
) -> bool {
    let mut close = false;
    panel(ctx, theme, |ui| {
        header(ui, theme, tr, &mut close);
        if state.read_error.is_some() {
            center_state(
                ui,
                theme,
                baked_icons::ALERT_TRIANGLE,
                tr.t("clipboard_viewer.popup.read_failed_title"),
                Some(tr.t("clipboard_viewer.popup.read_failed_sub")),
                true,
            );
        } else if state.available.is_empty() {
            center_state(
                ui,
                theme,
                baked_icons::CLIPBOARD,
                tr.t("clipboard_viewer.popup.empty_title"),
                Some(tr.t("clipboard_viewer.popup.empty_sub")),
                false,
            );
        } else {
            data_state(ui, theme, state, tr, &mut close);
        }
    });
    close
}

/// 추가 인스턴스에 이미 열려 있다는 안내를 그린다.
pub(crate) fn draw_already_open(ctx: &egui::Context, theme: &Theme, tr: &Translator) -> bool {
    let mut close = false;
    panel(ctx, theme, |ui| {
        header(ui, theme, tr, &mut close);
        center_state(
            ui,
            theme,
            baked_icons::LOCK,
            tr.t("clipboard_viewer.popup.already_open_title"),
            Some(tr.t("clipboard_viewer.popup.already_open_sub")),
            false,
        );
    });
    close
}

/// 호스트와 같은 배경색으로 채운다. 행 사이 여백은 각 행에서 직접 정한다.
fn panel(ctx: &egui::Context, theme: &Theme, add: impl FnOnce(&mut egui::Ui)) {
    let frame = egui::Frame::new()
        .fill(theme.bg_panel().to_egui())
        .inner_margin(egui::Margin::ZERO);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
        add(ui);
    });
}

fn header(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, close: &mut bool) {
    let full_w = ui.available_width();
    let pad_x = row_pad_x(theme);
    let pad_y = theme.spacing_md.value();
    let ctrl_h = ControlSize::Sm.height(theme);
    let h = pad_y * 2.0 + ctrl_h;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(full_w, h), egui::Sense::hover());

    let content = egui::Rect::from_min_max(
        egui::pos2(rect.left() + pad_x, rect.top()),
        egui::pos2(rect.right() - pad_x, rect.bottom()),
    );
    let mut lui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    lui.spacing_mut().item_spacing.x = theme.spacing_sm.value();
    icon_glyph(
        &mut lui,
        baked_icons::CLIPBOARD,
        theme.icon_glyph_size_md.value(),
        theme.text_muted().to_egui(),
    );
    lui.label(
        egui::RichText::new(tr.t("clipboard_viewer.popup.header"))
            .size(theme.font_size_max.value())
            .strong()
            .color(theme.text_primary().to_egui()),
    );
    tag(
        &mut lui,
        theme,
        tr.t("clipboard_viewer.popup.snapshot_badge"),
        TagVariant::Default,
        false,
    );

    let close_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + pad_y),
        egui::pos2(rect.right() - pad_x, rect.top() + pad_y + ctrl_h),
    );
    let mut rui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(close_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let resp = IconButton::new()
        .size(ControlSize::Sm)
        .show(&mut rui, theme, &|ui, r, c| {
            icon_in_button(ui, baked_icons::CLOSE, r, c);
        })
        .on_hover_text(tr.t("clipboard_viewer.popup.close_tooltip"));
    if resp.clicked() {
        *close = true;
    }

    bottom_separator(ui, theme, rect);
}

fn data_state(
    ui: &mut egui::Ui,
    theme: &Theme,
    state: &mut ViewerState,
    tr: &Translator,
    close: &mut bool,
) {
    let types: Vec<ClipboardType> = state.available.iter().map(|(t, _)| *t).collect();
    let active = state
        .selected
        .filter(|s| types.contains(s))
        .unwrap_or(types[0]);

    // 선택 변경 전 타입으로 우측 내용을 그린다. HTML은 정리 표시 체크박스를 쓴다.
    let active_meta = state
        .available
        .iter()
        .find(|(t, _)| *t == active)
        .and_then(|(_, c)| c.meta_text());
    // 기타 포맷이 있으면 개수를 툴팁으로 보여 준다.
    let other_count = state.available.iter().find_map(|(t, c)| match (t, c) {
        (ClipboardType::Other, ContentRepr::Other(entries)) => Some(entries.len()),
        _ => None,
    });
    let mut html_pretty = state.html_pretty;
    let picked = type_bar(ui, theme, tr, &types, active, other_count, |ui, theme| {
        if active == ClipboardType::Html {
            checkbox(
                ui,
                theme,
                &mut html_pretty,
                tr.t("clipboard_viewer.popup.pretty_print"),
                true,
                tasty_ui_widgets::Ground::Panel,
            );
        } else if let Some(meta) = &active_meta {
            meta_label(ui, theme, meta);
        }
    });
    state.html_pretty = html_pretty;
    if let Some(picked) = picked {
        state.selected = Some(picked);
    }
    let active = picked.unwrap_or(active);

    let cur = state
        .available
        .iter()
        .find(|(t, _)| *t == active)
        .map(|(t, c)| (*t, c.clone()));

    let footer_h = theme.spacing_sm.value() * 2.0 + ControlSize::Sm.height(theme);
    let body_h = (ui.available_height() - footer_h).max(0.0);
    let full_w = ui.available_width();
    let (body_rect, _) = ui.allocate_exact_size(egui::vec2(full_w, body_h), egui::Sense::hover());
    let mut bui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(body_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    if let Some((ty, content)) = &cur {
        type_body(&mut bui, theme, *ty, content, tr, state.html_pretty);
    }

    // HTML은 MIME에 문자·줄 수를 붙이고, 기타 포맷은 MIME 대신 개수를 표시한다.
    let footer_meta = cur.as_ref().and_then(|(ty, content)| {
        html_footer_meta(tr, *ty, content).or_else(|| other_footer_meta(tr, *ty, content))
    });
    footer(
        ui,
        theme,
        tr,
        cur.as_ref().map(|(t, _)| *t),
        footer_meta,
        close,
    );
}

/// 타입이 하나면 배지, 여러 개면 공용 타입 세그먼트를 그린다.
/// `SEG_COMPACT_AT` 이상이면 선택한 타입만 라벨을 표시한다.
/// 기타 포맷의 툴팁에는 개수를 넣는다.
fn type_switch(
    ui: &mut egui::Ui,
    theme: &Theme,
    tr: &Translator,
    types: &[ClipboardType],
    active: ClipboardType,
    other_count: Option<usize>,
) -> Option<ClipboardType> {
    if types.len() <= 1 {
        let ty = types.first().copied().unwrap_or(active);
        icon_glyph(
            ui,
            type_icon(ty),
            theme.icon_glyph_size_sm.value(),
            theme.text_muted().to_egui(),
        );
        let resp = tag(
            ui,
            theme,
            tr.t(ty.label_i18n_key()),
            TagVariant::Accent,
            false,
        );
        if ty == ClipboardType::Other
            && let Some(n) = other_count
        {
            resp.on_hover_text(other_unrecognized_text(tr, n));
        }
        return None;
    }

    let labels: Vec<&str> = types.iter().map(|ty| tr.t(ty.label_i18n_key())).collect();
    let tooltips: Vec<String> = types
        .iter()
        .zip(&labels)
        .map(|(ty, label)| match (ty, other_count) {
            (ClipboardType::Other, Some(n)) => other_unrecognized_text(tr, n),
            _ => label.to_string(),
        })
        .collect();
    let segments: Vec<TypeSegment<'_>> = labels
        .iter()
        .zip(&tooltips)
        .map(|(label, tooltip)| TypeSegment { label, tooltip })
        .collect();
    let active_idx = types
        .iter()
        .position(|ty| *ty == active)
        .unwrap_or(usize::MAX);
    draw_type_segments(
        ui,
        theme,
        &segments,
        active_idx,
        &|ui, i, center, size, fg| {
            baked_icon::draw(ui.painter(), type_icon(types[i]), center, size, fg);
        },
    )
    .map(|i| types[i])
}

/// 타입 선택과 우측 내용을 그린다. 우측에는 메타데이터나 HTML 체크박스를 넣는다.
fn type_bar(
    ui: &mut egui::Ui,
    theme: &Theme,
    tr: &Translator,
    types: &[ClipboardType],
    active: ClipboardType,
    other_count: Option<usize>,
    right_slot: impl FnOnce(&mut egui::Ui, &Theme),
) -> Option<ClipboardType> {
    let full_w = ui.available_width();
    let pad_x = row_pad_x(theme);
    let pad_y = theme.spacing_sm.value();
    let ctrl_h = ControlSize::Sm.height(theme);
    let h = pad_y * 2.0 + ctrl_h;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(full_w, h), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, theme.bg_sidebar().to_egui());

    let content = egui::Rect::from_min_max(
        egui::pos2(rect.left() + pad_x, rect.top()),
        egui::pos2(rect.right() - pad_x, rect.bottom()),
    );
    let mut lui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    lui.spacing_mut().item_spacing.x = theme.spacing_md.value();
    let picked = type_switch(&mut lui, theme, tr, types, active, other_count);

    let mut rui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    right_slot(&mut rui, theme);

    bottom_separator(ui, theme, rect);
    picked
}

/// 선택한 타입의 본문을 그린다.
fn type_body(
    ui: &mut egui::Ui,
    theme: &Theme,
    ty: ClipboardType,
    content: &ContentRepr,
    tr: &Translator,
    html_pretty: bool,
) {
    match (ty, content) {
        (ClipboardType::Text, ContentRepr::Text(text)) => text_body(ui, theme, text),
        (ClipboardType::Files, ContentRepr::Files(files)) => files_body(ui, theme, files),
        (ClipboardType::Image, ContentRepr::Image { .. }) => {
            let meta = content.meta_text().unwrap_or_default();
            image_body(ui, theme, tr, &meta);
        }
        (ClipboardType::Html, ContentRepr::Html(html)) => {
            // HTML은 렌더링하지 않고 소스를 표시한다. 체크하면 들여쓰기만 정리한다.
            if html_pretty {
                text_body(ui, theme, &prettify(html));
            } else {
                text_body(ui, theme, html);
            }
        }
        (ClipboardType::Other, ContentRepr::Other(entries)) => other_body(ui, theme, tr, entries),
        // read_available이 같은 종류의 타입과 내용을 짝지어 전달해야 한다.
        _ => unreachable!("ClipboardType/ContentRepr mismatch"),
    }
}

/// 이미지 픽셀 대신 아이콘·치수·크기와 미리보기 미지원 안내를 표시한다.
fn image_body(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, meta: &str) {
    well_centered(ui, theme, |ui| {
        icon_glyph(
            ui,
            baked_icons::IMAGE,
            CENTER_ICON_SIZE,
            theme.text_muted().to_egui(),
        );
        ui.add_space(theme.spacing_sm.value());
        meta_label(ui, theme, meta);
        ui.add_space(theme.spacing_xs.value());
        ui.label(
            egui::RichText::new(tr.t("clipboard_viewer.popup.image_no_preview"))
                .italics()
                .size(theme.font_size_caption.value())
                .color(theme.text_disabled().to_egui()),
        );
    });
}

/// 고정폭 글꼴과 caption 크기로 메타데이터를 표시한다.
fn meta_label(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .monospace()
            .size(theme.font_size_caption.value())
            .color(theme.text_muted().to_egui()),
    );
}

/// 긴 텍스트를 스크롤 영역에 표시한다. 공용 선택 가능한 표시 텍스트라 드래그 선택·복사만 되고
/// 캐럿·키 입력은 없다.
fn text_body(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    well(ui, theme, |ui| {
        tasty_ui_widgets::selectable_text(
            ui,
            text,
            theme.text_primary().to_egui(),
            theme.font_size_term_sm,
            true,
            false,
            tasty_ui_widgets::TextWrap::Wrap,
        );
    });
}

/// 파일 경로를 아이콘과 함께 나열한다. 긴 경로는 말줄임한다.
fn files_body(ui: &mut egui::Ui, theme: &Theme, files: &[std::path::PathBuf]) {
    well(ui, theme, |ui| {
        let icon_sz = theme.icon_glyph_size_sm.value();
        let gap = theme.spacing_sm.value();
        let row_h = icon_sz.max(theme.font_size_term_sm.value()) + theme.spacing_xs.value();
        let font = egui::FontId::monospace(theme.font_size_term_sm.value());
        for path in files {
            let full_w = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(egui::vec2(full_w, row_h), egui::Sense::hover());
            let icon_center = egui::pos2(rect.left() + icon_sz * 0.5, rect.center().y);
            baked_icon::draw(
                ui.painter(),
                baked_icons::FILE,
                icon_center,
                icon_sz,
                theme.text_muted().to_egui(),
            );
            let text_x = rect.left() + icon_sz + gap;
            let galley = truncated_galley(
                ui,
                &path.display().to_string(),
                font.clone(),
                theme.text_primary().to_egui(),
                (rect.right() - text_x).max(0.0),
            );
            ui.painter().galley(
                egui::pos2(text_x, rect.center().y - galley.size().y * 0.5),
                galley,
                theme.text_primary().to_egui(),
            );
        }
    });
}

/// 모든 기타 포맷을 스크롤 영역 안에 나열한다. 각 미리보기의 줄 수만 제한한다.
fn other_body(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, entries: &[OtherFormatEntry]) {
    well(ui, theme, |ui| {
        for (i, entry) in entries.iter().enumerate() {
            if i > 0 {
                ui.add_space(theme.spacing_sm.value());
                let full_w = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(full_w, theme.border_width.value()),
                    egui::Sense::hover(),
                );
                ui.painter().hline(
                    rect.x_range(),
                    rect.center().y,
                    egui::Stroke::new(
                        theme.border_width.value(),
                        theme.separator.to_egui_premultiplied(),
                    ),
                );
                ui.add_space(theme.spacing_sm.value());
            }
            other_format_block(ui, theme, tr, entry);
        }
    });
}

/// 포맷 이름·크기·미리보기를 그린다. 줄 수가 상한을 넘으면 생략한 줄 수를 표시한다.
fn other_format_block(ui: &mut egui::Ui, theme: &Theme, tr: &Translator, entry: &OtherFormatEntry) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(&entry.name)
                .monospace()
                .strong()
                .size(theme.font_size_caption.value())
                .color(theme.text_secondary().to_egui()),
        );
        ui.label(
            egui::RichText::new(format_bytes(entry.byte_len))
                .monospace()
                .size(theme.font_size_caption.value())
                .color(theme.text_muted().to_egui()),
        );
    });
    ui.add_space(theme.spacing_xs.value());
    let (shown, truncated_lines) = truncate_lines(&entry.preview, OTHER_PREVIEW_MAX_LINES);
    // 바이너리의 16진수 요약은 원래 텍스트와 구분하도록 기울임꼴로 표시한다.
    let mut preview_text = egui::RichText::new(shown)
        .monospace()
        .size(theme.font_size_term_sm.value())
        .color(theme.text_primary().to_egui());
    if entry.is_binary {
        preview_text = preview_text.italics();
    }
    ui.add(egui::Label::new(preview_text).wrap());
    if truncated_lines > 0 {
        ui.add_space(theme.spacing_xs.value());
        ui.label(
            egui::RichText::new(other_more_lines_text(tr, truncated_lines))
                .italics()
                .size(theme.font_size_caption.value())
                .color(theme.text_muted().to_egui()),
        );
    }
}

/// 표시할 앞부분과 생략된 줄 수를 반환한다.
fn truncate_lines(text: &str, max_lines: usize) -> (String, usize) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return (text.to_string(), 0);
    }
    (lines[..max_lines].join("\n"), lines.len() - max_lines)
}

/// 생략한 미리보기 줄 수 안내.
fn other_more_lines_text(tr: &Translator, n: usize) -> String {
    tr.t_count_template("clipboard_viewer.popup.other_more_lines", n as u64)
        .replace("{n}", &n.to_string())
}

/// 기타 포맷 개수 안내.
fn other_unrecognized_text(tr: &Translator, n: usize) -> String {
    tr.t_count_template(
        "clipboard_viewer.popup.other_unrecognized_formats",
        n as u64,
    )
    .replace("{n}", &n.to_string())
}

/// 기타 포맷에는 MIME 대신 개수를 표시한다. 다른 타입은 None이다.
fn other_footer_meta(tr: &Translator, ty: ClipboardType, content: &ContentRepr) -> Option<String> {
    match (ty, content) {
        (ClipboardType::Other, ContentRepr::Other(entries)) => {
            Some(other_unrecognized_text(tr, entries.len()))
        }
        _ => None,
    }
}

/// 너비를 넘는 한 줄 텍스트를 말줄임한다.
fn truncated_galley(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    ui.fonts(|f| f.layout_job(job))
}

/// 스크롤·중앙 정렬 영역이 함께 사용하는 배경·테두리·모서리 스타일.
fn well_frame(theme: &Theme) -> egui::Frame {
    let margin = theme.spacing_md.value();
    egui::Frame::new()
        .fill(theme.bg_app().to_egui())
        .stroke(egui::Stroke::new(
            theme.border_width.value(),
            theme.separator.to_egui_premultiplied(),
        ))
        .corner_radius(theme.corner_radius.value())
        .inner_margin(egui::Margin::symmetric(
            margin.round() as i8,
            theme.spacing_sm.value().round() as i8,
        ))
}

/// 긴 내용을 위한 스크롤 영역.
fn well(ui: &mut egui::Ui, theme: &Theme, add: impl FnOnce(&mut egui::Ui)) {
    well_frame(theme).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("clip_body")
            .drag_to_scroll(false)
            .show(ui, add);
    });
}

/// 이미지 정보를 상하좌우 중앙에 배치한다.
fn well_centered(ui: &mut egui::Ui, theme: &Theme, add: impl FnOnce(&mut egui::Ui)) {
    well_frame(theme).show(ui, |ui| {
        let h = ui.available_height().max(1.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), h),
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.vertical_centered(add);
            },
        );
    });
}

/// 타입별 MIME·메타데이터와 닫기 버튼을 그린다.
fn footer(
    ui: &mut egui::Ui,
    theme: &Theme,
    tr: &Translator,
    ty: Option<ClipboardType>,
    meta: Option<String>,
    close: &mut bool,
) {
    let full_w = ui.available_width();
    let pad_x = row_pad_x(theme);
    let pad_y = theme.spacing_sm.value();
    let ctrl_h = ControlSize::Sm.height(theme);
    let h = pad_y * 2.0 + ctrl_h;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(full_w, h), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.top() + theme.border_width.value() * 0.5,
        egui::Stroke::new(
            theme.border_width.value(),
            theme.separator.to_egui_premultiplied(),
        ),
    );

    if let Some(ty) = ty {
        let galley = ui.painter().layout_job(footer_job(
            theme.font_size_caption.value(),
            theme.text_muted().to_egui(),
            ty,
            meta.as_deref(),
        ));
        let y = rect.center().y - galley.size().y * 0.5;
        ui.painter().galley(
            egui::pos2(rect.left() + pad_x, y),
            galley,
            theme.text_muted().to_egui(),
        );
    }

    let btn_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + pad_y),
        egui::pos2(rect.right() - pad_x, rect.top() + pad_y + ctrl_h),
    );
    let mut bui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(btn_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    if Button::new(tr.t("clipboard_viewer.popup.close"))
        .variant(ButtonVariant::Secondary)
        .size(ControlSize::Sm)
        .show(&mut bui, theme)
        .clicked()
    {
        *close = true;
    }
}

/// HTML은 MIME과 메타데이터를, 기타 포맷은 개수를, 나머지는 MIME을 표시한다.
/// MIME은 모든 언어에서 고정폭인 값이고 메타데이터는 번역 문구라 run을 나눈다.
fn footer_runs(ty: ClipboardType, meta: Option<&str>) -> (Option<&'static str>, Option<&str>) {
    match (ty, meta) {
        (ClipboardType::Html, Some(meta)) => (Some(ty.mime_str()), Some(meta)),
        (ClipboardType::Other, Some(meta)) => (None, Some(meta)),
        _ => (Some(ty.mime_str()), None),
    }
}

fn footer_job(
    size: f32,
    color: egui::Color32,
    ty: ClipboardType,
    meta: Option<&str>,
) -> egui::text::LayoutJob {
    let (mime, words) = footer_runs(ty, meta);
    let mut job = egui::text::LayoutJob::default();
    if let Some(mime) = mime {
        job.append(
            mime,
            0.0,
            egui::TextFormat::simple(egui::FontId::monospace(size), color),
        );
    }
    if let Some(words) = words {
        let text = if mime.is_some() {
            format!(" · {words}")
        } else {
            words.to_string()
        };
        job.append(
            &text,
            0.0,
            egui::TextFormat::simple(tasty_ui_widgets::label_font(size), color),
        );
    }
    job
}

/// HTML의 문자·줄 수를 만든다. 다른 타입은 None이다.
fn html_footer_meta(tr: &Translator, ty: ClipboardType, content: &ContentRepr) -> Option<String> {
    match (ty, content) {
        (ClipboardType::Html, ContentRepr::Html(html)) => Some(format_html_meta(tr, html)),
        _ => None,
    }
}

fn format_html_meta(tr: &Translator, html: &str) -> String {
    let chars = html.chars().count();
    let lines = html.lines().count().max(1);
    let key = if lines == 1 {
        "clipboard_viewer.popup.html_meta_line"
    } else {
        "clipboard_viewer.popup.html_meta_lines"
    };
    tr.t(key)
        .replace("{chars}", &chars.to_string())
        .replace("{lines}", &lines.to_string())
}

/// 타입별 아이콘.
fn type_icon(ty: ClipboardType) -> &'static [&'static [[f32; 2]]] {
    match ty {
        ClipboardType::Text => baked_icons::TEXT_LEFT,
        ClipboardType::Files => baked_icons::FILE,
        ClipboardType::Image => baked_icons::IMAGE,
        ClipboardType::Html => baked_icons::HTML,
        ClipboardType::Other => baked_icons::LAYERS,
    }
}

/// 빈 상태·읽기 실패·이미 열림 안내를 중앙에 배치한다. danger는 오류 색상이다.
fn center_state(
    ui: &mut egui::Ui,
    theme: &Theme,
    icon: &'static [&'static [[f32; 2]]],
    title: &str,
    sub: Option<&str>,
    danger: bool,
) {
    let h = ui.available_height().max(1.0);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), h),
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.vertical_centered(|ui| {
                // 빈 상태 아이콘의 디자인 불투명도. 비활성 상태 토큰과는 역할이 다르다.
                const EMPTY_ICON_DANGER_OPACITY: f32 = 0.9;
                const EMPTY_ICON_MUTED_OPACITY: f32 = 0.5;
                let icon_tint = if danger {
                    theme
                        .accent_danger()
                        .to_egui()
                        .gamma_multiply(EMPTY_ICON_DANGER_OPACITY)
                } else {
                    theme
                        .text_muted()
                        .to_egui()
                        .gamma_multiply(EMPTY_ICON_MUTED_OPACITY)
                };
                icon_glyph(ui, icon, CENTER_ICON_SIZE, icon_tint);
                ui.add_space(theme.spacing_sm.value());
                let title_color = if danger {
                    theme.accent_danger().to_egui()
                } else {
                    theme.text_secondary().to_egui()
                };
                ui.label(
                    egui::RichText::new(title)
                        .size(theme.font_size_body.value())
                        .strong()
                        .color(title_color),
                );
                if let Some(sub) = sub {
                    ui.add_space(theme.spacing_xs.value());
                    ui.label(
                        egui::RichText::new(sub)
                            .size(theme.font_size_term_sm.value())
                            .color(theme.text_muted().to_egui()),
                    );
                }
            });
        },
    );
}

/// 지정한 정사각형 안에 폴리라인 아이콘을 그린다.
fn icon_glyph(ui: &mut egui::Ui, icon: &[&[[f32; 2]]], size: f32, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    baked_icon::draw(ui.painter(), icon, rect.center(), size, color);
}

/// 버튼 영역에 ICON_DRAW_RATIO 비율로 아이콘을 그린다.
fn icon_in_button(ui: &mut egui::Ui, icon: &[&[[f32; 2]]], rect: egui::Rect, color: egui::Color32) {
    baked_icon::draw(
        ui.painter(),
        icon,
        rect.center(),
        rect.height() * ICON_DRAW_RATIO,
        color,
    );
}

fn bottom_separator(ui: &mut egui::Ui, theme: &Theme, rect: egui::Rect) {
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - theme.border_width.value() * 0.5,
        egui::Stroke::new(
            theme.border_width.value(),
            theme.separator.to_egui_premultiplied(),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_runs_text_is_plain_mime_only() {
        assert_eq!(
            footer_runs(ClipboardType::Text, None),
            (Some("text/plain"), None)
        );
    }

    #[test]
    fn footer_runs_image_is_rgba8_mime_only() {
        assert_eq!(
            footer_runs(ClipboardType::Image, None),
            (Some("image/rgba8"), None)
        );
    }

    #[test]
    fn type_icon_text_uses_text_left_glyph() {
        // const의 주소는 달라질 수 있으므로 폴리라인 값을 비교한다.
        assert_eq!(type_icon(ClipboardType::Text), baked_icons::TEXT_LEFT);
    }

    #[test]
    fn footer_runs_files_is_uri_list_mime() {
        assert_eq!(
            footer_runs(ClipboardType::Files, None),
            (Some("text/uri-list"), None)
        );
    }

    #[test]
    fn type_icon_files_uses_file_glyph() {
        assert_eq!(type_icon(ClipboardType::Files), baked_icons::FILE);
    }

    #[test]
    fn type_icon_image_uses_image_glyph() {
        assert_eq!(type_icon(ClipboardType::Image), baked_icons::IMAGE);
    }

    #[test]
    fn footer_runs_html_without_meta_is_mime_only() {
        assert_eq!(
            footer_runs(ClipboardType::Html, None),
            (Some("text/html"), None)
        );
    }

    #[test]
    fn footer_runs_html_with_meta_combines_mime_and_meta() {
        assert_eq!(
            footer_runs(ClipboardType::Html, Some("312 chars · 1 line")),
            (Some("text/html"), Some("312 chars · 1 line"))
        );
    }

    #[test]
    fn type_icon_html_uses_html_glyph() {
        assert_eq!(type_icon(ClipboardType::Html), baked_icons::HTML);
    }

    #[test]
    fn html_footer_meta_is_none_for_non_html_types() {
        assert_eq!(
            html_footer_meta(
                &Translator::default(),
                ClipboardType::Text,
                &ContentRepr::Text("x".into())
            ),
            None
        );
    }

    fn test_translator() -> Translator {
        Translator::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lang"),
            "en",
        )
    }

    #[test]
    fn format_html_meta_singular_line_uses_singular_key() {
        assert_eq!(
            format_html_meta(&test_translator(), "<p>x</p>"),
            "8 chars · 1 line"
        );
    }

    #[test]
    fn format_html_meta_plural_lines_uses_plural_key() {
        assert_eq!(
            format_html_meta(&test_translator(), "line one\nline two"),
            "17 chars · 2 lines"
        );
    }

    #[test]
    fn type_icon_other_uses_layers_glyph() {
        assert_eq!(type_icon(ClipboardType::Other), baked_icons::LAYERS);
    }

    #[test]
    fn footer_runs_other_without_meta_falls_back_to_mime_str() {
        // read_available에서는 만들지 않는 빈 기타 목록도 처리할 수 있어야 한다.
        assert_eq!(
            footer_runs(ClipboardType::Other, None),
            (Some("application/octet-stream"), None)
        );
    }

    /// MIME은 고정폭 run이고 번역 메타데이터만 라벨 글꼴을 읽는다. en에서는 두 글꼴이 같아 구분되지
    /// 않고 언어는 프로세스마다 한 번 읽으므로, 라벨이 UI sans인 언어로 이 시험을 새 프로세스에서 잰다.
    #[test]
    fn footer_job_keeps_the_mime_run_mono() {
        const PROBE: &str = "CLIPBOARD_FOOTER_LOCALE_PROBE";
        if std::env::var_os(PROBE).is_none() {
            let exe = std::env::current_exe().expect("test binary path");
            for locale in ["ja", "zh-Hans"] {
                let out = std::process::Command::new(&exe)
                    .args(["--exact", "view::tests::footer_job_keeps_the_mime_run_mono"])
                    .env(PROBE, "1")
                    .env("TASTY_LOCALE", locale)
                    .output()
                    .expect("run the test binary again");
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(out.status.success(), "{locale}: {stdout}");
                assert!(stdout.contains("1 passed"), "{locale}: {stdout}");
            }
            return;
        }
        let job = footer_job(
            11.0,
            egui::Color32::WHITE,
            ClipboardType::Html,
            Some("312 chars"),
        );
        assert_eq!(job.text, "text/html · 312 chars");
        assert_eq!(job.sections.len(), 2);
        assert_eq!(
            job.sections[0].format.font_id.family,
            egui::FontFamily::Monospace
        );
        assert_eq!(
            job.sections[1].format.font_id.family,
            tasty_ui_widgets::label_font_family_for("ja")
        );
    }

    #[test]
    fn footer_runs_other_with_meta_replaces_mime_entirely() {
        assert_eq!(
            footer_runs(ClipboardType::Other, Some("2 unrecognized formats")),
            (None, Some("2 unrecognized formats"))
        );
    }

    #[test]
    fn other_footer_meta_is_none_for_non_other_types() {
        assert_eq!(
            other_footer_meta(
                &test_translator(),
                ClipboardType::Text,
                &ContentRepr::Text("x".into())
            ),
            None
        );
    }

    #[test]
    fn other_footer_meta_counts_entries() {
        let entries = vec![
            OtherFormatEntry::from_bytes("A".into(), b"1", 1024),
            OtherFormatEntry::from_bytes("B".into(), b"2", 1024),
        ];
        assert_eq!(
            other_footer_meta(
                &test_translator(),
                ClipboardType::Other,
                &ContentRepr::Other(entries)
            )
            .as_deref(),
            Some("2 unrecognized formats")
        );
    }

    #[test]
    fn truncate_lines_keeps_short_text_untouched() {
        assert_eq!(truncate_lines("a\nb\nc", 20), ("a\nb\nc".to_string(), 0));
    }

    #[test]
    fn truncate_lines_cuts_and_counts_remainder() {
        let text = (0..25)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (shown, cut) = truncate_lines(&text, 20);
        assert_eq!(shown.lines().count(), 20);
        assert_eq!(cut, 5);
    }

    #[test]
    fn other_more_lines_text_interpolates_count() {
        assert_eq!(
            other_more_lines_text(&test_translator(), 7),
            "+7 more lines"
        );
    }

    #[test]
    fn other_unrecognized_text_interpolates_count() {
        assert_eq!(
            other_unrecognized_text(&test_translator(), 3),
            "3 unrecognized formats"
        );
    }
}
