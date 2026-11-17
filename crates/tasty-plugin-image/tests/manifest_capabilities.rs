//! image 매니페스트의 kind 능력 선언을 검사한다.

use std::path::PathBuf;
use tasty_plugin_manifest::{Manifest, SurfaceKindCapabilityDecl};

/// image kind 는 바탕 포커스만 선언한다.
#[test]
fn image_kind_declares_bed_focus_only() {
    let m = Manifest::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))).expect("load");
    let kind = m
        .surface_kinds
        .iter()
        .find(|k| k.kind == "image")
        .expect("image kind");
    assert_eq!(
        kind.capabilities,
        SurfaceKindCapabilityDecl {
            bed_focus: Some(true),
            ..Default::default()
        }
    );
}
