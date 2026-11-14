//! 문서 위 주소 띠(HTML·CSS). 디자인 갤러리의 Markdown 주소 띠(`markdown-address-bar`)를 토큰 값으로 옮긴다.
//!
//! 로컬은 [필드 · Go], attach mirror 는 [Reload · 필드 · Go] 다. 모든 버튼은 IconButton ghost sm 과 같은
//! 상자이고 글리프는 CSS mask 로 그려 버튼 글자색(hover·disabled)을 따른다. 필드가
//! `md-address-go-hide-below` 보다 좁아질 폭에서는 Go 만 숨긴다(Enter 는 그대로).
//!
//! 원격 경로로 직접 이동하다 실패하면 입력값을 남기고 띠 아래 오류 행을 그린다. 문장은 실패 종류마다
//! 다르며 그 행은 다음 입력·Esc·성공에서 사라진다(`nav_script`). 결과 없이 [`ADDR_WAITING_AFTER_MS`]
//! 가 지나면 같은 자리에 대기 행을 그린다. 대기 행은 오류가 아니다.

use tasty_plugin_sdk::Translator;
use tasty_type_appearance::theme::Theme;

use super::callout::alert_icon_data_uri;
use super::{RemoteView, attr_escape, html_escape, ui_lang};

/// 원격 주소창 직접 이동을 보내고 결과 없이 이만큼 지나면 대기 행을 보인다(디자인 30초).
pub const ADDR_WAITING_AFTER_MS: u64 = 30_000;

/// 입력 포커스·오류 테두리 바깥 고리의 불투명도. 디자인 갤러리 필드가 쓰는
/// `color-mix(in srgb, <edge> 35%, transparent)` 와 같은 값이다(35% = 89/255).
const FIELD_RING_ALPHA: u8 = 89;

/// 경로 글꼴. 본문 `code` 와 같은 고정폭 글꼴 목록이다.
const MONO_STACK: &str = "ui-monospace,SFMono-Regular,Menlo,Consolas,monospace";

/// 원격 직접 이동이 실패한 종류. 다음에 할 일이 달라 종류마다 문장이 다르다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddrFailureKind {
    /// 원격이 그 경로에 파일이 없다고 답했다.
    NotFound,
    /// 원격이 거절하며 준 사유 한 줄. 번역하지 않고 그대로 보인다. 비어 있지 않다.
    Rejected(String),
    /// 결과 전에 연결이 끊겼다. 원격이 적용했는지 모른다.
    ConnectionLost,
    /// 보내기 전에 연결이나 대응이 바뀌어 보내지 않았다.
    Cancelled,
    /// 요청이 원격에 닿지 않았다(host 오류).
    Internal,
}

impl AddrFailureKind {
    /// 원격 거절. 사유가 비어 있으면 보일 사유가 없어 내부 실패 문장을 쓴다.
    pub fn rejected(reason: &str) -> Self {
        let reason = reason.trim();
        if reason.is_empty() {
            Self::Internal
        } else {
            Self::Rejected(reason.to_string())
        }
    }

    fn key(&self) -> &'static str {
        match self {
            Self::NotFound => "markdown.address.error.not_found",
            Self::Rejected(_) => "markdown.address.error.rejected",
            Self::ConnectionLost => "markdown.address.error.connection_lost",
            Self::Cancelled => "markdown.address.error.cancelled",
            Self::Internal => "markdown.address.error.internal",
        }
    }
}

/// 원격 주소창 직접 이동의 실패. 입력한 경로를 그대로 다시 보인다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddrFailure {
    pub typed: String,
    pub kind: AddrFailureKind,
}

/// 원격 주소창 직접 이동이 결과를 기다리는 중에 문서를 다시 그릴 때의 상태. 입력값을 그대로 보이고,
/// `after_ms` 뒤(0이면 바로) 대기 행을 그린다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddrWaitingView {
    pub typed: String,
    pub after_ms: u64,
}

/// 주소 띠 전체 높이(`--md-addr-bar-h`). 오류 행이 있으면 그만큼 늘어난다. 검색 바 위치와 제목 앵커
/// 여백이 이 값을 읽는다.
pub(super) fn bar_height_px(theme: &Theme, with_error_row: bool) -> f32 {
    let band = theme.md_address_bar_height().value() + theme.border_width.value();
    if with_error_row {
        band + theme.md_address_error_height().value() - theme.spacing_xs.value()
    } else {
        band
    }
}

/// 페이지 폭이 이 값보다 좁으면 필드가 `md-address-go-hide-below` 아래로 줄어든다. 그 폭에서 Go 를 숨긴다.
pub(super) fn go_hide_width_px(theme: &Theme, remote: bool) -> f32 {
    let btn = theme.icon_button_size_sm().value();
    let gap = theme.md_address_bar_gap().value();
    // 로컬은 Folder, 원격은 Folder·Reload 다.
    let nav = if remote {
        btn * 2.0 + theme.md_address_bar_nav_gap().value()
    } else {
        btn
    };
    theme.md_address_bar_padding_x().value() * 2.0
        + nav
        + gap
        + theme.md_address_go_hide_below().value()
        + gap
        + btn
}

/// 모션 감소에서 회전하는 호 대신 그리는 정적인 점 세 개(공용 Spinner 의 모션 감소 모양과 같다).
const SPINNER_DOTS: &str = ".tasty-addr-spinner svg{display:none;}.tasty-addr-spinner::before{content:\"\";display:block;width:100%;height:100%;background:radial-gradient(circle,currentColor 42%,transparent 46%) 0 50%/28% 28% no-repeat,radial-gradient(circle,currentColor 42%,transparent 46%) 50% 50%/28% 28% no-repeat,radial-gradient(circle,currentColor 42%,transparent 46%) 100% 50%/28% 28% no-repeat;}";

/// 공용 Spinner 의 SVG — viewBox 24, stroke 2, 22% 트랙 위 90° 호.
const SPINNER_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" aria-hidden="true"><circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="2" opacity="0.22"/><path d="M12 2 a 10 10 0 0 1 10 10" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>"#;

/// 아이콘 하나를 mask 로 그리는 data URI. 색은 mask 아래의 `background-color: currentColor` 가 정한다.
fn mask_uri(icon: &tasty_icons::Icon) -> String {
    alert_icon_data_uri(icon.body, icon.filled, "#000")
}

/// 주소 띠 CSS. 값은 모두 Theme 토큰에서 읽는다.
pub(super) fn addr_css(theme: &Theme) -> String {
    let edge_ring =
        |c: tasty_type_appearance::color::HexColor| c.with_alpha(FIELD_RING_ALPHA).to_hex();
    format!(
        r#"#tasty-addr-bar{{position:sticky;top:0;z-index:10;display:flex;flex-direction:column;background:{bar_bg};border-bottom:{bw}px solid {bar_border};}}
#tasty-addr-row{{display:flex;align-items:center;gap:{gap}px;height:{band_h}px;padding:0 {pad}px;box-sizing:border-box;overflow:hidden;}}
.tasty-addr-nav{{display:flex;align-items:center;gap:{nav_gap}px;flex:none;}}
.tasty-addr-tip{{display:inline-flex;flex:none;}}
.tasty-addr-btn{{width:{btn}px;height:{btn}px;display:inline-flex;align-items:center;justify-content:center;padding:0;margin:0;border:0;border-radius:{btn_r}px;background:transparent;color:{btn_fg};cursor:pointer;}}
.tasty-addr-btn:hover:not(:disabled){{background:{btn_hover};color:{btn_fg_hover};}}
.tasty-addr-btn:active:not(:disabled){{background:{btn_active};}}
.tasty-addr-btn:focus-visible{{outline:none;box-shadow:0 0 0 {ring_w}px {focus};}}
.tasty-addr-btn:disabled{{color:{disabled_fg};cursor:default;}}
#tasty-refresh[data-stale="true"]:not(:disabled){{background:{stale_bg};color:{stale_fg};}}
.tasty-addr-glyph{{display:block;width:{glyph}px;height:{glyph}px;background-color:currentColor;-webkit-mask:var(--g) center/{glyph}px {glyph}px no-repeat;mask:var(--g) center/{glyph}px {glyph}px no-repeat;}}
.tasty-addr-g-folder{{--g:url("{g_folder}");}}
.tasty-addr-g-refresh{{--g:url("{g_refresh}");}}
.tasty-addr-g-go{{--g:url("{g_go}");}}
#tasty-addr-field{{flex:1;min-width:{field_min}px;display:flex;align-items:center;gap:{in_gap}px;height:{in_h}px;box-sizing:border-box;padding:0 {space_sm}px;border-radius:{in_r}px;background:{in_bg};border:{bw}px solid {in_border};}}
#tasty-addr-field:focus-within{{border-color:{in_focus};box-shadow:0 0 0 {ring_w}px {focus_ring};}}
#tasty-addr-field.tasty-addr-invalid{{border-color:{in_invalid};box-shadow:0 0 0 {ring_w}px {invalid_ring};}}
#tasty-addr-field.tasty-addr-off{{background:{ro_bg};border-color:{ro_border};box-shadow:none;}}
.tasty-addr-kind{{flex:none;width:{kind}px;height:{kind}px;background-color:{in_icon};-webkit-mask:var(--g) center/{kind}px {kind}px no-repeat;mask:var(--g) center/{kind}px {kind}px no-repeat;}}
.tasty-addr-g-file{{--g:url("{g_file}");}}
.tasty-addr-g-remote{{--g:url("{g_remote}");}}
.tasty-addr-box{{position:relative;flex:1;min-width:0;height:100%;}}
#tasty-addr-input{{display:block;box-sizing:border-box;width:100%;height:100%;margin:0;padding:0;border:0;outline:none;background:transparent;color:{in_fg};font-family:{mono};font-size:{in_font}px;}}
#tasty-addr-input::placeholder{{color:{placeholder};}}
.tasty-addr-display{{position:absolute;inset:0;overflow:hidden;white-space:nowrap;text-overflow:ellipsis;direction:rtl;text-align:left;pointer-events:none;color:{idle_fg};font-family:{mono};font-size:{in_font}px;line-height:{line_h}px;}}
#tasty-addr-field:not(:focus-within) #tasty-addr-input{{color:transparent;}}
#tasty-addr-field:focus-within .tasty-addr-display{{visibility:hidden;}}
#tasty-addr-field.tasty-addr-invalid .tasty-addr-display{{color:{in_fg};}}
#tasty-addr-field.tasty-addr-off .tasty-addr-display{{color:{off_path_fg};}}
#tasty-addr-error{{display:flex;align-items:center;gap:{space_sm}px;height:{err_h}px;margin-top:-{space_xs}px;padding:0 {pad}px;box-sizing:border-box;}}
.tasty-addr-error-glyph{{flex:none;width:{kind}px;height:{kind}px;background-color:{err_glyph};-webkit-mask:url("{g_alert}") center/{kind}px {kind}px no-repeat;mask:url("{g_alert}") center/{kind}px {kind}px no-repeat;}}
.tasty-addr-error-text{{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:{caption}px;color:{err_fg};}}
.tasty-addr-path{{font-family:{mono};}}
#tasty-addr-field.tasty-addr-pending{{border-color:{in_focus};box-shadow:0 0 0 {ring_w}px {focus_ring};}}
#tasty-addr-field.tasty-addr-pending .tasty-addr-display{{color:{in_fg};}}
#tasty-addr-waiting[hidden]{{display:none;}}
#tasty-addr-waiting{{display:flex;align-items:center;gap:{space_sm}px;height:{err_h}px;margin-top:-{space_xs}px;padding:0 {pad}px;box-sizing:border-box;}}
.tasty-addr-waiting-text{{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:{caption}px;color:{pending_fg};}}
.tasty-addr-spinner{{flex:none;display:block;width:{kind}px;height:{kind}px;color:{spin_fg};}}
.tasty-addr-spinner svg{{display:block;width:100%;height:100%;animation:tasty-addr-spin {spin_ms}ms linear infinite;}}
@keyframes tasty-addr-spin{{to{{transform:rotate(360deg);}}}}
{spin_still}@media (prefers-reduced-motion:reduce){{{spin_dots}}}
@media (max-width:{local_hide}px){{#tasty-addr-bar[data-kind="local"] #tasty-addr-go-tip{{display:none;}}}}
@media (max-width:{remote_hide}px){{#tasty-addr-bar[data-kind="remote"] #tasty-addr-go-tip{{display:none;}}}}
"#,
        bar_bg = theme.md_address_bar_bg().to_hex(),
        bar_border = theme.md_address_bar_border().unpremultiplied().to_hex(),
        bw = theme.border_width.value(),
        gap = theme.md_address_bar_gap().value(),
        band_h = theme.md_address_bar_height().value(),
        pad = theme.md_address_bar_padding_x().value(),
        nav_gap = theme.md_address_bar_nav_gap().value(),
        btn = theme.icon_button_size_sm().value(),
        btn_r = theme.icon_button_radius().value(),
        btn_fg = theme.icon_button_fg().to_hex(),
        btn_fg_hover = theme.icon_button_fg_hover().to_hex(),
        btn_hover = theme.icon_button_overlay_hover().unpremultiplied().to_hex(),
        btn_active = theme.icon_button_bg_active().unpremultiplied().to_hex(),
        ring_w = theme.focus_ring_width.value(),
        focus = theme.border_focus().to_hex(),
        disabled_fg = theme.state_disabled_fg().to_hex(),
        off_path_fg = theme.text_muted_raised().to_hex(),
        stale_bg = theme.md_address_stale_bg().to_hex(),
        stale_fg = theme.md_address_stale_fg().to_hex(),
        glyph = theme.icon_glyph_size_md.value(),
        g_folder = mask_uri(&tasty_icons::FOLDER_OPEN),
        g_refresh = mask_uri(&tasty_icons::REFRESH),
        g_go = mask_uri(&tasty_icons::ARROW_RIGHT),
        field_min = theme.md_address_field_min_width().value(),
        in_gap = theme.input_gap().value(),
        in_h = theme.input_height().value(),
        space_sm = theme.spacing_sm.value(),
        space_xs = theme.spacing_xs.value(),
        in_r = theme.input_radius().value(),
        in_bg = theme.input_bg().to_hex(),
        in_border = theme.input_border().to_hex(),
        in_focus = theme.input_border_focus().to_hex(),
        focus_ring = edge_ring(theme.border_focus()),
        in_invalid = theme.input_border_invalid().to_hex(),
        invalid_ring = edge_ring(theme.input_border_invalid()),
        ro_bg = theme.input_readonly_bg().to_hex(),
        ro_border = theme.input_readonly_border().to_hex(),
        kind = theme.icon_glyph_size_sm.value(),
        in_icon = theme.input_icon_fg().to_hex(),
        g_file = mask_uri(&tasty_icons::FILE),
        g_remote = mask_uri(&tasty_icons::REMOTE),
        in_fg = theme.input_fg().to_hex(),
        mono = MONO_STACK,
        in_font = theme.input_font_size().value(),
        placeholder = theme.input_placeholder().to_hex(),
        idle_fg = theme.md_address_value_fg().to_hex(),
        line_h = theme.input_height().value() - theme.border_width.value() * 2.0,
        err_h = theme.md_address_error_height().value(),
        err_glyph = theme.md_address_error_glyph().to_hex(),
        g_alert = mask_uri(&tasty_icons::ALERT_CIRCLE),
        caption = theme.font_size_caption.value(),
        err_fg = theme.text_primary().to_hex(),
        pending_fg = theme.md_address_pending_fg().to_hex(),
        spin_fg = theme.text_muted().to_hex(),
        spin_ms = theme.spinner_duration().0,
        spin_still = if theme.reduced_motion {
            SPINNER_DOTS
        } else {
            ""
        },
        spin_dots = SPINNER_DOTS,
        // 경계 폭에서는 필드가 꼭 하한이라 Go 를 그대로 둔다. 소수 폭 viewport 를 위해 0.02px 뺀다.
        local_hide = go_hide_width_px(theme, false) - 0.02,
        remote_hide = go_hide_width_px(theme, true) - 0.02,
    )
}

/// 버튼 하나. 툴팁은 감싼 span 에 둔다 — disabled 버튼은 마우스 이벤트를 받지 않아 자기 title 을 보이지 않는다.
/// Linux(WebKitGTK) 측정에서 네이티브 title 에는 `lang` 이 닿지 않아 툴팁의 한자 형태가 OS 로케일을 따랐다([`super::ui_lang`]).
fn button(id: &str, glyph: &str, label: &str, tip: &str, off: bool, extra: &str) -> String {
    format!(
        r#"<span class="tasty-addr-tip" id="{id}-tip" title="{tip}"><button id="{id}" class="tasty-addr-btn" type="button" aria-label="{label}"{extra}{disabled}><span class="tasty-addr-glyph tasty-addr-g-{glyph}"></span></button></span>"#,
        tip = attr_escape(tip),
        label = attr_escape(label),
        disabled = if off { " disabled" } else { "" },
    )
}

/// 경로 입력, 최근 경로(로컬만)와 원격 문서 새로고침 버튼을 포함한 주소 띠.
pub(super) fn addr_bar_html(
    tr: &Translator,
    file_path: &str,
    recent: &[String],
    remote: Option<&RemoteView>,
) -> String {
    let lang = ui_lang::attr(tr);
    let off = remote.is_some_and(|r| r.disconnected);
    let failure = remote.and_then(|r| r.addr_failure.as_ref());
    let waiting = remote
        .and_then(|r| r.addr_waiting.as_ref())
        .filter(|_| failure.is_none() && !off);
    let value = failure
        .map(|f| f.typed.as_str())
        .or(waiting.map(|w| w.typed.as_str()))
        .unwrap_or(file_path);
    let tip = |label: &str| {
        if off {
            tr.t("markdown.address.disconnected").to_string()
        } else {
            label.to_string()
        }
    };

    let open = if remote.is_some() {
        tr.t("markdown.address.open_remote")
    } else {
        tr.t("markdown.address.open")
    };
    let mut nav = button("tasty-browse", "folder", open, &tip(open), off, "");
    if let Some(view) = remote {
        let (label, hint) = if view.stale {
            (
                tr.t("markdown.address.reload"),
                tr.t("markdown.address.stale"),
            )
        } else {
            (
                tr.t("markdown.address.reload"),
                tr.t("markdown.address.reload"),
            )
        };
        nav.push_str(&button(
            "tasty-refresh",
            "refresh",
            label,
            &tip(hint),
            off,
            &format!(r#" data-stale="{}""#, view.stale),
        ));
    }
    let nav = format!(r#"<div class="tasty-addr-nav">{nav}</div>"#);

    let field_class = match (off, failure.is_some(), waiting.is_some()) {
        (true, _, _) => " class=\"tasty-addr-off\"",
        (false, true, _) => " class=\"tasty-addr-invalid\"",
        (false, false, true) => " class=\"tasty-addr-pending\"",
        (false, false, false) => "",
    };
    // 로컬만 이 머신의 최근 목록을 붙인다. 원격 경로 칸에 로컬 경로를 제안하면 안 된다.
    let (list_attr, datalist, placeholder) = if remote.is_some() {
        (String::new(), String::new(), String::new())
    } else {
        let options: String = recent
            .iter()
            .map(|p| format!(r#"<option value="{}"></option>"#, attr_escape(p)))
            .collect();
        (
            r#" list="tasty-addr-recent""#.to_string(),
            format!(r#"<datalist id="tasty-addr-recent">{options}</datalist>"#),
            format!(
                r#" placeholder="{}""#,
                attr_escape(tr.t("markdown.addr.placeholder"))
            ),
        )
    };
    // placeholder 와 값은 한 요소라 나눌 수 없다. 앱 문구인 placeholder 를 위해 입력 칸에 언어를 붙인다.
    // 경로만 보이는 표시 요소에는 붙이지 않는다.
    let field = format!(
        r#"<div id="tasty-addr-field"{field_class}><span class="tasty-addr-kind tasty-addr-g-{kind}"></span><span class="tasty-addr-box"><input id="tasty-addr-input"{lang} value="{value}" spellcheck="false" autocomplete="off"{list_attr}{placeholder}{readonly}{invalid}><span class="tasty-addr-display" aria-hidden="true"><bdi>{display}</bdi></span></span>{datalist}</div>"#,
        kind = if remote.is_some() { "remote" } else { "file" },
        value = attr_escape(value),
        readonly = if off { " readonly" } else { "" },
        invalid = if failure.is_some() {
            r#" aria-invalid="true""#
        } else {
            ""
        },
        display = html_escape(value),
    );
    let go = button(
        "tasty-addr-go",
        "go",
        tr.t("markdown.address.go"),
        &tip(tr.t("markdown.address.go")),
        off,
        "",
    );
    let error = failure.map_or_else(String::new, |f| {
        let (text, tip) = failure_sentence(tr, f);
        format!(
            r#"<div id="tasty-addr-error" role="alert"><span class="tasty-addr-error-glyph"></span><span class="tasty-addr-error-text" title="{tip}">{text}</span></div>"#,
            tip = attr_escape(&tip),
        )
    });
    // 원격 문서는 대기 행을 숨겨 두고 페이지가 이동을 보낸 뒤 시간이 지나면 보인다.
    let pending = match remote {
        Some(_) if !off && failure.is_none() => {
            let shown = waiting.is_some_and(|w| w.after_ms == 0);
            format!(
                r#"<div id="tasty-addr-waiting" role="status"{hidden}><span class="tasty-addr-spinner">{SPINNER_SVG}</span><span class="tasty-addr-waiting-text"{lang}>{text}</span></div>"#,
                hidden = if shown { "" } else { " hidden" },
                text = html_escape(tr.t("markdown.address.waiting")),
            )
        }
        _ => String::new(),
    };
    let wait_attr = waiting
        .filter(|w| w.after_ms > 0)
        .map_or_else(String::new, |w| {
            format!(r#" data-wait-ms="{}""#, w.after_ms)
        });
    format!(
        r#"<div id="tasty-addr-bar" data-kind="{kind}"{wait_attr}><div id="tasty-addr-row">{nav}{field}{go}</div>{error}{pending}</div>"#,
        kind = if remote.is_some() { "remote" } else { "local" },
    )
}

/// 오류 행 문장의 (HTML, 툴팁 평문). 경로는 고정폭으로, 원격 사유는 번역하지 않은 영어 원문으로 넣는다.
/// 경로와 사유가 문장 안에 있으므로 언어는 번역 조각에만 하나씩 붙인다.
fn failure_sentence(tr: &Translator, failure: &AddrFailure) -> (String, String) {
    let lang = ui_lang::attr(tr);
    let template = tr.t(failure.kind.key());
    let reason = match &failure.kind {
        AddrFailureKind::Rejected(reason) => reason.as_str(),
        _ => "",
    };
    let mut html = String::new();
    let mut plain = String::new();
    let mut rest = template;
    while let Some(at) = rest.find('{') {
        let (head, tail) = rest.split_at(at);
        html.push_str(&ui_lang::text_html(head, &lang));
        plain.push_str(head);
        if let Some(after) = tail.strip_prefix("{path}") {
            html.push_str(&format!(
                r#"<span class="tasty-addr-path">{}</span>"#,
                html_escape(&failure.typed)
            ));
            plain.push_str(&failure.typed);
            rest = after;
        } else if let Some(after) = tail.strip_prefix("{reason}") {
            html.push_str(&format!(
                r#"<span lang="en">{}</span>"#,
                html_escape(reason)
            ));
            plain.push_str(reason);
            rest = after;
        } else {
            html.push('{');
            plain.push('{');
            rest = &tail[1..];
        }
    }
    html.push_str(&ui_lang::text_html(rest, &lang));
    plain.push_str(rest);
    (html, plain)
}

/// 오류 행이나 보이는 대기 행이 있으면 `<html>` 에 실을 높이 재정의. 행이 사라지면 스크립트가 지운다.
/// 오류 행은 끊긴 문서에도 남는다(결과 전에 연결이 끊긴 실패가 바로 그 경우다). 대기 행은 끊기면 없다.
pub(super) fn html_style_attr(theme: &Theme, remote: Option<&RemoteView>) -> String {
    let has_row = remote.is_some_and(|r| {
        r.addr_failure.is_some()
            || (!r.disconnected && r.addr_waiting.as_ref().is_some_and(|w| w.after_ms == 0))
    });
    if has_row {
        format!(
            r#" style="--md-addr-bar-h:{}px""#,
            bar_height_px(theme, true)
        )
    } else {
        String::new()
    }
}
