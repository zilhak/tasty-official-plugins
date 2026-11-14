#!/usr/bin/env bash
# Installs shellcheck and actionlint from their official GitHub releases into a tool cache
# and prints the directory that holds both. The versions and the SHA-256 sums of the release
# archives are pinned here: an archive with another sum fails the install. An install that
# is already there and reports the pinned versions is reused. Linux x86_64 and aarch64 only.
#
# Concurrent runs need no lock; the download, check, and publish steps are in
# scripts/lib/tool-cache.sh, which describes how.
#
# Usage: bash scripts/install-lint-tools.sh
# The cache is $LINT_TOOL_CACHE, else $RUNNER_TOOL_CACHE/lint-tools, else
# ${XDG_CACHE_HOME:-~/.cache}/tasty-official-plugins/lint-tools. Never ~/.cargo/bin or a
# system directory. Downloading needs curl, tar, xz, and network access.

set -euo pipefail

# shellcheck source=scripts/lib/tool-cache.sh
. "$(cd "$(dirname "$0")" && pwd)/lib/tool-cache.sh"
TC_TAG=install-lint-tools
TC_SCRIPT=scripts/install-lint-tools.sh
die() { tc_die "$@"; }

SHELLCHECK_VERSION=0.11.0
ACTIONLINT_VERSION=1.7.7

# actionlint's sums are the ones in its release's checksums.txt. shellcheck publishes no
# sums, so these are the sums of the release archives when they were pinned.
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64)
        sc_arch=x86_64
        sc_sha256=8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198
        al_arch=amd64
        al_sha256=023070a287cd8cccd71515fedc843f1985bf96c436b7effaecce67290e7e0757
        ;;
    Linux/aarch64 | Linux/arm64)
        sc_arch=aarch64
        sc_sha256=12b331c1d2db6b9eb13cfca64306b1b157a86eb69db83023e261eaa7e7c14588
        al_arch=arm64
        al_sha256=401942f9c24ed71e4fe71b76c7d638f66d8633575c4016efd2977ce7c28317d0
        ;;
    *) die "no pinned release for $(uname -s)/$(uname -m); Linux x86_64 and aarch64 only." ;;
esac

sc_url="https://github.com/koalaman/shellcheck/releases/download/v$SHELLCHECK_VERSION/shellcheck-v$SHELLCHECK_VERSION.linux.$sc_arch.tar.xz"
al_url="https://github.com/rhysd/actionlint/releases/download/v$ACTIONLINT_VERSION/actionlint_${ACTIONLINT_VERSION}_linux_$al_arch.tar.gz"

base="${LINT_TOOL_CACHE:-${RUNNER_TOOL_CACHE:+$RUNNER_TOOL_CACHE/lint-tools}}"
base="${base:-${XDG_CACHE_HOME:-$HOME/.cache}/tasty-official-plugins/lint-tools}"
case "$base" in /*) ;; *) base="$PWD/$base" ;; esac
TC_BASE=$base
TC_NAME="shellcheck-$SHELLCHECK_VERSION-actionlint-$ACTIONLINT_VERSION-$sc_arch"

# Whether both tools in $1 run and report the pinned versions.
tc_installed() {
    local sc al
    [ -x "$1/shellcheck" ] && [ -x "$1/actionlint" ] || return 1
    sc=$("$1/shellcheck" --version 2>/dev/null) || return 1
    al=$("$1/actionlint" -version 2>/dev/null) || return 1
    grep -qx "version: $SHELLCHECK_VERSION" <<<"$sc" && grep -qx "$ACTIONLINT_VERSION" <<<"$al"
}

tc_stage() {  # <work>
    local dl="$1/download"
    printf '[install-lint-tools] downloading shellcheck %s and actionlint %s into %s\n' \
        "$SHELLCHECK_VERSION" "$ACTIONLINT_VERSION" "$1" >&2
    mkdir -p "$dl/sc" "$dl/al"
    tc_fetch "$sc_url" "$dl/shellcheck.tar.xz" "$sc_sha256"
    tc_fetch "$al_url" "$dl/actionlint.tar.gz" "$al_sha256"
    tar -xJf "$dl/shellcheck.tar.xz" -C "$dl/sc" || die "cannot extract shellcheck (is xz installed?)."
    tar -xzf "$dl/actionlint.tar.gz" -C "$dl/al" || die "cannot extract actionlint."
    cp "$dl/sc/shellcheck-v$SHELLCHECK_VERSION/shellcheck" "$1/shellcheck" \
        || die "shellcheck is not where the release archive keeps it."
    cp "$dl/al/actionlint" "$1/actionlint" || die "actionlint is not in its release archive."
    rm -rf "$dl"
    chmod +x "$1/shellcheck" "$1/actionlint"
}

tc_install
