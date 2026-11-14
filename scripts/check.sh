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
#   scripts/check.sh list-plugins   print the plugin crates to build, one per line
#   scripts/check.sh list-bins      print the plugin crates that produce a binary
#
# Build and check commands use --locked and audit only reads Cargo.lock, so it never changes.
# Runs under bash on Linux and macOS and under Git Bash on Windows.
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
cmd_test() { run cargo test --workspace --locked "$@"; }
cmd_audit() { run cargo audit --file Cargo.lock; }

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

sub="${1:-all}"
[[ $# -gt 0 ]] && shift

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
  list-plugins) cmd_list_plugins ;;
  list-bins) cmd_list_bins ;;
  -h | --help | help)
    sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
    ;;
  *)
    echo "unknown subcommand: $sub (see scripts/check.sh help)" >&2
    exit 2
    ;;
esac
