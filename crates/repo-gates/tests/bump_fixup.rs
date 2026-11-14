//! The merge-step bump tool and the P.1 deferral mode of pre-commit, in synthetic Cargo
//! repositories. The tool is scripts/plugin-bump-fixup.sh; the mode decision is
//! scripts/lib/plugin-bump-mode.sh. Real cargo, rustfmt and git run, but without registry
//! dependencies nothing reaches the network. Bash, so Unix only.
//!
//! Ported from tasty tests/plugin_bump_fixup.rs.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

mod support;

use support::{git_env, repo_file};

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = git_env::command("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) {
    git_out(dir, args);
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().expect("parent")).expect("create dir");
    fs::write(&p, body).unwrap_or_else(|e| panic!("write {rel}: {e}"));
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn cargo_offline(dir: &Path, args: &[&str]) {
    let out = git_env::command("cargo")
        .args(args)
        .arg("--offline")
        .current_dir(dir)
        .output()
        .expect("run cargo");
    assert!(
        out.status.success(),
        "cargo {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Run {
    code: i32,
    output: String,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rc={} output:\n{}", self.code, self.output)
    }
}

fn run_script(dir: &Path, rel: &str, args: &[&str]) -> Run {
    let out = git_env::command("bash")
        .arg(repo_file(rel))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the script");
    Run {
        code: out.status.code().unwrap_or(-1),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn fixup(dir: &Path) -> Run {
    run_script(dir, "scripts/plugin-bump-fixup.sh", &[])
}

fn check_head(dir: &Path) -> Run {
    run_script(
        dir,
        "scripts/check-plugin-version-bump.sh",
        &["--range", "HEAD^", "HEAD"],
    )
}

const PLUGINS: [&str; 2] = ["alpha", "beta"];

fn manifest_of(p: &str) -> String {
    format!("crates/tasty-plugin-{p}/tasty-plugin.toml")
}

fn cargo_toml_of(p: &str) -> String {
    format!("crates/tasty-plugin-{p}/Cargo.toml")
}

fn main_rs_of(p: &str) -> String {
    format!("crates/tasty-plugin-{p}/src/main.rs")
}

/// A workspace where two plugins (alpha, beta) have a path dependency on `shared`, a crate
/// without a manifest.
fn seed() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    let d = tmp.path();
    git(d, &["init", "--quiet", "-b", "main"]);
    git(d, &["config", "user.email", "fixture@example.invalid"]);
    git(d, &["config", "user.name", "fixture"]);
    // Keep the user's git hooks out of the commits and amends of the synthetic repository.
    git(d, &["config", "core.hooksPath", "/dev/null"]);
    write(
        d,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"3\"\n\n[workspace.package]\nedition = \"2024\"\n",
    );
    // lane/ is where the P.1 tests below create a linked work tree.
    write(d, ".gitignore", "target\nlane/\n");
    for p in PLUGINS {
        write(
            d,
            &cargo_toml_of(p),
            &format!(
                "[package]\nname = \"tasty-plugin-{p}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
                 [dependencies]\nshared = {{ path = \"../shared\" }}\n"
            ),
        );
        write(
            d,
            &manifest_of(p),
            &format!("id = \"com.tasty.{p}\"\nversion = \"0.1.0\"\n"),
        );
        write(
            d,
            &main_rs_of(p),
            "fn main() {\n    shared::hi();\n}\n\nfn a() {}\n\n\n\n\nfn b() {}\n",
        );
    }
    write(
        d,
        "crates/shared/Cargo.toml",
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    write(d, "crates/shared/src/lib.rs", "pub fn hi() {}\n");
    cargo_offline(d, &["generate-lockfile"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "--quiet", "-m", "seed"]);
    tmp
}

fn edit_line(d: &Path, rel: &str, from: &str, to: &str) {
    let body = read(d, rel);
    assert_eq!(
        body.matches(from).count(),
        1,
        "line {from:?} to replace in {rel}"
    );
    write(d, rel, &body.replacen(from, to, 1));
}

fn commit_all(d: &Path, msg: &str) {
    git(d, &["commit", "--quiet", "-a", "-m", msg]);
}

fn version_line(d: &Path, rel: &str) -> String {
    read(d, rel)
        .lines()
        .find(|l| l.starts_with("version"))
        .unwrap_or_else(|| panic!("{rel} has no version line"))
        .to_string()
}

fn assert_versions(d: &Path, p: &str, v: &str) {
    let want = format!("version = \"{v}\"");
    assert_eq!(version_line(d, &cargo_toml_of(p)), want, "{p} Cargo.toml");
    assert_eq!(version_line(d, &manifest_of(p)), want, "{p} manifest");
    let lock = read(d, "Cargo.lock");
    assert!(
        lock.contains(&format!("name = \"tasty-plugin-{p}\"\n{want}\n")),
        "tasty-plugin-{p} in Cargo.lock is not {v}:\n{lock}"
    );
}

fn commit_count(d: &Path) -> String {
    git_out(d, &["rev-list", "--count", "HEAD"])
}

#[test]
fn content_change_gets_its_bump_amended_into_the_same_commit() {
    let tmp = seed();
    let d = tmp.path();
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn a() {}",
        "fn a() { let _a = 1; }",
    );
    commit_all(d, "alpha");
    let before = commit_count(d);

    let r = fixup(d);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(
        r.output.contains("tasty-plugin-alpha 0.1.0 → 0.1.1"),
        "{r:?}"
    );
    assert_versions(d, "alpha", "0.1.1");
    assert_versions(d, "beta", "0.1.0");
    assert_eq!(commit_count(d), before, "it must amend, not add a commit");
    assert_eq!(git_out(d, &["log", "-1", "--format=%s"]), "alpha");
    assert_eq!(
        git_out(d, &["status", "--porcelain"]),
        "",
        "the work tree must be clean"
    );
    let changed = git_out(d, &["diff", "--name-only", "HEAD^", "HEAD"]);
    assert_eq!(
        changed.lines().collect::<Vec<_>>(),
        [
            "Cargo.lock",
            "crates/tasty-plugin-alpha/Cargo.toml",
            "crates/tasty-plugin-alpha/src/main.rs",
            "crates/tasty-plugin-alpha/tasty-plugin.toml",
        ]
    );
    let r = check_head(d);
    assert_eq!(r.code, 0, "{r:?}");
}

#[test]
fn commit_without_plugin_content_change_is_left_alone() {
    let tmp = seed();
    let d = tmp.path();
    write(d, "README.md", "notes\n");
    git(d, &["add", "README.md"]);
    commit_all(d, "docs");
    let head = git_out(d, &["rev-parse", "HEAD"]);

    let r = fixup(d);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.output.contains("no plugin version to bump"), "{r:?}");
    assert_eq!(
        git_out(d, &["rev-parse", "HEAD"]),
        head,
        "the commit must not change"
    );
}

#[test]
fn commit_that_already_bumped_is_left_alone() {
    let tmp = seed();
    let d = tmp.path();
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn a() {}",
        "fn a() { let _a = 1; }",
    );
    for rel in [cargo_toml_of("alpha"), manifest_of("alpha")] {
        edit_line(d, &rel, "version = \"0.1.0\"", "version = \"0.1.5\"");
    }
    cargo_offline(d, &["metadata", "--format-version", "1"]);
    commit_all(d, "alpha bumped by hand");
    let head = git_out(d, &["rev-parse", "HEAD"]);

    let r = fixup(d);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.output.contains("no plugin version to bump"), "{r:?}");
    assert_eq!(git_out(d, &["rev-parse", "HEAD"]), head);
    assert_versions(d, "alpha", "0.1.5");
}

#[test]
fn every_changed_plugin_is_bumped_in_one_pass() {
    let tmp = seed();
    let d = tmp.path();
    for p in PLUGINS {
        edit_line(d, &main_rs_of(p), "fn b() {}", "fn b() { let _b = 2; }");
    }
    commit_all(d, "both plugins");

    let r = fixup(d);
    assert_eq!(r.code, 0, "{r:?}");
    for p in PLUGINS {
        assert_versions(d, p, "0.1.1");
    }
    assert!(r.output.contains("amended 2 version bumps"), "{r:?}");
}

#[test]
fn shared_path_dependency_change_bumps_every_plugin_that_links_it() {
    let tmp = seed();
    let d = tmp.path();
    write(
        d,
        "crates/shared/src/lib.rs",
        "pub fn hi() {}\npub fn bye() {}\n",
    );
    commit_all(d, "shared");

    let r = fixup(d);
    assert_eq!(r.code, 0, "{r:?}");
    for p in PLUGINS {
        assert_versions(d, p, "0.1.1");
    }
    let lock_diff = git_out(
        d,
        &["diff", "--numstat", "HEAD^", "HEAD", "--", "Cargo.lock"],
    );
    assert!(
        lock_diff.starts_with("2\t2\t"),
        "Cargo.lock must change only the version lines of the two packages: {lock_diff}"
    );
}

#[test]
fn undecidable_range_stops_without_touching_the_commit() {
    let tmp = seed();
    let d = tmp.path();
    // A commit that changes content and lowers the version makes the checker undecidable (2).
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn a() {}",
        "fn a() { let _a = 1; }",
    );
    for rel in [cargo_toml_of("alpha"), manifest_of("alpha")] {
        edit_line(d, &rel, "version = \"0.1.0\"", "version = \"0.0.9\"");
    }
    cargo_offline(d, &["metadata", "--format-version", "1"]);
    commit_all(d, "alpha downgraded");
    let head = git_out(d, &["rev-parse", "HEAD"]);

    let r = fixup(d);
    assert_eq!(r.code, 2, "{r:?}");
    assert!(
        r.output
            .contains("the start of this range is newer than its end"),
        "{r:?}"
    );
    assert!(
        r.output.contains("the version check ended with exit code"),
        "{r:?}"
    );
    assert!(r.output.contains("exit code 2"), "{r:?}");
    assert!(r.output.contains("undecidable"), "{r:?}");
    assert_eq!(git_out(d, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_out(d, &["status", "--porcelain"]), "");
}

#[test]
fn uncommitted_tracked_change_stops_before_amending() {
    let tmp = seed();
    let d = tmp.path();
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn a() {}",
        "fn a() { let _a = 1; }",
    );
    commit_all(d, "alpha");
    let head = git_out(d, &["rev-parse", "HEAD"]);
    edit_line(
        d,
        &main_rs_of("beta"),
        "fn a() {}",
        "fn a() { let _x = 9; }",
    );

    let r = fixup(d);
    assert_eq!(r.code, 2, "{r:?}");
    assert!(
        r.output.contains("uncommitted changes to tracked files"),
        "{r:?}"
    );
    assert_eq!(git_out(d, &["rev-parse", "HEAD"]), head);
    assert_versions(d, "alpha", "0.1.0");
}

/// Two lanes change the same plugin and leave the version lines alone. The merge step adds
/// the bumps per commit with rebase --exec.
#[test]
fn lanes_that_leave_versions_alone_rebase_without_conflict() {
    let tmp = seed();
    let d = tmp.path();
    let exec = format!("bash {}", repo_file("scripts/plugin-bump-fixup.sh"));

    git(d, &["checkout", "--quiet", "-b", "lane-a", "main"]);
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn a() {}",
        "fn a() { let _a = 1; }",
    );
    commit_all(d, "A: alpha");
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn main() {",
        "fn main() { let _a2 = 3;",
    );
    commit_all(d, "A: alpha again");

    git(d, &["checkout", "--quiet", "-b", "lane-b", "main"]);
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn b() {}",
        "fn b() { let _b = 2; }",
    );
    write(
        d,
        "crates/shared/src/lib.rs",
        "pub fn hi() {}\npub fn bye() {}\n",
    );
    commit_all(d, "B: alpha and shared");

    for lane in ["lane-a", "lane-b"] {
        git(d, &["checkout", "--quiet", lane]);
        git(d, &["rebase", "--quiet", "--exec", &exec, "main"]);
        git(d, &["checkout", "--quiet", "main"]);
        git(d, &["merge", "--quiet", "--ff-only", lane]);
    }

    assert_versions(d, "alpha", "0.1.3");
    assert_versions(d, "beta", "0.1.1");
    let landed = git_out(d, &["rev-list", "--reverse", "main~3..main"]);
    assert_eq!(landed.lines().count(), 3);
    for c in landed.lines() {
        let r = run_script(
            d,
            "scripts/check-plugin-version-bump.sh",
            &["--range", &format!("{c}^"), c],
        );
        assert_eq!(r.code, 0, "commit {c} on main: {r:?}");
        assert!(r.output.contains("pass — "), "{c}: {r:?}");
    }
    cargo_offline(d, &["metadata", "--locked", "--format-version", "1"]);
}

// ── A failure during rebase --exec leaves a clean tree; fix the cause, rerun, then --continue ─────

fn exec_fixup() -> String {
    format!("bash {}", repo_file("scripts/plugin-bump-fixup.sh"))
}

fn rebase_status(d: &Path) -> Run {
    let out = git_env::command("git")
        .args(["rebase", "--exec", &exec_fixup(), "main"])
        .current_dir(d)
        .output()
        .expect("run rebase");
    Run {
        code: out.status.code().unwrap_or(-1),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn tracked_changes(d: &Path) -> String {
    git_out(d, &["status", "--porcelain", "--untracked-files=no"])
}

fn rebase_in_progress(d: &Path) -> bool {
    let dir = git_out(d, &["rev-parse", "--path-format=absolute", "--git-dir"]);
    Path::new(&dir).join("rebase-merge").is_dir()
}

#[test]
fn amend_hook_failure_leaves_a_clean_tree_and_recovers_with_rerun_and_continue() {
    let tmp = seed();
    let d = tmp.path();
    git(d, &["checkout", "--quiet", "-b", "lane", "main"]);
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn a() {}",
        "fn a() { let _a = 1; }",
    );
    commit_all(d, "lane: alpha");

    // The pre-commit that runs during the amend fails.
    let hooks = tempfile::tempdir().expect("temp dir");
    let hook = hooks.path().join("pre-commit");
    fs::write(
        &hook,
        "#!/bin/sh\necho 'fixture hook refuses' >&2\nexit 1\n",
    )
    .expect("write the hook");
    Command::new("chmod")
        .args(["+x", hook.to_str().expect("UTF-8 path")])
        .status()
        .expect("chmod");
    git(
        d,
        &[
            "config",
            "core.hooksPath",
            hooks.path().to_str().expect("UTF-8 path"),
        ],
    );

    let r = rebase_status(d);
    assert_ne!(r.code, 0, "{r:?}");
    assert!(r.output.contains("git commit --amend failed"), "{r:?}");
    assert!(r.output.contains("edited files from HEAD"), "{r:?}");
    assert!(rebase_in_progress(d), "the rebase must stop at that commit");
    assert_eq!(tracked_changes(d), "", "no edited file may be left behind");
    assert_versions(d, "alpha", "0.1.0");

    // Fix the cause, run the same command again, and continue.
    git(d, &["config", "core.hooksPath", "/dev/null"]);
    let r = fixup(d);
    assert_eq!(r.code, 0, "{r:?}");
    git(d, &["rebase", "--continue"]);
    assert!(!rebase_in_progress(d));
    assert_versions(d, "alpha", "0.1.1");
    assert_eq!(check_head(d).code, 0);
}

#[test]
fn a_later_plugin_that_cannot_be_bumped_stops_before_touching_any_file() {
    let tmp = seed();
    let d = tmp.path();
    // main has beta with two different versions.
    edit_line(
        d,
        &manifest_of("beta"),
        "version = \"0.1.0\"",
        "version = \"0.1.3\"",
    );
    commit_all(d, "main: beta manifest drift");
    git(d, &["checkout", "--quiet", "-b", "lane", "main"]);
    for p in PLUGINS {
        edit_line(d, &main_rs_of(p), "fn b() {}", "fn b() { let _b = 2; }");
    }
    commit_all(d, "lane: both");

    let r = rebase_status(d);
    assert_ne!(r.code, 0, "{r:?}");
    assert!(r.output.contains("cannot tell which one to bump"), "{r:?}");
    assert!(rebase_in_progress(d));
    // alpha, earlier in the list, is left untouched too.
    assert_eq!(tracked_changes(d), "", "no edited file may be left behind");
    assert_versions(d, "alpha", "0.1.0");

    // Fix the cause (the mismatched version) in that commit, rerun, and continue.
    edit_line(
        d,
        &cargo_toml_of("beta"),
        "version = \"0.1.0\"",
        "version = \"0.1.3\"",
    );
    cargo_offline(d, &["metadata", "--format-version", "1"]);
    git(d, &["commit", "--quiet", "-a", "--amend", "--no-edit"]);
    let r = fixup(d);
    assert_eq!(r.code, 0, "{r:?}");
    git(d, &["rebase", "--continue"]);
    assert!(!rebase_in_progress(d));
    assert_versions(d, "alpha", "0.1.1");
    assert_versions(d, "beta", "0.1.4");
    assert_eq!(check_head(d).code, 0);
}

#[test]
fn abort_returns_to_the_original_commit_after_a_failed_fixup() {
    let tmp = seed();
    let d = tmp.path();
    edit_line(
        d,
        &manifest_of("beta"),
        "version = \"0.1.0\"",
        "version = \"0.1.3\"",
    );
    commit_all(d, "main: beta manifest drift");
    git(d, &["checkout", "--quiet", "-b", "lane", "main"]);
    edit_line(
        d,
        &main_rs_of("beta"),
        "fn b() {}",
        "fn b() { let _b = 2; }",
    );
    commit_all(d, "lane: beta");
    let before = git_out(d, &["rev-parse", "HEAD"]);

    let r = rebase_status(d);
    assert_ne!(r.code, 0, "{r:?}");
    git(d, &["rebase", "--abort"]);
    assert!(!rebase_in_progress(d));
    assert_eq!(git_out(d, &["rev-parse", "HEAD"]), before);
    assert_eq!(tracked_changes(d), "");
}

// ── When the start conditions do not hold, nothing changes and it stops as undecidable (2) ──────────────

fn assert_refused(r: &Run, words: &str) {
    assert_eq!(r.code, 2, "{r:?}");
    assert!(r.output.contains(words), "{words:?} is missing: {r:?}");
}

#[test]
fn fixup_takes_no_arguments() {
    let tmp = seed();
    let r = run_script(tmp.path(), "scripts/plugin-bump-fixup.sh", &["HEAD~1"]);
    assert_refused(&r, "takes no arguments");
}

#[test]
fn fixup_refuses_a_merge_commit() {
    let tmp = seed();
    let d = tmp.path();
    git(d, &["checkout", "--quiet", "-b", "side"]);
    edit_line(
        d,
        &main_rs_of("alpha"),
        "fn a() {}",
        "fn a() { let _s = 1; }",
    );
    commit_all(d, "side");
    git(d, &["checkout", "--quiet", "main"]);
    edit_line(
        d,
        &main_rs_of("beta"),
        "fn a() {}",
        "fn a() { let _m = 1; }",
    );
    commit_all(d, "main");
    git(d, &["merge", "--quiet", "--no-ff", "--no-edit", "side"]);
    let head = git_out(d, &["rev-parse", "HEAD"]);

    assert_refused(&fixup(d), "does not handle merge commits");
    assert_eq!(git_out(d, &["rev-parse", "HEAD"]), head);
}

#[test]
fn fixup_refuses_a_repository_without_commits() {
    let tmp = tempfile::tempdir().expect("temp dir");
    git(tmp.path(), &["init", "--quiet"]);
    assert_refused(&fixup(tmp.path()), "there is no HEAD commit");
}

#[test]
fn fixup_refuses_outside_a_repository_and_in_a_bare_one() {
    let plain = tempfile::tempdir().expect("temp dir");
    assert_refused(&fixup(plain.path()), "run it inside a git repository");

    let bare = tempfile::tempdir().expect("temp dir");
    git(bare.path(), &["init", "--quiet", "--bare"]);
    assert_refused(&fixup(bare.path()), "cannot move to the work tree root");
}

#[test]
fn fixup_refuses_when_the_checker_is_not_beside_it() {
    let tmp = seed();
    let lone = tempfile::tempdir().expect("temp dir");
    let copy = lone.path().join("plugin-bump-fixup.sh");
    fs::copy(repo_file("scripts/plugin-bump-fixup.sh"), &copy).expect("copy");
    let out = git_env::command("bash")
        .arg(&copy)
        .current_dir(tmp.path())
        .output()
        .expect("run the copy");
    let r = Run {
        code: out.status.code().unwrap_or(-1),
        output: String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    assert_refused(&r, "the check script is missing");
}

#[test]
fn checker_refuses_a_missing_or_unwritable_violations_file() {
    let tmp = seed();
    let d = tmp.path();
    let check = "scripts/check-plugin-version-bump.sh";
    let r = run_script(d, check, &["--range", "HEAD", "HEAD", "--violations-out"]);
    assert_refused(&r, "--violations-out needs a file path");

    let r = run_script(
        d,
        check,
        &[
            "--range",
            "HEAD",
            "HEAD",
            "--violations-out",
            "no/such/dir/list",
        ],
    );
    assert_refused(&r, "cannot write the --violations-out file");
}

#[test]
fn checker_writes_the_violation_list_relative_to_the_caller() {
    let tmp = seed();
    let d = tmp.path();
    edit_line(
        d,
        &main_rs_of("beta"),
        "fn a() {}",
        "fn a() { let _v = 1; }",
    );
    commit_all(d, "beta");
    let sub = d.join("crates");
    let r = run_script(
        &sub,
        "scripts/check-plugin-version-bump.sh",
        &["--range", "HEAD^", "HEAD", "--violations-out", "list.txt"],
    );
    assert_eq!(r.code, 1, "{r:?}");
    assert_eq!(read(&sub, "list.txt"), "crates/tasty-plugin-beta\n");
}

// ── P.1 deferral mode ───────────────────────────────────────────────

fn mode(dir: &Path) -> Run {
    let out = git_env::command("bash")
        .arg("-c")
        .arg(format!(
            ". '{}' && plugin_bump_mode",
            repo_file("scripts/lib/plugin-bump-mode.sh")
        ))
        .current_dir(dir)
        .output()
        .expect("run the mode decision");
    Run {
        code: out.status.code().unwrap_or(-1),
        output: format!(
            "{}|{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// Adds one linked work tree to the main repository and returns its path.
fn with_linked_worktree(d: &Path) -> std::path::PathBuf {
    let wt = d.join("lane");
    git(
        d,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "lane",
            wt.to_str().expect("UTF-8 path"),
        ],
    );
    git(d, &["config", "extensions.worktreeConfig", "true"]);
    wt
}

#[test]
fn mode_defers_only_for_worktree_scope_in_a_linked_worktree() {
    let tmp = seed();
    let d = tmp.path();
    let wt = with_linked_worktree(d);

    // Without a setting both places enforce.
    for dir in [d, wt.as_path()] {
        let r = mode(dir);
        assert!(r.output.starts_with("enforced|"), "{r:?}");
    }

    git(
        &wt,
        &["config", "--worktree", "tasty.pluginBump", "deferred"],
    );
    let r = mode(&wt);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.output.starts_with("deferred|"), "{r:?}");
    // The setting of the linked work tree does not leak to the main work tree.
    let r = mode(d);
    assert!(r.output.starts_with("enforced|"), "{r:?}");
}

#[test]
fn mode_refuses_shared_scope_main_worktree_and_unknown_values() {
    let tmp = seed();
    let d = tmp.path();
    let wt = with_linked_worktree(d);

    // A value in the shared config (.git/config) is visible to every work tree but not followed.
    git(d, &["config", "--local", "tasty.pluginBump", "deferred"]);
    for dir in [d, wt.as_path()] {
        let r = mode(dir);
        assert!(r.output.starts_with("enforced|"), "{r:?}");
        assert!(r.output.contains("worktree scope"), "{r:?}");
    }
    git(d, &["config", "--local", "--unset", "tasty.pluginBump"]);

    // A value in the main work tree's config.worktree is not followed either.
    git(d, &["config", "--worktree", "tasty.pluginBump", "deferred"]);
    let r = mode(d);
    assert!(r.output.starts_with("enforced|"), "{r:?}");
    assert!(r.output.contains("main work tree"), "{r:?}");

    git(&wt, &["config", "--worktree", "tasty.pluginBump", "later"]);
    let r = mode(&wt);
    assert!(r.output.starts_with("enforced|"), "{r:?}");
    assert!(r.output.contains("unknown value"), "{r:?}");
}

/// Runs the real pre-commit, with bash and cargo replaced by BASH_ENV functions that only
/// record the calls.
fn run_pre_commit(dir: &Path, version_rc: i32) -> (Run, String) {
    let stub_dir = tempfile::tempdir().expect("temp dir");
    let calls = stub_dir.path().join("calls");
    let env_file = stub_dir.path().join("stubs.sh");
    fs::write(
        &env_file,
        r#"
bash() {
    printf '%s\n' "$*" >> "$HOOK_CALLS"
    case "$1" in
        *check-plugin-version-bump.sh) return "$HOOK_VERSION_RC" ;;
        *) return 0 ;;
    esac
}
cargo() {
    printf 'cargo %s\n' "$*" >> "$HOOK_CALLS"
    return 0
}
"#,
    )
    .expect("write the stubs");
    let out = git_env::command("bash")
        .arg(repo_file(".githooks/pre-commit"))
        .current_dir(dir)
        .env("BASH_ENV", &env_file)
        .env("HOOK_CALLS", &calls)
        .env("HOOK_VERSION_RC", version_rc.to_string())
        .output()
        .expect("run pre-commit");
    let run = Run {
        code: out.status.code().unwrap_or(-1),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    };
    (run, fs::read_to_string(&calls).unwrap_or_default())
}

/// The hook reads the mode decision from scripts/lib at the repository root, so the synthetic
/// repository gets a copy.
fn stage_plugin_change_with_mode_lib(dir: &Path) {
    let lib = fs::read_to_string(repo_file("scripts/lib/plugin-bump-mode.sh")).expect("mode file");
    write(dir, "scripts/lib/plugin-bump-mode.sh", &lib);
    write(
        dir,
        "crates/tasty-plugin-alpha/assets/note.txt",
        "changed asset\n",
    );
    git(dir, &["add", "-A"]);
}

#[test]
fn pre_commit_skips_p1_in_a_deferred_lane_worktree() {
    let tmp = seed();
    let d = tmp.path();
    let wt = with_linked_worktree(d);
    git(
        &wt,
        &["config", "--worktree", "tasty.pluginBump", "deferred"],
    );
    stage_plugin_change_with_mode_lib(&wt);

    // Even when the checker would report a violation (1), deferral mode does not call it.
    let (r, calls) = run_pre_commit(&wt, 1);
    assert_eq!(r.code, 0, "{r:?}\ncalls:\n{calls}");
    assert!(
        !calls.contains("check-plugin-version-bump.sh --staged"),
        "{calls}"
    );
    assert!(r.output.contains("tasty.pluginBump=deferred"), "{r:?}");
}

#[test]
fn pre_commit_still_enforces_p1_without_the_worktree_setting() {
    let tmp = seed();
    let d = tmp.path();
    let wt = with_linked_worktree(d);
    for dir in [d, wt.as_path()] {
        stage_plugin_change_with_mode_lib(dir);
        let (r, calls) = run_pre_commit(dir, 1);
        assert_eq!(r.code, 1, "{r:?}\ncalls:\n{calls}");
        assert!(
            calls.contains("check-plugin-version-bump.sh --staged"),
            "{calls}"
        );
        assert!(r.output.contains("[P.1]"), "{r:?}");

        let (r, calls) = run_pre_commit(dir, 0);
        assert_eq!(r.code, 0, "{r:?}\ncalls:\n{calls}");
        assert!(
            calls.contains("check-plugin-version-bump.sh --staged"),
            "{calls}"
        );
    }
}

#[test]
fn pre_commit_enforces_p1_when_shared_config_asks_for_deferral() {
    let tmp = seed();
    let d = tmp.path();
    let wt = with_linked_worktree(d);
    git(d, &["config", "--local", "tasty.pluginBump", "deferred"]);
    stage_plugin_change_with_mode_lib(&wt);

    let (r, calls) = run_pre_commit(&wt, 1);
    assert_eq!(r.code, 1, "{r:?}\ncalls:\n{calls}");
    assert!(
        calls.contains("check-plugin-version-bump.sh --staged"),
        "{calls}"
    );
}
