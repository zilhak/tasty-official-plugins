# Git Viewer (`com.tasty.git-viewer`)

- **Status**: Implemented (bundled plugin)
- **주체**: 로컬 사용자 (도구 메뉴 → popup) · AI Agent (IPC trigger)
- **배포/통합**: bundled · 도구 메뉴 항목 + popup — [plugins 개념](https://github.com/zilhak/tasty/blob/develop/docs/concepts/plugins.md)
- **코드**: `crates/tasty-plugin-git-viewer/` · 조회 로직은 host core 와 공유하는
  `crates/tasty-git-core/`
- **권한**: `ui.popup` · `ui.tool_item` · `fs.read`
- **화면**: [screens/git-viewer.md](screens/git-viewer.md)

> **예제로서**: **도구 메뉴 항목 + popup** 예제 — main/render 모듈(조회 로직은 `tasty-git-core`)로 분리한 구조의 레퍼런스 → [plugin-development](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#도구-메뉴-항목--popup).

## 목적

git **status / log / diff 를 읽기 전용**으로 보여주는 popup 을 제공한다.
좌측 **worktree rail** 로 main + 모든 linked worktree 를 한눈에 보고, worktree 를 골라
그 worktree 기준 status/log/diff 로 전환한다(읽기 전용).

## 내부 동작

- **tool** `open-viewer` — [도구 메뉴](https://github.com/zilhak/tasty/blob/develop/docs/features/tools-menu/index.md)에 항목 추가(`ui.tool_item`), action `open_popup{com.tasty.git-viewer/viewer}`.
- **command** `open_viewer` — 단축키로도 뷰어를 연다(`scope = "global"`, `default_keybinding` 미지정 — 설정 > 단축키 > 플러그인에서 사용자가 직접 지정). action 은 tool 항목과 동일한 `open_popup{com.tasty.git-viewer/viewer}`.
- **popup** `viewer` — trigger `ipc`(IPC 로도 열림), `rendering = egui-mesh`. status/log/diff 를 `fs.read` 로 읽어 표시. **읽기 전용**(커밋/스테이징 등 변경 없음).
- **렌더링** — 팝업 콘텐츠를 **egui-mesh** 로 그린다(docs/dev-guide/egui-mesh-channel.md): plugin 이 자기 egui Context 에서
  디자인(`overlays/git_viewer.jsx`)을 직접 페인트하고 host 는 셸(scrim/border/Esc/outside-click)만
  소유한다. Theme 은 `popup.set_context` 의 `ThemeWire` 로 매 frame 받아 재구성한다.
- **worktree 종합 목록** — libgit2 `worktrees()` 는 linked 만 주므로 main working tree 는
  `commondir` 파일(폴백: 경로 추론)로 직접 식별해 목록 선두에 합성한다(`git worktree list` 와 동등).
  popup 이 받은 cwd 가 속한 worktree 에 `current` 마커, `locked`/`invalid` 상태 배지를 표시한다.
- **worktree 전환** — rail 행 선택 시 그 worktree 의 workdir 로 다시 열어 status/log/diff 를
  다시 조회한다. **실제 checkout/working dir/HEAD 변경은 없다**(플러그인 popup 내부 상태만 변경).
- **repo 핸들** — 로컬 모드는 활성 worktree 의 `Repository` 핸들 하나를 들고 재사용해 조작마다
  다시 열지 않는다. 무효화는 worktree 전환 · Refresh · repo 소실 셋뿐이며 Refresh 는 캐시를
  무조건 버린다(외부 편집·worktree add/remove·외부 커밋이 항상 반영된다).
  [플러그인 제작 가이드](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md).
- **빈 summary/author** — `tasty-git-core` 는 git 에 값이 없으면 `LogEntry.summary`/`author` 를 **빈 문자열**로
  주고 자연어 폴백을 만들지 않는다(호출자 주입, [공용 위젯의 문자열](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/i18n.md#공용-위젯의-문자열--호출자-주입)
  가이드 참조). plugin 이 빈 값을 자기 lang 의 `git_viewer.no_message` / `git_viewer.unknown_author` 로 그리므로
  로컬·원격(mirror) 조회 모두 plugin 로케일(`TASTY_LOCALE`)을 따른다.
- **fs 접근** — git2 가 파일을 직접 읽어(host fs 포트 우회) worktree 가 cwd 밖에 있어도 읽는다.
  권한 선언은 `fs.read` 유지.
- **attach mirror 워크스페이스** — 로컬 프로세스에 실제 PTY/파일시스템이 없는 mirror surface
  에서 열리면(`popup.open` context 의 `mirror`/`local_surface_id`), 로컬 `git2::Repository::
  discover` 대신 host 가 attach 채널로 **원격**(mirror 대상) tasty 인스턴스에 조회를 왕복시킨다
  (`git_viewer.query` plugin→host IPC → attach `git_query_request`/`git_query_result` 이벤트
  쌍 → Event Bus unicast로 plugin 회신). status/log/diff/worktrees 전부 이 경로로 동작하며,
  refresh·worktree 전환·파일→diff 클릭 각각 별도 왕복을 트리거한다. 서버는 client 가 forward한
  cwd 문자열이 아니라 서버의 `TerminalStore::cwd(surface_id)`로 OSC-7 캐시와 명시 Pty의 OS cwd를 조회해 저장소를
  discover 한다. 응답이 크면(700KiB 예산) status/log/diff 순으로 잘라 보낸다. 설계 근거·wire
  포맷 상세는 [원격 attach (Remote attach)](https://github.com/zilhak/tasty/blob/develop/docs/features/remote-attach/index.md).

## 인터페이스

- **사용자**: 도구 메뉴 `Git viewer` → popup.
- **AI Agent**: popup trigger 가 ipc — IPC 로 열 수 있다.

## 비-목표

- git *쓰기*(커밋/스테이징/브랜치 조작) — 읽기 전용.
- **worktree 조작**(add/remove/prune/lock 토글) — 읽기 전용 정체성 유지. 목록·전환만 제공.
- 상태바의 브랜치 표시 — [workspace-status-bar](https://github.com/zilhak/tasty/blob/develop/docs/features/workspace-status-bar/index.md)(별개).

## Acceptance Criteria

- Given 플러그인 활성 Then 도구 메뉴에 git viewer 항목이 보인다.
- Given repo 안에서 popup 열기 Then status/log/diff 가 표시된다.
- Given 비-repo Then 적절한 빈/에러 표시.
- Given worktree 가 여러 개인 repo Then rail 에 main + 모든 linked worktree 가 나열되고
      cwd 가 속한 worktree 에 `current` 마커가 표시된다.
- Given worktree 행 선택 Then status/log/diff 가 그 worktree 기준으로 전환되고
      실제 working dir/checkout 은 변하지 않는다.
- Given worktree 가 없는 일반 repo Then rail 에 main 한 항목만 표시된다(하위 호환).
- Given attach mirror 워크스페이스에서 popup 열기 Then 원격 저장소의 실제 status/log/diff/
      worktrees 가 표시된다(로컬 host 의 정보나 "No git repository found" 오표시가 아님).
- Given mirror popup 에서 Refresh Then 원격에 새 커밋이 생겼으면 목록에 반영된다.

## 화면

- [screens/git-viewer.md](screens/git-viewer.md) — git status/log/diff popup.
