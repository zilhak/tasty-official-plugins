//! 본 plugin 의 매니페스트가 `tasty-plugin-manifest` 의 schema 검증을 통과하는지
//! self-contained 검증. crate 가 `BUILTINS` 에 등록되지 않아 host 머지 경로가
//! 회귀를 잡지 못하므로, 본 통합 테스트가 schema 정합성 안전망 역할을 한다.

use std::path::PathBuf;
use tasty_plugin_manifest::Manifest;

#[test]
fn manifest_loads_and_validates() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let m = Manifest::load(&dir).expect("manifest should load + validate");
    assert_eq!(m.id, "com.tasty.markdown");
    assert_eq!(m.surface_kinds.len(), 1);
    assert_eq!(m.surface_kinds[0].kind, "markdown");
    assert!(
        m.surface_kinds[0].default_colors.is_some(),
        "default_colors block should be preserved (F.H schema demo)"
    );
}

/// markdown kind 는 원문 mirror 와 바탕 포커스만 선언한다.
#[test]
fn markdown_kind_declares_mirror_content_and_bed_focus() {
    let m = tasty_plugin_manifest::Manifest::load(&std::path::PathBuf::from(env!(
        "CARGO_MANIFEST_DIR"
    )))
    .expect("load");
    let kind = m
        .surface_kinds
        .iter()
        .find(|k| k.kind == "markdown")
        .expect("markdown kind");
    assert_eq!(
        kind.capabilities,
        tasty_plugin_manifest::SurfaceKindCapabilityDecl {
            mirror_content: Some(true),
            bed_focus: Some(true),
            ..Default::default()
        }
    );
}
