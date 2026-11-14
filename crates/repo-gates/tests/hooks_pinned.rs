//! The git hooks in .githooks stay what docs/branching.md describes: the pre-commit run list
//! matches its check functions, and scripts/dev-setup.sh points core.hooksPath at .githooks.
//! The core.hooksPath of a developer's clone is not checked here. scripts/check.sh warns
//! about it instead, and the tests below run a copy of check.sh in synthetic repositories
//! with a stub cargo to pin that warning.
//!
//! Ported in part from tasty crates/tasty-doc-guards/tests/githooks_are_pinned.rs.

#![cfg(unix)]

mod support;

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

const HOOKS: [&str; 3] = ["pre-commit", "pre-merge-commit", "pre-push"];

fn read(rel: &str) -> String {
    std::fs::read_to_string(support::repo_file(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn the_hook_directory_holds_exactly_the_named_hooks() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(support::repo_file(".githooks"))
        .expect("read .githooks")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    let named: BTreeSet<String> = HOOKS.iter().map(|h| h.to_string()).collect();
    assert_eq!(on_disk, named);
}

#[test]
fn each_hook_is_an_executable_bash_script() {
    use std::os::unix::fs::PermissionsExt;
    for hook in HOOKS {
        let rel = format!(".githooks/{hook}");
        let text = read(&rel);
        assert!(text.starts_with("#!/usr/bin/env bash\n"), "{rel}");
        let mode = std::fs::metadata(support::repo_file(&rel))
            .expect("metadata")
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "{rel} is not executable");
    }
}

#[test]
fn the_pre_commit_run_list_and_its_definitions_still_agree() {
    let text = read(".githooks/pre-commit");
    let defined: BTreeSet<String> = text
        .lines()
        .filter_map(|l| l.strip_suffix("() {"))
        .filter(|name| name.starts_with("check_"))
        .map(str::to_owned)
        .collect();
    let listed: BTreeSet<String> = text
        .lines()
        .skip_while(|l| !l.starts_with("CHECKS=("))
        .skip(1)
        .take_while(|l| !l.starts_with(')'))
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    assert!(
        !defined.is_empty(),
        "no check_* functions read from pre-commit"
    );
    assert_eq!(defined, listed, "check_* functions and the CHECKS array");
}

#[test]
fn dev_setup_points_core_hooks_path_at_the_hook_directory() {
    let text = read("scripts/dev-setup.sh");
    assert!(
        text.contains("git config --local core.hooksPath .githooks"),
        "{text}"
    );
    // The version check reads every target's crates offline.
    assert!(text.contains("cargo fetch --locked"), "{text}");
    // It shows which hooks folder this tree now uses.
    assert!(text.contains("git rev-parse --git-path hooks"), "{text}");
}

#[test]
fn branching_doc_names_the_hooks_and_the_setup() {
    let doc = read("docs/branching.md");
    for needle in [
        "`pre-merge-commit`",
        "`pre-commit`",
        "bash scripts/dev-setup.sh",
        "tasty.pluginBump deferred",
        "scripts/plugin-bump-fixup.sh",
    ] {
        assert!(
            doc.contains(needle),
            "docs/branching.md does not mention {needle}"
        );
    }
}

/// Words of the warning for a clone without core.hooksPath.
const UNSET: &str = "core.hooksPath is not set";
/// Words of the warning for hooks from another tree.
const OTHER_TREE: &str = "not from .githooks of this tree";
/// Words of the warning for a hooksPath that does not exist.
const MISSING: &str = "points to a missing folder";

fn git(dir: &Path, args: &[&str]) {
    let out = support::git_env::command("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

/// A repository with a copy of scripts/check.sh and a .githooks folder, committed once so a
/// worktree can be added, and a stub cargo that exits with `$STUB_CARGO_RC` (0 by default).
struct Fixture {
    tmp: tempfile::TempDir,
    stub: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("scripts")).expect("scripts dir");
        std::fs::copy(
            support::repo_file("scripts/check.sh"),
            repo.join("scripts/check.sh"),
        )
        .expect("copy check.sh");
        support::write_executable(&repo.join(".githooks/pre-commit"), "#!/usr/bin/env bash\n")
            .expect("hook");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "fixture"]);
        let stub = tempfile::tempdir().expect("stub dir");
        support::write_executable(
            &stub.path().join("cargo"),
            "#!/bin/sh\nexit \"${STUB_CARGO_RC:-0}\"\n",
        )
        .expect("stub cargo");
        Fixture { tmp, stub }
    }

    fn repo(&self) -> std::path::PathBuf {
        self.tmp.path().join("repo")
    }

    /// Runs `bash scripts/check.sh fmt` in `dir` outside CI unless `ci` is set.
    fn check_fmt(&self, dir: &Path, ci: bool, cargo_rc: i32) -> (i32, String) {
        let mut cmd: Command = support::git_env::command("bash");
        cmd.args(["scripts/check.sh", "fmt"])
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("STUB_CARGO_RC", cargo_rc.to_string())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.stub.path().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        if ci {
            cmd.env("GITHUB_ACTIONS", "true");
        } else {
            // CI sets it, and the warning is off there; these runs stand for a local clone.
            cmd.env_remove("GITHUB_ACTIONS");
        }
        let out = cmd.output().expect("run check.sh");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

#[test]
fn check_warns_when_hooks_path_is_unset() {
    let f = Fixture::new();
    let (code, err) = f.check_fmt(&f.repo(), false, 0);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(UNSET), "{err}");
    assert_eq!(err.matches("scripts/dev-setup.sh").count(), 1, "{err}");
}

#[test]
fn check_is_quiet_when_hooks_path_points_here() {
    let f = Fixture::new();
    git(
        &f.repo(),
        &["config", "--local", "core.hooksPath", ".githooks"],
    );
    let (code, err) = f.check_fmt(&f.repo(), false, 0);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("warning:"), "{err}");
}

#[test]
fn check_is_quiet_in_ci() {
    let f = Fixture::new();
    let (code, err) = f.check_fmt(&f.repo(), true, 0);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("warning:"), "{err}");
}

#[test]
fn a_linked_worktree_inherits_the_clone_setting() {
    let f = Fixture::new();
    let repo = f.repo();
    git(&repo, &["config", "--local", "core.hooksPath", ".githooks"]);
    let wt = f.tmp.path().join("wt");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "wt", &wt.to_string_lossy()],
    );
    let (code, err) = f.check_fmt(&wt, false, 0);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("warning:"), "{err}");
}

#[test]
fn check_warns_when_hooks_come_from_another_tree() {
    let f = Fixture::new();
    let other = f.tmp.path().join("other-hooks");
    std::fs::create_dir_all(&other).expect("other hooks dir");
    git(
        &f.repo(),
        &[
            "config",
            "--local",
            "core.hooksPath",
            &other.to_string_lossy(),
        ],
    );
    let (code, err) = f.check_fmt(&f.repo(), false, 0);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(OTHER_TREE), "{err}");
}

#[test]
fn check_warns_and_keeps_going_when_hooks_path_is_missing() {
    let f = Fixture::new();
    git(
        &f.repo(),
        &["config", "--local", "core.hooksPath", "no-such-folder"],
    );
    let (code, err) = f.check_fmt(&f.repo(), false, 0);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(MISSING), "{err}");
    assert!(
        err.contains("+ cargo fmt --all --check"),
        "fmt did not run:\n{err}"
    );
    // The exit code is the command's own.
    let (code, err) = f.check_fmt(&f.repo(), false, 7);
    assert_eq!(code, 7, "{err}");
    assert!(err.contains(MISSING), "{err}");
}
