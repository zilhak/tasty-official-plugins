//! A fake gh for the release tests. It answers each call from a table of exact argument
//! lists, copies files for `release download`, and records `release edit` with the notes
//! file it was given. A call the table does not hold prints "fake gh: no reply" and fails,
//! so a test also pins which reads a script makes. An entry can answer only the n-th call
//! with its arguments, and only before or after the first `release edit`.
//! The script runs under bash 3.2, as the macOS CI runner has it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = r#"#!/usr/bin/env bash
d="$FAKE_GH_DIR"
printf '%s\n' "$*" >>"$d/calls.log"
if [[ "$1 ${2:-}" == "release download" ]]; then
  shift 3
  out="" pats=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dir) out=$2 ;;
      --pattern) pats+=("$2") ;;
      *) echo "fake gh: unknown download option $1" >&2; exit 97 ;;
    esac
    shift 2
  done
  for f in "$d/assets"/*; do
    n=$(basename "$f")
    for p in "${pats[@]}"; do
      # shellcheck disable=SC2053
      if [[ "$n" == $p ]]; then cp "$f" "$out/"; fi
    done
  done
  exit 0
fi
if [[ "$1 ${2:-}" == "release edit" ]]; then
  prev=""
  for a in "$@"; do
    [[ "$prev" == --notes-file ]] && cp "$a" "$d/edited-notes.md"
    prev=$a
  done
  touch "$d/edited"
  exit 0
fi
phase=before
[[ -e "$d/edited" ]] && phase=after
mkdir -p "$d/counts"
key=$(printf '%s' "$*" | cksum | tr ' ' '_')
n=$(($(cat "$d/counts/$key" 2>/dev/null || echo 0) + 1))
echo "$n" >"$d/counts/$key"
for r in "$d"/replies/*; do
  [[ "$(cat "$r/args")" == "$*" ]] || continue
  p=$(cat "$r/phase")
  [[ "$p" == any || "$p" == "$phase" ]] || continue
  nth=$(cat "$r/nth")
  [[ "$nth" == 0 || "$nth" == "$n" ]] || continue
  cat "$r/out"
  cat "$r/err" >&2
  exit "$(cat "$r/code")"
done
echo "fake gh: no reply for: $*" >&2
exit 97
"#;

/// One answer of the fake gh.
#[derive(Clone)]
pub struct Reply {
    pub out: String,
    pub err: String,
    pub code: i32,
}

pub fn ok(out: impl Into<String>) -> Reply {
    Reply {
        out: out.into(),
        err: String::new(),
        code: 0,
    }
}

pub fn not_found() -> Reply {
    Reply {
        out: String::new(),
        err: "gh: Not Found (HTTP 404)\n".into(),
        code: 1,
    }
}

pub fn server_error() -> Reply {
    Reply {
        out: String::new(),
        err: "gh: HTTP 502: Bad Gateway\n".into(),
        code: 1,
    }
}

/// A table row: the reply to a call with exactly `args`, in `phase` ("before" or "after"
/// the first release edit, or "any"), for the `nth` such call (0: every call). The first
/// row that matches answers.
pub struct Entry {
    pub args: String,
    pub phase: &'static str,
    pub nth: u32,
    pub reply: Reply,
}

pub fn entry(args: impl Into<String>, reply: Reply) -> Entry {
    Entry {
        args: args.into(),
        phase: "any",
        nth: 0,
        reply,
    }
}

pub struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    /// `assets` are the files `release download` can copy.
    pub fn new(entries: &[Entry], assets: &BTreeMap<String, Vec<u8>>) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let d = dir.path();
        super::write_executable(&d.join("bin/gh"), SCRIPT).expect("write fake gh");
        std::fs::create_dir_all(d.join("assets")).expect("assets");
        for (name, data) in assets {
            std::fs::write(d.join("assets").join(name), data).expect("asset");
        }
        for (i, e) in entries.iter().enumerate() {
            // Zero-padded so the shell glob keeps the table order.
            let r = d.join("replies").join(format!("{i:04}"));
            std::fs::create_dir_all(&r).expect("reply dir");
            std::fs::write(r.join("args"), &e.args).expect("args");
            std::fs::write(r.join("phase"), e.phase).expect("phase");
            std::fs::write(r.join("nth"), e.nth.to_string()).expect("nth");
            std::fs::write(r.join("out"), &e.reply.out).expect("out");
            std::fs::write(r.join("err"), &e.reply.err).expect("err");
            std::fs::write(r.join("code"), e.reply.code.to_string()).expect("code");
        }
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    /// Puts the fake first on PATH. The real gh, if it were ever reached, has no account
    /// to act with.
    pub fn apply<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        let path = format!(
            "{}:{}",
            self.bin().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env("PATH", path)
            .env("FAKE_GH_DIR", self.dir.path())
            .env("GH_TOKEN", "fake")
            .env("GH_CONFIG_DIR", self.dir.path().join("gh-config"))
            .env_remove("GITHUB_ACTIONS")
    }

    pub fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
    }

    pub fn edits(&self) -> Vec<String> {
        self.calls()
            .lines()
            .filter(|l| l.starts_with("release edit"))
            .map(String::from)
            .collect()
    }

    pub fn edited_notes(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join("edited-notes.md")).ok()
    }
}
