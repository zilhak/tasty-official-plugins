//! UTF-8이 아닌 실행 이름이나 인자가 패닉 없이 위치만 알리는 사용 오류(종료 코드 2)로 끝나는지
//! 플러그인 바이너리를 실행해 확인한다. stderr에 쓰지 못해도 종료 코드는 2다. 깨진 바이트열은
//! Unix의 `OsStr::from_bytes`로만 만들 수 있어 이 시험은 Unix 전용이다.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};

const MARK: &str = "NONUTF8MARK";
const TAIL: &str = "TAIL";

/// 표지 문자열 사이에 UTF-8이 아닌 바이트(`C2` 뒤에 이어지는 바이트 없음)를 둔다.
/// 진단이 인자 원문을 어떤 형태로든(원본·이스케이프·치환) 옮기면 표지가 드러난다.
fn broken() -> OsString {
    let mut bytes = MARK.as_bytes().to_vec();
    bytes.push(0xC2);
    bytes.extend_from_slice(TAIL.as_bytes());
    OsStr::from_bytes(&bytes).to_os_string()
}

fn args(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

/// 플러그인 바이너리를 Tasty 밖의 환경으로 실행한다. 표준 입력은 닫아 플러그인 모드가 기다리지 않게 한다.
fn command(argv: &[OsString]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tasty-plugin-codex"));
    cmd.args(argv)
        .env_remove("TASTY_SURFACE_ID")
        .env_remove("TASTY_SESSION_TOKEN")
        .env_remove("TASTY_PARENT_HOME")
        .env_remove("TASTY_LOCALE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null());
    cmd
}

fn run(argv: &[OsString]) -> Output {
    command(argv).output().expect("run tasty-plugin-codex")
}

/// tasty CLI의 `cli.argv.not_utf8`(en)과 같은 문장.
fn expected(position: usize) -> String {
    format!(
        "Error: argument {position} contains invalid UTF-8. The first argument after the program name is numbered 1. Nothing was executed or sent.\n"
    )
}

/// tasty CLI의 `cli.argv.program_not_utf8`(en)과 같은 문장.
const PROGRAM_NOT_UTF8: &str =
    "Error: the program name (argv[0]) contains invalid UTF-8. Nothing was executed or sent.\n";

/// 종료 코드 2, 기대 문구 한 줄, 원문·대체 문자·패닉 흔적 없음.
fn assert_rejected(out: &Output, position: usize) {
    assert_rejected_with(out, &expected(position));
}

fn assert_rejected_with(out: &Output, expected: &str) {
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(!out.stderr.contains(&0xC2), "raw byte leaked: {out:?}");
    let stderr = std::str::from_utf8(&out.stderr).expect("stderr is UTF-8");
    assert_eq!(stderr, expected, "{stderr}");
    for leaked in [MARK, TAIL, "\\x", "\u{FFFD}", "panicked", "unwrap"] {
        assert!(!stderr.contains(leaked), "{leaked}: {stderr}");
    }
}

#[test]
fn a_non_utf8_first_argument_is_reported_by_position_with_exit_code_2() {
    let out = run(&[broken()]);
    assert_rejected(&out, 1);
}

/// 세션 helper 분기보다 검사가 먼저다.
#[test]
fn a_non_utf8_session_argument_is_rejected_before_the_session_helper() {
    let mut argv = args(&["session", "--resume"]);
    argv.push(broken());
    let out = run(&argv);
    assert_rejected(&out, 3);
}

#[test]
fn the_first_of_several_non_utf8_arguments_is_reported() {
    let mut argv = args(&["terminal"]);
    argv.push(broken());
    argv.push(broken());
    let out = run(&argv);
    assert_rejected(&out, 2);
}

/// UTF-8 비 ASCII 인자는 거절하지 않고 세션 helper의 인자 해석까지 간다(모르는 인자라
/// `codex.session.bad_args` 사용 오류). 번역 파일이 바이너리 옆에 없으면 키가 그대로 나오고,
/// 있으면 영어 문구가 나온다.
#[test]
fn valid_non_ascii_arguments_still_reach_the_session_helper() {
    let out = run(&args(&["session", "--한글·"]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        stderr.contains("codex.session.bad_args")
            || stderr.contains("tasty codex session: invalid arguments"),
        "{stderr}"
    );
    assert!(!stderr.contains("invalid UTF-8"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn a_non_utf8_program_name_has_its_own_message() {
    let out = command(&args(&["session", "--한글·"]))
        .arg0(broken())
        .output()
        .expect("run tasty-plugin-codex");
    assert_rejected_with(&out, PROGRAM_NOT_UTF8);
}

/// 실행 이름과 인자가 모두 깨졌으면 실행 이름을 먼저 알린다.
#[test]
fn a_non_utf8_program_name_is_reported_before_arguments() {
    let mut argv = args(&["session"]);
    argv.push(broken());
    let out = command(&argv)
        .arg0(broken())
        .output()
        .expect("run tasty-plugin-codex");
    assert_rejected_with(&out, PROGRAM_NOT_UTF8);
}

/// 읽는 쪽을 닫은 파이프로 stderr를 준다. Rust 실행 파일은 SIGPIPE를 무시하므로 쓰기는 EPIPE
/// 오류가 되고, 그래도 종료 코드는 2다(`eprintln!`이면 패닉해 101).
#[test]
fn a_closed_stderr_pipe_still_exits_with_code_2() {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let status = command(&[broken()])
        .stdout(Stdio::null())
        .stderr(writer)
        .status()
        .expect("run tasty-plugin-codex");
    assert_eq!(status.code(), Some(2), "{status:?}");
}

/// `/dev/full`은 Linux에만 있다. 쓰기가 ENOSPC로 실패해도 종료 코드는 2다.
#[cfg(target_os = "linux")]
#[test]
fn a_full_stderr_still_exits_with_code_2() {
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    let status = command(&[broken()])
        .stdout(Stdio::null())
        .stderr(full)
        .status()
        .expect("run tasty-plugin-codex");
    assert_eq!(status.code(), Some(2), "{status:?}");
}
