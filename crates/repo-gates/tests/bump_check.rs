//! Pass, violation and undecidable cases of the plugin version check in synthetic git
//! repositories. Nothing depends on the history or shallowness of this repository. Bash, so
//! Unix only; on Windows the cfg leaves the tests out, so a build there is not a test run.
//! Different errors can share an exit code; these tests do not check every failure branch
//! on its own.
//!
//! Ported from tasty tests/plugin_version_bump_channel.rs.

#![cfg(unix)]

mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use support::{gate_env, git_env};

fn script() -> String {
    support::repo_file("scripts/check-plugin-version-bump.sh")
}

fn run_git(dir: &Path, args: &[&str]) {
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
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().expect("parent")).expect("create dir");
    fs::write(&p, body).unwrap_or_else(|e| panic!("write {rel}: {e}"));
}

/// The gate as a child process without the judge, so a test does not depend on whether the
/// person running it installed one. Tests that need the judge use `judged_command`.
fn gate_command(gate: &str, dir: &Path) -> Command {
    let mut cmd = git_env::command("bash");
    cmd.arg(gate)
        .current_dir(dir)
        .env("TASTY_STRIP_CFG_TEST_BIN", "")
        .env("TASTY_JUDGE_CACHE", "/nonexistent/judge-cache");
    cmd
}

fn run(mut cmd: Command, args: &[&str]) -> (i32, String) {
    let out = cmd.args(args).output().expect("run the gate script");
    let run = gate_env::GateRun::from_output(&out);
    (run.code, run.output)
}

/// An explicit missing path, which is not replaced by a default one, reproduces a missing judge.
fn check_without_the_stripper(dir: &Path, args: &[&str]) -> (i32, String) {
    let mut cmd = gate_command(&script(), dir);
    cmd.env("TASTY_STRIP_CFG_TEST_BIN", "/nonexistent/strip-cfg-test");
    run(cmd, args)
}

fn check(dir: &Path, args: &[&str]) -> (i32, String) {
    run(gate_command(&script(), dir), args)
}

const PLUGIN: &str = "crates/tasty-plugin-fixture";

/// A copy of the script with another scan root, to see that every step that uses paths
/// follows the same root.
fn widened_script(d: &Path, root: &str) -> String {
    let src = fs::read_to_string(script()).expect("read the gate script");
    let from = "\nSCAN_ROOT=crates\n";
    assert_eq!(
        src.matches(from).count(),
        1,
        "the scan root must be declared in exactly one place"
    );
    // The helpers that the script reads by relative path are copied too.
    let gate_dir = d.join(".gate");
    fs::create_dir_all(gate_dir.join("lib")).expect("create the copy directory");
    for lib in ["judge-bin.sh", "judge-cache.sh"] {
        fs::copy(
            support::repo_file(&format!("scripts/lib/{lib}")),
            gate_dir.join("lib").join(lib),
        )
        .expect("copy a helper");
    }
    let out = gate_dir.join("widened-gate.sh");
    fs::write(&out, src.replace(from, &format!("\nSCAN_ROOT={root}\n"))).expect("write the copy");
    out.to_string_lossy().into_owned()
}

fn check_with(gate: &str, dir: &Path, args: &[&str]) -> (i32, String) {
    run(gate_command(gate, dir), args)
}

fn seed_repo() -> tempfile::TempDir {
    seed_repo_under("crates")
}

fn plugin_cargo_toml(v: &str) -> String {
    format!("[package]\nname = \"tasty-plugin-fixture\"\nversion = \"{v}\"\nedition = \"2024\"\n")
}

fn init_repo(d: &Path) {
    run_git(d, &["init", "--quiet"]);
    run_git(d, &["config", "user.email", "fixture@example.invalid"]);
    run_git(d, &["config", "user.name", "fixture"]);
    // Keep the user's git hooks out of the synthetic repository.
    run_git(d, &["config", "core.hooksPath", "/dev/null"]);
    run_git(d, &["config", "commit.gpgsign", "false"]);
}

fn seed_repo_under(root: &str) -> tempfile::TempDir {
    let plugin = format!("{root}/tasty-plugin-fixture");
    let tmp = tempfile::tempdir().expect("temp dir");
    let d = tmp.path();
    init_repo(d);
    write(
        d,
        "Cargo.toml",
        &format!("[workspace]\nmembers = [\"{root}/*\"]\n"),
    );
    write(
        d,
        &format!("{plugin}/Cargo.toml"),
        &plugin_cargo_toml("0.1.0"),
    );
    write(
        d,
        &format!("{plugin}/tasty-plugin.toml"),
        "id = \"com.tasty.fixture\"\nversion = \"0.1.0\"\n",
    );
    write(
        d,
        &format!("{plugin}/src/main.rs"),
        "fn main() {\n    let msg = \"a  b\";\n}\n",
    );
    run_git(d, &["add", "-A"]);
    run_git(d, &["commit", "--quiet", "-m", "seed"]);
    tmp
}

fn bump_to(d: &Path, v: &str) {
    write(d, &format!("{PLUGIN}/Cargo.toml"), &plugin_cargo_toml(v));
    write(
        d,
        &format!("{PLUGIN}/tasty-plugin.toml"),
        &format!("id = \"com.tasty.fixture\"\nversion = \"{v}\"\n"),
    );
}

fn commit_all(d: &Path, msg: &str) {
    run_git(d, &["add", "-A"]);
    run_git(d, &["commit", "--quiet", "-m", msg]);
}

/// A change that the original scope does not see must be found in both modes after widening.
#[test]
fn widening_the_left_side_moves_both_modes() {
    let tmp = seed_repo_under("extra");
    let d = tmp.path();
    let changed = "extra/tasty-plugin-fixture/src/main.rs";
    write(d, changed, "fn main() {\n    let msg = \"changed\";\n}\n");
    commit_all(d, "feat(fixture): change behaviour");

    let (base_code, base_text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        base_code, 0,
        "extra is checked before widening, so the effect cannot be compared:\n{base_text}"
    );
    assert!(
        base_text.contains("0 judged"),
        "nothing should be judged before widening:\n{base_text}"
    );

    let gate = widened_script(d, "extra");
    let (code, text) = check_with(&gate, d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 1,
        "--range did not check the added directory:\n{text}"
    );
    assert!(
        text.contains("src/main.rs"),
        "does not name the file:\n{text}"
    );

    write(
        d,
        changed,
        "fn main() {\n    let msg = \"changed twice\";\n}\n",
    );
    run_git(d, &["add", "-A"]);
    let (staged_code, staged_text) = check_with(&gate, d, &["--staged"]);
    assert_eq!(
        staged_code, 1,
        "--staged did not check the added directory:\n{staged_text}"
    );
}

#[test]
fn a_content_change_without_a_bump_is_rejected() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"changed\";\n}\n",
    );
    commit_all(d, "feat(fixture): change behaviour");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 1, "a change that needs a bump passed:\n{text}");
    assert!(
        text.contains("the version did not go up"),
        "the reason is missing:\n{text}"
    );
    assert!(text.contains("src/main.rs"), "the file is missing:\n{text}");
}

#[test]
fn the_same_change_with_a_bump_passes() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"changed\";\n}\n",
    );
    bump_to(d, "0.1.1");
    commit_all(d, "feat(fixture): change behaviour + bump");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 0, "blocked after a bump:\n{text}");
    assert!(
        text.contains("pass"),
        "a pass with a target is not called a pass:\n{text}"
    );
    assert!(
        text.contains("1 judged"),
        "the judged count is not 1 (the gate may not have looked at the change):\n{text}"
    );
}

#[test]
fn a_formatting_only_change_needs_no_bump() {
    let tmp = seed_repo();
    let d = tmp.path();
    // Only formatting differs; the spaces inside the string stay.
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn  main( )\n{\n        let  msg =   \"a  b\" ;\n}\n",
    );
    commit_all(d, "style(fixture): reformat");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 0, "asked for a bump on a formatting change:\n{text}");
    assert!(
        text.contains("0 judged"),
        "the formatting change was judged:\n{text}"
    );
    // The changed file count tells "nothing changed" from "left out".
    assert!(
        text.contains("of 1 changed files under crates"),
        "the changed file count is missing:\n{text}"
    );
}

#[test]
fn a_literal_only_change_is_not_swallowed_by_normalization() {
    let tmp = seed_repo();
    let d = tmp.path();
    // Spaces inside a string must not be removed by the formatting step.
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"ab\";\n}\n",
    );
    commit_all(d, "fix(fixture): collapse the literal");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 1,
        "a space change inside a string literal was normalized away (false negative):\n{text}"
    );
}

#[test]
fn a_docs_only_file_outside_the_build_output_needs_no_bump() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(d, &format!("{PLUGIN}/README.md"), "more notes\n");
    commit_all(d, "docs(fixture): add a readme");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 0,
        "asked for a bump on a file outside the output:\n{text}"
    );
    assert!(text.contains("0 judged"), "{text}");
}

/// With nothing to judge the exit code is 0, but nothing was checked, so the output must not
/// read as a pass.
#[test]
fn zero_judged_targets_read_as_none_not_as_a_pass() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(d, &format!("{PLUGIN}/README.md"), "more notes\n");
    commit_all(d, "docs(fixture): add a readme");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 0, "a commit with nothing to judge is normal:\n{text}");
    assert!(text.contains("nothing to judge"), "{text}");
    assert!(
        !text.contains("pass"),
        "0 judged was called a pass:\n{text}"
    );
}

#[test]
fn staged_mode_sees_the_index_before_a_commit_exists_for_it() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"staged\";\n}\n",
    );
    run_git(d, &["add", "-A"]);

    let (code, text) = check(d, &["--staged"]);
    assert_eq!(
        code, 1,
        "a staged change without a bump was not found:\n{text}"
    );

    bump_to(d, "0.1.1");
    run_git(d, &["add", "-A"]);
    let (code, text) = check(d, &["--staged"]);
    assert_eq!(code, 0, "blocked after a staged bump:\n{text}");
    assert!(text.contains("1 judged"), "{text}");
}

#[test]
fn a_new_plugin_has_nothing_to_bump_from() {
    let tmp = seed_repo();
    let d = tmp.path();
    let other = "crates/tasty-plugin-newcomer";
    write(
        d,
        &format!("{other}/Cargo.toml"),
        "[package]\nname = \"tasty-plugin-newcomer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    write(
        d,
        &format!("{other}/tasty-plugin.toml"),
        "id = \"com.tasty.newcomer\"\nversion = \"0.1.0\"\n",
    );
    write(d, &format!("{other}/src/main.rs"), "fn main() {}\n");
    commit_all(d, "feat(newcomer): add a plugin");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 0,
        "asked to bump the first version of a new plugin:\n{text}"
    );
}

#[test]
fn a_version_that_goes_down_is_undecidable_not_a_pass() {
    // A lower version cannot be told from a wrong range, so it is a measurement failure.
    // Both possible causes must be named.
    let tmp = seed_repo();
    let d = tmp.path();
    bump_to(d, "0.1.5");
    commit_all(d, "chore(fixture): move to 0.1.5");
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"down\";\n}\n",
    );
    bump_to(d, "0.1.4");
    commit_all(d, "feat(fixture): change with a lower version");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 2,
        "the version went down but this is not undecidable:\n{text}"
    );
    assert!(
        text.contains("the start of this range is newer than its end"),
        "the lowered plugin is not named:\n{text}"
    );
    assert!(
        text.contains("you are behind") && text.contains("lowered on purpose"),
        "only one of the two causes is named:\n{text}"
    );
    assert!(
        !text.contains("the version did not go up"),
        "a lowered version was reported as not raised, which invites a bump:\n{text}"
    );
}

#[test]
fn a_version_change_alone_is_not_evidence_that_the_artifact_changed() {
    // Counting a version-only revert as content would ask for another bump in a loop.
    let tmp = seed_repo();
    let d = tmp.path();
    bump_to(d, "0.1.5");
    commit_all(d, "chore(fixture): move to 0.1.5");
    bump_to(d, "0.1.4");
    commit_all(d, "revert(fixture): put the value back");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 0,
        "asked for another bump after a value revert:\n{text}"
    );
    assert!(
        text.contains("0 judged"),
        "the version line itself made a target:\n{text}"
    );
}

#[test]
fn the_rest_of_the_manifest_is_still_content() {
    // Leaving out the version line must not leave out the whole manifest.
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/Cargo.toml"),
        &format!(
            "{}\n[features]\ndefault = [\"extra\"]\nextra = []\n",
            plugin_cargo_toml("0.1.0")
        ),
    );
    commit_all(d, "feat(fixture): turn on a feature");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 1,
        "a line other than the version changed but it passed:\n{text}"
    );
    assert!(text.contains("Cargo.toml"), "the file is missing:\n{text}");
}

#[test]
fn a_non_git_directory_is_undecidable_not_a_pass() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (code, text) = check(tmp.path(), &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 2,
        "outside git the exit code is not undecidable; 0 would count undecidable as a pass:\n{text}"
    );
    assert!(text.contains("undecidable"), "{text}");
}

#[test]
fn a_missing_rev_is_undecidable_not_a_pass() {
    let tmp = seed_repo();
    let (code, text) = check(tmp.path(), &["--range", "deadbeefdeadbeef", "HEAD"]);
    assert_eq!(
        code, 2,
        "a missing rev (as in a shallow clone) is not undecidable:\n{text}"
    );
    assert!(
        text.contains("shallow"),
        "the shallow hint is missing:\n{text}"
    );
}

#[test]
fn the_first_commit_has_no_baseline_and_says_so() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let d = tmp.path();
    init_repo(d);
    write(d, "Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n");
    write(
        d,
        &format!("{PLUGIN}/tasty-plugin.toml"),
        "id = \"com.tasty.fixture\"\nversion = \"0.1.0\"\n",
    );
    run_git(d, &["add", "-A"]);

    let (code, text) = check(d, &["--staged"]);
    assert_eq!(code, 0, "blocked on the first commit:\n{text}");
    assert!(
        text.contains("first commit"),
        "the output does not say it is the first commit, so it reads like a silent pass:\n{text}"
    );
}

/// Only an unborn HEAD is a first commit. A --base that names no object is undecidable.
#[test]
fn a_base_that_names_no_commit_is_undecidable_not_a_first_commit() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"staged\";\n}\n",
    );
    run_git(d, &["add", "-A"]);

    let (code, text) = check(
        d,
        &[
            "--staged",
            "--base",
            "0123456789012345678901234567890123456789",
        ],
    );
    assert_eq!(code, 2, "{text}");
    assert!(!text.contains("first commit"), "{text}");
    assert!(text.contains("cannot find the --base rev"), "{text}");
}

/// Staged mode compares the whole base..index difference: a change committed after the base
/// still counts when only an unrelated file is staged on top of it.
#[test]
fn staged_mode_lists_the_changes_between_the_base_and_the_index() {
    let tmp = seed_repo();
    let d = tmp.path();
    let base = {
        let out = git_env::command("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(d)
            .output()
            .expect("rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"committed\";\n}\n",
    );
    commit_all(d, "change the plugin without a bump");
    write(d, "notes.md", "unrelated\n");
    run_git(d, &["add", "-A"]);

    let (code, text) = check(d, &["--staged", "--base", &base]);
    assert_eq!(
        code, 1,
        "the committed change after the base was not listed:\n{text}"
    );
    assert!(
        text.contains("crates/tasty-plugin-fixture: shipped content changed"),
        "{text}"
    );
}

#[test]
fn crates_that_disagree_on_the_edition_are_undecidable() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        "crates/other/Cargo.toml",
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"changed\";\n}\n",
    );
    commit_all(d, "feat(fixture): change behaviour");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 2, "two editions must be undecidable:\n{text}");
    assert!(text.contains("must declare one edition"), "{text}");
    assert!(text.contains("2021") && text.contains("2024"), "{text}");
}

// Whether the judge is stale comes from its --check-fresh exit code. A stale judge is not used,
// test-only code is then compared too, and the gate says so. These tests check the exit code
// contract; the fingerprint itself is tested in tasty.

fn plant_judge(d: &Path, fresh: bool) {
    let bin = d.join("target/debug/strip-cfg-test");
    let body = if fresh {
        "#!/bin/sh\nexit 0\n"
    } else {
        "#!/bin/sh\n[ \"$1\" = \"--check-fresh\" ] && exit 1\nexit 0\n"
    };
    support::write_executable(&bin, body).expect("stub judge");
}

#[test]
fn a_judge_that_reports_itself_stale_is_announced_and_widens() {
    let tmp = seed_repo();
    let d = tmp.path();
    plant_judge(d, false);
    let (_, text) = check(d, &["--staged"]);
    assert!(
        text.contains("was not built from the current source"),
        "a judge not built from the current source was used without a notice:\n{text}"
    );
    assert!(
        text.contains("bash scripts/install-judge.sh"),
        "does not say what to do:\n{text}"
    );
}

#[test]
fn a_judge_that_reports_itself_fresh_is_not_called_stale() {
    let tmp = seed_repo();
    let d = tmp.path();
    plant_judge(d, true);
    let (_, text) = check(d, &["--staged"]);
    assert!(
        !text.contains("was not built from the current source"),
        "a judge built from the current source was called stale:\n{text}"
    );
}

/// Real manifests that cargo tree can read, with one crate the plugin links and one it does not.
fn seed_workspace() -> tempfile::TempDir {
    seed_workspace_under("crates")
}

fn seed_workspace_under(root: &str) -> tempfile::TempDir {
    let plugin = format!("{root}/tasty-plugin-fixture");
    let tmp = tempfile::tempdir().expect("temp dir");
    let d = tmp.path();
    init_repo(d);

    write(
        d,
        "Cargo.toml",
        &format!(
            "[workspace]\nresolver = \"2\"\n\
             members = [\"{root}/tasty-plugin-fixture\", \"{root}/tasty-shared\", \"{root}/tasty-lonely\"]\n"
        ),
    );
    write(
        d,
        &format!("{plugin}/Cargo.toml"),
        &format!(
            "{}[dependencies]\ntasty-shared = {{ path = \"../tasty-shared\" }}\n",
            plugin_cargo_toml("0.1.0")
        ),
    );
    write(
        d,
        &format!("{plugin}/tasty-plugin.toml"),
        "id = \"com.tasty.fixture\"\nversion = \"0.1.0\"\n",
    );
    write(
        d,
        &format!("{plugin}/src/main.rs"),
        "fn main() {\n    tasty_shared::greet();\n}\n",
    );
    for c in ["tasty-shared", "tasty-lonely"] {
        write(
            d,
            &format!("{root}/{c}/Cargo.toml"),
            &format!("[package]\nname = \"{c}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        );
        write(
            d,
            &format!("{root}/{c}/src/lib.rs"),
            "pub fn greet() {\n    let _ = 1;\n}\n",
        );
    }
    run_git(d, &["add", "-A"]);
    run_git(d, &["commit", "--quiet", "-m", "seed"]);
    tmp
}

/// The plugin roster that only shared crate changes build must follow the widened root too.
#[test]
fn widening_the_left_side_moves_the_plugin_roster() {
    let tmp = seed_workspace_under("extra");
    let d = tmp.path();
    write(
        d,
        "extra/tasty-shared/src/lib.rs",
        "pub fn greet() {\n    let _ = 2;\n}\n",
    );
    commit_all(d, "fix(shared): change behaviour");

    let (base_code, base_text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        base_code, 0,
        "extra is checked before widening, so the effect cannot be compared:\n{base_text}"
    );

    let gate = widened_script(d, "extra");
    let (code, text) = check_with(&gate, d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 1,
        "the plugin under the widened root is not in the roster; check the manifest lookup:\n{text}"
    );
    assert!(
        text.contains("tasty-shared/src/lib.rs"),
        "does not name the file:\n{text}"
    );
}

#[test]
fn a_change_in_a_linked_workspace_crate_demands_a_bump() {
    let tmp = seed_workspace();
    let d = tmp.path();
    write(
        d,
        "crates/tasty-shared/src/lib.rs",
        "pub fn greet() {\n    let _ = 2;\n}\n",
    );
    commit_all(d, "fix(shared): change behaviour");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 1,
        "a linked crate changed but no bump was asked for:\n{text}"
    );
    assert!(
        text.contains("tasty-shared/src/lib.rs"),
        "the file is missing:\n{text}"
    );
}

#[test]
fn a_change_in_an_unlinked_workspace_crate_does_not() {
    let tmp = seed_workspace();
    let d = tmp.path();
    write(
        d,
        "crates/tasty-lonely/src/lib.rs",
        "pub fn greet() {\n    let _ = 2;\n}\n",
    );
    commit_all(d, "fix(lonely): change behaviour");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 0, "a crate no plugin links asked for a bump:\n{text}");
}

// Path dependencies outside the workspace are in scope too. These synthetic tests have no real
// judge, so they do not check that test-only changes in such a dependency are left out.

/// The path dependencies under the root are excluded so they do not become members.
fn seed_vendored_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    let d = tmp.path();
    init_repo(d);
    write(
        d,
        "Cargo.toml",
        "[workspace]\nresolver = \"2\"\n\
         members = [\"crates/tasty-plugin-fixture\"]\n\
         exclude = [\"vendor/upstream\", \"vendor/orphan\"]\n",
    );
    write(
        d,
        &format!("{PLUGIN}/Cargo.toml"),
        &format!(
            "{}[dependencies]\nupstream = {{ path = \"../../vendor/upstream\" }}\n",
            plugin_cargo_toml("0.1.0")
        ),
    );
    write(
        d,
        &format!("{PLUGIN}/tasty-plugin.toml"),
        "id = \"com.tasty.fixture\"\nversion = \"0.1.0\"\n",
    );
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    upstream::greet();\n}\n",
    );
    for c in ["upstream", "orphan"] {
        write(
            d,
            &format!("vendor/{c}/Cargo.toml"),
            &format!("[package]\nname = \"{c}\"\nversion = \"0.1.0\"\nedition = \"2018\"\n"),
        );
        write(
            d,
            &format!("vendor/{c}/src/lib.rs"),
            "pub fn greet() {\n    let _ = 1;\n}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn probe() {\n        assert_eq!(1, 1);\n    }\n}\n",
        );
    }
    run_git(d, &["add", "-A"]);
    run_git(d, &["commit", "--quiet", "-m", "seed"]);
    tmp
}

#[test]
fn a_change_in_a_linked_path_dependency_outside_the_workspace_demands_a_bump() {
    let tmp = seed_vendored_workspace();
    let d = tmp.path();
    write(
        d,
        "vendor/upstream/src/lib.rs",
        "pub fn greet() {\n    let _ = 2;\n}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn probe() {\n        assert_eq!(1, 1);\n    }\n}\n",
    );
    run_git(d, &["add", "-A"]);

    let (code, text) = check(d, &["--staged"]);
    assert_eq!(
        code, 1,
        "a path dependency outside the workspace changed but no bump was asked for:\n{text}"
    );
    assert!(
        text.contains("vendor/upstream/src/lib.rs"),
        "the file is missing:\n{text}"
    );

    run_git(
        d,
        &["commit", "--quiet", "-m", "fix(upstream): change behaviour"],
    );
    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 1, "--range did not find the outside change:\n{text}");
}

#[test]
fn a_change_in_an_unlinked_path_dependency_outside_the_workspace_does_not() {
    let tmp = seed_vendored_workspace();
    let d = tmp.path();
    write(
        d,
        "vendor/orphan/src/lib.rs",
        "pub fn greet() {\n    let _ = 2;\n}\n",
    );
    commit_all(d, "fix(orphan): change behaviour");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 0, "a copy no plugin links asked for a bump:\n{text}");
}

fn path_with(stub: &Path) -> String {
    format!(
        "{}:{}",
        stub.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[test]
fn an_unreadable_member_roster_is_undecidable_and_says_why() {
    const MARK: &str = "zz-cargo-metadata-died-here";
    let tmp = seed_vendored_workspace();
    let d = tmp.path();
    write(
        d,
        "vendor/upstream/src/lib.rs",
        "pub fn greet() {\n    let _ = 2;\n}\n",
    );
    commit_all(d, "fix(upstream): change behaviour");

    let stub = tempfile::tempdir().expect("stub dir");
    support::write_executable(
        &stub.path().join("cargo"),
        // tree succeeds so that no other error comes before the metadata failure.
        format!(
            "#!/bin/sh\nif [ \"$1\" = metadata ]; then echo {MARK} >&2; exit 101; fi\nexec \"{real}\" \"$@\"\n",
            real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
        ),
    )
    .expect("write the stub");

    let mut cmd = gate_command(&script(), d);
    cmd.env("PATH", path_with(stub.path()));
    let (code, text) = run(cmd, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 2,
        "the member roster was unreadable but this is not undecidable:\n{text}"
    );
    assert!(text.contains("cargo metadata failed"), "{text}");
    assert!(text.contains(MARK), "cargo's stderr is missing:\n{text}");
}

/// The closure runs in a subshell; its failure must reach the outer script.
#[test]
fn an_unreadable_dependency_closure_is_undecidable_and_says_why() {
    const MARK: &str = "zz-cargo-tree-died-here";
    let tmp = seed_workspace();
    let d = tmp.path();
    write(
        d,
        "crates/tasty-shared/src/lib.rs",
        "pub fn greet() {\n    let _ = 2;\n}\n",
    );
    commit_all(d, "fix(shared): change behaviour");

    let stub = tempfile::tempdir().expect("stub dir");
    support::write_executable(
        &stub.path().join("cargo"),
        format!("#!/bin/sh\nif [ \"$1\" = tree ]; then echo {MARK} >&2; exit 101; fi\nexit 0\n"),
    )
    .expect("write the stub");

    let mut cmd = gate_command(&script(), d);
    cmd.env("PATH", path_with(stub.path()));
    let (code, text) = run(cmd, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 2,
        "the closure was unreadable but this is not undecidable; 0 would let a linked change pass:\n{text}"
    );
    assert!(text.contains("cargo tree failed"), "{text}");
    assert!(text.contains(MARK), "cargo's stderr is missing:\n{text}");
}

#[test]
fn a_missing_shipping_judge_widens_and_says_so() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"changed\";\n}\n",
    );
    commit_all(d, "feat(fixture): change behaviour");

    let (code, text) = check(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("cannot narrow to shipped code"),
        "does not say the check got wider:\n{text}"
    );
}

#[test]
fn without_the_stripper_a_test_only_change_is_told_to_bump_and_that_is_deliberate() {
    // pre-commit also runs in a new clone without the judge. Then the raw text is compared and
    // a test change asks for a bump too. A missing judge is neither a pass nor undecidable.
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"a  b\";\n}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn probe() {}\n}\n",
    );
    commit_all(d, "test(fixture): add a test-only block");

    let (code, text) = check_without_the_stripper(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(
        code, 1,
        "without the judge the raw text is compared and this test change needs a bump:\n{text}"
    );
    assert!(text.contains("cannot narrow to shipped code"), "{text}");
}

/// The judge from scripts/install-judge.sh. scripts/check.sh test exports it; running the
/// tests without it is a setup error, not a skip.
fn installed_judge() -> String {
    match std::env::var("TASTY_STRIP_CFG_TEST_BIN") {
        Ok(bin) if !bin.is_empty() => bin,
        _ => panic!(
            "this test needs the real judge: export TASTY_STRIP_CFG_TEST_BIN=\"$(bash scripts/install-judge.sh)\" \
             or run scripts/check.sh test"
        ),
    }
}

fn check_with_the_judge(dir: &Path, args: &[&str]) -> (i32, String) {
    let mut cmd = gate_command(&script(), dir);
    cmd.env("TASTY_STRIP_CFG_TEST_BIN", installed_judge());
    run(cmd, args)
}

#[test]
fn with_the_judge_a_test_only_change_needs_no_bump() {
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"a  b\";\n}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn probe() {}\n}\n",
    );
    commit_all(d, "test(fixture): add a test-only block");

    let (code, text) = check_with_the_judge(d, &["--range", "HEAD^", "HEAD"]);
    assert_eq!(code, 0, "a test-only change asked for a bump:\n{text}");
    assert!(text.contains("nothing to judge"), "{text}");
    assert!(
        !text.contains("cannot narrow"),
        "the judge was not used:\n{text}"
    );
}

/// The printed count follows the judged plugins, apart from the plugin count and the file count.
#[test]
fn the_reported_considered_count_follows_the_judged_plugins() {
    const OTHER: &str = "crates/tasty-plugin-second";
    let tmp = seed_repo();
    let d = tmp.path();
    write(
        d,
        &format!("{OTHER}/Cargo.toml"),
        "[package]\nname = \"tasty-plugin-second\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    write(
        d,
        &format!("{OTHER}/tasty-plugin.toml"),
        "id = \"com.tasty.second\"\nversion = \"0.1.0\"\n",
    );
    write(d, &format!("{OTHER}/src/main.rs"), "fn main() {}\n");
    commit_all(d, "a second plugin");

    write(
        d,
        &format!("{PLUGIN}/src/main.rs"),
        "fn main() {\n    let msg = \"changed\";\n}\n",
    );
    bump_to(d, "0.1.1");
    commit_all(d, "change and bump only the first plugin");

    let (code, text) = check(d, &["--range", "HEAD~1", "HEAD"]);
    assert_eq!(
        code, 0,
        "the value followed but this is not a pass:\n{text}"
    );
    assert!(
        text.contains("1 judged"),
        "the printed count differs from the judged plugins:\n{text}"
    );
    assert!(
        text.contains("of 3 changed files under crates"),
        "the file count is not told apart from the judged count:\n{text}"
    );
}

#[test]
fn a_missing_rustfmt_is_undecidable_not_a_pass() {
    let tmp = seed_repo();
    // Keep git, leave rustfmt out of PATH. The message must tell this apart from other errors.
    let path = gate_env::only(&["bash", "git"]);
    let mut cmd = gate_command(&script(), tmp.path());
    cmd.env("PATH", path.path());
    let (code, text) = run(cmd, &["--staged"]);
    assert_eq!(
        code, 2,
        "rustfmt is missing but this is not undecidable:\n{text}"
    );
    assert!(text.contains("rustfmt is missing"), "{text}");
}

#[test]
fn no_mode_argument_is_undecidable_not_a_pass() {
    let tmp = seed_repo();
    let (code, text) = check(tmp.path(), &[]);
    assert_eq!(
        code, 2,
        "no mode was given but this is not undecidable:\n{text}"
    );
    assert!(text.contains("--staged or --range"), "{text}");
}

// ── tasty pin moves ──────────────────────────────────────────────────────────
// A synthetic "tasty" repository is a git dependency through a file:// URL. CARGO_HOME is a
// temporary directory, so the user's cache is not touched, and the gate reads the commits from
// the cargo git database that `cargo metadata` fills.

/// - tasty-a: used by alpha, reads themes/x.toml inside its crate.
/// - tasty-b: used by beta.
/// - tasty-i18n: used by alpha, reads lang/en.toml at the tasty root (listed in
///   scripts/lib/tasty-extra-inputs.txt).
struct PinFixture {
    _tmp: tempfile::TempDir,
    tasty: std::path::PathBuf,
    repo: std::path::PathBuf,
    cargo_home: std::path::PathBuf,
}

fn real_cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

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

const TASTY_A_LIB: &str = "pub const THEME: &str = include_str!(\"../themes/x.toml\");\n\
pub fn a() -> u32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert_eq!(super::a(), 1);\n    }\n}\n";

impl PinFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let tasty = tmp.path().join("tasty");
        let repo = tmp.path().join("plugins");
        let cargo_home = tmp.path().join("cargo-home");
        fs::create_dir_all(&tasty).expect("tasty dir");
        fs::create_dir_all(&repo).expect("plugins dir");
        fs::create_dir_all(&cargo_home).expect("cargo home");

        init_repo(&tasty);
        write(
            &tasty,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        );
        for c in ["tasty-a", "tasty-b", "tasty-i18n"] {
            write(
                &tasty,
                &format!("crates/{c}/Cargo.toml"),
                &format!("[package]\nname = \"{c}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            );
        }
        write(&tasty, "crates/tasty-a/src/lib.rs", TASTY_A_LIB);
        write(&tasty, "crates/tasty-a/themes/x.toml", "accent = 1\n");
        write(
            &tasty,
            "crates/tasty-b/src/lib.rs",
            "pub fn b() -> u32 {\n    2\n}\n",
        );
        write(
            &tasty,
            "crates/tasty-i18n/src/lib.rs",
            "pub const EN: &str = include_str!(\"../../../lang/en.toml\");\n",
        );
        write(&tasty, "lang/en.toml", "hello = \"Hello\"\n");
        commit_all(&tasty, "tasty seed");
        let t1 = git_out(&tasty, &["rev-parse", "HEAD"]);

        init_repo(&repo);
        write(&repo, ".gitignore", "target\n");
        let url = format!("file://{}", tasty.display());
        write(
            &repo,
            "Cargo.toml",
            &format!(
                "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n\n\
                 [workspace.dependencies]\n\
                 tasty-a = {{ git = \"{url}\", rev = \"{t1}\" }}\n\
                 tasty-b = {{ git = \"{url}\", rev = \"{t1}\" }}\n\
                 tasty-i18n = {{ git = \"{url}\", rev = \"{t1}\" }}\n\n\
                 [profile.release]\nopt-level = 3\n\n\
                 [profile.dist]\ninherits = \"release\"\n"
            ),
        );
        for (p, deps) in [
            (
                "alpha",
                "tasty-a.workspace = true\ntasty-i18n.workspace = true\n",
            ),
            ("beta", "tasty-b.workspace = true\n"),
        ] {
            write(
                &repo,
                &format!("crates/tasty-plugin-{p}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"tasty-plugin-{p}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
                     [dependencies]\n{deps}"
                ),
            );
            write(
                &repo,
                &format!("crates/tasty-plugin-{p}/tasty-plugin.toml"),
                &format!("id = \"com.tasty.{p}\"\nversion = \"0.1.0\"\n"),
            );
            write(
                &repo,
                &format!("crates/tasty-plugin-{p}/src/main.rs"),
                "fn main() {}\n",
            );
        }
        let f = Self {
            _tmp: tmp,
            tasty,
            repo,
            cargo_home,
        };
        f.cargo(&["generate-lockfile"]);
        commit_all(&f.repo, "seed");
        f
    }

    fn cargo(&self, args: &[&str]) {
        let out = git_env::command(real_cargo())
            .args(args)
            .current_dir(&self.repo)
            .env("CARGO_HOME", &self.cargo_home)
            .output()
            .expect("run cargo");
        assert!(
            out.status.success(),
            "cargo {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn tasty_write(&self, rel: &str, body: &str) {
        write(&self.tasty, rel, body);
    }

    fn tasty_commit(&self, msg: &str) -> String {
        commit_all(&self.tasty, msg);
        git_out(&self.tasty, &["rev-parse", "HEAD"])
    }

    /// Points every tasty entry at `rev`, lets Cargo update the lock, and commits.
    fn move_pin(&self, rev: &str) {
        let toml = fs::read_to_string(self.repo.join("Cargo.toml")).expect("read Cargo.toml");
        let mut out = String::new();
        for line in toml.lines() {
            match line.split_once("rev = \"") {
                Some((head, tail)) => {
                    let rest = tail.split_once('"').expect("closing quote").1;
                    out.push_str(&format!("{head}rev = \"{rev}\"{rest}\n"));
                }
                None => {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        write(&self.repo, "Cargo.toml", &out);
        self.cargo(&["metadata", "--format-version", "1"]);
    }

    fn bump(&self, plugin: &str, v: &str) {
        for rel in [
            format!("crates/tasty-plugin-{plugin}/Cargo.toml"),
            format!("crates/tasty-plugin-{plugin}/tasty-plugin.toml"),
        ] {
            let body = fs::read_to_string(self.repo.join(&rel)).expect("read");
            let line = body
                .lines()
                .find(|l| l.starts_with("version"))
                .expect("version line")
                .to_string();
            write(
                &self.repo,
                &rel,
                &body.replacen(&line, &format!("version = \"{v}\""), 1),
            );
        }
        self.cargo(&["metadata", "--format-version", "1"]);
    }

    fn commit(&self, msg: &str) {
        commit_all(&self.repo, msg);
    }

    fn command(&self, judged: bool) -> Command {
        let mut cmd = gate_command(&script(), &self.repo);
        cmd.env("CARGO_HOME", &self.cargo_home)
            .env_remove("TASTY_GIT_DIR");
        if judged {
            cmd.env("TASTY_STRIP_CFG_TEST_BIN", installed_judge());
        }
        cmd
    }

    fn check(&self, args: &[&str]) -> (i32, String) {
        run(self.command(false), args)
    }

    fn check_judged(&self, args: &[&str]) -> (i32, String) {
        run(self.command(true), args)
    }
}

const HEAD_RANGE: [&str; 3] = ["--range", "HEAD^", "HEAD"];

/// Case 1: A pin move that changes only a crate in alpha's closure.
#[test]
fn a_pin_move_flags_only_the_plugins_whose_closure_changed() {
    let f = PinFixture::new();
    f.tasty_write(
        "crates/tasty-a/src/lib.rs",
        &TASTY_A_LIB.replace("    1\n}\n\n#", "    10\n}\n\n#"),
    );
    let t2 = f.tasty_commit("change a");
    f.move_pin(&t2);
    f.commit("move the pin");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("crates/tasty-plugin-alpha: shipped content changed"),
        "{text}"
    );
    assert!(
        !text.contains("crates/tasty-plugin-beta:"),
        "beta does not link tasty-a:\n{text}"
    );
    // The cause names the crate with both lock versions, the pin move, and the plugins.
    assert!(text.contains("tasty-a 0.1.0 → 0.1.0 (tasty "), "{text}");
    assert!(text.contains("crates/tasty-a/src/lib.rs"), "{text}");
    assert!(
        text.contains("plugins: crates/tasty-plugin-alpha"),
        "{text}"
    );
}

/// Case 2: The same move with alpha's patch version raised passes.
#[test]
fn a_pin_move_with_the_bump_passes() {
    let f = PinFixture::new();
    f.tasty_write(
        "crates/tasty-a/src/lib.rs",
        &TASTY_A_LIB.replace("    1\n}\n\n#", "    10\n}\n\n#"),
    );
    let t2 = f.tasty_commit("change a");
    f.move_pin(&t2);
    f.bump("alpha", "0.1.1");
    f.commit("move the pin and bump alpha");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("pass — 1 judged"), "{text}");
}

/// Case 3: Only the #[cfg(test)] part changed; needs the real judge.
#[test]
fn a_pin_move_that_changes_only_test_code_needs_no_bump() {
    let f = PinFixture::new();
    f.tasty_write(
        "crates/tasty-a/src/lib.rs",
        &TASTY_A_LIB.replace(
            "assert_eq!(super::a(), 1);",
            "assert_eq!(super::a(), 1, \"a\");",
        ),
    );
    let t2 = f.tasty_commit("change a test");
    f.move_pin(&t2);
    f.commit("move the pin");

    let (code, text) = f.check_judged(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("nothing to judge"), "{text}");
}

/// A test-only module file that is added and later removed ships nothing either way; needs
/// the real judge.
#[test]
fn a_pin_move_that_adds_or_removes_a_test_only_module_file_needs_no_bump() {
    let f = PinFixture::new();
    f.tasty_write(
        "crates/tasty-a/src/lib.rs",
        &format!("{TASTY_A_LIB}\n#[cfg(test)]\nmod more;\n"),
    );
    f.tasty_write(
        "crates/tasty-a/src/more.rs",
        "#[test]\nfn more() {\n    assert_eq!(super::a(), 1);\n}\n",
    );
    let t2 = f.tasty_commit("add a test module file");
    f.move_pin(&t2);
    f.commit("move the pin to the added test file");

    let (code, text) = f.check_judged(&HEAD_RANGE);
    assert_eq!(code, 0, "adding a test-only file asked for a bump:\n{text}");
    assert!(text.contains("nothing to judge"), "{text}");

    f.tasty_write("crates/tasty-a/src/lib.rs", TASTY_A_LIB);
    fs::remove_file(f.tasty.join("crates/tasty-a/src/more.rs")).expect("remove more.rs");
    let t3 = f.tasty_commit("remove the test module file");
    f.move_pin(&t3);
    f.commit("move the pin past the removed test file");

    let (code, text) = f.check_judged(&HEAD_RANGE);
    assert_eq!(
        code, 0,
        "removing a test-only file asked for a bump:\n{text}"
    );
    assert!(text.contains("nothing to judge"), "{text}");
}

/// Case 4: Only the formatting changed.
#[test]
fn a_pin_move_that_changes_only_formatting_needs_no_bump() {
    let f = PinFixture::new();
    f.tasty_write(
        "crates/tasty-b/src/lib.rs",
        "pub fn   b( ) -> u32\n{\n        2\n}\n",
    );
    let t2 = f.tasty_commit("reformat b");
    f.move_pin(&t2);
    f.commit("move the pin");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("nothing to judge"), "{text}");
}

/// Case 5: A file inside the crate but outside src/ (read with include_str!) is content.
#[test]
fn a_pin_move_that_changes_a_crate_file_outside_src_is_content() {
    let f = PinFixture::new();
    f.tasty_write("crates/tasty-a/themes/x.toml", "accent = 2\n");
    let t2 = f.tasty_commit("change a theme");
    f.move_pin(&t2);
    f.commit("move the pin");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("crates/tasty-a/themes/x.toml"), "{text}");
    assert!(!text.contains("crates/tasty-plugin-beta:"), "{text}");
}

/// Case 6: A listed input at the tasty root (lang/) reaches only the plugins that link its crate.
#[test]
fn a_pin_move_that_changes_a_listed_root_input_flags_its_crate() {
    let f = PinFixture::new();
    f.tasty_write("lang/en.toml", "hello = \"Hi\"\n");
    let t2 = f.tasty_commit("change a string");
    f.move_pin(&t2);
    f.commit("move the pin");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("tasty-i18n 0.1.0 → 0.1.0"), "{text}");
    assert!(text.contains("lang/en.toml"), "{text}");
    assert!(
        text.contains("crates/tasty-plugin-alpha: shipped content changed"),
        "{text}"
    );
    assert!(!text.contains("crates/tasty-plugin-beta:"), "{text}");
}

/// Case 7: An unlisted path outside a crate stops the check.
#[test]
fn a_pin_move_with_an_unlisted_outside_input_is_undecidable() {
    let f = PinFixture::new();
    f.tasty_write("other/x.txt", "data\n");
    f.tasty_write(
        "crates/tasty-b/src/lib.rs",
        "pub const X: &str = include_str!(\"../../../other/x.txt\");\npub fn b() -> u32 {\n    2\n}\n",
    );
    let t2 = f.tasty_commit("read a file outside the crate");
    f.move_pin(&t2);
    f.commit("move the pin");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("tasty-b reads other/x.txt outside its crate"),
        "{text}"
    );
    assert!(text.contains("tasty-extra-inputs.txt"), "{text}");
}

/// Case 8: The old commit cannot be found anywhere (no TASTY_GIT_DIR, not in the cargo database,
/// and the URL does not have it).
#[test]
fn a_pin_move_from_an_unreadable_commit_is_undecidable() {
    let f = PinFixture::new();
    let t1 = git_out(&f.tasty, &["rev-parse", "HEAD"]);
    let lock = fs::read_to_string(f.repo.join("Cargo.lock")).expect("read lock");
    let gone = "1111111111111111111111111111111111111111";
    write(&f.repo, "Cargo.lock", &lock.replace(&t1, gone));
    f.commit("a lock that names a commit nobody has");
    write(&f.repo, "Cargo.lock", &lock);
    f.commit("back to the real commit");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("cannot read file://"), "{text}");
    assert!(text.contains("11111111"), "{text}");
}

/// Case 15: A lower version together with a pin move is undecidable, not a pass.
#[test]
fn a_pin_move_with_a_lower_version_is_undecidable() {
    let f = PinFixture::new();
    f.bump("alpha", "0.1.5");
    f.commit("alpha at 0.1.5");
    f.tasty_write(
        "crates/tasty-a/src/lib.rs",
        &TASTY_A_LIB.replace("    1\n}\n\n#", "    10\n}\n\n#"),
    );
    let t2 = f.tasty_commit("change a");
    f.move_pin(&t2);
    f.bump("alpha", "0.1.4");
    f.commit("move the pin and lower alpha");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("the start of this range is newer than its end"),
        "{text}"
    );
}

// ── root build inputs ────────────────────────────────────────────────────────

/// Case 9: Only an entry of [workspace.dependencies] changed (no rev change).
#[test]
fn a_workspace_dependency_entry_change_flags_the_plugins_that_link_it() {
    let f = PinFixture::new();
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    let line = toml
        .lines()
        .find(|l| l.starts_with("tasty-b = "))
        .expect("tasty-b entry")
        .to_string();
    let changed = line.replacen(" }", ", default-features = false }", 1);
    write(&f.repo, "Cargo.toml", &toml.replacen(&line, &changed, 1));
    f.cargo(&["metadata", "--format-version", "1"]);
    f.commit("turn off the default features of tasty-b");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("crates/tasty-plugin-beta: shipped content changed"),
        "{text}"
    );
    assert!(!text.contains("crates/tasty-plugin-alpha:"), "{text}");
    assert!(
        text.contains("Cargo.toml [workspace.dependencies] tasty-b:"),
        "{text}"
    );
    assert!(text.contains("default-features = false"), "{text}");
}

impl PinFixture {
    /// Gives tasty-a the features `x` and `y` and moves the pin there, so that entries can
    /// name them.
    fn with_features(&self) {
        self.tasty_write(
            "crates/tasty-a/Cargo.toml",
            "[package]\nname = \"tasty-a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [features]\nx = []\ny = []\n",
        );
        let t2 = self.tasty_commit("add features");
        self.move_pin(&t2);
        self.commit("move the pin to the features");
    }

    /// Replaces the tasty-a entry line of the root Cargo.toml with `with(line)`.
    fn edit_tasty_a_entry(&self, with: impl Fn(&str) -> String) {
        let toml = fs::read_to_string(self.repo.join("Cargo.toml")).expect("read");
        let line = toml
            .lines()
            .find(|l| l.trim_start().starts_with("tasty-a = "))
            .expect("tasty-a entry")
            .to_string();
        write(
            &self.repo,
            "Cargo.toml",
            &toml.replacen(&line, &with(&line), 1),
        );
        self.cargo(&["metadata", "--format-version", "1"]);
    }

    fn assert_only_alpha_flagged(&self, what: &str) {
        let (code, text) = self.check(&HEAD_RANGE);
        assert_eq!(code, 1, "{what}:\n{text}");
        assert!(
            text.contains("crates/tasty-plugin-alpha: shipped content changed"),
            "{what}:\n{text}"
        );
        assert!(
            !text.contains("crates/tasty-plugin-beta:"),
            "{what}:\n{text}"
        );
        assert!(
            text.contains("Cargo.toml [workspace.dependencies] tasty-a:"),
            "{what}:\n{text}"
        );
    }
}

/// An entry written over several lines is one entry: changing only a continuation line
/// changes the entry.
#[test]
fn a_continuation_line_of_a_multi_line_entry_is_part_of_the_entry() {
    let f = PinFixture::new();
    f.with_features();
    f.edit_tasty_a_entry(|l| l.replacen(" }", ", features = [\n    \"x\",\n] }", 1));
    f.commit("list the features of tasty-a on several lines");
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replacen("    \"x\",\n", "    \"x\",\n    \"y\",\n", 1),
    );
    f.cargo(&["metadata", "--format-version", "1"]);
    f.commit("add a feature on its own line");

    f.assert_only_alpha_flagged("a continuation line was skipped");
}

/// `name.features = [...]` belongs to the entry `name`.
#[test]
fn a_dotted_key_belongs_to_its_entry() {
    let f = PinFixture::new();
    f.with_features();
    f.edit_tasty_a_entry(|l| {
        let inner = l
            .trim_start_matches("tasty-a = { ")
            .trim_end_matches(" }")
            .to_string();
        inner
            .split(", ")
            .map(|kv| format!("tasty-a.{kv}"))
            .collect::<Vec<_>>()
            .join("\n")
    });
    f.commit("write tasty-a with dotted keys");
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    let rev_line = toml
        .lines()
        .find(|l| l.starts_with("tasty-a.rev = "))
        .expect("dotted rev")
        .to_string();
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replacen(
            &rev_line,
            &format!("{rev_line}\ntasty-a.features = [\"x\"]"),
            1,
        ),
    );
    f.cargo(&["metadata", "--format-version", "1"]);
    f.commit("turn on a feature with a dotted key");

    f.assert_only_alpha_flagged("a dotted key was skipped");
}

/// Indentation does not hide an entry.
#[test]
fn an_indented_entry_is_still_an_entry() {
    let f = PinFixture::new();
    f.with_features();
    f.edit_tasty_a_entry(|l| format!("  {l}"));
    f.commit("indent the tasty-a entry");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "indentation alone changed the entry:\n{text}");

    f.edit_tasty_a_entry(|l| l.replacen(" }", ", features = [\"x\"] }", 1));
    f.commit("turn on a feature in the indented entry");
    f.assert_only_alpha_flagged("an indented entry was skipped");
}

/// An entry that no locked package answers to cannot be judged.
#[test]
fn a_changed_entry_without_a_locked_package_is_undecidable() {
    let f = PinFixture::new();
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replacen(
            "[workspace.dependencies]\n",
            "[workspace.dependencies]\nnot-locked = \"1\"\n",
            1,
        ),
    );
    f.commit("declare an unused dependency");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains(
            "not-locked changed in the root Cargo.toml, but no package not-locked is in Cargo.lock"
        ),
        "{text}"
    );
}

/// A changed line whose key is not read (here a quoted key) stops the check.
#[test]
fn a_changed_entry_line_that_cannot_be_read_is_undecidable() {
    let f = PinFixture::new();
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replacen(
            "[workspace.dependencies]\n",
            "[workspace.dependencies]\n\"quoted\" = \"1\"\n",
            1,
        ),
    );
    f.commit("declare a dependency with a quoted key");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("cannot read a changed line of [workspace.dependencies]"),
        "{text}"
    );
    assert!(text.contains("\"quoted\" = \"1\""), "{text}");
}

/// Puts `block` in front of [workspace.dependencies], commits, then turns on feature `x` of
/// tasty-a and commits again.
fn multi_line_string_then_feature(block: &str) -> (i32, String) {
    let f = PinFixture::new();
    f.with_features();
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replacen(
            "[workspace.dependencies]\n",
            &format!("{block}\n[workspace.dependencies]\n"),
            1,
        ),
    );
    f.cargo(&["metadata", "--format-version", "1"]);
    f.commit("add a lint with a multi-line string");
    f.edit_tasty_a_entry(|l| l.replacen(" }", ", features = [\"x\"] }", 1));
    f.commit("turn on a feature of tasty-a");
    f.check(&HEAD_RANGE)
}

/// A multi-line string with a bracket in a skipped section must not swallow the
/// [workspace.dependencies] change after it.
#[test]
fn a_multi_line_string_in_an_inline_lint_table_is_undecidable() {
    let (code, text) = multi_line_string_then_feature(
        "[workspace.lints.rust]\n\
         dead_code = { level = \"deny\", priority = 0, note = \"\"\"\nkeep { open\n\"\"\" }\n",
    );
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("cannot read the root Cargo.toml"), "{text}");
    assert!(text.contains("a multi-line string"), "{text}");
}

#[test]
fn a_multi_line_string_in_a_lint_table_is_undecidable() {
    let (code, text) = multi_line_string_then_feature(
        "[workspace.lints.rust.dead_code]\nlevel = \"deny\"\nreason = \"\"\"\n[\n\"\"\"\n",
    );
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("cannot read the root Cargo.toml"), "{text}");
}

/// Build settings case 24: the release profile dist inherits release, so a change of
/// [profile.release] changes every plugin binary.
#[test]
fn a_release_profile_change_flags_every_plugin() {
    let f = PinFixture::new();
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.commit("change the release profile");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.release] (release profile chain: dist → release)",
    );
}

/// Case 12: The resolver changed; that can change feature resolution, so judge by hand.
#[test]
fn a_workspace_resolver_change_is_undecidable() {
    let f = PinFixture::new();
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replace("resolver = \"2\"", "resolver = \"3\""),
    );
    f.commit("move to resolver 3");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("[workspace] changed in the root Cargo.toml"),
        "{text}"
    );
}

/// Build settings case 27: the cargo config and the toolchain change every plugin binary.
#[test]
fn cargo_config_and_toolchain_changes_flag_every_plugin() {
    let f = PinFixture::new();
    f.write(
        ".cargo/config.toml",
        "[build]\nrustflags = [\"-C\", \"debuginfo=1\"]\n",
    );
    f.commit("add a cargo config");
    assert_every_plugin_flagged(&f.check(&HEAD_RANGE), ".cargo/config.toml (build settings)");

    f.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.96.0\"\n");
    f.commit("pin the toolchain");
    f.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.95.0\"\n");
    f.commit("move the toolchain");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "rust-toolchain.toml (build settings)",
    );
}

// ── Cargo.lock: external crates ──────────────────────────────────────────────
// A directory source stands in for crates.io, so the lock keeps the crates.io source without
// any network access. alpha and beta use `ext`; alpha also has `devx` as a dev-dependency;
// gamma uses neither.

struct ExtFixture {
    _tmp: tempfile::TempDir,
    repo: std::path::PathBuf,
    vendor: std::path::PathBuf,
    cargo_home: std::path::PathBuf,
}

impl ExtFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("plugins");
        let vendor = tmp.path().join("vendor");
        let cargo_home = tmp.path().join("cargo-home");
        write(
            &cargo_home,
            "config.toml",
            &format!(
                "[source.crates-io]\nreplace-with = \"vendored\"\n[source.vendored]\ndirectory = \"{}\"\n",
                vendor.display()
            ),
        );
        fs::create_dir_all(&repo).expect("plugins dir");
        init_repo(&repo);
        run_git(&repo, &["checkout", "--quiet", "-b", "main"]);
        write(&repo, ".gitignore", "target\n");
        write(
            &repo,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n\n\
             [workspace.dependencies]\next = \"1\"\ndevx = \"1\"\n",
        );
        for (p, deps) in [
            (
                "alpha",
                "[dependencies]\next.workspace = true\n\n[dev-dependencies]\ndevx.workspace = true\n",
            ),
            ("beta", "[dependencies]\next.workspace = true\n"),
            ("gamma", ""),
        ] {
            write(
                &repo,
                &format!("crates/tasty-plugin-{p}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"tasty-plugin-{p}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n{deps}"
                ),
            );
            write(
                &repo,
                &format!("crates/tasty-plugin-{p}/tasty-plugin.toml"),
                &format!("id = \"com.tasty.{p}\"\nversion = \"0.1.0\"\n"),
            );
            write(
                &repo,
                &format!("crates/tasty-plugin-{p}/src/main.rs"),
                "fn main() {}\n",
            );
        }
        let f = Self {
            _tmp: tmp,
            repo,
            vendor,
            cargo_home,
        };
        f.stock("ext", "1.0.0");
        f.stock("devx", "1.0.0");
        f.cargo(&["generate-lockfile", "--offline"]);
        commit_all(&f.repo, "seed");
        f
    }

    /// Makes one more version of a crate available, as a new crates.io release would.
    fn stock(&self, name: &str, v: &str) {
        let dir = self.vendor.join(format!("{name}-{v}"));
        write(
            &dir,
            "Cargo.toml",
            &format!("[package]\nname = \"{name}\"\nversion = \"{v}\"\nedition = \"2021\"\n"),
        );
        write(&dir, "src/lib.rs", "pub fn f() {}\n");
        write(
            &dir,
            ".cargo-checksum.json",
            "{\"files\":{},\"package\":null}\n",
        );
    }

    fn cargo(&self, args: &[&str]) {
        let out = git_env::command(real_cargo())
            .args(args)
            .current_dir(&self.repo)
            .env("CARGO_HOME", &self.cargo_home)
            .output()
            .expect("run cargo");
        assert!(
            out.status.success(),
            "cargo {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `cargo update -p <name>` after `v` becomes available. (--precise is not used: with
    /// --offline it picks the newest available version anyway.)
    fn update(&self, name: &str, v: &str) {
        self.stock(name, v);
        self.cargo(&["update", "--offline", "-p", name]);
        let lock = fs::read_to_string(self.repo.join("Cargo.lock")).expect("read lock");
        assert!(
            lock.contains(&format!("name = \"{name}\"\nversion = \"{v}\"\n")),
            "{name} is not at {v}:\n{lock}"
        );
    }

    fn bump(&self, plugin: &str, v: &str) {
        for rel in [
            format!("crates/tasty-plugin-{plugin}/Cargo.toml"),
            format!("crates/tasty-plugin-{plugin}/tasty-plugin.toml"),
        ] {
            let body = fs::read_to_string(self.repo.join(&rel)).expect("read");
            write(
                &self.repo,
                &rel,
                &body.replacen("version = \"0.1.0\"", &format!("version = \"{v}\""), 1),
            );
        }
        self.cargo(&["metadata", "--offline", "--format-version", "1"]);
    }

    fn check(&self, args: &[&str]) -> (i32, String) {
        let mut cmd = gate_command(&script(), &self.repo);
        cmd.env("CARGO_HOME", &self.cargo_home);
        run(cmd, args)
    }
}

/// Case 10: An external crate update in Cargo.lock flags the plugins whose normal/build
/// closure holds it, and names the crate, both versions and the plugins.
#[test]
fn an_external_lock_update_flags_the_plugins_that_link_the_crate() {
    let f = ExtFixture::new();
    f.update("ext", "1.0.1");
    commit_all(&f.repo, "update ext");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    for p in ["alpha", "beta"] {
        assert!(
            text.contains(&format!("crates/tasty-plugin-{p}: shipped content changed")),
            "{text}"
        );
    }
    assert!(!text.contains("crates/tasty-plugin-gamma:"), "{text}");
    assert!(text.contains("ext 1.0.0 → 1.0.1 (Cargo.lock)"), "{text}");
    assert!(
        text.contains("plugins: crates/tasty-plugin-alpha crates/tasty-plugin-beta"),
        "{text}"
    );
}

/// A dev-only crate is not in the normal/build closure, so its update needs no bump.
#[test]
fn a_dev_only_lock_update_needs_no_bump() {
    let f = ExtFixture::new();
    f.update("devx", "1.0.1");
    commit_all(&f.repo, "update devx");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("nothing to judge"), "{text}");
}

/// Releases the current commit of `repo` (origin/main points at it); work goes on in branch
/// `work`.
fn release_at(repo: &Path) {
    run_git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    run_git(repo, &["checkout", "--quiet", "-b", "work"]);
}

/// Lock updates that go out in one release need one bump: a plugin already raised above
/// its version at the tip of origin/main passes a second update, and a plugin still at its
/// released version does not.
#[test]
fn a_second_lock_update_after_a_bump_since_the_release_passes() {
    let f = ExtFixture::new();
    release_at(&f.repo);
    f.update("ext", "1.0.1");
    f.bump("alpha", "0.1.1");
    commit_all(&f.repo, "update ext and bump alpha");
    f.update("ext", "1.0.2");
    commit_all(&f.repo, "update ext again");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "beta was not bumped since the release:\n{text}");
    assert!(!text.contains("crates/tasty-plugin-alpha:"), "{text}");
    assert!(
        text.contains("crates/tasty-plugin-beta: shipped content changed"),
        "{text}"
    );
    assert!(text.contains("ext 1.0.1 → 1.0.2 (Cargo.lock)"), "{text}");
    assert!(
        text.contains(
            "crates/tasty-plugin-alpha is already at 0.1.1, above 0.1.0 at origin/main ("
        ),
        "{text}"
    );
}

/// The baseline is the tip of origin/main. A local main left at an older release would let
/// a version that is already published pass as a new bump.
#[test]
fn the_release_baseline_is_origin_main_not_an_old_local_main() {
    let f = ExtFixture::new();
    let old = git_out(&f.repo, &["rev-parse", "HEAD"]);
    f.update("ext", "1.0.1");
    f.bump("alpha", "0.1.1");
    commit_all(&f.repo, "update ext and bump alpha");
    // Release that commit on origin/main; the local main stays at the older release.
    release_at(&f.repo);
    run_git(&f.repo, &["branch", "--force", "main", &old]);
    f.update("ext", "1.0.2");
    commit_all(&f.repo, "update ext after the release");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("crates/tasty-plugin-alpha: shipped content changed"),
        "0.1.1 is already published on origin/main:\n{text}"
    );
    assert!(!text.contains("above 0.1.0"), "{text}");
}

/// Tags do not matter: with origin/main at a new release whose tag the clone never fetched,
/// and only an older tag present, the published version is still the baseline.
#[test]
fn a_release_whose_tag_was_not_fetched_is_still_the_baseline() {
    let f = ExtFixture::new();
    run_git(&f.repo, &["tag", "v0.1.0"]);
    f.update("ext", "1.0.1");
    f.bump("alpha", "0.1.1");
    commit_all(&f.repo, "update ext and bump alpha");
    // Released on origin/main, but its tag (say v0.2.0) is missing here.
    release_at(&f.repo);
    f.update("ext", "1.0.2");
    commit_all(&f.repo, "update ext after the release");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("crates/tasty-plugin-alpha: shipped content changed"),
        "0.1.1 is already published on origin/main:\n{text}"
    );
    assert!(!text.contains("at v0.1.0"), "{text}");
}

/// Without origin/main or main the second update still asks every linked plugin for a bump.
#[test]
fn without_a_release_branch_every_lock_update_needs_a_bump() {
    let f = ExtFixture::new();
    run_git(&f.repo, &["checkout", "--quiet", "-b", "work"]);
    run_git(&f.repo, &["branch", "--delete", "--force", "main"]);
    f.update("ext", "1.0.1");
    f.bump("alpha", "0.1.1");
    commit_all(&f.repo, "update ext and bump alpha");
    f.update("ext", "1.0.2");
    commit_all(&f.repo, "update ext again");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("crates/tasty-plugin-alpha: shipped content changed"),
        "{text}"
    );
}

/// A workspace dependency that only a crate outside every plugin uses passes, even when its
/// package sits near the start of a Cargo.lock bigger than a pipe buffer.
#[test]
fn an_entry_used_only_outside_the_plugins_passes_in_a_large_lock() {
    let f = ExtFixture::new();
    f.stock("aaa-early", "1.0.0");
    let mut deps = String::from("[dependencies]\naaa-early.workspace = true\n");
    for i in 0..700 {
        let name = format!("pad-{i:03}");
        f.stock(&name, "1.0.0");
        deps.push_str(&format!("{name} = \"1\"\n"));
    }
    write(
        &f.repo,
        "crates/helper/Cargo.toml",
        &format!("[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n{deps}"),
    );
    write(&f.repo, "crates/helper/src/lib.rs", "");
    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replacen(
            "[workspace.dependencies]\n",
            "[workspace.dependencies]\naaa-early = \"1\"\n",
            1,
        ),
    );
    f.cargo(&["metadata", "--offline", "--format-version", "1"]);
    commit_all(&f.repo, "add a helper crate");
    let lock = fs::read_to_string(f.repo.join("Cargo.lock")).expect("read lock");
    assert!(
        lock.len() > 64 * 1024,
        "the lock is only {} bytes",
        lock.len()
    );
    let first = lock
        .find("name = \"aaa-early\"")
        .expect("aaa-early is locked");
    assert!(first < 4096, "aaa-early is at byte {first}");

    let toml = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &toml.replacen(
            "aaa-early = \"1\"",
            "aaa-early = { version = \"1\", default-features = false }",
            1,
        ),
    );
    f.cargo(&["metadata", "--offline", "--format-version", "1"]);
    commit_all(&f.repo, "turn off the default features of aaa-early");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("nothing to judge"), "{text}");
}

/// The plugins ship for several platforms, so a crate that only Windows links is in the
/// closure on any host: its lock update needs a bump of the plugin that links it.
#[test]
fn a_lock_update_of_a_platform_only_crate_flags_its_plugin() {
    let f = ExtFixture::new();
    f.stock("winx", "1.0.0");
    write(
        &f.repo,
        "crates/tasty-plugin-gamma/Cargo.toml",
        "[package]\nname = \"tasty-plugin-gamma\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [target.'cfg(windows)'.dependencies]\nwinx = \"1\"\n",
    );
    f.cargo(&["metadata", "--offline", "--format-version", "1"]);
    f.bump("gamma", "0.1.1");
    commit_all(&f.repo, "gamma links winx on Windows");
    f.update("winx", "1.0.1");
    commit_all(&f.repo, "update winx");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("crates/tasty-plugin-gamma: shipped content changed"),
        "{text}"
    );
    assert!(text.contains("winx 1.0.0 → 1.0.1 (Cargo.lock)"), "{text}");
    assert!(!text.contains("crates/tasty-plugin-alpha:"), "{text}");
}

// ── Archive inputs outside the crates ────────────────────────────────────────
// alpha and beta have archives (tasty-plugin.toml); lib has none. The repository has the
// notice set, .gitattributes, small packaging files with the markers that the list looks
// for, and a copy of this repository's scripts/lib/archive-inputs.txt: the gate reads the
// list from the trees it judges.

const ARCHIVE_LIST: &str = "scripts/lib/archive-inputs.txt";

fn archive_list() -> String {
    fs::read_to_string(support::repo_file(ARCHIVE_LIST)).expect("read the archive input list")
}

const FIXTURE_CHECK_SH: &str = r#"#!/usr/bin/env bash
# The check entry point.
#
#   scripts/check.sh build   build the plugin crates
set -euo pipefail

PLUGINS=(
  tasty-plugin-alpha
  tasty-plugin-beta
  tasty-plugin-lib
)
LIBS=(
  tasty-plugin-lib
)

run() {
  echo "+ $*" >&2
  "$@"
}

cmd_fmt() { run cargo fmt --all --check; }
cmd_test() {
  local a
  for a in "$@"; do
    echo "$a"
  done
  run cargo test "$@"
}

cmd_list_bins() {
  echo tasty-plugin-alpha
  echo tasty-plugin-beta
}

cmd_build() {
  run cargo build --locked "$@"
}

cmd_audit() { run cargo audit; }

sub="${1:-all}"
case "$sub" in
  all)
    cmd_fmt
    cmd_test
    cmd_build "$@"
    ;;
  fmt) cmd_fmt ;;
  test) cmd_test "$@" ;;
  build) cmd_build "$@" ;;
  list-bins) cmd_list_bins ;;
  audit) cmd_audit ;;
  -h | --help | help)
    sed -n '2,4p' "$0" | sed 's/^# \{0,1\}//'
    ;;
  *)
    echo "unknown subcommand: $sub" >&2
    exit 2
    ;;
esac
"#;

const FIXTURE_PACKAGE_SH: &str = r#"#!/usr/bin/env bash
# Packs the plugins.

# Copies the files.
stage() {
  cp "$exe" "$dest/"
  grep -v '^#' "$list" > "$dest/list"
}
"#;

const FIXTURE_RELEASE_YML: &str = r#"name: Release

env:
  CARGO_TERM_COLOR: always

defaults:
  run:
    shell: bash

jobs:
  create-release:
    runs-on: x
    steps:
      - run: echo create
  package:
    runs-on: x
    steps:
      - run: bash scripts/package-plugin.sh stage --all --profile dist
  publish:
    runs-on: x
    steps:
      - run: echo publish
"#;

struct ArchiveFixture {
    _tmp: tempfile::TempDir,
    repo: std::path::PathBuf,
}

impl ArchiveFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().to_path_buf();
        init_repo(&repo);
        run_git(&repo, &["checkout", "--quiet", "-b", "main"]);
        let files: Vec<(String, String)> =
            vec![
            (
                "Cargo.toml".into(),
                "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n".into(),
            ),
            ("LICENSE".into(), "MIT License\n".into()),
            (ARCHIVE_LIST.into(), archive_list()),
            (
                "THIRD_PARTY_LICENSES.md".into(),
                "# Third-party licenses\n".into(),
            ),
            ("LICENSES/a.txt".into(), "license a\n".into()),
            (".gitattributes".into(), "* text=auto eol=lf\n".into()),
            ("scripts/check.sh".into(), FIXTURE_CHECK_SH.into()),
            ("scripts/package-plugin.sh".into(), FIXTURE_PACKAGE_SH.into()),
            (
                "scripts/lib/notice-set.sh".into(),
                "# The notice set.\nNOTICE_ASSET=\"assets/NOTICE.md\"\n".into(),
            ),
            (
                "scripts/lib/plugin-build.sh".into(),
                "# Build helpers.\nto_unix_path() { printf '%s\\n' \"$1\"; }\n".into(),
            ),
            (
                ".github/workflows/release.yml".into(),
                FIXTURE_RELEASE_YML.into(),
            ),
            (
                "crates/tasty-plugin-lib/Cargo.toml".into(),
                "[package]\nname = \"tasty-plugin-lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
                    .into(),
            ),
            (
                "crates/tasty-plugin-lib/src/lib.rs".into(),
                "pub fn f() {}\n".into(),
            ),
            (
                "crates/tasty-plugin-alpha/assets/NOTICE.md".into(),
                "# Notice\n".into(),
            ),
        ];
        for (rel, body) in &files {
            write(&repo, rel, body);
        }
        for p in ["alpha", "beta"] {
            let dir = format!("crates/tasty-plugin-{p}");
            write(
                &repo,
                &format!("{dir}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"tasty-plugin-{p}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
                ),
            );
            write(
                &repo,
                &format!("{dir}/tasty-plugin.toml"),
                &format!("id = \"com.tasty.{p}\"\nversion = \"0.1.0\"\n"),
            );
            write(&repo, &format!("{dir}/src/main.rs"), "fn main() {}\n");
            write(&repo, &format!("{dir}/lang/en.toml"), "hello = \"Hello\"\n");
        }
        commit_all(&repo, "seed");
        Self { _tmp: tmp, repo }
    }

    fn write(&self, rel: &str, body: &str) {
        write(&self.repo, rel, body);
    }

    /// Replaces the one occurrence of `from` in `rel` with `to`.
    fn edit(&self, rel: &str, from: &str, to: &str) {
        let body = fs::read_to_string(self.repo.join(rel)).expect("read");
        assert_eq!(body.matches(from).count(), 1, "{from:?} in {rel}");
        self.write(rel, &body.replacen(from, to, 1));
    }

    fn commit(&self, msg: &str) {
        commit_all(&self.repo, msg);
    }

    fn git(&self, args: &[&str]) {
        run_git(&self.repo, args);
    }

    fn bump(&self, plugin: &str, v: &str) {
        for rel in [
            format!("crates/tasty-plugin-{plugin}/Cargo.toml"),
            format!("crates/tasty-plugin-{plugin}/tasty-plugin.toml"),
        ] {
            self.edit(&rel, "version = \"0.1.0\"", &format!("version = \"{v}\""));
        }
    }

    fn check(&self, args: &[&str]) -> (i32, String) {
        check(&self.repo, args)
    }
}

/// Exit 1 with both archived plugins flagged for `path`, and the library left out.
fn assert_every_archive_flagged((code, text): &(i32, String), path: &str) {
    assert_eq!(*code, 1, "{text}");
    for p in ["alpha", "beta"] {
        assert!(
            text.contains(&format!("crates/tasty-plugin-{p}: shipped content changed")),
            "{text}"
        );
    }
    assert!(!text.contains("tasty-plugin-lib"), "{text}");
    assert!(text.contains(&format!("{path} (archive input)")), "{text}");
}

fn assert_nothing_to_judge((code, text): &(i32, String)) {
    assert_eq!(*code, 0, "{text}");
    assert!(text.contains("nothing to judge"), "{text}");
}

/// Exit 1 with only alpha flagged.
fn assert_only_alpha((code, text): &(i32, String)) {
    assert_eq!(*code, 1, "{text}");
    assert!(
        text.contains("crates/tasty-plugin-alpha: shipped content changed"),
        "{text}"
    );
    assert!(!text.contains("crates/tasty-plugin-beta:"), "{text}");
}

/// Archive case 1.
#[test]
fn a_notice_change_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.edit(
        "THIRD_PARTY_LICENSES.md",
        "licenses\n",
        "licenses\n\nOne more crate.\n",
    );
    f.commit("add a notice");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "THIRD_PARTY_LICENSES.md");
}

/// Archive case 2.
#[test]
fn a_new_license_text_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.write("LICENSES/b.txt", "license b\n");
    f.commit("add a license text");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSES/");
}

/// Archive case 3: the archive keeps the file mode, so a mode change is content.
#[test]
fn a_license_text_mode_change_is_content() {
    let f = ArchiveFixture::new();
    f.git(&["update-index", "--chmod=+x", "LICENSES/a.txt"]);
    f.git(&["commit", "--quiet", "-m", "make a license text executable"]);
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSES/");
}

/// Archive case 4.
#[test]
fn a_gitattributes_change_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.edit(".gitattributes", "eol=lf", "eol=crlf");
    f.commit("check out with CRLF");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), ".gitattributes");
}

/// Archive case 5.
#[test]
fn a_comment_only_change_in_a_packaging_script_needs_no_bump() {
    let f = ArchiveFixture::new();
    f.edit(
        "scripts/package-plugin.sh",
        "# Copies the files.\n",
        "\n  # Copies the files into the stage folder.\n\n",
    );
    f.commit("reword a comment");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));
}

/// Archive case 6: a code line counts, also when it holds a # inside a string, so the
/// comment rule must not cut a line at its first #.
#[test]
fn a_code_change_in_a_packaging_script_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.edit("scripts/package-plugin.sh", "\"$dest/\"", "\"$dest/bin/\"");
    f.commit("copy into bin");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/package-plugin.sh");

    f.edit("scripts/package-plugin.sh", "'^#'", "'^#x'");
    f.commit("change the pattern after a #");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/package-plugin.sh");
}

/// Archive case 7: check.sh is compared whole, without whole-line comments and blank lines.
#[test]
fn a_check_sh_comment_or_blank_change_needs_no_bump() {
    let f = ArchiveFixture::new();
    f.edit(
        "scripts/check.sh",
        "#   scripts/check.sh build   build the plugin crates\n",
        "#   scripts/check.sh build   build the plugin crates\n\
         #   scripts/check.sh audit   audit the dependencies\n",
    );
    f.edit(
        "scripts/check.sh",
        "cmd_audit() { run cargo audit; }\n",
        "cmd_audit() { run cargo audit; }\n\n# Subcommands.\n\n",
    );
    f.commit("comment check.sh");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));

    f.edit(
        "scripts/check.sh",
        "cmd_audit() { run cargo audit; }\n",
        "cmd_audit() { run cargo audit; }\ncmd_licenses() { run cargo deny check licenses; }\n",
    );
    f.edit(
        "scripts/check.sh",
        "  audit) cmd_audit ;;\n",
        "  audit) cmd_audit ;;\n  licenses) cmd_licenses ;;\n",
    );
    f.commit("add a licenses subcommand");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");
}

/// Archive case 8.
#[test]
fn a_check_sh_build_block_change_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.edit(
        "scripts/check.sh",
        "build --locked",
        "build --locked --verbose",
    );
    f.commit("build verbosely");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");

    f.edit(
        "scripts/check.sh",
        "  tasty-plugin-beta\n  tasty-plugin-lib\n)",
        "  tasty-plugin-lib\n)",
    );
    f.commit("stop building beta");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");
}

/// Archive case 9: another job of the release workflow does not decide the archives.
#[test]
fn a_release_workflow_change_outside_the_package_job_needs_no_bump() {
    let f = ArchiveFixture::new();
    f.edit(
        ".github/workflows/release.yml",
        "echo publish",
        "echo publish twice",
    );
    f.commit("change the publish job");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));
}

/// Archive case 9, second part.
#[test]
fn a_release_workflow_package_job_change_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.edit(".github/workflows/release.yml", "stage --all", "stage");
    f.commit("stage only the bundled plugins");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), ".github/workflows/release.yml");
}

/// Archive case 10. check.sh has no regions any more: a renamed function there is a change.
#[test]
fn a_missing_region_marker_is_undecidable() {
    let f = ArchiveFixture::new();
    f.edit(".github/workflows/release.yml", "  package:\n", "  pack:\n");
    f.commit("rename the package job");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains(".github/workflows/release.yml"), "{text}");
    assert!(text.contains("cannot find the line ^  package:"), "{text}");

    let f = ArchiveFixture::new();
    f.edit("scripts/check.sh", "cmd_build() {\n", "build_all() {\n");
    f.commit("rename cmd_build");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");
}

/// Archive case 11: like a lock update, one bump per release covers the archive inputs.
#[test]
fn a_second_notice_change_after_a_bump_since_the_release_passes() {
    let f = ArchiveFixture::new();
    release_at(&f.repo);
    f.bump("alpha", "0.1.1");
    f.commit("bump alpha");
    f.write("LICENSE", "MIT License\n\nCopyright\n");
    f.commit("change the license");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "beta was not bumped since the release:\n{text}");
    assert!(!text.contains("crates/tasty-plugin-alpha:"), "{text}");
    assert!(
        text.contains("crates/tasty-plugin-beta: shipped content changed"),
        "{text}"
    );
    assert!(
        text.contains("crates/tasty-plugin-alpha is already at 0.1.1, above 0.1.0 at origin/main"),
        "{text}"
    );
}

/// Archive case 12.
#[test]
fn without_a_release_branch_a_notice_change_needs_a_bump() {
    let f = ArchiveFixture::new();
    f.git(&["checkout", "--quiet", "-b", "work"]);
    f.git(&["branch", "--delete", "--force", "main"]);
    f.bump("alpha", "0.1.1");
    f.commit("bump alpha");
    f.write("LICENSE", "MIT License\n\nCopyright\n");
    f.commit("change the license");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSE");
}

/// Archive case 13.
#[test]
fn a_lang_mode_change_is_content() {
    let f = ArchiveFixture::new();
    f.git(&[
        "update-index",
        "--chmod=+x",
        "crates/tasty-plugin-alpha/lang/en.toml",
    ]);
    f.git(&["commit", "--quiet", "-m", "make a lang file executable"]);
    assert_only_alpha(&f.check(&HEAD_RANGE));
}

/// Archive case 14: the index is listed with ls-files.
#[test]
fn staged_mode_judges_archive_inputs_too() {
    let f = ArchiveFixture::new();
    f.edit(
        "THIRD_PARTY_LICENSES.md",
        "licenses\n",
        "licenses\n\nMore.\n",
    );
    f.git(&["add", "-A"]);
    assert_every_archive_flagged(&f.check(&["--staged"]), "THIRD_PARTY_LICENSES.md");
    f.git(&["update-index", "--chmod=+x", "LICENSES/a.txt"]);
    f.git(&["reset", "--quiet", "THIRD_PARTY_LICENSES.md"]);
    assert_every_archive_flagged(&f.check(&["--staged"]), "LICENSES/");

    // Modes come from ls-files -s in the index: a mode alone is a change, and an unchanged
    // mode is not one.
    f.git(&["reset", "--quiet", "--hard"]);
    f.git(&["update-index", "--chmod=+x", "LICENSE"]);
    assert_every_archive_flagged(&f.check(&["--staged"]), "LICENSE");
    f.git(&["reset", "--quiet", "--hard"]);
    f.git(&[
        "update-index",
        "--chmod=+x",
        "crates/tasty-plugin-alpha/tasty-plugin.toml",
    ]);
    assert_only_alpha(&f.check(&["--staged"]));
    f.git(&["reset", "--quiet", "--hard"]);
    f.bump("alpha", "0.1.1");
    f.git(&["add", "-A"]);
    assert_nothing_to_judge(&f.check(&["--staged"]));
}

/// Archive case 15: a move out of LICENSES/ removes a license text, so both paths count.
#[test]
fn moving_a_license_text_out_of_licenses_is_a_notice_change() {
    let f = ArchiveFixture::new();
    fs::create_dir_all(f.repo.join("docs")).expect("docs dir");
    f.git(&["mv", "LICENSES/a.txt", "docs/a.txt"]);
    assert_every_archive_flagged(&f.check(&["--staged"]), "LICENSES/");
    f.git(&["commit", "--quiet", "-m", "move a license text"]);
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSES/");
}

/// Archive case 16: a file that turns into a symbolic link would let its target change
/// unseen, so a link among the blob inputs stops the check.
#[test]
fn a_license_text_that_becomes_a_symlink_is_a_notice_change() {
    let f = ArchiveFixture::new();
    f.write("docs/a.txt", "license a\n");
    f.commit("keep a copy in docs");
    fs::remove_file(f.repo.join("LICENSES/a.txt")).expect("remove");
    std::os::unix::fs::symlink("../docs/a.txt", f.repo.join("LICENSES/a.txt")).expect("link");
    f.commit("link the license text");
    let r = f.check(&HEAD_RANGE);
    assert_undecidable_with(
        &r,
        "the archive input LICENSES/ is or holds a symbolic link",
    );
    assert!(r.1.contains("    LICENSES/a.txt"), "{}", r.1);
}

/// A root notice file that is a symbolic link stops every run, so a later change of its
/// target alone is not missed. The index is read in staged mode.
#[test]
fn a_root_notice_symlink_is_undecidable() {
    let f = ArchiveFixture::new();
    f.write("docs/license.txt", "MIT License\n");
    fs::remove_file(f.repo.join("LICENSE")).expect("remove");
    std::os::unix::fs::symlink("docs/license.txt", f.repo.join("LICENSE")).expect("link");
    f.git(&["add", "-A"]);
    assert_undecidable_with(
        &f.check(&["--staged"]),
        "the archive input LICENSE is or holds a symbolic link",
    );
    f.commit("link LICENSE");
    f.write("docs/license.txt", "MIT License\n\nCopyright\n");
    f.commit("change the link target alone");
    assert_undecidable_with(
        &f.check(&HEAD_RANGE),
        "the archive input LICENSE is or holds",
    );
}

/// Archive case 17: every file in LICENSES/ ships, Markdown too.
#[test]
fn a_markdown_license_text_is_a_notice_change() {
    let f = ArchiveFixture::new();
    f.write("LICENSES/example.md", "# Example\n");
    f.commit("add a Markdown license text");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSES/");
    f.write("LICENSES/example.md", "# Example\n\nMore.\n");
    f.commit("change it");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSES/");
}

/// Archive case 18: the archive copies these files as they are, final newline included.
#[test]
fn a_trailing_newline_change_in_lang_is_content() {
    let f = ArchiveFixture::new();
    f.write(
        "crates/tasty-plugin-alpha/lang/en.toml",
        "hello = \"Hello\"",
    );
    f.commit("drop the final newline of a lang file");
    assert_only_alpha(&f.check(&HEAD_RANGE));

    f.write("crates/tasty-plugin-alpha/assets/NOTICE.md", "# Notice");
    f.commit("drop the final newline of the notice");
    assert_only_alpha(&f.check(&HEAD_RANGE));
}

/// Archive case 19: line 1 of a sourced library is a comment, not a #! line.
#[test]
fn a_first_line_comment_change_in_a_sourced_lib_needs_no_bump() {
    let f = ArchiveFixture::new();
    f.edit(
        "scripts/lib/notice-set.sh",
        "# The notice set.",
        "# The notice set of every archive.",
    );
    f.commit("reword the first line");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));

    f.edit(
        "scripts/lib/notice-set.sh",
        "# The notice set of every archive.",
        "#!/usr/bin/env bash",
    );
    f.commit("start with a #! line");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/lib/notice-set.sh");
}

/// Archive case 20: package-plugin.sh also runs list-bins and run().
#[test]
fn a_check_sh_list_bins_or_run_change_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    for (what, from, to) in [
        (
            "list-bins body",
            "  echo tasty-plugin-beta\n}",
            "  echo tasty-plugin-beta\n  return 0\n}",
        ),
        ("run()", "  echo \"+ $*\" >&2\n", "  echo \"> $*\" >&2\n"),
        (
            "list-bins branch",
            "  list-bins) cmd_list_bins ;;",
            "  list-bins) cmd_list_bins | sort ;;",
        ),
    ] {
        f.edit("scripts/check.sh", from, to);
        f.commit(what);
        assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");
    }
}

/// Archive case 21: the package job inherits the top-level env and defaults.
#[test]
fn a_release_workflow_top_level_env_change_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.edit(
        ".github/workflows/release.yml",
        "  CARGO_TERM_COLOR: always\n",
        "  CARGO_TERM_COLOR: always\n  RUSTFLAGS: -C debuginfo=1\n",
    );
    f.commit("add RUSTFLAGS");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), ".github/workflows/release.yml");

    f.edit(".github/workflows/release.yml", "shell: bash", "shell: sh");
    f.commit("change the default shell");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), ".github/workflows/release.yml");
}

/// Archive case 22: a die inside the region reader ends the check, not only a subshell. A
/// block that is not closed before the next block opens is not closed, so the reader does not
/// take the closing line of the next function and compare it with the first.
#[test]
fn a_missing_closing_marker_is_undecidable() {
    let f = ArchiveFixture::new();
    f.write(
        "scripts/extra.sh",
        "a() {\n  echo a\n}\n\nb() {\n  echo b\n}\n",
    );
    let list = archive_list();
    f.write(
        ARCHIVE_LIST,
        &format!("{list}region\tscripts/extra.sh\tblock\t^a\\(\\) \\{{$\t^\\}}$\n"),
    );
    f.commit("judge a block of extra.sh");
    f.edit("scripts/extra.sh", "  echo a\n}\n", "  echo a\n");
    f.commit("drop the closing brace of a");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("is not closed by ^\\}$ before line"),
        "{text}"
    );
}

/// A trailing comment on a marker or on the key line of another job does not move a region:
/// the publish job stays outside the package job, and the markers are still found.
#[test]
fn trailing_comments_on_region_markers_are_read() {
    let f = ArchiveFixture::new();
    f.edit(
        ".github/workflows/release.yml",
        "  publish:\n",
        "  publish:  # publication\n",
    );
    f.edit(
        ".github/workflows/release.yml",
        "echo publish",
        "echo publish twice",
    );
    f.commit("comment the publish job and change it");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));

    for (from, to) in [
        ("env:\n", "env: # shared settings\n"),
        ("  package:\n", "  package:   # builds the archives\n"),
    ] {
        f.edit(".github/workflows/release.yml", from, to);
        f.commit("comment a marker");
        assert_every_archive_flagged(&f.check(&HEAD_RANGE), ".github/workflows/release.yml");
    }
}

/// Archive case 23: a .gitattributes in a crate changes how its shipped files check out.
#[test]
fn a_crate_gitattributes_change_flags_its_plugin() {
    let f = ArchiveFixture::new();
    f.write(
        "crates/tasty-plugin-alpha/lang/.gitattributes",
        "*.toml eol=crlf\n",
    );
    f.commit("check out the lang files of alpha with CRLF");
    assert_only_alpha(&f.check(&HEAD_RANGE));

    f.write("crates/tasty-plugin-alpha/.gitattributes", "* -text\n");
    f.commit("check out alpha as binary");
    assert_only_alpha(&f.check(&HEAD_RANGE));
}

/// Like the commit that added the license check to check.sh: a head comment line, a wider
/// help range, a cmd_licenses function, and its branch.
fn add_the_license_check(f: &ArchiveFixture) {
    f.edit(
        "scripts/check.sh",
        "#   scripts/check.sh build   build the plugin crates\n",
        "#   scripts/check.sh build   build the plugin crates\n\
         #   scripts/check.sh licenses   cargo deny license policy\n",
    );
    f.edit("scripts/check.sh", "'2,4p'", "'2,5p'");
    f.edit(
        "scripts/check.sh",
        "cmd_audit() { run cargo audit; }\n",
        "cmd_audit() { run cargo audit; }\n# Only the license check.\n\
         cmd_licenses() { run cargo deny --locked --workspace check licenses; }\n",
    );
    f.edit(
        "scripts/check.sh",
        "  audit) cmd_audit ;;\n",
        "  audit) cmd_audit ;;\n  licenses) cmd_licenses ;;\n",
    );
    f.commit("add the license check");
}

/// Like the commit that added the hook warning to check.sh, called in a subshell.
fn add_the_hook_warning(f: &ArchiveFixture) {
    f.edit(
        "scripts/check.sh",
        "sub=\"${1:-all}\"\n",
        "# Warns when the hooks do not run.\nwarn_hooks() {\n  local hp\n  \
         hp=$(git config --path --get core.hooksPath 2>/dev/null) || hp=\"\"\n  \
         if [[ -z \"$hp\" ]]; then\n    echo \"warning: no hooks\" >&2\n  fi\n  return 0\n}\n\n\
         sub=\"${1:-all}\"\n\ncase \"$sub\" in\n  # Never stops the run.\n  \
         all | fmt | test | build) ( warn_hooks ) || true ;;\nesac\n",
    );
    f.commit("warn when the hooks do not run");
}

/// check.sh is compared whole: changes like the ones that added the license check and the
/// hook warning need a bump.
#[test]
fn check_sh_changes_like_the_license_and_hook_lanes_need_a_bump() {
    for change in [add_the_license_check, add_the_hook_warning] {
        let f = ArchiveFixture::new();
        change(&f);
        assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");
    }
}

/// Like the other archive inputs, check.sh changes need one bump per release: once both
/// plugins are above their versions at origin/main, the two changes pass with notes.
#[test]
fn check_sh_changes_like_the_license_and_hook_lanes_pass_once_bumped_since_the_release() {
    let f = ArchiveFixture::new();
    release_at(&f.repo);
    f.bump("alpha", "0.1.1");
    f.bump("beta", "0.1.1");
    f.commit("bump both plugins");
    for change in [add_the_license_check, add_the_hook_warning] {
        change(&f);
        let (code, text) = f.check(&HEAD_RANGE);
        assert_eq!(code, 0, "{text}");
        assert!(
            text.contains("archive inputs changed (scripts/check.sh)"),
            "{text}"
        );
        for p in ["alpha", "beta"] {
            assert!(
                text.contains(&format!(
                    "crates/tasty-plugin-{p} is already at 0.1.1, above 0.1.0 at origin/main"
                )),
                "{text}"
            );
        }
    }
}

/// Ways the reviews found to change the build through parts of check.sh that an earlier
/// version of the list left out. check.sh is compared whole now, so each is a change.
#[test]
fn review_bypasses_of_left_out_check_sh_parts_flag_every_archived_plugin() {
    let warn = "warn_hooks() {\n  echo \"warning: no hooks\" >&2\n}\n\nsub=\"${1:-all}\"\n\n\
                case \"$sub\" in\n  all | fmt | test | build) ( warn_hooks ) || true ;;\nesac\n";
    // (name, edits committed first, edits of the judged commit)
    let cases: [(&str, Vec<(&str, &str)>, Vec<(&str, &str)>); 7] = [
        (
            "N1 one-line help branch before build",
            vec![],
            vec![(
                "  build) cmd_build \"$@\" ;;\n",
                "  -h | --help | help) ;; build) export RUSTFLAGS=\"-C opt-level=0\"; \
                 cmd_build \"$@\" ;;\n  build) cmd_build \"$@\" ;;\n",
            )],
        ),
        (
            "N2 indented closing brace, then a group",
            vec![],
            vec![(
                "cmd_audit() { run cargo audit; }\n",
                "cmd_audit() {\n  run cargo audit --file Cargo.lock\n  }\n  \
                 { export RUSTFLAGS=\"-C opt-level=0\"\n}\n",
            )],
        ),
        (
            "N3 the hook warning writes the cargo config",
            vec![("sub=\"${1:-all}\"\n", warn)],
            vec![(
                "  echo \"warning: no hooks\" >&2\n",
                "  printf \"[build]\\nrustflags = [\\\"-Copt-level=0\\\"]\\n\" >> .cargo/config.toml\n  \
                 echo \"warning: no hooks\" >&2\n",
            )],
        ),
        (
            "N5 a compared function calls cmd_audit, then cmd_audit changes",
            vec![(
                "  run cargo build --locked \"$@\"\n",
                "  cmd_audit\n  run cargo build --locked \"$@\"\n",
            )],
            vec![(
                "cmd_audit() { run cargo audit; }\n",
                "cmd_audit() { export RUSTFLAGS=\"-C opt-level=0\"; }\n",
            )],
        ),
        (
            "R1 cmd_fmt closed by an indented brace",
            vec![],
            vec![(
                "cmd_fmt() { run cargo fmt --all --check; }\n",
                "cmd_fmt() {\n  run cargo fmt --all --check\n  }\n\
                 export RUSTFLAGS=\"-C opt-level=0\"\nfunction review_dummy {\n  :\n}\n",
            )],
        ),
        (
            "R2 one-line help branch in its own case",
            vec![],
            vec![(
                "sub=\"${1:-all}\"\n",
                "sub=\"${1:-all}\"\ncase \"$sub\" in\n  -h | --help | help) : ;; \
                 build) export RUSTFLAGS=\"-C opt-level=0\" ;;\nesac\n",
            )],
        ),
        (
            "R3 the hook warning writes LICENSE",
            vec![("sub=\"${1:-all}\"\n", warn)],
            vec![(
                "  echo \"warning: no hooks\" >&2\n",
                "  printf \"changed notice\\n\" >> LICENSE\n  echo \"warning: no hooks\" >&2\n",
            )],
        ),
    ];
    for (name, first, judged) in cases {
        let f = ArchiveFixture::new();
        if !first.is_empty() {
            for (from, to) in first {
                f.edit("scripts/check.sh", from, to);
            }
            f.commit("set up");
        }
        for (from, to) in judged {
            f.edit("scripts/check.sh", from, to);
        }
        f.commit(name);
        let r = f.check(&HEAD_RANGE);
        assert!(
            r.1.contains("scripts/check.sh (archive input)"),
            "{name}: {}",
            r.1
        );
        assert_every_archive_flagged(&r, "scripts/check.sh");
    }
}

/// What runs for check.sh build outside the named functions counts: top-level statements and
/// a change of an array outside its declaration.
#[test]
fn a_top_level_check_sh_change_flags_every_archived_plugin() {
    for (what, from, to) in [
        (
            "export RUSTFLAGS",
            "set -euo pipefail\n",
            "set -euo pipefail\nexport RUSTFLAGS=\"-C debuginfo=2\"\n",
        ),
        (
            "PLUGINS+=",
            "run() {\n",
            "PLUGINS+=(tasty-plugin-extra)\nrun() {\n",
        ),
    ] {
        let f = ArchiveFixture::new();
        f.edit("scripts/check.sh", from, to);
        f.commit(what);
        assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");
    }
}

/// A sourced file must be in the list, by a path written out, so its later changes are
/// judged. The check runs on every commit.
#[test]
fn a_source_outside_the_list_is_undecidable() {
    let f = ArchiveFixture::new();
    f.write(
        "scripts/lib/extra.sh",
        "export CARGO_PROFILE_DIST_OPT_LEVEL=0\n",
    );
    f.edit(
        "scripts/lib/notice-set.sh",
        "# The notice set.\n",
        "# The notice set.\n. scripts/lib/extra.sh\n",
    );
    f.commit("source a helper");
    let r = f.check(&HEAD_RANGE);
    assert_undecidable_with(
        &r,
        "scripts/lib/notice-set.sh line 2 sources scripts/lib/extra.sh",
    );
    assert!(r.1.contains("does not list"), "{}", r.1);
    f.write(
        "scripts/lib/extra.sh",
        "export CARGO_PROFILE_DIST_OPT_LEVEL=1\n",
    );
    f.commit("change the helper alone");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "sources scripts/lib/extra.sh");

    let f = ArchiveFixture::new();
    f.edit(
        "scripts/package-plugin.sh",
        "stage() {\n",
        "if true; then source \"$(dirname \"$0\")/lib/x.sh\"; fi\nstage() {\n",
    );
    f.commit("source by a computed path");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "whose path is not written out");

    // A listed file may be sourced; the change itself is judged as usual.
    let f = ArchiveFixture::new();
    f.edit(
        "scripts/package-plugin.sh",
        "stage() {\n",
        ". ./scripts/lib/notice-set.sh\nstage() {\n",
    );
    f.commit("source the notice set");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/package-plugin.sh");
}

/// The package job must not hand the build to a local action or workflow, whose content the
/// list does not judge. Another job may.
#[test]
fn a_local_action_in_the_package_job_is_undecidable() {
    let f = ArchiveFixture::new();
    f.edit(
        ".github/workflows/release.yml",
        "      - run: bash scripts/package-plugin.sh stage --all --profile dist\n",
        "      - run: bash scripts/package-plugin.sh stage --all --profile dist\n      \
         - uses: ./.github/actions/pack\n",
    );
    f.commit("pack with a local action");
    assert_undecidable_with(
        &f.check(&HEAD_RANGE),
        "uses a local action or workflow (./)",
    );

    let f = ArchiveFixture::new();
    f.edit(
        ".github/workflows/release.yml",
        "      - run: echo publish\n",
        "      - run: echo publish\n      - uses: './.github/actions/notify'\n",
    );
    f.commit("notify with a local action");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));
}

/// The list is read from both judged trees: a commit that drops an input from the list and
/// changes it at once is still judged, in either mode, and the work tree list does not count.
#[test]
fn dropping_an_input_from_the_list_does_not_hide_its_change() {
    let f = ArchiveFixture::new();
    f.edit(ARCHIVE_LIST, "blob\tLICENSE\n", "");
    f.write("LICENSE", "MIT License\n\nCopyright\n");
    f.git(&["add", "-A"]);
    assert_every_archive_flagged(&f.check(&["--staged"]), "LICENSE");
    f.commit("drop LICENSE from the list and change it");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSE");

    // Only LICENSE is staged; the list line is dropped in the work tree alone.
    f.git(&["reset", "--quiet", "--hard", "HEAD^"]);
    f.write("LICENSE", "MIT License\n\nCopyright\n");
    f.git(&["add", "LICENSE"]);
    f.edit(ARCHIVE_LIST, "blob\tLICENSE\n", "");
    assert_every_archive_flagged(&f.check(&["--staged"]), "LICENSE");

    // A line added in the work tree alone is not an input of the staged change.
    f.git(&["reset", "--quiet", "--hard"]);
    f.write("NOTES.txt", "notes\n");
    f.commit("add notes");
    f.write("NOTES.txt", "more notes\n");
    f.git(&["add", "NOTES.txt"]);
    f.edit(
        ARCHIVE_LIST,
        "blob\tLICENSE\n",
        "blob\tLICENSE\nblob\tNOTES.txt\n",
    );
    assert_nothing_to_judge(&f.check(&["--staged"]));
}

/// The region lines of a path that differ between the two lists count as a change
/// of that path, also when the file itself did not change.
#[test]
fn a_changed_region_line_counts_as_a_change_of_its_file() {
    let f = ArchiveFixture::new();
    let list = archive_list();
    let line = list
        .lines()
        .find(|l| l.starts_with("region\t.github/workflows/release.yml\tjob\t"))
        .expect("the package job region");
    f.edit(
        ARCHIVE_LIST,
        &format!("{line}\n"),
        &format!("{}\n", line.replace("package", "publish")),
    );
    f.commit("look at another job");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), ".github/workflows/release.yml");
}

/// The list has no way to leave part of a script out: an except line of an earlier format
/// is an unknown kind on the after side. On the before side it is dropped, so a range that
/// starts before the change is still judged.
#[test]
fn an_except_line_in_the_list_is_undecidable() {
    let f = ArchiveFixture::new();
    f.edit(
        ARCHIVE_LIST,
        "script\tscripts/check.sh\n",
        "script\tscripts/check.sh\nexcept\tscripts/check.sh\tfunction\tcmd_audit\n",
    );
    f.commit("leave cmd_audit out");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "unknown kind 'except'");

    f.edit(
        ARCHIVE_LIST,
        "except\tscripts/check.sh\tfunction\tcmd_audit\n",
        "",
    );
    f.edit(
        "scripts/check.sh",
        "run cargo audit;",
        "run cargo audit --deny warnings;",
    );
    f.commit("compare check.sh whole and change cmd_audit");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "scripts/check.sh");
}

/// A side without the list uses the list of the other side; without a list on either side
/// nothing outside the crates is an archive input.
#[test]
fn a_side_without_the_list_uses_the_other_list() {
    let f = ArchiveFixture::new();
    f.git(&["rm", "--quiet", ARCHIVE_LIST]);
    f.commit("before the list existed");
    // The region lines of the new list are on both sides, so they did not change.
    f.write(ARCHIVE_LIST, &archive_list());
    f.edit(
        ".github/workflows/release.yml",
        "echo publish",
        "echo publish twice",
    );
    f.commit("add the list and change the publish job");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));

    f.git(&["rm", "--quiet", ARCHIVE_LIST]);
    f.commit("remove the list again");
    f.write(ARCHIVE_LIST, &archive_list());
    f.write("LICENSE", "MIT License\n\nCopyright\n");
    f.commit("add the list and change LICENSE");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSE");

    f.git(&["rm", "--quiet", ARCHIVE_LIST]);
    f.write("LICENSE", "MIT License\n\nCopyright 2\n");
    f.commit("remove the list and change LICENSE");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "LICENSE");

    f.write("LICENSE", "MIT License\n\nCopyright 3\n");
    f.commit("change LICENSE without a list");
    assert_nothing_to_judge(&f.check(&HEAD_RANGE));
}

/// A .gitattributes directly in crates/ belongs to no crate but decides how the files of
/// every crate check out, so it is an archive input like the root one.
#[test]
fn a_gitattributes_directly_in_crates_flags_every_archived_plugin() {
    let f = ArchiveFixture::new();
    f.write("crates/.gitattributes", "*.toml eol=crlf\n");
    f.commit("check out every lang file with CRLF");
    assert_every_archive_flagged(&f.check(&HEAD_RANGE), "crates/.gitattributes");
    f.git(&["rm", "--quiet", "crates/.gitattributes"]);
    assert_every_archive_flagged(&f.check(&["--staged"]), "crates/.gitattributes");

    // The other files of a crate stay with their crate.
    f.git(&["reset", "--quiet", "--hard"]);
    f.write(
        "crates/tasty-plugin-alpha/lang/de.toml",
        "hello = \"Hallo\"\n",
    );
    f.commit("add a lang file");
    assert_only_alpha(&f.check(&HEAD_RANGE));
}

// ── Build settings: profiles, toolchain, cargo config ───────────────────────
// PinFixture's root Cargo.toml has [profile.release] opt-level = 3 and [profile.dist]
// inherits = "release", like this repository.

const SEED_PROFILES: &str =
    "[profile.release]\nopt-level = 3\n\n[profile.dist]\ninherits = \"release\"\n";

impl PinFixture {
    fn write(&self, rel: &str, body: &str) {
        write(&self.repo, rel, body);
    }

    /// Replaces the one occurrence of `from` in the root Cargo.toml with `to`.
    fn edit_root(&self, from: &str, to: &str) {
        let body = fs::read_to_string(self.repo.join("Cargo.toml")).expect("read");
        assert_eq!(body.matches(from).count(), 1, "{from:?} in Cargo.toml");
        self.write("Cargo.toml", &body.replacen(from, to, 1));
    }

    fn git(&self, args: &[&str]) {
        run_git(&self.repo, args);
    }
}

/// Exit 1 with alpha and beta flagged, each for `reason`.
fn assert_every_plugin_flagged((code, text): &(i32, String), reason: &str) {
    assert_eq!(*code, 1, "{text}");
    for p in ["alpha", "beta"] {
        assert!(
            text.contains(&format!("crates/tasty-plugin-{p}: shipped content changed")),
            "{text}"
        );
    }
    assert!(text.matches(reason).count() >= 2, "{reason:?}:\n{text}");
}

fn assert_undecidable_with((code, text): &(i32, String), what: &str) {
    assert_eq!(*code, 2, "{text}");
    assert!(text.contains(what), "{what:?}:\n{text}");
}

/// Build settings case 25: dev is not in the chain of dist, so it passes with a note.
#[test]
fn a_dev_profile_change_passes_with_a_note() {
    let f = PinFixture::new();
    f.edit_root(
        SEED_PROFILES,
        &format!("{SEED_PROFILES}\n[profile.dev]\nopt-level = 1\n"),
    );
    f.commit("add a dev profile");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("note: [profile.dev] changed"), "{text}");
    assert!(text.contains("not in the inheritance chain"), "{text}");
    assert!(text.contains("nothing to judge"), "{text}");
}

/// Build settings case 26: per-package settings and build-override belong to their profile.
#[test]
fn a_profile_package_override_in_the_chain_is_content() {
    let f = PinFixture::new();
    for table in [
        "[profile.release.package.\"*\"]\nopt-level = 2\n",
        "[profile.release.build-override]\nopt-level = 0\n",
    ] {
        let body = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
        f.write("Cargo.toml", &format!("{body}\n{table}"));
        f.commit("add a profile table");
        assert_every_plugin_flagged(
            &f.check(&HEAD_RANGE),
            "Cargo.toml [profile.release] (release profile chain",
        );
    }
}

/// Build settings case 28: aliases and toolchain components do not change the build.
#[test]
fn config_alias_or_toolchain_components_only_pass_with_a_note() {
    let f = PinFixture::new();
    f.write(".cargo/config.toml", "[build]\nincremental = false\n");
    f.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.96.0\"\n");
    f.commit("add a cargo config and a toolchain file");
    f.write(
        ".cargo/config.toml",
        "[build]\nincremental = false\n\n[alias]\nxt = \"test --workspace\"\n",
    );
    f.write(
        "rust-toolchain.toml",
        "[toolchain]\nchannel = \"1.96.0\"\ncomponents = [\"rustfmt\"]\nprofile = \"minimal\"\n",
    );
    f.commit("add an alias and toolchain components");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("note: .cargo/config.toml changed only in [alias]"),
        "{text}"
    );
    assert!(
        text.contains("note: rust-toolchain.toml changed only in components or profile"),
        "{text}"
    );
}

/// Build settings case 29: a chain that cannot be followed stops the check, with the chain.
#[test]
fn an_undefined_or_cyclic_release_profile_is_undecidable() {
    let cases = [
        (
            "opt-level = 3\n\n[profile.dist]\ninherits = \"release\"\n",
            "opt-level = 2\n",
            "profile dist is not defined (dist)",
        ),
        (
            "inherits = \"release\"",
            "inherits = \"nosuch\"",
            "inherits nosuch, which is not defined (dist → nosuch)",
        ),
        (
            "inherits = \"release\"\n",
            "inherits = \"loop\"\n\n[profile.loop]\ninherits = \"dist\"\n",
            "the profiles inherit in a cycle (dist → loop → dist)",
        ),
    ];
    for (from, to, what) in cases {
        let f = PinFixture::new();
        f.edit_root(from, to);
        f.commit("break the release profile");
        assert_undecidable_with(&f.check(&HEAD_RANGE), what);
    }
}

/// Build settings case 30: one bump per release, as for a lock update.
#[test]
fn a_second_toolchain_change_after_a_bump_since_the_release_passes() {
    let f = PinFixture::new();
    release_at(&f.repo);
    f.bump("alpha", "0.1.1");
    f.commit("bump alpha");
    f.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.96.0\"\n");
    f.commit("pin the toolchain");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "beta was not bumped since the release:\n{text}");
    assert!(!text.contains("crates/tasty-plugin-alpha:"), "{text}");
    assert!(
        text.contains("crates/tasty-plugin-beta: shipped content changed"),
        "{text}"
    );
    assert!(
        text.contains(
            "rust-toolchain.toml (build settings): crates/tasty-plugin-alpha is already at 0.1.1"
        ),
        "{text}"
    );
}

/// Build settings case 31: the gate, the release workflow, and the packaging script name the
/// same release profile.
#[test]
fn the_release_profile_is_the_same_in_the_gate_the_workflow_and_the_packaging_script() {
    let read = |rel: &str| fs::read_to_string(support::repo_file(rel)).expect("read");
    let gate = read("scripts/check-plugin-version-bump.sh");
    let gate_profiles: Vec<&str> = gate
        .lines()
        .filter_map(|l| l.strip_prefix("RELEASE_PROFILE="))
        .collect();
    assert_eq!(gate_profiles, ["dist"], "RELEASE_PROFILE in the gate");
    let release = read(".github/workflows/release.yml");
    let package_job = release
        .split_once("\n  package:\n")
        .expect("the package job")
        .1
        .split_once("\n  publish:\n")
        .expect("the job after package")
        .0;
    // The profile of every call, without comments, so an old value in a comment does not
    // stand in for the call.
    let profiles: Vec<&str> = package_job
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| l.split(" #").next().unwrap_or(l))
        .flat_map(|l| l.split("--profile").skip(1))
        .map(|rest| rest.split_whitespace().next().unwrap_or(""))
        .collect();
    assert!(!profiles.is_empty(), "release.yml names no --profile");
    assert!(
        profiles.iter().all(|p| *p == "dist"),
        "release.yml builds the archives with another profile: {profiles:?}"
    );
    let package = read("scripts/package-plugin.sh");
    assert_eq!(
        package
            .lines()
            .filter(|l| l.starts_with("PROFILE="))
            .collect::<Vec<_>>(),
        ["PROFILE=\"dist\""],
        "the default profile of scripts/package-plugin.sh"
    );
}

/// Build settings case 32: the cargo config wins over the manifest, so a dist defined there
/// moves the chain.
#[test]
fn a_config_profile_redirects_the_chain() {
    let f = PinFixture::new();
    f.write(".cargo/config.toml", "[profile.dist]\ninherits = \"dev\"\n");
    f.commit("build dist from dev");
    f.edit_root(
        SEED_PROFILES,
        &format!("{SEED_PROFILES}\n[profile.dev]\nopt-level = 1\n"),
    );
    f.commit("change the dev profile");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.dev] (release profile chain: dist → dev)",
    );
}

/// Build settings case 33.
#[test]
fn a_dist_defined_only_in_the_cargo_config_is_not_undecidable() {
    let f = PinFixture::new();
    f.edit_root("\n[profile.dist]\ninherits = \"release\"\n", "");
    f.write(
        ".cargo/config.toml",
        "[profile.dist]\ninherits = \"release\"\n",
    );
    f.commit("define dist in the cargo config");
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.commit("change the release profile");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.release] (release profile chain: dist → release)",
    );
}

/// Build settings case 34: an included config file is a build setting too; an include that
/// leaves the repository or is a glob cannot be followed.
#[test]
fn a_change_in_an_included_cargo_config_flags_every_plugin() {
    let f = PinFixture::new();
    f.write(".cargo/config.toml", "include = [\"extra.toml\"]\n");
    f.write(".cargo/extra.toml", "[profile.release]\ndebug = 1\n");
    f.commit("include a cargo config");
    f.write(".cargo/extra.toml", "[profile.release]\ndebug = 2\n");
    f.commit("change the included config");
    assert_every_plugin_flagged(&f.check(&HEAD_RANGE), ".cargo/extra.toml (build settings)");

    for include in [
        "include = \"../../outside.toml\"\n",
        "include = [\"*.toml\"]\n",
    ] {
        f.write(".cargo/config.toml", include);
        f.commit("include something that cannot be read");
        assert_undecidable_with(
            &f.check(&HEAD_RANGE),
            "the cargo config .cargo/config.toml includes",
        );
    }
}

/// Build settings case 35: cargo reads quoted table names and single quotes too.
#[test]
fn quoted_profile_headers_are_read() {
    let f = PinFixture::new();
    f.edit_root(
        SEED_PROFILES,
        "[profile.\"release\"]\nopt-level = 3\n\n[ \"profile\" . \"dist\" ]\ninherits = 'release'\n",
    );
    f.commit("quote the profile tables");
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.commit("change the release profile");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.release] (release profile chain: dist → release)",
    );

    let body = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    f.write(
        "Cargo.toml",
        &format!("{body}\n[profile.release.package.\"a.b\"]\nopt-level = 1\n"),
    );
    f.commit("tune one package of the release profile");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.release] (release profile chain: dist → release)",
    );
}

/// Build settings case 36: in one [profile] table, each dotted key belongs to its profile.
#[test]
fn mixed_dotted_keys_in_one_profile_table_are_judged_per_key() {
    let f = PinFixture::new();
    f.edit_root(
        SEED_PROFILES,
        "[profile]\nrelease.opt-level = 3\ndist.inherits = \"release\"\ndev.opt-level = 0\n",
    );
    f.commit("write the profiles as dotted keys");
    f.edit_root("dev.opt-level = 0", "dev.opt-level = 1");
    f.commit("change the dev key");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("note: [profile.dev] changed"), "{text}");

    f.edit_root(
        "dist.inherits = \"release\"\n",
        "dist.inherits = \"release\"\ndist.lto = true\n",
    );
    f.commit("change a dist key");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.dist] (release profile chain: dist → release)",
    );
}

/// Build settings case 37: a top-level dotted profile key is not read.
#[test]
fn a_top_level_dotted_profile_key_is_undecidable() {
    let f = PinFixture::new();
    f.edit_root(
        "[workspace]\n",
        "profile.dist.opt-level = 2\n\n[workspace]\n",
    );
    f.commit("set a profile with a top-level dotted key");
    assert_undecidable_with(
        &f.check(&HEAD_RANGE),
        "[top level] changed in the root Cargo.toml",
    );
}

/// Build settings case 38: a user profile needs inherits; cargo fails without it.
#[test]
fn a_user_profile_without_inherits_is_undecidable() {
    let f = PinFixture::new();
    f.edit_root("inherits = \"release\"", "lto = true");
    f.commit("drop the inherits of dist");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "profile dist has no inherits (dist)");
}

/// Build settings case 38: an explicit inherits on test wins over its default (dev).
#[test]
fn an_explicit_inherits_on_test_is_followed() {
    let f = PinFixture::new();
    f.edit_root(
        "inherits = \"release\"\n",
        "inherits = \"test\"\n\n[profile.test]\ninherits = \"release\"\n",
    );
    f.commit("build dist from test");
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.commit("change the release profile");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.release] (release profile chain: dist → test → release)",
    );
}

/// Build settings case 39: the chains of both sides count.
#[test]
fn an_inherits_change_widens_the_chain_on_either_side() {
    let f = PinFixture::new();
    f.edit_root("inherits = \"release\"", "inherits = \"dev\"");
    f.commit("build dist from dev");
    assert_every_plugin_flagged(
        &f.check(&HEAD_RANGE),
        "Cargo.toml [profile.dist] (release profile chain: dist → release before, dist → dev after)",
    );
}

/// Build settings case 40.
#[test]
fn staged_mode_judges_build_config_too() {
    let f = PinFixture::new();
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.git(&["add", "-A"]);
    assert_every_plugin_flagged(
        &f.check(&["--staged"]),
        "Cargo.toml [profile.release] (release profile chain: dist → release)",
    );
    f.git(&["reset", "--quiet", "--hard"]);
    f.write(
        ".cargo/config.toml",
        "[build]\nrustflags = [\"-C\", \"debuginfo=1\"]\n",
    );
    f.git(&["add", "-A"]);
    assert_every_plugin_flagged(
        &f.check(&["--staged"]),
        ".cargo/config.toml (build settings)",
    );
}

/// Build settings case 41: a side without the root manifest is not compared as empty.
#[test]
fn an_unreadable_root_manifest_side_is_undecidable() {
    let f = PinFixture::new();
    f.git(&["rm", "--quiet", "--cached", "Cargo.toml"]);
    f.git(&[
        "commit",
        "--quiet",
        "-m",
        "drop the manifest from the index",
    ]);
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.git(&["add", "Cargo.toml"]);
    f.git(&["commit", "--quiet", "-m", "bring it back changed"]);
    assert_undecidable_with(
        &f.check(&HEAD_RANGE),
        "the root Cargo.toml is missing on one side of the range",
    );

    // The manifest exists on both sides, but git show fails on one of them: a git wrapper
    // fails `git show <side>:Cargo.toml` from its <n>th call on (the after side is read once
    // before, by the path patch check).
    let real_git = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join("git"))
        .find(|g| g.is_file())
        .expect("git on PATH");
    let stub = tempfile::tempdir().expect("stub dir");
    write(
        stub.path(),
        "git",
        &format!(
            "#!/usr/bin/env bash
             if [ \"$1\" = show ] && [ \"$2\" = \"$FAIL_SHOW\" ]; then
               n=$(( $(cat \"$FAIL_COUNT\" 2>/dev/null || echo 0) + 1 )); echo \"$n\" > \"$FAIL_COUNT\"
               if [ \"$n\" -ge \"$FAIL_FROM\" ]; then echo 'fatal: injected' >&2; exit 128; fi
             fi
             exec '{}' \"$@\"
",
            real_git.display()
        ),
    );
    fs::set_permissions(
        stub.path().join("git"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let f = PinFixture::new();
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.git(&["commit", "--quiet", "-am", "change the release profile"]);
    for (side, from, what) in [
        (
            "HEAD^:Cargo.toml",
            "1",
            "cannot read the root Cargo.toml at HEAD^.",
        ),
        (
            "HEAD:Cargo.toml",
            "2",
            "cannot read the root Cargo.toml at HEAD.",
        ),
    ] {
        let count = stub.path().join(format!("count.{from}"));
        let mut cmd = f.command(false);
        cmd.env("PATH", path_with(stub.path()))
            .env("FAIL_SHOW", side)
            .env("FAIL_FROM", from)
            .env("FAIL_COUNT", &count);
        let r = run(cmd, &HEAD_RANGE);
        assert_undecidable_with(&r, what);
        assert!(count.exists(), "the wrapper did not run:\n{}", r.1);
    }
}

// ── [patch] ──────────────────────────────────────────────────────────────────
// ExtFixture with [patch.crates-io] ext taken from a git repository (patch-src) that holds
// ext 1.0.0 and devx 1.0.0, or from a path outside the repository (local-ext). alpha and
// beta link ext; gamma does not.

impl ExtFixture {
    fn side_dir(&self, name: &str) -> std::path::PathBuf {
        self.repo.parent().expect("temp dir").join(name)
    }

    /// The git repository that patches come from: its file:// url and its HEAD.
    fn patch_source(&self) -> (String, String) {
        let src = self.side_dir("patch-src");
        if !src.join(".git").exists() {
            fs::create_dir_all(&src).expect("patch source");
            init_repo(&src);
            for name in ["ext", "devx"] {
                write(
                    &src,
                    &format!("crates/{name}/Cargo.toml"),
                    &format!(
                        "[package]\nname = \"{name}\"\nversion = \"1.0.0\"\nedition = \"2021\"\n"
                    ),
                );
                write(
                    &src,
                    &format!("crates/{name}/src/lib.rs"),
                    "pub fn f() {}\n",
                );
            }
            commit_all(&src, "patched copies");
        }
        (
            format!("file://{}", src.display()),
            git_out(&src, &["rev-parse", "HEAD"]),
        )
    }

    /// Commits `body` to `rel` in the patch source and returns the new commit.
    fn patch_source_commit(&self, rel: &str, body: &str) -> String {
        self.patch_source();
        let src = self.side_dir("patch-src");
        write(&src, rel, body);
        commit_all(&src, "change the patched copy");
        git_out(&src, &["rev-parse", "HEAD"])
    }

    /// A copy of ext outside the repository, for path patches.
    fn local_ext(&self) -> String {
        let dir = self.side_dir("local-ext");
        write(
            &dir,
            "Cargo.toml",
            "[package]\nname = \"ext\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        write(&dir, "src/lib.rs", "pub fn f() {}\n");
        "../local-ext".into()
    }

    /// Sets the [patch.crates-io] table of the root Cargo.toml to `entries` (none when
    /// empty), optionally updating the lock.
    fn set_patch(&self, entries: &str, lock: bool) {
        let body = fs::read_to_string(self.repo.join("Cargo.toml")).expect("read");
        let base = body
            .split("\n[patch.crates-io]\n")
            .next()
            .expect("manifest");
        let out = if entries.is_empty() {
            format!("{}\n", base.trim_end())
        } else {
            format!("{}\n\n[patch.crates-io]\n{entries}", base.trim_end())
        };
        write(&self.repo, "Cargo.toml", &out);
        if lock {
            self.cargo(&["metadata", "--format-version", "1"]);
        }
    }

    fn git_patch(&self, rev: &str) -> String {
        let (url, _) = self.patch_source();
        format!("ext = {{ git = \"{url}\", rev = \"{rev}\" }}\n")
    }

    fn commit(&self, msg: &str) {
        commit_all(&self.repo, msg);
    }
}

/// An ExtFixture whose ext comes from the patch source at its first commit.
fn git_patched() -> (ExtFixture, String) {
    let f = ExtFixture::new();
    let (_, rev) = f.patch_source();
    f.set_patch(&f.git_patch(&rev), true);
    f.commit("patch ext from git");
    (f, rev)
}

/// The reason lines under the violation of <plugin>.
fn reasons_of(text: &str, plugin: &str) -> Vec<String> {
    let head = format!("crates/tasty-plugin-{plugin}: shipped content changed");
    let block = text.split_once(&head).expect("the violation").1;
    let block = block.split_once("    fix:").expect("the fix line").0;
    block
        .lines()
        .filter(|l| l.starts_with("      "))
        .map(|l| l.trim().to_string())
        .collect()
}

fn assert_path_patch_rejected((code, text): &(i32, String), file: &str) {
    assert_eq!(*code, 2, "{text}");
    assert!(
        text.contains("is not reproducible in a clean clone"),
        "{text}"
    );
    assert!(
        text.contains(&format!("{file}: patch.crates-io.ext")),
        "{text}"
    );
    assert!(text.contains("scripts/dev-bundle.sh"), "{text}");
}

/// [patch] case 42.
#[test]
fn a_git_to_path_patch_is_rejected() {
    let (f, _) = git_patched();
    let path = f.local_ext();
    f.set_patch(&format!("ext = {{ path = \"{path}\" }}\n"), true);
    f.commit("patch ext from a local copy");
    assert_path_patch_rejected(&f.check(&HEAD_RANGE), "Cargo.toml");
}

/// [patch] case 43: the repository cargo config and what it includes are read too.
#[test]
fn a_path_patch_or_paths_in_the_cargo_config_is_rejected() {
    let f = ExtFixture::new();
    f.write_config(
        ".cargo/config.toml",
        "[patch.crates-io]\next = { path = \"../local-ext\" }\n",
    );
    assert_path_patch_rejected(&f.check(&HEAD_RANGE), ".cargo/config.toml");

    f.write_config(".cargo/config.toml", "paths = [\"../x\"]\n");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains(".cargo/config.toml: paths = [\"../x\"]"),
        "{text}"
    );

    f.write_config(
        ".cargo/config.toml",
        "include = [{ path = \"extra.toml\", optional = true }]\n",
    );
    f.write_config(
        ".cargo/extra.toml",
        "[patch.crates-io.ext]\npath = \"../local-ext\"\n",
    );
    assert_path_patch_rejected(&f.check(&HEAD_RANGE), ".cargo/extra.toml");

    // Two files that include each other are each read once.
    f.write_config(
        ".cargo/extra.toml",
        "include = \"config.toml\"\n[build]\njobs = 2\n",
    );
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains(".cargo/extra.toml (build settings)"),
        "{text}"
    );
}

impl ExtFixture {
    fn write_config(&self, rel: &str, body: &str) {
        write(&self.repo, rel, body);
        self.commit("change the cargo config");
    }
}

/// [patch] case 44: a path patch committed earlier blocks a commit that does not touch it.
#[test]
fn an_existing_path_patch_blocks_an_unrelated_commit() {
    let f = ExtFixture::new();
    let path = f.local_ext();
    f.set_patch(&format!("ext = {{ path = \"{path}\" }}\n"), true);
    f.commit("patch ext from a local copy, without the hooks");
    write(
        &f.repo,
        "crates/tasty-plugin-alpha/src/main.rs",
        "fn main() {\n    println!(\"x\");\n}\n",
    );
    f.commit("change alpha");
    assert_path_patch_rejected(&f.check(&HEAD_RANGE), "Cargo.toml");
}

/// [patch] case 45: from a path back to git, the lock shows a new git source; that rule
/// alone flags the plugins that link ext, with no relief and no second reason.
#[test]
fn moving_a_path_patch_back_to_git_is_judged_as_a_new_git_source() {
    let f = ExtFixture::new();
    let path = f.local_ext();
    f.set_patch(&format!("ext = {{ path = \"{path}\" }}\n"), true);
    f.commit("patch ext from a local copy, without the hooks");
    release_at(&f.repo);
    f.bump("alpha", "0.1.1");
    f.commit("bump alpha");
    let (_, rev) = f.patch_source();
    f.set_patch(&f.git_patch(&rev), true);
    f.commit("patch ext from git again");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    for p in ["alpha", "beta"] {
        let reasons = reasons_of(&text, p);
        assert_eq!(reasons.len(), 1, "{p}: {reasons:?}\n{text}");
        assert!(reasons[0].contains("new in Cargo.lock"), "{text}");
    }
    assert!(!text.contains("crates/tasty-plugin-gamma:"), "{text}");
    assert!(
        text.contains("note: [patch] crates-io.ext changed; judged through Cargo.lock"),
        "{text}"
    );
}

/// [patch] case 46.
#[test]
fn a_patch_rev_move_is_judged_once_through_the_lock() {
    let (f, _) = git_patched();
    let rev = f.patch_source_commit("crates/ext/src/lib.rs", "pub fn f() {}\npub fn g() {}\n");
    f.set_patch(&f.git_patch(&rev), true);
    f.commit("move the patch");

    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("2 violations"), "{text}");
    for p in ["alpha", "beta"] {
        let reasons = reasons_of(&text, p);
        assert_eq!(reasons.len(), 1, "{p}: {reasons:?}\n{text}");
        assert!(
            reasons[0].contains("changed: crates/ext/src/lib.rs"),
            "{text}"
        );
    }
    assert!(
        text.contains("note: [patch] crates-io.ext changed; judged through Cargo.lock"),
        "{text}"
    );
}

/// [patch] case 47.
#[test]
fn a_patch_rev_move_without_content_change_needs_no_bump() {
    let (f, _) = git_patched();
    let rev = f.patch_source_commit("README.md", "outside the crates\n");
    f.set_patch(&f.git_patch(&rev), true);
    f.commit("move the patch");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("nothing to judge"), "{text}");
}

/// [patch] case 48: back to crates.io is a registry update, once per release.
#[test]
fn removing_a_git_patch_back_to_crates_io_follows_the_lock_rule() {
    let (f, _) = git_patched();
    f.set_patch("", true);
    f.commit("drop the patch");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("ext <none> → 1.0.0 (Cargo.lock)"), "{text}");

    let (f, rev) = git_patched();
    release_at(&f.repo);
    f.bump("alpha", "0.1.1");
    f.commit("bump alpha");
    f.set_patch("", true);
    f.commit("drop the patch");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 1, "{text}");
    assert!(
        !text.contains("crates/tasty-plugin-alpha:"),
        "{rev}\n{text}"
    );
    assert!(
        text.contains("crates/tasty-plugin-beta: shipped content changed"),
        "{text}"
    );
}

/// [patch] case 49.
#[test]
fn an_unused_or_unlocked_patch_entry() {
    // devx is locked but no plugin links it.
    let (f, rev) = git_patched();
    let (url, _) = f.patch_source();
    f.set_patch(
        &format!(
            "{}devx = {{ git = \"{url}\", rev = \"{rev}\" }}\n",
            f.git_patch(&rev)
        ),
        true,
    );
    f.commit("patch devx too");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("note: [patch] crates-io.devx changed"),
        "{text}"
    );

    // A name in no lock.
    let (f, rev) = git_patched();
    let (url, _) = f.patch_source();
    f.set_patch(
        &format!(
            "{}nothere = {{ git = \"{url}\", rev = \"{rev}\" }}\n",
            f.git_patch(&rev)
        ),
        false,
    );
    f.commit("patch a crate that is not locked");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "no package nothere is in Cargo.lock");

    // Only the spelling changed.
    let (f, rev) = git_patched();
    let (url, _) = f.patch_source();
    f.set_patch(
        &format!("ext = {{ rev = \"{rev}\", git = \"{url}\" }}\n"),
        false,
    );
    f.commit("reorder the patch keys");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("note: [patch] crates-io.ext changed, but Cargo.lock locks ext the same"),
        "{text}"
    );
}

/// [patch] case 50.
#[test]
fn a_package_rename_in_a_patch_judges_both_packages() {
    let (f, rev) = git_patched();
    let (url, _) = f.patch_source();
    f.set_patch(
        &format!("ext = {{ git = \"{url}\", rev = \"{rev}\", package = \"devx\" }}\n"),
        false,
    );
    f.commit("name another package");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    for pkg in ["ext", "devx"] {
        assert!(
            text.contains(&format!("Cargo.lock locks {pkg} the same")),
            "{text}"
        );
    }

    f.set_patch(
        &format!("ext = {{ git = \"{url}\", rev = \"{rev}\", package = \"nothere\" }}\n"),
        false,
    );
    f.commit("name a package that is not locked");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "no package nothere is in Cargo.lock");
}

/// [patch] case 51: the pin rule pairs git packages by name, so two git sources of ext would
/// be compared with each other.
#[test]
fn two_git_sources_for_one_patched_name_are_undecidable() {
    let (f, _) = git_patched();
    let other = f.side_dir("ext-two");
    fs::create_dir_all(&other).expect("second source");
    init_repo(&other);
    write(
        &other,
        "Cargo.toml",
        "[package]\nname = \"ext\"\nversion = \"2.0.0\"\nedition = \"2021\"\n",
    );
    write(&other, "src/lib.rs", "pub fn two() {}\n");
    commit_all(&other, "ext 2");
    let rev_b = git_out(&other, &["rev-parse", "HEAD"]);
    let body = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &body.replacen(
            "devx = \"1\"\n",
            &format!(
                "devx = \"1\"\nextb = {{ git = \"file://{}\", rev = \"{rev_b}\", package = \"ext\" }}\n",
                other.display()
            ),
            1,
        ),
    );
    write(
        &f.repo,
        "crates/tasty-plugin-beta/Cargo.toml",
        "[package]\nname = \"tasty-plugin-beta\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\next.workspace = true\nextb.workspace = true\n",
    );
    f.cargo(&["metadata", "--format-version", "1"]);
    f.commit("beta links ext 2 from a second source");
    let lock = fs::read_to_string(f.repo.join("Cargo.lock")).expect("lock");
    assert_eq!(lock.matches("name = \"ext\"").count(), 2, "{lock}");

    let rev = f.patch_source_commit("crates/ext/src/lib.rs", "pub fn f() {}\npub fn g() {}\n");
    f.set_patch(&f.git_patch(&rev), true);
    f.commit("move the patch of ext 1");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "has ext from 2 git sources");
}

/// [patch] case 52.
#[test]
fn unsupported_patch_table_spellings_are_undecidable() {
    for (from, to) in [
        ("\n[patch.crates-io]\n", "\n[\"patch\".\"crates-io\"]\n"),
        ("\n[patch.crates-io]\next = ", "\n[patch]\ncrates-io.ext = "),
    ] {
        let (f, _) = git_patched();
        let body = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
        assert_eq!(body.matches(from).count(), 1, "{from:?}");
        write(&f.repo, "Cargo.toml", &body.replacen(from, to, 1));
        f.commit("spell the patch table another way");
        assert_undecidable_with(&f.check(&HEAD_RANGE), "cannot read a [patch] table");
    }

    let (f, rev) = git_patched();
    let (url, _) = f.patch_source();
    let body = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &format!(
            "{body}\n[patch.\"https://example.invalid/x\"]\next = {{ git = \"{url}\", rev = \"{rev}\" }}\n"
        ),
    );
    f.commit("patch a git source");
    let (code, text) = f.check(&HEAD_RANGE);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("note: [patch] https://example.invalid/x.ext changed"),
        "{text}"
    );
}

/// A quoted key of an inline table is the same key as a bare one, so { "path" = .. } and
/// { 'path' = .. } are path patches too, in the root Cargo.toml and in the cargo config. A
/// key with a backslash escape is not decoded and stops the check.
#[test]
fn quoted_inline_table_keys_read_as_bare_keys() {
    for key in ["\"path\"", "'path'"] {
        let (f, _) = git_patched();
        let path = f.local_ext();
        f.set_patch(&format!("ext = {{ {key} = \"{path}\" }}\n"), false);
        f.commit("patch ext from a local copy");
        assert_path_patch_rejected(&f.check(&HEAD_RANGE), "Cargo.toml");
    }

    let f = ExtFixture::new();
    f.write_config(
        ".cargo/config.toml",
        "[patch.crates-io]\next = { \"path\" = \"../local-ext\" }\n",
    );
    assert_path_patch_rejected(&f.check(&HEAD_RANGE), ".cargo/config.toml");
    f.write_config(
        ".cargo/config.toml",
        "include = [{ \"path\" = \"extra.toml\", 'optional' = true }]\n",
    );
    f.write_config(
        ".cargo/extra.toml",
        "[patch.crates-io]\next = { 'path' = \"../local-ext\" }\n",
    );
    assert_path_patch_rejected(&f.check(&HEAD_RANGE), ".cargo/extra.toml");

    let (f, _) = git_patched();
    f.set_patch("ext = { \"pa\\u0074h\" = \"../local-ext\" }\n", false);
    f.commit("spell path with an escape");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "cannot read a [patch] table");
}

/// TOML escapes in table names and keys are not decoded, so a name that uses one stops the
/// check with the file and the line, on every run, instead of reading as another name.
#[test]
fn backslash_escapes_in_toml_names_are_undecidable() {
    let f = PinFixture::new();
    f.edit_root("[profile.release]", "[profile.\"rele\\u0061se\"]");
    f.commit("spell release with an escape");
    let r = f.check(&HEAD_RANGE);
    assert_undecidable_with(&r, "cannot read the root Cargo.toml");
    assert!(
        r.1.contains("a backslash escape in a table name: [profile.\"rele\\u0061se\"]"),
        "{}",
        r.1
    );
    f.edit_root("opt-level = 3", "opt-level = 2");
    f.commit("change the escaped profile");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "a backslash escape in a table name");

    let (f, _) = git_patched();
    let body = fs::read_to_string(f.repo.join("Cargo.toml")).expect("read");
    write(
        &f.repo,
        "Cargo.toml",
        &body.replacen("[patch.crates-io]", "[patch.\"crates\\u002dio\"]", 1),
    );
    f.commit("spell crates-io with an escape");
    assert_undecidable_with(&f.check(&HEAD_RANGE), "a backslash escape in a table name");

    let f = ExtFixture::new();
    f.write_config(".cargo/config.toml", "[build]\n\"job\\u0073\" = 2\n");
    let r = f.check(&HEAD_RANGE);
    assert_undecidable_with(&r, ".cargo/config.toml");
    assert!(
        r.1.contains("a backslash escape in a key: \"job\\u0073\" = 2"),
        "{}",
        r.1
    );
}

/// [patch] case 53.
#[test]
fn staged_mode_rejects_a_path_patch_too() {
    let (f, _) = git_patched();
    let path = f.local_ext();
    f.set_patch(&format!("ext = {{ path = \"{path}\" }}\n"), true);
    run_git(&f.repo, &["add", "-A"]);
    assert_path_patch_rejected(&f.check(&["--staged"]), "Cargo.toml");
}
