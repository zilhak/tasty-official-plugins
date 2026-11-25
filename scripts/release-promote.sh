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
# --verified-on  the platforms where both Tasty releases were verified with the Release,
#                on one line: names from PLATFORMS below separated by commas, with spaces
#                around a name ignored, no empty name, and no name twice. Every name of
#                REQUIRED_PLATFORMS must be there; each other platform may be added on its
#                own. The notes carry the value as given.
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
#   7. The published files are the set a tag release of the tag commit uploads, with no
#      file missing or extra. The expected archives come from the tag commit, not from the
#      published lists: its scripts/check.sh list-bins and the id and version in each
#      crate's tasty-plugin.toml, named as scripts/package-plugin.sh names archives. The
#      published SHA256SUMS lists, plugins TSV lists, release-meta.toml plugins, and lock
#      file list exactly those archives, with the GitHub digests as hashes; the published
#      release-meta.toml is for this tag and commit; and the marker block matches it as
#      scripts/release-check.sh verify judges. The lock file has the same tasty_rev.
#   8. Every manifest of the tag commit (each crate of list-bins, read on its own) requires
#      the minimum Tasty: its top-level min_tasty_version equals --min-tasty. In order:
#      a. A tag in UNVALUED_TAGS whose commit is the listed one skips the check before any
#         manifest is read, so the manifest shape of an old tag cannot stop it. A listed
#         name with another commit is not an exception.
#      b. Each manifest is read line by line, as toml_lines in
#         scripts/check-plugin-version-bump.sh reads (comments dropped, brackets counted
#         outside strings, a line is a table header or a key line only when it starts with
#         all brackets closed). The read takes only a shape where the line it finds is the
#         TOML top-level value, and fails as unreadable on a multi-line string, brackets
#         that close nothing or stay open, a table header whose first part is quoted,
#         escaped, or named min_tasty_version, or a key before the first table that is not
#         a bare key. Every line that names min_tasty_version counts: there must be at most
#         one, and it must be a top-level line min_tasty_version = "X.Y.Z" (only spaces
#         after it) before the first table, with a value that release-check.sh semver
#         takes. An empty string is unreadable, not missing. The read does not judge that
#         the file is TOML: the check CI of the tag commit parses the manifests.
#      c. No manifest has the key: passes as a tag from before the key while UNVALUED_TAGS
#         is empty, and fails once it lists tags. Some have it: fails, naming the others.
#         All have it: each value must be --min-tasty as a string.
#      It catches a value left from an earlier Release only when --min-tasty is the right
#      new value. See docs/ci.md#minimum-tasty-version.
# The promotion is one gh release edit: the pending line becomes "Requires Tasty: <min>
# or later", a Verified with Tasty line is added, the Release stops being a pre-release,
# and it becomes Latest. Then the Latest Release must be the tag.
#
# Runs in a clone that has the tag commit (git reads its tree for check 7).
# Needs bash 3.2 or later (the macOS /bin/bash: no bash 4 syntax), git, gh, sha256sum, awk,
# and sed.
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck source=scripts/lib/min-tasty.sh
source scripts/lib/min-tasty.sh

CHECK=scripts/release-check.sh
TASTY_REPO=zilhak/tasty
# The platforms of a release, as PLATFORMS in release.yml.
PLATFORMS=(windows-x64 windows-arm64 macos-arm64 linux-x64 linux-arm64)
# The platforms --verified-on must name, as "Platforms" in
# docs/ci.md#verification-before-promotion asks. Change both together.
REQUIRED_PLATFORMS=(linux-x64 macos-arm64 windows-x64)
# The tags whose manifests have no min_tasty_version, as "<tag> <peeled commit>"; check 8
# skips them. These are all the v* tags made before the manifests carried the key; no tag
# is added after that. Keep it on one line, or one entry per line with ")" alone on the
# last line: the promotion tests rewrite it in those forms.
UNVALUED_TAGS=(
  "v0.1.0 ba234e6e4bc3a7789517ce38cc22a7341f8719e9"
  "v0.1.1 92385b5a08e6b34cb109bdb781b7f92905a8c395"
  "v0.1.2 7dc7ec7d8ff23700a9da8944fa161744c43b6b9b"
)

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
# --verified-on names release platforms, each once, with every required one.
verified=" "
rest="$ON,"
while [[ -n "$rest" ]]; do
  name=${rest%%,*}
  rest=${rest#*,}
  name=${name#"${name%%[![:space:]]*}"}
  name=${name%"${name##*[![:space:]]}"}
  [[ -n "$name" ]] || usage "--verified-on has an empty platform name: '$ON'"
  known=""
  for platform in "${PLATFORMS[@]}"; do
    [[ "$name" != "$platform" ]] || known=yes
  done
  [[ -n "$known" ]] ||
    usage "--verified-on names '$name', which is not a release platform (${PLATFORMS[*]})"
  [[ "$verified" != *" $name "* ]] || usage "--verified-on names $name twice"
  verified="$verified$name "
done
for platform in "${REQUIRED_PLATFORMS[@]}"; do
  [[ "$verified" == *" $platform "* ]] ||
    usage "--verified-on lacks $platform, which docs/ci.md#verification-before-promotion requires"
done

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

# The GitHub digest of published file $1, or nothing.
digest_of() {
  awk -v n="$1" '$1 == n { print $2 }' "$digests"
}

# The value of key $1 in tasty-plugin.toml $2, read as scripts/lib/plugin-build.sh
# manifest_string reads it: the first "$1 = ..." line, its quoted text.
manifest_value() {
  grep -m1 -E "^$1[[:space:]]*=" "$2" | sed 's/.*"\([^"]*\)".*/\1/' || true
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
# "<name> <sha256>" per published file. A file, not an associative array: bash 3.2 has none.
digests="$WORK/digests.txt"
: >"$digests"
while IFS= read -r line; do
  [[ "$line" =~ ^([^[:space:]/]+)\ sha256:([0-9a-f]{64})$ ]] ||
    fail "check 7: unexpected file reply (a name and a sha256 digest): '$line'"
  [[ -z "$(digest_of "${BASH_REMATCH[1]}")" ]] || fail "check 7: ${BASH_REMATCH[1]} is listed twice"
  echo "${BASH_REMATCH[1]} ${BASH_REMATCH[2]}" >>"$digests"
done <"$WORK/assets.txt"

dl="$WORK/download"
mkdir -p "$dl"
gh release download "$TAG" --dir "$dl" --pattern 'SHA256SUMS-*.txt' --pattern 'plugins-*.tsv' \
  --pattern release-meta.toml || fail "check 7: cannot download the lists of Release $TAG"

meta="$dl/release-meta.toml"
[[ -f "$meta" ]] || fail "check 7: Release $TAG has no release-meta.toml"
[[ "$(toml_value "$meta" tag release "check 7")" == "$TAG" ]] || fail "check 7: release-meta.toml is not for $TAG"
[[ "$(toml_value "$meta" commit release "check 7")" == "$commit" ]] || fail "check 7: release-meta.toml is not for $commit"

# The expected files come from the tag commit itself, not from the lists being checked:
# its own scripts/check.sh list-bins names the plugin crates that release.yml packages
# (stage --all), and scripts/package-plugin.sh names each archive
# <id>-<version>-<platform>.tar.gz from the id and version in the crate's tasty-plugin.toml.
git cat-file -e "$commit^{commit}" 2>/dev/null ||
  fail "check 7: the tag commit $commit is not in this clone; fetch it (promote.yml checks out main with its history)"
tree="$WORK/tree"
mkdir -p "$tree/scripts"
git show "$commit:scripts/check.sh" >"$tree/scripts/check.sh" ||
  fail "check 7: cannot read scripts/check.sh at $commit"
bins=$(bash "$tree/scripts/check.sh" list-bins) || fail "check 7: cannot list the plugin crates at $commit"
plugins="$WORK/plugins.txt"
: >"$plugins"
while IFS= read -r crate; do
  [[ -n "$crate" ]] || continue
  git show "$commit:crates/$crate/tasty-plugin.toml" >"$tree/manifest.toml" ||
    fail "check 7: cannot read crates/$crate/tasty-plugin.toml at $commit"
  id=$(manifest_value id "$tree/manifest.toml")
  version=$(manifest_value version "$tree/manifest.toml")
  [[ "$id" =~ ^[^[:space:]/]+$ && "$version" =~ ^[^[:space:]/]+$ ]] ||
    fail "check 7: cannot read the id and version of crates/$crate/tasty-plugin.toml at $commit"
  echo "$id $version" >>"$plugins"
done <<<"$bins"
[[ -s "$plugins" ]] || fail "check 7: the tag commit packages no plugins"

# release-meta.toml lists exactly these plugins.
if ! diff <(awk '
    function val(s) { sub(/^[^=]*=[[:space:]]*"/, "", s); sub(/"[[:space:]]*$/, "", s); return s }
    function flush() { if (inplug) print id " " ver; inplug = 0; id = ""; ver = "" }
    /^[[:space:]]*\[/ { flush(); if ($0 ~ /^[[:space:]]*\[\[plugin\]\][[:space:]]*$/) inplug = 1; next }
    inplug && $1 == "id" { id = val($0) }
    inplug && $1 == "version" { ver = val($0) }
    END { flush() }' "$meta" | sort) <(sort "$plugins") >"$WORK/meta.diff"; then
  sed 's/^/  /' "$WORK/meta.diff" >&2
  fail "check 7: release-meta.toml lists other plugins than $commit packages (< release-meta.toml, > tag commit)"
fi

expected="$WORK/expected.txt"
archives="$WORK/archives.txt"
echo release-meta.toml >"$expected"
: >"$archives"
for platform in "${PLATFORMS[@]}"; do
  sums="$dl/SHA256SUMS-$platform.txt"
  tsv="$dl/plugins-$platform.tsv"
  [[ -f "$sums" && -f "$tsv" ]] || fail "check 7: Release $TAG lacks the lists of $platform"
  printf '%s\n' "SHA256SUMS-$platform.txt" "plugins-$platform.tsv" >>"$expected"
  while read -r id version; do
    echo "$id-$version-$platform.tar.gz" >>"$expected"
  done <"$plugins"
  # SHA256SUMS lists exactly the expected archives of the platform. A last line without a
  # final newline is judged too.
  : >"$WORK/sums.txt"
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^([0-9a-f]{64})\ \ ([^[:space:]/]+)$ ]] ||
      fail "check 7: unexpected line in SHA256SUMS-$platform.txt: '$line'"
    echo "${BASH_REMATCH[2]} ${BASH_REMATCH[1]}" >>"$WORK/sums.txt"
  done <"$sums"
  if ! diff <(cut -d' ' -f1 "$WORK/sums.txt" | sort) \
    <(while read -r id version; do echo "$id-$version-$platform.tar.gz"; done <"$plugins" | sort) \
    >"$WORK/sums.diff"; then
    sed 's/^/  /' "$WORK/sums.diff" >&2
    fail "check 7: SHA256SUMS-$platform.txt lists other archives than $commit packages (< list, > tag commit)"
  fi
  cat "$WORK/sums.txt" >>"$archives"
  # The TSV has one row per expected archive (columns: id, version, platform, file, sha256),
  # with the hash of SHA256SUMS.
  if ! diff <(awk -F'\t' 'NR > 1 { print $1 " " $2 " " $3 " " $4 " " $5 }' "$tsv" | sort) \
    <(while read -r id version; do
      file="$id-$version-$platform.tar.gz"
      echo "$id $version $platform $file $(awk -v n="$file" '$1 == n { print $2 }' "$WORK/sums.txt")"
    done <"$plugins" | sort) >/dev/null; then
    fail "check 7: plugins-$platform.tsv does not list the archives of SHA256SUMS-$platform.txt"
  fi
done

if ! diff <(sort "$expected") <(cut -d' ' -f1 "$digests" | sort) >"$WORK/names.diff"; then
  sed 's/^/  /' "$WORK/names.diff" >&2
  fail "check 7: the files of Release $TAG are not the set a tag release uploads (< missing, > extra)"
fi
for file in "$dl"/*; do
  name=$(basename "$file")
  [[ "$(sha_of "$file")" == "$(digest_of "$name")" ]] || fail "check 7: $name differs from its GitHub digest"
done
while read -r name hash; do
  [[ "$(digest_of "$name")" == "$hash" ]] || fail "check 7: $name differs from its SHA256SUMS hash"
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

# ---- 8. The manifests require the minimum Tasty ----
listed=""
for entry in ${UNVALUED_TAGS[@]+"${UNVALUED_TAGS[@]}"}; do
  [[ "${entry%% *}" == "$TAG" ]] || continue
  if [[ "${entry#* }" == "$commit" ]]; then
    listed=yes
    break
  fi
  listed="for ${entry#* }"
done
if [[ "$listed" == yes ]]; then
  echo "Check 8 skipped: $TAG ($commit) is listed as a tag whose manifests have no min_tasty_version"
else
  minimums="$WORK/min-tasty.txt"
  : >"$minimums"
  while IFS= read -r crate; do
    [[ -n "$crate" ]] || continue
    manifest="$WORK/$crate.tasty-plugin.toml"
    git show "$commit:crates/$crate/tasty-plugin.toml" >"$manifest" ||
      fail "check 8: cannot read crates/$crate/tasty-plugin.toml at $commit"
    found=$(min_tasty_of "$manifest") || fail "check 8: cannot read crates/$crate/tasty-plugin.toml at $commit"
    case "$found" in
      none) echo "$crate -" >>"$minimums" ;;
      value$'\t'*)
        value=${found#value$'\t'}
        "$CHECK" semver "$value" ||
          fail "check 8: crates/$crate/tasty-plugin.toml at $commit cannot be read: min_tasty_version '$value' is not MAJOR.MINOR.PATCH"
        echo "$crate $value" >>"$minimums"
        ;;
      *) fail "check 8: crates/$crate/tasty-plugin.toml at $commit cannot be read: ${found#bad$'\t'}" ;;
    esac
  done <<<"$bins"
  missing=$(awk '$2 == "-" { printf "%s ", $1 }' "$minimums")
  if ! awk '$2 != "-" { found = 1 } END { exit !found }' "$minimums"; then
    if [[ ${#UNVALUED_TAGS[@]} -eq 0 ]]; then
      echo "Check 8 skipped: no manifest of $TAG has min_tasty_version, a tag from before the key"
    else
      fail "check 8: no manifest of $TAG ($commit) has min_tasty_version, and the tag is not listed in UNVALUED_TAGS${listed:+ (it is listed $listed)}"
    fi
  elif [[ -n "$missing" ]]; then
    fail "check 8: some manifests of $TAG have no min_tasty_version: ${missing% }"
  else
    while read -r crate value; do
      [[ "$value" == "$MIN" ]] ||
        fail "check 8: crates/$crate/tasty-plugin.toml at $commit has min_tasty_version $value, not --min-tasty $MIN"
    done <"$minimums"
  fi
fi

# ---- Promote ----
"$CHECK" promote "$WORK/notes.md" "$MIN" "$NEW" "$ON" >"$WORK/promoted.md" ||
  fail "cannot write the promoted notes"
check_latest_is_older 5
gh release edit "$TAG" --notes-file "$WORK/promoted.md" --prerelease=false --latest ||
  fail "the edit of Release $TAG failed; run again to check its state"
latest=$("$CHECK" latest) || fail "cannot read the Latest Release after the edit"
[[ "$latest" == "$TAG" ]] || fail "after the edit the Latest Release is '${latest:-none}', not $TAG"
echo "Release $TAG is Latest: Requires Tasty $MIN or later, verified with $MIN and $NEW on $ON"
