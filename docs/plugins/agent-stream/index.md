# Agent Stream (`com.tasty.agent-stream`)

- **Status**: Implemented — 이벤트 수집·SSE 전송·요청과 턴 매칭을 제공한다. 인바운드 웹훅은 운영자가 등록한다(아래 [요청과 턴의 매칭 및 웹훅 연결](#턴-correlation--웹훅-인바운드-배선))
- **주체**: AI Agent (CLI/IPC). 로컬 사용자 UI 없음 — headless
- **배포/통합**: workspace 번들(`BUILTINS` 등록) · CLI + IPC namespace — [plugins 개념](https://github.com/zilhak/tasty/blob/develop/docs/concepts/plugins.md)
  - `bundle = false` — 배포 패키징(DMG / AppImage / MSIX / deb)에서는 제외한다. 워크스페이스 빌드의 dev 번들 sync 는 그대로 동작한다.
- **코드**: `crates/tasty-plugin-agent-stream/`
- **권한**: `surface.read`(세션 id meta 조회 · 대상 생존 확인) · `fs.read`(transcript 읽기) · `fs.write`(data_dir 의 watch 스냅샷 쓰기) · `network`(SSE 엔드포인트 bind)
- **화면**: 없음
- **근거**: Agent Stream 설계 — 수집·SSE 노출·요청 매칭. [웹훅 신뢰 모델](https://github.com/zilhak/tasty/blob/develop/docs/features/webhook/index.md).

> 이 플러그인은 별도 수집 스레드와 HTTP 서버를 두고 CLI·IPC로 제어하는 예제다. SDK의 동기 콜백과 백그라운드 작업을 함께 사용하는 방법은 [플러그인 개발](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#cli--ipc-namespace)과 [SDK의 한계](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md#10-한계-현재-sdk)를 참고한다.

## 목적

Surface에서 실행 중인 AI 에이전트의 transcript를 읽어 구조화된 이벤트로 제공한다. 화면을 읽는 `tasty read screen`이나 `output-match` 훅과 달리, 응답 텍스트와 사고 블록을 원본 레코드의 종류에 따라 구분한다.

현재 해석하는 형식은 Claude Code transcript뿐이다. 다른 에이전트를 지원하려면 경로와
레코드 형식을 추가해야 하며, 이름만으로 Codex 지원을 뜻하지 않는다.

## 내부 동작

### 대상 해석 — surface_id → 세션 id → transcript

1. claude plugin 의 `SessionStart` 훅이 세션 id 를 surface meta `claude-session-id` 로 기록한다.
2. `surface.meta.get`으로 세션 ID를 읽는다. 값이 없으면 대상 파일을 정할 수 없어 watch를 거부한다.
3. 세션 id 가 그대로 파일명이다. transcript 루트(`$CLAUDE_CONFIG_DIR/projects`, 미설정 시 `~/.claude/projects`) **한 겹 아래를 훑어** `<session-id>.jsonl` 을 찾는다 — project-slug 규칙을 계산하지 않는다.

claude plugin 에 대한 **코드 의존은 없다**. 접점은 host IPC 로 읽는 surface meta 키 하나뿐이다.

### tail 루프

수집은 별도 스레드에서 수행해 플러그인의 요청 처리와 분리한다. 호스트는 15s 간격의 ping과 60s 무응답 기준을 사용하며, 실제 재시작 판단은 호스트가 다음 상태를 확인할 때 이뤄진다. 수집 루프는 아래 간격을 사용하지만 파일 읽기와 IPC 대기까지 포함한 처리 시간의 상한은 아니다.

| 주기 | 하는 일 |
|------|---------|
| 300ms | 파일을 offset 부터 읽어 완성된 라인을 이벤트로 변환. host IPC 호출 없음 |
| 3s (10 tick) | `surface.locate`로 surface 존재 확인, `surface.meta.get` 으로 세션 교체 확인 |

파일 상태에 따라 다음과 같이 처리한다.

| 상태 | 판정 | 대응 |
|------|------|------|
| 아직 생성 안 됨(세션 시작 직후 race) | `metadata` 가 `NotFound` | `awaiting_transcript` 로 대기, 매 tick 경로 재해석 |
| 읽는 중 삭제 | 위와 동일 | 재생성되면 처음부터 다시 읽는다 |
| 중간 truncate | `len < offset` | 0 부터 재동기화 |
| rotate / 파일 교체 | inode(Unix) · file index(Windows) 변화 | 0 부터 재동기화 |
| 개행 없이 끝난 부분 라인 | 버퍼 잔여 | 다음 읽기에서 개행까지 완성된 뒤 처리 |

다시 읽은 레코드는 최근 UUID 4096개를 기억해 중복 제거한다. UUID가 없거나 이미 캐시에서 빠졌다면 다시 처리될 수 있다.

### 이벤트 모델

`assistant` 레코드의 `message.content[]` 블록과 턴 종료만 이벤트가 된다.

| kind | 출처 | 실린 필드 |
|------|------|-----------|
| `text` | `content[].type == "text"` | `text` |
| `thinking` | `content[].type == "thinking"` (본문 키는 `thinking`) | `text` |
| `tool_use` | `content[].type == "tool_use"` | `tool_name` · `tool_input` |
| `turn_end` | 아래 표 | `reason` |

모든 이벤트에 `seq`, `surface_id`, `session_id`를 넣고, 파일에서 읽은 이벤트에는 `record_uuid`와 `timestamp`도 넣는다. `seq`는 한 실행 안에서 증가하며 재시작하면 마지막 저장값에서 이어간다. 마지막 저장 뒤 사용한 번호는 재사용될 수 있다. 요청 턴이 열려 있으면 `request_id`도 넣고, 턴 밖의 이벤트에는 생략한다([요청과 턴의 매칭](#턴-correlation--웹훅-인바운드-배선)).

> **`thinking` 의 본문은 비어 있을 수 있다.** Claude Code 버전/설정에 따라 transcript 의 `thinking` 블록이 `signature` 만 남기고 본문 없이 기록될 수 있다. 이 경우 `thinking` 이벤트의 `text` 가 빈 문자열이다 — 소스에 없는 것을 지어내지 않고 그대로 중계한다. kind 분리 자체는 유효하므로 소비자는 여전히 사고 블록을 골라 버릴 수 있다.

턴 종료에는 정상 완료뿐 아니라 취소, 오류와 스트림 해제도 포함한다.

`reason`의 접두사로 출처를 구분한다. `stop:` 뒤에는 transcript의 `stop_reason`을 그대로 넣고, `stream:` 뒤에는 이 플러그인이 판단한 사유를 넣는다. 소비자는 `reason.starts_with("stream:")`으로 두 종류를 구분할 수 있다.

| `reason` | 언제 |
|----------|------|
| `stop:end_turn` / `stop:max_tokens` / … | `assistant.message.stop_reason` 에 `stop:` 을 붙인 것 (단 `tool_use` 는 턴 종료가 아니다 — 툴 결과를 받아 계속된다) |
| `stream:api_error` | `isApiErrorMessage: true` — API 오류 응답은 `stop_reason` 이 평범한 값으로 오므로 이쪽을 먼저 본다 |
| `stream:cancelled` | `user` 레코드에 `[Request interrupted by user…]` 마커 |
| `stream:session_ended` | 대상 surface 가 사라졌거나, 같은 surface 에서 새 세션이 시작돼 이전 세션이 닫혔다 |
| `stream:unwatched` | `agent_stream.unwatch` 호출 |
| `stream:rewatched` | 같은 surface 를 다시 `watch` 해 이전 등록이 교체됐다 |
| `stream:turn_timeout` | 열린 correlation 턴이 자기 비활동 타임아웃을 넘겼다 — `turn_start` 뒤 `claude.tell` 이 실패한 경우의 안전망(아래 correlation 절) |

사용자 프롬프트 본문 · 툴 결과 · 첨부 · 모드 전환 등 나머지 레코드는 **중계 대상이 아니다**(비-목표).

### 세션 전환

verify tick 에서 surface meta 의 세션 id 가 바뀐 것을 발견하면, 이전 세션에 `turn_end{reason=stream:session_ended}` 를 남기고 tail 대상을 새 파일로 교체한다. 새 세션 파일은 **처음부터** 읽는다(그 세션의 전부가 대상이다). 새 파일이 아직 없으면 경로 미해결 상태로 두고 매 tick 재해석한다.

### 재시작 복구 — at-least-once

watch 대상과 byte offset, seq 커서를 `TASTY_PLUGIN_DATA_DIR/watches.json`에 저장한다.
같은 디렉터리의 임시 파일에 쓴 뒤 rename하며, 재시작하면 저장된 offset에서 다시 읽는다.
마지막 저장 뒤 읽었던 레코드가 반복될 수 있으므로 소비자는 재전송을 고려해야 한다.
실행 중 파일 재동기화의 중복은 최근 record UUID 캐시로 제거한다.

`record_uuid`는 이벤트 하나가 아닌 transcript 레코드의 식별자다. 한 레코드에서 text,
thinking, tool_use, turn_end 등 여러 이벤트가 나올 수 있으므로 UUID가 같다는 이유만으로
첫 이벤트 뒤를 전부 버리면 안 된다. seq는 같은 실행의 재전송 커서이며, 레코드 재처리 시
새 seq가 붙을 수 있다. 소비자는 필요한 단위에 맞춰 중복을 처리해야 한다.

이벤트 버퍼와 열린 요청 턴은 메모리에만 있어 재시작하면 사라진다.
저장된 offset 이전의 이벤트가 버퍼에서 사라졌다고 해서 자동으로 다시 읽는 것은 아니다.
따라서 at-least-once 재읽기 정책을 모든 네트워크 누락의 복구 보장으로 해석하지 않는다.
`TASTY_PLUGIN_DATA_DIR`이 없으면 다른 경로를 임의 선택하지 않고 영속화를 건너뛴다.

## 인터페이스

- **AI Agent**: `tasty agent-stream …` CLI 와 `agent_stream.*` IPC 양면. GUI 전용 경로 없음.
- **로컬 사용자**: 없음(headless).

### CLI / IPC

| CLI | IPC 메서드 | 설명 |
|-----|-----------|------|
| `tasty agent-stream watch [--surface N] [--from-start]` | `agent_stream.watch` | tail 시작. `--surface` 미지정 시 `TASTY_SURFACE_ID`. 기본은 현재 파일 끝부터 — `--from-start` 면 처음부터 |
| `tasty agent-stream turn-start --request-id ID [--surface N] [--timeout-secs S]` | `agent_stream.turn_start` | correlation 턴을 연다. 이후 그 surface 의 이벤트가 `request_id` 로 태깅된다. 웹훅 IpcSequence 의 첫 스텝. watch 중이 아니거나 이미 열린 턴이 있으면 거부 |
| `tasty agent-stream unwatch [--surface N]` | `agent_stream.unwatch` | tail 중지 + `turn_end{reason=stream:unwatched}` |
| `tasty agent-stream list` | `agent_stream.list` | 전 대상 조회(포커스 무관). `status` 는 `tailing` / `awaiting_transcript` |
| `tasty agent-stream poll [--surface N] [--after-seq S] [--limit L]` | `agent_stream.poll` | seq 커서 기반 **비파괴** 읽기. 여러 소비자가 각자 커서로 같은 버퍼를 읽는다 |
| `tasty agent-stream serve --port N [--bind ADDR] [--token T]` | `agent_stream.serve` | SSE 엔드포인트를 연다. 이미 떠 있으면 끄고 새 설정으로 다시 연다(`replaced: true`) |
| `tasty agent-stream serve-stop` | `agent_stream.serve_stop` | 엔드포인트를 닫고 열린 구독을 전부 끊는다 |
| `tasty agent-stream serve-info` | `agent_stream.serve_info` | 엔드포인트 상태 + 구독자별 카운터. **토큰은 싣지 않는다** |

`watch` 는 **surface_id 를 명시적으로 지정**하는 것만 지원한다 — "전부 watch" 와일드카드가 없다. 같은 surface 를 다시 `watch` 하면 이전 등록을 교체하고(`replaced: true`) 그 등록에 `turn_end{reason=stream:rewatched}` 를 남긴다.

**`--from-start` 의 예외** — `--from-start` 없이 등록했더라도, 등록 시점에 transcript 파일이 아직 없었다면(`awaiting_transcript`) 나중에 파일을 찾은 순간 **처음부터** 읽는다. "현재 파일 끝부터"의 기준이 될 파일이 애초에 없었으므로 그 세션의 전부가 대상이다. 세션 전환으로 파일이 바뀌는 경우도 같다.

`poll` 의 `--surface` 는 파라미터 이름이 `filter_surface` 다. CLI 계층이 `surface` 라는 이름의 u32 인자를 `TASTY_SURFACE_ID` 로 자동 채우기 때문에, 그대로 두면 지정하지 않았는데도 호출자 자신의 surface 로 조용히 좁혀진다.

수집 버퍼는 4096 개 상한의 링이다. 넘치면 오래된 것부터 버리고 `poll` 응답의 `dropped` 로 알린다.

## SSE 엔드포인트

외부 소비자가 이벤트를 구독할 수 있도록 플러그인 프로세스에서 HTTP 서버를 연다. 본체 웹훅 리스너는 요청을 받는 용도이므로 SSE 전송에 사용하지 않는다. 선택 이유는 Agent Stream 설계에 있다.

```bash
tasty agent-stream serve --port 8787                 # loopback, 무인증
tasty agent-stream serve --port 8787 --bind 0.0.0.0 --token s3cret
curl -N http://127.0.0.1:8787/events
```

### 계약

| 항목 | 값 |
|------|-----|
| 경로 | `GET /events` (그 밖의 경로는 404, 다른 메서드는 405) |
| 응답 | `200` · `Content-Type: text/event-stream` · `Cache-Control: no-cache` · `X-Accel-Buffering: no` · `Content-Length` 없음 |
| 프레임 | `id: <seq>` / `event: <kind>` / `data: <JSON>` + 빈 줄. `data` 는 개행마다 `data:` 를 다시 붙인다 |
| 첫 바디 | `retry: 3000` — 끊김이 정상 경로이므로 재접속 간격을 명시한다 |
| keep-alive | 15s 유휴마다 `: keep-alive` 주석 줄 |
| 종료 | plugin 이 죽거나 `serve-stop` 하면 연결이 닫힌다. 소비자는 **재구독 전제**로 만든다 |

`event` 는 `text` / `thinking` / `tool_use` / `turn_end` 네 kind 와, 재개 시에만 나가는 `gap` 하나다. 네 kind 의 `data` JSON 은 **`poll` 응답의 이벤트 객체와 완전히 같은 스키마**다(`kind` · `seq` · `surface_id` · `session_id` · `timestamp` · `record_uuid` + 열린 턴 안이면 `request_id` + kind 별 필드). 두 채널이 같은 직렬화 함수를 쓰므로 소비자는 같은 파서를 사용하면 된다. `gap` 은 수집 이벤트가 아니라 **재전송 불가 구간 통지**이고 `data` 는 `{"kind":"gap","from":<seq>,"to":<seq>}` 다(아래 재개 절).

### 구독 파라미터

| 파라미터 | 기본 | 의미 |
|----------|------|------|
| `?surface=<id>` | 전체 | 그 surface 의 이벤트만 받는다 |
| `?thinking=1` | **꺼짐** | 사고 블록(`thinking`) 포함. 응답 텍스트와 민감도가 달라 기본은 제외한다 |
| `?after_seq=<n>` | 없음 | 재개 커서(아래) |
| `?token=<t>` | — | 구독 토큰(헤더 대신 쓸 때) |

### 인증

토큰이 설정돼 있으면 `Authorization: Bearer <t>` 또는 `?token=<t>`로 제시한다. 길이가 같을 때 모든 바이트를 비교하는 구현이지만 컴파일된 실행 시간이 일정하다고 보장하지는 않는다. 토큰이 없거나 일치하지 않으면 빈 바디의 `401`을 반환한다. 쿼리 파라미터는 사용자 지정 헤더를 넣을 수 없는 브라우저 `EventSource`를 위해 제공한다.

**bind 정책**: 기본 주소는 `127.0.0.1`이다. Loopback 밖의 주소에는 `--token`이 필수다. SSE 서버의 HTTP 계층에는 헤더 크기와 읽기 시간 제한이 없으므로 외부에 열 때는 앞단 프록시에서 연결 제한·타임아웃·TLS를 설정한다.

**포트 정책**: `--port`는 필수다. 요청한 주소에 bind하지 못하면 오류를 반환하며 다른 포트로 바꾸지 않는다.

### 재개 (`Last-Event-ID`)

SSE의 `id`는 수집 이벤트의 `seq`이며 `poll`의 `after_seq`와 같은 커서다. 재접속할 때 `Last-Event-ID: <seq>` 또는 `?after_seq=<seq>`를 주면 보관 중인 후속 이벤트를 재전송한다. 재시작 전후의 번호 재사용 가능성은 [재시작 복구](#재시작-복구--at-least-once)를 참고한다.

- 재전송 원본은 **수집 버퍼 그대로**(4096 개 상한)다. 별도 재개 버퍼를 두지 않는다 — 두 버퍼의 상한이 다르면 "`poll` 로는 보이는데 SSE 로는 안 보이는" 불일치가 생긴다.
- 커서를 주지 않은 구독은 **재전송 없이 지금부터** 흘린다.
- 커서 다음에 받을 이벤트가 버퍼에서 이미 밀려났다면 그 구간은 복구되지 않는다. `poll` 응답의 `dropped` 와 같은 한계다. 다만 **조용히 건너뛰지는 않는다** — 재전송에 앞서 `gap` 이벤트로 잃어버린 구간을 먼저 알린다.

  ```
  id: 0
  event: gap
  data: {"kind":"gap","from":1,"to":5}
  ```

  `id` 는 소비자가 보낸 커서 그대로다 — 갭 통지가 커서를 전진시키면 그 뒤 재연결에서 남은 이벤트까지 건너뛴다.
- `gap`은 다음에 받을 번호(`after_seq + 1`)가 첫 가용 번호(`first_available`)보다 작을 때만 보낸다.
  예를 들어 첫 보관 seq가 6이고 커서가 5이면 빠진 이벤트가 없으므로 gap 없이 6부터 재개한다.
  커서가 4이면 seq 5를 잃었으므로 gap의 from과 to가 모두 5다.
- 재시작하면 메모리 버퍼는 비어 있다. 이때는 저장된 마지막 seq + 1을 첫 가용 번호로
  사용한다. 마지막 seq까지 받은 소비자에게는 gap이 없다.
- tail이 저장 offset 이후 레코드를 다시 읽으면 새 seq로 방출될 수 있지만, 이미 저장 offset이
  지나간 레코드까지 자동 복구하지는 않는다. `gap`을 데이터 복구 완료로 취급하지 않는다.

### 백압 · 느린 구독자

구독자마다 256개 메시지를 담는 큐를 둔다. `try_send`는 큐가 가득 찼을 때 기다리지 않고 이벤트를 버리며, `serve-info`의 `dropped`를 증가시킨다. 다만 구독 허브의 Mutex 잠금은 기다릴 수 있으므로 수집 경로 전체가 블로킹되지 않는다는 보장은 아니다.

한 구독에서 연속 64회 전송이 거절되면 허브에서 구독을 제거한다. 소비자는 재접속해 `Last-Event-ID` 이후 이벤트를 요청할 수 있지만, 버퍼에서 이미 빠진 이벤트까지 복구되지는 않는다.

끊기는 것은 **허브의 구독 등록**이고, 소켓은 진행 중인 write 가 끝나거나 실패할 때 닫힌다. 즉 TCP 송신 버퍼까지 채운 소비자에게는 커널이 그 write 를 포기할 때까지 연결 스레드 1 개가 남을 수 있다.

### 재시작 후 자동 복구

`serve`의 bind·port·token 설정은 `<data_dir>/watches.json`의 watch 스냅샷에 저장한다. 재시작하면 저장한 주소에 다시 bind를 시도한다. 복원 bind가 실패해도 수집은 계속하며 `poll`로 읽을 수 있다.

`serve`와 `serve-stop`은 리스너를 변경한 뒤 메모리의 설정도 갱신한다. 레지스트리 잠금이 poison되면 복구해 갱신하지만, 파일 저장에 실패하면 경고만 남고 디스크에는 이전 스냅샷이 남을 수 있다. 이때 재시작 결과는 마지막 성공한 저장 내용에 따른다.

`serve`로 주소를 바꿀 때 실제 bind가 실패하면 이전 리스너는 이미 종료된 상태이며 메모리의 serve 설정도 지운다. 이 변경의 파일 저장까지 성공하면 재시작 때 이전 주소를 다시 열지 않는다. 저장이 실패하면 옛 설정이 남을 수 있다. 다시 열려면 `serve`를 명시적으로 호출한다(Agent Stream 설계).

인자 검증에서 거절된 요청은 다르다. --port 누락·범위 오류, IP로 해석할 수 없는 bind, 토큰 없는 loopback 밖 bind는 이전 서버를 종료하기 전에 거절한다. 이 경우 실행 중인 엔드포인트와 저장된 설정을 유지한다.

> **토큰은 그 스냅샷 파일에 평문으로 남는다.** 본체 웹훅 토큰과 같은 신뢰 수준·같은 저장 방식이다(설정 파일 평문). unix 에서는 스냅샷 파일을 `0600` 으로 만들어 같은 머신의 다른 사용자에게 열리지 않게 한다(Windows 는 파일 ACL 기본값을 따른다).

### SSE 시작 경로의 검증

실제 서버 시작은 `sse::server::start`에서 요청 주소로 `tiny_http::Server::http`를 호출하고,
bind 뒤의 스레드·상태 구성은 `start_bound`가 맡는다. `handle_serve_with`와
`restore_endpoint_with`는 private starter를 받아 같은 검증·영속화·복원 흐름을 실행하며,
제품 코드의 호출자는 항상 기존 `server::start`를 전달한다. 공개 IPC의 포트 필수·0 거부와
bind 실패 시 다른 주소로 폴백하지 않는 계약은 그대로다.

시험의 `sse::server::test_support::ReservedEndpoint`는 예약 리스너를 해제하지 않고
`tiny_http::Server::from_listener`로 소유권을 넘겨 같은 `start_bound`를 실행한다.
이 모듈은 `cfg(test)`에서만 포함된다. 설정 주소와 예약 주소를 대조하므로, 재시작 시험도
스냅샷의 주소를 다른 포트로 바꾸지 않는다. 예약한 포트를 해제했다가 다시 여는 과정은 없다. 전달 전·후의 경쟁 bind 거부와 전달된 리스너의 HTTP 응답을
`ownership_transfer_keeps_the_reserved_address_occupied`가 검사한다.

poison 이전 거부는 starter 호출 0회와 기존 엔드포인트·스냅샷 보존을 검사한다.
실제 bind 실패는 시험이 계속 점유한 loopback 리스너로 유발한다: `serve` 재호출은
엔드포인트·스냅샷을 비우고, 기동 복원 실패는 기존 저장 주소를 유지한 채 서버 없이
진행한다. 포트 소유권을 인계한 복원 시험은 실제 프로세스 재시작이 아니라 새 registry가
스냅샷을 읽는 경로의 시험이며, 제품의 복원 bind가 주소 경쟁 없이 성공함을 보장하는 시험은 아니다.

<a id="턴-correlation--웹훅-인바운드-배선"></a>

## 요청과 턴의 매칭 및 웹훅 연결

웹 애플리케이션이 웹훅으로 프롬프트를 보내고 SSE로 응답을 받는 구성을 지원한다. 두 연결은 별개이므로 요청자가 보낸 ID로 요청과 이벤트를 연결한다.

<a id="correlation-모델--요청자-제공-request_id"></a>

### 요청자가 제공하는 request_id

FE 가 요청마다 **자기가 만든 `request_id`** 를 웹훅 페이로드에 담아 보낸다. 그 값이 `turn_start` 로 전달돼 열린 턴에 저장되고, 그 턴이 만든 모든 SSE 이벤트에 `request_id` 로 실려 돌아온다. FE 는 그 값으로 응답이 어느 요청에 속하는지 찾는다.

현재 웹훅 응답은 실행 결과를 담지 않는 고정 ACK다. 별도 ID 전달 경로를 추가하지 않고
요청과 응답을 연결하기 위해 요청자가 만든 `request_id`를 필수로 받는다.

### 턴 경계

| 신호 | 출처 | 성격 |
|------|------|------|
| `turn_start` | 웹훅 IpcSequence 의 첫 스텝 | 턴 시작 — 이후 이벤트가 `request_id` 로 태깅된다 |
| `turn_end` 이벤트 | transcript `stop_reason`(정상 종료·`max_tokens`) · 취소 · API 오류, 또는 해제/세션 소멸/재-watch/타임아웃 | 턴 종료 — 태깅을 멈추고 턴을 닫는다 |

턴 종료는 transcript의 `stop_reason`을 사용하며 Claude 플러그인의 `claude-idle` 훅에는 의존하지 않는다. `stop_reason=tool_use`는 도구 실행 후 계속되는 중간 단계이므로 턴을 닫지 않는다. 출력 없는 도구 실행이 오래 걸리면 아래 비활동 timeout이 먼저 턴을 닫을 수 있다.

종료 이벤트를 만들 때 요청 턴이 열려 있으면 `push_event`가 그 `request_id`를 붙인다. 열린 턴과 이벤트 버퍼는 휘발성이며 구독 큐도 넘칠 수 있으므로, 소비자가 모든 요청의 종료를 반드시 받는다는 보장은 없다.

### 시퀀스 구조 — 호출 순서

웹훅에 거는 `IpcSequence` 는 두 스텝이다. `${body.*}` 는 **값 leaf 에만** 치환되고 method·객체 key 는 owner 가 고정한 리터럴이다(docs/features/webhook/index.md, `src/hook_handler/exec.rs` 의 `substitute_params`).

1. `agent_stream.turn_start` — `surface`(owner 고정 리터럴) + `request_id`(`${body.request_id}`)
2. `claude.tell` — `message`(`${body.prompt}`) + `surface`(owner 고정 리터럴)

`turn_start`가 먼저 성공해야 `claude.tell`로 생긴 이벤트에 올바른 요청 ID가 붙는다.
`execute_sequence`는 순서대로 호출하지만 각 step의 응답 대기는 10초다. 실패하거나 대기가
끝나도 다음 step을 실행하며, 이미 주입된 앞 요청이 뒤늦게 실행될 수 있다. 따라서 이 순서만으로
앞 step의 성공 완료까지 보장하지 않는다. 아래의 겹침 거부 한계도 함께 적용된다.

### 입력 검증 — 크기 상한 · 악의적 페이로드

웹훅은 외부 입력이다. `${body.request_id}` 는 발신자가 통제하는 값 leaf 라, `turn_start` 가 이를 받는 경계에서 다음을 강제한다.

| 벡터 | 처리 |
|------|------|
| method·객체 key 주입 | 불가능. `${body.*}` 는 **값 leaf 에만** 치환되고 method(`agent_stream.turn_start`)·key(`surface`/`request_id`)는 owner가 고정한 값이다(docs/features/webhook/index.md). 발신자는 어느 IPC 를 부를지도, 어느 surface 에 걸지도 못 정한다 |
| 빈/누락 `request_id` | **거부**(`missing_request_id`). 매칭이 성립할 값이 없다 |
| 거대 `request_id` (증폭) | **거부**(`request_id_too_long`, 512 바이트 상한). 상한이 없으면 거대한 값이 열린 턴에 저장돼 그 턴의 **모든** 이벤트(SSE·poll)에 복제된다 — 한 번의 큰 페이로드가 스트림 전체로 증폭되는 것을 저장 단계에서 막는다. 자르지 않고 거부해 잘린 id 가 매칭을 깨는 것도 피한다. 타입은 문자열/숫자만 받아 문자열로 정규화한다 |
| `timeout_secs` 극단값 | 범위로 **클램프**(10s~86400s). 0 이나 과대값으로 타임아웃 안전망을 무력화할 수 없다 |

> **웹훅 JSON 입력 body 상한은 본체 리스너가 적용한다.** 요청당 기본 1 MiB이며 `TASTY_WEBHOOK_MAX_BODY_BYTES`로 조정한다. 선언된 `Content-Length`가 상한을 넘거나 chunked body가 상한을 넘으면 `413 payload too large`로 거부하고 시퀀스를 실행하지 않는다. `request_id`의 512바이트 상한은 그 다음 단계에서 이벤트마다 복제되는 값을 제한한다.
>
> body 크기는 경로 매칭·인증보다 먼저 검사한다. 따라서 인증은 이 크기 제한을 대신하지 않으며, 토큰 없는 작은 요청은 `401 unauthorized`, 상한을 넘는 요청은 인증 전에 `413`을 받는다. 남용차단은 `401`·`413` 반복도 집계한다. 이 상한은 JSON 입력에 적용된다. 413 응답은 `Connection: close`를 싣고 잔여 body를 읽지 않은 채 연결을 닫는다([웹훅 (Inbound webhook listener)](https://github.com/zilhak/tasty/blob/develop/docs/features/webhook/index.md)). 입력 상한은 총 메모리의 보장이 아니다. 요청당 상한이 동시 요청 수나 연결 수를 제한하지는 않으므로, 외부 노출 시 프록시에서 연결 제한·타임아웃·TLS를 설정한다. 현재 보장은 [웹훅 body 상한](https://github.com/zilhak/tasty/blob/develop/docs/features/webhook/index.md#body-상한-요청당)에 있다. 본체와 같은 `tiny_http` 사본으로 빌드되지만 SSE 서버의 응답 경로는 상류 동작 그대로다.

### 정책 — 겹침 · 중복 · 막힌 턴 · 턴 밖 이벤트

| 상황 | 동작 |
|------|------|
| 같은 surface 에 턴이 열린 채 `turn_start` 재호출 | **거부**(`turn_already_open`). claude 는 한 번에 한 턴만 처리한다 — FE 는 앞 턴의 `turn_end`(같은 `request_id`)를 받은 뒤 다음을 보낸다. plugin 은 요청을 큐잉하지 않는다 |
| watch 중이 아닌 surface 에 `turn_start` | **거부**(`turn_not_watched`). 태깅할 이벤트가 애초에 나오지 않는다 — 먼저 `agent-stream watch` 한다 |
| 앞 턴이 닫힌 뒤 같은 `request_id` 재사용 | **허용**. 겹침이 아니라 새 턴이다(id 는 요청자 소유의 불투명 값) |
| `turn_start` 는 됐는데 `claude.tell` 이 실패해 턴이 안 닫힘 | **비활동 타임아웃**으로 정리. 그 턴이 자기 타임아웃(기본 600s, `--timeout-secs` 로 조정) 동안 이벤트가 하나도 없으면 `turn_end{reason=stream:turn_timeout}` 로 닫는다. 이벤트가 오면 대기 시간을 갱신한다. 다만 **아무 출력 없이 타임아웃보다 오래 도는 툴**은 조기 종료될 수 있어, 그런 배치는 `--timeout-secs` 를 올린다 |
| 턴 밖 이벤트(사용자가 터미널에서 직접 입력한 응답 등) | **태그 없이 방출**한다(버리지 않는다). `request_id` 필드가 빠진 채 나가므로 FE는 열린 요청 턴과 매칭되지 않은 이벤트로 처리한다 |

> **겹쳐 보낸 요청의 한계**: `execute_sequence`는 step 실패 뒤에도 다음 step을 실행한다. `turn_start`가 거부돼도 `claude.tell`이 프롬프트를 보낼 수 있다. 이때 이벤트에는 앞서 열린 턴의 `request_id`가 붙거나 ID가 생략된다. 요청자는 앞 턴의 종료를 확인한 뒤 다음 요청을 보내야 한다. 이 매칭 기능이 요청 실행을 직렬화해 주지는 않는다.

### 등록 예시

`Persistent` + `Unlimited`(FE 서버가 상시 호출) + **인증 토큰** 조합으로 등록한다. `--ttl-secs`와 `--count`를 생략하면 `Unlimited`다. `Persistent` 등록은 제한 종류와 무관하게 `~/.tasty/webhooks.toml`에 저장되며, 재시작 때 아직 만료되지 않은 등록만 복원된다.

```bash
# 대상 surface(예: 42)를 먼저 watch 한다 — turn_start 는 watch 중인 surface 만 받는다.
tasty agent-stream watch --surface 42

# 웹훅을 등록한다: turn_start → claude.tell 2-스텝 시퀀스, 토큰 인증, 영속.
tasty webhook register \
  --method POST \
  --persistent \
  --auth-location bearer --auth-token "$WEBHOOK_TOKEN" \
  --sequence '[
    {"method":"agent_stream.turn_start","params":{"surface":42,"request_id":"${body.request_id}"}},
    {"method":"claude.tell","params":{"message":"${body.prompt}","surface":42}}
  ]'
```

등록하면 `http://<host>:<port>/<16-hex>` 형태의 opaque URL이 반환된다. 반환된 URL을 `WEBHOOK_URL`에 담아 그대로 POST한다. 외부에서는 접속할 호스트·포트 또는 HTTPS 프록시 주소로 바꾸되, 발급된 경로를 유지한다. `/webhook/` 접두사는 붙이지 않는다.

```bash
curl -X POST "$WEBHOOK_URL" \
  -H "Authorization: Bearer $WEBHOOK_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"request_id":"req-8f3a","prompt":"summarize the last build log"}'
```

그러면 surface 42 의 claude 가 그 프롬프트를 받아 실행하고, 그 실행이 만든 SSE 이벤트에 `"request_id":"req-8f3a"` 가 실린다. 그 요청의 `turn_end` 도 같은 값으로 실려 종료를 알린다.

> **인증 토큰을 반드시 건다.** 이 구성은 외부 발신자가 claude 에게 임의 자연어를 주입하게 하고 claude 는 셸에 닿는다. docs/features/webhook/index.md 은 이 경우를 이미 다룬다 — owner 가 값 슬롯에 민감한 IPC 를 열면 그 트리거 책임은 owner 몫이고, **대응책은 인증으로 트리거 주체를 좁히는 것**이다. 무인증 구성을 예시로 쓰지 않는다. 토큰 없는/틀린 호출은 본체 웹훅 리스너가 `401 unauthorized`로 거부하고 시퀀스를 실행하지 않는다. 웹훅의 거부 바디는 고정 문자열이며, SSE 구독의 빈 `401` 바디와 다르다. body 상한 초과나 남용차단 중인 요청은 각각 `413`·`429`가 먼저 적용된다. 웹훅 토큰은 SSE 토큰과 같은 신뢰 수준·같은 저장(설정 파일 평문, unix `0600`)이다.

### FE 계약 요약

- 요청마다 `request_id` 를 만들어 페이로드에 담는다(필수).
- 앞 요청의 `turn_end{request_id=…}` 를 SSE 로 받은 **뒤에** 다음 요청을 보낸다(직렬화).
- `request_id`로 이벤트와 요청을 연결한다. 값이 없으면 열린 요청 턴과 매칭되지 않은 이벤트다.
- 끊기면 `Last-Event-ID` 로 재구독한다(SSE 절). `request_id` 는 재전송 프레임에도 그대로 실린다.

근거·대안·재검토 조건은 Agent Stream 설계, 웹훅 신뢰 모델은 [웹훅 (Inbound webhook listener)](https://github.com/zilhak/tasty/blob/develop/docs/features/webhook/index.md).

## 비-목표

- **claude-idle 훅 구독** — 턴 종료를 위해 claude plugin 의 hook 이벤트를 구독하지 않는다. transcript 가 이미 그 신호를 만들고, 훅 구독은 claude plugin 활성 의존을 새로 만든다([턴 correlation](#턴-correlation--웹훅-인바운드-배선)).
- **동시 다중 턴 · 큐잉** — 한 surface 에 턴이 겹쳐 들어오면 거부한다. claude 는 한 번에 한 턴만 처리하므로 correlation 도 한 번에 하나만 연다. 요청 큐잉은 이 plugin 이 하지 않는다.
- **웹훅 등록 자체** — 등록은 owner 운영 작업(`tasty webhook register`)이다. 이 plugin 은 turn correlation 을 제공하고, 연결 방법은 [아래 예시](#등록-예시)로 문서에 남긴다.
- **사용자 프롬프트 · 툴 결과 중계** — 에이전트가 낸 것만 이벤트로 만든다.
- **codex transcript** — 이름은 담고 있으나 현재 해석되는 소스는 Claude Code 하나다.
- **transcript 쓰기/변경** — 읽기 전용이다.

## Acceptance Criteria

- Given 에이전트가 도는 surface Then `agent-stream watch` 가 세션 id 를 해석하고 transcript 경로를 돌려준다.
- Given watch 중인 surface 에서 에이전트가 응답 Then 수 초 내에 그 텍스트가 `text` 이벤트로 `poll` 에 나타난다.
- Given 사고 블록이 있는 응답 Then `thinking` 과 `text` 가 서로 다른 kind 로 나온다.
- Given `claude-session-id` meta 가 없는 surface Then `watch` 가 명확한 에러로 거부된다(조용한 무동작 없음).
- Given 턴이 오류/취소/세션 종료로 끝남 Then 그에 맞는 `reason` 의 `turn_end` 가 나온다.
- Given plugin `disable && enable` Then 저장된 offset 에서 tail 이 재개된다(중복 허용).
- Given `serve` 로 연 엔드포인트 Then `curl -N` 이 연결을 유지한 채 응답 이벤트를 순차 출력한다.
- Given 토큰을 설정한 엔드포인트 Then 토큰 없는/틀린 구독이 401 로 거부되고 바디가 비어 있다.
- Given `?thinking=1` 유무 Then 사고 블록이 구독별로 포함/제외된다.
- Given 수집 버퍼 밖으로 밀려난 재개 커서 Then 재전송보다 **먼저** `gap` 이벤트가 나오고 그 `from`/`to` 가 잃어버린 구간을 가리킨다.
- Given 구독자 큐가 가득 참 Then 송신을 기다리지 않고 버리며 `serve-info`의 `dropped`가 증가한다. 연속 64회 거절되면 구독을 해제한다.
- Given 설정 저장과 bind에 성공한 엔드포인트에서 plugin `disable && enable` Then 연결이 끊긴 뒤 같은 주소로 다시 구독할 수 있다.
- Given `turn_start --request-id R` 뒤의 응답 Then 그 응답 이벤트와 `turn_end` 가 `request_id=R` 로 태깅돼 나온다.
- Given 턴 밖에서 나온 이벤트 Then `request_id` 필드 없이 방출된다(버려지지 않는다).
- Given 같은 surface 에 턴이 열린 채 `turn_start` 재호출 Then `turn_already_open` 으로 거부된다.
- Given watch 중이 아닌 surface 에 `turn_start` Then `turn_not_watched` 로 거부된다.
- Given `turn_start` 뒤 `claude.tell` 이 실패해 이벤트가 오지 않음 Then 타임아웃 후 `turn_end{reason=stream:turn_timeout}` 로 그 턴이 정리된다.
- Given 토큰 없는 웹훅 호출 Then 본체 리스너가 `401` 로 거부하고 claude 가 실행되지 않는다.

## 관련

- Agent Stream 설계 — transcript 수집, SSE 공개 범위와 전달 보장, 요청 ID와 턴의 매칭을 선택한 이유
- [웹훅 (Inbound webhook listener)](https://github.com/zilhak/tasty/blob/develop/docs/features/webhook/index.md) — 인바운드 웹훅 신뢰 모델(값/흐름 분리 · 단방향 ACK · 인증)
- [features/webhook](https://github.com/zilhak/tasty/blob/develop/docs/features/webhook/index.md) — 웹훅 lifetime · 인증 위치 · 남용차단 · 영속화
- [웹훅 (Inbound webhook listener)](https://github.com/zilhak/tasty/blob/develop/docs/features/webhook/index.md) — 같은 HTTP 레이어(`tiny_http`)를 고른 근거
- [claude](../claude/index.md) — `claude-session-id` surface meta 를 기록하는 쪽
- [dev-guide/plugin-development](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/plugin-development.md) — §9.1 반영 절차 · §10 한계
- [features/terminal-output](https://github.com/zilhak/tasty/blob/develop/docs/features/terminal-output/index.md) — 화면 기반 출력 구조화(다른 소스)
