# Claude Code (`com.tasty.claude`)

- **Status**: Implemented (bundled plugin)
- **주체**: AI Agent / 로컬 사용자 (`tasty claude` CLI · IPC)
- **배포/통합**: bundled · cli + ipc_namespace + 멀티에이전트 — [plugins 개념](https://github.com/zilhak/tasty/blob/develop/docs/concepts/plugins.md)
- **코드**: `crates/tasty-plugin-claude/`
- **권한**: `terminal.spawn` 등 (매니페스트 `permissions`) · `memory.read`(Stop 훅이 실행된 surface의 goal을 읽는다 — 읽기 전용)
- **화면**: 없음 — CLI/IPC 로 터미널 surface 를 조작하는 실행 관리 플러그인 (headless).

> CLI·IPC, 부모·자식 관계, 훅 설치를 함께 제공하는 예제다. 구현 구조는 [플러그인 개발](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#cli--ipc-namespace)을 참고한다.

## 자식 세션 모델과 effort

`tasty claude spawn --workspace workers --model opus --effort high --prompt "작업 지시"`는 이번 자식의 Claude CLI에 `--model`과 `--effort`를 전달한다. IPC `claude.spawn`의 선택 필드는 `model`과 `effort`다. 생략하면 플래그를 추가하지 않아 기존 Claude 설정을 따른다. 부모·형제·전역 설정은 변경하지 않는다.

- `model`은 alias, full model ID, provider ARN·deployment 식별자 등을 받으며 모델 목록을 하드코딩하지 않는다. 비어 있는 값, 문자열 외 타입(null 포함), 하이픈으로 시작하는 값, 공백·제어 문자는 `terminal.spawn` 전에 거절한다. 셸 특수 문자는 인자로 인용한다.
- `effort`는 `low`·`medium`·`high`·`xhigh`·`max`·`ultracode`를 받는다. `ultracode`는 일반 effort 단계가 아니라 xhigh와 Ultracode를 요청하는 CLI 값이다. 설치된 Claude Code 2.1.307 도움말은 앞의 다섯 값만 표시하지만, [공식 CLI 문서](https://code.claude.com/docs/en/cli-reference)는 2.1.203부터 `ultracode`를 지원한다고 명시한다. 실제 지원·조정은 Claude CLI·모델·조직 정책에 따른다.
- `--profile-file` 또는 `--profile`과 함께 써도 원본 프로필은 바꾸지 않는다. 명시한 CLI 모델·effort가 프로필의 model·effortLevel보다 우선한다. 기존 permission-mode 충돌 검사와 훅·프로필 병합은 유지한다.
- 모델·effort 선택 인자는 `spawn`에만 제공한다. `launch`·`respawn`·`reboot`에 새 선택 인자를 추가하지 않는다. 새 child spawn/respawn 실행은 UUID를 미리 정해 Claude의 `--session-id`로 전달하고, 옵션이 없어도 해당 실행의 소유 세션 ID를 기록한다. `reboot`는 기존 세션 ID와 선택값을 유지한다.
- `claude-session-options` surface 메타데이터의 schema 2는 `session`·`pending`·`options`를 함께 기록한다. 첫 SessionStart가 소유 ID와 일치해야 pending 실행을 확정한다. 그 전에 도착한 다른 세션의 늦은 SessionStart는 상태·복원·프로필 기록을 변경하지 않는다. pending 중 이전 세션을 reboot하는 요청도 종료 절차 전에 거절한다.
- 확정된 옵션은 plugin 데이터의 `session-options/<session-id>.json`과 프로필을 반영한 `restore.command`에 기록한다. 같은 세션의 닫은 탭 복원·앱 복원·reboot에 유지한다. `/resume`으로 다른 ID를 선택하거나 `startup`·`clear`·`fork`가 다른 ID를 보고하면 그 세션의 저장 기록을 읽으며, 다른 세션 surface의 선택값을 복사하지 않는다. 기록이 실제로 없으면 미지정이다. 새 respawn은 이전 선택값을 승계하지 않는다. 종료 시 live 메타데이터만 지우고 세션별 파일은 보존한다.
- 플러그인이 셸에 입력하는 Claude 실행·spawn·복원·reboot 명령의 `--settings` 경로, 모델·effort 값, `launch`의 `--task` 값은 POSIX 셸 규칙으로 인용한다. Linux·macOS의 `--settings` 경로는 큰따옴표로 감싸고 `\` `"` `$` `` ` `` 앞에 역슬래시를 넣는다. Windows 터미널 셸은 Git Bash뿐이므로([터미널 surface "실행할 셸"](https://github.com/zilhak/tasty/blob/develop/docs/surfaces/terminal/index.md)) Windows의 `--settings` 경로는 작은따옴표 규칙을 쓴다. 모델·effort·`--task` 값은 모든 OS에서 작은따옴표 규칙을 쓴다. 작은따옴표 규칙은 ASCII 영문자·숫자와 `-` `_` `=` `/` `,` `.` `+`만 있는 값은 그대로 두고, 그 밖의 값은 작은따옴표로 감싼다. `'`는 따옴표 밖으로 빼서 `'\''`로 넣는다. `!`도 shell-escape 규칙을 따라 따옴표 밖으로 빼서 `'\!'`로 넣는다. bash에서는 `!`를 작은따옴표 안에 두어도 결과가 같다. 작은따옴표 안에서는 `\` `$` `` ` `` `"`가 글자 그대로라 Windows 경로의 역슬래시·UNC 접두(`\\server\share`)·`D$`가 바뀌지 않는다. 드라이브 문자 경로를 `/c/` 형태로 바꾸지 않는다. 결과는 Tasty가 상속한 `MSYSTEM` 값과 관계없다.
- schema 표지가 없는 이전 평평한 surface 옵션은 기존 settings 소유 세션 표지가 같은 ID일 때만 승계한다. 소유자를 모르는 명시값은 같은 세션의 파일 기록으로 복구하고, 그 기록도 없으면 오류로 남기며 빈값으로 덮지 않는다. 기존 평평한 세션 파일 형식은 유지한다. 호스트 조회·파일 읽기 실패, 잘못된 JSON과 알 수 없는 schema는 정상 미지정으로 취급하지 않으며, 옵션 없는 복원 명령으로 대체하지 않는다.
- spawn 응답의 `requested_session_options`는 요청값이다. 터미널 생성·명령 전달은 provider의 모델 수락·응답 성공을 뜻하지 않는다. Tasty는 provider 거절을 성공으로 바꾸거나 다른 모델로 재시도하지 않는다. Claude 자체의 effort 조정·fallback 정책과 실제 사용 모델은 Claude 화면·응답에서 확인한다.

## Tasty 자식에서 지우는 세션 변수

매니페스트 `[contributes.strip_inherited_env]`로 Claude Code가 자기 세션의 자식에 넣는 표지·비밀·세션 값을 선언한다. 이 플러그인이 설치·활성이면 Tasty는 터미널 셸·훅·작업 자식에서 이 값을 지운다([선언 형식과 적용 조건](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#상속-환경변수-제거-contributesstrip_inherited_env)). 바깥 Claude Code 세션 안에서 Tasty를 띄워도 그 터미널에서 실행한 Claude가 자식 세션 표지(`CLAUDE_CODE_CHILD_SESSION`)를 물려받아 transcript 저장을 끄지 않고, 세션 비밀(`CLAUDE_CODE_MESSAGING_TOKEN`)이 무관한 셸로 새지 않는다. 정본은 `crates/tasty-plugin-claude/tasty-plugin.toml`이며 목록은 Claude Code 2.1.291 기준이다.

- 이름: `CLAUDECODE`·`CLAUDE_PID`·`CLAUDE_EFFORT`·`CLAUDE_PLUGIN_ROOT`·`CLAUDE_PLUGIN_DATA`·`CLAUDE_PROJECT_DIR`·`CLAUDE_ENV_FILE`·`CLAUDE_CODE_SESSION_ID`·`CLAUDE_CODE_CHILD_SESSION`·`CLAUDE_CODE_SESSION_ATTENDED`·`CLAUDE_CODE_EXECPATH`·`CLAUDE_CODE_ENTRYPOINT`·`CLAUDE_CODE_MESSAGING_SOCKET`·`CLAUDE_CODE_MESSAGING_TOKEN`·`CLAUDE_CODE_INVOKED_SKILLS`·`CLAUDE_CODE_BRIDGE_SESSION_ID`. Bash·훅 자식 env 조립, 플러그인 훅·MCP 자식 env 조립, 새 Claude를 띄울 때 스스로 지우는 목록을 기준으로 하며 Claude Code 버전에 따라 늘어날 수 있다.
- 접두사: `CLAUDE_PLUGIN_OPTION_`. 플러그인 훅에 플러그인 옵션을 주입하는 이름공간이라 키가 옵션마다 다르다.
- 값 접두사: `AI_AGENT`의 값이 `claude-code_`나 `claude-code/`로 시작할 때만 지운다. 사용자나 다른 도구가 넣은 값은 남긴다.
- 지우지 않는 것: `CLAUDE_CODE_*` 접두사 전체는 지우지 않는다. 그 이름 대부분이 사용자가 넣는 설정(`CLAUDE_CODE_GIT_BASH_PATH`·`CLAUDE_CODE_OAUTH_TOKEN`·`CLAUDE_CODE_USE_BEDROCK` 등)이다. `ANTHROPIC_API_KEY`와 W3C 표준 이름 `TRACEPARENT`도 남는다.
- 플러그인을 비활성화하거나 제거하면 이 목록은 적용되지 않는다.

## Codex 호출자 요구

매니페스트 `[contributes] requires_caller_context = ["codex"]`로 Codex 플러그인이 선언한 호출자 문맥을 요구한다([선언 형식](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#cli-호출자-문맥-contributescaller_context)). Codex 대화에서 부른 `tasty claude …` 명령은 CLI가 붙인 `_tasty_context.provider = "codex"`로 Codex 부모를 찾는다. `CODEX_THREAD_ID` 형식이 틀리면 CLI가 이 플러그인 명령을 실행하지 않으므로 `TASTY_SURFACE_ID`의 다른 터미널을 부모로 삼지 않는다. Codex 플러그인을 제거하면 그 선언이 없어 Codex 호출자를 알아보지 않는다.

## 부모의 완료 수신 채널

자식의 상태 변경은 caller surface의 `<parent_home>/notify/<caller_surface>.log`에 한 줄로 기록한다. 자식 CLI 종류에 따라 수신 경로를 바꾸지 않는다. install은 훅과 부모 수신 mod를 함께 설치하지만 실제 세션의 mod 활성화·수신 확인은 설치 성공과 별개이며 idle·needs_input·interrupt·exit는 작업 성공을 뜻하지 않는다. [완료 알림 로그](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/external-interaction.md#child-완료-알림--completion-log)를 따른다.

### Codex 부모로 결과 반환

Codex 대화에서 실행한 `spawn`·`tell`·`children`·`broadcast`·`kill`·`respawn`은 부모를 자동으로 정하고, 자식의 확정된 턴 결과를 그 대화로 돌려보낸다. 코드는 `codex_return.rs`에 있다.

- **호출자 판별**: 요청의 `_tasty_context`가 `provider: "codex"`이고 `thread_id`가 UUID이면 Codex 호출이다. `provider`가 codex인데 thread id가 잘못됐으면 다른 부모로 대신하지 않고 `-32602`로 거절한다. 이 키가 없는 일반 호출은 기존 `surface`(기본값 `TASTY_SURFACE_ID`)·오류·응답을 그대로 쓴다.
- **부모 해석**: Codex 호출은 자기 surface id 인자 없이 `codex.parent({_tasty_context})`를 부른다. 호스트는 GUI와 헤드리스 모두 이 호출을 Codex 플러그인에 전달한다([헤드리스 IPC 표면](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/headless-ipc-surface.md)). 응답의 `receiver_id`는 32자리 16진수, `terminal_parent`는 `{parent: u32}`와 `{parent_external: string}` 중 정확히 하나여야 한다. 호출이 실패하면(Codex 플러그인 없음·비활성 등) 그 오류 코드를 그대로 돌려주고 다른 부모로 대신하지 않는다. 명시한 `--surface`가 해석한 물리 부모와 다르면 거절한다.
- **호스트 요청**: 물리 부모는 기존 키(`terminal.spawn`은 `parent`, `children`·`kill`·`broadcast`·`respawn`은 `surface`)로, 외부 부모는 `parent_external`로 싣는다. 자식 목록·index·상태는 호스트 child registry가 소유한다.
- **외부 부모의 workspace**: 외부 부모의 요청에 싣는 workspace는 이 순서로 정한다. 자식의 workspace는 저장하지 않는다. 호스트가 모든 창의 surface를 모아 돌려주는 `surface.list`에서 그 자식 surface id를 찾아, 지금 속한 `workspace_id`를 고른다. 복원으로 다른 workspace의 새 surface로 옮겨 간 자식도 지금 위치로 찾는다. 사용자 포커스나 마지막으로 쓴 workspace로는 정하지 않는다.
  1. 명시한 `--workspace`
  2. 대상 자식이 정해진 요청(tell의 소속 확인)은 그 자식이 지금 있는 workspace id
  3. 이 부모의 경로에 있는 자식들이 지금 한 workspace에만 있으면 그 workspace id. 호스트에 없는 surface는 세지 않는다
  4. 여러 workspace에 있으면 `-32602`로 거절하고 후보를 알린다. `children`·`kill`·`broadcast`·`respawn`의 `--workspace`로 고른다. 일반 호출에서는 이 인자를 쓰지 않는다.
- **tell**: 명시한 대상 surface를 그대로 쓴다. Codex 호출이면 대상이 해석한 부모의 `terminal.children`에 있는지 먼저 확인하고, 아니면 다른 부모의 자식을 건드리지 않도록 거절한다. 결과 경로를 저장한 뒤 `terminal.tell`을 보낸다. 경로의 세션이 끝났거나(`ended_at_ms`) `/clear`·`/resume` 넘기기가 멈춰 있으면 그 세션을 놓고 다음 hook의 세션으로 다시 묶는다. 실행 중인 세션에 묶인 경로는 세션을 바꾸지 않는다.
- **일반 호출의 새 지시**: Codex 호출이 아닌 `tell`과 `respawn`은 지시를 보내기 전(respawn은 호스트가 surface를 준비한 뒤 Claude를 띄우기 전)에 그 자식의 Codex 경로를 지운다. 이미 claim한 결과는 그대로 보낸다. 경로 파일을 읽거나 쓰지 못하면 결과가 이전 Codex 대화로 갈 수 있으므로 지시를 보내지 않고 오류를 돌려준다. 일반 `spawn`은 새 자식이라 지울 경로가 없다.
- **경로 저장**: spawn·respawn은 호스트가 자식을 만든 직후, Claude를 띄우기 전에 자식 → 수신처 경로를 플러그인 데이터 폴더의 `codex-return/routes.json`에 저장한다(tmp 파일 기록·fsync·rename). 같은 워커가 처리할 자식의 첫 hook보다 경로가 먼저 있다. 저장하지 못하면 만든 자식 id를 알리며 실패한다. 데이터 폴더가 없으면 자식을 만들기 전에 거절한다. Codex 부모의 자식에는 기존 completion-log 훅과 외부 부모의 자식 수 경고를 등록하지 않는다.
- **세션 결합**: spawn·respawn 경로는 새 실행의 `session-start`로 세션을 묶고, 실행 중인 자식에게 tell로 만든 경로는 다음 hook의 세션으로 묶는다. 경로가 새 세션으로 넘어가면 앞 세션을 넘어간 세션 목록(최근 16개)에 남긴다.
- **복원**: 묶인 세션의 `session-start`가 다른 surface에서 오면(복원된 실행) 경로의 surface를 옮긴다. 다른 hook은 경로를 옮기지 않는다. 결과도 경로의 surface에서 온 확정만 받는다. 그래서 옛 surface의 늦은 hook은 경로를 되돌리거나 `exited`를 만들지 못한다.
- **`/clear`·`/resume`**: Claude Code 2.1.296은 같은 프로세스에서 SessionEnd(`reason` `clear`·`resume`) 훅을 기다린 뒤 새 세션의 SessionStart(`source` 같은 값)를 실행한다. 기다리는 시간에는 상한이 있다. `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS`가 없으면 1.5초와 설정된 SessionEnd 훅 timeout(최대 60초) 중 큰 값이다. Tasty가 설치하는 SessionEnd 훅에는 timeout이 없으므로 1.5초다. 그 안에 끝나지 않으면 SessionStart가 먼저 실행되고, Tasty에 도착하는 SessionEnd는 늦거나 오지 않을 수 있다. 두 순서를 다음처럼 처리하며, 턴 순번은 이어진다.
  - SessionEnd가 먼저 오면 경로를 넘길 준비만 하고 `exited`를 보내지 않는다. 같은 surface의 다음 SessionStart(`clear`·`resume`)가 새 세션으로 경로를 잇는다.
  - SessionStart가 먼저 오면, 즉 같은 surface에서 끝나지 않은 다른 세션에 묶인 경로가 있으면 그 새 세션을 기다리는 후보로 적어 둔다(경로마다 최근 4개). 후보는 서로 밀어내지 않으며, 먼저 `prompt-submit`(새 턴의 시작)을 보낸 후보가 경로를 잇고 나머지 후보는 버린다. Stop·SessionEnd 같은 다른 hook으로는 잇지 않으므로, 늦게 온 예전 세션의 시작 뒤에 그 세션의 늦은 Stop이나 SessionEnd가 와도 경로를 가져가거나 결과·`exited`를 만들지 못한다. 묶인 세션이 그 뒤 프롬프트를 받아도 후보는 지우지 않는다. 묶인 세션의 hook이 늦게 왔을 수 있기 때문이다. 그래서 SessionEnd가 끝내 오지 않은 채 `/clear`한 세션이 프롬프트 없이 넘어간 세션으로 `/resume`해도, 프롬프트를 보낸 resume 세션이 잇는다.
  - 넘어간 세션 목록에 있는 세션의 `clear` 시작은 늦게 온 중복이므로 받지 않는다. `resume`은 예전 세션으로 돌아가는 정상 흐름이라 후보로 받는다.
  - `startup` 등 다른 source의 시작은 잇지 않는다. 넘긴 뒤 옛 세션의 hook은 어느 경로에도 닿지 않는다.
- **경로 정리**: 그 밖의 SessionEnd는 끝난 시각을 남기고 경로를 24시간 둔다. 복원된 같은 세션이 오면 다시 이어진다. 기간이 지난 경로는 다음 `session-start`나 spawn 때 지운다. `claude kill`이 닫은 자식의 경로는 바로 지운다. 보낼 요청은 경로와 따로 저장하므로 경로를 지워도 남는다.
- **턴 순번**: `prompt-submit`마다 경로의 턴 순번을 올린다. 진행 중 턴에 합쳐진 입력은 한 턴이다. 순번은 경로 파일에 저장해 재시작 뒤에도 이어진다. 순번은 그 세션에 보류된 게이트 Stop을 앞 턴으로 확정한 뒤에 올린다.
- **턴 식별자**: Stop이 온 때 경로 id·턴 순번·세션·surface·수신처(`receiver_id`)를 정하고(`turn_of`), 게이트 보류(`HeldStop`)와 확정(`Settled`)을 거쳐 그대로 들고 간다. 게이트 없는 Stop과 `stop-failure`도 호스트 보고 전에 정한다. 판정이 늦거나, 시간 초과 확정 스레드가 보고하기 전에 새 프롬프트가 몇 번 오거나, 새 턴 결과가 먼저 나가도 그 Stop의 결과는 원래 턴 키로 한 번 claim한다. 시간 초과 확정을 보고하기 전에 새 턴이 열리면 상태 보고(idle)는 버리지만 Codex 결과는 보낸다.
- **결과 생산**: 턴 종료가 확정된 곳에서만 만든다. 게이트 없는 Stop과 게이트 판정이 턴 종료로 확정된 Stop은 `report_ended`, `stop-failure`는 실패 보고 자리다. 게이트가 이어 가게 한 Stop과 백그라운드 작업을 기다리는 Stop은 결과가 아니다. 확정 전에 `session-end`가 오면 그 턴을 `exited`로 한 번 보낸다.
- **중복 방지**: `completion_key`는 `claude:<경로 id>:t<턴 순번>`이다. prompt_id가 없어도 된다. 경로마다 claim한 턴을 최근 64개까지 기억하고, 이미 claim한 턴이나 그보다 오래된 턴은 claim하지 않는다. `receiver_id`·`child_session`·`child_surface`는 경로가 그 뒤 다른 세션이나 수신처로 넘어갔어도 그 턴의 값이다. 판정을 기다리는 동안 다른 부모가 같은 자식에게 tell해도 그 턴의 결과는 그 턴의 부모에게 간다.
- **저장 파일 잠금**: 같은 데이터 폴더를 두 플러그인 프로세스가 쓸 수 있으므로(업그레이드 겹침 등) 경로 파일 갱신은 프로세스 안 잠금과 `codex-return/routes.lock` 파일 잠금으로 직렬화한다. tmp 파일 이름에는 프로세스 id를 붙인다.
- **송신**: 훅 처리는 claim과 보낼 요청을 같은 파일 쓰기로 남기고 송신 스레드(`claude-codex-return`)를 깨우기만 한다. `codex.deliver`는 그 스레드가 부르므로 Codex 응답이 늦어도 훅 처리가 막히지 않는다.
  - 호출 하나의 응답 상한은 15초다.
  - 한 차례에 4개까지 보내고, 남은 것은 곧바로 이어 보낸다.
  - 스레드는 플러그인이 시작할 때 이전 실행이 남긴 요청부터 보낸다. 보낼 것이 없으면 60초마다 다시 확인한다.
  - 플러그인이 끝나면 멈추고 회수된다.
- **재시도**: 실패한 요청은 본문·수신처·오류·시도 수와 함께 남는다. 같은 `completion_key`로 2초에서 두 배씩 늘어나는(최대 5분) 간격을 두고 자동으로 다시 보낸다. 받는 쪽이 (`receiver_id`, `completion_key`)로 중복을 거르므로 다시 보내도 결과는 하나다. 첫 실패에서 24시간이 지나면 자동 재시도를 멈추고(`gave_up`) 요청은 남긴다. `claude.codex_returns {retry: true}`(CLI `codex-returns --retry`)는 `gave_up`을 포함한 모든 요청을 지금 보낼 차례로 돌리고(재시도 기간도 새로 시작한다) 송신 스레드를 깨운다. 이 요청은 송신을 기다리지 않으므로 함께 돌려주는 `pending`은 요청 시점의 목록이며 `requeued`는 돌린 수다. 성공한 요청만 지운다.

`codex.deliver` 요청:

| 필드 | 값 |
|---|---|
| `receiver_id` | `codex.parent`가 준 수신처 |
| `completion_key` | `claude:<경로 id>:t<턴 순번>` (`[A-Za-z0-9._:-]`, 160바이트 이하) |
| `provider` | `"claude"` |
| `child_surface` · `child_session` | 자식 surface와 세션(세션은 null 가능) |
| `status` | `turn_ended`(Stop 확정, 작업 품질 판정이 아님) · `turn_failed`(StopFailure) · `exited`(확정 전 세션 종료). Claude 플러그인은 `interrupted`를 만들지 않는다 |
| `body` | `turn_ended`는 `last_assistant_message`, `turn_failed`는 오류 종류, `exited`는 null |
| `attempt_id` | 게이트 없는 Stop에 prompt_id가 있으면 그 값. 없으면 생략 |

### 원격 attach 자식

attach 로 연결한 원격 워크스페이스(mirror)에 `spawn` 하면 자식은 서버 Tasty 에서 서버의 Claude Code·설정·프로필·토큰으로 실행된다. 부모·Codex 결과 경로·완료 알림 훅은 이 Tasty 의 mirror surface 에 묶이고, `tell`·`kill` 은 서버의 같은 메서드로 간다. 서버에서 완료를 보고하는 것은 서버에 `tasty claude install` 로 설치한 기존 Stop·StopFailure 훅이다. 서버에 그 훅이 없으면 spawn 응답에 경고가 실린다. 부모가 물리 surface(Claude 세션·셸)여도 서버가 확정한 턴 본문이 돌아와 이 Tasty 의 plugin 데이터 폴더 `attach-results/` 에 저장되고, 부모 완료 로그에 결과 ID 와 `tasty claude remote-results --id <ID>` 가 한 줄로 남는다. 원격 자식의 결과 소유자는 만든 부모로 고정되며, 다른 부모의 `tell` 은 원격에 보내기 전에 `child_of_other_parent` 로 거절한다. 세부 계약은 [원격 attach 자식](https://github.com/zilhak/tasty/blob/develop/docs/plugins/remote-attach-children.md)에 있다.

## 목적

**Claude Code CLI를 tasty 안에서 실행하고 관리**하는 통합. 새 워크스페이스/페인에 Claude 인스턴스를 띄우고, 부모-자식 관계로 여러 인스턴스를 spawn·제어한다 (멀티에이전트).

## 내부 동작

- **입력 기본값**: 설치 후 최초 활성화에서 `settings.initialize_input_rule`로 `claude`의 Shift+Enter → LF 규칙을 등록한다. 기존 설치본도 업데이트 후 최초 활성화에서 등록한다. 사용자가 이미 지정한 값은 보존하며, 이후 수정·삭제한 규칙은 재시작·재활성화로 덮어쓰지 않는다. 등록 이력은 호스트 설정에 영속화한다. 프로세스 이름의 정확한 매칭을 사용하므로 `node` 등 런처 이름만 감지되는 환경은 해당 실행 파일명으로 사용자가 별도 규칙을 지정해야 한다.

짝 핸들러의 서로 다른 공개 응답과 번역 형식은 기존 호출자 호환을 위해 유지한다.
`children`/`kill`의 응답 형식과 상태 훅의 공통점·차이점은
[짝 핸들러 호환 경계](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/paired-agent-handlers.md)를 따른다. 완료 알림은
[completion-log](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/external-interaction.md#child-완료-알림--completion-log)에 기록하며 caller PTY에 메시지를 입력하지 않는다.

- **cli `claude`** (`tasty claude …`) — 서브커맨드: `launch`(새 워크스페이스에서 실행) · `spawn`(자식 인스턴스, 페인 분할) · `children`/`parent`(관계 조회) · `tell`/`broadcast`(메시지 전송) · `kill`/`respawn` · `reboot`(같은 세션 resume 재시작, 아래) · `child-profile`(자식에게 지속 프로필 부착, 아래) · `hook`(Claude Code 훅 통합, 아래 "Claude Code 훅 통합" 절) · `checklist-hook`(`continue-checklist` 세션 프로필 전용 `Stop` 훅, 아래 "continue-checklist 세션 프로필" 절) · `checklist-enable`/`checklist-disable`/`checklist-status`(게이트별 마커 파일을 켜고 끄고 조회 — `--gate` 생략 시 `continue-checklist`, 같은 절) · `quiet`(무출력 알림 선언, 아래 "무출력 알림 선언" 절) · `notify-done`(내부용: spawn/tell 상태 전환 시 caller 에게 알림 전달 + 형제 hook 정리·재등록, 아래) · `notify-quiet-release`(내부용: 끝난 선언의 묶음 전달, 아래) · `profile-register`/`profile-unregister`/`profile-list`/`profile-show`/`profile-current`(Claude 세션 프로필 레지스트리, 아래 "Claude 세션 프로필 레지스트리" 절).
- `spawn`/`tell`은 필요한 호스트 호출의 응답을 받은 뒤 반환하며 자식 작업 완료까지 기다리지는 않는다. `claude-idle`/`needs-input`/`process-exit` once 훅을 등록한다. 같은 부모·대상의 완료 훅 그룹(명령 앞부분 `tasty claude notify-done --caller-surface <부모> --target-surface <대상> --command `)이 이미 있으면 먼저 지우고 등록하므로, tell을 반복하거나 spawn 뒤 tell해도 부모마다 그룹은 하나이고 한 번의 상태 변경에 완료 줄은 한 줄이다. 다른 부모의 그룹은 그대로 둔다. 하나가 실행되면 `notify-done`이 상태 변경 로그를 쓰고 같은 명령의 형제 훅을 정리한다. 이후 `surface.locate`가 성공하면 훅을 다시 등록한다. Surface 존재는 프로세스 생존과 같지 않으며, 등록·호출·로그 기록 실패나 재등록 사이의 이벤트까지 전달한다고 보장하지 않는다.
- **ipc_namespace `claude`** — 위 동작에 대응하는 IPC API.
- 실제 Claude 프로세스는 터미널 surface 안에서 돌고(`terminal.spawn`), 플러그인은 그 생명주기·관계를 관리한다.
- **`reboot`** (`tasty claude reboot [--surface <id>] [--delay <초>] [--prompt <추가문구>] [--profile-file <경로> | --profile <이름[,이름2,...]>] [--clear-profile] [--permission-mode <모드>]`) — surface 안의 Claude 를 종료하고 **같은 세션으로 재시작**한다.
  Claude 는 스스로 자기 TUI 를 껐다 켤 수 없으므로 에이전트가 이 명령을 자기 surface 에 호출한다(설정/훅/버전 변경 반영용).
  요청 처리 중 세션·프로필 정보를 확인한 뒤 재시작 작업을 예약하고 응답한다. `--delay`(기본 5s) 후 Ctrl+C를 0.5s 간격으로 4회 보내고 전경 이름이 Claude에서 바뀌었는지 확인한다. 바뀌면 `claude -r <session_id>`를 전송하며 프로필이 있으면 `--settings "<경로>"`를 붙인다. Claude 복귀를 확인한 뒤 안내 문구와 별도 Enter를 보낸다. 전경 이름과 화면 확인은 휴리스틱이며, 조회와 입력 사이의 상태 변화까지 막는 원자적 제출은 아니다.
  전경이 계속 Claude로 보이면 resume 명령을 보내지 않고, 복귀를 확인하지 못하면 안내 문구를 보내지 않는다. 같은 surface의 중복 reboot는 거절한다.
  **턴의 마지막 행동으로 호출할 것** — delay 이후 진행 중이던 턴은 잘린다.
  - **`claude-session-id` meta 가 비어 reboot 가 실패하는 경우**: `no active claude session on surface {id} (claude-session-id meta not set …)` 에러는 hook 미설치가 아니어도 발생할 수 있다 — session-start hook 이 이 meta 를 못 심은 것이 원인.
    조용히 실패할 수 있는 지점이 최소 3곳: ① `install.rs`의 등록 커맨드가 `if [ -n "$TASTY_SURFACE_ID" ]; then tasty claude hook … || true; fi` 라 `TASTY_SURFACE_ID` 미설정 시 tasty 바이너리 자체가 실행되지 않음(로그 불가), ② `hook.rs`의 `apply_hook` session-start 분기가 stdin JSON 에 `session_id` 가 없으면 meta 기록을 건너뜀(`tracing::warn!`으로 로그, `tasty plugin logs com.tasty.claude --follow` 또는 `~/.tasty/plugins-logs/com.tasty.claude.log` 에서 확인), ③ `dynamic/stdin.rs`의 `read_stdin_json` 이 TTY/파싱 실패로 `None` 을 반환(hook 은 CLI 프로세스라 tracing 이 **stderr 로만** 나간다 — 공유 로그 파일에는 남지 않는다, [CLI 크레이트 내부 구조 — 세 갈래 대칭](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/cli-structure.md).
    전달 실패 자체는 `$TASTY_HOME/hook-failures.log` 에 기록된다).
    수동 복구: `tasty surface-meta set --key claude-session-id --value <세션ID>`.
- **`child-profile`** (`tasty claude child-profile [--surface <부모>] --child <index> [--delay <초>] [--prompt <추가문구>] [--profile-file <경로> | --profile <이름[,이름2,...]>] [--clear-profile] [--permission-mode <모드>]`) — **부모가 자식에게 지속 세션 프로필을 부착**한다.
  `--child <index>`(`claude children` 이 보여주는 index)를 `terminal.children` 으로 자식 surface id 로 해석한 뒤, 그 surface 에 대해 위 `reboot`과 같은 처리 경로를 사용한다(프로필 검증 → surface meta 부착 → Ctrl+C 시퀀스 → `claude -r <sid> --settings "<경로>"` → 안내 프롬프트).
  별도 부착 메커니즘이 아니라 reboot 진입점의 재사용이므로, 부착 상태는 자식의 이후 **무인자 `reboot` 에 그대로 승계**된다.
  중복 가드도 reboot 과 같은 set 을 쓴다 — 같은 자식에 `reboot` 과 이 명령이 겹치면 뒤엣것이 "이미 진행 중" 으로 거부된다.
  - **`reboot` 과 다른 점 1 — 턴이 잘리지 않는다.** `reboot` 의 "턴의 마지막 행동으로 호출할 것" 경고는 **호출자 자신이 재기동될 때**의 제약이다. 이 명령은 자식만 재기동시키므로 **부모의 턴은 잘리지 않는다** — 호출 후 계속 작업해도 된다.
  - **상태 알림**: `spawn`/`tell`과 같은 상태 변경 훅을 caller에 등록한다. `reboot` 자체는 이 알림을 등록하지 않는다. 실제 전달은 훅 실행과 로그 기록의 성공 여부에 달려 있다.
  - **`--child` 는 필수다.** 자기 자신에게 붙이는 것은 `reboot --profile` 의 몫이라 창구를 겹치지 않게 한다. 없는 index 를 주면 사용 가능한 index 목록과 함께 즉시 에러이며, **아무 자식도 죽지 않는다** — 프로필 인자 검증(상호배타 · 미등록 이름 · JSON 파싱)도 전부 Ctrl+C 시퀀스 시작 **이전**에 끝난다.
  - **`spawn`/`respawn`/`launch` 의 `--profile` 과의 차이**: 그쪽은 그 기동 명령 **1회에만** `--settings` 를 싣고 meta 를 건드리지 않는다 — 자식이 한 번이라도 `reboot` 하면 프로필이 빠진다. 지속 부착이 필요하면 이 명령을 쓴다.

- **Claude 세션 프로필**([용어 정의](https://github.com/zilhak/tasty/blob/develop/docs/concepts/ubiquitous-language.md)): 이 플러그인은 `reboot`/`spawn`/`respawn`/`launch`의 기동 명령에 프로필을 전달한다. 아래 두 지정 방법은 함께 사용할 수 없다.
  - `--profile-file <경로>`(`path_kind = "file"`, CLI 가 호출자 cwd 기준 절대경로로 정규화, **반복 지정 거부** — 아래 "왜 반복 지정을 CLI 가 거부하는가") — 파일 경로를 그대로 쓴다.
  - `--profile <이름[,이름2,...]>` — 아래 "Claude 세션 프로필 레지스트리"에 등록해 둔 프로필 이름, 또는 "Stop-훅 게이트 레지스트리"에 등록해 둔 **게이트 이름**으로 부착한다(두 레지스트리는 이름 공간을 공유한다). 이름을 둘 이상 쉼표로 주면 레지스트리가 머지해 만든 파일 하나를 쓴다.

  어느 쪽이든 최종적으로 기동 명령에 `--settings "<경로>"` 가 붙는다 — Claude Code 의 `--settings` 는 `~/.claude/settings.json` 의 기존 훅을 **대체가 아니라 병합**하므로 tasty 내장 훅(`claude hook` 경유)도 그대로 발생한다.
`reboot`(및 자식을 대상으로 같은 경로를 타는 `child-profile`) 만 부착 상태를 surface meta 에 기록해 **다음 무인자 reboot 가 기본값으로 승계**한다 — 경로로 부착하면 `claude-session-profile`(경로 그대로), 이름으로 부착하면 `claude-session-profile-names`(이름 문자열)에 기록되고 두 meta 는 상호 배타적으로 관리된다(한쪽을 새로 쓰면 다른 쪽은 지운다).
이름-meta 는 **승계 시점마다 레지스트리에서 다시 해석**한다 — 경로를 캐시하지 않으므로 그 사이 `profile-register` 로 내용이 갱신됐다면 다음 reboot 에 최신 내용이 반영된다.
두 meta 모두 파일 존재 + JSON 파싱을 매 reboot 마다 동기 재검증한다(승계된 프로필이 깨져 있으면 kill 시퀀스를 시작하지 않고 에러 반환).
`--clear-profile` 로 둘 다 뗀다.
`spawn`/`respawn`/`launch` 는 그 호출 1회의 기동 명령에만 반영하고 meta 를 건드리지 않는다(반복 재기동은 `reboot` 계열만의 개념) — 그렇게 띄운 자식에 지속 부착이 필요하면 `child-profile` 을 쓴다.
  - **복원을 건너 프로필이 유지된다** — 아래 "복원을 건너 프로필이 유지되는 방식".
  - **왜 반복 지정을 CLI 가 거부하는가**: Claude Code 의 `--settings` 는 반복 지정 시 **마지막 값만 남고 앞선 값이 조용히 사라진다**(실측). tasty CLI 인자 자체(`--profile-file`)를 실수로 두 번 주는 경우도 같은 함정에 빠질 수 있어, 매니페스트 `CliArg.reject_repeat = true`(`crates/tasty-cli/src/dynamic/build.rs`)로 clap 을 `ArgAction::Append` 로 등록해 두 번째 값이 들어오면 조용히 버리지 않고 에러로 거부한다.

### 복원을 건너 프로필이 유지되는 방식

앱 재시작이나 닫은 탭 복원에서는 새 surface ID를 사용하므로 이전 surface meta를 그대로 넘기지 않는다. 플러그인은 세션 ID별 부착 기록으로 프로필 정보를 복구한다. 호스트의 [레이아웃 복원](https://github.com/zilhak/tasty/blob/develop/docs/features/layout-persistence/index.md)은 에이전트 종류와 무관하게 `restore.command`를 사용한다.

- **기록 위치**: `TASTY_PLUGIN_DATA_DIR/profiles/attachments/<session_id>.json`. 내용은 `{"kind": "names"|"path", "value": …}` — **이름으로 부착한 것은 이름을** 남긴다(복원 시 재해석 대상). data dir 은 설치 디렉터리와 분리돼 있어 `upgrade-builtins`/재설치를 건너 보존된다([plugin-development](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md) §6 "data dir 수명 계약").
- **쓰기**: `reboot` 이 프로필을 부착/해제할 때 surface meta 갱신과 같은 지점에서 함께 갱신한다(`--clear-profile` 은 meta 2키와 기록을 함께 지운다).
- **복구**: `session-start` 훅이 프로필 meta 를 먼저 보고, 비어 있으면(=복원을 건너온 세션) 기록에서 meta 를 되살린다. 그다음 이름을 **매번 다시 해석**해(레지스트리 최신 내용 반영) `restore.command` 를 `claude -r <id> --settings "<경로>"` 로 쓴다. 세션 id 는 `claude -r <id>` 를 건너 보존되므로 기록의 키로 쓸 수 있다 — 이 전제가 깨지면(Claude Code 가 resume 시 새 id 를 발급하면) 프로필만 조용히 빠지므로, 재시작 후 세션 생존 확인을 릴리스 점검에 유지한다.
- **재기록(re-stamp)**: session-start 때 프로필 기록을 다시 쓰고 `ended_at` 종료 표시를 지운다.
  reboot 중 SessionEnd가 남긴 종료 표시도 이때 제거한다. 기록의 수정 시각도 갱신되어
  실행 중인 세션이 종료 유예나 오래된 기록 정리에 걸리지 않게 한다.
- **복구 실패**: 저장한 이름이 해제됐거나 경로를 사용할 수 없으면 경고를 남기고 프로필 없이 복원한다. 반면 명시적 `reboot` 요청에서는 프로필 검증에 실패하면 종료 시퀀스를 시작하지 않는다.
- **수명**: `session-end`는 `ended_at`만 기록하고, 24시간 유예가 지난 뒤 sweep에서 삭제를 시도한다. 닫은 탭을 다시 열 때 프로필 조회와 다음 reboot의 승계 정보가 필요하기 때문이다. 종료 훅이 오지 않은 기록은 90일 TTL로 정리하며 session-start에서 기록 시각을 갱신한다. `--clear-profile`은 유예 없이 삭제를 시도한다.

기록 파일은 세션 ID로 찾는다. ID의 파일명 형식을 검증하지만, 이것이 다른 세션의 접근을 차단하는 권한 검사나 `claude -r`만 같은 ID를 사용할 수 있다는 보장은 아니다. 파일 작업 실패도 경고로 남으므로 모든 meta·기록 갱신을 하나의 원자적 작업으로 보지 않는다.
- **게이트도 같은 경로**: 프로필과 게이트는 같은 이름 공간을 사용하므로 게이트 이름으로 부착한 것도 그대로 복원된다. 같은 이름이 프로필↔게이트로 재등록됐으면 다음 복원은 **새 정의**로 해석한다(경로를 캐시하지 않는 것의 귀결).
- **범위 밖**: 레이아웃 프리셋은 세션 복원이 아니라 구조 템플릿이라 `restore_command` 를 저장하지 않는다 — 프리셋 적용으로는 프로필이 붙지 않는다. `spawn`/`launch`/`respawn --profile` 은 부착 기록을 만들지 않는다(반복 재기동은 `reboot` 만의 개념).

### Claude 세션 프로필 레지스트리

프로필 JSON을 이름으로 등록하고 `--profile <이름>`으로 부착한다. 레지스트리는 이 플러그인의 `profile.rs`가 관리한다.

- **등록**: `tasty claude profile-register <이름> --file <경로>` — `<경로>`(JSON object) 를 읽어 `TASTY_PLUGIN_DATA_DIR/profiles/registered/<이름>.json` 에 **복사본**으로 저장한다(원본이 나중에 옮겨지거나 지워져도 레지스트리는 영향받지 않는다). 이미 등록된 이름이면 내용을 덮어쓴다. 이름은 소문자/숫자/`-`, 최대 32자.
- **해제**: `tasty claude profile-unregister <이름>`.
- **목록**: `tasty claude profile-list` — **이름으로 부착 가능한 것 전부**를 보여준다: 등록 프로필(`user/<이름>`, `description` 없음) · 등록 게이트(`user/<이름>`, 게이트임을 알리는 `description`) · host 기본 게이트(`host/continue-checklist`). 여기에 항상 전역 설치돼 있는 내장 훅 9종(`host/<token>`, attachable 아님 — 위 "Claude Code 훅 통합" 절의 `install.rs::MANAGED_HOOKS` 를 그대로 나열, 정의를 복제하지 않는다)이 더해진다. `profile-list` 와 `gate-list` 가 둘 다 게이트를 보여주는 것은 의도된 중복이다 — 전자는 "부착 가능한 것들" 관점, 후자는 "게이트 정의"(본문·센티넬·상한·on/off) 관점.
- **조회**: `tasty claude profile-show <이름>` — 등록 프로필이면 원본 JSON 그대로, 게이트면 그 게이트를 발동시키는 **생성된 Stop 훅 조각**. `owner` 는 실제 출처를 그대로 반영한다(`user` 등록 프로필/등록 게이트, `host` 기본 게이트). `tasty claude profile-current [--surface <id>]` — 그 surface 에 지금 부착된 것(이름 또는 경로)과 내장 훅 목록을 함께 보여준다("지금 이 세션에 무슨 프로필/게이트가 걸려 있나").
- **이름 해석 순서(부착 시)**: `--profile <이름>` 으로 **부착할 때**의 순서다 — ① `profiles/registered/<이름>.json` → ② 등록 게이트 → ③ host 기본 게이트 → ④ 내장 훅 토큰이면 "attach 불가" 에러 → ⑤ 그 외 미등록 에러. ①과 ②는 등록 시점에 상호 배제되지만(아래 "Stop-훅 게이트 레지스트리" 의 이름 충돌 거부) 순서는 방어적으로 고정돼 있다. 위 `profile-show` 는 이 경로를 쓰지 않으므로 ④가 적용되지 않는다 — 내장 훅 토큰을 주면 "attach 불가" 가 아니라 미등록 에러(`no registered profile named 'user/stop'`)가 난다.
- **프로필 병합**(`crates/tasty-plugin-claude/src/profile_merge.rs`): 여러 이름을 지정하면 순서대로 읽어 JSON 하나로 만들고 `TASTY_PLUGIN_DATA_DIR/profiles/generated/<정렬된-이름들>.json`에 저장한다. 부착할 때마다 다시 만들며 등록 원본은 덮어쓰지 않는다. 병합 규칙은 다음과 같다.

  | 키 유형 | 예 | 규칙 |
  |---|---|---|
  | 훅 이벤트 배열 | `hooks.Stop` | JSON 값 전체가 같은 항목만 중복 제거하고 나머지는 이어 붙임 |
  | 객체 맵 | `env`, `enabledPlugins` | 키 단위 재귀 병합. 리프 값 충돌은 스칼라 규칙과 동일 |
  | 허용/거부 리스트 | `permissions.allow`/`deny` | union 후 **불변식 강제**: `deny` 에 있는 항목은 `allow` 에서 제거한다 — 프로필 조합 순서와 무관하게 deny를 항상 우선한다(테스트: `profile_merge::tests::deny_beats_allow_*`) |
  | 스칼라(대부분) | `theme`, `effortLevel` | 값이 다르면 경고 로그 남기고 나중 프로필 값으로 last-wins |
  | 스칼라(보안 민감) | `permissions.defaultMode` | 값이 다르면 **거부**(에러) — 권한 모드가 조합으로 조용히 약해지는 것을 last-wins 보다 우선 차단 |

- **저장 위치** — 전부 `TASTY_PLUGIN_DATA_DIR`(`~/.tasty/plugin-data/com.tasty.claude/`) 하위. 호스트가 이 디렉토리를 미리 만들어 주므로 `fs.write` 권한 없이도 쓸 수 있다. 호스트가 이 env 를 주입하지 않은 비정상 기동(`data_dir = None`)이면 등록/부착 모두 명시적 에러로 거부한다 — `~/.claude/` 나 새 경로를 조용히 쓰지 않는다.
- IPC: `claude.profile_register`/`claude.profile_unregister`/`claude.profile_list`/`claude.profile_show`/`claude.profile_current` — CLI 서브커맨드와 1:1 대응(IPC·CLI 양면, 에이전트 조작 가능성 — [API 규약](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/api-conventions.md#ipc-메서드를-cli로-제공하는-기준)).
- spawn 시 조회한 parent의 자식 수가 설정 임계치를 넘으면 응답에 `warning`을 넣는다.
  Settings › Plugin › Claude Code에서 임계치를 조정한다. 후보 목록은 `idle` 보고를 받은
  자식과 `confidence: confirmed`인 `stale` 자식을 구분한다. 후자는 마지막으로 조회한
  전경 이름이 셸이라는 뜻이며, 에이전트 종료나 훅 유실을 확인한 결과는 아니다.
  훅의 idle 보고도 진위나 작업 성공을 검증한 값은 아니므로 현재 상태를 확인한 뒤 재사용한다.
  `confidence: heuristic`인 `stale`은 긴 추론·SIGSTOP·무출력 작업과 구별되지 않아
  후보 수에 넣지 않는다. [자식 상태 판정](https://github.com/zilhak/tasty/blob/develop/docs/features/child-terminal/index.md)을 참고한다.

### 승인 정책 (`--permission-mode`)

`launch`, `spawn`, `respawn`, `reboot`, `child-profile`은 `permission_mode`를 받아
Claude 기동 명령의 `--permission-mode`로 전달한다. 허용 값은 `acceptEdits`, `auto`,
`bypassPermissions`, `manual`, `dontAsk`, `plan`이며 다른 값은 거절한다.
이 플러그인은 값의 의미를 다시 정의하지 않는다.

우선순위는 호출별 인자 → `default_permission_mode` 설정 → 플래그 미부착이다.
설정 기본값 `inherit`도 미부착을 뜻한다. 이 경우 사용자 자신의 Claude 설정이 적용된다.
모드를 지정하지 않은 무인 자식은 사용자 설정에 따라 승인을 기다릴 수 있다.
관측과 알림은 대기를 알려줄 뿐, 승인을 대신하지 않는다.

프로필의 `permissions.defaultMode`와 별도 permission mode가 함께 지정되면 거절한다.
`permissions.allow`와 `deny` 목록은 모드와 함께 사용할 수 있다.
`reboot`와 `child-profile`의 모드는 해당 재시작에만 적용하고 `restore.command`에 넣지 않는다.
Codex와 옵션을 억지로 맞추지 않으며, 각각의 기본값은 해당 플러그인 문서를 따른다.

### Stop-훅 게이트 레지스트리

Stop 게이트는 턴을 끝내기 전에 확인할 본문, 종료 표시 문자열(sentinel), 반복 상한을
이름으로 등록한 것이다. 선택 이유는 [에이전트 작업 조율](https://github.com/zilhak/tasty/blob/develop/docs/features/agent-collaboration/index.md)을 따른다.

| 명령 | 동작 |
|---|---|
| `gate-register <이름> --body-file <경로> [--sentinel <문자열>] [--rounds <n>]` | 본문을 복사해 저장. 같은 이름은 정의와 본문을 갱신 |
| `gate-unregister <이름>` | 정의·본문·마커·반복 상태 제거. 실행 상태 정리 실패는 경고 |
| `gate-list` | 사용자 및 내장 게이트의 실효 sentinel·상한·상한 출처·enabled 표시 |
| `gate-show <이름>` | 정의와 본문 조회. 사용자 정의가 없으면 내장 게이트 조회 |

각 명령은 `tasty claude` 뒤에 붙이며 대응 IPC는 `claude.gate_*`다. 전용 GUI는 없다.
이름은 소문자·숫자·`-`로 최대 32자다. 등록 본문에는 비어 있지 않은 실효 sentinel이
있어야 하며 `--rounds`는 1 이상이다. 프로필과 게이트는 같은 이름을 사용할 수 없으며
양쪽 등록 경로에서 충돌을 검사한다. 사용자 정의가 같은 이름의 내장 게이트를 대체하면
목록에는 실효 사용자 항목만 표시한다.

데이터는 `TASTY_PLUGIN_DATA_DIR` 아래에 둔다.

| 경로 | 내용 |
|---|---|
| `gates/registered/<이름>.json` | 실효 sentinel, 선택한 round_limit |
| `gates/bodies/<이름>.md` | 등록한 본문의 복사본 |
| `checklist/gates/<이름>/enabled.marker` | 재시작 없이 바꾸는 활성 상태 |
| `checklist/gates/<이름>/rounds/<session_id>.json` | 게이트·세션별 반복 수 |

내장 `continue-checklist`는 파일로 만들지 않고 코드와 번역 카탈로그에서 제공한다.
데이터 디렉터리가 없으면 등록·해제는 실패하고 조회는 내장 게이트만 반환한다.

`launch`, `spawn`, `respawn`, `reboot`의 `--profile <게이트이름>`으로 부착한다.
`--profile gate-a,gate-b`처럼 여러 게이트나 일반 프로필을 함께 사용할 수 있다.
등록 프로필 → 등록 게이트 → 내장 게이트 → 내장 훅 토큰 순서로 이름을 해석한다.

부착 명령에는 `checklist-hook --gate <이름> --block-cap "${CLAUDE_CODE_STOP_HOOK_BLOCK_CAP:-}"`만 넣고 본문·상한은 넣지 않는다.
`--block-cap`은 Claude Code의 연속 block 상한을 플러그인에 알린다(아래 "Stop 게이트와 idle").
훅이 실행될 때 다시 읽으므로 재등록한 내용이 재부착 없이 반영된다.
이름이 셸 명령에 들어가므로 허용 문자를 넓히면 인용·이스케이프도 함께 바꿔야 한다.
명령 문자열은 `install.rs::tasty_guarded_command`를 공유한다.

### Stop-훅 게이트 판정

`checklist-hook`은 선택한 게이트의 sentinel·본문·상한을 읽고 다음 순서로 판단한다.

1. 저장된 prompt ID가 다르거나 상태가 없으면 반복 수를 0으로 본다.
2. 마지막 응답에 sentinel이 있으면 통과한다.
3. 반복 상한에 도달했으면 통과한다.
4. 나머지는 `decision: block`과 본문을 reason으로 반환하고 반복 수를 1 늘린다.

상한은 게이트의 `--rounds` → `continue_checklist_round_limit` 설정 → 3 순서다.
설정 키에 continue_checklist라는 이름이 남아 있지만, 상한을 지정하지 않은 사용자
게이트에도 같은 기본값을 적용한다. `round_limit_source`는 gate 또는 settings로 출처를 표시한다.
등록 본문은 매번 파일에서 읽고 내장 본문만 플러그인 기동 시 읽은 번역을 사용한다.

`--gate`를 생략하면 continue-checklist를 사용한다. 미등록 게이트, 꺼진 마커,
세션·prompt ID 부재, stdin 파싱 실패에서는 차단하지 않는다.
미등록 훅은 상태 파일을 만들거나 설정 조회를 보내지도 않는다.
Stop payload에는 게이트 이름이 없으므로 이름은 명령 인자로 전달한다.
`stop_hook_active`는 입력 부재와 false를 구분하기 위해 CLI 스키마에서 string으로 받고
핸들러에서 해석한다. 판정 자체는 위 반복 수와 sentinel을 사용한다.

게이트 여러 개가 붙으면 각각의 Stop 훅이 실행되고 각각의 reason이 전달될 수 있다.
하나라도 block이면 계속되므로 한 게이트가 상한에 도달해도 다른 게이트는 계속 막을 수 있다.
반복 수를 게이트·세션별로 분리하는 이유다.

#### 본문에 goal 넣기

본문에 `{{goal}}`이 있고 block을 반환할 때만 훅이 `memory.goal_get`을 호출한다.
goal이 있으면 현재 언어의 goal 절로 바꾸고, 없거나 조회에 실패하면 토큰이 있는 줄을
제거한다. 토큰이 없는 본문은 바꾸지 않는다. 번역에서도 토큰과 삽입 자리를 유지해야 한다.

목표는 서피스 범위에 둔다. CLI가 쓰고 플러그인은 읽기만 하며 부모의 목표를 자동 상속하지 않는다.
`surface` 인자를 생략하면 CLI가 `TASTY_SURFACE_ID`를 사용하므로 옛 훅 명령도 동작한다.
이 지시는 목표 범위에서 사용자 판단 없이 진행할 수 있는 일을 계속하도록 안내한다.
목표 충족 자체를 계산하거나 작업 성공을 보증하는 검사는 아니다.

### continue-checklist 세션 프로필

내장 게이트 `host/continue-checklist`를 `--profile continue-checklist`로 붙인다.
전역 install에는 포함되지 않으며 부착한 세션에서만 동작한다.
기본 sentinel은 `[[TASTY-CHECKLIST-DONE]]`이다.
본문은 요청 충족 여부, 실제 검증 여부, 남은 작업을 확인하고 선택적으로 goal을 덧붙인다.
goal이 없으면 범위를 넓혀 계속 진행하라는 지시는 하지 않는다.

`checklist-enable`, `checklist-disable`, `checklist-status`에 `--gate <이름>`을 붙여
마커를 제어한다. 생략하면 continue-checklist다. enable/disable은 미등록 이름을 거절하고,
status는 `{ enabled: false, registered: false }`로 답한다. 데이터 디렉터리가 없을 때
변경은 실패하고 status는 enabled false로 응답한다.

마커는 매 훅마다 확인하므로 이미 실행 중인 세션에도 즉시 적용된다.
옛 `checklist/enabled.marker`는 내장 게이트 위치로 한 번 옮긴다.
SessionEnd는 모든 게이트에서 해당 세션의 반복 파일을 지우고 옛 `checklist/rounds/`의
같은 세션 파일도 정리한다. 옛 반복 수는 읽거나 이전하지 않는다.

### 부모 완료 알림 mod

`tasty claude install`은 기존 훅과 함께 Claude Code `tasty-notify` mod와 중복 제출을 막는 `tasty-notify-gate` 플러그인을 고정 관리 경로에 나란히 설치한다. 지원되는 Claude에서는 세션 시작부터 자기 완료 로그를 감시하고 플러그인 출처의 새 턴으로 전달한다. 동작 확인 표식이 있는 세션은 Monitor를 함께 켜지 않는다. 설정 부모 경로의 별칭은 같은 관리 위치로 해석하며, 완전히 소비한 로그의 불필요한 본문 재읽기를 생략한다. 같은 등록의 중복 시작은 진행 중인 제출·체크포인트 소유자를 유지하고, 재로딩 중 대기 중이던 알림은 gate가 한 번만 들여보낸다. 경로 선택, 재개·보존 한계, 버전과 검증 범위는 [완료 알림 mod](notify-mod.md)에 있다. 기존 사용자는 install을 다시 실행한 뒤 새 Claude 프로세스를 시작한다. 설치·로컬 시험만으로 실제 모델 턴 수신이나 작업 성공이 입증되지는 않는다.

### Claude Code 훅 통합

훅은 호스트 호출에 실패해도 뒤의 로컬 정리를 계속한다. 응답의 `host_call_failures`에 실패한 호출 수를 넣고, 0이 아니면 `<tasty_home>/hook-failures.log`에도 기록을 시도한다. 따라서 응답의 `ok`만으로 모든 호출이 성공했다고 판단하지 않는다. [오류 처리](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/error-handling.md)와 [훅 실행 설계](https://github.com/zilhak/tasty/blob/develop/docs/features/hooks/index.md)를 참고한다.

`tasty claude install`이 `~/.claude/settings.json`의 `hooks`에 아래 10개 항목을 심는다. 이벤트는 9개이며 `PostToolUse`에는 matcher가 다른 항목이 둘이다. `background-start`를 뺀 모든 항목이 같은 형태의 명령 문자열을 쓴다(`background-start`는 아래 "백그라운드 작업이 남은 `StopFailure`"의 셸 판정 명령):

```
if [ -n "$TASTY_SURFACE_ID" ]; then tasty claude hook <token> || true; fi
```

`TASTY_SURFACE_ID`가 없으면 Tasty 밖에서 실행한 것으로 보고 훅 명령을 호출하지 않는다. 값이 있으면 훅을 실행하며, `|| true`는 훅 실패가 에이전트 턴을 막지 않게 한다. 훅 전달 실패는 `<tasty_home>/hook-failures.log`에 기록하는 경로를 사용한다([CLI 진단 로그](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/cli-structure.md)).

명령 문자열 생성은 `install.rs::tasty_guarded_command` 한 곳뿐이다 — 세션 프로필(`continue-checklist`)의 hook 명령도 같은 함수를 쓴다.

> **기존 사용자는 `tasty claude install` 재실행이 필요하다.** 명령 문자열은 사용자의 `settings.json` 에 이미 기록돼 있어, plugin 을 업데이트해도 옛 문자열 그대로다. 재실행하면 marker(`tasty claude hook <token>`) 가 일치하는 기존 entry 를 찾아 **제자리 갱신**하므로 entry 가 중복되지 않는다. 새로 넣은 항목이 없어도 기존 entry 의 matcher 나 명령이 현재 값과 다르면 파일을 다시 쓰며, 응답의 `installed` 에 새로 넣은 이벤트, `updated` 에 갱신한 이벤트를 적는다. 바뀐 것이 없으면 파일을 쓰지 않는다.

`session_id`/`prompt_id`/`message`/`notification_type`/`error`/`agent_id`/`background_tasks`/`tool_name`/`tool_response`/`prompt`/`source`/`reason`/`last_assistant_message` 같은 이벤트별 가변 데이터는 명령 인자가 아니라 **stdin JSON**으로 들어온다 — 매니페스트 `hook` cli 항목이 `stdin_json = true`를 선언하고, `--session`/`--prompt-id`/`--message`/`--notification-type`/`--error`/`--agent-id`/`--background-tasks`/`--waiting-on-background-work`/`--tool-name`/`--tool-response`/`--prompt`/`--source`/`--reason`/`--last-assistant-message` 플래그가 각각 `stdin_field`로 stdin JSON에서 자동 채워진다. 공식 hooks 문서와 실측 payload에는 없지만 `waiting_on_background_work`도 `--waiting-on-background-work`로 방어적으로 읽는다. `--background-tasks`·`--tool-response`와 이 인자는 문자열로 선언해 stdin의 배열·객체·bool 값을 그대로 넘긴다(Claude Code가 hook 실행 시 stdin으로 JSON payload를 준다). POSIX 셸 구문 1종만 발행한다 — [codex](../codex/index.md)처럼 Windows PowerShell 분기는 없다.

| Claude Code 이벤트 | matcher | tasty hook token | `terminal.set_state` | `surface.fire_hook` | surface meta | `surface.completion` kind |
|---|---|---|---|---|---|---|
| `Stop` | `""`(전체) | `stop` | `idle`. 단 백그라운드 작업을 기다리는 Stop(아래)은 `active`, Stop 게이트가 붙은 세션은 판정이 모일 때까지 `active`(아래 "Stop 게이트와 idle") | `claude-idle`(대기 Stop은 없음) | 대기 Stop은 `claude-background-wait` **set**, 그 밖의 Stop은 **unset** | `completion`(대기 Stop은 없음) |
| `SubagentStop` | `""`(전체) | `subagent-stop` | — (로그만) | — | — | — |
| `StopFailure` | `""`(전체) | `stop-failure` | `idle`. 단 메인 턴이 띄운 백그라운드 작업이 남아 있으면 `active`(아래) | `claude-idle` + `claude-stop-failure`(백그라운드 작업이 남은 경우는 `claude-stop-failure`만) | `claude-last-stop-failure` = stdin `error`(없으면 `unknown`) **set**. `claude-background-wait` **unset**. 백그라운드 작업이 남은 경우는 `claude-background-wait`를 **unset** 대신 **set**한다 | `completion`(백그라운드 작업이 남은 경우는 없음) |
| `SessionEnd` | `""`(전체) | `session-end` | `idle` | `claude-idle` | `claude-session-id`·`restore.command`·`claude-last-stop-failure`·`claude-background-wait` **unset**. stdin `reason`이 `clear`·`resume`이 아니고(값이 없을 때 포함) 이 세션이 `claude-settings-session`과 같으면 `claude-settings-file`·`claude-settings-session`도 **unset** (프로필 meta 2키는 건드리지 않는다. 프로필 **부착 기록**에는 종료 표시만 하고 유예 뒤 회수 — 아래 "복원을 건너 프로필이 유지되는 방식") | `completion` |
| `Notification` | `""`(전체) | `notification` | `notification_type`별(아래 "Notification 유형별 상태"). `needs_input`·`active`·변경 없음 | `needs-input`(`needs_input` 유형만) | — | `needs_input`(`needs_input` 유형만) |
| `UserPromptSubmit` | `""`(전체) | `prompt-submit` | `active` | — | `claude-last-stop-failure`·`claude-background-wait` **unset** | — |
| `SessionStart` | `""`(전체) | `session-start` | stdin `source`가 `startup`·`resume`·`clear`·`fork`이면 `idle`(입력 대기 화면). 단 플러그인이 프롬프트를 실어 실행한 세션(`claude-launch-prompt` meta)은 곧 그 턴을 시작하므로 `active`. 값이 없거나 모르는 값이면 `active`. idle 이어도 턴이 끝난 것이 아니므로 `claude-idle`·화면 알림·telemetry 는 없다. `compact`(대화 압축 뒤의 시작)는 상태를 바꾸지 않는다. 압축은 턴 도중에도, 턴이 끝나 쉬는 중에도 일어나고(Claude Code의 idle compaction, 화면에 "Compacted while idle, before the prompt cache expired") 같은 세션이 이어진다. 쉬는 중의 압축 뒤에는 Stop 이 오지 않으므로 `active`로 바꾸면 끝난 자식이 계속 `active`·`stale`로 남는다. 그래서 턴 도중이면 `prompt-submit`의 `active`가, 쉬는 중이면 Stop 의 `idle`이 그대로 남는다. 새 턴 처리(보류 Stop 확정·백그라운드 작업 기록 정리·자동 재개 예약 취소와 연속 시도 수 초기화·wall_time 시작·정지 관측 초기화)도 하지 않는다 | — | `claude-last-stop-failure`·`claude-background-wait` **unset**(`compact`는 제외). `claude-launch-prompt`가 있으면 **unset**(spawn·respawn 이 프롬프트를 실어 실행할 때 **set**, 프롬프트 없이 실행하면 **unset**). `claude-session-id`·`claude-settings-session` = 세션 ID, `restore.command` = `claude -r <id>` **set**(stdin JSON에 `session_id`가 없으면 건너뜀). 프로필이 부착돼 있으면 `claude -r <id> --settings "<경로>"` 로 쓰고, stdin `source`가 `resume`이면 그 경로를 `claude-settings-file`에 **set**하며, 복원으로 프로필 meta 가 사라졌으면 부착 기록에서 **복구**한다(아래 "복원을 건너 프로필이 유지되는 방식") | — |
| `PreToolUse` | `AskUserQuestion` | `pre-tool-use` | `needs_input` | `needs-input` | — | `needs_input` |
| `PostToolUse` | `AskUserQuestion` | `post-tool-use` | `active` | — | — | — |
| `PostToolUse` | `Bash\|Agent\|Task` | `background-start` | — (백그라운드 작업 기록만) | — | — | — |

`surface.completion` 은 `{ surface_id, kind }` 로 호출되며(`HostCall::SurfaceCompletion`,
`hook.rs`), `kind` 는 위 표의 값을 그대로 싣는다 — 호스트의 `AttentionStore` 가
`NeedsInput` 을 `Completion` 보다 높은 우선순위로 표시한다(surface 테두리 노랑, 탭
제목·워크스페이스 배지도 동일 우선순위, [surface-highlight](https://github.com/zilhak/tasty/blob/develop/docs/features/surface-highlight/index.md)
참고).

`UserPromptSubmit`은 child가 2번째 이후 prompt를 받을 때 직전 `Stop` hook이 남긴 `idle=true` 잔재를 지우는 데 필수다 — 미등록 시 실제로는 active인 child를 idle로 오보고하는 상태 버그가 생긴다.
`PreToolUse`/`PostToolUse`만 matcher `AskUserQuestion`으로 좁혀 등록돼 그 툴 호출에만 발생한다(나머지 7개는 matcher `""`로 이벤트 전체를 받는다) — 실측(실제 Claude Code를 띄워 hook stdin payload를 덤프해 확인) 결과 `AskUserQuestion` 답변은 `UserPromptSubmit`을 발생시키지 않으므로(질문/답변이 같은 prompt turn 안의 tool 상호작용이라 새 프롬프트로 집계되지 않음), 기존 `UserPromptSubmit`(→active)만으로는 이 케이스의 needs_input 해제 시점을 잡을 수 없다.
`PreToolUse`가 질문 UI가 뜨기 **전에** 발생해(`tool_input.questions` 포함) needs_input을 켜고, `PostToolUse`가 답변 즉시(관찰상 `duration_ms: 0`) 그 짝으로 active로 되돌린다 — `needs_input`은 이제 `Notification`과 `PreToolUse` 두 경로에서 나온다.

#### agent task 턴 보고

훅은 [agent task](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/agent-runner.md#agent-task)의 턴 경계를 `agent.task_turn_report`(`provider: "claude"`)로 호스트에 알린다. 이 호출에는 매니페스트의 `agent.turn_report` 권한을 쓴다(다른 agent.* 는 열지 않는 권한). 상태 갱신보다 먼저 보낸다. 시작 보고(prompt-submit)에는 훅이 받은 프롬프트에 지시 끝의 회차 표지(`[tasty-task-attempt:<token>]`)가 있었는지와 그 토큰을 싣고(`prompt_seen`·`attempt_marker`), 프롬프트 본문은 보내지 않는다. 호스트는 그 surface 에 묶인 agent task 회차가 있을 때만 적용하므로 일반 세션에는 영향이 없다. 보고 실패는 경고 로그만 남기고 훅의 다른 처리를 막지 않는다. 변환은 `task_turn.rs` 에 있다.

| 훅 token | 보고 |
|---|---|
| `prompt-submit` | `turn_started` |
| `stop` | `turn_ended` + `final_answer`(stdin `last_assistant_message`). 백그라운드 작업을 기다리는 Stop 은 턴을 끝내지 않으므로 보고하지 않는다. 게이트가 보류한 Stop 은 그 자리에서 보고하지 않고, 보류가 턴 종료로 확정될 때(모든 판정 통과, 연속 block 상한, 판정 시간 초과, 같은 세션의 새 턴) 보류할 때 보관한 그 Stop 의 `final_answer` 로 한 번 보고한다. block 으로 확정된 Stop 은 턴이 이어지므로 보고하지 않는다. 새 턴으로 확정될 때는 앞 턴의 끝을 새 턴의 `turn_started` 보다 먼저 보낸다 |
| `stop-failure` | `turn_ended` + `error`(stdin `error`, 없으면 `unknown`) |

#### Notification 유형별 상태

`Notification`은 `notification_type`에 따라 처리한다. 아래 표는 공식 hooks 문서의 Notification matcher 값 목록 전부다.
`needs_input`은 상태를 `needs_input`으로 바꾸고 `needs-input` 훅과 `needs_input` 화면 알림을 함께 보낸다.
`active`는 상태만 바꾼다. 변경 없음은 호스트 호출을 만들지 않는다. 입력 토큰 기록(`input_tokens`)은 유형과 무관하게 `message`에서 읽는다.

| 유형 | 상태 | 이유 |
|---|---|---|
| `permission_prompt` | `needs_input` | 도구 사용 승인 대기 |
| `elicitation_dialog` | `needs_input` | MCP 서버의 입력 폼 대기 |
| `elicitation_url_dialog` | `needs_input` | MCP 서버의 URL 열기 요청 대기 |
| `quota_auto_resume_stale` | `needs_input` | 사용량 한도가 풀린 뒤 `Enter`를 기다린다 |
| `agent_needs_input` | `needs_input` | agent view의 다른 백그라운드 세션이 기다리는 경우와 auto mode의 classifier 요금 안내(약 6초 동안 입력이 없을 때)도 이 유형이지만, payload로 teammate 설정 질문과 구분할 수 없어 이 세션의 대기로 본다 |
| `quota_auto_resume_fired` | `active` | 사용량 한도로 멈췄던 작업을 이어 간다 |
| `idle_prompt` | 변경 없음 | 응답을 마친 지 약 60초 뒤의 재알림이다. 턴 종료는 `Stop`이 이미 기록했다 |
| `auth_success` | 변경 없음 | 인증 완료. 입력 대기가 아니다 |
| `agent_completed` | 변경 없음 | 다른 백그라운드 세션의 종료다 |
| `elicitation_complete` | 변경 없음 | 대기가 풀렸다는 알림이다. `needs_input`의 원인을 기록하지 않으므로 `active`로 바꾸면 겹친 다른 대기까지 지운다 |
| `elicitation_response` | 변경 없음 | `elicitation_complete`와 같다 |
| `quota_auto_resume_disabled` | 변경 없음 | 사용량 한도 대기를 이어 가지 않고 끝냈다. 입력 대기가 아니다 |

- `notification_type`이 없으면 `needs_input`으로 처리한다. 유형 필드가 생기기 전 버전과 같은 동작이다.
- 목록에 없는 값은 상태를 바꾸지 않고 plugin 로그에 경고를 남긴다. 입력 대기가 아닌 새 유형을 `needs_input`으로 받으면 `spawn`/`tell` 대기 노드와 `--depends-on` 후속 작업이 자식 턴 도중에 진행된다. 입력 대기 유형이 새로 생기면 이 표와 코드 목록에 추가한다.
- 이 표는 `hook.rs`의 `NOTIFICATION_EFFECTS`와 같은 목록이다. plugin 크레이트 테스트가 유형과 상태를 양방향으로 비교한다.

`StopFailure`는 API 오류로 끝난 턴을 알린다. `529 Overloaded`, rate limit, 인증 실패 등의
오류가 여기에 해당한다. 이 훅에서는 상태를 idle로 바꾸고 `claude-idle`도 보내 부모의
상태 알림이 실행될 수 있게 한다. 별도로 `claude-stop-failure`를 발생시킨다.

오류 종류는 `claude-last-stop-failure` meta에 기록한다. stdin의 `error` 값은
`rate_limit`·`overloaded`·`authentication_failed`·`billing_error`·`invalid_request`·
`server_error`·`max_output_tokens`·`unknown` 등이다. Surface의 Custom 훅은 값 없이
이벤트 키만 전달하므로 이 meta를 사용한다([훅](https://github.com/zilhak/tasty/blob/develop/docs/features/hooks/index.md)).
Meta를 먼저 쓴 뒤 이벤트를 보내며 새 턴(`prompt-submit`/`session-start`/`active`, 압축 뒤의 `session-start`는 제외)과
`session-end`에서는 지운다. `notify-done`은 meta가 있고 자식 상태가 `idle`이면 상태 알림 뒤에 오류 종류를 붙인다. `needs_input`·종료 등 다른 상태의 줄에는 붙이지 않는다.

**백그라운드 작업이 남은 `StopFailure`는 턴 종료가 아니다.** `StopFailure` payload에는 `background_tasks`가
없다(공식 hooks 문서, Claude Code 2.1.285 실측). 백그라운드 작업이 끝나면 Claude Code는 `<task-notification>` 턴을
열고 그 턴의 `Stop`이 다시 온다. 그래서 플러그인은 메인 턴이 띄운 백그라운드 작업을 surface별로 따로 기록한다.

- 시작: `PostToolUse`(matcher `Bash|Agent|Task|Monitor`, token `background-start`)의 `tool_response`에서 읽는다.
  Bash는 `backgroundTaskId`, Agent는 `isAsync: true` 또는 `status: "async_launched"`와 함께 오는 `agentId`다(2.1.290 실측).
  다른 도구 호출과 포그라운드 호출은 기록하지 않으며 상태도 바꾸지 않는다.
  Monitor는 이 기록에 넣지 않고 아래 "Monitor 기록"에 따로 둔다. 이 기록이 `StopFailure`를 `active`로 둘지 정하기 때문이다.
  이 훅이 끝날 때까지 Claude Code는 다음 API 요청을 보내지 않으므로, 설치 명령은 stdin payload에 `"backgroundTaskId":"`, `"isAsync":true`,
  `"tool_name":"Monitor"` 중 하나가 있을 때만 `tasty claude hook background-start`를 실행하고 나머지는 셸에서 바로 끝낸다(2.1.291·2.1.319 payload는 공백 없는 JSON).
  이 matcher와 필터를 갖기 전의 훅이 설치돼 있으면 `tasty claude install`을 다시 실행하고 새 Claude 프로세스를 시작해야 Monitor를 구별한다. 그 전에는 Monitor도 셸로 보고 알린다.
  실측 지연과 한계는 docs/features/child-terminal/index.md 결과 절에 있다.
- 끝: `UserPromptSubmit`의 `prompt`가 `<task-notification>`으로 시작하면 그 안의 `<task-id>` 작업을 지운다.
- `Stop`의 `background_tasks`를 읽을 수 있으면 기록을 그 목록의 끝나지 않은 항목으로 바꾼다.
  `SessionStart`(압축 뒤의 `compact`는 제외)·`SessionEnd`는 기록을 버린다.
- 기록이 남은 채 `StopFailure`가 오면 대기 Stop과 같이 `active`와 `claude-background-wait`를 보내고,
  `claude-last-stop-failure`를 기록한 뒤 `claude-stop-failure`만 fire한다. 두 meta는 이벤트보다 먼저 기록한다.
  `claude-idle`·`surface.completion`·telemetry `wall_time_ms`·자동 재개 처리는 하지 않는다.
  작업이 끝나 열린 턴의 `Stop`이 idle을 기록한다.
- spawn·tell은 `claude-stop-failure`에 상시 훅 `tasty claude notify-stop-failure`를 등록한다. 이 훅은 `claude-background-wait`가
  있을 때만 부모 완료 로그에 `[error] `로 시작하는 한 줄을 남긴다. 영어 문구는 `surface <N>: the turn ended on an API error (<오류>), but background work (<종류>) is still running, so the child keeps waiting`이다.
  대기 기록이 없는 StopFailure(턴 종료)는 `claude-idle` 완료 줄이 오류를 적으므로 이 훅은 아무것도 쓰지 않는다.
  작업이 끝나 열린 턴이 `claude-last-stop-failure`를 지우므로, 그 턴의 완료 줄에는 오류가 붙지 않는다.
- 작업이 바쁜 턴 중에 끝나 `<task-notification>` 턴 없이 결과가 소비된 뒤 `StopFailure`가 오면, 기록이 남아 `active`로 머물다
  대기 정지 알림까지 갈 수 있다. 이 경우는 미측정이다.
- 기록은 메모리에만 있다. 플러그인이 다시 시작되면 사라지고 그 뒤의 `StopFailure`는 idle이 된다.
  이 훅은 `tasty claude install`을 다시 실행해야 설치된다. 설치 전에는 전처럼 idle이 된다.

**백그라운드 작업을 기다리는 Stop은 턴 종료가 아니다.** Claude Code는 백그라운드
서브에이전트나 `run_in_background` 셸이 실행 중이어도 메인 응답을 끝내고 `Stop`을 보낸다.
작업이 끝나면 `<task-notification>`으로 시작하는 prompt의 `UserPromptSubmit`으로 새 턴을 열고,
그 턴의 `Stop`이 실제 종료가 된다. 그래서 `stop` 훅은 stdin의 `waiting_on_background_work`가
있으면 그 값으로, 없으면 `background_tasks`에 끝나지 않은 항목이 있는지로 대기 여부를 판정한다.
끝난 항목은 `status`가 `completed`·`failed`·`cancelled`·`canceled`·`killed`·`stopped`·`error`·`done` 중 하나(대소문자 무관)인 항목이며, 그 밖의 값이나 `status`가 없는
항목은 끝나지 않은 것으로 센다. 항목의 종류(subagent·shell)는 가리지 않으며 Monitor만 남아도 대기다. 대기 Stop이면 상태를
`active`로만 보고하고 `claude-idle`·`surface.completion`·telemetry `wall_time_ms`·자동 재개의
성공 처리를 하지 않는다. 두 필드가 없거나 해석할 수 없으면 이전처럼 턴 종료로 처리한다.
대기 Stop은 surface meta `claude-background-wait`에 대기 시작 시각(`since_ms`, Unix ms)·끝나지 않은 작업 수(`tasks`)·
작업 종류(`types`)·Monitor를 뺀 작업 종류(`notice_types`)·정지 알림 여부(`notified`)·출력 파일 단서(`watch`: `session`·`cwd`·작업 `id`와 이름)를 JSON으로 남긴다. 같은 대기에서 이어진 대기 Stop은 시작 시각과 알림 여부를 유지한다.
정지 알림을 보내면 `notified`를 `true`로 다시 쓴다. 대기가 아닌 `Stop`·백그라운드 작업이 남지 않은 `StopFailure`·
새 턴(`UserPromptSubmit`·`SessionStart`, 압축 뒤의 `compact`는 제외)·`SessionEnd`가 이 meta를 지운다. 정지 알림은 이 대기를 따로 다룬다(아래 "정지 알림").
Claude Code 2.1.283 실측 payload에는 `background_tasks`(항목 `id`·`type`·`status`·`description`·
`agent_type` 또는 `command`)만 있고 `waiting_on_background_work`는 없었다. 공식 hooks 문서
(2026-09-28 확인)의 `background_tasks` 항목 필드도 실측과 같은 모양이다. 문서는 `status`를 값 목록 없이
"Current task status"로만 설명하고 `waiting_on_background_work`는 다루지 않는다. 문서는 항목을 진행 중인(in-flight)
작업이라 하고 진행 중인 작업이 없으면 배열이 비어 있다고 설명하므로, 끝을 뜻하는 값이 아니면 모르는 값도
대기로 본다. Tasty는 문서에 없는 `waiting_on_background_work`도 방어적으로 읽는다.
끝나지 않는 백그라운드 명령(개발 서버 등)을 남긴 채 턴을 끝내면 그 자식은 idle이 되지 않는다.
판정 근거와 재검토 조건은 [에이전트 상태와 완료 전달](https://github.com/zilhak/tasty/blob/develop/docs/features/child-terminal/index.md)에 있다.

#### Stop 게이트와 idle

Stop 게이트가 `block`을 반환하면 Claude Code는 턴을 끝내지 않고 이어 간다. 그 Stop은 턴 종료가 아니다.
Claude Code는 한 Stop의 훅을 병렬로 실행하므로 상태 훅(`claude hook stop`)과 게이트 판정(`claude checklist-hook`)은
어느 쪽이든 먼저 도착할 수 있다. 플러그인은 요청을 한 워커에서 차례로 처리하므로 한 요청이 다른 요청을 기다리지 않고
메모리의 표에서 둘을 짝짓는다.

- 게이트 수: Stop마다 지금 실행 중인 Claude의 settings 파일에서 `hooks.Stop`의 `tasty claude checklist-hook` 명령 수를 센다.
  경로는 surface meta `claude-settings-file`에서만 읽는다. `launch`·`spawn`·`respawn`·`reboot`가 Claude를 실행할 때 기록하고(프로필 없이 실행하면 지운다),
  `--resume`으로 다시 연 세션(SessionStart `source: resume`)은 복원 명령에 붙인 프로필 경로를 기록한다.
  실행 기록은 `claude-settings-session`을 지우고, 그 뒤 처음 시작한 세션의 SessionStart가 이 meta에 세션 id를 적어 경로의 소유 세션이 된다.
  소유 세션의 Claude 프로세스가 끝나면(SessionEnd의 `reason`이 `clear`·`resume`이 아니면) 두 meta를 지운다.
  respawn·reboot는 새 경로를 기록한 뒤 이전 Claude를 끝내므로 이전 세션의 SessionEnd가 늦게 와도(2.1.290에서 `reason: other`) 새 경로를 지우지 않는다. 그래서 같은 surface에서 사용자가 직접 실행한
  `claude`는 게이트가 없는 것으로 세고 idle이 늦어지지 않는다. 프로필 meta 2키(`claude-session-profile`·`claude-session-profile-names`)는
  다음 실행에 다시 붙일 프로필이라 게이트 수에 쓰지 않는다. 사용자가 직접 `--settings`를 붙여 실행한 Claude는 알 수 없어 게이트가 없는 것으로 센다.
  SessionEnd 없이 끝난 Claude(강제 종료 등)의 meta는 다음 실행 기록이나 SessionEnd까지 남는다. 프로필이 붙은 surface에서 사용자가 `--settings` 없이
  `claude -r`로 다시 열면 게이트가 있는 것으로 세어 Stop마다 idle이 최대 5초 늦는다. 같은 명령 문자열은 한 번만 센다. Claude Code는 같은 handler를
  한 번만 실행한다(공식 hooks 문서는 settings 파일이 여럿인 경우를 적고, 한 파일 안의 중복도 2.1.285에서 한 번 실행됐다).
- 게이트가 없거나 payload에 `session_id`가 없으면 지금처럼 곧바로 idle을 기록한다.
- 게이트가 있으면 상태를 `active`로 보내고 idle 처리(`claude-idle`·`surface.completion`·telemetry `wall_time_ms`·자동 재개 성공 처리)를
  보류한다. 같은 `session_id`·`prompt_id`의 게이트 판정이 게이트 수만큼 모이면 확정한다. 하나라도 `block`이면 턴이 이어지므로
  `active`를 유지하고, 모두 통과하면 보류한 idle 처리를 실행한다.
- 판정이 5초 안에 다 오지 않으면 온 판정만으로 확정하고 오지 않은 판정은 통과로 본다. 그 판정이 다음 Stop 전에 늦게 오면 버린다.
  다음 Stop이 오면 앞 Stop의 판정은 더 오지 않으므로(한 Stop의 훅이 모두 끝나야 턴이 이어진다) 빈자리를 지운다.
  시간 초과로 확정한 idle은 그 사이 같은 세션에 새 턴이 시작됐으면 보내지 않는다.
  격리 debug 인스턴스에서 게이트 CLI 한 번은 최대 115ms(80회 중앙값 99ms)였다.
- Claude Code는 연속 block을 8회(`CLAUDE_CODE_STOP_HOOK_BLOCK_CAP`로 변경)까지만 받아들이고 그 뒤의 block은 무시하고 턴을 끝낸다.
  플러그인은 `prompt_id`별로 연속 block을 세고 상한에 닿은 block은 통과로 본다. 게이트 명령은 환경 변수 값을 `--block-cap`으로 넘긴다.
  Claude Code의 상한은 tasty 게이트가 아닌 Stop 훅의 continuation도 세지만 플러그인은 tasty 게이트의 block만 센다. 그런 훅이 함께
  턴을 이어 가게 하면 상한으로 끝난 턴의 idle이 기록되지 않을 수 있다.
- 보류 중에 같은 세션의 새 턴(`prompt-submit`·`session-start`·`active`, 압축 뒤의 `session-start`는 제외)이 오면 보류한 Stop에서 턴이 끝난 것이므로
  새 턴의 `active`보다 먼저 idle을 기록한다. 판정이 모이기 전에 다음 Stop이 오면 앞 Stop은 block된 것으로 확정한다.
  `session-end`는 보류 기록을 버린다.
- 백그라운드 작업을 기다리는 Stop도 게이트가 판정한다. 그 판정은 연속 block 수에만 쓰고 상태는 이미 보고한 `active`다.
- block으로 확정된 Stop도 대기가 아닌 Stop이므로 `claude-background-wait` 기록과 meta를 지운다.
- 표는 메모리에만 있다. 보류 중에 플러그인이 다시 시작되면 그 Stop의 idle은 기록되지 않고 다음 훅이 상태를 정한다.

`SubagentStop`은 서브에이전트가 끝났다는 신호이며 메인 턴의 끝이 아니다. 상태·`claude-idle`·
`surface.completion`·telemetry `wall_time_ms`를 만들지 않고 로그만 남긴다. Claude Code 2.1.283은
사용자가 띄운 서브에이전트 외에 `agent_type`이 빈 내부 에이전트의 종료도 이 훅으로 보내며,
메인 턴 도중에도 보낸다(격리 인스턴스 실측). 이 훅을 idle로 보면 spawn·tell 대기 노드가
자식 작업 도중 끝난다. 이미 설치된 설정과 호환되도록 등록은 유지한다.

서브에이전트의 `stop-failure`는 메인 턴의 상태·알림·meta를 바꾸지 않고 로그만 남긴다.
Payload에 `agent_id`가 있는지로 구분한다. 메인 턴은 서브에이전트 실패를 도구 결과로 받고
계속 진행할 수 있기 때문이다. 이 필드 구분은 Claude Code 2.1.280의 payload 조립부를
확인한 근거이며 모든 버전의 외부 동작을 보장한다는 뜻은 아니다.

install이 심는 훅은 이 10개 항목뿐이다 — 위 matcher 밖의 `PreToolUse`/`PostToolUse` 호출이나 `PreCompact` 등 다른 Claude Code 이벤트는 걸지 않는다. `install.rs`의 `install_preserves_other_hooks` 테스트가 사용자가 직접 추가한(matcher가 다른) `PreToolUse` entry를 tasty의 `AskUserQuestion`-matcher entry와 분리해 그대로 보존함을 검증한다.

install은 marker substring(`tasty claude hook <token>`)으로 자기 entry를 식별해 멱등하게 동작한다 — marker가 일치하는 기존 entry는 명령 문자열만 최신 형태로 덮어쓰고(옛 버전이 심은 잘못된 명령이 남는 회귀 방지), 사용자가 직접 추가한 다른 entry는 건드리지 않는다. `PreToolUse`/`PostToolUse`처럼 matcher가 있는 이벤트는 marker 일치만으로는 matcher 값까지 보증되지 않으므로, install이 matcher도 canonical 값(`AskUserQuestion`, `Bash|Agent|Task`)으로 함께 갱신한다.

이 플러그인이 fire하는 surface hook 이벤트는 `claude-idle`/`needs-input`/`claude-stop-failure`/`claude-error`/`claude-error-stalled`/`claude-quiet-released` 6개이며, 매니페스트 `contributes.hook_events`로 선언한다 — host가 (내장 ∪ 활성 plugin 선언) 집합으로 `hook.set` 등록을 검증하므로([hooks](https://github.com/zilhak/tasty/blob/develop/docs/features/hooks/index.md)), 이 플러그인이 비활성이면 저 6개 키로의 hook 등록도 거부된다.
**이 6개가 전부 위 설치 훅에서 나오는 건 아니다** — `claude-idle`은 위 `apply_hook`(Stop/StopFailure/SessionEnd)에서, `claude-stop-failure`는 `StopFailure`에서, `needs-input`은 `Notification`(`needs_input` 유형)과 `PreToolUse`(matcher `AskUserQuestion`) 두 경로에서 나오지만, `claude-error`/`claude-error-stalled`는 이 훅 메커니즘과 무관한 별도 producer다: `error_scan.rs`가 surface 출력 텍스트를 패턴 매칭해 매치 시 직접 `surface.fire_hook`으로 보낸다(정지 판정은 아래 절). `claude-quiet-released`도 별도 producer로, 무출력 알림 선언이 끝나 묶음이 생기면 선언 종료 처리(`error_scan/quiet.rs`)가 직접 보낸다(아래 "무출력 알림 선언" 절).
API 에러로 턴이 **끝나면** `StopFailure`가 구조적 신호를 주지만, 요청이 응답 없이 매달리면 턴이 끝나지 않아 `Stop`도 `StopFailure`도 **발생하지 않는다** — 그때는 PTY에 찍히는 에러 문자열과 출력 정적이 얻을 수 있는 유일한 신호다.
`claude-idle`/`needs-input`은 [surface-highlight](https://github.com/zilhak/tasty/blob/develop/docs/features/surface-highlight/index.md)(Stop hook → highlight)와 [telemetry](https://github.com/zilhak/tasty/blob/develop/docs/features/telemetry/index.md)(`session-start`→`stop`의 `wall_time_ms`, 압축 뒤의 `session-start`는 시작으로 치지 않음, `notification`의 `input_tokens`)가 소비하고, `SessionStart`/`SessionEnd`의 meta set/unset은 [layout-persistence](https://github.com/zilhak/tasty/blob/develop/docs/features/layout-persistence/index.md)의 `restore.command` 복원이 소비한다.

### API 에러 뒤 자동 재개 (`auto_resume.rs`)

`StopFailure`로 끝난 턴에 일정 시간 뒤 재개 문구를 보낸다. 기본은 꺼짐이다.
선택 이유는 [에이전트 상태와 완료 전달](https://github.com/zilhak/tasty/blob/develop/docs/features/child-terminal/index.md)을 따른다.

| 설정 키 | 기본값 | 범위와 의미 |
|---|---|---|
| `auto_resume_enabled` | `false` | 자동 재개 사용 여부 |
| `auto_resume_delay_secs` | 10 | 1~86400초. UI와 코드 모두 범위를 제한 |
| `auto_resume_max_attempts` | 5 | 연속 재개 횟수, 최소 1 |

`overloaded`와 `server_error`만 예약한다. rate limit·인증·과금·잘못된 요청·출력 한도
오류는 자동 재개하지 않는다. `agent_id`가 있는 서브에이전트 실패도 예약하지 않는다.
이 분기의 외부 payload 근거는 Claude Code 2.1.280 조립부의 정적 확인이며, 서브에이전트
API 오류에서 실제 이벤트를 받은 실험까지 완료한 것은 아니다.

같은 서피스의 새 예약은 이전 예약을 덮는다. 새 턴, 성공 Stop, 세션 종료는 예약을 지운다.
전용 스레드가 500ms마다 만기를 확인하므로 훅 핸들러는 지연 시간 동안 기다리지 않는다.
시계 범위를 넘는 예약은 만들지 않는다.

보내기 직전에 다음을 다시 확인한다.

1. 설정이 켜져 있고 `terminal.state`가 idle이다.
2. 전경 이름이 Claude이고, 예약 때 PID를 얻었다면 지금도 같은 PID다.
   이름은 대소문자를 무시하고 `.exe` 접미사를 제거한다.
3. 실패한 턴 시작 이후 사용자 입력이 없었다. 턴 시작을 모르면 예약 시각부터 확인한다.
4. 연속 시도 수가 상한보다 작고, 예약 후 새 턴으로 바뀌지 않았다.

입력 확인은 `surface.is_typing`의 `idle_seconds`를 사용한다. 키보드·IME·붙여넣기
입력을 포함하고 에이전트의 send/tell은 포함하지 않는다. 초안을 썼다가 지운 경우도
취소한다. 조회에 실패해도 보내지 않는다. 이 조건을 통과했지만 지금 `typing`이면
5초 미루며 시도 수를 쓰지 않는다.

문구는 현재 언어의 `claude.auto_resume.message`로 고정한다. `terminal.tell`로 본문 쓰기를
확인한 뒤 잠시 기다리고 Enter를 보낸다. attach 점유 등으로 거절되면 다시 예약하지 않는다.
본문과 Enter 사이에 사용자가 입력할 가능성까지 없애는 원자적 제출은 아니다.

상한에 닿으면 보내지 않고 `notification.create`로 한 번 알린다.
성공 턴과 새 세션은 계수를 초기화하지만 자동 재개 자신의 `prompt-submit`은 초기화하지 않는다.
전송 수는 `claude-auto-resume-count` meta와 로그에 남기며 성공·새 세션·세션 종료 때 meta를 지운다.
부모에게는 실패 턴의 완료 로그가 이미 전달되므로 별도 자동 재개 로그를 더 쓰지 않는다.
그 완료 문구의 '입력 대기' 표현은 재개가 예약됐어도 같으며 실제 성공을 뜻하지 않는다.

### PTY 에러 스캔 (`claude-error`) 범위

`error_scan.rs`는 추적 중인 surface의 `surface.read_since_scan_mark` 결과를 읽고 알려진 오류 패턴을 찾는다. 패턴은 `API Error`, `Output blocked by content filtering policy`, `overloaded_error`, `rate_limit_error`, `Internal Server Error`, `network error`, `Bad Request`다. 매칭되면 `claude-error`를 발생시킨다. 같은 오류 텍스트의 반복 알림은 생략하며 새 턴 신호(`prompt-submit`/`session-start`/`active`, 압축 뒤의 `session-start`는 제외)에서 이 기록을 초기화한다.

회차 사이에는 800ms를 기다린다. 파일·IPC·잠금 대기 시간은 별도이므로 800ms 안에 감지한다는 보장은 아니다.

`claude launch`의 top-level surface와 `claude spawn`/`claude respawn`의 자식 surface를 모두 추적한다.
등록할 때 등록 경로를 surface meta `claude-scan-target`(`top-level`·`child`)에 남긴다.

추적 대상은 플러그인 메모리에만 있어 플러그인이 다시 시작되면(업데이트·disable/enable) 사라진다. 그래서 시작할 때 `surface.list`의 모든 터미널을 훑어 다시 채운다.

- `claude-scan-target`이 있으면 그 경로로 등록한다.
- 없더라도 `claude-session-id` meta와 부모·자식 관계가 있으면 자식으로 등록한다. 등록 경로를 남기기 전 버전이 띄운 자식도 업데이트 뒤 다시 추적하려는 것이다. 부모가 없는 Claude는 사용자가 직접 실행한 것일 수 있어 등록하지 않는다.
  그래서 등록 경로를 남기기 전 버전에서 `launch`한 top-level은 그 버전에서 이 버전으로 올린 직후 한 번만 다시 추적되지 않는다. 다시 `launch`하면 기록이 남는다.
- 아래 추적 유지 판단을 통과하지 못하면(예: release된 자식) 등록하지 않는다.
- `claude-background-wait` meta가 있으면 대기 기록도 되살린다. 그사이 대기 Stop이 먼저 기록됐으면 그 기록을 둔다.
- `claude-stall-notice` meta가 있으면 정지 알림을 보낸 상태와 알림 간격을 되살린다(아래).

되살린 surface의 화면 정지 시간은 플러그인 시작부터 다시 센다. 출력 파일 판정은 파일의 수정 시각을 쓰므로 재시작 전의 조용한 시간도 센다.
정지 알림을 보내기 직전에 surface meta `claude-stall-notice`에 그 시각(`at_ms`, Unix ms)을 남긴다. 대기가 아닌 정지면 문구 종류(`kind`: `stalled`·`stalled_no_error`)와 화면의 첫 오류 줄(`hint`, 160자까지)도 남긴다. 발화 요청이 실패하면 지운다. 출력이 바뀌거나 새 턴으로 그 조용한 구간이 끝나면 지운다.
재시작 뒤에는 누적 창이 빈 상태에서 시작하므로, 이 meta가 있으면 빈 창을 알림을 보낸 구간으로 등록하고 알림 간격은 남긴 시각부터 잇는다.
첫 스캔에 새 출력이 없으면(전용 커서는 호스트에 있어 재시작 사이의 출력도 읽힌다) 같은 구간이므로 다시 알리지 않는다. 새 출력이 있으면 구간이 끝난 것으로 보고 평소처럼 다시 센다.

추적을 계속할지는 등록 경로에 따라 아래와 같이 판단한다.

| 대상 | 등록 | 추적 유지 판단 |
|------|------|-----------|
| top-level (`launch`) | child registry에 없음 | `surface.locate`를 조회한다. 대상 부재를 포함해 조회 오류가 나도 추적을 유지한다 |
| 자식 (`spawn`/`respawn`) | 호스트 child registry | `terminal.parent`로 부모·자식 관계를 확인한다. 관계 없음은 추적을 해제하고 그 밖의 조회 오류는 유지한다 |

`terminal.release`는 surface를 남기고 부모·자식 관계와 soft 점유만 해제하므로 자식은 surface 존재만으로 판단하지 않는다. `claude kill`은 호스트의 `terminal.kill` 응답에 `killed`를 더해 돌려준다. 응답에 `killed_surface_id`가 있을 때만 `killed: true`로 답하고 해당 추적을 즉시 해제한다. 원격으로 넘긴 kill(`forwarded`)은 `killed: false`로 답하며, surface가 남아 있으므로 추적을 유지한다. 일반 조회 오류로 추적을 지우면 다시 등록할 기회가 없어 오류 시에는 유지하는 쪽을 택한다.

`surface.read_since_scan_mark`는 에이전트의 `tasty set mark`·`read since-mark`·`parse-since-mark`와 별도의 커서를 사용한다. 읽을 때마다 해당 커서만 전진하며, 다음 호출에는 그 이후 출력이 온다.

패턴 비교를 위해 플러그인이 읽은 출력을 크기 제한 안에서 누적한다. 이 커서는 스캐너 하나만 소비한다는 전제다. 다른 소비자가 같은 API를 호출하면 한쪽이 읽은 출력을 다른 쪽이 놓칠 수 있어 CLI로 제공하지 않는다. [터미널 입출력 설계](https://github.com/zilhak/tasty/blob/develop/docs/surfaces/terminal/index.md)를 참고한다.

### 정지 알림 (`claude-error-stalled` → 부모 completion-log)

`claude-error`는 부모 알림에 직접 연결하지 않는다. 자동 재시도 중에도 오류 문구가 나올 수 있기 때문이다. 일정 기간 누적 출력이 바뀌지 않으면 정지를 의심해 `claude-error-stalled`를 발생시키며, 부모 알림은 이 이벤트를 구독한다. 재시도와 실제 멈춤을 확정적으로 구분하는 검사는 아니다.

이벤트 이름에 `error`가 있지만 오류 문구가 없는 장시간 정적 상태와 호스트가 `stale`로 분류한 자식도 대상이다. 기존 등록과 호환되도록 키를 유지한다. 대기가 아닌 정지는 스캐너가 발화 직전 화면에서 오류 문구를 찾아 종류를 정한다. 찾으면 오류 뒤 정지(error)로 보고 그 줄을 힌트로 붙이고, 없으면 출력과 완료 신호가 없다는 안내(warning)를 보낸다([에이전트 상태 설계](https://github.com/zilhak/tasty/blob/develop/docs/features/child-terminal/index.md)).

판정 기준은 두 조건의 **동시** 충족이다(`error_scan.rs`):

| 조건 | 왜 |
|---|---|
| 누적 출력의 해시가 기준 시간 동안 같음 | 이번 턴의 마지막 출력 4 KiB에 오류 문구가 있으면 30초, 없으면 설정 `no_output_notice_secs`(기본 600초). 비교 대상은 스캐너가 보관한 크기 제한 안의 누적 텍스트이며 화면 전체나 프로세스 진행 상태가 아니다. 호스트의 `CHILD_OUTPUT_SILENCE`(120초)와 별개의 값이다. 그래서 `tasty claude children`이 이 알림보다 먼저 `stale`을 보여 줄 수 있다 |
| `terminal.state`가 `active` 또는 `stale` | `idle`·`needs_input`·`exited`는 별도 상태 훅이 다루므로 제외한다. 그 훅이나 알림이 실제 전달됐음을 확인하는 조건은 아니다. `stale`은 `confidence`와 무관하게 포함한다 |

오류 없는 기준은 플러그인 설정 `no_output_notice_secs`(`plugin_settings."com.tasty.claude".no_output_notice_secs`)로 정한다. 설정 › Plugins › Claude Code의 "Seconds without child output before notifying the parent"(`claude.settings.no_output_notice_secs`) 항목이다. 기본 600초, 범위 60~86400초다. 설정 파일을 직접 고친 값도 코드에서 이 범위로 조정하고, 숫자가 아니면 기본값을 쓴다. 끄는 값은 없다. 설정 변경 이벤트가 없어 스캐너가 30초 간격으로 다시 읽으므로 재시작 없이 반영된다. 조회에 실패하면 마지막 값을 유지한다. 오류 직후 30초와 재알림 간격 5분은 설정 대상이 아니다.

30초 기준은 조용해지기 직전에 오류를 본 경우만 쓴다. 이번 턴의 마지막 출력 4 KiB로 판단하므로, 앞 턴의 오류나 그 뒤로 4 KiB를 넘게 이어진 출력은 세지 않는다. `claude-error` 이벤트와 중복 생략은 지금처럼 누적 창 전체를 본다.

상태는 출력 조건을 넘긴 뒤에만 조회한다. 조회한 상태가 `idle`·`needs_input`·`exited`이면 그 조용한 구간에서는 다시 조회하지 않는다. 대기 meta 복원 조회도 함께 멈춘다. 이 세 상태는 hook이 보고한 값이라 출력이 바뀌거나 그 surface에 hook이 와야 바뀐다. 누적 출력이 바뀌거나 그 surface의 hook(종류 무관)이 플러그인에 오면 다음 회차에 다시 조회한다. 그래서 상태가 `active`로 돌아온 뒤 알리는 시점은 매 회차 조회할 때와 같다. 새 턴 신호(`prompt-submit`/`session-start`/`active`, 압축 뒤의 `session-start`는 제외)는 `active`를 호스트에 보고하기 **전에** 정지 관측을 초기화한다. 순서가 반대면 그 사이의 스캔 회차가 이전 턴의 조용한 구간과 새 상태 `active`를 함께 보고 재개 직후 거짓 정지 알림을 낼 수 있다. 상태 조회에 실패하면(빈 값) 확인하지 않은 것으로 보고 다음 회차에 다시 묻는다. hook을 거치지 않고 `terminal.set_state`로 상태를 바꾸면 다음 출력 변화나 hook까지 반영되지 않는다. 추적 유지 판단(`terminal.parent`·`surface.locate`)은 이 규칙과 무관하게 매 회차 한다.

**Monitor 기록.** Monitor는 이벤트가 있을 때만 출력하므로 조용한 것이 정상이다. 대기 Stop에서 Monitor는 정지 알림의 감시 대상·종류·이름에서 뺀다. 기다리는 작업이 모두 Monitor면 화면 기준과 출력 파일 기준 모두 알리지 않는다. Monitor와 다른 작업이 함께 있으면 다른 작업만으로 판정한다.

판별 근거는 Claude Code 2.1.319 실제 세션의 훅 입력이다.

| 후보 | 관측 | 판별 |
|---|---|---|
| `Stop`의 `background_tasks[].type` | Monitor 도구 작업도 `shell`이며 필드 모양이 백그라운드 Bash와 같다 | 불가 |
| 작업 id 접두 | 둘 다 `b` | 불가 |
| `.output` 파일 | 출력 없는 Monitor는 파일이 없고 첫 이벤트 뒤 생긴다. Bash는 시작부터 있다 | 불가. 파일 없음은 경로 규칙 변경과 구별되지 않는다 |
| `PostToolUse`의 `tool_name` | `Monitor`이고 `tool_response.taskId`가 `Stop` 항목의 `id`와 같다. 서브에이전트가 띄운 경우도 같다 | 가능 |

- 기록: `background-start`가 `taskId`를 surface meta `claude-monitor-tasks`(JSON 배열)에 더한다. 플러그인이 다시 시작돼도 남는다. 대기 meta와 달리 새 턴이 지우지 않는다. Monitor 중간 이벤트도 새 턴을 열기 때문이다.
- 지움: `<status>`가 있는 `<task-notification>`이 그 id를 지운다. 중간 이벤트에는 `<status>`가 없다. 새 세션의 `SessionStart`와 `SessionEnd`가 전체를 지우고 압축 뒤의 `compact`는 지우지 않는다. `Stop` 목록에 없다는 이유로는 지우지 않는다.
- `type`이 `monitor`인 항목은 기록 없이도 Monitor로 본다. 2.1.319 코드의 정적 확인이며(`monitor_mcp`·`monitor_ws`) 실제 payload는 측정하지 않았다.
- 기록이 없는 항목은 셸로 보고 지금처럼 알린다. 출력이 없는 것이 오류일 수 있어서다.
- 턴이 진행 중이면 Monitor가 떠 있어도 대기가 아니므로 일반 기준으로 알린다.
- 상태 판정은 바꾸지 않는다. 일을 마치고 Monitor만 남긴 자식은 `active`로 남고, Monitor가 끝날 때까지 부모에게 완료 신호도 정지 알림도 가지 않는다.
- `StopFailure`의 상태와 `notify-stop-failure` 문구는 Monitor도 그대로 센다(`types`). 정지 알림만 `notice_types`를 쓴다.
- 미측정: `persistent: true` Monitor, 시간 초과로 끝날 때의 prompt, `--resume` 뒤 유지, 2.1.319 외 버전, 실행 중인 서브에이전트의 Monitor가 `Stop` 목록에 나오는지.

**백그라운드 작업을 기다리는 자식**(대기 Stop 뒤, 위 "백그라운드 작업을 기다리는 Stop")은 기준을 따로 둔다.
먼저 기다리는 작업(Monitor 제외)의 출력 파일을 찾아 그 파일의 활동으로 판정한다.

- 단서: 대기 Stop의 `background_tasks`(끝나지 않은 항목의 `id`와 이름 `description`·`command`), stdin의 `session_id`·`cwd`.
  백그라운드 작업이 남은 `StopFailure`는 플러그인이 기록한 작업 id를 쓰며 이름도 id다.
- 경로: Claude Code는 작업 출력을 `<임시 폴더>/claude-<uid>/<cwd slug>/<session_id>/tasks/<id>.output`에 쓴다. 임시 폴더는
  `CLAUDE_CODE_TMPDIR`, 없으면 OS 임시 폴더다. slug는 cwd의 영숫자가 아닌 문자를 `-`로 바꾼 값이다(200자를 넘으면 해시가 붙는다).
  서브에이전트의 `.output`은 `~/.claude/projects/<slug>/<session_id>/subagents/agent-<id>.jsonl`을 가리키는 심볼릭 링크다.
  공식 문서에 없는 내부 규칙이며 Claude Code 2.1.291의 코드와 Linux의 실제 폴더로 확인했다.
- 찾기: 플러그인 프로세스의 `CLAUDE_CODE_TMPDIR`·임시 폴더 아래 `claude-*` 폴더에서 slug 경로를 먼저 보고, 없으면 프로젝트 폴더를 훑어
  `<session_id>/tasks`를 찾는다. 못 찾으면 10초마다 다시 찾는다. 서브에이전트 링크는 대상 transcript가 생기기 전에 만들어지고, transcript 저장이 꺼진 세션(예: `CLAUDE_CODE_CHILD_SESSION`을 물려받은 자식)에서는 끝까지 대상이 없다. 대상이 없는 링크는 활동 판정에서 빼고, 대상이 생기면 그 시각을 활동으로 센다.
- 판정: 파일 크기나 수정 시각이 바뀌면 활동이다. 찾은 모든 파일이 일반 기준(`no_output_notice_secs`) 동안 활동이 없으면 대기 한 번에 한 번 알린다.
  화면 출력은 보지 않는다. 알리기 전에 surface meta `claude-background-quiet`에 대기 시작 시각·작업 이름·조용한 시간·작업 id(`tasks`)·기록 시각(`at_ms`)을 남기고,
  `notify-error`는 대기 시작 시각이 같을 때만 이 기록으로 `[warning] surface <N>: background work <이름> has written no output for <분> min` 줄을 쓴다.
  이름은 80자까지 적는다. 이 meta는 지우지 않고 다음 기록이 덮어쓴다.
- 되돌림: 대상이 있는 출력 파일이 하나도 없으면(경로 규칙이 바뀌었거나, 자식의 임시 폴더 설정이 플러그인과 다르거나, 대상이 없는 링크뿐임)
  아래 화면 기준을 쓴다. 이 상태가 120초 이어지면 플러그인 로그에 대기당 한 번 warn을 남긴다. 셸 파일과 대상이 없는 링크가 함께 있으면
  셸 파일만으로 판정하고 그 작업 이름만 적는다. 셸 하위 프로세스의 CPU 시간은 보지 않는다.

출력 파일을 찾지 못한 대기와 대기 meta에서 되살린 기록은 Claude Code가 Stop payload로 알려 준 대기이므로 누적 출력이 **10분과 `no_output_notice_secs` 중 큰 값** 동안 같아야 알리고,
오류 문구 유무와 관계없이 이 기준을 쓴다.
대기 한 번에 한 번만 알린다. 출력이 바뀌거나 쿨다운이 지나도 같은 대기에서는 다시 알리지 않는다.
대기에 Monitor가 있으면, Monitor 이벤트의 새 턴으로 대기가 다시 시작돼도 출력 파일 기준으로 마지막에 알린 작업과 같고 그 알림 뒤로 파일 활동이 없으면 같은 조용한 구간으로 보고 다시 알리지 않는다. Monitor가 없는 대기는 새 대기마다 다시 알려 턴을 깨운다(아래 "무출력 알림 선언"으로 보류하거나 버릴 수 있다). `claude-background-quiet`의 `tasks`·`at_ms`로 판단하므로 플러그인을 다시 시작해도 이어진다.
대기 기록은 플러그인 메모리에 두며 대기를 끝내는 이벤트(대기가 아닌 `Stop`·백그라운드 작업이 남지 않은 `StopFailure`·새 턴·`SessionEnd`)와 추적 해제가 지운다.
플러그인이 다시 시작되면 메모리 기록은 사라지고 `claude-background-wait` meta는 남는다. 시작할 때 위 "추적" 절차가 이 meta로 기록을 되살린다.
그 뒤에 meta가 생긴 surface도 일반 기준에 닿으면 알리기 전에 이 meta를 읽어 기록을 되살리고 대기 기준으로 다시 판정한다. 알림 문구도 이 meta를 읽으므로 기준과 문구가 같은 상태를 가리킨다.
되살린 기록은 meta의 알림 여부와 출력 파일 단서를 이어받는다. 그래서 재시작 전에 알린 대기는 다시 알리지 않고, 출력 파일 감시도 이어 간다.
이때 부모 로그 문구는 "looks stuck" 대신 대기 사실만 적는다.
영어 줄은 `[warning] surface <N>: waiting on background work (<종류>) for <분> min, with no output meanwhile`이다.
종류는 `claude-background-wait`의 `notice_types`(없는 옛 meta는 `types`)이며, 비어 있으면 판정에 쓴 `waiting_on_background_work`를 적는다.
분은 `since_ms`부터 지난 시간을 버림한 값이다. 끝나지 않는 백그라운드 명령을 남긴 자식은 idle이 되지 않으므로, 이 알림이 부모가 받는 신호다. Monitor만 남긴 자식에는 이 신호도 없다.

같은 정적 구간에서는 한 번만 알리고 surface별로 최소 5분 간격을 둔다. 출력이 바뀌면 정적 구간을 다시 측정한다. 새 턴 신호도 중복 기록과 정적 측정을 초기화하지만 5분 쿨다운은 유지한다. 긴 추론이나 입력 대기와 실제 멈춤은 구분하지 못할 수 있다.

`StopFailure`가 상태를 idle로 바꾸면 이 정지 알림의 조건에서 빠진다. 대신 `claude-idle` 훅으로 실행된 `notify-done`이 API 오류 종류를 붙여 로그를 쓰는 경로를 사용한다. 백그라운드 작업이 남아 `active`로 남는 StopFailure는 `notify-stop-failure`가 한 줄을 쓴다. 훅 호출과 로그 쓰기가 실패하지 않았다는 보장까지 뜻하지는 않는다.

정지 스캐너는 `terminal.set_state`를 호출하지 않는다. 관측 결과로 알림만 만들며 `claude children`의 상태를 직접 바꾸지 않는다.

`register_notify_hooks`는 상태 변경용 once 훅 세 개와 별도로 `claude-error-stalled` 상시 훅을 등록한다. 명령은 `tasty claude notify-error --caller-surface … --target-surface …`이며, `notify-done` 형제 훅과 명령 문자열이 달라 그 정리 대상에 포함되지 않는다. 같은 방식으로 `claude-stop-failure` 상시 훅(`tasty claude notify-stop-failure --caller-surface … --target-surface …`)과 `claude-quiet-released` 상시 훅(`tasty claude notify-quiet-release --caller-surface … --target-surface …`)도 등록한다. 이 훅들은 spawn·tell과 완료 줄 뒤의 재등록 때 등록되므로, 이 버전 전에 등록된 훅만 있는 자식은 다음 재등록 전까지 묶음 줄을 받지 못한다. error 줄에 붙는 묶음은 이 훅과 무관하다.

같은 caller·target의 정지 알림을 다시 등록할 때는 같은 명령 문자열로 등록된 기존 훅을 정리한다.

`notify-error`는 대상의 `claude-background-wait` meta가 있으면 위 대기 문구를 쓴다. 없으면 스캐너가 `claude-stall-notice`에 남긴 종류와 오류 줄로 안내를 조립한다. 종류가 없는 기록(이 버전 전의 플러그인이 남긴 것)이면 `surface.screen_text`에서 오류 문구를 찾는다. 어느 쪽이든 조립한 문구를 `<parent_home>/notify/<caller_surface>.log`에 직접 추가한다. 다른 완료·상태 알림과 같은 [로그 경로](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/external-interaction.md#child-완료-알림--completion-log)를 사용하며 화면 조회나 기록은 실패할 수 있다.

#### 알림 타입 표지

부모 완료 로그의 child 알림 줄은 모두 번역하지 않는 ASCII 표지 `[info] `·`[warning] `·`[error] ` 중 하나로 시작한다. 기계가 읽는 계약이라 언어와 무관하다. 표지는 `tasty-utils::notify::append_notice`가 붙이며 타입 없이 줄을 쓰는 함수는 없다. 종류와 타입의 대응은 `notice.rs`의 `NoticeKind::notice_type`이 와일드카드 없는 `match`로 정하므로, 새 종류는 타입을 정하기 전까지 컴파일되지 않는다. 시험 `every_notify_message_key_belongs_to_a_notice_kind`는 `[claude.notify]`의 `_message` 키가 모두 어떤 종류의 문구인지 확인한다.

| 타입 | 종류 |
|---|---|
| `info` | 상태 변경(`idle`·`needs_input`), 원격 결과 저장 |
| `warning` | 무출력 3종(턴 진행 중 오류 없는 정지, 대기 중 화면 무출력, 작업 출력 파일 무출력), 끝난 선언의 묶음 |
| `error` | API 오류로 끝난 턴의 완료 줄, 백그라운드 작업이 남은 API 오류(`notify-stop-failure`), 오류 뒤 정지, child `exited` |

세 완료 훅(`claude-idle`·`needs-input`·`process-exit`)은 같은 명령을 쓰므로 `notify-done`은 어느 사건인지 모른다. 그래서 대상 상태로 가른다. `exited`면 error(부모가 kill로 끝낸 경우도 구분하지 않는다), `idle`에 API 오류 힌트가 붙으면 error, 그 밖은 info다. 상태 조회에 실패하면 info로 쓴다.

#### 무출력 알림 선언 (`claude.quiet`)

에이전트가 child의 무출력이 예상된다고 선언하면 그동안의 무출력 알림(warning)을 보류하거나 버린다. 전역 설정 `no_output_notice_secs`의 값·범위·설정 화면은 바꾸지 않는다.

- 명령: `tasty claude quiet --surface <N> [--task <id>] (--secs <초> | --clear)`, IPC `claude.quiet`. `--surface`를 생략하면 호출자 surface다. `--task`는 백그라운드 작업 id(Bash `backgroundTaskId`, Agent `agentId`)이며 생략하면 surface 전체가 대상이다.
- `--secs`는 초 단위 정수다. 양수는 보류, `-1`은 버림이다. 상한은 없다. 밀리초로 바꿀 때 넘치는 값은 감기지 않고 가장 먼 끝 시각에 머문다. `0`과 `-1`이 아닌 음수, `--secs`와 `--clear`를 함께 주거나 둘 다 빠진 요청, 스캐너가 추적하지 않는 surface(attach mirror의 원격 자식 포함)는 `invalid params`(-32602)로 거절하고 아무것도 기록하지 않는다. CLI는 `Error (-32602): …`를 출력하고 종료 코드 1이다.
- 응답: 선언은 `{surface, task, secs, mode: "hold"|"discard", until_ms}`(`-1`이면 `until_ms`가 null), `--clear`는 `{surface, task, cleared, released}`다. `secs`는 요청 값이고 `until_ms`는 재선언 규칙을 적용한 실제 끝 시각이다.
- 같은 대상을 다시 선언하면 양수→양수는 쌓인 것과 선언 시각을 두고, 새 끝 시각이 더 늦을 때만 끝 시각을 늦춘다(짧은 값으로 앞당기지 않는다). 양수→`-1`은 앞 선언을 끝내 쌓인 것을 묶어 보낸다(끝난 이유 `replaced`). `-1`→양수는 새로 쌓는다.

보류·버림 판정은 정지 알림이 나가기로 정해진 순간(시간·중복·쿨다운·상태 조건과 Monitor 중복 판정을 통과한 뒤)에 한다. 그래서 쌓이는 수는 선언이 없을 때 나갔을 알림 수와 같다.

- 작업 출력 파일 기준: 조용하다고 판정된 작업마다 그 작업의 선언, 없으면 surface 전체 선언을 따른다. 어느 선언에도 걸리지 않은 작업의 이름만 적어 지금처럼 바로 보낸다. 조용한 시간은 파일별이 아니라 찾은 파일 전체의 합산 값이다.
- 화면 기준 대기(출력 파일을 못 찾음): surface 전체 선언이 있거나, 기다리는 작업 모두에 작업 선언이 있을 때만 보류·버림한다. 아니면 지금처럼 보낸다. 알림 하나는 한 선언에만 쌓는다. surface 전체 선언이 있으면 그 선언, 없으면 걸린 작업 선언 중 끝 시각이 가장 늦은 양수 선언이고, 모두 `-1`이면 버린다.
- 대기가 아닌 턴의 오류 없는 정지: surface 전체 선언만 맡는다. 작업 선언만 있으면 지금처럼 보낸다.
- 보류나 버림을 해도 알린 것과 같은 기록(조용한 구간·대기의 알림 여부, `claude-stall-notice`, 쿨다운 시각)을 남긴다. 같은 대기는 선언이 끝난 뒤에도 다시 알리지 않고, 새 대기는 지금 규칙대로 판정한다.
- 양수 선언은 쌓고 `-1` 선언은 플러그인 로그에 info로 남기고 버린다. 같은 대상(작업 id, 대기의 작업 집합, 턴)의 반복은 한 항목으로 합쳐 횟수·첫·마지막 시각·가장 긴 조용한 시간을 남긴다. 대기의 작업 집합은 Stop이 알려 준 순서와 관계없이 id를 정렬해 비교한다. 선언 하나는 서로 다른 항목을 64개까지 적고, 그 뒤의 새 대상 알림은 버리지 않고 수와 첫·마지막 시각만 센다. 항목 이름은 묶음 줄과 같은 길이(80자)로 잘라 저장한다. meta는 밖에서 바뀔 수 있으므로 선언과 묶음을 meta에서 되살릴 때도 이름을 같은 길이로 자르고, 64개를 넘는 항목은 수와 첫·마지막 시각만 센다.

선언은 다음 경우에 끝난다. 양수 선언에 쌓인 것이 있을 때만 묶음을 만들고, `-1`이거나 쌓인 것이 없으면 아무것도 보내지 않는다.

| 끝나는 경우 | 대상 | 묶음 |
|---|---|---|
| 끝 시각이 지남(`expired`) | 양수 선언 | 폴링이 추적 중인 surface마다 정지 판정 전에 확인한다 |
| 작업이 끝남(`task ended`) | 작업 선언 | `<task-notification>` prompt의 `<task-id>`, 읽을 수 있는 Stop 목록에 없는 작업, `SessionStart`(압축 뒤 제외)·`SessionEnd`의 모든 작업 |
| `--clear`(`cleared`) | `--task`면 그 선언, 없으면 그 surface의 모든 선언 | |
| 재선언(`replaced`) | 양수→`-1` | |
| error 알림 | 그 surface의 모든 선언(`-1` 포함) | error 줄 뒤에 붙인다 |
| 완료 줄 없이 추적 끝남(release·kill·surface 닫힘) | 그 surface의 모든 선언 | 60초 안에 오는 error 완료 줄에만 붙고, 아니면 버린다 |

surface 전체 선언은 작업 종료나 대기가 아닌 Stop으로 끝나지 않는다.

- 묶음 줄(warning): 묶음을 surface meta `claude-quiet-release`에 남기고 `claude-quiet-released`를 발화한다. 부모 훅의 `notify-quiet-release`가 아직 받지 않은 묶음을 한 줄씩 쓰고 받은 부모를 기록한다. 부모가 여럿이면 각자 한 번씩 받는다. 묶음이 생길 때 그 surface에 `claude-quiet-released` 훅을 건 부모를 받을 부모로 기록한다(spawn·tell이 완료·error 훅과 함께 건다). 받을 부모가 모두 가져간 묶음은 최근 16개까지만 둔다. 완료 줄 없이 사라진 surface의 묶음은 개수와 관계없이 60초 보관으로만 지운다. 한 부모라도 아직 가져가지 않은 묶음은 개수와 관계없이 보관 시간(1시간, 사라진 surface의 묶음은 60초)이 지나야 지우고, 그때 플러그인 로그에 info로 남긴다. 받을 부모를 알 수 없으면(훅 조회 실패, 훅 없음) 한 부모가 가져가면 다 가져간 것으로 본다. error로 끝난 선언과 완료 줄 없이 사라진 surface의 묶음은 이 처리가 가져가지 않고 error 줄만 가져간다. 영어 문구는 `surface <N>: <수> held no-output notice(s) released — the <초>s quiet declaration on <task <id>|the whole surface> ended (<이유>): <항목>`이다.
- error 줄: `notify-done`(error로 분류된 완료 줄)·`notify-stop-failure`·오류 뒤 정지의 `notify-error`가 쓰기 직전에 그 surface의 선언을 모두 끝내고, 이 부모가 받지 않은 묶음(error로 끝난 것 포함)을 같은 줄 뒤에 `; held <수> no-output notice(s) of the <초>s quiet declaration on <대상>: <항목>`으로 붙인다. 오류 뒤 정지는 스캐너가 발화 직전에 선언을 끝낸다.
- 항목: `<이름> ×<횟수> (<task quiet|wait quiet|turn quiet>, +<첫 분>–<마지막 분> min, quiet up to <분> min)`. 시각은 선언 시작부터 지난 분이다. 이름은 저장할 때와 meta에서 되살릴 때 줄바꿈을 공백으로 바꾸고 80자에서 자르며, 줄로 쓸 때 한 번 더 같은 처리를 한다. 항목은 5개까지 적고, 적지 않은 항목과 항목 수 상한을 넘은 알림은 합친 알림 수로 `+<수> more`라 적는다. 줄 앞의 수에도 상한을 넘은 알림이 들어간다. 묶음 문구에는 `has written no output`·`with no output meanwhile`을 쓰지 않는다.
- 끝난 이유와 무출력 종류 표기는 번역하지 않는다. 문장은 플러그인 언어를 따른다.

선언과 쌓인 항목은 surface meta `claude-quiet`(`{"decls":[{task, mode, secs, set_at_ms, until_ms, held, overflow}]}`)에 남는다. 끝 시각은 재시작 뒤에도 이어지도록 Unix ms(벽시계)로 재므로, 시스템 시계를 바꾸면 선언 기간도 그만큼 바뀐다. 플러그인만 다시 시작하면 그 surface를 처음 볼 때 meta에서 되살리며, 꺼진 동안 끝 시각이 지났으면 첫 폴링에서 묶음을 낸다. Tasty를 다시 시작하면 복원된 surface가 새 ID를 쓰고 이전 meta를 넘기지 않으므로 선언과 쌓인 항목은 사라지고 보내지 않는다. `claude.quiet`는 `remote_attach.methods`에 없어 attach mirror의 원격 자식에는 쓸 수 없다.

## 인터페이스

- **AI Agent / 사용자**: `tasty claude launch|spawn|tell|broadcast|kill|respawn|children|parent|codex-returns|remote-results|hook|checklist-hook|checklist-enable|checklist-disable|checklist-status|profile-register|profile-unregister|profile-list|profile-show|profile-current|gate-register|gate-unregister|gate-list|gate-show …`.
- surface/페인 생성 자체는 [work-area](https://github.com/zilhak/tasty/blob/develop/docs/features/work-area/index.md) 도메인을 사용.

## 비-목표

- Claude Code 자체 기능 — 외부 CLI. 이 플러그인은 *실행·배치·관계 관리*.
- 터미널/PTY 내부 — host.

## Acceptance Criteria

- Given 플러그인 활성 When `tasty claude launch` Then 새 워크스페이스에서 Claude 가 실행된다.
- Given 부모 인스턴스 When `tasty claude spawn` Then 자식 인스턴스가 페인 분할로 생성되고 `children` 에 보인다.
- Given 상태 훅과 로그 기록이 정상 동작하는 자식 When `tasty claude spawn` 또는 `tell` 뒤 idle/needs_input/process-exit 훅이 발생 Then caller의 completion-log에 상태 변경을 기록하고 같은 명령의 형제 훅을 정리한다. `surface.locate`가 성공하면 세 훅을 다시 등록한다.
- Given 같은 부모가 같은 자식에 `tasty claude tell`을 세 번 보낸 상태 When 자식의 상태가 한 번 바뀜 Then 부모의 completion-log에 완료 줄이 한 줄 남는다. 다른 부모가 등록한 그룹은 지우지 않는다.
- Given `~/.claude/settings.json`에 사용자가 직접 추가한 hook entry가 있음 When `tasty claude install` 실행 Then 10개 tasty hook entry가 추가/갱신되고 사용자 entry는 그대로 보존된다.
- Given Codex 대화의 `_tasty_context`가 실린 `claude spawn` When 자식이 게이트에 두 번 block된 뒤 통과한 Stop으로 턴을 끝냄 Then `codex.deliver`는 마지막 Stop의 답으로 한 번만 불린다. 백그라운드 작업을 기다리는 Stop은 보내지 않는다.
- Given Codex 플러그인이 없거나 꺼진 상태 When Codex 대화에서 `claude spawn` Then 자식을 만들지 않고 `codex.parent` 오류를 돌려준다. 다른 부모로 대신하지 않는다.
- Given `codex.deliver`가 실패한 결과 When `tasty claude codex-returns` Then 그 결과가 본문·수신처와 함께 보이고, `--retry` 뒤 송신 스레드가 넘기는 데 성공하면 다음 조회의 목록에서 사라진다.
- Given 게이트 판정을 기다리는 Stop When 판정 전에 새 프롬프트가 오거나 시간 초과 확정의 보고가 새 턴 뒤로 밀림 Then 그 Stop의 답은 그 턴의 키로 한 번, 새 턴의 답은 새 턴의 키로 한 번 `codex.deliver`된다.
- Given Stop 으로 `idle`이 된 Claude 자식 When Claude Code가 쉬는 중에 대화를 압축해 SessionStart(`source: compact`)가 옴 Then 상태는 `idle`로 남고 `claude-idle`·화면 알림·정지 알림이 새로 생기지 않는다. 턴 도중의 압축이면 `active`가 그대로 남고, 백그라운드 작업 대기·게이트 판정을 기다리는 Stop 도 그대로 이어진다.
- Given `/clear`로 새 세션에 넘어간 Codex 경로 When 넘어간 세션의 늦은 SessionStart(`resume`)와 그 세션의 늦은 Stop·SessionEnd가 옴 Then 경로는 새 세션에 남고, 새 세션의 답만 새 세션의 턴 키로 간다.
- Given SessionEnd가 오지 않은 채 `/clear`로 시작한 세션 When 프롬프트 없이 넘어간 세션으로 `/resume`한 뒤 그 세션이 프롬프트를 받음 Then 그 resume 세션의 답이 Codex 대화로 간다.
- Given 판정을 기다리는 Stop When 다른 Codex 부모가 같은 자식에게 tell Then 그 Stop의 답은 원래 부모의 수신처로 간다.
- Given Codex 경로가 있는 자식 When 일반 셸에서 `tasty claude tell` Then 지시를 보내기 전에 경로가 지워지고 이후 결과는 Codex 대화로 가지 않는다.
- Given 유효한 프로필 JSON When `tasty claude reboot --profile-file <경로>` Then 재시작된 Claude 에서 프로필 훅과 tasty 내장 훅이 함께 실행되고, 무인자로 다시 reboot 해도 프로필이 승계된다. `--clear-profile` 후 reboot 하면 프로필 훅이 더 이상 발생하지 않는다. 존재하지 않는 경로/깨진 JSON 은 kill 시퀀스를 시작하지 않고 즉시 에러를 반환한다.
- Given 등록된 프로필 둘(각각 다른 마커를 남기는 `SessionStart` 훅) When 이름 둘을 쉼표로 `--profile` 에 함께 부착해 spawn Then **둘 다** 발생한다(머지가 last-wins 로 떨어지지 않는다). `permissions.deny`를 담은 프로필을 부착하면 그 자식에게서 해당 도구가 사라지고(거부 프롬프트가 아니라 툴셋에서 빠짐), `deny` 프로필과 그 도구를 `allow` 하는 프로필을 함께 부착해도 도구는 여전히 없다(deny를 allow보다 우선한다). `--profile-file` 과 `--profile` 을 함께 주면 즉시 에러.
- Given `--profile continue-checklist` 로 부착한 세션 + 마커 파일 존재 When Claude 가 센티넬 없이 응답을 끝내려 함 Then block 되고 체크리스트 본문이 주입되며, 센티넬을 포함해 응답하거나 라운드 상한에 도달하면 정상 종료된다. 프로필을 부착하지 않은 세션은 이 동작에 전혀 영향받지 않는다. 같은 프로필을 부착한 세션 둘을 동시에 진행해도 라운드 카운터가 서로 섞이지 않는다.
- Given 마커 파일 부재 When `tasty claude checklist-enable` Then `continue-checklist` 게이트의 마커 파일이 생성되고 `checklist-status` 가 `enabled: true` 를 보고한다. When `tasty claude checklist-disable` Then 마커 파일이 삭제되고 `checklist-status` 가 `enabled: false` 를 보고한다 — 마커가 이미 없는 상태에서 다시 `checklist-disable` 을 호출해도 에러 없이 `enabled: false` 를 반환한다(멱등).
- Given 등록 게이트 둘(`gate-a`/`gate-b`) When `checklist-enable --gate gate-a` Then `checklist-status --gate gate-a` 는 `enabled: true`, `--gate gate-b` 는 `enabled: false` 이고 `gate-list` 가 두 상태를 함께 보여준다. 그 세션에서 `gate-a` 훅만 block 을 걸고 `gate-b` 훅은 조용히 통과한다. 미등록 이름으로 `checklist-enable --gate nope` 를 호출하면 에러이고 마커 디렉토리도 생기지 않으며, `checklist-status --gate nope` 는 에러 없이 `enabled: false, registered: false` 를 돌려준다.
- Given 켜 두고 라운드가 쌓인 게이트 When `gate-unregister <이름>` 후 같은 이름으로 다시 `gate-register` Then `checklist-status` 가 `enabled: false` 이고 라운드 카운터도 1 부터 시작한다(이전 인스턴스의 상태를 물려받지 않는다).
- Given 게이트별로 나누기 전 경로(`checklist/enabled.marker`)에만 마커가 있는 인스턴스 When 아무 진입점(`checklist-status` 또는 훅 실행) Then 마커가 `gates/continue-checklist/enabled.marker` 로 옮겨지고 legacy 파일은 사라지며, 켜져 있던 상태가 그대로 보존된다.
- Given capability 를 지원하는 서버의 mirror workspace 와 물리 부모 P When `tasty claude spawn --workspace <mirror id>` Then 자식은 서버에서 실행되고 응답의 `child_surface_id` 는 이 Tasty 의 mirror surface, `remote` 는 true 이며, 서버의 Stop 훅이 보고한 idle 이 P 의 completion-log 에 한 줄 기록된다.
- Given Codex 대화가 원격 자식을 spawn 한 상태 When 서버에서 그 턴이 확정됨 Then 서버 plugin 은 `attach.child_notice` 로 결과를 맡기고 `delivered` 를 받기 전에는 영속 요청을 지우지 않으며, client plugin 은 그 결과를 `r.` 접두 키로 그 대화의 수신처에 한 번 기록한다. 같은 결과가 다시 와도 `duplicate` 로 답한다.
- Given Claude 세션 surface 가 원격 자식을 spawn·tell 한 상태 When 서버에서 그 턴이 확정됨 Then client plugin 은 본문을 `attach-results/` 에 먼저 저장하고 그 부모 완료 로그에 결과 ID 와 `tasty claude remote-results --id <ID>` 를 남긴 뒤 `recorded` 로 답한다. 같은 결과가 다시 오면 알리지 않고 `duplicate` 로 답하며, 그 명령은 본문을 돌려준다.
- Given 한 부모가 만든 원격 자식 When 다른 부모(다른 surface, 같은 surface 의 다른 Claude 세션, 다른 Codex 대화, 호출 surface 없음)가 그 자식에게 `tell` 함 Then client plugin 은 결과 수신·idle 여부와 관계없이 경로·토큰을 바꾸거나 원격에 보내지 않고 `child_of_other_parent` 로 거절하며, 만든 부모의 후속 `tell` 은 보낸다.
- Given 소유자 기록 파일이 없거나 깨진 원격 자식 When 어느 부모든 그 자식에게 `tell` 함 Then client plugin 은 소유자를 새로 맡기지 않고 원격에 보내지 않은 채 `child_without_owner` 로 거절한다.
- Given 원격 자식 기능을 모르는 서버의 mirror When `tasty claude spawn --workspace <mirror id>` Then 아무 터미널도 만들지 않고 오류를 돌려준다.
- Given 서버 결과를 알 수 없게 끝난 원격 spawn When 그 call 의 자식이 늦게 생김 Then 기다리던 부모가 있으면 그 부모로 경로를 묶고, 없으면 그 자식을 닫는다.
- Given `gate-register mygate --sentinel '[[MY-DONE]]' --rounds 2` 로 등록하고 `checklist-enable --gate mygate` 로 켜 둔 게이트 When `spawn --profile mygate` Then 그 자식의 `Stop` 훅으로 `checklist-hook --gate mygate` 가 걸리고, 센티넬 없이 턴을 끝내려 하면 **mygate 의 본문**(host 기본 체크리스트 본문이 아니라)이 주입되며 block 된다. `[[MY-DONE]]` 을 포함해 답하면 통과하고, 포함하지 않은 채 계속하면 **mygate 의 상한 2** 에서 백스톱으로 통과한다. 라운드는 `checklist/gates/mygate/rounds/<session>.json` 에 쌓이고 그 세션이 끝나면 정리된다.
- Given 등록 게이트 `mygate` 와 그것을 `reboot --profile mygate` 로 부착해 둔 자식(이름 meta 를 기록하는 기동 경로는 `reboot` 뿐이라 `spawn` 으로 띄운 자식은 훅이 걸려 있어도 `attached_names` 가 `null` 이다) When `profile-list` / `profile-show mygate` / `profile-current --surface <자식>` Then 목록에 `user/mygate`(attachable, 게이트 설명)와 `host/continue-checklist` 가 함께 보이고, show 는 owner `user` 와 생성된 Stop 훅 JSON 을 돌려주며, current 의 `attached_names` 에 `mygate` 가 있다.
- Given 게이트 둘 When `--profile mygate,continue-checklist` 로 부착 Then `generated/<정렬된이름>.json` 의 `hooks.Stop` 항목이 2개이고 각각 `--gate mygate` / `--gate continue-checklist` 를 지목한다.
