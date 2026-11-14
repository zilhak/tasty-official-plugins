#!/usr/bin/env bash
# Decides whether a plugin version must go up by comparing the content of each plugin and of
# the path dependencies in this repository that it links. Rust is compared without test-only
# code and after rustfmt, so comment changes can still count as content. Files that are
# copied into the archive as they are (lang/, assets/NOTICE.md, .gitattributes) are compared
# by file mode and blob id. The inputs outside the crates that decide every archive (the
# notice set, .gitattributes, and the packaging code, listed in scripts/lib/archive-inputs.txt)
# ask every plugin with an archive for a bump, once per release.
# --staged [--base rev] compares the index with base (default HEAD); --range before after
# compares two commits. --violations-out <file> writes the plugin directories whose version
# must go up, one per line. This does not build and compare binaries.
# Exit code: pass 0, violation 1, undecidable 2.
#
# Ported from tasty scripts/check-plugin-version-bump.sh.

set -uo pipefail

die() { printf 'undecidable: %s\n' "$*" >&2; exit 2; }

git rev-parse --git-dir >/dev/null 2>&1 || die "not a git repository (a release tarball?)."
ROOT="$(git rev-parse --show-toplevel)" || die "cannot find the repository root."
CALLER_DIR="$(pwd)"
cd "$ROOT" || die "cannot move to the repository root."

command -v rustfmt >/dev/null 2>&1 \
    || die "rustfmt is missing, so formatting changes cannot be told from real ones. rustup component add rustfmt"

# Read the judge lookup next to this script so synthetic repositories can use it too. No Cargo build here.
# shellcheck source=scripts/lib/judge-bin.sh
. "$(cd "$(dirname "$0")" && pwd)/lib/judge-bin.sh"
resolve_judge strip-cfg-test TASTY_STRIP_CFG_TEST_BIN "$ROOT"
STRIP_BIN="$JUDGE_BIN"
# Without the judge, test-only changes are compared too, which can ask for bumps that are not needed.
if [ -z "$STRIP_BIN" ]; then
    echo "[plugin-version] cannot narrow to shipped code; test-only changes also ask for a bump." >&2
fi

MODE=""; BASE="HEAD"; BEFORE=""; AFTER=""; VIOLATIONS_OUT=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --staged) MODE=staged; shift ;;
        --base)   BASE="${2:-}"; [ -n "$BASE" ] || die "--base needs a rev."; shift 2 ;;
        --range)  MODE=range; BEFORE="${2:-}"; AFTER="${3:-}"
                  [ -n "$BEFORE" ] && [ -n "$AFTER" ] || die "--range needs two revs."
                  shift 3 ;;
        --violations-out) VIOLATIONS_OUT="${2:-}"
                  [ -n "$VIOLATIONS_OUT" ] || die "--violations-out needs a file path."
                  shift 2 ;;
        *) die "unknown argument '$1'" ;;
    esac
done
[ -n "$MODE" ] || die "give either --staged or --range."
# Resolve a relative path against the caller's directory, as the caller meant it.
if [ -n "$VIOLATIONS_OUT" ]; then
    case "$VIOLATIONS_OUT" in /*) ;; *) VIOLATIONS_OUT="$CALLER_DIR/$VIOLATIONS_OUT" ;; esac
    : > "$VIOLATIONS_OUT" || die "cannot write the --violations-out file: $VIOLATIONS_OUT"
fi

if [ "$MODE" = staged ]; then
    if ! git rev-parse --verify --quiet "$BASE^{commit}" >/dev/null; then
        # Only an unborn HEAD (the first commit) has nothing to compare with. A base that
        # names no commit is a wrong argument or a missing object, not a first commit.
        head_ref=$(git symbolic-ref --quiet HEAD 2>/dev/null) || head_ref=""
        if [ "$BASE" = HEAD ] && [ -n "$head_ref" ] \
            && ! git rev-parse --verify --quiet "$head_ref" >/dev/null; then
            echo "[plugin-version] skip: no commit to compare with (first commit). 0 judged."
            exit 0
        fi
        die "cannot find the --base rev: $BASE (in a shallow clone, raise fetch-depth)"
    fi
    BEFORE_REV="$BASE"
    AFTER_PREFIX=""          # `:path`
else
    for r in "$BEFORE" "$AFTER"; do
        git rev-parse --verify --quiet "$r^{commit}" >/dev/null \
            || die "cannot find rev: $r (in a shallow clone, raise fetch-depth)"
    done
    BEFORE_REV="$BEFORE"
    AFTER_PREFIX="$AFTER"
fi

after_ref() { printf '%s:%s' "$AFTER_PREFIX" "$1"; }

exists_at() { git cat-file -e "$1" 2>/dev/null; }

# Whole crates are extracted because out-of-line test module declarations must be read too.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Caches the normal/build dependency list. Called without command substitution so that die
# ends the main shell. Empty or failed results are not cached, and Cargo's stderr is shown.
# --target all: the plugins ship for several platforms, so a crate that only one platform
# links (cfg(windows), cfg(target_os = "macos")) is part of the closure too. Every closure
# below (plugin content, pin moves, root inputs, skipping crates outside the plugins) comes
# from here.
closure_of() {
    local pname="$1" cache="$WORK/closure.$1"
    CLOSURE_FILE="$cache"
    CLOSURE_PATHS="$cache.paths"
    [ -f "$cache" ] && return 0
    if ! cargo tree -p "$pname" -e normal,build --target all --prefix none --offline 2>"$cache.err" \
        > "$cache.raw"; then
        if grep -q 'failed to download' "$cache.err"; then
            die "cargo tree failed ($pname): a crate for some target is not downloaded. The check reads the dependencies of every target offline; run cargo fetch --locked first. cargo's stderr:
$(cat "$cache.err")"
        fi
        die "cargo tree failed ($pname). cargo's stderr:
$(cat "$cache.err")"
    fi
    awk '{print $1}' "$cache.raw" | sort -u > "$cache.tmp"
    [ -s "$cache.tmp" ] || die "the dependency list of $pname is empty, so the scope cannot be set."
    # Keep only path dependencies inside the repository. The root package and outside paths drop out.
    sed -n 's|^[^ ]* v[^ ]* (\(/[^)]*\)).*$|\1|p' "$cache.raw" \
        | sed -n "s|^${ROOT_REAL}/||p" | sort -u > "$cache.paths"
    mv "$cache.tmp" "$cache"
}

# Read the cache file; piping a producer into grep -q can turn SIGPIPE into a wrong result.
links() {  # <pname> <crate-name>
    closure_of "$1"
    grep -qxF "$2" "$CLOSURE_FILE"
}

# Path dependencies outside the workspace can have a directory name other than the crate name.
links_crate() {  # <pname> <crate-dir>
    if is_extra_root "$2"; then
        closure_of "$1"
        grep -qxF "$2" "$CLOSURE_PATHS"
    else
        links "$1" "$(basename "$2")"
    fi
}

# Extract the crates from a git tree and make a copy with test-only code removed.
materialize() {
    local label="$1" tree="$2"; shift 2
    [ "$#" -gt 0 ] || return 0
    [ -n "$STRIP_BIN" ] || return 0
    local raw="$WORK/$label.raw" cooked="$WORK/$label.cooked"
    mkdir -p "$raw" "$cooked" || die "cannot create the work directory."
    # git archive fails as a whole on a missing path, so pass only existing ones.
    local present=() p
    for p in "$@"; do
        if git cat-file -e "$tree:$p" 2>/dev/null; then present+=("$p"); fi
    done
    [ "${#present[@]}" -gt 0 ] || return 0
    git archive --format=tar "$tree" -- "${present[@]}" | tar -x -C "$raw" \
        || die "cannot extract the tree ($label)."
    # Directories without Rust files are not passed to the judge.
    local scan=() r
    # EXTRA_ROOTS is a space-separated list, so word splitting is intended.
    # shellcheck disable=SC2086
    for r in "$SCAN_ROOT" $EXTRA_ROOTS; do
        [ -d "$raw/$r" ] || continue
        [ -n "$(find "$raw/$r" -name '*.rs' -print -quit)" ] && scan+=("$r")
    done
    [ "${#scan[@]}" -gt 0 ] || return 0
    "$STRIP_BIN" --blank-test-only-files "$cooked" "$raw" "${scan[@]}" >/dev/null \
        || die "the test-only code remover failed ($label)."
}

# The first version line is the version, and the content comparison leaves out the same line.
# TOML sections are not parsed.
VERSION_LINE_RE='^version[[:space:]]*=[[:space:]]*"'
version_line_no() { awk -v re="$VERSION_LINE_RE" '$0 ~ re { print NR; exit }'; }

# The files that <path> names on <side> (before or after) as "path mode blob" lines, or
# <absent>. <path> may be a folder. A tree is listed (ls-tree), not a rev:path blob, so a
# folder lists its files; the index is listed with ls-files. A listing that fails prints a
# value of its own side, so it never equals the other side.
archive_blob_id() {  # <side> <path>
    local out
    if [ "$1" = before ]; then
        out=$(git ls-tree -r "$BEFORE_REV" -- "$2") || { printf '<unreadable %s>' "$1"; return; }
        out=$(printf '%s\n' "$out" | awk -F '\t' 'NF { split($1, m, " "); print $2 " " m[1] " " m[3] }')
    elif [ "$MODE" = staged ]; then
        out=$(git ls-files -s -- "$2") || { printf '<unreadable %s>' "$1"; return; }
        out=$(printf '%s\n' "$out" | awk -F '\t' 'NF { split($1, m, " "); print $2 " " m[1] " " m[2] }')
    else
        out=$(git ls-tree -r "$AFTER" -- "$2") || { printf '<unreadable %s>' "$1"; return; }
        out=$(printf '%s\n' "$out" | awk -F '\t' 'NF { split($1, m, " "); print $2 " " m[1] " " m[3] }')
    fi
    [ -n "$out" ] || out="<absent>"
    printf '%s' "$out"
}

# Rust is compared after the judge and rustfmt, without blank lines. Without a result from a
# step the previous content is used. Files that the archive carries as they are, lang/ and
# assets/NOTICE.md, and .gitattributes files are compared by mode and blob, so a mode or a
# final newline counts; tasty-plugin.toml is compared without its version line and with its
# mode.
shipped_sum() {
    local label="$1" ref="$2" path="$3" out fmt cooked vline mode
    case "$path" in
        *.rs)
            # A missing Rust file and a copy emptied as test-only count as the same.
            if ! exists_at "$ref"; then printf '<no-ship>'; return; fi
            cooked="$WORK/$label.cooked/$path"
            if [ -f "$cooked" ]; then out=$(cat "$cooked"); else out=$(git show "$ref"); fi
            fmt=$(printf '%s' "$out" | rustfmt --edition "$RUST_EDITION" --emit stdout 2>/dev/null)
            [ -n "$fmt" ] && out="$fmt"
            out=$(printf '%s' "$out" | sed '/^[[:space:]]*$/d')
            if [ -z "$out" ]; then printf '<no-ship>'; return; fi
            ;;
        */lang/*|*/assets/NOTICE.md|*/.gitattributes)
            archive_blob_id "$label" "$path"
            return
            ;;
        */Cargo.toml|*/tasty-plugin.toml)
            # Leave the version line out so that a bump does not ask for another bump.
            if ! exists_at "$ref"; then printf '<absent>'; return; fi
            out=$(git show "$ref")
            vline=$(printf '%s' "$out" | version_line_no)
            [ -n "$vline" ] && out=$(printf '%s' "$out" | sed "${vline}d")
            case "$path" in
                */tasty-plugin.toml)
                    mode=$(archive_blob_id "$label" "$path" | awk '{ print $2 }')
                    out="$out
<mode $mode>"
                    ;;
            esac
            ;;
        *)
            # Assets tell a missing file from an empty one.
            if ! exists_at "$ref"; then printf '<absent>'; return; fi
            out=$(git show "$ref")
            ;;
    esac
    printf '%s' "$out" | cksum
}

# Reads the first version line chosen by the rule above.
version_at() {
    local ref="$1" text vline
    if ! exists_at "$ref"; then printf ''; return; fi
    text=$(git show "$ref")
    vline=$(printf '%s' "$text" | version_line_no)
    [ -n "$vline" ] || { printf ''; return; }
    printf '%s' "$text" | sed -n "${vline}s/${VERSION_LINE_RE}\([^\"]*\)\".*/\1/p"
}

# Reads the three numbers. A pre-release or build suffix is not compared.
ver_key() {
    printf '%s' "$1" | sed -n 's/^\([0-9][0-9]*\)\.\([0-9][0-9]*\)\.\([0-9][0-9]*\).*$/\1 \2 \3/p'
}

ver_gt() {
    local ka kb; ka=$(ver_key "$1"); kb=$(ver_key "$2")
    [ -n "$ka" ] && [ -n "$kb" ] || return 1
    # Split the six integers of the two versions into positional arguments.
    # shellcheck disable=SC2086
    set -- $ka $kb
    [ "$1" -gt "$4" ] && return 0
    [ "$1" -lt "$4" ] && return 1
    [ "$2" -gt "$5" ] && return 0
    [ "$2" -lt "$5" ] && return 1
    [ "$3" -gt "$6" ]
}

# The directory of changed paths and of the plugin roster.
SCAN_ROOT=crates
# The profile that release archives are built with (release.yml, scripts/package-plugin.sh).
# A change of a profile in its inheritance chain changes every plugin binary.
RELEASE_PROFILE=dist

# --no-renames lists both paths of a move, and T lists a file that became a symbolic link
# or the other way round; both can change an archive.
if [ "$MODE" = staged ]; then
    # The whole difference between the base and the index, not only what is staged on HEAD.
    CHANGED=$(git diff --cached --name-only --no-renames --diff-filter=ACMRDT "$BASE" -- "$SCAN_ROOT/")
    OUTSIDE=$(git diff --cached --name-only --no-renames --diff-filter=ACMRDT "$BASE" -- . ":(exclude)$SCAN_ROOT/")
else
    CHANGED=$(git diff --name-only --no-renames --diff-filter=ACMRDT "$BEFORE" "$AFTER" -- "$SCAN_ROOT/")
    OUTSIDE=$(git diff --name-only --no-renames --diff-filter=ACMRDT "$BEFORE" "$AFTER" -- . ":(exclude)$SCAN_ROOT/")
fi

# src/lang/assets are compared whatever the extension, and a .gitattributes in a crate
# decides how the files it ships are checked out. When this scope or the archive input list
# changes, compare it with the CI path filter (plugin-version-check.yml) as well.
build_affecting() {
    case "$1" in
        */src/*|*/lang/*|*/assets/*|*/Cargo.toml|*/tasty-plugin.toml|*/build.rs|*/.gitattributes) return 0 ;;
        *) return 1 ;;
    esac
}

# Changes outside SCAN_ROOT also bring in the linked path dependencies.
# Only repository paths that are not workspace members are added, using the current Cargo graph.
ROOT_REAL=$(pwd -P)
EXTRA_ROOTS=""
is_extra_root() {
    case " $EXTRA_ROOTS " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}
OUTSIDE_BUILD=""
while IFS= read -r f; do
    [ -z "$f" ] && continue
    if build_affecting "$f"; then OUTSIDE_BUILD="$OUTSIDE_BUILD
$f"; fi
done <<EOF2
$OUTSIDE
EOF2
MEMBERS=""; MEMBERS_READ=0
read_members() {
    [ "$MEMBERS_READ" = 1 ] && return 0
    MEMBERS=$(cargo metadata --no-deps --offline --format-version 1 2>"$WORK/metadata.err") \
        || die "cargo metadata failed. cargo's stderr:
$(cat "$WORK/metadata.err")"
    MEMBERS=$(printf '%s' "$MEMBERS" | grep -o '"manifest_path":"[^"]*"' \
        | sed -n "s|^\"manifest_path\":\"${ROOT_REAL}/\(.*\)/Cargo.toml\"\$|\1|p")
    MEMBERS_READ=1
}
if [ -n "$OUTSIDE_BUILD" ]; then
    for man in "$ROOT/$SCAN_ROOT"/*/tasty-plugin.toml; do
        [ -f "$man" ] || continue
        closure_of "$(basename "$(dirname "$man")")"
        while IFS= read -r p; do
            [ -z "$p" ] && continue
            case "$p" in "$SCAN_ROOT"/*) continue ;; esac
            read_members
            case "
$MEMBERS
" in *"
$p
"*) continue ;; esac
            is_extra_root "$p" || EXTRA_ROOTS="$EXTRA_ROOTS $p"
        done < "$CLOSURE_PATHS"
    done
    while IFS= read -r f; do
        [ -z "$f" ] && continue
        for r in $EXTRA_ROOTS; do
            case "$f" in "$r"/*) CHANGED="$CHANGED
$f"; break ;; esac
        done
    done <<EOF2
$OUTSIDE_BUILD
EOF2
fi

# Plugins that must be judged for a reason outside their files: directory, tab, reason; one per line.
EXTRA_DIRTY=""
add_dirty() {  # <plugin dir> <reason>
    EXTRA_DIRTY="$EXTRA_DIRTY
$1	$2"
}

# ── Cargo.lock: tasty pin moves ──────────────────────────────────────────────
# The tasty crates come from git, so their content does not show up as a path in this
# repository. When Cargo.lock moves a git package to another commit, the two commits are
# compared crate by crate, and a plugin whose dependency closure holds a changed crate must
# go up. See docs/branching.md.

LIB_DIR="$(cd "$(dirname "$0")" && pwd)/lib"
EXTRA_INPUTS="$LIB_DIR/tasty-extra-inputs.txt"
LOCAL_GIT_VARS=$(git rev-parse --local-env-vars)

# git on another repository, without the variables that point at this one (set inside hooks).
xgit() {
    (
        for v in $LOCAL_GIT_VARS; do unset "$v"; done
        exec git "$@"
    )
}

# A here-string, not a pipe: grep -q may stop reading early, and with pipefail the
# producer's SIGPIPE would turn a match into a failure.
outside_has() { grep -qxF "$1" <<<"$OUTSIDE"; }

# name, version, source, checksum of every package in the Cargo.lock at <ref>, tab separated.
lock_rows() {
    git show "$1" | awk '
        function flush() { if (name != "") print name "\t" ver "\t" src "\t" sum; name = ver = src = sum = "" }
        /^\[\[package\]\]/ { flush(); next }
        /^\[/ { flush(); next }
        /^name = /    { name = $3; gsub(/"/, "", name); next }
        /^version = / { ver = $3; gsub(/"/, "", ver); next }
        /^source = /  { src = $3; gsub(/"/, "", src); next }
        /^checksum = / { sum = $3; gsub(/"/, "", sum); next }
        END { flush() }'
}

# The plugin directories, one per line.
plugin_dirs() {
    local man
    for man in "$ROOT/$SCAN_ROOT"/*/tasty-plugin.toml; do
        [ -f "$man" ] && printf '%s\n' "$SCAN_ROOT/$(basename "$(dirname "$man")")"
    done
    return 0
}

# Sets LINKED to the plugin directories whose normal/build closure holds package <name>.
plugins_linking() {
    local p
    LINKED=""
    for p in $(plugin_dirs); do
        closure_of "$(basename "$p")"
        if grep -qxF "$1" "$CLOSURE_FILE"; then LINKED="$LINKED $p"; fi
    done
}

# Finds a git directory that has every commit given after the url; sets PIN_SRC.
# Order: TASTY_GIT_DIR, the cargo git databases, then a shallow fetch of the url.
PIN_FETCHES=0
pin_source() {  # <url> <commit>...
    local url="$1" d
    shift
    has_all() {
        local dir="$1" c
        shift
        for c in "$@"; do
            xgit -C "$dir" cat-file -e "$c^{commit}" 2>/dev/null || return 1
        done
    }
    PIN_SRC=""
    if [ -n "${TASTY_GIT_DIR:-}" ] && has_all "$TASTY_GIT_DIR" "$@"; then
        PIN_SRC="$TASTY_GIT_DIR"; return 0
    fi
    for d in "${CARGO_HOME:-$HOME/.cargo}"/git/db/*/; do
        [ -d "$d" ] || continue
        if has_all "$d" "$@"; then PIN_SRC="${d%/}"; return 0; fi
    done
    PIN_FETCHES=$((PIN_FETCHES + 1))
    d="$WORK/git-src.$PIN_FETCHES"
    if xgit init -q --bare "$d" \
        && xgit -C "$d" fetch -q --no-tags --depth=1 "$url" "$@" 2>"$WORK/fetch.err" \
        && has_all "$d" "$@"; then
        PIN_SRC="$d"; return 0
    fi
    return 1
}

# Writes "name<TAB>dir" for every package manifest in <src> at <commit> to <file>.
package_dirs_at() {  # <src> <commit> <file>
    local f n dir
    : > "$3"
    while IFS= read -r f; do
        n=$(xgit -C "$1" show "$2:$f" | awk '
            /^\[/ { in_pkg = ($0 == "[package]") }
            in_pkg && /^name[[:space:]]*=/ { sub(/^name[[:space:]]*=[[:space:]]*"/, ""); sub(/".*/, ""); print; exit }')
        [ -n "$n" ] || continue
        dir=$(dirname "$f")
        printf '%s\t%s\n' "$n" "$dir" >> "$3"
    done <<EOF2
$(xgit -C "$1" ls-tree -r --name-only "$2" | grep -E '(^|/)Cargo\.toml$')
EOF2
}

# Sets PKG_DIR to the directory of package <name> in <file>; returns 1 when missing,
# 2 when several manifests carry the name.
package_dir_in() {  # <file> <name>
    local hits
    hits=$(N="$2" awk -F '\t' '$1 == ENVIRON["N"] { print $2 }' "$1")
    PKG_DIR="$hits"
    [ -n "$hits" ] || return 1
    [ "$(printf '%s\n' "$hits" | wc -l)" -eq 1 ] || return 2
}

# Paths that tasty-extra-inputs.txt lists for <package> with <kind> (use or ignore).
extra_inputs_of() {  # <package> <kind>
    [ -f "$EXTRA_INPUTS" ] || return 0
    P="$1" K="$2" awk '/^[[:space:]]*(#|$)/ { next } $1 == ENVIRON["P"] && $2 == ENVIRON["K"] { print $3 }' "$EXTRA_INPUTS"
}

# Whether <path> is listed for <package> (use or ignore). A listed directory ends with "/".
extra_input_listed() {  # <package> <path>
    local e
    for e in $(extra_inputs_of "$1" use) $(extra_inputs_of "$1" ignore); do
        [ "$2" = "$e" ] && return 0
        case "$e" in */) case "$2" in "$e"*) return 0 ;; esac ;; esac
        case "$2" in */) case "$e" in "$2"*) return 0 ;; esac ;; esac
    done
    return 1
}

# Resolves "." and ".." in a relative path; keeps a trailing "/". Prints nothing when the
# path climbs above the root.
norm_path() {
    printf '%s\n' "$1" | awk '{
        n = split($0, part, "/"); top = 0; bad = 0
        for (i = 1; i <= n; i++) {
            if (part[i] == "" || part[i] == ".") continue
            if (part[i] == "..") { if (top == 0) { bad = 1; break } top--; continue }
            stack[++top] = part[i]
        }
        if (bad) exit
        out = ""
        for (i = 1; i <= top; i++) out = out (i > 1 ? "/" : "") stack[i]
        if ($0 ~ /\/$/ && out != "") out = out "/"
        print out
    }'
}

# Stops as undecidable when package <name> in <dir> reads a file outside its directory that
# tasty-extra-inputs.txt does not list. Test files and comment lines are skipped.
pin_check_outside_inputs() {  # <src> <commit> <dir> <name>
    local src="$1" c="$2" dir="$3" name="$4" hits line rest file content lit base resolved
    hits=$(xgit -C "$src" grep -nE '(include_(str|bytes)!|concat!|rerun-if-changed).*\.\./' "$c" -- \
        "$dir" ":(exclude)$dir/tests" ":(exclude)$dir/**/tests.rs" ":(exclude)$dir/**/tests/**" \
        ":(exclude)$dir/benches" ":(exclude)$dir/examples")
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        rest=${line#"$c":}
        file=${rest%%:*}
        content=${rest#*:}
        content=${content#*:}
        case "$content" in *[![:space:]]*) ;; *) continue ;; esac
        if grep -qE '^[[:space:]]*//' <<<"$content"; then continue; fi
        lit=$(printf '%s\n' "$content" | grep -oE '"[^"]*\.\./[^"]*"' | sed -n '1p')
        lit=${lit#\"}; lit=${lit%\"}
        case "$lit" in *rerun-if-changed=*) lit=${lit#*rerun-if-changed=}; base="$dir" ;;
            *) base=$(dirname "$file") ;; esac
        lit=${lit%%\{*}
        resolved=$(norm_path "$base/$lit")
        [ -n "$resolved" ] || die "$name reads a path above the source root ($file: $lit)"
        case "$resolved" in "$dir"/*) continue ;; esac
        extra_input_listed "$name" "$resolved" \
            || die "$name reads $resolved outside its crate ($file); add it to scripts/lib/tasty-extra-inputs.txt"
    done <<EOF2
$hits
EOF2
}

# Reads the edition from the manifest of <dir> at <commit>; stops when it cannot.
pin_edition() {  # <src> <commit> <dir> <name>
    local e
    e=$(xgit -C "$1" show "$2:$3/Cargo.toml" \
        | sed -n 's/^edition[[:space:]]*=[[:space:]]*"\([0-9]*\)".*/\1/p' | sed -n '1p')
    [ -n "$e" ] || die "cannot read the edition of $4 ($3/Cargo.toml at ${2:0:8})"
    PIN_EDITION="$e"
}

# The shipped content of one file of an extracted tree: Rust after the judge and rustfmt
# without blank lines, the crate manifest without its version line, anything else as is.
pin_sum() {  # <root> <cooked root> <path> <crate dir> <edition>
    local f="$1/$3" out fmt vline
    if [ ! -f "$f" ]; then
        # A missing Rust file and a copy emptied as test-only count as the same, as in
        # shipped_sum; other files tell a missing file from an empty one.
        case "$3" in *.rs) printf '<no-ship>' ;; *) printf '<absent>' ;; esac
        return
    fi
    case "$3" in
        *.rs)
            if [ -f "$2/$3" ]; then out=$(cat "$2/$3"); else out=$(cat "$f"); fi
            fmt=$(printf '%s' "$out" | rustfmt --edition "$5" --emit stdout 2>/dev/null)
            [ -n "$fmt" ] && out="$fmt"
            out=$(printf '%s' "$out" | sed '/^[[:space:]]*$/d')
            [ -n "$out" ] || { printf '<no-ship>'; return; }
            printf '%s' "$out" | cksum
            ;;
        "$4/Cargo.toml")
            out=$(cat "$f")
            vline=$(printf '%s' "$out" | version_line_no)
            [ -n "$vline" ] && out=$(printf '%s' "$out" | sed "${vline}d")
            printf '%s' "$out" | cksum
            ;;
        *) cksum < "$f" ;;
    esac
}

# Whether package <name> in <dir> ships other content at <new> than at <old>. Sets PIN_FILES
# to the changed paths. Tests, benches, examples and Markdown are left out.
PIN_SEQ=0
pin_package_changed() {  # <src> <old> <new> <dir> <name> <edition>
    local src="$1" old="$2" new="$3" dir="$4" name="$5" ed="$6" paths=("$4") e f
    local present side k raw cooked has_rs=0 a b c
    PIN_FILES=""
    for e in $(extra_inputs_of "$name" use); do paths+=("${e%/}"); done
    local diffs=()
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        case "$f" in
            "$dir"/tests/*|"$dir"/benches/*|"$dir"/examples/*|*.md) continue ;;
        esac
        diffs+=("$f")
        case "$f" in *.rs) has_rs=1 ;; esac
    done <<EOF2
$(xgit -C "$src" diff --name-only "$old" "$new" -- "${paths[@]}")
EOF2
    [ "${#diffs[@]}" -gt 0 ] || return 1
    PIN_SEQ=$((PIN_SEQ + 1))
    k="pin.$PIN_SEQ"
    for side in old new; do
        if [ "$side" = old ]; then c="$old"; else c="$new"; fi
        raw="$WORK/$k.$side.raw"; cooked="$WORK/$k.$side.cooked"
        mkdir -p "$raw" "$cooked" || die "cannot create the work directory."
        present=()
        for f in "${paths[@]}"; do
            xgit -C "$src" cat-file -e "$c:$f" 2>/dev/null && present+=("$f")
        done
        if [ "${#present[@]}" -gt 0 ]; then
            xgit -C "$src" archive --format=tar "$c" -- "${present[@]}" | tar -x -C "$raw" \
                || die "cannot extract $name at ${c:0:8}."
        fi
        if [ "$has_rs" = 1 ] && [ -n "$STRIP_BIN" ] && [ -d "$raw/$dir" ]; then
            "$STRIP_BIN" --blank-test-only-files "$cooked" "$raw" "$dir" >/dev/null \
                || die "the test-only code remover failed ($name at ${c:0:8})."
        fi
    done
    for f in "${diffs[@]}"; do
        a=$(pin_sum "$WORK/$k.old.raw" "$WORK/$k.old.cooked" "$f" "$dir" "$ed")
        b=$(pin_sum "$WORK/$k.new.raw" "$WORK/$k.new.cooked" "$f" "$dir" "$ed")
        [ "$a" = "$b" ] || PIN_FILES="$PIN_FILES $f"
    done
    [ -n "$PIN_FILES" ]
}

# Whether the [workspace.dependencies] or [workspace.package] sections of the tasty root
# Cargo.toml differ between two commits. Only matters to a crate that inherits from them.
pin_root_inherited_changed() {  # <src> <old> <new>
    local s c
    for c in "$2" "$3"; do
        s=$(xgit -C "$1" show "$c:Cargo.toml" 2>/dev/null | awk '
            /^\[/ { keep = ($0 ~ /^\[workspace\.(dependencies|package)/) }
            keep { print }')
        printf '%s' "$s" | cksum > "$WORK/root-inherit.$c"
    done
    ! cmp -s "$WORK/root-inherit.$2" "$WORK/root-inherit.$3"
}

# Whether the manifest of <dir> inherits something other than lints from the workspace.
pin_inherits() {  # <src> <commit> <dir>
    xgit -C "$1" show "$2:$3/Cargo.toml" | awk '
        /^\[/ { lints = ($0 ~ /^\[lints/) }
        !lints && /workspace[[:space:]]*=[[:space:]]*true/ { found = 1 }
        END { exit found ? 0 : 1 }'
}

# One summary line per changed crate and the plugins it reaches, for the messages.
PIN_SUMMARY=""

judge_pin_moves() {
    local before_lock="$BEFORE_REV:Cargo.lock" after_lock key url old new names name
    local dirs_old dirs_new d_old d_new rc reason vb va label plugins p
    exists_at "$before_lock" || die "cannot read Cargo.lock at $BEFORE_REV."
    exists_at "$(after_ref Cargo.lock)" \
        || die "cannot read Cargo.lock at ${AFTER_PREFIX:-the index}."
    after_lock=$(after_ref Cargo.lock)
    lock_rows "$before_lock" > "$WORK/lock.before" || die "cannot read Cargo.lock at $BEFORE_REV."
    lock_rows "$after_lock" > "$WORK/lock.after" || die "cannot read Cargo.lock at ${AFTER_PREFIX:-the index}."
    # url, old sha ("-" when new), new sha, name: git packages whose commit changed or appeared.
    awk -F '\t' '
        function split_src(s,   u, h) {
            sub(/^git\+/, "", s); h = s; sub(/^.*#/, "", h); u = s; sub(/[?#].*$/, "", u)
            URL = u; SHA = h
        }
        NR == FNR { if ($3 ~ /^git\+/) { split_src($3); old[$1] = SHA } next }
        $3 ~ /^git\+/ {
            split_src($3)
            o = ($1 in old) ? old[$1] : "-"
            if (o != SHA) print URL "\t" o "\t" SHA "\t" $1 "\t" $2
        }' "$WORK/lock.before" "$WORK/lock.after" | sort > "$WORK/pin.moves"
    [ -s "$WORK/pin.moves" ] || return 0

    while IFS= read -r key; do
        [ -n "$key" ] || continue
        url=$(printf '%s' "$key" | cut -f1); old=$(printf '%s' "$key" | cut -f2)
        new=$(printf '%s' "$key" | cut -f3)
        label="$(basename "$url" .git) ${old:0:8} → ${new:0:8}"
        [ "$old" = "-" ] && label="$(basename "$url" .git) (new) → ${new:0:8}"
        # Only packages that some plugin links matter.
        names=""
        while IFS="$(printf '\t')" read -r _ _ _ name _; do
            plugins_linking "$name"
            [ -n "$LINKED" ] && names="$names $name"
        done <<EOF2
$(K="$key" awk -F '\t' '($1 "\t" $2 "\t" $3) == ENVIRON["K"]' "$WORK/pin.moves")
EOF2
        [ -n "$names" ] || continue
        if [ "$old" = "-" ]; then
            pin_source "$url" "$new" || die "cannot read $url at ${new:0:8}: $(cat "$WORK/fetch.err" 2>/dev/null)"
        else
            pin_source "$url" "$old" "$new" \
                || die "cannot read $url at ${old:0:8} and ${new:0:8}: $(cat "$WORK/fetch.err" 2>/dev/null)"
        fi
        dirs_new="$WORK/dirs.${new}"
        [ -f "$dirs_new" ] || package_dirs_at "$PIN_SRC" "$new" "$dirs_new"
        if [ "$old" != "-" ]; then
            dirs_old="$WORK/dirs.${old}"
            [ -f "$dirs_old" ] || package_dirs_at "$PIN_SRC" "$old" "$dirs_old"
        fi
        for name in $names; do
            rc=0; package_dir_in "$dirs_new" "$name" || rc=$?
            [ "$rc" -eq 0 ] || die "cannot find the directory of $name in $url at ${new:0:8}$([ "$rc" -eq 2 ] && printf ' (several manifests: %s)' "$(printf '%s' "$PKG_DIR" | tr '\n' ' ')")"
            d_new="$PKG_DIR"
            pin_edition "$PIN_SRC" "$new" "$d_new" "$name"
            pin_check_outside_inputs "$PIN_SRC" "$new" "$d_new" "$name"
            reason=""
            if [ "$old" = "-" ]; then
                reason="new in Cargo.lock"
            else
                rc=0; package_dir_in "$dirs_old" "$name" || rc=$?
                d_old="$PKG_DIR"
                if [ "$rc" -ne 0 ] || [ "$d_old" != "$d_new" ]; then
                    reason="moved to $d_new"
                elif pin_package_changed "$PIN_SRC" "$old" "$new" "$d_new" "$name" "$PIN_EDITION"; then
                    reason="changed:$PIN_FILES"
                elif pin_inherits "$PIN_SRC" "$new" "$d_new" \
                    && pin_root_inherited_changed "$PIN_SRC" "$old" "$new"; then
                    reason="the inherited [workspace.dependencies] or [workspace.package] changed"
                fi
            fi
            [ -n "$reason" ] || continue
            vb=$(N="$name" awk -F '\t' '$1 == ENVIRON["N"] { print $2; exit }' "$WORK/lock.before")
            va=$(N="$name" awk -F '\t' '$1 == ENVIRON["N"] { print $2; exit }' "$WORK/lock.after")
            plugins_linking "$name"
            plugins="${LINKED# }"
            PIN_SUMMARY="$PIN_SUMMARY
    $name ${vb:-<none>} → ${va:-<none>} ($label; ${reason}); plugins: $plugins"
            for p in $LINKED; do
                add_dirty "$p" "$name ${vb:-<none>} → ${va:-<none>} ($label; $reason)"
            done
        done
    done <<EOF2
$(cut -f1-3 "$WORK/pin.moves" | sort -u)
EOF2
}

# ── Root build inputs ────────────────────────────────────────────────────────
# The plugins declare their dependencies in the root [workspace.dependencies], and their
# external crates are locked in the root Cargo.lock, so these files can change a plugin's
# build output without touching its directory.

NOTES=""
note() { NOTES="$NOTES
[plugin-version] note: $*"; }

# The released state: the tip of origin/main, written "<ref> (<sha8>)". main moves only for
# releases and hotfixes, so its tip is what was published, and reading versions there does
# not depend on which tags a clone fetched. A local main is used only when origin/main does
# not exist: it is rarely updated, so an old local main would pass versions that were
# already published.
release_base() {
    local ref=origin/main sha
    if ! sha=$(git rev-parse --verify --quiet "$ref^{commit}"); then
        ref=main
        sha=$(git rev-parse --verify --quiet "$ref^{commit}") || return 1
    fi
    RELEASE_COMMIT="$sha"
    printf '%s (%s)' "$ref" "${sha:0:8}"
}

# Whether an external crate update (Cargo.lock only) needs a version bump of <plugin dir>.
# Several lock updates that go out in one release need only one bump: a plugin whose version
# at the end of the range is already above its version at the tip of origin/main passes.
# Without origin/main or main, or without a readable plugin version there, the bump is
# required. This is the only place that decides it; RELEASED_NOTE explains a pass.
RELEASED_NOTE=""
RELEASE_COMMIT=""
external_update_needs_bump() {  # <plugin dir>
    local base vt va
    RELEASED_NOTE=""
    release_base > "$WORK/release.base" || return 0
    base=$(cat "$WORK/release.base")
    vt=$(version_at "$RELEASE_COMMIT:$1/tasty-plugin.toml")
    va=$(version_at "$(after_ref "$1/tasty-plugin.toml")")
    [ -n "$vt" ] && [ -n "$va" ] || return 0
    if ver_gt "$va" "$vt"; then
        RELEASED_NOTE="$1 is already at $va, above $vt at $base"
        return 1
    fi
    return 0
}

# The first field of each line of comm -3: a line only in the second file starts with a tab.
# awk, not sed 's/^\t//': BSD sed (macOS) reads \t in a pattern as the letter t.
first_field() { awk -F '\t' '{ print ($1 == "" ? $2 : $1) }'; }

# (ii) crates.io packages whose locked versions or checksums changed.
judge_external_lock() {
    local name vb va plugins p needed
    awk -F '\t' '$3 ~ /^(registry|sparse)\+/ { print $1 "\t" $2 "\t" $4 }' "$WORK/lock.before" | sort > "$WORK/ext.before"
    awk -F '\t' '$3 ~ /^(registry|sparse)\+/ { print $1 "\t" $2 "\t" $4 }' "$WORK/lock.after" | sort > "$WORK/ext.after"
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        plugins_linking "$name"
        [ -n "$LINKED" ] || continue
        vb=$(N="$name" awk -F '\t' '$1 == ENVIRON["N"] { print $2 }' "$WORK/ext.before" | tr '\n' ' ')
        va=$(N="$name" awk -F '\t' '$1 == ENVIRON["N"] { print $2 }' "$WORK/ext.after" | tr '\n' ' ')
        vb=${vb% }; va=${va% }
        needed=""
        for p in $LINKED; do
            if external_update_needs_bump "$p"; then
                add_dirty "$p" "$name ${vb:-<none>} → ${va:-<none>} (Cargo.lock)"
                needed="$needed $p"
            else
                note "$name ${vb:-<none>} → ${va:-<none>} (Cargo.lock): $RELEASED_NOTE; no further bump needed."
            fi
        done
        plugins="${LINKED# }"
        PIN_SUMMARY="$PIN_SUMMARY
    $name ${vb:-<none>} → ${va:-<none>} (Cargo.lock); plugins: $plugins"
    done <<EOF2
$(comm -3 "$WORK/ext.before" "$WORK/ext.after" | first_field | sort -u)
EOF2
}

# Sections of a root Cargo.toml as "section<TAB>line", one line per TOML key/value: comments,
# leading and trailing spaces and blank lines are dropped, and a value whose brackets stay
# open ([ or {, outside strings) takes the following lines until they close. Lines before
# the first header belong to section "". Multi-line strings (""" or ''') are not read: their
# brackets would be counted and swallow the sections after them. Such a line, brackets that
# close more than they open, and brackets still open at the end come out as
# "<unreadable><TAB>reason" so that the caller stops. So does a backslash in a table name or
# a key: escapes are not decoded, so "rele\u0061se" would not read as release.
toml_lines() {
    git show "$1" | awk '
        # The key of a key/value line: the text before the first = outside strings.
        function key_of(s,    i, c, q) {
            q = ""
            for (i = 1; i <= length(s); i++) {
                c = substr(s, i, 1)
                if (q != "") { if (c == q) q = ""; continue }
                if (c == "\"" || c == "\047") { q = c; continue }
                if (c == "=") return substr(s, 1, i - 1)
            }
            return s
        }
        # Drops a comment and counts the brackets outside strings; the count goes to depth.
        function scan(s,    i, n, c, q, out) {
            out = ""; q = ""; n = length(s)
            for (i = 1; i <= n; i++) {
                c = substr(s, i, 1)
                if (q != "") {
                    out = out c
                    if (q == "\"" && c == "\\" && i < n) { i++; out = out substr(s, i, 1); continue }
                    if (c == q) q = ""
                    continue
                }
                if (c == "#") break
                if ((c == "\"" || c == "\047") && substr(s, i, 3) == c c c) multi = 1
                if (c == "\"" || c == "\047") q = c
                else if (c == "[" || c == "{") depth++
                else if (c == "]" || c == "}") depth--
                out = out c
            }
            return out
        }
        {
            multi = 0
            line = scan($0)
            if (multi) { print "<unreadable>\tline " NR ", a multi-line string: " $0; bad = 1 }
            if (depth < 0) { print "<unreadable>\tline " NR ", a bracket that closes nothing: " $0; bad = 1; depth = 0 }
            sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line)
            if (line == "") next
            if (pending != "") {
                pending = pending " " line
                if (depth <= 0) { print sec "\t" pending; pending = ""; depth = 0 }
                next
            }
            if (line ~ /^\[/ && line ~ /\]$/ && depth == 0) {
                sec = line; gsub(/^\[+[[:space:]]*|[[:space:]]*\]+$/, "", sec)
                if (index(sec, "\\")) { print "<unreadable>\tline " NR ", a backslash escape in a table name: " $0; bad = 1 }
                next
            }
            if (index(key_of(line), "\\")) { print "<unreadable>\tline " NR ", a backslash escape in a key: " $0; bad = 1 }
            if (depth > 0) { pending = line; next }
            depth = 0
            print sec "\t" line
        }
        END {
            if (pending != "" || depth != 0) {
                print "<unreadable>\tbrackets still open at the end of the file, after: " substr(pending, 1, 80)
            }
        }'
}

# The entries of [workspace.dependencies] as "key<TAB>value" with any rev removed. Dotted
# keys (`name.features = [...]`) and [workspace.dependencies.<name>] tables are folded into
# the value of <name>. A line whose key cannot be read comes out under the key <unparsed>.
dep_entries() {  # <toml_lines file>
    awk -F '\t' '
        function add(k, sub_key, v) {
            if (sub_key == "rev") return
            seen[k] = 1
            if (sub_key == "") plain[k] = v
            else tab[k] = tab[k] " " sub_key " = " v ","
        }
        $1 == "workspace.dependencies" {
            line = $2
            if (!match(line, /^[A-Za-z0-9_-]+([[:space:]]*\.[[:space:]]*[A-Za-z0-9_-]+)*[[:space:]]*=/)) {
                print "<unparsed>\t" line; next
            }
            lhs = substr(line, 1, RLENGTH - 1); val = substr(line, RLENGTH + 1)
            sub(/^[[:space:]]+/, "", val); gsub(/[[:space:]]/, "", lhs)
            gsub(/,?[[:space:]]*rev[[:space:]]*=[[:space:]]*"[^"]*"/, "", val)
            key = lhs; sub(/\..*/, "", key)
            add(key, substr(lhs, length(key) + 2), val); next
        }
        index($1, "workspace.dependencies.") == 1 {
            key = substr($1, length("workspace.dependencies.") + 1); gsub(/[[:space:]]/, "", key)
            if (key !~ /^[A-Za-z0-9_-]+$/) { print "<unparsed>\t[" $1 "] " $2; next }
            if (!match($2, /^[A-Za-z0-9_-]+([[:space:]]*\.[[:space:]]*[A-Za-z0-9_-]+)*[[:space:]]*=/)) {
                print "<unparsed>\t[" $1 "] " $2; next
            }
            lhs = substr($2, 1, RLENGTH - 1); val = substr($2, RLENGTH + 1)
            sub(/^[[:space:]]+/, "", val); gsub(/[[:space:]]/, "", lhs)
            add(key, lhs, val); next
        }
        END {
            for (k in seen) {
                v = plain[k]
                if (k in tab) v = v (v == "" ? "" : " + ") "{" tab[k] " }"
                print k "\t" v
            }
        }' "$1" | sort
}

# Whether a package named <name> is in Cargo.lock on either side of the range. The lock is
# read from a file: grep -q on a pipe can stop early and the SIGPIPE of git show would make
# pipefail report a match as a failure.
in_either_lock() {  # <name>
    local side n=0
    for side in "$BEFORE_REV:Cargo.lock" "$(after_ref Cargo.lock)"; do
        n=$((n + 1))
        exists_at "$side" || continue
        if [ ! -f "$WORK/either-lock.$n" ]; then
            git show "$side" > "$WORK/either-lock.$n" || die "cannot read $side."
        fi
        grep -qxF "name = \"$1\"" "$WORK/either-lock.$n" && return 0
    done
    return 1
}

# ── Build settings: profiles, toolchain, cargo config ───────────────────────
# The release archives are built with the profile dist. A profile in its inheritance chain,
# the toolchain, and the repository cargo config change every plugin binary, so they ask
# every plugin with an archive for a bump, once per release (the rule of a lock update).

# awk functions for TOML keys: split_key splits a dotted key or table name into its parts,
# without quotes and without spaces around the dots; QUOTED[i] tells a quoted part. eq_at
# gives the position of the first = outside quotes, or 0.
TOML_KEY_AWK='
function split_key(s, parts,    n, i, c, q, cur, wasq) {
    n = 0; cur = ""; q = ""; wasq = 0
    for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (q != "") { if (c == q) q = ""; else cur = cur c; continue }
        if (c == "\"" || c == "\047") { q = c; wasq = 1; continue }
        if (c == ".") { n++; parts[n] = cur; QUOTED[n] = wasq; cur = ""; wasq = 0; continue }
        if (c ~ /[[:space:]]/) continue
        cur = cur c
    }
    n++; parts[n] = cur; QUOTED[n] = wasq
    return n
}
function eq_at(s,    i, c, q) {
    q = ""
    for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (q != "") { if (c == q) q = ""; continue }
        if (c == "\"" || c == "\047") { q = c; continue }
        if (c == "=") return i
    }
    return 0
}
function join_parts(parts, from, to,    i, out) {
    out = ""
    for (i = from; i <= to; i++) out = out (i > from ? "." : "") parts[i]
    return out
}
function trim(s) { sub(/^[[:space:]]+/, "", s); sub(/[[:space:]]+$/, "", s); return s }
# An inline table with its keys written bare, so { "path" = "x" } reads as { path = "x" };
# any other value as it is. <bad> for a key with a backslash (escapes are not decoded) or a
# table that cannot be read.
function norm_inline(v,    body, i, c, q, depth, item, items, n, j, e, k, np, kq, out) {
    v = trim(v)
    if (substr(v, 1, 1) != "{") return v
    if (substr(v, length(v), 1) != "}") return "<bad>"
    body = substr(v, 2, length(v) - 2); n = 0; item = ""; q = ""; depth = 0
    for (i = 1; i <= length(body); i++) {
        c = substr(body, i, 1)
        if (q != "") {
            item = item c
            if (q == "\"" && c == "\\" && i < length(body)) { i++; item = item substr(body, i, 1); continue }
            if (c == q) q = ""
            continue
        }
        if (c == "\"" || c == "\047") q = c
        else if (c == "[" || c == "{") depth++
        else if (c == "]" || c == "}") depth--
        if (c == "," && depth == 0) { items[++n] = item; item = ""; continue }
        item = item c
    }
    if (q != "" || depth != 0) return "<bad>"
    if (trim(item) != "") items[++n] = item
    out = ""
    for (j = 1; j <= n; j++) {
        e = eq_at(items[j]); if (!e) return "<bad>"
        k = substr(items[j], 1, e - 1)
        if (index(k, "\\")) return "<bad>"
        np = split_key(k, kq)
        out = out (j > 1 ? ", " : " ") join_parts(kq, 1, np) " = " trim(substr(items[j], e + 1))
    }
    return "{" out " }"
}
'

# The ref of <path> on <side> (before or after).
side_ref() {  # <side> <path>
    if [ "$1" = before ]; then printf '%s:%s' "$BEFORE_REV" "$2"; else after_ref "$2"; fi
}

# toml_lines of <ref> into <out>; stops when the file cannot be read or parsed.
toml_lines_to() {  # <ref> <out file> <what>
    toml_lines "$1" > "$2" || die "cannot read $3."
    if grep -q '^<unreadable>' "$2"; then
        die "cannot read $3; judge the plugin versions by hand:
$(grep '^<unreadable>' "$2" | cut -f2- | sed 's/^/    /')"
    fi
}

# The include entries of a cargo config file (toml_lines in <lines>) as "optional<TAB>path",
# each path as written. A form that is not read comes out as "<bad><TAB>reason".
config_includes() {  # <lines file>
    awk -F '\t' "$TOML_KEY_AWK"'
        function entry(t,    p, o) {
            if (t ~ /^\{/) {
                t = norm_inline(t)
                if (t == "<bad>") { print "<bad>\tan include table that is not read"; return }
                if (!match(t, /path[[:space:]]*=[[:space:]]*("[^"]*"|\047[^\047]*\047)/)) {
                    print "<bad>\tan include table without a path: " t; return
                }
                p = substr(t, RSTART, RLENGTH); sub(/^path[[:space:]]*=[[:space:]]*/, "", p)
                o = (t ~ /optional[[:space:]]*=[[:space:]]*true/) ? 1 : 0
            } else { p = t; o = 0 }
            print o "\t" substr(p, 2, length(p) - 2)
        }
        {
            sec = $1; line = substr($0, length($1) + 2)
            if (sec == "<unreadable>") next
            n = split_key(sec, sp)
            if (sp[1] == "include") { print "<bad>\tan [include] or [[include]] table: [" sec "] " line; next }
            if (sec != "") next
            e = eq_at(line); if (!e) next
            nk = split_key(substr(line, 1, e - 1), kp)
            if (kp[1] != "include") next
            if (nk != 1) { print "<bad>\ta dotted include key: " line; next }
            v = trim(substr(line, e + 1))
            if (v ~ /^("[^"]*"|\047[^\047]*\047)$/) { entry(v); next }
            if (v !~ /^\[.*\]$/) { print "<bad>\tan include value that is not read: " line; next }
            v = trim(substr(v, 2, length(v) - 2))
            while (v != "") {
                if (match(v, /^("[^"]*"|\047[^\047]*\047|\{[^}]*\})/)) {
                    len = RLENGTH; entry(substr(v, 1, len)); v = trim(substr(v, len + 1))
                    sub(/^,[[:space:]]*/, "", v)
                } else { print "<bad>\tan include value that is not read: " line; break }
            }
        }' "$1"
}

# Writes the repository cargo config files of <side> to <out>, one per line, lowest
# precedence first: each file follows the files it includes, and .cargo/config follows
# .cargo/config.toml. Includes are followed as cargo does (relative to the including file),
# each file once. An include outside the repository, a glob, or a missing file that is not
# optional stops the check.
INCLUDE_SEQ=0
cargo_config_files() {  # <side> <out file>
    : > "$2"
    : > "$WORK/config.seen.$1"
    config_walk "$1" .cargo/config.toml 1 "$2"
    config_walk "$1" .cargo/config 1 "$2"
}

config_walk() {  # <side> <path> <optional> <out file>
    local side="$1" path="$2" opt="$3" out="$4" ref inc o p dir resolved
    grep -qxF "$path" "$WORK/config.seen.$side" && return 0
    printf '%s\n' "$path" >> "$WORK/config.seen.$side"
    ref=$(side_ref "$side" "$path")
    if ! exists_at "$ref"; then
        [ "$opt" = 1 ] && return 0
        die "$path, included from the cargo config, is missing on the $side side."
    fi
    INCLUDE_SEQ=$((INCLUDE_SEQ + 1))
    inc="$WORK/config.inc.$INCLUDE_SEQ"
    toml_lines_to "$ref" "$inc.lines" "the cargo config $path ($side)"
    config_includes "$inc.lines" > "$inc" || die "cannot read the includes of $path."
    if grep -q '^<bad>' "$inc"; then
        die "cannot follow the includes of the cargo config $path ($side); judge the plugin versions by hand:
$(grep '^<bad>' "$inc" | cut -f2- | sed 's/^/    /')"
    fi
    dir=$(dirname "$path")
    while IFS="$TAB" read -r o p; do
        [ -n "$p" ] || continue
        case "$p" in
            /*|*'*'*|*'?'*|*'['*)
                die "the cargo config $path includes $p, which is outside the repository or a glob; judge the plugin versions by hand." ;;
        esac
        resolved=$(norm_path "$dir/$p")
        [ -n "$resolved" ] \
            || die "the cargo config $path includes $p, which is outside the repository; judge the plugin versions by hand."
        config_walk "$side" "$resolved" "$o" "$out"
    done < "$inc"
    printf '%s\n' "$path" >> "$out"
}

# The profile settings of a toml_lines file as "name<TAB>table<TAB>key<TAB>value": name is
# the profile, table the rest of the table name (package.<crate>, build-override), key the
# dotted key. A profile written as a top-level dotted key comes out as <unreadable>.
profile_entries() {  # <lines file>
    awk -F '\t' "$TOML_KEY_AWK"'
        {
            sec = $1; line = substr($0, length($1) + 2)
            if (sec == "<unreadable>") { print "<unreadable>\t\t\t" line; next }
            e = eq_at(line); if (!e) next
            nk = split_key(substr(line, 1, e - 1), kp); val = trim(substr(line, e + 1))
            if (sec == "") {
                if (kp[1] == "profile") print "<unreadable>\t\t\ta top-level dotted profile key: " line
                next
            }
            ns = split_key(sec, sp)
            if (sp[1] != "profile") next
            if (ns == 1) {
                if (nk < 2) { print "<unreadable>\t\t\ta key of [profile] without a profile name: " line; next }
                print kp[1] "\t\t" join_parts(kp, 2, nk) "\t" val
            } else {
                print sp[2] "\t" join_parts(sp, 3, ns) "\t" join_parts(kp, 1, nk) "\t" val
            }
        }' "$1"
}

# Writes the inheritance chain of RELEASE_PROFILE on <side> to <out> as "dist → release".
# The profiles are those of the root Cargo.toml and of the repository cargo config files,
# which win over the manifest. The chain follows inherits to release or dev; test and bench
# inherit dev and release unless they say otherwise. Stops when the chain cannot be found.
profile_chain() {  # <side> <out file>
    local side="$1" out="$2" f n=0
    : > "$WORK/chain.entries.$side"
    toml_lines_to "$(side_ref "$side" Cargo.toml)" "$WORK/chain.lines" "the root Cargo.toml ($side)"
    profile_entries "$WORK/chain.lines" >> "$WORK/chain.entries.$side" || die "cannot read the profiles ($side)."
    cargo_config_files "$side" "$WORK/chain.configs.$side"
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        n=$((n + 1))
        toml_lines_to "$(side_ref "$side" "$f")" "$WORK/chain.lines" "the cargo config $f ($side)"
        profile_entries "$WORK/chain.lines" >> "$WORK/chain.entries.$side" || die "cannot read the profiles of $f ($side)."
    done < "$WORK/chain.configs.$side"
    START="$RELEASE_PROFILE" awk -F '\t' '
        $1 == "<unreadable>" { bad = bad "\n    " $4; next }
        {
            defined[$1] = 1
            if ($2 == "" && $3 == "inherits") { v = $4; gsub(/^["\047]|["\047]$/, "", v); inh[$1] = v }
        }
        function builtin(p) { return p == "release" || p == "dev" || p == "test" || p == "bench" }
        END {
            if (bad != "") { print "<error>\tcannot read a profile:" bad; exit }
            cur = ENVIRON["START"]; chain = cur; seen[cur] = 1
            while (cur != "release" && cur != "dev") {
                if (cur in inh) nxt = inh[cur]
                else if (cur == "test") nxt = "dev"
                else if (cur == "bench") nxt = "release"
                else if (!(cur in defined)) { print "<error>\tprofile " cur " is not defined (" chain ")"; exit }
                else { print "<error>\tprofile " cur " has no inherits (" chain ")"; exit }
                if (!builtin(nxt) && !(nxt in defined)) {
                    print "<error>\tprofile " cur " inherits " nxt ", which is not defined (" chain " → " nxt ")"; exit
                }
                if (nxt in seen) { print "<error>\tthe profiles inherit in a cycle (" chain " → " nxt ")"; exit }
                seen[nxt] = 1; chain = chain " → " nxt; cur = nxt
            }
            print chain
        }' "$WORK/chain.entries.$side" > "$out" || die "cannot work out the profile chain ($side)."
    if grep -q '^<error>' "$out"; then
        die "the release profile $RELEASE_PROFILE cannot be followed on the $side side: $(cut -f2- "$out"); judge the plugin versions by hand."
    fi
    [ -s "$out" ] || die "cannot work out the profile chain ($side)."
}

# Bumps every plugin with an archive for <reason>, or notes that it is already above the
# release (the rule of a lock update).
bump_every_plugin() {  # <reason>
    local p
    for p in $(plugin_dirs); do
        if external_update_needs_bump "$p"; then
            add_dirty "$p" "$1"
        else
            note "$1: $RELEASED_NOTE; no further bump needed."
        fi
    done
}

# The profiles of the root Cargo.toml that changed, judged against the chain of both sides.
judge_profiles() {
    local name cb ca chain names
    profile_entries "$WORK/root.before" | sort > "$WORK/profiles.before" || die "cannot read the profiles."
    profile_entries "$WORK/root.after" | sort > "$WORK/profiles.after" || die "cannot read the profiles."
    if grep -q '^<unreadable>' "$WORK/profiles.before" "$WORK/profiles.after"; then
        die "cannot read a profile of the root Cargo.toml; judge the plugin versions by hand:
$(grep -h '^<unreadable>' "$WORK/profiles.before" "$WORK/profiles.after" | cut -f4- | sort -u | sed 's/^/    /')"
    fi
    names=$(comm -3 "$WORK/profiles.before" "$WORK/profiles.after" | first_field | sort -u)
    [ -n "$names" ] || return 0
    profile_chain before "$WORK/chain.before"
    profile_chain after "$WORK/chain.after"
    cb=$(cat "$WORK/chain.before"); ca=$(cat "$WORK/chain.after")
    chain="$cb"
    [ "$cb" = "$ca" ] || chain="$cb before, $ca after"
    for name in $names; do
        if printf '%s\n%s\n' "$cb" "$ca" | tr ' ' '\n' | grep -qxF "$name"; then
            bump_every_plugin "Cargo.toml [profile.$name] (release profile chain: $chain)"
        else
            note "[profile.$name] changed in Cargo.toml, not in the inheritance chain of the release profile $RELEASE_PROFILE ($chain); plugin versions are not bumped for it, see docs/ci.md."
        fi
    done
}

# Writes the build-relevant lines of a toolchain or cargo config file at <ref> to <out>: the
# toml_lines without the keys that do not change the build ([alias], [term], [net], [http]
# of a cargo config; components and profile of rust-toolchain.toml). The legacy
# rust-toolchain file is its lines without blank ones. A missing file is empty.
build_config_lines() {  # <ref> <path> <kind: config|toolchain|legacy> <out> <all lines out>
    : > "$4"; : > "$5"
    exists_at "$1" || return 0
    if [ "$3" = legacy ]; then
        git show "$1" > "$WORK/legacy.src" || die "cannot read $2."
        sed '/^[[:space:]]*$/d' "$WORK/legacy.src" > "$4"
        cp "$4" "$5"
        return 0
    fi
    toml_lines_to "$1" "$5" "$2"
    KIND="$3" awk -F '\t' "$TOML_KEY_AWK"'
        {
            sec = $1; line = substr($0, length($1) + 2)
            e = eq_at(line); nk = e ? split_key(substr(line, 1, e - 1), kp) : 0
            if (ENVIRON["KIND"] == "config") {
                if (sec != "") { split_key(sec, sp); top = sp[1] } else top = kp[1]
                if (top == "alias" || top == "term" || top == "net" || top == "http") next
            } else {
                ns = split_key(sec, sp)
                if (sec == "") { t = kp[1]; k = kp[2]; n = nk - 1 } else { t = sp[1]; k = kp[1]; n = ns - 1 + nk }
                if (t == "toolchain" && n == 1 && (k == "components" || k == "profile")) next
            }
            print
        }' "$5" > "$4" || die "cannot read $2."
}

# The toolchain files and the repository cargo config files, with what they include on
# either side. A change asks every plugin for a bump unless only the keys above changed.
judge_build_config() {
    local f kind
    cargo_config_files before "$WORK/configs.before"
    cargo_config_files after "$WORK/configs.after"
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        outside_has "$f" || continue
        case "$f" in
            rust-toolchain.toml) kind=toolchain ;;
            rust-toolchain) kind=legacy ;;
            *) kind=config ;;
        esac
        build_config_lines "$BEFORE_REV:$f" "$f" "$kind" "$WORK/bc.before" "$WORK/bc.before.all"
        build_config_lines "$(after_ref "$f")" "$f" "$kind" "$WORK/bc.after" "$WORK/bc.after.all"
        if ! cmp -s "$WORK/bc.before" "$WORK/bc.after"; then
            bump_every_plugin "$f (build settings)"
        elif ! cmp -s "$WORK/bc.before.all" "$WORK/bc.after.all"; then
            case "$kind" in
                toolchain) note "$f changed only in components or profile, which do not change the build; plugin versions are not bumped for it, see docs/ci.md." ;;
                *) note "$f changed only in [alias], [term], [net], or [http], which do not change the build; plugin versions are not bumped for it, see docs/ci.md." ;;
            esac
        fi
    done <<EOF2
$(printf '%s\n' .cargo/config.toml .cargo/config rust-toolchain.toml rust-toolchain \
    | cat - "$WORK/configs.before" "$WORK/configs.after" | awk 'NF && !seen[$0]++')
EOF2
}

# ── [patch] ──────────────────────────────────────────────────────────────────
# A [patch] entry changes where a crate comes from. Cargo.lock shows the result, and the lock
# rules above judge it (a git source each commit, crates.io once per release), so a patch
# entry adds no reason of its own: it is checked against the lock. A path patch is not
# reproducible in a clean clone or in CI and is rejected.

# The entries of the [patch.<source>] tables of a toml_lines file as
# "source<TAB>name<TAB>value", rev kept. Dotted keys (name.git = ...) and
# [patch.<source>.<name>] tables are folded into the value of <name>; a quoted source name
# loses its quotes. Other spellings ([patch] with dotted keys, a quoted "patch", a top-level
# patch key) come out as "<unparsed><TAB><TAB>line".
patch_entries() {  # <lines file>
    awk -F '\t' "$TOML_KEY_AWK"'
        function add(src, name, sk, v,    k) {
            k = src "\t" name; seen[k] = 1
            if (sk == "") plain[k] = v
            else tab[k] = tab[k] " " sk " = " v ","
        }
        {
            sec = $1; line = substr($0, length($1) + 2)
            if (sec == "<unreadable>") { print "<unparsed>\t\t" line; next }
            e = eq_at(line)
            if (sec == "") {
                if (e && split_key(substr(line, 1, e - 1), kp) && kp[1] == "patch") print "<unparsed>\t\t" line
                next
            }
            ns = split_key(sec, sp)
            if (sp[1] != "patch") next
            if (QUOTED[1] || ns == 1 || ns > 3 || !e) { print "<unparsed>\t\t[" sec "] " line; next }
            src = sp[2]; name = sp[3]
            nk = split_key(substr(line, 1, e - 1), kp); val = norm_inline(substr(line, e + 1))
            if (val == "<bad>") { print "<unparsed>\t\t[" sec "] " line; next }
            if (ns == 2) add(src, kp[1], join_parts(kp, 2, nk), val)
            else add(src, name, join_parts(kp, 1, nk), val)
        }
        END {
            for (k in seen) {
                v = plain[k]
                if (k in tab) v = v (v == "" ? "" : " + ") "{" tab[k] " }"
                print k "\t" v
            }
        }' "$1" | sort
}

# Stops when the after side declares a path [patch] in the root Cargo.toml or in a
# repository cargo config file (with its includes), or a paths override. Read on every run,
# not only when these files change, so a path patch committed earlier blocks every commit.
reject_path_patches() {
    local f bad="" what
    : > "$WORK/reject.found"
    if exists_at "$(after_ref Cargo.toml)"; then
        toml_lines "$(after_ref Cargo.toml)" > "$WORK/reject.lines" || die "cannot read the root Cargo.toml."
        if grep -q '^<unreadable>' "$WORK/reject.lines"; then
            die "cannot read the root Cargo.toml; judge the plugin versions by hand:
$(grep '^<unreadable>' "$WORK/reject.lines" | cut -f2- | sort -u | sed 's/^/    /')"
        fi
        printf 'Cargo.toml\n' > "$WORK/reject.files"
        cp "$WORK/reject.lines" "$WORK/reject.lines.1"
    else
        : > "$WORK/reject.files"
    fi
    cargo_config_files after "$WORK/reject.configs"
    cat "$WORK/reject.configs" >> "$WORK/reject.files"
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        if [ "$f" != Cargo.toml ]; then
            toml_lines_to "$(after_ref "$f")" "$WORK/reject.lines.1" "the cargo config $f"
        fi
        patch_entries "$WORK/reject.lines.1" > "$WORK/reject.patch" || die "cannot read the [patch] tables of $f."
        if grep -q '^<unparsed>' "$WORK/reject.patch"; then
            die "cannot read a [patch] table of $f, so a path patch cannot be ruled out; only [patch.<source>] tables with name = { ... } entries or dotted keys, and [patch.<source>.<name>] tables, are read:
$(grep '^<unparsed>' "$WORK/reject.patch" | cut -f3- | sed 's/^/    /')"
        fi
        what=$(F="$f" awk -F '\t' '$3 ~ /(^|[{ ,])path[[:space:]]*=/ { print "    " ENVIRON["F"] ": patch." $1 "." $2 " = " $3 }' "$WORK/reject.patch")
        [ -z "$what" ] || bad="$bad
$what"
        if [ "$f" != Cargo.toml ]; then
            what=$(F="$f" awk -F '\t' "$TOML_KEY_AWK"'
                $1 == "" { e = eq_at($2); if (e && split_key(substr($2, 1, e - 1), kp) == 1 && kp[1] == "paths") print "    " ENVIRON["F"] ": " $2 }' "$WORK/reject.lines.1")
            [ -z "$what" ] || bad="$bad
$what"
        fi
    done < "$WORK/reject.files"
    [ -z "$bad" ] || die "a path [patch] or paths override is not reproducible in a clean clone or in CI, which do not have the path:$bad
    Use git with the pinned rev, or pass the path with cargo --config as scripts/dev-bundle.sh --tasty does."
}

# The [patch] entries of the root Cargo.toml that changed, checked against Cargo.lock: the
# lock rules judge what the lock shows, so this only notes, or stops when the lock cannot
# show it. The lock rows of judge_pin_moves are used, or read here.
judge_patch_entries() {
    local src name vb va pkgs pkg rb ra n side
    patch_entries "$WORK/root.before" > "$WORK/patch.before" || die "cannot read the [patch] tables."
    patch_entries "$WORK/root.after" > "$WORK/patch.after" || die "cannot read the [patch] tables."
    if grep -q '^<unparsed>' "$WORK/patch.before" "$WORK/patch.after"; then
        die "cannot read a [patch] table of the root Cargo.toml; judge the plugin versions by hand:
$(grep -h '^<unparsed>' "$WORK/patch.before" "$WORK/patch.after" | cut -f3- | sort -u | sed 's/^/    /')"
    fi
    comm -3 "$WORK/patch.before" "$WORK/patch.after" | awk '{ sub(/^\t/, ""); print }' \
        | cut -f1-2 | sort -u > "$WORK/patch.changed"
    [ -s "$WORK/patch.changed" ] || return 0
    for side in before after; do
        [ -f "$WORK/lock.$side" ] && continue
        exists_at "$(side_ref "$side" Cargo.lock)" || die "[patch] changed, but there is no Cargo.lock on the $side side."
        lock_rows "$(side_ref "$side" Cargo.lock)" > "$WORK/lock.$side" || die "cannot read Cargo.lock ($side)."
    done
    while IFS="$TAB" read -r src name; do
        [ -n "$name" ] || continue
        vb=$(S="$src" N="$name" awk -F '\t' '$1 == ENVIRON["S"] && $2 == ENVIRON["N"] { print $3 }' "$WORK/patch.before")
        va=$(S="$src" N="$name" awk -F '\t' '$1 == ENVIRON["S"] && $2 == ENVIRON["N"] { print $3 }' "$WORK/patch.after")
        pkgs=$(printf '%s\n%s\n' "$vb" "$va" | sed -n 's/.*package[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p')
        [ -n "$(printf '%s\n%s\n' "$vb" "$va" | grep -v 'package[[:space:]]*=' | sed -n '/./p')" ] \
            && pkgs="$pkgs
$name"
        [ -n "$pkgs" ] || pkgs="$name"
        for pkg in $(printf '%s\n' "$pkgs" | sed -n '/./p' | sort -u); do
            # The pin rule pairs git packages by name only, so two git sources of one name
            # would be compared with each other.
            for side in before after; do
                n=$(P="$pkg" awk -F '\t' '$1 == ENVIRON["P"] && $3 ~ /^git\+/ { print $3 }' "$WORK/lock.$side" | sort -u | wc -l)
                [ "$n" -le 1 ] || die "[patch] $src.$name changed, and Cargo.lock ($side) has $pkg from $n git sources, which the lock rules cannot tell apart; judge the plugin versions by hand."
            done
            rb=$(P="$pkg" awk -F '\t' '$1 == ENVIRON["P"] { print $1 "\t" $2 "\t" $3 }' "$WORK/lock.before" | sort)
            ra=$(P="$pkg" awk -F '\t' '$1 == ENVIRON["P"] { print $1 "\t" $2 "\t" $3 }' "$WORK/lock.after" | sort)
            if [ -z "$rb" ] && [ -z "$ra" ]; then
                die "[patch] $src.$name changed in the root Cargo.toml, but no package $pkg is in Cargo.lock; judge the plugin versions by hand."
            fi
            if [ "$rb" != "$ra" ]; then
                note "[patch] $src.$name changed; judged through Cargo.lock ($pkg)."
            else
                note "[patch] $src.$name changed, but Cargo.lock locks $pkg the same; no bump needed for it."
            fi
        done
    done < "$WORK/patch.changed"
}

judge_root_manifest() {
    local before="$BEFORE_REV:Cargo.toml" after sec key pkg val_b val_a p
    after=$(after_ref Cargo.toml)
    if ! exists_at "$before" || ! exists_at "$after"; then
        die "the root Cargo.toml is missing on one side of the range."
    fi
    toml_lines "$before" > "$WORK/root.before" || die "cannot read the root Cargo.toml at $BEFORE_REV."
    toml_lines "$after" > "$WORK/root.after" || die "cannot read the root Cargo.toml at ${AFTER_PREFIX:-the index}."
    if grep -q '^<unreadable>' "$WORK/root.before" "$WORK/root.after"; then
        die "cannot read the root Cargo.toml; judge the plugin versions by hand:
$(grep -h '^<unreadable>' "$WORK/root.before" "$WORK/root.after" | cut -f2- | sort -u | sed 's/^/    /')"
    fi
    local profiles_changed=0 nsec
    while IFS= read -r sec; do
        # ENVIRON, not -v: awk would read the backslashes of a name as escapes.
        if cmp -s <(S="$sec" awk -F '\t' '$1 == ENVIRON["S"]' "$WORK/root.before") \
                  <(S="$sec" awk -F '\t' '$1 == ENVIRON["S"]' "$WORK/root.after"); then
            continue
        fi
        # Quotes and spaces around the dots do not change the table cargo reads.
        nsec=$(printf '%s' "$sec" | tr -d "\"' ")
        case "$nsec" in
            workspace.dependencies|workspace.dependencies.*) ;;  # by entry, below
            profile|profile.*) profiles_changed=1 ;;  # by profile, below
            patch.*) ;;  # by entry, below
            workspace.lints|workspace.lints.*) ;;
            *) die "[${sec:-top level}] changed in the root Cargo.toml; judge the plugin versions by hand." ;;
        esac
    done <<EOF2
$(cut -f1 "$WORK/root.before" "$WORK/root.after" | sort -u)
EOF2
    [ "$profiles_changed" = 0 ] || judge_profiles
    judge_patch_entries
    dep_entries "$WORK/root.before" > "$WORK/deps.before"
    dep_entries "$WORK/root.after" > "$WORK/deps.after"
    while IFS= read -r key; do
        [ -n "$key" ] || continue
        if [ "$key" = "<unparsed>" ]; then
            die "cannot read a changed line of [workspace.dependencies] in the root Cargo.toml:
$(awk -F '\t' '$1 == "<unparsed>"' "$WORK/deps.before" "$WORK/deps.after" | sort | uniq -u | cut -f2- | sed 's/^/    /')"
        fi
        val_b=$(K="$key" awk -F '\t' '$1 == ENVIRON["K"] { print $2 }' "$WORK/deps.before")
        val_a=$(K="$key" awk -F '\t' '$1 == ENVIRON["K"] { print $2 }' "$WORK/deps.after")
        pkg=$(printf '%s\n%s\n' "$val_a" "$val_b" | sed -n 's/.*package[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' | sed -n '1p')
        [ -n "$pkg" ] || pkg="$key"
        plugins_linking "$pkg"
        if [ -z "$LINKED" ]; then
            # A crate that is locked but outside every plugin closure cannot change a plugin.
            in_either_lock "$pkg" && continue
            die "[workspace.dependencies] $key changed in the root Cargo.toml, but no package $pkg is in Cargo.lock; judge the plugin versions by hand."
        fi
        for p in $LINKED; do
            add_dirty "$p" "Cargo.toml [workspace.dependencies] $key: ${val_b:-<none>} → ${val_a:-<none>}"
        done
        PIN_SUMMARY="$PIN_SUMMARY
    $key ${val_b:-<none>} → ${val_a:-<none>} (Cargo.toml [workspace.dependencies]); plugins: ${LINKED# }"
    done <<EOF2
$(comm -3 "$WORK/deps.before" "$WORK/deps.after" | first_field | sort -u)
EOF2
}

# ── Archive inputs outside the crates ────────────────────────────────────────
# The notice set, .gitattributes, and the packaging code go into or decide every plugin
# archive. scripts/lib/archive-inputs.txt lists them and how each is compared. The archive
# bytes are not reproducible (tar keeps mtimes), so the git content decides. These helpers
# write to a file and are called without command substitution, so that die ends the check.

# The list is read from the two trees that are judged, not from the running script, so that
# a commit cannot drop an input from the list and change it at once.
ARCHIVE_INPUTS=scripts/lib/archive-inputs.txt
ARCHIVE_SUMMARY=""
TAB=$(printf '\t')

# Whether OUTSIDE has <path>, or a path under it when <path> ends with "/".
outside_touches() {  # <path>
    case "$1" in
        */) P="$1" awk 'index($0, ENVIRON["P"]) == 1 { f = 1 } END { exit !f }' <<<"$OUTSIDE" ;;
        *) outside_has "$1" ;;
    esac
}

# The checksum of a packaging script at <ref>, into <out>. The text is compared as it is: a
# line that starts with # can be data of a string, a here-document, or a continued line,
# which only a shell parser could tell from a comment.
script_norm() {  # <ref> <out file>
    if ! exists_at "$1"; then printf '<absent>' > "$2"; return 0; fi
    git show "$1" | cksum > "$2" || die "cannot read $1."
}

# The checksum of the regions of <path> at <ref> that <regions> names ("how<TAB>open<TAB>close"
# lines), into <out>. Stops when the file exists but a region cannot be found or is not closed.
region_of() {  # <ref> <path> <regions file> <out file>
    local ref="$1" path="$2" regions="$3" out="$4" how open close where
    if ! exists_at "$ref"; then printf '<absent>' > "$out"; return 0; fi
    where="${ref%%:*}"; where="${where:-the index}"
    git show "$ref" > "$WORK/region.src" || die "cannot read $path at $where."
    : > "$WORK/region.out"
    while IFS="$TAB" read -r how open close; do
        [ -n "$how" ] || continue
        case "$how" in
            block) [ -n "$close" ] || die "$ARCHIVE_INPUTS: the block region $open of $path has no closing pattern." ;;
            line|top|job) ;;
            *) die "$ARCHIVE_INPUTS: unknown region kind '$how' for $path." ;;
        esac
        # ENVIRON, not -v: awk would read the backslashes of the patterns as escapes.
        if ! R_HOW="$how" R_OPEN="$open" R_CLOSE="$close" awk '
            function keep(s) { print s }
            BEGIN { how = ENVIRON["R_HOW"]; opn = ENVIRON["R_OPEN"]; cls = ENVIRON["R_CLOSE"]
                    print "@ " opn }
            # Markers and boundaries are read without a trailing comment and trailing blanks.
            { b = $0; sub(/[[:space:]]+#.*$/, "", b); sub(/[[:space:]]+$/, "", b) }
            how == "line" { if (b ~ opn) { found = 1; keep($0) } next }
            state == 0 {
                if (b ~ opn) { state = 1; found = 1; keep($0); match($0, /^ */); kin = RLENGTH }
                next
            }
            state != 1 { next }
            how == "block" {
                if (b ~ cls) { keep($0); state = 2; next }
                # A new function or array starts: this block was not closed.
                if ($0 ~ /^[A-Za-z_][A-Za-z0-9_]*(=\(|[[:space:]]*\(\))/) { bad = NR; exit }
                keep($0); next
            }
            how == "top" && /^[^[:space:]#]/ { state = 2; next }
            how == "job" && (b ~ /^  [A-Za-z0-9_-]+:$/ || /^[^[:space:]#]/) { state = 2; next }
            # Comment lines that start no deeper than the region key, and blank lines, wait in
            # held: before more region text they belong to the region, before the end of a top
            # or job region (or of the file) they belong to what follows. A deeper line can be
            # data of a block scalar and always counts.
            how != "block" && ($0 ~ /^[[:space:]]*$/ || ($0 ~ /^ *#/ && match($0, /^ */) && RLENGTH <= kin)) {
                held = held $0 "\n"; next
            }
            how != "block" && held != "" { printf "%s", held; held = "" }
            { keep($0) }
            END {
                if (!found) { print "cannot find the line " opn > "/dev/stderr"; exit 3 }
                if (how == "block" && state == 1) {
                    print "the block from " opn " is not closed by " cls (bad ? " before line " bad : "") > "/dev/stderr"
                    exit 3
                }
            }' "$WORK/region.src" >> "$WORK/region.out" 2>"$WORK/region.err"; then
            die "$path at $where: $(cat "$WORK/region.err"). The archive input regions are listed in $ARCHIVE_INPUTS; judge the plugin versions by hand or fix the list."
        fi
    done <<EOF2
$(cat "$regions")
EOF2
    cksum < "$WORK/region.out" > "$out"
}

# The list of <side> into <out> without comment and blank lines; returns 1 when the tree has
# no list.
archive_list_at() {  # <side> <out file>
    local ref
    ref=$(side_ref "$1" "$ARCHIVE_INPUTS")
    : > "$2"
    exists_at "$ref" || return 1
    git show "$ref" > "$WORK/list.src" || die "cannot read $ARCHIVE_INPUTS ($1)."
    awk '/^[[:space:]]*(#|$)/ { next } { print }' "$WORK/list.src" > "$2" \
        || die "cannot read $ARCHIVE_INPUTS ($1)."
}

# The region lines of <path> in <list> as "how<TAB>open<TAB>close", in list order.
regions_in() {  # <list file> <path> <out file>
    P="$2" awk -F '\t' '$1 == "region" && $2 == ENVIRON["P"] { print $3 "\t" $4 "\t" $5 }' "$1" > "$3" \
        || die "cannot read the region lines of $2."
}

# Checks the after side of the list on every run, like the path patch check, so that
# something committed earlier keeps blocking: a script or shell region file must source only
# files the list names, by a path written out; and a job region of a workflow may use only
# actions written as owner/repo@ref, since the list does not judge the content of others.
check_archive_structure() {
    local kind path how ref names sk ln target line
    names=$(awk -F '\t' '$1 == "blob" || $1 == "script" || $1 == "region" { print $2 }' "$WORK/list.after" | sort -u)
    # The packaging copies a blob input with cp, which follows a symbolic link, so a link would
    # let the target change without a change of the listed path.
    while IFS= read -r path; do
        [ -n "$path" ] || continue
        archive_blob_id after "$path" | awk '$2 == "120000" { print "    " $1 }' > "$WORK/structure.links" \
            || die "cannot list $path (after)."
        if [ -s "$WORK/structure.links" ]; then
            die "the archive input $path is or holds a symbolic link, which the archive would follow to a file the gate does not judge; judge the plugin versions by hand:
$(cat "$WORK/structure.links")"
        fi
    done <<EOF2
$(awk -F '\t' '$1 == "blob" { print $2 }' "$WORK/list.before" "$WORK/list.after" | sort -u)
EOF2
    while IFS="$TAB" read -r kind path how; do
        [ -n "$path" ] || continue
        ref=$(after_ref "$path")
        exists_at "$ref" || continue
        git show "$ref" > "$WORK/structure.src" || die "cannot read $path (after)."
        case "$kind" in
            region) case "$how" in job) ;; block|line) ;; *) continue ;; esac ;;
        esac
        if [ "$kind" = region ] && [ "$how" = job ]; then
            region_of "$ref" "$path" <(P="$path" awk -F '\t' '$1 == "region" && $2 == ENVIRON["P"] && $3 == "job" { print $3 "\t" $4 "\t" $5 }' "$WORK/list.after") "$WORK/structure.cksum"
            # The package job is read line by line as indentation, "- " items, a key, and a
            # value; text inside a scalar value is not read. A uses key passes only with
            # owner/repo@ref or owner/repo/path@ref as its value on the same line, quoted or
            # not, maybe with a trailing comment (U otherwise: a local action or workflow, a
            # Docker image, a value on the next line or in a block scalar). YAML forms that
            # could spell a uses key another way stop the check too (Y): an explicit key (?), a
            # double-quoted key with a backslash escape, a merge key (<<), a key or an item that
            # starts with a tag (!), an anchor (&), an alias (*), or a flow collection ({ [),
            # and a value that starts with one of these, and a quote in the place of a key (at
            # the start of a line or after "- ") that the line does not close. A one-line flow
            # sequence of plain words ([self-hosted, Linux, X64]) cannot hold a key and passes.
            # The body of a block scalar (run: |) and the rest of a quoted value over several
            # lines are data.
            awk '
                function lead(t) { match(t, /^ */); return RLENGTH }
                # The position of the quote that ends the quoted scalar at the start of t, or 0.
                function qend(t,   qc, i, c) {
                    qc = substr(t, 1, 1)
                    for (i = 2; i <= length(t); i++) {
                        c = substr(t, i, 1)
                        if (qc == "\"" && c == "\\") { i++; continue }
                        if (c == qc) {
                            if (qc == "\047" && substr(t, i + 1, 1) == "\047") { i++; continue }
                            return i
                        }
                    }
                    return 0
                }
                function y() { print "Y\t    " $0 }
                /^@ / { next }
                blk >= 0 { if ($0 ~ /^[[:space:]]*$/ || lead($0) > blk) next; blk = -1 }
                mq != "" { if (qend(mq $0)) mq = ""; next }
                /^[[:space:]]*(#|$)/ { next }
                {
                    r = $0; sub(/^ */, "", r); seq = 0
                    while (r ~ /^-([[:space:]]|$)/) { r = substr(r, 2); sub(/^[[:space:]]+/, "", r); seq = 1 }
                    col = length($0) - length(r)
                    if (r == "") next
                    if (r ~ /^\?([[:space:]]|$)/ || (seq && r ~ /^[{[&*!]/)) { y(); next }
                    # The key: a quoted scalar or plain text, followed by ": " or a final ":".
                    if (r ~ /^["\047]/) {
                        e = qend(r)
                        # A quote in the place of a key that the line does not close.
                        if (e == 0) { y(); next }
                        v = substr(r, e + 1)
                        if (v !~ /^[[:space:]]*:([[:space:]]|$)/) next
                        key = substr(r, 1, e); sub(/^[[:space:]]*:/, "", v)
                        if (key ~ /^"/ && key ~ /\\/) { y(); next }
                        name = substr(key, 2, e - 2)
                    } else if (match(r, /:([[:space:]]|$)/)) {
                        key = substr(r, 1, RSTART - 1); v = substr(r, RSTART + 1); name = key
                        sub(/[[:space:]]+$/, "", name)
                        if (key ~ /^[{[&*!]/) { y(); next }
                    } else next
                    if (name == "<<") { y(); next }
                    sub(/^[[:space:]]+/, "", v)
                    # The value: quoted, plain without a trailing comment, or a block scalar.
                    if (v ~ /^["\047]/) {
                        e = qend(v)
                        if (e == 0) { mq = substr(v, 1, 1); val = "" } else val = substr(v, 2, e - 2)
                    } else {
                        val = v; sub(/(^|[[:space:]]+)#.*$/, "", val); sub(/[[:space:]]+$/, "", val)
                        if (val ~ /^[&*!{]/ || (val ~ /^\[/ && val !~ /^\[[^][{}:&*!?"\047#]*\]$/)) { y(); next }
                    }
                    if (name == "uses" && val !~ /^[A-Za-z0-9_-][A-Za-z0-9_.-]*\/[A-Za-z0-9_.-]+(\/[^@[:space:]]+)?@[A-Za-z0-9_.\/-]+$/) {
                        print "U\t    " $0
                    }
                    # A block scalar starts: its body is the lines indented deeper than its key.
                    if (v !~ /^["\047]/ && val ~ /^[|>][-+0-9]*$/) blk = col
                }
            ' blk=-1 "$WORK/region.out" > "$WORK/structure.uses" || die "cannot read $path (after)."
            if grep -q '^U' "$WORK/structure.uses"; then
                die "a job that builds the archives in $path has a uses: value that is not owner/repo@ref or owner/repo/path@ref on its line (a local action or workflow, a Docker image, or a form the gate does not read), whose content is not judged; judge the plugin versions by hand:
$(sed -n "s/^U$TAB//p" "$WORK/structure.uses")"
            fi
            if [ -s "$WORK/structure.uses" ]; then
                die "a job that builds the archives in $path uses a YAML form the gate does not read (an explicit ? key, a double-quoted key with a backslash escape, a merge key, a tag, an anchor, an alias, a flow collection, or a quote in the place of a key that its line does not close), which could spell a uses: key another way; judge the plugin versions by hand:
$(sed -n "s/^Y$TAB//p" "$WORK/structure.uses")"
            fi
            continue
        fi
        # A source or . in command position: at the start of a line, after one of ; & | ( ) and
        # the words { and }, inside $( ), <( ), >( ) and backticks, after the ) of a case
        # pattern, after then do else elif if while until ! coproc (and its name) time exec
        # builtin command (and their - options), after an assignment (also a[i]= and +=), and
        # after a redirection. Quoted text, text after an unquoted #, the words after a
        # substitution that ends inside a word, case patterns, and here-document bodies are no
        # commands, except the substitutions in a body whose end word has no quote or
        # backslash; the word after source or . is its target, without its quotes. A command
        # word written with a backslash or with $'...' or $"..." quoting is not read: it stops
        # the check (Q), and so do a here-document without its end word (H), a quote that
        # is still open at the end of the file (O), and a substitution, arithmetic, or ${ }
        # still open there (P). A here-document starts at << or <<- outside
        # quotes, also inside a word (cat<<EOF), but not at <<< or in arithmetic ($(( )),
        # (( )), for (( ))), where << is a shift. ${ } is part of its word up to its closing
        # brace, and the substitutions inside it and inside arithmetic are read as commands.
        awk '
            # Ends a word. raw is the word as written, word the word without its quotes.
            function endword(   w, r, cop, d, dr, h) {
                if (!inword) return
                w = word; r = raw; h = hst; word = ""; raw = ""; inword = 0; hst = -1
                cop = coproc; coproc = 0
                # A here-document: its body runs from the next line to its end word. With a
                # quote or a backslash in the end word the body is data; without, only the
                # substitutions in it are read.
                if (hdnext) {
                    d = w; gsub(/\\/, "", d); hw[++hn] = d; hdash[hn] = hdnext - 1
                    hquo[hn] = (r ~ /["\047\\]/); hdnext = 0; redir = 0; return
                }
                # << or <<- anywhere outside quotes (cat<<EOF, cmd>f<<EOF), not <<<.
                if (h >= 0) {
                    d = substr(w, h + 1); sub(/^<<-?/, "", d); gsub(/\\/, "", d)
                    dr = substr(r, hrst + 1); sub(/^<<-?/, "", dr)
                    if (dr == "") hdnext = hdsh ? 2 : 1
                    else { hw[++hn] = d; hdash[hn] = hdsh; hquo[hn] = (dr ~ /["\047\\]/) }
                    w = substr(w, 1, h); r = substr(r, 1, hrst)
                    if (w ~ /^[0-9]*$/) return
                }
                if (redir) { redir = 0; return }
                if (want) { print "S\t" NR "\t" w "\t" $0; want = 0; cmd = 0; return }
                # The words of a case statement up to in, and its patterns up to ), are no commands.
                if (pat) { if (w == "esac") { pat = 0; cases--; cmd = 0 } return }
                if (casehead) { if (w == "in") { casehead = 0; pat = 1; cases++ } return }
                if (!cmd) return
                # A redirection before the command, with its target attached or next.
                if (r ~ /^[0-9]*(<<<|<>|<&|>&|>>|>\||&>>|&>|<|>)/) {
                    if (r ~ /^[0-9]*(<<<|<>|<&|>&|>>|>\||&>>|&>|<|>)$/) redir = 1
                    return
                }
                if (opts && w ~ /^-/) return
                opts = 0
                if (w ~ /^[A-Za-z_][A-Za-z0-9_]*(\[[^]]*\])?\+?=/) return
                if (r ~ /\\|\$\047|\$"/) { print "Q\t" NR "\t" r "\t" $0; cmd = 0; return }
                if (w == "source" || w == ".") { want = 1; cmd = 0; return }
                if (w ~ /^(time|command|exec|builtin)$/) { opts = 1; return }
                if (w == "coproc") { coproc = 1; return }
                if (w == "case") { casehead = 1; cmd = 0; return }
                if (w == "esac" && cases > 0) { cases--; cmd = 0; return }
                if (w ~ /^(then|do|else|elif|if|while|until|!|\{|\})$/) return
                if (cop && w ~ /^[A-Za-z_][A-Za-z0-9_]*$/) return
                cmd = 0
            }
            # After a separator a command starts.
            function sep() { want = 0; opts = 0; redir = 0; coproc = 0; cmd = 1 }
            # $( ), <( ), >( ), and backticks: the text inside is commands; the substitution is
            # part of the word around it, and the state outside comes back after it. A new line
            # inside it starts a command, as at the top level. $(( )) and (( )) are arithmetic
            # (A): only the command substitutions inside are read, and a new line starts nothing.
            function opensub(t) {
                sd++; st[sd] = t; sl[sd] = NR; pc[sd] = 0; bd[sd] = 0; sw[sd] = word; sr[sd] = raw
                scmd[sd] = cmd; swant[sd] = want; sopts[sd] = opts; sredir[sd] = redir; shst[sd] = hst
                word = ""; raw = ""; inword = 0; hst = -1; sep()
            }
            function closesub(arith) {
                if (arith) { word = ""; raw = ""; inword = 0 } else endword()
                word = sw[sd] "$()"; raw = sr[sd] "$()"; inword = 1; hst = shst[sd]
                cmd = scmd[sd]; want = swant[sd]; opts = sopts[sd]; redir = sredir[sd]; sd--
            }
            BEGIN { hst = -1; sd = 0 }
            # A line of a here-document body outside the substitutions in it: its end word,
            # data (a quoted end word), or text whose substitutions are read (below).
            hmode && sd == hbase && bd[sd] == 0 {
                l = $0
                if (hdash[hi]) sub(/^\t+/, "", l)
                if (l == hw[hi]) {
                    if (++hi > hn) { hmode = 0; hn = 0; word = ""; raw = ""; inword = 0 }
                    next
                }
                if (hquo[hi]) next
            }
            {
                s = $0; n = length(s)
                if (q == "" && !cont && !(sd > 0 && st[sd] == "A")) { cmd = 1; want = 0; opts = 0; redir = 0; coproc = 0 }
                cont = 0
                for (i = 1; i <= n; i++) {
                    c = substr(s, i, 1)
                    c2 = substr(s, i + 1, 1); c3 = substr(s, i + 2, 1)
                    # The body of a here-document with an unquoted end word: quotes are text, a
                    # backslash escapes the next character, and $( ), $(( )), backticks, and ${ }
                    # are read as outside quotes. The body goes on where they close.
                    if (hmode && sd == hbase) {
                        if (c == "\\") { i++; continue }
                        if (c == "$" && c2 == "{") { if (!bd[sd]) bl[sd] = NR; bd[sd]++; i++; continue }
                        if (c == "$" && c2 == "(") { i++; if (c3 == "(") { i++; opensub("A") } else opensub("("); continue }
                        if (c == "`") { opensub("`"); continue }
                        if (bd[sd] > 0 && c == "}") bd[sd]--
                        continue
                    }
                    if (q == "\047") { raw = raw c; if (c == "\047") q = ""; else word = word c; continue }
                    if (q == "\"") {
                        raw = raw c
                        if (c == "\\" && i < n) { i++; raw = raw substr(s, i, 1); word = word "\\" substr(s, i, 1); continue }
                        if (c == "\"") q = ""; else word = word c
                        continue
                    }
                    if (c == "\\") {
                        if (i == n) { cont = 1; break }
                        i++; word = word "\\" substr(s, i, 1); raw = raw "\\" substr(s, i, 1); inword = 1; continue
                    }
                    if (c == "\047" || c == "\"") { q = c; qline = NR; raw = raw c; inword = 1; continue }
                    # Arithmetic: parentheses are counted, << is a shift.
                    if (sd > 0 && st[sd] == "A") {
                        if (c == "$" && c2 == "(") { i++; if (c3 == "(") { i++; opensub("A") } else opensub("("); continue }
                        if (c == "`") { opensub("`"); continue }
                        if (c == "(") { pc[sd]++; continue }
                        if (c == ")") { if (pc[sd] > 0) pc[sd]--; else if (c2 == ")") { i++; closesub(1) } }
                        continue
                    }
                    # ${ }: part of the word, up to its closing brace; substitutions inside are read.
                    if (c == "$" && c2 == "{") { if (!bd[sd]) bl[sd] = NR; bd[sd]++; word = word "${"; raw = raw "${"; inword = 1; i++; continue }
                    if (bd[sd] > 0) {
                        if (c == "}") bd[sd]--
                        else if (c == "$" && c2 == "(") {
                            word = word c; raw = raw c; inword = 1; i++
                            if (c3 == "(") { i++; opensub("A") } else opensub("(")
                            continue
                        }
                        else if (c == "`") { opensub("`"); continue }
                        word = word c; raw = raw c; continue
                    }
                    if (c == "#" && !inword) break
                    if (c == " " || c == "\t") { endword(); continue }
                    # >& <& >| belong to a redirection word; &> and &>> start one.
                    if ((c == "&" || c == "|") && inword && raw ~ /[<>]$/) { word = word c; raw = raw c; continue }
                    if (c == "&" && c2 == ">") { endword(); word = c; raw = c; inword = 1; continue }
                    # << outside quotes starts a here-document, <<< does not.
                    if (c == "<" && c2 == "<" && c3 != "<" && raw !~ /<$/) { hst = length(word); hrst = length(raw); hdsh = (c3 == "-") }
                    if (c == "(" && inword && raw ~ /\$$/ && c2 == "(") { i++; opensub("A"); continue }
                    if (c == "(" && !inword && c2 == "(") { i++; opensub("A"); continue }
                    if (c == "(" && inword && raw ~ /[$<>]$/) { opensub("("); continue }
                    if (c == "`") { if (sd > 0 && st[sd] == "`") closesub(); else opensub("`"); continue }
                    if (c == ")" && pat) { endword(); pat = 0; sep(); continue }
                    if (c == ")" && sd > 0 && st[sd] == "(" && pc[sd] == 0) { closesub(); continue }
                    if (c == ";") {
                        endword()
                        if (c2 == ";") { i++; if (substr(s, i + 1, 1) == "&") i++; if (cases > 0) pat = 1 }
                        else if (c2 == "&") { i++; if (cases > 0) pat = 1 }
                        sep(); continue
                    }
                    if (c == "|" || c == "(") {
                        endword()
                        if (pat) continue
                        if (c == "(" && sd > 0) pc[sd]++
                        sep(); continue
                    }
                    if (c == ")") { endword(); if (sd > 0) pc[sd]--; sep(); continue }
                    if (c == "&") { endword(); sep(); continue }
                    word = word c; raw = raw c; inword = 1
                }
                if (q == "" && !cont && !(sd > 0 && st[sd] == "A") && bd[sd] == 0 && !(hmode && sd == hbase)) {
                    endword(); if (hn > 0 && !hdnext && !hmode) { hmode = 1; hi = 1; hbase = sd }
                }
            }
            END {
                if (hmode) print "H\t" NR "\t" hw[hi] "\t"
                if (q != "") print "O\t" qline "\t" q "\t"
                # A substitution, arithmetic, or ${ } still open at the end of the file.
                if (sd > 0) print "P\t" sl[1] "\t" (st[1] == "A" ? "((" : st[1] == "`" ? "`" : "(") "\t"
                else if (bd[0] > 0) print "P\t" bl[0] "\t${\t"
            }
        ' "$WORK/structure.src" > "$WORK/structure.sources" || die "cannot read $path (after)."
        while IFS="$TAB" read -r sk ln target line; do
            [ -n "$ln" ] || continue
            if [ "$sk" = H ]; then
                die "$path has a here-document whose end word $target is not found by the end of the file, so the source check cannot tell its text from commands; judge the plugin versions by hand."
            fi
            if [ "$sk" = O ]; then
                die "$path line $ln opens a quote ($target) that is not closed by the end of the file, so the source check cannot tell text from commands; judge the plugin versions by hand."
            fi
            if [ "$sk" = P ]; then
                die "$path line $ln opens $target that is not closed by the end of the file, so the source check cannot tell where its commands end; judge the plugin versions by hand."
            fi
            if [ "$sk" = Q ]; then
                die "$path line $ln has a command word written with a backslash or with \$'...' or \$\"...\" quoting ($target), which the source check does not read; write it plainly or judge the plugin versions by hand:
    $line"
            fi
            target=${target#\"}; target=${target%\"}; target=${target#\'}; target=${target%\'}
            target=${target#./}
            case "$target" in
                *[!A-Za-z0-9_./-]*|'')
                    die "$path line $ln sources a file whose path is not written out, so the list cannot name it; judge the plugin versions by hand:
    $line" ;;
            esac
            printf '%s\n' "$names" | grep -qxF "$target" \
                || die "$path line $ln sources $target, which $ARCHIVE_INPUTS does not list, so its changes would not be judged; add it to the list:
    $line"
        done < "$WORK/structure.sources"
    done <<EOF2
$(awk -F '\t' '$1 == "script" || $1 == "region" { print $1 "\t" $2 "\t" $3 }' "$WORK/list.after" | awk '!seen[$0]++')
EOF2
}

# The inputs of the lists of both sides that changed. blob and script lines count from either
# list, so a line that one side drops is still judged. The region lines of a path are compared
# first: when they differ between the lists, the path counts as changed. A side without a
# list uses the list of the other side; without a list on either side nothing is judged here.
judge_archive_inputs() {
    local kind path changed="" seen=" " a b p f has_b=1 has_a=1
    # A file directly in SCAN_ROOT belongs to no crate. crates/.gitattributes decides how the
    # files of every crate check out, so such a file counts as an archive input.
    while IFS= read -r f; do
        case "$f" in "$SCAN_ROOT"/*/*) ;; "$SCAN_ROOT"/?*) changed="$changed $f" ;; esac
    done <<EOF2
$CHANGED
EOF2
    archive_list_at before "$WORK/list.before" || has_b=0
    # An earlier format of the list left parts of a script out with except lines. Dropped on the
    # before side, they only widen the comparison; on the after side they are an unknown kind.
    awk -F '\t' '$1 != "except"' "$WORK/list.before" > "$WORK/list.before.kept" \
        || die "cannot read $ARCHIVE_INPUTS (before)."
    mv "$WORK/list.before.kept" "$WORK/list.before" || die "cannot read $ARCHIVE_INPUTS (before)."
    archive_list_at after "$WORK/list.after" || has_a=0
    if [ "$has_b" = 0 ] && [ "$has_a" = 1 ]; then cp "$WORK/list.after" "$WORK/list.before"; fi
    if [ "$has_a" = 0 ] && [ "$has_b" = 1 ]; then cp "$WORK/list.before" "$WORK/list.after"; fi
    check_archive_structure
    while IFS="$TAB" read -r kind path _; do
        [ -n "$kind" ] || continue
        [ -n "$path" ] || die "$ARCHIVE_INPUTS: a $kind line without a path."
        case "$seen" in *" $kind:$path "*) continue ;; esac
        seen="$seen$kind:$path "
        case "$kind" in
            blob)
                outside_touches "$path" || continue
                a=$(archive_blob_id before "$path"); b=$(archive_blob_id after "$path") ;;
            script)
                outside_touches "$path" || continue
                script_norm "$BEFORE_REV:$path" "$WORK/archive.before"
                script_norm "$(after_ref "$path")" "$WORK/archive.after"
                a=$(cat "$WORK/archive.before"); b=$(cat "$WORK/archive.after") ;;
            region)
                regions_in "$WORK/list.before" "$path" "$WORK/regions.before"
                regions_in "$WORK/list.after" "$path" "$WORK/regions.after"
                if ! cmp -s "$WORK/regions.before" "$WORK/regions.after"; then
                    a=list-before; b=list-after
                else
                    outside_touches "$path" || continue
                    region_of "$BEFORE_REV:$path" "$path" "$WORK/regions.after" "$WORK/archive.before"
                    region_of "$(after_ref "$path")" "$path" "$WORK/regions.after" "$WORK/archive.after"
                    a=$(cat "$WORK/archive.before"); b=$(cat "$WORK/archive.after")
                fi ;;
            *) die "$ARCHIVE_INPUTS: unknown kind '$kind' for $path." ;;
        esac
        case "$changed " in *" $path "*) continue ;; esac
        [ "$a" = "$b" ] || changed="$changed $path"
    done <<EOF2
$(cat "$WORK/list.before" "$WORK/list.after")
EOF2
    [ -n "$changed" ] || return 0
    changed=${changed# }
    for p in $(plugin_dirs); do
        if external_update_needs_bump "$p"; then
            for path in $changed; do add_dirty "$p" "$path (archive input)"; done
        else
            note "archive inputs changed ($changed): $RELEASED_NOTE; no further bump needed."
        fi
    done
    ARCHIVE_SUMMARY="$changed"
}

reject_path_patches
# The root manifest first: its [patch] check stops on a lock that the pin rule would misread.
if outside_has Cargo.toml; then
    judge_root_manifest
fi
if outside_has Cargo.lock; then
    judge_pin_moves
    judge_external_lock
fi
judge_build_config
judge_archive_inputs
if [ -n "$ARCHIVE_SUMMARY" ]; then
    printf '[plugin-version] archive inputs that changed, which go into every plugin archive: %s\n' \
        "$ARCHIVE_SUMMARY" >&2
fi
if [ -n "$PIN_SUMMARY" ]; then
    printf '[plugin-version] crates that changed in a plugin dependency closure:%s\n' "$PIN_SUMMARY" >&2
fi
[ -z "$NOTES" ] || printf '%s\n' "${NOTES#
}" >&2

crate_dir_of() {
    local r
    for r in $EXTRA_ROOTS; do
        case "$1" in "$r"/*) printf '%s' "$r"; return ;; esac
    done
    printf '%s' "$1" | sed -n "s|^\\(${SCAN_ROOT}/[^/]*\\)/.*\$|\\1|p"
}

VIOLATIONS=0
CONSIDERED=0
BEHIND=0

CHANGED_CRATES=""
while IFS= read -r f; do
    [ -z "$f" ] && continue
    # A folder in SCAN_ROOT without a Cargo.toml on either side is no crate, so no plugin
    # version would cover a change in it.
    case "$f" in
        "$SCAN_ROOT"/*/*)
            c=${f#"$SCAN_ROOT"/}; c="$SCAN_ROOT/${c%%/*}"
            exists_at "$BEFORE_REV:$c/Cargo.toml" || exists_at "$(after_ref "$c/Cargo.toml")" \
                || die "$f is in $c, which has no Cargo.toml on either side, so it belongs to no crate and no plugin version covers it; judge the plugin versions by hand."
            ;;
    esac
    build_affecting "$f" || continue
    # Files directly in SCAN_ROOT are judged as archive inputs above.
    case "$f" in "$SCAN_ROOT"/*/*) ;; "$SCAN_ROOT"/*) continue ;; esac
    c=$(crate_dir_of "$f")
    [ -n "$c" ] || die "cannot tell which crate $f belongs to; judge the plugin versions by hand."
    CHANGED_CRATES="$CHANGED_CRATES
$c"
done <<EOF2
$CHANGED
EOF2
CHANGED_CRATES=$(printf '%s\n' "$CHANGED_CRATES" | sed -n '/./p' | sort -u)

SHARED_CHANGED=""
for c in $CHANGED_CRATES; do
    [ -f "$ROOT/$c/tasty-plugin.toml" ] || SHARED_CHANGED="$SHARED_CHANGED $c"
done

PLUGINS=""
for c in $CHANGED_CRATES; do
    [ -f "$ROOT/$c/tasty-plugin.toml" ] && PLUGINS="$PLUGINS $c"
done

# cargo tree reads the current work tree, not the graph of each past commit in the range.
# Added or removed dependencies can make the judgement wider or narrower than it was then.
if [ -n "${SHARED_CHANGED// /}" ]; then
    for man in "$ROOT/$SCAN_ROOT"/*/tasty-plugin.toml; do
        [ -f "$man" ] || continue
        pdir=$(dirname "$man"); pname=$(basename "$pdir"); prel="$SCAN_ROOT/$pname"
        case " $PLUGINS " in *" $prel "*) continue ;; esac
        for c in $SHARED_CHANGED; do
            if links_crate "$pname" "$c"; then PLUGINS="$PLUGINS $prel"; break; fi
        done
    done
fi
EXTRA_PLUGINS=$(printf '%s\n' "$EXTRA_DIRTY" | awk -F '\t' 'NF { print $1 }')
# shellcheck disable=SC2086
PLUGINS=$(printf '%s\n' $PLUGINS $EXTRA_PLUGINS | sed -n '/./p' | sort -u)

if [ -n "$PLUGINS" ]; then
    # The root Cargo.toml has no [package] here, so rustfmt takes the edition the crates declare.
    # They must agree on one value. Read only when there are plugins to compare.
    RUST_EDITION=$(
        for f in "$SCAN_ROOT"/*/Cargo.toml; do
            [ -f "$f" ] || continue
            sed -n 's/^edition[[:space:]]*=[[:space:]]*"\([0-9]*\)".*/\1/p' "$f" | sed -n '1p'
        done | sort -u
    )
    case "$RUST_EDITION" in
        ''|*[!0-9]*) die "$SCAN_ROOT/*/Cargo.toml must declare one edition (found: $(printf '%s' "$RUST_EDITION" | tr '\n' ' '))" ;;
    esac
fi

# Reasons recorded for a plugin outside its own files, one per line.
extra_reasons_of() {
    printf '%s\n' "$EXTRA_DIRTY" | P="$1" awk -F '\t' '$1 == ENVIRON["P"] { print "      " $2 }'
}

INVOLVED=$(printf '%s\n' "$CHANGED_CRATES" | sed -n '/./p' | sort -u)
if [ "$MODE" = staged ]; then
    AFTER_TREE=$(git write-tree) || die "cannot write the index tree."
else
    AFTER_TREE="$AFTER"
fi
# Pass each path in INVOLVED as its own argument.
# shellcheck disable=SC2086
materialize before "$BEFORE_REV" $INVOLVED
# shellcheck disable=SC2086
materialize after "$AFTER_TREE" $INVOLVED

for base in $PLUGINS; do
    man="$base/tasty-plugin.toml"
    exists_at "$(after_ref "$man")" || continue
    exists_at "$BEFORE_REV:$man" || continue

    scope="$base"
    if [ -n "${SHARED_CHANGED// /}" ]; then
        for c in $SHARED_CHANGED; do
            if links_crate "$(basename "$base")" "$c"; then scope="$scope $c"; fi
        done
    fi
    files=""
    for d in $scope; do
        files="$files
$(printf '%s\n' "$CHANGED" | sed -n "s|^\($d/.*\)$|\1|p")"
    done
    files=$(printf '%s\n' "$files" | sed -n '/./p' | sort -u)
    content_changed=0
    changed_list=""
    for f in $files; do
        build_affecting "$f" || continue
        a=$(shipped_sum before "$BEFORE_REV:$f" "$f")
        b=$(shipped_sum after "$(after_ref "$f")" "$f")
        if [ "$a" != "$b" ]; then
            content_changed=1
            changed_list="$changed_list
      $f"
        fi
    done
    extra=$(extra_reasons_of "$base")
    if [ -n "$extra" ]; then
        content_changed=1
        changed_list="$changed_list
$extra"
    fi
    [ "$content_changed" = 1 ] || continue

    CONSIDERED=$((CONSIDERED + 1))
    vb=$(version_at "$BEFORE_REV:$man")
    va=$(version_at "$(after_ref "$man")")
    if [ -z "$vb" ] || [ -z "$va" ]; then
        printf '✘ [plugin-version] %s: cannot read the version from the manifest (before=%s after=%s)\n' \
            "$base" "${vb:-<none>}" "${va:-<none>}" >&2
        VIOLATIONS=$((VIOLATIONS + 1))
        continue
    fi
    # A lower version cannot be told apart from a wrong comparison base, so it is undecidable.
    if ver_gt "$vb" "$va"; then
        printf '▲ [plugin-version] %s: the start of this range is newer than its end (%s → %s)\n' \
            "$base" "$vb" "$va" >&2
        printf '    This check alone cannot tell a wrong base from a deliberate downgrade:\n' >&2
        printf '      (a) you are behind: the base (%s) is ahead of where the work started.\n' \
            "$BEFORE_REV" >&2
        printf '          Check whether a shared ref with other changes was used as the base.\n' >&2
        printf '          Run again from the exact commit the work started at:\n' >&2
        printf '            %s --range <start commit> HEAD\n' \
            "scripts/check-plugin-version-bump.sh" >&2
        printf '      (b) it was lowered on purpose: review the downgrade separately.\n' >&2
        printf '    Do not just raise the version before checking the base.\n' >&2
        printf '      Compare the released version with the work first.\n' >&2
        BEHIND=$((BEHIND + 1))
    elif ! ver_gt "$va" "$vb"; then
        printf '✘ [plugin-version] %s: shipped content changed but the version did not go up (%s → %s)\n' \
            "$base" "$vb" "$va" >&2
        printf '    changed:%s\n' "$changed_list" >&2
        printf '    fix: patch +1 on the version of %s and %s, to the same value.\n' \
            "$base/Cargo.toml" "$man" >&2
        [ -z "$VIOLATIONS_OUT" ] || printf '%s\n' "$base" >> "$VIOLATIONS_OUT"
        if [ "$MODE" = staged ]; then
            printf '    This check compares the index with %s. Before a release, also check\n' \
                "$BEFORE_REV" >&2
            printf '      every change since the last push. When one piece of work is split over\n' >&2
            printf '      several commits, a later content change can ask for another bump even\n' >&2
            printf '      though an earlier commit already raised the version. Either:\n' >&2
            printf '        (a) fold this change into the earlier commit that bumped, or\n' >&2
            printf '        (b) bump once more and ask the person merging to confirm the final version.\n' >&2
            printf '      To see the release range: %s --range <last push> HEAD\n' \
                "scripts/check-plugin-version-bump.sh" >&2
            printf '      Before a commit the staged content is checked, before a push the range\n' >&2
            printf '        after the remote; the two use different bases.\n' >&2
            printf '        The pre-push hook (B.9) checks again from the remote tip.\n' >&2
        fi
        VIOLATIONS=$((VIOLATIONS + 1))
    fi
done

# When every candidate fails, the range itself may be wrong, so say so.
flagged=$((VIOLATIONS + BEHIND))
if [ "$CONSIDERED" -ge 2 ] && [ "$flagged" -eq "$CONSIDERED" ]; then
    printf '[plugin-version] all %d judged plugins failed. They may all have changed, or the\n' \
        "$CONSIDERED" >&2
    printf '    range may be wrong; check the base (BEFORE=%s).\n' "$BEFORE_REV" >&2
fi

if [ "$VIOLATIONS" -gt 0 ]; then
    printf '[plugin-version] %d violations / %d judged' "$VIOLATIONS" "$CONSIDERED" >&2
    [ "$BEHIND" -gt 0 ] && printf ' (and %d with a reversed range)' "$BEHIND" >&2
    printf '\n' >&2
    exit 1
fi
if [ "$BEHIND" -gt 0 ]; then
    printf '[plugin-version] undecidable — %d with a reversed range / %d judged.\n' \
        "$BEHIND" "$CONSIDERED" >&2
    printf '    Nothing was measured, so do not read this as a pass.\n' >&2
    exit 2
fi
# Files of path dependencies outside the workspace are counted apart from files under crates.
n_all=$(printf '%s\n' "$CHANGED" | sed -n '/./p' | wc -l)
n_extra=$(printf '%s\n' "$CHANGED" | sed -n "/^${SCAN_ROOT}\//!{/./p;}" | wc -l)
# Nothing to judge is normal and exits 0, but nothing was checked, so it is not called a pass.
if [ "$CONSIDERED" -eq 0 ]; then
    printf '[plugin-version] nothing to judge — 0 judged (of %d changed files under crates). No plugin content changed that needs a version check.\n' \
        "$n_all"
elif [ "$n_extra" -gt 0 ]; then
    printf '[plugin-version] pass — %d judged (of %d changed files under crates and %d path dependency files outside the workspace)\n' \
        "$CONSIDERED" "$((n_all - n_extra))" "$n_extra"
else
    printf '[plugin-version] pass — %d judged (of %d changed files under crates)\n' \
        "$CONSIDERED" "$n_all"
fi
exit 0
