//! Finds pictographic emoji in the Rust sources under `crates/` and in every plugin
//! manifest (`tasty-plugin.toml`) of the repository. Icons come from the design SVGs.
//! tasty ran the same check on the plugin crates before they moved here.
//!
//! The range is U+1F000..=U+1FAFF and the regional indicator letters of the flags,
//! U+1F1E6..=U+1F1FF. Arrows, command key symbols, and other text symbols outside it are
//! not checked. This file names code points instead of glyphs so that it does not report
//! itself.
//!
//! One walk from the repository root collects both, as tasty's check did: every file
//! whose path ends in `.rs` under `crates/` (build scripts and a file named just `.rs`
//! included), and every file named `tasty-plugin.toml` wherever it sits. The walk follows
//! links to folders. It skips target, dist, node_modules, git and worktree folders, the
//! local agent folders, and folders marked as build caches. Like tasty's walk it does not
//! guard against a link cycle; such a walk ends when the path grows too long to open.
//! The shared Rust reader of tasty-doc-guards is not used: it does not follow folder links
//! and reads a file only by its extension, so it skips a file named `.rs`.

use std::path::{Path, PathBuf};

use plugin_guards::{MANIFEST_NAME, repo_root};

const PRUNE_DIRS: &[&str] = &["target", "dist", ".worktree", ".git", "node_modules"];

/// The local agent folder names, assembled so that this file does not spell them.
const LOCAL_HEAD: &str = "claude";
const LOCAL_TAIL: &str = "-workspace";

/// Rust files under crates/: 181 measured on 2026-11-21 at fd948f3 (178 under src/ and
/// tests/, 3 build scripts), 185 with the checks added since. Per crate: codex 49, claude 42, markdown 21, agent-stream
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

/// A file the walk reads: a Rust file under `crates/`, or a plugin manifest anywhere.
#[derive(PartialEq)]
enum Kind {
    Rust,
    Manifest,
}

fn kind_of(rel: &str) -> Option<Kind> {
    if rel.rsplit('/').next() == Some(MANIFEST_NAME) {
        Some(Kind::Manifest)
    } else if rel.starts_with("crates/") && rel.ends_with(".rs") {
        Some(Kind::Rust)
    } else {
        None
    }
}

/// Each collected file as (shown path, path to open, kind). The shown path uses `/` and
/// is only for sorting and reports; the file is read through the path the walk found.
fn gather(path: &Path, root: &Path, out: &mut Vec<(String, PathBuf, Kind)>) {
    // Path::is_dir and Path::is_file follow links, so a linked folder is walked and a
    // linked file is read like any other. An entry that is neither, such as the end of a
    // link loop that no longer resolves, is left out.
    if !path.is_dir() {
        if path.is_file() {
            let rel = rel_of(path, root);
            if let Some(kind) = kind_of(&rel) {
                out.push((rel, path.to_path_buf(), kind));
            }
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
            // A CARGO_TARGET_DIR under another name is pruned by its cache marker.
            if is_pruned(name) || tasty_doc_guards::is_build_cache_dir(&p) {
                continue;
            }
        }
        gather(&p, root, out);
    }
}

/// The Rust files and the manifests of the tree at `root`, each as (relative path, LF
/// text) sorted by path.
type Files = Vec<(String, String)>;

fn scan_files(root: &Path) -> (Files, Files) {
    let mut found = Vec::new();
    gather(root, root, &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let mut rust = Vec::new();
    let mut manifests = Vec::new();
    for (rel, path, kind) in found {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
            .replace("\r\n", "\n");
        if kind == Kind::Rust {
            rust.push((rel, text));
        } else {
            manifests.push((rel, text));
        }
    }
    (rust, manifests)
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
    let (rust, manifests) = scan_files(&repo_root());
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
        found.extend(violations(rel, text));
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

    let (_, found) = scan_files(root);
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

/// Creates a link to the folder `target` at `link`. Unix only: creating a folder link on
/// Windows needs a privilege or developer mode that the CI runner does not promise, so
/// the case that needs it is skipped there and the other cases still run.
#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(not(unix))]
fn link_dir(_target: &Path, _link: &Path) -> bool {
    false
}

/// Rust files are chosen by the end of their path and found through folder links.
#[test]
fn rust_files_are_found_by_name_and_through_folder_links() {
    let dir = tasty_doc_guards::temp_scratch::Scratch::new("no-emoji-rust");
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    };
    write("crates/tasty-plugin-x/src/.rs", "// \u{1F600}\n");
    write("crates/tasty-plugin-x/build.rs", "fn main() {}\n");
    write("examples/gen/helper.rs", "fn f() {}\n// \u{1F600}\n");
    write(
        "crates/tasty-plugin-x/target/debug/out.rs",
        "// \u{1F600}\n",
    );
    let linked = link_dir(
        &root.join("examples/gen"),
        &root.join("crates/tasty-plugin-x/src/generated"),
    );

    let (rust, _) = scan_files(root);
    let mut found = Vec::new();
    for (rel, text) in &rust {
        found.extend(violations(rel, text));
    }
    let mut want = vec!["  crates/tasty-plugin-x/src/.rs:1 U+1F600"];
    if linked {
        want.push("  crates/tasty-plugin-x/src/generated/helper.rs:2 U+1F600");
    }
    assert_eq!(
        found,
        want,
        "files: {:?}",
        rust.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    assert!(
        rust.iter()
            .any(|(rel, _)| rel == "crates/tasty-plugin-x/build.rs")
    );
}

/// A file is read through the path the walk found, not through its shown path. On Unix a
/// file name may hold a backslash, which the shown path turns into `/`.
#[cfg(unix)]
#[test]
fn a_file_is_read_through_the_path_the_walk_found() {
    let dir = tasty_doc_guards::temp_scratch::Scratch::new("no-emoji-backslash");
    let root = dir.path();
    let src = root.join("crates/tasty-plugin-x/src");
    std::fs::create_dir_all(src.join("name")).expect("mkdir");
    std::fs::write(src.join("name\\with.rs"), "// \u{1F600}\n").expect("write");
    std::fs::write(src.join("name/with.rs"), "fn f() {}\n").expect("write");

    let (rust, _) = scan_files(root);
    let mut found = Vec::new();
    for (rel, text) in &rust {
        found.extend(violations(rel, text));
    }
    assert_eq!(
        found,
        ["  crates/tasty-plugin-x/src/name/with.rs:1 U+1F600"]
    );
}

/// A link that loops back to its own folder ends the walk where it no longer resolves,
/// and that last name is not read as a file.
#[cfg(unix)]
#[test]
fn a_link_loop_ends_the_walk_without_reading_its_last_name() {
    let dir = tasty_doc_guards::temp_scratch::Scratch::new("no-emoji-loop");
    let root = dir.path();
    let crate_dir = root.join("crates/tasty-plugin-x");
    std::fs::create_dir_all(crate_dir.join("src")).expect("mkdir");
    std::fs::write(crate_dir.join("build.rs"), "// \u{1F600}\n").expect("write");
    std::os::unix::fs::symlink(".", crate_dir.join("src/back.rs")).expect("symlink");

    let (rust, _) = scan_files(root);
    let mut found = Vec::new();
    for (rel, text) in &rust {
        found.extend(violations(rel, text));
    }
    assert_eq!(found, ["  crates/tasty-plugin-x/build.rs:1 U+1F600"]);
}
