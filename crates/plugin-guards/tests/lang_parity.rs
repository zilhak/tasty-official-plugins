//! Compares the keys, placeholders, and same-as-English values of each plugin's
//! translation catalog (`crates/<plugin>/lang/<code>.toml`) with English. It also checks
//! that the translation keys written in the plugin sources exist and that every catalog key
//! has a use. Flattening uses the real loader function of tasty-i18n.
//!
//! Translation keys live inside strings, so literals are not masked. The source scan reads
//! lines and tokens and does not prove a real call or catch every dynamic key. Whether to
//! remove a key without a use is decided by also checking the real UI.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tasty_doc_guards::floored_walk::normalized_rel;

const LANGS: &[&str] = &tasty_i18n::BUILTIN_CODES;

/// Keys, languages, and reasons for values that must equal English. Values accepted by
/// their form (symbols, short upper-case abbreviations) are not listed.
const SAME_AS_ENGLISH_ALLOWLIST: &[(&str, &[&str], &str)] = &[
    (
        "image.state.too_large_size",
        &["ko", "ja", "zh-Hans"],
        "number slots and unit signs (× px)",
    ),
    (
        "settings.subtab.claude",
        &["ko", "ja", "zh-Hans"],
        "product name (Claude Code)",
    ),
    (
        "settings.subtab.codex",
        &["ko", "ja", "zh-Hans"],
        "product name (Codex)",
    ),
    (
        "settings.subtab.markdown",
        &["ko", "ja", "zh-Hans"],
        "format name",
    ),
    (
        "git_viewer.tools_menu_item",
        &["ko", "ja", "zh-Hans"],
        "tool name (Git)",
    ),
    (
        "git_viewer.heading",
        &["ko", "ja", "zh-Hans"],
        "tool name (Git)",
    ),
    (
        "git_viewer.diff_heading",
        &["ko", "ja"],
        "the usual developer tool word (Diff)",
    ),
    (
        "git_viewer.detached",
        &["ko", "ja"],
        "git HEAD state token; must be the word the command prints",
    ),
    (
        "claude.profile.error_prefix",
        &["ko", "ja"],
        "error prefix identifier (profile:)",
    ),
    (
        "claude.gate.error_prefix",
        &["ko", "ja", "zh-Hans"],
        "error prefix identifier (gate:)",
    ),
    (
        "surface.kind.markdown",
        &["zh-Hans"],
        "the zh-Hans glossary keeps Markdown in Latin letters",
    ),
    (
        "file_handler.format.markdown",
        &["zh-Hans"],
        "the zh-Hans glossary keeps Markdown in Latin letters",
    ),
];

const MODIFIER_TOKENS: &[&str] = &[
    "Ctrl", "Alt", "Shift", "Cmd", "Command", "Option", "Super", "Meta", "Win", "Fn",
];

/// Keys whose translation has not arrived. The English text was added or changed after the
/// translation base, so the key is left out of that language file and the runtime falls back
/// to English. Remove an entry when its translation arrives; an entry left behind fails the
/// check. Do not copy the English value in, or untranslated keys can no longer be found.
const PENDING_TRANSLATION_KEYS: &[(&str, &str, &str)] = &[
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.notify.quiet_released_message",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.notify.quiet_held_suffix",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.notify.quiet_item",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.notify.quiet_more",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.notify.quiet_scope_task",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.notify.quiet_scope_surface",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.quiet.invalid_secs",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.quiet.secs_or_clear",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.quiet.not_tracked",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.cli.sub.quiet.description",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.cli.sub.notify-quiet-release.description",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.cli.arg.quiet_args.surface.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.cli.arg.quiet_args.task.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.cli.arg.quiet_args.secs.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ko",
        "claude.cli.arg.quiet_args.clear.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.notify.quiet_released_message",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.notify.quiet_held_suffix",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.notify.quiet_item",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.notify.quiet_more",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.notify.quiet_scope_task",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.notify.quiet_scope_surface",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.quiet.invalid_secs",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.quiet.secs_or_clear",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.quiet.not_tracked",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.cli.sub.quiet.description",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.cli.sub.notify-quiet-release.description",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.cli.arg.quiet_args.surface.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.cli.arg.quiet_args.task.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.cli.arg.quiet_args.secs.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "ja",
        "claude.cli.arg.quiet_args.clear.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.notify.quiet_released_message",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.notify.quiet_held_suffix",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.notify.quiet_item",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.notify.quiet_more",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.notify.quiet_scope_task",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.notify.quiet_scope_surface",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.quiet.invalid_secs",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.quiet.secs_or_clear",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.quiet.not_tracked",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.cli.sub.quiet.description",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.cli.sub.notify-quiet-release.description",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.cli.arg.quiet_args.surface.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.cli.arg.quiet_args.task.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.cli.arg.quiet_args.secs.help",
    ),
    (
        "crates/tasty-plugin-claude/lang",
        "zh-Hans",
        "claude.cli.arg.quiet_args.clear.help",
    ),
];

/// Temporary exceptions for keys known to be missing. Remove an entry when its key is added.
/// A new missing key is fixed, not listed.
const PENDING_FIX_MISSING_KEYS: &[(&str, &str)] = &[];

const PRUNE_DIRS: &[&str] = &["target", "dist", ".worktree", ".git", "node_modules"];

/// Local folder names left out of the walk, put together from pieces.
const LOCAL_HEAD: &str = "claude";
const LOCAL_TAIL: &str = "-workspace";

fn is_pruned(name: &str) -> bool {
    PRUNE_DIRS.contains(&name)
        || name
            .strip_prefix('.')
            .is_some_and(|rest| rest == LOCAL_HEAD || rest == format!("{LOCAL_HEAD}{LOCAL_TAIL}"))
}

fn root() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(plugin_guards::repo_root)
}

fn rel_of(file: &Path) -> String {
    normalized_rel(file, root())
}

/// The other checks treat LANGS[1..] as the languages other than English.
#[test]
fn builtin_codes_start_with_english() {
    assert_eq!(
        tasty_i18n::BUILTIN_CODES.first(),
        Some(&"en"),
        "this check treats LANGS[1..] as the languages other than English, so the first entry of BUILTIN_CODES must be en."
    );
}

/// The plugin catalogs: `crates/tasty-plugin-*/lang` folders that hold an en.toml.
fn lang_dirs() -> Vec<PathBuf> {
    let crates = root().join("crates");
    let entries =
        std::fs::read_dir(&crates).unwrap_or_else(|e| panic!("read_dir {}: {e}", crates.display()));
    let mut plugin_dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|dir| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(plugin_guards::PLUGIN_CRATE_PREFIX))
        })
        .map(|dir| dir.join("lang"))
        .filter(|lang| lang.join("en.toml").is_file())
        .collect();
    plugin_dirs.sort();
    plugin_dirs
}

/// Flattening is shared with the real loader, but a parse failure is reported at once, so
/// that two empty catalogs do not pass for being equal.
fn load(dir: &Path, lang: &str) -> BTreeMap<String, String> {
    let path = dir.join(format!("{lang}.toml"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let value: toml::Value = text
        .parse()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let mut flat = std::collections::HashMap::new();
    tasty_i18n::flatten_catalog_toml("", &value, &mut flat);
    flat.into_iter().collect()
}

struct Catalog {
    rel: String,
    by_lang: BTreeMap<&'static str, BTreeMap<String, String>>,
}

fn catalogs() -> Vec<Catalog> {
    lang_dirs()
        .into_iter()
        .map(|dir| Catalog {
            rel: rel_of(&dir),
            by_lang: LANGS.iter().map(|lang| (*lang, load(&dir, lang))).collect(),
        })
        .collect()
}

/// Plugin catalogs: 8 measured on 2026-11-14 at 50b6411. The floor allows one plugin to be
/// removed or merged.
const MIN_CATALOGS: usize = 7;

/// Empty catalogs could pass the key set comparison, so the number of catalogs and of keys
/// is checked too, which also finds a single plugin going empty. Before changing the floor,
/// check whether translations were really removed or the collection or flattening broke.
#[test]
fn every_catalog_carries_more_than_a_handful_of_keys() {
    let cats = catalogs();
    assert!(
        cats.len() >= MIN_CATALOGS,
        "found only {} catalogs (floor {MIN_CATALOGS}). Check what lang_dirs collects.",
        cats.len()
    );
    for cat in &cats {
        for lang in LANGS {
            let n = cat.by_lang[lang].len();
            assert!(
                n > 0,
                "found no key in {}/{lang}.toml. Check the file and the flattening; empty languages can pass the key set comparison.",
                cat.rel
            );
        }
    }
}

/// The three placeholder forms of a string: the count of `{}` filled in order, the set of
/// numbered `{N}`, and the set of named `{name}`. A name must look like an identifier.
fn placeholder_forms(value: &str) -> (usize, BTreeSet<usize>, BTreeSet<String>) {
    let mut positional = 0;
    let mut numbered = BTreeSet::new();
    let mut named = BTreeSet::new();
    let mut rest = value;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        let inner = &after[..close];
        if inner.is_empty() {
            positional += 1;
        } else if inner.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = inner.parse() {
                numbered.insert(n);
            }
        } else if is_identifier(inner) {
            named.insert(inner.to_string());
        }
        rest = &after[close + 1..];
    }
    (positional, numbered, named)
}

/// The argument slots and the name set of a value. n `{}` take slots 0..n; numbered
/// placeholders take their numbers. So en `{} / {}` and ko `{1} / {0}` take the same slots.
fn placeholders(value: &str) -> (BTreeSet<usize>, BTreeSet<String>) {
    let (positional, numbered, named) = placeholder_forms(value);
    let slots = if numbered.is_empty() {
        (0..positional).collect()
    } else {
        numbered
    };
    (slots, named)
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn without_placeholders(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..open]);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

fn is_language_neutral(value: &str) -> bool {
    let body = without_placeholders(value);
    let letters: Vec<char> = body.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return true; // `—` / `99+` / `{} · {}` / `%`
    }
    let trimmed = body.trim();
    if trimmed.contains("://")
        || trimmed.starts_with("~/")
        || trimmed.starts_with('/')
        || trimmed.starts_with("tasty ")
        || trimmed.contains('$')
    {
        return true; // path, URL, shell command
    }
    if letters.len() <= 5 && letters.iter().all(|c| c.is_ascii_uppercase()) {
        return true;
    }
    MODIFIER_TOKENS.contains(&trimmed)
}

fn allowlisted_same(key: &str, lang: &str) -> bool {
    SAME_AS_ENGLISH_ALLOWLIST
        .iter()
        .any(|(k, langs, _)| *k == key && langs.contains(&lang))
}

fn pending_translation(rel: &str, lang: &str, key: &str) -> bool {
    PENDING_TRANSLATION_KEYS
        .iter()
        .any(|(r, l, k)| *r == rel && *l == lang && *k == key)
}

#[test]
fn key_sets_match_english() {
    let mut problems = Vec::new();
    let cats = catalogs();
    for (rel, lang, key) in PENDING_TRANSLATION_KEYS {
        if !cats.iter().any(|cat| cat.rel == *rel) || !LANGS.contains(lang) {
            problems.push(format!(
                "  PENDING_TRANSLATION_KEYS names `{key}` in {rel}/{lang}.toml, which is not a catalog"
            ));
        }
    }
    for cat in cats {
        let en: BTreeSet<&String> = cat.by_lang["en"].keys().collect();
        for lang in &LANGS[1..] {
            let other: BTreeSet<&String> = cat.by_lang[lang].keys().collect();
            // An en singular variant (`<key>_one`) need not be in a language that needs one
            // string only.
            for key in en
                .difference(&other)
                .filter(|k| one_variant_base(k, &cat.by_lang["en"]).is_none())
            {
                if pending_translation(&cat.rel, lang, key) {
                    continue;
                }
                problems.push(format!("  {}/{lang}.toml: missing `{key}`", cat.rel));
            }
            for (_, _, key) in PENDING_TRANSLATION_KEYS
                .iter()
                .filter(|(rel, l, _)| *rel == cat.rel && l == lang)
            {
                if !cat.by_lang["en"].contains_key(*key) {
                    problems.push(format!(
                        "  {}/{lang}.toml: PENDING_TRANSLATION_KEYS names `{key}`, which en does not have",
                        cat.rel
                    ));
                } else if cat.by_lang[lang].contains_key(*key) {
                    problems.push(format!(
                        "  {}/{lang}.toml: `{key}` is translated now — remove it from PENDING_TRANSLATION_KEYS",
                        cat.rel
                    ));
                }
            }
            for key in other.difference(&en) {
                problems.push(format!(
                    "  {}/{lang}.toml: extra `{key}` (not in en)",
                    cat.rel
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "translation catalogs diverge from en (add every key to every language file):\n{}",
        problems.join("\n")
    );
}

/// The base key when `key` is an en singular variant. A key ending in `_one` whose base is
/// not in the catalog is not a variant.
fn one_variant_base<'a>(key: &'a str, en: &BTreeMap<String, String>) -> Option<&'a str> {
    key.strip_suffix(tasty_i18n::plural::ONE_SUFFIX)
        .filter(|base| en.contains_key(*base))
}

/// A singular variant takes the same argument slots as its base key. Keeping the count slot
/// lets `t_count` fill both with the same arguments.
#[test]
fn one_variants_take_the_slots_of_their_key() {
    let mut problems = Vec::new();
    let mut variants = 0usize;
    for cat in catalogs() {
        let en = &cat.by_lang["en"];
        for lang in LANGS {
            for (key, value) in &cat.by_lang[*lang] {
                let Some(base) = one_variant_base(key, en) else {
                    continue;
                };
                variants += 1;
                let Some(base_value) = cat.by_lang[*lang].get(base) else {
                    problems.push(format!(
                        "  {}/{lang}.toml `{key}`: `{base}` is missing",
                        cat.rel
                    ));
                    continue;
                };
                if placeholders(value) != placeholders(base_value) {
                    problems.push(format!(
                        "  {}/{lang}.toml `{key}`: the slots of {value:?} differ from `{base}` {base_value:?}",
                        cat.rel
                    ));
                }
            }
        }
    }
    assert!(
        variants > 0,
        "found no singular variant; check the collection"
    );
    assert!(
        problems.is_empty(),
        "singular variants take other argument slots than their base key:\n{}",
        problems.join("\n")
    );
}

#[test]
fn placeholders_match_english() {
    let mut problems = Vec::new();
    for cat in catalogs() {
        let en = &cat.by_lang["en"];
        for lang in &LANGS[1..] {
            for (key, value) in &cat.by_lang[lang] {
                let Some(en_value) = en.get(key) else {
                    continue; // key_sets_match_english reports it
                };
                let expected = placeholders(en_value);
                let actual = placeholders(value);
                if expected != actual {
                    problems.push(format!(
                        "  {}/{lang}.toml `{key}`: en has slots {:?} + {:?}, {lang} has slots {:?} + {:?}\n      en: {en_value:?}\n      {lang}: {value:?}",
                        cat.rel, expected.0, expected.1, actual.0, actual.1
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "placeholder mismatch — argument slots (`{{}}` in order, or `{{0}}` `{{1}}` by number) and `{{name}}` set must equal en for every key:\n{}",
        problems.join("\n")
    );
}

/// A value does not mix `{}` and `{0}`. When mixed, the runtime fills by number and leaves
/// `{}` as it is (tasty_i18n::fill_args).
#[test]
fn no_value_mixes_positional_and_numbered_placeholders() {
    let mut problems = Vec::new();
    for cat in catalogs() {
        for lang in LANGS {
            for (key, value) in &cat.by_lang[lang] {
                let (positional, numbered, _) = placeholder_forms(value);
                if positional > 0 && !numbered.is_empty() {
                    problems.push(format!("  {}/{lang}.toml `{key}` = {value:?}", cat.rel));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "a value mixes `{{}}` and `{{0}}` placeholders — use one form per string:\n{}",
        problems.join("\n")
    );
}

/// Synthetic checks of the reading the two checks above use.
#[test]
fn placeholder_slots_read_both_forms() {
    assert_eq!(placeholders("{} / {}").0, placeholders("{1} / {0}").0);
    assert_ne!(placeholders("{} / {}").0, placeholders("{0} / {0}").0);
    assert_eq!(placeholder_forms("{} {0}").0, 1);
    assert_eq!(placeholder_forms("{} {0}").1.len(), 1);
    assert_eq!(placeholders("{n} {}").1.len(), 1);
}

#[test]
fn same_as_english_values_are_allowlisted() {
    let mut problems = Vec::new();
    for cat in catalogs() {
        let en = &cat.by_lang["en"];
        for lang in &LANGS[1..] {
            for (key, value) in &cat.by_lang[lang] {
                let Some(en_value) = en.get(key) else {
                    continue;
                };
                if value != en_value || is_language_neutral(value) || allowlisted_same(key, lang) {
                    continue;
                }
                problems.push(format!("  {}/{lang}.toml `{key}` = {value:?}", cat.rel));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "values identical to en look untranslated — translate them, or if the value must stay \
         (proper noun, fixed identifier, machine-parsed output) add the key to \
         SAME_AS_ENGLISH_ALLOWLIST with a reason:\n{}",
        problems.join("\n")
    );
}

fn is_source_target(rel: &str) -> bool {
    if !rel.ends_with(".rs") {
        return false;
    }
    let in_crate_src = rel
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(_, after)| after.starts_with("src/"));
    if !(rel.starts_with("src/") || in_crate_src) {
        return false;
    }
    !(rel.contains("/tests/") || rel.ends_with("/tests.rs") || rel.ends_with("_tests.rs"))
}

fn gather(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if is_source_target(&rel_of(path)) {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // A cache under another name can come from CARGO_TARGET_DIR, so the name list
            // and the real build marker are both checked.
            if is_pruned(name) || tasty_doc_guards::is_build_cache_dir(&p) {
                continue;
            }
        }
        gather(&p, out);
    }
}

/// Skips what follows an exact #[cfg(test)] or #[test] line by brace depth. Multi-line cfg
/// and other predicates are not read.
#[derive(Default)]
struct TestRegion {
    skipping: bool,
    depth: i32,
    opened: bool,
}

impl TestRegion {
    fn skip(&mut self, line: &str) -> bool {
        let trimmed = line.trim_start();
        if !self.skipping {
            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[test]") {
                self.skipping = true;
                self.depth = 0;
                self.opened = false;
                return true;
            }
            return false;
        }
        let (opens, closes) = brace_counts(line);
        if opens > 0 {
            self.opened = true;
        }
        self.depth += opens - closes;
        if self.opened {
            if self.depth <= 0 {
                self.skipping = false; // end of the body; this line is still test code
            }
        } else if trimmed.ends_with(';') {
            self.skipping = false; // one-line item: `#[cfg(test)] mod tests;` / `use …;`
        }
        true
    }
}

/// Counts the braces outside plain strings, some char literals, and line comments. Raw
/// strings and multi-line forms are not fully handled, so a region can be misjudged.
fn brace_counts(line: &str) -> (i32, i32) {
    let mut opens = 0;
    let mut closes = 0;
    let mut in_str = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '\'' => {
                if let Some(&next) = chars.peek()
                    && (next == '{' || next == '}')
                {
                    chars.next();
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'/') => break,
            '{' => opens += 1,
            '}' => closes += 1,
            _ => {}
        }
    }
    (opens, closes)
}

/// The translation entry points that take a key as their first argument. They are the
/// `pub fn` named `t` or starting with `t_` in crates/tasty-i18n/src/lib.rs and
/// crates/tasty-plugin-sdk/src/i18n.rs at the pinned tasty commit: 8 names, read on
/// 2026-11-14. This repository reads those crates as a pinned dependency, so the list is
/// written out here; the floor below shows that plugin sources still call them.
const TRANSLATION_ENTRY_POINTS: &[&str] = &[
    "t",
    "t_args",
    "t_count",
    "t_count_template",
    "t_fmt",
    "t_fmt2",
    "t_fmt_fit",
    "t_replace",
];

/// Call prefixes, longest first, so that `t_fmt2(` is not read as `t_fmt(`.
fn translation_call_prefixes() -> Vec<String> {
    let mut names: Vec<&str> = TRANSLATION_ENTRY_POINTS.to_vec();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    names.iter().map(|n| format!("{n}(")).collect()
}

fn literal_keys(line: &str, next: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();
    for call in translation_call_prefixes() {
        let mut from = 0;
        while let Some(pos) = line[from..].find(call.as_str()) {
            let start = from + pos;
            from = start + call.len();
            let preceded_by_ident = line[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if preceded_by_ident {
                continue;
            }
            let same_line = line[from..].trim_start();
            let rest = if same_line.is_empty() {
                next.map(str::trim_start).unwrap_or("")
            } else {
                same_line
            };
            let Some(after_quote) = rest.strip_prefix('"') else {
                continue; // variable or format!: a dynamic key
            };
            let Some(end) = after_quote.find('"') else {
                continue;
            };
            keys.push(after_quote[..end].to_string());
        }
    }
    keys
}

/// The next line is read only when the argument place on the same line is empty, so a
/// dynamic argument is not mistaken for another string.
#[test]
fn a_key_wrapped_to_the_next_line_is_still_seen() {
    let found = literal_keys(
        "        return Err(IpcMethodError::new(tr.t_replace(",
        Some("            \"codex.reboot.screen_unreadable\","),
    );
    assert_eq!(
        found,
        vec!["codex.reboot.screen_unreadable".to_string()],
        "missed the key of a call that breaks the line at the opening parenthesis"
    );

    let dynamic = literal_keys("    tr.t(key_var);", Some("    \"not.a.key\","));
    assert!(
        dynamic.is_empty(),
        "took the string on the next line as the key of a dynamic call: {dynamic:?}"
    );

    let still_dynamic = literal_keys("    tr.t(", Some("        key_var,"));
    assert!(
        still_dynamic.is_empty(),
        "made up a key although the next line is a variable: {still_dynamic:?}"
    );
}

/// Literal key calls in the shipping plugin sources: 335 measured on 2026-11-14 at e09f14d.
/// The floor shows that the fixed entry point list still matches what the plugins call.
const MIN_LITERAL_KEY_CALLS: usize = 250;

#[test]
fn literal_translation_keys_exist_in_catalog() {
    let mut known: BTreeSet<String> = BTreeSet::new();
    for dir in lang_dirs() {
        known.extend(load(&dir, "en").into_keys());
    }
    let mut files = Vec::new();
    gather(root(), &mut files);
    files.sort();

    // The file is read once and kept, so that the exclusion and the key scan see the same text.
    let sources: Vec<(PathBuf, String)> = files
        .iter()
        .filter_map(|file| {
            let contents = std::fs::read_to_string(file).ok()?;
            Some((PathBuf::from(rel_of(file)), contents))
        })
        .collect();

    // Test-only modules that the file name does not show are left out by the shared reader.
    let not_shipped = tasty_doc_guards::shipping_scope::test_only_files(root(), &sources);

    assert!(
        !not_shipped.is_empty(),
        "found no test-only file. Check what the exclusion reads and the module declarations."
    );
    let shipping_probe = Path::new("crates/tasty-plugin-claude/src/main.rs");
    assert!(
        sources.iter().any(|(p, _)| p == shipping_probe),
        "the shipping file {} used for comparison was not collected",
        shipping_probe.display()
    );
    assert!(
        !not_shipped.contains(shipping_probe),
        "the shipping file {} was classed as test-only, so its translation keys would be skipped.",
        shipping_probe.display()
    );

    let mut problems = Vec::new();
    let mut pending_seen: BTreeSet<&str> = BTreeSet::new();
    let mut calls = 0usize;
    for (rel_path, contents) in &sources {
        if not_shipped.contains(rel_path) {
            continue;
        }
        let rel = rel_path.display().to_string();
        let mut tests = TestRegion::default();
        let lines: Vec<&str> = contents.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if tests.skip(line) {
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for key in literal_keys(line, lines.get(idx + 1).copied()) {
                calls += 1;
                if known.contains(&key) {
                    continue;
                }
                if let Some((pending, _)) = PENDING_FIX_MISSING_KEYS.iter().find(|(k, _)| *k == key)
                {
                    pending_seen.insert(pending);
                    continue;
                }
                problems.push(format!("  {rel}:{}: `{key}`", idx + 1));
            }
        }
    }
    assert!(
        calls >= MIN_LITERAL_KEY_CALLS,
        "found only {calls} literal key calls of {TRANSLATION_ENTRY_POINTS:?} in the plugin \
         sources (floor {MIN_LITERAL_KEY_CALLS}). Check whether the entry points were renamed \
         or the collection broke before lowering the floor."
    );
    assert!(
        problems.is_empty(),
        "t() keys not found in any lang/en.toml (typo, or the key was never added — t() would \
         render the key itself):\n{}",
        problems.join("\n")
    );
    let stale: Vec<String> = PENDING_FIX_MISSING_KEYS
        .iter()
        .filter(|(k, _)| !pending_seen.contains(k))
        .map(|(k, note)| format!("  `{k}` ({note})"))
        .collect();
    assert!(
        stale.is_empty(),
        "PENDING_FIX_MISSING_KEYS entries are no longer missing — remove them:\n{}",
        stale.join("\n")
    );
}

/// Checks that the keys and languages of the exceptions exist. Whether a translation still
/// equals English and needs the exception is not judged.
#[test]
fn same_as_english_allowlist_points_at_keys_that_exist() {
    let cats = catalogs();
    let mut known: BTreeSet<&String> = BTreeSet::new();
    for cat in &cats {
        known.extend(cat.by_lang["en"].keys());
    }
    assert!(
        !known.is_empty(),
        "the catalogs are empty, so the exception keys cannot be checked"
    );

    let mut problems = Vec::new();
    for (key, langs, _) in SAME_AS_ENGLISH_ALLOWLIST {
        if !known.iter().any(|k| k.as_str() == *key) {
            problems.push(format!("  no such key: `{key}`"));
        }
        for lang in *langs {
            if !LANGS.contains(lang) {
                problems.push(format!("  no such language: `{key}` → `{lang}`"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "an exception points at something that does not exist — if the key was renamed, change \
         the entry too; if it is gone, remove it:\n{}",
        problems.join("\n")
    );
}

// The use check reads the key tokens of Rust and TOML and the dynamic assembly forms. A
// mention in docs or comments is not counted as a use.

/// Keys for which the use check found no use.
const ORPHAN_KEYS: &[(&str, &str)] = &[
    (
        "clipboard_viewer.popup.title",
        "No use found by the consumer check. Check the real UI before removing it.",
    ),
    (
        "git_viewer.error",
        "No use found by the consumer check. Check the real UI before removing it.",
    ),
    (
        "html.settings.note",
        "No use found by the consumer check. Check the real UI before removing it.",
    ),
    (
        "markdown.addr.no_recent",
        "No use found by the consumer check. Check the real UI before removing it.",
    ),
    (
        "git_viewer.diff_heading",
        "No caller; SAME_AS_ENGLISH_ALLOWLIST in this file is its only mention.",
    ),
];

/// Reads a single placeholder inside a string with a key prefix ending in a dot as a
/// template candidate. Whether it is a real format call is not checked.
fn dynamic_key_templates() -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut files = Vec::new();
    gather(root(), &mut files);
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for lit in string_literals(&text) {
            let Some(open) = lit.find('{') else { continue };
            let Some(close) = lit[open..].find('}') else {
                continue;
            };
            let (head, tail) = (&lit[..open], &lit[open + close + 1..]);
            let inner = &lit[open + 1..open + close];
            if !(inner.is_empty() || is_identifier(inner)) {
                continue;
            }
            if !head.ends_with('.') || head.len() < 2 {
                continue;
            }
            if !head
                .trim_end_matches('.')
                .split('.')
                .all(|seg| !seg.is_empty() && is_identifier(seg))
            {
                continue;
            }
            if !tail
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                continue;
            }
            out.insert((head.to_string(), tail.to_string()));
        }
    }
    out
}

/// Reads between double quotes and skips escapes. It is not a full Rust lexer and also
/// reads strings inside comments.
fn string_literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && bytes[j] != b'"' {
            if bytes[j] == b'\\' {
                j += 1;
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        if let Ok(s) = std::str::from_utf8(&bytes[start..j]) {
            out.push(s.to_string());
        }
        i = j + 1;
    }
    out
}

/// The filled part may not hold a dot, so one prefix does not mark every key below it as used.
fn is_dynamically_assembled(key: &str, templates: &BTreeSet<(String, String)>) -> bool {
    templates.iter().any(|(head, tail)| {
        key.len() > head.len() + tail.len()
            && key.starts_with(head.as_str())
            && key.ends_with(tail.as_str())
            && !key[head.len()..key.len() - tail.len()].contains('.')
    })
}

/// This file, relative to the repository root. Its own lists are not counted as uses.
const SELF_REL: &str = "crates/plugin-guards/tests/lang_parity.rs";

/// Collects Rust and TOML outside the catalogs and this file. Whether a file is tracked by
/// Git is not checked. Docs are left out, because they may describe a key without using it.
fn consumer_files() -> Vec<PathBuf> {
    const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules"];
    const CONSUMER_EXT: &[&str] = &["rs", "toml"];
    let mut out = Vec::new();
    let mut stack = vec![root().to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let rel = rel_of(&path);
            if LANGS
                .iter()
                .any(|l| rel.ends_with(&format!("lang/{l}.toml")))
            {
                continue;
            }
            // Counting the keys of this file's own lists as uses would let a listing alone pass.
            if rel == SELF_REL {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !CONSUMER_EXT.contains(&ext.as_str()) {
                continue;
            }
            out.push(path);
        }
    }
    out
}

fn all_english_keys() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for dir in lang_dirs() {
        keys.extend(load(&dir, "en").into_keys());
    }
    keys
}

/// Leaves out comment lines. A Rust block comment is left out from its opening line to its
/// closing line, so code on those lines is dropped too. A comment-like line inside a raw
/// string is not told apart.
fn code_lines(text: &str, rs: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim_start();
        if rs {
            if in_block {
                if t.contains("*/") {
                    in_block = false;
                }
                continue;
            }
            if t.starts_with("//") {
                continue;
            }
            if t.starts_with("/*") {
                if !t.contains("*/") {
                    in_block = true;
                }
                continue;
            }
        } else if t.starts_with('#') {
            continue;
        }
        out.push(line);
    }
    out
}

/// Measured on 2026-11-14 at 50b6411: 188 consumer files, 7993 key tokens, and 678 English
/// catalog keys. The floors leave room for normal removals and stop an empty or broken walk.
const MIN_CONSUMER_FILES: usize = 140;
const MIN_KEY_TOKENS: usize = 6000;
const MIN_CATALOG_KEYS: usize = 500;

#[test]
fn every_catalog_key_has_a_consumer() {
    let keys = all_english_keys();
    // The sources are split into key-like tokens once, so each key does not search them all.
    let mut files = 0usize;
    let mut tokens: BTreeSet<String> = BTreeSet::new();
    for path in consumer_files() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        files += 1;
        // The hyphen is a token character too, so keys holding a plugin id are not split.
        let rs = path.extension().is_some_and(|e| e == "rs");
        for line in code_lines(&text, rs) {
            for token in line
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'))
            {
                if token.contains('.') && token.len() > 2 {
                    tokens.insert(token.to_string());
                }
            }
        }
    }

    assert!(
        files >= MIN_CONSUMER_FILES,
        "found only {files} consumer files (floor {MIN_CONSUMER_FILES}). Check the collection."
    );
    assert!(
        tokens.len() >= MIN_KEY_TOKENS,
        "found only {} key tokens (floor {MIN_KEY_TOKENS}). Check the collection and the split.",
        tokens.len()
    );
    assert!(
        keys.len() >= MIN_CATALOG_KEYS,
        "the catalogs hold only {} keys (floor {MIN_CATALOG_KEYS})",
        keys.len()
    );

    let templates = dynamic_key_templates();
    assert!(
        !templates.is_empty(),
        "found no dynamic key template. Check the string reading and the source range."
    );

    let mut orphans = BTreeSet::new();
    for key in &keys {
        // The whole token must match, so another key with the same prefix is not a use.
        if tokens.contains(key.as_str()) {
            continue;
        }
        // A singular variant is used where its base key is read with `t_count`.
        if let Some(base) = key.strip_suffix(tasty_i18n::plural::ONE_SUFFIX)
            && keys.contains(base)
            && (tokens.contains(base) || is_dynamically_assembled(base, &templates))
        {
            continue;
        }
        if is_dynamically_assembled(key, &templates) {
            continue;
        }
        orphans.insert(key.clone());
    }

    let listed: BTreeSet<String> = ORPHAN_KEYS.iter().map(|(k, _)| (*k).to_string()).collect();
    assert_eq!(
        ORPHAN_KEYS.len(),
        listed.len(),
        "ORPHAN_KEYS lists a key twice"
    );

    let new: Vec<&String> = orphans.difference(&listed).collect();
    let gone: Vec<&String> = listed.difference(&orphans).collect();
    assert!(
        new.is_empty() && gone.is_empty(),
        "the use check and ORPHAN_KEYS differ. Check the real UI, then update the catalog or the list.\nnew keys without a use ({}):\n{}\nkeys only in the list ({}):\n{}",
        new.len(),
        new.iter()
            .map(|k| format!("  {k}"))
            .collect::<Vec<_>>()
            .join("\n"),
        gone.len(),
        gone.iter()
            .map(|k| format!("  {k}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn a_key_named_only_in_prose_is_not_a_consumer() {
    let rs = "// this key is `demo.only.in_comment`\nlet k = \"demo.in_code\";\n";
    let kept: Vec<&str> = code_lines(rs, true);
    assert_eq!(kept.len(), 1, "only the code line should be kept: {kept:?}");
    assert!(kept[0].contains("demo.in_code"));

    let block = "/*\n demo.in_block\n*/\nlet k = \"demo.after_block\";\n";
    let kept: Vec<&str> = code_lines(block, true);
    assert_eq!(
        kept.len(),
        1,
        "the block comment was not left out: {kept:?}"
    );
    assert!(kept[0].contains("demo.after_block"));

    let attr = "#[derive(Debug)]\nstruct S;\n";
    assert_eq!(
        code_lines(attr, true).len(),
        2,
        "an attribute line was taken for a comment"
    );

    let toml = "# demo.toml_comment\ndescription_i18n_key = \"demo.toml_code\"\n";
    let kept: Vec<&str> = code_lines(toml, false);
    assert_eq!(kept.len(), 1, "the TOML comment was not left out: {kept:?}");
    assert!(kept[0].contains("demo.toml_code"));
}

/// Markdown files outside dot folders and build caches: 14 measured on 2026-11-14 at 50b6411.
const MIN_MARKDOWN_FILES: usize = 10;

/// Also checks that Markdown files exist but are not in the consumer collection.
#[test]
fn the_consumer_corpus_holds_code_and_manifests_only() {
    let files = consumer_files();
    assert!(
        files.len() >= MIN_CONSUMER_FILES,
        "the corpus holds only {} files (floor {MIN_CONSUMER_FILES})",
        files.len()
    );
    let bad: Vec<String> = files
        .iter()
        .filter(|p| !p.extension().is_some_and(|e| e == "rs" || e == "toml"))
        .map(|p| rel_of(p))
        .collect();
    assert!(
        bad.is_empty(),
        "consumer files that are neither code nor manifests: {bad:?}"
    );

    let mut md = 0usize;
    let mut all = Vec::new();
    gather_any(root(), &mut all);
    for p in &all {
        if p.extension().is_some_and(|e| e == "md") {
            md += 1;
        }
    }
    assert!(
        md >= MIN_MARKDOWN_FILES,
        "found only {md} Markdown files (floor {MIN_MARKDOWN_FILES}), so the extension filter cannot be compared"
    );
}

/// The walk used for comparison does not filter by extension, so one filter fault does not
/// show up in both results.
fn gather_any(dir: &Path, out: &mut Vec<PathBuf>) {
    const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules"];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            gather_any(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn every_orphan_entry_names_a_catalog_key() {
    let keys = all_english_keys();
    let missing: Vec<&str> = ORPHAN_KEYS
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| !keys.contains(*k))
        .collect();
    assert!(
        missing.is_empty(),
        "ORPHAN_KEYS names keys that do not exist: {missing:?}"
    );
    let no_reason: Vec<&str> = ORPHAN_KEYS
        .iter()
        .filter(|(_, why)| why.trim().is_empty())
        .map(|(k, _)| *k)
        .collect();
    assert!(
        no_reason.is_empty(),
        "entries without a reason: {no_reason:?}"
    );
}

#[test]
fn a_dynamically_assembled_key_is_not_called_an_orphan() {
    let mut templates = BTreeSet::new();
    templates.insert(("convert_popup.".to_string(), String::new()));
    assert!(is_dynamically_assembled("convert_popup.html", &templates));
    assert!(!is_dynamically_assembled("convert_popup.a.b", &templates));
    assert!(!is_dynamically_assembled("convert_popup.", &templates));
}

#[test]
fn a_brace_outside_the_literal_is_not_a_template() {
    let lits = string_literals(r#"probe(port, "attach.list", json!({}))"#);
    assert!(lits.contains(&"attach.list".to_string()), "got {lits:?}");
    assert!(
        lits.iter().all(|l| !l.contains('{')),
        "pulled a brace outside the literal inside it: {lits:?}"
    );
}
