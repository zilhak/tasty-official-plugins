//! ammonia 공지가 전제하는 style 속성과 SVG 애니메이션을 허용 목록이 열어 두지 않는지 본다.
//! 입력은 RUSTSEC-2021-0157, RUSTSEC-2021-0158, RUSTSEC-2026-0373의 예시 형태다.

use super::sanitize_bare;

#[test]
fn style_attribute_with_css_url_does_not_survive() {
    for dirty in [
        r#"<div style='background:url("https://evil.example/ping");'>kept</div>"#,
        r#"<div style='background:URL("https://evil.example/ping");'>kept</div>"#,
    ] {
        let clean = sanitize_bare(dirty);
        assert_eq!(clean, "<div>kept</div>");
    }
}

#[test]
fn svg_animation_does_not_survive() {
    let clean = sanitize_bare(
        r#"<p>kept</p><svg xmlns="http://www.w3.org/2000/svg"><a><animateMotion attributeName="href" to="javascript:alert(1)"></animateMotion><text y="30">label</text></a></svg>"#,
    );
    assert!(clean.contains("kept"));
    for removed in ["svg", "animateMotion", "attributeName", "javascript"] {
        assert!(!clean.contains(removed), "{removed} survived: {clean}");
    }
}
