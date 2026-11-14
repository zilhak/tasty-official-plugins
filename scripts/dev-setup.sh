#!/usr/bin/env bash
# Points this clone at the git hooks in .githooks and fetches the crates of every target for
# the plugin version check. Run once per clone; every worktree of the clone shares the
# setting and runs the hooks of its own branch.
# Usage: bash scripts/dev-setup.sh
# Ported from tasty scripts/dev-setup.sh (the hook part only).

set -e

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "✘ Run it inside the git repository." >&2
    exit 1
}

cd "$REPO_ROOT"

if [ ! -d .githooks ]; then
    echo "✘ There is no .githooks directory. Check whether the clone is complete." >&2
    exit 1
fi

CURRENT="$(git config --local --get core.hooksPath || echo '')"
if [ "$CURRENT" = ".githooks" ]; then
    echo "core.hooksPath is already .githooks"
else
    git config --local core.hooksPath .githooks
    echo "core.hooksPath set to .githooks"
fi

chmod +x .githooks/* 2>/dev/null || true

# The setting is shared by every worktree of the clone, and the relative path is taken from
# each worktree's own top folder, so each one runs the hooks of its own branch.
# This summary only reports; when it cannot be read it shows "?" and the fetch below still runs.
HOOKS_DIR="$(git rev-parse --git-path hooks 2>/dev/null)" || HOOKS_DIR="?"
WORKTREE_LIST="$(git worktree list --porcelain 2>/dev/null)" || WORKTREE_LIST=""
WORKTREES=""
if [ -n "$WORKTREE_LIST" ]; then
    WORKTREES="$(printf '%s\n' "$WORKTREE_LIST" | grep -c '^worktree ' || true)"
fi
[ -n "$HOOKS_DIR" ] || HOOKS_DIR="?"
[ -n "$WORKTREES" ] || WORKTREES="?"
echo "Hooks of this tree: $HOOKS_DIR. Every worktree of this clone ($WORKTREES now) uses"
echo "  .githooks of its own tree unless it sets core.hooksPath with git config --worktree."

# The version check reads the dependency closure of every target offline, and a build
# fetches only the host's crates, so fetch them all once here.
echo "Fetching the crates of every target for the version check: cargo fetch --locked"
FETCH_RC=0
cargo fetch --locked || FETCH_RC=$?
if [ "$FETCH_RC" -ne 0 ]; then
    echo "✘ cargo fetch --locked failed (exit code $FETCH_RC). The hooks are set; run cargo fetch --locked again before committing plugin or dependency changes." >&2
    exit "$FETCH_RC"
fi

echo "The hooks now run on git commit, git merge and git push. See docs/branching.md."
echo "The version check needs the judge for test-only changes: bash scripts/install-judge.sh"
