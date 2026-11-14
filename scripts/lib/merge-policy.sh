# Decides whether a merge commit may be made. Used by pre-merge-commit and by M.1 in pre-commit.
# Allowed merges: docs/branching.md.
# Ported from tasty scripts/lib/merge-policy.sh.
# shellcheck shell=bash

# The long-lived branches. The source of a merged commit is the one of these that contains it.
MERGE_POLICY_BRANCHES="develop release main"

# 0 when branch $2 may be merged into the current branch $1 with a merge commit, else 1.
# Only back merges (main→release, main→develop, release→develop) and promotions
# (develop→release, release→main) are allowed.
merge_pair_allowed() {
    case "$1:$2" in
        release:main | develop:main | develop:release) return 0 ;;
        release:develop | main:release) return 0 ;;
    esac
    return 1
}

# Writes the merged commits, one per line.
# When a conflict is resolved and committed (M.1) they are in the MERGE_HEAD file. When
# pre-merge-commit runs, git has not written MERGE_HEAD yet, so they come from the
# GITHEAD_<commit> environment variables that git merge passes for each merged commit.
merge_heads() {
    local file name
    file=$(git rev-parse --git-path MERGE_HEAD)
    if [ -s "$file" ]; then
        grep -E '^[0-9a-f]{40,64}$' "$file" || true
        return 0
    fi
    while IFS= read -r name; do
        case "$name" in
            GITHEAD_*) printf '%s\n' "${name#GITHEAD_}" ;;
        esac
    done <<< "$(compgen -e)"
    return 0
}

# Writes the refs of long-lived branch $1, one per line: the local branch and the origin
# remote-tracking branch, whichever exist. Both record the shared branch, so a commit on
# origin counts even when the local branch is behind. Nothing when neither exists.
long_branch_refs() {
    local ref
    for ref in "refs/heads/$1" "refs/remotes/origin/$1"; do
        if git rev-parse -q --verify "$ref^{commit}" >/dev/null 2>&1; then
            printf '%s\n' "$ref"
        fi
    done
    return 0
}

# Writes the long-lived branches that contain commit $1 (as an ancestor or the tip), one per line.
branches_containing() {
    local b ref
    git rev-parse -q --verify "$1^{commit}" >/dev/null 2>&1 || return 0
    for b in $MERGE_POLICY_BRANCHES; do
        while IFS= read -r ref; do
            [ -n "$ref" ] || continue
            if git merge-base --is-ancestor "$1" "$ref" 2>/dev/null; then
                printf '%s\n' "$b"
                break
            fi
        done <<< "$(long_branch_refs "$b")"
    done
    return 0
}

# 0 when the merge in progress may be committed, else 1. The reason goes to stderr.
merge_commit_allowed() {
    local current heads count src found=""
    if ! current=$(git symbolic-ref -q --short HEAD 2>/dev/null); then
        printf '[merge] no current branch (detached HEAD). Merge commits are made only on long-lived branches.\n' >&2
        return 1
    fi
    heads=$(merge_heads)
    count=$(grep -c . <<< "$heads" || true)
    if [ "$count" -eq 0 ]; then
        printf '[merge] cannot find the merged commit.\n' >&2
        return 1
    fi
    if [ "$count" -gt 1 ]; then
        printf '[merge] a merge of %s commits at once is not allowed.\n' "$count" >&2
        return 1
    fi
    while IFS= read -r src; do
        [ -n "$src" ] || continue
        found="$found $src"
        if merge_pair_allowed "$current" "$src"; then
            printf '[merge] allowing the merge commit %s ← %s.\n' "$current" "$src" >&2
            return 0
        fi
    done <<< "$(branches_containing "$heads")"
    if [ -n "$found" ]; then
        printf '[merge] %s ←%s is not an allowed back merge or promotion.\n' "$current" "$found" >&2
    else
        printf '[merge] no develop, release or main branch contains the merged commit (current branch %s).\n' "$current" >&2
    fi
    return 1
}
