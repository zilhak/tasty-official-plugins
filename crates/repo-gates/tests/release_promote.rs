//! scripts/release-promote.sh marks a plugin pre-release Latest only after every check
//! passes. These tests run it with a fake gh first on PATH: the fake answers each call
//! from a table of exact argument lists, copies files for `release download`, and records
//! `release edit`. A call the table does not hold fails the run, so the tests also pin
//! which reads the script makes. Each refusal must leave the Release unedited.

#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::io::Write;
use std::process::Stdio;

use support::gate_env::GateRun;

const REPO: &str = "zilhak/tasty-official-plugins";
const TASTY: &str = "zilhak/tasty";
const TAG: &str = "v1.2.3";
const COMMIT: &str = "1111111111111111111111111111111111111111";
const TAG_OBJECT: &str = "5555555555555555555555555555555555555555";
const OTHER_COMMIT: &str = "2222222222222222222222222222222222222222";
const TASTY_REV: &str = "3333333333333333333333333333333333333333";
const OTHER_REV: &str = "4444444444444444444444444444444444444444";
const MIN: &str = "0.15.0";
const NEW: &str = "0.15.0";
const ON: &str = "linux-x64, macos-arm64, windows-x64";
const PLATFORMS: [&str; 5] = [
    "windows-x64",
    "windows-arm64",
    "macos-arm64",
    "linux-x64",
    "linux-arm64",
];
const PLUGINS: [(&str, &str); 2] = [("com.tasty.a", "0.1.0"), ("com.tasty.b", "0.2.0")];

const STATE_JQ: &str = r#""\(.draft) \(.prerelease)""#;
const OBJECT_JQ: &str = r#""\(.object.type) \(.object.sha)""#;
const ASSETS_JQ: &str = r#".assets[] | "\(.name) \(.digest)""#;

fn pending(rev: &str) -> String {
    format!("Requires Tasty: pending (a Tasty release that includes tasty commit {rev})")
}

fn sha256(data: &[u8]) -> String {
    let mut child = std::process::Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run sha256sum");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(data)
        .expect("write");
    let out = child.wait_with_output().expect("sha256sum output");
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

/// One answer of the fake gh.
#[derive(Clone)]
struct Reply {
    out: String,
    err: String,
    code: i32,
}

fn ok(out: impl Into<String>) -> Reply {
    Reply {
        out: out.into(),
        err: String::new(),
        code: 0,
    }
}

fn not_found() -> Reply {
    Reply {
        out: String::new(),
        err: "gh: Not Found (HTTP 404)\n".into(),
        code: 1,
    }
}

fn server_error() -> Reply {
    Reply {
        out: String::new(),
        err: "gh: HTTP 502: Bad Gateway\n".into(),
        code: 1,
    }
}

/// The GitHub state the fake gh shows. `World::good()` passes every check.
struct World {
    latest: Reply,
    latest_after_edit: Reply,
    state: Reply,
    tag_ref: Reply,
    tag_object: Reply,
    notes: String,
    tasty: BTreeMap<String, Reply>,
    lock: Reply,
    /// The published files by name.
    assets: BTreeMap<String, Vec<u8>>,
    /// Digests that differ from the file content, by name.
    digests: BTreeMap<String, String>,
}

fn archive_name(id: &str, version: &str, platform: &str) -> String {
    format!("{id}-{version}-{platform}.tar.gz")
}

fn meta_text(commit: &str, rev: &str) -> String {
    let mut s = format!(
        "schema = 1\n\n[release]\nrepository = \"{REPO}\"\ntag = \"{TAG}\"\ncommit = \"{commit}\"\n\
         tasty_rev = \"{rev}\"\n"
    );
    for (id, version) in PLUGINS {
        s.push_str(&format!(
            "\n[[plugin]]\nid = \"{id}\"\nversion = \"{version}\"\nbundle = true\n"
        ));
    }
    s
}

fn notes_with(block: &[String]) -> String {
    format!(
        "Changes:\n- fixes\nCommit: {OTHER_COMMIT}\n\n{}\n",
        block.join("\n")
    )
}

fn good_block() -> Vec<String> {
    vec![
        format!("Commit: {COMMIT}"),
        format!("tasty rev: {TASTY_REV}"),
        pending(TASTY_REV),
    ]
}

impl World {
    fn good() -> Self {
        let mut assets = BTreeMap::new();
        for platform in PLATFORMS {
            let mut sums = String::new();
            let mut tsv = String::from("id\tversion\tplatform\tfile\tsha256\n");
            for (id, version) in PLUGINS {
                let name = archive_name(id, version, platform);
                let data = format!("archive {name}").into_bytes();
                let hash = sha256(&data);
                sums.push_str(&format!("{hash}  {name}\n"));
                tsv.push_str(&format!("{id}\t{version}\t{platform}\t{name}\t{hash}\n"));
                assets.insert(name, data);
            }
            assets.insert(format!("SHA256SUMS-{platform}.txt"), sums.into_bytes());
            assets.insert(format!("plugins-{platform}.tsv"), tsv.into_bytes());
        }
        assets.insert(
            "release-meta.toml".into(),
            meta_text(COMMIT, TASTY_REV).into_bytes(),
        );
        let mut tasty = BTreeMap::new();
        tasty.insert(NEW.to_string(), ok("false false\n"));
        tasty.insert("0.14.0".to_string(), ok("false false\n"));
        let mut w = Self {
            latest: ok("v1.2.2\n"),
            latest_after_edit: ok(format!("{TAG}\n")),
            state: ok("false true\n"),
            tag_ref: ok(format!("tag {TAG_OBJECT}\n")),
            tag_object: ok(format!("commit {COMMIT}\n")),
            notes: notes_with(&good_block()),
            tasty,
            lock: ok(String::new()),
            assets,
            digests: BTreeMap::new(),
        };
        w.lock = ok(w.lock_text(REPO, TAG, COMMIT, TASTY_REV));
        w
    }

    /// The tasty lock file that bundles the archives of this world.
    fn lock_text(&self, repo: &str, tag: &str, commit: &str, rev: &str) -> String {
        let mut s = format!(
            "schema = 1\n\n[release]\nrepository = \"{repo}\"\ntag = \"{tag}\"\n\
             commit = \"{commit}\"\ntasty_rev = \"{rev}\"\nmanifest_version = 1\n"
        );
        for (id, version) in PLUGINS {
            s.push_str(&format!(
                "\n[[plugin]]\nid = \"{id}\"\nversion = \"{version}\"\nbundle = true\n"
            ));
            for platform in PLATFORMS {
                let name = archive_name(id, version, platform);
                let hash = sha256(&self.assets[&name]);
                s.push_str(&format!(
                    "\n  [[plugin.artifact]]\n  platform = \"{platform}\"\n  file = \"{name}\"\n  \
                     url = \"https://github.com/{repo}/releases/download/{tag}/{name}\"\n  \
                     sha256 = \"{hash}\"\n"
                ));
            }
        }
        s
    }

    fn promoted_notes(&self) -> String {
        notes_with(&[
            format!("Commit: {COMMIT}"),
            format!("tasty rev: {TASTY_REV}"),
            format!("Requires Tasty: {MIN} or later"),
            format!("Verified with Tasty: {MIN} (minimum), {NEW} (bundled) on {ON}"),
        ])
    }

    fn replies(&self) -> Vec<(String, &'static str, Reply)> {
        let release = format!("api repos/{REPO}/releases/tags/{TAG} --jq");
        let mut assets = String::new();
        for (name, data) in &self.assets {
            let digest = self
                .digests
                .get(name)
                .cloned()
                .unwrap_or_else(|| sha256(data));
            assets.push_str(&format!("{name} sha256:{digest}\n"));
        }
        let mut r = vec![
            (
                format!("api repos/{REPO}/releases/latest --jq .tag_name"),
                "before",
                self.latest.clone(),
            ),
            (
                format!("api repos/{REPO}/releases/latest --jq .tag_name"),
                "after",
                self.latest_after_edit.clone(),
            ),
            (
                format!("api repos/{REPO} --jq .full_name"),
                "any",
                ok(format!("{REPO}\n")),
            ),
            (format!("{release} {STATE_JQ}"), "any", self.state.clone()),
            (
                format!("{release} .body"),
                "any",
                ok(format!("{}\n", self.notes)),
            ),
            (format!("{release} {ASSETS_JQ}"), "any", ok(assets)),
            (
                format!("api repos/{REPO}/git/ref/tags/{TAG} --jq {OBJECT_JQ}"),
                "any",
                self.tag_ref.clone(),
            ),
            (
                format!("api repos/{REPO}/git/tags/{TAG_OBJECT} --jq {OBJECT_JQ}"),
                "any",
                self.tag_object.clone(),
            ),
            (
                format!(
                    "api -H Accept: application/vnd.github.raw+json \
                     repos/{TASTY}/contents/official-plugins.lock.toml?ref=v{NEW}"
                ),
                "any",
                self.lock.clone(),
            ),
        ];
        for (version, reply) in &self.tasty {
            r.push((
                format!("api repos/{TASTY}/releases/tags/v{version} --jq {STATE_JQ}"),
                "any",
                reply.clone(),
            ));
        }
        r
    }
}

const FAKE_GH: &str = r#"#!/usr/bin/env bash
d="$FAKE_GH_DIR"
printf '%s\n' "$*" >>"$d/calls.log"
if [[ "$1 ${2:-}" == "release download" ]]; then
  shift 3
  out="" pats=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dir) out=$2 ;;
      --pattern) pats+=("$2") ;;
      *) echo "fake gh: unknown download option $1" >&2; exit 97 ;;
    esac
    shift 2
  done
  for f in "$d/assets"/*; do
    n=$(basename "$f")
    for p in "${pats[@]}"; do
      # shellcheck disable=SC2053
      if [[ "$n" == $p ]]; then cp "$f" "$out/"; fi
    done
  done
  exit 0
fi
if [[ "$1 ${2:-}" == "release edit" ]]; then
  prev=""
  for a in "$@"; do
    [[ "$prev" == --notes-file ]] && cp "$a" "$d/edited-notes.md"
    prev=$a
  done
  touch "$d/edited"
  exit 0
fi
phase=before
[[ -e "$d/edited" ]] && phase=after
for r in "$d"/replies/*; do
  [[ "$(cat "$r/args")" == "$*" ]] || continue
  p=$(cat "$r/phase")
  [[ "$p" == any || "$p" == "$phase" ]] || continue
  cat "$r/out"
  cat "$r/err" >&2
  exit "$(cat "$r/code")"
done
echo "fake gh: no reply for: $*" >&2
exit 97
"#;

struct Run {
    result: GateRun,
    calls: String,
    edited_notes: Option<String>,
}

impl Run {
    fn edits(&self) -> Vec<&str> {
        self.calls
            .lines()
            .filter(|l| l.starts_with("release edit"))
            .collect()
    }

    fn assert_refused(&self, what: &str) {
        assert_eq!(self.result, 1, "{what}");
        assert!(
            self.edits().is_empty(),
            "{what}: refused run edited the Release\n{}",
            self.calls
        );
        assert!(
            !self.result.output.contains("fake gh: no reply"),
            "{what}: the script made a call the fake does not know\n{:?}",
            self.result
        );
    }
}

fn promote(world: &World, args: &[&str]) -> Run {
    let dir = tempfile::tempdir().expect("temp dir");
    let d = dir.path();
    let bin = d.join("bin");
    support::write_executable(&bin.join("gh"), FAKE_GH).expect("write fake gh");
    std::fs::create_dir_all(d.join("assets")).expect("assets");
    for (name, data) in &world.assets {
        std::fs::write(d.join("assets").join(name), data).expect("asset");
    }
    for (i, (args, phase, reply)) in world.replies().into_iter().enumerate() {
        let r = d.join("replies").join(i.to_string());
        std::fs::create_dir_all(&r).expect("reply dir");
        std::fs::write(r.join("args"), args).expect("args");
        std::fs::write(r.join("phase"), phase).expect("phase");
        std::fs::write(r.join("out"), &reply.out).expect("out");
        std::fs::write(r.join("err"), &reply.err).expect("err");
        std::fs::write(r.join("code"), reply.code.to_string()).expect("code");
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = support::git_env::command("bash")
        .arg(support::repo_file("scripts/release-promote.sh"))
        .args(args)
        .env("PATH", path)
        .env("FAKE_GH_DIR", d)
        .env("GH_REPO", REPO)
        // The real gh, if it were ever reached, has no account to act with.
        .env("GH_TOKEN", "fake")
        .env("GH_CONFIG_DIR", d.join("gh-config"))
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("run scripts/release-promote.sh");
    Run {
        result: GateRun::from_output(&out),
        calls: std::fs::read_to_string(d.join("calls.log")).unwrap_or_default(),
        edited_notes: std::fs::read_to_string(d.join("edited-notes.md")).ok(),
    }
}

fn default_args() -> Vec<&'static str> {
    vec![
        "--tag",
        TAG,
        "--min-tasty",
        MIN,
        "--new-tasty",
        NEW,
        "--verified-on",
        ON,
    ]
}

fn run_good(world: &World) -> Run {
    promote(world, &default_args())
}

#[test]
fn promotion_rewrites_the_requires_line_once_and_marks_latest() {
    let w = World::good();
    let run = run_good(&w);
    assert_eq!(run.result, 0);
    let edits = run.edits();
    assert_eq!(edits.len(), 1, "{}", run.calls);
    assert!(
        edits[0].starts_with(&format!("release edit {TAG} --notes-file ")),
        "{}",
        edits[0]
    );
    assert!(
        edits[0].ends_with(" --prerelease=false --latest"),
        "{}",
        edits[0]
    );
    let notes = run.edited_notes.expect("notes sent with the edit");
    assert_eq!(notes, format!("{}\n", w.promoted_notes().trim_end()));
    assert_eq!(notes.matches("Requires Tasty:").count(), 1, "{notes}");
    assert!(!notes.contains("pending"), "{notes}");
}

#[test]
fn promotion_is_allowed_when_there_is_no_latest_release() {
    let mut w = World::good();
    w.latest = not_found();
    let run = run_good(&w);
    assert_eq!(run.result, 0);
    assert_eq!(run.edits().len(), 1);
}

#[test]
fn a_failed_latest_lookup_is_not_taken_as_no_latest_release() {
    let mut w = World::good();
    w.latest = server_error();
    promote(&w, &default_args()).assert_refused("Latest lookup failed");
}

#[test]
fn an_already_promoted_release_with_the_same_lines_succeeds_without_an_edit() {
    let mut w = World::good();
    w.latest = ok(format!("{TAG}\n"));
    w.notes = w.promoted_notes();
    // Check 0 comes before check 1, which would refuse a release that is no pre-release.
    w.state = ok("false false\n");
    let run = run_good(&w);
    assert_eq!(run.result, 0);
    assert!(run.edits().is_empty(), "{}", run.calls);
}

#[test]
fn an_already_latest_release_with_other_lines_is_refused() {
    let mut w = World::good();
    w.latest = ok(format!("{TAG}\n"));
    w.notes = w.promoted_notes().replace(ON, "linux-x64");
    run_good(&w).assert_refused("Latest with other Verified line");
    let mut w = World::good();
    w.latest = ok(format!("{TAG}\n"));
    run_good(&w).assert_refused("Latest with a pending line");
}

#[test]
fn promotion_refuses_a_release_that_is_not_a_published_prerelease() {
    for state in ["false false\n", "true true\n"] {
        let mut w = World::good();
        w.state = ok(state);
        run_good(&w).assert_refused(state);
    }
    let mut w = World::good();
    w.state = not_found();
    run_good(&w).assert_refused("no Release");
}

#[test]
fn the_commit_line_is_compared_with_the_peeled_tag_commit() {
    // The Commit line names the tag object, not the commit it points at.
    let mut w = World::good();
    w.notes = notes_with(&[
        format!("Commit: {TAG_OBJECT}"),
        format!("tasty rev: {TASTY_REV}"),
        pending(TASTY_REV),
    ]);
    run_good(&w).assert_refused("Commit is the tag object");
    // The annotated tag points at another commit.
    let mut w = World::good();
    w.tag_object = ok(format!("commit {OTHER_COMMIT}\n"));
    run_good(&w).assert_refused("tag points at another commit");
    // A lightweight tag points at the commit directly.
    let mut w = World::good();
    w.tag_ref = ok(format!("commit {COMMIT}\n"));
    assert_eq!(run_good(&w).result, 0);
}

#[test]
fn promotion_refuses_tasty_releases_that_are_not_published() {
    let mut w = World::good();
    w.tasty.insert(NEW.into(), ok("false true\n"));
    run_good(&w).assert_refused("new Tasty is a pre-release");
    let mut w = World::good();
    w.tasty.insert(NEW.into(), not_found());
    run_good(&w).assert_refused("no new Tasty release");
    let mut w = World::good();
    w.tasty.insert("0.14.0".into(), ok("true false\n"));
    let mut args = default_args();
    args[3] = "0.14.0";
    promote(&w, &args).assert_refused("min Tasty is a draft");
}

#[test]
fn promotion_refuses_a_release_that_no_published_tasty_bundles() {
    let mut w = World::good();
    w.lock = not_found();
    run_good(&w).assert_refused("no lock file");
    for (what, lock) in [
        (
            "other repository",
            w.lock_text("someone/tasty-official-plugins", TAG, COMMIT, TASTY_REV),
        ),
        ("other tag", w.lock_text(REPO, "v1.2.2", COMMIT, TASTY_REV)),
        (
            "other commit",
            w.lock_text(REPO, TAG, OTHER_COMMIT, TASTY_REV),
        ),
        ("tag object", w.lock_text(REPO, TAG, TAG_OBJECT, TASTY_REV)),
    ] {
        let mut w = World::good();
        w.lock = ok(lock);
        run_good(&w).assert_refused(what);
    }
}

#[test]
fn the_minimum_tasty_may_not_be_newer_than_the_bundling_tasty() {
    let mut w = World::good();
    w.tasty.insert("0.16.0".into(), ok("false false\n"));
    let mut args = default_args();
    args[3] = "0.16.0";
    promote(&w, &args).assert_refused("min > new");
    // An older minimum is fine.
    let mut args = default_args();
    args[3] = "0.14.0";
    assert_eq!(promote(&World::good(), &args).result, 0);
}

#[test]
fn an_older_tag_is_never_made_latest_again() {
    for latest in ["v1.3.0\n", "v1.2.10\n", "v2.0.0\n"] {
        let mut w = World::good();
        w.latest = ok(latest);
        run_good(&w).assert_refused(latest);
    }
    let mut w = World::good();
    w.latest = ok("v1.2.03\n");
    run_good(&w).assert_refused("Latest is no SemVer tag");
}

#[test]
fn the_marker_block_must_hold_one_pending_line() {
    let cases = [
        ("no Requires line", good_block()[..2].to_vec()),
        ("settled Requires line", {
            let mut b = good_block()[..2].to_vec();
            b.push("Requires Tasty: 0.14.0 or later".into());
            b
        }),
        ("two pending lines", {
            let mut b = good_block();
            b.push(pending(TASTY_REV));
            b
        }),
    ];
    for (what, block) in cases {
        let mut w = World::good();
        w.notes = notes_with(&block);
        run_good(&w).assert_refused(what);
    }
}

#[test]
fn the_published_files_must_be_the_set_a_tag_release_uploads() {
    let mut w = World::good();
    w.assets.insert("extra.txt".into(), b"extra".to_vec());
    run_good(&w).assert_refused("extra file");

    let mut w = World::good();
    w.assets
        .remove(&archive_name("com.tasty.a", "0.1.0", "linux-x64"));
    run_good(&w).assert_refused("missing archive");

    let mut w = World::good();
    w.assets.remove("plugins-macos-arm64.tsv");
    run_good(&w).assert_refused("missing TSV list");
}

#[test]
fn the_published_hashes_must_agree() {
    // An archive whose GitHub digest is not its SHA256SUMS hash.
    let mut w = World::good();
    w.digests.insert(
        archive_name("com.tasty.b", "0.2.0", "windows-arm64"),
        "0".repeat(64),
    );
    run_good(&w).assert_refused("archive digest");

    // A downloaded list whose GitHub digest differs from its content.
    let mut w = World::good();
    w.digests
        .insert("SHA256SUMS-linux-arm64.txt".into(), "0".repeat(64));
    run_good(&w).assert_refused("list digest");

    // The TSV list names another hash than SHA256SUMS.
    let mut w = World::good();
    let tsv = String::from_utf8(w.assets["plugins-linux-x64.tsv"].clone()).expect("utf-8");
    let first_hash = tsv
        .lines()
        .nth(1)
        .expect("row")
        .rsplit('\t')
        .next()
        .expect("hash");
    let changed = tsv.replace(first_hash, &"f".repeat(64));
    w.assets
        .insert("plugins-linux-x64.tsv".into(), changed.into_bytes());
    run_good(&w).assert_refused("TSV hash");

    // The lock file pins another hash.
    let mut w = World::good();
    let lock = w.lock.out.clone();
    let name = archive_name("com.tasty.a", "0.1.0", "macos-arm64");
    let hash = sha256(&w.assets[&name]);
    w.lock = ok(lock.replacen(&hash, &"e".repeat(64), 1));
    run_good(&w).assert_refused("lock hash");
}

#[test]
fn the_marker_block_and_the_lock_must_match_the_published_release_meta() {
    // release-meta.toml for another commit.
    let mut w = World::good();
    w.assets.insert(
        "release-meta.toml".into(),
        meta_text(OTHER_COMMIT, TASTY_REV).into_bytes(),
    );
    run_good(&w).assert_refused("release-meta commit");

    // Only the tasty rev line changed.
    let mut w = World::good();
    w.notes = notes_with(&[
        format!("Commit: {COMMIT}"),
        format!("tasty rev: {OTHER_REV}"),
        pending(TASTY_REV),
    ]);
    run_good(&w).assert_refused("tasty rev line");

    // Only the pending pin changed.
    let mut w = World::good();
    w.notes = notes_with(&[
        format!("Commit: {COMMIT}"),
        format!("tasty rev: {TASTY_REV}"),
        pending(OTHER_REV),
    ]);
    run_good(&w).assert_refused("pending pin");

    // The lock file was made from another tasty rev.
    let mut w = World::good();
    w.lock = ok(w.lock_text(REPO, TAG, COMMIT, OTHER_REV));
    run_good(&w).assert_refused("lock tasty_rev");
}

#[test]
fn the_latest_release_must_be_the_tag_after_the_edit() {
    let mut w = World::good();
    w.latest_after_edit = ok("v1.2.2\n");
    let run = run_good(&w);
    assert_eq!(run.result, 1);
    assert_eq!(run.edits().len(), 1);
}

#[test]
fn bad_inputs_are_refused_before_any_call() {
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("two-line platforms", {
            let mut a = default_args();
            a[7] = "linux-x64\nmacos-arm64";
            a
        }),
        ("carriage return", {
            let mut a = default_args();
            a[7] = "linux-x64\r";
            a
        }),
        ("empty platforms", {
            let mut a = default_args();
            a[7] = " ";
            a
        }),
        ("tag with a leading zero", {
            let mut a = default_args();
            a[1] = "v01.2.3";
            a
        }),
        ("tag without v", {
            let mut a = default_args();
            a[1] = "1.2.3";
            a
        }),
        ("min is no version", {
            let mut a = default_args();
            a[3] = "0.15";
            a
        }),
        ("new is no version", {
            let mut a = default_args();
            a[5] = "v0.15.0";
            a
        }),
        ("missing option value", vec!["--tag"]),
    ];
    for (what, args) in cases {
        let run = promote(&World::good(), &args);
        assert_eq!(run.result, 2, "{what}");
        assert!(run.calls.is_empty(), "{what}: {}", run.calls);
    }
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(support::repo_file(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn line_value<'a>(text: &'a str, prefix: &str, rel: &str) -> &'a str {
    text.lines()
        .find_map(|l| l.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("{rel} has no line starting with {prefix:?}"))
}

#[test]
fn the_promotion_platforms_are_the_release_platforms() {
    let release = read(".github/workflows/release.yml");
    let promote = read("scripts/release-promote.sh");
    let from_release: Vec<&str> = line_value(&release, "  PLATFORMS: ", "release.yml")
        .split_whitespace()
        .collect();
    let from_promote: Vec<&str> = line_value(&promote, "PLATFORMS=(", "release-promote.sh")
        .trim_end_matches(')')
        .split_whitespace()
        .collect();
    assert_eq!(from_promote, from_release);
    assert_eq!(from_release, PLATFORMS);
}

#[test]
fn the_promote_workflow_runs_the_script_one_at_a_time() {
    let wf = read(".github/workflows/promote.yml");
    assert!(wf.contains("\n  workflow_dispatch:\n"), "{wf}");
    assert!(!wf.contains("\n  push:"), "{wf}");
    // One group for the whole repository, not one per tag.
    assert!(
        wf.contains("concurrency:\n  group: promote\n  cancel-in-progress: false\n"),
        "{wf}"
    );
    assert!(
        wf.contains("scripts/release-promote.sh --tag \"$TAG\""),
        "{wf}"
    );
    assert!(wf.contains("refs/heads/main"), "{wf}");
}
