//! The git hooks in .githooks stay what docs/branching.md describes: the pre-commit run list
//! matches its check functions, and scripts/dev-setup.sh points core.hooksPath at .githooks.
//! The core.hooksPath of a developer's clone is not checked.
//!
//! Ported in part from tasty crates/tasty-doc-guards/tests/githooks_are_pinned.rs.

#![cfg(unix)]

mod support;

use std::collections::BTreeSet;

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
