//! Checks that the four egui plugins read the locale font variable and call the shared
//! validation helper, and that each place that installs the language pack font passes the
//! face number the host handed over. The convention keeps one font across the processes;
//! the glyphs drawn on screen are not checked. tasty defines the variable on the host side
//! (font_env_path in its src/boot/locale.rs).

use std::path::Path;

use plugin_guards::repo_root;
use tasty_doc_guards::floored_walk::{CountedOn, Descend, Floor, normalized_rel, walk_with_floor};

/// The plugins and the file that installs their fonts. Update the list when a file moves.
const UI_PLUGINS: &[(&str, &str)] = &[
    ("tasty-plugin-clipboard-viewer", "src/main.rs"),
    ("tasty-plugin-git-viewer", "src/main.rs"),
    ("tasty-plugin-image", "src/main.rs"),
    ("tasty-plugin-markdown", "src/popup.rs"),
];

/// The variable through which the host passes the font path it resolved. All four must
/// read this spelling.
const LOCALE_FONT_ENV: &str = "TASTY_LOCALE_FONT";
/// The shared helper that validates with ab_glyph and appends the font. All four must call
/// it rather than keep a copy.
const HELPER: &str = "install_locale_font_fallback";

#[test]
fn every_ui_plugin_reads_the_same_locale_font_env_and_calls_the_shared_helper() {
    let root = repo_root();
    let mut checked = 0usize;
    let mut missing_env = Vec::new();
    let mut missing_helper = Vec::new();

    for (plugin, file) in UI_PLUGINS {
        let path = root.join("crates").join(plugin).join(file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        checked += 1;
        if !src.contains(LOCALE_FONT_ENV) {
            missing_env.push(*plugin);
        }
        if !src.contains(HELPER) {
            missing_helper.push(*plugin);
        }
    }

    assert_eq!(
        checked,
        UI_PLUGINS.len(),
        "scanned {checked} plugin(s), expected {} — the scan is broken, not clean",
        UI_PLUGINS.len()
    );
    assert!(
        missing_env.is_empty(),
        "UI plugins missing the locale font variable {LOCALE_FONT_ENV}: {missing_env:?}"
    );
    assert!(
        missing_helper.is_empty(),
        "UI plugins missing the shared font validation helper {HELPER}: {missing_helper:?}"
    );
}

/// Searched in the source with whitespace removed. The face number is either taken into a
/// variable or written straight into the call; those are the only two forms.
const FACE_IN_A_VARIABLE: &str = "letface=tasty_egui_theme::locale_font_face_from_env();";
const FACE_VARIABLE_PASSED: &str = "install_locale_font_fallback(&mutfonts,&path,face)";
const FACE_PASSED_DIRECTLY: &str =
    "install_locale_font_fallback(&mutfonts,&path,tasty_egui_theme::locale_font_face_from_env(),)";

/// The host's own installers live in tasty, which checks them there.
#[test]
fn every_locale_font_install_passes_the_face_number_from_the_host() {
    let root = repo_root();
    let mut checked = 0usize;
    let mut dropped = Vec::new();
    for (plugin, file) in UI_PLUGINS {
        let file = format!("crates/{plugin}/{file}");
        let src = std::fs::read_to_string(root.join(&file))
            .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
        let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        checked += 1;
        let through_variable =
            compact.contains(FACE_IN_A_VARIABLE) && compact.contains(FACE_VARIABLE_PASSED);
        if !through_variable && !compact.contains(FACE_PASSED_DIRECTLY) {
            dropped.push(file);
        }
    }
    assert_eq!(checked, UI_PLUGINS.len(), "the scan is broken, not clean");
    assert!(
        dropped.is_empty(),
        "these files do not pass locale_font_face_from_env to the locale font: {dropped:?}"
    );
}

/// Every file name among the system CJK font candidates. tasty lists them once, in
/// crates/tasty-egui-theme/src/lib.rs; a copy here would let a plugin pick another font
/// than the host. A new candidate goes in this list too, or its copies are not found.
const SYSTEM_CJK_FONT_FILES: &[&str] = &[
    "NotoSansCJK",
    "malgun.ttf",
    "msyh.ttc",
    "AppleSDGothicNeo",
    "ヒラギノ",
    "Arial Unicode",
    "Hiragino Sans GB",
    "STHeiti",
];

/// Keeps an empty walk from passing. It catches a large loss, not one or two files moving.
const RUST_SOURCES_FLOOR: Floor = Floor {
    min: 120,
    measured: 160,
    measured_on: "2026-11-14",
    counted_on: CountedOn::Tree("7f9684b — .rs files under crates/ from git ls-files"),
    why_this_gap: "About a quarter is left so that crates can be merged or split without a \
                   failure. The check looks for copies of the candidate names, so stopping \
                   an empty walk matters more than finding a few missing files.",
};

#[test]
fn system_cjk_font_candidates_are_not_repeated_in_the_plugins() {
    let root = repo_root();
    // On Windows file!() uses backslashes, while the walk's rel uses slashes.
    let this_file = normalized_rel(Path::new(file!()), Path::new(""));
    let walked = walk_with_floor(
        &root,
        &root,
        &RUST_SOURCES_FLOOR,
        Descend::SkipBuildCachesAndDotDirs,
        &|found| {
            found.rel.ends_with(".rs")
                // This file writes the names down as its input.
                && found.rel != this_file
                && found.rel.starts_with("crates/")
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut copies = Vec::new();
    for file in &walked {
        let src = std::fs::read_to_string(&file.path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.rel));
        if SYSTEM_CJK_FONT_FILES.iter().any(|name| src.contains(name)) {
            copies.push(file.rel.clone());
        }
    }
    assert!(
        copies.is_empty(),
        "system CJK font candidates are repeated outside tasty-egui-theme: {copies:?}"
    );
}
