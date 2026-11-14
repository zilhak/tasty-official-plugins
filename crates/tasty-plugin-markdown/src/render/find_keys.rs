//! 단축키 설정의 바인딩 문자열을 페이지 `keydown` 판정 값으로 바꾼다.
//! 토큰 규칙은 `docs/design/policies/key-mapping.md`를 따른다. `alt`는 macOS에서 Command,
//! `option`은 macOS Option·다른 OS의 Win·Super다. modifier가 있는 조합은 host와 같이
//! US 물리 키로 맞추므로 `KeyboardEvent.code`와 비교한다.

use serde::Serialize;

/// 페이지가 `KeyboardEvent`와 비교하는 한 조합. modifier는 정확히 같아야 한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct DomKeyMatcher {
    pub(super) code: String,
    pub(super) ctrl: bool,
    pub(super) alt: bool,
    pub(super) meta: bool,
    pub(super) shift: bool,
    /// 입력칸에서 글자 입력·편집으로 쓰이는 조합이다. 페이지는 입력칸이나 IME 조합 중에는
    /// 이 조합을 찾기로 가로채지 않는다.
    pub(super) typing: bool,
}

/// 바인딩 목록을 판정 값의 JSON 배열로 만든다. 읽을 수 없는 바인딩은 뺀다.
pub(super) fn dom_matchers_json(bindings: &[String]) -> String {
    let matchers: Vec<DomKeyMatcher> = bindings
        .iter()
        .filter_map(|b| dom_matcher(b, cfg!(target_os = "macos")))
        .collect();
    serde_json::to_string(&matchers).unwrap_or_else(|_| "[]".to_string())
}

/// 한 바인딩을 판정 값으로 바꾼다. 설정이 저장한 조합은 modifier 유무와 관계없이 모두 받는다.
/// ctrl·alt·option이 없고 기능 키(F1~F24)가 아닌 조합은 `typing`으로 표시한다.
pub(super) fn dom_matcher(binding: &str, macos: bool) -> Option<DomKeyMatcher> {
    let lower = binding.trim().to_ascii_lowercase();
    let mut rest = lower.as_str();
    let (mut ctrl, mut shift, mut alt, mut option) = (false, false, false, false);
    loop {
        if let Some(r) = rest.strip_prefix("ctrl+") {
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("shift+") {
            shift = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("alt+") {
            alt = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("option+") {
            option = true;
            rest = r;
        } else {
            break;
        }
    }
    let code = key_code(rest)?;
    let typing = !(ctrl || alt || option || is_function_key(&code));
    let (alt_key, meta_key) = if macos { (option, alt) } else { (alt, option) };
    Some(DomKeyMatcher {
        code,
        ctrl,
        alt: alt_key,
        meta: meta_key,
        shift,
        typing,
    })
}

/// `F1`~`F24`. 입력칸에서 글자나 편집 동작을 만들지 않는다.
fn is_function_key(code: &str) -> bool {
    code.strip_prefix('F')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// 키 토큰의 US 배열 `KeyboardEvent.code`.
fn key_code(token: &str) -> Option<String> {
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' => Some(format!("Key{}", c.to_ascii_uppercase())),
            '0'..='9' => Some(format!("Digit{c}")),
            ',' => Some("Comma".into()),
            '.' => Some("Period".into()),
            '/' => Some("Slash".into()),
            ';' => Some("Semicolon".into()),
            '\'' => Some("Quote".into()),
            '[' => Some("BracketLeft".into()),
            ']' => Some("BracketRight".into()),
            '\\' => Some("Backslash".into()),
            '-' => Some("Minus".into()),
            '=' | '+' => Some("Equal".into()),
            '`' => Some("Backquote".into()),
            _ => None,
        };
    }
    let named = match token {
        "minus" => "Minus",
        "equals" | "plus" => "Equal",
        "tab" => "Tab",
        "space" => "Space",
        "enter" => "Enter",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "escape" => "Escape",
        _ => {
            let n: u8 = token.strip_prefix('f')?.parse().ok()?;
            return (1..=24).contains(&n).then(|| format!("F{n}"));
        }
    };
    Some(named.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(code: &str, ctrl: bool, alt: bool, meta: bool, shift: bool) -> DomKeyMatcher {
        DomKeyMatcher {
            code: code.to_string(),
            ctrl,
            alt,
            meta,
            shift,
            typing: false,
        }
    }

    #[test]
    fn default_find_bindings_keep_ctrl_f_and_command_f_on_macos() {
        assert_eq!(
            dom_matcher("ctrl+f", true),
            Some(m("KeyF", true, false, false, false))
        );
        assert_eq!(
            dom_matcher("alt+f", true),
            Some(m("KeyF", false, false, true, false))
        );
    }

    #[test]
    fn alt_and_option_tokens_follow_key_position_outside_macos() {
        assert_eq!(
            dom_matcher("alt+f", false),
            Some(m("KeyF", false, true, false, false))
        );
        assert_eq!(
            dom_matcher("option+k", false),
            Some(m("KeyK", false, false, true, false))
        );
        assert_eq!(
            dom_matcher("option+k", true),
            Some(m("KeyK", false, true, false, false))
        );
    }

    #[test]
    fn rebound_find_combo_is_parsed_and_the_old_key_is_gone() {
        let json = dom_matchers_json(&["ctrl+shift+g".to_string()]);
        assert_eq!(
            json,
            r#"[{"code":"KeyG","ctrl":true,"alt":false,"meta":false,"shift":true,"typing":false}]"#
        );
        assert!(!json.contains("KeyF"), "got: {json}");
    }

    #[test]
    fn function_keys_without_modifiers_are_kept() {
        assert_eq!(
            dom_matcher("f3", false),
            Some(m("F3", false, false, false, false))
        );
        assert_eq!(
            dom_matcher("shift+f3", true),
            Some(m("F3", false, false, false, true))
        );
    }

    #[test]
    fn plain_text_keys_are_kept_and_marked_as_typing() {
        let typing = |code: &str, shift: bool| DomKeyMatcher {
            typing: true,
            ..m(code, false, false, false, shift)
        };
        assert_eq!(dom_matcher("f", false), Some(typing("KeyF", false)));
        assert_eq!(dom_matcher("shift+f", false), Some(typing("KeyF", true)));
        assert_eq!(dom_matcher("enter", false), Some(typing("Enter", false)));
        assert_eq!(dom_matcher("ctrl+f", false).map(|x| x.typing), Some(false));
    }

    #[test]
    fn named_and_symbol_keys_map_to_us_codes() {
        assert_eq!(
            dom_matcher("ctrl+f3", false).map(|x| x.code),
            Some("F3".into())
        );
        assert_eq!(
            dom_matcher("ctrl+/", false).map(|x| x.code),
            Some("Slash".into())
        );
        assert_eq!(
            dom_matcher("ctrl+pageup", false).map(|x| x.code),
            Some("PageUp".into())
        );
        assert_eq!(dom_matcher("ctrl+f25", false), None);
        assert_eq!(dom_matcher("ctrl+", false), None);
    }

    #[test]
    fn an_empty_binding_list_opens_nothing() {
        assert_eq!(dom_matchers_json(&[]), "[]");
    }
}
