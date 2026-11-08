//! Checks that the tasty plugin crates come from one pinned tasty commit and
//! that this repository holds no copy of their sources.
//!
//! The plugin crates' own tests are not run from this repository: Cargo cannot
//! test a git dependency that is not a workspace member, so tasty CI runs them
//! until the sources move here.
//!
//! The checks read Cargo.lock rather than `cargo metadata`, because metadata
//! leaves out the bin-only plugin crates (the "missing a lib target" warning).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tasty_plugin_manifest::Manifest;

const TASTY_GIT: &str = "https://github.com/zilhak/tasty";
const PLUGIN_PREFIX: &str = "tasty-plugin-";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate sits two levels below the workspace root")
        .to_path_buf()
}

fn read_root_file(name: &str) -> String {
    std::fs::read_to_string(workspace_root().join(name))
        .unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// Locked package entry: (version, source).
type LockEntry = (String, String);

/// Cargo.lock packages by name. Each name may appear with several versions.
fn lock_packages() -> BTreeMap<String, Vec<LockEntry>> {
    let lock: toml::Table = read_root_file("Cargo.lock")
        .parse()
        .expect("parse Cargo.lock");
    let mut out: BTreeMap<String, Vec<LockEntry>> = BTreeMap::new();
    for p in lock["package"].as_array().expect("package array") {
        let get = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        out.entry(get("name").to_owned())
            .or_default()
            .push((get("version").to_owned(), get("source").to_owned()));
    }
    out
}

/// The single pinned rev, after checking every tasty entry in Cargo.toml uses it.
fn pinned_rev() -> String {
    let text = read_root_file("Cargo.toml");
    let mut revs = BTreeSet::new();
    for line in text.lines().filter(|l| l.contains(TASTY_GIT)) {
        let rest = line
            .split_once("rev = \"")
            .unwrap_or_else(|| panic!("tasty git dependency without rev: {line}"))
            .1;
        let rev = rest.split_once('"').expect("closing quote of rev").0;
        revs.insert(rev.to_owned());
    }
    assert_eq!(
        revs.len(),
        1,
        "Cargo.toml pins tasty at several revs: {revs:?}"
    );
    let rev = revs.into_iter().next().unwrap_or_default();
    assert_eq!(rev.len(), 40, "pin must be a full commit sha: {rev}");
    rev
}

fn pinned_source() -> String {
    let rev = pinned_rev();
    format!("git+{TASTY_GIT}?rev={rev}#{rev}")
}

/// The plugin list in scripts/check.sh, the single list the build uses.
fn script_plugins() -> BTreeSet<String> {
    let script = read_root_file("scripts/check.sh");
    let start = script.find("\nPLUGINS=(").expect("PLUGINS=( in check.sh");
    let body = &script[start + "\nPLUGINS=(".len()..];
    let end = body.find(')').expect("closing ) of PLUGINS");
    body[..end].split_whitespace().map(str::to_owned).collect()
}

/// The tasty-plugin-* crates in the checker crate's [dependencies].
fn declared_plugins() -> BTreeSet<String> {
    let manifest: toml::Table =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read checker Cargo.toml")
            .parse()
            .expect("parse checker Cargo.toml");
    manifest["dependencies"]
        .as_table()
        .expect("[dependencies]")
        .keys()
        .filter(|n| n.starts_with(PLUGIN_PREFIX))
        .cloned()
        .collect()
}

/// Directory of the pinned tasty checkout, found through a library crate that
/// `cargo metadata` does report.
fn tasty_checkout() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(workspace_root())
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("parse cargo metadata");
    let pkg = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|p| p["name"] == "tasty-plugin-manifest")
        .expect("tasty-plugin-manifest in metadata");
    assert_eq!(pkg["source"].as_str(), Some(pinned_source().as_str()));
    Path::new(pkg["manifest_path"].as_str().expect("manifest_path"))
        .ancestors()
        .nth(3)
        .expect("checkout root above crates/tasty-plugin-manifest/Cargo.toml")
        .to_path_buf()
}

#[test]
fn build_list_matches_declared_dependencies() {
    let declared = declared_plugins();
    assert_eq!(
        declared.len(),
        9,
        "expected nine plugin crates: {declared:?}"
    );
    assert_eq!(script_plugins(), declared);
}

#[test]
fn all_tasty_crates_come_from_one_pinned_commit() {
    let expected = pinned_source();
    let lock = lock_packages();

    for plugin in declared_plugins() {
        let entries = lock
            .get(&plugin)
            .unwrap_or_else(|| panic!("{plugin} missing from Cargo.lock"));
        assert_eq!(entries.len(), 1, "{plugin} locked more than once");
        assert_eq!(entries[0].1, expected, "{plugin}");
    }
    for (name, entries) in &lock {
        for (_, source) in entries {
            let from_tasty = source.contains("github.com/zilhak/tasty");
            if from_tasty || name.starts_with(PLUGIN_PREFIX) {
                assert_eq!(source, &expected, "{name}");
            }
        }
    }
}

#[test]
fn no_plugin_sources_are_copied_here() {
    let crates = workspace_root().join("crates");
    for entry in std::fs::read_dir(&crates).expect("read crates/") {
        let name = entry.expect("dir entry").file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.starts_with(PLUGIN_PREFIX),
            "crates/{name} looks like a copied plugin crate"
        );
    }
    for (name, entries) in lock_packages() {
        if name.starts_with(PLUGIN_PREFIX) {
            assert!(
                entries.iter().all(|(_, source)| !source.is_empty()),
                "{name} is a local package, not a git dependency"
            );
        }
    }
}

#[test]
fn plugin_manifest_versions_match_crate_versions() {
    let checkout = tasty_checkout();
    let lock = lock_packages();
    let mut checked = 0;
    for plugin in declared_plugins() {
        let dir = checkout.join("crates").join(&plugin);
        let cargo_toml: toml::Table = std::fs::read_to_string(dir.join("Cargo.toml"))
            .unwrap_or_else(|e| panic!("{plugin}: read Cargo.toml: {e}"))
            .parse()
            .unwrap_or_else(|e| panic!("{plugin}: parse Cargo.toml: {e}"));
        let crate_version = cargo_toml["package"]["version"]
            .as_str()
            .expect("package.version");
        assert_eq!(
            lock[&plugin][0].0, crate_version,
            "{plugin}: Cargo.lock version differs from the pinned source"
        );
        if !dir.join("tasty-plugin.toml").exists() {
            // Library crates such as tasty-plugin-agent-common have no manifest.
            continue;
        }
        let manifest = Manifest::read(&dir).unwrap_or_else(|e| panic!("{plugin}: {e:#}"));
        manifest
            .validate()
            .unwrap_or_else(|e| panic!("{plugin}: invalid manifest: {e:#}"));
        assert_eq!(
            manifest.version, crate_version,
            "{plugin}: tasty-plugin.toml version differs from Cargo.toml version"
        );
        checked += 1;
    }
    assert_eq!(checked, 8, "expected eight plugin manifests");
}
