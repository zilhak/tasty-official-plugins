//! Changing the mtime through a handle opened with File::open can fail with a permission
//! error on Windows. Change it through a handle with write access. Passing on other
//! platforms does not show that Windows works.
//!
//! The masked source is split at semicolons, and a span holding both File::open and
//! set_modified/set_times is reported. The span is wide enough for a chain wrapped over
//! lines; whether it is really the same handle is not worked out. A handle bound to a
//! variable and changed in the next statement is not followed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use plugin_guards::repo_root;
use tasty_doc_guards::source_text::mask_non_code;

/// Rust files collected under crates/: 164 measured on 2026-11-14 on cbdee5c plus this
/// file. The floor leaves room for files being merged or moved and fails when the walk
/// loses most of a large crate.
const MIN_SCANNED_FILES: usize = 120;

/// If every mtime change is gone, review whether the check is still needed with its floor.
const MIN_MTIME_WRITE_SITES: usize = 1;

/// The number of mtime changes per file, so that a file losing, gaining, or changing its
/// count is found. Removing one use in a file and adding another keeps the count and is
/// not told apart. Test code that changes the mtime to check a judgment unrelated to
/// timestamps is checked too, so a test change that does not change behaviour may need
/// this list updated.
const EXPECTED_MTIME_SITES: &[(&str, usize)] = &[
    ("crates/tasty-plugin-agent-common/src/prompt_file.rs", 1),
    // The remote result cleanup tests move a result file's mtime back through a write
    // handle (write(true)) to check the retention period and the cap.
    ("crates/tasty-plugin-agent-common/src/remote_results.rs", 2),
    ("crates/tasty-plugin-claude/src/error_scan.rs", 1),
    // The quiet declaration test moves a job output file's mtime back through a write
    // handle (write(true)) so that it looks quiet for a long time.
    (
        "crates/tasty-plugin-claude/src/error_scan/quiet_tests.rs",
        1,
    ),
];

/// The Rust sources under crates/ as (relative path, LF text), test code included.
fn rust_sources() -> Vec<(PathBuf, String)> {
    let out = tasty_doc_guards::source_text::rust_sources(&repo_root(), &["crates"]);
    assert!(
        out.len() >= MIN_SCANNED_FILES,
        "collected only {} sources (floor {MIN_SCANNED_FILES}). Check the roots and the walk.",
        out.len()
    );
    out
}

fn line_of(masked: &str, pos: usize) -> usize {
    masked[..pos].bytes().filter(|b| *b == b'\n').count() + 1
}

fn scan_site_population() -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for (path, text) in rust_sources() {
        let found = scan(&mask_non_code(&text));
        if found.sites > 0 {
            out.insert(path.to_string_lossy().replace('\\', "/"), found.sites);
        }
    }
    out
}

fn site_drift(actual: &BTreeMap<String, usize>) -> Vec<String> {
    let expected: BTreeMap<String, usize> = EXPECTED_MTIME_SITES
        .iter()
        .map(|(path, n)| ((*path).to_string(), *n))
        .collect();
    let mut drift = Vec::new();
    for (path, n) in &expected {
        match actual.get(path) {
            None => drift.push(format!("  gone: {path} ({n} in the list)")),
            Some(m) if m != n => {
                drift.push(format!("  count differs: {path} — list {n}, found {m}"));
            }
            Some(_) => {}
        }
    }
    for (path, m) in actual {
        if !expected.contains_key(path) {
            drift.push(format!("  new: {path} ({m})"));
        }
    }
    drift
}

const READ_ONLY_OPEN: &str = "File::open(";
const MTIME_WRITES: &[&str] = &[".set_modified(", ".set_times("];

/// Line numbers start at 1.
struct Scan {
    /// First set_modified and first set_times of each semicolon span, counted apart.
    sites: usize,
    violations: Vec<usize>,
}

fn scan(masked: &str) -> Scan {
    let mut out = Scan {
        sites: 0,
        violations: Vec::new(),
    };
    let mut stmt_start = 0usize;
    for (offset, _) in masked.match_indices(';').chain([(masked.len(), "")]) {
        let stmt = &masked[stmt_start..offset];
        for needle in MTIME_WRITES {
            let Some(rel) = stmt.find(needle) else {
                continue;
            };
            out.sites += 1;
            if stmt.contains(READ_ONLY_OPEN) {
                out.violations.push(line_of(masked, stmt_start + rel));
            }
        }
        stmt_start = offset + 1;
    }
    out
}

#[test]
fn mtime_is_never_written_through_a_read_only_handle() {
    let mut sites = 0usize;
    let mut violations = Vec::new();
    for (path, text) in rust_sources() {
        let found = scan(&mask_non_code(&text));
        sites += found.sites;
        for line in found.violations {
            violations.push(format!("{}:{line}", path.display()));
        }
    }
    assert!(
        sites >= MIN_MTIME_WRITE_SITES,
        "found {sites} calls that write the mtime (floor {MIN_MTIME_WRITE_SITES}). If they are \
         really gone, change this floor with them"
    );
    let drift = site_drift(&scan_site_population());
    assert!(
        drift.is_empty(),
        "the mtime changes per file differ from the list. Check for a real change of use or a \
         gap in the collection.\n{}",
        drift.join("\n")
    );
    assert!(
        violations.is_empty(),
        "`File::open` gets read access only; writing the mtime through that handle fails on \
         Windows with `PermissionDenied(os error 5)` (Linux and macOS pass, so it stays hidden). \
         Open it with `std::fs::OpenOptions::new().write(true).open(..)`.\n  {}",
        violations.join("\n  ")
    );
}

mod exemption_mutations {
    //! Semicolons in comments and strings must not split the call span.

    use super::*;

    #[test]
    fn catches_a_chain_whose_semicolon_hides_in_a_string() {
        let src = "std::fs::File::open(&p.join(\"a;b\")).unwrap().set_modified(t).unwrap();\n";
        let found = scan(&mask_non_code(src));
        assert_eq!(found.sites, 1);
        assert_eq!(found.violations, vec![1]);
    }

    #[test]
    fn catches_a_chain_whose_semicolon_hides_in_a_comment() {
        let src = "std::fs::File::open(&p /* ; */).unwrap().set_modified(t).unwrap();\n";
        let found = scan(&mask_non_code(src));
        assert_eq!(found.sites, 1);
        assert_eq!(found.violations, vec![1]);
    }

    #[test]
    fn catches_a_chain_broken_across_lines_by_rustfmt() {
        let src =
            "std::fs::File::open(&stale)\n    .unwrap()\n    .set_modified(old)\n    .unwrap();\n";
        assert_eq!(scan(&mask_non_code(src)).violations, vec![3]);
    }

    #[test]
    fn does_not_flag_a_handle_opened_for_write() {
        let src =
            "std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_modified(t).unwrap();\n";
        let found = scan(&mask_non_code(src));
        assert_eq!(found.sites, 1);
        assert!(found.violations.is_empty());
    }

    /// Following a handle across semicolons is not supported.
    #[test]
    fn intentionally_misses_a_handle_bound_across_statements() {
        let src = "let f = std::fs::File::open(&p).unwrap();\nf.set_modified(t).unwrap();\n";
        let found = scan(&mask_non_code(src));
        assert_eq!(found.sites, 1);
        assert!(
            found.violations.is_empty(),
            "found a handle used across statements. If the tracking got wider, update the description and this test with it."
        );
    }

    #[test]
    fn catches_set_times_as_well_as_set_modified() {
        let src = "std::fs::File::open(&p).unwrap().set_times(times).unwrap();\n";
        assert_eq!(scan(&mask_non_code(src)).violations, vec![1]);
    }
}

mod population_mutations {
    use super::*;

    #[test]
    fn the_unmutated_site_map_has_no_drift() {
        let actual = scan_site_population();
        assert!(site_drift(&actual).is_empty(), "drift without a mutation");
        let total: usize = actual.values().sum();
        assert!(
            total >= MIN_MTIME_WRITE_SITES,
            "found {total} mtime changes (floor {MIN_MTIME_WRITE_SITES}). Check the collection."
        );
    }

    /// A file going missing can pass the total floor, but the per-file list must fail.
    #[test]
    fn a_lost_file_is_named_not_just_counted() {
        let actual = scan_site_population();
        let victim = actual.keys().next().expect("the control is empty").clone();
        let mut lost = actual.clone();
        lost.remove(&victim).expect("the key just picked");
        let drift = site_drift(&lost);
        assert_eq!(drift.len(), 1, "must name the one lost file: {drift:?}");
        assert!(drift[0].contains(&victim), "does not name it: {drift:?}");

        let left: usize = lost.values().sum();
        assert!(
            left >= MIN_MTIME_WRITE_SITES,
            "only {left} left after removing one file, so the total floor fails too. Another input is needed to show what the floor misses."
        );
    }

    #[test]
    fn a_new_site_in_another_file_is_caught() {
        let mut grown = scan_site_population();
        grown.insert("crates/tasty-plugin-codex/src/handlers.rs".into(), 2);

        let drift = site_drift(&grown);
        assert_eq!(drift.len(), 1, "{drift:?}");
        assert!(drift[0].contains("new"), "{drift:?}");
        assert!(drift[0].contains("tasty-plugin-codex"), "{drift:?}");
    }

    #[test]
    fn a_single_lost_site_inside_a_file_is_caught() {
        let actual = scan_site_population();
        let victim = actual.keys().next().expect("the control is empty").clone();
        let mut thinner = actual.clone();
        *thinner.get_mut(&victim).expect("the key just picked") -= 1;

        let drift = site_drift(&thinner);
        assert_eq!(drift.len(), 1, "{drift:?}");
        assert!(drift[0].contains("count differs"), "{drift:?}");
    }

    /// Swapping uses within a file is not told apart while the count stays. If the
    /// supported range grows, update this test with it.
    #[test]
    fn a_same_file_swap_is_not_distinguished() {
        let together = "\
fn only_here() {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(t).unwrap();
    let g = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    g.set_modified(t).unwrap();
}
";
        let apart = "\
fn first() {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(t).unwrap();
}

fn second() {
    let g = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    g.set_modified(t).unwrap();
}
";
        let a = scan(&mask_non_code(together));
        let b = scan(&mask_non_code(apart));
        assert_ne!(together, apart, "the two synthetic sources are the same");
        assert!(
            a.violations.is_empty() && b.violations.is_empty(),
            "a violation in the synthetic sources would split the comparison below for another reason"
        );
        assert_eq!(a.sites, 2, "did not count the synthetic sites");

        assert_eq!(
            a.sites, b.sites,
            "told apart a swap that keeps the count per file. If the supported range changed, update the description and the test with it."
        );
        let same_map: BTreeMap<String, usize> =
            [("x.rs".to_string(), a.sites)].into_iter().collect();
        let also_same: BTreeMap<String, usize> =
            [("x.rs".to_string(), b.sites)].into_iter().collect();
        assert_eq!(same_map, also_same);
    }

    #[test]
    fn the_guard_file_does_not_count_itself() {
        let actual = scan_site_population();
        let me = "crates/plugin-guards/tests/read_only_handle_mtime.rs";
        assert!(
            !actual.contains_key(me),
            "the guard counted itself: {actual:?}"
        );
        assert!(
            !actual.is_empty(),
            "the mtime list is empty, so whether the guard file is left out cannot be seen"
        );
    }
}
