# Git Viewer popup 화면

- **부모 기획**: [../index.md](../index.md)
- **시각 소스**: 디자인 `ui_kits/terminal/overlays/git_viewer.jsx`.

도구 메뉴/IPC 로 뜨는 git status/log/diff 읽기 전용 popup. **egui-mesh** 로 그린다 — plugin 이
자기 프로세스 egui Context 에서 콘텐츠를 직접 페인트하고, host 는 셸(scrim/border/Esc/outside-click)만
소유한다(docs/dev-guide/egui-mesh-channel.md). Theme 은 `popup.set_context` 의 `ThemeWire` 로 매 frame 전달돼 host 와
동일 `Theme` 으로 재구성된다.

## 트리거

[도구 메뉴](https://github.com/zilhak/tasty/blob/develop/docs/features/tools-menu/index.md#화면) git viewer 항목 또는 IPC. 단일
인스턴스 — 두 번째 open 은 "이미 열림" 안내를 보인다.

## 레이아웃

960×640. `header + context strip + body`.
- **header** — `Git` 타이틀 + `Refresh`(secondary). 하단 `separator`.
- **context strip** — `bg-sidebar` 밴드: 활성 worktree · branch · HEAD oid pill · repo path(우측, ellipsis).
- **body** — 좌 **worktree rail**(고정 232px) | 우 **right column**. right column 은 상 **Changes** /
  하 **Commits↔Diff** 로 50/50 분할.

## UI 요소 인벤토리

- **worktree rail** — main + 모든 linked worktree 를 2줄 행으로. line1 = 이름(mono) + 타입 pill
  (`main`=sky / `linked`=neutral), line2 = short oid(sky) + 상태 pill(`current`/`locked`/`invalid`, dot).
  선택 행 = `surface-active` + 좌측 2px inset `accent-primary-on-active` bar, 이름과 oid는 `text-on-active`(hover·press는 `text-on-active-hover`). 선택 시 우측 내용을 그 worktree 기준으로 갱신.
  invalid 행은 흐림 + 전환 불가.
- **Changes** — 변경된 파일 목록. 고정폭 상태 pill(`M/A/D/R/?/U`) + 경로(dir `text-muted` / file
  `text-primary`, 선택 행은 둘 다 `text-on-active`). 행 선택 시 하단 pane 이 diff 로 교체(선택 행에 inset bar).
- **Commits** — 커밋 히스토리. oid(sky) + refs pills(sky, 있을 때만) + summary(flex) + author + time.
- **Diff** — 선택 파일의 unified hunk. 툴바(`Back` ghost + 파일 경로) + recessed `bg-app` well:
  old/new 라인번호 거터 + `+`/`-`/context 부호 컬럼 + ±-라인 배경 tint + hunk header 밴드(sky).
- 전부 **읽기 전용**(액션 버튼 없음). 상호작용 = Refresh · worktree 선택(rebind) · 파일 선택(→diff) ·
  diff Back. 입력은 host 가 forward 한 실제 사용자 입력으로 plugin egui 안에서 처리된다.

## 스크롤 (virtualization)

리스트 4개(worktree rail · Changes · Commits · Diff)는 모두 **보이는 행만 레이아웃한다** —
`ScrollArea::show_rows` 로 뷰포트에 걸치는 행 범위만 그린다(근거·대안:
[egui-mesh 렌더 채널](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/egui-mesh-channel.md)). 커밋 목록은
조회 상한 200 행, diff 는 파일 전체 라인이라 목록 길이와 무관하게 프레임당 비용이 뷰포트 높이에
비례한다.

- 행 높이는 리스트마다 하나의 값이다(Changes 26 · Commits 28 고정, worktree rail 과 diff 는 theme
  파생이지만 행마다 동일). 행 함수와 높이 헬퍼(`wt_row_h` / `diff_row_h`)가 짝을 이루므로 한쪽만
  고치면 행이 겹치거나 벌어진다.
- 선택 핸들러는 **전체 목록 기준 인덱스**를 받는다 — 부분 범위를 순회해도 worktree 선택과 파일→diff
  전환 대상이 바뀌지 않는다.
- diff 의 가로 스크롤 폭은 전 라인(hunk 헤더 포함)의 최장 폭을 **한 번 재서 캐시**하고 모든 행이 그
  폭을 할당한다. 보이는 라인만 재면 스크롤 위치마다 폭이 출렁이기 때문이다. 캐시는 폰트 크기를 키로
  갖고, diff 가 바뀌면 비워진다.
- diff 의 ±-라인 tint 와 hunk header 밴드는 **그 행 자신의 텍스트 폭**까지만 칠한다 — 위 할당 폭(전
  라인 최장)과 분리된 값이다. 밴드까지 할당 폭으로 칠하면 짧은 행의 밴드가 가로 스크롤 콘텐츠 끝까지
  늘어난다.

## git 조회 (repo 핸들과 무효화)

로컬 모드는 **활성 worktree 의 `Repository` 핸들 하나만** 들고 재사용한다 — 조작마다 다시 열지
않는다(근거·대안:
아래 기능 계약). 그 결과 파일을
연달아 클릭할 때 repo open 이 일어나지 않는다. popup 최초 로드와 Refresh 는 **plugin 자체
`discover` 기준 1 회**다 — 단 활성 worktree 가 popup cwd 의 worktree(`current`)일 때이고, 활성이
다른 worktree 면 목록 수집용 1 회 + 대상 재바인딩용 1 회로 2 회다. 여기에 더해 **worktree 목록
수집이 항목마다 HEAD 를 읽느라 여는 open 은 별도로 남는다**(main + linked 각 1 회) — 이 캐시가
줄이는 대상이 아니다.

무효화 조건은 셋이고 전부 명시적이다.

| 조건 | 동작 |
|---|---|
| worktree 전환 | 이전 worktree 의 핸들을 버리고 대상 worktree 를 새로 연다 |
| Refresh | 핸들을 먼저 버린 뒤 다시 연다 — 외부 파일 편집 · 외부 `git worktree add/remove` · 외부 커밋이 항상 반영된다 |
| repo 소실 | 재열기 실패 → 캐시는 빈 상태로 남고 "repo lost" 를 표시한다 |

핸들 접근은 "꺼내 쓰고 돌려놓는" 한 쌍으로만 한다. 꺼내는 쪽이 항상 캐시를 비우므로, 에러로 중간에
빠져나가면 캐시가 빈 채 남아 다음 조작이 무조건 다시 연다 — 낡은 핸들이 살아남는 경로가 없다.

worktree 목록은 Refresh 마다 다시 수집한다(외부 add/remove 를 반영하는 유일한 경로). 커밋 목록의
ref pill 도 조회마다 다시 읽는다 — ref 는 커밋/브랜치 조작 한 번으로 바뀌고, 조회 시점이 곧 최신
상태를 요구하는 순간이라 캐시하지 않는다.

Repository는 Send이지만 Sync가 아니므로 현재 SDK의 직렬 plugin worker를 전제로 한다.
worktree 중복 검사는 항목별 canonical path를 한 번 구해 비교한다.
경로 표기 차이는 cache miss만 만들며 다른 저장소의 핸들을 재사용하지 않는다.
Windows에서는 popup 동안 packfile mmap을 잡아 외부 git gc·repack의 파일 삭제가 실패할 수 있다.
popup을 닫으면 핸들을 해제한다. 다중 thread dispatch나 저장소 변경 기능을 추가하면 무효화 규칙을 다시 정한다.

원격(attach) 모드는 로컬 repo 를 열지 않는다 — 조회는 host 가 요청마다 수행하고 plugin 은 wire JSON
만 받으므로 이 캐시가 관여하지 않는다.

## 상태별 시각

- repo / 비-repo(중앙 안내) / detached(branch 자리에 `detached`).
- worktree: current(녹색 dot pill) / linked / locked(노랑 dot pill, 사유 hover) / invalid(빨강 dot pill) /
  worktree 0개(`no_worktrees`).
- 빈/없음 안내(Changes `no_changes` · Commits `no_commits` · rail `no_worktrees`)는 해당 pane 중앙 한 줄. 커밋 행 **안의** 빈 summary/author 는 그 자리에 `no_message` / `unknown_author` 로 대체된다(git-core 는 빈 문자열만 준다).
- error(`accent-danger` 라인, tinted 밴드) 는 header 아래·body 위에 표시.
- already-open(단일 인스턴스 두 번째 인스턴스) 중앙 안내.

## 디자인 토큰 매핑

시각 수치·토큰의 단일 출처는 tasty-page `vendor/ui_kits/terminal/overlays/git_viewer.jsx` 다 — [시각 소스](#시각-소스).

## 갤러리 specimen

`crates/tasty-gallery/src/catalog/components/git_viewer.rs`의 Overlays › `Git worktree viewer
popup`에서 확인한다. 상단 정보 영역, 섹션 제목, 두 줄로 표시한 워크트리 행,
Changes·Commits 목록과 diff 영역에 디자인의 구조와 토큰을 반영한다.
픽셀까지 똑같이 재현하는 것은 목표가 아니며, 필요한 UI 요소를 빠짐없이 포함하는 기준은 docs/design/systems/theme.md를 따른다.
디자인·갤러리·제품 구현의 대응표:
[design-gallery-mapping.md](https://github.com/zilhak/tasty/blob/develop/docs/design/systems/design-gallery-mapping.md#git-viewer-plugins).

## 시각 소스

디자인 `ui_kits/terminal/overlays/git_viewer.jsx`(+`.html` preview). popup 구현은 egui-mesh
채널(docs/dev-guide/egui-mesh-channel.md) + `EguiMeshPopup` SDK 헬퍼.
