//! Plugins do not write the system clipboard. tasty allowed clipboard writes only in its
//! own copy functions and ran this check on the plugin copies before they moved here.
//! This repository has no such function, so no plugin code may write the clipboard.
//!
//! Every Rust file under `crates/` is read through the shared reader, tests and build
//! scripts included, and this file too: the shapes below and the synthetic inputs are
//! string literals, which the mask hides. Comments and strings are masked, and each
//! masked line is searched for the write call shapes of the clipboard APIs a plugin can
//! depend on: arboard text, html, and image writes, Wayland and X11 selection ownership,
//! macOS pasteboard writes, Windows clipboard writes (`SetClipboardData` and
//! clipboard-win), egui and egui-winit output copies, and the names tasty used for its
//! own copy paths. A shape that starts with a dot counts wherever it appears. A shape
//! that starts with a name does not count after another identifier character
//! (`my_handle_platform_output(`) or right after `fn` (a definition). The Core port
//! shape `.write_text(` counts only when the lowercased masked line contains
//! `clipboard`.
//!
//! Not found: writes under common names such as arboard `clear()`, renamed aliases, and
//! calls split over several lines (`.set()` is found on its own line, not the `.text(`
//! after it). Copying selected text in an eframe widget (Ctrl+C) is handled inside
//! eframe and is not a call in this code. Copies done by a WebView or a page script are
//! not Rust calls and are outside this check.
//!
//! One exception: the test double `ScanHost::set_text` in the claude plugin's
//! `error_scan.rs` shares the arboard name and only sets the text of a fake screen. Its
//! `.set_text(` calls pass when the receiver is `host` or `restarted_host` and the
//! innermost function around the call is one of the listed tests. Every listed test
//! must still use it, so a stale entry fails.

use std::collections::BTreeMap;

use plugin_guards::repo_root;
use tasty_doc_guards::match_arms::Source;
use tasty_doc_guards::source_text::rust_sources;

/// A write call shape and, for a name shared with non-clipboard methods, a word the
/// lowercased masked line must also contain.
struct Shape {
    call: &'static str,
    receiver: Option<&'static str>,
}

const fn shape(call: &'static str) -> Shape {
    Shape {
        call,
        receiver: None,
    }
}

const SHAPES: &[Shape] = &[
    // arboard and tasty's TerminalClipboard: text, formatted, and image writes.
    shape(".set_text("),
    shape(".set_text_primary("),
    shape(".set()"),
    shape(".set_image("),
    // Wayland selection writes and the data device call under them.
    shape(".copy_checked("),
    shape(".copy_with_png("),
    shape("copy_selection_text("),
    shape("set_selection("),
    // X11 selection ownership (x11rb uses the same name).
    shape("set_selection_owner("),
    // macOS pasteboard writes (objc2-app-kit uses the same names).
    shape("setString_forType("),
    shape("setData_forType("),
    shape("writeObjects("),
    // Platform writes of text and image together.
    shape("write_text_and_png("),
    shape("SetClipboardData("),
    // egui-winit output handling writes copies itself, and egui image copies.
    shape("handle_platform_output("),
    shape("copy_with_egui_winit("),
    shape("take_egui_copy("),
    shape("CopyImage("),
    shape(".copy_image("),
    // tasty's Core clipboard port. The same name writes text to a PTY elsewhere.
    Shape {
        call: ".write_text(",
        receiver: Some("clipboard"),
    },
    // Capture publishing (text and image under one owner).
    shape("write_text_and_image("),
    shape("publish_text_with_image("),
    // Added here: arboard html writes.
    shape(".set_html("),
    // Added here: clipboard-win, a Windows dependency of the clipboard viewer.
    shape("set_clipboard("),
    shape("set_clipboard_string("),
    shape("EmptyClipboard("),
    // Added here: clearing a macOS pasteboard takes its ownership.
    shape("clearContents("),
    // Added here: egui text copies.
    shape(".copy_text("),
    shape("CopyText("),
];

/// The test double whose `set_text` is not a clipboard write.
const DOUBLE_PATH: &str = "crates/tasty-plugin-claude/src/error_scan.rs";
const DOUBLE_CALL: &str = ".set_text(";
const DOUBLE_RECEIVERS: &[&str] = &["host", "restarted_host"];
const DOUBLE_FNS: &[&str] = &[
    "transient_error_that_keeps_producing_output_never_stalls",
    "an_old_error_followed_by_more_output_uses_the_normal_threshold",
    "an_error_followed_by_a_short_redraw_still_uses_the_error_threshold",
    "status_line_redraws_of_an_idle_target_never_fire_a_stall",
    "new_output_starts_a_new_quiet_stretch_for_the_state_check",
    "output_resumption_rearms_and_does_not_double_fire",
    "a_screen_notice_before_a_restart_is_not_repeated_while_the_output_stays",
    "a_background_wait_is_noticed_once",
    "an_error_split_across_two_polls_is_still_detected",
    "an_error_stall_after_a_stale_stall_reaches_a_parent_with_an_idle_check",
];

/// Rust files under crates/: 181 measured on 2026-11-21 at fd948f3. The floor and its
/// reason are those of `no_emoji_in_source`: losing all of codex (49), claude (42), or
/// markdown (21) fails, losing one of the smaller crates does not.
const MIN_RUST_FILES: usize = 165;

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Every position of `call` in `line`. A shape that starts with a name does not count
/// after another identifier character or right after `fn`.
fn uses_at(line: &str, call: &str) -> Vec<usize> {
    let mut found: Vec<usize> = line.match_indices(call).map(|(at, _)| at).collect();
    found.retain(|&at| {
        if call.starts_with('.') {
            return true;
        }
        let before = &line[..at];
        let defines = before
            .trim_end()
            .strip_suffix("fn")
            .is_some_and(|rest| !rest.chars().next_back().is_some_and(is_ident));
        !defines && !before.chars().next_back().is_some_and(is_ident)
    });
    found
}

/// Every position in `line` where `shape` is used.
fn shape_uses_at(shape: &Shape, line: &str) -> Vec<usize> {
    if shape
        .receiver
        .is_some_and(|word| !line.to_ascii_lowercase().contains(word))
    {
        return Vec::new();
    }
    let mut found = uses_at(line, shape.call);
    found.sort_unstable();
    found.dedup();
    found
}

/// Whether any shape is used in `line`.
fn writes(line: &str) -> bool {
    SHAPES.iter().any(|s| !shape_uses_at(s, line).is_empty())
}

/// The identifier right before byte `at` of `line`.
fn receiver_before(line: &str, at: usize) -> String {
    let mut name: Vec<char> = line[..at]
        .chars()
        .rev()
        .take_while(|c| is_ident(*c))
        .collect();
    name.reverse();
    name.into_iter().collect()
}

fn is_word_boundary(code: &str, pos: usize, word: &str) -> bool {
    let before = code[..pos].chars().next_back();
    let after = code[pos + word.len()..].chars().next();
    !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
}

fn word_positions(code: &str, word: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = code[from..].find(word) {
        let pos = from + rel;
        if is_word_boundary(code, pos, word) {
            out.push(pos);
        }
        from = pos + word.len();
    }
    out
}

/// The byte position of the delimiter that closes the (), {}, or [] at `open`.
fn matching_delim(code: &str, open: usize) -> Option<usize> {
    let opener = code[open..].chars().next()?;
    let closer = match opener {
        '(' => ')',
        '{' => '}',
        '[' => ']',
        _ => return None,
    };
    let mut depth = 0usize;
    for (offset, c) in code[open..].char_indices() {
        if c == opener {
            depth += 1;
        } else if c == closer {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

/// The first block at the same depth after `from`. The () and [] of the signature are
/// skipped, and a `;` met first means a declaration without a body.
fn block_after(code: &str, from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < code.len() {
        let c = code[i..].chars().next()?;
        match c {
            '{' => return matching_delim(code, i).map(|end| (i, end)),
            '(' | '[' => i = matching_delim(code, i)? + 1,
            ';' => return None,
            _ => i += c.len_utf8(),
        }
    }
    None
}

/// Each function with a body: name, body open, body close.
fn fn_spans(code: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    for kw in word_positions(code, "fn") {
        let after = kw + "fn".len();
        let name: String = code[after..]
            .chars()
            .skip_while(|c| c.is_whitespace())
            .take_while(|c| is_ident(*c))
            .collect();
        if name.is_empty() {
            continue; // A type such as `fn(u32) -> u32`.
        }
        let Some((open, close)) = block_after(code, after) else {
            continue;
        };
        out.push((name, open, close));
    }
    out
}

/// The innermost function whose body holds `pos`.
fn enclosing_fn(spans: &[(String, usize, usize)], pos: usize) -> Option<&str> {
    spans
        .iter()
        .filter(|(_, open, close)| *open < pos && pos < *close)
        .min_by_key(|(_, open, close)| close - open)
        .map(|(name, _, _)| name.as_str())
}

fn is_double(rel: &str, call: &str, receiver: &str, function: Option<&str>) -> bool {
    rel == DOUBLE_PATH
        && call == DOUBLE_CALL
        && DOUBLE_RECEIVERS.contains(&receiver)
        && function.is_some_and(|f| DOUBLE_FNS.contains(&f))
}

/// The writes of one file, and how often each listed test uses the double.
fn offenders_in(rel: &str, raw: &str) -> (Vec<String>, BTreeMap<String, usize>) {
    // Call positions and function spans both come from this one byte-aligned mask.
    let source = Source::new(raw);
    let spans = fn_spans(&source.code);
    let raw_lines: Vec<&str> = raw.split('\n').collect();
    let mut offenders = Vec::new();
    let mut double_uses = BTreeMap::new();
    let mut start = 0usize;
    for (i, line) in source.code.split_inclusive('\n').enumerate() {
        for shape in SHAPES {
            for at in shape_uses_at(shape, line) {
                let function = enclosing_fn(&spans, start + at);
                let receiver = receiver_before(line, at);
                if is_double(rel, shape.call, &receiver, function) {
                    *double_uses
                        .entry(function.unwrap_or_default().to_string())
                        .or_insert(0) += 1;
                    continue;
                }
                offenders.push(format!(
                    "  {rel}:{} (fn {})  {}  {}",
                    i + 1,
                    function.unwrap_or("-"),
                    shape.call,
                    raw_lines.get(i).map_or("", |l| l.trim())
                ));
            }
        }
        start += line.len();
    }
    (offenders, double_uses)
}

#[test]
fn no_clipboard_write() {
    let root = repo_root();
    let sources = rust_sources(&root, &["crates"]);
    assert!(
        sources.len() >= MIN_RUST_FILES,
        "read {} Rust files under crates/ (floor {MIN_RUST_FILES}). Check the reader. If the \
         tree really shrank, update the floor with the new measurement.",
        sources.len()
    );

    let mut offenders = Vec::new();
    let mut double_uses = BTreeMap::new();
    for (rel, raw) in &sources {
        let rel = rel.to_string_lossy().replace('\\', "/");
        let (found, uses) = offenders_in(&rel, raw);
        offenders.extend(found);
        for (function, n) in uses {
            *double_uses.entry(function).or_insert(0) += n;
        }
    }
    assert!(
        offenders.is_empty(),
        "{} clipboard writes in plugin code. Plugins do not write the system clipboard:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
    let unused: Vec<&str> = DOUBLE_FNS
        .iter()
        .copied()
        .filter(|f| !double_uses.contains_key(*f))
        .collect();
    assert!(
        unused.is_empty(),
        "these tests in {DOUBLE_PATH} no longer call the screen double's set_text. Remove \
         them from DOUBLE_FNS: {unused:?}"
    );
}

#[test]
fn the_exception_path_exists() {
    assert!(
        repo_root().join(DOUBLE_PATH).is_file(),
        "{DOUBLE_PATH} is gone. Remove the exception from this check."
    );
}

#[test]
fn each_write_shape_is_found_and_other_calls_pass() {
    let cases = [
        ("selection.copy_checked(text.to_string())", true),
        ("self.0.copy_with_png(text, Some(png))", true),
        ("pasteboard.setString_forType(&string, string_type)", true),
        ("gpu.copy_with_egui_winit(window, text);", true),
        ("if gpu.take_egui_copy().is_some() {", true),
        ("GpuState::take_egui_copy(gpu)", true),
        (
            "pub fn take_egui_copy(&mut self) -> Option<String> {",
            false,
        ),
        ("self.clipboard.set_text(text)", true),
        ("clipboard.set_text_primary(text)", true),
        ("cb.set_text(text)", true),
        // tasty passed this line. Here `.copy_text(` is the added egui shape.
        ("self.copy_text(&text, false)", true),
        ("clipboard()?.set().text(t)", true),
        ("            .set()", true),
        ("cb.set_image(image)", true),
        ("flag.set(true)", false),
        (
            "self.egui_state.handle_platform_output(window, output);",
            true,
        ),
        ("my_handle_platform_output(x)", false),
        (
            "o.commands.push(egui::OutputCommand::CopyImage(img));",
            true,
        ),
        ("ui.ctx().copy_image(img);", true),
        ("core.clipboard_arc().write_text(text)", true),
        ("self.clipboard.write_text_and_image(text, image)", true),
        (
            "Ok(crate::clipboard::files::write_text_and_image(text, image)?)",
            true,
        ),
        (
            "let c = crate::ports::clipboard::publish_text_with_image(",
            true,
        ),
        (
            "fn write_text_and_image(&self, text: &str) -> Result<()> {",
            false,
        ),
        ("self.0.copy_selection_text(text)", true),
        // The method of the same name that sends text to a PTY is not a clipboard write.
        ("self.write_text(surface_id, &text)", false),
        ("device.set_selection(Some(&source), serial);", true),
        ("conn.set_selection_owner(w, a, t)?", true),
        ("pb.writeObjects(&objects)", true),
        ("pb.setData_forType(Some(&d), t)", true),
        ("x11::write_text_and_png(text, png)", true),
        ("unsafe { SetClipboardData(format, h) }", true),
        // A definition is not a write.
        ("pub(crate) fn write_text_and_png(text: &str) {", false),
        // The shapes added here.
        ("clip.set_html(html, Some(alt))", true),
        ("clipboard_win::set_clipboard(formats::Unicode, text)", true),
        ("set_clipboard_string(text)", true),
        ("unsafe { EmptyClipboard() }", true),
        ("pasteboard.clearContents();", true),
        ("ui.ctx().copy_text(text);", true),
        (
            "o.commands.push(egui::OutputCommand::CopyText(text));",
            true,
        ),
    ];
    for (line, expected) in cases {
        assert_eq!(writes(line), expected, "{line}");
    }
}

/// Given the double's file, when a call falls outside the listed tests or uses another
/// receiver, then that call alone is reported, under its innermost function.
#[test]
fn the_double_passes_only_inside_its_tests_and_receivers() {
    let rel = DOUBLE_PATH;
    let cases = [
        ("fn other() { host.set_text(x); }\n", "fn other"),
        (
            "fn a_background_wait_is_noticed_once() { clip.set_text(x); }\n",
            "fn a_background_wait_is_noticed_once",
        ),
        // A function nested on one line inside a listed test.
        (
            "fn a_background_wait_is_noticed_once(host: &mut ScanHost) { host.set_text(x); \
             fn other(host: &mut Clip) { host.set_text(x); } }\n",
            "fn other",
        ),
        // An array length in the nested signature does not hide the nested function.
        (
            "fn a_background_wait_is_noticed_once(host: &mut ScanHost) {\n    \
             host.set_text(\"ok\");\n    \
             fn other(host: &mut arboard::Clipboard, _: [u8; 1]) {\n        \
             let _ = host.set_text(\"x\");\n    }\n}\n",
            "fn other",
        ),
        (
            "fn a_background_wait_is_noticed_once(host: &mut ScanHost) {\n    \
             host.set_text(\"ok\");\n    \
             fn other(host: &mut arboard::Clipboard, _: [u8; { 1 }]) {\n        \
             let _ = host.set_text(\"x\");\n    }\n}\n",
            "fn other",
        ),
    ];
    for (src, function) in cases {
        let (offenders, _) = offenders_in(rel, src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(&format!("({function})")),
            "{src}: {offenders:?}"
        );
    }

    // The same call in another file is a write.
    let (offenders, _) = offenders_in(
        "crates/tasty-plugin-x/src/lib.rs",
        "fn a_background_wait_is_noticed_once() { host.set_text(x); }\n",
    );
    assert_eq!(offenders.len(), 1, "{offenders:?}");
}

/// Non-ASCII text before a function keeps the call positions and the function spans in
/// the same coordinates.
#[test]
fn non_ascii_text_before_a_test_keeps_the_calls_in_their_functions() {
    let src = "// 화면 대역의 글자를 바꾼다.\n\
               const NOTE: &str = \"한글 문자열\";\n\
               fn a_background_wait_is_noticed_once(host: &mut ScanHost) {\n    \
               let _ = \"다시 한글\";\n    \
               host.set_text(\"ok\");\n\
               }\n\
               // 허용 밖 함수.\n\
               fn other(host: &mut ScanHost) {\n    \
               host.set_text(\"x\");\n\
               }\n";
    let (offenders, uses) = offenders_in(DOUBLE_PATH, src);
    assert_eq!(
        uses.get("a_background_wait_is_noticed_once"),
        Some(&1),
        "{uses:?}"
    );
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(offenders[0].contains(":9 (fn other)"), "{offenders:?}");
}
