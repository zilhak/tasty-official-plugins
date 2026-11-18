//! The release profile strips only debug info, and dist inherits it unchanged. The shipped
//! plugin binaries keep their symbol table this way, so crash backtraces show function
//! names, and the MSVC linker still writes the PDB that packaging puts in each Windows
//! archive.

use std::path::Path;

#[test]
fn release_strips_only_debuginfo_and_dist_inherits_it() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let root: toml::Table = toml::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let profile = root["profile"].as_table().expect("[profile] table");
    let release = profile["release"].as_table().expect("[profile.release]");
    let dist = profile["dist"].as_table().expect("[profile.dist]");

    assert_eq!(
        release.get("strip").and_then(|v| v.as_str()),
        Some("debuginfo"),
        "profile.release.strip must be \"debuginfo\": the shipped binaries keep their symbol \
         table so crash backtraces show function names, and Windows builds still get a PDB"
    );
    assert_eq!(
        dist.get("inherits").and_then(|v| v.as_str()),
        Some("release"),
        "profile.dist must inherit release, so the archives get the same strip level"
    );
    for key in ["strip", "debug", "split-debuginfo"] {
        assert!(
            !dist.contains_key(key),
            "profile.dist must not set {key}: it would override the release strip level that \
             keeps the symbol table and the Windows PDB"
        );
    }
}
