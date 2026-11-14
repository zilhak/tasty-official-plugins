//! 바탕 포커스 상태에 따라 바뀌는 문서 값. 호스트가 `:root`에 `--tasty-bed-focused`를 정의하면 focus
//! 값이고, 정의하지 않으면 unfocus 값이다. 문서는 두 상태의 값을 함께 가지므로 포커스가 바뀌어도
//! 다시 싣지 않는다. 계약은 호스트 문서 `docs/design/systems/webview.md`의 "바탕 포커스 상태" 절에 있다.

use tasty_type_appearance::color::HexColor;
use tasty_type_appearance::theme::Theme;

/// 호스트가 바탕 포커스 상태인 동안 빈 값으로 정의하는 속성.
pub(super) use tasty_plugin_protocol::bed_focus::BED_FOCUSED_PROPERTY;

/// 상태로 바뀌는 값 셋. 바탕, 표 본문 칸, 표 zebra 줄이다. 나머지 문서 값은 포커스로 바뀌지 않는다.
fn pairs(theme: &Theme) -> [(&'static str, HexColor, HexColor); 3] {
    let (bg, bg_unfocused) = doc_bg(theme);
    [
        ("--md-bg", bg, bg_unfocused),
        (
            "--md-row",
            theme.md_table_row_bg(),
            theme.md_table_row_bg_unfocused(),
        ),
        (
            "--md-zebra",
            theme.md_table_row_bg_zebra(),
            theme.md_table_row_bg_zebra_unfocused(),
        ),
    ]
}

/// 문서 바탕 쌍(토큰 md-doc-bg = surface-markdown-focused-bg와 그 unfocused 짝).
/// `surface_themes`를 싣지 않는 이전 호스트에는 markdown 항목이 없어 두 상태 모두 bg_app이다.
/// `Theme::surface`의 대체 색은 검정이라 쓰지 않는다.
pub(super) fn doc_bg(theme: &Theme) -> (HexColor, HexColor) {
    theme.surface_themes.get("markdown").map_or_else(
        || (theme.bg_app(), theme.bg_app()),
        |surface| (surface.focused_bg, surface.unfocused_bg),
    )
}

/// 호스트가 이 OS에서 표지를 넣는지. 넣지 않는 OS는 전처럼 focus 값으로 그린다.
const HOST_SENDS_MARKER: bool = cfg!(target_os = "linux");

/// `:root` 안에 넣을 선언. 이름마다 표지가 있을 때만 유효한 `-on` 값과, 그것이 무효일 때
/// unfocus 값을 쓰는 본 변수를 둔다.
pub(super) fn state_vars(theme: &Theme) -> String {
    state_vars_for(theme, HOST_SENDS_MARKER)
}

fn state_vars_for(theme: &Theme, host_sends_marker: bool) -> String {
    let mut css: String = pairs(theme)
        .iter()
        .map(|(name, focused, unfocused)| {
            format!(
                "{name}-on:var({BED_FOCUSED_PROPERTY}) {};\n{name}:var({name}-on, {});\n",
                focused.to_hex(),
                unfocused.to_hex()
            )
        })
        .collect();
    if !host_sends_marker {
        css.push_str(&format!("{BED_FOCUSED_PROPERTY}: ;\n"));
    }
    css
}

#[cfg(test)]
mod tests {
    use tasty_type_appearance::color::HexColor;
    use tasty_type_appearance::theme::Theme;

    use super::{doc_bg, state_vars_for};
    use crate::render::theme_css;

    fn mocha() -> Theme {
        Theme::with_colors_and_zoom(tasty_themes::mocha_fallback_colors(), false, 1.0)
    }

    fn declared(css: &str, name: &str, focused: HexColor, unfocused: HexColor) -> bool {
        css.contains(&format!(
            "{name}-on:var(--tasty-bed-focused) {};\n{name}:var({name}-on, {});\n",
            focused.to_hex(),
            unfocused.to_hex()
        ))
    }

    /// 바탕·row·zebra 세 값만 두 상태를 가진다. 값은 디자이너 표의 토큰이다.
    #[test]
    fn the_three_state_values_carry_both_tokens() {
        let theme = mocha();
        let css = theme_css(&theme);
        let md = theme.surface("markdown");
        assert!(
            declared(&css, "--md-bg", md.focused_bg, md.unfocused_bg),
            "{css}"
        );
        assert!(declared(
            &css,
            "--md-row",
            theme.md_table_row_bg(),
            theme.md_table_row_bg_unfocused()
        ));
        assert!(declared(
            &css,
            "--md-zebra",
            theme.md_table_row_bg_zebra(),
            theme.md_table_row_bg_zebra_unfocused()
        ));
        assert_eq!(css.matches("var(--tasty-bed-focused)").count(), 3, "{css}");
        // 호스트가 표지를 보내는 분기로 잰다. 보내지 않는 OS의 표지 선언 줄은 아래 시험이 따로 잰다.
        assert_eq!(state_vars_for(&theme, true).lines().count(), 6);
    }

    #[test]
    fn the_body_table_and_even_rows_paint_the_state_values() {
        let css = theme_css(&mocha());
        assert_eq!(css.matches("background:var(--md-bg)").count(), 1, "{css}");
        assert!(css.contains("table{border-collapse:collapse;background:var(--md-row);}"));
        assert!(css.contains("tr:nth-child(even){background:var(--md-zebra);}"));
    }

    /// 사용자 테마가 `[surfaces.markdown]`에서 두 값을 같게 두어도 그대로 쓴다.
    #[test]
    fn a_user_theme_with_equal_values_is_used_as_is() {
        let mut colors = tasty_themes::mocha_fallback_colors();
        let same = HexColor::from_hex("#203040").expect("valid hex");
        let md = colors
            .surface_themes
            .get_mut("markdown")
            .expect("markdown surface in the fallback theme");
        md.focused_bg = same;
        md.unfocused_bg = same;
        let theme = Theme::with_colors_and_zoom(colors, false, 1.0);
        assert_eq!(doc_bg(&theme), (same, same));
        assert!(declared(&theme_css(&theme), "--md-bg", same, same));
    }

    #[test]
    fn a_host_without_surface_colors_keeps_the_app_background() {
        // surface_themes 를 싣지 않는 이전 호스트의 ThemeWire.
        let mut colors = tasty_themes::mocha_fallback_colors();
        colors.surface_themes.clear();
        let theme = Theme::with_colors_and_zoom(colors, false, 1.0);
        assert_eq!(doc_bg(&theme), (theme.bg_app(), theme.bg_app()));
    }

    #[test]
    fn the_theme_wire_carries_the_markdown_surface_colors() {
        // 호스트가 보내는 ThemeWire(JSON)를 플러그인이 받아도 markdown surface 색이 남는다.
        let mut colors = tasty_themes::mocha_fallback_colors();
        let focused = HexColor::from_hex("#203040").expect("valid hex");
        let unfocused = HexColor::from_hex("#405060").expect("valid hex");
        let md = colors
            .surface_themes
            .get_mut("markdown")
            .expect("markdown surface");
        md.focused_bg = focused;
        md.unfocused_bg = unfocused;
        let host = Theme::with_colors_and_zoom(colors, false, 1.0);
        let wire = tasty_plugin_protocol::ThemeWire {
            colors: host.to_colors(),
            is_light: host.is_light,
            ui_zoom: 1.0,
        };
        let json = serde_json::to_string(&wire).unwrap();
        let back: tasty_plugin_protocol::ThemeWire = serde_json::from_str(&json).unwrap();
        let plugin = Theme::with_colors_and_zoom(back.colors, back.is_light, back.ui_zoom);
        assert_eq!(doc_bg(&plugin), (focused, unfocused));
    }

    /// 표지를 넣지 않는 OS의 문서는 자기 선언으로 늘 focus 값이다. 표지를 넣는 OS의 문서에는 선언이 없다.
    #[test]
    fn the_page_declares_the_marker_only_where_the_host_sends_none() {
        let theme = mocha();
        let without_host = state_vars_for(&theme, false);
        let with_host = state_vars_for(&theme, true);
        assert!(without_host.ends_with("--tasty-bed-focused: ;\n"));
        assert_eq!(without_host.matches("--tasty-bed-focused:").count(), 1);
        assert!(!with_host.contains("--tasty-bed-focused:"));
        assert_eq!(without_host, format!("{with_host}--tasty-bed-focused: ;\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_page_leaves_the_marker_to_the_host() {
        let theme = mocha();
        assert_eq!(super::state_vars(&theme), state_vars_for(&theme, true));
        assert!(!theme_css(&theme).contains("--tasty-bed-focused:"));
    }
}
