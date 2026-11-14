#!/usr/bin/env bash
# Builds every plugin of this repository and copies it into a tasty builtin-plugins
# folder, laid out as tasty lays out its own builds: DIR/<id>/ holds the executable,
# tasty-plugin.toml, and lang/ when the plugin has one. Plugins whose manifest says
# `bundle = false` are included, as in tasty's development builds.
#
#   scripts/dev-bundle.sh --out DIR [--profile debug|release|dist] [--tasty DIR]
#                         [--target TRIPLE]
#
# --out      The builtin-plugins folder to fill, for example
#            ../tasty/target/debug/builtin-plugins. Only the <id> folders of the plugins
#            built here are replaced, each as a whole; anything else in DIR is left alone.
# --profile  debug (the default; cargo's dev profile, output in target/debug), release,
#            or dist.
# --tasty    A local tasty checkout, for example ../tasty. Every package that Cargo.lock
#            takes from the tasty git repository, including the tiny_http and epaint
#            copies, is built from that checkout instead, with cargo --config patch
#            options. The build runs in a copy of this repository under
#            target/dev-bundle/, so Cargo.lock here does not change. Without --tasty the
#            build runs here with --locked.
# --target   A target triple to build for.
#
# Nothing is signed and no .sig file is written. tasty's debug builds do not check
# plugin signatures. A tasty release or dist build loads the plugins in DIR only after
# tasty signs their manifests with the key it was built with (tasty's
# scripts/sign-bundle.sh). Needs bash, tar, awk, and cargo. Runs under Git Bash on
# Windows.
set -euo pipefail

# --out, --tasty, and CARGO_TARGET_DIR are relative to where the script is run from.
# They are made absolute before the script changes directory.
caller=$PWD
abs_path() {
  case "$1" in
    /* | [A-Za-z]:[\\/]*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "$caller" "$1" ;;
  esac
}
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR=$(abs_path "$CARGO_TARGET_DIR")
  export CARGO_TARGET_DIR
fi
cd "$(dirname "$0")/.."
repo=$PWD

die() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-2}"
}

# shellcheck source=scripts/lib/plugin-build.sh
. scripts/lib/plugin-build.sh

TASTY_GIT=https://github.com/zilhak/tasty

OUT=""
PROFILE=debug
TASTY=""
TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) [[ $# -ge 2 ]] || usage; OUT="$2"; shift 2 ;;
    --profile) [[ $# -ge 2 ]] || usage; PROFILE="$2"; shift 2 ;;
    --tasty) [[ $# -ge 2 ]] || usage; TASTY="$2"; shift 2 ;;
    --target) [[ $# -ge 2 ]] || usage; TARGET="$2"; shift 2 ;;
    -h | --help) usage 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ -n "$OUT" ]] || die "--out is required"
OUT=$(abs_path "$OUT")
if [[ -n "$TASTY" ]]; then TASTY=$(abs_path "$TASTY"); fi

# cargo has no profile named debug: the debug build is the dev profile, chosen by
# passing no profile at all.
build_args=()
case "$PROFILE" in
  debug) ;;
  release) build_args+=(--release) ;;
  dist) build_args+=(--profile dist) ;;
  *) die "--profile must be debug, release, or dist" ;;
esac
[[ -n "$TARGET" ]] && build_args+=(--target "$TARGET")

# A path as cargo on this platform reads it, inside a TOML string.
toml_path() {
  local p="$1"
  if command -v cygpath >/dev/null 2>&1; then p=$(cygpath -m "$p"); fi
  p=${p//\\/\\\\}
  p=${p//\"/\\\"}
  printf '"%s"' "$p"
}

# Names of the packages that the Cargo.lock in $1 takes from the tasty git repository.
tasty_git_packages() {
  awk -v src="\"git+$TASTY_GIT?" '
    /^\[\[package\]\]/ { name = "" }
    /^name = / { name = $3; gsub(/"/, "", name) }
    /^source = / && index($3, src) == 1 { print name }
  ' "$1/Cargo.lock"
}

# Names under [patch.crates-io] in the Cargo.toml in $1.
crates_io_patches() {
  awk '
    /^\[/ { in_patch = ($0 == "[patch.crates-io]"); next }
    in_patch && /^[A-Za-z0-9_-]+[[:space:]]*=/ { sub(/[[:space:]]*=.*/, ""); print }
  ' "$1/Cargo.toml"
}

bins=$(scripts/check.sh list-bins) || die "cannot list the plugin crates"
mkdir -p "$OUT"
out=$(cd "$OUT" && pwd)

build_dir=$repo
cargo_opts=(--locked)
if [[ -n "$TASTY" ]]; then
  tasty=$(cd "$TASTY" 2>/dev/null && pwd) || die "--tasty: no such directory: $TASTY"
  [[ -f "$tasty/Cargo.toml" && -d "$tasty/crates" ]] || die "--tasty: $tasty is not a tasty checkout"

  # Build in a copy, so the Cargo.lock that the patches change stays out of this
  # repository. tar keeps the modification times, so cargo does not rebuild unchanged
  # crates on the next run.
  work="${CARGO_TARGET_DIR:-$repo/target}/dev-bundle"
  build_dir="$work/src"
  rm -rf "$build_dir"
  mkdir -p "$build_dir"
  # Tracked and new files as they are in the working tree; a deleted file is left out.
  git ls-files -z --cached --others --exclude-standard >"$work/files" \
    || die "cannot list the files of this repository"
  while IFS= read -r -d '' f; do
    if [[ -e "$f" ]]; then printf '%s\0' "$f"; fi
  done <"$work/files" | tar --null -T - -cf - | tar -xf - -C "$build_dir"
  export CARGO_TARGET_DIR="$work/target"

  io_patches=$(crates_io_patches "$build_dir")
  packages=$(tasty_git_packages "$build_dir")
  [[ -n "$packages" ]] || die "Cargo.lock takes no package from $TASTY_GIT"
  cargo_opts=()
  while IFS= read -r name; do
    path=""
    for d in crates vendor; do
      if [[ -f "$tasty/$d/$name/Cargo.toml" ]]; then path="$tasty/$d/$name"; break; fi
    done
    [[ -n "$path" ]] || die "--tasty: $tasty has no crates/$name or vendor/$name"
    if grep -qx "$name" <<<"$io_patches"; then
      cargo_opts+=(--config "patch.crates-io.$name.path=$(toml_path "$path")")
    else
      cargo_opts+=(--config "patch.\"$TASTY_GIT\".$name.path=$(toml_path "$path")")
    fi
    echo "using $path for $name" >&2
  done <<<"$packages"
fi

pkg_args=()
while IFS= read -r crate; do
  [[ -n "$crate" ]] && pkg_args+=(-p "$crate")
done <<<"$bins"
[[ ${#pkg_args[@]} -gt 0 ]] || die "no plugin crates to build"

cd "$build_dir"
echo "+ cargo build ${cargo_opts[*]} ${pkg_args[*]} ${build_args[*]}" >&2
cargo build "${cargo_opts[@]}" "${pkg_args[@]}" "${build_args[@]}"
# A second, fresh build prints the artifact messages without rebuilding.
artifacts=$(
  cargo build "${cargo_opts[@]}" "${pkg_args[@]}" "${build_args[@]}" \
    --message-format=json-render-diagnostics | cargo_executables
)

if [[ -n "$TASTY" ]]; then
  # Every patched package must resolve to the local checkout: a lock entry without a
  # source line. A patch that cargo did not use is listed under [[patch.unused]], and the
  # package then comes from git, or for a [patch.crates-io] name from crates.io (cargo
  # only warns). Fail instead of handing out plugins built against another copy.
  wrong=$(awk -v names="$(tr '\n' ' ' <<<"$packages")" '
    BEGIN { n = split(names, a, " "); for (i = 1; i <= n; i++) patched[a[i]] = 1 }
    function flush() {
      if (kind == "unused") print "  " name " " version ": patch not used"
      else if (kind == "package" && (name in patched) && src != "") print "  " name " " version ": from " src
      kind = ""; name = ""; version = ""; src = ""
    }
    /^\[\[package\]\]/ { flush(); kind = "package"; next }
    /^\[\[patch\.unused\]\]/ { flush(); kind = "unused"; next }
    /^\[/ { flush(); next }
    /^name = / { name = $3; gsub(/"/, "", name) }
    /^version = / { version = $3; gsub(/"/, "", version) }
    /^source = / { src = $3; gsub(/"/, "", src) }
    END { flush() }
  ' "$build_dir/Cargo.lock")
  [[ -z "$wrong" ]] || die "not built from $tasty:
$wrong
A version in $tasty that the requirements here do not accept leaves its patch unused."
fi

staging=""
old=""
cleanup() {
  if [[ -n "$staging" ]]; then rm -rf "$staging"; fi
  if [[ -n "$old" ]]; then rm -rf "$old"; fi
  return 0
}
trap cleanup EXIT

count=0
while IFS= read -r crate; do
  [[ -n "$crate" ]] || continue
  src="crates/$crate"
  manifest="$src/tasty-plugin.toml"
  [[ -f "$manifest" ]] || die "missing $manifest"
  id=$(manifest_string id "$manifest")
  exe=$(awk -F'\t' -v c="$crate" '$1 == c { print $2; exit }' <<<"$artifacts")
  [[ -n "$exe" ]] || die "cargo reported no executable for $crate"
  exe=$(to_unix_path "$exe")
  [[ -f "$exe" ]] || die "executable missing: $exe"

  # Fill a folder next to the target, then swap it in, so DIR/<id> is never half
  # written. A .sig left from an earlier signed build goes with the old folder: it
  # would not match the new manifest.
  staging="$out/.$id.dev-bundle.$$"
  rm -rf "$staging"
  mkdir -p "$staging"
  cp "$exe" "$staging/"
  cp "$manifest" "$staging/tasty-plugin.toml"
  if [[ -d "$src/lang" ]]; then
    cp -R "$src/lang" "$staging/lang"
  fi
  if [[ -e "$out/$id" ]]; then
    old="$out/.$id.old.$$"
    mv "$out/$id" "$old"
  fi
  mv "$staging" "$out/$id"
  staging=""
  if [[ -n "$old" ]]; then
    rm -rf "$old"
    old=""
  fi
  echo "bundled $id ($crate) into $out/$id" >&2
  count=$((count + 1))
done <<<"$bins"

[[ $count -gt 0 ]] || die "no plugins bundled"
echo "bundled $count plugins into $out ($PROFILE${TARGET:+, $TARGET}); nothing is signed" >&2
