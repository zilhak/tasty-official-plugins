//! Helpers shared by the gate tests. Each test binary uses a different subset.

// Each test file compiles this module on its own and uses only part of it.
#![allow(dead_code)]

pub mod gate_env;
pub mod git_env;

use std::path::{Path, PathBuf};

/// A file of this repository, by its path from the repository root.
pub fn repo_file(rel: &str) -> String {
    repo_root().join(rel).to_string_lossy().into_owned()
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate sits two levels below the repository root")
        .to_path_buf()
}

/// Writes an executable file.
pub fn write_executable(path: &Path, body: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}
