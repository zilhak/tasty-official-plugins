//! 호스트의 바탕 포커스 시트가 실제 WebKitGTK 문서에서 값을 바꾸는지 잰다(Linux 전용).
//! 디스플레이가 필요해 `#[ignore]`로 두고, CI 단계가 이름으로 부른다(`docs/dev-guide/ci-gates.md`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use tasty_type_appearance::theme::Theme;
use webkit2gtk::{LoadEvent, WebView, WebViewExt as _};

use super::webview_layout_tests::{Page, VIEW_H, VIEW_W, document, long_source, spin_until};
/// Tasty 본체가 바탕 포커스 상태인 동안 넣는 시트와 같은 문자열. 계약 속성을 빈 값으로 정의한다.
fn bed_focus_sheet() -> webkit2gtk::UserStyleSheet {
    webkit2gtk::UserStyleSheet::new(
        &format!(":root{{{}: ;}}", super::bed_focus::BED_FOCUSED_PROPERTY),
        webkit2gtk::UserContentInjectedFrames::TopFrame,
        webkit2gtk::UserStyleLevel::User,
        &[],
        &[],
    )
}

/// 엔진이 시트 변경을 반영할 시간을 준다. 계산값은 다음 질의가 동기로 다시 계산한다.
fn pump() {
    for _ in 0..50 {
        gtk::main_iteration_do(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// 보이는 영역을 찍어 (x, y) 픽셀을 `#rrggbb`로 읽는다. 스크립트 없이 읽으므로 JS를 끈 문서에도 쓴다.
fn snapshot_pixel(view: &WebView, x: i32, y: i32) -> String {
    use gtk::cairo;
    let out: Rc<RefCell<Option<Result<cairo::Surface, String>>>> = Rc::new(RefCell::new(None));
    let slot = out.clone();
    view.snapshot(
        webkit2gtk::SnapshotRegion::Visible,
        webkit2gtk::SnapshotOptions::NONE,
        None::<&gtk::gio::Cancellable>,
        move |r| *slot.borrow_mut() = Some(r.map_err(|e| e.to_string())),
    );
    spin_until(|| out.borrow().is_some(), "snapshot");
    let surface = out
        .borrow_mut()
        .take()
        .expect("filled above")
        .unwrap_or_else(|e| panic!("snapshot: {e}"));
    let image = cairo::ImageSurface::try_from(surface).expect("image surface");
    let stride = image.stride() as usize;
    let mut px = String::new();
    image
        .with_data(|data| {
            // ARGB32은 little-endian에서 B, G, R, A 순서다. 바탕은 불투명이라 premultiply가 값을 바꾸지 않는다.
            let i = y as usize * stride + x as usize * 4;
            px = format!("#{:02x}{:02x}{:02x}", data[i + 2], data[i + 1], data[i]);
        })
        .expect("image data");
    px
}

fn css_rgb(hex: tasty_type_appearance::color::HexColor) -> String {
    let h = hex.to_hex();
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).expect("hex");
    format!("rgb({}, {}, {})", c(1), c(3), c(5))
}

/// 시트를 넣고 빼면 바탕·표 칸·zebra 줄이 정본 값으로 바뀌고, 문서를 다시 싣지 않아 스크롤과 선택이
/// 그대로다. 글자와 머리 칸은 바뀌지 않는다. JS를 끈 문서에서도 시트로 바탕이 바뀐다(픽셀로 잰다).
/// 시트를 먼저 넣고 실은 문서는 첫 계산값부터 focus 값이다. GTK는 한 프로세스에서 한 스레드만
/// 초기화하므로 한 시험에 모았다.
/// 실행: `xvfb-run -a cargo test -p tasty-plugin-markdown -- --ignored --exact render::bed_focus_webkit_tests::bed_focus_sheet_switches_state_values_in_place`
#[test]
#[ignore = "WebKitGTK 를 띄울 디스플레이가 필요하다 — CI 단계가 xvfb-run 안에서 이름으로 실행한다"]
fn bed_focus_sheet_switches_state_values_in_place() {
    gtk::init().expect("GTK 초기화");
    use webkit2gtk::{SettingsExt as _, UserContentManagerExt as _};
    let theme = Theme::with_colors_and_zoom(tasty_themes::mocha_fallback_colors(), false, 1.0);
    let md = theme.surface("markdown");
    let window = gtk::OffscreenWindow::new();
    window.set_default_size(VIEW_W, VIEW_H);
    let view = WebView::new();
    window.add(&view);
    window.show_all();
    view.user_content_manager()
        .expect("ucm")
        .add_style_sheet(&bed_focus_sheet());
    let finished = Rc::new(Cell::new(false));
    let flag = finished.clone();
    view.connect_load_changed(move |_, e| {
        if e == LoadEvent::Finished {
            flag.set(true);
        }
    });
    view.load_html(&document("# Title\n\nBody."), None);
    spin_until(|| finished.get(), "문서 적재");
    let first = Page {
        _window: window,
        view,
    };
    assert_eq!(
        first.string("return getComputedStyle(document.body).backgroundColor;"),
        css_rgb(theme.surface("markdown").focused_bg),
        "시트를 먼저 넣고 실은 문서는 첫 계산값부터 focus 값이다"
    );
    drop(first);

    let mut source = String::from("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n\n");
    source.push_str(&long_source());
    let page = Page::load(&document(&source));
    let loads = Rc::new(Cell::new(0u32));
    let count = loads.clone();
    page.view.connect_load_changed(move |_, e| {
        if e == LoadEvent::Started {
            count.set(count.get() + 1);
        }
    });

    let read = |p: &Page| {
        [
            "return getComputedStyle(document.body).backgroundColor;",
            "return getComputedStyle(document.querySelector('table')).backgroundColor;",
            "return getComputedStyle(document.querySelector('tbody tr:nth-child(2)')).backgroundColor;",
            "return getComputedStyle(document.querySelector('th')).backgroundColor;",
            "return getComputedStyle(document.querySelector('td')).color;",
        ]
        .map(|s| p.string(s))
    };
    let unfocused = [
        css_rgb(md.unfocused_bg),
        css_rgb(theme.md_table_row_bg_unfocused()),
        css_rgb(theme.md_table_row_bg_zebra_unfocused()),
        css_rgb(theme.md_table_header_bg()),
        css_rgb(theme.md_table_cell_fg()),
    ];
    let focused = [
        css_rgb(md.focused_bg),
        css_rgb(theme.md_table_row_bg()),
        css_rgb(theme.md_table_row_bg_zebra()),
        css_rgb(theme.md_table_header_bg()),
        css_rgb(theme.md_table_cell_fg()),
    ];
    assert_eq!(read(&page), unfocused, "표지가 없으면 unfocus 값");

    page.number("window.scrollTo(0, 1500); return window.scrollY;");
    let selected = page.string(
        "const p=document.querySelectorAll('#tasty-md-body p')[30]; const r=document.createRange(); \
         r.selectNodeContents(p); getSelection().removeAllRanges(); getSelection().addRange(r); \
         return getSelection().toString();",
    );
    let scroll = page.number("return window.scrollY;");

    let ucm = page.view.user_content_manager().expect("ucm");
    let sheet = bed_focus_sheet();
    ucm.add_style_sheet(&sheet);
    pump();
    assert_eq!(read(&page), focused, "표지가 있으면 focus 값");
    assert_eq!(page.number("return window.scrollY;"), scroll);
    assert_eq!(page.string("return getSelection().toString();"), selected);

    ucm.remove_style_sheet(&sheet);
    pump();
    assert_eq!(read(&page), unfocused);
    assert_eq!(page.number("return window.scrollY;"), scroll);
    assert_eq!(page.string("return getSelection().toString();"), selected);

    // JS를 끈 문서. 계산값은 스크립트로 못 읽어 왼쪽 여백의 바탕 픽셀을 찍는다.
    let settings = webkit2gtk::WebViewExt::settings(&page.view).expect("settings");
    settings.set_enable_javascript(false);
    pump();
    let edge = (2, VIEW_H - 4);
    assert_eq!(
        snapshot_pixel(&page.view, edge.0, edge.1),
        md.unfocused_bg.to_hex()
    );
    ucm.add_style_sheet(&sheet);
    pump();
    assert_eq!(
        snapshot_pixel(&page.view, edge.0, edge.1),
        md.focused_bg.to_hex(),
        "JS를 꺼도 시트로 바뀐다"
    );
    assert_eq!(loads.get(), 0, "문서를 다시 싣지 않는다");
}
