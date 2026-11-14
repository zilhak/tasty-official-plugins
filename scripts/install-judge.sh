#!/usr/bin/env bash
# Installs strip-cfg-test, the judge that check-plugin-version-bump.sh uses to leave
# test-only code out of the comparison, from the tasty commit that Cargo.lock pins.
#
#   bash scripts/install-judge.sh
#
# The binary goes to a cache directory, never to ~/.cargo/bin:
#   $TASTY_JUDGE_CACHE, else $RUNNER_TOOL_CACHE/strip-cfg-test, else
#   ${XDG_CACHE_HOME:-~/.cache}/tasty-official-plugins/strip-cfg-test,
# followed by /<first 8 characters of the tasty sha>/bin/strip-cfg-test.
# Prints the binary path on stdout; export it as TASTY_STRIP_CFG_TEST_BIN.
# Exit code: 0 installed or already there, 1 failed.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib/judge-cache.sh
. scripts/lib/judge-cache.sh

src=$(judge_tasty_source .) || {
    echo "[install-judge] Cargo.lock has no tasty-plugin-sdk git package to take the tasty commit from" >&2
    exit 1
}
url=${src% *}
sha=${src##* }
dir=$(judge_cache_dir .)
bin="$dir/bin/strip-cfg-test"

# The judge has no --help that exits 0. --check-fresh exits 0 or 3 (no source to compare
# with in this repository) when the binary runs.
runs() {
    local rc=0
    "$1" --check-fresh . >/dev/null 2>&1 || rc=$?
    [ "$rc" -eq 0 ] || [ "$rc" -eq 3 ]
}

if [ -x "$bin" ] && runs "$bin"; then
    echo "[install-judge] already installed: $bin" >&2
    printf '%s\n' "$bin"
    exit 0
fi

target=$(mktemp -d)
trap 'rm -rf "${target:?}"' EXIT
echo "[install-judge] building strip-cfg-test from $url at ${sha:0:8} into $dir" >&2
CARGO_TARGET_DIR="$target" cargo install --quiet --git "$url" --rev "$sha" --locked \
    --root "$dir" tasty-doc-guards --bin strip-cfg-test >&2
runs "$bin" || {
    echo "[install-judge] $bin does not run after the install" >&2
    exit 1
}
printf '%s\n' "$bin"
