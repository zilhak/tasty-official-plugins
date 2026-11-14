//! Checks the third-party notice set that every plugin archive ships: `LICENSES/` holds at
//! least one license text and regular files only, `THIRD_PARTY_LICENSES.md` links every
//! file in `LICENSES/`, and every `LICENSES/` file it links exists directly in that folder.
//! Whether the inventory lists every bundled asset is not checked.

use std::collections::BTreeSet;
use std::path::Path;

use plugin_guards::repo_root;

const INVENTORY: &str = "THIRD_PARTY_LICENSES.md";
const TEXTS_DIR: &str = "LICENSES";

/// The file names directly under `LICENSES/`, as the packaging script copies them, and the
/// entries that are not regular files. `LICENSES/` is flat: the packaging script fails on a
/// folder, link, or other entry there, so this check does too.
fn license_files(root: &Path) -> (BTreeSet<String>, Vec<String>) {
    let dir = root.join(TEXTS_DIR);
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    let mut files = BTreeSet::new();
    let mut others = Vec::new();
    for entry in entries {
        let entry = entry.expect("directory entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        // file_type does not follow a link, so a link is not taken for a file.
        let kind = entry.file_type().expect("file type");
        if kind.is_file() {
            files.insert(name);
        } else {
            others.push(name);
        }
    }
    others.sort();
    (files, others)
}

/// Links that point below a subfolder of `LICENSES/`.
fn nested_links(links: &BTreeSet<String>) -> Vec<String> {
    links
        .iter()
        .filter(|l| l.contains('/') || l.contains('\\'))
        .cloned()
        .collect()
}

/// The `LICENSES/<name>` targets of the Markdown links `](LICENSES/<name>)` in `text`.
fn linked_texts(text: &str) -> BTreeSet<String> {
    let needle = format!("]({TEXTS_DIR}/");
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(pos) = rest.find(&needle) {
        let after = &rest[pos + needle.len()..];
        if let Some(end) = after.find(')') {
            out.insert(after[..end].to_string());
        }
        rest = after;
    }
    out
}

/// (files without a link, links without a file).
fn compare(files: &BTreeSet<String>, links: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    (
        files.difference(links).cloned().collect(),
        links.difference(files).cloned().collect(),
    )
}

#[test]
fn the_inventory_links_every_license_text_and_nothing_else() {
    let root = repo_root();
    let (files, others) = license_files(&root);
    assert!(
        others.is_empty(),
        "{TEXTS_DIR}/ holds regular files only; these entries are folders, links, or other \
         kinds that the packaging script refuses: {others:?}"
    );
    assert!(
        !files.is_empty(),
        "{TEXTS_DIR}/ holds no license text; the archives would ship no third-party notice"
    );
    let text = std::fs::read_to_string(root.join(INVENTORY))
        .unwrap_or_else(|e| panic!("cannot read {INVENTORY}: {e}"));
    let links = linked_texts(&text);
    let nested = nested_links(&links);
    assert!(
        nested.is_empty(),
        "{INVENTORY} links files below a subfolder of {TEXTS_DIR}/, which holds files only: \
         {nested:?}"
    );
    let (unlinked, missing) = compare(&files, &links);
    assert!(
        unlinked.is_empty() && missing.is_empty(),
        "{INVENTORY} and {TEXTS_DIR}/ differ.\nfiles in {TEXTS_DIR}/ that {INVENTORY} does not \
         link: {unlinked:?}\nlinks in {INVENTORY} to files that do not exist: {missing:?}"
    );
}

#[test]
fn links_are_read_from_markdown_link_targets_only() {
    let text = "| a | [`LICENSES/a.txt`](LICENSES/a.txt) |\nplain LICENSES/b.txt\n\
                [x](LICENSES/c.txt) and [y](LICENSES/c.txt)";
    let links = linked_texts(text);
    assert_eq!(
        links,
        BTreeSet::from(["a.txt".to_string(), "c.txt".to_string()]),
        "a path in plain text is not a link, and a repeated link counts once"
    );
}

#[test]
fn both_directions_are_reported() {
    let files = BTreeSet::from(["a.txt".to_string(), "b.txt".to_string()]);
    let links = BTreeSet::from(["b.txt".to_string(), "c.txt".to_string()]);
    assert_eq!(
        compare(&files, &links),
        (vec!["a.txt".to_string()], vec!["c.txt".to_string()])
    );
}

#[test]
fn a_link_into_a_subfolder_is_reported() {
    let links = BTreeSet::from(["a.txt".to_string(), "nested/b.txt".to_string()]);
    assert_eq!(nested_links(&links), vec!["nested/b.txt".to_string()]);
}
