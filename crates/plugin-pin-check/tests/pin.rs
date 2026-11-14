//! Checks that the plugin crates are the workspace members the build list
//! names, that every tasty crate comes from one pinned tasty commit, that
//! each plugin manifest version matches its crate version, and that each
//! plugin's PLUGIN_ID constant matches its manifest id, and that no package comes
//! from a path outside the repository or from a path [patch] in the committed
//! manifest and cargo config, which a clean clone cannot build.
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

// ── Path sources ─────────────────────────────────────────────────────────────
// A path [patch] or a paths override names a folder that a clean clone and CI do not have.
// Cargo.lock shows the result as a package without a source; the declarations are read
// from the committed manifest and cargo config. scripts/dev-bundle.sh passes local tasty
// copies with cargo --config in a copy of the repository, so neither check sees them.

/// The packages without a source (path packages) whose manifest is not a file that git
/// tracks inside `root`, as "name version (manifest path)". Packages are told apart by name,
/// version, and path, not by name alone.
fn sourceless_outside(packages: &[Value], root: &Path, tracked: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for p in packages {
        if !p["source"].is_null() {
            continue;
        }
        let manifest = p["manifest_path"].as_str().unwrap_or_default();
        let inside = Path::new(manifest)
            .strip_prefix(root)
            .ok()
            .and_then(|rel| rel.to_str())
            .is_some_and(|rel| tracked.contains(rel));
        if !inside {
            out.push(format!(
                "{} {} ({manifest})",
                p["name"].as_str().unwrap_or_default(),
                p["version"].as_str().unwrap_or_default()
            ));
        }
    }
    out
}

/// The path patches and the paths override of a parsed manifest or cargo config, as
/// "patch.<source>.<name>" and "paths". A git or registry entry is not listed.
fn path_patches(doc: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(sources) = doc.get("patch").and_then(toml::Value::as_table) {
        for (source, entries) in sources {
            for (name, entry) in entries.as_table().into_iter().flatten() {
                if entry.get("path").is_some() {
                    out.push(format!("patch.{source}.{name}"));
                }
            }
        }
    }
    if doc.get("paths").is_some() {
        out.push("paths".to_owned());
    }
    out
}

/// The include entries of a parsed cargo config as (path as written, optional): a string, an
/// array of strings, or an array of { path, optional } tables.
fn config_includes(doc: &toml::Value) -> Result<Vec<(String, bool)>, String> {
    let entry = |v: &toml::Value| match v {
        toml::Value::String(s) => Ok((s.clone(), false)),
        toml::Value::Table(t) => match t.get("path").and_then(toml::Value::as_str) {
            Some(p) => Ok((
                p.to_owned(),
                t.get("optional")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false),
            )),
            None => Err(format!("an include table without a path: {t}")),
        },
        other => Err(format!("an include entry that is not read: {other}")),
    };
    match doc.get("include") {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(items)) => items.iter().map(entry).collect(),
        Some(one) => Ok(vec![entry(one)?]),
    }
}

/// `dir/rel` with "." and ".." resolved, or None when it leaves the root or is absolute.
fn join_inside(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in dir.split('/').chain(rel.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// The repository cargo config files (.cargo/config.toml, .cargo/config, and what they
/// include, each once) as (path, parsed file). `read` gives the text of a repository path.
fn committed_cargo_configs(
    read: impl Fn(&str) -> Option<String>,
) -> Result<Vec<(String, toml::Value)>, String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut todo: Vec<(String, bool)> = vec![
        (".cargo/config".to_owned(), true),
        (".cargo/config.toml".to_owned(), true),
    ];
    while let Some((path, optional)) = todo.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(text) = read(&path) else {
            if optional {
                continue;
            }
            return Err(format!("{path} is included but missing"));
        };
        let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("parse {path}: {e}"))?;
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
        for (inc, opt) in config_includes(&doc).map_err(|e| format!("{path}: {e}"))? {
            if inc.contains(['*', '?', '[']) {
                return Err(format!("{path} includes a glob: {inc}"));
            }
            let target = join_inside(&dir, &inc)
                .ok_or_else(|| format!("{path} includes {inc}, outside the repository"))?;
            todo.push((target, opt));
        }
        out.push((path, doc));
    }
    Ok(out)
}

/// The files that git tracks in the workspace, as paths relative to its root.
fn tracked_files(root: &Path) -> BTreeSet<String> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("run git ls-files");
    assert!(
        out.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn no_path_sources_outside_the_repository() {
    let meta = cargo_metadata();
    let root = PathBuf::from(meta["workspace_root"].as_str().expect("workspace_root"));
    let packages = meta["packages"].as_array().expect("packages");
    let outside = sourceless_outside(packages, &root, &tracked_files(&root));
    assert!(
        outside.is_empty(),
        "path packages outside the repository, from a path [patch] or override that a clean \
         clone does not have; use git with the pinned rev, or scripts/dev-bundle.sh --tasty \
         for a local copy: {outside:#?}"
    );
}

#[test]
fn no_path_patch_in_the_committed_cargo_config() {
    let root = workspace_root();
    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).ok();
    let manifest: toml::Value = toml::from_str(&read_root_file("Cargo.toml")).expect("Cargo.toml");
    let mut found: Vec<String> = path_patches(&manifest)
        .into_iter()
        .filter(|p| p != "paths")
        .map(|p| format!("Cargo.toml: {p}"))
        .collect();
    for (path, doc) in committed_cargo_configs(read).unwrap_or_else(|e| panic!("{e}")) {
        found.extend(
            path_patches(&doc)
                .into_iter()
                .map(|p| format!("{path}: {p}")),
        );
    }
    assert!(
        found.is_empty(),
        "path patches that a clean clone cannot build; use git with the pinned rev, or \
         scripts/dev-bundle.sh --tasty for a local copy: {found:?}"
    );
}

fn package(name: &str, version: &str, source: Option<&str>, manifest: &str) -> Value {
    serde_json::json!({
        "name": name,
        "version": version,
        "source": source,
        "manifest_path": manifest,
    })
}

#[test]
fn sourceless_outside_keeps_tracked_packages_in_the_repository() {
    let root = Path::new("/repo");
    let tracked: BTreeSet<String> = [
        "crates/plugin-guards/Cargo.toml",
        "crates/tasty-plugin-alpha/Cargo.toml",
        "vendor/inside/Cargo.toml",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let packages = [
        package(
            "plugin-guards",
            "0.0.0",
            None,
            "/repo/crates/plugin-guards/Cargo.toml",
        ),
        package(
            "tasty-plugin-alpha",
            "0.1.0",
            None,
            "/repo/crates/tasty-plugin-alpha/Cargo.toml",
        ),
        package("inside", "1.0.0", None, "/repo/vendor/inside/Cargo.toml"),
        package(
            "serde",
            "1.0.0",
            Some("registry+https://github.com/rust-lang/crates.io-index"),
            "/home/x/serde/Cargo.toml",
        ),
        package(
            "tiny_http",
            "0.12.0",
            None,
            "/elsewhere/tasty/vendor/tiny_http/Cargo.toml",
        ),
        package(
            "untracked",
            "0.1.0",
            None,
            "/repo/scratch/untracked/Cargo.toml",
        ),
        // Same names as members, other versions or paths.
        package(
            "tasty-plugin-rogue",
            "0.1.0",
            None,
            "/elsewhere/tasty-plugin-rogue/Cargo.toml",
        ),
        package(
            "plugin-guards",
            "9.9.9",
            None,
            "/elsewhere/plugin-guards/Cargo.toml",
        ),
    ];
    assert_eq!(
        sourceless_outside(&packages, root, &tracked),
        [
            "tiny_http 0.12.0 (/elsewhere/tasty/vendor/tiny_http/Cargo.toml)",
            "untracked 0.1.0 (/repo/scratch/untracked/Cargo.toml)",
            "tasty-plugin-rogue 0.1.0 (/elsewhere/tasty-plugin-rogue/Cargo.toml)",
            "plugin-guards 9.9.9 (/elsewhere/plugin-guards/Cargo.toml)",
        ]
    );
}

#[test]
fn path_patches_are_found_in_every_spelling() {
    let cases = [
        (
            "[patch.crates-io]\na = { path = \"x\" }\n",
            "patch.crates-io.a",
        ),
        ("[patch.crates-io]\na.path = \"x\"\n", "patch.crates-io.a"),
        ("[patch.crates-io.a]\npath = \"x\"\n", "patch.crates-io.a"),
        (
            "[patch.\"https://github.com/zilhak/tasty\"]\nb = { path = \"y\" }\n",
            "patch.https://github.com/zilhak/tasty.b",
        ),
        (
            "[\"patch\".\"crates-io\"]\nc = { path = \"z\" }\n",
            "patch.crates-io.c",
        ),
        ("[patch]\ncrates-io.d.path = \"w\"\n", "patch.crates-io.d"),
        (
            "[patch.crates-io]\nf = { path = \"x\", version = \"1\" }\n",
            "patch.crates-io.f",
        ),
        ("paths = [\"z\"]\n", "paths"),
    ];
    for (text, want) in cases {
        let doc: toml::Value = toml::from_str(text).expect(text);
        assert_eq!(path_patches(&doc), [want], "{text}");
    }
    let git: toml::Value = toml::from_str(
        "[patch.crates-io]\na = { git = \"https://github.com/zilhak/tasty\", rev = \"abc\" }\n",
    )
    .expect("git patch");
    assert!(path_patches(&git).is_empty());
}

#[test]
fn config_includes_are_followed_once_and_optional_ones_may_be_missing() {
    let files: BTreeMap<&str, &str> = [
        (".cargo/config.toml", "include = \"a.toml\"\n"),
        (".cargo/a.toml", "include = [\"b.toml\", \"../c.toml\"]\n"),
        (
            ".cargo/b.toml",
            "include = [{ path = \"config.toml\" }, { path = \"gone.toml\", optional = true }]\n",
        ),
        ("c.toml", "[patch.crates-io]\ne = { path = \"v\" }\n"),
    ]
    .into_iter()
    .collect();
    let read = |p: &str| files.get(p).map(|s| (*s).to_owned());
    let configs = committed_cargo_configs(read).expect("follow the includes");
    let paths: BTreeSet<&str> = configs.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            ".cargo/a.toml",
            ".cargo/b.toml",
            ".cargo/config.toml",
            "c.toml"
        ]
        .into_iter()
        .collect()
    );
    let found: Vec<String> = configs
        .iter()
        .flat_map(|(p, d)| {
            path_patches(d)
                .into_iter()
                .map(move |x| format!("{p}: {x}"))
        })
        .collect();
    assert_eq!(found, ["c.toml: patch.crates-io.e"]);

    let missing =
        |p: &str| (p == ".cargo/config.toml").then(|| "include = \"gone.toml\"\n".to_owned());
    assert!(committed_cargo_configs(missing).is_err());
    let outside =
        |p: &str| (p == ".cargo/config.toml").then(|| "include = \"../../x.toml\"\n".to_owned());
    assert!(committed_cargo_configs(outside).is_err());
    let glob = |p: &str| (p == ".cargo/config.toml").then(|| "include = [\"*.toml\"]\n".to_owned());
    assert!(committed_cargo_configs(glob).is_err());
}
