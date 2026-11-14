#!/usr/bin/env bash
# Builds the plugin binaries and packs one archive per plugin, plus SHA-256 lists.
#
#   scripts/package-plugin.sh stage --platform NAME --out DIR [--target TRIPLE]
#                                   [--profile NAME] [--all]
#   scripts/package-plugin.sh pack --platform NAME --out DIR
#   scripts/package-plugin.sh all --platform NAME --out DIR [stage options]
#
# The steps run in this order: stage, pack. The packages are not signed, so pack
# fails if a .sig file would go into an archive. tasty signs the manifests of the
# plugins it bundles when it packages them.
#
# stage   Builds with `scripts/check.sh build` (profile dist by default) and copies
#         each plugin into DIR/NAME/stage/<id>/: the executable, tasty-plugin.toml,
#         lang/, and the repository LICENSE. Plugins whose manifest says
#         `bundle = false` are skipped unless --all is given, as in tasty's own
#         packaging scripts.
# pack    Writes DIR/NAME/dist/<id>-<version>-NAME.tar.gz for each staged plugin,
#         then DIR/NAME/dist/SHA256SUMS-NAME.txt (sha256sum format) and
#         DIR/NAME/dist/plugins-NAME.tsv (id, version, platform, file, sha256).
#         Fails if the stage or an archive contains a .sig file.
#
# NAME is a platform label such as linux-x64 or windows-arm64. The executable path
# comes from cargo's compiler-artifact messages and the manifest and lang/ from
# crates/<crate>/ in this repository, so no target/ path is assembled by hand.
#
# Needs bash, tar, awk, and sha256sum or shasum. Runs under Git Bash on
# Windows.
set -euo pipefail

cd "$(dirname "$0")/.."

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
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

# Reads cargo's JSON messages from stdin and prints "<target name>\t<executable>"
# for each compiler-artifact message with an executable. It uses awk, so packaging
# needs neither jq nor Python. On Windows, python3 can be the Microsoft Store alias,
# which exits at once without reading its input.
#
# Each message is one line of compact JSON. In the executable path only the
# escapes \\, \", and \/ are accepted: cargo escapes only those and control
# characters, which a path does not hold. Any other escape fails. The target name
# is read from the "target" object, which holds no nested object.
cargo_executables() {
  tr -d '\r' | awk '
    function unescape(s,    out, i, c) {
      out = ""
      for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "\\") {
          i++
          c = substr(s, i, 1)
          if (c != "\\" && c != "\"" && c != "/") { bad = 1; return "" }
        }
        out = out c
      }
      return out
    }
    index($0, "{\"reason\":\"compiler-artifact\"") == 1 {
      if (!match($0, /"executable":"([^"\\]|\\.)*"/)) next
      exe = unescape(substr($0, RSTART + 14, RLENGTH - 15))
      if (bad) { print "unsupported escape in a cargo executable path: " $0 > "/dev/stderr"; exit 1 }
      if (!match($0, /"target":[{][^}]*[}]/)) next
      target = substr($0, RSTART, RLENGTH)
      if (!match(target, /"name":"[^"\\]*"/)) next
      print substr(target, RSTART + 8, RLENGTH - 9) "\t" exe
    }'
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
      cargo_executables
  )

  # Read before the loop: a process substitution would hide a failure of
  # list-bins after it printed part of the list.
  local bins
  bins=$(scripts/check.sh list-bins) || die "cannot list the plugin crates"

  rm -rf "$STAGE_DIR"
  mkdir -p "$STAGE_DIR"

  local crate src manifest id exe count=0
  while IFS= read -r crate; do
    [[ -n "$crate" ]] || continue
    src="crates/$crate"
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
    cp LICENSE "$STAGE_DIR/$id/LICENSE"
    if [[ -d "$src/lang" ]]; then
      cp -R "$src/lang" "$STAGE_DIR/$id/lang"
    fi
    echo "staged $id ($crate)" >&2
    count=$((count + 1))
  done <<<"$bins"

  [[ $count -gt 0 ]] || die "no plugins staged"
  echo "staged $count plugins in $STAGE_DIR" >&2
}

cmd_pack() {
  [[ -d "$STAGE_DIR" ]] || die "nothing staged in $STAGE_DIR"
  local sigs
  sigs=$(find "$STAGE_DIR" -name '*.sig')
  [[ -z "$sigs" ]] || die "signature files in the stage; packages are not signed: $sigs"
  rm -rf "$DIST_DIR"
  mkdir -p "$DIST_DIR"

  local sums="$DIST_DIR/SHA256SUMS-$PLATFORM.txt"
  local index="$DIST_DIR/plugins-$PLATFORM.tsv"
  printf 'id\tversion\tplatform\tfile\tsha256\n' >"$index"
  : >"$sums"

  local dir id version file hash list
  for dir in "$STAGE_DIR"/*/; do
    id=$(basename "$dir")
    version=$(manifest_string version "$dir/tasty-plugin.toml")
    file="$id-$version-$PLATFORM.tar.gz"
    tar -czf "$DIST_DIR/$file" -C "$STAGE_DIR" "$id"
    # Read first, then grep: with pipefail, grep -q ending early can make the
    # pipeline fail through SIGPIPE and hide a match.
    list=$(tar -tzf "$DIST_DIR/$file")
    if grep -q '\.sig$' <<<"$list"; then
      die "$file contains a .sig file; packages are not signed"
    fi
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
  pack) parse_args "$@"; cmd_pack ;;
  all) parse_args "$@"; cmd_stage; cmd_pack ;;
  -h | --help | help) usage 0 ;;
  *) die "unknown subcommand: $sub" ;;
esac
