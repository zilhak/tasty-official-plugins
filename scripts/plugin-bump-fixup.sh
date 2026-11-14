#!/usr/bin/env bash
# Raises the patch version of every plugin that the HEAD commit changed and folds the bump
# into HEAD (git commit --amend). The decision is check-plugin-version-bump.sh --range HEAD^ HEAD.
# When the versions already went up it does nothing.
# The merge step runs it per commit: git rebase --exec 'bash scripts/plugin-bump-fixup.sh' <base>
# See docs/branching.md. Exit code: done or nothing to do 0, failed 1, undecidable 2.
# Ported from tasty scripts/plugin-bump-fixup.sh.

set -uo pipefail

TAG='[plugin-bump-fixup]'
die() { printf '%s undecidable: %s\n' "$TAG" "$*" >&2; exit 2; }
fail() { printf '%s %s\n' "$TAG" "$*" >&2; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CHECK="$SCRIPT_DIR/check-plugin-version-bump.sh"
[ -f "$CHECK" ] || die "the check script is missing: $CHECK"

git rev-parse --git-dir >/dev/null 2>&1 || die "run it inside a git repository."
# A bare repository has no work tree root.
if ! ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || ! cd "$ROOT"; then
    die "cannot move to the work tree root (a bare repository?)."
fi

[ "$#" -eq 0 ] || die "it takes no arguments (it handles the HEAD commit only)."
git rev-parse --verify --quiet 'HEAD^{commit}' >/dev/null || die "there is no HEAD commit."
if ! git rev-parse --verify --quiet 'HEAD^1^{commit}' >/dev/null; then
    printf '%s the commit has no parent, so there is no earlier version to compare with. Nothing to do.\n' "$TAG"
    exit 0
fi
git rev-parse --verify --quiet 'HEAD^2' >/dev/null && die "it does not handle merge commits."
# Go on only with clean tracked files, so the amend does not pick up other changes.
if ! git diff --quiet || ! git diff --cached --quiet; then
    die "there are uncommitted changes to tracked files. Clean them up and run it again."
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

bash "$CHECK" --range HEAD^ HEAD --violations-out "$WORK/violations"
rc=$?
case "$rc" in
    0) printf '%s no plugin version to bump.\n' "$TAG"; exit 0 ;;
    1) ;;
    *) die "the version check ended with exit code $rc. Fix the cause above first." ;;
esac
[ -s "$WORK/violations" ] \
    || fail "the version check reported violations but the list of plugins to bump is empty. See the messages above."

# One definition of the version line, shared by the lookup and the rewrite. It has no BRE- or
# ERE-only syntax, so grep, sed and awk read it the same way.
VERSION_LINE_RE='^version[[:space:]]*=[[:space:]]*"'
first_version_line() { awk -v re="$VERSION_LINE_RE" '$0 ~ re { print NR; exit }' "$1"; }
version_in() {
    local n
    n=$(first_version_line "$1")
    [ -n "$n" ] || return 0
    sed -n "${n}s/${VERSION_LINE_RE}\([^\"]*\)\".*/\1/p" "$1"
}
# Rewrites the value of the first version line. awk and a temporary file instead of sed -i,
# which BSD sed (macOS) reads differently. cat keeps the file mode, links and inode.
set_version_in() {  # <file> <new>
    local n tmp
    n=$(first_version_line "$1")
    [ -n "$n" ] || return 1
    tmp=$(mktemp "$WORK/ver.XXXXXX") || return 1
    awk -v n="$n" -v v="$2" -v re="$VERSION_LINE_RE" '
        NR == n && match($0, re) {
            head = substr($0, 1, RLENGTH); rest = substr($0, RLENGTH + 1)
            sub(/^[^"]*"/, "", rest)
            $0 = head v "\"" rest
        }
        { print }' "$1" > "$tmp" || return 1
    cat "$tmp" > "$1"
}
package_name() {
    awk '/^\[/ { in_pkg = ($0 == "[package]") } in_pkg && /^name[[:space:]]*=/ {
        sub(/^name[[:space:]]*=[[:space:]]*"/, ""); sub(/".*/, ""); print; exit }' "$1"
}

# Check the whole list before changing anything, so a stop at a later plugin leaves no edits behind.
PLAN_BASE=()
PLAN_OLD=()
PLAN_NEW=()
NAMES=()
while IFS= read -r base; do
    [ -n "$base" ] || continue
    cargo_toml="$base/Cargo.toml"
    manifest="$base/tasty-plugin.toml"
    if [ ! -f "$cargo_toml" ] || [ ! -f "$manifest" ]; then
        fail "$base has no Cargo.toml or tasty-plugin.toml."
    fi
    vc=$(version_in "$cargo_toml")
    vm=$(version_in "$manifest")
    [ "$vc" = "$vm" ] || fail "$base: Cargo.toml ($vc) and the manifest ($vm) differ, so it cannot tell which one to bump."
    if ! [[ "$vc" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        fail "$base: version '$vc' is not a numeric MAJOR.MINOR.PATCH."
    fi
    name=$(package_name "$cargo_toml")
    [ -n "$name" ] || fail "cannot read the [package] name from $cargo_toml."
    PLAN_BASE+=("$base")
    PLAN_OLD+=("$vc")
    PLAN_NEW+=("${vc%.*}.$(( ${vc##*.} + 1 ))")
    NAMES+=("$name")
done < "$WORK/violations"

# Edited files go to TOUCHED. On failure only those are restored from HEAD, leaving the tree
# as clean as it started.
TOUCHED=()
undo_touched() {
    [ "${#TOUCHED[@]}" -gt 0 ] || return 0
    if git restore --source=HEAD --staged --worktree -- "${TOUCHED[@]}"; then
        printf '%s restored %d edited files from HEAD.\n' "$TAG" "${#TOUCHED[@]}" >&2
    else
        printf '%s could not restore the edited files. Check git status: %s\n' "$TAG" "${TOUCHED[*]}" >&2
    fi
}
fail_undo() { printf '%s %s\n' "$TAG" "$*" >&2; undo_touched; exit 1; }

for i in "${!PLAN_BASE[@]}"; do
    base="${PLAN_BASE[$i]}"
    new="${PLAN_NEW[$i]}"
    TOUCHED+=("$base/Cargo.toml" "$base/tasty-plugin.toml")
    set_version_in "$base/Cargo.toml" "$new" || fail_undo "cannot change the version in $base/Cargo.toml."
    set_version_in "$base/tasty-plugin.toml" "$new" || fail_undo "cannot change the version in $base/tasty-plugin.toml."
    if [ "$(version_in "$base/Cargo.toml")" != "$new" ] || [ "$(version_in "$base/tasty-plugin.toml")" != "$new" ]; then
        fail_undo "$base: could not write version $new."
    fi
    printf '%s %s %s → %s\n' "$TAG" "${NAMES[$i]}" "${PLAN_OLD[$i]}" "$new"
done

if [ -f Cargo.lock ]; then
    TOUCHED+=(Cargo.lock)
    # Cargo writes the new versions of path packages to the lock. Offline, registry
    # dependencies do not change.
    cargo metadata --offline --format-version 1 >/dev/null 2>"$WORK/metadata.err" \
        || fail_undo "cannot update Cargo.lock. cargo's stderr:
$(cat "$WORK/metadata.err")"
    # Only the version lines of the bumped packages may change in the lock.
    changed=$(git diff --numstat -- Cargo.lock | awk '{ print $1 + $2 }')
    in_lock=0
    for name in "${NAMES[@]}"; do
        grep -qxF "name = \"$name\"" Cargo.lock && in_lock=$((in_lock + 1))
    done
    [ "${changed:-0}" -eq $((in_lock * 2)) ] \
        || fail_undo "Cargo.lock changed more than the version lines of the $in_lock bumped packages (${changed:-0} lines changed)."
fi

git add -- "${TOUCHED[@]}" || fail_undo "git add failed."
git commit --amend --no-edit --quiet || fail_undo "git commit --amend failed. See the hook messages above."
# After the amend there are no uncommitted edits left to restore.
TOUCHED=()

bash "$CHECK" --range HEAD^ HEAD >/dev/null 2>"$WORK/recheck.err"
rc=$?
[ "$rc" -eq 0 ] || fail "the check after the bump ended with exit code $rc. The bump is already in HEAD:
$(cat "$WORK/recheck.err")"
printf '%s amended %d version bumps into the HEAD commit.\n' "$TAG" "${#PLAN_BASE[@]}"
