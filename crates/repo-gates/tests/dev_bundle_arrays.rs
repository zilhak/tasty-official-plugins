//! scripts/dev-bundle.sh and the library it sources expand every array in a form that an
//! empty array passes under set -u in bash before 4.4, such as the /bin/bash of macOS:
//! `${a[@]+"${a[@]}"}` for words and `${a[*]+${a[*]}}` inside a string. The bash on Linux
//! accepts an empty "${a[@]}", so a run there does not catch a bare expansion; this test
//! reads the scripts instead. Element counts such as `${#a[@]}` are not expansions of the
//! elements and are left alone. Lines that start with `#` are comments and are skipped.

use std::path::Path;

const SCRIPTS: [&str; 2] = ["scripts/dev-bundle.sh", "scripts/lib/plugin-build.sh"];

/// What a scan of one script found: the guarded expansions and every other expansion of
/// array elements, with its line.
#[derive(Default)]
struct Scan {
    guarded: usize,
    other: Vec<String>,
}

fn scan(text: &str) -> Scan {
    let mut found = Scan::default();
    for (n, line) in text.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("${") {
            let tail = &rest[at..];
            let name_len = tail[2..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tail.len() - 2);
            let name = &tail[2..2 + name_len];
            let after = &tail[2 + name_len..];
            let subscript = if name.is_empty() {
                None
            } else if after.starts_with("[@]") {
                Some('@')
            } else if after.starts_with("[*]") {
                Some('*')
            } else {
                None
            };
            let Some(sub) = subscript else {
                rest = &tail[2..];
                continue;
            };
            let guard = if sub == '@' {
                format!("${{{name}[@]+\"${{{name}[@]}}\"}}")
            } else {
                format!("${{{name}[*]+${{{name}[*]}}}}")
            };
            if tail.starts_with(&guard) {
                found.guarded += 1;
                rest = &tail[guard.len()..];
            } else {
                found.other.push(format!("{}: {}", n + 1, line.trim()));
                rest = &tail[2..];
            }
        }
    }
    found
}

#[test]
fn the_dev_bundle_scripts_expand_arrays_in_the_guarded_form() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut guarded = 0;
    let mut other = Vec::new();
    for rel in SCRIPTS {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let found = scan(&text);
        guarded += found.guarded;
        other.extend(found.other.into_iter().map(|l| format!("{rel}:{l}")));
    }
    assert!(
        other.is_empty(),
        "array expansions outside the guarded form:\n{}",
        other.join("\n")
    );
    // The scripts do expand arrays; finding none means the scan no longer sees them.
    assert!(
        guarded > 0,
        "no guarded array expansion found in {SCRIPTS:?}"
    );
}

#[test]
fn the_scan_tells_guarded_expansions_from_bare_ones() {
    let found = scan(concat!(
        "f ${a[@]+\"${a[@]}\"} \"${b[*]+${b[*]}}\" ${#c[@]} ${d:-x}\n",
        "# a comment with \"${e[@]}\"\n",
        "g \"${f[@]}\" ${g[*]} ${h[@]:-} ${i[@]+${i[@]}}\n",
    ));
    assert_eq!(found.guarded, 2);
    assert_eq!(found.other.len(), 5, "{:?}", found.other);
    assert!(found.other.iter().all(|l| l.starts_with("3: ")));
}
