//! Finds Hangul, kana, and CJK ideograph strings written directly in the shipped code of
//! the plugins. Text for users and agents comes from the translation files (`lang/`).
//! The check cannot judge English literals or the real delivery path, and whether a hit
//! is text to translate is decided from its context.
//!
//! Test-only files and spans found from their declarations, and lines classed as
//! diagnostic calls, are skipped. A diagnostic call is found by name and its line range
//! by counting parentheses, so another string on the same line can be skipped too.
//! Strings are read line by line and lines starting with // are skipped. Multi-line raw
//! strings and block comments are not fully parsed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use plugin_guards::{plugin_crates, repo_root};
use tasty_doc_guards::cfg_predicate as cfg_span;
use tasty_doc_guards::source_text::{is_locale_specific, repo_relative, rust_sources};

/// Plugin crates with a manifest and a src directory: 8 measured on 2026-11-14 at
/// 7f9684b. One fewer is allowed so that removing a plugin is not mistaken for an empty
/// walk; fewer than that means the walk is broken.
const MIN_PLUGINS: usize = 7;

/// Shipped source files scanned: 107 measured on 2026-11-14 at 7f9684b, with 27 more
/// left out as test-only. The floor leaves about a quarter for files being merged or
/// moved and still fails when the walk loses the larger plugins.
const MIN_FILES: usize = 80;

/// Search strings of the diagnostic calls the check skips.
const DIAGNOSTIC_MARKERS: &[&str] = &[
    "assert",
    "panic!",
    "unreachable!",
    ".expect(",
    "tracing::",
    "warn!",
    "info!",
    "error!",
    "debug!",
    "trace!",
];

/// Parenthesis balance of one line, leaving out plain string literals, char literals,
/// and // comments. Not a full Rust lexer.
fn paren_delta(line: &str) -> i32 {
    let b = line.as_bytes();
    let mut depth = 0i32;
    let mut i = 0usize;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            break;
        }
        if c == b'"' {
            in_str = true;
        } else if c == b'\'' {
            let esc = i + 1 < b.len() && b[i + 1] == b'\\';
            let end = if esc { i + 3 } else { i + 2 };
            if end < b.len() && b[end] == b'\'' {
                i = end + 1;
                continue;
            }
        } else if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
        }
        i += 1;
    }
    depth
}

/// Marks the lines from a diagnostic search string to the line where its parentheses close.
fn diagnostic_lines(lines: &[&str]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        if !DIAGNOSTIC_MARKERS.iter().any(|m| lines[i].contains(m)) {
            i += 1;
            continue;
        }
        out[i] = true;
        let mut depth = paren_delta(lines[i]);
        let mut j = i;
        while depth > 0 && j + 1 < lines.len() {
            j += 1;
            out[j] = true;
            depth += paren_delta(lines[j]);
        }
        i = j + 1;
    }
    out
}

fn locale_specific_literals(src: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = src.lines().collect();
    let gated = cfg_span::cfg_gated_lines(&lines, "test");
    let diagnostic = diagnostic_lines(&lines);
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if gated[idx] || diagnostic[idx] {
            continue;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        for lit in string_literals(line) {
            if lit.chars().any(is_locale_specific) {
                out.push((idx + 1, lit));
            }
        }
    }
    out
}

fn string_literals(line: &str) -> Vec<String> {
    let b: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != '"' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut cur = String::new();
        while j < b.len() {
            if b[j] == '\\' {
                j += 2;
                continue;
            }
            if b[j] == '"' {
                break;
            }
            cur.push(b[j]);
            j += 1;
        }
        if j >= b.len() {
            break;
        }
        out.push(cur);
        i = j + 1;
    }
    out
}

fn plugin_srcs() -> Vec<PathBuf> {
    plugin_crates()
        .into_iter()
        .map(|(_, dir)| dir.join("src"))
        .filter(|src| src.is_dir())
        .collect()
}

fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Test-only files of the repository, from the shared module declaration reading. The
/// whole of crates/ is read so that a module declared from another file is found.
fn test_only_files(root: &Path) -> BTreeSet<PathBuf> {
    tasty_doc_guards::shipping_scope::test_only_files(root, &rust_sources(root, &["crates"]))
}

#[test]
fn no_plugin_ships_a_locale_specific_literal() {
    let srcs = plugin_srcs();
    assert!(
        srcs.len() >= MIN_PLUGINS,
        "found only {} plugins with a manifest (floor {MIN_PLUGINS}, 8 measured on 2026-11-14). Check the walk.",
        srcs.len()
    );
    let root = repo_root();
    let test_only = test_only_files(&root);
    let mut files = 0usize;
    let mut skipped = 0usize;
    let mut scanned: Vec<PathBuf> = Vec::new();
    let mut hits: Vec<String> = Vec::new();
    for dir in &srcs {
        for f in rs_files(dir) {
            // The relative path must match the shared reader's, Windows separators included,
            // for test-only files to be left out.
            let rel = repo_relative(f.strip_prefix(&root).unwrap_or(&f));
            if test_only.contains(&rel) {
                skipped += 1;
                continue;
            }
            scanned.push(rel.clone());
            files += 1;
            let src = std::fs::read_to_string(&f)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", f.display()))
                .replace("\r\n", "\n");
            let rel = rel.to_string_lossy().to_string();
            for (line, lit) in locale_specific_literals(&src) {
                let shown: String = lit.chars().take(60).collect();
                hits.push(format!("{rel}:{line}  {shown}"));
            }
        }
    }
    assert!(
        files >= MIN_FILES,
        "collected only {files} plugin source files (floor {MIN_FILES}, {skipped} test-only left out). Check the paths and the walk."
    );
    // The test-only files must not reach the list that is checked.
    let leaked: Vec<String> = scanned
        .iter()
        .filter(|p| test_only.contains(*p))
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert!(
        leaked.is_empty(),
        "files declared test-only were checked as shipped code:\n  {}",
        leaked.join("\n  ")
    );
    assert!(
        hits.is_empty(),
        "plugin code holds CJK strings written directly. Text for users or agents goes in lang/{{en,ko,ja,zh-Hans}}.toml under a key and is read with Translator::t. Check also that the hits are really such text:\n  {}",
        hits.join("\n  ")
    );
}

#[test]
fn the_detector_skips_diagnostics_comments_and_tests() {
    let src = "\
// 주석 안의 \"한국어 문구\" 는 대상이 아니다
let msg = \"완료했습니다\";
tracing::warn!(\"경고 문구\");
assert!(x, \"단정 문구\");
let ok = \"plain english\";
#[cfg(test)]
mod tests {
    const T: &str = \"테스트 안의 문구\";
}
";
    let hits = locale_specific_literals(src);
    let lits: BTreeSet<&str> = hits.iter().map(|(_, l)| l.as_str()).collect();
    assert!(
        lits.contains("완료했습니다"),
        "missed the delivered text: {lits:?}"
    );
    assert!(
        !lits.contains("경고 문구") && !lits.contains("단정 문구"),
        "took a developer surface: {lits:?}"
    );
    assert!(
        !lits.contains("한국어 문구"),
        "took text inside a comment: {lits:?}"
    );
    assert!(
        !lits.contains("테스트 안의 문구"),
        "took text after `#[cfg(test)]`: {lits:?}"
    );
    assert!(
        !lits.contains("plain english"),
        "took English too: {lits:?}"
    );
}

#[test]
fn production_code_after_a_test_helper_is_still_scanned() {
    let src = "\
#[cfg(test)]
fn helper() -> &'static str { \"테스트 헬퍼 문구\" }

fn shipped() { notify(\"배포되는 문구\"); }
";
    let lits: BTreeSet<String> = locale_specific_literals(src)
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    assert!(
        lits.contains("배포되는 문구"),
        "missed the shipped text after a test helper; the scan stopped at the first `#[cfg(test)]`: {lits:?}"
    );
    assert!(
        !lits.contains("테스트 헬퍼 문구"),
        "took the gated helper: {lits:?}"
    );
}

#[test]
fn a_wrapped_assert_message_is_still_a_diagnostic() {
    let src = "\
fn f() {
    assert!(
        cond,
        \"삭제가 error 상태로 감지되어야 한다\"
    );
    notify(\"사용자에게 나가는 문구\");
}
";
    let lits: BTreeSet<String> = locale_specific_literals(src)
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    assert!(
        !lits.contains("삭제가 error 상태로 감지되어야 한다"),
        "read a wrapped assert message as delivered text: {lits:?}"
    );
    assert!(
        lits.contains("사용자에게 나가는 문구"),
        "skipped text after the diagnostic call ended: {lits:?}"
    );
}

#[test]
fn an_escaped_quote_does_not_split_the_literal() {
    let hits = locale_specific_literals("let s = \"그는 \\\"안녕\\\" 이라 했다\";");
    assert_eq!(hits.len(), 1, "the literal was split: {hits:?}");
}
