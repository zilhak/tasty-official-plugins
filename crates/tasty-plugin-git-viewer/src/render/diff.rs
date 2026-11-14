//! diff의 도구 모음과 이전·이후 줄 번호, 부호, 텍스트를 그린다.
//! 가로 스크롤 너비는 전체 줄의 최댓값으로 고정한다.
//! 줄 배경은 표시 영역과 해당 줄에 필요한 너비 중 큰 값으로 칠한다.

use egui::{Align, Align2, Color32, Layout, Rect, Sense, UiBuilder, vec2};
use tasty_plugin_sdk::Translator;
use tasty_type_appearance::theme::Theme;
use tasty_ui_widgets::{Button, ButtonVariant, ControlSize};

use tasty_git_core::{DiffData, DiffLine, DiffLineKind};

use super::{bottom_separator, empty_line, mono};

/// diff old/new 라인번호 거터 폭(jsx `width: 34`).
const DIFF_GUTTER_W: f32 = 34.0;
/// diff `+`/`-`/context 부호 컬럼 폭(jsx `width: 14`).
const DIFF_SIGN_W: f32 = 14.0;

/// diff 툴바: Back(ghost) + 파일 경로(mono muted). Back 클릭 여부 반환.
pub(super) fn diff_toolbar(
    ui: &mut egui::Ui,
    theme: &Theme,
    tr: &Translator,
    area: Rect,
    diff: Option<&DiffData>,
) -> bool {
    ui.painter()
        .rect_filled(area, 0.0, theme.bg_sidebar().to_egui());
    bottom_separator(ui, theme, area);
    let pad_x = theme.spacing_sm.value();
    let mut cui = ui.new_child(
        UiBuilder::new()
            .max_rect(area.shrink2(vec2(pad_x, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    cui.spacing_mut().item_spacing.x = theme.spacing_sm.value();
    let back = Button::new(tr.t("git_viewer.back_to_log"))
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Sm)
        .show(&mut cui, theme)
        .clicked();
    if let Some(diff) = diff {
        cui.label(
            egui::RichText::new(&diff.file_path)
                .font(mono(theme.font_size_caption.value()))
                .color(theme.text_muted().to_egui()),
        );
    }
    back
}

pub(super) fn draw_diff(
    ui: &mut egui::Ui,
    theme: &Theme,
    tr: &Translator,
    diff: &DiffData,
    width_cache: &mut Option<(f32, f32)>,
    area: Rect,
) {
    ui.painter()
        .rect_filled(area, 0.0, theme.bg_app().to_egui());
    let mut pane = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(Layout::top_down(Align::Min)),
    );
    pane.spacing_mut().item_spacing = vec2(0.0, 0.0);
    if diff.hunks.is_empty() {
        empty_line(&mut pane, theme, tr.t("git_viewer.no_changes"));
        return;
    }
    // 각 hunk 헤더의 전체 행 인덱스를 계산한다.
    let mut starts: Vec<usize> = Vec::with_capacity(diff.hunks.len());
    let mut total_rows = 0usize;
    for hunk in &diff.hunks {
        starts.push(total_rows);
        total_rows += 1 + hunk.lines.len();
    }

    // 보이는 줄만 재면 스크롤 범위가 흔들리므로 전체 최장 너비를 캐시한다.
    // 글꼴 크기가 바뀌거나 set_diff가 내용을 바꾸면 다시 계산한다.
    let sz = theme.font_size_caption.value();
    let row_w = match *width_cache {
        Some((cached_sz, w)) if cached_sz == sz => w,
        _ => {
            let w = diff_content_w(&pane, theme, diff, sz);
            *width_cache = Some((sz, w));
            w
        }
    };

    egui::ScrollArea::both()
        .id_salt("gv_diff")
        .drag_to_scroll(false)
        .auto_shrink([false, false])
        .show_rows(&mut pane, diff_row_h(theme), total_rows, |ui, row_range| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            for row in row_range {
                // flat 행 → (hunk, 헤더 | 라인). `starts` 는 오름차순이라 이분 탐색.
                let h = starts
                    .partition_point(|&start| start <= row)
                    .wrapping_sub(1);
                let (Some(hunk), Some(start)) = (diff.hunks.get(h), starts.get(h)) else {
                    continue;
                };
                let off = row - start;
                ui.push_id(row, |ui| {
                    if off == 0 {
                        diff_line(ui, theme, DiffRow::hunk(&hunk.header), row_w);
                    } else if let Some(line) = hunk.lines.get(off - 1) {
                        diff_line(ui, theme, DiffRow::line(line), row_w);
                    }
                });
            }
        });
}

/// hunk 헤더와 일반 줄에 같은 높이를 사용한다.
fn diff_row_h(theme: &Theme) -> f32 {
    (theme.font_size_caption.value() * 1.65).round()
}

/// hunk 헤더를 포함한 전체 줄에서 필요한 최대 너비를 구한다.
fn diff_content_w(ui: &egui::Ui, theme: &Theme, diff: &DiffData, sz: f32) -> f32 {
    let p = ui.painter();
    let mut w = 0.0f32;
    for hunk in &diff.hunks {
        w = w.max(diff_min_w(p, theme, &hunk.header, sz));
        for line in &hunk.lines {
            w = w.max(diff_min_w(p, theme, &line.content, sz));
        }
    }
    w
}

/// 줄 번호 두 칸·부호·텍스트·여백을 포함한 한 줄의 너비.
fn diff_min_w(p: &egui::Painter, theme: &Theme, text: &str, sz: f32) -> f32 {
    DIFF_GUTTER_W * 2.0
        + DIFF_SIGN_W
        + p.layout_no_wrap(text.to_owned(), mono(sz), Color32::PLACEHOLDER)
            .rect
            .width()
        + theme.spacing_md.value()
}

/// diff 한 줄의 렌더 입력 — hunk 헤더와 일반 라인을 한 모양으로 다룬다.
struct DiffRow<'a> {
    kind: DiffLineKind,
    is_hunk: bool,
    old_no: Option<u32>,
    new_no: Option<u32>,
    text: &'a str,
}

impl<'a> DiffRow<'a> {
    fn hunk(header: &'a str) -> Self {
        Self {
            kind: DiffLineKind::Context,
            is_hunk: true,
            old_no: None,
            new_no: None,
            text: header,
        }
    }

    fn line(line: &'a DiffLine) -> Self {
        Self {
            kind: line.kind,
            is_hunk: false,
            old_no: line.old_lineno,
            new_no: line.new_lineno,
            text: &line.content,
        }
    }
}

/// diff 한 줄: gutter(old/new) + sign + text. hunk 헤더 밴드 / ± 라인 tint.
fn diff_line(ui: &mut egui::Ui, theme: &Theme, row: DiffRow<'_>, row_w: f32) {
    let DiffRow {
        kind,
        is_hunk,
        old_no,
        new_no,
        text,
    } = row;
    let sz = theme.font_size_caption.value();
    let h = diff_row_h(theme);
    // 모든 행에 같은 전체 너비를 할당해 스크롤 범위를 유지한다.
    let avail = ui.available_width();
    let full_w = avail.max(row_w);
    let (rect, _) = ui.allocate_exact_size(vec2(full_w, h), Sense::hover());
    // diff 줄 배경 톤. hunk 머리만 한 단계 더 옅다. 대응 토큰 없음.
    const DIFF_HUNK_BG_OPACITY: f32 = 0.09;
    const DIFF_LINE_BG_OPACITY: f32 = 0.10;

    let (fg, bg) = match (is_hunk, kind) {
        (true, _) => (
            theme.accent_info().to_egui(),
            theme
                .accent_info()
                .to_egui()
                .gamma_multiply(DIFF_HUNK_BG_OPACITY),
        ),
        (false, DiffLineKind::Addition) => (
            theme.accent_success().to_egui(),
            theme
                .accent_success()
                .to_egui()
                .gamma_multiply(DIFF_LINE_BG_OPACITY),
        ),
        (false, DiffLineKind::Deletion) => (
            theme.accent_danger().to_egui(),
            theme
                .accent_danger()
                .to_egui()
                .gamma_multiply(DIFF_LINE_BG_OPACITY),
        ),
        (false, DiffLineKind::Context) => (theme.text_primary().to_egui(), Color32::TRANSPARENT),
    };
    if bg != Color32::TRANSPARENT {
        // 배경은 최소한 표시 영역을 채우고, 긴 줄이면 해당 줄 너비까지 늘린다.
        let band_w = avail.max(diff_min_w(ui.painter(), theme, text, sz));
        ui.painter()
            .rect_filled(Rect::from_min_size(rect.min, vec2(band_w, h)), 0.0, bg);
    }
    let p = ui.painter();
    let cy = rect.center().y;
    let gutter = theme.text_muted_raised().to_egui();
    if !is_hunk {
        let old_s = old_no.map(|n| n.to_string()).unwrap_or_default();
        let new_s = new_no.map(|n| n.to_string()).unwrap_or_default();
        p.text(
            egui::pos2(rect.left() + DIFF_GUTTER_W - 6.0, cy),
            Align2::RIGHT_CENTER,
            old_s,
            mono(sz),
            gutter,
        );
        p.text(
            egui::pos2(rect.left() + DIFF_GUTTER_W * 2.0 - 8.0, cy),
            Align2::RIGHT_CENTER,
            new_s,
            mono(sz),
            gutter,
        );
        let sign = match kind {
            DiffLineKind::Addition => "+",
            DiffLineKind::Deletion => "-",
            DiffLineKind::Context => "",
        };
        // 부호 글리프는 jsx `opacity: 0.8`(hunk band border 패턴과 동일 gamma_multiply).
        // +/- 부호는 본문 전경보다 한 단계 물러난다. 대응 토큰 없음.
        const DIFF_SIGN_OPACITY: f32 = 0.8;
        p.text(
            egui::pos2(rect.left() + DIFF_GUTTER_W * 2.0 + DIFF_SIGN_W * 0.5, cy),
            Align2::CENTER_CENTER,
            sign,
            mono(sz),
            fg.gamma_multiply(DIFF_SIGN_OPACITY),
        );
    }
    let text_x = rect.left() + DIFF_GUTTER_W * 2.0 + DIFF_SIGN_W;
    p.text(
        egui::pos2(text_x, cy),
        Align2::LEFT_CENTER,
        text,
        mono(sz),
        fg,
    );
}
