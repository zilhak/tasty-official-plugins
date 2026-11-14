//! Checks the spelling of terms in the plugin translation files (the text the plugins
//! show).
//! Pane is written 페인 in Korean, not 패인, and Window is written ウィンドウ in Japanese,
//! not ウインドウ. In Korean, 창 stands for Window only when the text means an OS window
//! owned by a View, and then it is written 윈도우; another meaning of 창, such as a popup
//! the plugin declares in its manifest, is registered phrase by phrase with its evidence.
//! This follows the Window and Popup distinction of the tasty terms.
//!
//! A ban on a spelling alone lets an empty scan pass, so the number of canonical spellings
//! and of files collected is checked too.

use std::path::Path;

use tasty_doc_guards::floored_walk::{CountedOn, Descend, Floor, walk_with_floor};

struct Retired {
    needle: &'static str,
    canonical: &'static str,
    /// Keeps the check from passing with zero retired spellings when nothing was read.
    canonical_floor: usize,
    why: &'static str,
}

const RETIRED: &[Retired] = &[
    Retired {
        needle: "패인",
        canonical: "페인",
        canonical_floor: 2,
        why: "The Korean spelling of Pane is 페인. 4 measured in the plugin translation files on \
              2026-11-14 at 79b94f5; the floor leaves room for wording changes.",
    },
    Retired {
        needle: "ウインドウ",
        canonical: "ウィンドウ",
        canonical_floor: 2,
        why: "One Japanese spelling of Window. 3 measured in the plugin translation files on \
              2026-11-14 at 79b94f5; the floor leaves room for wording changes.",
    },
];

/// Registers the file, phrase, count, and evidence, so that a new phrase of the same kind
/// is not exempted on its own.
struct NotAWindow {
    path: &'static str,
    /// The phrase that picks the place. It must occur exactly `count` times in the file.
    phrase: &'static str,
    count: usize,
    /// Why it is not a Window.
    evidence: &'static str,
}

const NOT_A_WINDOW: &[NotAWindow] = &[
    NotAWindow {
        path: "crates/tasty-plugin-image/lang/ko.toml",
        phrase: "파일 선택 창",
        count: 1,
        evidence: "The save picker that image browsing opens. It is the host popup with the \
                   PopupDef id file_picker, not a separate OS View.",
    },
    NotAWindow {
        path: "crates/tasty-plugin-clipboard-viewer/lang/ko.toml",
        phrase: "스냅샷 창은",
        count: 1,
        evidence: "The manifest `crates/tasty-plugin-clipboard-viewer/tasty-plugin.toml` \
                   declares it with `[[contributes.popup]]`",
    },
    NotAWindow {
        path: "crates/tasty-plugin-clipboard-viewer/lang/ko.toml",
        phrase: "기존 창을 앞으로",
        count: 1,
        evidence: "Names the same popup a second time in one sentence; the evidence is the same \
                   manifest as above",
    },
    NotAWindow {
        path: "crates/tasty-plugin-git-viewer/lang/ko.toml",
        phrase: "기존 창을 앞으로",
        count: 1,
        evidence: "The manifest `crates/tasty-plugin-git-viewer/tasty-plugin.toml` declares it \
                   with `[[contributes.popup]]`",
    },
];

const PLUGIN_LANG_FLOOR: Floor = Floor {
    min: 20,
    measured: 32,
    measured_on: "2026-11-14",
    counted_on: CountedOn::Tree("79b94f5 — toml files in the plugin lang folders"),
    why_this_gap: "8 plugins with 4 languages each give 32 files. The floor allows three \
                   plugins to be removed; fewer files means a real removal or a broken walk.",
};

type Surface = (String, String);

fn user_facing_files() -> Vec<Surface> {
    let root = plugin_guards::repo_root();
    collect(
        &root.join("crates"),
        &root,
        &PLUGIN_LANG_FLOOR,
        Descend::SkipBuildCaches,
        &|rel| rel.contains("/lang/") && rel.ends_with(".toml"),
    )
}

/// Takes the root, floor, and filter, so the same walk can be checked on a synthetic tree.
fn collect(
    dir: &Path,
    rel_base: &Path,
    floor: &Floor,
    descend: Descend,
    keep: &dyn Fn(&str) -> bool,
) -> Vec<Surface> {
    let walked = walk_with_floor(dir, rel_base, floor, descend, &|w| keep(&w.rel))
        .unwrap_or_else(|why| panic!("{why}"));
    walked
        .into_iter()
        .map(|w| {
            // A read failure is reported even after the walk cleared its floor.
            let text = std::fs::read_to_string(&w.path)
                .unwrap_or_else(|e| panic!("cannot read the translation file {}: {e}", w.rel));
            (w.rel, text)
        })
        .collect()
}

fn count_of(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

fn offenders(files: &[Surface], needle: &str) -> Vec<String> {
    files
        .iter()
        .filter_map(|(rel, text)| match count_of(text, needle) {
            0 => None,
            n => Some(format!("  {rel} — {n} times")),
        })
        .collect()
}

fn total(files: &[Surface], needle: &str) -> usize {
    files.iter().map(|(_, t)| count_of(t, needle)).sum()
}

/// A place where the count an entry declares differs from the real count.
#[derive(Debug, PartialEq, Eq)]
struct Mismatch {
    path: String,
    phrase: String,
    found: usize,
    declared: usize,
}

/// An entry whose file is not among those collected is an error, not an empty result.
fn roster_mismatches(files: &[Surface], roster: &[NotAWindow]) -> Result<Vec<Mismatch>, String> {
    let mut out = Vec::new();
    for e in roster {
        let text = files
            .iter()
            .find(|(rel, _)| rel == e.path)
            .map(|(_, t)| t)
            .ok_or_else(|| e.path.to_string())?;
        let found = count_of(text, e.phrase);
        if found != e.count {
            out.push(Mismatch {
                path: e.path.to_string(),
                phrase: e.phrase.to_string(),
                found,
                declared: e.count,
            });
        }
    }
    Ok(out)
}

struct Uncovered {
    path: String,
    found: usize,
    registered: usize,
}

/// When an entry phrase holds the word more than once, that count is multiplied in.
fn uncovered(files: &[Surface], roster: &[NotAWindow], word: &str) -> Vec<Uncovered> {
    files
        .iter()
        .filter_map(|(rel, text)| {
            let found = count_of(text, word);
            let registered: usize = roster
                .iter()
                .filter(|e| e.path == rel)
                .map(|e| count_of(text, e.phrase) * count_of(e.phrase, word))
                .sum();
            (found != registered).then(|| Uncovered {
                path: rel.clone(),
                found,
                registered,
            })
        })
        .collect()
}

fn shared_evidence(roster: &[NotAWindow]) -> Option<(&NotAWindow, &NotAWindow)> {
    for (i, a) in roster.iter().enumerate() {
        for b in roster.iter().skip(i + 1) {
            if a.path == b.path && a.evidence == b.evidence {
                return Some((a, b));
            }
        }
    }
    None
}

#[test]
fn no_retired_spelling_survives_on_the_user_facing_surface() {
    let files = user_facing_files();
    for r in RETIRED {
        let found = offenders(&files, r.needle);
        assert!(
            found.is_empty(),
            "the retired spelling `{}` is left in the plugin text. Write `{}` instead; this \
             spelling has no exceptions:\n{}",
            r.needle,
            r.canonical,
            found.join("\n")
        );
    }
}

#[test]
fn each_canonical_word_is_still_present() {
    let files = user_facing_files();
    for r in RETIRED {
        let n = total(&files, r.canonical);
        assert!(
            n >= r.canonical_floor,
            "the canonical spelling `{}` occurs only {} times (floor {}). Check the files \
             collected and whether the text really shrank.\nwhy: {}",
            r.canonical,
            n,
            r.canonical_floor,
            r.why
        );
    }
}

#[test]
fn every_registration_occurs_exactly_as_many_times_as_declared() {
    let files = user_facing_files();
    let mismatches = roster_mismatches(&files, NOT_A_WINDOW).unwrap_or_else(|missing| {
        panic!(
            "the registered file `{missing}` was not collected. Check whether it moved or was \
             removed and the scan range, then update the entry path."
        )
    });
    assert!(
        mismatches.is_empty(),
        "the count registered differs from the real number of phrases. Check the evidence of \
         new phrases and the removed ones, then update the list:\n{}",
        mismatches
            .iter()
            .map(|m| format!(
                "  {} — `{}` {} times (registered {})",
                m.path, m.phrase, m.found, m.declared
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn every_remaining_window_word_is_registered_as_not_a_window() {
    let files = user_facing_files();
    let left = uncovered(&files, NOT_A_WINDOW, "창");
    assert!(
        left.is_empty(),
        "some `창` is not registered:\n{}\nIf it means an OS window, write 윈도우. If it \
         means something else, register it in NOT_A_WINDOW with the evidence checked, such as \
         the popup declaration of the manifest.",
        left.iter()
            .map(|u| format!(
                "  {} — `창` {} times, the list covers {}",
                u.path, u.found, u.registered
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn registrations_in_the_same_file_do_not_share_evidence() {
    // The same evidence in different files is allowed; only a repeat in one file is caught.
    if let Some((a, b)) = shared_evidence(NOT_A_WINDOW) {
        panic!(
            "two phrases of `{}` (`{}` · `{}`) share one evidence. Write why each phrase does \
             not mean a Window.",
            a.path, a.phrase, b.phrase
        );
    }
}

/// Checks the counting and the exceptions on path and text pairs, without real files.
fn surface(pairs: &[(&str, &str)]) -> Vec<Surface> {
    pairs
        .iter()
        .map(|(p, t)| ((*p).to_string(), (*t).to_string()))
        .collect()
}

#[test]
fn the_offender_scan_names_the_file_and_its_count_and_skips_the_clean_ones() {
    let files = surface(&[
        ("zone/alpha.toml", "패인 을 나눕니다. 패인 이 둘입니다."),
        ("zone/bravo.toml", "페인 을 나눕니다."),
        ("zone/charlie.md", "패인 하나."),
    ]);
    assert_eq!(
        offenders(&files, "패인"),
        vec![
            "  zone/alpha.toml — 2 times".to_string(),
            "  zone/charlie.md — 1 times".to_string()
        ],
        "only the files with the retired spelling, with their counts"
    );
    assert!(
        offenders(&files, "ウインドウ").is_empty(),
        "found a spelling that is not there"
    );
    assert_eq!(total(&files, "페인"), 1, "miscounted the canonical control");
}

/// Puts the word twice in an entry phrase to check that the count inside the phrase is used.
#[test]
fn the_uncovered_scan_multiplies_by_the_word_count_inside_the_phrase() {
    let files = surface(&[("zone/delta.md", "이 창과 저 창은 다릅니다.")]);
    let roster = &[NotAWindow {
        path: "zone/delta.md",
        phrase: "창과 저 창",
        count: 1,
        evidence: "synthetic place built by this test",
    }];
    assert!(
        uncovered(&files, roster, "창").is_empty(),
        "did not count both words inside the entry phrase"
    );

    let files = surface(&[("zone/echo.md", "창 하나 더 있는 창.")]);
    let roster = &[NotAWindow {
        path: "zone/echo.md",
        phrase: "창 하나",
        count: 1,
        evidence: "synthetic place",
    }];
    let left = uncovered(&files, roster, "창");
    assert_eq!(left.len(), 1, "did not name the uncovered place");
    assert_eq!((left[0].found, left[0].registered), (2, 1), "counted wrong");
}

#[test]
fn two_entries_in_one_file_may_not_share_evidence_but_two_files_may() {
    let same_file = &[
        NotAWindow {
            path: "zone/foxtrot.toml",
            phrase: "첫 창",
            count: 1,
            evidence: "same sentence",
        },
        NotAWindow {
            path: "zone/foxtrot.toml",
            phrase: "둘째 창",
            count: 1,
            evidence: "same sentence",
        },
    ];
    let hit = shared_evidence(same_file).expect("missed the evidence copied within one file");
    assert_eq!(
        (hit.0.phrase, hit.1.phrase),
        ("첫 창", "둘째 창"),
        "must name the two entries that share it"
    );

    let two_files = &[
        NotAWindow {
            path: "zone/foxtrot.toml",
            phrase: "첫 창",
            count: 1,
            evidence: "same sentence",
        },
        NotAWindow {
            path: "zone/golf.toml",
            phrase: "첫 창",
            count: 1,
            evidence: "same sentence",
        },
    ];
    assert!(
        shared_evidence(two_files).is_none(),
        "the same evidence must be allowed in different files"
    );
}

#[test]
fn the_roster_reports_a_count_that_grew_as_well_as_one_that_shrank() {
    let files = surface(&[
        ("zone/hotel.md", "확인 창이 뜹니다. 확인 창이 뜹니다."),
        ("zone/india.md", "아무것도 없습니다."),
    ]);
    let grew = &[NotAWindow {
        path: "zone/hotel.md",
        phrase: "확인 창이 뜹니다",
        count: 1,
        evidence: "synthetic place",
    }];
    let m = roster_mismatches(&files, grew).expect("said a collected file is missing");
    assert_eq!(
        m.len(),
        1,
        "missed a grown count; a new place would come in without evidence"
    );
    assert_eq!(
        (m[0].found, m[0].declared),
        (2, 1),
        "reported the wrong counts"
    );

    let shrank = &[NotAWindow {
        path: "zone/india.md",
        phrase: "확인 창이 뜹니다",
        count: 1,
        evidence: "synthetic place",
    }];
    let m = roster_mismatches(&files, shrank).expect("said a collected file is missing");
    assert_eq!((m[0].found, m[0].declared), (0, 1), "missed a shrunk count");

    let exact = &[NotAWindow {
        path: "zone/hotel.md",
        phrase: "확인 창이 뜹니다",
        count: 2,
        evidence: "synthetic place",
    }];
    assert!(
        roster_mismatches(&files, exact)
            .expect("the file is collected")
            .is_empty(),
        "reported a mismatch for a correct list"
    );
}

#[test]
fn a_roster_line_pointing_outside_the_population_is_an_error_not_a_skip() {
    let files = surface(&[("zone/juliett.md", "창 하나.")]);
    let moved = &[NotAWindow {
        path: "zone/kilo.md",
        phrase: "창 하나",
        count: 1,
        evidence: "synthetic place",
    }];
    assert_eq!(
        roster_mismatches(&files, moved),
        Err("zone/kilo.md".to_string()),
        "an entry outside the files collected must be an error naming its path, not an empty list"
    );
}
