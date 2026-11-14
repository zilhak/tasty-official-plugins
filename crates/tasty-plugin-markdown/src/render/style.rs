//! 테마 값을 문서 CSS로 옮긴다. 색·크기·간격은 모두 테마 토큰에서 읽는다.

use tasty_type_appearance::theme::Theme;

use super::address;
use super::bed_focus;
use super::callout::{self, alert_css, alert_icon_data_uri};

/// 테마 값을 CSS 변수와 스타일로 변환한다. 제목과 각주의 scroll-margin-top은
/// 고정 주소창 높이를 고려한다. 주소창, 검색창 위치와 앵커 여백은 --md-addr-bar-h를
/// 공유한다. 검색 개수 표시의 min-width:40px는 별도의 너비 값이다.
///
/// html의 height:100%와 body의 min-height:100%는 서로 역할이 다르다.
/// body가 문서만큼 늘어나야 sticky 주소창이 끝까지 남고, html 높이가 정해져야
/// 짧은 문서에서도 body의 백분율 최소 높이가 viewport를 채운다.
pub(super) fn theme_css(theme: &Theme) -> String {
    let [h1, h2, h3, h4, h5, h6] = heading_sizes_px(theme);
    let body = theme.font_size_body.value();
    format!(
        r#":root{{
--md-fg:{fg};
--md-strong:{strong};
--md-link:{link};
--md-code-bg:{code_bg};
--md-code-border:{code_border};
--md-border:{border};
--md-quote-bar:{quote_bar};
--md-quote-fg:{quote_fg};
--md-callout-fg:{callout_fg};
--md-rule:{rule};
{bed_vars}--md-control-bg:{control_bg};
--md-input-bg:{input_bg};
--md-disabled-fill:{disabled_fill};
--md-disabled-border:{disabled_border};
--md-disabled-fg:{disabled_fg};
--md-radius:{radius}px;
--md-border-w:{border_w}px;
--md-quote-bar-w:{quote_bar_w}px;
--md-space-xs:{space_xs}px;
--md-space-sm:{space_sm}px;
--md-space-md:{space_md}px;
--md-callout-icon:{callout_icon}px;
--md-callout-icon-gap:{callout_icon_gap}px;
--md-callout-marker:{callout_marker}px;
--md-addr-bar-h:{addr_bar_h}px;
--md-font-body:{body}px;
--md-h1:{h1}px;--md-h2:{h2}px;--md-h3:{h3}px;--md-h4:{h4}px;--md-h5:{h5}px;--md-h6:{h6}px;
}}
html{{height:100%;margin:0;padding:0;}}
body{{min-height:100%;margin:0;padding:0;background:var(--md-bg);color:var(--md-fg);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;font-size:var(--md-font-body);line-height:1.6;}}
{addr_css}#tasty-find-bar{{position:fixed;top:calc(var(--md-addr-bar-h) + var(--md-space-xs));right:var(--md-space-sm);z-index:20;display:flex;align-items:center;gap:var(--md-space-xs);height:28px;padding:0 var(--md-space-xs);background:{bg_sidebar};border:var(--md-border-w) solid {separator};border-radius:var(--md-radius);box-shadow:0 2px 8px rgba(0,0,0,0.25);}}
#tasty-find-bar[hidden]{{display:none;}}
#tasty-addr-bar,#tasty-find-bar{{-webkit-user-select:none;user-select:none;}}
#tasty-addr-input,#tasty-find-input{{-webkit-user-select:text;user-select:text;}}
#tasty-find-input{{width:140px;height:22px;border:var(--md-border-w) solid var(--md-border);border-radius:var(--md-radius);padding:0 var(--md-space-xs);background:var(--md-control-bg);color:var(--md-fg);font-size:var(--md-font-body);}}
#tasty-find-count{{min-width:40px;text-align:center;font-size:calc(var(--md-font-body) * 0.85);color:{muted};}}
#tasty-find-count.tasty-find-nomatch{{color:{danger};}}
.tasty-find-btn{{height:22px;width:22px;flex-shrink:0;display:inline-flex;align-items:center;justify-content:center;border:var(--md-border-w) solid var(--md-border);border-radius:var(--md-radius);background:var(--md-code-bg);color:var(--md-fg);font-size:10px;line-height:1;padding:0;cursor:pointer;}}
.tasty-find-btn:disabled{{background:var(--md-disabled-fill);border-color:var(--md-disabled-border);color:var(--md-disabled-fg);cursor:default;}}
mark.tasty-find-hit{{background:{find_hit_bg};color:{find_hit_fg};border-radius:2px;}}
mark.tasty-find-hit.tasty-find-current{{background:{find_current_bg};color:{find_current_fg};}}
#tasty-md-body{{padding:var(--md-space-sm) var(--md-space-md);}}
h1,h2,h3,h4,h5,h6{{color:var(--md-strong);font-weight:600;margin:1em 0 0.5em;scroll-margin-top:calc(var(--md-addr-bar-h) + var(--md-space-sm));}}
.footnote-reference,.footnote-definition{{scroll-margin-top:calc(var(--md-addr-bar-h) + var(--md-space-sm));}}
h1{{font-size:var(--md-h1);}}h2{{font-size:var(--md-h2);}}h3{{font-size:var(--md-h3);}}
h4{{font-size:var(--md-h4);}}h5{{font-size:var(--md-h5);}}h6{{font-size:var(--md-h6);}}
#tasty-toc{{margin:var(--md-space-sm) var(--md-space-md) 0;padding:var(--md-space-sm) var(--md-space-md);border:var(--md-border-w) solid var(--md-border);border-radius:var(--md-radius);background:var(--md-code-bg);}}
#tasty-toc-toggle{{all:unset;cursor:pointer;display:inline-flex;align-items:center;gap:var(--md-space-xs);font-weight:600;font-size:var(--md-font-body);color:var(--md-strong);}}
#tasty-toc-toggle::before{{content:"\25be";display:inline-block;}}
#tasty-toc.tasty-toc-collapsed #tasty-toc-toggle::before{{content:"\25b8";}}
#tasty-toc-list{{list-style:none;margin:var(--md-space-xs) 0 0;padding:0;max-height:280px;overflow-y:auto;}}
#tasty-toc.tasty-toc-collapsed #tasty-toc-list{{display:none;}}
#tasty-toc-list a{{display:block;padding:var(--md-space-xs) 0;color:var(--md-link);text-decoration:none;font-size:var(--md-font-body);}}
#tasty-toc-list a:hover{{text-decoration:underline;}}
.tasty-toc-l1 a{{padding-left:0;}}
.tasty-toc-l2 a{{padding-left:var(--md-space-sm);}}
.tasty-toc-l3 a{{padding-left:calc(var(--md-space-sm) * 2);}}
.tasty-toc-l4 a{{padding-left:calc(var(--md-space-sm) * 3);}}
.tasty-toc-l5 a{{padding-left:calc(var(--md-space-sm) * 4);}}
.tasty-toc-l6 a{{padding-left:calc(var(--md-space-sm) * 5);}}
a{{color:var(--md-link);}}
.tasty-wikilink-missing a{{color:{danger_raised};text-decoration:underline dotted;}}
strong{{color:var(--md-strong);font-weight:600;}}
code{{background:var(--md-code-bg);border-radius:var(--md-radius);padding:0.1em 0.35em;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;}}
pre{{position:relative;background:var(--md-code-bg);border:var(--md-border-w) solid var(--md-code-border);border-radius:var(--md-radius);padding:var(--md-space-sm);overflow:auto;}}
pre code{{background:none;padding:0;}}
.tasty-copy-btn{{position:absolute;top:var(--md-space-xs);right:var(--md-space-xs);height:22px;padding:0 var(--md-space-xs);border:var(--md-border-w) solid var(--md-border);border-radius:var(--md-radius);background:var(--md-control-bg);color:var(--md-fg);font-size:calc(var(--md-font-body) * 0.8);line-height:1;cursor:pointer;opacity:0;transition:opacity 0.15s ease;}}
pre:hover .tasty-copy-btn,.tasty-copy-btn:focus-visible{{opacity:1;}}
@media (hover:none){{.tasty-copy-btn{{opacity:1;}}}}
.tasty-copy-btn[data-state="copied"]{{border-color:{success};color:{success};}}
.tasty-copy-btn[data-state="failed"]{{border-color:{danger};color:{danger};}}
table{{border-collapse:collapse;background:var(--md-row);}}
th,td{{border:var(--md-border-w) solid var(--md-border);padding:var(--md-space-xs) var(--md-space-sm);text-align:left;}}
th{{background:{table_header_bg};color:{table_header_fg};}}
td{{color:{table_cell_fg};}}
tr:nth-child(even){{background:var(--md-zebra);}}
blockquote{{border-left:var(--md-quote-bar-w) solid var(--md-quote-bar);margin:0.5em 0;padding:0.1em var(--md-space-md);color:var(--md-quote-fg);}}
blockquote[class^="markdown-alert-"],details[class^="markdown-alert-"]{{color:var(--md-callout-fg);}}
blockquote[class^="markdown-alert-"]{{border-radius:var(--md-radius);padding:var(--md-space-sm) var(--md-space-md);}}
blockquote[class^="markdown-alert-"]>.tasty-callout-label{{display:block;}}
blockquote[class^="markdown-alert-"]>.tasty-callout-label::before,blockquote[class^="markdown-alert-"][data-label]::before{{content:attr(data-label);display:block;font-weight:600;margin-bottom:var(--md-space-xs);padding-left:calc(var(--md-callout-icon) + var(--md-callout-icon-gap));background-repeat:no-repeat;background-position:left center;background-size:var(--md-callout-icon) var(--md-callout-icon);}}
details[class^="markdown-alert-"]{{border-radius:var(--md-radius);padding:var(--md-space-sm) var(--md-space-md);border-left-width:var(--md-quote-bar-w);border-left-style:solid;}}
details[class^="markdown-alert-"]>summary{{cursor:pointer;font-weight:600;list-style:none;padding-left:calc(var(--md-callout-marker) + var(--md-callout-icon-gap));background-repeat:no-repeat;background-position:left center;background-size:var(--md-callout-marker) var(--md-callout-marker);}}
details[class^="markdown-alert-"]>summary::-webkit-details-marker{{display:none;}}
details[class^="markdown-alert-"]>summary::before{{content:"";display:inline-block;width:var(--md-callout-icon);height:var(--md-callout-icon);margin-right:var(--md-callout-icon-gap);vertical-align:middle;background-repeat:no-repeat;background-position:center;background-size:var(--md-callout-icon) var(--md-callout-icon);}}
details[class^="markdown-alert-"][open]>summary{{margin-bottom:var(--md-space-xs);}}
{alert_rules}
{hljs_rules}
hr{{border:none;border-top:var(--md-border-w) solid var(--md-rule);margin:var(--md-space-md) 0;}}
img{{max-width:100%;}}
.tasty-img-error{{display:inline-flex;align-items:center;gap:var(--md-space-xs);flex-wrap:wrap;max-width:100%;box-sizing:border-box;padding:var(--md-space-xs) var(--md-space-sm);border:var(--md-border-w) dashed {danger};border-radius:var(--md-radius);background:var(--md-code-bg);}}
.tasty-img-error-icon{{flex:0 0 auto;width:16px;height:16px;background-image:url("{img_error_icon}");background-repeat:no-repeat;background-size:16px 16px;}}
.tasty-img-error-label{{color:{danger};font-weight:600;}}
.tasty-img-error-path{{color:{muted};word-break:break-all;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;}}
figure{{margin:var(--md-space-sm) 0;text-align:center;}}
figcaption{{margin-top:var(--md-space-xs);font-size:var(--md-font-body);color:{muted};}}
ul,ol{{padding-left:1.5em;}}
li input[type=checkbox]{{margin-right:0.4em;}}
.tasty-state{{padding:var(--md-space-md);color:{muted};}}
.tasty-state-title{{font-size:var(--md-font-body);font-weight:600;color:{danger_raised};}}
.tasty-state-detail{{color:{muted};white-space:pre-wrap;}}
.tasty-state-action{{display:block;margin-top:var(--md-space-sm);height:{btn_sm_h}px;padding:0 {btn_sm_pad_x}px;border:var(--md-border-w) solid {btn_secondary_border};border-radius:{btn_radius}px;background:{btn_secondary_bg};color:{btn_fg};font-family:inherit;font-size:{btn_sm_font}px;cursor:pointer;}}
.tasty-state-action:hover{{border-color:{btn_secondary_border_hover};background-image:linear-gradient({btn_overlay_hover},{btn_overlay_hover});}}
.tasty-state-action:active{{background-image:linear-gradient({btn_overlay_active},{btn_overlay_active});}}
"#,
        fg = theme.text_secondary().to_hex(),
        strong = theme.text_primary().to_hex(),
        link = theme.accent_primary_raised().to_hex(),
        code_bg = theme.surface_raised().to_hex(),
        code_border = theme.separator.unpremultiplied().to_hex(),
        border = theme.md_table_border().to_hex(),
        quote_bar = theme.md_quote_bar().to_hex(),
        quote_fg = theme.md_quote_fg().to_hex(),
        // 콜아웃 본문은 인용 글자색을 물려받지 않는다.
        callout_fg = theme.text_primary().to_hex(),
        table_header_bg = theme.md_table_header_bg().to_hex(),
        table_header_fg = theme.md_table_header_fg().to_hex(),
        table_cell_fg = theme.md_table_cell_fg().to_hex(),
        rule = theme.separator.unpremultiplied().to_hex(),
        bed_vars = bed_focus::state_vars(theme),
        // 찾기 입력과 코드 복사 버튼은 문서 바탕이 아니라 이전과 같은 bg_app으로 칠한다.
        control_bg = theme.bg_app().to_hex(),
        // 주소 입력은 디자인 킷의 공용 입력 칸과 같은 input-bg다.
        input_bg = theme.input_bg().to_hex(),
        radius = theme.corner_radius.value(),
        border_w = theme.border_width.value(),
        quote_bar_w = theme.md_quote_bar_width().value(),
        space_xs = theme.spacing_xs.value(),
        space_sm = theme.spacing_sm.value(),
        space_md = theme.spacing_md.value(),
        callout_icon = theme.icon_glyph_size_md.value(),
        callout_icon_gap = theme.md_callout_icon_gap().value(),
        callout_marker = theme.md_callout_marker_size().value(),
        body = body,
        h1 = h1,
        h2 = h2,
        h3 = h3,
        h4 = h4,
        h5 = h5,
        h6 = h6,
        bg_sidebar = theme.bg_sidebar().to_hex(),
        separator = theme.separator.unpremultiplied().to_hex(),
        addr_bar_h = address::bar_height_px(theme, false),
        addr_css = address::addr_css(theme),
        muted = theme.text_muted().to_hex(),
        danger = theme.accent_danger().to_hex(),
        danger_raised = theme.accent_danger_raised().to_hex(),
        success = theme.accent_success().to_hex(),
        disabled_fill = theme.state_disabled_fill().to_hex(),
        disabled_border = theme.state_disabled_border().to_hex(),
        disabled_fg = theme.state_disabled_fg().to_hex(),
        img_error_icon = alert_icon_data_uri(
            tasty_icons::IMAGE.body,
            false,
            &theme.accent_danger().to_hex()
        ),
        alert_rules = alert_css(theme),
        hljs_rules = hljs_css(theme),
        find_hit_bg = theme.md_find_hit_bg().to_hex(),
        find_hit_fg = theme.md_find_hit_fg().to_hex(),
        find_current_bg = theme.md_find_current_bg().to_hex(),
        btn_sm_h = theme.item_height_tab.value(),
        btn_sm_pad_x = theme.spacing_sm.value(),
        btn_sm_font = theme.font_size_caption.value(),
        btn_radius = theme.button_radius().value(),
        btn_secondary_bg = theme.button_secondary_bg().to_hex(),
        btn_secondary_border = theme.button_secondary_border().to_hex(),
        btn_secondary_border_hover = theme.button_secondary_border_hover().to_hex(),
        btn_fg = theme.button_fg().to_hex(),
        btn_overlay_hover = theme.button_overlay_hover().unpremultiplied().to_hex(),
        btn_overlay_active = theme.button_overlay_active().unpremultiplied().to_hex(),
        find_current_fg = theme.md_find_current_fg().to_hex(),
    )
}

/// 구문 강조 클래스에 테마 색을 연결한다.
pub(super) fn hljs_css(theme: &Theme) -> String {
    format!(
        r#"code.hljs{{background:none;}}
.hljs-comment,.hljs-quote{{color:{comment};font-style:italic;}}
.hljs-meta{{color:{comment};}}
.hljs-keyword,.hljs-selector-tag,.hljs-subst,.hljs-operator{{color:{keyword};}}
.hljs-number,.hljs-literal{{color:{number};}}
.hljs-string,.hljs-doctag,.hljs-regexp,.hljs-link{{color:{string};}}
.hljs-title,.hljs-section,.hljs-selector-id,.hljs-title.function_{{color:{title};}}
.hljs-type,.hljs-class .hljs-title{{color:{type_};}}
.hljs-tag,.hljs-name,.hljs-attribute,.hljs-attr{{color:{tag};}}
.hljs-variable,.hljs-template-variable,.hljs-symbol,.hljs-bullet{{color:{variable};}}
.hljs-built_in,.hljs-builtin-name{{color:{builtin};}}
.hljs-deletion{{background:{deletion_bg};}}
.hljs-addition{{background:{addition_bg};}}
.hljs-emphasis{{font-style:italic;}}
.hljs-strong{{font-weight:700;}}
"#,
        comment = theme.text_muted().to_hex(),
        keyword = theme.mauve.to_hex(),
        number = theme.peach.to_hex(),
        string = theme.green.to_hex(),
        title = theme.blue.to_hex(),
        type_ = theme.yellow.to_hex(),
        tag = theme.teal.to_hex(),
        variable = theme.lavender.to_hex(),
        builtin = theme.red.to_hex(),
        deletion_bg = theme
            .accent_danger()
            .with_alpha(callout::tint_fill_alpha(theme))
            .to_hex(),
        addition_bg = theme
            .accent_success()
            .with_alpha(callout::tint_fill_alpha(theme))
            .to_hex(),
    )
}

/// Per-level heading pixel sizes, linearly interpolated between `font_size_prose_h1` (h1) and
/// `font_size_body` (h6) — real CSS, unlike the retired `egui_commonmark` renderer, can set
/// each level independently (module doc "library exceptions" this rewrite resolves).
pub(super) fn heading_sizes_px(theme: &Theme) -> [f32; 6] {
    let h1 = theme.font_size_prose_h1.value();
    let body = theme.font_size_body.value();
    let step = (h1 - body) / 5.0;
    [
        h1,
        h1 - step,
        h1 - 2.0 * step,
        h1 - 3.0 * step,
        h1 - 4.0 * step,
        body,
    ]
}

#[cfg(test)]
mod tests {
    use tasty_type_appearance::theme::Theme;

    use super::theme_css;

    fn mocha() -> Theme {
        Theme::with_colors_and_zoom(tasty_themes::mocha_fallback_colors(), false, 1.0)
    }

    /// 밝은 테마. raised 단계는 밝은 테마에서만 기본 강조색과 달라 둘을 구별할 수 있다.
    fn light() -> Theme {
        Theme::with_colors_and_zoom(tasty_themes::mocha_fallback_colors(), true, 1.0)
    }

    /// 한 선택자의 규칙 줄. 같은 선택자로 시작하는 줄이 하나여야 한다.
    fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
        let mut found = css.lines().filter(|l| l.starts_with(selector));
        let line = found.next().unwrap_or_else(|| panic!("{selector}: {css}"));
        assert!(found.next().is_none(), "{selector} twice");
        line
    }

    /// 포커스로 바뀌는 것은 문서 바탕뿐이다. 찾기 입력·복사 버튼은 bg_app, 주소 입력은 input-bg 다.
    #[test]
    fn the_controls_do_not_take_the_document_bed() {
        let theme = mocha();
        let css = theme_css(&theme);
        let control = theme.bg_app().to_hex();
        assert!(
            css.contains(&format!("--md-control-bg:{control};")),
            "{css}"
        );
        let input = theme.input_bg().to_hex();
        assert!(css.contains(&format!("--md-input-bg:{input};")), "{css}");
        let addr = rule(&css, "#tasty-addr-field{");
        assert!(addr.contains(&format!("background:{input};")), "{addr}");
    }

    #[test]
    fn links_read_the_raised_primary() {
        let theme = light();
        assert_ne!(theme.accent_primary_raised(), theme.accent_primary());
        let css = theme_css(&theme);
        let raised = theme.accent_primary_raised().to_hex();
        assert!(css.contains(&format!("--md-link:{raised};")), "{css}");
        assert!(rule(&css, "a{").contains("color:var(--md-link);"));
        assert!(rule(&css, "#tasty-toc-list a{").contains("color:var(--md-link);"));
    }

    #[test]
    fn a_quote_reads_its_own_ink_without_opacity() {
        let theme = mocha();
        let css = theme_css(&theme);
        let quote = theme.md_quote_fg().to_hex();
        assert!(css.contains(&format!("--md-quote-fg:{quote};")), "{css}");
        let line = rule(&css, "blockquote{");
        assert!(line.contains("color:var(--md-quote-fg);"), "{line}");
        assert!(!css.contains("opacity:0.9"), "{css}");
    }

    /// 콜아웃은 인용 요소지만 본문은 인용 글자가 아니라 text-primary다. 두 형태가 같다.
    #[test]
    fn a_callout_body_does_not_take_the_quote_ink() {
        let theme = mocha();
        let css = theme_css(&theme);
        let fg = theme.text_primary().to_hex();
        assert!(css.contains(&format!("--md-callout-fg:{fg};")), "{css}");
        let line = rule(
            &css,
            r#"blockquote[class^="markdown-alert-"],details[class^="markdown-alert-"]{"#,
        );
        assert!(line.contains("color:var(--md-callout-fg);"), "{line}");
    }

    #[test]
    fn a_missing_wiki_link_and_the_state_title_read_the_raised_danger() {
        let theme = light();
        assert_ne!(theme.accent_danger_raised(), theme.accent_danger());
        let css = theme_css(&theme);
        let raised = format!("color:{};", theme.accent_danger_raised().to_hex());
        assert!(rule(&css, ".tasty-wikilink-missing a{").contains(&raised));
        assert!(rule(&css, ".tasty-state-title{").contains(&raised));
    }

    #[test]
    fn table_header_and_cells_read_the_table_tokens() {
        let theme = mocha();
        let css = theme_css(&theme);
        assert_eq!(
            rule(&css, "th{"),
            format!(
                "th{{background:{};color:{};}}",
                theme.md_table_header_bg().to_hex(),
                theme.md_table_header_fg().to_hex()
            )
        );
        assert_eq!(
            rule(&css, "td{"),
            format!("td{{color:{};}}", theme.md_table_cell_fg().to_hex())
        );
    }
}
