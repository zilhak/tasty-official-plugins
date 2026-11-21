//! Finds pictographic emoji in the Rust sources under `crates/` and in every plugin
//! manifest (`tasty-plugin.toml`) of the repository. Icons come from the design SVGs.
//! tasty ran the same check on the plugin crates before they moved here.
//!
//! The range is U+1F000..=U+1FAFF and the regional indicator letters of the flags,
//! U+1F1E6..=U+1F1FF. Arrows, command key symbols, and other text symbols outside it are
//! not checked. This file names code points instead of glyphs so that it does not report
//! itself.
//!
//! The Rust files come from the shared reader of tasty-doc-guards, which walks `crates/`
//! and skips only `target` folders. The manifests are found by name from the repository
//! root, wherever they sit, and that walk also skips dist, node_modules, git and worktree
//! folders, the local agent folders, and folders marked as build caches. No folder with
//! one of those names sits under `crates/` today.

use std::path::{Path, PathBuf};

use plugin_guards::{MANIFEST_NAME, repo_root};
use tasty_doc_guards::source_text::rust_sources;

const PRUNE_DIRS: &[&str] = &["target", "dist", ".worktree", ".git", "node_modules"];

/// The local agent folder names, assembled so that this file does not spell them.
const LOCAL_HEAD: &str = "claude";
const LOCAL_TAIL: &str = "-workspace";

/// Rust files under crates/: 181 measured on 2026-11-21 at fd948f3 (178 under src/ and
/// tests/, 3 build scripts). Per crate: codex 49, claude 42, markdown 21, agent-stream
/// 13, the others 11 or fewer. Losing all of codex, claude, or markdown fails; losing one
/// of the smaller crates does not.
const MIN_RUST_FILES: usize = 165;

/// Plugin manifests: 8 measured on 2026-11-21 at fd948f3, all directly under
/// `crates/<plugin>/`. One fewer is allowed so that removing a plugin is not mistaken
/// for an empty walk.
const MIN_MANIFESTS: usize = 7;

fn is_pruned(name: &str) -> bool {
    PRUNE_DIRS.contains(&name)
        || name
            .strip_prefix('.')
            .is_some_and(|rest| rest == LOCAL_HEAD || rest == format!("{LOCAL_HEAD}{LOCAL_TAIL}"))
}

fn is_forbidden_emoji(cp: u32) -> bool {
    (0x1F000..=0x1FAFF).contains(&cp) || (0x1F1E6..=0x1F1FF).contains(&cp)
}

fn rel_of(file: &Path, root: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every file named `tasty-plugin.toml` under `dir`, outside the pruned folders.
fn gather_manifests(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // A CARGO_TARGET_DIR under another name is pruned by its cache marker.
            if is_pruned(name) || tasty_doc_guards::is_build_cache_dir(&path) {
                continue;
            }
            gather_manifests(&path, out);
        } else if path.file_name().is_some_and(|n| n == MANIFEST_NAME) {
            out.push(path);
        }
    }
}

/// The manifests of the tree at `root`, as (relative path, LF text), sorted by path.
fn manifests(root: &Path) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    gather_manifests(root, &mut paths);
    let mut out: Vec<(String, String)> = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            (rel_of(path, root), text.replace("\r\n", "\n"))
        })
        .collect();
    out.sort();
    out
}

/// `path:line U+XXXX` for each forbidden code point in `text`.
fn violations(rel: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        for ch in line.chars() {
            let cp = ch as u32;
            if is_forbidden_emoji(cp) {
                out.push(format!("  {rel}:{} U+{cp:04X}", i + 1));
            }
        }
    }
    out
}

fn scan_is_credible(rust_files: usize, manifests: usize) -> bool {
    rust_files >= MIN_RUST_FILES && manifests >= MIN_MANIFESTS
}

#[test]
fn the_scan_refuses_to_report_zero_from_an_empty_walk() {
    assert!(!scan_is_credible(0, 0), "an empty walk was called credible");
    assert!(!scan_is_credible(MIN_RUST_FILES - 1, MIN_MANIFESTS));
    assert!(!scan_is_credible(MIN_RUST_FILES, MIN_MANIFESTS - 1));
    assert!(scan_is_credible(MIN_RUST_FILES, MIN_MANIFESTS));
}

#[test]
fn no_emoji_in_source() {
    let root = repo_root();
    let rust = rust_sources(&root, &["crates"]);
    let manifests = manifests(&root);
    assert!(
        scan_is_credible(rust.len(), manifests.len()),
        "collected {} Rust files (floor {MIN_RUST_FILES}) and {} manifests (floor \
         {MIN_MANIFESTS}). Check the walk and the pruned folders. If the tree really \
         shrank, update the floor with the new measurement.",
        rust.len(),
        manifests.len()
    );

    let mut found = Vec::new();
    for (rel, text) in &rust {
        found.extend(violations(&rel.to_string_lossy(), text));
    }
    for (rel, text) in &manifests {
        found.extend(violations(rel, text));
    }
    assert!(
        found.is_empty(),
        "pictographic emoji (U+1F000..=U+1FAFF, flags U+1F1E6..=U+1F1FF) in the Rust sources \
         or plugin manifests. Use the design SVG icons and remove emoji from manifest icons \
         and comments:\n{}",
        found.join("\n")
    );
}

/// The manifest walk takes the file name, not the plugin crate list, and leaves out the
/// pruned folders.
#[test]
fn manifests_are_found_by_name_outside_the_plugin_crates() {
    let dir = tasty_doc_guards::temp_scratch::Scratch::new("no-emoji-manifests");
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    };
    write(
        "examples/demo/tasty-plugin.toml",
        "description = \"demo \u{1F600}\"\n",
    );
    write("crates/tasty-plugin-x/tasty-plugin.toml", "id = \"x\"\n");
    write("target/x/tasty-plugin.toml", "id = \"x\"\n");
    write("node_modules/x/tasty-plugin.toml", "id = \"x\"\n");

    let found = manifests(root);
    let rels: Vec<&str> = found.iter().map(|(rel, _)| rel.as_str()).collect();
    assert_eq!(
        rels,
        [
            "crates/tasty-plugin-x/tasty-plugin.toml",
            "examples/demo/tasty-plugin.toml"
        ],
        "missed a manifest by name or collected one in a pruned folder"
    );
    let (rel, text) = &found[1];
    assert_eq!(
        violations(rel, text),
        ["  examples/demo/tasty-plugin.toml:1 U+1F600"]
    );
}
