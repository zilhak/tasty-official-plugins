#!/usr/bin/env bash
# Runs shellcheck on every shell script in the repository and actionlint on every workflow.
# The scripts are found, not listed: tracked files that end in .sh, the git hooks in
# .githooks, and tracked files whose first line is a bash or sh shebang. Each checked file is
# printed. actionlint also runs shellcheck on the run blocks when shellcheck is on PATH.
#
# Usage: bash scripts/lint-scripts.sh
# Both tools must be on PATH; bash scripts/install-lint-tools.sh installs the pinned versions
# and prints their directory.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

for tool in shellcheck actionlint; do
    command -v "$tool" >/dev/null 2>&1 \
        || { echo "✘ $tool is not on PATH. Run: PATH=\"\$(bash scripts/install-lint-tools.sh):\$PATH\"" >&2; exit 2; }
done

# Whether tracked file $1 is a shell script.
is_shell() {
    local first=""
    case "$1" in
        *.sh | .githooks/*) return 0 ;;
    esac
    [ -f "$1" ] && [ ! -L "$1" ] || return 1
    IFS= read -r first < "$1" || true
    case "$first" in
        '#!'*bash* | '#!'*/sh | '#!'*'/env sh'*) return 0 ;;
    esac
    return 1
}

scripts=()
while IFS= read -r -d '' f; do
    [ -f "$f" ] || continue
    if is_shell "$f"; then scripts+=("$f"); fi
done < <(git ls-files -z)

workflows=()
for f in .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$f" ] && workflows+=("$f")
done

[ "${#scripts[@]}" -gt 0 ] || { echo "✘ no shell scripts found." >&2; exit 2; }
[ "${#workflows[@]}" -gt 0 ] || { echo "✘ no workflows found." >&2; exit 2; }

rc=0
echo "shellcheck $(shellcheck --version | sed -n 's/^version: //p') on ${#scripts[@]} files:"
printf '  %s\n' "${scripts[@]}"
shellcheck -x "${scripts[@]}" || rc=1

echo "actionlint $(actionlint -version | sed -n '1p') on ${#workflows[@]} files:"
printf '  %s\n' "${workflows[@]}"
actionlint "${workflows[@]}" || rc=1

if [ "$rc" -eq 0 ]; then
    echo "✓ shellcheck and actionlint found no problems."
fi
exit "$rc"
