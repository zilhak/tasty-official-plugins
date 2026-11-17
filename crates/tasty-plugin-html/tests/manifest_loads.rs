//! HTML 매니페스트와 프리셋 필드 선언을 검사한다.

use std::path::PathBuf;
use tasty_plugin_manifest::{Manifest, PresetFieldInputType, SurfaceKindCapabilityDecl};

#[test]
fn manifest_loads_and_validates() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let m = Manifest::load(&dir).expect("manifest should load + validate");
    assert_eq!(m.id, "com.tasty.html");
    assert_eq!(m.surface_kinds.len(), 1);
    let kind = &m.surface_kinds[0];
    assert_eq!(kind.kind, "html");
    // 프리셋 편집 필드 — URL(경로 파생 없음).
    assert_eq!(kind.preset_fields.len(), 1);
    let f = &kind.preset_fields[0];
    assert_eq!(f.param_key, "url");
    assert_eq!(f.input_type, PresetFieldInputType::Url);
    assert!(!f.derive_cwd);
}

#[test]
fn svg_handler_is_declared_on_the_host_svg_detector() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let m = Manifest::load(&dir).expect("manifest should load + validate");
    // html·svg 두 감지기에 핸들러를 하나씩 붙인다.
    assert_eq!(m.contributes.handler.len(), 2);
    let svg: Vec<_> = m
        .contributes
        .handler
        .iter()
        .filter(|h| h["detector"] == "svg")
        .collect();
    assert_eq!(svg.len(), 1);
    let action = &svg[0]["action"];
    assert_eq!(action["kind"], "open_surface");
    assert_eq!(action["surface_kind"], "html");
    assert_eq!(action["param_key"], "url");
}

/// html kind 는 host 의 이름 규칙이 주던 능력만 선언한다. mirror_content 는 원문 채널을 처리하지 않아 선언하지 않는다.
#[test]
fn html_kind_declares_the_host_capabilities_it_uses() {
    let m = Manifest::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))).expect("load");
    let kind = m
        .surface_kinds
        .iter()
        .find(|k| k.kind == "html")
        .expect("html kind");
    assert_eq!(
        kind.capabilities,
        SurfaceKindCapabilityDecl {
            address_band: Some(true),
            inset_notices: Some(true),
            script_gate: Some(true),
            opens_url: Some(true),
            remote_http_serve: Some(true),
            mirror_viewer: Some(true),
            ..Default::default()
        }
    );
}
