//! When a plugin declares an IPC namespace that tasty's host methods also use, calls to
//! those methods are routed to the plugin. A host method the plugin's inbound dispatch
//! does not handle can then be refused from outside even though the host implements it.
//! tasty ran this check on the plugin copies before they moved here.
//!
//! The host methods come from `METHOD_TABLE` in `crates/tasty-ipc/src/method_meta.rs` of
//! the pinned tasty commit, every key including the local-only ones. The plugin side is
//! the string patterns of the match arms inside `handle_ipc_method`, read from the
//! masked source. Strings in `host.call`, logs, or comments are not evidence that the
//! plugin handles a method. The check sees that an arm exists, not what the arm does.
//!
//! Each file may define `handle_ipc_method` at most once. The shared parser does not
//! evaluate `#[cfg(...)]` and returns every definition it recognizes, so reading only
//! the first could miss an arm that lives in the second, and merging them could let one
//! definition hide a gap in the other. The parser looks for the literal `fn ` with one
//! space: when a tab follows `fn` directly (`fn\thandle_ipc_method`), the definition is
//! not recognized (`fn \thandle_ipc_method` is). It skips a definition only when a `;`
//! comes before the first `{` after the name (as in `[u8; 1]`). When that first `{` sits
//! inside the signature (`fn handle_ipc_method(_: [Slot<{ 1 }>; 1]) { real_body() }`),
//! it can return the signature block as the body. All six dispatch functions today use
//! the SDK trait signature `(&mut self, ctx: IpcMethodCtx) -> Result<Value,
//! IpcMethodError>`, one per file.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use plugin_guards::{MANIFEST_NAME, plugin_crates, repo_root};
use tasty_doc_guards::match_arms::Source;

const DISPATCH_FN: &str = "handle_ipc_method";

/// Declared ipc_namespace entries: 6 measured on 2026-11-21 at fd948f3. Losing a third
/// of them fails.
const MIN_DECLARED_NAMESPACES: usize = 4;

/// Host methods in the pinned METHOD_TABLE: 315 measured on 2026-11-21 at tasty 7ee1fd1e.
/// The floor catches an empty or truncated read.
const MIN_HOST_METHODS: usize = 200;

/// Prefixes both the host and a plugin use: 2 (image, markdown) on 2026-11-21. A run
/// that compares nothing fails.
const MIN_JOINED_PREFIXES: usize = 1;

/// The tasty commit the plugins build against, and the host methods under the plugin
/// namespaces at that commit. When the pin moves and this set changes, compare it with
/// the new METHOD_TABLE text and note the source next to the update. Names missing from
/// the read change this set, which the floors alone would not show.
const PINNED_TASTY: &str = "7ee1fd1eece154d4b74dce34c5178a66ad87ff03";
const COMPARED_AT_PIN: &[(&str, &[&str])] = &[
    (
        "image",
        &[
            "image.export_png",
            "image.list",
            "image.next",
            "image.open",
            "image.paste",
            "image.prev",
            "image.reload",
            "image.save",
        ],
    ),
    ("markdown", &["markdown.navigate"]),
];

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

fn rel_of(path: &Path) -> String {
    let root = repo_root();
    path.strip_prefix(&root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The ipc_namespace prefixes of a plugin. Only the manifest is read; validation is
/// checked elsewhere.
fn declared_prefixes(dir: &Path) -> Vec<String> {
    let text = read(&dir.join(MANIFEST_NAME));
    let manifest: tasty_plugin_manifest::Manifest = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("cannot parse {}: {e}", dir.join(MANIFEST_NAME).display()));
    manifest
        .contributes
        .ipc_namespace
        .iter()
        .map(|d| d.prefix.clone())
        .collect()
}

/// Host method names grouped by the part before the first dot.
fn host_methods_by_prefix() -> (usize, BTreeMap<String, BTreeSet<String>>) {
    let path = tasty_doc_guards::repo_root().join("crates/tasty-ipc/src/method_meta.rs");
    let table = tasty_doc_guards::method_table(&read(&path));
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in table.keys() {
        if let Some((prefix, _)) = name.split_once('.') {
            out.entry(prefix.to_string())
                .or_default()
                .insert(name.clone());
        }
    }
    (table.len(), out)
}

/// The one dispatch body of a file, `Ok(None)` without one, `Err(count)` with more.
fn dispatch_body(source: &Source) -> Result<Option<Range<usize>>, usize> {
    let mut bodies = source.fn_bodies(DISPATCH_FN);
    match bodies.len() {
        0 => Ok(None),
        1 => Ok(bodies.pop()),
        n => Err(n),
    }
}

/// The names under `prefix` that the match arm patterns inside `body` handle.
fn handled_methods(source: &Source, body: Range<usize>, prefix: &str) -> BTreeSet<String> {
    let want = format!("{prefix}.");
    let mut out = BTreeSet::new();
    let blocks = source
        .match_blocks(body)
        .unwrap_or_else(|e| panic!("cannot read the match blocks of the dispatch: {e}"));
    for block in blocks {
        let arms = source
            .match_arms(block)
            .unwrap_or_else(|e| panic!("cannot read the arms of the dispatch: {e}"));
        for arm in arms {
            for alt in source.alternatives(&arm.pattern) {
                if let Some(name) = source.plain_string(&alt)
                    && name.starts_with(&want)
                {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// The `.rs` files under `dir`, sorted.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_host_method_under_a_plugin_namespace_is_handled_by_that_plugin() {
    let (host_count, by_prefix) = host_methods_by_prefix();
    assert!(
        host_count >= MIN_HOST_METHODS,
        "read {host_count} host methods (floor {MIN_HOST_METHODS}, 315 measured at the pin). \
         Check the METHOD_TABLE reader."
    );

    let mut declared: Vec<String> = Vec::new();
    let mut joined: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut doubled: Vec<String> = Vec::new();
    for (name, dir) in plugin_crates() {
        let prefixes = declared_prefixes(&dir);
        declared.extend(prefixes.iter().map(|p| format!("{name}::{p}")));
        let overlapping: Vec<(&String, &BTreeSet<String>)> = prefixes
            .iter()
            .filter_map(|p| by_prefix.get(p).map(|host| (p, host)))
            .collect();
        if overlapping.is_empty() {
            continue;
        }

        let mut handled_by_prefix: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        let mut found_body = false;
        for file in rust_files(&dir.join("src")) {
            let text = read(&file);
            let source = Source::new(&text);
            match dispatch_body(&source) {
                Ok(None) => {}
                Ok(Some(body)) => {
                    found_body = true;
                    for (prefix, _) in &overlapping {
                        handled_by_prefix
                            .entry(prefix.as_str())
                            .or_default()
                            .extend(handled_methods(&source, body.clone(), prefix));
                    }
                }
                Err(count) => doubled.push(format!(
                    "  {}: {count} definitions of {DISPATCH_FN}",
                    rel_of(&file)
                )),
            }
        }
        assert!(
            found_body,
            "found no {DISPATCH_FN} body in {name}/src although it declares a namespace the \
             host uses. Check the SDK method name and the reader."
        );
        for (prefix, host) in overlapping {
            joined.insert(prefix.clone(), host.clone());
            let handled = handled_by_prefix
                .get(prefix.as_str())
                .cloned()
                .unwrap_or_default();
            for method in host.difference(&handled) {
                missing.push(format!(
                    "  {method}: the host implements it, but the inbound dispatch of {name} has \
                     no arm for it"
                ));
            }
        }
    }

    assert!(
        declared.len() >= MIN_DECLARED_NAMESPACES,
        "read {} ipc_namespace entries (floor {MIN_DECLARED_NAMESPACES}): {declared:?}. Compare \
         with the manifests.",
        declared.len()
    );
    assert!(
        joined.len() >= MIN_JOINED_PREFIXES,
        "compared {} prefixes (floor {MIN_JOINED_PREFIXES}). Declared: {declared:?}",
        joined.len()
    );
    let expected: BTreeMap<String, BTreeSet<String>> = COMPARED_AT_PIN
        .iter()
        .map(|(prefix, names)| {
            (
                prefix.to_string(),
                names.iter().map(|n| n.to_string()).collect(),
            )
        })
        .collect();
    assert_eq!(
        joined, expected,
        "the compared host methods differ from the record for tasty {PINNED_TASTY}. If the pin \
         moved, check the new METHOD_TABLE text and update COMPARED_AT_PIN with that source."
    );
    assert!(
        doubled.is_empty(),
        "a file defines {DISPATCH_FN} more than once. The check reads one definition per \
         file:\n{}",
        doubled.join("\n")
    );
    assert!(
        missing.is_empty(),
        "host methods under a plugin namespace are missing from that plugin's inbound \
         dispatch. Add an arm that handles or forwards them, or review whether the host \
         should provide them:\n{}",
        missing.join("\n")
    );
}

fn handled_in(src: &str, prefix: &str) -> (BTreeSet<String>, String) {
    let source = Source::new(src);
    let body = dispatch_body(&source)
        .expect("one definition")
        .expect("the dispatch body is found");
    let text = source.slice(&body).to_string();
    (handled_methods(&source, body, prefix), text)
}

/// Strings of the same prefix outside the function are not counted.
#[test]
fn the_cut_is_the_dispatch_body_not_the_whole_file() {
    let src = "\
fn other_before(m: &str) { match m { \"ns.before\" => ok(), _ => no() } }
fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {
    match ctx.method.as_str() {
        \"ns.inside\" => ok(),
        other => not_found(other),
    }
}
fn other_after(m: &str) { match m { \"ns.after\" => ok(), _ => no() } }
";
    let (found, body) = handled_in(src, "ns");
    let want: BTreeSet<String> = ["ns.inside".to_string()].into();
    assert_eq!(found, want, "the cut took arms outside the dispatch body");
    assert!(
        !body.contains("other_after") && !body.contains("other_before"),
        "the body ran past its function: {body}"
    );
}

#[test]
fn braces_inside_string_literals_do_not_close_the_body() {
    let src = "\
fn handle_ipc_method(&mut self) -> R {
    emit(format!(\"{}\", x));
    emit(\"}\");
    match m { \"ns.inside\" => ok() }
}
fn after() { emit(\"ns.after\"); }
";
    let (found, _) = handled_in(src, "ns");
    assert!(
        found.contains("ns.inside") && !found.contains("ns.after"),
        "a brace in a string ended the body early or late: {found:?}"
    );
}

/// Inside the dispatch, only strings in arm patterns are handled methods.
#[test]
fn only_arm_patterns_count_as_handled() {
    let src = "\
fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {
    // \"ns.commented\" is not handled here
    tracing::warn!(\"ns.logged is not there yet\");
    let _ = host.call(\"ns.forwarded\", p);
    match ctx.method.as_str() {
        \"ns.inside\" | \"ns.also\" => ok(),
        \"ns.guarded\" if ready => ok(),
        other => not_found(other),
    }
}
";
    let (found, _) = handled_in(src, "ns");
    let want: BTreeSet<String> = ["ns.inside", "ns.also", "ns.guarded"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        found, want,
        "counted a name outside the arms or missed an arm"
    );
}

/// A second definition behind a cfg that never holds is still refused.
#[test]
fn a_second_dispatch_definition_in_one_file_is_refused() {
    let src = "\
fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {
    match ctx.method.as_str() {
        other => not_found(other),
    }
}
#[cfg(any())]
fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {
    match ctx.method.as_str() {
        \"ns.inside\" => ok(),
        other => not_found(other),
    }
}
";
    assert_eq!(dispatch_body(&Source::new(src)), Err(2));
}
