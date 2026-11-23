//! Plugins do not write the system clipboard. tasty allowed clipboard writes only in its
//! own copy functions and ran this check on the plugin copies before they moved here.
//! This repository has no such function, so no plugin code may write the clipboard.
//!
//! Every Rust file under `crates/` is read through the shared reader, tests and build
//! scripts included, and this file too: the shapes below and the synthetic inputs are
//! string literals, which the mask hides. Comments and strings are masked, and the masked
//! code is read as tokens, so a call spaced out or split over lines is still found.
//! `SHAPES` lists the write calls of the clipboard APIs a plugin can depend on: arboard,
//! Wayland and X11 selections, the macOS pasteboard, the Windows clipboard, egui and
//! egui-winit copies, and the names tasty used for its own copy paths. A write function
//! renamed by `use ... as` is followed, and clearing an arboard clipboard (bound to a
//! name or created in the same statement) counts as a write. The reading is syntax only:
//! a write function passed as a value, an arboard clipboard reached through another
//! binding, and calls a macro produces are not found. `offenders_in` and the tests below
//! hold the exact rules. Copies done by an eframe widget, a WebView, or a
//! page script are not Rust calls and are outside this check.
//!
//! One exception: the test double `ScanHost::set_text` in the claude plugin's
//! `error_scan.rs` shares the arboard name and only sets the text of a fake screen. Its
//! `.set_text(` calls pass when the receiver is `host` or `restarted_host` and the
//! innermost function around the call is one of the listed tests. Every listed test
//! must still use it, so a stale entry fails.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use plugin_guards::{FnSpan, fn_spans, is_ident, repo_root};
use tasty_doc_guards::match_arms::Source;
use tasty_doc_guards::source_text::rust_sources;

/// A write call shape and, for a name shared with non-clipboard methods, a word the
/// masked code around the call must also contain.
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

/// The masked code of one file read as tokens: identifier runs and single other
/// characters, by byte range. Whitespace and line breaks only separate tokens.
struct Code<'a> {
    source: Source<'a>,
    tokens: Vec<Range<usize>>,
    line_starts: Vec<usize>,
}

impl<'a> Code<'a> {
    fn new(raw: &'a str) -> Self {
        let source = Source::new(raw);
        let mut tokens = Vec::new();
        let mut chars = source.code.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c.is_whitespace() {
                continue;
            }
            let mut end = at + c.len_utf8();
            if is_ident(c) {
                while let Some(&(next, d)) = chars.peek() {
                    if !is_ident(d) {
                        break;
                    }
                    end = next + d.len_utf8();
                    chars.next();
                }
            }
            tokens.push(at..end);
        }
        let line_starts = std::iter::once(0)
            .chain(source.code.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        Code {
            source,
            tokens,
            line_starts,
        }
    }

    /// The text of token `i`, or "" past either end.
    fn tok(&self, i: usize) -> &str {
        self.tokens
            .get(i)
            .map_or("", |r| &self.source.code[r.clone()])
    }

    fn start(&self, i: usize) -> usize {
        self.tokens[i].start
    }

    fn is_ident_tok(&self, i: usize) -> bool {
        self.tok(i).chars().next().is_some_and(is_ident)
    }

    /// The zero-based line that holds byte `pos`.
    fn line_of(&self, pos: usize) -> usize {
        self.line_starts.partition_point(|&s| s <= pos) - 1
    }

    /// The masked line that holds byte `pos`.
    fn line_text(&self, pos: usize) -> &str {
        let line = self.line_of(pos);
        let end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.code.len());
        &self.source.code[self.line_starts[line]..end]
    }

    /// The masked code from the last `;`, `{`, or `}` before byte `pos` up to `pos`.
    fn statement_before(&self, pos: usize) -> &str {
        let code = &self.source.code[..pos];
        let from = code.rfind([';', '{', '}']).map_or(0, |at| at + 1);
        &code[from..]
    }

    /// The token after a name at token `i`: past a turbofish `::<...>`, if any.
    fn after_name(&self, i: usize) -> usize {
        let mut j = i + 1;
        if self.tok(j) == ":" && self.tok(j + 1) == ":" && self.tok(j + 2) == "<" {
            let mut depth = 0usize;
            j += 2;
            while j < self.tokens.len() {
                match self.tok(j) {
                    "<" => depth += 1,
                    ">" => {
                        depth -= 1;
                        if depth == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        j
    }

    /// Whether the `(` at token `i` is followed by `)` with nothing but whitespace and
    /// comments between them.
    fn empty_args(&self, i: usize) -> bool {
        self.tok(i + 1) == ")"
            && self
                .source
                .trim(self.tokens[i].end..self.tokens[i + 1].start)
                .is_empty()
    }
}

/// A shape split into its parts: whether it starts with a dot, the name, and whether it
/// takes no arguments.
fn parts(call: &str) -> (bool, &str, bool) {
    let dot = call.starts_with('.');
    let rest = call.trim_start_matches('.');
    match rest.strip_suffix("()") {
        Some(name) => (dot, name, true),
        None => (dot, rest.trim_end_matches('('), false),
    }
}

/// Whether `shape` is used at token `i`, called by `alias` when the file renamed it.
fn shape_at(code: &Code, shape: &Shape, alias: Option<&str>, i: usize) -> bool {
    let (dot, name, empty) = parts(shape.call);
    let name = alias.unwrap_or(name);
    let name_at = if dot {
        if code.tok(i) != "." {
            return false;
        }
        i + 1
    } else {
        if i > 0 && code.tok(i - 1) == "fn" {
            return false; // A definition.
        }
        i
    };
    if code.tok(name_at) != name {
        return false;
    }
    let open = code.after_name(name_at);
    if code.tok(open) != "(" || (empty && !code.empty_args(open)) {
        return false;
    }
    shape.receiver.is_none_or(|word| {
        let pos = code.start(i);
        code.line_text(pos).to_ascii_lowercase().contains(word)
            || code
                .statement_before(pos)
                .to_ascii_lowercase()
                .contains(word)
    })
}

/// The paths a `use` tree starting at token `i` brings in, each with the name it binds,
/// appended to `out`. Returns the token after the tree.
fn use_tree(
    code: &Code,
    mut i: usize,
    prefix: &[String],
    out: &mut Vec<(Vec<String>, String)>,
) -> usize {
    let mut path = prefix.to_vec();
    while i < code.tokens.len() {
        match code.tok(i) {
            ":" if code.tok(i + 1) == ":" => i += 2,
            "{" => {
                i += 1;
                while i < code.tokens.len() && code.tok(i) != "}" {
                    let next = use_tree(code, i, &path, out);
                    i = if code.tok(next) == "," {
                        next + 1
                    } else {
                        next.max(i + 1)
                    };
                }
                return i + 1;
            }
            "*" => return i + 1,
            "as" => {
                out.push((path, code.tok(i + 1).to_string()));
                return i + 2;
            }
            t if code.is_ident_tok(i) => {
                path.push(t.to_string());
                i += 1;
            }
            _ => break,
        }
    }
    if path.len() > prefix.len()
        && let Some(last) = path.last().cloned()
    {
        out.push((path, last));
    }
    i
}

/// The paths every `use` item of the file brings in, each with the name it binds.
fn use_leaves(code: &Code) -> Vec<(Vec<String>, String)> {
    let mut out = Vec::new();
    for i in 0..code.tokens.len() {
        if code.tok(i) == "use" && (i == 0 || code.tok(i - 1) != ".") {
            use_tree(code, i + 1, &[], &mut out);
        }
    }
    out
}

/// The names a `use ... as` gives the write functions of `SHAPES` in this file.
fn aliases_of(leaves: &[(Vec<String>, String)]) -> Vec<(String, &'static Shape)> {
    let mut out = Vec::new();
    for (path, bound) in leaves {
        let Some(last) = path.last() else { continue };
        if last == bound {
            continue;
        }
        for shape in SHAPES {
            let (dot, name, _) = parts(shape.call);
            if !dot && name == last {
                out.push((bound.clone(), shape));
            }
        }
    }
    out
}

/// The arboard clipboard of a file: the names its type goes by and the names bound to
/// one. Clearing an arboard clipboard writes it, but `clear` is too common a name to
/// count on its own.
struct Arboard {
    types: BTreeSet<String>,
    names: BTreeSet<String>,
}

impl Arboard {
    fn new(code: &Code, leaves: &[(Vec<String>, String)]) -> Self {
        let mut arboard = Arboard {
            types: BTreeSet::new(),
            names: BTreeSet::new(),
        };
        for (path, bound) in leaves {
            if path.len() >= 2 && path[path.len() - 2..] == ["arboard", "Clipboard"] {
                arboard.types.insert(bound.clone());
            }
        }
        for i in 0..code.tokens.len() {
            if code.tok(i) == "type"
                && code.is_ident_tok(i + 1)
                && code.tok(i + 2) == "="
                && arboard
                    .type_at(code, i + 3)
                    .is_some_and(|end| code.tok(end) == ";")
            {
                arboard.types.insert(code.tok(i + 1).to_string());
            }
        }
        for i in 0..code.tokens.len() {
            if let Some(name) = arboard.bound_at(code, i) {
                arboard.names.insert(name.to_string());
            }
        }
        arboard
    }

    /// The token after an arboard clipboard type named at token `i`.
    fn type_at(&self, code: &Code, i: usize) -> Option<usize> {
        if code.tok(i) == "arboard"
            && code.tok(i + 1) == ":"
            && code.tok(i + 2) == ":"
            && code.tok(i + 3) == "Clipboard"
        {
            return Some(i + 4);
        }
        let after_path = i > 0 && code.tok(i - 1) == ":";
        (!after_path && self.types.contains(code.tok(i))).then_some(i + 1)
    }

    /// Whether an arboard clipboard is created (`<type>::new`) at token `i`.
    fn created_at(&self, code: &Code, i: usize) -> bool {
        self.type_at(code, i).is_some_and(|end| {
            code.tok(end) == ":" && code.tok(end + 1) == ":" && code.tok(end + 2) == "new"
        })
    }

    /// The name bound to an arboard clipboard at token `i`: `let [mut] N = ...;` whose
    /// value creates one, or `N: [&] [mut] <type>`.
    fn bound_at<'c>(&self, code: &'c Code, i: usize) -> Option<&'c str> {
        if code.tok(i) == "let" {
            let mut j = i + 1;
            if code.tok(j) == "mut" {
                j += 1;
            }
            if !code.is_ident_tok(j) {
                return None;
            }
            let name = code.tok(j);
            let mut depth = 0usize;
            for k in j + 1..code.tokens.len() {
                match code.tok(k) {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth = depth.saturating_sub(1),
                    ";" if depth == 0 => break,
                    _ if self.created_at(code, k) => return Some(name),
                    _ => {}
                }
            }
            return None;
        }
        if !code.is_ident_tok(i)
            || code.tok(i + 1) != ":"
            || code.tok(i + 2) == ":"
            || (i > 0 && code.tok(i - 1) == ":")
        {
            return None;
        }
        let mut j = i + 2;
        if code.tok(j) == "&" {
            j += 1;
        }
        if code.tok(j) == "mut" {
            j += 1;
        }
        self.type_at(code, j).map(|_| code.tok(i))
    }

    /// The clear call at token `i` when it clears an arboard clipboard: its receiver is a
    /// bound name, or the same statement creates the clipboard.
    fn clear_at(&self, code: &Code, i: usize) -> Option<&'static str> {
        if code.tok(i) != "." || code.tok(i + 2) != "(" {
            return None;
        }
        let call = match code.tok(i + 1) {
            "clear" => ".clear(",
            "clear_with" => ".clear_with(",
            _ => return None,
        };
        if i > 0 && self.names.contains(code.tok(i - 1)) {
            return Some(call);
        }
        let mut k = i;
        while k > 0 && !matches!(code.tok(k - 1), ";" | "{" | "}") {
            k -= 1;
            if self.created_at(code, k) {
                return Some(call);
            }
        }
        None
    }
}

/// Whether any shape is used in `src`.
fn writes(src: &str) -> bool {
    !offenders_in("crates/tasty-plugin-x/src/lib.rs", src)
        .0
        .is_empty()
}

/// The innermost function whose body holds `pos`.
fn enclosing_fn(spans: &[FnSpan], pos: usize) -> Option<&str> {
    spans
        .iter()
        .filter(|s| s.open < pos && pos < s.close)
        .min_by_key(|s| s.close - s.open)
        .map(|s| s.name.as_str())
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
    let code = Code::new(raw);
    let spans = fn_spans(&code.source.code);
    let raw_lines: Vec<&str> = raw.split('\n').collect();
    let leaves = use_leaves(&code);
    let calls: Vec<(Option<String>, &Shape)> = SHAPES
        .iter()
        .map(|shape| (None, shape))
        .chain(
            aliases_of(&leaves)
                .into_iter()
                .map(|(alias, shape)| (Some(alias), shape)),
        )
        .collect();
    let arboard = Arboard::new(&code, &leaves);
    let mut offenders = Vec::new();
    let mut double_uses = BTreeMap::new();
    let report = |offenders: &mut Vec<String>, i: usize, call: &str| {
        let pos = code.start(i);
        let line = code.line_of(pos);
        offenders.push(format!(
            "  {rel}:{} (fn {})  {}  {}",
            line + 1,
            enclosing_fn(&spans, pos).unwrap_or("-"),
            call,
            raw_lines.get(line).map_or("", |l| l.trim())
        ));
    };
    for i in 0..code.tokens.len() {
        if let Some(call) = arboard.clear_at(&code, i) {
            report(&mut offenders, i, call);
        }
        for (alias, shape) in &calls {
            if !shape_at(&code, shape, alias.as_deref(), i) {
                continue;
            }
            let pos = code.start(i);
            let function = enclosing_fn(&spans, pos);
            let receiver = if i > 0 && code.is_ident_tok(i - 1) {
                code.tok(i - 1)
            } else {
                ""
            };
            if is_double(rel, shape.call, receiver, function) {
                *double_uses
                    .entry(function.unwrap_or_default().to_string())
                    .or_insert(0) += 1;
                continue;
            }
            report(&mut offenders, i, shape.call);
        }
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

/// The writes `offenders_in` reports for `src` in a plugin file.
fn found(src: &str) -> Vec<String> {
    offenders_in("crates/tasty-plugin-x/src/lib.rs", src).0
}

/// Given a write call spaced out, split over lines, or with a turbofish, when the file is
/// read, then the call is reported once, on the line of its first token.
#[test]
fn a_write_call_split_over_lines_is_found() {
    let cases = [
        ("fn f() {\n    clip.set_text\n        (\"x\");\n}\n", 2),
        ("fn f() {\n    clip.set_text (\"x\");\n}\n", 2),
        ("fn f() {\n    clip.set_text::<&str>(\"x\");\n}\n", 2),
        // The receiver word on an earlier line of the same statement.
        (
            "fn f() {\n    core\n        .clipboard_arc()\n        .write_text(t);\n}\n",
            4,
        ),
        // The receiver word after the call or in a block before it, on the same line.
        ("fn f() {\n    port.write_text(clipboard_data);\n}\n", 2),
        (
            "fn f() {\n    ({ core.clipboard_arc() }).write_text(text);\n}\n",
            2,
        ),
        // A comment is not an argument.
        ("fn f() {\n    cb.set( /* none */ );\n}\n", 2),
    ];
    for (src, line) in cases {
        let offenders = found(src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(&format!(":{line} (fn f)")),
            "{src}: {offenders:?}"
        );
    }
}

/// Given calls that share a write name but are not writes, when the file is read, then
/// nothing is reported.
#[test]
fn calls_that_only_share_a_write_name_pass() {
    let cases = [
        // The method of the same name that sends text to a PTY.
        "fn f() {\n    self.write_text(surface_id, &text);\n}\n",
        // A setter with an argument is not the arboard builder.
        "fn f() {\n    flag.set(\"value\");\n}\n",
        "fn f() {\n    flag.set('x');\n}\n",
    ];
    for src in cases {
        let offenders = found(src);
        assert!(offenders.is_empty(), "{src}: {offenders:?}");
    }
}

/// Given a write function renamed by `use ... as`, alone or in a group, when the file
/// calls the new name, then the call is reported and the `use` item is not.
#[test]
fn a_write_function_renamed_by_use_is_found() {
    let cases = [
        "use clipboard_win::set_clipboard_string as put;\nfn f() {\n    put(t);\n}\n",
        "pub use clipboard_win::{set_clipboard as put_all, other};\nfn f() {\n    put_all(f, t);\n}\n",
    ];
    for src in cases {
        let offenders = found(src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(offenders[0].contains(":3 (fn f)"), "{src}: {offenders:?}");
    }
}

/// Given a write function taken as a value, when the file is read, then nothing is
/// reported: the check reads calls only.
#[test]
fn a_write_function_taken_as_a_value_is_not_a_call() {
    let offenders = found("fn f() {\n    let f = set_clipboard;\n}\n");
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// Given an arboard clipboard bound to a name, typed through an import or an alias, or
/// created in the statement, when it is cleared, then that clear is reported alone.
#[test]
fn clearing_an_arboard_clipboard_is_a_write() {
    let cases = [
        "fn f() {\n    let mut cb = arboard::Clipboard::new()?;\n    cb.clear()?;\n}\n",
        "fn f(clip: &mut arboard::Clipboard) {\n    let _ = 1;\n    clip.clear_with().default();\n}\n",
        "use arboard::Clipboard as Cb;\nfn f() {\n    let mut c = Cb::new()?; c.clear()?;\n}\n",
        "type Board = arboard::Clipboard;\nfn f(b: &mut Board) {\n    b.clear();\n}\n",
        "fn f() {\n    let _ = 1;\n    arboard::Clipboard::new()?.clear()?;\n}\n",
        "use arboard::Clipboard;\nfn f(c: &mut Clipboard) {\n    c.clear();\n}\n",
        "use arboard::{Clipboard};\nfn f(c: &mut Clipboard) {\n    c.clear();\n}\n",
    ];
    for src in cases {
        let offenders = found(src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(":3 (fn f)") && offenders[0].contains(".clear"),
            "{src}: {offenders:?}"
        );
    }
}

/// Given a clear on something that is not an arboard clipboard, when the file is read,
/// then nothing is reported.
#[test]
fn clearing_other_values_passes() {
    let cases = [
        "fn f() {\n    let mut v = Vec::new();\n    v.clear();\n}\n",
        "fn f() {\n    cb.clear();\n}\n",
    ];
    for src in cases {
        let offenders = found(src);
        assert!(offenders.is_empty(), "{src}: {offenders:?}");
    }
}
