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
                 [profile.release]\nopt-level = 3\n"
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

/// 1. A pin move that changes only a crate in alpha's closure.
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

/// 2. The same move with alpha's patch version raised passes.
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

/// 3. Only the #[cfg(test)] part changed; needs the real judge.
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

/// 4. Only the formatting changed.
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

/// 5. A file inside the crate but outside src/ (read with include_str!) is content.
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

/// 6. A listed input at the tasty root (lang/) reaches only the plugins that link its crate.
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

/// 7. An unlisted path outside a crate stops the check.
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

/// 8. The old commit cannot be found anywhere (no TASTY_GIT_DIR, not in the cargo database,
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

/// 15. A lower version together with a pin move is undecidable, not a pass.
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
