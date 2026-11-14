#!/usr/bin/env bash
# Checks the merge commit decision (scripts/lib/merge-policy.sh) with a table and with real
# merges in a throwaway repository, which gets this repository's pre-commit, pre-merge-commit
# and decision file. cargo and the version check that pre-commit also calls are replaced with
# stand-ins that always succeed.
# Usage: merge-policy.sh <repository root> <empty temporary folder>
# Ported from tasty scripts/tests/merge-policy.sh.
set -eu -o pipefail
root="$(cd "$1" && pwd -P)"
fixture="$(cd "$2" && pwd -P)"
local_vars="$(git rev-parse --local-env-vars)"
while IFS= read -r name; do
    [ -z "$name" ] || unset "$name"
done <<< "$local_vars"
# Values passed by an outer merge must not change the decision.
while IFS= read -r name; do
    case "$name" in GITHEAD_*) unset "$name" ;; esac
done <<< "$(compgen -e)"

failures=0
expect() {
    local want="$1" got="$2" what="$3"
    if [ "$want" = "$got" ]; then
        printf 'ok   %s (rc=%s)\n' "$what" "$got"
    else
        printf 'FAIL %s: rc=%s, expected %s\n' "$what" "$got" "$want" >&2
        failures=$((failures + 1))
    fi
}

# 1. The table: everything but the five allowed pairs is refused.
# shellcheck source=scripts/lib/merge-policy.sh
. "$root/scripts/lib/merge-policy.sh"
allowed=" release:main develop:main develop:release release:develop main:release "
for current in develop release main work/x conductor/wt-1; do
    for source in develop release main work/x conductor/wt-1; do
        want=1
        case "$allowed" in *" $current:$source "*) want=0 ;; esac
        got=0
        merge_pair_allowed "$current" "$source" || got=$?
        expect "$want" "$got" "table $current ← $source"
    done
done

# 2. A throwaway repository with the hooks; the tools pre-commit also calls are stand-ins.
repo="$fixture/repo"
mkdir -p "$repo/.githooks" "$repo/scripts/lib" "$fixture/bin"
git -C "$repo" init -q -b main
cp "$root/.githooks/pre-commit" "$root/.githooks/pre-merge-commit" "$repo/.githooks/"
cp "$root/scripts/lib/merge-policy.sh" "$repo/scripts/lib/merge-policy.sh"
chmod +x "$repo/.githooks/pre-commit" "$repo/.githooks/pre-merge-commit"
printf '#!/usr/bin/env bash\nexit 0\n' > "$repo/scripts/check-plugin-version-bump.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/bin/cargo"
chmod +x "$fixture/bin/cargo"
export PATH="$fixture/bin:$PATH"
git -C "$repo" config core.hooksPath .githooks
git -C "$repo" config user.name fixture
git -C "$repo" config user.email fixture@example.invalid
git -C "$repo" config commit.gpgsign false
git -C "$repo" config merge.ff false
cd "$repo"

commit() {
    printf '%s\n' "$1" > "$1"
    git add "$1"
    git commit -qm "$1" >/dev/null 2>&1
}
merge() {
    local rc=0
    git merge -q --no-edit "$@" >/dev/null 2>&1 || rc=$?
    if [ "$rc" -ne 0 ] && git rev-parse -q --verify MERGE_HEAD >/dev/null; then
        git merge --abort
    fi
    printf '%s' "$rc"
}
judge() {
    local rc=0
    bash -c '. scripts/lib/merge-policy.sh; merge_commit_allowed' >/dev/null 2>&1 || rc=$?
    printf '%s' "$rc"
}

git add .githooks scripts
git commit -qm base >/dev/null 2>&1
git branch develop
git branch release

# (f) A commit that is not a merge passes the real pre-commit. (e) Outside a merge the decision refuses.
git switch -q develop
rc=0; commit d1 || rc=$?
expect 0 "$rc" "pre-commit: not a merge"
expect 1 "$(judge)" "no MERGE_HEAD or GITHEAD"

# (c) The tip of a work branch is on no long-lived branch, so it is refused.
git switch -q -c work/x
commit w1
git switch -q develop
expect 1 "$(merge work/x)" "merge develop ← work/x"
expect 1 "$(merge "$(git rev-parse work/x)")" "merge develop ← work/x by hash"
expect 0 "$(merge --ff-only work/x)" "fast-forward develop ← work/x"

git switch -q main
commit h1
git switch -q develop
expect 0 "$(merge main)" "back merge develop ← main"
git switch -q release
commit r1
expect 0 "$(merge main)" "back merge release ← main"
git switch -q develop
expect 0 "$(merge release)" "back merge develop ← release"
git switch -q release
commit r2
expect 0 "$(merge develop)" "promotion release ← develop"
git switch -q main
expect 0 "$(merge --ff-only release)" "fast-forward main ← release (after the hotfix back merge)"
git switch -q release
commit r3
git switch -q main
commit h2
expect 0 "$(merge release)" "promotion main ← release"
git switch -q develop
commit d2
git switch -q main
expect 1 "$(merge develop)" "merge main ← develop"
git switch -q --detach release
expect 1 "$(merge develop)" "merge detached ← develop"
git switch -q -c work/z develop
commit z1
git switch -q develop
commit d3
expect 1 "$(merge main work/z)" "octopus develop ← main, work/z"

# (a) A past point of develop promoted to release without fast-forward.
git switch -q develop
commit d4
commit d5
git switch -q release
commit r4
expect 0 "$(merge develop~1)" "promotion release ← a past point of develop"
# (b) A past point of release merged into main.
commit r5
git switch -q main
commit h3
expect 0 "$(merge release~1)" "promotion main ← a past point of release"
# (d) A commit on both develop and release may go to main; one only on develop may not.
git switch -q develop
commit d6
git switch -q release
git merge -q --no-edit develop >/dev/null 2>&1
git switch -q develop
commit d7
git switch -q main
expect 0 "$(merge develop~1)" "main ← a commit on develop and release"
expect 1 "$(merge develop)" "main ← a commit only on develop"
# Without a local long-lived branch the origin remote-tracking branch is used.
git switch -q release
commit r6
git update-ref refs/remotes/origin/main "$(git rev-parse main)"
git switch -q main
commit h4
git switch -q release
git branch -q -D main
expect 0 "$(merge origin/main)" "back merge release ← origin/main (no local main)"
git branch -q main origin/main
# A commit on origin counts even when the local long-lived branch is behind.
git switch -q main
commit h5
git update-ref refs/remotes/origin/main "$(git rev-parse HEAD)"
git reset -q --hard HEAD~1
git switch -q develop
expect 0 "$(merge origin/main)" "back merge develop ← origin/main (local main behind)"
commit d8
commit d9
git update-ref refs/remotes/origin/develop "$(git rev-parse HEAD)"
git reset -q --hard HEAD~2
git switch -q release
expect 0 "$(merge origin/develop~1)" "promotion release ← a past point of origin/develop (local develop behind)"
# On origin too only long-lived branch names count. A commit only on origin/<work branch> is refused.
git switch -q -c work/q develop
commit q1
git update-ref refs/remotes/origin/work/q "$(git rev-parse HEAD)"
git switch -q develop
git branch -q -D work/q
expect 1 "$(merge origin/work/q)" "merge develop ← origin/work/q"

# (g) A merge with a conflict is decided by the real pre-commit (M.1) on git commit.
conflict_commit() {
    local rc=0
    git merge -q --no-edit "$1" >/dev/null 2>&1 || true
    printf 'resolved\n' > clash
    git add clash
    git commit -q --no-edit >/dev/null 2>&1 || rc=$?
    if git rev-parse -q --verify MERGE_HEAD >/dev/null; then
        git merge --abort
    fi
    printf '%s' "$rc"
}
git switch -q develop
printf 'develop\n' > clash
git add clash
git commit -qm clash-develop >/dev/null 2>&1
git switch -q main
printf 'main\n' > clash
git add clash
git commit -qm clash-main >/dev/null 2>&1
git switch -q -c work/y develop~1
printf 'work\n' > clash
git add clash
git commit -qm clash-work >/dev/null 2>&1
git switch -q develop
expect 1 "$(conflict_commit work/y)" "pre-commit: resolved merge develop ← work/y"
expect 0 "$(conflict_commit main)" "pre-commit: resolved back merge develop ← main"

if [ "$failures" -ne 0 ]; then
    printf '%s failures\n' "$failures" >&2
    exit 1
fi
echo "merge-policy: all passed"
