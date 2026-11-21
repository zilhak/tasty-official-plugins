# The file modes of a plugin archive: folders and the entry command are 755, every other
# file is 644, and nothing else (a link or any other kind of entry) is allowed. Source it
# with the repository root as the working directory.
# shellcheck shell=bash

# Prints the command of the [entry] table of the manifest <file>. Unlike manifest_string,
# a command key in another table is not read.
entry_command() {
  awk '
    /^[[:space:]]*\[/ {
      t = $0
      sub(/#.*/, "", t)
      gsub(/[[:space:]]/, "", t)
      in_entry = (t == "[entry]")
      next
    }
    in_entry && /^[[:space:]]*command[[:space:]]*=/ {
      v = $0
      sub(/^[^=]*=[[:space:]]*"/, "", v)
      sub(/".*$/, "", v)
      print v
      found = 1
      exit
    }
    END { exit !found }
  ' "$1"
}

# Gives the staged plugin folder <dir> the archive modes: folders and <dir>/<exe> 755,
# every other file 644. The symbolic forms also clear the setuid, setgid, and sticky bits,
# which a numeric 755 keeps on a folder with GNU chmod. On Windows chmod cannot change the
# modes that Git Bash reports; pack checks them in the archive.
normalize_stage_modes() {
  local dir="$1" exe="$2"
  [[ -f "$dir/$exe" ]] || {
    echo "error: $dir/$exe is not staged" >&2
    return 1
  }
  find "$dir" -type d -exec chmod a-st,u=rwx,go=rx {} + || return 1
  find "$dir" -type f -exec chmod a-st,u=rw,go=r {} + || return 1
  chmod a-st,u=rwx,go=rx "$dir/$exe"
}

# Checks the modes of the archive <archive> whose entry names, read with LC_ALL=C tar -tzf,
# are <names>. <id>/<exe> is the entry command. The verbose listing is paired with the
# names line by line, as tasty's official-plugins check does: a line must start with - or
# d, end with " " and its name, and carry no link mark after the name (bsdtar shows a hard
# link whose header holds a file type as -rw-r--r-- ... <name> link to <target>). Then its
# first ten characters must be the mode the rule gives. Every problem is printed as an
# error line with <label>; returns 1 if there is one.
verify_archive_modes() {
  local archive="$1" names="$2" id="$3" exe="$4" label="$5" long rc
  if long=$(LC_ALL=C tar -tvzf "$archive"); then :; else
    rc=$?
    echo "error: $label: tar -tvzf failed (exit $rc)" >&2
    return 1
  fi
  NAMES="$names" WANT_EXE="$id/$exe" LABEL="$label" awk '
    BEGIN {
      n = (ENVIRON["NAMES"] == "") ? 0 : split(ENVIRON["NAMES"], name, "\n")
      exe = ENVIRON["WANT_EXE"]
      label = ENVIRON["LABEL"]
    }
    { line[++m] = $0 }
    END {
      if (n == 0 || n != m) {
        printf "error: %s: cannot read the archive listing (%d names, %d long lines)\n", label, n, m > "/dev/stderr"
        exit 1
      }
      bad = 0
      seen = 0
      for (i = 1; i <= n; i++) {
        N = name[i]
        L = line[i]
        if (N == exe) seen = 1
        kind = substr(L, 1, 1)
        if (kind != "-" && kind != "d") {
          printf "error: %s: %s is not a regular file or folder (%s)\n", label, N, L > "/dev/stderr"
          bad = 1
          continue
        }
        tail = " " N
        if (length(L) <= length(tail) || substr(L, length(L) - length(tail) + 1) != tail ||
            index(L, tail " link to ") || index(L, tail " -> ")) {
          printf "error: %s: the listing line of %s does not end with its name (link) (%s)\n", label, N, L > "/dev/stderr"
          bad = 1
          continue
        }
        if (kind == "d") want = "drwxr-xr-x"
        else if (N == exe) want = "-rwxr-xr-x"
        else want = "-rw-r--r--"
        got = substr(L, 1, 10)
        if (got != want) {
          printf "error: %s: %s has mode %s; expected %s\n", label, N, got, want > "/dev/stderr"
          bad = 1
        }
      }
      if (!seen) {
        printf "error: %s: %s (the entry command) is not in the archive\n", label, exe > "/dev/stderr"
        bad = 1
      }
      exit bad
    }
  ' <<<"$long"
}
