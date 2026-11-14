//! Helpers shared by the source checks in `tests/`. The checks read the plugin crates of
//! this repository; this crate builds nothing that ships.

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
