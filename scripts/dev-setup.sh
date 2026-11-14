#!/usr/bin/env bash
# Points this clone at the git hooks in .githooks. Run once per clone; every worktree of the
# clone shares the setting and runs the hooks of its own branch.
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

echo "The hooks now run on git commit, git merge and git push. See docs/branching.md."
echo "The version check needs the judge for test-only changes: bash scripts/install-judge.sh"
