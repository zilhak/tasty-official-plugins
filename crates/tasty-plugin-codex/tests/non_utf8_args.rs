//! UTF-8이 아닌 인자가 패닉 없이 위치만 알리는 사용 오류(종료 코드 2)로 끝나는지 플러그인
//! 바이너리를 실행해 확인한다. 깨진 바이트열은 Unix의 `OsStr::from_bytes`로만 만들 수 있어
//! 이 시험은 Unix 전용이다.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
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
fn run(argv: &[OsString]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tasty-plugin-codex"))
        .args(argv)
        .env_remove("TASTY_SURFACE_ID")
        .env_remove("TASTY_SESSION_TOKEN")
        .env_remove("TASTY_PARENT_HOME")
        .env_remove("TASTY_LOCALE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run tasty-plugin-codex")
}

fn expected(position: usize) -> String {
    format!(
        "Error: argument {position} is not valid UTF-8. Arguments are counted from 1 after the program name. Nothing was run or sent.\n"
    )
}

/// 종료 코드 2, 기대 문구 한 줄, 원문·대체 문자·패닉 흔적 없음.
fn assert_rejected(out: &Output, position: usize) {
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(!out.stderr.contains(&0xC2), "raw byte leaked: {out:?}");
    let stderr = std::str::from_utf8(&out.stderr).expect("stderr is UTF-8");
    assert_eq!(stderr, expected(position), "{stderr}");
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

/// UTF-8 비 ASCII 인자는 거절하지 않고 세션 helper의 인자 해석까지 간다(모르는 인자라 사용 오류).
#[test]
fn valid_non_ascii_arguments_still_reach_the_session_helper() {
    let out = run(&args(&["session", "--한글·"]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(!stderr.contains("not valid UTF-8"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.is_empty(), "{out:?}");
}
