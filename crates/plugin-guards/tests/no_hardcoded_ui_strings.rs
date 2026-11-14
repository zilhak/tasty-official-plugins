//! Finds user-facing strings written directly in the plugin sources. Text for users comes
//! from the translation files (`lang/`).
//!
//! The check reads call forms and character patterns, so it does not find every
//! user-facing string.
//! - W: in egui widget arguments, two ASCII letters in a row or CJK.
//! - H: hint_text allows example identifiers and finds sentences (a space and an English
//!   word of four or more letters) and CJK. For the color helper
//!   `tasty_egui_theme::hint_text(theme, ..)` the second argument is read.
//! - N/F/E: in native menu calls, unwrap_or fallbacks, and print macros, finds sentences,
//!   capitalized English words, and CJK. One lower-case word may be an identifier and is
//!   allowed, but it must be translated if it is really shown.
//!
//! Key names, language names, a font sample, units, and product names are allowed tokens.
//! Tracing logs are not among the calls read, and test items and comment lines are skipped.
//! A known violation not yet fixed is listed in PENDING_FIX_LITERALS with how to fix it,
//! and the entry is removed once the literal is gone.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tasty_doc_guards::floored_walk::normalized_rel;

/// Tokens allowed as they are: proper nouns and identifiers whose meaning changes when
/// translated.
const LITERAL_TOKEN_ALLOWLIST: &[&str] = &[
    // An IPC type identifier of the host's empty surface, not display text.
    "Empty",
    // Modifier and key names
    "Ctrl",
    "Alt",
    "Shift",
    "Cmd",
    "Command",
    "Option",
    "Super",
    "Meta",
    "Win",
    "Fn",
    "Esc",
    "Escape",
    "Enter",
    "Return",
    "Tab",
    "Space",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    // Font preview and language names
    "AaBbCcDdEeFfGg",
    "English",
    "한국어",
    "日本語",
    "简体中文",
    // Units
    "KiB",
    "MiB",
    "GiB",
    "KB",
    "MB",
    "GB",
    "ms",
    "px",
    "fps",
    "Hz",
    // Product name
    "Tasty",
    // Protocol token: the HTTP 403 reason phrase the Windows webview returns for a
    // blocked remote load.
    "Blocked",
    // A Windows clipboard format name passed to RegisterClipboardFormatW; it must equal
    // the one Explorer uses.
    "Preferred DropEffect",
];

/// Known violations left to fix, as (file, literal, how to fix). While listed they do not
/// fail, but an entry whose literal is gone fails so that it is removed. A new violation
/// is fixed, not listed.
const PENDING_FIX_LITERALS: &[(&str, &str, &str)] = &[(
    "crates/tasty-plugin-git-viewer/src/main.rs",
    "remote git query failed",
    "shown in the plugin error panel (self.error); replace with a Translator key",
)];

/// Source files scanned: 122 measured on 2026-11-14 at 79b94f5. The floor leaves about a
/// quarter for files being merged or moved and still fails when the walk loses the larger
/// plugins.
const MIN_SCANNED_FILES: usize = 90;

const PRUNE_DIRS: &[&str] = &["target", "dist", ".worktree", ".git", "node_modules"];

/// Local folder names left out of the walk, put together from pieces.
const LOCAL_HEAD: &str = "claude";
const LOCAL_TAIL: &str = "-workspace";

fn is_pruned(name: &str) -> bool {
    PRUNE_DIRS.contains(&name)
        || name
            .strip_prefix('.')
            .is_some_and(|rest| rest == LOCAL_HEAD || rest == format!("{LOCAL_HEAD}{LOCAL_TAIL}"))
}

/// Single-argument widget calls; the literal after them is judged strictly (W).
const WIDGET_CALLS: &[&str] = &[
    "ui.label(",
    "ui.button(",
    "ui.heading(",
    "ui.small(",
    "ui.strong(",
    "ui.weak(",
    "ui.monospace(",
    "ui.code(",
    "ui.small_button(",
    "ui.link(",
    "RichText::new(",
    "Button::new(",
    "Label::new(",
    "CollapsingHeader::new(",
    ".on_hover_text(",
    ".on_disabled_hover_text(",
];

/// OS native calls; the literal after them is judged allowing identifiers (N).
const NATIVE_CALLS: &[&str] = &[
    "NSString::from_str(",
    "MenuItem::new(",
    ".with_tooltip(",
    "w!(",
];

/// Fallback calls (F).
const FALLBACK_CALLS: &[&str] = &["unwrap_or(", "unwrap_or_else(|| ", "unwrap_or_else(|_| "];

/// Put together so that the pre-commit check does not take the patterns for real calls.
const PRINT_CALLS: &[&str] = &[
    concat!("println", "!("),
    concat!("eprintln", "!("),
    concat!("print", "!("),
    concat!("eprint", "!("),
];

fn root() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(plugin_guards::repo_root)
}

fn rel_of(file: &Path) -> String {
    normalized_rel(file, root())
}

/// Scan targets: `src/**/*.rs` and `crates/*/src/**/*.rs`, without test files, debug
/// folders, and build scripts.
fn is_scan_target(rel: &str) -> bool {
    if !rel.ends_with(".rs") || rel.ends_with("/build.rs") || rel == "build.rs" {
        return false;
    }
    let in_crate_src = rel
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(_, after)| after.starts_with("src/"));
    if !(rel.starts_with("src/") || in_crate_src) {
        return false;
    }
    if rel.contains("/tests/") || rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs") {
        return false;
    }
    if rel.contains("/debug/") {
        return false; // debug-only surfaces such as user input replay
    }
    true
}

fn gather(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if is_scan_target(&rel_of(path)) {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // A CARGO_TARGET_DIR under another name is left out by its cache marker.
            if is_pruned(name) || tasty_doc_guards::is_build_cache_dir(&p) {
                continue;
            }
        }
        gather(&p, out);
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF   // Hangul Jamo
        | 0x3040..=0x30FF // Hiragana · Katakana
        | 0x3130..=0x318F // Hangul Compatibility Jamo
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7A3 // Hangul Syllables
    )
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

/// Two ASCII letters in a row: even one word counts as user-facing text.
fn has_word(s: &str) -> bool {
    s.as_bytes()
        .windows(2)
        .any(|w| w[0].is_ascii_alphabetic() && w[1].is_ascii_alphabetic())
}

/// The text without trailing punctuation (`…` / `:` / `.` / `!` / `?`).
fn strip_trailing_punct(s: &str) -> &str {
    s.trim().trim_end_matches(['…', '.', ':', '!', '?', ' '])
}

/// One English word of three or more letters, capitalized and otherwise lower case
/// (`Quit` / `Shell`).
fn is_capitalized_word(s: &str) -> bool {
    let s = strip_trailing_punct(s);
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && s.len() >= 3
        && chars.all(|c| c.is_ascii_lowercase())
}

/// Sentence form: among the words split by spaces is a plain English word of four or more
/// letters.
fn looks_like_sentence(s: &str) -> bool {
    s.contains(' ')
        && s.split_whitespace().any(|w| {
            let w = strip_trailing_punct(w).trim_end_matches(',');
            w.len() >= 4 && w.chars().all(|c| c.is_ascii_alphabetic())
        })
}

fn is_token_allowlisted(s: &str) -> bool {
    LITERAL_TOKEN_ALLOWLIST.contains(&s.trim())
}

/// W, widget argument: a word or CJK is a violation.
fn strict_violation(lit: &str) -> bool {
    !is_token_allowlisted(lit) && (has_cjk(lit) || has_word(lit))
}

/// H, placeholder: only a sentence or CJK is a violation.
fn hint_violation(lit: &str) -> bool {
    has_cjk(lit) || looks_like_sentence(lit)
}

/// N / F / E: identifiers are allowed; a capitalized word, a sentence, or CJK is a
/// violation.
fn prose_violation(lit: &str) -> bool {
    !is_token_allowlisted(lit)
        && (has_cjk(lit) || looks_like_sentence(lit) || is_capitalized_word(lit))
}

/// The content of the string literal that `after` starts with (after spaces and `&`).
fn leading_literal(after: &str) -> Option<&str> {
    let rest = after.trim_start().trim_start_matches('&').trim_start();
    let body = rest.strip_prefix('"')?;
    let mut prev_backslash = false;
    for (i, c) in body.char_indices() {
        if c == '"' && !prev_backslash {
            return Some(&body[..i]);
        }
        prev_backslash = c == '\\' && !prev_backslash;
    }
    None
}

/// Judges the text argument of the placeholder color helper (after the first top-level
/// comma) by rule H when it is a literal. So that a call over several lines is not missed,
/// the whole file is read by matching parentheses in a copy with comments and literals
/// masked, and the literal at the same byte position is taken from a copy with only
/// comments masked. A comment before the second argument becomes spaces and a quote inside
/// a comment is not read as a literal. A call starting on a line where `skipped[i]` is true
/// (a test region) is skipped. The line number returned is the line of the literal.
fn find_hint_helper_calls(contents: &str, skipped: &[bool]) -> Vec<(usize, String)> {
    const HELPER: &str = "tasty_egui_theme::hint_text(";
    let code = tasty_doc_guards::source_text::mask_non_code_aligned(contents);
    let literals = tasty_doc_guards::source_text::mask_comments_aligned(contents);
    let line_of = |at: usize| code[..at].matches('\n').count();
    let mut out = Vec::new();
    for (pos, _) in code.match_indices(HELPER) {
        if skipped.get(line_of(pos)).copied().unwrap_or(false) {
            continue;
        }
        let open = pos + HELPER.len();
        let Some(comma) = top_level_comma(&code[open..]) else {
            continue;
        };
        let arg = open + comma + 1;
        if let Some(lit) = leading_literal(&literals[arg..])
            && hint_violation(lit)
        {
            let lit_at = arg + literals[arg..].find('"').unwrap_or(0);
            out.push((line_of(lit_at) + 1, format!("hint_text {lit:?}")));
        }
    }
    out
}

/// The first comma at depth 0 between just after an opening parenthesis and its closing
/// parenthesis.
fn top_level_comma(code: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in code.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            ',' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Finds every place in `line` where a literal follows `call` and judges it with `judge`.
fn find_calls(line: &str, calls: &[&str], judge: fn(&str) -> bool, out: &mut Vec<String>) {
    for call in calls {
        let mut from = 0;
        while let Some(pos) = line[from..].find(call) {
            let start = from + pos;
            from = start + call.len();
            // So that `w!(` does not match the end of `anyhow!(` nor `println!(` that of
            // `eprintln!(`: when the pattern starts with an identifier character, the
            // character before it must not be one.
            let starts_with_ident = call
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let preceded_by_ident = line[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if starts_with_ident && preceded_by_ident {
                continue;
            }
            if let Some(lit) = leading_literal(&line[from..])
                && judge(lit)
            {
                out.push(format!(
                    "{} {lit:?}",
                    call.trim_end_matches('(').trim_end_matches('!')
                ));
            }
        }
    }
}

/// Skips only the bodies of test items, so product code after a test module is still
/// checked.
#[derive(Default)]
struct TestRegion {
    skipping: bool,
    depth: i32,
    opened: bool,
}

impl TestRegion {
    /// True when this line is in a test region.
    fn skip(&mut self, line: &str) -> bool {
        let trimmed = line.trim_start();
        if !self.skipping {
            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[test]") {
                self.skipping = true;
                self.depth = 0;
                self.opened = false;
                return true;
            }
            return false;
        }
        let (opens, closes) = brace_counts(line);
        if opens > 0 {
            self.opened = true;
        }
        self.depth += opens - closes;
        if self.opened {
            if self.depth <= 0 {
                self.skipping = false; // end of the body; this line is still test code
            }
        } else if trimmed.ends_with(';') {
            self.skipping = false; // one-line item: `#[cfg(test)] mod tests;` / `use …;`
        }
        true
    }
}

/// Counts the braces outside plain strings, brace char literals, and line comments. Raw
/// strings are not read on their own, so a test region can be misjudged.
fn brace_counts(line: &str) -> (i32, i32) {
    let mut opens = 0;
    let mut closes = 0;
    let mut in_str = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '\'' => {
                // Char literals `'{'` / `'}'`; a lifetime `'a` is not followed by a brace.
                if let Some(&next) = chars.peek()
                    && (next == '{' || next == '}')
                {
                    chars.next();
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'/') => break,
            '{' => opens += 1,
            '}' => closes += 1,
            _ => {}
        }
    }
    (opens, closes)
}

/// The violations of one file, as (1-based line, description).
fn scan_file(contents: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = contents.lines().collect();
    let mut found = Vec::new();
    let mut tests = TestRegion::default();
    let mut skipped = vec![false; lines.len()];
    for (idx, line) in lines.iter().enumerate() {
        if tests.skip(line) {
            skipped[idx] = true;
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let mut hits = Vec::new();
        find_calls(line, WIDGET_CALLS, strict_violation, &mut hits);
        find_calls(line, &[".hint_text("], hint_violation, &mut hits);
        find_calls(line, NATIVE_CALLS, prose_violation, &mut hits);
        find_calls(line, FALLBACK_CALLS, prose_violation, &mut hits);
        find_calls(line, PRINT_CALLS, prose_violation, &mut hits);
        for hit in hits {
            found.push((idx + 1, hit));
        }
    }
    found.extend(find_hint_helper_calls(contents, &skipped));
    found.sort();
    found
}

/// The (file, literal) of the PENDING entry that `hit` matches.
fn pending_entry_of(rel: &str, hit: &str) -> Option<(&'static str, &'static str)> {
    PENDING_FIX_LITERALS
        .iter()
        .find(|(file, lit, _)| *file == rel && hit.ends_with(&format!("{lit:?}")))
        .map(|(file, lit, _)| (*file, *lit))
}

#[test]
fn no_hardcoded_user_facing_strings() {
    let mut files = Vec::new();
    gather(root(), &mut files);
    files.sort();
    assert!(
        files.len() >= MIN_SCANNED_FILES,
        "scanned only {} source files (floor {MIN_SCANNED_FILES}). Check the walk and the \
         scan targets.",
        files.len()
    );

    let mut violations = Vec::new();
    let mut pending_seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    for file in &files {
        let Ok(contents) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = rel_of(file);
        for (line_no, hit) in scan_file(&contents) {
            if let Some(entry) = pending_entry_of(&rel, &hit) {
                pending_seen.insert(entry);
                continue;
            }
            violations.push(format!("  {rel}:{line_no}: {hit}"));
        }
    }
    assert!(
        violations.is_empty(),
        "user-facing string literals must go through the translator. Move each to the \
         plugin's lang/{{en,ko,ja,zh-Hans}}.toml, or if it is a fixed identifier register it \
         in LITERAL_TOKEN_ALLOWLIST with a reason:\n{}",
        violations.join("\n")
    );
    let stale: Vec<String> = PENDING_FIX_LITERALS
        .iter()
        .filter(|(file, lit, _)| !pending_seen.contains(&(*file, *lit)))
        .map(|(file, lit, fix)| format!("  {file} {lit:?} ({fix})"))
        .collect();
    assert!(
        stale.is_empty(),
        "PENDING_FIX_LITERALS entries no longer occur — remove them:\n{}",
        stale.join("\n")
    );
}

/// A sentence literal in a hint wrapped by the color helper is caught like a bare hint.
#[test]
fn a_sentence_inside_the_hint_color_helper_is_reported() {
    let wrapped = ".hint_text(tasty_egui_theme::hint_text(th, \"Enter a host name\"))";
    assert_eq!(
        scan_file(wrapped).len(),
        1,
        "missed the sentence inside the helper"
    );
    let example = ".hint_text(tasty_egui_theme::hint_text(th, \"10.0.4.12\"))";
    assert!(
        scan_file(example).is_empty(),
        "a technical example is allowed"
    );
    let bare = ".hint_text(\"Enter a host name\")";
    assert_eq!(scan_file(bare).len(), 1);
}

/// A helper call written over several lines is judged the same. The line reported is the
/// line of the literal.
#[test]
fn a_sentence_inside_a_multi_line_hint_color_helper_is_reported() {
    let wrapped = "let e = TextEdit::singleline(v)\n    .hint_text(tasty_egui_theme::hint_text(\n        th,\n        \"Enter a host name\",\n    ));";
    assert_eq!(
        scan_file(wrapped),
        vec![(4, "hint_text \"Enter a host name\"".to_owned())],
        "missed the sentence inside a multi-line helper"
    );
    let example = ".hint_text(tasty_egui_theme::hint_text(\n    th,\n    \"10.0.4.12\",\n))";
    assert!(
        scan_file(example).is_empty(),
        "a technical example is allowed"
    );
    // A comma inside the first argument does not separate arguments.
    let nested =
        ".hint_text(tasty_egui_theme::hint_text(\n    pick(a, b),\n    \"Enter a host name\",\n))";
    assert_eq!(
        scan_file(nested).len(),
        1,
        "split at the comma of the first argument"
    );
    let translated =
        ".hint_text(tasty_egui_theme::hint_text(\n    th,\n    t(\"remote.host_hint\"),\n))";
    assert!(
        scan_file(translated).is_empty(),
        "a translation call is allowed"
    );
    // Calls in comments and test regions are not read.
    let commented = "// .hint_text(tasty_egui_theme::hint_text(\n//     th,\n//     \"Enter a host name\",\n// ))";
    assert!(scan_file(commented).is_empty());
    let in_test = "#[cfg(test)]\nmod tests {\n    fn f() {\n        x.hint_text(tasty_egui_theme::hint_text(\n            th,\n            \"Enter a host name\",\n        ));\n    }\n}";
    assert!(scan_file(in_test).is_empty());
}

/// A comment before the second argument does not hide the literal after it, and a quote
/// inside a comment is not read as a literal.
#[test]
fn a_comment_before_the_hint_helper_text_does_not_hide_it() {
    let line_comment = ".hint_text(tasty_egui_theme::hint_text(\n    th,\n    // input hint\n    \"Enter a host name\",\n))";
    assert_eq!(
        scan_file(line_comment),
        vec![(4, "hint_text \"Enter a host name\"".to_owned())],
        "missed the sentence after a line comment"
    );
    let block_comment = ".hint_text(tasty_egui_theme::hint_text(\n    th,\n    /* input hint */ \"Enter a host name\",\n))";
    assert_eq!(
        scan_file(block_comment),
        vec![(3, "hint_text \"Enter a host name\"".to_owned())],
        "missed the sentence after a block comment"
    );
    // A fake string in a comment is not reported, and the real argument (a translation
    // call) passes.
    let fake_in_comment = ".hint_text(tasty_egui_theme::hint_text(\n    th,\n    /* \"Enter a host name\" */ t(\"remote.host_hint\"),\n))";
    assert!(
        scan_file(fake_in_comment).is_empty(),
        "reported a string inside a comment"
    );
    let fake_line = ".hint_text(tasty_egui_theme::hint_text(\n    th,\n    // \"Enter a host name\"\n    t(\"remote.host_hint\"),\n))";
    assert!(
        scan_file(fake_line).is_empty(),
        "reported a string inside a line comment"
    );
}
