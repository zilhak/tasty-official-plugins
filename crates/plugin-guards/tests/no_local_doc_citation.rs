//! Finds citations of local work documents, plan numbers, and design changelog entries in
//! the text files of this repository. Other developers cannot open those documents, so a
//! file states the reason itself or links a committed document instead. tasty ran the
//! same check on the plugin files before they moved here.
//!
//! The ten patterns are listed in PATTERNS, and each find function describes its shape:
//! numbered TODO citations, numbered conductor tickets, paths into the local work folder,
//! dated design changelog slugs, TODO numbers in anchors, mentions of the local agent
//! folders, TODO in prose, zero-padded split numbers, dotted plan coordinates, and review
//! round numbers. The first six ignore case. Prose TODO is not checked in Markdown, so
//! documents can describe the rule. A task marker `TODO:` or `TODO(scope):` is allowed.
//!
//! Which files are read:
//! - In a Git checkout the list comes from Git, as NUL separated paths. Every tracked path
//!   is read from the index, and also from the work tree when that copy differs, because a
//!   commit takes the index. A tracked link is read as its target text from the index,
//!   whatever form the checkout gave it, and a work tree path through a linked folder is
//!   not read. Every untracked path that Git does not ignore is read too, except the local
//!   agent files and folders and anything under a folder marked as a build cache. Tracked
//!   paths are read wherever they sit, also under `dist` or a cache folder.
//! - Without Git, as in a source tarball, the tree is walked. The walk skips build,
//!   dependency, and editor folders, the local agent files and folders, and folders marked
//!   as build caches.
//!
//! Files with a binary extension and the vendored minified bundles are not read. A file
//! that is not UTF-8 is listed and fails the check, so a new text format is not skipped
//! quietly; a file that cannot be read fails the check with the error. Paths are compared
//! with the allowlist exactly as Git or the walk gives them, with `/` between folders.
//!
//! The home folder of the user holds a folder with the same name as a local agent folder.
//! A mention right after a home prefix or near a home word (HOME_NEARBY) is allowed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use plugin_guards::repo_root;
use tasty_doc_guards::git_env;
use tasty_doc_guards::temp_scratch::Scratch;

/// This file, which holds the pattern descriptions and the synthetic inputs.
const SELF_KEY: &str = "crates/plugin-guards/tests/no_local_doc_citation.rs";

/// (exact path, pattern ids it may hold). Only the named patterns are allowed, not the
/// whole file. `.gitignore` has to name the local folders it leaves out. The synthetic
/// inputs of the patterns not listed for this file are assembled with fx!.
const ALLOWLIST: &[(&str, &[&str])] = &[
    (".gitignore", &["local-folder"]),
    (
        SELF_KEY,
        &[
            "prose-todo",
            "split-number",
            "plan-coordinate",
            "round-number",
        ],
    ),
];

/// (id, description, finder). Every finder runs on every line.
type Finder = fn(&str) -> Option<String>;
const PATTERNS: &[(&str, &str, Finder)] = &[
    ("todo-number", "numbered TODO citation", find_todo_number),
    (
        "conductor-ticket",
        "numbered conductor ticket",
        find_conductor_ticket,
    ),
    (
        "workspace-subdir",
        "path into the local work folder",
        find_workspace_subdir,
    ),
    ("dated-slug", "dated design changelog slug", find_dated_slug),
    (
        "anchor-number",
        "TODO number in an anchor",
        find_anchor_number,
    ),
    ("local-folder", "local agent folder", find_local_folder),
    ("prose-todo", "TODO in prose", find_prose_todo),
    (
        "split-number",
        "zero-padded split number",
        find_split_number,
    ),
    (
        "plan-coordinate",
        "dotted plan coordinate",
        find_plan_coordinate,
    ),
    ("round-number", "review round number", find_round_number),
];

/// File and folder names the walk skips, and that an untracked path may not pass
/// through. A worktree's `.git` can be a file, so names are checked before kinds.
/// AGENTS.md is a local agent instruction file whose name the tools require.
const PRUNE_NAMES: &[&str] = &[
    "target",
    "dist",
    ".worktree",
    ".git",
    ".idea",
    ".serena",
    ".playwright-mcp",
    "node_modules",
    "AGENTS.md",
];

/// Vendored minified bundles, left out one by one: a pattern can match by chance in a
/// large bundle, and the folder also holds files written here.
const VENDORED_FILES: &[&str] = &[
    "crates/tasty-plugin-markdown/assets/highlight.min.js",
    "crates/tasty-plugin-markdown/assets/katex.min.css",
    "crates/tasty-plugin-markdown/assets/katex.min.js",
    "crates/tasty-plugin-markdown/assets/mermaid.min.js",
];

/// Pieces of the local folder names, kept apart so that this file does not spell them.
const LOCAL_HEAD: &str = "claude";
const LOCAL_TAIL: &str = "-workspace";

/// The local work folder name, without the leading dot.
fn ws_dir() -> String {
    format!("{LOCAL_HEAD}{LOCAL_TAIL}")
}

/// (local folder name, needs a leading dot). The longer name comes first because the
/// names share a prefix. The short name also exists in the home folder, so it counts
/// only with its dot and outside a home context.
fn local_dirs() -> Vec<(String, bool)> {
    vec![(ws_dir(), false), (LOCAL_HEAD.to_string(), true)]
}

fn is_pruned(name: &str) -> bool {
    PRUNE_NAMES.contains(&name)
        || name
            .strip_prefix('.')
            .is_some_and(|rest| rest == LOCAL_HEAD || rest == ws_dir())
}

/// A CARGO_TARGET_DIR under another name is pruned by its cache marker.
fn is_pruned_dir(path: &Path, name: &str) -> bool {
    is_pruned(name) || tasty_doc_guards::is_build_cache_dir(path)
}

/// Sub folders of the local work folder that the path pattern reports.
const FORBIDDEN_SUBDIRS: &[&str] = &["todo-conductor", "todo", "plans", "conductor"];

/// Home prefixes right before the folder name, one path separator run stripped.
const HOME_PREFIXES: &[&str] = &["~", "$home", "%userprofile%"];

/// Words that mark a home context in the short window before the name. Word boundaries
/// are checked so that a longer word such as Homebrew does not count.
const HOME_NEARBY: &[&str] = &["home", "claude_config_dir", "홈의"];

/// Size of that window, in characters.
const HOME_WINDOW: usize = 32;

fn skip_run(bytes: &[u8], mut i: usize, allowed: &[u8]) -> usize {
    while i < bytes.len() && allowed.contains(&bytes[i]) {
        i += 1;
    }
    i
}

/// End of the run of digits that starts at `i`, if one does.
fn digits_end(bytes: &[u8], i: usize) -> Option<usize> {
    if i >= bytes.len() || !bytes[i].is_ascii_digit() {
        return None;
    }
    let mut end = i;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    Some(end)
}

/// Korean ordinal suffix written after a number.
const KOREAN_ORDINAL: &str = "번";

/// A number in matching brackets, with an optional ordinal suffix. Only a closed pair
/// counts as a numbered group.
fn bracketed_number_end(line: &str, i: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let close = match bytes.get(i)? {
        b'(' => b')',
        b'[' => b']',
        _ => return None,
    };
    let num_start = skip_run(bytes, i + 1, b" \t");
    let after_digits = digits_end(bytes, num_start)?;
    let after_ordinal = match line[after_digits..].strip_prefix(KOREAN_ORDINAL) {
        Some(_) => after_digits + KOREAN_ORDINAL.len(),
        None => after_digits,
    };
    let end = skip_run(bytes, after_ordinal, b" \t");
    (bytes.get(end) == Some(&close)).then_some(end + 1)
}

/// `todo` followed by a number, or a number with the ordinal suffix followed by `todo`.
/// The number may follow spaces, tabs, slashes, and one hyphen, or sit in a closed pair
/// of brackets. A number is required, so task markers and identifiers do not match.
fn find_todo_number(line: &str) -> Option<String> {
    // ASCII lowercasing keeps byte offsets, so the original text can be reported.
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("todo") {
        let start = from + pos;
        let after_space = skip_run(bytes, start + 4, b" \t/");
        let mut i = after_space;
        if i < bytes.len() && bytes[i] == b'-' {
            i += 1;
        }
        if let Some(end) = digits_end(bytes, i) {
            return Some(line[start..end].to_string());
        }
        if let Some(end) = bracketed_number_end(line, after_space) {
            return Some(line[start..end].to_string());
        }
        if let Some(num_start) = korean_ordinal_start(line, start) {
            return Some(line[num_start..start + 4].to_string());
        }
        from = start + 4;
    }
    None
}

/// Whether the text before `todo_at` ends in digits, the ordinal suffix, and blanks.
/// Returns where the digits start.
fn korean_ordinal_start(line: &str, todo_at: usize) -> Option<usize> {
    let head = line[..todo_at].trim_end_matches([' ', '\t']);
    let digits = head.strip_suffix(KOREAN_ORDINAL)?;
    let num_start = digits.len()
        - digits
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .count();
    (num_start < digits.len()).then_some(num_start)
}

/// `todo-conductor` in any case, a run of separators, and a number.
fn find_conductor_ticket(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let needle = "todo-conductor";
    let mut from = 0;
    while let Some(pos) = lower[from..].find(needle) {
        let start = from + pos;
        let i = skip_run(bytes, start + needle.len(), b" \t/_-");
        if let Some(end) = digits_end(bytes, i) {
            return Some(line[start..end].to_string());
        }
        from = start + needle.len();
    }
    None
}

/// The local work folder followed by a path separator and one of the forbidden sub
/// folders. Case and repeated separators do not hide it. A sub folder is matched by its
/// prefix.
fn find_workspace_subdir(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let dir = ws_dir();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(&dir) {
        let start = from + pos;
        from = start + dir.len();
        let after = skip_run(bytes, from, b"/\\");
        if after == from {
            continue; // No separator after the name: not a path into it.
        }
        if let Some(sub) = FORBIDDEN_SUBDIRS
            .iter()
            .find(|s| lower[after..].starts_with(**s))
        {
            return Some(format!("{dir}/{sub}"));
        }
    }
    None
}

/// A date followed by a hyphen and a word (`YYYY-MM-DD-word`), in any case. A digit run
/// longer than four is not read from its middle as a year.
fn find_dated_slug(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let is_d = |i: usize| i < bytes.len() && bytes[i].is_ascii_digit();
    let is_dash = |i: usize| i < bytes.len() && bytes[i] == b'-';
    for start in 0..bytes.len() {
        // A digit before means this is the middle of a longer number.
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            continue;
        }
        if !(is_d(start) && is_d(start + 1) && is_d(start + 2) && is_d(start + 3)) {
            continue;
        }
        if !(is_dash(start + 4) && is_d(start + 5) && is_d(start + 6)) {
            continue;
        }
        if !(is_dash(start + 7) && is_d(start + 8) && is_d(start + 9)) {
            continue;
        }
        if !is_dash(start + 10) {
            continue;
        }
        let mut end = start + 11;
        if !(end < bytes.len() && bytes[end].is_ascii_alphabetic()) {
            continue;
        }
        while end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'-') {
            end += 1;
        }
        return Some(line[start..end].to_string());
    }
    None
}

/// In a lowercased anchor, `todo-` at the start or after a hyphen, then a number.
fn slug_todo_number(slug: &str) -> Option<String> {
    let bytes = slug.as_bytes();
    let needle = "todo-";
    let mut from = 0;
    while let Some(pos) = slug[from..].find(needle) {
        let start = from + pos;
        // A continuation byte of a multi-byte character never equals `-`.
        let at_boundary = start == 0 || bytes[start - 1] == b'-';
        let digits_start = skip_run(bytes, start + needle.len(), b"-");
        from = start + needle.len();
        if !at_boundary {
            continue;
        }
        if let Some(end) = digits_end(bytes, digits_start) {
            return Some(slug[start..end].to_string());
        }
    }
    None
}

/// A TODO number inside a Markdown anchor (`#slug`).
fn find_anchor_number(line: &str) -> Option<String> {
    let mut from = 0;
    while let Some(pos) = line[from..].find('#') {
        let start = from + pos;
        let rest = &line[start + 1..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | '`' | ',' | '"' | '\''))
            .map(|i| start + 1 + i)
            .unwrap_or(line.len());
        let slug = &line[start + 1..end];
        if let Some(hit) = slug_todo_number(&slug.to_ascii_lowercase()) {
            return Some(format!("#{slug} ({hit})"));
        }
        from = start + 1;
    }
    None
}

/// Looks only right before the name, so that a home path does not excuse another
/// mention on the same line.
fn home_context_before(lower: &str, at: usize) -> bool {
    let head = &lower[..at];
    // Skip escaped or repeated separators.
    let trimmed = head.trim_end_matches(['/', '\\']);
    if HOME_PREFIXES.iter().any(|p| trimmed.ends_with(p)) {
        return true;
    }
    let window = head
        .char_indices()
        .rev()
        .take(HOME_WINDOW)
        .last()
        .map_or(head, |(i, _)| &head[i..]);
    HOME_NEARBY.iter().any(|a| contains_word(window, a))
}

/// Whether `word` occurs in `hay` as a word: an ASCII letter or digit on either side
/// makes it part of another word.
fn contains_word(hay: &str, word: &str) -> bool {
    let bytes = hay.as_bytes();
    let mut from = 0;
    while let Some(pos) = hay[from..].find(word) {
        let start = from + pos;
        from = start + word.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = bytes.get(from).is_none_or(|c| !c.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// A mention of a local agent folder, with or without a sub path.
fn find_local_folder(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for (name, needs_dot) in local_dirs() {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(&name) {
            let start = from + pos;
            from = start + name.len();
            let dotted = start > 0 && bytes[start - 1] == b'.';
            if needs_dot && !dotted {
                continue; // The short name counts only as a path.
            }
            // An identifier character before the name (or its dot) makes it part of a
            // longer name, such as a reverse-DNS plugin id.
            let prev = if dotted {
                start.checked_sub(2)
            } else {
                start.checked_sub(1)
            };
            if let Some(p) = prev {
                let c = bytes[p];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                    continue;
                }
            }
            // An identifier character after it makes it another name. The longer name is
            // checked first, so the local work folder is caught before this.
            if let Some(&c) = bytes.get(from)
                && (c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                continue;
            }
            if needs_dot && home_context_before(&lower, start - 1) {
                continue;
            }
            return Some(if dotted {
                format!(".{name}")
            } else {
                name.clone()
            });
        }
    }
    None
}

/// The index after the colon of `TODO:` or `TODO(scope):`. A colon is required so that
/// prose with only a closing bracket is not taken for a task marker.
fn todo_marker_end(line: &str, after_todo: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let colon_at = match bytes.get(after_todo)? {
        b':' => return Some(after_todo + 1),
        b'(' => after_todo + 1 + line[after_todo + 1..].find(')')? + 1,
        _ => return None,
    };
    (bytes.get(colon_at) == Some(&b':')).then_some(colon_at + 1)
}

/// Upper case TODO that is not a task marker. Lower case is left out: it is hard to tell
/// from ordinary words and identifiers.
fn find_prose_todo(line: &str) -> Option<String> {
    let mut from = 0;
    while let Some(pos) = line[from..].find("TODO") {
        let start = from + pos;
        let after = start + 4;
        from = after;
        if todo_marker_end(line, after).is_none() {
            return Some(line[start..after].to_string());
        }
    }
    None
}

/// Two digits in brackets whose first digit is 0 and second is not. Other numbers are
/// hard to tell from footnotes, data, and versions.
fn find_split_number(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(pos) = line[from..].find('(') {
        let start = from + pos;
        from = start + 1;
        let d0 = bytes.get(start + 1);
        let d1 = bytes.get(start + 2);
        if d0 == Some(&b'0')
            && d1.is_some_and(|c| c.is_ascii_digit() && *c != b'0')
            && bytes.get(start + 3) == Some(&b')')
        {
            return Some(line[start..start + 4].to_string());
        }
    }
    None
}

/// Upper case letter, number, upper case letter, upper case letter, joined by dots. Only
/// the first four items are read when more follow. A version made of numbers only is not
/// a coordinate.
fn find_plan_coordinate(line: &str) -> Option<String> {
    let b = line.as_bytes();
    for i in 0..b.len() {
        // A word character before means this is not the head of a coordinate.
        if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.') {
            continue;
        }
        if !b[i].is_ascii_uppercase() || b.get(i + 1) != Some(&b'.') {
            continue;
        }
        let mut j = i + 2;
        let ds = j;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j == ds || b.get(j) != Some(&b'.') {
            continue;
        }
        j += 1;
        if !b.get(j).is_some_and(u8::is_ascii_uppercase) || b.get(j + 1) != Some(&b'.') {
            continue;
        }
        j += 2;
        if b.get(j).is_some_and(u8::is_ascii_uppercase) {
            return Some(line[i..=j].to_string());
        }
    }
    None
}

/// Upper case R followed by two or more digits, not inside an identifier. One digit and
/// Korean round words are left out: they are hard to tell from data and counts. Removing
/// the number is not enough; write the reason it stood for.
fn find_round_number(line: &str) -> Option<String> {
    let b = line.as_bytes();
    for i in 0..b.len() {
        if b[i] != b'R' {
            continue;
        }
        if i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') {
            continue;
        }
        let mut j = i + 1;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j - (i + 1) < 2 {
            continue;
        }
        if b.get(j)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            continue;
        }
        return Some(line[i..j].to_string());
    }
    None
}

/// Markdown may describe the rule with TODO, so only the prose pattern skips it.
fn out_of_scope(id: &str, key: &str) -> bool {
    id == "prose-todo" && key.to_ascii_lowercase().ends_with(".md")
}

/// Everything except the vendored bundles and binary extensions is read, files without
/// an extension included.
fn is_scan_target(key: &str) -> bool {
    if VENDORED_FILES.contains(&key) {
        return false;
    }
    let name = key.rsplit('/').next().unwrap_or("");
    // A leading dot is not an extension separator: a dotfile has no extension.
    let ext = name
        .trim_start_matches('.')
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase());
    match ext {
        Some(e) => !tasty_doc_guards::is_binary_artifact_ext(&e),
        None => true,
    }
}

/// The patterns allowed in the file at exactly this path, not a path sharing a prefix or
/// a suffix with it.
fn allowed_patterns(key: &str) -> &'static [&'static str] {
    ALLOWLIST
        .iter()
        .find(|(f, _)| *f == key)
        .map_or(&[], |(_, pats)| *pats)
}

/// (id, report) for each pattern found in `line` of the file at `key`, after the
/// allowlist and the Markdown rule.
fn violations_in_line(key: &str, line: &str) -> Vec<(&'static str, String)> {
    let allowed = allowed_patterns(key);
    PATTERNS
        .iter()
        .filter(|(id, _, _)| !allowed.contains(id) && !out_of_scope(id, key))
        .filter_map(|(id, kind, find)| find(line).map(|m| (*id, format!("{kind}: `{m}`"))))
        .collect()
}

/// Where a text came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The staged copy of a tracked path.
    Index,
    /// The work tree copy of a tracked path, read when it differs from the index.
    WorkTree,
    /// An untracked path that Git does not ignore.
    Untracked,
    /// A file found by the walk outside Git.
    Walked,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Index => "index",
            Source::WorkTree => "work tree",
            Source::Untracked => "untracked, not committed",
            Source::Walked => "file",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Finding {
    key: String,
    source: Source,
    line: usize,
    id: &'static str,
    report: String,
}

#[derive(Default)]
struct Report {
    findings: Vec<Finding>,
    /// Paths that passed the filters but are not UTF-8.
    undecodable: Vec<String>,
    /// Paths that passed the filters.
    scanned: Vec<String>,
}

/// A path to read, as Git or the walk gave it.
enum Candidate {
    Tracked {
        key: String,
        mode: String,
        oid: String,
    },
    Untracked {
        key: String,
    },
    Walked {
        key: String,
        path: PathBuf,
    },
}

impl Candidate {
    fn key(&self) -> &str {
        match self {
            Candidate::Tracked { key, .. }
            | Candidate::Untracked { key }
            | Candidate::Walked { key, .. } => key,
        }
    }
}

/// The path of `key` under `root`, built from its parts so that a backslash inside a
/// name stays part of that name.
fn path_of(root: &Path, key: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in key.split('/') {
        path.push(part);
    }
    path
}

/// Whether a part of `key` is a link in the work tree. Stops at the first part that does
/// not exist.
fn has_link_part(root: &Path, key: &str) -> bool {
    let mut path = root.to_path_buf();
    for part in key.split('/') {
        path.push(part);
        match path.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

fn run_git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = git_env::command("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", root.display()))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn key_from_git(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        format!(
            "a path from Git is not UTF-8 and cannot be matched with the allowlist: {}",
            String::from_utf8_lossy(bytes)
        )
    })
}

/// The tracked and untracked paths of the Git checkout at `root`.
fn git_candidates(root: &Path) -> Result<Vec<Candidate>, String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    // NUL separated output keeps every name as it is; the line output would quote
    // names with non-ASCII bytes.
    for record in run_git(root, &["ls-files", "-z", "--stage"])?.split(|b| *b == 0) {
        if record.is_empty() {
            continue;
        }
        // `<mode> <object> <stage>\t<path>`
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| format!("unexpected ls-files record: {record:?}"))?;
        let meta = String::from_utf8_lossy(&record[..tab]).into_owned();
        let mut fields = meta.split(' ');
        let (Some(mode), Some(oid)) = (fields.next(), fields.next()) else {
            return Err(format!("unexpected ls-files record: {meta}"));
        };
        let key = key_from_git(&record[tab + 1..])?;
        // A conflicted path has more than one stage; the first is enough to read it.
        if seen.insert(key.clone()) {
            out.push(Candidate::Tracked {
                key,
                mode: mode.to_string(),
                oid: oid.to_string(),
            });
        }
    }
    let untracked = run_git(root, &["ls-files", "-z", "--others", "--exclude-standard"])?;
    for record in untracked.split(|b| *b == 0) {
        if !record.is_empty() {
            out.push(Candidate::Untracked {
                key: key_from_git(record)?,
            });
        }
    }
    Ok(out)
}

/// Whether an untracked path is a local agent file or folder, or sits under a folder
/// marked as a build cache. Those depend on the machine, not on the repository.
fn is_local_untracked(root: &Path, key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| is_pruned(p)) {
        return true;
    }
    let mut dir = root.to_path_buf();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        dir.push(part);
        if tasty_doc_guards::is_build_cache_dir(&dir) {
            return true;
        }
    }
    false
}

/// The relative path of `path` under `root`, with its parts joined by `/`.
fn walked_key(path: &Path, root: &Path) -> Result<String, String> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside {}", path.display(), root.display()))?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(
                s.to_str()
                    .ok_or_else(|| format!("a path is not UTF-8: {}", path.display()))?
                    .to_string(),
            ),
            _ => return Err(format!("unexpected path part in {}", path.display())),
        }
    }
    Ok(parts.join("/"))
}

/// Walks the tree outside Git. Links are followed. A folder or entry that cannot be read
/// fails the walk, so a sub tree is not dropped quietly.
fn gather(path: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    if path.is_file() {
        out.push((walked_key(path, root)?, path.to_path_buf()));
        return Ok(());
    }
    let entries =
        std::fs::read_dir(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("cannot read an entry of {}: {e}", path.display()))?;
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // A worktree's .git can be a file, so the name is checked before the kind.
        if is_pruned(name) {
            continue;
        }
        if p.is_dir() && is_pruned_dir(&p, name) {
            continue;
        }
        gather(&p, root, out)?;
    }
    Ok(())
}

fn walked_candidates(root: &Path) -> Result<Vec<Candidate>, String> {
    let mut found = Vec::new();
    gather(root, root, &mut found)?;
    Ok(found
        .into_iter()
        .map(|(key, path)| Candidate::Walked { key, path })
        .collect())
}

/// The blobs named by `oids`, read with one `git cat-file --batch`.
fn read_blobs(root: &Path, oids: &[String]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut blobs = BTreeMap::new();
    if oids.is_empty() {
        return Ok(blobs);
    }
    let mut child = git_env::command("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git cat-file in {}: {e}", root.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin for git cat-file")?;
    let mut stdout = child.stdout.take().ok_or("no stdout for git cat-file")?;
    let mut request = String::new();
    for oid in oids {
        request.push_str(oid);
        request.push('\n');
    }
    let mut raw = Vec::new();
    let read = std::thread::scope(|s| {
        // Write from another thread so that a full output pipe cannot block the input.
        let writer = s.spawn(move || stdin.write_all(request.as_bytes()));
        let read = stdout.read_to_end(&mut raw);
        let written = writer.join();
        (read, written)
    });
    match read {
        (Ok(_), Ok(Ok(()))) => {}
        (Err(e), _) | (_, Ok(Err(e))) => return Err(format!("git cat-file failed: {e}")),
        (_, Err(_)) => return Err("git cat-file writer panicked".to_string()),
    }
    let status = child
        .wait()
        .map_err(|e| format!("git cat-file failed: {e}"))?;
    if !status.success() {
        return Err(format!("git cat-file failed in {}", root.display()));
    }
    // `<oid> blob <size>\n<content>\n` for each request.
    let mut at = 0;
    for oid in oids {
        let nl = raw[at..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("short git cat-file output")?;
        let header = String::from_utf8_lossy(&raw[at..at + nl]).into_owned();
        at += nl + 1;
        let mut fields = header.split(' ');
        let (Some(got), Some(_kind), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("cannot read object {oid}: {header}"));
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("cannot read object {oid}: {header}"))?;
        if got != oid || at + size > raw.len() {
            return Err(format!("cannot read object {oid}: {header}"));
        }
        blobs.insert(oid.clone(), raw[at..at + size].to_vec());
        at += size + 1;
    }
    Ok(blobs)
}

/// CRLF and LF copies of the same text count as the same body.
fn same_text(a: &[u8], b: &[u8]) -> bool {
    let strip = |v: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(v.len());
        let mut i = 0;
        while i < v.len() {
            if v[i] == b'\r' && v.get(i + 1) == Some(&b'\n') {
                i += 1;
                continue;
            }
            out.push(v[i]);
            i += 1;
        }
        out
    };
    strip(a) == strip(b)
}

/// The bodies to read: (path, where from, bytes).
fn read_bodies(
    root: &Path,
    candidates: &[Candidate],
) -> Result<Vec<(String, Source, Vec<u8>)>, String> {
    let oids: Vec<String> = candidates
        .iter()
        .filter_map(|c| match c {
            // A gitlink names a commit of another repository, not a text.
            Candidate::Tracked { mode, oid, .. } if mode != "160000" => Some(oid.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let blobs = read_blobs(root, &oids)?;
    let mut bodies = Vec::new();
    for candidate in candidates {
        match candidate {
            Candidate::Tracked { key, mode, oid } => {
                if mode == "160000" {
                    continue;
                }
                let staged = blobs
                    .get(oid)
                    .ok_or_else(|| format!("cannot read {key} from the index"))?;
                // A commit takes the index, so the staged copy is always read. A link is
                // read only from there: its text is the link target, whatever form the
                // checkout gave it.
                let regular = mode == "100644" || mode == "100755";
                let path = path_of(root, key);
                if regular && !has_link_part(root, key) && path.symlink_metadata().is_ok() {
                    let work = std::fs::read(&path)
                        .map_err(|e| format!("cannot read {key} from the work tree: {e}"))?;
                    if !same_text(&work, staged) {
                        bodies.push((key.clone(), Source::WorkTree, work));
                    }
                }
                bodies.push((key.clone(), Source::Index, staged.clone()));
            }
            Candidate::Untracked { key } => {
                if is_local_untracked(root, key) {
                    continue;
                }
                let path = path_of(root, key);
                // A link, or a nested repository listed as a folder, is not a text here.
                let is_file = path
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_file());
                if !is_file {
                    continue;
                }
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("cannot read untracked {key}: {e}"))?;
                bodies.push((key.clone(), Source::Untracked, bytes));
            }
            Candidate::Walked { key, path } => {
                let bytes = std::fs::read(path).map_err(|e| format!("cannot read {key}: {e}"))?;
                bodies.push((key.clone(), Source::Walked, bytes));
            }
        }
    }
    Ok(bodies)
}

/// Reads the tree at `root`: from Git when it is a Git checkout, by walking otherwise.
fn scan(root: &Path) -> Result<Report, String> {
    let candidates = if root.join(".git").exists() {
        git_candidates(root)?
    } else {
        walked_candidates(root)?
    };
    let candidates: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| is_scan_target(c.key()))
        .collect();
    let mut report = Report::default();
    for (key, source, bytes) in read_bodies(root, &candidates)? {
        if !report.scanned.contains(&key) {
            report.scanned.push(key.clone());
        }
        let Ok(text) = String::from_utf8(bytes) else {
            if !report.undecodable.contains(&key) {
                report.undecodable.push(key);
            }
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            for (id, found) in violations_in_line(&key, line) {
                report.findings.push(Finding {
                    key: key.clone(),
                    source,
                    line: i + 1,
                    id,
                    report: found,
                });
            }
        }
    }
    report.scanned.sort();
    Ok(report)
}

/// Top folders that must each give at least one file, and root files that must be read.
/// A count alone would not show which part of the tree a walk lost.
const MUST_BE_SCANNED: &[&str] = &[
    "crates/",
    "docs/",
    "scripts/",
    ".github/",
    ".githooks/",
    ".cargo/",
    "LICENSES/",
    "CLAUDE.md",
    "README.md",
];

/// Files read: 313 measured on 2026-11-23 at 6c22565 before this check was added (256
/// under crates/, 21 under scripts/, 11 under docs/, 11 at the root). The largest crates
/// give 64 (claude), 55 (codex), and 28 (markdown). Losing any of the three fails;
/// losing one of the smaller crates does not.
const MIN_SCANNED: usize = 290;

/// The parts of MUST_BE_SCANNED that `scanned` does not reach.
fn missing_branches(scanned: &[String]) -> Vec<&'static str> {
    MUST_BE_SCANNED
        .iter()
        .copied()
        .filter(|want| {
            !scanned.iter().any(|k| {
                if want.ends_with('/') {
                    k.starts_with(want)
                } else {
                    k == want
                }
            })
        })
        .collect()
}

fn format_findings(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| {
            format!(
                "  {}:{} [{}] {} {}",
                f.key,
                f.line,
                f.source.label(),
                f.id,
                f.report
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_local_doc_citation() {
    let root = repo_root();
    let report = scan(&root).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        report.undecodable.is_empty(),
        "these files are not UTF-8, so the citation check cannot read them. If the format \
         is binary, add its extension to the binary list of tasty-doc-guards; otherwise \
         save the file as UTF-8: {:?}",
        report.undecodable
    );
    assert!(
        report.findings.is_empty(),
        "a file cites a local work document, a plan number, or a design changelog entry \
         that is not in the repository. Write the reason itself or link a committed \
         document; fix a link together with the heading it points at. Task markers \
         `TODO:` and `TODO(scope):` are allowed. A file that needs one of these shapes \
         goes into ALLOWLIST with its path and the pattern ids. A path marked untracked is \
         not committed; clean it up or keep it out of the repository folder:\n{}",
        format_findings(&report.findings)
    );
}

#[test]
fn the_scan_reaches_every_major_branch_of_the_tree() {
    let report = scan(&repo_root()).unwrap_or_else(|e| panic!("{e}"));
    let missing = missing_branches(&report.scanned);
    assert!(
        missing.is_empty() && report.scanned.len() >= MIN_SCANNED,
        "read {} files (floor {MIN_SCANNED}); nothing was read from {missing:?}. Check the \
         Git list, the filters, and the pruned names. Do not lower the floor or drop a \
         path without a change in what the check should read.",
        report.scanned.len()
    );
}

#[test]
fn the_floor_and_branches_reject_a_partial_scan() {
    let full: Vec<String> = MUST_BE_SCANNED
        .iter()
        .map(|p| {
            if p.ends_with('/') {
                format!("{p}x")
            } else {
                (*p).to_string()
            }
        })
        .collect();
    assert!(missing_branches(&full).is_empty());
    let without_docs: Vec<String> = full
        .iter()
        .filter(|k| !k.starts_with("docs/"))
        .cloned()
        .collect();
    assert_eq!(missing_branches(&without_docs), vec!["docs/"]);
    let only_crates: Vec<String> = vec!["crates/x".to_string()];
    assert!(missing_branches(&only_crates).contains(&"CLAUDE.md"));
}

/// Assembles a synthetic input of a pattern that this file may not hold, so that the
/// source of this check does not report itself.
macro_rules! fx {
    ($($p:literal),+ $(,)?) => { concat!($($p),+) };
}

/// The ten pattern ids, in table order.
const IDS: &[&str] = &[
    "todo-number",
    "conductor-ticket",
    "workspace-subdir",
    "dated-slug",
    "anchor-number",
    "local-folder",
    "prose-todo",
    "split-number",
    "plan-coordinate",
    "round-number",
];

/// One line per pattern that this pattern alone reports. The numbers, slug, coordinate,
/// and file name are made up for this check.
fn one_line_per_pattern() -> Vec<(&'static str, String)> {
    vec![
        ("todo-number", fx!("see todo", " 7391").to_string()),
        (
            "conductor-ticket",
            fx!("todo-conductor", "/6047").to_string(),
        ),
        // The identifier prefix keeps the folder mention pattern out of this line.
        (
            "workspace-subdir",
            fx!("x", "claude", "-workspace/todo", "/sample.md").to_string(),
        ),
        ("dated-slug", fx!("2001-02", "-03-sample-slug").to_string()),
        // The doubled hyphen keeps the numbered TODO pattern out of this line.
        (
            "anchor-number",
            fx!("[x](y.md#a-todo", "--8213)").to_string(),
        ),
        (
            "local-folder",
            fx!("the folder .", "claude", " holds it").to_string(),
        ),
        ("prose-todo", "a TODO in prose".to_string()),
        ("split-number", "x (04) y".to_string()),
        ("plan-coordinate", "see Q.7.X.Y here".to_string()),
        ("round-number", "see R7391 here".to_string()),
    ]
}

#[test]
fn the_table_holds_the_ten_patterns_and_each_one_reports() {
    let ids: Vec<&str> = PATTERNS.iter().map(|(id, _, _)| *id).collect();
    assert_eq!(ids, IDS, "the pattern table changed");
    for (id, line) in one_line_per_pattern() {
        let found: Vec<&str> = violations_in_line("crates/x/src/lib.rs", &line)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            found,
            vec![id],
            "the line for {id} reported {found:?}: {line}"
        );
    }
}

#[test]
fn todo_number_catches_numbered_citations_only() {
    let cases: &[(&str, Option<&str>)] = &[
        (fx!("see TODO", " 7391"), Some(fx!("TODO", " 7391"))),
        (fx!("(TODO", "7391)"), Some(fx!("TODO", "7391"))),
        (
            fx!("TODO", "-6047 is in the list"),
            Some(fx!("TODO", "-6047")),
        ),
        (fx!("see TODO", "  7391"), Some(fx!("TODO", "  7391"))),
        (fx!("see TODO", " \t 7391"), Some(fx!("TODO", " \t 7391"))),
        (fx!("see TODO", " -7391"), Some(fx!("TODO", " -7391"))),
        ("// TODO: refactor this later", None),
        ("TODOS are not numbered", None),
        (fx!("8213번 ", "TODO", " — x"), Some(fx!("8213번 ", "TODO"))),
        (fx!("(9175번", "TODO", ")"), Some(fx!("9175번", "TODO"))),
        (fx!("이번 ", "TODO", " 는 크다"), None),
        (fx!("번 ", "TODO"), None),
        (fx!("TODO: ", "7391"), None),
        (fx!("TODO. ", "7391"), None),
        (fx!("TODO #", "7391"), None),
        (fx!("TODO_", "7391"), None),
    ];
    for (line, want) in cases {
        assert_eq!(find_todo_number(line).as_deref(), *want, "{line}");
    }
}

#[test]
fn todo_number_is_not_evaded_by_case() {
    let cases: &[(&str, Option<&str>)] = &[
        (fx!("see todo", " 7391"), Some(fx!("todo", " 7391"))),
        (fx!("ToDo", "-6047"), Some(fx!("ToDo", "-6047"))),
        (fx!("(todo", "8213)"), Some(fx!("todo", "8213"))),
        (fx!("see Todo", " 7391"), Some(fx!("Todo", " 7391"))),
        ("// todo: refactor this later", None),
        ("// todo(scope): replace it later", None),
        ("let todos = todo_marker_end(line, 4);", None),
        (fx!("todo_", "7391"), None),
    ];
    for (line, want) in cases {
        assert_eq!(find_todo_number(line).as_deref(), *want, "{line}");
    }
}

#[test]
fn todo_number_takes_the_slash_as_a_separator() {
    let cases: &[(&str, Option<&str>)] = &[
        (fx!("(todo", "/6047 R2)"), Some(fx!("todo", "/6047"))),
        (fx!("TODO", "/9175"), Some(fx!("TODO", "/9175"))),
        (fx!("x/todo", "//8213.md"), Some(fx!("todo", "//8213"))),
        ("TODO/changelog either way", None),
        ("note/todo/abstract/quote", None),
    ];
    for (line, want) in cases {
        assert_eq!(find_todo_number(line).as_deref(), *want, "{line}");
    }
}

#[test]
fn todo_number_catches_bracketed_numbers() {
    let cases: &[(&str, Option<&str>)] = &[
        (fx!("the TODO", "(7391) list"), Some(fx!("TODO", "(7391)"))),
        (
            fx!("the TODO", "(6047번) list"),
            Some(fx!("TODO", "(6047번)")),
        ),
        (fx!("TODO", "[8213] here"), Some(fx!("TODO", "[8213]"))),
        (fx!("TODO", "  ( 9175 )"), Some(fx!("TODO", "  ( 9175 )"))),
        (fx!("TODO", " (1999 onward"), None),
        (fx!("TODO", "(7391 here"), None),
        (fx!("TODO", "(7391]"), None),
        ("TODO(someone): fix it later", None),
        ("TODO(fixme)", None),
        ("TODO(v2): drop the cache", None),
        ("TODO(3rd)", None),
        ("TODO(0.1.2)", None),
    ];
    for (line, want) in cases {
        assert_eq!(find_todo_number(line).as_deref(), *want, "{line}");
    }
}

#[test]
fn conductor_ticket_catches_numbered_tickets() {
    let cases: &[(&str, Option<&str>)] = &[
        (
            fx!("todo-conductor", "/6047 here"),
            Some(fx!("todo-conductor", "/6047")),
        ),
        (
            fx!("TODO-CONDUCTOR", " 6047"),
            Some(fx!("TODO-CONDUCTOR", " 6047")),
        ),
        (
            fx!("todo-conductor", "//6047"),
            Some(fx!("todo-conductor", "//6047")),
        ),
        (
            fx!("todo-conductor", "  6047"),
            Some(fx!("todo-conductor", "  6047")),
        ),
        ("todo-conductor folder", None),
        (fx!("todo-conductor", "#6047"), None),
    ];
    for (line, want) in cases {
        assert_eq!(find_conductor_ticket(line).as_deref(), *want, "{line}");
    }
}

#[test]
fn workspace_subdir_catches_paths_into_the_folder() {
    let want = Some(fx!("claude", "-workspace/todo"));
    assert_eq!(
        find_workspace_subdir(fx!("claude", "-workspace/todo", "/sample.md")).as_deref(),
        want
    );
    assert_eq!(
        find_workspace_subdir(fx!("claude", "-workspace/Todo", "/sample.md")).as_deref(),
        want
    );
    assert_eq!(
        find_workspace_subdir(fx!("claude", "-workspace//todo", "/sample.md")).as_deref(),
        want
    );
    assert_eq!(
        find_workspace_subdir(fx!("claude", "-workspace/temp/x.png")),
        None
    );
}

#[test]
fn dated_slug_catches_date_and_word() {
    assert_eq!(
        find_dated_slug(fx!("the slug was 2001-02", "-03-sample-slug here")).as_deref(),
        Some(fx!("2001-02", "-03-sample-slug"))
    );
    assert_eq!(
        find_dated_slug(fx!("2001-02", "-03-Sample-Slug")).as_deref(),
        Some(fx!("2001-02", "-03-Sample-Slug"))
    );
    assert_eq!(find_dated_slug("Date: 2001-02-03"), None);
    // Six digits: the year is not read from the middle of a longer number.
    assert_eq!(find_dated_slug(fx!("id 120010-02", "-03-x")), None);
}

#[test]
fn anchor_number_catches_numbers_in_anchors() {
    assert!(find_anchor_number(fx!("[link](x.md#a-todo", "-7391-b)")).is_some());
    assert!(find_anchor_number(fx!("[link](x.md#a-TODO", "-7391-b)")).is_some());
    assert!(find_anchor_number(fx!("[link](x.md#a-todo", "--7391)")).is_some());
    assert_eq!(
        find_anchor_number("[link](x.md#todo-conductor-notes)"),
        None
    );
    assert_eq!(find_anchor_number(fx!("[link](x.md#todo", "7391)")), None);
    assert_eq!(find_anchor_number("# a plain heading"), None);
}

#[test]
fn local_folder_catches_mentions() {
    let ws = Some(fx!(".", "claude", "-workspace"));
    let short = Some(fx!(".", "claude"));
    assert_eq!(
        find_local_folder(fx!("output goes under .", "claude", "-workspace here")).as_deref(),
        ws
    );
    assert_eq!(
        find_local_folder(fx!("shots go to .", "claude", "-workspace/temp/ here")).as_deref(),
        ws
    );
    assert_eq!(
        find_local_folder(fx!("claude", "-workspace/temp is the place")).as_deref(),
        Some(fx!("claude", "-workspace"))
    );
    assert_eq!(
        find_local_folder(fx!(".", "CLAUDE", "-WORKSPACE/temp")).as_deref(),
        ws
    );
    assert_eq!(
        find_local_folder(fx!("the setting is in .", "claude", "/CLAUDE.md")).as_deref(),
        short
    );
    assert_eq!(
        find_local_folder(fx!("./.", "claude", "/x is read")).as_deref(),
        short
    );
    assert_eq!(
        find_local_folder(fx!(r".\.", "claude", r"\x is read")).as_deref(),
        short
    );
    assert_eq!(
        find_local_folder(fx!("the folder .", "claude", " alone")).as_deref(),
        short
    );
}

#[test]
fn local_folder_home_exemption_is_adjacent_not_line_wide() {
    for line in [
        "~/.claude/settings.json is merged",
        "$HOME/.claude/projects is read",
        "%USERPROFILE%\\.claude\\settings.json",
        "Ok(base.home_dir().join(\".claude\"))",
        "아니면 홈의 `.claude/projects`.",
        "$CLAUDE_CONFIG_DIR unset means .claude/projects",
    ] {
        assert_eq!(find_local_folder(line), None, "{line}");
    }
    let short = Some(fx!(".", "claude"));
    for line in [
        fx!("~/.tasty and .", "claude", "/CLAUDE.md are compared"),
        fx!("after Homebrew installs, .", "claude", "/CLAUDE.md changes"),
        fx!("renderHome() reads .", "claude", "/settings.json"),
        fx!("홈 화면 설정은 .", "claude", "/CLAUDE.md 가 정한다"),
    ] {
        assert_eq!(find_local_folder(line).as_deref(), short, "{line}");
    }
}

#[test]
fn local_folder_allows_identifiers_and_build_outputs() {
    for line in [
        "id = \"com.tasty.claude\"",
        "com.tasty.claude-design was removed",
        "crates/tasty-plugin-claude/tasty-plugin.toml.sig",
        "site/release.json is read",
        "target/release/tasty-plugin-claude",
        "notify_mod/claude/.claude-plugin/plugin.json",
    ] {
        assert_eq!(find_local_folder(line), None, "{line}");
    }
}

#[test]
fn prose_todo_catches_prose_but_not_task_markers() {
    let cases: &[(&str, bool)] = &[
        ("//! this TODO is only a move", true),
        ("// unlike the \"one `*` only\" line of the TODO page", true),
        ("/// (kept apart — a separate TODO if needed).", true),
        ("/// left as `TODO` in its own source", true),
        ("// noted as a separate TODO.", true),
        ("// later TODOs take the same path", true),
        ("// TODO: switch to the released version", false),
        ("// TODO(scope): replace after the change", false),
        ("// TODO(someone): upstream decides", false),
        ("// TODO: this TODO goes on below", true),
        ("// TODO(someone) later", true),
        ("// TODO fix later", true),
        ("let todo = 3; // todo list here", false),
        ("fn todo_marker() {}", false),
    ];
    for (line, caught) in cases {
        assert_eq!(find_prose_todo(line).is_some(), *caught, "{line}");
    }
}

#[test]
fn split_number_catches_a_zero_and_one_digit_in_brackets() {
    // The digit is generated over its whole range; no single value stands for a split.
    for d in 1..=9 {
        let alone = format!("(0{d})");
        assert_eq!(find_split_number(&alone), Some(alone.clone()));
        assert_eq!(
            find_split_number(&format!("see {alone} here")),
            Some(alone.clone())
        );
        for no in [
            format!("({d})"),
            format!("(1{d})"),
            format!("(0{d}{d})"),
            format!("0{d}"),
        ] {
            assert_eq!(find_split_number(&no), None, "{no}");
        }
    }
    assert_eq!(find_split_number("(00)"), None);
}

#[test]
fn plan_coordinate_catches_dotted_coordinates_only() {
    assert_eq!(
        find_plan_coordinate("refactor (Q.7.X.Y.1 step 1)").as_deref(),
        Some("Q.7.X.Y")
    );
    assert_eq!(
        find_plan_coordinate("the end of Q.7.X.Z.3.c").as_deref(),
        Some("Q.7.X.Z")
    );
    assert_eq!(
        find_plan_coordinate("see Q.7.X.Y.10.1 here").as_deref(),
        Some("Q.7.X.Y")
    );
    // A version made of numbers.
    assert_eq!(find_plan_coordinate("version 1.4.27"), None);
    // Every item a number.
    assert_eq!(find_plan_coordinate("1.2.3.4"), None);
    // The fourth item in lower case.
    assert_eq!(find_plan_coordinate("Q.7.X.y is not one"), None);
    // The second item not a number.
    assert_eq!(find_plan_coordinate("Q.W.X.Y form"), None);
    // A word character before the head.
    assert_eq!(find_plan_coordinate("xQ.7.X.Y.1"), None);
    // The third item in lower case.
    assert_eq!(find_plan_coordinate("Q.7.x.Y"), None);
}

#[test]
fn round_number_catches_two_or_more_digits() {
    // The smallest and largest value of each length, generated rather than chosen.
    for n in [10, 99, 100, 999, 1000, 9999] {
        let want = format!("R{n}");
        assert_eq!(
            find_round_number(&format!("see {want} here")).as_deref(),
            Some(want.as_str())
        );
        assert_eq!(
            find_round_number(&format!("({want})")).as_deref(),
            Some(want.as_str())
        );
        for no in [
            format!("let {want}x = 1;"),
            format!("xR{n}"),
            format!("_R{n}"),
            format!("r{n}"),
            format!("VAR{n}"),
        ] {
            assert_eq!(find_round_number(&no), None, "{no}");
        }
    }
    assert_eq!(find_round_number("R1 and R2"), None);
    assert_eq!(find_round_number("RGB12_FOO"), None);
}

#[test]
fn prose_todo_is_out_of_scope_in_markdown_only() {
    let prose = "a TODO in prose";
    for key in [
        "crates/x/src/lib.rs",
        "scripts/check.sh",
        ".github/workflows/ci.yml",
        "Makefile",
    ] {
        assert!(
            violations_in_line(key, prose)
                .iter()
                .any(|(id, _)| *id == "prose-todo"),
            "prose TODO was not caught in {key}"
        );
    }
    for key in [
        "CLAUDE.md",
        "docs/ci.md",
        "crates/tasty-plugin-markdown/assets/NOTICE.md",
    ] {
        assert!(
            violations_in_line(key, prose).is_empty(),
            "Markdown came into the prose TODO scope: {key}"
        );
    }
    assert!(
        violations_in_line("docs/ci.md", fx!("see TODO", " 7391"))
            .iter()
            .any(|(id, _)| *id == "todo-number")
    );
}

#[test]
fn scan_target_covers_scripts_ci_and_root_docs() {
    for key in [
        "scripts/check.sh",
        "CLAUDE.md",
        ".github/workflows/ci.yml",
        ".githooks/pre-commit",
        "crates/tasty-plugin-claude/lang/en.toml",
        "crates/tasty-plugin-markdown/assets/NOTICE.md",
    ] {
        assert!(is_scan_target(key), "{key} is not read");
    }
    for key in [
        "crates/tasty-plugin-claude/tasty-plugin.toml.sig",
        "crates/tasty-plugin-markdown/assets/fonts/KaTeX_Main-Regular.woff2",
        "crates/tasty-plugin-markdown/assets/katex.min.js",
    ] {
        assert!(!is_scan_target(key), "{key} is read");
    }
}

#[test]
fn our_own_files_under_assets_are_read() {
    let key = "crates/tasty-plugin-markdown/assets/NOTICE.md";
    assert!(is_scan_target(key));
    for part in key.split('/') {
        assert!(
            !is_pruned(part),
            "a part of {key} is pruned by name: {part}"
        );
    }
}

#[test]
fn prunes_build_outputs_and_local_dirs_but_not_assets() {
    assert!(is_pruned("target"));
    assert!(is_pruned("node_modules"));
    assert!(is_pruned("AGENTS.md"));
    assert!(!is_pruned("assets"));
    assert!(is_pruned(fx!(".", "claude")));
    assert!(is_pruned(fx!(".", "claude", "-workspace")));
    assert!(!is_pruned(fx!("claude")));
    assert!(!is_pruned("src"));
}

#[test]
fn allowlist_exempts_only_the_named_pattern_not_the_whole_file() {
    let planted = fx!("x/", "claude", "-workspace/todo", "/sample.md");
    let found = violations_in_line(".gitignore", planted);
    assert!(
        found.iter().any(|(id, _)| *id == "workspace-subdir"),
        "a pattern the allowlist does not name passed in an allowlisted file: {found:?}"
    );
    let folder = fx!(".", "claude");
    assert!(violations_in_line(".gitignore", folder).is_empty());
    assert!(
        violations_in_line("crates/x/src/lib.rs", folder)
            .iter()
            .any(|(id, _)| *id == "local-folder")
    );
}

#[test]
fn allowlist_paths_match_exactly_not_by_prefix_or_suffix() {
    assert!(allowed_patterns(".gitignore").contains(&"local-folder"));
    for key in ["docs/.gitignore", ".gitignore.bak", "x/.gitignore", ""] {
        assert!(allowed_patterns(key).is_empty(), "{key}");
    }
    assert!(!allowed_patterns(".gitignore").contains(&"workspace-subdir"));
}

/// Checks that each entry names an existing file and real pattern ids. Whether an entry
/// is still needed is a separate review.
#[test]
fn allowlist_entries_point_at_things_that_exist() {
    let root = repo_root();
    for (key, pats) in ALLOWLIST {
        assert!(
            path_of(&root, key).is_file(),
            "the allowlist names a missing file; remove or move the entry: {key}"
        );
        assert!(!pats.is_empty(), "an empty entry should be removed: {key}");
        for pat in *pats {
            assert!(IDS.contains(pat), "unknown pattern id for {key}: {pat}");
        }
    }
}

/// A Git repository in a scratch folder. Its ignore settings do not depend on the user:
/// the global exclude file is replaced with an empty one, and names are quoted in line
/// output, so a check reading line output would not see them as they are.
struct Repo {
    dir: Scratch,
}

impl Repo {
    fn new(what: &str) -> Self {
        let dir = Scratch::new(what);
        let repo = Repo { dir };
        repo.git(&["init", "-q"]);
        let empty = repo.root().join(".git").join("empty-excludes");
        std::fs::write(&empty, "").expect("write the empty exclude file");
        let empty = empty.to_string_lossy().replace('\\', "/");
        repo.git(&["config", "core.excludesFile", &empty]);
        repo.git(&["config", "core.quotePath", "true"]);
        repo
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        run_git(self.root(), args).unwrap_or_else(|e| panic!("{e}"))
    }

    fn write(&self, key: &str, text: &[u8]) {
        let path = path_of(self.root(), key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create a folder");
        }
        std::fs::write(&path, text).expect("write a file");
    }

    fn add(&self, key: &str, text: &[u8]) {
        self.write(key, text);
        // Literal pathspecs: a backslash in a name is not a glob escape.
        self.git(&["--literal-pathspecs", "add", "--", key]);
    }

    /// Stages `text` at `key` as a link, without making one in the work tree.
    fn add_link(&self, key: &str, target: &str) {
        let mut child = git_env::command("git")
            .arg("-C")
            .arg(self.root())
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run git hash-object");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(target.as_bytes())
            .expect("write the link target");
        let out = child.wait_with_output().expect("git hash-object");
        assert!(out.status.success(), "git hash-object failed");
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        self.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("120000,{oid},{key}"),
        ]);
    }

    fn scan(&self) -> Report {
        scan(self.root()).unwrap_or_else(|e| panic!("{e}"))
    }
}

/// (path, where from, id) of each finding.
fn found(report: &Report) -> Vec<(String, Source, &'static str)> {
    report
        .findings
        .iter()
        .map(|f| (f.key.clone(), f.source, f.id))
        .collect()
}

const TODO_LINE: &[u8] = fx!("see todo", " 7391\n").as_bytes();
const CLEAN_LINE: &[u8] = b"nothing to see\n";

#[test]
fn the_notice_body_is_read_with_every_pattern_but_prose() {
    let repo = Repo::new("cite-notice");
    let key = "crates/tasty-plugin-markdown/assets/NOTICE.md";
    let lines = one_line_per_pattern();
    let mut text = String::new();
    for (_, line) in &lines {
        text.push_str(line);
        text.push('\n');
    }
    repo.add(key, text.as_bytes());
    let report = repo.scan();
    let got: Vec<(usize, &str)> = report
        .findings
        .iter()
        .filter(|f| f.key == key)
        .map(|f| (f.line, f.id))
        .collect();
    let want: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, (id, _))| *id != "prose-todo")
        .map(|(i, (id, _))| (i + 1, *id))
        .collect();
    assert_eq!(got, want, "NOTICE.md was not read with the nine patterns");
}

#[test]
fn tracked_paths_are_read_wherever_they_sit() {
    let repo = Repo::new("cite-tracked");
    repo.add("docs/review/dist/guide.md", TODO_LINE);
    repo.add("AGENTS.md", TODO_LINE);
    repo.write(
        "build-cache/CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    repo.add("build-cache/report.txt", TODO_LINE);
    let keys: BTreeSet<String> = repo.scan().findings.into_iter().map(|f| f.key).collect();
    for key in [
        "docs/review/dist/guide.md",
        "AGENTS.md",
        "build-cache/report.txt",
    ] {
        assert!(keys.contains(key), "tracked {key} was not read: {keys:?}");
    }
}

#[test]
fn untracked_local_files_and_caches_are_left_out() {
    let repo = Repo::new("cite-untracked");
    repo.add(".gitignore", b"*.swp\n");
    repo.write("AGENTS.md", TODO_LINE);
    repo.write(&format!(".{LOCAL_HEAD}/x.md"), TODO_LINE);
    repo.write(&format!(".{}/y.md", ws_dir()), TODO_LINE);
    repo.write(
        "other-target/CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    repo.write("other-target/z.txt", TODO_LINE);
    repo.write("notes.swp", TODO_LINE);
    repo.write("draft.md", TODO_LINE);
    assert_eq!(
        found(&repo.scan()),
        vec![("draft.md".to_string(), Source::Untracked, "todo-number")],
        "only the untracked draft should be read"
    );
}

#[test]
fn the_index_and_the_work_tree_are_both_read() {
    let repo = Repo::new("cite-index");
    // Staged violation, clean work tree.
    repo.add("a.md", TODO_LINE);
    repo.write("a.md", CLEAN_LINE);
    // Clean staged copy, violation only in the work tree.
    repo.add("b.md", CLEAN_LINE);
    repo.write("b.md", TODO_LINE);
    // Staged violation, removed from the work tree only.
    repo.add("c.md", TODO_LINE);
    std::fs::remove_file(path_of(repo.root(), "c.md")).expect("remove c.md");
    // Staged removal.
    repo.add("d.md", TODO_LINE);
    repo.git(&["--literal-pathspecs", "rm", "-q", "-f", "--", "d.md"]);
    // Taken out of the index, kept in the work tree.
    repo.add("e.md", TODO_LINE);
    repo.git(&["--literal-pathspecs", "rm", "-q", "--cached", "--", "e.md"]);
    let mut got = found(&repo.scan());
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        vec![
            ("a.md".to_string(), Source::Index, "todo-number"),
            ("b.md".to_string(), Source::WorkTree, "todo-number"),
            ("c.md".to_string(), Source::Index, "todo-number"),
            ("e.md".to_string(), Source::Untracked, "todo-number"),
        ]
    );
}

#[test]
fn a_tracked_link_is_read_from_the_index_in_every_checkout_form() {
    let target = fx!("notes/todo", "/7391.md");
    let want = vec![("link.md".to_string(), Source::Index, "todo-number")];

    // Spread out as a plain file with the same text, as without link support.
    let repo = Repo::new("cite-link-file");
    repo.add_link("link.md", target);
    repo.write("link.md", target.as_bytes());
    assert_eq!(found(&repo.scan()), want);

    // The plain file changed to clean text: the index still holds the target.
    let repo = Repo::new("cite-link-clean");
    repo.add_link("link.md", target);
    repo.write("link.md", CLEAN_LINE);
    assert_eq!(found(&repo.scan()), want);

    // A real link.
    #[cfg(unix)]
    {
        let repo = Repo::new("cite-link-real");
        repo.add_link("link.md", target);
        std::os::unix::fs::symlink(target, path_of(repo.root(), "link.md")).expect("link");
        assert_eq!(found(&repo.scan()), want);
    }
}

#[cfg(unix)]
#[test]
fn a_path_through_a_linked_folder_is_read_from_the_index() {
    let outside = Scratch::new("cite-parent-outside");
    let slug_line = fx!("2001-02", "-03-sample-slug\n");
    std::fs::write(outside.path().join("guide.md"), slug_line).expect("write");
    let repo = Repo::new("cite-parent-link");
    repo.add("docs/guide.md", TODO_LINE);
    std::fs::remove_dir_all(path_of(repo.root(), "docs")).expect("remove docs");
    std::os::unix::fs::symlink(outside.path(), path_of(repo.root(), "docs")).expect("link");
    assert_eq!(
        found(&repo.scan()),
        vec![("docs/guide.md".to_string(), Source::Index, "todo-number")],
        "a text behind a linked folder was read"
    );
}

#[test]
fn paths_are_matched_as_git_gives_them() {
    let repo = Repo::new("cite-names");
    repo.add("docs/검토.md", TODO_LINE);
    assert_eq!(
        found(&repo.scan()),
        vec![("docs/검토.md".to_string(), Source::Index, "todo-number")]
    );
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_name_does_not_match_an_allowlisted_path() {
    let line = "x (04) y";
    assert!(violations_in_line(SELF_KEY, line).is_empty());
    let name = SELF_KEY.replace('/', "\\");
    let repo = Repo::new("cite-backslash");
    repo.add(&name, format!("{line}\n").as_bytes());
    assert_eq!(
        found(&repo.scan()),
        vec![(name, Source::Index, "split-number")],
        "a single file named like the allowlisted path got its exemption"
    );
}

#[test]
fn a_file_that_is_not_utf8_is_listed_and_an_unreadable_one_fails() {
    let repo = Repo::new("cite-undecodable");
    repo.add("bad.txt", &[0xff, 0xfe, b'x', b'\n']);
    let report = repo.scan();
    assert_eq!(report.undecodable, vec!["bad.txt".to_string()]);

    let repo = Repo::new("cite-unreadable");
    repo.add("gone.md", CLEAN_LINE);
    let path = path_of(repo.root(), "gone.md");
    std::fs::remove_file(&path).expect("remove");
    std::fs::create_dir(&path).expect("a folder in its place");
    let Err(err) = scan(repo.root()) else {
        panic!("an unreadable file must fail the scan");
    };
    assert!(err.contains("gone.md"), "{err}");
}

/// A tree without Git, as in a source tarball: the common entry walks it.
#[test]
fn without_git_the_walk_prunes_local_and_build_folders() {
    let dir = Scratch::new("cite-tarball");
    let root = dir.path();
    let write = |key: &str, text: &[u8]| {
        let path = path_of(root, key);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(path, text).expect("write");
    };
    write("sub/.git", b"gitdir: elsewhere\n");
    write("sub/.worktree", b"x\n");
    write(&format!("sub/.{LOCAL_HEAD}/x.md"), TODO_LINE);
    write(&format!("sub/.{}/y.md", ws_dir()), TODO_LINE);
    write(
        "sub/cache/CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    write("sub/cache/z.md", TODO_LINE);
    write("sub/keep.md", TODO_LINE);
    let report = scan(root).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        found(&report),
        vec![("sub/keep.md".to_string(), Source::Walked, "todo-number")]
    );
}

/// The walk function itself, called without the Git or tarball choice: a `.git` file
/// here would send the common entry to Git.
#[test]
fn the_walk_prunes_by_name_not_by_kind() {
    let dir = Scratch::new("cite-prune-kind");
    let root = dir.path();
    std::fs::write(root.join(".git"), "gitdir: elsewhere\n").expect("write");
    std::fs::write(root.join(".worktree"), "x\n").expect("write");
    std::fs::write(root.join("keep.md"), "x").expect("write");
    let mut out = Vec::new();
    gather(root, root, &mut out).unwrap_or_else(|e| panic!("{e}"));
    let keys: Vec<String> = out.into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, vec!["keep.md".to_string()]);
}

#[test]
fn a_build_dir_under_another_name_is_still_pruned() {
    let dir = Scratch::new("cite-cache");
    assert!(
        !is_pruned_dir(dir.path(), "target-e2e"),
        "without a marker a build-like name is not pruned"
    );
    std::fs::write(
        dir.path().join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .expect("write the marker");
    assert!(
        is_pruned_dir(dir.path(), "target-e2e"),
        "with a marker the folder is pruned whatever its name"
    );
}
