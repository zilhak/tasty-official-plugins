//! A PATH holding symlinks to only the named executables, to reproduce a missing tool.
//! Every target that includes this module is Unix only.
//! GateRun shows the run's stdout and stderr when an exit code assertion fails.
//!
//! Ported from tasty tests/gate_env/mod.rs.

use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::TempDir;

/// Names that are not found are skipped. A typo in a tool name has the same result, so callers
/// also check the error message.
pub fn only(names: &[&str]) -> TempDir {
    let d = tempfile::tempdir().expect("temp dir");
    let path = std::env::var("PATH").unwrap_or_default();
    for name in names {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let cand = Path::new(dir).join(name);
            if cand.is_file() {
                symlink(&cand, d.path().join(name))
                    .unwrap_or_else(|e| panic!("symlink {name}: {e}"));
                break;
            }
        }
    }
    d
}

/// Exit code with stdout and stderr. Compares with i32, and its Debug output includes the log.
pub struct GateRun {
    pub code: i32,
    pub output: String,
}

impl GateRun {
    pub fn from_output(out: &std::process::Output) -> Self {
        Self {
            // A signal exit gets a value that does not overlap with 0, 1 and 2.
            code: out.status.code().unwrap_or(-1),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }
}

impl PartialEq<i32> for GateRun {
    fn eq(&self, other: &i32) -> bool {
        self.code == *other
    }
}

impl std::fmt::Debug for GateRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rc {}\n--- gate output (stdout+stderr) ---\n{}--- end ---",
            self.code, self.output
        )
    }
}
