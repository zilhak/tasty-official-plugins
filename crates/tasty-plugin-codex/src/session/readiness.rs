//! Restored terminals can start before boot publishes IPC. Wait with read-only probes;
//! never retry session-open, whose outcome could be ambiguous after it was sent.
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(25);
const RETRY: Duration = Duration::from_millis(100);

#[derive(Debug, PartialEq)]
pub(crate) enum Error {
    Cancelled,
    Failed(String),
}

/// Own and reap every probe, including a CLI that connects but never answers.
struct Probe(Child);
impl Drop for Probe {
    fn drop(&mut self) {
        match self.0.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => tracing::warn!("codex readiness probe status: {error}"),
        }
        self.stop();
    }
}

impl Probe {
    /// Stop and reap a probe that has not exited.
    fn stop(&mut self) {
        if let Err(error) = self.0.kill() {
            tracing::warn!("codex readiness probe stop: {error}");
        }
        if let Err(error) = self.0.wait() {
            tracing::warn!("codex readiness probe reap: {error}");
        }
    }
}

pub(crate) fn wait(
    mut command: impl FnMut() -> Command,
    timeout: Duration,
    mut cancelled: impl FnMut() -> bool,
) -> Result<(), Error> {
    let deadline = Instant::now() + timeout;
    let timeout_error = || {
        Error::Failed(format!(
            "Tasty IPC was not ready within {}s",
            timeout.as_secs_f64()
        ))
    };
    loop {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(timeout_error());
        }
        let mut command = command();
        command
            .args(["list", "info"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut probe = Probe(
            command
                .spawn()
                .map_err(|error| Error::Failed(format!("Tasty readiness probe: {error}")))?,
        );
        loop {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(timeout_error());
            }
            if let Some(status) = probe
                .0
                .try_wait()
                .map_err(|error| Error::Failed(error.to_string()))?
            {
                if status.success() {
                    return Ok(());
                }
                break;
            }
            std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
        drop(probe);
        std::thread::sleep(RETRY.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn a_late_host_is_probed_without_retrying_a_mutation() {
        let mut attempts = 0;
        wait(
            || {
                attempts += 1;
                let mut command = Command::new("sh");
                command.args([
                    "-c",
                    if attempts < 3 {
                        "exit 1"
                    } else {
                        "test \"$0\" = list && test \"$1\" = info"
                    },
                ]);
                command
            },
            Duration::from_secs(3),
            || false,
        )
        .unwrap();
        assert_eq!(attempts, 3);
    }
    #[test]
    fn a_missing_cli_fails_without_waiting_for_boot() {
        let result = wait(
            || Command::new("/no-such-tasty-probe"),
            Duration::from_secs(3),
            || false,
        );
        assert!(matches!(result, Err(Error::Failed(error)) if error.contains("readiness probe")));
    }
    #[test]
    fn cancellation_before_a_probe_starts_no_process() {
        let result = wait(|| panic!("must not spawn"), Duration::from_secs(1), || true);
        assert_eq!(result, Err(Error::Cancelled));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn a_hung_probe_is_reaped_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("pid");
        let result = wait(
            || {
                let mut command = Command::new("sh");
                command.args(["-c", "echo $$ > \"$PROBE_PID_FILE\"; exec sleep 60"]);
                command.env("PROBE_PID_FILE", &marker);
                command
            },
            Duration::from_millis(300),
            || false,
        );
        assert!(matches!(result, Err(Error::Failed(_))));
        let pid = std::fs::read_to_string(marker).unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{}", pid.trim())).exists());
    }
}
