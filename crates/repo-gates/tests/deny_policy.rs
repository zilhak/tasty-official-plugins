//! cargo-deny checks only the license policy in this repository: deny.toml holds no
//! advisories, bans, or sources settings and no license exceptions, and
//! `scripts/check.sh licenses` runs `check licenses` and nothing else. Advisories stay with
//! cargo audit, and a crate outside the policy is updated or replaced, not excused.

mod support;

fn read(rel: &str) -> String {
    std::fs::read_to_string(support::repo_file(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The lines of a TOML file without comments, trimmed. A `#` inside a string is not
/// expected in deny.toml, so the rest of a line after `#` is dropped.
fn code_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Settings that widen cargo-deny beyond the license policy, found in `text`.
fn widening_settings(text: &str) -> Vec<String> {
    const SECTIONS: [&str; 3] = ["advisories", "bans", "sources"];
    let mut found = Vec::new();
    for line in code_lines(text) {
        let header = line.trim_start_matches('[').trim_end_matches(']').trim();
        let is_header = line.starts_with('[');
        let key = line.split('=').next().unwrap_or("").trim();
        for section in SECTIONS {
            let named = |s: &str| s == section || s.starts_with(&format!("{section}."));
            if (is_header && named(header)) || (!is_header && named(key)) {
                found.push(line.clone());
            }
        }
        if line.contains("exceptions") {
            found.push(line.clone());
        }
    }
    found
}

/// The cargo deny arguments after `check` in the body of `cmd_licenses` in check.sh.
fn licenses_checks(script: &str) -> Vec<String> {
    let line = script
        .lines()
        .find(|l| l.starts_with("cmd_licenses()"))
        .expect("cmd_licenses in scripts/check.sh");
    // Words without the `;` and `}` that close the one-line function.
    let words: Vec<&str> = line
        .split_whitespace()
        .map(|w| w.trim_end_matches(';').trim_end_matches('}'))
        .filter(|w| !w.is_empty())
        .collect();
    assert!(
        words.windows(2).any(|w| w == ["cargo", "deny"]),
        "cmd_licenses does not run cargo deny: {line}"
    );
    let after = words
        .iter()
        .position(|w| *w == "check")
        .unwrap_or_else(|| panic!("cmd_licenses has no check: {line}"));
    words[after + 1..].iter().map(|w| w.to_string()).collect()
}

#[test]
fn deny_toml_holds_only_the_license_policy() {
    let text = read("deny.toml");
    assert!(
        code_lines(&text).iter().any(|l| l == "[licenses]"),
        "deny.toml has no [licenses] table"
    );
    let found = widening_settings(&text);
    assert!(
        found.is_empty(),
        "deny.toml widens cargo-deny beyond the license policy: {found:?}"
    );
}

#[test]
fn check_sh_licenses_runs_only_the_license_check() {
    let script = read("scripts/check.sh");
    assert_eq!(licenses_checks(&script), vec!["licenses".to_owned()]);
    assert!(
        script
            .lines()
            .any(|l| l.trim() == "licenses) cmd_licenses ;;"),
        "the licenses subcommand does not call cmd_licenses"
    );
}

#[test]
fn a_widening_setting_is_found() {
    for text in [
        "[advisories]\nignore = []\n",
        "[bans]\nmultiple-versions = \"deny\"\n",
        "[sources.allow-org]\ngithub = []\n",
        "sources.unknown-git = \"deny\"\n",
        "[[licenses.exceptions]]\nname = \"x\"\nallow = [\"GPL-3.0\"]\n",
        "[licenses]\nexceptions = []\n",
    ] {
        assert!(!widening_settings(text).is_empty(), "missed: {text}");
    }
    let quiet =
        "# [advisories] stay with cargo audit; no exceptions\n[licenses]\nallow = [\"MIT\"]\n";
    assert!(widening_settings(quiet).is_empty(), "a comment counted");
}

#[test]
fn another_check_in_the_command_is_found() {
    let wide =
        "cmd_licenses() { run cargo deny --locked --workspace check licenses advisories; }\n";
    assert_eq!(licenses_checks(wide), vec!["licenses", "advisories"]);
    let all = "cmd_licenses() { run cargo deny --locked --workspace check; }\n";
    assert!(licenses_checks(all).is_empty());
}
