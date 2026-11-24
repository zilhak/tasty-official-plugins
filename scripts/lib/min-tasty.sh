# The reading of min_tasty_version that check 8 of scripts/release-promote.sh uses.
# Sourced by release-promote.sh.
# shellcheck shell=bash

# The top-level min_tasty_version of tasty-plugin.toml $1, read as check 8 (b) in the
# header of scripts/release-promote.sh describes: "value<TAB>X", "none", or "bad<TAB>reason"
# for the first unreadable line.
# The key and bracket reading is the one of toml_lines in scripts/check-plugin-version-bump.sh.
min_tasty_of() {
  awk '
    # The key of a key/value line: the text before the first = outside strings.
    function key_of(s,    i, c, q) {
      q = ""
      for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (q != "") { if (c == q) q = ""; continue }
        if (c == "\"" || c == "\047") { q = c; continue }
        if (c == "=") return substr(s, 1, i - 1)
      }
      return s
    }
    # Drops a comment and counts the brackets outside strings; the count goes to depth.
    function scan(s,    i, n, c, q, out) {
      out = ""; q = ""; n = length(s)
      for (i = 1; i <= n; i++) {
        c = substr(s, i, 1)
        if (q != "") {
          out = out c
          if (q == "\"" && c == "\\" && i < n) { i++; out = out substr(s, i, 1); continue }
          if (c == q) q = ""
          continue
        }
        if (c == "#") break
        if ((c == "\"" || c == "\047") && substr(s, i, 3) == c c c) multi = 1
        if (c == "\"" || c == "\047") q = c
        else if (c == "[" || c == "{") depth++
        else if (c == "]" || c == "}") depth--
        out = out c
      }
      return out
    }
    function unreadable(why) { if (bad == "") bad = "line " NR ", " why }
    {
      multi = 0
      start = depth
      line = scan($0)
      if (multi) unreadable("a multi-line string")
      if (depth < 0) { unreadable("a bracket that closes nothing"); depth = 0 }
      if (index($0, "min_tasty_version")) {
        hits++
        if (hits > 1) {
          unreadable("a second line that names min_tasty_version")
        } else if (tables || start != 0 || $0 !~ /^[[:space:]]*min_tasty_version[[:space:]]*=[[:space:]]*"[^"]*"[[:space:]]*$/) {
          unreadable("min_tasty_version not on a line min_tasty_version = \"X.Y.Z\" before the first table")
        } else {
          value = $0; sub(/^[^"]*"/, "", value); sub(/"[[:space:]]*$/, "", value)
        }
      }
      sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line)
      # A line that starts inside open brackets is part of the value before it.
      if (line == "" || start != 0) next
      if (line ~ /^\[/ && line ~ /\]$/ && depth == 0) {
        tables++
        name = line; gsub(/^\[+[[:space:]]*|[[:space:]]*\]+$/, "", name)
        if (index(name, ".")) name = substr(name, 1, index(name, ".") - 1)
        sub(/[[:space:]]+$/, "", name)
        if (index(name, "\"") || index(name, "\047") || index(name, "\\")) {
          unreadable("a quote or backslash in the first part of a table name")
        } else if (name == "min_tasty_version") {
          unreadable("a table named min_tasty_version")
        }
        next
      }
      if (!tables) {
        key = key_of(line); sub(/[[:space:]]+$/, "", key)
        if (key !~ /^[A-Za-z0-9_-]+$/) unreadable("a key before the first table that is not a bare key")
      }
    }
    END {
      if (depth != 0) unreadable("brackets still open at the end of the file")
      if (bad != "") print "bad\t" bad
      else if (hits) print "value\t" value
      else print "none"
    }' "$1"
}
