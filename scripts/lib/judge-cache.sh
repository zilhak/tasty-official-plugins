# Where scripts/install-judge.sh puts the strip-cfg-test judge, and which tasty commit it
# is built from. Sourced by install-judge.sh and judge-bin.sh.
# shellcheck shell=bash

# Prints "<git url> <full sha>" of the tasty commit that Cargo.lock in $1 pins, read from
# the tasty-plugin-sdk package. Returns 1 when the lock has no such git package.
judge_tasty_source() {
    local lock="${1:-.}/Cargo.lock"
    [ -f "$lock" ] || return 1
    awk '
        /^\[\[package\]\]/ { name = ""; next }
        /^name = / { name = $3; gsub(/"/, "", name); next }
        /^source = "git\+/ && name == "tasty-plugin-sdk" {
            src = $3; gsub(/"/, "", src); sub(/^git\+/, "", src)
            sha = src; sub(/^.*#/, "", sha)
            url = src; sub(/[?#].*$/, "", url)
            print url, sha; found = 1; exit
        }
        END { exit found ? 0 : 1 }
    ' "$lock"
}

# Prints the cache directory for the judge built from the pinned tasty commit of the
# repository at $1. Returns 1 when the commit cannot be read.
judge_cache_dir() {
    local src sha base
    src=$(judge_tasty_source "${1:-.}") || return 1
    sha=${src##* }
    [ "${#sha}" -ge 8 ] || return 1
    base="${TASTY_JUDGE_CACHE:-${RUNNER_TOOL_CACHE:+$RUNNER_TOOL_CACHE/strip-cfg-test}}"
    [ -n "$base" ] || base="${XDG_CACHE_HOME:-$HOME/.cache}/tasty-official-plugins/strip-cfg-test"
    printf '%s/%s\n' "$base" "${sha:0:8}"
}
