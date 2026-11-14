# Helpers for the scripts that build plugins and copy them out of cargo's target
# directory. Sourced by package-plugin.sh and dev-bundle.sh, which define die().
# shellcheck shell=bash

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
# Each message is one line of compact JSON. The executable path is decoded from
# the JSON string escapes cargo (serde_json) writes: \", \\, \/, \b, \f, \n, \r,
# \t, and \u00XX for the other control characters. A Unix path can hold control
# characters, for example from CARGO_TARGET_DIR. Characters from U+007F up are
# written as they are. A \u escape outside U+0001 to U+007F, such as U+0000 or a
# surrogate, fails, as does any other escape. The target name is read from the
# "target" object, which holds no nested object.
cargo_executables() {
  tr -d '\r' | awk '
    # Value of a 4-digit hex string, or -1 if it is not hex.
    function hex4(h,    v, i, d) {
      if (length(h) != 4) return -1
      v = 0
      for (i = 1; i <= 4; i++) {
        d = index("0123456789abcdef", tolower(substr(h, i, 1))) - 1
        if (d < 0) return -1
        v = v * 16 + d
      }
      return v
    }
    function unescape(s,    out, i, c, v) {
      out = ""
      for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "\\") {
          i++
          c = substr(s, i, 1)
          if (c == "b") c = "\b"
          else if (c == "f") c = "\f"
          else if (c == "n") c = "\n"
          else if (c == "r") c = "\r"
          else if (c == "t") c = "\t"
          else if (c == "u") {
            v = hex4(substr(s, i + 1, 4))
            if (v < 1 || v > 127) { bad = 1; return "" }
            c = sprintf("%c", v)
            i += 4
          } else if (c != "\\" && c != "\"" && c != "/") { bad = 1; return "" }
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

# Reads a top-level string key from a tasty-plugin.toml.
manifest_string() {
  local key="$1" file="$2" value
  value=$(grep -m1 -E "^${key}[[:space:]]*=" "$file" | sed 's/.*"\([^"]*\)".*/\1/' || true)
  [[ -n "$value" ]] || die "cannot read '$key' from $file"
  printf '%s\n' "$value"
}
