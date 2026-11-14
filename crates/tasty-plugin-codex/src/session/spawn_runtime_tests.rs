//! Execute helper-built TUI commands with an isolated argv-recording CLI.
use super::*;
use crate::session::helper::{Args, tui_command};
use serde_json::{Value, json};

#[test]
fn actual_tui_argv_and_resumed_settings_preserve_child_overrides() {
    let _logs =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
    for (model, effort) in [("provider/model-a", "medium"), ("provider:model-b", "high")] {
        let args = vec![
            "-a".into(),
            "never".into(),
            "-m".into(),
            model.into(),
            "-c".into(),
            format!("model_reasoning_effort={effort}"),
        ];
        for resume in [
            None,
            Some("019f55e7-3dfa-7a00-8000-0000000000aa".to_owned()),
        ] {
            assert_mock_tui_overrides(model, effort, &args, resume);
        }
    }
    for effort in crate::spawn_options::EFFORTS {
        assert!(
            validate_tui_args(&["-c".into(), format!("model_reasoning_effort={effort}")]).is_ok()
        );
    }
    for bad in [
        "other=medium",
        "model_reasoning_effort=",
        "model_reasoning_effort=medium;id",
        "model_reasoning_effort=medium -c x=1",
    ] {
        assert!(validate_tui_args(&["-c".into(), bad.into()]).is_err());
    }
    let default = ResumeSettings::from_tui_args(&[]);
    assert_eq!(
        default.params("t"),
        json!({"threadId":"t","excludeTurns":true})
    );
    assert_eq!(default.verify(&Value::Null), Ok(()));
}

fn assert_mock_tui_overrides(model: &str, effort: &str, args: &[String], resume: Option<String>) {
    let params = Args {
        resume: resume.clone(),
        tui_args: args.to_vec(),
        ..Default::default()
    };
    // The mock executable receives the same argv assembled for the real TUI.
    let built = tui_command("ws://127.0.0.1:32100", None, &params, None);
    let mut mock = std::process::Command::new("python3");
    mock.args(["-c", "import json,sys;print(json.dumps(sys.argv[1:]))"]);
    mock.args(built.get_args());
    let out = mock.output().unwrap();
    assert!(out.status.success());
    let observed: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        observed.iter().any(|a| a == "--remote-auth-token-env"),
        resume.is_none()
    );
    if resume.is_none() {
        assert!(observed.windows(2).any(|a| a == ["-m", model]));
        assert!(observed.contains(&format!("model_reasoning_effort={effort}")));
    } else {
        assert!(!observed.contains(&"-m".to_owned()));
        assert!(
            !observed
                .iter()
                .any(|a| a.starts_with("model_reasoning_effort="))
        );
        let settings = ResumeSettings::from_tui_args(args);
        assert_eq!(settings.params(resume.as_deref().unwrap())["model"], model);
        assert_eq!(
            settings.params(resume.as_deref().unwrap())["config"]["model_reasoning_effort"],
            effort
        );
    }
    tracing::info!(
        "mock TUI argv: {}",
        String::from_utf8_lossy(&out.stdout).trim()
    );
}
