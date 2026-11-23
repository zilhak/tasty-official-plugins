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
//! Each file may define `handle_ipc_method` at most once. The reader does not evaluate
//! `#[cfg(...)]` and counts every definition it finds, so reading only the first could
//! miss an arm that lives in the second, and merging them could let one definition hide a
//! gap in the other. The dispatch body is found with the function span reader of this
//! crate (`plugin_guards::fn_spans`); the fixture tests below show the signatures it
//! reads.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use plugin_guards::{MANIFEST_NAME, fn_spans, plugin_crates, repo_root};
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

fn rel_of(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
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
    let mut bodies: Vec<Range<usize>> = fn_spans(&source.code)
        .into_iter()
        .filter(|span| span.name == DISPATCH_FN)
        .map(|span| span.open..span.close + 1)
        .collect();
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

/// What one plugin crate shows: its declared prefixes, the host methods compared, the
/// missing arms, the files that define the dispatch more than once, and whether a plugin
/// that shares a prefix with the host has no dispatch body.
#[derive(Default)]
struct PluginScan {
    declared: Vec<String>,
    joined: BTreeMap<String, BTreeSet<String>>,
    missing: Vec<String>,
    doubled: Vec<String>,
    no_body: bool,
}

/// Scans the plugin crate `name` at `dir`. Every file under `src/` is counted for its
/// dispatch definitions, whether or not the plugin shares a prefix with the host.
fn scan_plugin(
    name: &str,
    dir: &Path,
    root: &Path,
    by_prefix: &BTreeMap<String, BTreeSet<String>>,
) -> PluginScan {
    let mut scan = PluginScan::default();
    let prefixes = declared_prefixes(dir);
    scan.declared = prefixes.iter().map(|p| format!("{name}::{p}")).collect();

    let mut bodies: Vec<(String, Range<usize>)> = Vec::new();
    for file in rust_files(&dir.join("src")) {
        let text = read(&file);
        match dispatch_body(&Source::new(&text)) {
            Ok(None) => {}
            Ok(Some(body)) => bodies.push((text, body)),
            Err(count) => scan.doubled.push(format!(
                "  {}: {count} definitions of {DISPATCH_FN}",
                rel_of(&file, root)
            )),
        }
    }

    let overlapping: Vec<(&String, &BTreeSet<String>)> = prefixes
        .iter()
        .filter_map(|p| by_prefix.get(p).map(|host| (p, host)))
        .collect();
    if overlapping.is_empty() {
        return scan;
    }
    if bodies.is_empty() {
        // A file with two definitions is reported by its path and count, not as a
        // missing body.
        scan.no_body = scan.doubled.is_empty();
        return scan;
    }
    for (prefix, host) in overlapping {
        scan.joined.insert(prefix.clone(), host.clone());
        let mut handled = BTreeSet::new();
        for (text, body) in &bodies {
            handled.extend(handled_methods(&Source::new(text), body.clone(), prefix));
        }
        for method in host.difference(&handled) {
            scan.missing.push(format!(
                "  {method}: the host implements it, but the inbound dispatch of {name} has \
                 no arm for it"
            ));
        }
    }
    scan
}

#[test]
fn every_host_method_under_a_plugin_namespace_is_handled_by_that_plugin() {
    let (host_count, by_prefix) = host_methods_by_prefix();
    assert!(
        host_count >= MIN_HOST_METHODS,
        "read {host_count} host methods (floor {MIN_HOST_METHODS}, 315 measured at the pin). \
         Check the METHOD_TABLE reader."
    );

    let root = repo_root();
    let mut declared: Vec<String> = Vec::new();
    let mut joined: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut doubled: Vec<String> = Vec::new();
    let mut no_body: Vec<String> = Vec::new();
    for (name, dir) in plugin_crates() {
        let scan = scan_plugin(&name, &dir, &root, &by_prefix);
        declared.extend(scan.declared);
        joined.extend(scan.joined);
        missing.extend(scan.missing);
        doubled.extend(scan.doubled);
        if scan.no_body {
            no_body.push(name);
        }
    }

    assert!(
        doubled.is_empty(),
        "a file defines {DISPATCH_FN} more than once. The check reads one definition per \
         file:\n{}",
        doubled.join("\n")
    );
    assert!(
        no_body.is_empty(),
        "found no {DISPATCH_FN} body in {no_body:?} although they declare a namespace the \
         host uses. Check the SDK method name and the reader."
    );
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

const TWO_DISPATCHES: &str = "\
fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {
    match ctx.method.as_str() {
        \"ns.a\" => ok(),
        other => not_found(other),
    }
}
#[cfg(any())]
fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {
    match ctx.method.as_str() {
        other => not_found(other),
    }
}
";

/// A synthetic plugin crate `tasty-plugin-x` under `root` that declares `prefix`, with
/// these files under `src/`.
fn write_plugin(root: &Path, prefix: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = root.join("tasty-plugin-x");
    std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
    std::fs::write(
        dir.join(MANIFEST_NAME),
        format!(
            "manifest_version = 1\nid = \"com.example.x\"\nname = \"X\"\nversion = \"0.0.0\"\n\
             api_version = \"1\"\n\n[entry]\ntype = \"process\"\ncommand = \"x\"\n\n\
             [[contributes.ipc_namespace]]\nprefix = \"{prefix}\"\n"
        ),
    )
    .expect("write manifest");
    for (rel, text) in files {
        std::fs::write(dir.join("src").join(rel), text).expect("write source");
    }
    dir
}

fn host_of(names: &[&str]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in names {
        let (prefix, _) = name.split_once('.').expect("dotted name");
        out.entry(prefix.to_string())
            .or_default()
            .insert(name.to_string());
    }
    out
}

/// A plugin whose namespace the host does not use is still read for its definitions.
#[test]
fn a_second_definition_is_refused_in_a_plugin_the_host_does_not_share() {
    let scratch = tasty_doc_guards::temp_scratch::Scratch::new("namespace-dispatch-plugin");
    let root = scratch.path();
    let dir = write_plugin(root, "other", &[("main.rs", TWO_DISPATCHES)]);
    let scan = scan_plugin("tasty-plugin-x", &dir, root, &host_of(&["ns.a"]));
    assert_eq!(
        scan.doubled,
        ["  tasty-plugin-x/src/main.rs: 2 definitions of handle_ipc_method"]
    );
    assert!(scan.joined.is_empty() && !scan.no_body);
}

/// A plugin that shares a prefix with the host and has its only dispatch in a file with
/// two definitions is reported by that file, not as a plugin without a body.
#[test]
fn a_second_definition_in_the_only_dispatch_file_names_that_file() {
    let scratch = tasty_doc_guards::temp_scratch::Scratch::new("namespace-dispatch-plugin");
    let root = scratch.path();
    let dir = write_plugin(
        root,
        "ns",
        &[("main.rs", TWO_DISPATCHES), ("util.rs", "fn x() {}\n")],
    );
    let scan = scan_plugin("tasty-plugin-x", &dir, root, &host_of(&["ns.a"]));
    assert_eq!(
        scan.doubled,
        ["  tasty-plugin-x/src/main.rs: 2 definitions of handle_ipc_method"]
    );
    assert!(
        !scan.no_body,
        "the doubled file was reported as a missing body"
    );
}

/// Given a dispatch whose signature the reader must step over, when the body is read,
/// then it is the function body and its arm is handled.
#[test]
fn the_dispatch_body_is_found_past_its_signature() {
    let body = "{ match m { \"ns.inside\" => ok(), other => not_found(other) } }";
    let cases = [
        // A tab right after `fn`.
        format!("fn\thandle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {body}\n"),
        // A block inside an argument type.
        format!("fn handle_ipc_method(_: [Slot<{{ 1 }}>; 1]) {body}\n"),
        // A `;` inside an argument type.
        format!("fn handle_ipc_method(_: [u8; 1]) {body}\n"),
        // A block and a `;` inside an array return type, outside the argument list.
        format!("fn handle_ipc_method() -> [Slot<{{ 1 }}>; 1] {body}\n"),
    ];
    let want: BTreeSet<String> = ["ns.inside".to_string()].into();
    for src in cases {
        let (found, text) = handled_in(&src, "ns");
        assert_eq!(text, body, "{src}");
        assert_eq!(found, want, "{src}");
    }
}

/// Given one definition with a tab after `fn` and one with a space, when the definitions
/// are counted, then both count.
#[test]
fn a_definition_after_a_tab_counts_as_a_second_definition() {
    let src = "\
fn\thandle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R { match m { \"ns.a\" => ok() } }
#[cfg(any())]
fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R { match m { \"ns.a\" => ok() } }
";
    assert_eq!(dispatch_body(&Source::new(src)), Err(2));
}

/// Given a plugin crate whose only dispatch has a tab after `fn`, when the crate is
/// scanned against a host method under its prefix, then the body is found and the
/// method is handled.
#[test]
fn a_plugin_with_a_tab_after_fn_has_its_dispatch_read() {
    let scratch = tasty_doc_guards::temp_scratch::Scratch::new("namespace-dispatch-plugin");
    let root = scratch.path();
    let src = "\
fn\thandle_ipc_method(&mut self, ctx: IpcMethodCtx) -> R {
    match ctx.method.as_str() {
        \"ns.inside\" => ok(),
        other => not_found(other),
    }
}
";
    let dir = write_plugin(root, "ns", &[("main.rs", src)]);
    let scan = scan_plugin("tasty-plugin-x", &dir, root, &host_of(&["ns.inside"]));
    assert!(!scan.no_body, "the dispatch body was not found");
    let want: BTreeSet<String> = ["ns.inside".to_string()].into();
    assert_eq!(scan.joined.get("ns"), Some(&want));
    assert!(scan.missing.is_empty(), "{:?}", scan.missing);
    assert!(scan.doubled.is_empty(), "{:?}", scan.doubled);
}
