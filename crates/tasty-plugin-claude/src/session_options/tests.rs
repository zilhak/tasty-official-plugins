use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::*;
use crate::codex_return::Returns;
use crate::error_scan::ErrorScanner;

fn tr() -> Translator {
    Translator::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("lang"), "en")
}

#[derive(Default)]
struct Host {
    calls: Mutex<Vec<(String, Value)>>,
    meta: Mutex<HashMap<(u32, String), String>>,
}

impl HostCall for Host {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        let mut calls = self.calls.lock().unwrap();
        calls.push((method.into(), params.clone()));
        let surface = params["surface_id"].as_u64().unwrap_or(0) as u32;
        let key = params["key"].as_str().unwrap_or("").to_string();
        match method {
            "terminal.spawn" => Ok(
                json!({"child_surface_id": 100 + calls.iter().filter(|(m, _)| m == method).count(), "index": 0}),
            ),
            "surface.meta.set" => {
                self.meta
                    .lock()
                    .unwrap()
                    .insert((surface, key), params["value"].as_str().unwrap().into());
                Ok(json!({}))
            }
            "surface.meta.unset" => {
                self.meta.lock().unwrap().remove(&(surface, key));
                Ok(json!({}))
            }
            "surface.meta.get" => {
                Ok(json!({"value": self.meta.lock().unwrap().get(&(surface, key))}))
            }
            "settings.get_plugin_setting" => Ok(json!({})),
            "session.issue" => Ok(json!({"token": "mock-token"})),
            "surface.send" | "hook.set" => Ok(json!({})),
            "terminal.children" => Ok(json!({"children": []})),
            "hook.list" => Ok(json!([])),
            other => panic!("unexpected host call {other}: {params}"),
        }
    }
}

fn spawn(host: &Host, extra: Value) -> Result<Value, IpcMethodError> {
    let mut params = json!({"surface": 7, "workspace": "test"});
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    crate::handlers::handle_spawn(
        &Arc::new(Mutex::new(ErrorScanner::new())),
        &Returns::new(None),
        host,
        &params,
        None,
        &tr(),
    )
}

#[test]
fn invalid_values_are_rejected_before_any_host_call_or_spawn() {
    let host = Host::default();
    let invalid = [
        Value::Null,
        json!(true),
        json!(7),
        json!([]),
        json!({}),
        json!(""),
        json!(" "),
        json!("--help"),
        json!("-m"),
        json!("opus --fallback-model sonnet"),
        json!("opus\nsonnet"),
        json!("a\0b"),
    ];
    for key in ["model", "effort"] {
        for value in &invalid {
            let mut extra = json!({});
            extra[key] = value.clone();
            let err = spawn(&host, extra).unwrap_err();
            assert!(err.message.contains(key), "{err:?}");
            assert!(
                host.calls.lock().unwrap().is_empty(),
                "side effect before rejecting {key}={value}"
            );
        }
    }
    assert!(spawn(&host, json!({"effort": "unlimited"})).is_err());
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn aliases_full_ids_arns_and_documented_efforts_are_not_model_allowlisted() {
    for model in [
        "opus",
        "sonnet[1m]",
        "claude-opus-4-7",
        "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-opus-v1:0",
        "vendor/custom-deployment",
    ] {
        for effort in EFFORTS {
            let options =
                SessionOptions::parse(&json!({"model": model, "effort": effort}), &tr()).unwrap();
            assert_eq!(options.model.as_deref(), Some(model));
            assert_eq!(options.effort.as_deref(), Some(*effort));
        }
    }
}

#[test]
fn session_record_survives_lost_surface_metadata_and_default_clears_reused_surface() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::default();
    let first = SessionOptions::parse(&json!({"model": "opus", "effort": "max"}), &tr()).unwrap();
    first.record(&host, 101, "session-a").unwrap();
    store(Some(dir.path()), "session-a", &first);
    assert_eq!(
        load(&host, 999, Some(dir.path()), "session-a", &tr()).unwrap(),
        first
    );
    assert_eq!(
        load(&host, 999, Some(dir.path()), "session-b", &tr()).unwrap(),
        SessionOptions::default()
    );
    SessionOptions::default()
        .record(&host, 101, "session-new")
        .unwrap();
    assert_eq!(
        load(&host, 101, Some(dir.path()), "session-a", &tr()).unwrap(),
        first
    );
    assert!(record_path(Some(dir.path()), "../escape").is_none());
    std::fs::write(record_path(Some(dir.path()), "broken").unwrap(), "{broken").unwrap();
    assert!(load(&host, 999, Some(dir.path()), "broken", &tr()).is_err());
}

#[cfg(unix)]
fn assert_cli_session_owner(host: &Host, surface: u32, args: &mut Vec<String>) {
    assert_eq!(args.remove(0), "--session-id");
    let id = args.remove(0);
    tracing::info!("mock CLI launch: session_id={id}, argv={args:?}");
    assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
    let meta: Value =
        serde_json::from_str(&host.meta.lock().unwrap()[&(surface, META_KEY.into())]).unwrap();
    assert_eq!(meta["session"], id);
    assert_eq!(meta["pending"], true);
}

#[cfg(unix)]
fn run_mock_child_commands(host: &Host, dir: &Path, commands: &[String]) -> Vec<Vec<String>> {
    use std::process::Command;

    let mut argv = Vec::new();
    for command in commands {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command.trim_end_matches('\r'))
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let mut args = serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap();
        assert_cli_session_owner(host, 101 + argv.len() as u32, &mut args);
        argv.push(args);
    }
    argv
}

#[cfg(unix)]
fn assert_mock_rejection(executable: &Path, dir: &Path, command: &str, expected_argv: &[String]) {
    use std::process::Command;

    std::fs::write(
        executable,
        "#!/usr/bin/python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\nsys.exit(42)\n",
    )
    .unwrap();
    let rejected = Command::new("sh")
        .arg("-c")
        .arg(command.trim_end_matches('\r'))
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(42));
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&rejected.stdout).unwrap()[2..],
        *expected_argv
    );
    tracing::info!("mock rejection: shell rc=42; one invocation, no fallback-model argument");
}

/// Exercise the real spawn handler, then run its shell command against an isolated mock CLI.
/// No user CLI, settings, provider or app server is involved.
#[cfg(unix)]
#[test]
fn isolated_mock_cli_receives_independent_child_argv_profiles_and_omission() {
    use std::os::unix::fs::PermissionsExt;

    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(std::io::stderr)
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("claude");
    std::fs::write(
        &executable,
        "#!/usr/bin/python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = dir.path().join("profile'$(false)`false`\".json");
    let profile_text = r#"{"model":"profile-model","effortLevel":"low","permissions":{"allow":["Read"]},"hooks":{}}"#;
    std::fs::write(&profile, profile_text).unwrap();
    let host = Host::default();
    host.meta
        .lock()
        .unwrap()
        .insert((7, META_KEY.into()), "parent-unchanged".into());
    host.meta
        .lock()
        .unwrap()
        .insert((8, META_KEY.into()), "sibling-unchanged".into());
    let model = "arn:aws:bedrock:region:123:profile/vendor'$(false);`false`";
    let a = spawn(&host, json!({"model": model, "effort": "high", "profile_file": profile, "permission_mode": "plan", "prompt": "literal $HOME ; `false` 'quoted'"})).unwrap();
    let b = spawn(&host, json!({"model": "sonnet[1m]", "effort": "medium"})).unwrap();
    let c = spawn(&host, json!({})).unwrap();
    assert_eq!(a["requested_session_options"]["model"], model);
    assert_eq!(b["requested_session_options"]["effort"], "medium");
    assert_eq!(c["requested_session_options"], json!({}));
    let commands: Vec<_> = host
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "surface.send")
        .map(|(_, p)| p["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(commands.len(), 3);
    let argv = run_mock_child_commands(&host, dir.path(), &commands);
    assert_eq!(
        argv[0],
        vec![
            "--model",
            model,
            "--effort",
            "high",
            "--settings",
            profile.to_str().unwrap(),
            "--permission-mode",
            "plan",
            "literal $HOME ; `false` 'quoted'"
        ]
    );
    assert_eq!(argv[1], vec!["--model", "sonnet[1m]", "--effort", "medium"]);
    assert!(argv[2].is_empty());
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), profile_text);
    let meta = host.meta.lock().unwrap();
    assert_eq!(meta[&(7, META_KEY.into())], "parent-unchanged");
    assert_eq!(meta[&(8, META_KEY.into())], "sibling-unchanged");
    assert_mock_rejection(&executable, dir.path(), &commands[1], &argv[1]);
    tracing::info!(
        "mock_cli child argv: {}",
        serde_json::to_string(&argv).unwrap()
    );
}

/// POSIX shells need the brackets quoted; Git Bash on Windows reads the same rule.
const MODEL_ARG: &str = "'provider/model[1m]'";

/// Linux·macOS keep the double-quoted path; Git Bash on Windows leaves plain characters bare.
const SETTINGS_ARG: &str = if cfg!(windows) {
    "/profile.json"
} else {
    "\"/profile.json\""
};

#[cfg(not(windows))]
#[test]
fn settings_path_keeps_the_posix_double_quote_escapes() {
    assert_eq!(
        quote_path(r#"/home/user name/a\b"c$d`e.json"#),
        r#""/home/user name/a\\b\"c\$d\`e.json""#
    );
}

#[cfg(windows)]
#[test]
fn settings_path_uses_git_bash_single_quotes() {
    assert_eq!(
        quote_path(r"C:\Users\user name\profiles\reviewer.json"),
        r"'C:\Users\user name\profiles\reviewer.json'"
    );
    assert_eq!(
        quote_path(r"\\localhost\D$\x\r.json"),
        r"'\\localhost\D$\x\r.json'"
    );
}

/// Git Bash reads the Windows command lines, so backslashes, UNC prefixes, `$`, backquotes and
/// double quotes must stay literal; `'` and `!` leave the quotes.
#[test]
fn git_bash_quoting_keeps_windows_paths_literal() {
    let cases = [
        (
            r"C:\Users\user name\p\reviewer.json",
            r"'C:\Users\user name\p\reviewer.json'",
        ),
        (r"\\localhost\D$\x\r.json", r"'\\localhost\D$\x\r.json'"),
        (r"C:\a$HOME\r.json", r"'C:\a$HOME\r.json'"),
        (r"C:\a`id`\r.json", r"'C:\a`id`\r.json'"),
        (r"C:\a!b\r.json", r"'C:\a'\!'b\r.json'"),
        (r"C:\it's\r.json", r"'C:\it'\''s\r.json'"),
        (r#"C:\a"b\r.json"#, r#"'C:\a"b\r.json'"#),
        (r"C:\dir\", r"'C:\dir\'"),
        ("provider/model[1m]", "'provider/model[1m]'"),
        ("a&b", "'a&b'"),
        ("a;b", "'a;b'"),
        ("ultracode", "ultracode"),
    ];
    for (value, quoted) in cases {
        assert_eq!(posix_single_quote(value), quoted, "{value}");
    }
}

/// The quoted word must come back unchanged from an interactive-style bash with history
/// expansion on. Skipped where bash is missing.
#[cfg(unix)]
#[test]
fn git_bash_quoting_round_trips_through_bash() {
    let values = [
        r"C:\Users\user name\p\reviewer.json",
        r"\\localhost\D$\x\r.json",
        r"C:\a$HOME`id`\r.json",
        r"C:\a!b\r.json",
        r"C:\it's\r.json",
        r#"C:\a"b\r.json"#,
        r"C:\dir\",
        "provider/model[1m]",
        "a&b;c|d<e>f*",
    ];
    let script = values
        .iter()
        .map(|v| format!("printf '%s\\0' {}", posix_single_quote(v)))
        .collect::<Vec<_>>()
        .join("\n");
    let output = match std::process::Command::new("bash")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            &format!("set -H -o history\n{script}"),
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            tracing::warn!("bash unavailable, round trip not checked: {e}");
            return;
        }
    };
    assert!(output.status.success(), "{output:?}");
    let words: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .split_terminator('\0')
        .collect();
    assert_eq!(words, values);
}

#[test]
fn hook_restore_and_reboot_preserve_options_and_settings_without_permission_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let options = SessionOptions::parse(
        &json!({"model":"provider/model[1m]", "effort":"ultracode"}),
        &tr(),
    )
    .unwrap();
    store(Some(dir.path()), "sess-a", &options);
    let host = Host::default();
    let mut calls = vec![PlannedCall::MetaSet {
        surface_id: 999,
        key: RESTORE_COMMAND_META_KEY,
        value: crate::reboot::resume_command_line("sess-a", Some("/profile.json")),
    }];
    session_start(&mut calls, &host, 999, Some(dir.path()), "sess-a", &tr());
    let PlannedCall::MetaSet { value, .. } = &calls[0] else {
        panic!("restore command was not refreshed: {:?}", calls[0]);
    };
    assert_eq!(
        value,
        &format!(
            "claude -r sess-a --settings {SETTINGS_ARG} --model {MODEL_ARG} --effort ultracode"
        )
    );
    assert!(matches!(&calls[1], PlannedCall::MetaSet {key, value, ..}
        if *key == META_KEY && value == &bound_json("sess-a", false, &options).to_string()));
    let reboot =
        crate::reboot::resume_with_options("sess-a", Some("/profile.json"), Some("plan"), &options);
    assert_eq!(
        reboot,
        format!(
            "claude -r sess-a --settings {SETTINGS_ARG} --permission-mode plan --model {MODEL_ARG} --effort ultracode\r"
        )
    );
    std::fs::write(record_path(Some(dir.path()), "sess-a").unwrap(), "bad JSON").unwrap();
    session_start(&mut calls, &host, 999, Some(dir.path()), "sess-a", &tr());
    assert!(!calls.iter().any(
        |call| matches!(call, PlannedCall::MetaSet { key, .. } if *key == RESTORE_COMMAND_META_KEY)
    ));
}

#[test]
fn registered_profile_is_not_edited_by_explicit_model_and_effort() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("profile.json");
    let body = r#"{"model":"profile-model","effortLevel":"low","env":{"KEEP":"yes"}}"#;
    std::fs::write(&source, body).unwrap();
    crate::profile::register(Some(dir.path()), "worker", &source).unwrap();
    let params = json!({"profile": "worker", "model": "opus", "effort": "high"});
    let file = crate::handlers::resolve_profile_file_param(Some(dir.path()), &params, &tr())
        .unwrap()
        .unwrap();
    let options = SessionOptions::parse(&params, &tr()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&file).unwrap()).unwrap(),
        serde_json::from_str::<Value>(body).unwrap()
    );
    let command = format!("claude --settings {}{}", quote_path(&file), options.flags());
    assert!(command.ends_with(" --model opus --effort high"));
    assert_eq!(std::fs::read_to_string(&source).unwrap(), body);
}

#[test]
fn r1_old_metadata_migrates_only_with_session_ownership_or_a_matching_durable_record() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::default();
    let a = SessionOptions::parse(&json!({"model":"opus", "effort":"high"}), &tr()).unwrap();
    let b = SessionOptions::parse(&json!({"model":"sonnet"}), &tr()).unwrap();
    host.meta
        .lock()
        .unwrap()
        .insert((42, META_KEY.into()), a.json().to_string());
    // Unknown ownership cannot justify binding an old explicit model to an arbitrary session.
    assert!(load(&host, 42, Some(dir.path()), "session-a", &tr()).is_err());
    assert!(!record_path(Some(dir.path()), "session-a").unwrap().exists());
    store(Some(dir.path()), "session-b", &b);
    assert_eq!(
        load(&host, 42, Some(dir.path()), "session-b", &tr()).unwrap(),
        b
    );
    host.meta.lock().unwrap().insert(
        (42, crate::stop_pairing::SETTINGS_SESSION_META_KEY.into()),
        "session-a".into(),
    );
    assert_eq!(
        load(&host, 42, Some(dir.path()), "session-a", &tr()).unwrap(),
        a
    );
    assert_eq!(
        load(&host, 42, Some(dir.path()), "new-session", &tr()).unwrap(),
        SessionOptions::default()
    );
    let mut calls = Vec::new();
    session_start(&mut calls, &host, 42, Some(dir.path()), "session-a", &tr());
    assert!(matches!(&calls[0], PlannedCall::MetaSet { value, .. }
        if serde_json::from_str::<Value>(value).unwrap()["session"] == "session-a"));
}

#[test]
fn r1_missing_records_are_distinct_from_host_read_failures_corruption_and_io_errors() {
    struct FailedHost;
    impl HostCall for FailedHost {
        fn call(&self, _: &str, _: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
            Err(tasty_plugin_sdk::PluginError::HostClosed)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let host = Host::default();
    assert_eq!(
        load(&host, 42, Some(dir.path()), "absent", &tr()).unwrap(),
        SessionOptions::default()
    );
    assert!(load(&FailedHost, 42, Some(dir.path()), "absent", &tr()).is_err());
    let path = record_path(Some(dir.path()), "unreadable").unwrap();
    std::fs::create_dir_all(&path).unwrap(); // EISDIR on read, even under privileged test users.
    assert!(load(&host, 42, Some(dir.path()), "unreadable", &tr()).is_err());
    for corrupt in [
        "bad JSON",
        "null",
        r#"{"schema":99}"#,
        r#"{"schema":2,"session":"a","pending":false,"options":null}"#,
    ] {
        host.meta
            .lock()
            .unwrap()
            .insert((42, META_KEY.into()), corrupt.into());
        assert!(
            load(&host, 42, Some(dir.path()), "absent", &tr()).is_err(),
            "{corrupt}"
        );
    }
}

#[test]
fn r1_malformed_meta_reply_is_not_a_successful_omitted_option() {
    struct MalformedHost(Value);
    impl HostCall for MalformedHost {
        fn call(&self, _: &str, _: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
            Ok(self.0.clone())
        }
    }
    for reply in [json!({}), json!({"value": 42}), json!({"value": false})] {
        let host = MalformedHost(reply);
        assert!(load(&host, 42, None, "session-a", &tr()).is_err());
        assert!(stale_start(&host, 42, "session-a", &tr()).is_err());
    }
    assert_eq!(
        load(
            &MalformedHost(json!({"value": null})),
            42,
            None,
            "session-a",
            &tr()
        )
        .unwrap(),
        SessionOptions::default()
    );
}
