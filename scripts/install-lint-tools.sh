#!/usr/bin/env bash
# Installs shellcheck and actionlint from their official GitHub releases into a tool cache
# and prints the directory that holds both. The versions and the SHA-256 sums of the release
# archives are pinned here: an archive with another sum fails the install. An install that
# is already there and reports the pinned versions is reused. Runs that share the cache
# install one at a time and never delete a working install. Linux x86_64 and aarch64 only.
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

mkdir -p "$base" || die "cannot create the tool cache $base."

# Runs that share the cache install one at a time: a lock directory next to the install,
# taken with mkdir, which is atomic everywhere (flock is not on macOS). The holder writes
# its pid into it. A lock whose pid no longer runs is left from a run that was killed and
# is taken over; a lock without a pid yet, or with a live pid, is waited for.
lock="$dir.lock"
lock_wait="${LINT_TOOL_LOCK_WAIT:-300}"
work=""
have_lock=0
cleanup() {
    if [ -n "$work" ]; then rm -rf "$work"; fi
    if [ "$have_lock" -eq 1 ]; then rm -rf "$lock"; fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

# kill -0 fails for a live process of another user, so ps confirms.
alive() { kill -0 "$1" 2>/dev/null || ps -p "$1" >/dev/null 2>&1; }

waited=0
until mkdir "$lock" 2>/dev/null; do
    owner=$(cat "$lock/pid" 2>/dev/null) || owner=""
    if [ -n "$owner" ] && ! alive "$owner"; then
        # Move it aside before deleting, and give it back if another run took it over
        # in between, so a live run's lock is never deleted.
        stale="$lock.stale.$$"
        if mv "$lock" "$stale" 2>/dev/null; then
            if [ "$(cat "$stale/pid" 2>/dev/null)" = "$owner" ]; then
                printf '[install-lint-tools] removed the lock of run %s, which no longer runs\n' "$owner" >&2
                rm -rf "$stale"
            else
                mv "$stale" "$lock" 2>/dev/null || rm -rf "$stale"
            fi
        fi
        continue
    fi
    [ "$waited" -lt "$lock_wait" ] \
        || die "waited ${lock_wait}s for $lock (pid ${owner:-not written}). Remove it if no install is running."
    sleep 1
    waited=$((waited + 1))
done
have_lock=1
printf '%s\n' "$$" > "$lock/pid"

# Another run may have installed while this one waited.
if installed "$dir"; then
    printf '[install-lint-tools] installed by another run: %s\n' "$dir" >&2
    printf '%s\n' "$dir"
    exit 0
fi

# Staged next to the install, on the same file system, so publishing is one rename. Only
# the lock holder stages for $dir, so a staging folder already there was left by a killed run.
for old in "$dir".staging.*; do
    if [ -e "$old" ]; then rm -rf "$old"; fi
done
work=$(mktemp -d "$dir.staging.XXXXXX")

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

# Publish with a single rename, so a run that stops halfway leaves no partial install
# behind. Under the lock, an existing $dir failed the check above: it is a broken install,
# and only then is it replaced. mv into an existing directory would nest, so the rename
# only happens when $dir is gone.
if [ -e "$dir" ] || [ -L "$dir" ]; then
    printf '[install-lint-tools] replacing the broken install in %s\n' "$dir" >&2
    rm -rf "$dir"
fi
[ ! -e "$dir" ] || die "cannot remove the broken install in $dir."
mv "$work/bin" "$dir" || die "cannot move the tools into $dir."
installed "$dir" || die "the installed tools in $dir do not run."
printf '%s\n' "$dir"
