# 공식 플러그인 이전 계획 (초안)

> **상태: 초안.** 이 문서의 조사 내용은 모두 tasty 커밋 `89efb560203f7041959515ebe17dc7f3de2bbcd2`(이하 tasty@89efb560) 기준이다. tasty 최신 main과 다를 수 있다. 이 커밋은 릴리스 태그가 아니다. 릴리스 태그 기준이 정해지면 그 태그로 다시 확인한다.
>
> 파일 경로는 따로 적지 않으면 tasty 레포 루트 기준 상대 경로다.

## 배경과 진행 단계

공식 플러그인 소스를 tasty 레포에서 이 레포로 옮긴다. 이전은 다음 순서로 진행한다. 이 문서에서 "N단계"는 이 순서를 가리킨다.

| 단계 | 내용 |
|---|---|
| 1단계 | 이 레포를 미리 구성한다. 플러그인 크레이트는 tasty를 git 의존으로 특정 커밋에 고정해 받아 빌드하고, 소스 사본은 두지 않는다. 정본은 이전 전까지 tasty에 있다 |
| 2단계 | 플러그인 계약과 신뢰 체계를 정한다(최소 호스트 버전, 서명 범위, 키) |
| 3단계 | 본체가 플러그인을 내려받아 설치할 수 있게 한다 |
| 4단계 | 플러그인 소스를 이 레포로 옮기고 tasty에서 지운다(이 문서의 대상) |
| 5단계 | 일반 패키지에서 플러그인을 빼고, 플러그인을 함께 넣은 풀 패키지를 따로 만든다 |

이미 정한 것:

- 플러그인 protocol, manifest, SDK는 tasty에 남는다. 이 레포는 tasty 태그를 가리키는 git 의존으로 가져온다. 레포 전체를 태그 하나로 맞춘다.
- `tasty-plugin-agent-common`은 본체가 제품 코드에서 쓰지 않으므로 이 레포로 옮긴다.
- 풀 패키지는 tasty Releases에 올린다. 잠금 파일에 플러그인 버전과 플랫폼별 SHA-256을 적고, 플러그인을 다시 빌드하지 않고 받아서 넣는다.

## 1. 옮길 크레이트

### 1.1 tasty의 `tasty-plugin-*` 크레이트 (14개)

`git ls-tree --name-only 89efb560203f7041959515ebe17dc7f3de2bbcd2 crates/ | grep tasty-plugin-`의 결과 전부다.

| 크레이트 | 종류 | 매니페스트 id | `bundle` | 성격 | 이전 |
|---|---|---|---|---|---|
| `tasty-plugin-agent-common` | lib | 없음 | - | claude·codex 공용 도우미 | 옮김(확정). 2절 참고 |
| `tasty-plugin-agent-stream` | bin | `com.tasty.agent-stream` | `false` | 기능 플러그인(에이전트 기록 중계, SSE) | 옮김 |
| `tasty-plugin-claude` | bin | `com.tasty.claude` | 기본값 | 배포 | 옮김 |
| `tasty-plugin-clipboard-viewer` | bin | `com.tasty.clipboard-viewer` | 기본값 | 배포 | 옮김 |
| `tasty-plugin-codex` | bin | `com.tasty.codex` | 기본값 | 배포 | 옮김 |
| `tasty-plugin-git-viewer` | bin | `com.tasty.git-viewer` | 기본값 | 배포 | 옮김 |
| `tasty-plugin-html` | bin | `com.tasty.html` | 기본값 | 배포 | 옮김 |
| `tasty-plugin-image` | bin | `com.tasty.image` | 기본값 | 배포 | 옮김 |
| `tasty-plugin-markdown` | bin | `com.tasty.markdown` | 기본값 | 배포 | 옮김 |
| `tasty-plugin-mesh-demo` | bin | `com.tasty.mesh-demo` | `false` | **데모용.** 매니페스트 설명 "egui-mesh channel proof-of-concept surface (A1)" | 남김(SDK 예제, D-1) |
| `tasty-plugin-protocol` | lib | - | - | 계약 | tasty에 남음(확정) |
| `tasty-plugin-manifest` | lib | - | - | 계약 | tasty에 남음(확정) |
| `tasty-plugin-sdk` | lib | - | - | SDK | tasty에 남음(확정) |
| `tasty-plugin-sdk-wasm` | lib (POC) | - | - | 실험용 WASM SDK. 루트 workspace의 `exclude`에 있고 자체 `Cargo.lock`을 가짐 | tasty에 남음(SDK) |

옮기는 크레이트는 9개다. `mesh-demo`는 tasty에 SDK 예제로 남는다(D-1).

- `bundle`이 "기본값"인 플러그인은 매니페스트에 `bundle` 키가 없다.
- `crates/tasty-host-plugin/src/builtin.rs`의 `BUILTINS`에는 bin 9개(`mesh-demo`, `agent-stream` 포함)가 모두 있다. Windows와 그 밖의 플랫폼용 목록이 `#[cfg]`로 나뉘어 있지만 9개는 같다.
- 배포 스크립트는 `bundle = false`인 2개를 빼고 7개를 넣는다. 루트 `Cargo.toml`의 deb·rpm 자산 목록도 7개다. `bundle = false` 플러그인은 지금 규칙대로 배포 묶음에서 뺀다(D-2).

### 1.2 tasty에 남는 크레이트와 이유

- `tasty-plugin-protocol`, `tasty-plugin-manifest`: 본체와 플러그인 사이의 계약이다. 본체가 직접 쓰므로 본체와 같은 레포에서 버전을 맞춘다.
- `tasty-plugin-sdk`: 계약을 감싸는 SDK다. 계약과 함께 바뀌므로 tasty에 둔다. 서드파티 플러그인도 이것을 쓴다.
- `tasty-plugin-sdk-wasm`: 실험 SDK다. 위와 같은 이유로 tasty에 둔다.

### 1.3 플러그인이 쓰는 tasty 비플러그인 크레이트

아래 크레이트는 이전 뒤에도 tasty에 남는다. 이 레포는 계속 git 의존으로 받는다.

| 크레이트 | 쓰는 곳 |
|---|---|
| `tasty-utils` | agent-stream, claude, codex, git-viewer, sdk |
| `tasty-shm` | sdk |
| `tasty-i18n` | sdk, egui-theme |
| `tasty-ui-widgets` | clipboard-viewer, git-viewer, image, markdown, mesh-demo, sdk(`egui-mesh` feature) |
| `tasty-egui-theme` | clipboard-viewer, git-viewer, image, markdown, ui-widgets |
| `tasty-type-appearance` | clipboard-viewer, git-viewer, image, markdown, mesh-demo, protocol, ui-widgets, egui-theme |
| `tasty-type-geometry` | 전이 의존(ui-widgets, egui-theme, type-appearance) |
| `tasty-icons` | clipboard-viewer·image·markdown의 build-dependency, markdown의 dependency, ui-widgets |
| `tasty-git-core` | git-viewer. 본체 원격 attach와 함께 쓴다 |
| `tasty-reaper` | codex |

dev-dependency로만 쓰는 것:

| 크레이트 | 쓰는 곳 |
|---|---|
| `tasty-test-support` | claude, codex |
| `tasty-themes` | markdown, sdk, ui-widgets, egui-theme |
| `tasty-plugin-manifest` | clipboard-viewer, html, markdown, mesh-demo |
| `tasty-design-tokens` | ui-widgets |
| `tasty-type-appearance` | sdk |

지금 이 레포에서는 플러그인이 git 의존이라 workspace 멤버가 아니다. 그래서 플러그인의 dev-dependency는 해석되지 않고 플러그인 시험도 이 레포에서 돌지 않는다. 4단계 뒤에는 플러그인이 멤버가 되므로 `tasty-test-support` 등을 git dev-dependency로 받아야 한다.

### 1.4 Cargo 의존 목록에 드러나지 않는 빌드 입력

아래 크레이트는 크레이트 폴더 밖의 tasty 파일을 컴파일 시간에 읽는다. 지금은 cargo가 git 의존을 받을 때 레포 전체를 checkout하므로 빌드된다. **이 크레이트들은 레포 전체 checkout을 전제로 하므로, crates.io에 게시하거나 크레이트 단위로 떼어 내면 빌드가 깨진다.**

- `tasty-i18n`: `crates/tasty-i18n/build.rs`가 `../../lang`(tasty 루트 `lang/`)을 읽고, `crates/tasty-i18n/src/lib.rs:53`이 `include_str!(concat!("../../../lang/", $code, ".toml"))`로 루트 `lang/*.toml`을 포함한다.
- `tasty-egui-theme`: `crates/tasty-egui-theme/src/lib.rs:328`이 `include_bytes!("../../tasty-font/assets/D2Coding-ligature-Regular.ttf")`로 다른 크레이트의 글꼴 파일을 포함한다.
- `tasty-ui-widgets`: `crates/tasty-ui-widgets/src/brand.rs:13`이 `include_bytes!("../../../assets/icons/icon_256.png")`로 루트 `assets/`를 포함한다.

### 1.5 옮길 플러그인이 크레이트 밖 tasty 파일을 읽는 곳

- `crates/tasty-plugin-claude/src/hook.rs:2984`의 시험이 `../../docs/plugins/claude/index.md`(tasty의 `docs/plugins/claude/index.md`)를 읽는다. 같은 파일 886행의 문서 주석도 이 문서를 가리킨다. 이전 때 그 문서를 함께 옮기거나 시험이 읽는 경로를 바꿔야 한다.
- 플러그인 `build.rs` 3개(`clipboard-viewer`, `image`, `markdown`)는 `OUT_DIR`만 쓰고 크레이트 밖을 읽지 않는다.
- 이전 직전에 다시 확인한다: `git grep -n -E '"\.\./\.\./' <sha> -- 'crates/tasty-plugin-*'`. `checklist.rs`, `gate.rs`, `ids.rs`의 `../../../outside` 같은 값은 경로 탈출 시험의 입력이라 해당하지 않는다.

## 2. `tasty-plugin-agent-common` 이전

확정 결정은 "본체가 직접 쓰지 않으니 옮긴다"이다. 정규 의존(`[dependencies]`)으로 쓰는 곳은 `tasty-plugin-claude`, `tasty-plugin-codex` 둘뿐이므로 제품 코드 기준으로는 맞다. 다만 tasty@89efb560에는 본체 쪽 참조가 두 곳 있다.

- `crates/tasty-task-runtime/Cargo.toml`의 `[dev-dependencies]`에 `tasty-plugin-agent-common = { path = "../tasty-plugin-agent-common" }`가 있다. 시험 `crates/tasty-task-runtime/src/runner_host/agent_tests.rs`의 `the_plugins_read_the_marker_the_host_writes`가 `tasty_plugin_agent_common::task_turn::ATTEMPT_MARKER_PREFIX`와 `tasty_agent::task::agent::ATTEMPT_MARKER_PREFIX`가 같은지 확인한다. 값은 둘 다 `"[tasty-task-attempt:"`이고, 두 크레이트가 같은 문자열을 따로 갖고 있다.
- `crates/tasty-agent/src/task/agent.rs:137`의 문서 주석이 `tasty_plugin_agent_common::task_turn::ATTEMPT_MARKER_PREFIX`를 가리킨다.

크레이트 구성: `src/` 아래 `children`, `host_call`, `params`, `prompt_file`, `reboot`, `remote_attach`, `remote_owner`, `remote_results`, `task_turn` 모듈. 의존은 `tasty-plugin-sdk` 하나다.

이전할 때 대조 시험을 처리하는 선택지(tasty 쪽 결정이므로 여기서는 정하지 않는다):

1. **시험을 이 레포로 옮긴다.** 이 레포가 `tasty-agent`를 git dev-dependency로 받아 두 상수를 대조한다. tasty에서는 dev-dependency와 시험을 지운다. 이 레포가 본체 내부 크레이트에 기대게 된다.
2. **표지 문자열을 계약 크레이트로 올린다.** `ATTEMPT_MARKER_PREFIX`를 `tasty-plugin-protocol` 같은 계약 크레이트에 두고 본체와 플러그인이 함께 쓴다. 대조 시험이 필요 없어진다. tasty의 계약 변경이므로 tasty에서 먼저 바꾼다.
3. **시험을 지우고 문서로만 맞춘다.** 문자열이 어긋나도 잡을 수단이 없어진다.

어느 쪽이든 `crates/tasty-agent/src/task/agent.rs:137` 주석은 새 위치를 가리키도록 고친다.

## 3. 크레이트별 egui 버전과 호환

### 3.1 버전 표

| 크레이트 | 요구 버전 | egui-mesh 사용 | 비고 |
|---|---|---|---|
| tasty 본체(루트 패키지) | `0.31` | 호스트 | `gui` feature의 optional 의존. `egui-wgpu`, `egui-winit`, `egui_extras`도 `0.31` |
| `tasty-plugin-protocol` | `0.31` | 와이어 정의 | optional(`egui-mesh` feature), dev `0.31` |
| `tasty-plugin-sdk` | `0.31` | 플러그인 쪽 | optional(`egui-mesh` feature) |
| `tasty-ui-widgets` | `0.31` | - | 비플러그인. `egui_extras` `0.31` |
| `tasty-egui-theme` | `0.31` | - | 비플러그인 |
| `tasty-type-appearance` | `0.31` | - | 비플러그인. optional(`egui-compat` feature). egui를 쓰는 플러그인 5개와 ui-widgets·egui-theme이 이 feature를 켠다 |
| `tasty-plugin-clipboard-viewer` | `0.31` | 사용 | `default-features = false`, `bytemuck`, `default_fonts` |
| `tasty-plugin-git-viewer` | `0.31` | 사용 | 같음 |
| `tasty-plugin-image` | `0.31` | 사용 | 같음 |
| `tasty-plugin-markdown` | `0.31` | 사용 | 같음 |
| `tasty-plugin-mesh-demo` | `0.31` | 사용 | 같음 |
| `tasty-plugin-agent-common`, `-agent-stream`, `-claude`, `-codex`, `-html` | 쓰지 않음 | - | - |

잠금 버전: tasty `Cargo.lock`의 `egui`와 `epaint`는 `0.31.1`이다. 이 레포에서 같은 커밋을 git 의존으로 받아 잠금을 풀어도 `0.31.1`로 풀린다. **지금은 본체와 플러그인의 egui 버전이 모두 같다.**

### 3.2 버전이 달라질 때 검사되는 것과 검사되지 않는 것

레포를 나누면 본체와 플러그인을 따로 빌드한다. 그러면 egui 잠금 버전이 서로 달라질 수 있다. egui를 쓰는 플러그인은 egui-mesh 채널로 paint 출력을 바이트로 보내고, 본체가 자신의 egui 타입으로 복원한다. 와이어 형식은 `crates/tasty-plugin-protocol/src/mesh_wire.rs`에 있다.

**검사되는 것** (`decode_paint`가 헤더에서 확인하고 거절한다):

- 매직 `MAGIC = b"TMSH"`
- 와이어 버전 `WIRE_VERSION = 1`. 와이어 형식을 바꿀 때 사람이 올리는 값이다. egui 버전과 묶여 있지 않다.
- `size_of::<epaint::Vertex>()`(정점 크기). 다르면 `VertexStrideMismatch`로 거절한다.
- 알 수 없는 enum 태그(`TextureId`, `ImageData`, `TextureFilter`, `TextureWrapMode`, `ImageDelta.pos`, mipmap)는 `BadTag`로 거절한다.

**검사되지 않는 것** (`mesh_wire.rs` 첫머리 주석도 "크기를 유지한 모든 레이아웃 변경까지 검출하는 것은 아니다"라고 적는다):

- `epaint::Vertex`는 `#[repr(C)]` POD 바이트를 `bytemuck`으로 그대로 복사한다. 크기가 같고 필드 순서·의미(`pos`, `uv`, `color`)나 색 표현(premultiplied 여부 등)이 바뀌면 걸리지 않는다.
- `Color32`(`ImageData::Color`의 픽셀)도 `[u8; 4]` 바이트 그대로 복사한다. 색 공간이나 premultiply 규칙이 바뀌어도 걸리지 않는다.
- `FontImage` 픽셀(`f32` coverage)도 바이트 그대로 복사한다. 글꼴 아틀라스 표현이 바뀌면(예: 상류에서 FontImage 형식을 바꾸는 경우) 의미가 어긋난다.
- `TextureId::Managed(u64)`/`User(u64)`의 값 의미. 특히 `Managed(0)`을 글꼴 아틀라스로 쓰는 관례 같은 것은 검사하지 않는다.
- `TextureOptions`: 와이어에 쓰는 필드는 `magnification`, `minification`, `wrap_mode`, `mipmap_mode` 넷이다. 디코더는 이 네 필드로 구조체 리터럴을 만든다. egui가 필드를 더하거나 열거형에 변형을 늘리면 같은 egui로 빌드할 때는 컴파일 오류로 드러난다. 그러나 본체와 플러그인이 다른 egui로 빌드되면 상대 쪽이 아는 필드·변형은 와이어에 실리지 않거나 `BadTag`가 될 뿐, 버전 차이 자체는 드러나지 않는다.
- `ClippedPrimitive`, `Mesh`, `Rect`, `TexturesDelta`, `ImageDelta`의 구조 변경. 인코더와 디코더가 각자의 egui로 컴파일되므로, 한쪽만 바뀐 경우는 와이어 버전이 같은 한 드러나지 않는다.
- `Primitive::Callback`은 인코드할 때 경고 뒤 버린다(버전과 무관).

**등록 게이트**: `src/runtime/surface_registry/egui_mesh.rs`는 egui-mesh surface kind를 등록할 때 매니페스트 `api_version`이 `HOST_API_VERSION`(`crates/tasty-plugin-manifest/src/types.rs:13`, 현재 `"1"`)과 같은지만 본다. egui 버전은 보지 않는다.

**깨지는 전제**: `crates/tasty-plugin-protocol/CHANGELOG.md`는 몇몇 변경에 "in-tree egui-mesh whitelist plugin 은 본체와 함께 재빌드되므로 실사용 영향 없음"이라고 적는다. 레포를 나누면 플러그인과 본체를 따로 빌드하므로 이 전제가 깨진다. 이전 전에 이 CHANGELOG의 해당 항목들을 다시 검토한다.

### 3.3 호환 확인 (결정 대기)

- 서로 다른 egui 버전으로 빌드한 본체와 플러그인을 실제로 연결해 보는 확인은 아직 하지 않았다.
- 이 확인을 언제·어디서 할지, 2단계 계약에 egui 버전 규칙을 넣을지는 **2단계 결정 대기**(6절)다. 생각할 수 있는 규칙:
  - 매니페스트나 와이어 헤더에 egui(epaint) 메이저·마이너 버전을 적고 다르면 거절한다.
  - 레포 전체를 tasty 태그 하나에 맞추는 확정 결정을 이용해, 이 레포의 egui 잠금 버전이 그 태그의 tasty 잠금 버전과 같은지 CI에서 확인한다.

## 4. 이전 절차 (git 의존 → 로컬 소스, tasty에서 삭제)

### 4.1 선행 조건

- 2단계(계약·신뢰 체계)가 정해져 있다. 최소 호스트 버전, 서명 범위와 키가 정해져야 이 레포가 tasty와 따로 플러그인을 배포할 수 있다.
- 이 레포가 따를 tasty 릴리스 태그가 있다.
- 3절의 egui 버전 규칙이 정해져 있다.
- 제품 결정 D-1(mesh-demo 처리), D-3(이력 보존 방식)은 2026-11-08에 정했다("결정 기록" 절).

### 4.2 이 레포에서

1. 플러그인 크레이트(1.1절의 "옮김" 9개)를 이 레포 `crates/` 아래로 단순 복사한다. git 이력은 가져오지 않는다(D-3). 가져온 커밋 메시지 본문에 출처 tasty 커밋을 적는다.
2. 옮기는 모든 크레이트의 버전을 현재 버전에서 마이너 버전 하나 올린다(예 `0.2.24` → `0.3.0`). lib 크레이트 `tasty-plugin-agent-common`도 포함한다(`0.1.0` → `0.2.0`). 크레이트 `Cargo.toml`의 `version`을 올리고, 매니페스트가 있는 플러그인은 매니페스트의 `version`도 함께 올린다(D-3, F-3).
3. 가져온 크레이트를 workspace 멤버로 만들고, 해당 크레이트의 git 의존을 path 의존으로 바꾼다.
4. tasty에 남는 크레이트(1.2·1.3절)는 git 의존을 유지하고 `rev`를 tasty 릴리스 태그(`tag = "..."`)로 바꾼다. 레포 전체를 태그 하나에 맞춘다.
5. 가져온 크레이트의 path 의존(`path = "../tasty-plugin-sdk"` 등)을 workspace 의존으로 정리한다. `tasty-test-support`, `tasty-themes`, `tasty-plugin-manifest`, `tasty-design-tokens` 같은 dev-dependency도 git 의존으로 바꾼다.
6. lint 설정을 옮긴다. 가져온 크레이트의 `[lints] workspace = true`는 이 레포의 `[workspace.lints]`로 해석된다. tasty의 규칙을 옮길지 정하고 옮긴다.
   - tasty 루트 `Cargo.toml`의 `[workspace.lints.clippy]`: `undocumented_unsafe_blocks`, `cognitive_complexity`, `multiple_unsafe_ops_per_block`, `result_large_err`(allow), `let_underscore_must_use`, `disallowed_methods`, `missing_safety_doc`. `[workspace.lints.rust]`도 함께 본다.
   - tasty `clippy.toml`: `cognitive-complexity-threshold = 20`, 그리고 `disallowed_methods` lint가 쓰는 `disallowed-methods` 목록(`HexColor::from_rgb`, `Color32::from_rgb` 등). `disallowed_methods`는 이 목록에 기대므로 `clippy.toml`을 함께 옮긴다.
   - tasty의 복잡도 게이트 파일(`.complexity-file-allowlist`, `.complexity-frozen-files`)에 플러그인 파일 항목이 있다. 이 레포에 같은 게이트를 둘지 정한다.
7. 플러그인 `Cargo.toml`의 `repository = "https://github.com/zilhak/tasty"`를 이 레포 주소로 바꾼다.
8. 플러그인 버전 게이트를 이 레포에도 둔다. tasty의 `scripts/check-plugin-version-bump.sh`, `scripts/plugin-bump-fixup.sh`, `scripts/lib/plugin-bump-mode.sh`, `.github/workflows/plugin-version-check.yml`(매니페스트 version을 올렸는지 확인하는 게이트)와 그 시험(`tests/plugin_bump_fixup.rs`, `tests/plugin_version_bump_channel.rs`)을 이 레포에 맞게 복사한다. tasty의 원본은 tasty에 남는다. `mesh-demo`가 매니페스트 있는 플러그인으로 tasty에 남기 때문이다(F-2).
9. `[patch.crates-io]`의 `tiny_http`를 정리한다. tasty는 `tiny_http = { path = "vendor/tiny_http" }`로 패치한 사본을 쓰고(`vendor/tiny_http/PATCHES.md`), `agent-stream`이 `tiny_http`를 쓴다. `[patch]`는 소비하는 쪽 루트 workspace의 것만 적용되므로 이 레포가 따로 정해야 한다. 1단계 구성에서 이 레포가 어떻게 받기로 했는지(tasty git의 사본으로 돌리는지, crates.io 상류를 쓰는지)는 이 레포의 루트 `Cargo.toml`과 `README.md`를 따른다. 4단계에서는 같은 방식을 유지하거나, 패치 사본을 이 레포 `vendor/`로 가져온다. `epaint`도 같다. tasty는 `vendor/epaint`로 패치한 사본을 쓰고, 이 레포는 tasty git의 사본을 받는다. 걷는 조건은 tasty의 `docs/dev-guide/dep-issues.md`에 있다.
10. 1.5절의 `docs/plugins/claude/index.md` 참조를 처리한다(문서를 함께 옮기거나 시험 경로를 바꾼다).
11. 2절의 `ATTEMPT_MARKER_PREFIX` 대조 시험을 정한 선택지대로 처리한다.
12. 플러그인 매니페스트, `lang/`, 서명 파일(`.sig`)을 이 레포의 릴리스 산출물로 만드는 빌드·서명 워크플로를 실제로 동작하게 한다(1단계에서는 골격만 있다).
13. 이 레포에서 첫 플러그인 릴리스 산출물을 만든다.

### 4.3 tasty에서 지우는 시점

tasty에서 플러그인 소스를 지우는 것은 다음이 모두 끝난 뒤다.

1. 이 레포의 첫 플러그인 릴리스 산출물이 있다(4.2절 13).
2. tasty가 그 산출물을 받아 설치할 수 있다(3단계 다운로드 설치).
3. tasty 패키징이 플러그인을 빌드하지 않고 받아 넣을 수 있다(5단계 풀 패키지 잠금 파일. 플러그인 버전과 플랫폼별 SHA-256).
4. 5절의 tasty 쪽 변경이 같은 변경 묶음으로 준비되어 있다. 플러그인 크레이트를 지우면 tasty 본체 시험과 문서 가드 일부가 바로 깨지므로 삭제와 함께 고친다.

지우는 순서:

1. tasty에서 5절의 본체 코드·시험 참조를 먼저 고친다(플러그인 소스가 없어도 통과하도록).
2. 패키징과 CI가 이 레포의 산출물을 받도록 바꾼다.
3. `crates/tasty-plugin-*` 중 옮긴 크레이트를 지우고 루트 `Cargo.toml`, `Cargo.lock`을 정리한다.
4. 문서를 고친다.
5. 이 레포 쪽 git 의존의 태그를 이 변경이 들어간 tasty 태그로 올린다.

## 5. tasty 쪽에서 바뀌어야 할 것

다음 명령의 결과를 분류한 목록이다(tasty@89efb560).

```bash
git grep -l -E 'tasty[-_]plugin[-_](agent[-_]common|agent[-_]stream|claude|clipboard[-_]viewer|codex|git[-_]viewer|html|image|markdown|mesh[-_]demo)' \
  89efb560203f7041959515ebe17dc7f3de2bbcd2 -- . ':!crates/tasty-plugin-*'
```

이 명령은 크레이트 이름을 직접 쓰는 곳만 찾는다. `crates/tasty-plugin-*` 같은 glob이나 변수로 플러그인 폴더를 찾는 스크립트는 잡지 못하므로 보조 검색 결과(5.5절)를 함께 본다.

### 5.1 패키징

- `Cargo.toml`: `[package.metadata.deb]`와 `[package.metadata.generate-rpm]`의 assets(플러그인 7개의 바이너리, 매니페스트, `.sig`, `lang/` 경로), `[profile.dev.package.tasty-plugin-*]` 항목들, workspace 멤버
- `Cargo.lock`, `THIRD_PARTY_LICENSES.md`, `deny.toml`(git2 예외 주석)
- `Justfile`: `build-plugins`, `build-plugin`, `link-plugins`, `build-all`
- `scripts/build-linux.sh`, `scripts/build-macos-dmg.sh`, `scripts/build-windows.ps1`: `crates/tasty-plugin-*` 탐색, 빌드, `stage_plugins` — `crates/tasty-plugin-*` 탐색은 tasty에 남는다(`mesh-demo`, F-2). 옮긴 플러그인의 빌드와 `stage_plugins` 대상만 바뀐다.
- `scripts/sign-bundle.sh`(`--all-builtins`가 crates 아래 매니페스트를 찾음), `scripts/ensure-sign-key.sh`
- `wix/main.wxs`: MSI 플러그인 파일 목록
- `crates/tasty-host-plugin/src/builtin.rs`: `BUILTINS`의 `crate_dir`(dev workspace에서 매니페스트와 `lang/` 원본을 찾는 위치) — `BUILTINS`의 `mesh-demo` 항목은 tasty에 남는다(F-2). 옮긴 플러그인 항목만 바뀐다.
- `.gitignore`: `crates/tasty-plugin-*/tasty-plugin.toml.sig`

### 5.2 CI와 게이트

- `.github/workflows/crossplatform-check.yml`: `cargo test -p tasty-plugin-markdown ... webview_layout_tests`
- `.github/workflows/plugin-version-check.yml`, `scripts/check-plugin-version-bump.sh`, `scripts/plugin-bump-fixup.sh`, `scripts/lib/plugin-bump-mode.sh`, 시험 `tests/plugin_bump_fixup.rs`, `tests/plugin_version_bump_channel.rs` — tasty에 남는다(`mesh-demo`, F-2). 대상에서 옮긴 플러그인만 빠진다.
- `.github/workflows/release.yml`, `build-check.yml`(빌드 스크립트를 거쳐 플러그인을 빌드)
- `.githooks/pre-commit`(플러그인 매니페스트 경로 패턴, `check-plugin-version-bump.sh` 호출), `.githooks/pre-push`, `scripts/tests/pre-push-git-environment.sh`
- `.complexity-file-allowlist`, `.complexity-frozen-files`

### 5.3 본체 코드와 시험

- `crates/tasty-task-runtime/Cargo.toml`, `crates/tasty-task-runtime/src/runner_host/agent_tests.rs`: agent-common dev-dependency와 대조 시험(2절)
- `crates/tasty-agent/src/task/agent.rs`: 문서 주석(2절)
- `crates/tasty-test-support/src/lib.rs`, `crates/tasty-test-support/Cargo.toml`: `bundled_plugin_rules`가 claude·codex 매니페스트를 읽음
- `crates/tasty-git-core/Cargo.toml`: git-viewer와 함께 쓴다는 설명
- `crates/tasty-host-plugin/src/builtin.rs`(5.1절)
- `src/completion_strategy/config.rs`, `src/completion_strategy/registry_tests.rs`: 시험이 claude·codex 매니페스트를 읽음
- `src/host_api/webview.rs`: 시험이 `../../crates/tasty-plugin-html/tasty-plugin.toml`과 markdown 매니페스트를 읽음
- `src/source_guards/read_only_handle_mtime.rs`, `src/source_guards/reserved_ipc_prefixes.rs`
- `src/view/plugins/ui/add/folder_tests.rs`
- `tests/spawn_diag/mod.rs`
- `crates/tasty-cli/src/caller_context/tests.rs`, `crates/tasty-cli/src/cwd_resolve.rs`, `crates/tasty-cli/src/dynamic/tests.rs`
- `crates/tasty-gallery/src/catalog/components/git_viewer.rs`, `crates/tasty-gallery/src/catalog/components/markdown_viewer/css_path.rs`, `crates/tasty-gallery/src/catalog/components/md_address.rs`: 플러그인 소스·문구 경로 참조
- `crates/tasty-doc-guards/src/workflow_triggers.rs`
- `crates/tasty-doc-guards/tests/` 시험들: `ci_channel_claims_match_workflows.rs`, `gate_thresholds_are_not_stale_copies.rs`, `no_hardcoded_ui_strings.rs`, `no_todo_file_citation.rs`, `one_concept_one_word_on_the_user_facing_surface.rs`, `plugin_locale_font_call_convention.rs`

### 5.4 문서

- `docs/dev-guide/`: `plugin-packaging.md`, `build.md`, `release.md`, `ci-gates.md`, `egui-mesh-channel.md`, `api-conventions.md`, `dep-issues.md`, `error-handling.md`, `external-interaction.md`, `i18n.md`, `paired-agent-handlers.md`. 위 grep에 나오지 않지만 함께 볼 문서: `plugin-development.md`, `plugin-permissions.md`
- `docs/plugins/`: `agent-stream/`, `claude/`, `clipboard-viewer/`, `codex/`, `git-viewer/`, `remote-attach-children.md`. 이 문서들을 이 레포로 옮길지 정한다(`docs/plugins/claude/index.md`는 1.5절의 시험이 읽는다)
- `docs/surfaces/html/`, `docs/surfaces/image/`, `docs/surfaces/markdown/`
- `docs/features/`: `child-terminal/`, `hooks/`, `native-file-picker/`, `remote-attach/`, `telemetry/`
- `docs/architecture/index.md`, `docs/concepts/ubiquitous-language.md`, `docs/concepts/plugins.md`
- `docs/design/systems/design-gallery-mapping.md`, `docs/design/systems/design-token-mapping.md`
- `CHANGELOG.md`, `CLAUDE.md`(tasty 레포의 기여자 안내)
- `crates/tasty-plugin-protocol/CHANGELOG.md`: 3.2절의 "함께 재빌드" 전제

### 5.5 보조 검색으로 찾은 곳

크레이트 이름 대신 glob, 폴더 경로, 매니페스트 id(`com.tasty.*`), 버전 게이트 이름으로 플러그인을 가리키는 곳이다.

```bash
git grep -l -E 'tasty-plugin-(\*|\$|\{)|crates/tasty-plugin-?["*$/]|plugin-version|check-plugin-version|com\.tasty\.(claude|codex|markdown|html|image|git-viewer|clipboard-viewer|mesh-demo|agent-stream)' \
  89efb560203f7041959515ebe17dc7f3de2bbcd2 -- . ':!crates/tasty-plugin-*' ':!docs'
```

- 고쳐야 하는 것: 5.1·5.2절에 넣은 `Justfile`, 빌드·서명 스크립트, `.githooks/`, `plugin-version-check.yml`, `.gitignore`. 그리고 `crates/tasty-doc-guards/tests/automatic_job_roster_is_pinned.rs`, `githooks_are_pinned.rs`(게이트·훅 목록을 고정하는 시험), `src/source_guards/bundled_plugin_namespace_coverage.rs`, `src/source_guards/builtin_plugin_roster.rs`(`BUILTINS`와 디스크의 `crates/tasty-plugin-*/tasty-plugin.toml`을 대조). 이 가운데 `plugin-version-check.yml`과 버전 게이트 스크립트, 빌드 스크립트의 `crates/tasty-plugin-*` 탐색, `builtin_plugin_roster.rs`가 대조하는 `BUILTINS`는 `mesh-demo` 때문에 tasty에 남고, 옮긴 플러그인 항목만 고친다(F-2).
- 고치지 않아도 되는 것: `.github/workflows/format-check.yml`(`tasty-plugin-sdk-wasm`만 가리킴, tasty에 남음).
- 매니페스트 id만 쓰는 곳: `crates/tasty-cli/src/commands/debug.rs`(문서 주석의 예시), 본체 코드(`src/adapters/ipc/handler/`, `src/app/attach_client/`, `src/remote/`, `src/runtime/surface_registry/`, `src/plugin_bridge/` 등), `crates/tasty-file-handler/`, `crates/tasty-file-format/`, `crates/tasty-host-plugin/`, `crates/tasty-ipc/`, `crates/tasty-model/`, `crates/tasty-telemetry/`, `crates/tasty-ui-widgets/`, `crates/tasty-gallery/`, `lang/*.toml`. id는 이전 뒤에도 바뀌지 않으므로 소스 이전만으로는 고칠 필요가 없다. 다만 시험이 플러그인을 workspace에서 빌드해 띄우는 경우(`tests/plugin_disable_withdraws_surface_kinds.rs` 등)는 이전 때 하나씩 확인한다.

이전 직전에 두 명령을 그 시점의 tasty 커밋으로 다시 돌려 목록을 갱신한다.

## 6. 2단계 결정에 기대는 부분

아래는 2단계(플러그인 계약·신뢰 체계)에서 정한다. 이 문서는 정하지 않고 표시만 한다.

- **최소 호스트 버전**: 매니페스트에 둘지, 지금의 `api_version`(`HOST_API_VERSION = "1"`)과 어떤 관계로 둘지. 레포가 나뉘면 플러그인이 지원하는 본체 버전 범위를 표시할 수단이 필요하다. — **결정 대기(2단계)**
- **서명 범위와 키**: 지금은 매니페스트 한 파일만 서명한다. 바이너리를 서명 범위에 넣을지, 장기 발급자 키를 둘지, 키 회전을 어떻게 할지. tasty `docs/dev-guide/plugin-packaging.md`의 "영구 release 키를 두지 않는 이유"는 "앱과 독립적으로 플러그인을 배포하거나 업데이트할 때는 신뢰 루트·키 회전·서명 범위를 함께 다시 정한다"고 적고 있다. — **결정 대기(2단계)**
- **egui 버전 규칙**(3.3절): 본체와 플러그인의 egui 버전이 다를 때 거절할지, 무엇으로 확인할지, 실측 호환 시험을 어디서 할지. — **결정 대기(2단계)**
- **풀 패키지 잠금 파일 형식**: 확정된 것은 "플러그인 버전과 플랫폼별 SHA-256을 적는다"까지다. 파일 이름, 위치, 형식은 정하지 않았다. — **결정 대기(2·5단계)**

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

## 결정 대기 모음

### tasty 쪽 결정

| ID | 항목 | 참고 |
|---|---|---|
| T-1 | `ATTEMPT_MARKER_PREFIX` 대조 시험 처리 | 2절의 선택지 1~3 |
| T-2 | 이 레포에 tasty lint·복잡도 게이트를 그대로 옮길지 | 4.2절 6 |
| T-3 | `docs/plugins/` 문서를 이 레포로 옮길지 | 1.5절, 5.4절 |

### 2단계 결정

- 최소 호스트 버전, 서명 범위와 키, egui 버전 규칙, 풀 패키지 잠금 파일 형식(6절)
