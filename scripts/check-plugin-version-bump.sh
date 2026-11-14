#!/usr/bin/env bash
# Decides whether a plugin version must go up by comparing the content of each plugin and of
# the path dependencies in this repository that it links. Rust is compared without test-only
# code and after rustfmt, so comment changes can still count as content.
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
        echo "[plugin-version] skip: no commit to compare with (first commit). 0 judged."
        exit 0
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
closure_of() {
    local pname="$1" cache="$WORK/closure.$1"
    CLOSURE_FILE="$cache"
    CLOSURE_PATHS="$cache.paths"
    [ -f "$cache" ] && return 0
    if ! cargo tree -p "$pname" -e normal,build --prefix none --offline 2>"$cache.err" \
        > "$cache.raw"; then
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

# Rust is compared after the judge and rustfmt, without blank lines. Without a result from a
# step the previous content is used.
shipped_sum() {
    local label="$1" ref="$2" path="$3" out fmt cooked vline
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
        */Cargo.toml|*/tasty-plugin.toml)
            # Leave the version line out so that a bump does not ask for another bump.
            if ! exists_at "$ref"; then printf '<absent>'; return; fi
            out=$(git show "$ref")
            vline=$(printf '%s' "$out" | version_line_no)
            [ -n "$vline" ] && out=$(printf '%s' "$out" | sed "${vline}d")
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

if [ "$MODE" = staged ]; then
    CHANGED=$(git diff --cached --name-only --diff-filter=ACMRD -- "$SCAN_ROOT/")
    OUTSIDE=$(git diff --cached --name-only --diff-filter=ACMRD -- . ":(exclude)$SCAN_ROOT/")
else
    CHANGED=$(git diff --name-only --diff-filter=ACMRD "$BEFORE" "$AFTER" -- "$SCAN_ROOT/")
    OUTSIDE=$(git diff --name-only --diff-filter=ACMRD "$BEFORE" "$AFTER" -- . ":(exclude)$SCAN_ROOT/")
fi

# src/lang/assets are compared whatever the extension. When this scope changes, compare it with
# the CI path filter as well.
build_affecting() {
    case "$1" in
        */src/*|*/lang/*|*/assets/*|*/Cargo.toml|*/tasty-plugin.toml|*/build.rs) return 0 ;;
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

# Plugins that must be judged for a reason outside their files: directory and reason, one per line.
EXTRA_DIRTY=""

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
    build_affecting "$f" || continue
    CHANGED_CRATES="$CHANGED_CRATES
$(crate_dir_of "$f")"
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
    printf '%s\n' "$EXTRA_DIRTY" | awk -F '\t' -v p="$1" '$1 == p { print "      " $2 }'
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
