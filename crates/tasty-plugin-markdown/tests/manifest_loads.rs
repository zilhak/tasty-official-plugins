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
