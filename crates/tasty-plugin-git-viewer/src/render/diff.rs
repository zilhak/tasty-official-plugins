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

/// diff 한 행의 잉크와 채움. 그리기 호출은 이 값만 받는다.
/// 정본: tasty-page `vendor/ui_kits/terminal/overlays/git_viewer.jsx` `DiffLine`.
/// 채움은 page component `git-diff-*-bg`가 가리키는 semantic tint 토큰이다.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DiffRowInk {
    /// 본문과 `+`/`-` 부호의 잉크.
    text: Color32,
    /// 줄 번호 잉크. hunk 머리는 번호를 그리지 않는다.
    number: Color32,
    /// 행 채움. context 행은 채우지 않는다.
    fill: Option<Color32>,
}

fn diff_row_ink(theme: &Theme, is_hunk: bool, kind: DiffLineKind) -> DiffRowInk {
    let (text, number, fill) = match (is_hunk, kind) {
        (true, _) => (
            theme.accent_info_raised(),
            theme.text_muted(),
            Some(theme.tint_info_bg()),
        ),
        (false, DiffLineKind::Addition) => (
            theme.accent_success_raised(),
            theme.text_muted_raised(),
            Some(theme.tint_success_bg()),
        ),
        (false, DiffLineKind::Deletion) => (
            theme.accent_danger_raised(),
            theme.text_muted_raised(),
            Some(theme.tint_danger_bg()),
        ),
        (false, DiffLineKind::Context) => (theme.text_primary(), theme.text_muted(), None),
    };
    DiffRowInk {
        text: text.to_egui(),
        number: number.to_egui(),
        fill: fill.map(|c| c.to_egui()),
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
    let ink = diff_row_ink(theme, is_hunk, kind);
    if let Some(fill) = ink.fill {
        // 배경은 최소한 표시 영역을 채우고, 긴 줄이면 해당 줄 너비까지 늘린다.
        let band_w = avail.max(diff_min_w(ui.painter(), theme, text, sz));
        ui.painter()
            .rect_filled(Rect::from_min_size(rect.min, vec2(band_w, h)), 0.0, fill);
    }
    let p = ui.painter();
    let cy = rect.center().y;
    if !is_hunk {
        let old_s = old_no.map(|n| n.to_string()).unwrap_or_default();
        let new_s = new_no.map(|n| n.to_string()).unwrap_or_default();
        p.text(
            egui::pos2(rect.left() + DIFF_GUTTER_W - 6.0, cy),
            Align2::RIGHT_CENTER,
            old_s,
            mono(sz),
            ink.number,
        );
        p.text(
            egui::pos2(rect.left() + DIFF_GUTTER_W * 2.0 - 8.0, cy),
            Align2::RIGHT_CENTER,
            new_s,
            mono(sz),
            ink.number,
        );
        let sign = match kind {
            DiffLineKind::Addition => "+",
            DiffLineKind::Deletion => "-",
            DiffLineKind::Context => "",
        };
        // 부호는 추가·삭제를 가르는 글자라 본문과 같은 줄 잉크로 그린다.
        p.text(
            egui::pos2(rect.left() + DIFF_GUTTER_W * 2.0 + DIFF_SIGN_W * 0.5, cy),
            Align2::CENTER_CENTER,
            sign,
            mono(sz),
            ink.text,
        );
    }
    let text_x = rect.left() + DIFF_GUTTER_W * 2.0 + DIFF_SIGN_W;
    p.text(
        egui::pos2(text_x, cy),
        Align2::LEFT_CENTER,
        text,
        mono(sz),
        ink.text,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::ClippedShape;
    use egui::{Pos2, Shape};
    use tasty_git_core::DiffHunk;

    fn mocha() -> Theme {
        Theme::with_colors_and_zoom(tasty_themes::mocha_fallback_colors(), false, 1.0)
    }

    /// 내장 Latte와 팔레트·밝기 분기가 같은 테마.
    fn latte() -> Theme {
        let file =
            tasty_themes::ThemeFile::parse(tasty_themes::LATTE_TOML_TEXT).expect("latte.toml");
        let (partial, is_light) = file.to_partial();
        let mut colors = tasty_themes::mocha_fallback_colors();
        colors.apply_partial(&partial);
        Theme::with_colors(colors, is_light.unwrap_or(true))
    }

    /// 줄 번호는 행 바탕의 text-muted 셀을 쓴다.
    /// Latte는 text_muted와 text_muted_raised를 가르고, Mocha는 text_muted_raised와 text_primary를 가른다.
    #[test]
    fn line_numbers_take_the_muted_cell_of_the_row() {
        let l = latte();
        assert_ne!(l.text_muted(), l.text_muted_raised());
        let m = mocha();
        assert_ne!(m.text_muted_raised(), m.text_primary());
        for t in [m, l] {
            assert_eq!(
                diff_row_ink(&t, false, DiffLineKind::Context).number,
                t.text_muted().to_egui()
            );
            for k in [DiffLineKind::Addition, DiffLineKind::Deletion] {
                assert_eq!(
                    diff_row_ink(&t, false, k).number,
                    t.text_muted_raised().to_egui()
                );
            }
        }
    }

    /// 채움은 각 색상의 semantic tint 토큰이고 context 행은 채우지 않는다.
    #[test]
    fn row_fills_read_the_tint_tokens() {
        for t in [mocha(), latte()] {
            assert_eq!(
                diff_row_ink(&t, true, DiffLineKind::Context).fill,
                Some(t.tint_info_bg().to_egui())
            );
            assert_eq!(
                diff_row_ink(&t, false, DiffLineKind::Addition).fill,
                Some(t.tint_success_bg().to_egui())
            );
            assert_eq!(
                diff_row_ink(&t, false, DiffLineKind::Deletion).fill,
                Some(t.tint_danger_bg().to_egui())
            );
            assert_eq!(diff_row_ink(&t, false, DiffLineKind::Context).fill, None);
        }
    }

    /// 줄 잉크는 -raised accent이고 context는 text_primary다.
    /// Mocha는 기본 accent와 -raised가 같아 -raised 판별은 Latte가 맡고,
    /// Latte는 text_primary와 text_muted_raised가 같아 context 본문 판별은 Mocha가 맡는다.
    #[test]
    fn line_ink_is_the_raised_accent() {
        let l = latte();
        assert_ne!(l.accent_info_raised(), l.accent_info());
        assert_ne!(l.accent_success_raised(), l.accent_success());
        assert_ne!(l.accent_danger_raised(), l.accent_danger());
        let m = mocha();
        assert_ne!(m.text_primary(), m.text_muted_raised());
        for t in [m, l] {
            assert_eq!(
                diff_row_ink(&t, true, DiffLineKind::Context).text,
                t.accent_info_raised().to_egui()
            );
            assert_eq!(
                diff_row_ink(&t, false, DiffLineKind::Addition).text,
                t.accent_success_raised().to_egui()
            );
            assert_eq!(
                diff_row_ink(&t, false, DiffLineKind::Deletion).text,
                t.accent_danger_raised().to_egui()
            );
            assert_eq!(
                diff_row_ink(&t, false, DiffLineKind::Context).text,
                t.text_primary().to_egui()
            );
        }
    }

    const HUNK: &str = "@@ -10,2 +20,2 @@";
    const KEPT: &str = "kept_body";
    const GONE: &str = "gone_body";
    const CAME: &str = "came_body";

    /// hunk 하나에 context·삭제·추가 줄이 하나씩 있다. 본문과 줄 번호는 서로 겹치지 않는다.
    fn sample_diff() -> DiffData {
        let line = |kind, content: &str, old, new| DiffLine {
            kind,
            content: content.to_owned(),
            old_lineno: old,
            new_lineno: new,
        };
        DiffData {
            file_path: "src/lib.rs".to_owned(),
            hunks: vec![DiffHunk {
                header: HUNK.to_owned(),
                lines: vec![
                    line(DiffLineKind::Context, KEPT, Some(10), Some(20)),
                    line(DiffLineKind::Deletion, GONE, Some(11), None),
                    line(DiffLineKind::Addition, CAME, None, Some(31)),
                ],
            }],
        }
    }

    /// 출력에 그려진 글자 도형 하나.
    struct PaintedText {
        text: String,
        rect: Rect,
        /// 실제 테셀레이터가 낸 정점 색의 종류. galley 색, override·fallback, 도형 opacity가 모두 반영된다.
        /// 밑줄·배경이 없는 글자 도형이라 정점은 모두 글리프다.
        glyph_colors: Vec<Color32>,
        opacity_factor: f32,
    }

    fn flatten(shape: &Shape, out: &mut Vec<Shape>) {
        match shape {
            Shape::Vec(v) => v.iter().for_each(|s| flatten(s, out)),
            s => out.push(s.clone()),
        }
    }

    fn tessellated_colors(ctx: &egui::Context, clipped: ClippedShape, ppp: f32) -> Vec<Color32> {
        let mut out: Vec<Color32> = Vec::new();
        for prim in ctx.tessellate(vec![clipped], ppp) {
            if let egui::epaint::Primitive::Mesh(mesh) = prim.primitive {
                for v in &mesh.vertices {
                    if !out.contains(&v.color) {
                        out.push(v.color);
                    }
                }
            }
        }
        out
    }

    fn paint_frame(
        ctx: &egui::Context,
        theme: &Theme,
        diff: &DiffData,
        width_cache: &mut Option<(f32, f32)>,
    ) -> (Vec<PaintedText>, Vec<(Rect, Color32)>) {
        let tr = Translator::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lang"),
            "en",
        );
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    let area = ui.max_rect();
                    draw_diff(ui, theme, &tr, diff, width_cache, area);
                });
        });
        let (mut texts, mut rects) = (Vec::new(), Vec::new());
        for clipped in &out.shapes {
            let mut shapes = Vec::new();
            flatten(&clipped.shape, &mut shapes);
            for shape in shapes {
                match &shape {
                    Shape::Text(t) => texts.push(PaintedText {
                        text: t.galley.text().to_owned(),
                        rect: t.galley.rect.translate(t.pos.to_vec2()),
                        opacity_factor: t.opacity_factor,
                        glyph_colors: tessellated_colors(
                            ctx,
                            ClippedShape {
                                clip_rect: clipped.clip_rect,
                                shape: shape.clone(),
                            },
                            out.pixels_per_point,
                        ),
                    }),
                    // 화면에 칠해지는 영역은 사각형과 클립의 교차다.
                    Shape::Rect(r) => rects.push((r.rect.intersect(clipped.clip_rect), r.fill)),
                    _ => {}
                }
            }
        }
        (texts, rects)
    }

    /// 그리기 호출이 `diff_row_ink`의 값을 쓴다. 같은 Context·diff·너비 캐시로 Mocha 다음 Latte를 그리고,
    /// 프레임마다 그 테마로 글자의 유효 글리프 색·불투명도와 행별 채움을 본다.
    #[test]
    fn painted_rows_use_the_row_ink() {
        let ctx = egui::Context::default();
        let diff = sample_diff();
        let mut width_cache = None;
        for (name, theme) in [("mocha", mocha()), ("latte", latte())] {
            let (texts, rects) = paint_frame(&ctx, &theme, &diff, &mut width_cache);
            for t in &texts {
                assert_eq!(t.opacity_factor, 1.0, "{name}: '{}' opacity", t.text);
            }
            let find = |s: &str| -> &PaintedText {
                let hits: Vec<&PaintedText> = texts.iter().filter(|t| t.text == s).collect();
                assert_eq!(hits.len(), 1, "{name}: '{s}' 글자 도형 수");
                hits[0]
            };
            let hunk = diff_row_ink(&theme, true, DiffLineKind::Context);
            let ctx_ink = diff_row_ink(&theme, false, DiffLineKind::Context);
            let del = diff_row_ink(&theme, false, DiffLineKind::Deletion);
            let add = diff_row_ink(&theme, false, DiffLineKind::Addition);
            let expected = [
                (HUNK, hunk.text, theme.accent_info_raised().to_egui()),
                (KEPT, ctx_ink.text, theme.text_primary().to_egui()),
                (GONE, del.text, theme.accent_danger_raised().to_egui()),
                ("-", del.text, theme.accent_danger_raised().to_egui()),
                (CAME, add.text, theme.accent_success_raised().to_egui()),
                ("+", add.text, theme.accent_success_raised().to_egui()),
                ("10", ctx_ink.number, theme.text_muted().to_egui()),
                ("20", ctx_ink.number, theme.text_muted().to_egui()),
                ("11", del.number, theme.text_muted_raised().to_egui()),
                ("31", add.number, theme.text_muted_raised().to_egui()),
            ];
            for (s, ink, token) in expected {
                assert_eq!(ink, token, "{name}: '{s}' 행 잉크와 토큰");
                assert_eq!(
                    find(s).glyph_colors,
                    vec![token],
                    "{name}: '{s}' 유효 글리프 색"
                );
            }
            // 행 채움: 본문 글자 영역과 겹쳐 보이는 채움(사각형 ∩ 클립). 높이와 무관하다.
            // 본문 전체 바탕(well, bg_app)과 투명 사각형은 행 채움이 아니라 뺀다.
            let well = theme.bg_app().to_egui();
            let fills_over = |s: &str| -> Vec<(Rect, Color32)> {
                let body = find(s).rect;
                rects
                    .iter()
                    .filter(|(_, fill)| *fill != well && fill.a() > 0)
                    .filter(|(r, _)| {
                        let o = r.intersect(body);
                        o.width() > 0.0 && o.height() > 0.0
                    })
                    .copied()
                    .collect()
            };
            for (s, tint, what) in [
                (HUNK, theme.tint_info_bg().to_egui(), "hunk"),
                (GONE, theme.tint_danger_bg().to_egui(), "삭제"),
                (CAME, theme.tint_success_bg().to_egui(), "추가"),
            ] {
                let over = fills_over(s);
                let colors: Vec<Color32> = over.iter().map(|(_, c)| *c).collect();
                assert_eq!(colors, vec![tint], "{name}: {what} 채움");
                assert!(
                    over[0].0.contains_rect(find(s).rect),
                    "{name}: {what} 채움이 본문 전체를 덮는다"
                );
            }
            assert!(
                fills_over(KEPT).is_empty(),
                "{name}: context 행은 채우지 않는다"
            );
        }
    }
}
