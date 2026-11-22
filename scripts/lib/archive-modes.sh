# The file modes and owners of a plugin archive: folders and the entry command are 755,
# every other file is 644, and nothing else (a link or any other kind of entry) is allowed.
# The archive is POSIX USTAR, and every entry has uid 0, gid 0, and no user or group name.
# Source it with the repository root as the working directory.
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
# first ten characters must be the mode the rule gives. The entry command counts as
# present only as a regular file with mode 755 at <id>/<exe>; a folder there is an error.
# Every problem is printed as an error line with <label>; returns 1 if there is one.
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
        # A folder whose name has no trailing slash can stand at the path of the entry
        # command; only a regular file with mode 755 there counts as the entry command.
        if (N == exe && kind == "d") {
          printf "error: %s: %s is a folder, not the entry command (%s)\n", label, N, L > "/dev/stderr"
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
        } else if (N == exe) {
          seen = 1
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

# Checks the headers of the archive <archive>, whose entry names, read with LC_ALL=C tar
# -tzf, are <names>. A listing shows a name only when one is stored and a number
# otherwise, so a name "0" can hide another id; the headers are read instead, with gzip
# and od. Every header must be POSIX USTAR (magic "ustar\0", version "00") with a right
# checksum and type 0 (file) or 5 (folder, size 0); a PAX or GNU header, a file whose
# stored path ends with "/", and an AppleDouble entry (a last path part that starts with
# "._") fail. Every entry needs uid 0, gid 0, and user and group names of NUL bytes only.
# The numeric fields take octal digits between leading blanks and trailing blanks or NULs.
# The archive must end with two zero blocks followed by zero bytes only, and hold as many
# headers as <names> has lines; names, kinds, and links are verify_archive_modes's part.
# Every problem is printed as an error line with <label>, never with a stored name, as
# release logs are public. Returns 1 if there is one.
verify_archive_owners() (
  archive="$1" names="$2" label="$3"
  tmp=$(mktemp -d) || {
    echo "error: $label: cannot make a temporary folder" >&2
    exit 1
  }
  # Leave the folder first: Windows cannot remove the working directory.
  trap 'cd / && rm -rf "$tmp"' EXIT
  if gzip -dc <"$archive" >"$tmp/a.tar"; then :; else
    rc=$?
    echo "error: $label: gzip -dc failed (exit $rc)" >&2
    exit 1
  fi
  size=$(wc -c <"$tmp/a.tar") || exit 1
  # od reads a.tar by a plain relative name, so no path is quoted in the awk commands.
  cd "$tmp" || exit 1
  NAMES="$names" LABEL="$label" TARFILE=a.tar SIZE="$size" awk '
    # Reads up to 512 bytes at <off> into b[0..511] and returns how many there were.
    function block(off,   cmd, line, nf, f, i, k) {
      for (i = 0; i < 512; i++) b[i] = 0
      if (off >= total) return 0
      k = 0
      cmd = "od -An -v -tu1 -j " off " -N 512 " file
      while ((cmd | getline line) > 0) {
        nf = split(line, f, " ")
        for (i = 1; i <= nf; i++) {
          if (k < 512) b[k] = f[i] + 0
          k++
        }
      }
      close(cmd)
      return k
    }
    function zero(   i) {
      for (i = 0; i < 512; i++) if (b[i] != 0) return 0
      return 1
    }
    # The octal number in b[at, at + len), or -1 when the field is not one.
    function num(at, len,   i, end, v, d) {
      end = at + len
      if (b[at] >= 128) return -1
      i = at
      while (i < end && b[i] == 32) i++
      v = 0
      d = 0
      while (i < end && b[i] >= 48 && b[i] <= 55) {
        v = v * 8 + b[i] - 48
        d++
        i++
      }
      if (d == 0) return -1
      for (; i < end; i++) if (b[i] != 32 && b[i] != 0) return -1
      return v
    }
    function fail(msg) {
      printf "error: %s: %s\n", label, msg > "/dev/stderr"
      bad = 1
    }
    function bytes(from, to,   i, c) {
      c = 0
      for (i = from; i < to; i++) if (b[i] != 0) c++
      return c
    }
    function short(off, k) {
      fail(sprintf("the block at byte %d has %d bytes, not 512", off, k))
    }
    BEGIN {
      label = ENVIRON["LABEL"]
      file = ENVIRON["TARFILE"]
      total = ENVIRON["SIZE"] + 0
      want = (ENVIRON["NAMES"] == "") ? 0 : split(ENVIRON["NAMES"], name, "\n")
      bad = 0
      n = 0
      off = 0
      ended = 0
      while (1) {
        k = block(off)
        if (k == 0) { fail("the archive ends without two zero blocks"); break }
        if (k < 512) { short(off, k); break }
        if (zero()) {
          k = block(off + 512)
          if (k == 0) { fail("the archive ends without two zero blocks"); break }
          if (k < 512) { short(off + 512, k); break }
          if (!zero()) {
            fail(sprintf("a header follows a single zero block at byte %d", off))
            break
          }
          ended = 1
          rest = off + 1024
          break
        }
        n++
        sum = 0
        for (i = 0; i < 512; i++) sum += (i >= 148 && i < 156) ? 32 : b[i]
        if (num(148, 8) != sum) { fail(sprintf("entry %d has a wrong header checksum", n)); break }
        if (b[257] != 117 || b[258] != 115 || b[259] != 116 || b[260] != 97 ||
            b[261] != 114 || b[262] != 0) {
          fail(sprintf("entry %d is not a USTAR header (magic)", n))
          break
        }
        if (b[263] != 48 || b[264] != 48) {
          fail(sprintf("entry %d is not a USTAR header (version)", n))
          break
        }
        size = num(124, 12)
        if (size < 0) { fail(sprintf("entry %d has an unreadable size field", n)); break }
        t = b[156]
        if (t != 48 && t != 53)
          fail(sprintf("entry %d has type flag byte %d; only 0 (file) and 5 (folder) are allowed", n, t))
        if (t == 53 && size != 0) fail(sprintf("entry %d is a folder with size %d", n, size))
        # The stored path: prefix "/" name when prefix is set, else name.
        len = 0
        if (b[345] != 0) {
          for (i = 345; i < 500 && b[i] != 0; i++) p[len++] = b[i]
          p[len++] = 47
        }
        for (i = 0; i < 100 && b[i] != 0; i++) p[len++] = b[i]
        if (t == 48 && len > 0 && p[len - 1] == 47)
          fail(sprintf("entry %d is a file whose stored path ends with a slash", n))
        j = len - 1
        while (j >= 0 && p[j] == 47) j--
        e = j
        while (j >= 0 && p[j] != 47) j--
        if (e - j >= 2 && p[j + 1] == 46 && p[j + 2] == 95)
          fail(sprintf("entry %d is an AppleDouble entry (its last path part starts with ._)", n))
        uid = num(108, 8)
        gid = num(116, 8)
        un = bytes(265, 297)
        gn = bytes(297, 329)
        if (uid != 0 || gid != 0 || un || gn)
          fail(sprintf("entry %d has %s, %s, %s, %s; expected uid 0, gid 0, no names", n,
            (uid < 0) ? "an unreadable uid field" : "uid " uid,
            (gid < 0) ? "an unreadable gid field" : "gid " gid,
            un ? "a user name (" un " bytes)" : "no user name",
            gn ? "a group name (" gn " bytes)" : "no group name"))
        off += 512 + int((size + 511) / 512) * 512
      }
      if (ended) {
        if (rest < total) {
          cmd = "od -An -v -tu1 -j " rest " " file
          nz = 0
          while ((cmd | getline line) > 0) {
            nf = split(line, f, " ")
            for (i = 1; i <= nf; i++) if (f[i] + 0 != 0) nz++
          }
          close(cmd)
          if (nz) fail("the archive has nonzero bytes after its two zero blocks")
        }
        if (n != want)
          fail(sprintf("the archive has %d headers but tar -tzf listed %d names", n, want))
      }
      exit bad
    }
  ' </dev/null
)
