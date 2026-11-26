//! 실제 플러그인 바이너리의 `terminal`·`session` 진입이 같은 실행에서 전용 서버와 TUI에 넘긴
//! `HERDR_ENV`를 확인한다. PATH 앞의 가짜 `tasty`가 준비 조회와 세대 발급에 답하고, 가짜 `codex`가
//! 서버와 TUI로 실행되며 받은 값을 기록한다. 서버 준비 조회가 연결할 unix 소켓은 시험이 연다. 값
//! 조합 전체는 helper의 `only_the_tui_gets_herdr_env_and_only_when_it_is_unset`이 맡는다.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// 전용 서버와 TUI가 받은 `HERDR_ENV`(없으면 `None`)를 돌려준다.
fn run(mode: &str, herdr: Option<&str>) -> (Option<String>, Option<String>) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let state = dir.path().join("state");
    let out = dir.path().join("out");
    for d in [&bin, &state, &out] {
        std::fs::create_dir(d).unwrap();
    }
    // macOS의 unix 소켓 경로 길이 제한 안에 들도록 짧은 이름을 쓴다.
    let socket = dir.path().join("s");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let opened = serde_json::json!({
        "gen": "0123456789abcdef",
        "state_dir": state,
        "listen": format!("unix://{}", socket.display()),
    });
    script(
        &bin.join("tasty"),
        &format!(
            "case \"$1 $2\" in\n\
             'list info') exit 0 ;;\n\
             'codex session-open') printf '%s' {} ; exit 0 ;;\n\
             esac\nexit 1\n",
            quote(&opened.to_string()),
        ),
    );
    // 서버는 정리될 때까지 남고 TUI는 기록한 뒤 바로 끝난다.
    script(
        &bin.join("codex"),
        &format!(
            "if [ \"$1\" = app-server ]; then role=server; else role=tui; fi\n\
             if [ -n \"${{HERDR_ENV+x}}\" ]; then printf 'set:%s' \"$HERDR_ENV\"; else printf unset; fi > {out}/\"$role.tmp\"\n\
             mv {out}/\"$role.tmp\" {out}/\"$role\"\n\
             if [ \"$role\" = server ]; then exec sleep 30; fi\n",
            out = quote(&out.to_string_lossy()),
        ),
    );
    let mut path = std::ffi::OsString::from(&bin);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tasty-plugin-codex"));
    cmd.arg(mode)
        .env("PATH", path)
        .env("TASTY_SURFACE_ID", "7")
        .env_remove("TASTY_SESSION_TOKEN")
        .env_remove("TASTY_PARENT_HOME")
        .env_remove("TASTY_LOCALE")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null());
    match herdr {
        Some(value) => cmd.env("HERDR_ENV", value),
        None => cmd.env_remove("HERDR_ENV"),
    };
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "{mode} {herdr:?}: {output:?}");
    let read = |role: &str| {
        let text = std::fs::read_to_string(out.join(role))
            .unwrap_or_else(|e| panic!("{mode} {herdr:?} {role}: {e}: {output:?}"));
        text.strip_prefix("set:").map(str::to_owned)
    };
    (read("server"), read("tui"))
}

#[test]
fn both_helper_modes_give_only_the_tui_herdr_env_when_it_is_unset() {
    for mode in ["terminal", "session"] {
        assert_eq!(
            run(mode, None),
            (None, Some("1".to_owned())),
            "{mode}: (server, tui)"
        );
    }
}

#[test]
fn both_helper_modes_keep_a_defined_herdr_env_for_the_server_and_the_tui() {
    for mode in ["terminal", "session"] {
        assert_eq!(
            run(mode, Some("0")),
            (Some("0".to_owned()), Some("0".to_owned())),
            "{mode}: (server, tui)"
        );
    }
}
