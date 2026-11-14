#!/usr/bin/env bash
# Installs shellcheck and actionlint from their official GitHub releases into a tool cache
# and prints the directory that holds both. The versions and the SHA-256 sums of the release
# archives are pinned here: an archive with another sum fails the install. An install that
# is already there and reports the pinned versions is reused. Linux x86_64 and aarch64 only.
#
# Concurrent runs need no lock. Each run downloads into a folder of its own in the cache and
# publishes it by creating the symlink $dir, which fails when another run published first.
# The script prints the folder the link points at, not the link, so a later replacement of
# the link does not pull the tools from under a job that is using them. No run deletes a
# folder that another run may use: a broken install is moved aside, not deleted.
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
case "$base" in /*) ;; *) base="$PWD/$base" ;; esac
name="shellcheck-$SHELLCHECK_VERSION-actionlint-$ACTIONLINT_VERSION-$sc_arch"
dir="$base/$name"

# Whether both tools in $1 run and report the pinned versions.
installed() {
    local sc al
    [ -x "$1/shellcheck" ] && [ -x "$1/actionlint" ] || return 1
    sc=$("$1/shellcheck" --version 2>/dev/null) || return 1
    al=$("$1/actionlint" -version 2>/dev/null) || return 1
    grep -qx "version: $SHELLCHECK_VERSION" <<<"$sc" && grep -qx "$ACTIONLINT_VERSION" <<<"$al"
}

# Prints the folder that $dir stands for when it holds a working install: the target of the
# link, or $dir itself when an earlier version of this script made it a plain directory.
current_install() {
    local real
    if [ -L "$dir" ]; then
        real=$(readlink "$dir") || return 1
        case "$real" in /*) ;; *) real="$base/$real" ;; esac
    elif [ -d "$dir" ]; then
        real=$dir
    else
        return 1
    fi
    installed "$real" && printf '%s\n' "$real"
}

use() {  # <message> <folder>
    printf '[install-lint-tools] %s: %s\n' "$1" "$2" >&2
    printf '%s\n' "$2"
    exit 0
}

if real=$(current_install); then use "already installed" "$real"; fi

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

mkdir -p "$base" || die "cannot create the tool cache $base."
# This run's own folder, next to $dir on the same file system. Only this run writes to it,
# and it is removed on exit only while it cannot have been published (see below).
work=$(mktemp -d "$base/.$name.XXXXXX")
exposed=0
trap '[ "$exposed" -eq 1 ] || rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

fetch() {  # <url> <file> <sha256>
    local got
    curl -fsSL --retry 3 -o "$2" "$1" || die "cannot download $1"
    got=$(sha256_of "$2")
    [ "$got" = "$3" ] || die "SHA-256 of $(basename "$2") is $got, expected $3 (pinned in scripts/install-lint-tools.sh)."
}

printf '[install-lint-tools] downloading shellcheck %s and actionlint %s into %s\n' \
    "$SHELLCHECK_VERSION" "$ACTIONLINT_VERSION" "$work" >&2
dl="$work/download"
mkdir -p "$dl/sc" "$dl/al"
fetch "$sc_url" "$dl/shellcheck.tar.xz" "$sc_sha256"
fetch "$al_url" "$dl/actionlint.tar.gz" "$al_sha256"
tar -xJf "$dl/shellcheck.tar.xz" -C "$dl/sc" || die "cannot extract shellcheck (is xz installed?)."
tar -xzf "$dl/actionlint.tar.gz" -C "$dl/al" || die "cannot extract actionlint."
cp "$dl/sc/shellcheck-v$SHELLCHECK_VERSION/shellcheck" "$work/shellcheck" \
    || die "shellcheck is not where the release archive keeps it."
cp "$dl/al/actionlint" "$work/actionlint" || die "actionlint is not in its release archive."
rm -rf "$dl"
chmod +x "$work/shellcheck" "$work/actionlint"
installed "$work" || die "the downloaded tools do not report the pinned versions."

# Publish: ln -sn fails when $dir exists, so exactly one run's link wins. Without -n, ln
# would create the link inside the folder that an existing $dir points at, and succeed. -n
# is in GNU, BSD (macOS), and busybox ln. It does not help when $dir is a plain directory,
# so then ln is not tried before that directory has been checked and moved aside. A run
# that loses uses the install that won. When $dir is broken (a link to a folder that fails
# the check, or an old plain directory), it is renamed aside and the link is created again.
# mv onto an existing link to a directory would move into that directory, so the broken one
# is never replaced in place. Nothing is deleted: another job may still use the old folder.
#
# From the first ln on, this run never deletes its folder. ln may have created the link
# even when it reports a failure or this run is stopped right after it, and another run may
# have printed the folder since, so neither ln's status nor the current link tells whether
# the folder was ever visible. A run that loses therefore leaves its folder behind.
link=$(basename "$work")
exposed=1
for attempt in 1 2 3; do
    if [ -L "$dir" ] || [ ! -d "$dir" ]; then
        if ln -sn "$link" "$dir" 2>/dev/null; then
            use "installed" "$work"
        fi
    fi
    if real=$(current_install); then
        # ln can fail after it made the link, and then the link is this run's own.
        if [ "$real" = "$work" ]; then use "installed" "$work"; fi
        printf '[install-lint-tools] left the unused folder of this run, remove it when no job runs: %s\n' "$work" >&2
        use "installed by another run" "$real"
    fi
    if [ -e "$dir" ] || [ -L "$dir" ]; then
        aside="$base/.broken.$name.$$.$attempt"
        if mv "$dir" "$aside" 2>/dev/null; then
            printf '[install-lint-tools] moved the broken install aside: %s\n' "$aside" >&2
        fi
    fi
done
die "cannot publish $dir: it is still there and broken after three attempts. Left the folder of this run: $work"
