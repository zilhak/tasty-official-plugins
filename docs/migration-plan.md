# 공식 플러그인 이전 기록

> **상태: 완료 기록.** 공식 플러그인 소스를 tasty 레포에서 이 레포로 옮긴 일이 어떻게 끝났는지 적는다. 기준 tasty 커밋은 `e277fce41d9a2db8bf3c14fda8f56dd6415bfcbc`(태그 없음)이고, 이 레포의 첫 릴리스는 `v0.1.0`이다.
>
> 지금의 운영 규칙(빌드, CI, 릴리스, 버전)은 이 문서가 아니라 [README.md](../README.md), [docs/ci.md](ci.md), [docs/branching.md](branching.md)가 정본이다. 파일 경로는 따로 적지 않으면 tasty 레포 루트 기준 상대 경로다.

## 옮긴 것과 남은 것

- 옮긴 크레이트 9개: `tasty-plugin-agent-common`(lib), `-agent-stream`, `-claude`, `-clipboard-viewer`, `-codex`, `-git-viewer`, `-html`, `-image`, `-markdown`. 이 레포 `crates/` 아래에 있고, 플러그인 문서는 `docs/plugins/<플러그인>/`에 함께 옮겼다.
- git 이력 없이 기준 커밋에서 단순 복사했고(D-3), 옮긴 모든 크레이트의 버전을 마이너 하나 올렸다(F-3). 매니페스트가 있는 플러그인은 크레이트 `Cargo.toml`과 `tasty-plugin.toml`의 `version`을 함께 올렸다.
- tasty에 남은 것: `tasty-plugin-mesh-demo`(SDK 예제, D-1), 계약과 SDK(`tasty-plugin-protocol`, `tasty-plugin-manifest`, `tasty-plugin-sdk`, 실험용 `tasty-plugin-sdk-wasm`), 플러그인이 쓰는 비플러그인 크레이트(`tasty-utils`, `tasty-i18n`, `tasty-ui-widgets`, `tasty-egui-theme`, `tasty-git-core` 등).
- 이 레포는 tasty에 남은 크레이트와 `tiny_http`·`epaint` 패치 사본을 루트 `Cargo.toml`의 같은 `rev = "<기준 커밋>"`으로 받는다. 처음 계획한 tasty 태그 대신 커밋 하나에 맞췄다.
- 이 레포 전용 크레이트를 더했다: `plugin-pin-check`(pin·lock 검사), `repo-gates`(버전 게이트 통합 시험), `plugin-guards`(tasty가 플러그인 크레이트에 돌리던 소스 가드). 모두 `publish = false`다.

## 진행 순서

처음 계획은 계약·신뢰 체계 결정(2단계)과 다운로드 설치(3단계)를 먼저 끝내고 소스를 옮기는(4단계) 순서였다. 실제로는 레포 분리를 앞당겼다.

1. 이 레포를 git 의존 빌드로 먼저 구성했다.
2. tasty가 보고한 필수 선행 작업만 하고 소스를 옮겼다. 최소 호스트 버전, 서명 범위와 키 같은 2단계 결정은 선행 조건으로 두지 않았고, 신뢰 체계는 바꾸지 않았다.
3. 이 레포가 `v*` 태그로 플러그인별 아카이브, SHA-256, `plugins-<platform>.tsv`, `release-meta.toml`을 자기 GitHub Release에 올린다(첫 릴리스 `v0.1.0`).
4. tasty 일반 패키지는 잠금 파일로 이 레포 Release 산출물을 고정해 받아 계속 함께 넣는다. 플러그인을 빼고 풀 패키지를 따로 만드는 계획은 이 방식으로 바뀌었다. 잠금 파일은 tasty에 있다.
5. 본체가 플러그인을 내려받아 설치하는 기능(옛 3단계)은 이번 범위 밖이다.

## 서명

A안으로 정했다. 이 레포의 산출물은 서명하지 않고 `.sig`도 만들지 않는다. 매니페스트 서명은 tasty가 패키징할 때 한다. 처음 계획한 빌드·서명 워크플로와 영구 키 재결정은 하지 않았다.

## 이 레포에서 처리한 계획 항목

- 플러그인 버전 게이트: tasty의 스크립트와 워크플로를 그대로 복사하지 않고 이 레포에 맞게 구현했다(`scripts/check-plugin-version-bump.sh`, `scripts/plugin-bump-fixup.sh`, `.github/workflows/plugin-version-check.yml`, `.githooks/`). tasty의 게이트는 `mesh-demo` 때문에 tasty에 남는다(F-2).
- lint: 이 레포의 `[workspace.lints]`를 그대로 두었다. 옮긴 크레이트가 이 lint로 clippy를 통과한다. tasty의 lint·`clippy.toml`과 복잡도 게이트는 옮기지 않았다.
- `tiny_http`·`epaint`: 계속 tasty git의 패치 사본을 받는다. 걷는 조건은 tasty의 `docs/dev-guide/dep-issues.md`에 있다.
- `ATTEMPT_MARKER_PREFIX` 대조 시험: 대조할 상수가 tasty protocol로 올라가서 이 레포에서 할 일이 없었다.
- `docs/plugins/claude/index.md`를 읽는 시험: 문서를 함께 옮겼다.
- 플러그인 `Cargo.toml`의 `repository`: 이 레포 주소로 바꿨다.
- 외부 크레이트 lock: `Cargo.lock`은 이 레포가 관리한다. 규칙은 [README.md](../README.md#cargolock)에 있다.

## tasty 쪽 변경

옮긴 크레이트를 tasty에서 지우는 일, 패키징과 CI가 이 레포 산출물을 받도록 바꾸는 일, 본체 시험·문서 가드·문서의 참조 정리는 tasty 레포가 맡는다. 관련 문서는 tasty의 `docs/dev-guide/plugin-packaging.md`, `docs/dev-guide/build.md`, `docs/dev-guide/release.md`다.

## 결정 기록

### 제품 결정 (2026-11-08, 사용자 결정)

| ID | 항목 | 결정 |
|---|---|---|
| D-1 | `tasty-plugin-mesh-demo` 처리 | 이 레포로 옮기지 않는다. tasty에 SDK 예제로 남긴다 |
| D-2 | 배포 묶음에 `bundle = false` 플러그인(`agent-stream`, `mesh-demo`)을 넣을지 | 지금 규칙대로 뺀다. 릴리스 골격(`include_unbundled` 기본값 `false`)은 그대로 둔다 |
| D-3 | 소스를 옮길 때 이력 보존 방식과 버전 | git 이력 없이 단순 복사한다. 옮길 때 각 플러그인 버전을 마이너 하나 올린다(예 `0.2.24` → `0.3.0`) — 범위는 F-3 |

### 후속 결정 (2026-11-08)

| ID | 항목 | 결정 |
|---|---|---|
| F-1 | 이 레포의 `mesh-demo` 빌드 | 지금 뺀다. git 의존, `plugin-pin-check`, `scripts/check.sh`, README, `docs/ci.md`에서 지웠다 |
| F-2 | `mesh-demo`가 tasty에 남을 때 tasty에 남는 장치 | 플러그인 버전 게이트, `BUILTINS`의 `mesh-demo` 항목, 빌드 스크립트의 `crates/tasty-plugin-*` 탐색은 tasty에 남는다. tasty 쪽 변경은 4단계에서 tasty 요청으로 보낸다 |
| F-3 | D-3 버전 올림 범위 | 옮기는 모든 크레이트에 적용한다. `tasty-plugin-agent-common`도 포함한다(`0.1.0` → `0.2.0`) |

## 결정 대기였던 항목

| ID | 항목 | 결론 |
|---|---|---|
| T-1 | `ATTEMPT_MARKER_PREFIX` 대조 시험 처리 | 상수를 tasty protocol로 올렸다. 이 레포에는 대조 시험이 없다 |
| T-2 | 이 레포에 tasty lint·복잡도 게이트를 옮길지 | 옮기지 않았다. 이 레포의 lint를 쓴다 |
| T-3 | `docs/plugins/` 문서를 이 레포로 옮길지 | 옮겼다(`docs/plugins/<플러그인>/`) |
| - | 서명 범위와 키 | A안: 이 레포는 서명하지 않고 tasty가 패키징할 때 매니페스트를 서명한다 |
| - | 풀 패키지 잠금 파일 | tasty 일반 패키지가 잠금 파일로 이 레포 산출물을 받아 넣는다. 잠금 파일은 tasty에 있다 |

### 열린 항목

- **egui 호환 확인**: 서로 다른 egui 버전으로 빌드한 본체와 플러그인을 실제로 연결해 보는 확인은 하지 않았다. egui-mesh 와이어(tasty `crates/tasty-plugin-protocol/src/mesh_wire.rs`)는 매직, 와이어 버전, 정점 크기, 알 수 없는 enum 태그만 거절하고, 크기가 같은 레이아웃·색 표현 변경은 잡지 못한다. 지금은 이 레포의 pin 시험이 egui 계열(`egui`, `epaint`, `emath`, `ecolor`, `bytemuck`)의 잠금 버전을 기준 커밋의 tasty `Cargo.lock`과 같게 묶는다. 버전 규칙을 계약에 넣을지는 정하지 않았다.
- **최소 호스트 버전**: 플러그인이 지원하는 본체 버전 범위를 매니페스트에 둘지 정하지 않았다. 다운로드 설치와 함께 다룬다.
- **protocol CHANGELOG의 "함께 재빌드" 전제**: tasty `crates/tasty-plugin-protocol/CHANGELOG.md`의 몇몇 항목은 플러그인이 본체와 함께 재빌드된다고 전제한다. 레포가 나뉘어 이 전제가 깨졌으므로 tasty 쪽에서 다시 검토한다.
