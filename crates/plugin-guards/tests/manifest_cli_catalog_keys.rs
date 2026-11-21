//! The CLI text of a plugin manifest, its translation keys, and the shipped catalogs move
//! together. tasty ran this check on the plugin copies before they moved here.
//!
//! Four fields are read from each plugin manifest: the description of a top-level CLI
//! command, the description of a subcommand, and the help of a positional argument and
//! of a flag. For every such text that is not empty:
//!
//! - `no key`: the matching `description_i18n_key` or `help_i18n_key` is missing.
//! - `differs`: the English catalog value is not the manifest text. A key missing from
//!   the English catalog is reported here as `missing in en`.
//! - `blank`: the catalog loaded for a built-in language has no value or a blank one.
//!
//! The catalogs are loaded with tasty's own plugin catalog loader, which lays English
//! first and keeps the English value where a translation is missing or blank. The
//! manifest is only deserialized here; validation is checked elsewhere. tasty also
//! checked `help_group` against its host list, which stays in tasty.

use std::collections::HashMap;
use std::path::Path;

use plugin_guards::{MANIFEST_NAME, plugin_crates};
use tasty_i18n::BUILTIN_CODES;

/// Plugin manifests: 8 measured on 2026-11-21 at fd948f3.
const MIN_MANIFESTS: usize = 7;

/// Texts per field, measured on 2026-11-21 at fd948f3: top-level 6, subcommand 72,
/// positional 14, flag 192 (284 in all). Missing one field entirely fails its floor.
const MIN_TOP: usize = 5;
const MIN_SUB: usize = 60;
const MIN_POSITIONAL: usize = 10;
const MIN_FLAG: usize = 150;

#[derive(Default, Debug, PartialEq)]
struct Counts {
    top: usize,
    sub: usize,
    positional: usize,
    flag: usize,
}

/// The diagnostics of one text, or none when it is empty.
fn check_text(
    id: &str,
    catalogs: &[(&str, HashMap<String, String>)],
    field: &str,
    text: &Option<String>,
    key: &Option<String>,
    out: &mut Vec<String>,
) -> bool {
    let Some(text) = text.as_ref().filter(|t| !t.is_empty()) else {
        return false;
    };
    let Some(key) = key else {
        out.push(format!("  {id} {field}: no key"));
        return true;
    };
    let en = &catalogs[0].1;
    match en.get(key) {
        None => {
            out.push(format!("  {id} {field} ({key}): differs: missing in en"));
            return true;
        }
        Some(value) if value != text => {
            out.push(format!("  {id} {field} ({key}): differs from en {value:?}"));
        }
        Some(_) => {}
    }
    for (code, catalog) in catalogs {
        if catalog.get(key).is_none_or(|s| s.trim().is_empty()) {
            out.push(format!("  {id} {field} ({key}): blank in {code}"));
        }
    }
    true
}

/// The text counts and diagnostics of the plugin manifest in `dir`.
fn check_manifest(dir: &Path) -> (Counts, Vec<String>) {
    let path = dir.join(MANIFEST_NAME);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace("\r\n", "\n");
    let manifest: tasty_plugin_manifest::Manifest =
        toml::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()));
    let lang_dir = dir.join(&manifest.lang_dir);
    // BUILTIN_CODES starts with en.
    let catalogs: Vec<(&str, HashMap<String, String>)> = BUILTIN_CODES
        .iter()
        .map(|code| {
            (
                *code,
                tasty_i18n::plugin_catalog::load(&lang_dir, code, &manifest.id, None),
            )
        })
        .collect();
    let id = manifest.id.as_str();
    let mut counts = Counts::default();
    let mut out = Vec::new();
    for cli in &manifest.contributes.cli {
        let at = format!("cli[{}]", cli.name);
        let field = format!("{at}.description");
        counts.top += usize::from(check_text(
            id,
            &catalogs,
            &field,
            &cli.description,
            &cli.description_i18n_key,
            &mut out,
        ));
        for sub in &cli.subcommands {
            let field = format!("{at}.subcommands[{}].description", sub.name);
            counts.sub += usize::from(check_text(
                id,
                &catalogs,
                &field,
                &sub.description,
                &sub.description_i18n_key,
                &mut out,
            ));
        }
        let mut groups: Vec<_> = cli.arg_groups.iter().collect();
        groups.sort_by(|a, b| a.0.cmp(b.0));
        for (group, args) in groups {
            for arg in &args.positional {
                let field = format!("{at}.arg_groups[{group}].positional[{}].help", arg.name);
                counts.positional += usize::from(check_text(
                    id,
                    &catalogs,
                    &field,
                    &arg.help,
                    &arg.help_i18n_key,
                    &mut out,
                ));
            }
            for arg in &args.flags {
                let field = format!("{at}.arg_groups[{group}].flags[{}].help", arg.name);
                counts.flag += usize::from(check_text(
                    id,
                    &catalogs,
                    &field,
                    &arg.help,
                    &arg.help_i18n_key,
                    &mut out,
                ));
            }
        }
    }
    (counts, out)
}

#[test]
fn every_plugin_cli_text_has_its_catalog_key() {
    let plugins = plugin_crates();
    assert!(
        plugins.len() >= MIN_MANIFESTS,
        "found {} plugin manifests (floor {MIN_MANIFESTS})",
        plugins.len()
    );
    let mut total = Counts::default();
    let mut found = Vec::new();
    for (_, dir) in &plugins {
        let (counts, diagnostics) = check_manifest(dir);
        total.top += counts.top;
        total.sub += counts.sub;
        total.positional += counts.positional;
        total.flag += counts.flag;
        found.extend(diagnostics);
    }
    assert!(
        total.top >= MIN_TOP
            && total.sub >= MIN_SUB
            && total.positional >= MIN_POSITIONAL
            && total.flag >= MIN_FLAG,
        "read {total:?} CLI texts (floors top {MIN_TOP}, sub {MIN_SUB}, positional \
         {MIN_POSITIONAL}, flag {MIN_FLAG}). Check the manifest reader. If the CLI really \
         shrank, update the floor with the new measurement."
    );
    assert!(
        found.is_empty(),
        "plugin CLI texts out of step with their catalogs. Give each text a key, keep the en \
         value equal to the manifest text, and keep every language non-blank:\n{}",
        found.join("\n")
    );
}

/// The manifest head and an empty CLI of a synthetic plugin.
const HEAD: &str = "manifest_version = 1
id = \"com.example.x\"
name = \"X\"
version = \"0.0.0\"
api_version = \"1\"

[entry]
type = \"process\"
command = \"x\"

";

/// One CLI command with each field once, keys as given.
fn cli(top: &str, sub: &str, pos: &str, flag: &str) -> String {
    format!(
        "[[contributes.cli]]
name = \"x\"
description = \"Top\"
{top}
subcommands = [
    {{ name = \"a\", ipc_method = \"x.a\", args = \"g\", description = \"Sub\", {sub} }},
]

[contributes.cli.arg_groups.g]
positional = [{{ name = \"p\", type = \"string\", help = \"Pos\", {pos} }}]
flags = [{{ name = \"f\", type = \"bool\", flag = \"--f\", help = \"Flag\", {flag} }}]
"
    )
}

const KEYS: [&str; 4] = [
    "description_i18n_key = \"x.top\"",
    "description_i18n_key = \"x.sub\"",
    "help_i18n_key = \"x.pos\"",
    "help_i18n_key = \"x.flag\"",
];

const EN: &str = "[x]\ntop = \"Top\"\nsub = \"Sub\"\npos = \"Pos\"\nflag = \"Flag\"\n";

/// Diagnostics of a synthetic plugin with this manifest body and these catalogs.
fn diagnostics_of(body: &str, langs: &[(&str, &str)]) -> Vec<String> {
    let dir = tasty_doc_guards::temp_scratch::Scratch::new("manifest-cli-catalog-keys");
    let root = dir.path();
    std::fs::write(root.join(MANIFEST_NAME), format!("{HEAD}{body}")).expect("write manifest");
    std::fs::create_dir_all(root.join("lang")).expect("mkdir lang");
    for (code, text) in langs {
        std::fs::write(root.join("lang").join(format!("{code}.toml")), text).expect("write lang");
    }
    check_manifest(root).1
}

#[test]
fn a_text_without_a_key_is_reported_in_each_field() {
    let fields = [
        "cli[x].description",
        "cli[x].subcommands[a].description",
        "cli[x].arg_groups[g].positional[p].help",
        "cli[x].arg_groups[g].flags[f].help",
    ];
    for (i, field) in fields.iter().enumerate() {
        let mut keys = KEYS;
        keys[i] = "";
        let found = diagnostics_of(&cli(keys[0], keys[1], keys[2], keys[3]), &[("en", EN)]);
        assert_eq!(
            found,
            [format!("  com.example.x {field}: no key")],
            "{field}"
        );
    }
}

#[test]
fn an_en_value_that_differs_or_is_missing_is_reported_apart() {
    let body = cli(KEYS[0], KEYS[1], KEYS[2], KEYS[3]);
    let found = diagnostics_of(&body, &[("en", &EN.replace("\"Sub\"", "\"Other\""))]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("differs from en"), "{found:?}");
    assert!(!found[0].contains("no key") && !found[0].contains("blank"));

    let found = diagnostics_of(&body, &[("en", &EN.replace("sub = \"Sub\"\n", ""))]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("differs: missing in en"), "{found:?}");
    assert!(!found[0].contains("no key") && !found[0].contains("blank"));
}

/// A text of three spaces passes the empty check and fails the blank check, even with
/// an equal en value.
#[test]
fn a_blank_text_with_an_equal_en_value_is_blank() {
    let body = cli(KEYS[0], KEYS[1], KEYS[2], KEYS[3]).replace("\"Sub\"", "\"   \"");
    let found = diagnostics_of(&body, &[("en", &EN.replace("\"Sub\"", "\"   \""))]);
    assert!(!found.is_empty(), "a blank text passed");
    assert!(
        found
            .iter()
            .all(|d| d.contains("(x.sub): blank in") && !d.contains("differs")),
        "{found:?}"
    );
    assert!(
        found.iter().any(|d| d.ends_with("blank in en")),
        "{found:?}"
    );
}

/// A translation that drops the plural form of the key leaves that language without it,
/// while en still matches. This shows the non-English catalogs are really checked.
#[test]
fn a_plural_form_dropped_by_a_translation_is_blank_in_that_language() {
    let body = cli(
        "description_i18n_key = \"x.cli_one\"",
        KEYS[1],
        KEYS[2],
        KEYS[3],
    );
    let en = format!("{EN}cli = \"Tops\"\ncli_one = \"Top\"\n");
    let found = diagnostics_of(&body, &[("en", &en), ("ko", "[x]\ncli = \"Ko\"\n")]);
    assert_eq!(
        found,
        ["  com.example.x cli[x].description (x.cli_one): blank in ko"],
    );
}

/// A translation that lacks a key falls back to en and passes.
#[test]
fn a_missing_translation_falls_back_to_en() {
    let body = cli(KEYS[0], KEYS[1], KEYS[2], KEYS[3]);
    let found = diagnostics_of(&body, &[("en", EN), ("ko", "[x]\ntop = \"Ko\"\n")]);
    assert!(found.is_empty(), "{found:?}");
}
