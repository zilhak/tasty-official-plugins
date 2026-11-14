//! HTML 미리보기의 태그 깊이에 맞춰 들여쓰기를 추가한다.
//! DOM 구성·렌더링·보안 정제는 하지 않는 단순한 토크나이저다.
//! 태그 사이 공백을 줄이고 script/style/pre 블록은 원문 그대로 되돌려 넣는다.

/// 깊이 증가 없이 처리되는 void element(닫는 태그가 없는 표준 HTML 요소).
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// 원본 그대로 보존할 태그(재포맷 시 의미가 깨질 수 있는 영역).
const VERBATIM_TAGS: &[&str] = &["script", "style", "pre"];

/// 원문 보존 블록을 바꿔 넣을 표식. 입력에 같은 문자가 있으면 충돌할 수 있다.
const MARK: char = '\u{E000}';

/// 태그 깊이에 맞춰 들여쓴다. 잘못된 HTML도 가능한 부분까지 처리한다.
pub(crate) fn prettify(src: &str) -> String {
    let (masked, verbatim) = extract_verbatim_blocks(src);
    let collapsed = collapse_intertag_whitespace(&masked);
    let parts: Vec<String> = split_tags(&collapsed)
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();

    let mut depth: usize = 0;
    let mut lines = Vec::with_capacity(parts.len());
    for p in &parts {
        if p.starts_with("</") {
            depth = depth.saturating_sub(1);
        }
        lines.push(format!("{}{}", "  ".repeat(depth), p.trim()));
        if is_opening_tag(p) {
            depth += 1;
        }
    }
    restore_verbatim_blocks(&lines.join("\n"), &verbatim)
}

/// 태그 사이 공백을 없앤다(>\s+< → ><).
fn collapse_intertag_whitespace(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        out.push(chars[i]);
        if chars[i] == '>' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j > i + 1 && j < chars.len() && chars[j] == '<' {
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// 태그와 일반 텍스트를 나눈다. 내부가 빈 <>는 태그로 보지 않는다.
fn split_tags(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut parts = Vec::new();
    let mut cur_text = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '>')
            && rel >= 1
        {
            if !cur_text.is_empty() {
                parts.push(std::mem::take(&mut cur_text));
            }
            let end = i + 1 + rel;
            parts.push(chars[i..=end].iter().collect());
            i = end + 1;
            continue;
        }
        cur_text.push(chars[i]);
        i += 1;
    }
    if !cur_text.is_empty() {
        parts.push(cur_text);
    }
    parts
}

/// 닫는 태그·자체 종료 태그·void 요소를 제외한 여는 태그인지 확인한다.
fn is_opening_tag(p: &str) -> bool {
    let mut chars = p.chars();
    if chars.next() != Some('<') {
        return false;
    }
    match chars.next() {
        Some('/') | Some('!') | None => return false,
        _ => {}
    }
    if p.ends_with("/>") {
        return false;
    }
    match tag_name(p) {
        Some(name) => !VOID_ELEMENTS.contains(&name.to_ascii_lowercase().as_str()),
        // 이름을 찾지 못해도 void 요소로 보지 않아 깊이를 늘린다.
        None => true,
    }
}

/// < 다음의 영숫자·하이픈으로 된 태그 이름을 읽는다.
fn tag_name(p: &str) -> Option<&str> {
    let rest = &p[1..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 { None } else { Some(&rest[..end]) }
}

/// `<script>`/`<style>`/`<pre>` 완전한 span(여는 태그~대응하는 닫는 태그)을 원문
/// 그대로 추출해 placeholder 로 치환한다. 반환값: (치환된 문자열, 추출된 블록 목록).
fn extract_verbatim_blocks(src: &str) -> (String, Vec<String>) {
    let chars: Vec<char> = src.chars().collect();
    let mut blocks: Vec<String> = Vec::new();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<'
            && let Some(tag) = matching_verbatim_open(&chars, i)
            && let Some(open_end_rel) = chars[i..].iter().position(|&c| c == '>')
        {
            let open_end = i + open_end_rel;
            let close_needle: Vec<char> = format!("</{tag}").chars().collect();
            if let Some(close_start) = find_ci(&chars, &close_needle, open_end + 1)
                && let Some(close_end_rel) = chars[close_start..].iter().position(|&c| c == '>')
            {
                let close_end = close_start + close_end_rel;
                let block: String = chars[i..=close_end].iter().collect();
                let idx = blocks.len();
                blocks.push(block);
                out.push(MARK);
                out.push_str(&idx.to_string());
                out.push(MARK);
                i = close_end + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, blocks)
}

/// `at` 위치가 `<script`/`<style`/`<pre` (대소문자 무시) 로 시작하고 태그 이름 경계가
/// 유효하면(다음 문자가 공백/`>`/`/`) 그 태그 이름을 반환.
fn matching_verbatim_open(chars: &[char], at: usize) -> Option<&'static str> {
    for &tag in VERBATIM_TAGS {
        let tag_chars: Vec<char> = tag.chars().collect();
        let end = at + 1 + tag_chars.len();
        if end > chars.len() {
            continue;
        }
        let candidate = &chars[at + 1..end];
        let matches = candidate
            .iter()
            .zip(tag_chars.iter())
            .all(|(&c, &t)| c.eq_ignore_ascii_case(&t));
        if !matches {
            continue;
        }
        let boundary_ok = match chars.get(end) {
            Some(&c) => c.is_whitespace() || c == '>' || c == '/',
            None => true,
        };
        if boundary_ok {
            return Some(tag);
        }
    }
    None
}

/// `needle` 을 `from` 이후에서 대소문자 무시 검색.
fn find_ci(chars: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= chars.len() {
        return None;
    }
    let last_start = chars.len().checked_sub(needle.len())?;
    if from > last_start {
        return None;
    }
    (from..=last_start).find(|&start| {
        chars[start..start + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(&c, &n)| c.eq_ignore_ascii_case(&n))
    })
}

fn restore_verbatim_blocks(text: &str, blocks: &[String]) -> String {
    let mut out = text.to_string();
    for (i, block) in blocks.iter().enumerate() {
        let marker = format!("{MARK}{i}{MARK}");
        out = out.replace(&marker, block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prettify_simple_nested_tags() {
        let input = "<div><p>hi</p></div>";
        let out = prettify(input);
        assert!(out.contains('\n')); // 최소한 줄바꿈이 들어감
    }

    #[test]
    fn prettify_does_not_panic_on_malformed_input() {
        let input = "<div><p>unterminated";
        let _ = prettify(input); // panic 없이 반환되어야 함
    }

    #[test]
    fn prettify_preserves_script_style_content_verbatim() {
        let input = "<script>if (a < b) { f(); }</script>";
        let out = prettify(input);
        assert!(out.contains("if (a < b) { f(); }")); // 내부의 `<`를 태그로 오인해 깨면 안 됨
    }

    #[test]
    fn prettify_is_idempotent() {
        let input = "<div><p>hi</p></div>";
        let once = prettify(input);
        let twice = prettify(&once);
        assert_eq!(once, twice); // 이미 정돈된 결과를 다시 넣어도 결과가 흔들리면 안 됨
    }

    #[test]
    fn prettify_preserves_comment_verbatim_without_shifting_depth() {
        let input = "<div><!-- note --><p>hi</p></div>";
        let out = prettify(input);
        assert!(out.contains("<!-- note -->"));
        let lines: Vec<&str> = out.lines().collect();
        // <p> 는 주석과 무관하게 <div> 바로 아래(depth 1)에 와야 한다.
        let p_line = lines.iter().find(|l| l.trim_start().starts_with("<p>"));
        assert_eq!(p_line, Some(&"  <p>"));
    }

    #[test]
    fn prettify_handles_doctype_without_incrementing_depth() {
        let input = "<!DOCTYPE html><html><body>hi</body></html>";
        let out = prettify(input);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "<!DOCTYPE html>");
        // <html> 도 depth 0 에서 시작해야 한다(doctype 가 depth 를 올리면 안 됨).
        assert_eq!(lines[1], "<html>");
    }

    #[test]
    fn prettify_void_elements_do_not_increase_depth() {
        let input = "<div><img src=\"x\"><p>hi</p></div>";
        let out = prettify(input);
        let lines: Vec<&str> = out.lines().collect();
        let img_line = lines.iter().find(|l| l.contains("<img")).expect("img line");
        let p_line = lines
            .iter()
            .find(|l| l.trim_start() == "<p>")
            .expect("p line");
        // img 와 p 는 <div> 바로 아래 같은 depth(들여쓰기 폭 동일)여야 한다.
        let img_indent = img_line.len() - img_line.trim_start().len();
        let p_indent = p_line.len() - p_line.trim_start().len();
        assert_eq!(img_indent, p_indent);
    }

    #[test]
    fn prettify_self_closing_tags_do_not_increase_depth() {
        let input = "<div><br/><p>hi</p></div>";
        let out = prettify(input);
        let lines: Vec<&str> = out.lines().collect();
        let br_line = lines.iter().find(|l| l.contains("<br")).expect("br line");
        let p_line = lines
            .iter()
            .find(|l| l.trim_start() == "<p>")
            .expect("p line");
        let br_indent = br_line.len() - br_line.trim_start().len();
        let p_indent = p_line.len() - p_line.trim_start().len();
        assert_eq!(br_indent, p_indent);
    }

    #[test]
    fn prettify_preserves_multiline_script_verbatim_at_correct_depth() {
        // script 내부는 들여쓰기를 다시 하지 않는다.
        let input = "<div>\n  <script>\n    var x = 1;\n  </script>\n</div>";
        let out = prettify(input);
        assert!(out.contains("var x = 1;"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "<div>");
        assert_eq!(lines[1], "  <script>");
        assert_eq!(lines[2], "    var x = 1;");
        assert_eq!(lines[3], "  </script>");
        assert_eq!(lines[4], "</div>");
    }
}
