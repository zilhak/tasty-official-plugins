//! 앱 문구에만 UI 언어가 붙고 사용자 문서에는 붙지 않는지 정제 뒤 HTML로 본다.

use std::path::Path;

use tasty_plugin_sdk::Translator;
use tasty_type_appearance::theme::Theme;

use super::{
    AddrFailure, AddrFailureKind, AddrWaitingView, DocumentInput, RemoteView, copy_button_script,
    image_error_script, render_document, sanitize_html,
};

const UI: &str = "zh-Hans";

/// 문구는 영어 카탈로그에서 읽고 언어 표시만 시험 값으로 둔다. 표시 위치만 보는 시험이라
/// `lang/`에 시험 언어의 카탈로그가 생겨도 기대 문구가 바뀌지 않아야 한다.
fn translator() -> Translator {
    let mut tr = Translator::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("lang"), "en");
    tr.locale = UI.to_string();
    tr
}

fn render(source: &str) -> String {
    render_with(&translator(), source, None, None, false)
}

fn render_with(
    tr: &Translator,
    source: &str,
    load_error: Option<&str>,
    remote: Option<RemoteView>,
    large_deferred: bool,
) -> String {
    let theme = Theme::with_colors_and_zoom(tasty_themes::mocha_fallback_colors(), false, 1.0);
    render_document(DocumentInput {
        find_bindings: &[],
        address_bindings: &[],
        theme: &theme,
        tr,
        file_path: "/a/見出し.md",
        source,
        load_error,
        base_dir: Some(Path::new("/a")),
        recent: &[],
        remote,
        large_deferred,
    })
}

fn render_remote(view: RemoteView) -> String {
    render_with(&translator(), "# t", None, Some(view), false)
}

const MARK: &str = r#"lang="zh-Hans""#;

/// 문서 본문 조각. 앱 문구가 있는 띠와 스크립트를 빼고 본다.
fn body_of(html: &str) -> &str {
    let start = html.find(r#"<div id="tasty-md-body">"#).expect("본문 시작");
    let end = html[start..].find("</div><script>").expect("본문 끝");
    &html[start..start + end]
}

/// `needle`로 시작하는 여는 태그 하나를 돌려준다.
fn tag_with<'a>(html: &'a str, needle: &str) -> &'a str {
    let at = html
        .find(needle)
        .unwrap_or_else(|| panic!("{needle}: {html}"));
    let start = html[..at].rfind('<').expect("태그 시작");
    let end = at + html[at..].find('>').expect("태그 끝");
    &html[start..=end]
}

#[test]
fn default_callout_label_keeps_the_ui_language_after_sanitizing() {
    let html = render("> [!NOTE]\n> 本文");
    let label = tag_with(&html, r#"class="tasty-callout-label""#);
    assert!(label.contains(r#"lang="zh-Hans""#), "{label}");
    assert!(label.contains(r#"data-label="Note""#), "{label}");
}

#[test]
fn callout_label_is_an_empty_span_so_it_adds_no_text_node() {
    let body = render("> [!NOTE]\n> 本文");
    let label = tag_with(&body, r#"class="tasty-callout-label""#);
    assert!(body.contains(&format!("{label}</span>")), "{body}");
}

#[test]
fn callout_body_does_not_inherit_the_ui_language() {
    let html = render("> [!NOTE]\n> 本文");
    let quote = tag_with(&html, r#"class="markdown-alert-note""#);
    assert!(!quote.contains("lang="), "{quote}");
    let body = body_of(&html);
    assert_eq!(
        body.matches("lang=").count(),
        1,
        "라벨 하나뿐이어야 한다: {body}"
    );
}

#[test]
fn custom_callout_title_carries_no_language() {
    let html = render("> [!info] 見出し\n> 本文");
    let body = body_of(&html);
    assert!(body.contains(r#"data-label="見出し""#), "{body}");
    assert!(!body.contains("lang="), "{body}");
}

#[test]
fn foldable_default_label_marks_only_the_summary_text() {
    let html = render("> [!NOTE]+\n> 本文");
    let body = body_of(&html);
    assert!(
        body.contains(r#"<summary><span lang="zh-Hans">Note</span></summary>"#),
        "{body}"
    );
    assert_eq!(body.matches("lang=").count(), 1, "{body}");
}

#[test]
fn foldable_custom_title_carries_no_language() {
    let html = render("> [!NOTE]+ 見出し\n> 本文");
    let body = body_of(&html);
    assert!(body.contains("<summary>見出し</summary>"), "{body}");
    assert!(!body.contains("lang="), "{body}");
}

#[test]
fn document_written_lang_is_removed_and_headings_stay_unmarked() {
    let html = render(
        "# 見出し\n\n<span lang=\"ja\">日本語</span>\n\n<p lang=\"ja\">段落</p>\n\n<blockquote lang=\"ja\">引用</blockquote>",
    );
    let body = body_of(&html);
    assert!(body.contains("見出し"), "{body}");
    assert!(body.contains("日本語"), "{body}");
    assert!(!body.contains("lang="), "{body}");
}

#[test]
fn html_element_carries_no_language() {
    let html = render("> [!NOTE]\n> 本文");
    let root = tag_with(&html, "<html");
    assert!(!root.contains("lang="), "{root}");
}

#[test]
fn sanitizer_keeps_only_the_ui_language_on_span() {
    let dirty =
        r#"<span lang="zh-Hans">a</span><span lang="ja">b</span><span lang="ZH-HANS">c</span>"#;
    assert_eq!(
        sanitize_html(dirty, UI),
        r#"<span lang="zh-Hans">a</span><span>b</span><span>c</span>"#
    );
}

#[test]
fn sanitizer_without_a_ui_language_removes_every_lang() {
    let dirty = r#"<span lang="zh-Hans">a</span><span lang="">b</span>"#;
    assert_eq!(sanitize_html(dirty, ""), "<span>a</span><span>b</span>");
}

#[test]
fn sanitizer_keeps_other_attributes_through_the_filter() {
    let dirty =
        r#"<a href="https://example.com" title="t">x</a><span class="math math-inline">y</span>"#;
    let clean = sanitize_html(dirty, UI);
    assert!(clean.contains(r#"href="https://example.com""#), "{clean}");
    assert!(clean.contains(r#"class="math math-inline""#), "{clean}");
}

#[test]
fn address_input_is_marked_and_the_path_display_is_not() {
    let html = render("# t");
    let input = tag_with(&html, r#"id="tasty-addr-input""#);
    assert!(input.contains(MARK), "{input}");
    let display = tag_with(&html, r#"class="tasty-addr-display""#);
    assert!(!display.contains("lang="), "{display}");
    let field = tag_with(&html, r#"id="tasty-addr-field""#);
    assert!(!field.contains("lang="), "{field}");
    let bar = tag_with(&html, r#"id="tasty-addr-bar""#);
    assert!(!bar.contains("lang="), "{bar}");
}

#[test]
fn find_bar_root_is_marked() {
    let html = render("# t");
    let bar = tag_with(&html, r#"id="tasty-find-bar""#);
    assert!(bar.contains(MARK), "{bar}");
}

#[test]
fn toc_marks_the_toggle_and_not_the_nav_or_the_document_headings() {
    let html = render("# 見出し");
    // 버튼 자체의 lang은 all:unset에 지워진다.
    let toggle = tag_with(&html, r#"id="tasty-toc-toggle""#);
    assert!(!toggle.contains("lang="), "{toggle}");
    let label = format!(r#"aria-expanded="true"><span {MARK}>"#);
    assert!(html.contains(&label), "{html}");
    let nav = tag_with(&html, r#"id="tasty-toc""#);
    assert!(!nav.contains("lang="), "{nav}");
    let list = html
        .split_once(r#"<ul id="tasty-toc-list">"#)
        .and_then(|(_, rest)| rest.split_once("</ul>"))
        .expect("목차 목록")
        .0;
    assert!(list.contains("見出し"), "{list}");
    assert!(!list.contains("lang="), "{list}");
}

#[test]
fn empty_state_is_marked() {
    let html = render("");
    let state = tag_with(&html, r#"class="tasty-state""#);
    assert!(state.contains(MARK), "{state}");
}

#[test]
fn load_failure_marks_the_title_and_not_the_os_error() {
    let html = render_with(&translator(), "", Some("権限がありません"), None, false);
    let title = tag_with(&html, r#"class="tasty-state-title""#);
    assert!(title.contains(MARK), "{title}");
    let detail = tag_with(&html, r#"class="tasty-state-detail""#);
    assert!(!detail.contains("lang="), "{detail}");
    assert!(html.contains("権限がありません"));
}

#[test]
fn large_file_state_marks_the_sentence_and_the_button() {
    let html = render_with(&translator(), "", None, None, true);
    let body = body_of(&html);
    assert_eq!(body.matches(MARK).count(), 2, "{body}");
    let button = tag_with(body, r#"class="tasty-state-action""#);
    assert!(button.contains(MARK), "{button}");
}

#[test]
fn remote_states_mark_their_text() {
    for view in [
        RemoteView {
            loading: true,
            ..RemoteView::default()
        },
        RemoteView {
            disconnected: true,
            ..RemoteView::default()
        },
    ] {
        let html = render_remote(view);
        assert_eq!(body_of(&html).matches(MARK).count(), 1, "{html}");
    }
}

#[test]
fn waiting_row_is_marked() {
    let html = render_remote(RemoteView {
        addr_waiting: Some(AddrWaitingView {
            typed: "/見出し.md".into(),
            after_ms: 0,
        }),
        ..RemoteView::default()
    });
    let row = tag_with(&html, r#"class="tasty-addr-waiting-text""#);
    assert!(row.contains(MARK), "{row}");
}

#[test]
fn failure_row_marks_translated_pieces_and_leaves_the_path_and_reason_alone() {
    let html = render_remote(RemoteView {
        addr_failure: Some(AddrFailure {
            typed: "/見出し.md".into(),
            kind: AddrFailureKind::Rejected("not a <markdown> file".into()),
        }),
        ..RemoteView::default()
    });
    let text = tag_with(&html, r#"class="tasty-addr-error-text""#);
    assert!(
        !text.contains("lang="),
        "문장 전체에 붙이면 경로로 상속된다: {text}"
    );
    assert!(
        html.contains(concat!(
            r#"<span lang="zh-Hans">Can't open </span><span class="tasty-addr-path">/見出し.md</span>"#,
            r#"<span lang="zh-Hans"> on the remote: </span><span lang="en">not a &lt;markdown&gt; file</span>"#,
        )),
        "{html}"
    );
}

#[test]
fn scripted_strings_mark_their_own_element() {
    let tr = translator();
    assert!(copy_button_script(&tr).contains(r#"btn.setAttribute('lang',"zh-Hans");"#));
    let image = image_error_script(&tr);
    assert!(
        image.contains(r#"label.setAttribute('lang',"zh-Hans");"#),
        "{image}"
    );
    assert!(
        !image.contains("path.setAttribute('lang'"),
        "경로는 표시하지 않는다"
    );
}

#[test]
fn unknown_ui_language_marks_nothing() {
    let html = render_with(
        &Translator::default(),
        "> [!NOTE]\n> 本文",
        None,
        None,
        false,
    );
    assert!(!html.contains("tasty-find-bar\" role=\"search\" lang="));
    let input = tag_with(&html, r#"id="tasty-addr-input""#);
    assert!(!input.contains("lang="), "{input}");
    assert!(!body_of(&html).contains("lang="));
    assert!(!copy_button_script(&Translator::default()).contains("setAttribute('lang'"));
}

#[test]
fn raw_callout_keeps_the_label_the_document_wrote_without_a_language() {
    let html =
        render(r#"<blockquote class="markdown-alert-note" data-label="見出し">b</blockquote>"#);
    let body = body_of(&html);
    assert!(
        body.contains(r#"<blockquote class="markdown-alert-note" data-label="見出し">"#),
        "{body}"
    );
    assert!(!body.contains("lang="), "{body}");
    assert!(
        html.contains(
            r#"blockquote[class^="markdown-alert-"][data-label]::before{content:attr(data-label);"#
        ),
        "라벨을 그리는 규칙"
    );
    assert!(
        html.contains(".markdown-alert-note[data-label]::before,"),
        "아이콘과 색 규칙"
    );
}

#[test]
fn app_callout_label_stays_on_its_span() {
    let body = render("> [!NOTE]\n> b");
    let quote = tag_with(body_of(&body), r#"<blockquote class="markdown-alert-note""#);
    assert!(!quote.contains("data-label"), "{quote}");
}
