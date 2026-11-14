# Installs pinned release tools into a tool cache folder and prints the folder. Source it from
# an install script, which sets these and then calls tc_install:
#
#   TC_TAG     the log prefix, for example install-lint-tools
#   TC_SCRIPT  the script that pins the sums, named in a checksum error
#   TC_BASE    the cache directory (absolute)
#   TC_NAME    the install name in it; the install is $TC_BASE/$TC_NAME
#   tc_installed <folder>  whether the tools in <folder> run and report the pinned versions
#   tc_stage <work>        downloads (with tc_fetch), unpacks, and copies the tools into <work>
#
# Concurrent runs need no lock. Each run downloads into a folder of its own in the cache and
# publishes it by creating the symlink $TC_BASE/$TC_NAME, which fails when another run
# published first. The folder the link points at is printed, not the link, so a later
# replacement of the link does not pull the tools from under a job that is using them. No run
# deletes a folder that another run may use: a broken install is moved aside, not deleted.
# shellcheck shell=bash

tc_die() { printf '[%s] %s\n' "$TC_TAG" "$*" >&2; exit 1; }

# Prints the folder that the install name stands for when it holds a working install: the
# target of the link, or the name itself when an earlier version made it a plain directory.
tc_current_install() {
    local dir="$TC_BASE/$TC_NAME" real
    if [ -L "$dir" ]; then
        real=$(readlink "$dir") || return 1
        case "$real" in /*) ;; *) real="$TC_BASE/$real" ;; esac
    elif [ -d "$dir" ]; then
        real=$dir
    else
        return 1
    fi
    tc_installed "$real" && printf '%s\n' "$real"
}

tc_use() {  # <message> <folder>
    printf '[%s] %s: %s\n' "$TC_TAG" "$1" "$2" >&2
    printf '%s\n' "$2"
    exit 0
}

tc_sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

tc_fetch() {  # <url> <file> <sha256>
    local got
    curl -fsSL --retry 3 -o "$2" "$1" || tc_die "cannot download $1"
    got=$(tc_sha256_of "$2")
    [ "$got" = "$3" ] || tc_die "SHA-256 of $(basename "$2") is $got, expected $3 (pinned in $TC_SCRIPT)."
}

# Reuses a working install, or downloads, checks, and publishes a new one. Exits.
tc_install() {
    local dir="$TC_BASE/$TC_NAME" real link attempt aside
    if real=$(tc_current_install); then tc_use "already installed" "$real"; fi

    mkdir -p "$TC_BASE" || tc_die "cannot create the tool cache $TC_BASE."
    # This run's own folder, next to the install name on the same file system. Only this run
    # writes to it, and it is removed on exit only while it cannot have been published.
    work=$(mktemp -d "$TC_BASE/.$TC_NAME.XXXXXX")
    exposed=0
    trap '[ "$exposed" -eq 1 ] || rm -rf "$work"' EXIT
    trap 'exit 1' HUP INT TERM

    tc_stage "$work"
    tc_installed "$work" || tc_die "the downloaded tools do not report the pinned versions."

    # Publish: ln -sn fails when the name exists, so exactly one run's link wins. Without -n,
    # ln would create the link inside the folder that an existing link points at, and
    # succeed. -n is in GNU, BSD (macOS), and busybox ln. It does not help when the name is a
    # plain directory, so then ln is not tried before that directory has been checked and
    # moved aside. A run that loses uses the install that won. When the name is broken (a
    # link to a folder that fails the check, or an old plain directory), it is renamed aside
    # and the link is created again. mv onto an existing link to a directory would move into
    # that directory, so the broken one is never replaced in place. Nothing is deleted:
    # another job may still use the old folder.
    #
    # From the first ln on, this run never deletes its folder. ln may have created the link
    # even when it reports a failure or this run is stopped right after it, and another run
    # may have printed the folder since, so neither ln's status nor the current link tells
    # whether the folder was ever visible. A run that loses therefore leaves its folder.
    link=$(basename "$work")
    exposed=1
    for attempt in 1 2 3; do
        if [ -L "$dir" ] || [ ! -d "$dir" ]; then
            if ln -sn "$link" "$dir" 2>/dev/null; then
                tc_use "installed" "$work"
            fi
        fi
        if real=$(tc_current_install); then
            # ln can fail after it made the link, and then the link is this run's own.
            if [ "$real" = "$work" ]; then tc_use "installed" "$work"; fi
            printf '[%s] left the unused folder of this run, remove it when no job runs: %s\n' "$TC_TAG" "$work" >&2
            tc_use "installed by another run" "$real"
        fi
        if [ -e "$dir" ] || [ -L "$dir" ]; then
            aside="$TC_BASE/.broken.$TC_NAME.$$.$attempt"
            if mv "$dir" "$aside" 2>/dev/null; then
                printf '[%s] moved the broken install aside: %s\n' "$TC_TAG" "$aside" >&2
            fi
        fi
    done
    tc_die "cannot publish $dir: it is still there and broken after three attempts. Left the folder of this run: $work"
}
