//! cargo-deny checks only the license policy in this repository, and the policy cannot be
//! widened without changing this test. deny.toml is parsed as TOML and must hold exactly the
//! keys and values below: no advisories, bans, or sources settings, no exceptions,
//! clarifications, private-crate skips, or graph exclusions, and the allow list of tasty's
//! deny.toml. `scripts/check.sh licenses` must run exactly `check licenses` with no other
//! argument. Advisories stay with cargo audit, and a crate outside the policy is updated or
//! replaced, not excused.

mod support;

use std::collections::BTreeSet;

use toml::Value;

/// The license allow list. It is tasty's deny.toml list: change it together with tasty's
/// deny.toml, never here alone.
const ALLOW: [&str; 17] = [
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "BSL-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "CC0-1.0",
    "WTFPL",
    "MPL-2.0",
    "OFL-1.1",
    "Ubuntu-font-1.0",
];

/// The only definition of the license command and its only case branch in check.sh.
const LICENSES_FN: &str = "cmd_licenses() { run cargo deny --locked --workspace check licenses; }";
const LICENSES_BRANCH: &str = "licenses) cmd_licenses ;;";

fn read(rel: &str) -> String {
    std::fs::read_to_string(support::repo_file(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn keys(table: &toml::Table) -> BTreeSet<&str> {
    table.keys().map(String::as_str).collect()
}

fn set<'a>(names: &[&'a str]) -> BTreeSet<&'a str> {
    names.iter().copied().collect()
}

/// What in `text` differs from the pinned policy. Empty when deny.toml is exactly the policy.
fn policy_problems(text: &str) -> Vec<String> {
    let doc: toml::Table = match text.parse() {
        Ok(doc) => doc,
        Err(e) => return vec![format!("deny.toml does not parse: {e}")],
    };
    let mut problems = Vec::new();
    if keys(&doc) != set(&["graph", "licenses"]) {
        problems.push(format!(
            "top-level keys {:?}, expected graph and licenses",
            keys(&doc)
        ));
    }

    match doc.get("graph").and_then(Value::as_table) {
        Some(graph) => {
            if keys(graph) != set(&["all-features"]) {
                problems.push(format!(
                    "[graph] keys {:?}, expected all-features",
                    keys(graph)
                ));
            }
            if graph.get("all-features") != Some(&Value::Boolean(true)) {
                problems.push("[graph] all-features is not true".to_owned());
            }
        }
        None => problems.push("no [graph] table".to_owned()),
    }

    let Some(licenses) = doc.get("licenses").and_then(Value::as_table) else {
        problems.push("no [licenses] table".to_owned());
        return problems;
    };
    let expected_keys = set(&[
        "version",
        "confidence-threshold",
        "unused-allowed-license",
        "allow",
    ]);
    if keys(licenses) != expected_keys {
        problems.push(format!(
            "[licenses] keys {:?}, expected {expected_keys:?}",
            keys(licenses)
        ));
    }
    if licenses.get("version") != Some(&Value::Integer(2)) {
        problems.push("[licenses] version is not 2".to_owned());
    }
    if licenses.get("confidence-threshold") != Some(&Value::Float(0.9)) {
        problems.push("[licenses] confidence-threshold is not 0.9".to_owned());
    }
    if licenses.get("unused-allowed-license") != Some(&Value::String("allow".to_owned())) {
        problems.push("[licenses] unused-allowed-license is not \"allow\"".to_owned());
    }
    match licenses.get("allow").and_then(Value::as_array) {
        Some(allow) => {
            let names: Vec<Option<&str>> = allow.iter().map(Value::as_str).collect();
            let listed: BTreeSet<&str> = names.iter().flatten().copied().collect();
            let pinned = set(&ALLOW);
            if names.iter().any(Option::is_none) || listed != pinned {
                problems.push(format!(
                    "[licenses] allow differs from the pinned list: added {:?}, missing {:?}",
                    listed.difference(&pinned).collect::<Vec<_>>(),
                    pinned.difference(&listed).collect::<Vec<_>>()
                ));
            }
        }
        None => problems.push("[licenses] allow is not a list".to_owned()),
    }
    problems
}

/// Whether a line of check.sh defines cmd_licenses, in any of bash's spellings.
fn defines_cmd_licenses(line: &str) -> bool {
    let compact: String = line.split_whitespace().collect();
    let name = compact.strip_prefix("function").unwrap_or(&compact);
    name.starts_with("cmd_licenses()") || name.starts_with("cmd_licenses{")
}

/// What in check.sh differs from the pinned license command. Empty when it matches.
fn command_problems(script: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let defs: Vec<&str> = script
        .lines()
        .filter(|l| defines_cmd_licenses(l))
        .map(str::trim)
        .collect();
    if defs != [LICENSES_FN] {
        problems.push(format!(
            "cmd_licenses must be defined once as {LICENSES_FN:?}: {defs:?}"
        ));
    }
    let branches: Vec<&str> = script
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("licenses)"))
        .collect();
    if branches != [LICENSES_BRANCH] {
        problems.push(format!(
            "the licenses branch must be {LICENSES_BRANCH:?}: {branches:?}"
        ));
    }
    problems
}

#[test]
fn deny_toml_is_exactly_the_license_policy() {
    let problems = policy_problems(&read("deny.toml"));
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn check_sh_licenses_runs_only_the_license_check() {
    let problems = command_problems(&read("scripts/check.sh"));
    assert!(problems.is_empty(), "{problems:#?}");
}

/// deny.toml with one edit: `from` replaced by `to`, or `to` appended when `from` is empty.
fn edited_policy(from: &str, to: &str) -> String {
    let text = read("deny.toml");
    if from.is_empty() {
        return format!("{text}\n{to}\n");
    }
    assert!(text.contains(from), "deny.toml has no {from:?}");
    text.replacen(from, to, 1)
}

#[test]
fn a_widened_policy_is_rejected() {
    let cases = [
        (
            "quoted advisories table",
            edited_policy("", "[\"advisories\"]\nignore = []"),
        ),
        (
            "dotted advisories key",
            edited_policy("[graph]", "advisories . ignore = []\n\n[graph]"),
        ),
        (
            "spaced sources table",
            edited_policy("", "[sources . allow-org]\ngithub = [\"x\"]"),
        ),
        (
            "quoted exceptions key",
            edited_policy(
                "allow = [",
                "\"exceptions\" = [{ name = \"cssparser\", allow = [\"MPL-2.0\"] }]\nallow = [",
            ),
        ),
        (
            "clarify",
            edited_policy(
                "",
                "[[licenses.clarify]]\nname = \"cssparser\"\nexpression = \"MIT\"\nlicense-files = []",
            ),
        ),
        (
            "graph exclude",
            edited_policy(
                "all-features = true",
                "all-features = true\nexclude = [\"cssparser\"]",
            ),
        ),
        (
            "private skip",
            edited_policy("", "[licenses.private]\nignore = true"),
        ),
        (
            "an added license",
            edited_policy("  \"MIT\",\n", "  \"MIT\",\n  \"GPL-3.0\",\n"),
        ),
        ("a removed license", edited_policy("  \"MPL-2.0\",", "")),
        (
            "a lower confidence",
            edited_policy("confidence-threshold = 0.9", "confidence-threshold = 0.5"),
        ),
        (
            "bans",
            edited_policy("", "[bans]\nmultiple-versions = \"deny\""),
        ),
    ];
    for (what, text) in cases {
        // Each case is valid TOML, so it is rejected for what it says, not for a parse error.
        assert!(
            text.parse::<toml::Table>().is_ok(),
            "{what} does not parse:\n{text}"
        );
        assert!(
            !policy_problems(&text).is_empty(),
            "{what} was not rejected:\n{text}"
        );
    }
}

#[test]
fn a_widened_command_is_rejected() {
    let script = read("scripts/check.sh");
    let swap = |to: &str| {
        assert!(script.contains(LICENSES_FN));
        script.replacen(LICENSES_FN, to, 1)
    };
    let cases = [
        (
            "an excluded crate",
            swap(
                "cmd_licenses() { run cargo deny --locked --workspace --exclude cssparser check licenses; }",
            ),
        ),
        (
            "another config",
            swap(
                "cmd_licenses() { run cargo deny --locked --workspace --config x.toml check licenses; }",
            ),
        ),
        (
            "passed-through arguments",
            swap("cmd_licenses() { run cargo deny --locked --workspace check licenses \"$@\"; }"),
        ),
        (
            "another check",
            swap(
                "cmd_licenses() { run cargo deny --locked --workspace check licenses advisories; }",
            ),
        ),
        (
            "a second definition",
            format!("{script}\ncmd_licenses() {{ run cargo deny check; }}\n"),
        ),
        (
            "a second definition with function",
            format!("{script}\nfunction cmd_licenses {{ run cargo deny check; }}\n"),
        ),
        (
            "another branch",
            script.replacen(LICENSES_BRANCH, "licenses) cmd_licenses \"$@\" ;;", 1),
        ),
    ];
    for (what, text) in cases {
        assert!(
            !command_problems(&text).is_empty(),
            "{what} was not rejected"
        );
    }
}
