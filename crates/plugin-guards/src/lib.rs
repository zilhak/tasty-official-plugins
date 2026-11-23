//! Helpers shared by the source checks in `tests/`, including the function span reader
//! they use on masked Rust code. The checks read the plugin crates of this repository;
//! this crate builds nothing that ships.

use std::path::{Path, PathBuf};

/// Directory name prefix of the plugin crates.
pub const PLUGIN_CRATE_PREFIX: &str = "tasty-plugin-";

/// File that marks a plugin crate with a manifest. Library crates such as
/// tasty-plugin-agent-common have none.
pub const MANIFEST_NAME: &str = "tasty-plugin.toml";

/// Root of this repository: two levels above this crate.
pub fn repo_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the repository root")
        .to_path_buf();
    for marker in ["Cargo.toml", "Cargo.lock", "crates", "scripts/check.sh"] {
        assert!(
            root.join(marker).exists(),
            "{} has no {marker}; it is not the repository root",
            root.display()
        );
    }
    root
}

/// The plugin crates under `crates/` that have a manifest, as (directory name, path),
/// sorted by name.
pub fn plugin_crates() -> Vec<(String, PathBuf)> {
    let crates = repo_root().join("crates");
    let entries = std::fs::read_dir(&crates)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", crates.display()));
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(PLUGIN_CRATE_PREFIX) && path.join(MANIFEST_NAME).is_file() {
            out.push((name, path));
        }
    }
    out.sort();
    out
}

/// Whether `c` can be part of a Rust identifier.
pub fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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
pub fn matching_delim(code: &str, open: usize) -> Option<usize> {
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

/// A function with a body: its name and the byte positions of the body's braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnSpan {
    pub name: String,
    pub open: usize,
    pub close: usize,
}

/// Each function with a body in `code`, a masked copy of a Rust source that keeps the
/// byte positions of the text (the `code` of the tasty-doc-guards `Source`).
pub fn fn_spans(code: &str) -> Vec<FnSpan> {
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
        out.push(FnSpan { name, open, close });
    }
    out
}
