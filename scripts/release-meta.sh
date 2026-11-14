#!/usr/bin/env bash
# Writes release-meta.toml, the release metadata that goes next to the
# plugins-<platform>.tsv files of a release. tasty reads it with the TSV files to
# build its official plugin lock file.
#
#   scripts/release-meta.sh [--tag TAG] [--commit SHA] [--out FILE]
#
# --tag     Release tag, such as v0.1.0. Default: unreleased, for a run that
#           is not a tag release (a manual workflow run or a local check).
# --commit  Commit of this repository that the packages were built from.
#           Default: the commit checked out (HEAD).
# --out     Output file. Default: standard output.
#
# [release] values and where they come from:
#   repository         zilhak/tasty-official-plugins
#   tag, commit        the options above
#   tasty_rev          the tasty rev pinned in the root Cargo.toml
#   host_api_version   HOST_API_VERSION in crates/tasty-plugin-manifest/src/types.rs
#   manifest_version   MANIFEST_VERSION in the same file
#   protocol_version   version of the tasty-plugin-protocol package
#   epaint_version     version of epaint in Cargo.lock
#   epaint_patch_tree  git tree id of vendor/epaint at tasty_rev
# The tasty values are read from the cargo git checkout of the tasty crates at
# tasty_rev, so they follow the pin. Each [[plugin]] entry (id, version, bundle)
# comes from the tasty-plugin.toml of a crate in `scripts/check.sh list-bins`;
# bundle is true unless the manifest says otherwise.
#
# Needs bash, git, cargo, and jq or python3. Runs under Git Bash on Windows.
set -euo pipefail

cd "$(dirname "$0")/.."

REPOSITORY="zilhak/tasty-official-plugins"
TASTY_GIT="https://github.com/zilhak/tasty"

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

# Runs a jq filter, or the equivalent Python code when jq is missing. Output goes
# through tr -d '\r' because jq.exe and Python on Windows write CRLF.
json_query() {
  if command -v jq >/dev/null 2>&1; then
    jq -r "$1" | tr -d '\r'
  elif command -v python3 >/dev/null 2>&1; then
    python3 -I -c "$2" | tr -d '\r'
  elif command -v python >/dev/null 2>&1; then
    python -I -c "$2" | tr -d '\r'
  else
    die "jq or python3 is required to read cargo metadata"
  fi
}

to_unix_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s\n' "$1"
  fi
}

# Prints the single value a command printed, or fails.
one_value() {
  local what="$1" values="$2" count
  count=$(printf '%s' "$values" | grep -c . || true)
  [[ "$count" == "1" ]] || die "expected one $what, found $count: $values"
  printf '%s\n' "$values"
}

# Reads a top-level key of a tasty-plugin.toml, before the first [table].
manifest_value() {
  local key="$1" file="$2"
  awk -v key="$key" '
    /^[[:space:]]*\[/ { exit }
    $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
      sub(/^[^=]*=[[:space:]]*/, ""); sub(/[[:space:]]*(#.*)?$/, ""); gsub(/"/, "")
      print; exit
    }' "$file"
}

TAG="unreleased"
COMMIT=""
OUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) [[ $# -ge 2 ]] || usage; TAG="$2"; shift 2 ;;
    --commit) [[ $# -ge 2 ]] || usage; COMMIT="$2"; shift 2 ;;
    --out) [[ $# -ge 2 ]] || usage; OUT="$2"; shift 2 ;;
    -h | --help) usage 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ "$TAG" =~ ^[A-Za-z0-9._-]+$ ]] || die "--tag must match [A-Za-z0-9._-]+: $TAG"
[[ -n "$COMMIT" ]] || COMMIT=$(git rev-parse HEAD)
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || die "--commit must be a full commit sha: $COMMIT"

tasty_rev=$(one_value "tasty rev in Cargo.toml" "$(
  grep -F "$TASTY_GIT\"" Cargo.toml | grep -o 'rev = "[0-9a-f]\{40\}"' |
    sed 's/rev = "\(.*\)"/\1/' | sort -u
)")

meta=$(cargo metadata --format-version 1 --locked)
manifest_path=$(one_value "tasty-plugin-manifest package" "$(
  printf '%s\n' "$meta" | json_query \
    '.packages[] | select(.name == "tasty-plugin-manifest") | .manifest_path' \
    'import json,sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == "tasty-plugin-manifest": print(p["manifest_path"])'
)")
protocol_version=$(one_value "tasty-plugin-protocol package" "$(
  printf '%s\n' "$meta" | json_query \
    '.packages[] | select(.name == "tasty-plugin-protocol") | .version' \
    'import json,sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == "tasty-plugin-protocol": print(p["version"])'
)")

# <checkout>/crates/tasty-plugin-manifest/Cargo.toml
checkout=$(dirname "$(dirname "$(dirname "$(to_unix_path "$manifest_path")")")")
checkout_rev=$(git -C "$checkout" rev-parse HEAD) ||
  die "the tasty checkout at $checkout is not a git checkout"
[[ "$checkout_rev" == "$tasty_rev" ]] ||
  die "the tasty checkout is at $checkout_rev, not at the pinned $tasty_rev"

types="$checkout/crates/tasty-plugin-manifest/src/types.rs"
host_api_version=$(one_value "HOST_API_VERSION in $types" "$(
  sed -n 's/^pub const HOST_API_VERSION: &str = "\([^"]*\)";.*/\1/p' "$types"
)")
manifest_version=$(one_value "MANIFEST_VERSION in $types" "$(
  sed -n 's/^pub const MANIFEST_VERSION: u32 = \([0-9][0-9]*\);.*/\1/p' "$types"
)")
epaint_patch_tree=$(git -C "$checkout" rev-parse "$tasty_rev:vendor/epaint")

epaint_version=$(one_value "epaint in Cargo.lock" "$(
  awk '/^\[\[package\]\]/ { name = "" }
       /^name = / { name = $3 }
       /^version = / && name == "\"epaint\"" { gsub(/"/, "", $3); print $3 }' Cargo.lock
)")

# Built in full before anything is written, so a failure leaves no partial file.
render() {
  echo "# Release metadata of $REPOSITORY. Written by scripts/release-meta.sh."
  echo "schema = 1"
  echo
  echo "[release]"
  echo "repository = \"$REPOSITORY\""
  echo "tag = \"$TAG\""
  echo "commit = \"$COMMIT\""
  echo "tasty_rev = \"$tasty_rev\""
  echo "host_api_version = \"$host_api_version\""
  echo "protocol_version = \"$protocol_version\""
  echo "manifest_version = $manifest_version"
  echo "epaint_version = \"$epaint_version\""
  echo "epaint_patch_tree = \"$epaint_patch_tree\""

  local count=0 crate manifest id version bundle
  while IFS= read -r crate; do
    [[ -n "$crate" ]] || continue
    manifest="crates/$crate/tasty-plugin.toml"
    [[ -f "$manifest" ]] || die "missing $manifest"
    id=$(manifest_value id "$manifest")
    version=$(manifest_value version "$manifest")
    bundle=$(manifest_value bundle "$manifest")
    [[ -n "$id" && -n "$version" ]] || die "cannot read id and version from $manifest"
    case "${bundle:-true}" in
      true | false) ;;
      *) die "bundle in $manifest is neither true nor false: $bundle" ;;
    esac
    echo
    echo "[[plugin]]"
    echo "id = \"$id\""
    echo "version = \"$version\""
    echo "bundle = ${bundle:-true}"
    count=$((count + 1))
  done < <(scripts/check.sh list-bins)
  [[ $count -gt 0 ]] || die "no plugins listed"
}

text=$(render)
if [[ -n "$OUT" ]]; then
  printf '%s\n' "$text" >"$OUT"
else
  printf '%s\n' "$text"
fi
