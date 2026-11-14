//! cargo-deny checks only the license policy in this repository, and the policy cannot be
//! widened without changing this test. deny.toml is parsed as TOML and must hold exactly the
//! keys and values below: no advisories, bans, or sources settings, no exceptions,
//! clarifications, private-crate skips, or graph exclusions, and the allow list of tasty's
//! deny.toml. `scripts/check.sh licenses` is run with a cargo that records its arguments,
//! with GITHUB_ACTIONS unset and set to true and with a fixed set of sample extra
//! arguments. In each run cargo must be called exactly once with
//! `deny --locked --workspace check licenses`, and a failing cargo must fail the run.
//! This does not prove every branch on other arguments; CI calls the command without any.
//! Advisories stay with cargo audit, and a crate outside the policy is updated or replaced,
//! not excused.

#![cfg(unix)]

mod support;

use std::collections::BTreeSet;
use std::path::Path;

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

/// The one cargo call that `scripts/check.sh licenses` makes, as its arguments.
const LICENSES_ARGV: [&str; 5] = ["deny", "--locked", "--workspace", "check", "licenses"];

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

/// Extra arguments given after `licenses` in the runs. CI calls it without any; the others
/// are samples of what a local caller might pass, so an argument-dependent branch for one of
/// them is caught. Other arguments are not proven.
const EXTRA_SAMPLES: [&[&str]; 9] = [
    &[],
    &["extra", "args"],
    &["--exclude", "cssparser"],
    &["--exclude-dev"],
    &["--config", "x.toml"],
    &["--all-features"],
    &["--workspace"],
    &["check", "licenses"],
    &["advisories"],
];

/// A cargo that appends its arguments to `$CARGO_RECORD`, one per line, and a line `--end--`
/// after each call, then exits with `$STUB_RC` (0 by default).
fn recording_cargo(dir: &Path) {
    support::write_executable(
        &dir.join("cargo"),
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$CARGO_RECORD\"; done\n\
         echo --end-- >> \"$CARGO_RECORD\"\nexit \"${STUB_RC:-0}\"\n",
    )
    .expect("recording cargo");
}

/// One run of `bash <script> licenses <extra>`: its exit code and the recorded cargo calls.
struct Run {
    code: Option<i32>,
    calls: Vec<Vec<String>>,
}

/// Runs `bash <script> licenses <extra>` with the recording cargo first on PATH, in CI
/// (`GITHUB_ACTIONS=true`) or outside it, with the cargo exiting `stub_rc`. check.sh changes
/// to its repository root.
fn run_licenses(script: &Path, extra: &[&str], in_ci: bool, stub_rc: i32) -> Run {
    let stub = tempfile::tempdir().expect("stub dir");
    recording_cargo(stub.path());
    let record = stub.path().join("record");
    let mut cmd = support::git_env::command("bash");
    cmd.arg(script)
        .arg("licenses")
        .args(extra)
        .env("CARGO_RECORD", &record)
        .env("STUB_RC", stub_rc.to_string())
        .env(
            "PATH",
            format!(
                "{}:{}",
                stub.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    if in_ci {
        cmd.env("GITHUB_ACTIONS", "true");
    } else {
        cmd.env_remove("GITHUB_ACTIONS");
    }
    let out = cmd.output().expect("run check.sh");
    let text = std::fs::read_to_string(&record).unwrap_or_default();
    let mut calls = Vec::new();
    let mut call = Vec::new();
    for line in text.lines() {
        if line == "--end--" {
            calls.push(std::mem::take(&mut call));
        } else {
            call.push(line.to_owned());
        }
    }
    Run {
        code: out.status.code(),
        calls,
    }
}

/// Runs `<script> licenses` in both environments with every extra-argument sample, once with
/// a cargo that succeeds and once with one that fails with 3. Each run must call cargo
/// exactly once with LICENSES_ARGV and exit 0 when cargo succeeds and non-zero when it fails.
/// Returns what differed.
fn license_command_problems(script: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for in_ci in [false, true] {
        for extra in EXTRA_SAMPLES {
            for stub_rc in [0, 3] {
                let run = run_licenses(script, extra, in_ci, stub_rc);
                let argv_ok = run.calls.len() == 1 && run.calls[0] == LICENSES_ARGV;
                let code_ok = if stub_rc == 0 {
                    run.code == Some(0)
                } else {
                    run.code.is_some_and(|c| c != 0)
                };
                if !argv_ok || !code_ok {
                    problems.push(format!(
                        "in CI: {in_ci}, extra {extra:?}, cargo exit {stub_rc}: exit {:?}, \
                         cargo calls {:?}",
                        run.code, run.calls
                    ));
                }
            }
        }
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
    let problems = license_command_problems(&support::repo_root().join("scripts/check.sh"));
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

/// The pinned policy written as two inline tables, with `graph_extra` and `licenses_extra`
/// added inside them.
fn inline_policy(graph_extra: &str, licenses_extra: &str) -> String {
    let allow: Vec<String> = ALLOW.iter().map(|l| format!("{l:?}")).collect();
    format!(
        "graph = {{ all-features = true{graph_extra} }}\nlicenses = {{ version = 2, \
         confidence-threshold = 0.9, unused-allowed-license = \"allow\", allow = [{}]{licenses_extra} }}\n",
        allow.join(", ")
    )
}

#[test]
fn the_inline_form_of_the_policy_itself_passes() {
    // The check reads the parsed values, so the same policy in another TOML form passes.
    let problems = policy_problems(&inline_policy("", ""));
    assert!(problems.is_empty(), "{problems:#?}");
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
        (
            "exceptions key with a unicode escape",
            edited_policy(
                "allow = [",
                "\"except\\u0069ons\" = [{ name = \"cssparser\", allow = [\"MPL-2.0\"] }]\nallow = [",
            ),
        ),
        (
            "dotted private key inside [licenses]",
            edited_policy("allow = [", "private . ignore = true\nallow = ["),
        ),
        (
            "inline graph table with an exclusion",
            inline_policy(", exclude = [\"cssparser\"]", ""),
        ),
        (
            "inline licenses table with a private skip",
            inline_policy("", ", private = { ignore = true }"),
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

/// A copy of scripts/check.sh with `to` put in place of `from` (which must be there), in a
/// repository folder of its own.
fn edited_script(dir: &Path, from: &str, to: &str) -> std::path::PathBuf {
    let text = read("scripts/check.sh");
    assert!(text.contains(from), "scripts/check.sh has no {from:?}");
    let script = dir.join("scripts/check.sh");
    std::fs::create_dir_all(script.parent().expect("scripts dir")).expect("scripts dir");
    std::fs::write(&script, text.replacen(from, to, 1)).expect("write the copy");
    std::fs::copy(support::repo_file("deny.toml"), dir.join("deny.toml")).expect("deny.toml");
    script
}

#[test]
fn a_widened_command_is_rejected() {
    const DEF: &str = "cmd_licenses() { run cargo deny --locked --workspace check licenses; }\n";
    const MAIN_CASE: &str = "case \"$sub\" in\n  all)";
    let after_def = |extra: &str| (DEF, format!("{DEF}{extra}\n"));
    let before_case = |extra: &str| (MAIN_CASE, format!("{extra}\n{MAIN_CASE}"));
    let in_case = |branch: &str| (MAIN_CASE, format!("case \"$sub\" in\n  {branch}\n  all)"));
    let widened = "run cargo deny --locked --workspace --exclude x check licenses";
    let cases: Vec<(&str, (&str, String))> = vec![
        (
            "function redefinition on two lines",
            after_def(&format!("function cmd_licenses\n{{ {widened}; }}")),
        ),
        (
            "redefinition after true",
            after_def(&format!("true; cmd_licenses() {{ {widened}; }}")),
        ),
        (
            "redefinition inside if",
            after_def(&format!(
                "if true; then cmd_licenses() {{ {widened}; }}; fi"
            )),
        ),
        (
            "redefinition across a line continuation",
            after_def(&format!("cmd_licen\\\nses() {{ {widened}; }}")),
        ),
        (
            "alias",
            after_def(&format!(
                "shopt -s expand_aliases\nalias cmd_licenses='{widened}'"
            )),
        ),
        (
            "an earlier alternative pattern",
            in_case(&format!("licenses | zz) {widened} ;;")),
        ),
        (
            "an earlier quoted pattern",
            in_case(&format!("\"licenses\") {widened} ;;")),
        ),
        (
            "an earlier glob pattern",
            in_case(&format!("licens*) {widened} ;;")),
        ),
        (
            "a test before the case",
            before_case(&format!("[[ $sub == licenses ]] && {{ {widened}; exit; }}")),
        ),
        (
            "a cargo function",
            after_def("cargo() { command cargo deny --exclude x \"${@:2}\"; }"),
        ),
        (
            "a run that changes the arguments",
            after_def(
                "run() { if [[ \"$*\" == *\"check licenses\"* ]]; then \"$@\" advisories; \
                 else \"$@\"; fi; }",
            ),
        ),
        (
            "a branch that passes arguments",
            (
                "  licenses) cmd_licenses ;;",
                "  licenses) run cargo deny --locked --workspace check licenses \"$@\" ;;"
                    .to_owned(),
            ),
        ),
        (
            "a wider check outside CI only",
            after_def(&format!(
                "if [[ \"${{GITHUB_ACTIONS:-}}\" != true ]]; then\n  \
                 cmd_licenses() {{ {widened}; }}\nfi"
            )),
        ),
        (
            // After the shift, so $1 is the first argument after `licenses`.
            "a wider check when --exclude is passed",
            before_case(&format!(
                "if [[ \"${{1:-}}\" == --exclude ]]; then\n  cmd_licenses() {{ {widened}; }}\nfi"
            )),
        ),
        (
            "a swallowed cargo failure",
            (
                DEF,
                "cmd_licenses() { run cargo deny --locked --workspace check licenses || true; }\n"
                    .to_owned(),
            ),
        ),
        (
            // `cmd_licenses; exit 0` alone still stops at the failure under set -e.
            "a branch that exits 0",
            (
                "  licenses) cmd_licenses ;;",
                "  licenses) cmd_licenses || :; exit 0 ;;".to_owned(),
            ),
        ),
    ];
    for (what, (from, to)) in cases {
        let dir = tempfile::tempdir().expect("copy dir");
        let script = edited_script(dir.path(), from, &to);
        assert!(
            !license_command_problems(&script).is_empty(),
            "{what} was not rejected"
        );
    }
}
