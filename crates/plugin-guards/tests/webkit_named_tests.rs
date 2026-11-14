//! Checks that the automatic workflow calls that pick the markdown WebKitGTK tests by name
//! match the real tests. A call that names a test that does not exist passes with
//! `0 passed`, so the test names and the calls are compared here. GTK initializes on one
//! thread per process, so each file holds one test and each is named on its own.

use std::path::Path;

use plugin_guards::repo_root;
use tasty_doc_guards::floored_walk::{CountedOn, Descend, Floor, Walked, walk_with_floor};
use tasty_doc_guards::workflow_triggers::automatic_job_bodies;

/// Files under .github/workflows: 3 measured on 2026-11-14 at 25f4750. Only an empty walk
/// is stopped, because the calls checked here sit in one workflow.
const WORKFLOW_FLOOR: Floor = Floor {
    min: 1,
    measured: 3,
    measured_on: "2026-11-14",
    counted_on: CountedOn::Tree("25f4750 — files under .github/workflows"),
    why_this_gap: "The repository has few workflows and they may be merged, so the floor only \
                   stops an empty walk; the check below fails on its own when no automatic \
                   cargo test call is found.",
};

/// Both .yml and .yaml, as GitHub Actions reads them.
fn is_workflow_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yml") | Some("yaml")
    )
}

fn workflow_files(workflows: &Path) -> Vec<Walked> {
    walk_with_floor(
        workflows,
        workflows,
        &WORKFLOW_FLOOR,
        Descend::Everything,
        &|_| true,
    )
    .unwrap_or_else(|why| panic!("{why}"))
    .into_iter()
    .filter(|w| is_workflow_file(&w.path))
    .collect()
}

fn automatic_job_bodies_of_dir(workflows: &Path) -> Vec<String> {
    let mut bodies = Vec::new();
    for w in workflow_files(workflows) {
        let text = std::fs::read_to_string(&w.path).unwrap_or_default();
        // The automatic triggers are read from push:, pull_request:, and schedule: before
        // jobs:.
        let head: String = text
            .lines()
            .take_while(|l| !l.starts_with("jobs:"))
            .collect();
        if !(head.contains("push:") || head.contains("pull_request:") || head.contains("schedule:"))
        {
            continue;
        }
        // Splitting jobs and leaving out manual-only ones uses the shared reader.
        bodies.extend(automatic_job_bodies(&text));
    }
    bodies
}

/// The commands under run: only; a command written in a step name is not one. Folded (>)
/// and plain scalars join their lines; a literal scalar (|) joins only lines continued
/// with a backslash.
fn run_commands(body: &str) -> Vec<String> {
    let indent_of = |l: &str| l.len() - l.trim_start().len();
    let lines: Vec<&str> = body.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let bare = line.trim_start();
        let bare = bare.strip_prefix("- ").unwrap_or(bare);
        let Some(rest) = bare.strip_prefix("run:") else {
            i += 1;
            continue;
        };
        let base = indent_of(line);
        let head = rest.trim();
        // Without a block scalar mark the command starts on the same line.
        let literal = head.starts_with('|');
        let block = literal || head.starts_with('>');
        let mut collected: Vec<String> = Vec::new();
        if !block && !head.is_empty() {
            collected.push(head.to_string());
        }
        i += 1;
        while i < lines.len() {
            let l = lines[i];
            if l.trim().is_empty() {
                collected.push(String::new());
                i += 1;
                continue;
            }
            if indent_of(l) <= base {
                break;
            }
            collected.push(l.trim().to_string());
            i += 1;
        }
        let mut cur = String::new();
        for l in &collected {
            if l.is_empty() {
                if !cur.trim().is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
                continue;
            }
            let continues = !literal || l.ends_with('\\');
            cur.push_str(l.trim_end_matches('\\'));
            cur.push(' ');
            if !continues {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.trim().is_empty() {
            out.push(cur);
        }
    }
    out
}

/// The `cargo test` calls under run:, as the arguments after the command name.
fn cargo_test_tails(body: &str) -> Vec<String> {
    run_commands(body)
        .iter()
        .filter_map(|cmd| cmd.split_once("cargo test").map(|(_, t)| t.to_string()))
        .collect()
}

/// The `cargo test` calls of the automatic jobs, as argument tails.
fn automatic_test_invocations(root: &Path) -> Vec<String> {
    automatic_job_bodies_of_dir(&root.join(".github/workflows"))
        .iter()
        .flat_map(|body| cargo_test_tails(body))
        .collect()
}

/// The test name filters after `--`, and whether `--exact` is given.
fn positive_filters(tail: &str) -> (Vec<String>, bool) {
    let words: Vec<&str> = tail.split_whitespace().collect();
    let Some(sep) = words.iter().position(|w| *w == "--") else {
        return (Vec::new(), false);
    };
    let after = &words[sep + 1..];
    let exact = after.contains(&"--exact");
    let mut filters = Vec::new();
    let mut i = 0;
    while i < after.len() {
        let w = after[i];
        // Harness flags that take a value skip that value too.
        if w == "--skip" || w == "--test-threads" || w == "--logfile" || w == "--format" {
            i += 2;
            continue;
        }
        if w.starts_with('-') {
            i += 1;
            continue;
        }
        filters.push(w.to_string());
        i += 1;
    }
    (filters, exact)
}

/// The `#[test]` function names and whether an `#[ignore]` follows, so that tests left out
/// of the normal run are told apart.
fn test_fns_with_ignore(text: &str) -> Vec<(String, bool)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "#[test]" {
            continue;
        }
        let mut ignored = false;
        for next in lines.iter().skip(i + 1).take(4) {
            let t = next.trim_start();
            if t.starts_with("#[ignore") {
                ignored = true;
                continue;
            }
            let t = t.strip_prefix("async ").unwrap_or(t);
            if let Some(rest) = t.strip_prefix("fn ")
                && let Some(name) = rest.split(['(', '<']).next()
                && !name.is_empty()
            {
                out.push((name.to_string(), ignored));
                break;
            }
        }
    }
    out
}

#[test]
fn the_webview_layout_step_names_the_one_layout_test() {
    for leaf in ["webview_layout_tests", "bed_focus_webkit_tests"] {
        assert_one_named_webkit_test(leaf);
    }
}

fn assert_one_named_webkit_test(leaf: &str) {
    const PACKAGE: &str = "tasty-plugin-markdown";
    const PARENT: &str = "crates/tasty-plugin-markdown/src/render.rs";
    let module = format!("render::{leaf}");
    let file = format!("crates/tasty-plugin-markdown/src/render/{leaf}.rs");
    let root = repo_root();

    let parent = std::fs::read_to_string(root.join(PARENT)).expect("cannot read render.rs");
    assert!(
        parent.contains(&format!("mod {leaf};")),
        "{PARENT} does not declare the {leaf} module. If the module path {module} changed, change this check and the workflow with it."
    );
    let text = std::fs::read_to_string(root.join(&file))
        .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
    let fns = test_fns_with_ignore(&text);
    assert_eq!(
        fns.len(),
        1,
        "{file} holds {} tests ({fns:?}). The call names one test, so adding tests means changing the workflow call and this check with it.",
        fns.len()
    );
    let (name, ignored) = &fns[0];
    assert!(
        *ignored,
        "{name} has no #[ignore]. It would also run in the blocking test step without a display."
    );
    let full = format!("{module}::{name}");

    let invocations = automatic_test_invocations(&root);
    assert!(
        !invocations.is_empty(),
        "found no cargo test call in the automatic jobs. Check the collection and the command reading."
    );
    let selected = invocations.iter().any(|tail| {
        let words: Vec<&str> = tail.split_whitespace().collect();
        let package = words.windows(2).any(|w| w[0] == "-p" && w[1] == PACKAGE);
        let (filters, exact) = positive_filters(tail);
        package && exact && words.contains(&"--ignored") && filters.contains(&full)
    });
    assert!(
        selected,
        "no automatic call picks this test with `-p {PACKAGE} … -- --ignored --exact {full}`. If the test name changed, change the webview layout step of the workflow with it."
    );
}
