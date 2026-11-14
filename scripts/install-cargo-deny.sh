#!/usr/bin/env bash
# Installs cargo-deny from its official GitHub release into a tool cache and prints the
# directory that holds it. The version and the SHA-256 sums of the release archives are
# pinned here: an archive with another sum fails the install. An install that is already
# there and reports the pinned version is reused. Linux x86_64 and aarch64 only. The
# download, check, and lock-free publish steps are in scripts/lib/tool-cache.sh.
#
# Usage: bash scripts/install-cargo-deny.sh
# The cache is $CARGO_DENY_CACHE, else $RUNNER_TOOL_CACHE/cargo-deny, else
# ${XDG_CACHE_HOME:-~/.cache}/tasty-official-plugins/cargo-deny. Never ~/.cargo/bin or a
# system directory. Downloading needs curl, tar, and network access.

set -euo pipefail

# shellcheck source=scripts/lib/tool-cache.sh
. "$(cd "$(dirname "$0")" && pwd)/lib/tool-cache.sh"
TC_TAG=install-cargo-deny
TC_SCRIPT=scripts/install-cargo-deny.sh

CARGO_DENY_VERSION=0.20.2

# The sums of the musl release archives, as published next to them (<file>.sha256).
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64)
        triple=x86_64-unknown-linux-musl
        sha256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
        ;;
    Linux/aarch64 | Linux/arm64)
        triple=aarch64-unknown-linux-musl
        sha256=995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d
        ;;
    *) tc_die "no pinned release for $(uname -s)/$(uname -m); Linux x86_64 and aarch64 only." ;;
esac

archive="cargo-deny-$CARGO_DENY_VERSION-$triple"
url="https://github.com/EmbarkStudios/cargo-deny/releases/download/$CARGO_DENY_VERSION/$archive.tar.gz"

base="${CARGO_DENY_CACHE:-${RUNNER_TOOL_CACHE:+$RUNNER_TOOL_CACHE/cargo-deny}}"
base="${base:-${XDG_CACHE_HOME:-$HOME/.cache}/tasty-official-plugins/cargo-deny}"
case "$base" in /*) ;; *) base="$PWD/$base" ;; esac
TC_BASE=$base
TC_NAME="cargo-deny-$CARGO_DENY_VERSION-$triple"

# Whether the cargo-deny in $1 runs and reports the pinned version.
tc_installed() {
    local v
    [ -x "$1/cargo-deny" ] || return 1
    v=$("$1/cargo-deny" --version 2>/dev/null) || return 1
    [ "$v" = "cargo-deny $CARGO_DENY_VERSION" ]
}

tc_stage() {  # <work>
    local dl="$1/download"
    printf '[install-cargo-deny] downloading cargo-deny %s into %s\n' "$CARGO_DENY_VERSION" "$1" >&2
    mkdir -p "$dl"
    tc_fetch "$url" "$dl/cargo-deny.tar.gz" "$sha256"
    tar -xzf "$dl/cargo-deny.tar.gz" -C "$dl" || tc_die "cannot extract cargo-deny."
    cp "$dl/$archive/cargo-deny" "$1/cargo-deny" \
        || tc_die "cargo-deny is not where the release archive keeps it."
    rm -rf "$dl"
    chmod +x "$1/cargo-deny"
}

tc_install
