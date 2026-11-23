#!/usr/bin/env bash
# Promotes a plugin pre-release to Latest, after a published Tasty release bundles it.
# promote.yml runs it; it also runs locally with gh signed in. See docs/ci.md#promotion.
#
#   scripts/release-promote.sh --tag TAG --min-tasty X.Y.Z --new-tasty X.Y.Z \
#       --verified-on "PLATFORMS"
#
# --tag          the plugin Release to promote, such as v0.1.3
# --min-tasty    the oldest Tasty release the Release supports
# --new-tasty    the Tasty release that bundles the Release
# --verified-on  the platforms the Release was verified on, one line
#
# The repository is GH_REPO (owner/name), and gh needs a token that may edit its Releases.
# Before anything is changed, every check below must pass; the first one that fails ends
# the run with nothing changed. The checks, in order:
#   0. Already done: the Release is Latest and its marker block holds exactly the lines
#      these inputs write. Then nothing is changed and the run succeeds, so a run whose
#      edit reply was lost can be run again. Latest with other lines fails.
#   1. The Release is published and a pre-release, and the Commit line of its marker
#      block is the commit the tag points at (an annotated tag is peeled).
#   2. tasty has published Releases v<new-tasty> and v<min-tasty> (not drafts or
#      pre-releases).
#   3. official-plugins.lock.toml at tasty tag v<new-tasty> names this repository, this
#      tag, and the peeled commit in [release].
#   4. min-tasty is not higher than new-tasty.
#   5. The Latest Release is older than the tag, or there is no Latest Release. Checked
#      again right before the edit.
#   6. The marker block has one Requires Tasty line, and it is pending.
#   7. The published files are the set a tag release uploads, with no file missing or
#      extra; their GitHub digests match the published SHA256SUMS lists, plugins TSV
#      lists, and the lock file; the published release-meta.toml is for this tag and
#      commit; and the marker block matches it as scripts/release-check.sh verify judges.
#      The lock file has the same tasty_rev.
# The promotion is one gh release edit: the pending line becomes "Requires Tasty: <min>
# or later", a Verified with Tasty line is added, the Release stops being a pre-release,
# and it becomes Latest. Then the Latest Release must be the tag.
#
# Needs bash, gh, sha256sum, awk, and sed.
set -euo pipefail

cd "$(dirname "$0")/.."

CHECK=scripts/release-check.sh
TASTY_REPO=zilhak/tasty
# The platforms of a release, as PLATFORMS in release.yml.
PLATFORMS=(windows-x64 windows-arm64 macos-arm64 linux-x64 linux-arm64)

say() {
  if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
}

fail() {
  say "$@"
  exit 1
}

usage() {
  say "$@"
  echo "usage: scripts/release-promote.sh --tag TAG --min-tasty X.Y.Z --new-tasty X.Y.Z --verified-on PLATFORMS" >&2
  exit 2
}

TAG="" MIN="" NEW="" ON=""
while [[ $# -gt 0 ]]; do
  [[ $# -ge 2 ]] || usage "$1 needs a value"
  case "$1" in
    --tag) TAG=$2 ;;
    --min-tasty) MIN=$2 ;;
    --new-tasty) NEW=$2 ;;
    --verified-on) ON=$2 ;;
    *) usage "unknown option: $1" ;;
  esac
  shift 2
done
[[ -n "${GH_REPO:-}" ]] || usage "GH_REPO is not set"
if [[ "$TAG" != v* ]] || ! "$CHECK" semver "${TAG#v}"; then
  usage "the tag is not v<MAJOR.MINOR.PATCH>: '$TAG'"
fi
"$CHECK" semver "$MIN" || usage "--min-tasty is not MAJOR.MINOR.PATCH: '$MIN'"
"$CHECK" semver "$NEW" || usage "--new-tasty is not MAJOR.MINOR.PATCH: '$NEW'"
[[ -n "${ON//[[:space:]]/}" ]] || usage "--verified-on is empty"
# A line break would break the marker block of the notes.
[[ "$ON" != *$'\n'* && "$ON" != *$'\r'* ]] || usage "--verified-on must be one line"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

release_api() {
  gh api "repos/$GH_REPO/releases/tags/$TAG" --jq "$1"
}

read_notes() {
  release_api .body >"$WORK/notes.md" || fail "cannot read the notes of Release $TAG"
}

# Passes when there is no Latest Release or it is older than the tag.
check_latest_is_older() {
  local latest
  latest=$("$CHECK" latest) || fail "check $1: cannot read the Latest Release"
  [[ -z "$latest" ]] && return 0
  if [[ "$latest" != v* ]] || ! "$CHECK" semver "${latest#v}"; then
    fail "check $1: the Latest Release $latest is not v<MAJOR.MINOR.PATCH>"
  fi
  "$CHECK" semver-lt "${latest#v}" "${TAG#v}" ||
    fail "check $1: the Latest Release $latest is not older than $TAG; an older tag is never made Latest again"
}

# The commit the tag points at, through annotated tags.
peel_tag() {
  local obj type sha i
  obj=$(gh api "repos/$GH_REPO/git/ref/tags/$TAG" --jq '"\(.object.type) \(.object.sha)"') ||
    fail "check 1: cannot read the tag $TAG"
  for i in 1 2 3 4 5; do
    type=${obj%% *}
    sha=${obj#* }
    [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || fail "check 1: unexpected tag object reply: '$obj'"
    case "$type" in
      commit)
        echo "$sha"
        return 0
        ;;
      tag)
        obj=$(gh api "repos/$GH_REPO/git/tags/$sha" --jq '"\(.object.type) \(.object.sha)"') ||
          fail "check 1: cannot read the tag object $sha"
        ;;
      *) fail "check 1: the tag $TAG points at a $type" ;;
    esac
  done
  fail "check 1: the tag $TAG is nested deeper than $i tag objects"
}

# The value of key $2 in table [$3] of TOML file $1; fails unless there is exactly one.
toml_value() {
  local found count
  found=$(awk -v key="$2" -v table="[$3]" '
    /^[[:space:]]*\[/ { sec = $0; gsub(/[[:space:]]/, "", sec); next }
    sec == table && $1 == key && $2 == "=" {
      v = $0; sub(/^[^=]*=[[:space:]]*"/, "", v); sub(/"[[:space:]]*$/, "", v); print v
    }' "$1")
  count=0
  [[ -z "$found" ]] || count=$(wc -l <<<"$found")
  ((count == 1)) || fail "$4: $(basename "$1") has $count '$2' values in [$3], expected one"
  printf '%s\n' "$found"
}

sha_of() {
  sha256sum "$1" | cut -d' ' -f1
}

# ---- 0. Already done ----
latest=$("$CHECK" latest) || fail "check 0: cannot read the Latest Release"
if [[ "$latest" == "$TAG" ]]; then
  read_notes
  if "$CHECK" promoted "$WORK/notes.md" "$MIN" "$NEW" "$ON"; then
    echo "Release $TAG is already Latest with these Requires Tasty and Verified with Tasty lines; nothing to change"
    exit 0
  fi
  fail "check 0: Release $TAG is already Latest, but its marker block does not hold the lines for these inputs"
fi

# ---- 1. A published pre-release of the tag commit ----
state=$(release_api '"\(.draft) \(.prerelease)"') || fail "check 1: cannot read Release $TAG"
[[ "$state" == "false true" ]] ||
  fail "check 1: Release $TAG is not a published pre-release (draft prerelease: $state)"
commit=$(peel_tag)
read_notes
marked=$("$CHECK" marker "$WORK/notes.md" Commit) || fail "check 1: the marker block has no single Commit line"
[[ "$marked" == "$commit" ]] ||
  fail "check 1: the marker block has Commit $marked, but $TAG points at $commit"

# ---- 2. Published Tasty releases ----
for version in "$NEW" "$MIN"; do
  tasty_state=$(gh api "repos/$TASTY_REPO/releases/tags/v$version" --jq '"\(.draft) \(.prerelease)"') ||
    fail "check 2: $TASTY_REPO has no readable Release v$version"
  [[ "$tasty_state" == "false false" ]] ||
    fail "check 2: $TASTY_REPO Release v$version is not published as a release (draft prerelease: $tasty_state)"
done

# ---- 3. The lock file of the bundling Tasty names this Release ----
lock="$WORK/official-plugins.lock.toml"
gh api -H 'Accept: application/vnd.github.raw+json' \
  "repos/$TASTY_REPO/contents/official-plugins.lock.toml?ref=v$NEW" >"$lock" ||
  fail "check 3: $TASTY_REPO v$NEW has no readable official-plugins.lock.toml"
lock_repo=$(toml_value "$lock" repository release "check 3")
lock_tag=$(toml_value "$lock" tag release "check 3")
lock_commit=$(toml_value "$lock" commit release "check 3")
[[ "$lock_repo" == "$GH_REPO" ]] || fail "check 3: the lock file of v$NEW is for $lock_repo, not $GH_REPO"
[[ "$lock_tag" == "$TAG" ]] || fail "check 3: the lock file of v$NEW pins $lock_tag, not $TAG"
[[ "$lock_commit" == "$commit" ]] || fail "check 3: the lock file of v$NEW pins commit $lock_commit, not $commit"

# ---- 4. min-tasty <= new-tasty ----
if "$CHECK" semver-lt "$NEW" "$MIN"; then
  fail "check 4: --min-tasty $MIN is higher than --new-tasty $NEW"
fi

# ---- 5. The Latest Release is older ----
check_latest_is_older 5

# ---- 6. One pending Requires Tasty line ----
requires=$("$CHECK" marker "$WORK/notes.md" "Requires Tasty") ||
  fail "check 6: the marker block does not have exactly one Requires Tasty line"
[[ "$requires" == pending* ]] || fail "check 6: the Requires Tasty line is '$requires', not pending"

# ---- 7. The published files and notes, judged as at publish time ----
release_api '.assets[] | "\(.name) \(.digest)"' >"$WORK/assets.txt" ||
  fail "check 7: cannot list the files of Release $TAG"
declare -A digest=()
while IFS= read -r line; do
  [[ "$line" =~ ^([^[:space:]/]+)\ sha256:([0-9a-f]{64})$ ]] ||
    fail "check 7: unexpected file reply (a name and a sha256 digest): '$line'"
  [[ -z "${digest[${BASH_REMATCH[1]}]:-}" ]] || fail "check 7: ${BASH_REMATCH[1]} is listed twice"
  digest[${BASH_REMATCH[1]}]=${BASH_REMATCH[2]}
done <"$WORK/assets.txt"

dl="$WORK/download"
mkdir -p "$dl"
gh release download "$TAG" --dir "$dl" --pattern 'SHA256SUMS-*.txt' --pattern 'plugins-*.tsv' \
  --pattern release-meta.toml || fail "check 7: cannot download the lists of Release $TAG"

meta="$dl/release-meta.toml"
[[ -f "$meta" ]] || fail "check 7: Release $TAG has no release-meta.toml"
[[ "$(toml_value "$meta" tag release "check 7")" == "$TAG" ]] || fail "check 7: release-meta.toml is not for $TAG"
[[ "$(toml_value "$meta" commit release "check 7")" == "$commit" ]] || fail "check 7: release-meta.toml is not for $commit"
plugins=$(grep -cx '\[\[plugin\]\]' "$meta" || true)
((plugins > 0)) || fail "check 7: release-meta.toml lists no plugins"

expected="$WORK/expected.txt"
archives="$WORK/archives.txt"
echo release-meta.toml >"$expected"
: >"$archives"
for platform in "${PLATFORMS[@]}"; do
  sums="$dl/SHA256SUMS-$platform.txt"
  tsv="$dl/plugins-$platform.tsv"
  [[ -f "$sums" && -f "$tsv" ]] || fail "check 7: Release $TAG lacks the lists of $platform"
  printf '%s\n' "SHA256SUMS-$platform.txt" "plugins-$platform.tsv" >>"$expected"
  n=0
  while IFS= read -r line; do
    [[ "$line" =~ ^([0-9a-f]{64})\ \ ([^[:space:]/]+-$platform\.tar\.gz)$ ]] ||
      fail "check 7: unexpected line in SHA256SUMS-$platform.txt: '$line'"
    echo "${BASH_REMATCH[2]}" >>"$expected"
    echo "${BASH_REMATCH[2]} ${BASH_REMATCH[1]}" >>"$archives"
    n=$((n + 1))
  done <"$sums"
  ((n == plugins)) || fail "check 7: SHA256SUMS-$platform.txt lists $n archives, release-meta.toml $plugins plugins"
  # The TSV lists the same archives and hashes (columns: id, version, platform, file, sha256).
  if ! diff <(awk -F'\t' 'NR > 1 { print $4 " " $5 }' "$tsv" | sort) \
    <(awk '{ print $2 " " $1 }' "$sums" | sort) >/dev/null; then
    fail "check 7: plugins-$platform.tsv and SHA256SUMS-$platform.txt list different archives"
  fi
done

if ! diff <(sort "$expected") <(printf '%s\n' "${!digest[@]}" | sort) >"$WORK/names.diff"; then
  sed 's/^/  /' "$WORK/names.diff" >&2
  fail "check 7: the files of Release $TAG are not the set a tag release uploads (< missing, > extra)"
fi
for file in "$dl"/*; do
  name=$(basename "$file")
  [[ "$(sha_of "$file")" == "${digest[$name]:-}" ]] || fail "check 7: $name differs from its GitHub digest"
done
while read -r name hash; do
  [[ "${digest[$name]:-}" == "$hash" ]] || fail "check 7: $name differs from its SHA256SUMS hash"
done <"$archives"

# The lock file pins the same archives and hashes.
if ! diff <(awk '
    function val(s) { sub(/^[^=]*=[[:space:]]*"/, "", s); sub(/"[[:space:]]*$/, "", s); return s }
    function flush() { if (inart) print file " " sha; inart = 0; file = ""; sha = "" }
    /^[[:space:]]*\[/ { flush(); if ($0 ~ /^[[:space:]]*\[\[plugin\.artifact\]\][[:space:]]*$/) inart = 1; next }
    inart && $1 == "file" { file = val($0) }
    inart && $1 == "sha256" { sha = val($0) }
    END { flush() }' "$lock" | sort) <(sort "$archives") >"$WORK/lock.diff"; then
  sed 's/^/  /' "$WORK/lock.diff" >&2
  fail "check 7: the lock file of v$NEW pins other archives or hashes than Release $TAG has (< lock, > Release)"
fi
[[ "$(toml_value "$lock" tasty_rev release "check 7")" == "$(toml_value "$meta" tasty_rev release "check 7")" ]] ||
  fail "check 7: the lock file of v$NEW and release-meta.toml name different tasty_rev values"
"$CHECK" verify "$WORK/notes.md" "$meta" || fail "check 7: the marker block does not match release-meta.toml"

# A check that the Requires Tasty value matches the min_tasty_version of the plugin
# manifests goes here once the manifests carry one.

# ---- Promote ----
"$CHECK" promote "$WORK/notes.md" "$MIN" "$NEW" "$ON" >"$WORK/promoted.md" ||
  fail "cannot write the promoted notes"
check_latest_is_older 5
gh release edit "$TAG" --notes-file "$WORK/promoted.md" --prerelease=false --latest ||
  fail "the edit of Release $TAG failed; run again to check its state"
latest=$("$CHECK" latest) || fail "cannot read the Latest Release after the edit"
[[ "$latest" == "$TAG" ]] || fail "after the edit the Latest Release is '${latest:-none}', not $TAG"
echo "Release $TAG is Latest: Requires Tasty $MIN or later, verified with $MIN and $NEW on $ON"
