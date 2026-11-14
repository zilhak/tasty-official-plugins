//! Checks that the plugin crates are the workspace members the build list
//! names, that every tasty crate comes from one pinned tasty commit, and that
//! each plugin manifest version matches its crate version.
//!
//! The source checks read Cargo.lock, which is what the build uses.

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
    parse_lock_packages(&read_root_file("Cargo.lock"), "Cargo.lock")
}

/// Packages by name from the text of a Cargo.lock; `what` names it in panics.
fn parse_lock_packages(text: &str, what: &str) -> BTreeMap<String, Vec<LockEntry>> {
    let lock: toml::Table = text.parse().unwrap_or_else(|e| panic!("parse {what}: {e}"));
    let mut out: BTreeMap<String, Vec<LockEntry>> = BTreeMap::new();
    let packages = lock["package"]
        .as_array()
        .unwrap_or_else(|| panic!("package array in {what}"));
    for p in packages {
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

/// The library-only crates in scripts/check.sh (LIBS).
fn script_libs() -> BTreeSet<String> {
    let script = read_root_file("scripts/check.sh");
    let start = script.find("\nLIBS=(").expect("LIBS=( in check.sh");
    let body = &script[start + "\nLIBS=(".len()..];
    let end = body.find(')').expect("closing ) of LIBS");
    body[..end].split_whitespace().map(str::to_owned).collect()
}

fn cargo_metadata() -> Value {
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
    serde_json::from_slice(&out.stdout).expect("parse cargo metadata")
}

/// The tasty-plugin-* workspace members, each with whether it builds a binary.
fn member_plugins() -> BTreeMap<String, bool> {
    let meta = cargo_metadata();
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .expect("workspace_members")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter(|p| members.contains(p["id"].as_str().unwrap_or_default()))
        .filter_map(|p| {
            let name = p["name"].as_str()?;
            if !name.starts_with(PLUGIN_PREFIX) {
                return None;
            }
            let has_bin = p["targets"].as_array()?.iter().any(|t| {
                t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k == "bin"))
            });
            Some((name.to_owned(), has_bin))
        })
        .collect()
}

/// Directory of the pinned tasty checkout, found through tasty-plugin-manifest.
fn tasty_checkout() -> PathBuf {
    let meta = cargo_metadata();
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

/// Cargo.lock packages of the pinned tasty checkout.
fn tasty_lock_packages() -> BTreeMap<String, Vec<LockEntry>> {
    let path = tasty_checkout().join("Cargo.lock");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    parse_lock_packages(&text, "the tasty Cargo.lock")
}

/// The locked versions of one crate, or an empty set when it is not locked.
fn locked_versions(lock: &BTreeMap<String, Vec<LockEntry>>, name: &str) -> BTreeSet<String> {
    lock.get(name)
        .into_iter()
        .flatten()
        .map(|(version, _)| version.clone())
        .collect()
}

fn show_versions(versions: &BTreeSet<String>) -> String {
    if versions.is_empty() {
        "(none)".to_owned()
    } else {
        versions.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

#[test]
fn build_list_matches_workspace_members() {
    let members = member_plugins();
    assert_eq!(members.len(), 9, "expected nine plugin crates: {members:?}");
    let names: BTreeSet<String> = members.keys().cloned().collect();
    assert_eq!(script_plugins(), names, "PLUGINS in scripts/check.sh");
    let libs: BTreeSet<String> = members
        .iter()
        .filter(|(_, has_bin)| !**has_bin)
        .map(|(name, _)| name.clone())
        .collect();
    assert_eq!(script_libs(), libs, "LIBS in scripts/check.sh");
}

#[test]
fn all_tasty_crates_come_from_one_pinned_commit() {
    let expected = pinned_source();
    let lock = lock_packages();

    let plugins = script_plugins();
    for plugin in &plugins {
        let entries = lock
            .get(plugin)
            .unwrap_or_else(|| panic!("{plugin} missing from Cargo.lock"));
        assert_eq!(entries.len(), 1, "{plugin} locked more than once");
        assert_eq!(entries[0].1, "", "{plugin} must be a local workspace crate");
    }
    for (name, entries) in &lock {
        for (_, source) in entries {
            if source.contains("github.com/zilhak/tasty") {
                assert_eq!(source, &expected, "{name}");
            } else if name.starts_with(PLUGIN_PREFIX) {
                assert!(
                    plugins.contains(name),
                    "{name} comes from {source:?}, not from this repository or the pin"
                );
            }
        }
    }
}

#[test]
fn epaint_comes_from_the_patched_copy_at_the_pin() {
    let lock = lock_packages();
    let entries = lock.get("epaint").expect("epaint in Cargo.lock");
    assert_eq!(entries.len(), 1, "epaint locked more than once");
    assert_eq!(
        entries[0].1,
        pinned_source(),
        "epaint must come from the pin"
    );

    // Cargo only warns about a [patch] that matches nothing, and records it here.
    let table: toml::Table = read_root_file("Cargo.lock")
        .parse()
        .expect("parse Cargo.lock");
    assert!(
        !table.contains_key("patch"),
        "Cargo.lock records an unused [patch]: {:?}",
        table.get("patch")
    );

    let atlas = tasty_checkout().join("vendor/epaint/src/texture_atlas.rs");
    let source =
        std::fs::read_to_string(&atlas).unwrap_or_else(|e| panic!("read {}: {e}", atlas.display()));
    assert!(
        source
            .contains("while required_height >= image.height() && image.height() < image.width()"),
        "the atlas height limit is gone from the copy"
    );
    assert!(
        source.contains("pub fn can_always_allocate"),
        "the atlas size check is gone from the copy"
    );
}

#[test]
fn plugin_manifest_versions_match_crate_versions() {
    let crates = workspace_root().join("crates");
    let lock = lock_packages();
    let mut checked = 0;
    for plugin in script_plugins() {
        let dir = crates.join(&plugin);
        let cargo_toml: toml::Table = std::fs::read_to_string(dir.join("Cargo.toml"))
            .unwrap_or_else(|e| panic!("{plugin}: read Cargo.toml: {e}"))
            .parse()
            .unwrap_or_else(|e| panic!("{plugin}: parse Cargo.toml: {e}"));
        let crate_version = cargo_toml["package"]["version"]
            .as_str()
            .expect("package.version");
        assert_eq!(
            lock[&plugin][0].0, crate_version,
            "{plugin}: Cargo.lock version differs from Cargo.toml"
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

/// Crates whose types the egui-mesh wire carries as raw bytes between tasty and a
/// plugin. The wire checks only its header and sizes, so both sides must lock the
/// same versions.
const EGUI_FAMILY: [&str; 5] = ["egui", "epaint", "emath", "ecolor", "bytemuck"];

/// Crates of the family that every plugin build must lock.
const EGUI_REQUIRED: [&str; 2] = ["egui", "epaint"];

#[test]
fn egui_family_matches_the_tasty_lock_at_the_pin() {
    let ours = lock_packages();
    let tasty = tasty_lock_packages();
    for name in EGUI_REQUIRED {
        assert!(ours.contains_key(name), "{name} missing from Cargo.lock");
    }
    for name in EGUI_FAMILY {
        let our_versions = locked_versions(&ours, name);
        if our_versions.is_empty() {
            continue;
        }
        let tasty_versions = locked_versions(&tasty, name);
        assert!(
            our_versions == tasty_versions,
            "{name} {} != tasty lock {}",
            show_versions(&our_versions),
            show_versions(&tasty_versions)
        );
    }
}
