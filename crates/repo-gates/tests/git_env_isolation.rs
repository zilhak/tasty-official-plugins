//! The helper in support/git_env.rs keeps a child git from writing to the outer repository.
//!
//! Ported from the tests in tasty crates/tasty-doc-guards/src/git_env.rs.

#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use support::git_env::{command, isolate, local_env_vars};

fn git(cmd: Command, dir: &Path, args: &[&str]) {
    let mut cmd = cmd;
    let out = cmd
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Passes the outer GIT_DIR to the child directly, as `git rebase --exec` in a linked worktree
/// does. The test process's own environment does not change.
fn leaking(program: Command, outer: &Path) -> Command {
    let mut cmd = program;
    cmd.env("GIT_DIR", outer.join(".git"));
    cmd
}

struct Outer {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    child: PathBuf,
    config: Vec<u8>,
    index: Vec<u8>,
}

fn outer_repo() -> Outer {
    let scratch = tempfile::tempdir().expect("temp dir");
    let root = scratch.path().join("outer");
    let child = scratch.path().join("child");
    std::fs::create_dir_all(&root).expect("outer dir");
    std::fs::create_dir_all(&child).expect("child dir");
    git(command("git"), &root, &["init", "-q"]);
    std::fs::write(root.join("f"), "outer\n").expect("write f");
    git(command("git"), &root, &["add", "f"]);
    git(
        command("git"),
        &root,
        &[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "outer",
        ],
    );
    let config = std::fs::read(root.join(".git/config")).expect("read config");
    let index = std::fs::read(root.join(".git/index")).expect("read index");
    Outer {
        _scratch: scratch,
        root,
        child,
        config,
        index,
    }
}

impl Outer {
    fn unchanged(&self) -> bool {
        std::fs::read(self.root.join(".git/config")).ok() == Some(self.config.clone())
            && std::fs::read(self.root.join(".git/index")).ok() == Some(self.index.clone())
    }
}

#[test]
fn the_list_names_the_variables_that_redirect_a_repository() {
    let vars = local_env_vars();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        assert!(
            vars.iter().any(|v| v == name),
            "{name} is missing: {vars:?}"
        );
    }
    assert!(!vars.iter().any(|v| v == "GIT_AUTHOR_NAME"));
}

#[test]
fn an_isolated_child_initializes_its_own_repository() {
    let o = outer_repo();
    let mut bare = leaking(Command::new("git"), &o.root);
    isolate(&mut bare);
    git(bare, &o.child, &["init", "-q", "--bare"]);
    let mut work = leaking(Command::new("git"), &o.root);
    isolate(&mut work);
    git(work, &o.child, &["config", "core.bare", "false"]);
    assert!(o.unchanged(), "the outer config or index changed");
    assert!(
        o.child.join("config").is_file(),
        "the child repository was not created"
    );
}

/// Without the helper the same command changes the outer repository; this is why the test
/// above means something.
#[test]
fn without_isolation_the_same_child_rewrites_the_outer_repository() {
    let o = outer_repo();
    git(
        leaking(Command::new("git"), &o.root),
        &o.child,
        &["init", "-q", "--bare"],
    );
    assert!(
        !o.unchanged(),
        "the outer repository stayed the same with a leaked GIT_DIR"
    );
    assert!(!o.child.join("config").exists());
}
