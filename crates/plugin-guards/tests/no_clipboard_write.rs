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
//! name or created in the same statement) counts as a write. The egui copy commands are
//! enum variants: named in a pattern after `let` or at the start of a match arm, they only
//! take a command apart and are not writes (`Code::in_pattern`). The reading is syntax only:
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
    /// Whether the name is an enum variant, which a pattern can also name. A pattern
    /// only takes a value apart, so the variant named there is not a write.
    variant: bool,
}

const fn shape(call: &'static str) -> Shape {
    Shape {
        call,
        receiver: None,
        variant: false,
    }
}

const fn variant(call: &'static str) -> Shape {
    Shape {
        call,
        receiver: None,
        variant: true,
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
    variant("CopyImage("),
    shape(".copy_image("),
    // tasty's Core clipboard port. The same name writes text to a PTY elsewhere.
    Shape {
        call: ".write_text(",
        receiver: Some("clipboard"),
        variant: false,
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
    variant("CopyText("),
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

/// Keywords that end a pattern: a pattern path or binding never holds them.
const STOP_WORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "pub", "return", "static", "struct",
    "type", "unsafe", "use", "where", "while",
];

/// The masked code of one file read as tokens: identifier runs and single other
/// characters, by byte range. Whitespace and line breaks only separate tokens.
struct Code<'a> {
    source: Source<'a>,
    tokens: Vec<Range<usize>>,
    line_starts: Vec<usize>,
    /// The `<` of each turbofish, keyed by the token of its closing `>`.
    turbofish: BTreeMap<usize, usize>,
    /// The closing `>` of each turbofish, keyed by the token of its `<`.
    turbofish_close: BTreeMap<usize, usize>,
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
        let mut code = Code {
            source,
            tokens,
            line_starts,
            turbofish: BTreeMap::new(),
            turbofish_close: BTreeMap::new(),
        };
        code.turbofish = code.turbofish_opens();
        code.turbofish_close = code.turbofish.iter().map(|(&gt, &lt)| (lt, gt)).collect();
        code
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

    /// The token that closes the `(`, `[`, or `{` at token `i`, counting all three kinds.
    fn bracket_close(&self, i: usize) -> Option<usize> {
        let mut depth = 0usize;
        for j in i..self.tokens.len() {
            match self.tok(j) {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The `<` of each turbofish `::<...>` in the file, keyed by the token of its closing
    /// `>`. A turbofish that does not close is left out.
    fn turbofish_opens(&self) -> BTreeMap<usize, usize> {
        let mut out = BTreeMap::new();
        for lt in 2..self.tokens.len() {
            if self.tok(lt) != "<" || self.tok(lt - 1) != ":" || self.tok(lt - 2) != ":" {
                continue;
            }
            let mut depth = 0usize;
            let mut j = lt;
            while j < self.tokens.len() {
                match self.tok(j) {
                    "<" => depth += 1,
                    ">" if matches!(self.tok(j - 1), "-" | "=") => {}
                    ">" => {
                        depth -= 1;
                        if depth == 0 {
                            out.insert(j, lt);
                            break;
                        }
                    }
                    "(" | "[" | "{" => match self.bracket_close(j) {
                        Some(close) => j = close,
                        None => break,
                    },
                    ";" | ")" | "]" | "}" => break,
                    _ => {}
                }
                j += 1;
            }
        }
        out
    }

    /// The masked code of the call whose first token is `first` and whose argument list
    /// opens at token `open`: back to the start of its statement or of its argument in an
    /// outer call, skipping blocks, brackets, and turbofish generics of the receiver, and
    /// on to the `)` that closes the arguments.
    fn call_statement(&self, first: usize, open: usize) -> &str {
        let mut depth = 0usize;
        let mut from = first;
        while from > 0 {
            if let Some(&lt) = self.turbofish.get(&(from - 1)) {
                from = lt;
                continue;
            }
            match self.tok(from - 1) {
                ")" | "]" => depth += 1,
                "}" if depth > 0 => depth += 1,
                "(" | "[" | "{" if depth > 0 => depth -= 1,
                "," | ";" if depth == 0 => break,
                "{" | "}" | "(" | "[" => break,
                _ => {}
            }
            from -= 1;
        }
        let mut depth = 0usize;
        let mut close = open;
        while close < self.tokens.len() {
            match self.tok(close) {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            close += 1;
        }
        let end = self
            .tokens
            .get(close)
            .map_or(self.source.code.len(), |r| r.end);
        &self.source.code[self.start(from)..end]
    }

    /// The token after a name at token `i`: past the turbofish `::<...>` right after it,
    /// if `turbofish_opens` paired one there.
    fn after_name(&self, i: usize) -> usize {
        self.turbofish_close
            .get(&(i + 3))
            .map_or(i + 1, |&gt| gt + 1)
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

    /// The text of the token `n` places before token `i`, or "" before the start.
    fn back(&self, i: usize, n: usize) -> &str {
        i.checked_sub(n).map_or("", |j| self.tok(j))
    }

    /// Whether token `i` is a name a pattern can hold: an identifier run, a number, or
    /// `_`, but not a keyword that ends a pattern.
    fn is_name(&self, i: usize) -> bool {
        self.is_ident_tok(i) && !STOP_WORDS.contains(&self.tok(i))
    }

    /// Whether token `i` is a `|` with no `|` on either side.
    fn is_single_bar(&self, i: usize) -> bool {
        self.tok(i) == "|" && self.back(i, 1) != "|" && self.tok(i + 1) != "|"
    }

    /// Whether token `i` belongs inside one pattern: a name, `&`, `@`, a path `::`, or a
    /// rest or range `..`.
    fn is_pattern_tok(&self, i: usize) -> bool {
        let pair = |t: &str| self.back(i, 1) == t || self.tok(i + 1) == t;
        self.is_name(i)
            || matches!(self.tok(i), "&" | "@")
            || (self.tok(i) == ":" && pair(":"))
            || (self.tok(i) == "." && pair("."))
    }

    /// Whether the `[` at token `i` indexes the value before it. After `let` it opens a
    /// slice pattern, although `let` reads as an identifier.
    fn is_index(&self, i: usize) -> bool {
        (i > 0 && self.is_ident_tok(i - 1) && self.back(i, 1) != "let")
            || matches!(self.back(i, 1), ")" | "]" | "?")
    }

    /// The `(`, `[`, or `{` that the token `close` closes, counting all three kinds.
    fn bracket_open(&self, close: usize) -> Option<usize> {
        let mut depth = 0usize;
        for j in (0..=close).rev() {
            match self.tok(j) {
                ")" | "]" | "}" => depth += 1,
                "(" | "[" | "{" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The first token of the path that ends with the name at token `name`: back over
    /// `::` segments and their turbofish generics.
    fn path_start(&self, mut name: usize) -> usize {
        while self.back(name, 1) == ":" && self.back(name, 2) == ":" {
            let colons = name - 2;
            let segment = colons.checked_sub(1).and_then(|end| {
                self.turbofish
                    .get(&end)
                    .map_or(Some(end), |&lt| lt.checked_sub(3))
            });
            match segment.filter(|&j| self.is_name(j)) {
                Some(j) => name = j,
                None => return colons,
            }
        }
        name
    }

    /// The first token of the bracket group that opens at token `open`, with the path
    /// that names it.
    fn group_start(&self, open: usize) -> usize {
        let Some(end) = open.checked_sub(1) else {
            return open;
        };
        match self.turbofish.get(&end) {
            Some(&lt) if lt >= 3 && self.is_name(lt - 3) => self.path_start(lt - 3),
            Some(_) => open,
            None if self.is_name(end) => self.path_start(end),
            None => open,
        }
    }

    /// The first token of the one pattern that ends just before token `end`.
    fn alternative_start(&self, end: usize) -> usize {
        let mut k = end;
        while k > 0 {
            if matches!(self.tok(k - 1), ")" | "]") {
                match self.bracket_open(k - 1) {
                    Some(open) => k = open,
                    None => break,
                }
            } else if let Some(&lt) = self.turbofish.get(&(k - 1)) {
                k = lt;
            } else if self.is_pattern_tok(k - 1) {
                k -= 1;
            } else {
                break;
            }
        }
        k
    }

    /// The token before a pattern that starts at token `start`: back over reference
    /// and binding prefixes, the alternatives before it, and a leading `|`.
    fn before_pattern(&self, mut start: usize) -> Option<usize> {
        loop {
            let from = start;
            while self.back(start, 1) == "&"
                || (self.back(start, 1) == "mut" && self.back(start, 2) == "&")
            {
                start -= if self.back(start, 1) == "&" { 1 } else { 2 };
            }
            if self.back(start, 1) == "@" && start >= 2 && self.is_name(start - 2) {
                start -= 2;
                if self.back(start, 1) == "mut" {
                    start -= 1;
                }
                if self.back(start, 1) == "ref" {
                    start -= 1;
                }
            }
            if start > 0 && self.is_single_bar(start - 1) {
                let alternative = self.alternative_start(start - 1);
                if alternative == start - 1 {
                    return alternative.checked_sub(1); // A leading `|`.
                }
                start = alternative;
            }
            if start == from {
                return start.checked_sub(1);
            }
        }
    }

    /// The token after a pattern that ends at token `end`: past the alternatives after it.
    /// A `|` followed by what cannot start a pattern, such as `if`, is a bitwise or.
    fn after_pattern(&self, end: usize) -> usize {
        let mut k = end + 1;
        while self.is_single_bar(k)
            && (self.is_pattern_tok(k + 1) || matches!(self.tok(k + 1), "(" | "["))
        {
            k += 1;
            loop {
                if matches!(self.tok(k), "(" | "[") {
                    match self.bracket_close(k) {
                        Some(close) => k = close + 1,
                        None => break,
                    }
                } else if let Some(&gt) = self.turbofish_close.get(&k) {
                    k = gt + 1;
                } else if self.is_pattern_tok(k) {
                    k += 1;
                } else {
                    break;
                }
            }
        }
        k
    }

    /// Whether the `}` at token `close` ends an arm body: its `{` follows `=>`.
    fn ends_arm_body(&self, close: usize) -> bool {
        self.bracket_open(close).is_some_and(|open| {
            self.tok(open) == "{"
                && self.bracket_close(open) == Some(close)
                && self.back(open, 2) == "="
                && self.back(open, 1) == ">"
        })
    }

    /// Whether the `,` at token `comma` ends an arm: the `=>` of the arm before it comes
    /// first when read back, with no guard of the same arm between. Inside other brackets,
    /// macro arguments among them, the comma is read locally as before.
    fn ends_arm(&self, comma: usize) -> bool {
        let mut k = comma;
        while k > 0 {
            match self.tok(k - 1) {
                ")" | "]" | "}" => match self.bracket_open(k - 1) {
                    Some(open) => k = open,
                    None => return true,
                },
                "{" => return self.back(k - 1, 1) == "!",
                "(" | "[" => return true,
                ">" if self.back(k - 1, 1) == "=" => return true,
                "if" => return false,
                _ => k -= 1,
            }
        }
        true
    }

    /// Whether the name at token `name`, called with the arguments that open at token
    /// `open`, stands in a pattern: after `let`, or at the start of a match arm. Read
    /// from the tokens around it alone, inside macro arguments too. The tests below
    /// hold the forms it takes and the ones it leaves as calls.
    fn in_pattern(&self, name: usize, open: usize) -> bool {
        let Some(mut end) = self.bracket_close(open) else {
            return false;
        };
        let mut start = self.path_start(name);
        // Climb out of the tuple, tuple struct, and slice patterns around it.
        let mut k = start;
        while k > 0 {
            let t = self.tok(k - 1);
            if matches!(t, ")" | "]") {
                match self.bracket_open(k - 1) {
                    Some(open) => k = open,
                    None => break,
                }
            } else if let Some(&lt) = self.turbofish.get(&(k - 1)) {
                k = lt;
            } else if t == "(" || (t == "[" && !self.is_index(k - 1)) {
                if self.back(k - 1, 1) == "!" {
                    break; // Macro arguments.
                }
                match self.bracket_close(k - 1) {
                    Some(close) => end = close,
                    None => break,
                }
                start = self.group_start(k - 1);
                k = start;
            } else if self.is_pattern_tok(k - 1) || t == "," || self.is_single_bar(k - 1) {
                k -= 1;
            } else {
                break;
            }
        }
        let Some(before) = self.before_pattern(start) else {
            return false;
        };
        match self.tok(before) {
            // The raw identifier `r#let` names a function, not the keyword.
            "let" => !(self.back(before, 1) == "#" && self.back(before, 2) == "r"),
            "{" | "," | "}" => {
                let after = self.after_pattern(end);
                let arm_end = (self.tok(after) == "=" && self.tok(after + 1) == ">")
                    || self.tok(after) == "if";
                arm_end
                    && match self.tok(before) {
                        "}" => self.ends_arm_body(before),
                        "," => self.ends_arm(before),
                        _ => true,
                    }
            }
            _ => false,
        }
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
    if shape.variant && code.in_pattern(name_at, open) {
        return false; // A pattern takes the command apart.
    }
    shape.receiver.is_none_or(|word| {
        let pos = code.start(i);
        code.line_text(pos).to_ascii_lowercase().contains(word)
            || code
                .call_statement(i, open)
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

/// Given the Core port call with its receiver word only in the arguments on a later line,
/// or only inside a receiver block on an earlier line, when the file is read, then the
/// call is reported; a port call without the word, or with it only in an earlier
/// statement, passes.
#[test]
fn the_receiver_word_is_read_over_the_whole_call() {
    let writes = [
        (
            "fn f() {\n    port.write_text(\n        clipboard_data\n    );\n}\n",
            2,
        ),
        (
            "fn f() {\n    ({ core.clipboard_arc() })\n        .write_text(text);\n}\n",
            3,
        ),
    ];
    for (src, line) in writes {
        let offenders = found(src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(&format!(":{line} (fn f)")),
            "{src}: {offenders:?}"
        );
    }
    let passes = [
        "fn f() {\n    self.write_text(surface_id, &text);\n}\n",
        "fn f() {\n    self.write_text(\n        surface_id,\n        &text,\n    );\n}\n",
        "fn f() {\n    let clipboard = 1;\n    port.write_text(t);\n}\n",
    ];
    for src in passes {
        let offenders = found(src);
        assert!(offenders.is_empty(), "{src}: {offenders:?}");
    }
}

/// Given a port call that is one argument of an outer call, with the receiver word only
/// in an earlier argument, when the file is read, then nothing is reported.
#[test]
fn an_earlier_argument_of_an_outer_call_is_not_the_receiver() {
    let src = "fn f() {\n    let clipboard_data = String::from(\"payload\");\n    consume(\n        ({ clipboard_data }),\n        self.write_text(surface_id, &text),\n    );\n}\n";
    let offenders = found(src);
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// Given a multi-line Core port call whose receiver holds commas inside its own brackets
/// or block, when the file is read, then the receiver word before those commas counts
/// and the call is reported.
#[test]
fn commas_inside_the_receiver_do_not_end_the_call() {
    let cases = [
        (
            "fn f() {\n    self.core\n        .port(Port::Clipboard, id)\n        .write_text(id, &t);\n}\n",
            4,
        ),
        (
            "fn f() {\n    select(core.clipboard_arc(), enabled)\n        .write_text(text);\n}\n",
            3,
        ),
        (
            "fn f() {\n    (clipboard, id).0\n        .write_text(t);\n}\n",
            3,
        ),
        (
            "fn f() {\n    ({ pick(core.clipboard_arc(), 1) })\n        .write_text(t);\n}\n",
            3,
        ),
        (
            "fn f() {\n    [clipboard, other][0]\n        .write_text(t);\n}\n",
            3,
        ),
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

/// Given a multi-line Core port call whose receiver block holds a statement end, when the
/// file is read, then the receiver word before that statement end counts and the call is
/// reported.
#[test]
fn statement_ends_inside_the_receiver_do_not_end_the_call() {
    let src = "fn f() {\n    ({ let c = core.clipboard_arc(); c })\n        .write_text(t);\n}\n";
    let offenders = found(src);
    assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
    assert!(offenders[0].contains(":3 (fn f)"), "{src}: {offenders:?}");
}

/// Given a multi-line Core port call whose receiver holds a turbofish with commas, when
/// the file is read, then the receiver word before the turbofish counts and the call is
/// reported; a generic outer call whose earlier argument alone holds the word passes.
#[test]
fn commas_inside_a_turbofish_of_the_receiver_do_not_end_the_call() {
    let cases = [
        (
            "fn f() {\n    self.core\n        .port::<ClipboardPort, Id>()\n        .write_text(t);\n}\n",
            4,
        ),
        (
            "fn f() {\n    Ports::<ClipboardPort, Id>::get(core)\n        .write_text(t);\n}\n",
            3,
        ),
        (
            "fn f() {\n    core.clipboard_for::<u8, u16>()\n        .write_text(text);\n}\n",
            3,
        ),
        (
            "fn f() {\n    ClipboardPair::<u8, u16>::PORT\n        .write_text(text);\n}\n",
            3,
        ),
        (
            "fn f() {\n    core.clipboard_for::<Vec<(u8, u16)>, Option<u32>>()\n        .write_text(text);\n}\n",
            3,
        ),
    ];
    for (src, line) in cases {
        let offenders = found(src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(&format!(":{line} (fn f)")),
            "{src}: {offenders:?}"
        );
    }
    let src = "fn f() {\n    consume::<u8, u16>(\n        ({ clipboard_data }),\n        self.write_text(surface_id, &text),\n    );\n}\n";
    let offenders = found(src);
    assert!(offenders.is_empty(), "{src}: {offenders:?}");
}

/// Given comparisons inside const arguments of a turbofish, an unclosed turbofish in a
/// macro, and a function type with `->` inside a turbofish, when the file is read, then
/// only the calls whose own receiver holds the word are reported.
#[test]
fn a_turbofish_ends_at_its_own_closing_angle_bracket() {
    let passes = [
        "fn f() {\n    let k = f::<{ A < B }>();\n    let clipboard = 1;\n    port_for(a > b)\n        .write_text(t);\n}\n",
        "fn f() {\n    consume(\n        flag::<{ 1 < 2 }>(),\n        clipboard_data,\n        2 > port\n            .write_text(x),\n    );\n}\n",
        "fn f() {\n    macro_tokens!(tag::<);\n    let clipboard = 1;\n    port_for(a > b)\n        .write_text(t);\n}\n",
    ];
    for src in passes {
        let offenders = found(src);
        assert!(offenders.is_empty(), "{src}: {offenders:?}");
    }
    let writes = [
        (
            "fn f() {\n    clipboard_from(make(flag::<{ 1 < 2 }>(), 2) > 1)\n        .write_text(text);\n}\n",
            3,
        ),
        (
            "fn f() {\n    core.clipboard_for::<fn(u8) -> u8, Id>()\n        .write_text(t);\n}\n",
            3,
        ),
    ];
    for (src, line) in writes {
        let offenders = found(src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(&format!(":{line} (fn f)")),
            "{src}: {offenders:?}"
        );
    }
}

/// Given a `::<` that a statement end or an unmatched closing bracket cuts off before its
/// angle brackets close, and a comparison `>` before a later receiver, when the file is
/// read, then the comparison does not close that turbofish and no call is reported.
#[test]
fn an_unclosed_turbofish_does_not_pair_with_a_later_comparison() {
    let passes = [
        "fn f() {\n    macro_tokens!(tag::<);\n    let clipboard = 1;\n    2 > port\n        .write_text(t);\n}\n",
        "fn f() {\n    macro_tokens!((tag::<A), clipboard_data, 2 > port\n        .write_text(t));\n}\n",
        "fn f() {\n    macro_tokens!([tag::<A], clipboard_data, 2 > port\n        .write_text(t));\n}\n",
        "fn f() {\n    macro_tokens!({tag::<A}, clipboard_data, 2 > port\n        .write_text(t));\n}\n",
        "fn f() {\n    macro_tokens!(tag::<A; clipboard_data, 2 > port\n        .write_text(t));\n}\n",
    ];
    for src in passes {
        let offenders = found(src);
        assert!(offenders.is_empty(), "{src}: {offenders:?}");
    }
}

/// Given a write call whose name is followed by a turbofish with a const block holding a
/// comparison or a function type with `->`, when the file is read, then the call is
/// reported on its first line, its arguments read from their own `(`; the same calls with
/// an argument the shape does not take, or with the receiver word only in an earlier
/// statement, pass, and so does a function type whose return type opens with `(`.
#[test]
fn a_call_name_turbofish_ends_where_the_receiver_reader_ends_it() {
    let writes = [
        ("fn f() {\n    clip.set_text::<{ 1 < 2 }>(\"x\");\n}\n", 2),
        ("fn f() {\n    clip.set_text::<fn(u8) -> u8>(f);\n}\n", 2),
        // The arguments on later lines hold the receiver word.
        (
            "fn f() {\n    port.write_text::<fn(u8) -> u8>(\n        clipboard_data\n    );\n}\n",
            2,
        ),
        (
            "fn f() {\n    port.write_text::<fn() -> (u8, u8)>(\n        clipboard_data\n    );\n}\n",
            2,
        ),
        // The empty argument list is the call's own.
        ("fn f() {\n    clip.set::<{ 1 < 2 }>( /*empty*/ );\n}\n", 2),
        // A statement end inside the receiver block and a turbofish after the call name.
        (
            "fn f() {\n    ({ let c = core.clipboard_arc(); c })\n        .write_text::<fn(u8) -> u8>(t);\n}\n",
            3,
        ),
    ];
    for (src, line) in writes {
        let offenders = found(src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(&format!(":{line} (fn f)")),
            "{src}: {offenders:?}"
        );
    }
    let passes = [
        "fn f() {\n    let clipboard = 1;\n    port.write_text::<fn(u8) -> u8>(t);\n}\n",
        "fn f() {\n    clip.set::<{ 1 < 2 }>(\"x\");\n}\n",
        "fn f() {\n    flag.set::<fn() -> ()>(true);\n}\n",
    ];
    for src in passes {
        let offenders = found(src);
        assert!(offenders.is_empty(), "{src}: {offenders:?}");
    }
}

/// Given the double called with a turbofish after its name, when the file is read, then
/// a listed test with a listed receiver counts as a use, and another receiver or another
/// function is reported.
#[test]
fn the_double_is_read_past_a_call_name_turbofish() {
    let (offenders, uses) = offenders_in(
        DOUBLE_PATH,
        "fn a_background_wait_is_noticed_once() {\n    host.set_text::<fn(u8) -> u8>(f);\n}\n",
    );
    assert!(offenders.is_empty(), "{offenders:?}");
    assert_eq!(
        uses.get("a_background_wait_is_noticed_once"),
        Some(&1),
        "{uses:?}"
    );
    let cases = [
        (
            "fn a_background_wait_is_noticed_once() {\n    clip.set_text::<fn(u8) -> u8>(f);\n}\n",
            "fn a_background_wait_is_noticed_once",
        ),
        (
            "fn other() {\n    host.set_text::<fn(u8) -> u8>(f);\n}\n",
            "fn other",
        ),
    ];
    for (src, function) in cases {
        let (offenders, _) = offenders_in(DOUBLE_PATH, src);
        assert_eq!(offenders.len(), 1, "{src}: {offenders:?}");
        assert!(
            offenders[0].contains(&format!(":2 ({function})")),
            "{src}: {offenders:?}"
        );
    }
}

/// Given a write function renamed by `use ... as` and named with a turbofish, when the
/// file calls it, then the call is reported; taking it as a value passes.
#[test]
fn a_renamed_write_function_with_a_turbofish_is_found() {
    let use_put = "use clipboard_win::set_clipboard_string as put;\n";
    let offenders = found(&format!(
        "{use_put}fn f() {{\n    put::<fn(u8) -> u8>(f);\n}}\n"
    ));
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(offenders[0].contains(":3 (fn f)"), "{offenders:?}");
    let offenders = found(&format!(
        "{use_put}fn f() {{\n    let write_fn = put::<fn() -> (u8, u8)>;\n}}\n"
    ));
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// Given a `::<` after a call name in macro tokens that a statement end or an unmatched
/// closing bracket cuts off, when the file is read, then the name is not read as a call.
#[test]
fn a_call_name_turbofish_left_unclosed_is_not_a_call() {
    let passes = [
        "fn f() {\n    emit!(clip.set_text::<A; B>(\"x\"));\n}\n",
        "fn f() {\n    emit!((clip.set_text::<A), 2 > (\"x\"));\n}\n",
    ];
    for src in passes {
        let offenders = found(src);
        assert!(offenders.is_empty(), "{src}: {offenders:?}");
    }
}

/// The writes reported for `body` placed in `fn f`, one line below its signature.
fn found_in_fn(body: &str) -> Vec<String> {
    found(&format!("fn f() {{\n    {body}\n}}\n"))
}

/// Asserts that each body in `cases` holds no write.
fn assert_no_writes(cases: &[&str]) {
    for body in cases {
        let offenders = found_in_fn(body);
        assert!(offenders.is_empty(), "{body}: {offenders:?}");
    }
}

/// Asserts that each body in `cases` holds the given number of writes, all reported on
/// the line of the body.
fn assert_writes(cases: &[(&str, usize)]) {
    for &(body, count) in cases {
        let offenders = found_in_fn(body);
        assert_eq!(offenders.len(), count, "{body}: {offenders:?}");
        for offender in &offenders {
            assert!(offender.contains(":2 (fn f)"), "{body}: {offenders:?}");
        }
    }
}

/// Given an egui copy command named in the pattern of `let`, `if let`, `let ... else`,
/// or a `let` chain, when the file is read, then nothing is reported.
#[test]
fn a_copy_command_pattern_after_let_is_not_a_write() {
    assert_no_writes(&[
        "let CopyImage(_) = image;",
        "if let OutputCommand::CopyText(t) = c {}",
        "let OutputCommand::CopyText(t) = c else { return };",
        "if let Some(c) = o && let OutputCommand::CopyText(t) = c {}",
    ]);
}

/// Given a tuple struct named like an egui copy command and taken apart by `let` with a
/// turbofish, when the file is read, then nothing is reported.
#[test]
fn a_tuple_struct_named_like_a_copy_command_taken_apart_by_let_is_not_a_write() {
    let offenders = found(
        "struct CopyImage<T>(T);\n\
         fn unpack(image: CopyImage<fn() -> u8>) {\n    \
         let CopyImage::<fn() -> u8>(_) = image;\n\
         }\n",
    );
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// Given egui copy commands matched at the start of match arms, as egui-winit handles
/// them, when the file is read, then nothing is reported.
#[test]
fn copy_commands_matched_in_arms_are_not_writes() {
    assert_no_writes(&[
        "match command { egui::OutputCommand::CopyText(text) => { drop(text); } \
         egui::OutputCommand::CopyImage(_) => {} _ => {} }",
    ]);
}

/// Given an egui copy command inside a tuple struct pattern after `while let`, or bound
/// with `@` inside one in an arm with a guard, when the file is read, then nothing is
/// reported.
#[test]
fn a_copy_command_inside_an_outer_pattern_is_not_a_write() {
    assert_no_writes(&[
        "while let Some(OutputCommand::CopyImage(i)) = it.next() {}",
        "match o { Some(c @ OutputCommand::CopyText(_)) if ready => {} _ => {} }",
    ]);
}

/// Given egui copy commands joined by `|` in an arm or after `if let`, when the file is
/// read, then nothing is reported.
#[test]
fn alternative_copy_command_patterns_are_not_writes() {
    assert_no_writes(&[
        "match c { OutputCommand::CopyText(_) | OutputCommand::CopyImage(_) => {} _ => {} }",
        "if let A(_) | OutputCommand::CopyText(_) = c {}",
    ]);
}

/// Given a pattern with a leading `|` before an egui copy command, after `if let` or in
/// an arm, when the file is read, then nothing is reported.
#[test]
fn a_leading_bar_before_a_copy_command_pattern_is_read_past() {
    assert_no_writes(&[
        "if let | OutputCommand::CopyText(_) = c {}",
        "match c { | OutputCommand::CopyText(_) => {} _ => {} }",
    ]);
}

/// Given a reference pattern of an egui copy command, after `if let` or in an arm, when
/// the file is read, then nothing is reported.
#[test]
fn a_reference_pattern_of_a_copy_command_is_not_a_write() {
    assert_no_writes(&[
        "if let &OutputCommand::CopyText(_) = r {}",
        "if let &mut OutputCommand::CopyText(_) = r {}",
        "match r { &OutputCommand::CopyText(_) => {}, _ => {} }",
    ]);
}

/// Given an egui copy command bound with `ref` or `ref mut` and `@`, after `if let` or
/// in an arm, when the file is read, then nothing is reported.
#[test]
fn a_copy_command_bound_by_reference_is_not_a_write() {
    assert_no_writes(&[
        "if let ref x @ OutputCommand::CopyText(_) = c {}",
        "if let ref mut x @ OutputCommand::CopyText(_) = c {}",
        "match r { ref x @ OutputCommand::CopyText(_) => {}, _ => {} }",
    ]);
}

/// Given a copy command bound with `mut` and `@`, after `if let` or in an arm, when the
/// file is read, then nothing is reported.
#[test]
fn a_copy_command_bound_by_a_mut_name_is_not_a_write() {
    assert_no_writes(&[
        "if let mut x @ Command::CopyText(_) = c {}",
        "match c { mut x @ Command::CopyText(_) => {} _ => {} }",
    ]);
}

/// Given a reference pattern of a copy command in an arm that follows a block arm with
/// no comma, when the file is read, then nothing is reported.
#[test]
fn a_reference_pattern_after_an_arm_without_a_comma_is_not_a_write() {
    assert_no_writes(&["match c { &Command::Other => {} &Command::CopyText(_) => {} _ => {} }"]);
}

/// Given an egui copy command in a slice pattern after `if let` or `let ... else`, when
/// the file is read, then nothing is reported: the `[` after `let` is not an index.
#[test]
fn a_slice_pattern_after_let_is_not_a_write() {
    assert_no_writes(&[
        "if let [OutputCommand::CopyText(_)] = v {}",
        "let [OutputCommand::CopyText(_)] = v else { return };",
    ]);
}

/// Given an egui copy command renamed by `use ... as` and named in an `if let` pattern,
/// when the file is read, then nothing is reported.
#[test]
fn a_renamed_copy_command_in_a_pattern_is_not_a_write() {
    let offenders =
        found("use egui::OutputCommand::CopyText as Ct;\nfn f() {\n    if let Ct(t) = c {}\n}\n");
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// Given egui copy commands built as values, in an initializer, an arm body, a closure,
/// a reference, an index, a tuple, or after `||`, when the file is read, then each is
/// reported, and a `let` with a copy command on both sides reports only the right one.
#[test]
fn copy_commands_built_as_values_are_writes() {
    assert_writes(&[
        ("let x = CopyImage(data);", 1),
        (
            "match a { A => OutputCommand::CopyText(t), _ => OutputCommand::CopyImage(i), }",
            2,
        ),
        (
            "if let Some(x) = o.map(|v| OutputCommand::CopyText(v)) {}",
            1,
        ),
        ("let x = &OutputCommand::CopyText(t);", 1),
        ("let x = m[OutputCommand::CopyText(k)];", 1),
        ("let v = (a, OutputCommand::CopyImage(i));", 1),
        ("let ok = done || send(OutputCommand::CopyText(t));", 1),
    ]);
    let offenders = found("fn f() {\n    let CopyImage(x) =\n        CopyImage(y);\n}\n");
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(offenders[0].contains(":3 (fn f)"), "{offenders:?}");
}

/// Given an egui copy command built in a match guard, when the file is read, then it is
/// reported although `=>` follows it.
#[test]
fn a_copy_command_built_in_a_guard_is_a_write() {
    assert_writes(&[(
        "match c { x if x == OutputCommand::CopyText(t) => {} _ => {} }",
        1,
    )]);
}

/// Given a copy command built after a block and `&` in a match guard, when the file is
/// read, then it is reported: that block does not end an arm body.
#[test]
fn a_copy_command_built_after_a_block_in_a_guard_is_a_write() {
    let offenders = found(
        "fn keep(_: Command) -> bool { true }\n\
         fn f() {\n    \
         match () { _ if { true } & keep(Command::CopyText(t)) => (), _ => () }\n\
         }\n",
    );
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(offenders[0].contains(":3 (fn f)"), "{offenders:?}");
}

/// Given copy command patterns the reader does not take as patterns (a struct pattern
/// field, a place that takes only irrefutable patterns, the second argument of a
/// matches macro without a guard, an arm after a non-block arm body or a labeled block
/// with no comma, an arm after an attribute, a raw identifier), when the file is read,
/// then each is reported as before.
#[test]
fn copy_command_patterns_outside_the_read_places_stay_reported() {
    assert_writes(&[
        (
            "match w { Wrapper { cmd: OutputCommand::CopyText(_) } => {} }",
            1,
        ),
        ("fn g(CopyImage(x): CopyImage<u8>) {}", 1),
        ("for CopyImage(x) in v {}", 1),
        ("CopyImage(x) = img;", 1),
        (
            "commands.retain(|c| !matches!(c, OutputCommand::CopyText(_)));",
            1,
        ),
        (
            "!matches!(c, Command::CopyText(_) | Command::CopyImage(_))",
            2,
        ),
        ("assert_matches!(c, OutputCommand::CopyText(_))", 1),
        ("debug_assert_matches!(c, OutputCommand::CopyText(_))", 1),
        (
            "matches!(factory::<u8, u16>(), OutputCommand::CopyText(_))",
            1,
        ),
        (
            "matches!(<Maker as Factory<u8, u16>>::make(), OutputCommand::CopyText(_))",
            1,
        ),
        ("matches!(|a, x| OutputCommand::CopyText(join(a, x)), _)", 1),
        (
            "match c { A => if a {} else {} &Command::CopyText(_) => {} _ => {} }",
            1,
        ),
        (
            "match c { A => 'body: { break 'body; } &Command::CopyText(_) => {} _ => {} }",
            1,
        ),
        (
            "match r { #[allow(unused_variables)] OutputCommand::CopyText(_) => {}, _ => {} }",
            1,
        ),
        ("if let r#CopyText(_) = c {}", 1),
        (
            "match cmd { OutputCommand::CopyText(_) | OpenUrl { .. } => {} _ => {} }",
            1,
        ),
        (
            "match cmd { OpenUrl { .. } | OutputCommand::CopyText(_) => {} _ => {} }",
            1,
        ),
        (
            "if let OpenUrl { .. } | OutputCommand::CopyText(_) = cmd {}",
            1,
        ),
        (
            "match c { A => if a { x } else { y }, OutputCommand::CopyText(_) => {} _ => {} }",
            1,
        ),
    ]);
    let offenders = found(
        "macro_rules! matches { ($a:expr, $b:expr) => {}; }\n\
         fn f() {\n    \
         matches!((), OutputCommand::CopyText(t));\n\
         }\n",
    );
    assert_eq!(offenders.len(), 1, "{offenders:?}");
}

/// Given copy commands inside macro tokens that the local reading takes as patterns (the
/// left side of `=>` in a user macro, an `if let` in a block passed through, the guarded
/// second argument of a matches macro), when the file is read, then nothing is reported.
#[test]
fn macro_tokens_are_read_by_the_same_local_rule() {
    assert_no_writes(&[
        "table!{ Command::CopyText(t) => 1 }",
        "passthrough!({ if let Command::CopyText(_) = c {} })",
        "matches!(c, Command::CopyText(_) if ready)",
        "matches![c, Command::CopyText(_) if ready]",
        "matches!{c, Command::CopyText(_) if ready}",
        "matches!(c, Command::CopyText(_) | Command::CopyImage(_) if ready)",
    ]);
}

/// Given an egui copy command built by a closure in a match guard, after the comma
/// between the closure's parameters, when the file is read, then it is reported: that
/// comma does not end an arm.
#[test]
fn a_copy_command_built_by_a_closure_in_a_guard_is_a_write() {
    let offenders = found(
        "fn f() {\n    \
         match () {\n        \
         _ if Send(out) & |_, _| OutputCommand::CopyText(text.clone()) => (),\n        \
         _ => (),\n    \
         }\n\
         }\n",
    );
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(offenders[0].contains(":3 (fn f)"), "{offenders:?}");
}

/// Given an egui copy command returned by a `move` closure in a match guard, when the
/// file is read, then it is reported.
#[test]
fn a_copy_command_returned_by_a_move_closure_in_a_guard_is_a_write() {
    assert_writes(&[(
        "match () { _ if return move |_, _| OutputCommand::CopyText(t.clone()) => (), _ => () }",
        1,
    )]);
}

/// Given an egui copy command at the start of an arm after a comma that ends the arm
/// before it, when the file is read, then nothing is reported.
#[test]
fn a_copy_command_pattern_after_an_arm_ending_in_a_comma_is_not_a_write() {
    assert_no_writes(&[
        "match c { A => x, OutputCommand::CopyText(_) => {} _ => {} }",
        "match c { A => f(a, b), OutputCommand::CopyText(_) if ready => {} _ => {} }",
        "match c { OutputCommand::CopyText(_) => {}, OutputCommand::CopyImage(_) => {} }",
    ]);
}

/// Given an egui copy command at the start of an arm after an arm body with commas in a
/// turbofish or between closure parameters, when the file is read, then nothing is
/// reported: those commas belong to the body before.
#[test]
fn a_copy_command_pattern_after_an_arm_body_with_commas_is_not_a_write() {
    assert_no_writes(&[
        "match c { None => discard::<u8, u16>(), Some(OutputCommand::CopyText(_)) => (), _ => () }",
        "match c { A => it.collect::<Result<Vec<u8>, E>>(), OutputCommand::CopyText(_) => {} _ => {} }",
        "match c { A => HashMap::<u8, u8>::new(), OutputCommand::CopyText(_) => {} _ => {} }",
        "match c { A => |a, b| a, OutputCommand::CopyText(_) => {} _ => {} }",
    ]);
}

/// Given an egui copy command built by a closure in the guard of an arm that follows
/// other arms, when the file is read, then it is reported: the comma between the closure
/// parameters does not end the arm before.
#[test]
fn a_copy_command_built_by_a_closure_in_a_later_guard_is_a_write() {
    let offenders = found(
        "fn f() {\n    \
         match x {\n        \
         0 => {}\n        \
         _ if G & |_, _| OutputCommand::CopyText(t.clone()) => (),\n        \
         _ => (),\n    \
         }\n\
         }\n",
    );
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(offenders[0].contains(":4 (fn f)"), "{offenders:?}");
}

/// Given egui copy commands built by closures in the guards of arms after other arms,
/// when the file is read, then each is reported.
#[test]
fn copy_commands_built_by_closures_in_later_guards_are_writes() {
    assert_writes(&[
        (
            "match () { () if false => (), _ if return move |_, _| OutputCommand::CopyText(t.clone()) => (), _ => () }",
            1,
        ),
        (
            "match x { 1 | 2 => drop(x), _ if G & |a: u8, b| OutputCommand::CopyText(t.clone()) => (), _ => () }",
            1,
        ),
        (
            "match true { false => (), _ if Send(out) & |_, _| OutputCommand::CopyText(text.to_owned()) => (), _ => () }",
            1,
        ),
        (
            "match true { false => {} _ if Send(out) & |_, _| OutputCommand::CopyText(text.to_owned()) => (), _ => () }",
            1,
        ),
    ]);
}

/// Given an egui copy command passed to a method or function named by the raw identifier
/// `r#let`, when the file is read, then it is reported: `r#let` is not the keyword.
#[test]
fn a_copy_command_passed_to_a_raw_let_call_is_a_write() {
    let offenders = found(
        "fn send(out: &mut PlatformOutput, text: &str) {\n    \
         out.r#let(OutputCommand::CopyText(text.to_owned()));\n\
         }\n",
    );
    assert_eq!(offenders.len(), 1, "{offenders:?}");
    assert!(offenders[0].contains(":2 (fn send)"), "{offenders:?}");
}

/// Given an egui copy command passed to a free function named `r#let`, when the file is
/// read, then it is reported.
#[test]
fn a_copy_command_passed_to_a_raw_let_function_is_a_write() {
    assert_writes(&[("r#let(OutputCommand::CopyText(x));", 1)]);
}

/// Given an egui copy command built and joined by a bitwise or to an `if` expression,
/// as a statement, when the file is read, then it is reported: that `|` does not start
/// an alternative and the `if` is not a guard.
#[test]
fn a_copy_command_joined_by_a_bitwise_or_to_an_if_is_a_write() {
    assert_writes(&[(
        "OutputCommand::CopyText(text) | if ready { Route(out) } else { Route(out) };",
        1,
    )]);
}

/// Given egui copy commands built by closures in a tuple or an array and joined by a
/// bitwise or to an `if` expression, when the file is read, then each is reported.
#[test]
fn copy_commands_in_closures_joined_by_a_bitwise_or_to_an_if_are_writes() {
    assert_writes(&[
        (
            "let make = (|_: u8, _| OutputCommand::CopyText(text) | if ready { Route(out) } else { Route(out) },);",
            1,
        ),
        (
            "let [make] = [|_: u8, _| OutputCommand::CopyText(text) | if ready { Route(out) } else { Route(out) }];",
            1,
        ),
        (
            "let mut make = (async |_, _| OutputCommand::CopyText(text.clone()) | if ready { Route(out) } else { Route(out) },);",
            1,
        ),
    ]);
}
