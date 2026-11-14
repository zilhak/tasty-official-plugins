//! 제품 명령 생성기의 결과를 실제 셸로 실행한다. helper 자리에 인자를 그대로 출력하는 가짜
//! 실행 파일을 두고, codex를 바꾸는 alias·function이 있어도 helper가 받은 인자가 그대로인지
//! 확인한다. 받은 인자는 helper의 인자 해석기에도 넣어 본다.
use std::process::Command;

// 셸 시작 설정만 격리한다. 일반 환경은 그대로 둬 전달 여부를 함께 확인한다.
fn isolated_shell(shell: &str) -> Command {
    let mut process = Command::new(shell);
    for key in ["BASH_ENV", "ENV", "ZDOTDIR", "SHELLOPTS", "BASHOPTS"] {
        process.env_remove(key);
    }
    if shell == "bash" {
        process.args(["--noprofile", "--norc", "-O", "expand_aliases"]);
    } else if shell == "zsh" {
        process.arg("-f");
    }
    process
}

/// sh는 필수이며 다른 셸은 설치돼 있을 때만 실행한다. 실행 실패는 시험 실패로 처리한다.
fn shell_is_installed(shell: &str) -> bool {
    match isolated_shell(shell).args(["-c", ":"]).output() {
        Ok(output) => {
            assert!(output.status.success(), "{shell}: {output:?}");
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && shell != "sh" => {
            tracing::warn!("SKIP {shell}: shell not installed");
            false
        }
        Err(error) => panic!("cannot start {shell}: {error}"),
    }
}

/// 명령 줄 하나를 `setup` 뒤에 실행하고 helper가 받은 인자를 확인한다.
fn run_helper_command(
    shell: &str,
    setup: &str,
    program: &str,
    policy: &str,
    resume: Option<&str>,
    prompt: Option<&std::path::Path>,
) {
    let tui = crate::session::command::policy_tokens(policy);
    let line = crate::session::command::command_line(program, resume, prompt, &tui);
    let out = isolated_shell(shell)
        .args([
            "-c",
            &format!("{setup}eval \"$1\""),
            "codex-shell-test",
            &line,
        ])
        .env("TASTY_SURFACE_ID", "77")
        .output()
        .unwrap();
    assert!(out.status.success(), "{shell}: {line}: {out:?}");
    let mut argv: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .map(|s| String::from_utf8(s.to_vec()).unwrap())
        .collect();
    assert_eq!(argv.pop().as_deref(), Some(""));
    assert_eq!(argv.remove(0), "77", "surface는 터미널 환경에서 읽는다");
    assert_eq!(argv.remove(0), "session");
    let parsed = crate::session::helper::parse_args(&argv)
        .unwrap_or_else(|e| panic!("{shell}: {argv:?}: {e}"));
    assert_eq!(parsed.resume.as_deref(), resume, "{shell}: {line}");
    assert_eq!(parsed.prompt_file.as_deref(), prompt, "{shell}: {line}");
    assert_eq!(parsed.tui_args, tui, "{shell}: {line}");
}

#[test]
fn the_session_helper_receives_its_arguments_verbatim_in_every_shell() {
    let dir = tempfile::tempdir().unwrap();
    // 공백·작은따옴표·$가 든 설치 경로도 하나의 인자로 실행돼야 한다.
    let install = dir.path().join("Tasty plugins 'x' $HOME");
    std::fs::create_dir(&install).unwrap();
    let fake = install.join("tasty-plugin-codex");
    tasty_test_support::write_executable(
        &fake,
        "#!/bin/sh\nprintf '%s\\0' \"$TASTY_SURFACE_ID\" \"$@\"\n",
    )
    .unwrap();
    let program = fake.to_string_lossy().into_owned();
    let thread = "019f55e7-3dfa-7a00-8000-0123456789ab";
    let prompt_file = dir.path().join("p r'o\"mpt $(false)");
    let bypass = "--dangerously-bypass-approvals-and-sandbox";
    let mut executions = 0;
    for shell in ["sh", "bash", "zsh"] {
        if !shell_is_installed(shell) {
            continue;
        }
        for setup in [
            format!("alias codex='codex {bypass}'\n"),
            format!("codex() {{ command codex {bypass} \"$@\"; }}\n"),
        ] {
            for policy in ["", "-a never", "-a on-request -s read-only", bypass] {
                for (resume, prompt) in [
                    (None, None),
                    (Some(thread), None),
                    (None, Some(prompt_file.as_path())),
                ] {
                    run_helper_command(shell, &setup, &program, policy, resume, prompt);
                    executions += 1;
                }
            }
        }
    }
    // sh 하나만 있어도 2개 래퍼 × 4개 정책 × 3개 형태를 실행한다.
    assert!(executions >= 24, "{executions}");
    tracing::info!("PASS {executions} helper command executions");
}
