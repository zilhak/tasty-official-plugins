# Finds a judge tool for a script that sources this file. resolve_judge returns the result
# in JUDGE_BIN, not on stdout. A path in the named environment variable comes first; then
# the cache of scripts/install-judge.sh, then target/debug and target/release of the
# repository. --check-fresh confirms the tool matches its source. Nothing here runs Cargo.
# shellcheck shell=bash

# The judge's source folder in tasty. It is absent here; the installed judge then reports
# undecidable freshness (rc 3), which is accepted.
# shellcheck disable=SC2034
JUDGE_SRC_DIR="crates/tasty-doc-guards/src"

# shellcheck source=scripts/lib/judge-cache.sh
. "$(dirname "${BASH_SOURCE[0]}")/judge-cache.sh"

resolve_judge() {
    # Clear first so that a failed call does not return an earlier result.
    JUDGE_BIN=""
    _jb_name="$1"; _jb_env="$2"; _jb_root="$3"
    eval "_jb_bin=\${$_jb_env:-}"
    # A wrong explicit path is not replaced by a default one.
    if [ -n "$_jb_bin" ] && [ ! -x "$_jb_bin" ]; then
        echo "[judge] $_jb_env does not point at an executable: $_jb_bin" >&2
        return 0
    fi
    if [ -z "$_jb_bin" ]; then
        _jb_cache=$(judge_cache_dir "$_jb_root" 2>/dev/null) || _jb_cache=""
        for _jb_cand in ${_jb_cache:+"$_jb_cache/bin/$_jb_name"} \
            "$_jb_root/target/debug/$_jb_name" "$_jb_root/target/release/$_jb_name"; do
            if [ -x "$_jb_cand" ]; then _jb_bin="$_jb_cand"; break; fi
        done
    fi
    if [ -z "$_jb_bin" ]; then
        echo "[judge] $_jb_name is missing. Install it: bash scripts/install-judge.sh" >&2
        return 0
    fi
    # Accept rc 0 and 3 from --check-fresh. Refuse a tool without it or any other error.
    if "$_jb_bin" --check-fresh "$_jb_root" >/dev/null 2>&1; then
        _jb_rc=0
    else
        _jb_rc=$?
    fi
    if [ "$_jb_rc" -eq 1 ]; then
        echo "[judge] $_jb_name was not built from the current source; refusing to use results from older source." >&2
        echo "        Install it again: bash scripts/install-judge.sh" >&2
        return 0
    fi
    if [ "$_jb_rc" -ne 0 ] && [ "$_jb_rc" -ne 3 ]; then
        echo "[judge] cannot tell whether $_jb_name matches its source (exit code $_jb_rc); treating it as missing." >&2
        return 0
    fi
    # shellcheck disable=SC2034
    JUDGE_BIN="$_jb_bin"
}
