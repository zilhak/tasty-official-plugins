# Clipboard Viewer (`com.tasty.clipboard-viewer`)

- **Status**: Implemented (bundled plugin)
- **주체**: 로컬 사용자 (도구 메뉴 / 단축키 → popup)
- **배포/통합**: bundled · 도구 메뉴 항목 + popup — [plugins 개념](https://github.com/zilhak/tasty/blob/develop/docs/concepts/plugins.md)
- **코드**: `crates/tasty-plugin-clipboard-viewer/`
- **권한**: `ui.tool_item` · `ui.popup` · `clipboard.read`
- **화면**: [screens/clipboard-viewer.md](screens/clipboard-viewer.md)

> **예제로서**: **도구 메뉴 항목 + popup** 예제. 클립보드를 host 백엔드 없이 **plugin 프로세스가 `arboard` 로 직접 read** 하는 [플러그인 권한 모델](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-permissions.md) 비-샌드박스 모델의 레퍼런스 → [plugin-development](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#도구-메뉴-항목--popup).

## 목적

현재 시스템 클립보드의 내용을 **타입별로 분류해 미리보기**하는 read-only popup 을 제공한다.
히스토리(과거 항목 누적)는 다루지 않는다 — 지금 클립보드에 무엇이 들어 있는지 보여줄 뿐이다.
타입에 따라 미리보기 방식이 다르다 — Text 는 본문을 그대로 보여주지만, Image 는 **인라인 렌더링을 하지 않는다**(design-system 이 명시적으로 내린 결정): 아이콘 + 치수/용량 메타 + "인라인 미리보기 없음" 안내 문구만 표시한다.
Text/Files/Image/HTML 어디에도 속하지 않는 나머지 raw 포맷은 **"Other"(기타) 버킷** 하나로 묶여 표시된다 — arboard 는 클립보드 포맷 열거 자체를 노출하지 않아(`Error::ContentNotAvailable`가 "비어있음"과 "이 4 개가 아닌 포맷"을 구분하지 않는다), plugin 이 플랫폼 raw API(Windows `clipboard-win`/macOS `objc2-app-kit`/Linux `x11rb` `TARGETS`)로 직접 열거해 text/files/image/html 로 이미 소비된 변형(단일 ID 비교가 아니라 플랫폼별 매핑 테이블)을 제외한 나머지를 raw 텍스트로 대략 보여준다.

## 내부 동작

- **tool** `open-viewer` — [도구 메뉴](https://github.com/zilhak/tasty/blob/develop/docs/features/tools-menu/index.md)에 항목 추가(`ui.tool_item`), action `open_popup{com.tasty.clipboard-viewer/viewer}`.
- **command** `open_viewer` — 단축키로도 뷰어를 연다(`scope = "global"`, 기본값 `ctrl+shift+h` — 설정 > 단축키 > 플러그인에서 변경 가능). action 은 tool 항목과 동일한 `open_popup{com.tasty.clipboard-viewer/viewer}`.
- **popup** `viewer` — trigger `ipc`. header(아이콘+타이틀+snapshot 뱃지+close) → type-bar(타입 1 개면 아이콘+뱃지, 2 개 이상이면 가로 세그먼트 스위치) → body(well 스크롤 미리보기) → footer(mime[+meta]+Close) 4단 구조.
- popup open 시 `arboard::Clipboard` 로 클립보드를 1 회 읽어 사용 가능한 타입 목록을 만든다. 현재 Text/Files/Image/HTML/Other 타입이 채워진다(Files 는 `arboard::Get::file_list()`, Image 는 `arboard::Clipboard::get_image()` 로 치수(width/height)와 바이트 수만 보존하고 픽셀 데이터 자체는 들고 있지 않는다, 렌더링을 안 하므로 필요 없다, HTML 은 `arboard::Clipboard::get().html()`, Other 는 arboard 를 거치지 않고 `raw_formats` 모듈이 플랫폼 raw API 로 직접 열거). 타입 선택 → body 미리보기 갱신.
- **HTML 타입**: 렌더링하지 않고 원본 소스를 text 타입과 동일한 mono 텍스트로 보여준다(기본 상태). type-bar 우측 슬롯에 "Pretty print" 체크박스가 뜨며, 체크하면 `html_format::prettify()`(새 의존성 없는 태그 깊이 인덴터, `<script>`/`<style>`/`<pre>` 는 원본 그대로 보존)를 거친 결과로 바뀐다. 체크 상태는 popup 인스턴스 생존 동안만 유지(닫으면 리셋, 설정에 영속화하지 않음). 밀려난 메타(문자수·줄수)는 푸터에서 `{mime} · {meta}` 로 결합 표시.
- **Other 타입**: text/files/image/html 어디에도 안 걸린 raw 포맷 전부를 하나의 타입으로 묶는다. body 는 발견된 포맷마다 이름(mono, 굵게)+크기(mono, muted)를 같은 줄에, 그 아래 raw 바이트를 텍스트화한 미리보기(`from_utf8_lossy` + 크기 상한 + 바이너리 판단 시 hex 요약 fallback)를 보여주는 블록을 세로로 나열하고, 블록 사이는 1px separator 로 구분한다 — 목록 자체(포맷 개수)는 접지 않는다. 미리보기가 길면 `+N more lines` 로 절삭한다. 포맷별 조회 실패는 개별 격리(하나 실패해도 나머지는 정상 표시)되고, raw 바이트 내용은 로그에 남기지 않는다. 순수 Wayland(XWayland 미실행) 세션에서 X11 연결 자체가 실패하면 빈 목록 + debug 로그로 구분한다(조용히 빈 목록만 나오면 "기타 없음"과 "조회 못 함"이 혼동된다). footer 는 mime 자리에 "{n} unrecognized formats" 문구가 대신 들어간다(여러 이종 포맷을 묶은 버킷이라 단일 mime 이 없음).
- **단일 인스턴스**: 이미 열려 있으면 재호출은 무시(`already_open`).

## 인터페이스

- **사용자**: 도구 메뉴 `Clipboard Viewer` 또는 설정 > 단축키 > 플러그인에서 지정한 단축키 → popup. type-bar 에서 타입 선택 → body 에서 내용 확인.
- **AI Agent**: 단발 클립보드 읽기/쓰기는 host 가 아닌 각 에이전트 프로세스의 직접 접근 영역이다(docs/dev-guide/plugin-permissions.md). 이 plugin 은 IPC 네임스페이스를 노출하지 않는 순수 뷰어다.

`clipboard.read` 선언은 이 직접 OS 읽기에 대한 의도 표시이며 호스트 API 권한으로 OS 접근을 차단하지는 않는다.
호스트의 polling·history DB·clipboard.copied 이벤트와 clipboard history CLI/IPC는 제공하지 않는다.
샌드박스나 history 기능을 새로 도입할 때 직접 접근과 보존 범위를 다시 정한다.

## 비-목표

- 클립보드 **히스토리** 수집·재복사. 이 plugin 은 *현재* 클립보드 표시만 한다.
- 클립보드 **쓰기/편집** — read-only.
- 도구 메뉴 자체 — [tools-menu](https://github.com/zilhak/tasty/blob/develop/docs/features/tools-menu/index.md).

## Acceptance Criteria

- Given 플러그인 활성 Then 도구 메뉴에 `Clipboard Viewer` 항목이 보인다.
- Given 단축키(플러그인 커맨드 `open_viewer`) Then 뷰어 popup 이 열린다.
- Given 클립보드에 텍스트가 있음 Then type-bar 에 text 타입 뱃지가 보이고 body 에 내용이 미리보기된다.
- Given 클립보드에 파일(경로 목록)이 있음 Then type-bar 에 files 타입이 보이고 선택 시 body 에 아이콘+경로 목록이 한 줄씩 표시된다.
- Given 클립보드에 이미지가 있음 Then type-bar 에 image 타입이 노출되고, 선택 시 body 에 아이콘 + 치수/용량 메타 + "인라인 미리보기 없음" 안내 문구가 보인다(실제 그림은 렌더링하지 않음).
- Given 클립보드에 HTML 이 있음 Then type-bar 에 HTML 타입이 보이고, body 에 렌더링되지 않은 원본 소스가 표시되며, type-bar 우측에 "Pretty print" 체크박스가 뜬다.
- Given HTML 타입에서 "Pretty print" 체크 Then body 가 들여쓰기 적용된 형태로 바뀌고, 체크 해제 시 원본으로 돌아온다. 푸터에 `{mime} · {n} chars · {n} line(s)` 가 표시된다.
- Given 클립보드에 text/files/image/html 어디에도 속하지 않는 raw 포맷이 있음(예: 특정 앱 전용 커스텀 포맷) Then type-bar 에 "Other" 타입이 나타나고, 선택 시 body 에 포맷마다 이름+크기+텍스트화된 미리보기 블록이 separator 로 구분되어 나열된다. footer 에 "{n} unrecognized formats" 가 표시된다.
- Given 브라우저에서 서식 있는 텍스트를 복사해 text 와 html 이 동시에 클립보드에 있음 Then "Other" 에 그 text/html 의 raw 변형이 중복으로 잡히지 않는다.
- Given 클립보드가 비어 있음 Then 빈 상태 메시지가 보인다(Other 도 나타나지 않는다).

## 화면

- [screens/clipboard-viewer.md](screens/clipboard-viewer.md) — 뷰어 popup.
