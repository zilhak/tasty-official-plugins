# Clipboard Viewer popup 화면

- **부모 기획**: [../index.md](../index.md)
- **시각 소스**: tasty-page `vendor/ui_kits/terminal/overlays/clipboard_viewer.jsx`.

도구 메뉴/단축키로 뜨는 클립보드 뷰어 popup. header → type-bar → body → footer 4단 수직 배치.

## 트리거

[도구 메뉴](https://github.com/zilhak/tasty/blob/develop/docs/features/tools-menu/index.md#화면) `Clipboard Viewer` 또는 플러그인 커맨드 `open_viewer`(설정 > 단축키 > 플러그인, 기본값 `ctrl+shift+h`).

## UI 요소 인벤토리

- **header** — 클립보드 아이콘 + "Clipboard" 타이틀(14px/600) + `snapshot` 뱃지(default tag) + 우측 close IconButton.
- **type-bar** — 좌측 [`type_switch`]: 가용 타입이 1개면 아이콘 + accent 뱃지(읽기전용), 2개 이상이면 가로 세그먼트 버튼 그룹(rail 없음, 갤러리와 같은 공용 view `tasty_ui_widgets::draw_type_segments`). 5개 이상(`SEG_COMPACT_AT`)이면 비활성 세그먼트가 아이콘 전용으로 압축되고 hover 시 전체 타입명 툴팁이 뜬다. Other 세그먼트/뱃지의 hover 툴팁은 기본 라벨("Other") 대신 "{n} unrecognized formats"(발견된 포맷 개수)를 보여준다. 우측 슬롯은 HTML 타입일 때만 "Pretty print" `Checkbox`(`tasty_ui_widgets::checkbox`)로 스왑되고, 다른 타입은 빈 슬롯.
- **body** — well(border+radius+bg-app) 안에 타입별 콘텐츠.
  Text 는 mono pre 스크롤(`well`). 본문은 공용 선택 가능한 표시 텍스트(`tasty_ui_widgets::selectable_text`)라 드래그 선택·복사만 되고 캐럿·키 입력은 없다.
  Files 는 아이콘+mono 경로 한 줄씩(긴 경로는 말줄임, `well` 스크롤).
  Image 는 **인라인 렌더링 없음**(design 결정) — well 을 상하좌우 중앙 정렬로 바꿔(`well_centered`) 아이콘(28px, CenterState 와 같은 값) + 치수·용량 메타(mono caption) + "인라인 미리보기 없음" 안내(caption, italic, `text-disabled`)만 표시한다.
  HTML(렌더링 없이 원본 소스 또는 prettify 결과를 동일 스타일로 표시).
  Other(text/files/image/html 이 아닌 raw 포맷 전부를 `well` 스크롤 안에 포맷별 블록으로 나열, 블록마다 이름(mono caption, 굵게, `text-secondary`)+크기(mono caption, `text-muted`)를 같은 줄에, 그 아래 텍스트화된 미리보기(mono term-sm, `text-primary`)를 표시, 블록 사이 1px `separator`, 길면 `+N more lines` 절삭 — 목록 자체는 접지 않는다).
- **footer** — mime 텍스트(mono caption, 좌, HTML 타입은 `{mime} · {n} chars · {n} line(s)` 로 메타 결합, Other 타입은 mime 이 없어 "{n} unrecognized formats" 가 그 자리를 통째로 대체) + Close 버튼(secondary, 우). 바깥 클릭과 Esc로도 닫을 수 있다.
- (빈 상태) 아이콘 + 굵은 타이틀 + 옅은 부제 2줄.
- (읽기 실패) 위와 동일 구조, danger 톤.
- (이미 열림) 위와 동일 구조, lock 아이콘.

## 상태별 시각

- 타입 있음(header+type-bar+body+footer 4단) / 빈 클립보드 / 읽기 실패 / 이미 열림(재호출 무시) — 후자 3개는 header 는 그대로 유지하고 본문만 CenterState(아이콘+타이틀+부제)로 교체된다.

## 렌더 경로

popup 은 **egui-mesh**(docs/dev-guide/egui-mesh-channel.md)로 그린다. plugin 이 자기 프로세스에서 popup 콘텐츠를
egui 로 tessellate 한 mesh 를 host 가 content 영역에 합성한다. host 는 `popup.set_context` 에
Theme 스냅샷(`ThemeWire`)을 실어 보내고, plugin 은 `Theme::with_colors_and_zoom` 으로 재구성해
디자인 토큰대로 그린다. chrome(scrim/border/outside-click/Esc/단일 인스턴스 셸)은 host 소유 —
plugin 은 header~footer content 영역만 그린다(`cbFrame`/`Scrim` 은 design 의 standalone 프리뷰
전용 목업이며 plugin 이 다시 그리지 않는다).

헤더/푸터의 Close 버튼 클릭은 `view::draw`/`draw_already_open` 이 `bool` 로 반환하고,
`main.rs` 가 그 값을 보고 `popup.close` IPC 로 host 에 닫기를 요청한다(host 가 셸 생애주기를
계속 소유 — [popup-implementation.md](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/popup-implementation.md)).

아이콘은 빌드타임 SVG→벡터 베이크(`build.rs`, `tasty-plugin-image/build.rs` 정본 패턴)로
`tasty_plugin_sdk::baked_icon::draw` 가 그린다.

## 디자인 토큰 매핑

시각 수치·토큰의 단일 출처는 tasty-page `vendor/ui_kits/terminal/overlays/clipboard_viewer.jsx` 다 — [시각 소스](#시각-소스).

## HTML prettify 인덴터

`crates/tasty-plugin-clipboard-viewer/src/html_format.rs::prettify` — 정규 HTML5 파서가 아니라
태그 깊이를 세는 휴리스틱 토크나이저(새 외부 의존성 없음). Design System 시안이 검증한 참조
알고리즘(`>\s+<` 공백 정규화 → `<...>` 태그 경계 split → 닫는 태그는 먼저 depth 감소, 여는
태그는 출력 후 depth 증가, void element/self-closing 은 증가 없음)을 그대로 포팅했다. 단
`<script>`/`<style>`/`<pre>` 내부는 별도로 완전한 span 을 추출해 verbatim 보존한다 — 참조
알고리즘을 문자 그대로 이식하면 내부에 `<`/`>` 가 섞인 스크립트에서 원본이 깨지는 사례가
있어(예: `if (a < b)`), 이 부분만 명시적 예외로 대체했다. Display-only re-indenter —
DOM 파싱/sanitize/render 는 하지 않는다. malformed 입력(닫히지 않은 태그 등)에도 panic 없이
최선의 결과를 낸다(idempotence 포함 단위 테스트로 보장).

## Other raw 포맷 열거

`crates/tasty-plugin-clipboard-viewer/src/raw_formats/{mod,windows,macos,x11}.rs` — arboard 는
클립보드 포맷 열거를 노출하지 않는다(`arboard::Error::ContentNotAvailable` doc comment 가
"비어있음"과 "이 4개(text/files/image/html)가 아닌 포맷"을 구분하지 않는다고 명시). 플랫폼별로
raw API 를 직접 호출해 나머지를 열거한다:

- **Windows** — `clipboard-win` 의 `EnumFormats`(전체 포맷 ID 열거) + `format_name_big`(사람이
  읽는 이름) + `get_vec`(임의 포맷 raw 바이트). `CF_TEXT`/`CF_UNICODETEXT`/`CF_OEMTEXT`(텍스트
  변형 전부) · `CF_HDROP`(files) · `CF_DIB`/`CF_DIBV5`+등록 포맷 이름 "PNG"(image) · 등록 포맷
  이름 "HTML Format"(html) 을 제외.
- **macOS** — `NSPasteboard.types()`(전체 UTI 배열) + `dataForType:`(임의 타입 raw 바이트).
  `public.utf8-plain-text` 등 텍스트 변형 · `public.file-url`(files) · `public.tiff`/`public.png`
  등 이미지 변형 · `public.html` 을 제외.
- **Linux(X11/XWayland)** — ICCCM 표준 절차를 x11rb 로 직접 구현: `TARGETS` atom 을
  `ConvertSelection` 으로 요청 → `SelectionNotify` 응답 → `GetProperty` 로 지원 atom 목록 회수,
  각 atom 을 같은 절차로 재조회. `UTF8_STRING`/`STRING`/`TEXT`/`text/plain*` · `text/uri-list`
  (files) · `image/png` · `text/html` 을 제외. `wayland-data-control` feature 를 켜지 않아(다른
  타입도 이미 이 경로) 순수 Wayland(XWayland 미실행) 세션에서는 연결 자체가 실패 — 빈 벡터 +
  `tracing::debug!` 로 원인을 남겨 "기타 없음"과 "조회 못 함"을 구분한다.
- **공통** — 세 플랫폼 다 제외는 단일 ID/이름 비교가 아니라 매핑 테이블(같은 semantic 타입의
  여러 raw 변형을 전부 나열)로 한다 — 브라우저가 서식 있는 텍스트를 복사할 때 text/html 이
  동시에 클립보드에 오르는 것처럼, 같은 의미의 포맷이 여러 raw ID/이름으로 동시에 존재할 수
  있어서다. TARGETS(또는 EnumFormats/types) 조회와 개별 포맷 재조회 사이 클립보드 소유자가
  바뀌는 race 는 그 포맷 하나만 건너뛰는 개별 격리로 처리(전체 "기타" 열거를 실패시키지 않음).
  raw 바이트 → 텍스트화는 `clipboard::OtherFormatEntry::from_bytes`(`from_utf8_lossy` + 크기
  상한 + U+FFFD 비율 기반 바이너리 판단 시 hex 요약 fallback) 하나로 3플랫폼이 공유하며, raw
  바이트 내용 자체는 어떤 경로로도 로그에 남기지 않는다.

## 갤러리 specimen

`crates/tasty-gallery/src/catalog/components/clipboard_viewer.rs` — Overlays › `Clipboard viewer
popup`. header/type-bar(배지)/body(well)/footer 4행(text) + header/type-bar(Text/Files 세그먼트)/
body(아이콘+경로 행)/footer 4행(files) + image 상태(아이콘+메타+안내문구) + HTML raw/pretty 2행
(type-bar 우측 Pretty print 체크박스 포함) + other 상태(포맷 블록 2개 나열 —
하나는 짧은 텍스트, 하나는 `+N more lines` 절삭 예시) + empty/read-failed/already-open 3
CenterState 를 토큰으로 전사(본체/plugin crate 비의존, 픽셀 동일성 비목표). HTML pretty 상태의
인덴트 결과와 other 상태의 포맷 블록 샘플은 각각 `html_format::prettify()` /
`clipboard::OtherFormatEntry` 와 동일 규칙으로 수기 정리한 샘플이다(갤러리는 plugin crate 를
의존할 수 없다). 압축 세그먼트(타입 5종이 동시에 살아 있어 비활성 세그먼트가 아이콘 전용으로 줄어든 상태)도
specimen 에 있다. 디자인·갤러리·제품 구현의 대응표:
[design-gallery-mapping.md](https://github.com/zilhak/tasty/blob/develop/docs/design/systems/design-gallery-mapping.md#clipboard-viewer-plugins).

## 시각 소스

tasty-page `vendor/ui_kits/terminal/overlays/clipboard_viewer.jsx`(구조 전사 소스) ·
`clipboard_viewer.html`(standalone 프리뷰) · `shared.jsx`(`Scrim`/`Icon`/`Spinner` 공용
프리미티브). popup 은 egui-mesh 채널로 plugin 이 자가 렌더한다
([popup-implementation.md](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/popup-implementation.md), docs/dev-guide/egui-mesh-channel.md).
