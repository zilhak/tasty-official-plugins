# Tasty 공식 플러그인

[Tasty](https://github.com/zilhak/tasty) 개발자가 만들어 제공하는 플러그인의 소스 레포다. 공개 레포다.

## 역할

- 공식 플러그인의 소스, 빌드, 릴리스를 맡는다.
- 플러그인 프로토콜, 매니페스트, SDK의 정본은 본체 레포에 있다. 계약을 바꿔야 하면 본체 레포에서 먼저 바꾼다.
- 플러그인은 별도 OS 프로세스로 실행되며 본체 바이너리와 링크되지 않는다.

플러그인 소스는 `crates/`에, 플러그인 문서는 `docs/plugins/`에 있다. 본체 레포의 기준 커밋에서 이력 없이 옮겨 왔다.

## 버전 정책

- 레포 릴리스는 `VERSION`의 semver로 `v*` 태그(`v` + `VERSION`)를 찍는다. 첫 릴리스는 `v0.1.0`이다.
- 플러그인 빌드 결과가 바뀌면 그 플러그인의 패치 버전을 하나 올린다. 크레이트 `Cargo.toml`과 `tasty-plugin.toml`의 `version`은 같이 움직인다.
- 기준 tasty 커밋을 옮기면 바뀐 tasty 크레이트가 의존 폐포에 든 플러그인만 올린다.
- `Cargo.lock`의 crates.io 크레이트가 바뀌어도 그 크레이트가 의존 폐포에 든 플러그인의 패치 버전을 올린다. 한 릴리스 안의 여러 lock 변경은 한 번만 올린다.
- 검사는 `scripts/check-plugin-version-bump.sh`(git 훅)와 `.github/workflows/plugin-version-check.yml`이 한다. 자세한 규칙은 [docs/branching.md](docs/branching.md)와 [docs/ci.md](docs/ci.md#plugin-version-check)에 있다.

## Git 규칙

- [Conventional Commits](https://www.conventionalcommits.org/)를 따른다: `type(scope): description`. type은 `feat`, `fix`, `docs`, `refactor`, `test`, `perf`, `style`, `i18n`, `chore` 중에서 고른다.
- 제목과 본문은 영어 평문으로 쓰고 Markdown 서식을 쓰지 않는다.
- 한 커밋에는 한 변경만 담는다. 기능 수정이나 추가가 끝나면 다시 묻지 않고 커밋한다. 리팩토링과 기능 추가는 나눈다.
- `--no-verify`를 쓰지 않는다. 훅이 막으면 우회하지 말고 고치거나 보고한다.
- `.claude`와 `.claude-workspace`는 git에 넣지 않는다. 로컬 경로와 로컬 작업 문서를 이 레포에 적지 않는다.
- 자세한 기준은 [커밋 컨벤션](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/commit-convention.md)을 따른다.
