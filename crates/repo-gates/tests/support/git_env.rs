//! Removes the git variables that select a repository from the child processes of a test.
//!
//! When the tests run inside `git rebase --exec` or a hook, the parent passes GIT_DIR,
//! GIT_INDEX_FILE and others. A child that inherits them and runs `git init --bare` or
//! `git commit` in a temporary folder changes the config, index and history of the outer
//! repository instead. The list to remove is what `git rev-parse --local-env-vars` names.
//! Other GIT_* values such as author and committer stay.
//!
//! Ported from tasty crates/tasty-doc-guards/src/git_env.rs.

use std::ffi::OsStr;
use std::process::Command;
use std::sync::OnceLock;

/// The names this git treats as repository variables. Asked once.
/// Empty when git cannot run; then the child cannot run git either.
pub fn local_env_vars() -> &'static [String] {
    static VARS: OnceLock<Vec<String>> = OnceLock::new();
    VARS.get_or_init(|| {
        let out = match Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .output()
        {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => panic!("cannot run git rev-parse --local-env-vars: {e}"),
        };
        assert!(
            out.status.success(),
            "git rev-parse --local-env-vars failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let vars: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        assert!(
            vars.iter().any(|v| v == "GIT_DIR"),
            "git rev-parse --local-env-vars does not name GIT_DIR: {vars:?}"
        );
        vars
    })
}

/// Keeps `cmd` from inheriting the parent's repository variables.
pub fn isolate(cmd: &mut Command) -> &mut Command {
    for name in local_env_vars() {
        cmd.env_remove(name);
    }
    cmd
}

/// `Command::new(program)` without the repository variables.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    isolate(&mut cmd);
    cmd
}
