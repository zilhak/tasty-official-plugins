# Codex (`com.tasty.codex`)

- **Status**: Implemented (bundled plugin)
- **주체**: AI Agent / 로컬 사용자 (`tasty codex` CLI · IPC)
- **배포/통합**: bundled · cli + ipc_namespace + 멀티에이전트 — [plugins 개념](https://github.com/zilhak/tasty/blob/develop/docs/concepts/plugins.md)
- **코드**: `crates/tasty-plugin-codex/`
- **권한**: `terminal.spawn` 등 (매니페스트 `permissions`)
- **화면**: 없음 — CLI/IPC 실행 관리 플러그인 (headless).

> CLI·IPC로 자식 프로세스를 실행하고 관리하는 예제다. [플러그인 개발](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#cli--ipc-namespace)을 참고한다.

## 부모의 완료 수신 채널

상태 변경은 caller surface의 `<parent_home>/notify/<caller_surface>.log`에 한 줄로 기록한다. 자식 CLI 종류로 경로를 바꾸지 않는다. 훅 설치와 부모의 수신 준비는 별개이며 idle·needs_input·interrupt·exit는 작업 성공을 뜻하지 않는다. [완료 알림 로그](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/external-interaction.md#child-완료-알림--completion-log)를 따른다.

Codex 대화에서 spawn한 자식의 턴 결과는 완료 로그와 별도로 [부모 대화에 돌아간다](#codex-부모에게-결과-반환).

### 원격 attach 자식

attach 로 연결한 원격 워크스페이스(mirror)에 `spawn` 하면 자식은 서버 Tasty 에서 서버의 Codex CLI·plugin 설정·승인 정책으로 실행된다. 부모·Codex 결과 경로·완료 알림 훅은 이 Tasty 의 mirror surface 에 묶이고, `tell`·`kill` 은 서버의 같은 메서드로 간다. 서버에서 턴을 보고하는 것은 서버에 `tasty codex install` 로 설치한 기존 훅이다. 부모가 Codex 대화면 서버가 확정한 턴 결과가 그 대화의 대기열에 들어간다. 부모가 물리 surface(Claude 세션·셸)면 결과 본문을 plugin 데이터 폴더 `attach-results/` 에 저장하고 부모 완료 로그에 결과 ID 와 `tasty codex remote-results --id <ID>` 를 남긴다. 원격 자식의 결과 소유자는 만든 부모로 고정되며, 다른 부모의 `tell` 은 원격에 보내기 전에 `child_of_other_parent` 로 거절한다. 세부 계약은 [원격 attach 자식](https://github.com/zilhak/tasty/blob/develop/docs/plugins/remote-attach-children.md)에 있다.

## Tasty 자식에서 지우는 세션 변수

매니페스트 `[contributes.strip_inherited_env]`로 `CODEX_THREAD_ID`를 선언한다. Codex는 도구 명령마다 그 대화의 thread ID를 이 이름으로 넣는다. Codex 도구가 Tasty를 띄우면 이 값이 남아, 그 터미널의 셸이나 그 안의 Claude가 부른 `tasty`를 바깥 Codex 대화의 호출로 해석한다. 이 플러그인이 설치·활성이면 Tasty는 터미널 셸·훅·작업 자식에서 이 값을 지운다([선언 형식과 적용 조건](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#상속-환경변수-제거-contributesstrip_inherited_env)). 그 터미널에서 실행한 Codex는 도구마다 자기 thread ID를 다시 넣으므로 Codex 호출자 판정은 그대로다. `CODEX_HOME` 등 다른 `CODEX_*` 값은 사용자 설정이라 남긴다. 플러그인을 비활성화하거나 제거하면 지우지 않는다.

## 목적

**Codex CLI를 tasty 안에서 실행하고 관리**하는 통합. [claude](../claude/index.md) 플러그인과 동형이며, 주로 작성한 코드/판단을 Codex 에게 교차 검증시키는 용도.

## 내부 동작

짝 핸들러의 서로 다른 공개 응답과 번역 형식은 기존 호출자 호환을 위해 유지한다.
`children`/`kill`의 응답 형식과 상태 훅의 공통점·차이점은
[짝 핸들러 호환 경계](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/paired-agent-handlers.md)를 따른다. 완료 알림은
[completion-log](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/external-interaction.md#child-완료-알림--completion-log)에 기록하며 caller PTY에 메시지를 입력하지 않는다.

- **cli `codex`** (`tasty codex …`) — 서브커맨드: `launch` · `spawn`(자식, 페인 분할) · `children`/`parent` · `tell`(메시지 전송, 줄바꿈 보존·자동 제출) · `notify-caller`(내부용, 아래) · `broadcast` · `kill`/`respawn` · `reboot`(같은 세션 resume 재시작, 아래) · `hook`(stop/prompt-submit/session-start/permission-request/post-tool-use/interrupt/session-end) · `result`(부모에게 돌아온 자식 결과 읽기) · `remote-results`(원격 attach 자식이 물리 surface 부모에게 돌려준 결과 읽기) · `session-open`(내부용, 세션 helper의 세대 발급) · `session-bind`(내부용, TUI의 대화 선택 등록). `install`/`uninstall`(Tasty 훅을 Codex CLI 설정에 설치).
- **IPC 전용** — `codex.parent`(Codex 호출자 thread의 수신 준비), `codex.deliver`(자식 결과 기록), `codex.delivery_outcome`·`codex.delivery_wake`·`codex.turn_outcome`·`codex.turn_wake`(내부용). CLI `parent`는 자식 surface의 부모를 조회하는 `codex.parent_of`다.
- **ipc_namespace `codex`** — 위 동작에 대응하는 IPC API.

### 자식별 모델과 추론 수준

`spawn --model <식별자> --effort <수준>`은 그 자식 세션에만 적용한다. IPC 필드는 선택 문자열
`model`·`effort`다. 생략하면 기존 Codex 설정을 사용하며 전역·부모·형제 설정은 바꾸지 않는다.
모델은 비어 있지 않은 식별자(영숫자와 `- _ . / :`, 첫 글자 `-` 제외)로 받으며 목록을 고정하지
않는다. effort는 `none|minimal|low|medium|high|xhigh|max|ultra`로 제한한다. 모델별 사용 가능 여부는
Codex/provider가 판단한다. 거절은 오류로 보이고 다른 모델로 자동 대체하지 않는다. 잘못된 타입·빈값·
옵션 형태의 값은 `terminal.spawn` 전에 거절한다.

모델은 helper의 `-m`, effort는 제한된 `-c model_reasoning_effort=<수준>` 인자로 만든다.
model 또는 effort가 있는 helper는 새 세션에 세션 전용 relay를 사용한다. relay는 thread 요청에서
`config.model_reasoning_effort`가 생략되거나 null이면 호출 시 지정한 effort를 채운다.
명시적인 요청 effort와 다른 config 키는 보존하며, 최종 요청의 effort를 기준으로 응답의
`reasoningEffort`를 검증한다.
승인·sandbox는 기존 정책 우선순위를 따른다. provider 오류 응답은 그대로 전달한다.
등록부의 helper 인자와 복원 명령에 명시값을 보존하며 같은 세션의 탭 복원·reboot에도 적용한다.
재개 helper는 TUI를 띄우기 전에 `thread/resume`으로 모델·effort를 서버에 적용하고 검증한다.
그 연결의 기존 구독 유지 경로를 사용하므로 재개 TUI의 기본값이 명시값을 덮어쓰지 않는다.

### 훅 설치와 실행

install은 Windows에 PowerShell, 다른 OS에 POSIX sh 명령을 설치한다.
두 명령 모두 세션 helper가 서버 환경에 넣은 `TASTY_CODEX_GEN`과 터미널의 `TASTY_SURFACE_ID`가
있을 때만 실행하며 `--surface`와 `--gen`을 함께 보낸다. 대상 인스턴스는 터미널의
`TASTY_PARENT_HOME`을 그 호출의 `TASTY_HOME`으로 넘겨 정한다.
PowerShell은 `if ($env:TASTY_CODEX_GEN -and $env:TASTY_SURFACE_ID) { … }`에서 `$input`을 전달하고,
POSIX는 `if [ -n "$TASTY_CODEX_GEN" ] && [ -n "$TASTY_SURFACE_ID" ]; then … || true; fi`를 쓴다.
플러그인은 `--gen`이 그 surface의 현재 세대일 때만 훅을 적용한다. 세대가 없거나(셸 통합을 거치지 않고 직접 실행한
`codex`, 공유 daemon) 지난 세대이거나, 세대의 thread와 다른 thread의 훅은 `ignored`로 답하고
상태·메타데이터·턴 보고를 바꾸지 않는다.
실패 진단은 [CLI 훅 실패 기록](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/cli-structure.md#에이전트-훅-전달-실패-기록)을 따른다.

업데이트로 설치 명령이 바뀌면 `tasty codex install`을 다시 실행한다.
`tasty codex hook` marker로 Tasty 항목만 교체하며 같은 matcher의 사용자 핸들러와 설정은 보존한다.
훅 명령이 바뀌면 Codex는 그 훅을 다시 검토 대상으로 본다. Tasty는 훅 검토를 우회하지 않으며
검토하지 않은 훅은 실행되지 않는다. 이 경우 상태 보고와 부모 결과 반환이 동작하지 않는다.
설치된 trust 메타데이터만으로 실제 훅 실행을 보장하지 않는다.

훅 stdin의 session_id는 `codex-session-id`와 `restore.command` meta에 기록하며 reboot와
세션 복원이 사용한다. stdin의 `model`(SessionStart)은 복원 명령에, `turn_id`(UserPromptSubmit·Stop·Interrupt)는
부모 결과 반환의 완료 키에 쓴다. 설치 이벤트는 아래 일곱 개다. 승인이 어느 도구에서나 발생할 수 있어
도구 이름 matcher를 두지 않으며 PostToolUse도 매 호출마다 프로세스를 실행한다.

### Codex 실행 파일과 PATH

Codex 대화에서 CLI를 호출하면 호출자의 PATH에서 찾은 Codex 절대경로와 PATH를 내부 문맥의
`execution`으로 전달한다. 상대 PATH 항목은 호출 cwd 기준으로 절대화한다. 플러그인의 PATH에
Codex가 없어도 공식 proxy를 실행할 수 있고, npm의 `/usr/bin/env node` 런처도 전달된 PATH를 쓴다.
PATH는 해당 자식 프로세스에만 설정한다. 호스트 전역 환경, 인증 파일, 부모 ID 설정은 바꾸지 않는다.
Claude의 Codex 부모 해석도 이 문맥을 `codex.parent`에 그대로 전달한다.

외부 부모의 수신 기록에는 실행 정보가 함께 저장되어 백그라운드 전송·재시도와 plugin 재시작 때도
쓰인다. 같은 부모에서 실행 정보가 갱신되면 기존 수신자 ID와 완료 claim을 유지한 채 기록만 바꾼다.
실행 정보가 없는 이전 CLI는 저장된 값을 지우지 않는다. 이전 수신 기록에 실행 정보가 없으면 기존처럼
plugin 환경에서 Codex를 찾으며, 다음 정상 CLI 호출로 실행 정보를 저장한다. 지정된 실행 파일이 사라졌으면
다른 설치본으로 바꾸지 않고 전송 전 실패로 남긴다. 로드 여부 확인과 수신 준비가 실패하면 child는 생성하지 않는다.

세션 helper는 자기 터미널의 환경에서 실행 정보를 한 번 확보하고 서버와 TUI에 같은 정보를 쓴다.
실제 Linux proxy 회귀 검사는 `TASTY_CODEX_EXECUTION_PROBE=1`로 실행하는 opt-in 시험이다.
Codex가 없는 PATH의 별도 검사 프로세스에서 전달된 실행 정보로 기존 대화의 로드 여부만 조회한다.
모델·child·새 대화를 실행하지 않는다.

### 세션 실행 (전용 app-server)

launch·spawn·respawn·reboot·복원은 모두 플러그인 실행 파일의 `session` 모드(세션 helper)를
터미널에 실행한다. 명령은 `'<플러그인 폴더>/tasty-plugin-codex' session [--resume <thread>]
[--prompt-file <파일>] [-- <정책·모델 인자>]` 형태이며 각 인자를 작은따옴표로 감싼다. 셸의
`codex` alias·function은 이 명령에 영향을 주지 않는다. 프롬프트는 임시 파일로 넘기고 helper가
읽은 뒤 지운다.

helper 문구는 터미널 셸이 물려받은 `TASTY_LOCALE`(설정 `general.language`) 언어로 나온다. `LANG`은
보지 않고, 값이 없거나 비면 영어다. 번역 층과 사용자 파일은 [i18n 가이드](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/i18n.md#plugin-네임스페이스)와 같다.

helper는 그 터미널의 cwd와 환경에서 다음 순서로 실행한다.

1. `tasty codex session-open`으로 surface의 새 세대를 발급받는다. 같은 surface의 이전 세대는
   helper가 시작 중이거나 준비된 상태가 아니면 정리한다.
2. `codex app-server --listen <주소>`를 자식 프로세스로 띄운다. 서버 환경에는 `TASTY_CODEX_GEN`이
   들어가며 훅은 이 서버의 자식으로 실행된다. 출력은 세대 폴더의 `server.log`에 쓴다.
3. 서버 준비를 최대 30초 기다린다. 준비되지 않거나 서버가 먼저 끝나면 서버를 정리하고 실패로
   끝낸다. embedded 서버나 사용자의 공유 daemon으로 내려가지 않는다.
4. 재개(`--resume`)이면 그 서버에 연결해 `thread/resume {threadId, approvalPolicy?, sandbox?, model?,
   excludeTurns}`로 thread를 다시 열고, 응답의 승인 정책·sandbox·모델이 요청과 같은지 확인한다.
   실패하거나 다르면 서버를 정리하고 `failed`(사유 포함)를 기록한 뒤 TUI를 띄우지 않고 끝낸다.
   이 연결은 TUI가 끝날 때까지 열어 두고 서버가 보내는 메시지를 계속 읽어 버린다(아래 "재개 정책").
5. 상태를 `ready`로 기록한다. 재개였다면 서버가 확인한 thread를 `resumed`로 함께 기록한다.
   `ready`는 서버 준비와 재개 확인까지의 뜻이며 TUI가 계속 실행된다는 보장은 아니다.
6. `codex [resume] --remote <주소> -c check_for_update_on_startup=false <정책 인자> [thread] [프롬프트]`로
   TUI를 붙인다. 재개 TUI에는 정책·모델 인자를 넘기지 않는다. `--dangerously-bypass-hook-trust`는
   전달하지 않는다.
7. TUI가 끝나면 `exited`를 기록하고 서버를 종료한 뒤 TUI의 종료 코드로 끝난다. 서버가 끝나면
   4의 연결도 닫힌다.

**재개 정책.** Codex 0.154 TUI는 `--remote`로 기존 thread를 재개할 때 승인·sandbox 재정의
(`-a`·`-s`·`--dangerously-bypass-approvals-and-sandbox`)가 있으면 "Permission overrides are not
supported when resuming a remote task."로 시작을 멈춘다. 재정의 없이 재개하면 서버는 그 thread의
마지막 턴에 저장된 정책을 복원한다. 그래서 helper가 소유 서버의 `thread/resume`에 요청한 정책을 넣는다.
TUI는 재개 때 정책 없이 `thread/resume`을 보낸다. 서버는 이미 로드된 thread에 다른 인자로 들어온
재개를 구독자가 없고 idle이면 cold 재시작으로 처리해 helper가 적용한 정책을 버린다. 구독자가 있으면
로드된 thread에 붙는다. helper 연결이 TUI가 끝날 때까지 구독을 유지하는 이유다.
구독을 유지하는 동안의 동작은 다음과 같다.

- 서버의 승인 요청 등은 구독한 모든 연결로 가고 한 응답으로 해결되므로 helper는 응답하지 않는다.
- 이 연결이 구독하는 동안 그 thread는 idle unload되지 않는다. 같은 TUI에서 `/new`로 옮긴 뒤에도 원래
  thread는 TUI가 끝날 때까지 로드된 채 남는다.
- 전송 오류나 해석할 수 없는 메시지로 읽기를 멈추면 연결을 닫는다. 그 뒤로는 구독하지 않는다. 메시지 본문 크기만으로 구독을 끊지는 않는다.

탭 복원과 reboot는 이 helper로 기존 thread를 재개하므로 같은 방식으로 정책을 적용한다. launch·spawn·respawn은 새 세션을 띄운다.

이 경로의 실서버 검증은 opt-in 시험(`TASTY_CODEX_NORMAL_HOME_E2E=1`, Linux)으로 한다.

- 사용자의 기존 Codex 로그인을 쓰고 모델은 호출하지 않는다.
- thread는 read-only로 시작하고 재개 때 다른 승인·sandbox를 요청한다. app-server는 cwd를 명시한
  `thread/start`가 그 cwd에 쓸 수 있으면 임시 cwd를 `trusted`로 config에 영속하므로, read-only로
  시작해 이 기록을 피한다.
- 본문이나 정리가 실패해도 끝에서 `config.toml`이 시험 전과 같은지(sha256, 임시 cwd 키 부재) 판정한다.
- 시험이 만든 thread만 지우고 소유한 서버만 끝낸다.

npm으로 설치한 `codex`는 실제 서버를 다시 띄우는 래퍼이고 종료 신호를 한 번만 전달한다. 실제
서버는 첫 SIGTERM을 받은 뒤 진행 중인 작업을 기다리느라 몇 초 넘게 남을 수 있다. 그래서 helper는
서버를 정리할 때 래퍼만 끝내지 않는다. Linux·macOS는 서버를 별도 프로세스 그룹으로 띄우고 그룹에
SIGTERM을 보낸다. 5초 안에 끝나지 않거나 그룹에 프로세스가 남으면 SIGKILL로 그룹 전체를 끝낸다.
Windows는 서버를 Job Object에 넣고 job을 종료한다. 별도 그룹이므로 터미널의 Ctrl+C는 서버에 직접
가지 않는다. Linux에서는 helper가 강제 종료(SIGKILL)돼도 서버 래퍼가 종료 신호를 받는다.

Linux·macOS의 helper는 SIGINT·SIGHUP·SIGTERM에 바로 끝나지 않고 받은 신호를 단계별로 처리한다.
TUI가 끝나 터미널이 일반 모드로 돌아온 뒤 남은 Ctrl+C(reboot는 4회 보낸다)나 터미널 닫기가 서버 정리를
끊지 않게 하기 위해서다.

| 받은 시점 | 처리 |
|---|---|
| 서버 준비 전 | 서버를 정리하고 TUI 없이 종료 코드 130으로 끝낸다 |
| TUI 실행 중 | Ctrl+C는 같은 전경 그룹의 TUI가 직접 받으므로 helper는 무시한다. hangup·SIGTERM은 TUI에 같은 신호를 한 번 전달하고, 5초 안에 TUI가 끝나지 않으면 강제로 끝낸다 |
| TUI 종료 뒤 | 무시하고 서버 그룹 정리를 마친다 |

처리기를 둔 신호는 exec 때 기본 처리로 돌아가므로 서버와 TUI의 신호 처리는 바뀌지 않는다. helper가
실행될 때 이미 무시하도록 받은 신호는 그대로 무시한다. Windows는 helper가 끝나면 서버 job이 닫혀 서버가
함께 끝난다.

| OS | 주소 | 접근 제한 |
|---|---|---|
| Linux·macOS | `unix://<세대 폴더>/s.sock`. 경로가 길면 `$XDG_RUNTIME_DIR` 또는 임시 폴더 아래 0700 폴더 | 소켓을 담은 0700 폴더 |
| Windows | `ws://127.0.0.1:0`. 서버가 출력한 실제 포트를 읽는다 | `--ws-auth capability-token`과 세대별 token 파일. TUI는 `--remote-auth-token-env`로 받는다 |

플러그인은 helper가 직접 만든 주소만 사용하며 다른 주소나 token을 추측하지 않는다.
Windows 경로는 Codex 0.154.0의 도움말과 Linux에서 측정한 loopback token 인증(token 없음·틀림은 401,
`Authorization: Bearer`는 101)에 근거한다. Windows 실 기기와 TUI의 token 환경변수 전달은 측정하지 않았다.

세대 상태는 플러그인 데이터 폴더의 `return/sessions/<세대>/`에 둔다. SessionStart 훅이 세대의
thread를 정하고 `return/threads/<thread>.json`으로 그 surface의 현재 세대를 가리킨다. SessionEnd는
그 경로와 `codex-session-id` meta를 지우고 세대의 thread를 비운다.

Codex 0.154에서 두 훅의 시점은 다음과 같다(측정: app-server 로그와 훅 실행 기록).

- `/new`는 새 thread를 만들지만 SessionStart는 그 대화의 첫 턴 직전(`turn/start` 직후)에 실행된다.
  첫 턴 전에는 rollout이 없어 재개할 대상도 없다.
- SessionEnd는 thread가 내려갈 때 실행된다. TUI가 끝날 때와, 대화를 바꾼 뒤 구독자가 없는 이전
  thread를 서버가 unload할 때(측정에서 구독 해제 60초 뒤) 모두 온다. 입력의 `reason`은 늘
  `other`라 둘을 구분하지 못한다. 그래서 SessionEnd는 세대 종료로 보지 않는다([턴 결과
  확정](#턴-결과-확정)의 종료 기록 조건).
- 세대의 thread가 비어 있으면 SessionStart는 그 thread를 바로 묶는다. SessionStart가 아닌 훅은
  아래 표처럼 서버가 그 thread를 `active`로 보일 때만 묶는다. 비운 세대에 늦게 온 이전 thread의
  Stop은 thread가 없으므로 결과를 만들지 않는다.
- SessionStart가 오기 전(첫 턴 전) 또는 unload 뒤 첫 턴 전에는 `codex-session-id`가 없으므로
  `reboot`은 재개할 세션이 없다고 거절한다.

같은 TUI에서 대화를 바꾸면(`/new`, `/resume`, 이미 연 대화로 돌아가기) 다른 thread의 훅이 온다.
같은 서버에 이미 로드된 대화로 돌아가면 SessionStart 없이 UserPromptSubmit부터 온다. 훅은 따로
실행되는 프로세스라 이전 대화의 훅이 늦게 도착할 수도 있다. 그래서 세대의 thread와 다른 thread의
훅이 오면 세대의 전용 서버에 `thread/read`로 묻는다. Codex는 훅을 동기로 실행하므로 훅이 실행되는
동안 그 thread는 `active`다.

| 서버 응답 | 처리 |
|---|---|
| 들어온 thread가 `active`이고 지금 thread는 아니다(세대의 thread가 비었으면 들어온 thread가 `active`다) | 세대의 thread를 옮기고 이전 thread의 경로를 지운다. 복원 명령도 바꾼다 |
| 들어온 thread가 `active`가 아니거나 둘 다 `active`다 | `stale_session`으로 무시한다 |
| helper가 준비되지 않았거나 서버에 닿지 못했다 | `unverified_session`으로 무시한다 |

SessionStart의 `source`나 thread ID 순서로는 판정하지 않는다. 이전 thread의 SessionEnd는 세대를
끝내지 않는다. 훅 명령은 helper가 서버 환경에 넣은 세대 값을
실어 오므로, 다른 세대나 helper 밖에서 실행한 Codex의 훅은 이 경로를 바꿀 수 없다.

### 터미널 세션 복원

부팅 때 선택된 터미널의 PTY는 IPC 서버가 열리기 전에 복원될 수 있다. 세션 helper는 터미널의
`TASTY_PARENT_HOME`을 적용한 동일 CLI로 `list info`를 읽어 준비를 확인한 뒤 `session-open`을
한 번만 실행한다. 준비 조회는 최대 30초 동안 재시도하며, 응답하지 않는 조회 프로세스도 기한이나
취소 시 종료·회수한다. 세션 등록 자체는 중복 세대 발급을 피하기 위해 재시도하지 않는다.
준비 전의 일시적인 접속 실패는 화면에 반복 출력하지 않는다.

SessionStart에서 현재 surface에 세션 ID와 `restore.command`를 저장한다. 복원 명령은 helper의
`--resume <thread>` 명령이며 세대에 기록한 정책 인자와 훅이 알려 준 모델(`-m`)을 그대로 쓰고
제출 문자는 넣지 않는다. 호스트는 이 명령을 복원한 터미널의 셸에서 실행하므로 cwd는 복원된
터미널의 것을 쓰고, 새 surface ID는 그 PTY의 환경에서 받는다.

`tasty codex install`은 Tasty 데이터 홈의 셸 통합 디렉터리에 `shell-integrations/com.tasty.codex.sh`도 설치한다.
Tasty의 Bash·zsh 통합은 프롬프트 준비 때 이 디렉터리의 스크립트를 읽는다([셸 통합 확장](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#셸-통합-확장-스크립트)).
사용자 전역 rc나 기존 `codex` 셸 함수는 덮어쓰지 않는다. 이전 이름 `codex.sh`가 남아 있으면
install과 uninstall이 지워 같은 스크립트를 두 번 읽지 않게 한다.
대화형 터미널에서 입력한 `codex`·`codex resume`은 플러그인의 `terminal` 모드로 연결된다.
Codex 도구 안의 실행, 비대화형 stdin, `command codex`, 유틸리티 명령과 명시적 `--remote`는
원래 CLI로 실행한다. uninstall은 통합 파일도 제거한다.

자동 연결은 프롬프트, `resume`의 ID·이름·선택기 및 `--last`·`--all`·`--include-non-interactive`,
승인(`-a`/`--ask-for-approval`), sandbox(`-s`/`--sandbox`), 모델(`-m`/`--model`),
`--dangerously-bypass-approvals-and-sandbox`, `-C`/`--cd`, `--no-alt-screen`·`--search`를 지원한다.
그 밖의 옵션(`-c`, profile, fork 등)은 경고 후 원래 인자를 바꾸지 않은 native CLI로 실행하며,
자동 복원은 등록하지 않는다. Bash·zsh 외의 셸에서는 `tasty codex launch`를 사용한다.

native TUI는 세대별 전용 서버 앞의 loopback relay에 임의 capability token으로 연결한다.
relay 종료는 등록된 소켓 목록의 락이 poison돼도 남은 소켓을 shutdown하고 연결 스레드를 회수한다.
목록은 Weak 참조만 보관하므로 공용 poison 복구로 읽고 최초 한 번 진단한다. Drop이 panic을
풀어내는 중에도 poison 자체를 새 panic으로 바꾸지 않는다. 프레임 writer는 별개이며 poison 뒤
이어 쓰지 않고 오류를 전파한다. 인증값 슬롯도 완성된 String의 단일 대입이므로 복구해 TUI의
인증 환경변수에 전달한다. 토큰 값은 진단 로그에 쓰지 않는다.
relay는 thread/start·resume·fork 요청에 실행 때 명시한 정책·모델을 넣고, 응답이 그 값을 적용했는지 확인한다.

중계와 직접 RPC의 WebSocket 수신은 메시지 본문에 고정 바이트 상한을 두지 않는다.
카탈로그·대화 이력 등 서버의 유효한 응답을 크기만으로 거절하지 않으며, 플러그인별 허용 목록이나
본문 잘라내기를 사용하지 않는다. 프레임이 선언한 전체 길이만큼 미리 할당하지 않고 실제 수신한
바이트를 8 KiB 이하 청크로 모은다. 이 청크는 전체 메시지 상한이 아니다. 메모리 확보 실패·전송
단절·잘못된 JSON은 여전히 오류다. WebSocket 제어 프레임의 형식과 길이 필드의 예약 비트는 검사한다.

중계는 JSON 봉투의 ID·method를 읽고 params/result/error는 원문을 빌린다. 정책을 바꾸는 thread
요청과 그 응답의 확인에만 JSON 객체를 만들며, 나머지 요청·응답은 원문 그대로 전달한다.
따라서 plugin/list 전체 카탈로그를 JSON 객체로 확장하거나 재직렬화하지 않는다. 메시지 문자열과
송신 프레임은 여전히 메모리에 있으므로 총 메모리 사용량은 메시지 크기에 비례하며 고정 메모리
스트리밍이라는 보장은 아니다. 프레임 쓰기 전체를 같은 writer 잠금 안에 두어 ping 응답과 섞이지 않는다.

양방향 전송 오류는 peer를 닫기 전에 방향과 원인을 기록한다. 정상 종료·연결 해제는 debug이고,
JSON/할당 등 처리 실패는 helper의 기본 warn 수준에서 보인다. JSON 오류 로그는 위치와 종류만
기록하고 본문을 싣지 않는다. TUI→server worker의 정상 join에 담긴 오류도 확인한다.

native TUI에는 새 대화든 재개든 승인·sandbox 옵션을 넘기지 않는다. Codex 0.154 TUI는 이 옵션을 받으면
시작 때의 원격 재개뿐 아니라 TUI 안의 `/resume`도 "Permission overrides are not supported when resuming a
remote task."로 거부하기 때문이다. 모델(`-m`)은 TUI에 그대로 넘긴다.

옵션 없이 시작한 TUI는 thread 요청에 자기 설정의 기본 승인·sandbox 값을 채워 보낸다(측정: 첫
`thread/start`와 `/new`에 `on-request`와 `workspace-write` 또는 `read-only`). 값만으로는 기본값과 사용자
선택을 구분할 수 없으므로 relay는 요청 종류로 구분한다.

| 시점 | relay가 thread/start·resume·fork의 승인·sandbox를 다루는 방식 |
|---|---|
| TUI에서 권한을 고르기 전 | 실행 때 지정한 값으로 바꾼다. 지정하지 않은 항목은 TUI 값을 그대로 둔다 |
| TUI가 권한 필드(`approvalPolicy`·`approvalsReviewer`·`sandboxPolicy`·`permissions`)가 non-null인 `thread/settings/update`를 보낸 뒤(`/permissions`) | TUI 값을 그대로 두고 빠진 항목만 채운다 |
| 요청 `config`에 권한 profile·sandbox 설정이 있음 | 승인·sandbox를 넣지 않는다 |

- 이 판정은 relay(TUI 실행 하나) 단위로 유지된다. 모델만 바꾼 `thread/settings/update`는 권한 필드를
  null로 보내므로 권한 선택으로 보지 않는다.
- 모델은 요청에 값이 없을 때만 채운다.
- 응답 확인은 relay가 실제로 보낸 값을 기준으로 한다. 서버가 실행 정책 대신 다른 값을 적용하면
  TUI에는 오류로 전달하고 thread를 등록하지 않는다.
- `turn/start`는 relay가 바꾸지 않는다. TUI는 thread 응답의 정책을 자기 상태로 받아 사용자 턴의
  `turn/start`에 싣는다(측정: relay가 바꾼 `never`·`danger-full-access` 응답 뒤 TUI 헤더가 YOLO mode로
  바뀌고 이후 턴의 rollout `turn_context`도 같은 값이었다).
- `ephemeral: true`인 thread 요청은 TUI가 대화 이름 짓기 같은 보조 작업에 쓰는 thread다(측정: 첫 턴 뒤
  `read-only`·작은 모델로 시작한다). relay는 이 요청의 정책을 바꾸지 않고, 응답을 확인하거나 세대의
  thread로 등록하지 않는다. 기록이 남지 않는 thread라 복원 대상이 될 수 없기 때문이다.
- 실행 옵션이 없으면 relay는 승인·sandbox를 넣지 않으므로 권한을 넓히지 않는다.

성공 응답을 TUI에 보내기 전에 `session-bind`가 현재 세대와 소유 서버의 로드 목록을 확인하고
선택된 UUID·복원 명령을 저장한다. 첫 모델 턴을 시작하지 않은 재개도 저장된다.
복원 명령에는 검색 조건이나 최초 프롬프트를 넣지 않고 UUID, 실행 옵션과 모델을 넣는다.
TUI에서 `/permissions`로 고른 권한은 복원 명령에 기록하지 않으므로 복원한 대화는 실행 옵션으로 다시 열린다.

native 세대의 현재 thread는 relay의 선택 응답으로만 바뀐다. 같은 서버에 남아 있는 다른 대화의
훅은 상태·복원 대상·결과 반환 경로를 바꾸지 않는다. 이 등록은 native 훅 승인과 별개이며,
턴 종료·입력 대기·자식 응답 전달에는 승인된 훅이 필요하다.

설치 전부터 실행 중이던 세션은 재연결하거나 종료하지 않는다. 세션 ID·복원 명령이 없는 기존
surface를 추측으로 다른 thread에 연결하지 않는다. CWD, 포커스, 가장 최근 세션은 식별 근거가 아니다.

### 설치 이벤트

  | Codex config table 키 | tasty hook token | trust state 키 | 산출 상태 | 추가 이벤트·동작 |
  |---|---|---|---|---|
  | `Stop` | `stop` | `stop` | `idle` | `codex-idle`, Codex 부모 반환 후보(턴 상태 확인 뒤 기록) |
  | `UserPromptSubmit` | `prompt-submit` | `user_prompt_submit` | `active` | — |
  | `SessionStart` | `session-start` | `session_start` | `active` | — |
  | `SessionEnd` | `session-end` | `session_end` | 변경 없음 | `codex-session-id` meta·thread 경로 삭제, 세대의 thread 비움, 세대가 끝났을 때만 Codex 부모에 `exited` 반환 |
  | `PermissionRequest` | `permission-request` | `permission_request` | `needs_input` | `needs-input` · `surface.completion`(kind=needs_input) |
  | `PostToolUse` | `post-tool-use` | `post_tool_use` | `active` | — |
  | `Interrupt` | `interrupt` | `interrupt` | `idle` | `codex-idle`, Codex 부모 반환 후보(턴 상태 확인 뒤 기록) |

  `~/.codex/config.toml`의 `[hooks]` 섹션에 심는다(`settings.json`이 아니다 — Codex 의 hook dispatch 경로가 아니다). Codex 는 새 hook entry를 *trust*하기 전엔 fire하지 않는다. 사용자가 Codex의 `/hooks`에서 검토한다.
- **도구 실행 승인 대기**: `PermissionRequest`는 상태를 `needs_input`으로 바꾸고 부모 알림과
  공용 attention 표시를 요청한다. 승인이 필요 없는 실행에는 이 훅이 발생하지 않는다.
  `PostToolUse`는 active로, 거절·Esc·Ctrl-C의 `Interrupt`는 idle로 바꾼다.
  호스트의 idle 전이는 `terminal.state`에 남아 있던 `needs_input` 상태를 해제한다.

  승인 직후 이벤트가 없으므로 승인해도 도구가 끝날 때까지 `terminal.state`는 `needs_input`일 수 있다.
  화면의 attention 알림은 별도 상태이며 [사용자 확인·clear 규칙](https://github.com/zilhak/tasty/blob/develop/docs/features/surface-highlight/index.md)을 따른다.
  `PermissionRequest`에 tool ID가 없어 한 서피스의 연속 승인을 도구별로 대응시키지 못한다.
  이 제한은 codex-cli 0.154.0 관측에 근거하며 다른 버전에서는 실제 훅을 확인한다.
  일반 질문(`request_user_input` 등)의 입력 대기는 이 훅의 대상이 아니다.
  Codex의 작업 완료 전략은 `needs_input`을 성공 종료 상태로 사용하지 않는다.

### agent task 턴 보고

훅은 [agent task](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/agent-runner.md#agent-task)의 턴 경계를 `agent.task_turn_report`(`provider: "codex"`)로 호스트에 알린다. 이 호출에는 매니페스트의 `agent.turn_report` 권한을 쓴다(다른 agent.* 는 열지 않는 권한). `terminal.set_state` 보다 먼저 보낸다. 시작 보고(prompt-submit)에는 훅이 받은 프롬프트에 지시 끝의 회차 표지(`[tasty-task-attempt:<token>]`)가 있었는지와 그 토큰을 싣고(`prompt_seen`·`attempt_marker`), 프롬프트 본문은 보내지 않는다. 프롬프트는 UserPromptSubmit 의 stdin `prompt` 에서 읽는다. 호스트는 그 surface 에 묶인 agent task 회차가 있을 때만 적용한다. 보고 실패는 경고 로그만 남긴다. 변환은 `task_turn.rs` 에 있다.

| 훅 token | 보고 |
|---|---|
| `prompt-submit` | `turn_started` |
| `stop` | `turn_ended` + `final_answer`(stdin `last_assistant_message`, `--last-assistant-message` 로 받는다) |
| `interrupt` | `turn_ended` + `error: "interrupted"` |

### 자식 상태 알림

spawn과 tell은 필요한 호스트 호출의 응답을 받은 뒤 반환하며 자식 작업 완료까지 기다리지 않는다. Codex 대화에서 호출한 경우에는 아래 훅 대신 [결과 반환](#codex-부모에게-결과-반환)을 쓴다. 그 밖의 호출은 `codex-idle`, `needs-input`, `process-exit` once 훅을 등록한다. 같은 부모·대상의 완료 훅 그룹(명령 앞부분 `tasty codex notify-caller --caller <부모> --target <대상> --kind `)이 이미 있으면 먼저 지우고 등록하므로, tell을 반복하거나 spawn 뒤 tell해도 부모마다 그룹은 하나이고 한 번의 상태 변경에 완료 줄은 한 줄이다. 다른 부모의 그룹은 그대로 둔다. 하나가 실행되면 notify-caller가 상태 변경 로그를 쓰고 같은 command의 나머지 훅을 정리한다.

명령 문자열은 caller·target·kind로 구분한다. 이 조합이 같으면 반복 요청도 같은 훅 그룹이므로 요청마다 독립된 구독은 아니다. 이후 `surface.locate`가 성공하면 세 훅을 다시 등록한다. Surface 존재는 프로세스 생존과 다르며, 훅 등록과 로그 쓰기 실패나 재등록 사이의 이벤트 전달까지 보장하지 않는다.

완료 로그 줄은 타입 표지로 시작한다. 대상 상태가 `exited`면 `[error] `(부모가 kill로 끝낸 경우도 같다), 샌드박스 안내가 붙으면 `[error] `, 그 밖의 상태 변경과 원격 결과 저장은 `[info] `다. 종류와 타입의 대응은 `notice.rs`의 `NoticeKind`가 와일드카드 없는 `match`로 정한다([완료 알림 로그](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/external-interaction.md#child-완료-알림--completion-log)).

notify-caller는 최근 화면 800줄에서 RTM_NEWADDR를 찾으면 샌드박스 초기화 실패 가능성을
힌트로 덧붙인다. 조회 실패나 패턴 미검출은 기존 알림을 막지 않는다.
힌트는 진단용이며 Tasty가 실행 정책을 자동으로 바꾸거나 재시작한다는 뜻은 아니다.

- **훅 호출 실패 집계**: `terminal.set_state` 실패는 오류 응답으로 반환한다. 나머지 정리·알림 호출은 실패해도 계속하고 `host_call_failures`로 실패 수를 알린다. 0이 아니면 `<tasty_home>/hook-failures.log`에도 기록을 시도한다. `ok`만으로 모든 호출이 성공했다고 판단하지 않는다([오류 처리](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/error-handling.md)).
- **훅 출력 계약** — `hook`의 IPC/직접 CLI 응답은 진단용 `host_call_failures`를 포함한다. 설치된 일곱 셸 래퍼는 CLI stdout을 버리고 Codex에는 빈 JSON 객체 `{}`만 반환한다 — `PermissionRequest` 에서 그 값은 "결정 없음"이라 승인 흐름을 바꾸지 않는다. 실패 기록(`hook-failures.log`)과 stderr는 유지한다. [Codex 훅 출력 규약](https://learn.chatgpt.com/docs/hooks)에 없는 내부 필드를 전달하지 않으며, 기존 설치에는 `tasty codex install`을 다시 실행해 래퍼를 갱신한다.
- **훅 검토를 우회하지 않는다** — 기동 명령에 `--dangerously-bypass-hook-trust`를 넣지 않는다. `install`의 trust 표시는 선택한 파일의 metadata이며 실제 훅 실행이나 원격 daemon 신뢰의 증명이 아니다. 검토가 필요한 훅은 사용자가 Codex에서 확인하며 Tasty가 대신 승인하지 않는다.
- **명령 셸** — helper 명령은 POSIX 작은따옴표 인용을 쓴다. Linux·macOS의 sh/bash/zsh와 Windows의 Git Bash가 대상이다. Windows 터미널 셸은 Git Bash뿐이다.

### 승인과 샌드박스 설정

launch·spawn·respawn·reboot는 다음 값을 helper에 전달한다. 새 세션은 helper가 Codex TUI 인자로
적용하고, 기존 thread 재개는 helper가 소유 서버의 `thread/resume`으로 적용한다(위 "재개 정책").

| Tasty 옵션 | 전달 내용 |
|---|---|
| `--approval untrusted\|on-request\|never` | `-a` 승인 정책 |
| `--sandbox read-only\|workspace-write\|danger-full-access` | `-s` 샌드박스 모드 |
| `--full-auto` | `--dangerously-bypass-approvals-and-sandbox` |

full-auto와 approval 또는 sandbox를 함께 지정하면 invalid_params로 거절한다.
우선순위는 호출별 옵션 → 플러그인 설정 → 기본값이다.
설정 키는 `default_approval_policy`와 `default_sandbox_mode`다.
호출별 approval이 없으면 플러그인 설정을 사용한다. 이 설정도 없거나 inherit이면 never를 사용한다.
호출별 sandbox와 유효한 플러그인 기본값이 모두 없으면 해당 옵션을 붙이지 않아 Codex 자체 설정을 따른다.

승인을 직접 받으려면 untrusted 또는 on-request를 명시한다.
PermissionRequest 훅은 그 대기를 알려주며 대신 승인하지 않는다.
중첩 샌드박스를 지원하지 않는 환경에서는 read-only나 workspace-write가
RTM_NEWADDR 오류로 시작에 실패할 수 있다. 실행 환경과 선택한 정책을 함께 확인한다.

- **`reboot`** (`tasty codex reboot [--surface <id>] [--delay <초>] [--prompt <추가문구>] [--approval <값>] [--sandbox <값>] [--full-auto]`) — surface 안의 Codex 를 종료하고 **같은 세션으로 재시작**한다([claude reboot](../claude/index.md) 와 동형).
  요청 시 `codex-session-id`를 읽고 재시작 작업을 예약한 뒤 `reboot_in_secs`로 응답한다. 응답은 접수 결과이며 재시작의 성공은 아니다. helper의 `--resume <session_id> [-- 정책 플래그]` 명령으로 새 세대의 전용 서버에서 같은 thread를 연다. session_id는 thread UUID여야 한다. 정책 플래그는 위 승인/샌드박스 규칙과 동일하게 해석된다. 같은 surface의 중복 reboot는 거절한다.
  - **helper 세션**(surface의 현재 세대가 helper `ready`): 요청 때 전경 프로세스 pid를 읽는다. 읽을 수 없으면 Codex를 끝내기 전에 요청을 거절한다. delay(기본 5s) 후 Ctrl+C를 0.5s 간격으로 최대 4회 보내되, 보낼 때마다 그 세대의 helper 상태를 확인해 `exited`·`failed`가 되면 더 보내지 않는다. Codex는 `/new`에도 `run codex resume` 안내를 찍고 보이는 화면의 문구 수는 스크롤로 줄어들 수 있으므로 화면 문구는 종료 근거로 쓰지 않는다.
    전경 pid는 Linux·macOS에서 터미널의 전경 프로세스 그룹이다. helper는 TUI를 자기 그룹에서 실행하고 서버만 별도 그룹에 두므로, helper가 끝날 때까지 helper의 그룹이다. Windows는 셸의 가장 얕은 셸 아닌 자손(helper)이고, 자손이 없으면 셸 자신이다.
    helper는 `exited`를 기록한 뒤 서버를 정리하고 끝난다. 그래서 resume 명령은 호스트의 `surface.foreground_process`가 전경을 셸로 분류(`is_shell: true`)한 뒤에만 보낸다. pid·이름이 바뀌어도 셸 위에서 띄운 일반 프로그램(예: vim)일 수 있으므로, 셸 판단의 근거는 호스트 분류다. `is_shell`이 `false`·`null`이거나 응답에 없으면 보내지 않는다. 추가 방어로 전경의 pid와 이름이 요청 때의 helper와 모두 달라야 하며, 그래서 아직 세대를 등록하지 않은 새 helper는 걸러진다.
    기다리는 동안 같은 surface에 다른 helper 세대가 열릴 수 있다(사용자의 실행, respawn, 탭 복원). 그래서 Ctrl+C를 보낼 때마다, 그리고 resume·안내를 보내기 직전에 surface의 현재 세대가 대상 세대인지 확인한다. 바뀌었으면 그 뒤로는 아무것도 보내지 않는다.
    재시작 완료는 resume 전송 뒤 만들어지고 같은 thread를 기록한 새 세대의 helper가 `ready`이면서 서버 응답으로 확인한 `resumed`가 그 thread인 것으로만 확인한다. `ready`만으로는 완료가 아니다. 새 세대가 확인 전에 `failed`·`exited`가 되면(예: 재개 거부, 정책 미적용) 재개 실패로 끝낸다.
    안내 문구와 별도 Enter는 이때 확인한 그 세대에만 보낸다. 현재 세대를 다시 읽어 대상으로 삼지 않는다. TUI 준비 대기(3s) 뒤 안내를 넣기 직전마다(재시도 포함)와 별도 Enter 직전에 surface가 아직 그 세대이고 그 helper가 `ready`(TUI 실행 중)인지 확인한다. 세대가 바뀌었거나 helper가 `exited`·`failed`를 기록했으면 TUI가 끝나 셸이 입력을 받으므로 그 뒤로는 보내지 않는다.
    | 상황 | 결과(플러그인 로그) |
    |---|---|
    | 마지막 Ctrl+C 뒤 8s 안에 세대가 끝나지 않음 | `did not end` 경고, resume 미전송 |
    | 세대는 끝났지만 5s 안에 셸로 분류된 전경(요청 때 helper와 다른 pid·이름)을 조회하지 못함 | `shell did not return` 경고, resume 미전송 |
    | surface가 다른 세대로 바뀜(입력 전), 또는 resume 뒤 다른 thread나 전송 전에 만든 세대가 나타남 | `surface no longer runs session` 경고, 이후 입력·안내 미전송 |
    | resume 뒤 30s 안에 새 세대가 재개를 확인하지 않음 | `no new session became ready` 경고, 안내 미전송 |
    | 새 세대가 재개 확인 전에 `failed`·`exited` | `the resumed session ended before it was confirmed` 경고, 안내 미전송. 사유는 그 세대 helper의 `detail`과 터미널 출력 |
    | 자기 새 세대 준비 | `session … is ready` 정보, 안내 전송 |
    | 안내 중 surface가 확인한 세대를 떠났거나 그 helper가 `exited`·`failed` | `surface left session … ; notice stopped` 경고, 이후 안내·Enter 미전송 |
  - **helper 세션이 아님**: 요청 때 화면의 종료 안내 횟수를 읽고, Ctrl+C 4회 뒤 화면에서 `run codex resume`의 횟수가 늘었는지 확인해 resume 명령을 보내고 시작 배너로 복귀를 확인한다. 화면 문자열 판정이므로 임의 출력이나 화면 이동으로 판정이 틀릴 수 있다.
  **턴의 마지막 행동으로 호출할 것.** SessionEnd는 현재 세션 meta를 정리한다.
  `Disconnected from this task.`나 원격 연결 문맥은 이 재시작 판정에서 사용하지 않는다.

### 자식 수 경고

spawn 뒤 조회한 자식 수가 설정의 Spawn child warning threshold(기본 6)를 넘으면
응답에 warning을 넣되 생성은 막지 않는다. 후보 목록은 idle 보고를 받은 자식과
`confidence: confirmed`인 stale 자식을 구분한다. 후자는 마지막으로 조회한 전경 이름이
셸이라는 뜻이며, 프로세스 종료나 훅 유실을 확인한 결과는 아니다.
idle 보고도 진위나 작업 성공을 검증한 값은 아니므로 현재 상태를 확인한 뒤 재사용한다.
문구는 `codex.spawn_warning.{total,idle,stale}`을 현재 언어로 번역한다.

`confidence: heuristic`인 stale은 후보 수에 넣지 않는다. 긴 추론·SIGSTOP·무출력 작업과
구별되지 않기 때문이다. 판정 기준은 [자식 터미널](https://github.com/zilhak/tasty/blob/develop/docs/features/child-terminal/index.md)의
판정 응답 필드를 참고한다.

## Codex 부모에게 결과 반환

Codex 대화(`CODEX_THREAD_ID`가 있는 호출자)에서 `tasty codex spawn`·`tell`·`respawn`을 호출하면
자식의 턴 경계가 그 대화로 자동으로 돌아간다. CLI가 호출자 문맥(`_tasty_context`)을 붙이며,
이 값은 같은 사용자의 프로세스가 알려 준 것이지 인증된 신원이 아니다. 판별 규칙은 이 플러그인의
매니페스트 `[[contributes.caller_context]]`가 선언한다. `CODEX_THREAD_ID`(UUID 형식)를 `thread_id`로,
비어 있지 않은 `CODEX_HOME`을 절대 경로 `codex_home`으로, PATH에서 찾은 `codex`를 `execution`으로 싣고
`TASTY_SURFACE_ID`로 surface를 채우지 않는다. 형식이 틀리면 이 플러그인과 Claude 플러그인의 명령은
실행하지 않는다([선언 형식](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#cli-호출자-문맥-contributescaller_context)). Claude 부모나 일반
surface 부모의 spawn은 기존 상태 알림 흐름을 그대로 쓴다.

### 수신 대상

`codex.parent`는 호출자 thread를 받을 수 있는지 확인하고 불투명한 `receiver_id`(32자리 16진수
난수)와 호스트에 넘길 부모 키를 돌려준다. spawn은 이 준비를 자식을 만들기 전에 먼저 한다.
thread·송신 경로(소유 세션 또는 공유 daemon과 그 `CODEX_HOME`)가 같으면 같은 `receiver_id`를
다시 쓴다. 그래서 spawn·tell을 반복해도 수신자가 늘지 않고 중복 판정 범위도 바뀌지 않는다.

| 호출자 thread | 부모 키 | 송신 경로 |
|---|---|---|
| Tasty helper 세션의 현재 세대 thread | `{"parent_external": "codex:<thread>"}` | 그 세대의 전용 서버 |
| 사용자 공유 daemon에 로드된 thread | `{"parent_external": "codex:<thread>"}` | `codex app-server proxy` |
| 둘 다 아님 | 실패 | 없음 |

Codex 부모는 실행 위치와 관계없이 자기 thread로 자식을 소유한다. helper surface는 대화가 실행되는
곳이자 전용 서버로 보내는 주소일 뿐 부모 키가 아니다. 같은 surface에서 `/new`로 대화가 바뀌면 새
대화는 다른 부모이므로 이전 대화의 자식을 관리하지 못한다. Claude 부모와 일반 surface 부모는 숫자
surface 부모 키를 그대로 쓴다.

공유 daemon의 로드 여부는 공식 proxy의 `thread/loaded/list`로 확인하며 호출자의 `CODEX_HOME`만
그 proxy 프로세스에 넘긴다. 다른 대상으로 대신 보내지 않는다.

proxy 연결 하나에는 연결부터 마지막 응답까지 하나의 기한이 있다. npm의 `codex`는 실제 proxy를 다시
띄우는 래퍼이고 실제 proxy가 stdout을 물려받으므로, 래퍼만 끝내면 응답 없는 proxy가 파이프를 쥔 채
남아 읽기가 풀리지 않는다. 그래서 Linux·macOS는 proxy를 별도 프로세스 그룹으로 띄우고 기한이 지나거나
연결을 닫을 때 그룹 전체를 끝낸다. Windows는 proxy를 Job Object에 넣고 job을 종료한다. proxy는 이미
실행 중인 daemon의 소켓에 바이트만 전달하므로 사용자의 daemon은 끝나지 않는다.

### 자식 소유와 반환 경로

Codex 호출자는 호스트가 그 대화(`codex:<thread>`)의 자식으로 기록한 surface만 관리한다.

- spawn·children·broadcast·kill·respawn에 다른 부모 surface를 명시하면 호스트에 요청하지 않고
  거절한다. 대화가 실행 중인 helper surface를 명시한 것은 받되, 호스트에는 thread 부모 키를 넘긴다.
- tell은 `terminal.parent`로 대상의 부모가 호출자인지 확인한다. 응답의 `parent_external`이
  `codex:<thread>`와 같아야 하며, 숫자 부모의 자식은 같은 surface에서 실행 중이어도 호출자의
  자식이 아니다. 이 필드를 알리지 않는 호스트에서는 소유를 증명할 수 없어 거절한다.
- 반환 경로(`children/<surface>.json`)는 자식에게 지시를 보내기 전에 기록한다. tell 전송이
  실패하면 이전 경로로 되돌린다.
- Codex 세션이 아닌 자식(helper 세션 기록도 반환 경로도 없는 surface)에 tell하면 경로를 두지
  않고 응답의 `warning`으로 결과가 돌아오지 않는다고 알린다.
- Codex 대화가 아닌 호출자(Claude 부모, 일반 surface)가 tell·spawn·respawn으로 새 지시를 보내면
  지시 전에 이전 Codex 반환 경로를 지우고, 그 자식의 아직 기록하지 않은 턴·종료 후보를 모두
  무효화한다([턴 결과 확정](#턴-결과-확정)). 이미 기록한 결과는 원장에 남아 읽을 수 있다.
- kill이 자식을 닫으면 그 경로를 지운다.

외부 부모 키만으로는 호스트가 창을 고르지 못하므로 Codex 부모의 children·broadcast·kill·respawn에는
`workspace`를 넣는다. 호스트는 그 workspace가 속한 창을 골라 그 창 안의 자식만 처리한다. 다른 창의
자식은 그 창의 workspace로 따로 요청해야 한다. 이때 호스트 응답의 `sent_count`와 children 상태는
창 밖 자식을 정확히 보고하지 않는다(창 밖 자식을 보낸 것으로 세거나 `exited`로 보고한다). 같은
창 안의 workspace들 사이에서는 어느 workspace로 요청해도 각 자식이 한 번씩 받는다. 플러그인은
요청을 한 번만 보내고 workspace는 다음 순서로 정한다.

1. 사용자가 `--workspace`로 명시한 값.
2. kill·respawn은 그 index로 반환 경로를 기록한 자식이 지금 있는 workspace.
3. 이 thread로 반환 경로를 기록한 자식 중 호스트가 지금도 그 부모의 자식으로 기록한 surface가
   모두 한 workspace에 있으면 그 workspace. 위치는 `surface.list`로 지금 상태를 읽는다.

후보가 둘 이상이면 어느 창의 자식인지 정할 수 없어 거절하고 `--workspace`를 요구한다. 포커스나
마지막으로 쓴 workspace로 정하지 않는다. 자식이 여러 창에 있으면 창마다 그 창의 workspace로
요청한다. spawn 경고의 자식 수도 spawn한 workspace의 창에서 센다.

### 기록과 송신

`codex.deliver`는 `receiver_id`, `completion_key`(1~160바이트 `[A-Za-z0-9._:-]`), `provider`
(`codex`·`claude`), `child_surface`, `child_session`(문자열 또는 null), `status`, `body`(문자열 또는
null), 선택 `attempt_id`를 받는다. `status`는 턴 경계의 종류이며 작업 품질의 판정이 아니다.

| status | 뜻 | Codex 자식의 발생 근거 |
|---|---|---|
| `turn_ended` | 모델 턴이 끝났다 | 소유 서버의 턴 상태 `completed` |
| `turn_failed` | 턴이 오류로 끝났다 | 소유 서버의 턴 상태 `failed` |
| `exited` | 세션(세대)이 끝났다 | SessionEnd 훅 뒤 세대 종료 확인 |
| `interrupted` | 사용자가 턴을 중단했다 | 소유 서버의 턴 상태 `interrupted` |

본문이 1MiB를 넘으면 문자 경계에서 자르고 `body_truncated`를 표시한다. 같은 `receiver_id`와
`completion_key`는 한 번만 기록하고 같은 `event_id`(32자리 16진수 난수)와 `duplicate: true`를
돌려준다. Codex 자식의 완료 키는 `codex:<thread>:<turn_id>`이고 종료는
`codex:<thread>:end.<세대>`다. 턴은 Codex가 준 `turn_id`로만 식별한다. Codex 0.154의
UserPromptSubmit·Stop·Interrupt 훅 입력은 `turn_id`를 반드시 담으며, 실제 실행에서 한 턴의
UserPromptSubmit과 Stop은 같은 값을 줬다. agent task 회차 표지도 같은 `turn_id`의 프롬프트에서
찾는다. `turn_id`가 없거나 형식이 틀린 Stop·Interrupt는 지원하지 않는 입력이다. 다른 턴에 붙이거나
새 키를 만들지 않고, 훅 응답의 `child_return_rejected: "missing_turn_id"`와 경고로 알린다.

### 턴 결과 확정

Stop 훅은 턴이 끝났다는 확정이 아니다. 실제 실행에서 Stop 훅이 실행되는 동안 그 턴의 서버
상태는 `inProgress`였고, 다른 Stop 훅이 턴을 이어 가게 하면 같은 `turn_id`의 Stop이 다시 온다.
훅의 `turn_id`는 app-server의 Turn ID와 같다. 그래서 결과는 세대의 소유 서버가 보고한 턴 상태로
정한다.

1. Stop·Interrupt 훅은 바뀌지 않는 후보(수신자, 자식 thread, 세대, `turn_id`, 회차 표지)를 남기고
   바로 돌아간다. 같은 턴의 후보가 이미 있으면 그대로 둔다. 수신자는 이때 정해지므로 뒤에
   반환 경로가 바뀌어도 이 턴의 수신자는 바뀌지 않는다.
2. 별도 스레드가 소유 서버에 `thread/turns/list`(최근부터 16개씩 4페이지까지, 항목 전체)로 그
   턴을 찾는다. 동시에 실행하는 확인 작업은 2개까지다. 슬롯은 작업 단위이며 그 작업의 결과가 돌아올 때만 빈다. 훅은 확인을 기다리지 않는다.
3. 턴 상태가 `completed`·`interrupted`·`failed`면 그 턴의 마지막 agentMessage를 본문으로 한 번
   기록하고 후보를 지운다. 실패한 턴에 메시지가 없으면 오류 문구가 본문이다. Stop 입력의
   `last_assistant_message`는 쓰지 않는다.
4. 아직 `inProgress`이거나, 턴을 찾지 못했거나, 서버에 닿지 못하면 1초부터 두 배씩(상한 60초)
   기다렸다가 다시 확인한다. 한 프로세스에서 24번 확인하지 못하면 보류하고, 같은 세대의 다음
   턴 경계 훅이나 플러그인 재시작 때 다시 확인한다. 다른 세대의 보류 후보는 다시 열지 않는다.
   상태를 추정해 기록하지 않는다.
5. 재확인 깨우기는 후보마다 만들지 않는다. 한 번 처리할 때 남은 후보 중 가장 이른 시각 하나를
   플러그인 프로세스의 스케줄러 스레드 하나에 보낸다. 스케줄러는 가장 이른 예약 하나만
   기억하고(늦은 예약은 합쳐진다) 그 시각에 깨우기를 한 번 보낸 뒤 기억을 비운다. 깨우기를 받은
   처리가 남은 후보의 다음 시각을 다시 예약한다. 스레드는 예약 수와 관계없이 하나다. 깨우기는
   공유 예약 상태를 지우지 않으므로 옛 깨우기가 새 예약을 취소하지 못한다. 원장 송신 재시도
   타이머는 이 스케줄러를 쓰지 않는다.
6. 세대의 helper가 서버를 끝냈다고 기록했거나(`exited`·`failed`) 세션 기록이 없으면 그 세대의
   턴은 더 확인할 수 없다. 그 후보는 바로 보류한다.
7. Codex 대화가 아닌 호출자가 그 자식에게 새 지시를 보내면, 지시 전에 그 자식(surface)의 아직
   기록하지 않은 턴·종료 후보를 모두 `turns/void/`로 옮겨 무효화한다. 확인 중이던 턴이 그 뒤
   끝나도, 같은 턴의 Stop이 다시 와도 옛 수신자에게 기록하지 않는다. 이미 원장에 기록한 결과는
   그대로 둔다. 확인 결과는 후보마다 만든 정체(`id`)가 같은 후보에만 반영하므로, 늦게 온 결과가
   무효화한 후보를 되살리지 못하고, 그 뒤 다시 묶인 경로에서 같은 턴 키로 만든 새 후보를 끝내거나
   지우지도 못한다. 후보의 수신자는 Stop 때 고정한 값 그대로이며 무효화가 다른 수신자로 바꾸지
   않는다. 끝났지만 아직 확인하지 못한 턴(재확인 대기 중)도 결과가 아니므로 함께 무효화된다.
   무효화는 예약 정보만 지우며, 이미 실행 중인 그 후보의 확인 작업은 결과가 돌아올 때까지 슬롯을
   차지한다. 늦은 결과는 자기 작업의 슬롯만 비우고 새 후보의 확인 표시를 건드리지 않으므로, 같은
   후보의 확인이 겹치거나 무효화를 거듭해 실제 확인이 상한을 넘지 않는다. 비운 슬롯으로 기다리던
   후보를 이어서 확인한다.
8. 해석할 수 없는 후보 파일(손상된 JSON, 필드가 빠진 형식)은 `turns/corrupt/`로 옮겨 격리하고
   경고를 남긴다. 다른 읽기 오류는 경고만 남기고 건너뛴다. 어느 경우든 다른 후보의 확인과 다른
   자식에 대한 일반 지시는 막지 않는다.

SessionEnd는 종료 후보를 남기고, 세대가 끝났다고 확인될 때만 종료(`exited`)를 기록한다. 후보는
1초(`EXIT_POLL`) 간격으로 다시 확인한다.

| 조건 | 처리 | 남은 턴 후보 |
|---|---|---|
| helper가 `exited`·`failed`를 기록했거나 세대 기록이 없다 | 바로 종료를 기록한다 | 결과로 기록하지 않고 후보 파일로 남긴다 |
| SessionEnd 뒤 10초(`EXIT_GRACE`)가 지났고 세대의 서버에 닿지 못한다 | 종료를 기록한다 | 같다 |
| SessionEnd 뒤 10초가 지났고 세대의 서버가 응답한다 | 대화만 바뀐 것이므로 기록하지 않고 후보를 `turns/void/`로 옮긴다 | 그대로 확인을 이어 간다 |
| 같은 세대에 다른 thread가 새로 묶였다(빈 세대의 SessionStart, 확인된 prompt, 대화 전환) | 새 바인딩이 세대가 살아 있다는 증거이므로 이전 thread의 종료 후보를 바로 `turns/void/`로 옮긴다 | 그대로 확인을 이어 간다 |
| 그 밖 | 기다린다 | 확인을 이어 간다 |

TUI가 끝나면 helper가 곧바로 `exited`를 기록하므로 종료는 보통 SessionEnd 뒤 1초 안에 기록된다.
한 세대의 종료는 그때 묶여 있던 thread의 것으로 한 번만 기록된다.
정상 실행에서 턴은 Stop 훅 직후 `completed`가 되고 확인 응답은 수십 ms라, 부모는 마지막 턴
결과를 종료보다 먼저 받는다. 서버에 닿지 못하거나 서버가 끝난 경우에는 종료가 먼저 갈 수 있고,
확인하지 못한 턴은 결과가 아니라 남은 후보로만 있다.

deliver는 이벤트 기록과 claim까지만 동기로 처리하고 송신은 백그라운드 스레드에 맡긴다.
송신은 부모 thread마다 한 건씩 기록 순서대로 하고, 전체 동시 송신은 4건까지다. 결과 하나를
기록할 때 지난 이벤트의 본문을 다시 읽지 않는다.
부모 thread의 대기열(`thread/queue/add`)에는 다음 고정 문장만 넣는다. 자식 본문은 넣지 않는다.

```text
[tasty] A child agent reported a turn boundary. provider=<provider> status=<status> event=<event_id>. This is a reference, not the result. Read the result with: tasty codex result <event_id>
```

| delivery_state | 뜻 | 다음 동작 |
|---|---|---|
| `captured` | 기록했고 아직 보내지 못했다 | 보낸다. 보내지 못한 뒤에는 1초부터 두 배씩(상한 60초) 기다렸다가 다시 보낸다 |
| `held` | 보낼 대상이 없다(소유 부모 TUI가 붙지 않음) 또는 자동 재시도 12회를 넘었다 | 그 thread의 다음 결과, 부모 TUI의 SessionStart, 플러그인 재시작 때 다시 보낸다 |
| `sending` | 송신 중 | 결과 반영 |
| `accepted` | 서버가 대기열에 넣었다 | 없음. 부모가 읽었다는 뜻은 아니다 |
| `rejected` | 서버가 거절했다 | 없음. 오류 코드·문구를 남긴다 |
| `not_loaded` | 외부 부모 thread가 로드돼 있지 않다 | 없음 |
| `unknown` | 보낸 뒤 응답을 잃었다 | 대기열을 한 번 조회해 있으면 accepted. 다시 보내지 않는다 |

플러그인이 송신 중에 재시작되면 sending은 unknown이 되고 대기열을 한 번 조회한다. captured와
held는 다시 보낸다. 원장은 플러그인 데이터 폴더의 `return/`(`receivers/`·`events/`·`claims/`·
`pending/`·`children/`)에 있으며 폴더는 0700으로 만든다. `pending/`은 아직 할 일이 남은
이벤트의 색인이다. 송신과 재시작 복구는 이 색인만 읽는다.

기록은 색인 표지, 이벤트, claim 순서로 하고 claim은 내용을 다 쓴 임시 파일을 hard link로
붙여 만든다. 어느 단계에서 중단돼도 이벤트를 잃지 않는다. 비었거나 없는 이벤트를 가리키는
claim은 손상으로 보고 남은 이벤트 쪽으로 고친다. claim 파일 이름은 완료 키의 SHA-256이라
대소문자를 구분하지 않는 파일 시스템이나 Windows 예약 이름에서도 키가 겹치지 않는다.

할 일이 끝난 이벤트는 30일 동안 보관하고, 그 뒤 플러그인이 시작할 때 지운다. 지운 이벤트는
`tasty codex result`로 읽을 수 없다. 보내지 못한 이벤트는 나이와 관계없이 지우지 않는다. claim은
가리키는 이벤트가 남아 있는 동안 지우지 않으므로 오래 보류된 결과의 같은 키가 다시 와도 중복으로
처리한다. 지운 이벤트를 가리키는 30일 넘은 claim만 함께 지운다.

### 결과 읽기

```sh
tasty codex result <event_id>
```

참조를 받은 Codex 대화에서 실행한다. 이벤트의 부모 thread와 호출자 thread가 다르면 거절한다.
응답은 provider·status·body·body_truncated·child_surface·child_session·completion_key·attempt_id와
전달 상태를 담는다. 읽어도 본문은 지워지지 않으며 읽은 횟수와 처음 읽은 시각만 기록한다.

## 훅 설치 대상 선택

`tasty codex install --codex-home <절대 디렉터리>` 는 그 Codex home 의 hook 을 설치한다.
`--config-file <절대 파일>` 은 설정 파일을 직접 고르며 `--codex-home` 과 함께 쓸 수 없다.
기존 설정을 읽거나 파싱할 수 없으면 덮어쓰지 않고 실패한다. 둘 다 생략하면 호스트 환경의
`CODEX_HOME` 을, 그것도 없으면 `HOME`/`USERPROFILE` 아래 `.codex` 를 쓴다. 원격 서버의
설정은 로컬 설치가 성공했다는 것만으로 갱신되지 않는다.

## 인터페이스

- **AI Agent / 사용자**: `tasty codex launch|spawn|tell|broadcast|kill|respawn|children|parent|result|hook|install …`.
- 일반 흐름: `spawn --prompt "…"` → (선택) `tell` → 완료 알림 대기(caller의 completion-log 조회) → 출력 확인.

## 비-목표

- Codex 자체 기능 — 외부 CLI. 이 플러그인은 *실행·배치·관계 관리*.
- 터미널/PTY 내부 — host.

## Acceptance Criteria

- Given Codex 대화가 부모이고 대상이 attach mirror workspace When `tasty codex spawn --workspace <mirror id>` Then 자식은 서버에서 서버 설정으로 실행되고, mirror 자식에 결과 경로를 묶은 뒤에만 서버가 통지를 보내며, 자식의 턴 결과는 부모 대화에 한 번 기록된다.
- Given 셸이나 Claude 세션 surface 가 원격 Codex 자식을 spawn·tell 한 상태 When 서버에서 그 턴이 확정됨 Then client plugin 은 Codex 원장 대신 `attach-results/` 에 본문을 먼저 저장하고 부모 완료 로그에 결과 ID 와 `tasty codex remote-results --id <ID>` 를 남긴 뒤 `recorded` 로 답한다.
- Given 한 부모가 만든 원격 자식 When 다른 부모(다른 surface, 같은 surface 의 다른 Claude 세션, 다른 Codex 대화, 호출 surface 없음)가 그 자식에게 `tell` 함 Then client plugin 은 결과 수신·idle 여부와 관계없이 경로·토큰을 바꾸거나 원격에 보내지 않고 `child_of_other_parent` 로 거절하며, 만든 부모의 후속 `tell` 은 보낸다.
- Given 소유자 기록 파일이 없거나 깨진 원격 자식 When 어느 부모든 그 자식에게 `tell` 함 Then client plugin 은 소유자를 새로 맡기지 않고 원격에 보내지 않은 채 `child_without_owner` 로 거절한다.
- Given 서버가 원격 자식의 턴을 확정했지만 client 가 아직 확인하지 않음 When 서버 plugin 이 다시 시작 Then 보낼 기록은 남아 있고 같은 키로 다시 맡기며, client 는 같은 결과를 한 번만 기록한다.
- Given mirror 자식의 결과 경로를 묶지 못함 When spawn Then 통지를 풀지 않고(commit 없음) 서버의 그 자식을 닫은 뒤 오류로 끝난다.
- Given Tasty helper 세션의 Codex 부모 When 그 대화에서 `tasty codex spawn --prompt "…"`를 실행하고 자식의 턴이 끝난다 Then 부모 thread의 대기열에 고정 형식의 참조가 한 번 들어가고 부모는 `tasty codex result <event_id>`로 본문을 읽는다.
- Given Tasty 세션도 아니고 공유 daemon에 로드되지도 않은 Codex 호출자 When spawn Then 자식을 만들지 않고 받을 수 없는 부모라고 알린다.
- Given 같은 턴의 Stop 훅이 두 번 실행 When 결과를 기록 Then 같은 event_id를 돌려주고 대기열에는 한 번만 넣는다.
- Given 송신 중 플러그인이 재시작 When 다시 시작 Then 그 이벤트를 다시 보내지 않고 대기열을 한 번 확인해 accepted 또는 unknown으로 둔다.
- Given 공유 daemon에 일시적으로 연결하지 못해 보내지 못한 결과 When 다른 spawn·tell이 없다 Then backoff 뒤 다시 보낸다.
- Given 같은 TUI에서 A 대화 뒤 `/new`로 B 대화를 시작 When A의 훅이 B보다 늦게 도착 Then 서버가 A를 실행 중으로 보고하지 않으므로 경로는 B에 남는다.
- Given B로 옮긴 뒤 이미 로드된 A로 돌아감 When A의 UserPromptSubmit이 SessionStart 없이 옴 Then 서버가 A를 실행 중으로 보고하므로 경로가 A로 옮겨 A의 결과가 돌아간다.
- Given 서로 다른 두 턴이 같은 응답으로 끝남 When 두 Stop이 옴 Then turn_id가 달라 두 결과를 모두 기록한다.
- Given 다른 Stop 훅이 턴을 이어 가게 해 같은 turn_id의 Stop이 두 번 옴 When 첫 Stop 뒤 서버가 그 턴을 `inProgress`로 보고 Then 기록하지 않고 다시 확인하며, 서버가 `completed`로 보고한 뒤 마지막 메시지로 한 번 기록한다.
- Given turn_id가 없는 Stop When 훅이 옴 Then 결과를 기록하지 않고 `child_return_rejected: "missing_turn_id"`로 알린다.
- Given 확인하지 못한 턴 후보 50개 When 같은 세대의 훅이 반복됨 Then 한 번의 처리는 깨우기를 하나만 요청하고, 더 이른 예약을 거듭해도 스케줄러 스레드는 하나다.
- Given 확인 중인 턴이 있는 Codex 자식 When 일반 셸에서 그 자식에게 tell Then 그 턴은 나중에 끝나도 부모에게 기록되지 않고, 이미 기록한 결과는 남는다.
- Given 확인이 도는 중 무효화와 재bind가 거듭됨 When 같은 턴 Stop이 반복됨 Then 실제 확인 작업은 2개를 넘지 않고 한 후보의 확인이 겹치지 않는다.
- Given 해석할 수 없는 후보 파일 When 다른 자식에게 일반 셸에서 tell Then tell은 성공하고 그 파일은 격리된다.
- Given 확인하지 못한 턴 후보가 있는 세션 When SessionEnd 뒤 helper가 서버 종료를 기록했거나 10초가 지나도 서버에 닿지 못함 Then 종료를 기록하고, 그 턴은 결과로 기록하지 않는다.
- Given `/new` 뒤 이전 thread가 unload되어 SessionEnd가 옴 When 10초 뒤에도 세대의 서버가 응답 Then 종료를 기록하지 않고 세대의 thread를 비우며, 다음 대화의 SessionStart가 바로 묶여 그 턴이 부모에게 돌아간다.
- Given 새 대화가 이미 묶인 세대 When 이전 thread의 SessionEnd가 늦게 옴 Then 무시하고 새 thread와 meta는 그대로다.
- Given A의 unload SessionEnd 뒤 10초 안에 B가 SessionStart나 확인된 prompt로 묶임 When TUI가 끝남 Then 종료는 B의 것 한 번만 기록되고, A의 미확정 턴 후보는 남아 확인된다.
- Given thread가 빈 세대 When SessionStart 없이 prompt가 옴 Then 서버가 그 thread를 실행 중으로 보일 때만 묶는다.
- Given Tasty가 띄운 Codex 부모가 같은 TUI에서 `/new`로 새 대화를 시작 When 새 대화에서 spawn Then 새 thread가 그 surface의 소유 부모로 결과를 받고, 늦게 온 이전 thread의 훅은 무시한다.
- Given Codex 부모 A의 자식 X When 다른 Codex 부모 B가 X에 tell Then 보내지 않고 X의 반환 경로도 바꾸지 않는다.
- Given helper surface 5의 Codex 부모 A가 자식 X를 만든 뒤 같은 TUI에서 `/new`로 B가 됨 When B가 X에 tell·kill하거나 children·broadcast Then B의 부모 키(`codex:<B>`)로 요청하므로 X는 대상이 아니고 X의 반환 경로는 A에 남는다.
- Given Codex 부모 A의 자식 X When Claude 부모가 X에 tell Then 전송 전에 A로 가는 경로를 지우고 이미 기록한 결과는 남긴다.
- Given 두 workspace에 자식이 나뉜 Codex 부모 When `--workspace` 없이 children·broadcast Then 호스트에 보내지 않고 workspace를 고르라고 알린다. `--workspace`를 주면 요청을 한 번 보낸다. 두 workspace가 같은 창이면 각 자식이 한 번씩 받고, 다른 창이면 지정한 workspace의 창에 있는 자식만 받는다.
- Given claim을 만드는 도중 플러그인이 끝나 빈 claim이 남음 When 다시 시작하거나 같은 키를 다시 기록 Then 이벤트를 지우지 않고 claim을 고친다.
- Given 전용 app-server가 시작하지 못함 When helper 실행 Then TUI를 시작하지 않고 실패를 알리며 embedded·공유 서버로 내려가지 않는다.
- Given 승인·sandbox 정책이 있는 reboot·탭 복원 When helper가 기존 thread를 재개 Then TUI에는 정책 인자를 넘기지 않고, 소유 서버의 `thread/resume`이 적용한 정책을 TUI가 붙은 뒤에도 서버가 유지한다.
- Given 소유 서버가 재개를 거부하거나 요청한 정책과 다른 값으로 응답 When helper 실행 Then 서버를 정리하고 `failed`와 사유를 남기며 TUI를 시작하지 않는다.
- Given reboot로 재개한 TUI가 시작 직후 끝나 셸로 돌아옴 When 안내를 보낼 차례 Then helper `exited`를 보고 안내와 Enter를 셸에 넣지 않는다.
- Given TUI가 끝나 서버를 정리 중인 Linux·macOS helper When 터미널에서 Ctrl+C·hangup이 반복해 도착 Then helper는 끝나지 않고 서버 그룹 정리를 마치며 npm 래퍼·실제 서버·TUI가 남지 않는다.
- Given 응답하지 않는 공유 daemon proxy When 연결 기한이 지남 Then 연결은 송신 전 실패로 끝나고 npm 래퍼와 실제 proxy가 모두 끝난다.
- Given 셸 통합을 우회해 helper 없이 실행한 `command codex` When 훅이 실행 Then 상태·메타데이터를 바꾸지 않는다.

- Given Linux/macOS 의 sh/bash/zsh 에 `codex` alias 또는 function 이 우회 옵션을 추가한다 When `launch`/`spawn`/`respawn`/`reboot` 의 helper 명령을 실행한다 Then helper 는 tasty 가 결정한 thread·프롬프트 파일·정책 인자만 받는다.

- Given 플러그인 활성 When `tasty codex spawn --prompt "…"` Then 자식 Codex 가 페인 분할로 생성되고 CLI는 호스트 응답 후 반환하며 자식 작업 완료는 기다리지 않는다.
- Given 자식 When `tasty codex tell <msg>` Then 줄바꿈 보존하며 메시지가 전송·제출되고 CLI는 호스트 응답 후 반환하며 자식 작업 완료는 기다리지 않는다.
- Given 상태 훅과 로그 기록이 정상 동작하는 대상 When 등록된 idle·needs_input·process-exit 훅이 실행 Then caller의 상태 로그를 쓰고 형제 훅을 정리한다. `surface.locate`가 성공하면 다시 등록한다.
- Given 같은 부모가 같은 자식에 `tasty codex tell`을 여러 번 보낸 상태 When 자식의 상태가 한 번 바뀜 Then 부모의 상태 로그에 완료 줄이 한 줄 남는다. 다른 부모가 등록한 그룹은 지우지 않는다.
- Given 훅이 설치된 Codex When 도구 실행 승인 프롬프트가 뜬다 Then 그 surface 의 상태가 `needs_input` 으로 조회되고 비포커스 대상의 탭·워크스페이스에 기존 노란 표시가 난다.
- Given 승인 대기 When 사용자가 승인하고 그 도구가 끝난다(`PostToolUse`) 또는 거절·Esc·Ctrl-C 로 중단한다(`Interrupt`) Then 상태가 각각 `active` · `idle` 로 돌아오고 `needs_input` 이 잔류하지 않는다.
- Given 승인이 필요 없는 실행 When 도구가 오래 걸리거나 출력이 없다 Then `needs_input` 으로 오판하지 않는다.

- Given Bash·zsh에서 셸 통합으로 실행한 `codex resume` When 선택기에서 대화를 고름 Then 첫 모델 턴 전에도 선택된 UUID와 실행 정책으로 복원 명령이 저장된다.
- Given 복원 정보를 저장한 Codex 터미널 When Tasty를 정상 종료하고 다시 열어 해당 surface를 깨움 Then 새로운 세대에서 같은 대화·승인·sandbox 설정으로 재개한다.

- Given 시작 화면에 선택된 Codex 터미널 When PTY 복원이 IPC 시작보다 먼저 진행됨 Then helper는 읽기 전용 준비 조회 뒤 한 번만 세대를 등록하고 수동 wake 없이 같은 대화를 재개한다.
