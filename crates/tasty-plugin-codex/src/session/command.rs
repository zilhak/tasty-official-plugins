//! launch·spawn·respawn·reboot·복원이 함께 쓰는 세션 helper 명령.
//!
//! 터미널에는 플러그인 실행 파일의 `session` 모드를 실행하는 명령 한 줄만 보낸다. 서버와
//! TUI의 시작·종료는 helper가 맡는다. 명령은 기존 기동 명령과 같이 POSIX 셸 문법이며
//! 모든 값을 작은따옴표로 감싼다.

use std::path::Path;

/// 작은따옴표로 감싼 POSIX 셸 단어.
pub(crate) fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// helper로 실행할 플러그인 실행 파일. 설치 폴더의 실행 파일을 우선하고, 없으면 현재
/// 프로세스의 경로를 쓴다.
pub(crate) fn helper_program() -> std::io::Result<String> {
    let name = if cfg!(windows) {
        "tasty-plugin-codex.exe"
    } else {
        "tasty-plugin-codex"
    };
    if let Some(dir) = std::env::var_os("TASTY_PLUGIN_DIR") {
        let candidate = Path::new(&dir).join(name);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

/// 제출 문자 없는 helper 명령. 복원 명령으로 저장하는 형태다.
pub(crate) fn command_line(
    program: &str,
    resume: Option<&str>,
    prompt_file: Option<&Path>,
    tui_args: &[String],
) -> String {
    let mut parts = vec![sh_quote(program), "session".to_string()];
    if let Some(thread) = resume {
        parts.push("--resume".into());
        parts.push(sh_quote(thread));
    }
    if let Some(file) = prompt_file {
        parts.push("--prompt-file".into());
        parts.push(sh_quote(&file.to_string_lossy()));
    }
    if !tui_args.is_empty() {
        parts.push("--".into());
        parts.extend(tui_args.iter().map(|a| sh_quote(a)));
    }
    parts.join(" ")
}

/// 정책 문자열(`-a never -s read-only` 등)을 TUI 인자 목록으로 나눈다.
pub(crate) fn policy_tokens(policy_args: &str) -> Vec<String> {
    policy_args.split_whitespace().map(str::to_string).collect()
}

/// helper가 TUI에 넘길 수 있는 인자인지 검사한다. 정책·모델 외의 옵션은 받지 않는다.
pub(crate) fn validate_tui_args(args: &[String]) -> Result<(), String> {
    let value_ok = |v: &str| {
        !v.is_empty()
            && v.len() <= 256
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':'))
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-a" | "-s" | "-m" => {
                let Some(v) = args.get(i + 1) else {
                    return Err(format!("missing value after {}", args[i]));
                };
                if !value_ok(v) || (args[i] == "-m" && !crate::spawn_options::model_ok(v)) {
                    return Err(format!("invalid value for {}", args[i]));
                }
                i += 2;
            }
            "-c" => {
                if args
                    .get(i + 1)
                    .and_then(|v| crate::spawn_options::effort_config(v))
                    .is_none()
                {
                    return Err("only a valid model_reasoning_effort override is supported".into());
                }
                i += 2;
            }
            "--dangerously-bypass-approvals-and-sandbox" => i += 1,
            other => return Err(format!("unsupported session argument {other}")),
        }
    }
    Ok(())
}

/// 모델 인자를 붙인다. 이미 지정돼 있으면 그대로 둔다.
pub(crate) fn with_model(mut args: Vec<String>, model: Option<&str>) -> Vec<String> {
    if let Some(model) = model
        && !args.iter().any(|a| a == "-m")
        && validate_tui_args(&["-m".into(), model.into()]).is_ok()
    {
        args.push("-m".into());
        args.push(model.into());
    }
    args
}

/// 재개할 thread에 적용할 승인·sandbox·모델.
///
/// Codex 0.154 TUI는 `--remote`로 기존 thread를 재개할 때 승인·sandbox 재정의(`-a`·`-s`·
/// `--dangerously-bypass-approvals-and-sandbox`)가 있으면 "Permission overrides are not supported
/// when resuming a remote task."로 시작을 멈춘다. 재정의를 빼면 서버가 그 thread의 마지막 턴에
/// 저장된 정책을 복원하므로 요청한 정책이 조용히 사라진다. 그래서 helper가 소유 서버의
/// `thread/resume`에 이 값을 넣어 적용하고 응답으로 확인한 뒤, TUI에는 넘기지 않는다.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ResumeSettings {
    pub approval: Option<String>,
    pub sandbox: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl ResumeSettings {
    /// helper 인자에서 정책·모델을 읽는다. 우회 플래그는 승인 never와 danger-full-access다.
    pub(crate) fn from_tui_args(args: &[String]) -> Self {
        let mut out = Self::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-a" => out.approval = it.next().cloned(),
                "-s" => out.sandbox = it.next().cloned(),
                "-m" => out.model = it.next().cloned(),
                "-c" => {
                    out.effort = it
                        .next()
                        .and_then(|v| crate::spawn_options::effort_config(v))
                        .map(str::to_owned)
                }
                "--dangerously-bypass-approvals-and-sandbox" => {
                    out.approval = Some("never".into());
                    out.sandbox = Some("danger-full-access".into());
                }
                _ => {}
            }
        }
        out
    }

    /// `thread/resume` 요청 인자. 기록은 받지 않는다.
    pub(crate) fn params(&self, thread: &str) -> serde_json::Value {
        let mut p = serde_json::json!({ "threadId": thread, "excludeTurns": true });
        if let Some(a) = &self.approval {
            p["approvalPolicy"] = a.as_str().into();
        }
        if let Some(s) = &self.sandbox {
            p["sandbox"] = s.as_str().into();
        }
        if let Some(m) = &self.model {
            p["model"] = m.as_str().into();
        }
        if let Some(e) = &self.effort {
            p["config"] = serde_json::json!({"model_reasoning_effort": e});
        }
        p
    }

    /// 서버 응답이 요청한 값을 적용했는지 확인한다. 이미 로드된 thread는 다른 값을 무시하고 기존
    /// 설정으로 응답할 수 있다.
    pub(crate) fn verify(&self, result: &serde_json::Value) -> Result<(), String> {
        if let Some(want) = &self.approval {
            let got = result.get("approvalPolicy");
            if got.and_then(serde_json::Value::as_str) != Some(want.as_str()) {
                return Err(format!(
                    "the server kept approval policy {got:?}, requested {want}"
                ));
            }
        }
        if let Some(want) = &self.sandbox {
            let got = result
                .get("sandbox")
                .and_then(|s| s.get("type"))
                .and_then(serde_json::Value::as_str);
            let expected = match want.as_str() {
                "read-only" => "readOnly",
                "workspace-write" => "workspaceWrite",
                "danger-full-access" => "dangerFullAccess",
                other => other,
            };
            if got != Some(expected) {
                return Err(format!("the server kept sandbox {got:?}, requested {want}"));
            }
        }
        if let Some(want) = &self.model {
            let got = result.get("model").and_then(serde_json::Value::as_str);
            if got != Some(want.as_str()) {
                return Err(format!("the server kept model {got:?}, requested {want}"));
            }
        }
        if let Some(want) = &self.effort {
            let got = result
                .get("reasoningEffort")
                .and_then(serde_json::Value::as_str);
            if got != Some(want.as_str()) {
                return Err(format!(
                    "the server kept reasoning effort {got:?}, requested {want}"
                ));
            }
        }
        Ok(())
    }
}

/// 재개하는 TUI에 넘길 인자. 정책·모델은 helper가 서버에 적용하므로 뺀다.
pub(crate) fn resume_tui_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-a" | "-s" | "-m" | "-c" => {
                it.next();
            }
            "--dangerously-bypass-approvals-and-sandbox" => {}
            _ => out.push(a.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 재개 TUI에는 정책·모델 인자가 남지 않는다.
    #[test]
    fn a_resuming_tui_gets_no_policy_or_model_arguments() {
        for args in [
            &["-a", "never", "-s", "read-only", "-m", "gpt-x"][..],
            &["--dangerously-bypass-approvals-and-sandbox"][..],
            &[][..],
        ] {
            assert!(resume_tui_args(&strings(args)).is_empty(), "{args:?}");
        }
    }

    /// 정책·모델은 `thread/resume` 인자로 옮긴다. 우회 플래그는 never와 danger-full-access다.
    #[test]
    fn resume_settings_carry_the_requested_policy() {
        let s = ResumeSettings::from_tui_args(&strings(&[
            "-a",
            "on-request",
            "-s",
            "workspace-write",
            "-m",
            "gpt-x",
        ]));
        assert_eq!(
            s.params("t1"),
            serde_json::json!({
                "threadId": "t1", "excludeTurns": true, "approvalPolicy": "on-request",
                "sandbox": "workspace-write", "model": "gpt-x",
            })
        );
        let b = ResumeSettings::from_tui_args(&strings(&[
            "--dangerously-bypass-approvals-and-sandbox",
        ]));
        assert_eq!(b.approval.as_deref(), Some("never"));
        assert_eq!(b.sandbox.as_deref(), Some("danger-full-access"));
        assert_eq!(
            ResumeSettings::from_tui_args(&[]).params("t1"),
            serde_json::json!({ "threadId": "t1", "excludeTurns": true })
        );
    }

    /// 서버 응답(0.154 실측 형태)이 요청과 다르면 실패한다.
    #[test]
    fn resume_settings_are_verified_against_the_server_reply() {
        let s = ResumeSettings::from_tui_args(&strings(&["-a", "never", "-s", "read-only"]));
        let applied = serde_json::json!({
            "approvalPolicy": "never", "sandbox": { "type": "readOnly", "networkAccess": false },
        });
        assert_eq!(s.verify(&applied), Ok(()));
        let kept = serde_json::json!({
            "approvalPolicy": "on-request",
            "sandbox": { "type": "workspaceWrite", "writableRoots": [] },
        });
        assert!(s.verify(&kept).unwrap_err().contains("approval policy"));
        let sandbox_kept = serde_json::json!({
            "approvalPolicy": "never", "sandbox": { "type": "workspaceWrite" },
        });
        assert!(s.verify(&sandbox_kept).unwrap_err().contains("sandbox"));
        let m = ResumeSettings::from_tui_args(&strings(&["-m", "gpt-x"]));
        assert!(m.verify(&serde_json::json!({ "model": "other" })).is_err());
        assert_eq!(
            ResumeSettings::default().verify(&serde_json::json!({})),
            Ok(())
        );
    }

    #[test]
    fn quoting_keeps_every_value_literal() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("$(rm -rf ~)`x`"), "'$(rm -rf ~)`x`'");
    }

    #[test]
    fn the_command_line_names_the_helper_mode_and_quotes_each_part() {
        let line = command_line(
            "/opt/my plugins/tasty-plugin-codex",
            Some("01a12f74-dab9-7a00-8000-0123456789ab"),
            Some(Path::new("/tmp/p'1")),
            &policy_tokens("-a never -s read-only"),
        );
        assert_eq!(
            line,
            "'/opt/my plugins/tasty-plugin-codex' session --resume '01a12f74-dab9-7a00-8000-0123456789ab' \
             --prompt-file '/tmp/p'\\''1' -- '-a' 'never' '-s' 'read-only'"
        );
        assert_eq!(command_line("p", None, None, &[]), "'p' session");
    }

    #[test]
    fn only_policy_and_model_arguments_reach_the_tui() {
        assert!(
            validate_tui_args(&policy_tokens("-a never -s workspace-write -m gpt-5.5")).is_ok()
        );
        assert!(
            validate_tui_args(&policy_tokens("--dangerously-bypass-approvals-and-sandbox")).is_ok()
        );
        for bad in ["-c x=1", "-a", "--remote ws://x", "-m a;b", "resume"] {
            assert!(validate_tui_args(&policy_tokens(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_known_model_is_added_once() {
        assert_eq!(with_model(vec![], Some("gpt-5.5")), vec!["-m", "gpt-5.5"]);
        assert_eq!(
            with_model(vec!["-m".into(), "o3".into()], Some("gpt-5.5")),
            vec!["-m", "o3"]
        );
        assert!(with_model(vec![], Some("bad model")).is_empty());
    }
}

#[cfg(all(test, unix))]
#[path = "spawn_runtime_tests.rs"]
mod spawn_runtime_tests;
