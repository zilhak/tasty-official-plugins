//! Checks that the plugin crates are the workspace members the build list
//! names, that every tasty crate comes from one pinned tasty commit, that
//! each plugin manifest version matches its crate version, and that each
//! plugin's PLUGIN_ID constant matches its manifest id.
//!
//! The source checks read Cargo.lock, which is what the build uses.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tasty_doc_guards::floored_walk::{CountedOn, Descend, Floor, Walked, walk_with_floor};
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

/// Rust files under crates/*/src: 147 measured on 2026-11-14 at 25f4750.
const CRATES_FLOOR: Floor = Floor {
    min: 110,
    measured: 147,
    measured_on: "2026-11-14",
    counted_on: CountedOn::Tree("25f4750 — Rust files under crates/*/src"),
    why_this_gap: "The floor leaves room for files being merged or split and fails when the \
                   walk loses a large plugin.",
};

/// A crate source as (path relative to the workspace root with `/`, text).
type Source = (String, String);

/// The Rust files under crates/<crate>/src, through the shared walk that skips symbolic
/// links, build caches, and dependency trees.
fn crate_sources() -> Vec<Source> {
    let root = workspace_root();
    walk_with_floor(
        &root.join("crates"),
        &root,
        &CRATES_FLOOR,
        Descend::SkipBuildCaches,
        &|w: &Walked| w.rel.ends_with(".rs") && w.rel.contains("/src/"),
    )
    .unwrap_or_else(|why| panic!("{why}"))
    .iter()
    .map(|w| {
        (
            w.rel.clone(),
            std::fs::read_to_string(&w.path).unwrap_or_default(),
        )
    })
    .collect()
}

/// The value of a `const PLUGIN_ID: ... = "..."` declaration. The colon after the name
/// keeps other constants with a longer name out.
fn plugin_id_in_line(line: &str) -> Option<&str> {
    let (_, rest) = line.trim_start().split_once("const PLUGIN_ID:")?;
    rest.split('"').nth(1)
}

/// Plugins without any constant, and constants whose value differs from the manifest.
/// Counted per plugin, so an extra constant in one plugin does not hide a missing one in
/// another.
struct IdScan {
    blind: Vec<String>,
    mismatches: Vec<String>,
}

/// Takes the sources and the (crate, manifest id) pairs, so synthetic inputs go through
/// the same check.
fn id_scan(sources: &[Source], plugins: &[(String, String)]) -> IdScan {
    let mut blind = Vec::new();
    let mut mismatches = Vec::new();
    for (name, manifest_id) in plugins {
        let mut found_here = 0usize;
        let prefix = format!("crates/{name}/src/");
        for (rel, text) in sources.iter().filter(|(rel, _)| rel.starts_with(&prefix)) {
            for line in text.lines() {
                let Some(value) = plugin_id_in_line(line) else {
                    continue;
                };
                found_here += 1;
                if value != manifest_id {
                    mismatches.push(format!(
                        "  {rel}: manifest id={manifest_id} vs const PLUGIN_ID={value}"
                    ));
                }
            }
        }
        if found_here == 0 {
            blind.push(format!("  {name}"));
        }
    }
    IdScan { blind, mismatches }
}

#[test]
fn manifest_id_matches_the_plugin_id_constant() {
    let sources = crate_sources();
    let crates = workspace_root().join("crates");
    let plugins: Vec<(String, String)> = script_plugins()
        .into_iter()
        .filter(|plugin| crates.join(plugin).join("tasty-plugin.toml").exists())
        .map(|plugin| {
            let manifest =
                Manifest::read(&crates.join(&plugin)).unwrap_or_else(|e| panic!("{plugin}: {e:#}"));
            (plugin, manifest.id)
        })
        .collect();
    assert_eq!(plugins.len(), 8, "expected eight plugin manifests");
    let IdScan { blind, mismatches } = id_scan(&sources, &plugins);
    assert!(
        blind.is_empty(),
        "no PLUGIN_ID constant found for these plugins. Check the constant name and the walk:\n{}",
        blind.join("\n")
    );
    assert!(
        mismatches.is_empty(),
        "the manifest id and the PLUGIN_ID constant differ. Keep the id a plugin sends equal to the one it registers:\n{}",
        mismatches.join("\n")
    );
}

/// Synthetic file names stay inside strings so that they are not taken for real paths.
fn corpus(pairs: &[(&str, &str)]) -> Vec<Source> {
    pairs
        .iter()
        .map(|(rel, src)| ((*rel).to_string(), (*src).to_string()))
        .collect()
}

fn roster(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, id)| ((*name).to_string(), (*id).to_string()))
        .collect()
}

#[test]
fn the_constant_needle_needs_the_colon_and_the_quotes() {
    assert_eq!(
        plugin_id_in_line("    pub const PLUGIN_ID: &str = \"zone.alpha\";"),
        Some("zone.alpha")
    );
    assert_eq!(
        plugin_id_in_line("const PLUGIN_IDENT: &str = \"zone.alpha\";"),
        None,
        "counted another constant as this one"
    );
    assert_eq!(
        plugin_id_in_line("    let sent = PLUGIN_ID;"),
        None,
        "counted a use as the declaration"
    );
    assert_eq!(
        plugin_id_in_line("const PLUGIN_ID: &str = OTHER;"),
        None,
        "made up a value without quotes"
    );
}

#[test]
fn a_plugin_with_no_constant_is_blind_not_matching() {
    let sources = corpus(&[(
        "crates/tasty-plugin-zonealpha/src/lib.rs",
        "pub const PLUGIN_ID: &str = \"zone.alpha\";",
    )]);
    let plugins = roster(&[
        ("tasty-plugin-zonealpha", "zone.alpha"),
        ("tasty-plugin-zonebeta", "zone.beta"),
    ]);

    let scan = id_scan(&sources, &plugins);
    assert_eq!(scan.blind, vec!["  tasty-plugin-zonebeta".to_string()]);
    assert_eq!(
        scan.mismatches,
        Vec::<String>::new(),
        "counted a missing constant as a mismatch too"
    );
}

/// Two constants in one plugin cannot stand in for one missing from another.
#[test]
fn the_blind_check_is_per_plugin_not_a_sum() {
    let sources = corpus(&[
        (
            "crates/tasty-plugin-zonealpha/src/lib.rs",
            "pub const PLUGIN_ID: &str = \"zone.alpha\";",
        ),
        (
            "crates/tasty-plugin-zonealpha/src/main.rs",
            "const PLUGIN_ID: &str = \"zone.alpha\";",
        ),
    ]);
    let plugins = roster(&[
        ("tasty-plugin-zonealpha", "zone.alpha"),
        ("tasty-plugin-zonebeta", "zone.beta"),
    ]);

    assert_eq!(
        id_scan(&sources, &plugins).blind,
        vec!["  tasty-plugin-zonebeta".to_string()]
    );
}

/// Another crate with a longer name that starts the same is not this plugin.
#[test]
fn a_constant_under_another_prefix_does_not_cover_this_plugin() {
    let sources = corpus(&[(
        "crates/tasty-plugin-zonealphax/src/lib.rs",
        "pub const PLUGIN_ID: &str = \"zone.alpha\";",
    )]);
    let plugins = roster(&[("tasty-plugin-zonealpha", "zone.alpha")]);

    assert_eq!(
        id_scan(&sources, &plugins).blind,
        vec!["  tasty-plugin-zonealpha".to_string()],
        "counted the constant of a crate with an overlapping prefix for this plugin"
    );
}

#[test]
fn a_drifted_constant_is_named_with_the_file_that_holds_it() {
    let sources = corpus(&[
        (
            "crates/tasty-plugin-zonealpha/src/lib.rs",
            "pub const PLUGIN_ID: &str = \"zone.alpha\";",
        ),
        (
            "crates/tasty-plugin-zonealpha/src/main.rs",
            "const PLUGIN_ID: &str = \"zone.alpha.old\";",
        ),
    ]);
    let plugins = roster(&[("tasty-plugin-zonealpha", "zone.alpha")]);

    let scan = id_scan(&sources, &plugins);
    assert_eq!(scan.blind, Vec::<String>::new());
    assert_eq!(
        scan.mismatches,
        vec![
            "  crates/tasty-plugin-zonealpha/src/main.rs: manifest id=zone.alpha vs const PLUGIN_ID=zone.alpha.old"
                .to_string()
        ]
    );
}
