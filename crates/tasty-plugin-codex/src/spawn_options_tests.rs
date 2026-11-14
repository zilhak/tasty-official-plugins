use super::*;
use crate::delivery::testing::Fixture;
use serde_json::json;
use std::cell::RefCell;
use tasty_plugin_agent_common::host_call::HostCall;

#[derive(Default)]
struct Host(RefCell<Vec<(String, Value)>>);
impl HostCall for Host {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        self.0.borrow_mut().push((method.into(), params));
        Ok(match method {
            "terminal.spawn" => json!({"child_surface_id":40,"child_index":1}),
            "settings.get_plugin_setting" => json!({"value":"inherit"}),
            "terminal.children" => json!({"children":[]}),
            _ => json!({}),
        })
    }
}

#[test]
fn invalid_ipc_values_are_rejected_before_any_host_side_effect() {
    let _logs =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
    let fx = Fixture::new();
    for (key, values) in [
        (
            "model",
            vec![
                json!(null),
                json!(false),
                json!(42),
                json!([]),
                json!({}),
                json!(""),
                json!(" "),
                json!("--help"),
                json!("-m"),
                json!("a b"),
                json!("a\nb"),
                json!("a;touch marker"),
                json!("$(id)"),
                json!("`id`"),
                json!("a\"b"),
            ],
        ),
        (
            "effort",
            vec![
                json!(null),
                json!(true),
                json!(7),
                json!([]),
                json!({}),
                json!(""),
                json!(" "),
                json!("impossible-effort"),
                json!("medium -c x=1"),
                json!("--help"),
            ],
        ),
    ] {
        for value in values {
            let host = Host::default();
            let mut params = json!({"surface":7,"workspace":"1"});
            params[key] = value;
            assert!(
                fx.with(|d| crate::handlers::handle_spawn(&host, &params, &fx.tr, d))
                    .is_err(),
                "{params}"
            );
            assert!(host.0.borrow().is_empty(), "{params}");
            tracing::info!("invalid-before-spawn: {params}; host_calls=0");
        }
    }
}

#[test]
fn independent_children_and_omission_keep_model_arguments_local() {
    let fx = Fixture::new();
    for (model, effort) in [
        (Some("provider/model-a"), Some("medium")),
        (Some("provider:model-b"), Some("high")),
        (None, None),
    ] {
        let host = Host::default();
        let mut params = json!({"surface":7,"workspace":"1"});
        if let Some(m) = model {
            params["model"] = m.into();
        }
        if let Some(e) = effort {
            params["effort"] = e.into();
        }
        fx.with(|d| crate::handlers::handle_spawn(&host, &params, &fx.tr, d))
            .unwrap();
        let calls = host.0.borrow();
        let command = calls.iter().find(|(m, _)| m == "surface.send").unwrap().1["text"]
            .as_str()
            .unwrap();
        if let Some(m) = model {
            assert!(command.contains(&format!("'-m' '{m}'")), "{command}");
        } else {
            assert!(!command.contains("'-m'"));
        }
        if let Some(e) = effort {
            assert!(
                command.contains(&format!("'-c' 'model_reasoning_effort={e}'")),
                "{command}"
            );
        } else {
            assert!(!command.contains("model_reasoning_effort"));
        }
        assert!(
            !calls
                .iter()
                .any(|(m, _)| m.contains("setting") && m != "settings.get_plugin_setting")
        );
    }
}

#[test]
fn same_session_restore_and_reboot_keep_explicit_values() {
    let fx = Fixture::new();
    let args = parse(
        &json!({"model":"provider/model-a","effort":"medium"}),
        &fx.tr,
    )
    .unwrap();
    let mut session =
        crate::session::registry::open_session(fx.root(), 40, None, args.clone()).unwrap();
    session.model = Some("hook-model".into());
    let thread = "019f55e7-3dfa-7a00-8000-0000000000aa";
    let line = crate::session::restore_line(&session, thread).unwrap();
    assert!(line.contains("'-m' 'provider/model-a'"));
    assert!(!line.contains("hook-model"));
    assert!(line.contains("'model_reasoning_effort=medium'"));
    assert_eq!(saved_tokens(&session), args);
    let settings = crate::session::command::ResumeSettings::from_tui_args(&session.tui_args);
    assert_eq!(
        settings.params(thread)["config"]["model_reasoning_effort"],
        "medium"
    );
    assert!(crate::session::command::resume_tui_args(&session.tui_args).is_empty());
}

#[cfg(unix)]
#[test]
fn actual_shell_helper_argv_is_literal_for_each_child() {
    let _logs =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("mock helper's executable");
    std::fs::write(
        &program,
        "#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let fx = Fixture::new();
    for params in [
        json!({"model":"provider/model-a","effort":"medium"}),
        json!({"model":"provider:model-b","effort":"high"}),
        json!({}),
    ] {
        let args = parse(&params, &fx.tr).unwrap();
        let line =
            crate::session::command::command_line(program.to_str().unwrap(), None, None, &args);
        let output = std::process::Command::new("sh")
            .args(["-c", &line])
            .output()
            .unwrap();
        assert!(output.status.success());
        let observed: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
        let mut expected = vec!["session".to_owned()];
        if !args.is_empty() {
            expected.push("--".into());
            expected.extend(args);
        }
        assert_eq!(observed, expected);
        tracing::info!(
            "mock helper argv: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
}
