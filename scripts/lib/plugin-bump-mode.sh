# Decides whether the plugin version check (P.1) of pre-commit is deferred in this work tree.
# The mode and the merge step that adds the bumps: docs/branching.md.
# Ported from tasty scripts/lib/plugin-bump-mode.sh.
# shellcheck shell=bash

# Writes one word to stdout: deferred or enforced. Why a setting is not followed goes to stderr.
# All three are needed to defer: the value is deferred, its scope is worktree
# (config.worktree), and this is a linked work tree (not the main one).
# A value in the shared config or in the main work tree would turn the check off for every
# work tree or for the main repository, so it is refused.
plugin_bump_mode() {
    local line scope value git_dir common_dir
    if ! line=$(git config --show-scope --get tasty.pluginBump 2>/dev/null); then
        echo enforced
        return 0
    fi
    scope=${line%%$'\t'*}
    value=${line#*$'\t'}
    if [ "$value" != deferred ]; then
        printf '[P.1] tasty.pluginBump=%s is an unknown value. The version check runs as usual.\n' "$value" >&2
        echo enforced
        return 0
    fi
    if [ "$scope" != worktree ]; then
        printf '[P.1] tasty.pluginBump=deferred is set in the %s scope. Only a value in the worktree scope (git config --worktree) is followed.\n' "$scope" >&2
        echo enforced
        return 0
    fi
    git_dir=$(git rev-parse --path-format=absolute --git-dir 2>/dev/null) || git_dir=""
    common_dir=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || common_dir=""
    if [ -z "$git_dir" ] || [ "$git_dir" = "$common_dir" ]; then
        printf '[P.1] tasty.pluginBump=deferred is followed only in a linked work tree. The main work tree runs the version check as usual.\n' >&2
        echo enforced
        return 0
    fi
    echo deferred
}
