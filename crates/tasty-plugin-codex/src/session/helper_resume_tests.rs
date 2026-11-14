//! 실제 Codex로 원격 TUI 재개의 정책 적용을 확인한다.
//!
//! 사용자의 기존 로그인을 쓰므로 `TASTY_CODEX_NORMAL_HOME_E2E=1`로만 실행한다. 모델은 호출하지
//! 않는다. 시험이 만든 thread 하나만 쓰고 끝에 삭제한다. 서버와 TUI 환경에서는 `TASTY_*` 변수를
//! 지운다. TUI 실행에는 util-linux `script`로 만든 가상 터미널을 쓴다.
//!
//! 사용자 홈의 `config.toml`에 흔적을 남기지 않는다. Codex 0.154 app-server는 cwd를 명시한
//! `thread/start`에서 그 프로젝트의 trust가 없고 유효 권한이 cwd에 쓸 수 있으면 임시 cwd를
//! `trusted`로 영속한다(`thread_processor.rs`의 `thread_start_task`). `thread/resume`과 원격 TUI
//! 재개에는 이 기록이 없다. 그래서 thread는 read-only로 시작하고, 재개 때 다른 승인·sandbox를
//! 요청한다. 본문이나 thread 정리가 panic해도 시험 끝에서 `config.toml`이 시험 전과 같은지
//! (sha256, 임시 cwd 키 부재) 판정한다. 시험 프로세스가 강제 종료되는 경우는 다루지 않는다.
use super::*;
use crate::rpc::Client;

/// cwd에 쓸 수 없는 sandbox로 시작해야 서버가 임시 cwd를 trust로 영속하지 않는다.
const THREAD_START_APPROVAL: &str = "on-request";
const THREAD_START_SANDBOX: &str = "read-only";
/// 재개 때 요청하는 정책. 승인과 sandbox 모두 시작 정책과 다르다.
const RESUME_APPROVAL: &str = "never";
const RESUME_SANDBOX: &str = "workspace-write";

/// 사용자 Codex 홈의 `config.toml`. 없으면 None.
fn user_config() -> (PathBuf, Option<Vec<u8>>) {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".codex"));
    let path = home.join("config.toml");
    let bytes = std::fs::read(&path).ok();
    (path, bytes)
}

fn digest(bytes: &Option<Vec<u8>>) -> String {
    use sha2::Digest;
    match bytes {
        Some(b) => format!("{:x}", sha2::Sha256::digest(b)),
        None => "absent".into(),
    }
}

/// 시험 중 실패해도 소유 서버를 남기지 않는다. spawn 직후부터 감싼다.
struct ServerGuard(Option<OwnedServer>);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(mut server) = self.0.take() {
            server.stop();
        }
    }
}

fn strip_tasty_env(cmd: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("TASTY_") {
            cmd.env_remove(&key);
        }
    }
    cmd.env_remove("CODEX_THREAD_ID");
}

/// helper와 같은 명령으로 소유 서버를 띄우고 준비된 URL을 돌려준다. 준비 대기 중 실패하면 guard가
/// 서버를 끝낸다.
fn start_server(dir: &Path, tag: &str) -> (ServerGuard, String) {
    let opened = Opened {
        generation: "0123456789abcdef".into(),
        state_dir: dir.into(),
        listen: format!("unix://{}/{tag}.sock", dir.display()),
        token_file: None,
    };
    let log_path = dir.join(format!("server-{tag}.log"));
    let log = std::fs::File::create(&log_path).unwrap();
    let mut cmd = server_command(&opened);
    strip_tasty_env(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let server = ServerGuard(Some(OwnedServer::spawn(cmd).unwrap()));
    let mut probe = unix_ready(&opened.listen);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(url) = probe(&log_path) {
            return (server, url);
        }
        assert!(
            Instant::now() < deadline,
            "not ready: {}",
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
        std::thread::sleep(READY_POLL);
    }
}

/// TUI를 가상 터미널에서 `secs`초 동안 실행한다. (종료 코드, 화면 기록)을 돌려준다.
/// 기한까지 살아 있으면 종료 코드는 timeout의 124다.
fn run_tui(dir: &Path, tag: &str, tui: &Command, secs: u32) -> (Option<i32>, String) {
    let mut line = super::super::command::sh_quote(&tui.get_program().to_string_lossy());
    for arg in tui.get_args() {
        line.push(' ');
        line.push_str(&super::super::command::sh_quote(&arg.to_string_lossy()));
    }
    let typescript = dir.join(format!("tui-{tag}.typescript"));
    let mut cmd = Command::new("timeout");
    cmd.arg(secs.to_string())
        .args(["script", "-q", "-e", "-c", &line])
        .arg(&typescript)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    strip_tasty_env(&mut cmd);
    let status = cmd.status().unwrap();
    let screen =
        String::from_utf8_lossy(&std::fs::read(&typescript).unwrap_or_default()).into_owned();
    (status.code(), screen)
}

/// 실행 중 설정을 다른 연결에서 읽는다. 재정의 없이 재개하면 로드된 thread의 설정을 돌려준다.
fn active_policy(url: &str, thread: &str) -> (String, String) {
    let endpoint = super::super::registry::parse_endpoint(url, None).unwrap();
    let mut client = Client::connect(&endpoint, Duration::from_secs(10)).unwrap();
    let reply = client
        .call(
            "thread/resume",
            serde_json::json!({ "threadId": thread, "excludeTurns": true }),
        )
        .unwrap();
    client
        .call(
            "thread/unsubscribe",
            serde_json::json!({ "threadId": thread }),
        )
        .unwrap();
    client.close();
    (
        reply["approvalPolicy"].as_str().unwrap_or_default().into(),
        reply["sandbox"]["type"].as_str().unwrap_or_default().into(),
    )
}

/// rt-f25-1 E8 회귀: 재개 TUI에 정책 인자를 주면 0.154가 시작을 거부한다. helper는 소유 서버의
/// `thread/resume`에 정책을 적용하고 구독을 유지한 채 정책 없는 TUI를 붙인다. TUI가 붙은 뒤에도
/// 서버의 실행 설정이 요청한 정책이어야 한다.
#[test]
#[ignore = "실제 codex와 사용자의 기존 로그인을 쓴다. TASTY_CODEX_NORMAL_HOME_E2E=1로만 실행한다"]
fn a_resumed_remote_tui_keeps_the_policy_the_helper_applied() {
    assert_eq!(
        std::env::var("TASTY_CODEX_NORMAL_HOME_E2E").as_deref(),
        Ok("1")
    );
    let (config_path, config_before) = user_config();
    let dir = tempfile::tempdir().unwrap();
    let evidence = std::env::var_os("TASTY_CODEX_EVIDENCE").map(PathBuf::from);
    let mut created: Option<(String, String)> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_probe(dir.path(), evidence.as_deref(), &mut created)
    }));
    // 본문이 thread를 지우지 못하고 끝났으면 여기서 지운다. 재개 서버가 이미 끝났다면 삭제용 서버를
    // 다시 띄운다. 이 정리의 실패도 잡아 아래 config 판정을 건너뛰지 않는다.
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some((thread, url)) = created.as_ref() else {
            return;
        };
        let mut owned = None;
        let url = match super::super::registry::parse_endpoint(url, None)
            .ok()
            .filter(|e| Client::connect(e, Duration::from_secs(5)).is_ok())
        {
            Some(_) => url.clone(),
            None => {
                let (server, url) = start_server(dir.path(), "c");
                owned = Some(server);
                url
            }
        };
        let endpoint = super::super::registry::parse_endpoint(&url, None).unwrap();
        let mut cleanup = Client::connect(&endpoint, Duration::from_secs(10)).unwrap();
        cleanup
            .call("thread/delete", serde_json::json!({ "threadId": thread }))
            .unwrap();
        cleanup.close();
        drop(owned);
    }));
    let config_after = std::fs::read(&config_path).ok();
    let leaked = config_after
        .as_deref()
        .is_some_and(|b| String::from_utf8_lossy(b).contains(&*dir.path().to_string_lossy()));
    if let Some(path) = &evidence {
        let mut record: serde_json::Value = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        record["config"] = serde_json::json!({
            "before": digest(&config_before),
            "after": digest(&config_after),
            "temp_cwd": dir.path(),
            "temp_cwd_in_config": leaked,
        });
        if let Err(e) = std::fs::write(path, record.to_string()) {
            tracing::warn!("resume probe evidence {}: {e}", path.display());
        }
    }
    // 본문이나 정리가 실패했어도 config 흔적을 먼저 판정한다. 잡은 panic의 메시지는 panic 때 이미
    // 출력됐다. 판정이 통과하면 본문, 정리 순으로 실패를 다시 일으킨다.
    assert!(
        !leaked,
        "임시 cwd가 사용자 config에 남았다: {}",
        dir.path().display()
    );
    assert_eq!(
        digest(&config_after),
        digest(&config_before),
        "사용자 config.toml이 시험 중 바뀌었다"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    if let Err(panic) = cleanup {
        std::panic::resume_unwind(panic);
    }
}

/// `created`에는 만든 thread와 재개 서버 주소를 기록해 실패해도 정리할 수 있게 한다.
fn run_probe(dir: &Path, evidence: Option<&Path>, created: &mut Option<(String, String)>) {
    // 저장된 thread를 하나 만든다. 첫 정책은 재개 때 요청할 정책과 다르게 둔다.
    let (mut first, url) = start_server(dir, "a");
    let endpoint = super::super::registry::parse_endpoint(&url, None).unwrap();
    let mut client = Client::connect(&endpoint, Duration::from_secs(10)).unwrap();
    let started = client
        .call(
            "thread/start",
            serde_json::json!({
                "cwd": dir,
                "approvalPolicy": THREAD_START_APPROVAL,
                "sandbox": THREAD_START_SANDBOX,
            }),
        )
        .unwrap();
    let thread = started["thread"]["id"].as_str().unwrap().to_string();
    *created = Some((thread.clone(), url.clone()));
    client
        .call(
            "thread/inject_items",
            serde_json::json!({ "threadId": thread, "items": [{
                "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "tasty resume probe" }],
            }]}),
        )
        .unwrap();
    client.close();
    if let Some(mut server) = first.0.take() {
        server.stop();
    }

    // 새 세대처럼 새 서버에서 재개한다.
    let (_server, url) = start_server(dir, "b");
    *created = Some((thread.clone(), url.clone()));
    let args = Args {
        resume: Some(thread.clone()),
        tui_args: ["-a", RESUME_APPROVAL, "-s", RESUME_SANDBOX]
            .map(String::from)
            .to_vec(),
        ..Args::default()
    };
    // 원인: 정책 인자를 준 원격 재개 TUI는 시작을 거부한다.
    let mut old = crate::rpc::codex_command().unwrap();
    old.args([
        "resume",
        "--remote",
        &url,
        "-c",
        "check_for_update_on_startup=false",
    ])
    .args(["-a", RESUME_APPROVAL, "-s", RESUME_SANDBOX, &thread]);
    let (old_rc, old_screen) = run_tui(dir, "old", &old, 15);
    assert_eq!(old_rc, Some(1), "{old_screen}");
    assert!(
        old_screen.contains("Permission overrides are not supported when resuming a remote task"),
        "{old_screen}"
    );

    // 수정: helper가 정책을 적용하고 구독을 유지한 채 정책 없는 TUI를 붙인다.
    let prepared = prepare_resume(&url, None, &args).unwrap();
    assert_eq!(prepared.resumed.as_deref(), Some(thread.as_str()));
    let tui = tui_command(&url, None, &args, None);
    let (rc, screen) = run_tui(dir, "new", &tui, 10);
    assert_eq!(rc, Some(124), "TUI가 기한까지 살아 있어야 한다: {screen}");
    assert!(!screen.contains("Permission overrides"), "{screen}");
    let active = active_policy(&url, &thread);
    assert_eq!(active, (RESUME_APPROVAL.into(), "workspaceWrite".into()));
    if let Some(path) = evidence {
        let record = serde_json::json!({
            "thread": thread,
            "start": [THREAD_START_APPROVAL, THREAD_START_SANDBOX],
            "resume": [RESUME_APPROVAL, RESUME_SANDBOX],
            "old_tui_rc": old_rc,
            "new_tui_rc": rc,
            "active_after_tui": [active.0, active.1],
        });
        std::fs::write(path, record.to_string()).unwrap();
    }
    // thread 삭제는 이 서버로 한다. 구독은 그 뒤 서버 종료와 함께 끝난다.
    let endpoint = super::super::registry::parse_endpoint(&url, None).unwrap();
    let mut cleanup = Client::connect(&endpoint, Duration::from_secs(10)).unwrap();
    cleanup
        .call("thread/delete", serde_json::json!({ "threadId": thread }))
        .unwrap();
    cleanup.close();
    *created = None;
    drop(prepared.hold);
}
