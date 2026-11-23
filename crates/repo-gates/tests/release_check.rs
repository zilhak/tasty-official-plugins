//! scripts/release-check.sh holds the release checks that release.yml and
//! scripts/release-promote.sh share: the SemVer form of a tag, and the marker block at the
//! end of the Release notes. These tests run the script on notes and release-meta.toml
//! files in a temporary folder, and check that release.yml calls it where a tag release
//! settles and checks its notes.

#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};

use support::gate_env::GateRun;

const COMMIT: &str = "1111111111111111111111111111111111111111";
const OTHER_COMMIT: &str = "2222222222222222222222222222222222222222";
const TASTY_REV: &str = "3333333333333333333333333333333333333333";
const OTHER_REV: &str = "4444444444444444444444444444444444444444";

fn pending(rev: &str) -> String {
    format!("Requires Tasty: pending (a Tasty release that includes tasty commit {rev})")
}

/// Free text above the block, with marker-like lines that must never be read.
const BODY: &str = "Changes since v0.1.0:\n- fixes\nCommit: 9999999999999999999999999999999999999999\n\
                    \nRequires Tasty: 0.1.0 or later\ntasty rev: 9999999999999999999999999999999999999999\n";

fn run(args: &[&str]) -> GateRun {
    let out = support::git_env::command("bash")
        .arg(support::repo_file("scripts/release-check.sh"))
        .args(args)
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run scripts/release-check.sh");
    GateRun::from_output(&out)
}

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temp dir"),
        }
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, text).expect("write");
        path
    }

    fn notes(&self, block: &[String]) -> PathBuf {
        self.write("notes.md", &format!("{BODY}\n{}\n", block.join("\n")))
    }

    fn meta(&self, commit: &str, rev: &str) -> PathBuf {
        self.write(
            "release-meta.toml",
            &format!(
                "schema = 1\n\n[release]\nrepository = \"zilhak/tasty-official-plugins\"\n\
                 tag = \"v1.2.3\"\ncommit = \"{commit}\"\ntasty_rev = \"{rev}\"\n\n\
                 [[plugin]]\nid = \"com.tasty.x\"\nversion = \"0.1.0\"\nbundle = true\n"
            ),
        )
    }
}

fn p(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

fn base_block() -> Vec<String> {
    vec![
        format!("Commit: {COMMIT}"),
        format!("tasty rev: {TASTY_REV}"),
        pending(TASTY_REV),
    ]
}

#[test]
fn semver_takes_core_versions_without_leading_zeros() {
    for good in ["0.0.0", "0.1.2", "1.0.0", "10.20.30", "0.10.0"] {
        assert_eq!(run(&["semver", good]), 0, "{good}");
    }
    for bad in [
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "00.1.2",
        "1.2",
        "1.2.3.4",
        "v1.2.3",
        "1.2.3-rc.1",
        "1.2.3+b",
        "",
        " 1.2.3",
        "1.2.3 ",
        "a.b.c",
    ] {
        assert_eq!(run(&["semver", bad]), 1, "{bad:?}");
    }
}

#[test]
fn tag_must_be_v_and_a_semver_version() {
    assert_eq!(run(&["tag", "v0.1.2", "0.1.2"]), 0);
    assert_eq!(run(&["tag", "v10.0.0", "10.0.0"]), 0);
    // The old check took three groups of digits, so these passed and were published.
    assert_eq!(run(&["tag", "v01.2.3", "01.2.3"]), 1);
    assert_eq!(run(&["tag", "v1.2.03", "1.2.03"]), 1);
    assert_eq!(run(&["tag", "v1.2.4", "1.2.3"]), 1);
    assert_eq!(run(&["tag", "1.2.3", "1.2.3"]), 1);
}

#[test]
fn semver_lt_compares_numbers_not_text() {
    for (a, b) in [
        ("0.1.2", "0.1.10"),
        ("0.9.0", "0.10.0"),
        ("1.9.9", "2.0.0"),
        ("0.1.2", "0.2.0"),
        ("9.0.0", "10.0.0"),
    ] {
        assert_eq!(run(&["semver-lt", a, b]), 0, "{a} < {b}");
        assert_eq!(run(&["semver-lt", b, a]), 1, "{b} < {a}");
    }
    assert_eq!(run(&["semver-lt", "1.0.0", "1.0.0"]), 1);
    assert_eq!(run(&["semver-lt", "01.0.0", "1.0.0"]), 2);
    assert_eq!(run(&["semver-lt", "1.0.0", "v1.0.0"]), 2);
}

#[test]
fn marker_reads_only_the_last_paragraph() {
    let f = Files::new();
    let notes = f.notes(&base_block());
    let r = run(&["marker", p(&notes), "Commit"]);
    assert_eq!(r, 0);
    assert_eq!(r.output, format!("{COMMIT}\n"));
    let r = run(&["marker", p(&notes), "tasty rev"]);
    assert_eq!(r.output, format!("{TASTY_REV}\n"));

    // Two Commit lines in the block, or none, have no single value.
    let mut two = base_block();
    two.push(format!("Commit: {OTHER_COMMIT}"));
    assert_eq!(run(&["marker", p(&f.notes(&two)), "Commit"]), 1);
    let none = base_block()[1..].to_vec();
    assert_eq!(run(&["marker", p(&f.notes(&none)), "Commit"]), 1);

    // A last paragraph with free text is no marker block, even with a Commit line in it.
    let free = f.write("free.md", &format!("Commit: {COMMIT}\nSome text\n"));
    assert_eq!(run(&["marker", p(&free), "Commit"]), 1);
    let empty = f.write("empty.md", "\n\n");
    assert_eq!(run(&["marker", p(&empty), "Commit"]), 1);

    // Carriage returns and trailing blank lines from the GitHub reply do not matter.
    let crlf = f.write(
        "crlf.md",
        &format!("text\r\n\r\nCommit: {COMMIT}\r\ntasty rev: {TASTY_REV}\r\n\r\n"),
    );
    let r = run(&["marker", p(&crlf), "Commit"]);
    assert_eq!(r, 0);
    assert_eq!(r.output, format!("{COMMIT}\n"));
}

#[test]
fn prepare_settles_exactly_one_pending_line_and_keeps_the_body() {
    let f = Files::new();
    let meta = f.meta(COMMIT, TASTY_REV);
    let expected = format!("{BODY}\n{}\n", base_block().join("\n"));

    // Missing: added. A notes file from before this change has only Commit and tasty rev.
    let r = run(&["prepare", p(&f.notes(&base_block()[..2])), p(&meta)]);
    assert_eq!(r, 0);
    assert_eq!(r.output, expected);

    // Already there: unchanged.
    let r = run(&["prepare", p(&f.notes(&base_block())), p(&meta)]);
    assert_eq!(r.output, expected);

    // Identical copies: one is kept.
    let mut twice = base_block();
    twice.insert(1, pending(TASTY_REV));
    let r = run(&["prepare", p(&f.notes(&twice)), p(&meta)]);
    assert_eq!(r, 0);
    assert_eq!(r.output, expected);
}

#[test]
fn prepare_refuses_lines_it_cannot_settle() {
    let f = Files::new();
    let meta = f.meta(COMMIT, TASTY_REV);
    for extra in [
        pending(OTHER_REV),
        "Requires Tasty: 0.14.0 or later".to_string(),
        "Requires Tasty: pending".to_string(),
        "Verified with Tasty: 0.14.0 (minimum), 0.14.0 (bundled) on linux-x64".to_string(),
    ] {
        let mut block = base_block();
        block.push(extra.clone());
        let r = run(&["prepare", p(&f.notes(&block)), p(&meta)]);
        assert_eq!(r, 1, "{extra}");
    }
    // Notes without a marker block cannot be settled either.
    let free = f.write("free.md", "Only text\n");
    assert_eq!(run(&["prepare", p(&free), p(&meta)]), 1);
}

#[test]
fn verify_takes_a_block_that_matches_release_meta() {
    let f = Files::new();
    let meta = f.meta(COMMIT, TASTY_REV);
    assert_eq!(run(&["verify", p(&f.notes(&base_block())), p(&meta)]), 0);
}

#[test]
fn verify_refuses_each_marker_that_differs_from_release_meta() {
    let f = Files::new();
    let meta = f.meta(COMMIT, TASTY_REV);
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "other Commit",
            vec![
                format!("Commit: {OTHER_COMMIT}"),
                format!("tasty rev: {TASTY_REV}"),
                pending(TASTY_REV),
            ],
        ),
        ("no Commit", base_block()[1..].to_vec()),
        ("two Commit", {
            let mut b = base_block();
            b.push(format!("Commit: {COMMIT}"));
            b
        }),
        (
            "other tasty rev",
            vec![
                format!("Commit: {COMMIT}"),
                format!("tasty rev: {OTHER_REV}"),
                pending(TASTY_REV),
            ],
        ),
        (
            "no tasty rev",
            vec![format!("Commit: {COMMIT}"), pending(TASTY_REV)],
        ),
        ("two tasty rev", {
            let mut b = base_block();
            b.push(format!("tasty rev: {TASTY_REV}"));
            b
        }),
        (
            "pending pin differs",
            vec![
                format!("Commit: {COMMIT}"),
                format!("tasty rev: {TASTY_REV}"),
                pending(OTHER_REV),
            ],
        ),
        ("no pending", base_block()[..2].to_vec()),
        ("two pending", {
            let mut b = base_block();
            b.push(pending(TASTY_REV));
            b
        }),
        ("a settled Requires line", {
            let mut b = base_block();
            b.push("Requires Tasty: 0.14.0 or later".into());
            b
        }),
        ("a Verified line", {
            let mut b = base_block();
            b.push("Verified with Tasty: 0.14.0 (minimum), 0.14.0 (bundled) on x".into());
            b
        }),
    ];
    for (name, block) in cases {
        let r = run(&["verify", p(&f.notes(&block)), p(&meta)]);
        assert_eq!(r, 1, "{name}");
    }
    // The values are compared with release-meta.toml, not only counted.
    let other_meta = f.meta(OTHER_COMMIT, TASTY_REV);
    assert_eq!(
        run(&["verify", p(&f.notes(&base_block())), p(&other_meta)]),
        1
    );
    let other_meta = f.meta(COMMIT, OTHER_REV);
    assert_eq!(
        run(&["verify", p(&f.notes(&base_block())), p(&other_meta)]),
        1
    );
}

#[test]
fn pending_line_takes_only_one_tasty_commit() {
    let r = run(&["pending-line", TASTY_REV]);
    assert_eq!(r, 0);
    assert_eq!(r.output, format!("{}\n", pending(TASTY_REV)));
    // Two pins in Cargo.toml give two lines from sort -u.
    assert_eq!(
        run(&["pending-line", &format!("{TASTY_REV}\n{OTHER_REV}")]),
        1
    );
    assert_eq!(run(&["pending-line", ""]), 1);
}

/// The text of one job of release.yml, from its key to the next job key.
fn job(workflow: &str, name: &str) -> String {
    let start = workflow
        .find(&format!("\n  {name}:\n"))
        .unwrap_or_else(|| panic!("no job {name}"));
    let mut lines = workflow[start + 1..].lines();
    let mut text = format!("{}\n", lines.next().expect("job key line"));
    for line in lines {
        // The next job key, or a top-level key, ends the job.
        let next_job = line.starts_with("  ")
            && line[2..].starts_with(|c: char| c.is_ascii_alphanumeric())
            && line.trim_end().ends_with(':');
        let top = line.starts_with(|c: char| c.is_ascii_alphanumeric());
        if next_job || top {
            break;
        }
        text.push_str(line);
        text.push('\n');
    }
    text
}

#[test]
fn release_workflow_settles_and_checks_the_notes_with_the_shared_script() {
    let wf = std::fs::read_to_string(support::repo_file(".github/workflows/release.yml"))
        .expect("read release.yml");
    let create = job(&wf, "create-release");
    for needle in [
        "scripts/release-check.sh tag ",
        "scripts/release-check.sh marker ",
        "scripts/release-check.sh pending-line ",
        "gh release create \"$TAG\" --draft --prerelease ",
    ] {
        assert!(create.contains(needle), "create-release lacks {needle}");
    }
    let publish = job(&wf, "publish");
    for needle in [
        "uses: actions/checkout@",
        "scripts/release-check.sh prepare ",
        "scripts/release-check.sh verify ",
        "gh release edit \"$TAG\" --draft=false --prerelease --notes-file ",
        "scripts/release-check.sh latest",
    ] {
        assert!(publish.contains(needle), "publish lacks {needle}");
    }
    // The notes are checked before the publish request.
    let verify = publish.find("release-check.sh verify").expect("verify");
    let edit = publish.find("--draft=false").expect("publish request");
    assert!(verify < edit, "publish checks the notes after publishing");
    // Publishing without --prerelease would let GitHub make the Release Latest.
    assert_eq!(wf.matches("--draft=false").count(), 1, "{wf}");
}
