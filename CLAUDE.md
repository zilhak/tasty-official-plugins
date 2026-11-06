# Tasty 공식 플러그인

[Tasty](https://github.com/zilhak/tasty) 개발자가 만들어 제공하는 플러그인의 소스 레포다. 공개 레포다.

## 역할

- 공식 플러그인의 소스, 빌드, 릴리스를 맡는다.
- 플러그인 프로토콜, 매니페스트, SDK의 정본은 본체 레포에 있다. 계약을 바꿔야 하면 본체 레포에서 먼저 바꾼다.
- 플러그인은 별도 OS 프로세스로 실행되며 본체 바이너리와 링크되지 않는다.

플러그인 소스는 아직 본체 레포에서 옮겨 오는 중이다.

## Git 규칙

- [Conventional Commits](https://www.conventionalcommits.org/)를 따른다: `type(scope): description`. type은 `feat`, `fix`, `docs`, `refactor`, `test`, `perf`, `style`, `i18n`, `chore` 중에서 고른다.
- 제목과 본문은 영어 평문으로 쓰고 Markdown 서식을 쓰지 않는다.
- 한 커밋에는 한 변경만 담는다. 기능 수정이나 추가가 끝나면 다시 묻지 않고 커밋한다. 리팩토링과 기능 추가는 나눈다.
- `--no-verify`를 쓰지 않는다. 훅이 막으면 우회하지 말고 고치거나 보고한다.
- `.claude`와 `.claude-workspace`는 git에 넣지 않는다. 로컬 경로와 로컬 작업 문서를 이 레포에 적지 않는다.
- 자세한 기준은 [커밋 컨벤션](https://github.com/zilhak/tasty/blob/main/docs/dev-guide/commit-convention.md)을 따른다.
