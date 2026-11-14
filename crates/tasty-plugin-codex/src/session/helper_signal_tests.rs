//! 터미널 신호를 받은 helper가 소유한 서버·TUI를 모두 정리하는지 별도 프로세스로 확인한다.
//!
//! 신호 처리기는 프로세스 전역이므로 시험 실행 파일을 다시 띄워 [`lifecycle_signal_child`]만
//! helper 역할로 실행한다. 그 프로세스를 자기 프로세스 그룹의 리더로 띄워 터미널의 전경 그룹을
//! 흉내 낸다. 서버는 helper가 따로 만든 그룹에 있으므로 그룹에 보낸 신호는 helper와 TUI만 받는다.
//! 남은 프로세스는 이 시험이 환경변수로 넣은 난수 표지로만 찾고 그 PID만 끝낸다.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::*;

const DIR_ENV: &str = "TASTY_TEST_HELPER_SIGNAL_DIR";
const MARK_ENV: &str = "TASTY_TEST_HELPER_SIGNAL_MARK";

/// npm `codex`처럼 실제 서버를 자식으로 띄우고 신호를 한 번만 전달하는 래퍼.
const WRAPPER: &str = r#"trap 'kill -TERM $c' TERM; trap 'kill -INT $c' INT; trap 'kill -HUP $c' HUP
sh "$1/inner.sh" "$1" & c=$!
echo $$ > "$1/wrapper.pid"
while kill -0 $c 2>/dev/null; do wait $c; done
"#;

/// 첫 종료 요청에 바로 끝나지 않는 실제 서버. 그룹 SIGKILL로만 끝난다.
const INNER: &str = r#"trap '' TERM INT HUP
echo $$ > "$1/server.tmp" && mv "$1/server.tmp" "$1/server.pid"
while :; do sleep 0.05; done
"#;

/// Ctrl+C·hangup에 끝나고, 전달받은 종료 요청을 기록하는 TUI.
const TUI: &str = r#"trap 'exit 0' INT HUP
trap 'echo term > "$1/tui.term"; exit 0' TERM
echo $$ > "$1/tui.tmp" && mv "$1/tui.tmp" "$1/tui.pid"
while :; do sleep 0.05; done
"#;

/// 시험이 띄운 helper 역할 프로세스. 직접 실행하면 아무것도 하지 않는다.
#[test]
#[ignore = "helper 신호 시험이 별도 프로세스로 실행한다"]
fn lifecycle_signal_child() {
    let Some(dir) = std::env::var_os(DIR_ENV).map(PathBuf::from) else {
        return;
    };
    tasty_reaper::defer_termination_signals().unwrap();
    let scenario = std::fs::read_to_string(dir.join("scenario")).unwrap();
    let state = dir.join("state");
    let listen = format!("unix://{}/s.sock", dir.display());
    let (server, probe): (Command, ReadyProbe<'_>) = match scenario.as_str() {
        "real" => {
            let opened = Opened {
                generation: "0123456789abcdef".into(),
                state_dir: state.clone(),
                listen: listen.clone(),
                token_file: None,
            };
            let mut cmd = server_command(&opened);
            cmd.env(
                "CODEX_HOME",
                std::env::var_os("TASTY_CODEX_REAL_HOME").unwrap(),
            );
            (cmd, unix_ready(&listen))
        }
        _ => {
            let mut cmd = Command::new("sh");
            cmd.arg(dir.join("wrapper.sh")).arg(&dir);
            let pid = dir.join("server.pid");
            let never = scenario == "never-ready";
            let probe: ReadyProbe<'_> =
                Box::new(move |_| (!never && pid.exists()).then(|| "unix:///owned".into()));
            (cmd, probe)
        }
    };
    let tui_dir = dir.clone();
    let result = run_lifecycle(&state, server, probe, Duration::from_secs(30), move |_| {
        let mut tui = Command::new("sh");
        tui.arg(tui_dir.join("tui.sh")).arg(&tui_dir);
        tui.stdin(Stdio::null());
        tui
    });
    std::fs::write(dir.join("done"), format!("{result:?}")).unwrap();
}

struct Run {
    dir: tempfile::TempDir,
    helper: Child,
    mark: String,
}

impl Run {
    fn start(scenario: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("wrapper.sh", WRAPPER),
            ("inner.sh", INNER),
            ("tui.sh", TUI),
            ("scenario", scenario),
        ] {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        std::fs::create_dir(dir.path().join("state")).unwrap();
        let mark = crate::ids::random_hex(16).unwrap();
        let log = std::fs::File::create(dir.path().join("helper.log")).unwrap();
        let helper = std::os::unix::process::CommandExt::process_group(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "session::helper::signal_tests::lifecycle_signal_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env(DIR_ENV, dir.path())
                .env(MARK_ENV, &mark)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log),
            0,
        )
        .spawn()
        .unwrap();
        Self { dir, helper, mark }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn wait_for(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.path(name).exists() {
            assert!(
                Instant::now() < deadline,
                "{name} 이 생기지 않았다: {}",
                std::fs::read_to_string(self.path("helper.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 터미널의 전경 그룹(helper와 TUI)에 보낸다.
    fn to_group(&self, signal: &str) {
        send(signal, &format!("-{}", self.helper.id()));
    }

    fn to_helper(&self, signal: &str) {
        send(signal, &self.helper.id().to_string());
    }

    /// 정리하는 5초 동안 Ctrl+C와 hangup·종료 요청을 계속 보낸다.
    fn signal_during_grace(&self) {
        for i in 0..20 {
            self.to_group("INT");
            if i % 5 == 4 {
                self.to_group(if i % 10 == 4 { "HUP" } else { "TERM" });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// helper가 끝나기를 기다린 뒤 표지를 가진 프로세스가 남았는지 본다. 남은 프로세스는 끝내고
    /// 실패 보고에 쓴다.
    fn finish(mut self) -> (Option<ExitStatus>, Option<String>, Vec<String>) {
        self.finish_inner()
    }

    fn finish_inner(&mut self) -> (Option<ExitStatus>, Option<String>, Vec<String>) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self.helper.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if status.is_none() {
            send("KILL", &self.helper.id().to_string());
            self.helper.wait().unwrap();
        }
        // 그룹 SIGKILL 직후 사라지는 중인 프로세스를 기다린다.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut left = marked(&self.mark);
        while !left.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            left = marked(&self.mark);
        }
        let report = left
            .iter()
            .map(|pid| {
                let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                send("KILL", pid);
                format!("{pid} {}", String::from_utf8_lossy(&cmd).replace('\0', " "))
            })
            .collect();
        let done = std::fs::read_to_string(self.path("done")).ok();
        (status, done, report)
    }
}

/// 중간 단언이 실패해도 이 시험이 띄운 프로세스를 남기지 않는다.
impl Drop for Run {
    fn drop(&mut self) {
        if !marked(&self.mark).is_empty() {
            self.finish_inner();
        }
    }
}

fn send(signal: &str, target: &str) {
    let status = Command::new("kill")
        .args([&format!("-{signal}"), "--", target])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    // 이미 끝난 대상이면 실패한다. 결과는 finish가 프로세스 표로 판정하므로 기록만 한다.
    if !status.success() {
        tracing::debug!("kill -{signal} {target}: {status}");
    }
}

/// 환경에 이 시험의 표지를 가진 프로세스(helper 자신 포함)의 PID.
fn marked(mark: &str) -> Vec<String> {
    let needle = format!("{MARK_ENV}={mark}");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|pid| pid.bytes().all(|b| b.is_ascii_digit()))
        .filter(|pid| {
            std::fs::read(Path::new("/proc").join(pid).join("environ"))
                .map(|env| env.split(|b| *b == 0).any(|kv| kv == needle.as_bytes()))
                .unwrap_or(false)
        })
        .collect()
}

fn assert_all_gone(scenario: &str, finished: (Option<ExitStatus>, Option<String>, Vec<String>)) {
    let (status, done, left) = finished;
    assert!(left.is_empty(), "{scenario}: 남은 프로세스 {left:?}");
    assert!(
        status.is_some_and(|s| s.success()),
        "{scenario}: helper가 정리를 마치지 못했다 {status:?}"
    );
    assert!(
        done.is_some(),
        "{scenario}: run_lifecycle이 돌아오지 않았다"
    );
}

/// reboot의 남은 Ctrl+C: TUI가 끝난 뒤 서버를 정리하는 동안 Ctrl+C와 hangup이 계속 와도 helper가
/// 끝까지 정리한다.
#[test]
fn ctrl_c_during_cleanup_does_not_orphan_the_server() {
    let run = Run::start("fake");
    run.wait_for("tui.pid");
    run.to_group("INT");
    run.signal_during_grace();
    let finished = run.finish();
    assert_all_gone("ctrl-c", finished);
}

/// 터미널 닫기: 전경 그룹이 hangup을 받은 뒤에도 서버까지 정리한다.
#[test]
fn a_terminal_hangup_ends_the_tui_and_the_server() {
    let run = Run::start("fake");
    run.wait_for("tui.pid");
    run.to_group("HUP");
    run.signal_during_grace();
    let finished = run.finish();
    assert_all_gone("hangup", finished);
}

/// helper만 종료 요청을 받으면 TUI에 전달하고 서버까지 정리한다.
#[test]
fn a_termination_request_is_forwarded_to_the_tui() {
    let run = Run::start("fake");
    run.wait_for("tui.pid");
    run.to_helper("TERM");
    // TUI에는 직접 보내지 않았으므로 이 표지는 helper가 전달했다는 증거다.
    run.wait_for("tui.term");
    run.signal_during_grace();
    let finished = run.finish();
    assert_all_gone("terminate", finished);
}

/// 서버 준비 전 Ctrl+C는 시작을 취소하고 TUI를 띄우지 않는다.
#[test]
fn ctrl_c_before_ready_cancels_without_a_tui() {
    let run = Run::start("never-ready");
    run.wait_for("server.pid");
    run.to_group("INT");
    run.signal_during_grace();
    let tui = run.path("tui.pid");
    let finished = run.finish();
    assert!(!tui.exists(), "준비 전에 취소했는데 TUI가 시작됐다");
    assert_eq!(finished.1.as_deref(), Some("Err(Interrupted)"));
    assert_all_gone("before-ready", finished);
}

/// 실제 npm `codex app-server`로 같은 정리를 확인한다. 모델 호출과 로그인은 필요 없다.
#[test]
#[ignore = "실제 codex CLI가 필요하다. TASTY_CODEX_REAL_HOME에 격리된 CODEX_HOME을 준다"]
fn the_real_server_tree_is_gone_after_ctrl_c_and_hangup_during_cleanup() {
    assert!(std::env::var_os("TASTY_CODEX_REAL_HOME").is_some());
    let run = Run::start("real");
    run.wait_for("tui.pid");
    let server = marked(&run.mark).len();
    // helper, TUI(sh와 sleep)와 서버 래퍼·실제 서버가 모두 표지를 가진다.
    assert!(server >= 4, "실제 서버 트리가 보이지 않는다: {server}");
    run.to_group("INT");
    run.signal_during_grace();
    let finished = run.finish();
    assert_all_gone("real", finished);
}

/// 실제 npm `codex app-server proxy`(node 래퍼와 실제 proxy)를 응답하지 않는 소유 소켓에 붙인다.
/// 기한에 연결이 끝나고 래퍼와 실제 proxy가 모두 사라져야 한다. 사용자 daemon에는 붙지 않는다.
#[test]
#[ignore = "실제 codex CLI가 필요하다. TASTY_CODEX_REAL_HOME에 격리된 CODEX_HOME을 준다"]
fn the_real_proxy_tree_ends_at_the_deadline() {
    use crate::rpc::tests_support::{connect_proxy_command, proxy_args};
    let home = std::env::var_os("TASTY_CODEX_REAL_HOME").expect("TASTY_CODEX_REAL_HOME");
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("silent.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    // 연결을 받기만 하고 응답하지 않는다. 시험이 끝날 때까지 연결을 쥔다.
    let holder = std::thread::spawn(move || listener.accept().map(|(s, _)| s));
    let mark = crate::ids::random_hex(16).unwrap();
    let mut cmd = crate::rpc::codex_command().unwrap();
    cmd.args(proxy_args()).arg("--sock").arg(&sock);
    cmd.env("CODEX_HOME", &home).env(MARK_ENV, &mark);
    let started = Instant::now();
    let err = connect_proxy_command(cmd, Duration::from_secs(3));
    let elapsed = started.elapsed();
    assert!(err.is_some(), "응답 없는 소켓에 연결되면 안 된다");
    assert!(
        elapsed < Duration::from_secs(6),
        "3초 기한의 연결이 {elapsed:?} 걸렸다"
    );
    let connected = holder.join().unwrap();
    assert!(connected.is_ok(), "실제 proxy가 소켓에 연결하지 않았다");
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut left = marked(&mark);
    while !left.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        left = marked(&mark);
    }
    for pid in &left {
        send("KILL", pid);
    }
    assert!(left.is_empty(), "proxy 프로세스가 남았다 {left:?}");
}
