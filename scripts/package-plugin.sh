#!/usr/bin/env bash
# Builds the plugin binaries and packs one archive per plugin, plus SHA-256 lists.
#
#   scripts/package-plugin.sh stage --platform NAME --out DIR [--target TRIPLE]
#                                   [--profile NAME] [--all]
#   scripts/package-plugin.sh sign-manifests --platform NAME --out DIR
#   scripts/package-plugin.sh pack --platform NAME --out DIR
#   scripts/package-plugin.sh all --platform NAME --out DIR [stage options]
#
# The steps run in this order: stage, (codesign binaries, done by the workflow),
# sign-manifests, pack. Signing works on the staged copies, never on the cargo
# git checkout, which is a shared cache.
#
# stage   Builds with `scripts/check.sh build` (profile dist by default) and copies
#         each plugin into DIR/NAME/stage/<id>/: the executable, tasty-plugin.toml,
#         and lang/. Plugins whose manifest says `bundle = false` are skipped
#         unless --all is given, as in tasty's own packaging scripts.
# sign-manifests
#         Signs each staged tasty-plugin.toml into tasty-plugin.toml.sig, the same
#         way as tasty's scripts/sign-bundle.sh (Ed25519 over the SHA-256 digest of
#         the manifest, raw 64 bytes). The private key is read from the PEM file
#         named by PLUGIN_MANIFEST_SIGNING_KEY_FILE. Without it, signing is skipped
#         and an "unsigned" line is logged.
# pack    Writes DIR/NAME/dist/<id>-<version>-NAME.tar.gz for each staged plugin,
#         then DIR/NAME/dist/SHA256SUMS-NAME.txt (sha256sum format) and
#         DIR/NAME/dist/plugins-NAME.tsv (id, version, platform, file, sha256).
#
# NAME is a platform label such as linux-x64 or windows-arm64. The executable path
# comes from cargo's compiler-artifact messages and the manifest from the pinned
# tasty checkout that cargo fetched, so no target/ path is assembled by hand.
#
# Needs bash, tar, sha256sum or shasum, and jq or python3. Runs under Git Bash on
# Windows. Signing needs openssl 1.1.1 or later.
set -euo pipefail

cd "$(dirname "$0")/.."

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

# Converts a Windows path from cargo to a form Git Bash tools accept.
to_unix_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s\n' "$1"
  fi
}

# Runs a jq filter, or the equivalent Python expression when jq is missing.
# $1 = jq filter, $2 = python code that reads JSON lines from stdin.
json_query() {
  if command -v jq >/dev/null 2>&1; then
    jq -r "$1"
  elif command -v python3 >/dev/null 2>&1; then
    python3 -I -c "$2"
  elif command -v python >/dev/null 2>&1; then
    python -I -c "$2"
  else
    die "jq or python3 is required to read cargo JSON output"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Reads a top-level string key from a tasty-plugin.toml.
manifest_string() {
  local key="$1" file="$2" value
  value=$(grep -m1 -E "^${key}[[:space:]]*=" "$file" | sed 's/.*"\([^"]*\)".*/\1/' || true)
  [[ -n "$value" ]] || die "cannot read '$key' from $file"
  printf '%s\n' "$value"
}

# Root of the tasty git checkout that holds the pinned plugin crates. cargo
# metadata leaves out the bin-only plugin crates, so the checkout is found
# through tasty-plugin-manifest, a library from the same pinned commit.
tasty_checkout() {
  local manifest_path
  manifest_path=$(
    cargo metadata --format-version 1 --locked 2>/dev/null |
      json_query \
        '.packages[] | select(.name == "tasty-plugin-manifest") | .manifest_path' \
        'import json,sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == "tasty-plugin-manifest": print(p["manifest_path"])'
  )
  [[ -n "$manifest_path" ]] || die "tasty-plugin-manifest not found in cargo metadata"
  manifest_path=$(to_unix_path "$manifest_path")
  # <checkout>/crates/tasty-plugin-manifest/Cargo.toml
  dirname "$(dirname "$(dirname "$manifest_path")")"
}

PLATFORM=""
OUT=""
TARGET=""
PROFILE="dist"
INCLUDE_ALL=0

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --platform) [[ $# -ge 2 ]] || usage; PLATFORM="$2"; shift 2 ;;
      --out) [[ $# -ge 2 ]] || usage; OUT="$2"; shift 2 ;;
      --target) [[ $# -ge 2 ]] || usage; TARGET="$2"; shift 2 ;;
      --profile) [[ $# -ge 2 ]] || usage; PROFILE="$2"; shift 2 ;;
      --all) INCLUDE_ALL=1; shift ;;
      -h | --help) usage 0 ;;
      *) die "unknown argument: $1" ;;
    esac
  done
  [[ -n "$PLATFORM" ]] || die "--platform is required"
  [[ -n "$OUT" ]] || die "--out is required"
  [[ "$PLATFORM" =~ ^[a-z0-9_-]+$ ]] || die "--platform must match [a-z0-9_-]+"
  STAGE_DIR="$OUT/$PLATFORM/stage"
  DIST_DIR="$OUT/$PLATFORM/dist"
}

cmd_stage() {
  local build_args=(--profile "$PROFILE")
  [[ -n "$TARGET" ]] && build_args+=(--target "$TARGET")

  scripts/check.sh build "${build_args[@]}"

  # A second, fresh build prints the artifact messages without rebuilding.
  local artifacts
  artifacts=$(
    scripts/check.sh build "${build_args[@]}" --message-format=json-render-diagnostics |
      json_query \
        'select(.reason == "compiler-artifact" and .executable != null) | "\(.target.name)\t\(.executable)"' \
        'import json,sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["target"]["name"] + "\t" + m["executable"])'
  )

  local checkout
  checkout=$(tasty_checkout)
  echo "tasty checkout: $checkout" >&2

  rm -rf "$STAGE_DIR"
  mkdir -p "$STAGE_DIR"

  local crate src manifest id exe count=0
  while IFS= read -r crate; do
    [[ -n "$crate" ]] || continue
    src="$checkout/crates/$crate"
    manifest="$src/tasty-plugin.toml"
    [[ -f "$manifest" ]] || die "missing $manifest"
    if [[ $INCLUDE_ALL -eq 0 ]] &&
      grep -Eq '^[[:space:]]*bundle[[:space:]]*=[[:space:]]*false' "$manifest"; then
      echo "skip $crate (bundle = false)" >&2
      continue
    fi
    id=$(manifest_string id "$manifest")
    exe=$(printf '%s\n' "$artifacts" | awk -F'\t' -v c="$crate" '$1 == c { print $2; exit }')
    [[ -n "$exe" ]] || die "cargo reported no executable for $crate"
    exe=$(to_unix_path "$exe")
    [[ -f "$exe" ]] || die "executable missing: $exe"

    mkdir -p "$STAGE_DIR/$id"
    cp "$exe" "$STAGE_DIR/$id/"
    cp "$manifest" "$STAGE_DIR/$id/tasty-plugin.toml"
    if [[ -d "$src/lang" ]]; then
      cp -R "$src/lang" "$STAGE_DIR/$id/lang"
    fi
    echo "staged $id ($crate)" >&2
    count=$((count + 1))
  done < <(scripts/check.sh list-bins)

  [[ $count -gt 0 ]] || die "no plugins staged"
  echo "staged $count plugins in $STAGE_DIR" >&2
}

cmd_sign_manifests() {
  [[ -d "$STAGE_DIR" ]] || die "nothing staged in $STAGE_DIR"
  local key="${PLUGIN_MANIFEST_SIGNING_KEY_FILE:-}"
  if [[ -z "$key" ]]; then
    echo "unsigned: PLUGIN_MANIFEST_SIGNING_KEY_FILE is not set, manifest signing skipped" >&2
    return 0
  fi
  [[ -f "$key" ]] || die "signing key not found: $key"
  command -v openssl >/dev/null 2>&1 || die "openssl not found"

  local tmp manifest sig size
  tmp=$(mktemp -d)
  # shellcheck disable=SC2064 # expand $tmp now
  trap "rm -rf '$tmp'" RETURN
  for manifest in "$STAGE_DIR"/*/tasty-plugin.toml; do
    sig="$manifest.sig"
    openssl dgst -sha256 -binary "$manifest" >"$tmp/digest"
    openssl pkeyutl -sign -inkey "$key" -rawin -in "$tmp/digest" -out "$sig"
    size=$(wc -c <"$sig" | tr -d ' ')
    if [[ "$size" != "64" ]]; then
      rm -f "$sig"
      die "$sig is $size bytes, expected 64 (is the key Ed25519?)"
    fi
    echo "signed $manifest" >&2
  done
}

cmd_pack() {
  [[ -d "$STAGE_DIR" ]] || die "nothing staged in $STAGE_DIR"
  rm -rf "$DIST_DIR"
  mkdir -p "$DIST_DIR"

  local sums="$DIST_DIR/SHA256SUMS-$PLATFORM.txt"
  local index="$DIST_DIR/plugins-$PLATFORM.tsv"
  printf 'id\tversion\tplatform\tfile\tsha256\n' >"$index"
  : >"$sums"

  local dir id version file hash
  for dir in "$STAGE_DIR"/*/; do
    id=$(basename "$dir")
    version=$(manifest_string version "$dir/tasty-plugin.toml")
    if [[ ! -f "$dir/tasty-plugin.toml.sig" ]]; then
      echo "unsigned: $id has no tasty-plugin.toml.sig" >&2
    fi
    file="$id-$version-$PLATFORM.tar.gz"
    tar -czf "$DIST_DIR/$file" -C "$STAGE_DIR" "$id"
    hash=$(sha256_of "$DIST_DIR/$file")
    printf '%s  %s\n' "$hash" "$file" >>"$sums"
    printf '%s\t%s\t%s\t%s\t%s\n' "$id" "$version" "$PLATFORM" "$file" "$hash" >>"$index"
    echo "packed $file" >&2
  done
}

sub="${1:-}"
[[ -n "$sub" ]] || usage
shift

case "$sub" in
  stage) parse_args "$@"; cmd_stage ;;
  sign-manifests) parse_args "$@"; cmd_sign_manifests ;;
  pack) parse_args "$@"; cmd_pack ;;
  all) parse_args "$@"; cmd_stage; cmd_sign_manifests; cmd_pack ;;
  -h | --help | help) usage 0 ;;
  *) die "unknown subcommand: $sub" ;;
esac
