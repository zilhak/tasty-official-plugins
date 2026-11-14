# The third-party notice set that goes into every plugin archive: LICENSE,
# THIRD_PARTY_LICENSES.md, and every file in LICENSES/. Source it with the repository
# root as the working directory. Follows tasty's scripts/lib/notice-set.sh.
# shellcheck shell=bash

# The asset notice a plugin crate may carry; staged as <id>/NOTICE.md. Read by the
# scripts that source this file.
# shellcheck disable=SC2034
NOTICE_ASSET="assets/NOTICE.md"

# Prints the notice set, one repository path per line. LICENSES/ holds regular files
# only: every entry directly in it, including names that start with a dot, is read, and a
# folder, link, or other entry fails instead of being left out. An empty LICENSES/ means
# a checkout without the license texts, so it fails too. dotglob is turned on only inside
# this function and set back to the caller's state before it returns.
notice_set_files() {
  local f found=0 bad="" saved
  saved=$(shopt -p dotglob)
  shopt -s dotglob
  echo "LICENSE"
  echo "THIRD_PARTY_LICENSES.md"
  for f in LICENSES/*; do
    [[ -e "$f" || -L "$f" ]] || continue
    if [[ -L "$f" || ! -f "$f" ]]; then
      bad="$f"
      break
    fi
    echo "$f"
    found=1
  done
  eval "$saved"
  if [[ -n "$bad" ]]; then
    echo "error: $bad is not a regular file; LICENSES/ holds files only" >&2
    return 1
  fi
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

# Whether <listing> has an entry equal to <path>. Only one leading "./" of an entry is
# dropped before comparing, so another prefix does not match. This checks names, not
# contents. The listing is taken as one string so a producer is not cut off by SIGPIPE.
listing_has() {
  local listing="$1" want="$2"
  awk -v t="$want" '{ sub(/^\.\//, "") } $0 == t { hit = 1 } END { exit !hit }' <<<"$listing"
}

# Checks with listing_has that the listing holds each file of the notice set at
# <prefix><path>.
verify_notice_listing() {
  local listing="$1" prefix="$2" label="$3" files f
  files=$(notice_set_files) || return 1
  while IFS= read -r f; do
    if ! listing_has "$listing" "$prefix$f"; then
      echo "error: $prefix$f not in $label" >&2
      return 1
    fi
  done <<<"$files"
}
