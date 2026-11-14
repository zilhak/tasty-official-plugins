#!/usr/bin/env bash
# Local and CI entry point for this repository.
#
#   scripts/check.sh [all]          fmt + lint + test + build (debug)
#   scripts/check.sh fmt            cargo fmt --all --check
#   scripts/check.sh lint           cargo clippy on workspace members, -D warnings
#   scripts/check.sh test [ARGS]    cargo test on workspace members; ARGS go to
#                                   cargo test as-is, e.g. --lib --bins
#   scripts/check.sh build [ARGS]   build the plugin crates; ARGS go to cargo build
#                                   as-is, e.g. --release, --profile dist,
#                                   --target aarch64-pc-windows-msvc
#   scripts/check.sh audit          cargo audit on Cargo.lock; needs cargo-audit, not in all
#   scripts/check.sh licenses       cargo deny license policy (deny.toml); needs cargo-deny
#   scripts/check.sh list-plugins   print the plugin crates to build, one per line
#   scripts/check.sh list-bins      print the plugin crates that produce a binary
#
# Build and check commands use --locked and audit only reads Cargo.lock, so it never changes.
# Runs under bash on Linux and macOS and under Git Bash on Windows.
# fmt, lint, test, build, and all warn on stderr when this clone does not run .githooks.
set -euo pipefail

cd "$(dirname "$0")/.."

# The only list of plugin crates to build. A pin-check test compares it with the
# tasty-plugin-* workspace members in crates/.
PLUGINS=(
  tasty-plugin-agent-common
  tasty-plugin-agent-stream
  tasty-plugin-claude
  tasty-plugin-clipboard-viewer
  tasty-plugin-codex
  tasty-plugin-git-viewer
  tasty-plugin-html
  tasty-plugin-image
  tasty-plugin-markdown
)
# Library-only crates among PLUGINS. They build but produce no executable. A
# pin-check test compares this list with the members that have no bin target.
LIBS=(
  tasty-plugin-agent-common
)

run() {
  echo "+ $*" >&2
  "$@"
}

cmd_fmt() { run cargo fmt --all --check; }
cmd_lint() { run cargo clippy --workspace --all-targets --locked -- -D warnings; }
# The repo-gates tests compare with the real strip-cfg-test judge. They are integration tests
# and Unix only, so the judge is installed only there, and not for a --lib or --bins run
# (see scripts/install-judge.sh).
cmd_test() {
  local a only_units=0
  for a in "$@"; do
    case "$a" in --lib | --bins) only_units=1 ;; esac
  done
  if [[ -z "${TASTY_STRIP_CFG_TEST_BIN:-}" && $only_units -eq 0 ]]; then
    case "$(uname -s)" in
      MINGW* | MSYS* | CYGWIN*) ;;
      *)
        TASTY_STRIP_CFG_TEST_BIN="$(bash scripts/install-judge.sh)"
        export TASTY_STRIP_CFG_TEST_BIN
        ;;
    esac
  fi
  run cargo test --workspace --locked "$@"
}
cmd_audit() { run cargo audit --file Cargo.lock; }
# Only the license check: advisories stay with cargo audit, and bans and sources are not
# checked (see deny.toml). --workspace takes every member as a root.
cmd_licenses() { run cargo deny --locked --workspace check licenses; }

cmd_build() {
  local args=()
  local p
  for p in "${PLUGINS[@]}"; do
    args+=(-p "$p")
  done
  run cargo build --locked "${args[@]}" "$@"
}

cmd_list_plugins() { printf '%s\n' "${PLUGINS[@]}"; }

cmd_list_bins() {
  local p l is_lib
  for p in "${PLUGINS[@]}"; do
    is_lib=0
    for l in "${LIBS[@]}"; do
      [[ "$p" == "$l" ]] && is_lib=1
    done
    [[ $is_lib -eq 0 ]] && echo "$p"
  done
  return 0
}

# Warns on stderr when the git hooks of this tree's .githooks do not run here: core.hooksPath
# is not set (bash scripts/dev-setup.sh was not run in this clone), points to a missing
# folder, or points to another tree. It never fails and does not change the exit code. CI
# checkouts have no hooks, and a source tarball has no git, so both stay quiet.
warn_hooks() {
  local top hp got want
  [[ "${GITHUB_ACTIONS:-}" == "true" ]] && return 0
  git rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 0
  top=$(git rev-parse --show-toplevel 2>/dev/null) || return 0
  # --path, not --type=path, which git before 2.18 rejects.
  hp=$(git config --path --get core.hooksPath 2>/dev/null) || hp=""
  if [[ -z "$hp" ]]; then
    echo "warning: core.hooksPath is not set for this clone, so the commit, merge, and push" \
      "checks in .githooks do not run. Run bash scripts/dev-setup.sh once; every worktree" \
      "of the clone shares the setting." >&2
    return 0
  fi
  # A Windows drive path (C:/... or C:\...) is absolute too.
  if ! [[ "$hp" == /* || "$hp" =~ ^[A-Za-z]:[/\\] ]]; then
    hp="$top/$hp"
  fi
  if ! got=$(cd "$hp" 2>/dev/null && pwd -P); then
    echo "warning: core.hooksPath points to a missing folder ($hp), so no git hook runs." \
      "Run bash scripts/dev-setup.sh." >&2
    return 0
  fi
  want=$(cd "$top/.githooks" 2>/dev/null && pwd -P) || want=""
  if [[ "$got" != "$want" ]]; then
    echo "warning: the git hooks come from $got, not from .githooks of this tree, so the" \
      "hooks of another tree or branch run here. Run bash scripts/dev-setup.sh." >&2
  fi
  return 0
}

sub="${1:-all}"
[[ $# -gt 0 ]] && shift

case "$sub" in
  # In an || list errexit is off inside the function, so a failed write cannot stop the run.
  # The subshell keeps the warning from changing the shell that builds.
  all | fmt | lint | test | build) ( warn_hooks ) || true ;;
esac

case "$sub" in
  all)
    cmd_fmt
    cmd_lint
    cmd_test
    cmd_build "$@"
    ;;
  fmt) cmd_fmt ;;
  lint) cmd_lint ;;
  test) cmd_test "$@" ;;
  build) cmd_build "$@" ;;
  audit) cmd_audit ;;
  licenses) cmd_licenses ;;
  list-plugins) cmd_list_plugins ;;
  list-bins) cmd_list_bins ;;
  -h | --help | help)
    sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'
    ;;
  *)
    echo "unknown subcommand: $sub (see scripts/check.sh help)" >&2
    exit 2
    ;;
esac
