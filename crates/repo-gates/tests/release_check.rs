//! scripts/release-check.sh holds the release checks that release.yml and
//! scripts/release-promote.sh share: the SemVer form of a tag, the marker block at the
//! end of the Release notes, and the lines of a SHA256SUMS list. These tests run the
//! script on notes, release-meta.toml files, and lists in a temporary folder, and check
//! that release.yml calls it where a tag release settles and checks its notes. They also
//! run the step of the publish job that checks the release files.

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

const SUM_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SUM_B: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const LIST: &str = "SHA256SUMS-linux-x64.txt";
const CONTEXT: &str = "a context";

/// A run whose stderr is also kept on its own.
struct Split {
    run: GateRun,
    stderr: String,
}

impl Split {
    fn new(out: &std::process::Output) -> Self {
        Self {
            run: GateRun::from_output(out),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Exactly one message: one line of all the output starts as `say` starts a message,
    /// and stderr is that one line.
    fn assert_one_message(&self, what: &str) {
        let errors = self
            .run
            .output
            .lines()
            .filter(|l| l.starts_with("error: ") || l.starts_with("::error::"))
            .count();
        assert_eq!(errors, 1, "{what}: {:?}", self.run);
        assert_eq!(
            self.stderr.lines().count(),
            1,
            "{what}: stderr {:?}",
            self.stderr
        );
    }
}

fn run_sums(list: &Path) -> Split {
    let out = support::git_env::command("bash")
        .arg(support::repo_file("scripts/release-check.sh"))
        .args(["sums", p(list), CONTEXT])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run scripts/release-check.sh sums");
    Split::new(&out)
}

fn good_list() -> String {
    format!("{SUM_A}  a-0.1.0-linux-x64.tar.gz\n{SUM_B}  b-0.1.0-linux-x64.tar.gz\n")
}

/// Refused with one message that starts with the context and names the list.
fn assert_sums_refused(r: &Split, what: &str, needle: &str) {
    assert_eq!(r.run, 1, "{what}");
    r.assert_one_message(what);
    // Nothing on stdout: the caller gets no pairs from a refused list.
    assert_eq!(r.run.output, r.stderr, "{what}");
    assert!(
        r.stderr.contains(&format!("{CONTEXT}: ")) && r.stderr.contains(LIST),
        "{what}: {:?}",
        r.stderr
    );
    assert!(
        r.stderr.contains(needle),
        "{what}: no {needle:?} in {:?}",
        r.stderr
    );
}

#[test]
fn sums_prints_each_file_and_hash_with_or_without_a_final_newline() {
    let f = Files::new();
    let want = format!("a-0.1.0-linux-x64.tar.gz {SUM_A}\nb-0.1.0-linux-x64.tar.gz {SUM_B}\n");
    let r = run_sums(&f.write(LIST, &good_list()));
    assert_eq!(r.run, 0);
    assert_eq!(r.run.output, want);
    let r = run_sums(&f.write(LIST, good_list().trim_end_matches('\n')));
    assert_eq!(r.run, 0);
    assert_eq!(r.run.output, want);
}

#[test]
fn sums_refuses_every_other_line_with_one_message() {
    let f = Files::new();
    let good = good_list();
    let upper = SUM_A.to_uppercase();
    let short = &SUM_A[1..];
    let cases: Vec<(&str, String)> = vec![
        ("junk last line without a newline", format!("{good}junk")),
        (
            "blank line",
            format!("{SUM_A}  a-0.1.0-linux-x64.tar.gz\n\n{SUM_B}  b-0.1.0-linux-x64.tar.gz\n"),
        ),
        ("blank last line", format!("{good}\n")),
        ("one space", format!("{SUM_A} a-0.1.0-linux-x64.tar.gz\n")),
        ("tab", format!("{SUM_A}\ta-0.1.0-linux-x64.tar.gz\n")),
        (
            "binary mark",
            format!("{SUM_A} *a-0.1.0-linux-x64.tar.gz\n"),
        ),
        ("path", format!("{SUM_A}  ./a-0.1.0-linux-x64.tar.gz\n")),
        ("CRLF", good.replace('\n', "\r\n")),
        (
            "uppercase hex",
            format!("{upper}  a-0.1.0-linux-x64.tar.gz\n"),
        ),
        (
            "63 hex digits",
            format!("{short}  a-0.1.0-linux-x64.tar.gz\n"),
        ),
        (
            "65 hex digits",
            format!("{SUM_A}0  a-0.1.0-linux-x64.tar.gz\n"),
        ),
        (
            "BSD tag form",
            format!("SHA256 (a-0.1.0-linux-x64.tar.gz) = {SUM_A}\n"),
        ),
        (
            "trailing space",
            format!("{SUM_A}  a-0.1.0-linux-x64.tar.gz \n"),
        ),
    ];
    let runs: Vec<(&str, Split)> = cases
        .into_iter()
        .map(|(what, list)| (what, run_sums(&f.write(LIST, &list))))
        .collect();
    // Every case that was not refused, named at once.
    let taken: Vec<&str> = runs
        .iter()
        .filter(|(_, r)| r.run.code != 1)
        .map(|(what, _)| *what)
        .collect();
    assert!(taken.is_empty(), "not refused: {taken:?}");
    for (what, r) in &runs {
        assert_sums_refused(r, what, "unexpected line in ");
    }
}

#[test]
fn sums_refuses_a_list_with_a_nul_byte() {
    let f = Files::new();
    let good = good_list();
    for (what, list) in [
        (
            "NUL after an archive name",
            good.replacen(".tar.gz\n", ".tar.gz\0\n", 1),
        ),
        ("NUL alone on the last line", format!("{good}\0")),
    ] {
        let path = f.dir.path().join(LIST);
        std::fs::write(&path, list.as_bytes()).expect("write");
        let r = run_sums(&path);
        assert_sums_refused(&r, what, "NUL");
    }
}

fn running_as_root() -> bool {
    let out = std::process::Command::new("id")
        .arg("-u")
        .output()
        .expect("run id -u");
    String::from_utf8_lossy(&out.stdout).trim() == "0"
}

#[test]
fn sums_refuses_a_list_it_cannot_read_with_one_message() {
    use std::os::unix::fs::PermissionsExt;
    let f = Files::new();
    let r = run_sums(&f.dir.path().join(LIST));
    assert_sums_refused(&r, "missing list", "cannot read ");
    let dir = tempfile::tempdir().expect("temp dir");
    let list = dir.path().join(LIST);
    std::fs::create_dir(&list).expect("mkdir");
    let r = run_sums(&list);
    assert_sums_refused(&r, "directory as the list", "cannot read ");
    // A regular file without read permission. root reads it anyway, so the case is skipped.
    if running_as_root() {
        eprintln!("running as root: the unreadable list case is skipped");
        return;
    }
    let path = f.write(LIST, &good_list());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let r = run_sums(&path);
    assert_sums_refused(&r, "unreadable list", "cannot read ");
}

#[test]
fn sums_takes_a_list_and_a_context() {
    let f = Files::new();
    let list = f.write(LIST, &good_list());
    assert_eq!(run(&["sums", p(&list)]), 2);
    assert_eq!(run(&["sums", p(&list), CONTEXT, "x"]), 2);
}

const TSV: &str = "plugins-linux-x64.tsv";
const PAIRS: &str = "sums-linux-x64.txt";
const TSV_HEADER: &str = "id\tversion\tplatform\tfile\tsha256";
const SUM_C: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// The rows of three archives of linux-x64, without newlines.
fn good_rows() -> Vec<String> {
    [("a", SUM_A), ("b", SUM_B), ("c", SUM_C)]
        .iter()
        .map(|(id, sum)| format!("{id}\t0.1.0\tlinux-x64\t{id}-0.1.0-linux-x64.tar.gz\t{sum}"))
        .collect()
}

/// A TSV of the header line and `rows`, each line with a newline.
fn tsv_of(rows: &[String]) -> String {
    std::iter::once(TSV_HEADER.to_string())
        .chain(rows.iter().cloned())
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The sums pairs of the three archives of `good_rows`.
fn good_pairs() -> String {
    format!(
        "a-0.1.0-linux-x64.tar.gz {SUM_A}\nb-0.1.0-linux-x64.tar.gz {SUM_B}\n\
         c-0.1.0-linux-x64.tar.gz {SUM_C}\n"
    )
}

fn run_rows(tsv: &Path, pairs: &Path) -> Split {
    let out = support::git_env::command("bash")
        .arg(support::repo_file("scripts/release-check.sh"))
        .args(["rows", p(tsv), p(pairs), "linux-x64", CONTEXT])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run scripts/release-check.sh rows");
    Split::new(&out)
}

/// Refused with one message that starts with the context and holds `needle`.
fn assert_rows_refused(r: &Split, what: &str, needle: &str) {
    assert_eq!(r.run, 1, "{what}");
    r.assert_one_message(what);
    // Nothing on stdout: the caller gets no rows from a refused TSV.
    assert_eq!(r.run.output, r.stderr, "{what}");
    assert!(
        r.stderr.contains(&format!("{CONTEXT}: ")),
        "{what}: {:?}",
        r.stderr
    );
    assert!(
        r.stderr.contains(needle),
        "{what}: no {needle:?} in {:?}",
        r.stderr
    );
}

#[test]
fn rows_prints_the_id_and_version_of_each_row_in_its_order() {
    let f = Files::new();
    let pairs = f.write(PAIRS, &good_pairs());
    let want = "a 0.1.0\nb 0.1.0\nc 0.1.0\n";
    let r = run_rows(&f.write(TSV, &tsv_of(&good_rows())), &pairs);
    assert_eq!(r.run, 0, "final newline");
    assert_eq!(r.run.output, want, "final newline");
    let tsv = tsv_of(&good_rows());
    let r = run_rows(&f.write(TSV, tsv.trim_end_matches('\n')), &pairs);
    assert_eq!(r.run, 0, "no final newline");
    assert_eq!(r.run.output, want, "no final newline");
    let mut rows = good_rows();
    rows.reverse();
    let r = run_rows(&f.write(TSV, &tsv_of(&rows)), &pairs);
    assert_eq!(r.run, 0, "other order");
    assert_eq!(r.run.output, "c 0.1.0\nb 0.1.0\na 0.1.0\n", "other order");
}

#[test]
fn rows_refuses_a_tsv_that_does_not_list_the_pairs() {
    let f = Files::new();
    let pairs = f.write(PAIRS, &good_pairs());
    let g = good_rows();
    let with = |i: usize, row: String| {
        let mut v = g.clone();
        v[i] = row;
        tsv_of(&v)
    };
    let cases: Vec<(&str, String)> = vec![
        ("empty file", String::new()),
        ("header line only", tsv_of(&[])),
        ("a row twice, one row missing", with(1, g[0].clone())),
        ("the last row once more", {
            let mut v = g.clone();
            v.push(g[2].clone());
            tsv_of(&v)
        }),
        ("one row missing and a blank last line", {
            format!("{}\n", tsv_of(&g[..2]))
        }),
        ("a blank line between rows", {
            let mut v = g.clone();
            v.insert(1, String::new());
            tsv_of(&v)
        }),
        ("another hash", with(0, g[0].replace(SUM_A, SUM_C))),
        (
            "another platform column",
            with(0, g[0].replacen("\tlinux-x64\t", "\tlinux-arm64\t", 1)),
        ),
        ("another id column", with(0, g[0].replacen("a\t", "x\t", 1))),
        (
            "another version column",
            with(0, g[0].replacen("\t0.1.0\t", "\t0.2.0\t", 1)),
        ),
        ("CRLF", tsv_of(&g).replace('\n', "\r\n")),
        (
            "rows without the header line",
            g.iter().map(|l| format!("{l}\n")).collect(),
        ),
        (
            "junk header line",
            tsv_of(&g).replacen(TSV_HEADER, "junk", 1),
        ),
        (
            "space after the header line",
            tsv_of(&g).replacen(TSV_HEADER, &format!("{TSV_HEADER} "), 1),
        ),
        ("a sixth column", with(1, format!("{}\textra", g[1]))),
    ];
    let runs: Vec<(&str, Split)> = cases
        .into_iter()
        .map(|(what, tsv)| (what, run_rows(&f.write(TSV, &tsv), &pairs)))
        .collect();
    // Every case that was not refused, named at once.
    let taken: Vec<&str> = runs
        .iter()
        .filter(|(_, r)| r.run.code != 1)
        .map(|(what, _)| *what)
        .collect();
    assert!(taken.is_empty(), "not refused: {taken:?}");
    for (what, r) in &runs {
        assert_rows_refused(
            r,
            what,
            &format!("{TSV} does not list the archives of SHA256SUMS-linux-x64.txt"),
        );
    }
}

#[test]
fn rows_refuses_empty_or_missing_pairs() {
    let f = Files::new();
    let empty = f.write(PAIRS, "");
    // Both sides empty would agree: an empty pairs file is refused before any row.
    for (what, tsv) in [
        ("empty pairs, right TSV", tsv_of(&good_rows())),
        ("empty pairs, header line only", tsv_of(&[])),
    ] {
        let r = run_rows(&f.write(TSV, &tsv), &empty);
        assert_rows_refused(&r, what, PAIRS);
    }
    let tsv = f.write(TSV, &tsv_of(&good_rows()));
    let r = run_rows(&tsv, &f.dir.path().join("missing-pairs.txt"));
    assert_rows_refused(&r, "missing pairs", "cannot read missing-pairs.txt");
}

#[test]
fn rows_refuses_a_tsv_it_cannot_read_with_one_message() {
    use std::os::unix::fs::PermissionsExt;
    let f = Files::new();
    let pairs = f.write(PAIRS, &good_pairs());
    let path = f.dir.path().join(TSV);
    let tsv = tsv_of(&good_rows()).replacen('\n', "\0\n", 2);
    std::fs::write(&path, tsv.as_bytes()).expect("write");
    let r = run_rows(&path, &pairs);
    assert_rows_refused(
        &r,
        "NUL at the end of a row",
        &format!("{TSV} has a NUL byte"),
    );
    let dir = tempfile::tempdir().expect("temp dir");
    let r = run_rows(&dir.path().join(TSV), &pairs);
    assert_rows_refused(&r, "missing TSV", &format!("cannot read {TSV}"));
    std::fs::create_dir(dir.path().join(TSV)).expect("mkdir");
    let r = run_rows(&dir.path().join(TSV), &pairs);
    assert_rows_refused(&r, "directory as the TSV", &format!("cannot read {TSV}"));
    // root reads the file anyway, so the case is skipped.
    if running_as_root() {
        eprintln!("running as root: the unreadable TSV case is skipped");
        return;
    }
    let path = f.write(TSV, &tsv_of(&good_rows()));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let r = run_rows(&path, &pairs);
    assert_rows_refused(&r, "unreadable TSV", &format!("cannot read {TSV}"));
}

#[test]
fn rows_takes_a_tsv_pairs_a_platform_and_a_context() {
    let f = Files::new();
    let tsv = f.write(TSV, &tsv_of(&good_rows()));
    let pairs = f.write(PAIRS, &good_pairs());
    assert_eq!(run(&["rows", p(&tsv), p(&pairs), "linux-x64"]), 2);
    assert_eq!(
        run(&["rows", p(&tsv), p(&pairs), "linux-x64", CONTEXT, "x"]),
        2
    );
}

fn run_archives(dir: &Path) -> Split {
    let out = support::git_env::command("bash")
        .arg(support::repo_file("scripts/release-check.sh"))
        .args(["archives", p(dir), "linux-x64", CONTEXT])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run scripts/release-check.sh archives");
    Split::new(&out)
}

/// An artifact folder of linux-x64 with two archives, its two lists, and `extra` files.
fn artifact_folder(extra: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for name in [
        "a-0.1.0-linux-x64.tar.gz",
        "b-0.1.0-linux-x64.tar.gz",
        LIST,
        TSV,
    ]
    .iter()
    .chain(extra)
    {
        std::fs::write(dir.path().join(name), name).expect("write");
    }
    dir
}

#[test]
fn archives_prints_the_archives_of_the_platform() {
    let dir = artifact_folder(&["notes.txt", "c-0.1.0-linux-arm64.tar.gz"]);
    // A folder with an archive name is no archive.
    std::fs::create_dir(dir.path().join("d-0.1.0-linux-x64.tar.gz")).expect("mkdir");
    let r = run_archives(dir.path());
    assert_eq!(r.run, 0);
    let mut got: Vec<&str> = r.run.output.lines().collect();
    got.sort();
    assert_eq!(
        got,
        ["a-0.1.0-linux-x64.tar.gz", "b-0.1.0-linux-x64.tar.gz"],
        "{:?}",
        r.run
    );
}

#[test]
fn archives_refuses_a_name_that_a_list_line_cannot_hold_or_that_starts_with_a_dot() {
    let cases = [
        ("line break, archive name", "x\ny-linux-x64.tar.gz"),
        ("line break, other file", "notes\nx.txt"),
        ("space", "a b-0.1.0-linux-x64.tar.gz"),
        ("tab", "notes\tx.txt"),
        ("dot, archive name", ".x-0.1.0-linux-x64.tar.gz"),
        ("dot, other file", ".hidden"),
    ];
    let runs: Vec<(&str, Split)> = cases
        .iter()
        .map(|(what, name)| (*what, run_archives(artifact_folder(&[name]).path())))
        .collect();
    // Every case that was not refused, named at once.
    let taken: Vec<&str> = runs
        .iter()
        .filter(|(_, r)| r.run.code != 1)
        .map(|(what, _)| *what)
        .collect();
    assert!(taken.is_empty(), "not refused: {taken:?}");
    for (what, r) in &runs {
        r.assert_one_message(what);
        // Nothing on stdout: the caller gets no names from a refused folder.
        assert_eq!(r.run.output, r.stderr, "{what}");
        assert!(
            r.stderr.contains(&format!("{CONTEXT}: linux-x64: ")),
            "{what}: {:?}",
            r.stderr
        );
    }
}

#[test]
fn archives_takes_a_folder_a_platform_and_a_context() {
    let dir = artifact_folder(&[]);
    assert_eq!(run(&["archives", p(dir.path()), "linux-x64"]), 2);
    assert_eq!(
        run(&["archives", p(dir.path()), "linux-x64", CONTEXT, "x"]),
        2
    );
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
        "scripts/release-check.sh sums ",
        "scripts/release-check.sh rows ",
        "scripts/release-check.sh archives ",
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

const TAG: &str = "v1.2.3";
const REPO: &str = "zilhak/tasty-official-plugins";

/// The `run: |` block of the step named `step` in release.yml, without its indentation.
fn run_block(workflow: &str, step: &str) -> String {
    let mut lines = workflow.lines();
    lines
        .by_ref()
        .find(|l| l.trim() == format!("- name: {step}"))
        .unwrap_or_else(|| panic!("no step {step}"));
    let run = lines
        .by_ref()
        .find(|l| l.trim() == "run: |")
        .unwrap_or_else(|| panic!("no run block in {step}"));
    let indent = run.len() - run.trim_start().len() + 2;
    let mut script = String::new();
    for line in lines {
        if line.trim().is_empty() {
            script.push('\n');
            continue;
        }
        if line.len() - line.trim_start().len() < indent {
            break;
        }
        script.push_str(&line[indent..]);
        script.push('\n');
    }
    script
}

/// Runs a step of the publish job as GitHub runs a bash step (`bash --noprofile --norc -eo
/// pipefail`), from the repository root, with the fake gh answering `entries`.
fn run_publish_step(
    step: &str,
    entries: &[support::fake_gh::Entry],
    meta: Option<&Path>,
) -> (GateRun, support::fake_gh::Fake) {
    let wf = std::fs::read_to_string(support::repo_file(".github/workflows/release.yml"))
        .expect("read release.yml");
    let fake = support::fake_gh::Fake::new(entries, &Default::default());
    let script = fake.dir().join("step.sh");
    std::fs::write(&script, run_block(&wf, step)).expect("write step");
    let temp = fake.dir().join("runner-temp");
    std::fs::create_dir_all(&temp).expect("runner temp");
    let mut cmd = support::git_env::command("bash");
    cmd.args(["--noprofile", "--norc", "-eo", "pipefail"])
        .arg(&script)
        .current_dir(support::repo_root())
        .env("TAG", TAG)
        .env("GH_REPO", REPO)
        .env("RUNNER_TEMP", &temp);
    if let Some(meta) = meta {
        cmd.env("META", meta);
    }
    let out = fake.apply(&mut cmd).output().expect("run the step");
    (GateRun::from_output(&out), fake)
}

fn publish_entries(notes: &str) -> Vec<support::fake_gh::Entry> {
    use support::fake_gh::{entry, ok};
    vec![
        entry(
            format!("release view {TAG} --json isDraft --jq .isDraft"),
            ok("true\n"),
        ),
        entry(
            format!("release view {TAG} --json body --jq .body"),
            ok(notes.to_string()),
        ),
    ]
}

#[test]
fn the_publish_step_publishes_the_settled_notes_as_a_prerelease() {
    let f = Files::new();
    let meta = f.meta(COMMIT, TASTY_REV);
    // A draft from before the pending line: publish adds it.
    let notes = std::fs::read_to_string(f.notes(&base_block()[..2])).expect("notes");
    let (r, fake) = run_publish_step(
        "Publish the Release as a pre-release",
        &publish_entries(&notes),
        Some(&meta),
    );
    assert_eq!(r, 0);
    let edits = fake.edits();
    assert_eq!(edits.len(), 1, "{}", fake.calls());
    assert!(
        edits[0].starts_with(&format!(
            "release edit {TAG} --draft=false --prerelease --notes-file "
        )),
        "{}",
        edits[0]
    );
    let sent = fake
        .edited_notes()
        .expect("notes sent with the publish request");
    assert_eq!(sent, format!("{BODY}\n{}\n", base_block().join("\n")));
}

#[test]
fn the_publish_step_does_not_publish_notes_that_do_not_match_release_meta() {
    let f = Files::new();
    let meta = f.meta(COMMIT, TASTY_REV);
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("other Commit", {
            let mut b = base_block();
            b[0] = format!("Commit: {OTHER_COMMIT}");
            b
        }),
        ("other tasty rev", {
            let mut b = base_block();
            b[1] = format!("tasty rev: {OTHER_REV}");
            b
        }),
        ("two Commit lines", {
            let mut b = base_block();
            b.push(format!("Commit: {COMMIT}"));
            b
        }),
        ("other pending pin", {
            let mut b = base_block();
            b[2] = pending(OTHER_REV);
            b
        }),
    ];
    for (what, block) in cases {
        let notes = std::fs::read_to_string(f.notes(&block)).expect("notes");
        let (r, fake) = run_publish_step(
            "Publish the Release as a pre-release",
            &publish_entries(&notes),
            Some(&meta),
        );
        assert_ne!(r, 0, "{what}");
        assert!(
            fake.edits().is_empty(),
            "{what}: published anyway\n{}",
            fake.calls()
        );
        assert!(!r.output.contains("fake gh: no reply"), "{what}: {r:?}");
    }
}

fn state_entries(state: &str, latest: support::fake_gh::Reply) -> Vec<support::fake_gh::Entry> {
    use support::fake_gh::{entry, ok};
    vec![
        entry(
            format!(
                "release view {TAG} --json isDraft,isPrerelease --jq \"\\(.isDraft) \\(.isPrerelease)\""
            ),
            ok(format!("{state}\n")),
        ),
        entry(
            format!("api repos/{REPO}/releases/latest --jq .tag_name"),
            latest,
        ),
        entry(
            format!("api repos/{REPO} --jq .full_name"),
            ok(format!("{REPO}\n")),
        ),
    ]
}

#[test]
fn the_published_state_step_fails_when_the_tag_became_latest() {
    use support::fake_gh::{not_found, ok, server_error};
    let step = "Check the published state";
    for (what, state, latest, want_ok) in [
        ("older Latest", "false true", ok("v1.2.2\n"), true),
        ("no Latest", "false true", not_found(), true),
        (
            "the tag is Latest",
            "false true",
            ok(format!("{TAG}\n")),
            false,
        ),
        ("Latest lookup failed", "false true", server_error(), false),
        ("not a pre-release", "false false", ok("v1.2.2\n"), false),
        ("still a draft", "true true", ok("v1.2.2\n"), false),
    ] {
        let (r, fake) = run_publish_step(step, &state_entries(state, latest), None);
        assert_eq!(r.code == 0, want_ok, "{what}: {r:?}");
        assert!(!r.output.contains("fake gh: no reply"), "{what}: {r:?}");
        assert!(fake.edits().is_empty(), "{what}: {}", fake.calls());
    }
}

/// The step "Verify the release files" runs on the Linux runners of the publish job only,
/// with GNU tools (find -printf, wc without padding), so its tests are Linux only.
#[cfg(target_os = "linux")]
mod release_files {
    use super::*;

    /// The platform whose list a case changes.
    const PLATFORM: &str = "linux-x64";
    const STEP: &str = "Verify the release files";

    /// The value of a top-level env key of release.yml, such as PLATFORMS.
    fn workflow_env(workflow: &str, key: &str) -> String {
        let prefix = format!("  {key}: ");
        workflow
            .lines()
            .find_map(|l| l.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("release.yml has no env {key}"))
            .trim_matches('"')
            .to_string()
    }

    fn sha256(path: &Path) -> String {
        let out = std::process::Command::new("sha256sum")
            .arg(path)
            .output()
            .expect("run sha256sum");
        assert!(out.status.success(), "sha256sum {}", path.display());
        String::from_utf8_lossy(&out.stdout)[..64].to_string()
    }

    /// The downloaded artifacts of a tag run, as the package and release-meta jobs make
    /// them, for the platforms and plugin count of release.yml.
    struct Artifacts {
        dir: tempfile::TempDir,
        workflow: String,
        platforms: String,
        count: usize,
    }

    impl Artifacts {
        fn new() -> Self {
            let workflow =
                std::fs::read_to_string(support::repo_file(".github/workflows/release.yml"))
                    .expect("read release.yml");
            let platforms = workflow_env(&workflow, "PLATFORMS");
            let count: usize = workflow_env(&workflow, "PLUGIN_COUNT")
                .parse()
                .expect("PLUGIN_COUNT");
            assert!(platforms.split_whitespace().any(|p| p == PLATFORM));
            let a = Self {
                dir: tempfile::tempdir().expect("temp dir"),
                workflow,
                platforms,
                count,
            };
            for platform in a.platforms.split_whitespace() {
                let dir = a.artifacts().join(format!("plugins-{platform}"));
                std::fs::create_dir_all(&dir).expect("artifact dir");
                let mut list = String::new();
                let mut tsv = String::from("id\tversion\tplatform\tfile\tsha256\n");
                for i in 0..count {
                    let (id, version) = (format!("com.tasty.p{i}"), format!("0.1.{i}"));
                    let file = format!("{id}-{version}-{platform}.tar.gz");
                    let path = dir.join(&file);
                    std::fs::write(&path, format!("{file}\n")).expect("archive");
                    let hash = sha256(&path);
                    list.push_str(&format!("{hash}  {file}\n"));
                    tsv.push_str(&format!("{id}\t{version}\t{platform}\t{file}\t{hash}\n"));
                }
                std::fs::write(dir.join(format!("SHA256SUMS-{platform}.txt")), list).expect("list");
                std::fs::write(dir.join(format!("plugins-{platform}.tsv")), tsv).expect("tsv");
            }
            let meta = a.artifacts().join("release-meta");
            std::fs::create_dir_all(&meta).expect("meta dir");
            let mut text = format!(
                "schema = 1\n\n[release]\nrepository = \"{REPO}\"\ntag = \"{TAG}\"\n\
                 commit = \"{COMMIT}\"\ntasty_rev = \"{TASTY_REV}\"\n"
            );
            for i in 0..count {
                text.push_str(&format!(
                    "\n[[plugin]]\nid = \"com.tasty.p{i}\"\nversion = \"0.1.{i}\"\nbundle = true\n"
                ));
            }
            std::fs::write(meta.join("release-meta.toml"), text).expect("meta");
            a
        }

        fn artifacts(&self) -> PathBuf {
            self.dir.path().join("artifacts")
        }

        fn platform_dir(&self) -> PathBuf {
            self.artifacts().join(format!("plugins-{PLATFORM}"))
        }

        fn list_path(&self) -> PathBuf {
            self.platform_dir()
                .join(format!("SHA256SUMS-{PLATFORM}.txt"))
        }

        /// The lines of the list of PLATFORM, without their newlines.
        fn lines(&self) -> Vec<String> {
            std::fs::read_to_string(self.list_path())
                .expect("read list")
                .lines()
                .map(String::from)
                .collect()
        }

        fn set_list(&self, text: &str) {
            std::fs::write(self.list_path(), text).expect("write list");
        }

        fn tsv_path(&self) -> PathBuf {
            self.platform_dir().join(format!("plugins-{PLATFORM}.tsv"))
        }

        /// The lines of the TSV of PLATFORM (the header line first), without newlines.
        fn tsv_lines(&self) -> Vec<String> {
            std::fs::read_to_string(self.tsv_path())
                .expect("read TSV")
                .lines()
                .map(String::from)
                .collect()
        }

        fn set_tsv(&self, text: &str) {
            std::fs::write(self.tsv_path(), text).expect("write TSV");
        }

        /// The archives and hashes that the SHA256SUMS list of `platform` names.
        fn listed(&self, platform: &str) -> Vec<(String, String)> {
            let list = self
                .artifacts()
                .join(format!("plugins-{platform}/SHA256SUMS-{platform}.txt"));
            std::fs::read_to_string(list)
                .expect("read list")
                .lines()
                .map(|l| (l[66..].to_string(), l[..64].to_string()))
                .collect()
        }

        /// DIST holds exactly the verified files: per platform the archives of its
        /// SHA256SUMS list with those hashes and the two lists, and release-meta.toml.
        fn assert_dist_is_verified(&self, what: &str) {
            let mut want = vec!["release-meta.toml".to_string()];
            for platform in self.platforms.split_whitespace() {
                want.push(format!("SHA256SUMS-{platform}.txt"));
                want.push(format!("plugins-{platform}.tsv"));
                for (name, hash) in self.listed(platform) {
                    assert_eq!(sha256(&self.dist().join(&name)), hash, "{what}: {name}");
                    want.push(name);
                }
            }
            want.sort();
            let mut got: Vec<String> = std::fs::read_dir(self.dist())
                .expect("dist")
                .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                .collect();
            got.sort();
            assert_eq!(got, want, "{what}");
            let platforms = self.platforms.split_whitespace().count();
            assert_eq!(got.len(), platforms * (self.count + 2) + 1, "{what}");
        }

        fn dist(&self) -> PathBuf {
            self.dir.path().join("dist")
        }

        /// Runs the step as GitHub runs a bash step, from the repository root.
        fn run(&self) -> Split {
            self.run_on(&self.platforms)
        }

        /// Runs the step with `platforms` as PLATFORMS.
        fn run_on(&self, platforms: &str) -> Split {
            let script = self.dir.path().join("step.sh");
            std::fs::write(&script, run_block(&self.workflow, STEP)).expect("write step");
            let temp = self.dir.path().join("runner-temp");
            std::fs::create_dir_all(&temp).expect("runner temp");
            let out = support::git_env::command("bash")
                .args(["--noprofile", "--norc", "-eo", "pipefail"])
                .arg(&script)
                .current_dir(support::repo_root())
                .env("TAG", TAG)
                .env("COMMIT", COMMIT)
                .env("PLATFORMS", platforms)
                .env("PLUGIN_COUNT", self.count.to_string())
                .env("ARTIFACTS", self.artifacts())
                .env("DIST", self.dist())
                .env("RUNNER_TEMP", &temp)
                .output()
                .expect("run the step");
            Split::new(&out)
        }
    }

    fn joined(lines: &[String]) -> String {
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn the_verify_step_takes_right_lists_with_or_without_a_final_newline() {
        for (what, newline) in [("final newline", true), ("no final newline", false)] {
            let a = Artifacts::new();
            if !newline {
                let list = joined(&a.lines());
                a.set_list(list.trim_end_matches('\n'));
            }
            let r = a.run();
            assert_eq!(r.run, 0, "{what}");
            let files = std::fs::read_dir(a.dist()).expect("dist").count();
            let platforms = a.platforms.split_whitespace().count();
            assert_eq!(files, platforms * (a.count + 2) + 1, "{what}: {:?}", r.run);
        }
    }

    #[test]
    fn the_verify_step_refuses_a_list_that_promotion_refuses() {
        let probe = Artifacts::new();
        let l = probe.lines();
        let hash_of = |line: &str| line[..64].to_string();
        let file_of = |line: &str| line[66..].to_string();
        let tsv = probe.platform_dir().join(format!("plugins-{PLATFORM}.tsv"));
        let cases: Vec<(&str, String)> = vec![
            // As many lines as archives, so a count of lines alone takes it.
            ("c3 the last line replaced by junk", {
                let mut v = l[..l.len() - 1].to_vec();
                v.push("junk".into());
                joined(&v)
            }),
            ("c5 blank line", {
                let mut v = l.clone();
                v.insert(1, String::new());
                joined(&v)
            }),
            ("c6 a line twice, one archive missing", {
                let mut v = l.clone();
                v[1] = l[0].clone();
                joined(&v)
            }),
            ("c7 the TSV listed, one archive missing", {
                let mut v = l.clone();
                v[1] = format!("{}  plugins-{PLATFORM}.tsv", sha256(&tsv));
                joined(&v)
            }),
            ("c8 CRLF", joined(&l).replace('\n', "\r\n")),
            ("c9 binary mark", {
                let mut v = l.clone();
                v[0] = format!("{} *{}", hash_of(&l[0]), file_of(&l[0]));
                joined(&v)
            }),
            ("c10 path", {
                let mut v = l.clone();
                v[1] = format!("{}  ./{}", hash_of(&l[1]), file_of(&l[1]));
                joined(&v)
            }),
            ("c11 every archive and one twice", {
                let mut v = l.clone();
                v.push(l[0].clone());
                joined(&v)
            }),
            ("c12 one hash of another archive", {
                let mut v = l.clone();
                v[0] = format!("{}  {}", hash_of(&l[1]), file_of(&l[0]));
                joined(&v)
            }),
        ];
        let runs: Vec<(&str, Split)> = cases
            .into_iter()
            .map(|(what, list)| {
                let a = Artifacts::new();
                a.set_list(&list);
                (what, a.run())
            })
            .collect();
        // Every case that was not refused, named at once.
        let taken: Vec<&str> = runs
            .iter()
            .filter(|(_, r)| r.run.code != 1)
            .map(|(what, _)| *what)
            .collect();
        assert!(taken.is_empty(), "not refused: {taken:?}");
        for (what, r) in &runs {
            assert!(r.run.output.contains(PLATFORM), "{what}: {:?}", r.run);
        }
        // Only the list of the platform is missing.
        let a = Artifacts::new();
        std::fs::remove_file(a.list_path()).expect("remove list");
        let r = a.run();
        assert_eq!(r.run, 1, "missing list");
        r.assert_one_message("missing list");
        assert!(
            r.stderr.contains("publish: ")
                && r.stderr.contains(&format!("SHA256SUMS-{PLATFORM}.txt")),
            "missing list: {:?}",
            r.stderr
        );
    }

    #[test]
    fn the_verify_step_takes_right_tsv_lists() {
        let cases: Vec<(&str, Box<dyn Fn(&[String]) -> String>)> = vec![
            ("final newline", Box::new(|l: &[String]| joined(l))),
            (
                "no final newline",
                Box::new(|l: &[String]| joined(l).trim_end_matches('\n').to_string()),
            ),
            (
                "rows in another order",
                Box::new(|l: &[String]| {
                    let mut v = l.to_vec();
                    v[1..].reverse();
                    joined(&v)
                }),
            ),
        ];
        for (what, change) in cases {
            let a = Artifacts::new();
            a.set_tsv(&change(&a.tsv_lines()));
            let r = a.run();
            assert_eq!(r.run, 0, "{what}");
            a.assert_dist_is_verified(what);
        }
    }

    #[test]
    fn the_verify_step_refuses_a_tsv_that_promotion_refuses() {
        let probe = Artifacts::new();
        let l = probe.tsv_lines();
        let with = |i: usize, row: String| {
            let mut v = l.clone();
            v[i] = row;
            joined(&v)
        };
        let hash_of = |row: &str| row.rsplit('\t').next().expect("hash").to_string();
        let cases: Vec<(&str, String)> = vec![
            ("t3 empty file", String::new()),
            ("t4 header line only", joined(&l[..1])),
            ("t5 a row twice, one row missing", with(2, l[1].clone())),
            (
                "t6 one hash of another archive",
                with(1, l[1].replace(&hash_of(&l[1]), &hash_of(&l[2]))),
            ),
            ("t7 one row missing and a blank last line", {
                format!("{}\n", joined(&l[..l.len() - 1]))
            }),
            (
                "t8 another platform column",
                with(
                    1,
                    l[1].replacen(&format!("\t{PLATFORM}\t"), "\tlinux-arm64\t", 1),
                ),
            ),
            ("t9 CRLF", joined(&l).replace('\n', "\r\n")),
            ("t11 a sixth column", with(1, format!("{}\textra", l[1]))),
            ("t12 junk header line", with(0, "junk".into())),
            ("t14 NUL at the end of a row", {
                let mut v = l.clone();
                v[1].push('\0');
                joined(&v)
            }),
            ("t15 the last row once more", {
                let mut v = l.clone();
                v.push(l[l.len() - 1].clone());
                joined(&v)
            }),
            ("t16 rows only, one twice", {
                let mut v = l[1..].to_vec();
                v.push(l[1].clone());
                joined(&v)
            }),
            (
                "t17 another id column",
                with(1, l[1].replacen("com.tasty.p0\t", "com.tasty.px\t", 1)),
            ),
            ("t18 a blank line between rows", {
                let mut v = l.clone();
                v.insert(2, String::new());
                joined(&v)
            }),
            (
                "t19 another version column",
                with(1, l[1].replacen("\t0.1.0\t", "\t0.2.0\t", 1)),
            ),
            ("space after the header line", with(0, format!("{} ", l[0]))),
        ];
        let runs: Vec<(&str, Split)> = cases
            .into_iter()
            .map(|(what, tsv)| {
                let a = Artifacts::new();
                a.set_tsv(&tsv);
                (what, a.run())
            })
            .collect();
        // Every case that was not refused, named at once.
        let taken: Vec<&str> = runs
            .iter()
            .filter(|(_, r)| r.run.code != 1)
            .map(|(what, _)| *what)
            .collect();
        assert!(taken.is_empty(), "not refused: {taken:?}");
        for (what, r) in &runs {
            assert!(r.run.output.contains(PLATFORM), "{what}: {:?}", r.run);
        }
        // release-check.sh reports the TSV once, and the step adds no message.
        let (what, r) = &runs[2];
        r.assert_one_message(what);
        assert!(
            r.stderr.contains(&format!(
                "publish: plugins-{PLATFORM}.tsv does not list the archives of SHA256SUMS-{PLATFORM}.txt"
            )),
            "{what}: {:?}",
            r.stderr
        );
        // t13: only the TSV of the platform is missing.
        let a = Artifacts::new();
        std::fs::remove_file(a.tsv_path()).expect("remove TSV");
        let r = a.run();
        assert_eq!(r.run, 1, "t13 missing TSV");
        r.assert_one_message("t13 missing TSV");
        assert!(
            r.stderr
                .contains(&format!("publish: cannot read plugins-{PLATFORM}.tsv")),
            "t13 missing TSV: {:?}",
            r.stderr
        );
    }

    /// The message of release-check.sh archives for an artifact of PLATFORM.
    fn archives_message() -> String {
        format!(
            "publish: {PLATFORM}: the artifact has a file whose name a SHA256SUMS line cannot hold"
        )
    }

    /// Writes `name` into the artifact folder of PLATFORM.
    fn add_file(a: &Artifacts, name: &str, text: &str) -> PathBuf {
        let path = a.platform_dir().join(name);
        std::fs::write(&path, text).expect("write file");
        path
    }

    #[test]
    fn the_verify_step_refuses_an_artifact_file_name_that_a_list_cannot_hold() {
        let mut cases: Vec<(&str, Artifacts)> = Vec::new();
        // n1: an archive name with a line break.
        let a = Artifacts::new();
        add_file(&a, "x\ny-linux-x64.tar.gz", "x");
        cases.push(("n1 line break in an archive name", a));
        // n2: the parts of a name with a line break are files of the list too, so a
        // line count of the names takes the archives.
        let a = Artifacts::new();
        let mut lines = a.lines();
        for line in lines.drain(5..) {
            std::fs::remove_file(a.platform_dir().join(&line[66..])).expect("remove archive");
        }
        let p_txt = add_file(&a, "p.txt", "p");
        let q = add_file(&a, &format!("q-{PLATFORM}.tar.gz"), "q");
        add_file(&a, &format!("p.txt\nq-{PLATFORM}.tar.gz"), "pq");
        lines.push(format!("{}  p.txt", sha256(&p_txt)));
        lines.push(format!("{}  q-{PLATFORM}.tar.gz", sha256(&q)));
        lines.push(format!("{}  q-{PLATFORM}.tar.gz", sha256(&q)));
        a.set_list(&joined(&lines));
        cases.push(("n2 the parts of a name with a line break listed", a));
        // n3: another file with a line break.
        let a = Artifacts::new();
        add_file(&a, "notes\nx.txt", "x");
        cases.push(("n3 line break in another file name", a));
        // n4: an archive whose name starts with a dot, not listed.
        let a = Artifacts::new();
        add_file(&a, &format!(".x-0.1.0-{PLATFORM}.tar.gz"), "x");
        cases.push(("n4 an archive name with a dot first", a));
        // o2: an archive renamed with a dot first and listed so, and one more file.
        let a = Artifacts::new();
        let name = a.lines()[0][66..].to_string();
        let dotted = format!(".{name}");
        std::fs::rename(a.platform_dir().join(&name), a.platform_dir().join(&dotted))
            .expect("rename");
        a.set_list(&joined(&a.lines()).replace(&name, &dotted));
        a.set_tsv(
            &joined(&a.tsv_lines()).replace(&format!("\t{name}\t"), &format!("\t{dotted}\t")),
        );
        add_file(&a, "notes.txt", "notes");
        cases.push(("o2 a listed archive name with a dot first", a));
        let runs: Vec<(&str, Split)> = cases.iter().map(|(what, a)| (*what, a.run())).collect();
        // Every case that was not refused, named at once.
        let taken: Vec<&str> = runs
            .iter()
            .filter(|(_, r)| r.run.code != 1)
            .map(|(what, _)| *what)
            .collect();
        assert!(taken.is_empty(), "not refused: {taken:?}");
        for (what, r) in &runs {
            r.assert_one_message(what);
            assert!(
                r.stderr.contains(&archives_message()),
                "{what}: {:?}",
                r.stderr
            );
        }
    }

    #[test]
    fn the_verify_step_uploads_only_the_files_it_checked() {
        // o1: a file of another platform's folder with the name of a checked archive.
        let a = Artifacts::new();
        let name = a.lines()[0][66..].to_string();
        std::fs::write(
            a.artifacts().join("plugins-linux-arm64").join(&name),
            "other bytes",
        )
        .expect("write");
        let r = a.run();
        assert_eq!(r.run, 0, "o1");
        a.assert_dist_is_verified("o1");
        // A file that no list names is left out.
        let a = Artifacts::new();
        add_file(&a, "notes.txt", "notes");
        let r = a.run();
        assert_eq!(r.run, 0, "extra file");
        a.assert_dist_is_verified("extra file");
    }

    #[test]
    fn the_verify_step_never_replaces_a_release_file() {
        // Only a platform named twice reaches the check: the names of two platforms differ.
        let a = Artifacts::new();
        let r = a.run_on(&format!("{} {PLATFORM}", a.platforms));
        assert_eq!(r.run, 1);
        let errors: Vec<&str> = r
            .run
            .output
            .lines()
            .filter(|l| l.starts_with("error: ") || l.starts_with("::error::"))
            .collect();
        assert_eq!(
            errors,
            [format!(
                "::error::{PLATFORM}: com.tasty.p0-0.1.0-{PLATFORM}.tar.gz is already in the release files"
            )],
            "{:?}",
            r.run
        );
    }

    #[test]
    fn the_verify_step_reports_a_bad_line_once() {
        // c2: the right list, then a line that is no "<sha256>  <file>", without a newline.
        let a = Artifacts::new();
        let list = format!("{}junk", joined(&a.lines()));
        a.set_list(&list);
        let r = a.run();
        assert_eq!(r.run, 1);
        r.assert_one_message("junk last line");
        assert!(
            r.stderr.contains(&format!(
                "publish: unexpected line in SHA256SUMS-{PLATFORM}.txt: 'junk'"
            )),
            "{:?}",
            r.stderr
        );
    }
}
