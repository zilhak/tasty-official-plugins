#!/usr/bin/env bash
# Release checks that both release stages share. release.yml (create-release and publish)
# and scripts/release-promote.sh call this script, so a tag release and a promotion judge
# the tag and read the Release notes the same way. See docs/ci.md#releases.
#
# Release notes end with a marker block: the last paragraph of the notes, after the last
# blank line. Every line of it starts with one of these prefixes:
#   Commit: <sha>              the commit of this repository that the tag points at
#   tasty rev: <sha>           the tasty commit the plugins were built from
#   Requires Tasty: ...        "pending (...)" until promotion, then "<version> or later"
#   Verified with Tasty: ...   written by promotion
# The text above the block is free text, such as the tag commit body, and is never read.
#
#   scripts/release-check.sh semver VERSION
#       VERSION is a SemVer 2.0.0 core version: MAJOR.MINOR.PATCH, no leading zeros.
#   scripts/release-check.sh tag TAG VERSION
#       VERSION is a SemVer core version and TAG is v<VERSION>.
#   scripts/release-check.sh semver-lt A B
#       A is lower than B. Both must be SemVer core versions (exit 2 otherwise).
#   scripts/release-check.sh pending-line TASTY_REV
#       Prints the pending Requires Tasty line for a build from TASTY_REV.
#   scripts/release-check.sh marker NOTES PREFIX
#       Prints the value of the one PREFIX line of the marker block, such as Commit.
#   scripts/release-check.sh prepare NOTES META
#       Prints NOTES with exactly one pending line for the tasty_rev of META
#       (release-meta.toml): adds it when missing, keeps one of identical copies. Fails
#       when the block holds another Requires Tasty line or a Verified with Tasty line.
#   scripts/release-check.sh verify NOTES META
#       The marker block of a pre-release matches META: one Commit line with its commit,
#       one tasty rev line with its tasty_rev, one pending line pinned to that tasty_rev,
#       and no other Requires Tasty or Verified with Tasty line.
#   scripts/release-check.sh promote NOTES MIN NEW VERIFIED_ON
#       Prints NOTES with the pending line replaced by the promoted lines.
#   scripts/release-check.sh promoted NOTES MIN NEW VERIFIED_ON
#       The marker block already holds exactly the promoted lines for these inputs.
#   scripts/release-check.sh sums LIST CONTEXT
#       Prints "<file> <sha256>" for each line of the SHA256SUMS list LIST, in its order.
#       Every line, a last line without a final newline too, must be "<sha256>  <file>":
#       64 lowercase hex digits, two spaces, and a file name without spaces or slashes
#       (SUMS_RE below). A list that is missing, not a regular file, or not readable, a list
#       with a NUL byte, and a list with any other line fail with one message that starts
#       with CONTEXT and names the list. Promotion (check 7) and the publish job call it.
#   scripts/release-check.sh latest
#       Prints the tag of the Latest Release of GH_REPO, or nothing when the repository
#       reads and has no Latest Release. Any other failure to read fails.
#
# Exit codes: 0 yes or done, 1 no or check failed, 2 usage error.
# Needs bash 3.2 or later (the macOS /bin/bash): no bash 4 syntax. latest also needs gh, and
# GH_REPO and GH_TOKEN in the environment.
set -euo pipefail

# Messages show as annotations in a workflow log.
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
  exit 2
}

SEMVER_RE='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
SHA_RE='^[0-9a-f]{40}$'
SUMS_RE='^([0-9a-f]{64})  ([^[:space:]/]+)$'

is_semver() {
  [[ "$1" =~ $SEMVER_RE ]]
}

# Compares two numbers of any length: shorter is lower (no leading zeros), then by text.
num_lt() {
  if ((${#1} != ${#2})); then
    ((${#1} < ${#2}))
  else
    [[ "$1" < "$2" ]]
  fi
}

semver_lt() {
  local a=$1 b=$2 i
  is_semver "$a" || usage "not a SemVer version: '$a'"
  is_semver "$b" || usage "not a SemVer version: '$b'"
  local -a x y
  IFS=. read -r -a x <<<"$a"
  IFS=. read -r -a y <<<"$b"
  for i in 0 1 2; do
    if [[ "${x[i]}" != "${y[i]}" ]]; then
      num_lt "${x[i]}" "${y[i]}"
      return
    fi
  done
  return 1
}

pending_line() {
  [[ "$1" =~ $SHA_RE ]] || fail "not a tasty commit: '$1'"
  echo "Requires Tasty: pending (a Tasty release that includes tasty commit $1)"
}

# Splits the notes in $1 into BODY (the lines above the marker block, without the blank
# lines before it) and BLOCK (the marker block). Carriage returns are dropped.
split_notes() {
  local file=$1 line
  local -a lines=()
  [[ -f "$file" ]] || usage "no notes file: $file"
  while IFS= read -r line || [[ -n "$line" ]]; do
    lines+=("${line%$'\r'}")
  done <"$file"
  local n=${#lines[@]}
  while ((n > 0)) && [[ -z "${lines[n - 1]//[[:space:]]/}" ]]; do n=$((n - 1)); done
  local start=$n
  while ((start > 0)) && [[ -n "${lines[start - 1]//[[:space:]]/}" ]]; do start=$((start - 1)); done
  local end=$start
  while ((end > 0)) && [[ -z "${lines[end - 1]//[[:space:]]/}" ]]; do end=$((end - 1)); done
  # ${a[@]+...} keeps an empty array from failing set -u in bash 3.2.
  BLOCK=(${lines[@]+"${lines[@]:start:n-start}"})
  BODY=(${lines[@]+"${lines[@]:0:end}"})
  ((${#BLOCK[@]} > 0)) || fail "the release notes have no marker block"
  for line in "${BLOCK[@]}"; do
    case "$line" in
      "Commit: "* | "tasty rev: "* | "Requires Tasty: "* | "Verified with Tasty: "*) ;;
      *) fail "the last paragraph of the release notes is not a marker block: '$line'" ;;
    esac
  done
}

# Prints the lines of BLOCK that start with "$1: ".
block_lines() {
  local line
  for line in "${BLOCK[@]}"; do
    if [[ "$line" == "$1: "* ]]; then printf '%s\n' "$line"; fi
  done
}

# The value of the one "$1: " line of BLOCK. Fails when there is none or more than one.
one_value() {
  local found count
  found=$(block_lines "$1")
  count=0
  [[ -z "$found" ]] || count=$(wc -l <<<"$found")
  ((count == 1)) || fail "the marker block has $count '$1:' lines, expected one"
  printf '%s\n' "${found#"$1: "}"
}

# Prints BODY, a blank line, and the lines given as arguments.
print_notes() {
  if ((${#BODY[@]} > 0)); then
    printf '%s\n' "${BODY[@]}"
    echo
  fi
  printf '%s\n' "$@"
}

# The one value of key $2 in release-meta.toml $1, a 40 digit commit.
meta_sha() {
  local found count
  [[ -f "$1" ]] || usage "no release-meta.toml: $1"
  found=$(sed -n "s/^$2 = \"\\(.*\\)\"\$/\\1/p" "$1")
  count=0
  [[ -z "$found" ]] || count=$(wc -l <<<"$found")
  ((count == 1)) || fail "$1 has $count '$2' values, expected one"
  [[ "$found" =~ $SHA_RE ]] || fail "$1 has $2 = '$found', not a commit"
  printf '%s\n' "$found"
}

prepare() {
  local notes=$1 meta=$2 rev expected line
  local -a kept=()
  rev=$(meta_sha "$meta" tasty_rev)
  expected=$(pending_line "$rev")
  split_notes "$notes"
  for line in "${BLOCK[@]}"; do
    case "$line" in
      "Requires Tasty: "*)
        [[ "$line" == "$expected" ]] ||
          fail "the marker block has '$line'; a release in publish takes only '$expected'"
        ;;
      "Verified with Tasty: "*)
        fail "the marker block has '$line'; a release in publish is not verified yet"
        ;;
      *) kept+=("$line") ;;
    esac
  done
  print_notes ${kept[@]+"${kept[@]}"} "$expected"
}

verify() {
  local notes=$1 meta=$2 commit rev value requires
  commit=$(meta_sha "$meta" commit)
  rev=$(meta_sha "$meta" tasty_rev)
  split_notes "$notes"
  value=$(one_value Commit)
  [[ "$value" == "$commit" ]] || fail "the marker block has 'Commit: $value', release-meta.toml has $commit"
  value=$(one_value "tasty rev")
  [[ "$value" == "$rev" ]] || fail "the marker block has 'tasty rev: $value', release-meta.toml has $rev"
  requires=$(block_lines "Requires Tasty")
  [[ "$requires" == "$(pending_line "$rev")" ]] ||
    fail "the marker block must hold exactly one Requires Tasty line, '$(pending_line "$rev")'; found: ${requires:-none}"
  [[ -z "$(block_lines "Verified with Tasty")" ]] ||
    fail "the marker block of a pre-release has a Verified with Tasty line"
}

check_promotion_inputs() {
  is_semver "$1" || usage "the minimum Tasty version is not MAJOR.MINOR.PATCH: '$1'"
  is_semver "$2" || usage "the bundling Tasty version is not MAJOR.MINOR.PATCH: '$2'"
  [[ -n "${3//[[:space:]]/}" ]] || usage "the list of verified platforms is empty"
  [[ "$3" != *$'\n'* && "$3" != *$'\r'* ]] || usage "the list of verified platforms must be one line"
}

requires_line() { echo "Requires Tasty: $1 or later"; }
verified_line() { echo "Verified with Tasty: $1 (minimum), $2 (bundled) on $3"; }

promote() {
  local notes=$1 min=$2 new=$3 on=$4 line pending=0
  local -a out=()
  check_promotion_inputs "$min" "$new" "$on"
  split_notes "$notes"
  for line in "${BLOCK[@]}"; do
    case "$line" in
      "Requires Tasty: pending"*)
        pending=$((pending + 1))
        out+=("$(requires_line "$min")")
        ;;
      "Requires Tasty: "* | "Verified with Tasty: "*)
        fail "the marker block already has '$line'"
        ;;
      *) out+=("$line") ;;
    esac
  done
  ((pending == 1)) || fail "the marker block has $pending pending Requires Tasty lines, expected one"
  print_notes "${out[@]}" "$(verified_line "$min" "$new" "$on")"
}

promoted() {
  local notes=$1 min=$2 new=$3 on=$4
  check_promotion_inputs "$min" "$new" "$on"
  split_notes "$notes"
  [[ "$(block_lines "Requires Tasty")" == "$(requires_line "$min")" ]] || return 1
  [[ "$(block_lines "Verified with Tasty")" == "$(verified_line "$min" "$new" "$on")" ]] || return 1
}

# The pairs of the SHA256SUMS list $1, judged line by line. $2 starts every message.
sums() {
  local file=$1 context=$2 name line
  local -a out=()
  name=${file##*/}
  [[ -f "$file" && -r "$file" ]] || fail "$context: cannot read $name"
  # read -d '' succeeds only when it finds a NUL. A plain read drops a NUL (bash 5) or ends
  # the line there (bash 3.2), so a NUL is refused before the lines are read.
  if IFS= read -r -d '' line <"$file"; then
    fail "$context: $name has a NUL byte"
  fi
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ $SUMS_RE ]] || fail "$context: unexpected line in $name: '$line'"
    out+=("${BASH_REMATCH[2]} ${BASH_REMATCH[1]}")
  done <"$file"
  if ((${#out[@]} > 0)); then printf '%s\n' "${out[@]}"; fi
}

lower() {
  printf '%s\n' "$1" | tr '[:upper:]' '[:lower:]'
}

latest() {
  local err out name
  [[ -n "${GH_REPO:-}" ]] || usage "GH_REPO is not set"
  err=$(mktemp)
  if out=$(gh api "repos/$GH_REPO/releases/latest" --jq .tag_name 2>"$err"); then
    rm -f "$err"
    [[ "$out" =~ ^v[^[:space:]]+$ ]] || fail "unexpected Latest Release reply: '$out'"
    printf '%s\n' "$out"
    return 0
  fi
  if ! grep -q '(HTTP 404)' "$err"; then
    cat "$err" >&2
    rm -f "$err"
    fail "cannot read the Latest Release of $GH_REPO"
  fi
  rm -f "$err"
  # A repository that cannot be read also answers 404, so read it before taking the 404
  # as "no Latest Release".
  name=$(gh api "repos/$GH_REPO" --jq .full_name) || fail "cannot read the repository $GH_REPO"
  [[ "$(lower "$name")" == "$(lower "$GH_REPO")" ]] || fail "unexpected repository reply: '$name'"
}

cmd=${1:-}
[[ $# -gt 0 ]] && shift
case "$cmd" in
  semver)
    [[ $# -eq 1 ]] || usage "usage: release-check.sh semver VERSION"
    is_semver "$1" || fail "not a SemVer version (MAJOR.MINOR.PATCH, no leading zeros): '$1'"
    ;;
  tag)
    [[ $# -eq 2 ]] || usage "usage: release-check.sh tag TAG VERSION"
    is_semver "$2" || fail "VERSION must be MAJOR.MINOR.PATCH without leading zeros, found '$2'"
    [[ "$1" == "v$2" ]] || fail "Tag $1 does not match VERSION $2 (expected v$2)"
    ;;
  semver-lt)
    [[ $# -eq 2 ]] || usage "usage: release-check.sh semver-lt A B"
    semver_lt "$1" "$2"
    ;;
  pending-line)
    [[ $# -eq 1 ]] || usage "usage: release-check.sh pending-line TASTY_REV"
    pending_line "$1"
    ;;
  marker)
    [[ $# -eq 2 ]] || usage "usage: release-check.sh marker NOTES PREFIX"
    split_notes "$1"
    one_value "$2"
    ;;
  prepare)
    [[ $# -eq 2 ]] || usage "usage: release-check.sh prepare NOTES META"
    prepare "$1" "$2"
    ;;
  verify)
    [[ $# -eq 2 ]] || usage "usage: release-check.sh verify NOTES META"
    verify "$1" "$2"
    ;;
  promote)
    [[ $# -eq 4 ]] || usage "usage: release-check.sh promote NOTES MIN NEW VERIFIED_ON"
    promote "$@"
    ;;
  promoted)
    [[ $# -eq 4 ]] || usage "usage: release-check.sh promoted NOTES MIN NEW VERIFIED_ON"
    promoted "$@"
    ;;
  sums)
    [[ $# -eq 2 ]] || usage "usage: release-check.sh sums LIST CONTEXT"
    sums "$1" "$2"
    ;;
  latest)
    [[ $# -eq 0 ]] || usage "usage: release-check.sh latest"
    latest
    ;;
  *)
    usage "usage: release-check.sh semver|tag|semver-lt|pending-line|marker|prepare|verify|promote|promoted|sums|latest ..."
    ;;
esac
