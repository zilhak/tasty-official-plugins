# The third-party notice set that goes into every plugin archive: LICENSE,
# THIRD_PARTY_LICENSES.md, and every file in LICENSES/. Source it with the repository
# root as the working directory. Follows tasty's scripts/lib/notice-set.sh.
# shellcheck shell=bash

# The asset notice a plugin crate may carry; staged as <id>/NOTICE.md. Read by the
# scripts that source this file.
# shellcheck disable=SC2034
NOTICE_ASSET="assets/NOTICE.md"

# Prints the notice set, one repository path per line. An empty LICENSES/ means a
# checkout without the license texts, so it fails.
notice_set_files() {
  local f found=0
  echo "LICENSE"
  echo "THIRD_PARTY_LICENSES.md"
  for f in LICENSES/*; do
    [[ -f "$f" ]] || continue
    echo "$f"
    found=1
  done
  if [[ "$found" -eq 0 ]]; then
    echo "error: LICENSES/ holds no license text" >&2
    return 1
  fi
}

# Copies the notice set into <dest>, keeping the LICENSES/ subpath so the links in
# THIRD_PARTY_LICENSES.md still work.
stage_notice() {
  local dest="$1" files f
  files=$(notice_set_files) || return 1
  mkdir -p "$dest/LICENSES"
  while IFS= read -r f; do
    cp "$f" "$dest/$f"
  done <<<"$files"
}

# Checks that each line of the notice set, with <prefix> in front, ends some line of
# <listing>. This checks names, not contents. The listing is taken as one string so a
# producer is not cut off by SIGPIPE.
verify_notice_listing() {
  local listing="$1" prefix="$2" label="$3" files f
  files=$(notice_set_files) || return 1
  while IFS= read -r f; do
    if ! awk -v t="$prefix$f" '
        length($0) >= length(t) && substr($0, length($0) - length(t) + 1) == t { hit = 1 }
        END { exit !hit }' <<<"$listing"; then
      echo "error: $prefix$f not in $label" >&2
      return 1
    fi
  done <<<"$files"
}
