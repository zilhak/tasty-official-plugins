#!/usr/bin/env bash
# Installs shellcheck and actionlint from their official GitHub releases into a tool cache
# and prints the directory that holds both. The versions and the SHA-256 sums of the release
# archives are pinned here: an archive with another sum fails the install. An install that
# is already there and reports the pinned versions is reused. Linux x86_64 and aarch64 only.
#
# Usage: bash scripts/install-lint-tools.sh
# The cache is $LINT_TOOL_CACHE, else $RUNNER_TOOL_CACHE/lint-tools, else
# ${XDG_CACHE_HOME:-~/.cache}/tasty-official-plugins/lint-tools. Never ~/.cargo/bin or a
# system directory. Downloading needs curl, tar, xz, and network access.

set -euo pipefail

die() { printf '[install-lint-tools] %s\n' "$*" >&2; exit 1; }

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
dir="$base/shellcheck-$SHELLCHECK_VERSION-actionlint-$ACTIONLINT_VERSION-$sc_arch"

# Whether both tools in $1 run and report the pinned versions.
installed() {
    local sc al
    [ -x "$1/shellcheck" ] && [ -x "$1/actionlint" ] || return 1
    sc=$("$1/shellcheck" --version 2>/dev/null) || return 1
    al=$("$1/actionlint" -version 2>/dev/null) || return 1
    grep -qx "version: $SHELLCHECK_VERSION" <<<"$sc" && grep -qx "$ACTIONLINT_VERSION" <<<"$al"
}

if installed "$dir"; then
    printf '[install-lint-tools] already installed: %s\n' "$dir" >&2
    printf '%s\n' "$dir"
    exit 0
fi

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {  # <url> <file> <sha256>
    local got
    curl -fsSL --retry 3 -o "$2" "$1" || die "cannot download $1"
    got=$(sha256_of "$2")
    [ "$got" = "$3" ] || die "SHA-256 of $(basename "$2") is $got, expected $3 (pinned in scripts/install-lint-tools.sh)."
}

printf '[install-lint-tools] downloading shellcheck %s and actionlint %s into %s\n' \
    "$SHELLCHECK_VERSION" "$ACTIONLINT_VERSION" "$dir" >&2
fetch "$sc_url" "$work/shellcheck.tar.xz" "$sc_sha256"
fetch "$al_url" "$work/actionlint.tar.gz" "$al_sha256"

mkdir -p "$work/sc" "$work/al" "$work/bin"
tar -xJf "$work/shellcheck.tar.xz" -C "$work/sc" || die "cannot extract shellcheck (is xz installed?)."
tar -xzf "$work/actionlint.tar.gz" -C "$work/al" || die "cannot extract actionlint."
cp "$work/sc/shellcheck-v$SHELLCHECK_VERSION/shellcheck" "$work/bin/shellcheck" \
    || die "shellcheck is not where the release archive keeps it."
cp "$work/al/actionlint" "$work/bin/actionlint" || die "actionlint is not in its release archive."
chmod +x "$work/bin/shellcheck" "$work/bin/actionlint"
installed "$work/bin" || die "the downloaded tools do not report the pinned versions."

# Move into place last, so a run that stops halfway leaves no partial install behind.
mkdir -p "$base"
rm -rf "$dir"
mv "$work/bin" "$dir"
installed "$dir" || die "the installed tools in $dir do not run."
printf '%s\n' "$dir"
