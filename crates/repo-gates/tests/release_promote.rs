//! scripts/release-promote.sh marks a plugin pre-release Latest only after every check
//! passes. These tests run it with the fake gh of support/fake_gh.rs first on PATH. A call
//! its table does not hold fails the run, so the tests also pin which reads the script
//! makes. Each refusal must leave the Release unedited.
//!
//! Check 7 reads the plugin set of the tag commit with git, so each test runs the script
//! from a throwaway repository whose one commit is the tag commit: it holds this
//! repository's scripts/check.sh and the tasty-plugin.toml of every crate that
//! `scripts/check.sh list-bins` names, so the good world publishes the files of a real
//! release (8 plugins on 5 platforms, 51 files). Check 8 reads min_tasty_version from those
//! manifests, so the tree drops the value the real files have and each world sets its own:
//! the good world gives every manifest MIN.

#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use support::fake_gh::{Entry, Fake, Reply, entry, not_found, ok, server_error};
use support::gate_env::GateRun;

const REPO: &str = "zilhak/tasty-official-plugins";
const TASTY: &str = "zilhak/tasty";
const TAG: &str = "v1.2.3";
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

const STATE_JQ: &str = r#""\(.draft) \(.prerelease)""#;
const OBJECT_JQ: &str = r#""\(.object.type) \(.object.sha)""#;
const ASSETS_JQ: &str = r#".assets[] | "\(.name) \(.digest)""#;

/// A plugin as an archive names it: id and version.
type Plugin = (String, String);

fn pending(rev: &str) -> String {
    format!("Requires Tasty: pending (a Tasty release that includes tasty commit {rev})")
}

/// The SHA-256 of each file, from one sha256sum run.
fn sha256_all(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, data) in files {
        std::fs::write(dir.path().join(name), data).expect("write");
    }
    let out = std::process::Command::new("sha256sum")
        .args(files.keys())
        .current_dir(dir.path())
        .output()
        .expect("run sha256sum");
    assert!(out.status.success(), "sha256sum failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            let (hash, name) = l.split_once("  ").expect("sha256sum line");
            (name.to_string(), hash.to_string())
        })
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = support::git_env::command("git")
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

/// The first `key = "value"` line of a tasty-plugin.toml.
fn manifest_value(text: &str, key: &str) -> String {
    text.lines()
        .find(|l| {
            l.strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .and_then(|l| l.split('"').nth(1))
        .unwrap_or_else(|| panic!("no {key} in manifest"))
        .to_string()
}

/// `manifest` without its min_tasty_version lines.
fn without_min_tasty(manifest: &str) -> String {
    let out: String = manifest
        .lines()
        .filter(|l| !l.trim_start().starts_with("min_tasty_version"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(!out.contains("min_tasty_version"), "{out}");
    out
}

/// `manifest` with `lines` right before its first table.
fn before_first_table(manifest: &str, lines: &[&str]) -> String {
    let mut out: Vec<&str> = manifest.lines().collect();
    let at = out
        .iter()
        .position(|l| l.starts_with('['))
        .expect("a table in the manifest");
    out.splice(at..at, lines.iter().copied());
    out.join("\n") + "\n"
}

/// `manifest` with the top-level line min_tasty_version = "<value>".
fn with_min_tasty(manifest: &str, value: &str) -> String {
    before_first_table(manifest, &[&format!("min_tasty_version = \"{value}\"")])
}

/// `manifest` with `lines` at its end, in its last table.
fn at_end(manifest: &str, lines: &[&str]) -> String {
    format!("{manifest}{}\n", lines.join("\n"))
}

/// The tag commit: a throwaway repository with the release scripts to run.
struct Tree {
    dir: tempfile::TempDir,
    commit: String,
    /// The plugins the tag commit packages.
    plugins: Vec<Plugin>,
    /// Their crates, in the same order.
    crates: Vec<String>,
}

/// The tag commit whose manifests all carry min_tasty_version = MIN.
fn tree() -> Tree {
    tree_with(|_, m| with_min_tasty(m, MIN))
}

/// The tag commit whose manifest number `i` (in list-bins order) is `manifest(i, base)`,
/// where `base` is the manifest of this repository without min_tasty_version.
fn tree_with(manifest: impl Fn(usize, &str) -> String) -> Tree {
    let dir = tempfile::tempdir().expect("temp dir");
    let d = dir.path();
    git(d, &["init", "--quiet", "-b", "main"]);
    git(d, &["config", "user.email", "fixture@example.invalid"]);
    git(d, &["config", "user.name", "fixture"]);
    // Keep the user's git hooks out of the commit of the synthetic repository.
    git(d, &["config", "core.hooksPath", "/dev/null"]);
    for script in [
        "scripts/check.sh",
        "scripts/release-check.sh",
        "scripts/release-promote.sh",
        "scripts/lib/min-tasty.sh",
    ] {
        let body = std::fs::read(support::repo_file(script)).expect("read script");
        support::write_executable(&d.join(script), body).expect("copy script");
    }
    let bins = support::git_env::command("bash")
        .arg(support::repo_file("scripts/check.sh"))
        .arg("list-bins")
        .output()
        .expect("run check.sh list-bins");
    assert!(bins.status.success(), "check.sh list-bins failed");
    let mut plugins = Vec::new();
    let mut crates = Vec::new();
    for (i, crate_name) in String::from_utf8_lossy(&bins.stdout).lines().enumerate() {
        let rel = format!("crates/{crate_name}/tasty-plugin.toml");
        let real = std::fs::read_to_string(support::repo_file(&rel)).expect("read manifest");
        let text = manifest(i, &without_min_tasty(&real));
        std::fs::create_dir_all(d.join(format!("crates/{crate_name}"))).expect("crate dir");
        std::fs::write(d.join(&rel), &text).expect("write manifest");
        plugins.push((
            manifest_value(&text, "id"),
            manifest_value(&text, "version"),
        ));
        crates.push(crate_name.to_string());
    }
    assert!(plugins.len() > 1, "list-bins names {plugins:?}");
    git(d, &["add", "-A"]);
    git(d, &["commit", "--quiet", "-m", "tag commit"]);
    let commit = git(d, &["rev-parse", "HEAD"]);
    let tree = Tree {
        dir,
        commit,
        plugins,
        crates,
    };
    // The worlds do not lean on the tags the script lists: each run starts from an empty
    // list, and the list tests set their own.
    list_unvalued(&tree, &[]);
    tree
}

/// Sets UNVALUED_TAGS of the script copy that the runs use to `entries` ("<tag>
/// <commit>"), whatever the script lists. It takes the two forms the comment above the
/// list in the script names: all on one line ending in `)`, or one entry per line with a
/// line `)` alone at the end. The tag commit is made already, so its sha stays.
fn list_unvalued(tree: &Tree, entries: &[String]) {
    let path = tree.dir.path().join("scripts/release-promote.sh");
    let script = std::fs::read_to_string(&path).expect("read script copy");
    let lines: Vec<&str> = script.lines().collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with("UNVALUED_TAGS=("))
        .collect();
    assert_eq!(starts.len(), 1, "one UNVALUED_TAGS assignment");
    let start = starts[0];
    let end = if lines[start].trim_end().ends_with(')') {
        start
    } else {
        start
            + lines[start..]
                .iter()
                .position(|l| l.trim() == ")")
                .expect("the line that closes UNVALUED_TAGS")
    };
    let list: Vec<String> = entries.iter().map(|e| format!("\"{e}\"")).collect();
    let listed = format!("UNVALUED_TAGS=({})", list.join(" "));
    let mut out: Vec<&str> = lines[..start].to_vec();
    out.push(&listed);
    out.extend_from_slice(&lines[end + 1..]);
    std::fs::write(&path, out.join("\n") + "\n").expect("write script copy");
}

/// The GitHub state the fake gh shows. `World::good()` passes every check.
struct World {
    tree: Tree,
    latest: Reply,
    /// The answer to the third Latest lookup, the one right before the edit (the first two
    /// are checks 0 and 5). None: the same as `latest`.
    latest_recheck: Option<Reply>,
    latest_after_edit: Reply,
    state: Reply,
    tag_ref: Reply,
    tag_object: Reply,
    notes: String,
    tasty: BTreeMap<String, Reply>,
    lock: Reply,
    /// The plugins whose archives, list rows, and lock entries are published.
    archived: Vec<Plugin>,
    /// The plugins the published release-meta.toml lists.
    meta_plugins: Vec<Plugin>,
    /// The published files by name.
    assets: BTreeMap<String, Vec<u8>>,
    /// Digests that differ from the file content, by name.
    digests: BTreeMap<String, String>,
}

fn archive_name(plugin: &Plugin, platform: &str) -> String {
    format!("{}-{}-{platform}.tar.gz", plugin.0, plugin.1)
}

fn notes_with(block: &[String]) -> String {
    format!(
        "Changes:\n- fixes\nCommit: {OTHER_COMMIT}\n\n{}\n",
        block.join("\n")
    )
}

impl World {
    /// Publishes the release of the tag commit as a tag run uploads it.
    fn good() -> Self {
        Self::of(tree())
    }

    /// Publishes the release of `tree` as a tag run uploads it.
    fn of(tree: Tree) -> Self {
        let plugins = tree.plugins.clone();
        Self::publishing(tree, plugins.clone(), plugins)
    }

    /// Publishes archives, lists, and lock entries for `archived`, and a release-meta.toml
    /// that lists `meta_plugins`. All of them agree on names and hashes.
    fn publishing(tree: Tree, archived: Vec<Plugin>, meta_plugins: Vec<Plugin>) -> Self {
        let mut tasty = BTreeMap::new();
        tasty.insert(NEW.to_string(), ok("false false\n"));
        tasty.insert("0.14.0".to_string(), ok("false false\n"));
        let commit = tree.commit.clone();
        let mut w = Self {
            tree,
            latest: ok("v1.2.2\n"),
            latest_recheck: None,
            latest_after_edit: ok(format!("{TAG}\n")),
            state: ok("false true\n"),
            tag_ref: ok(format!("tag {TAG_OBJECT}\n")),
            tag_object: ok(format!("commit {commit}\n")),
            notes: String::new(),
            tasty,
            lock: ok(String::new()),
            archived,
            meta_plugins,
            assets: BTreeMap::new(),
            digests: BTreeMap::new(),
        };
        w.notes = notes_with(&w.good_block());
        let mut archives = BTreeMap::new();
        for platform in PLATFORMS {
            for plugin in &w.archived {
                let name = archive_name(plugin, platform);
                archives.insert(name.clone(), format!("archive {name}").into_bytes());
            }
        }
        let hashes = sha256_all(&archives);
        for platform in PLATFORMS {
            let mut sums = String::new();
            let mut tsv = String::from("id\tversion\tplatform\tfile\tsha256\n");
            for plugin in &w.archived {
                let name = archive_name(plugin, platform);
                let hash = &hashes[&name];
                sums.push_str(&format!("{hash}  {name}\n"));
                tsv.push_str(&format!(
                    "{}\t{}\t{platform}\t{name}\t{hash}\n",
                    plugin.0, plugin.1
                ));
            }
            w.assets
                .insert(format!("SHA256SUMS-{platform}.txt"), sums.into_bytes());
            w.assets
                .insert(format!("plugins-{platform}.tsv"), tsv.into_bytes());
        }
        w.assets.extend(archives);
        let meta = w.meta_text(TAG, &commit, TASTY_REV);
        w.assets
            .insert("release-meta.toml".into(), meta.into_bytes());
        w.lock = ok(w.lock_text(REPO, TAG, &commit, TASTY_REV));
        w
    }

    fn commit(&self) -> &str {
        &self.tree.commit
    }

    fn good_block(&self) -> Vec<String> {
        vec![
            format!("Commit: {}", self.commit()),
            format!("tasty rev: {TASTY_REV}"),
            pending(TASTY_REV),
        ]
    }

    fn meta_text(&self, tag: &str, commit: &str, rev: &str) -> String {
        let mut s = format!(
            "schema = 1\n\n[release]\nrepository = \"{REPO}\"\ntag = \"{tag}\"\n\
             commit = \"{commit}\"\ntasty_rev = \"{rev}\"\n"
        );
        for (id, version) in &self.meta_plugins {
            s.push_str(&format!(
                "\n[[plugin]]\nid = \"{id}\"\nversion = \"{version}\"\nbundle = true\n"
            ));
        }
        s
    }

    /// The tasty lock file that bundles the published archives.
    fn lock_text(&self, repo: &str, tag: &str, commit: &str, rev: &str) -> String {
        let hashes = sha256_all(&self.assets);
        let mut s = format!(
            "schema = 1\n\n[release]\nrepository = \"{repo}\"\ntag = \"{tag}\"\n\
             commit = \"{commit}\"\ntasty_rev = \"{rev}\"\nmanifest_version = 1\n"
        );
        for plugin in &self.archived {
            s.push_str(&format!(
                "\n[[plugin]]\nid = \"{}\"\nversion = \"{}\"\nbundle = true\n",
                plugin.0, plugin.1
            ));
            for platform in PLATFORMS {
                let name = archive_name(plugin, platform);
                let hash = &hashes[&name];
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
            format!("Commit: {}", self.commit()),
            format!("tasty rev: {TASTY_REV}"),
            format!("Requires Tasty: {MIN} or later"),
            format!("Verified with Tasty: {MIN} (minimum), {NEW} (bundled) on {ON}"),
        ])
    }

    fn entries(&self) -> Vec<Entry> {
        let release = format!("api repos/{REPO}/releases/tags/{TAG} --jq");
        let latest = format!("api repos/{REPO}/releases/latest --jq .tag_name");
        let hashes = sha256_all(&self.assets);
        let mut assets = String::new();
        for name in self.assets.keys() {
            let digest = self.digests.get(name).unwrap_or(&hashes[name]);
            assets.push_str(&format!("{name} sha256:{digest}\n"));
        }
        let mut r = Vec::new();
        if let Some(reply) = &self.latest_recheck {
            r.push(Entry {
                args: latest.clone(),
                phase: "before",
                nth: 3,
                reply: reply.clone(),
            });
        }
        r.push(Entry {
            args: latest.clone(),
            phase: "before",
            nth: 0,
            reply: self.latest.clone(),
        });
        r.push(Entry {
            args: latest,
            phase: "after",
            nth: 0,
            reply: self.latest_after_edit.clone(),
        });
        r.extend([
            entry(
                format!("api repos/{REPO} --jq .full_name"),
                ok(format!("{REPO}\n")),
            ),
            entry(format!("{release} {STATE_JQ}"), self.state.clone()),
            entry(format!("{release} .body"), ok(format!("{}\n", self.notes))),
            entry(format!("{release} {ASSETS_JQ}"), ok(assets)),
            entry(
                format!("api repos/{REPO}/git/ref/tags/{TAG} --jq {OBJECT_JQ}"),
                self.tag_ref.clone(),
            ),
            entry(
                format!("api repos/{REPO}/git/tags/{TAG_OBJECT} --jq {OBJECT_JQ}"),
                self.tag_object.clone(),
            ),
            entry(
                format!(
                    "api -H Accept: application/vnd.github.raw+json \
                     repos/{TASTY}/contents/official-plugins.lock.toml?ref=v{NEW}"
                ),
                self.lock.clone(),
            ),
        ]);
        for (version, reply) in &self.tasty {
            r.push(entry(
                format!("api repos/{TASTY}/releases/tags/v{version} --jq {STATE_JQ}"),
                reply.clone(),
            ));
        }
        r
    }
}

struct Run {
    result: GateRun,
    calls: String,
    edits: Vec<String>,
    edited_notes: Option<String>,
}

impl Run {
    fn edits(&self) -> &[String] {
        &self.edits
    }

    fn assert_refused(&self, what: &str) {
        assert_eq!(self.result, 1, "{what}");
        assert!(
            self.edits.is_empty(),
            "{what}: refused run edited the Release\n{}",
            self.calls
        );
        assert!(
            !self.result.output.contains("fake gh: no reply"),
            "{what}: the script made a call the fake does not know\n{:?}",
            self.result
        );
    }

    /// Refused by check 8, with each of `names` in its message.
    fn assert_check_8(&self, what: &str, names: &[&str]) {
        self.assert_refused(what);
        for name in std::iter::once("check 8: ").chain(names.iter().copied()) {
            assert!(
                self.result.output.contains(name),
                "{what}: no {name:?} in\n{:?}",
                self.result
            );
        }
    }

    /// Promoted with the one edit, check 8 skipped or not.
    fn assert_promoted(&self, what: &str, skipped: bool) {
        assert_eq!(self.result, 0, "{what}");
        assert_eq!(self.edits.len(), 1, "{what}: {}", self.calls);
        assert_eq!(
            self.result.output.contains("Check 8 skipped"),
            skipped,
            "{what}: {:?}",
            self.result
        );
    }
}

fn promote(world: &World, args: &[&str]) -> Run {
    let fake = Fake::new(&world.entries(), &world.assets);
    let mut cmd = support::git_env::command("bash");
    cmd.arg(world.tree.dir.path().join("scripts/release-promote.sh"))
        .args(args)
        .env("GH_REPO", REPO);
    let out = fake
        .apply(&mut cmd)
        .output()
        .expect("run scripts/release-promote.sh");
    Run {
        result: GateRun::from_output(&out),
        calls: fake.calls(),
        edits: fake.edits(),
        edited_notes: fake.edited_notes(),
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
    w.tag_ref = ok(format!("commit {}\n", w.commit()));
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
    for what in [
        "other repository",
        "other tag",
        "other commit",
        "tag object",
    ] {
        let mut w = World::good();
        let commit = w.commit().to_string();
        let lock = match what {
            "other repository" => {
                w.lock_text("someone/tasty-official-plugins", TAG, &commit, TASTY_REV)
            }
            "other tag" => w.lock_text(REPO, "v1.2.2", &commit, TASTY_REV),
            "other commit" => w.lock_text(REPO, TAG, OTHER_COMMIT, TASTY_REV),
            _ => w.lock_text(REPO, TAG, TAG_OBJECT, TASTY_REV),
        };
        w.lock = ok(lock);
        run_good(&w).assert_refused(what);
    }
}

#[test]
fn the_minimum_tasty_may_not_be_newer_than_the_bundling_tasty() {
    // The manifests require 0.16.0 too, so that only check 4 is left to refuse.
    let mut w = World::of(tree_with(|_, m| with_min_tasty(m, "0.16.0")));
    w.tasty.insert("0.16.0".into(), ok("false false\n"));
    let mut args = default_args();
    args[3] = "0.16.0";
    let run = promote(&w, &args);
    run.assert_refused("min > new");
    assert!(run.result.output.contains("check 4: "), "{:?}", run.result);
    // An older minimum is fine.
    let mut args = default_args();
    args[3] = "0.14.0";
    let w = World::of(tree_with(|_, m| with_min_tasty(m, "0.14.0")));
    promote(&w, &args).assert_promoted("older minimum", false);
}

#[test]
fn check_8_judges_the_manifests_of_the_good_world() {
    run_good(&World::good()).assert_promoted("values equal to --min-tasty", false);
}

#[test]
fn a_tag_without_values_passes_check_8_while_no_tag_is_listed() {
    let w = World::of(tree_with(|_, m| m.to_string()));
    run_good(&w).assert_promoted("no values, empty list", true);
}

#[test]
fn a_listed_tag_skips_check_8_before_its_manifests_are_read() {
    let t = tree_with(|_, m| m.to_string());
    list_unvalued(
        &t,
        &[
            format!("v1.2.2 {OTHER_COMMIT}"),
            format!("{TAG} {}", t.commit),
        ],
    );
    run_good(&World::of(t)).assert_promoted("listed, no values", true);
    // A manifest shape check 8 cannot read does not matter for a listed tag.
    let t = tree_with(|_, m| {
        before_first_table(
            m,
            &[
                "review_note = \"\"\"",
                "min_tasty_version = \"0.14.1\"",
                "\"\"\"",
            ],
        )
    });
    list_unvalued(&t, &[format!("{TAG} {}", t.commit)]);
    run_good(&World::of(t)).assert_promoted("listed, unreadable manifests", true);
}

#[test]
fn a_tag_without_values_is_refused_once_tags_are_listed() {
    // Another tag is listed.
    let t = tree_with(|_, m| m.to_string());
    list_unvalued(&t, &[format!("v1.2.2 {}", t.commit)]);
    run_good(&World::of(t)).assert_check_8("not listed", &[TAG]);
    // The tag is listed for another commit.
    let t = tree_with(|_, m| m.to_string());
    list_unvalued(&t, &[format!("{TAG} {OTHER_COMMIT}")]);
    run_good(&World::of(t)).assert_check_8("listed for another commit", &[TAG, OTHER_COMMIT]);
}

#[test]
fn each_manifest_is_compared_with_the_minimum_tasty_on_its_own() {
    let count = tree().crates.len();
    for i in [0, count / 2, count - 1] {
        // One value differs; the others are MIN.
        let t = tree_with(|j, m| with_min_tasty(m, if j == i { "0.14.1" } else { MIN }));
        let name = t.crates[i].clone();
        run_good(&World::of(t)).assert_check_8("one value differs", &[&name, "0.14.1"]);
        // One manifest has no value.
        let t = tree_with(|j, m| {
            if j == i {
                m.to_string()
            } else {
                with_min_tasty(m, MIN)
            }
        });
        let name = t.crates[i].clone();
        run_good(&World::of(t))
            .assert_check_8("one value missing", &[&name, "no min_tasty_version"]);
    }
}

/// Gives the work tree manifests of `tree` min_tasty_version = `value`, after the tag
/// commit is made: the commit keeps its own values.
fn change_work_tree_min_tasty(tree: &Tree, value: &str) {
    for crate_name in &tree.crates {
        let path = tree
            .dir
            .path()
            .join(format!("crates/{crate_name}/tasty-plugin.toml"));
        let text = std::fs::read_to_string(&path).expect("read work tree manifest");
        let changed = with_min_tasty(&without_min_tasty(&text), value);
        std::fs::write(&path, changed).expect("write work tree manifest");
    }
}

#[test]
fn check_8_reads_the_manifests_of_the_tag_commit_not_the_work_tree() {
    // The tag commit requires MIN; the work tree has another value.
    let t = tree();
    change_work_tree_min_tasty(&t, "0.14.1");
    run_good(&World::of(t)).assert_promoted("tag commit MIN, work tree other", false);
    // The work tree requires MIN; the tag commit has another value.
    let t = tree_with(|_, m| with_min_tasty(m, "0.14.1"));
    change_work_tree_min_tasty(&t, MIN);
    let name = t.crates[0].clone();
    run_good(&World::of(t)).assert_check_8("work tree MIN, tag commit other", &[&name, "0.14.1"]);
}

/// Refused by check 8 as unreadable when only manifest 1 is `shape(base with MIN)`, or when
/// every manifest is `shape(base)`.
fn assert_unreadable(what: &str, shape: impl Fn(&str) -> String) {
    let t = tree_with(|j, m| {
        let m = with_min_tasty(m, MIN);
        if j == 1 { shape(&m) } else { m }
    });
    let name = t.crates[1].clone();
    run_good(&World::of(t)).assert_check_8(what, &[&name, "cannot be read"]);
}

/// Refused by check 8 as unreadable when every manifest has `shape` and no plain line, the
/// world that would pass as "no manifest has the key" if the shape were taken for no key.
fn assert_unreadable_everywhere(what: &str, shape: impl Fn(&str) -> String) {
    let t = tree_with(|_, m| shape(m));
    let name = t.crates[0].clone();
    run_good(&World::of(t)).assert_check_8(what, &[&name, "cannot be read"]);
}

#[test]
fn manifests_whose_value_is_not_one_plain_top_level_line_are_refused() {
    let quoted = r#""min_tasty_version" = "0.15.0""#;
    assert_unreadable("quoted key", |m| {
        m.replace(r#"min_tasty_version = "0.15.0""#, quoted)
    });
    assert_unreadable("key in a table", |m| {
        at_end(&without_min_tasty(m), &[r#"min_tasty_version = "0.15.0""#])
    });
    assert_unreadable("empty string", |m| m.replace("\"0.15.0\"", "\"\""));
    assert_unreadable("leading zero", |m| m.replace("\"0.15.0\"", "\"01.0.0\""));
    assert_unreadable("two lines", |m| {
        before_first_table(m, &[r#"min_tasty_version = "0.15.0""#])
    });
    assert_unreadable("table named min_tasty_version", |m| {
        at_end(m, &["[min_tasty_version]", "probe = 1"])
    });
}

#[test]
fn shapes_where_a_line_search_misreads_the_top_level_value_are_refused() {
    let fake = [
        "review_note = \"\"\"",
        r#"min_tasty_version = "0.14.1""#,
        "\"\"\"",
    ];
    let escaped = r#""min_tasty_version" = "0.15.0""#;
    // A line inside a multi-line string, and an escaped key that TOML reads as the key.
    assert_unreadable("multi-line string and escaped key", |m| {
        let mut lines = fake.to_vec();
        lines.push(escaped);
        before_first_table(&without_min_tasty(m), &lines)
    });
    assert_unreadable("multi-line string", |m| {
        before_first_table(&without_min_tasty(m), &fake)
    });
    assert_unreadable("escaped key", |m| {
        before_first_table(&without_min_tasty(m), &[escaped])
    });
    // A top-level key made a table by a quoted header after the tables.
    assert_unreadable("quoted table header", |m| {
        at_end(m, &[r#"["min_tasty_version"]"#, "probe = 1"])
    });
    // An array element that looks like a table header does not end the top level.
    assert_unreadable("array then escaped key", |m| {
        before_first_table(
            &without_min_tasty(m),
            &["review_note = [", r#"["x"]"#, "]", escaped],
        )
    });
    // In every manifest and with no plain line, so that "no key" would pass the tag.
    assert_unreadable_everywhere("multi-line string only", |m| {
        before_first_table(
            m,
            &[
                "review_note = \"\"\"",
                r#"min_tasty_version = "0.15.0""#,
                "\"\"\"",
            ],
        )
    });
    // The string ends on a line that also reads as a bare key line, so only the multi-line
    // string itself makes the manifest unreadable.
    assert_unreadable_everywhere("multi-line string ending on a key line", |m| {
        before_first_table(
            m,
            &[
                "review_note = \"\"\"",
                r#"min_tasty_version = "0.15.0""#,
                "end = 1\"\"\"",
            ],
        )
    });
    assert_unreadable_everywhere("escape in a key", |m| {
        before_first_table(m, &[r#""min_tasty\u005fversion" = "0.15.0""#])
    });
    assert_unreadable_everywhere("escape in a table header", |m| {
        at_end(m, &[r#"["min_tasty\u005fversion"]"#, "probe = 1"])
    });
}

/// Refused by check 8 as unreadable for `reason` when manifest 1 is `shape(base)`, with
/// `base` holding no min_tasty_version, and the others carry MIN.
fn assert_unreadable_because(what: &str, reason: &str, shape: impl Fn(&str) -> String) {
    let t = tree_with(|j, m| {
        if j == 1 {
            shape(m)
        } else {
            with_min_tasty(m, MIN)
        }
    });
    let name = t.crates[1].clone();
    run_good(&World::of(t)).assert_check_8(what, &[&name, "cannot be read", reason]);
}

#[test]
fn table_headers_are_refused_for_their_first_part() {
    let quoted = "a quote or backslash in the first part of a table name";
    // Valid TOML whose quoted table does not spell the key: only the quote rule refuses it.
    assert_unreadable_because("quoted table beside the value", quoted, |m| {
        at_end(
            &with_min_tasty(m, MIN),
            &[r#"["release-notes"]"#, "probe = 1"],
        )
    });
    // Valid TOML whose escaped table is the top-level key min_tasty_version.
    assert_unreadable_because("escaped table only", quoted, |m| {
        at_end(m, &[r#"["min_tasty\u005fversion"]"#, "probe = 1"])
    });
    // Valid TOML with the bare table [min_tasty_version] only. The line names the key, so
    // the rule for lines that name it reports it first; the table rule gives the same
    // refusal.
    assert_unreadable_because("bare table only", "min_tasty_version not on a line", |m| {
        at_end(m, &["[min_tasty_version]", "probe = 1"])
    });
}

#[test]
fn brackets_must_balance_outside_strings() {
    assert_unreadable_because(
        "brackets open at the end",
        "brackets still open at the end of the file",
        |m| at_end(&with_min_tasty(m, MIN), &["review_note = ["]),
    );
    assert_unreadable_because(
        "a bracket that closes nothing",
        "a bracket that closes nothing",
        |m| at_end(&with_min_tasty(m, MIN), &["review_note = ]"]),
    );
}

#[test]
fn a_plain_value_line_is_read_beside_arrays_and_other_lines() {
    // A multi-line array before the first table, with an element like a table header.
    let w = World::of(tree_with(|_, m| {
        before_first_table(
            &with_min_tasty(m, MIN),
            &["review_note = [", r#"["x"]"#, "]"],
        )
    }));
    run_good(&w).assert_promoted("array with a header-like element", false);
    // A multi-line array whose element lines carry comments.
    let w = World::of(tree_with(|_, m| {
        before_first_table(
            &with_min_tasty(m, MIN),
            &[
                "permissions = [",
                r#"  "a", # a comment ]"#,
                r#"  "b",  # ["c"]"#,
                "]",
            ],
        )
    }));
    run_good(&w).assert_promoted("array with comments", false);
    // Check 8 does not judge whether a manifest is TOML: a value next to a line that is
    // not TOML is still read. The check CI of the tag commit parses the manifests with the
    // tasty parser (crates/plugin-pin-check/tests/pin.rs) and fails on it.
    let w = World::of(tree_with(|_, m| {
        before_first_table(&with_min_tasty(m, MIN), &["review_note = not_toml"])
    }));
    run_good(&w).assert_promoted("a line that is not TOML", false);
}

#[test]
fn the_latest_release_is_compared_again_right_before_the_edit() {
    // A newer Release becomes Latest while the checks run: check 5 passed with v1.2.2, the
    // lookup right before the edit sees v1.3.0.
    let mut w = World::good();
    w.latest_recheck = Some(ok("v1.3.0\n"));
    let run = run_good(&w);
    run.assert_refused("newer Latest right before the edit");
    assert!(run.result.output.contains("check 5"), "{:?}", run.result);
    assert_eq!(
        run.calls
            .lines()
            .filter(|l| l.ends_with("releases/latest --jq .tag_name"))
            .count(),
        3,
        "{}",
        run.calls
    );
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
    for what in [
        "no Requires line",
        "settled Requires line",
        "two pending lines",
    ] {
        let mut w = World::good();
        let mut block = w.good_block();
        match what {
            "no Requires line" => block.truncate(2),
            "settled Requires line" => block[2] = "Requires Tasty: 0.14.0 or later".into(),
            _ => block.push(pending(TASTY_REV)),
        }
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
    let name = archive_name(&w.archived[0], "linux-x64");
    w.assets.remove(&name);
    run_good(&w).assert_refused("missing archive");

    let mut w = World::good();
    w.assets.remove("plugins-macos-arm64.tsv");
    run_good(&w).assert_refused("missing TSV list");
}

#[test]
fn the_published_hashes_must_agree() {
    // An archive whose GitHub digest is not its SHA256SUMS hash.
    let mut w = World::good();
    let name = archive_name(&w.archived[1], "windows-arm64");
    w.digests.insert(name, "0".repeat(64));
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
    let name = archive_name(&w.archived[0], "macos-arm64");
    let hash = sha256_all(&w.assets)[&name].clone();
    assert!(lock.contains(&hash));
    w.lock = ok(lock.replacen(&hash, &"e".repeat(64), 1));
    run_good(&w).assert_refused("lock hash");
}

#[test]
fn the_marker_block_and_the_lock_must_match_the_published_release_meta() {
    // release-meta.toml for another commit.
    let mut w = World::good();
    let meta = w.meta_text(TAG, OTHER_COMMIT, TASTY_REV);
    w.assets
        .insert("release-meta.toml".into(), meta.into_bytes());
    run_good(&w).assert_refused("release-meta commit");

    // Only the tasty rev line changed.
    let mut w = World::good();
    let mut block = w.good_block();
    block[1] = format!("tasty rev: {OTHER_REV}");
    w.notes = notes_with(&block);
    run_good(&w).assert_refused("tasty rev line");

    // Only the pending pin changed.
    let mut w = World::good();
    let mut block = w.good_block();
    block[2] = pending(OTHER_REV);
    w.notes = notes_with(&block);
    run_good(&w).assert_refused("pending pin");

    // The lock file was made from another tasty rev.
    let mut w = World::good();
    let commit = w.commit().to_string();
    w.lock = ok(w.lock_text(REPO, TAG, &commit, OTHER_REV));
    run_good(&w).assert_refused("lock tasty_rev");
}

#[test]
fn release_meta_for_another_tag_is_refused() {
    let mut w = World::good();
    let commit = w.commit().to_string();
    let meta = w.meta_text("v1.2.4", &commit, TASTY_REV);
    w.assets
        .insert("release-meta.toml".into(), meta.into_bytes());
    let run = run_good(&w);
    run.assert_refused("release-meta tag");
    assert!(
        run.result
            .output
            .contains("release-meta.toml is not for v1.2.3"),
        "{:?}",
        run.result
    );
}

#[test]
fn a_plugin_left_out_of_every_list_is_still_missing() {
    // The published files, lists, release-meta.toml, and lock agree with each other, but
    // the tag commit packages one plugin more.
    let t = tree();
    let mut published = t.plugins.clone();
    published.remove(0);
    let w = World::publishing(t, published.clone(), published);
    run_good(&w).assert_refused("one plugin left out everywhere");
}

#[test]
fn archives_named_unlike_the_tag_commit_are_refused() {
    // release-meta.toml lists the plugins of the tag commit, but the archives of one plugin
    // carry another name on all five platforms, and the lists, lock, and hashes follow it.
    let t = tree();
    let meta_plugins = t.plugins.clone();
    let mut archived = t.plugins.clone();
    archived[1].0 = format!("{}-renamed", archived[1].0);
    let w = World::publishing(t, archived, meta_plugins);
    run_good(&w).assert_refused("renamed archives");
}

#[test]
fn release_meta_must_list_the_plugins_of_the_tag_commit() {
    // The archives, lists, lock, and hashes are those of the tag commit; only the
    // [[plugin]] entries of release-meta.toml differ.
    for what in [
        "one version differs",
        "one plugin twice",
        "one plugin left out",
    ] {
        let t = tree();
        let archived = t.plugins.clone();
        let mut meta_plugins = t.plugins.clone();
        match what {
            "one version differs" => meta_plugins[1].1 = "9.9.9".into(),
            "one plugin twice" => meta_plugins.push(meta_plugins[0].clone()),
            _ => {
                meta_plugins.remove(1);
            }
        }
        let run = run_good(&World::publishing(t, archived, meta_plugins));
        run.assert_refused(what);
        assert!(
            run.result
                .output
                .contains("check 7: release-meta.toml lists other plugins"),
            "{what}: {:?}",
            run.result
        );
    }
}

/// `manifest` with its first version line, the one an archive name reads, set to
/// `version`.
fn with_version(manifest: &str, version: &str) -> String {
    let mut done = false;
    let out: String = manifest
        .lines()
        .map(|l| {
            let is_version = l
                .strip_prefix("version")
                .is_some_and(|rest| rest.trim_start().starts_with('='));
            if is_version && !done {
                done = true;
                format!("version = \"{version}\"\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    assert!(done, "no version line in\n{manifest}");
    out
}

/// Gives the work tree manifests of `tree` the version `version(i, tag commit version)`,
/// after the tag commit is made: the commit keeps its own versions.
fn change_work_tree_versions(tree: &Tree, version: impl Fn(usize, &str) -> String) {
    for (i, crate_name) in tree.crates.iter().enumerate() {
        let path = tree
            .dir
            .path()
            .join(format!("crates/{crate_name}/tasty-plugin.toml"));
        let text = std::fs::read_to_string(&path).expect("read work tree manifest");
        let changed = with_version(&text, &version(i, &tree.plugins[i].1));
        std::fs::write(&path, changed).expect("write work tree manifest");
    }
}

#[test]
fn check_7_reads_the_manifests_of_the_tag_commit_not_the_work_tree() {
    // The tag commit has the published versions; the work tree has other ones.
    let t = tree();
    change_work_tree_versions(&t, |_, _| "9.9.9".into());
    run_good(&World::of(t)).assert_promoted("tag commit published, work tree other", false);
    // The work tree has the published versions; the tag commit has other ones.
    let t = tree_with(|_, m| with_version(&with_min_tasty(m, MIN), "9.9.9"));
    let real = tree();
    change_work_tree_versions(&t, |i, _| real.plugins[i].1.clone());
    let published = real.plugins.clone();
    let run = run_good(&World::publishing(t, published.clone(), published));
    run.assert_refused("work tree published, tag commit other");
    assert!(run.result.output.contains("check 7: "), "{:?}", run.result);
}

#[test]
fn check_7_runs_the_list_bins_of_the_tag_commit_not_the_work_tree() {
    // The work tree scripts/check.sh lists one plugin crate less than the tag commit.
    let t = tree();
    let fewer: Vec<String> = t.crates[1..].to_vec();
    let script = format!(
        "#!/usr/bin/env bash\n[[ \"$1\" == list-bins ]] || exit 2\nprintf '%s\\n' {}\n",
        fewer.join(" ")
    );
    support::write_executable(&t.dir.path().join("scripts/check.sh"), script.into_bytes())
        .expect("write work tree check.sh");
    run_good(&World::of(t)).assert_promoted("work tree list-bins lists one less", false);
}

#[test]
fn the_good_world_publishes_the_files_of_a_real_release() {
    let w = World::good();
    // Each list-bins crate on each platform, the two lists per platform, release-meta.toml.
    assert_eq!(
        w.assets.len(),
        w.tree.plugins.len() * PLATFORMS.len() + 2 * PLATFORMS.len() + 1
    );
    // As many plugins as a tag run of release.yml checks for.
    let release = read(".github/workflows/release.yml");
    let count: usize = line_value(&release, "  PLUGIN_COUNT: ", "release.yml")
        .trim_matches('"')
        .parse()
        .expect("PLUGIN_COUNT");
    assert_eq!(w.tree.plugins.len(), count, "{:?}", w.tree.plugins);
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
