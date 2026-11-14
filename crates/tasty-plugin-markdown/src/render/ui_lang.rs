//! 앱 문구 요소에 붙이는 언어 표시.
//!
//! Linux(WebKitGTK)에서 측정한 결과 `lang`이 없는 한자의 형태는 OS 로케일로 정해진다. 앱 문구의 언어는
//! Tasty가 알고 있으므로 요소에 표시한다. 사용자 문서는 언어를 알 수 없어 표시하지 않는다. 그래서 `<html>`이나 문서를 감싸는
//! 요소에는 붙이지 않는다.
//!
//! 현재 한계: Linux(WebKitGTK)에서 측정한 결과 네이티브 `title` tooltip에는 `lang`이 닿지 않아 OS 로케일
//! 형태로 남는다. 고르려면 HTML tooltip이 필요하며, Windows와 macOS를 측정한 뒤 정한다.

use std::borrow::Cow;

use pulldown_cmark::Event;
use tasty_plugin_sdk::Translator;

use super::{attr_escape, html_escape};

/// 앱 문구 요소에 넣는 ` lang="<UI 언어>"`. 언어를 모르면 빈 문자열이다.
/// 빈 `lang=""`은 "언어 없음"을 선언하는 별개의 값이라 넣지 않는다.
pub(super) fn attr(tr: &Translator) -> String {
    if tr.locale.is_empty() {
        String::new()
    } else {
        format!(r#" lang="{}""#, attr_escape(&tr.locale))
    }
}

/// 스크립트가 만드는 앱 문구 요소 `var`에 언어를 넣는 JavaScript 문장. 언어를 모르면 빈 문자열이다.
pub(super) fn set_in_script(var: &str, tr: &Translator) -> String {
    if tr.locale.is_empty() {
        return String::new();
    }
    let value = serde_json::to_string(&tr.locale).unwrap_or_else(|_| "\"\"".to_string());
    format!("{var}.setAttribute('lang',{value});")
}

/// 사용자 값과 섞인 문장에서 번역 조각 하나를 이스케이프하고 언어를 표시한다. `lang`은 [`attr`]의 값이다.
pub(super) fn text_html(text: &str, lang: &str) -> String {
    if text.is_empty() || lang.is_empty() {
        return html_escape(text);
    }
    format!("<span{lang}>{}</span>", html_escape(text))
}

/// 정제 단계의 속성 필터. `span`의 `lang`은 UI 언어와 같은 값만 남긴다. 사용자 문서가 쓴 다른 언어가
/// 통과하면 본문의 한자 형태가 바뀌기 때문이다. 필터는 다른 속성을 볼 수 없어 라벨 span만 골라낼 수는
/// 없다. 문서가 UI 언어와 같은 값을 직접 쓴 `span`은 그대로 남는다.
pub(super) fn span_lang_filter(
    ui_lang: &str,
) -> impl for<'u> Fn(&str, &str, &'u str) -> Option<Cow<'u, str>> + Send + Sync + 'static {
    let ui_lang = ui_lang.to_string();
    move |element, attribute, value| {
        let refused =
            element == "span" && attribute == "lang" && (ui_lang.is_empty() || value != ui_lang);
        (!refused).then_some(Cow::Borrowed(value))
    }
}

/// 콜아웃 머리의 라벨. 번역한 기본 라벨에만 언어를 표시한다. 사용자가 쓴 제목은 언어를 알 수 없다.
pub(super) struct CalloutLabel {
    text: String,
    lang: String,
}

impl CalloutLabel {
    pub(super) fn new(title: Option<&str>, tr: &Translator, label_key: &str) -> Self {
        match title {
            Some(title) => Self {
                text: title.to_string(),
                lang: String::new(),
            },
            None => Self {
                text: tr.t(label_key).to_string(),
                lang: attr(tr),
            },
        }
    }

    /// 접지 않는 콜아웃의 라벨 요소. 글자는 CSS 생성 내용이다. 글자 노드가 없어야 찾기와 선택에
    /// 잡히지 않고, `blockquote`가 아닌 자기 요소에 언어를 받아야 본문으로 상속되지 않는다.
    pub(super) fn empty_span(&self) -> String {
        format!(
            r#"<span class="tasty-callout-label" data-label="{}"{}></span>"#,
            attr_escape(&self.text),
            self.lang
        )
    }

    /// 접는 콜아웃의 `summary` 내용. 글자 노드이며 기본 라벨이면 언어를 표시한 span으로 감싼다.
    pub(super) fn summary_events<'a>(&self) -> Vec<Event<'a>> {
        let text = Event::Text(self.text.clone().into());
        if self.lang.is_empty() {
            vec![text]
        } else {
            vec![
                Event::Html(format!("<span{}>", self.lang).into()),
                text,
                Event::Html("</span>".into()),
            ]
        }
    }
}
