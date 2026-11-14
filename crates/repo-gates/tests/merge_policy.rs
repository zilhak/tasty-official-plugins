//! Merge commits are allowed only for back merges and promotions between develop, release
//! and main. tests/fixtures/merge-policy.sh checks the decision table and real `git merge`
//! runs with the hooks installed in a throwaway repository.
//!
//! Ported from tasty crates/tasty-doc-guards/tests/githooks_are_pinned.rs
//! (merge_commits_are_allowed_only_for_back_merges_and_promotions).

#![cfg(unix)]

mod support;

#[test]
fn merge_commits_are_allowed_only_for_back_merges_and_promotions() {
    let scratch = tempfile::tempdir().expect("temp dir");
    let output = support::git_env::command("bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/merge-policy.sh"
        ))
        .arg(support::repo_root())
        .arg(scratch.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run the throwaway merge repository fixture");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
