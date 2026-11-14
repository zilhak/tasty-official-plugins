//! 기존 대화의 로드 여부만 읽는다. 모델·child·새 대화를 실행하지 않는 opt-in 실서버 검사다.

use std::process::Command;
use std::time::Duration;

use crate::execution::CodexExecution;
use crate::rpc::{Client, Endpoint};

#[test]
#[ignore = "TASTY_CODEX_EXECUTION_PROBE=1 with an existing shared-server Codex conversation"]
fn a_real_proxy_works_when_only_the_caller_can_find_codex() {
    assert_eq!(
        std::env::var("TASTY_CODEX_EXECUTION_PROBE").as_deref(),
        Ok("1")
    );
    let thread = std::env::var("CODEX_THREAD_ID").expect("existing conversation");
    if let Ok(raw) = std::env::var("TASTY_CODEX_EXECUTION_PROBE_CONTEXT") {
        let execution: CodexExecution = serde_json::from_str(&raw).unwrap();
        let home = std::env::var_os("CODEX_HOME").map(Into::into);
        // GUI에서 실행한 plugin처럼 현재 PATH에서는 Codex를 실행할 수 없다.
        let old = Endpoint::Proxy {
            codex_home: home.clone(),
            execution: None,
        };
        assert!(Client::connect(&old, Duration::from_secs(10)).is_err());
        let fixed = Endpoint::Proxy {
            codex_home: home,
            execution: Some(execution),
        };
        let mut client = Client::connect(&fixed, Duration::from_secs(10)).unwrap();
        let loaded = crate::rpc::thread_loaded(&mut client, &thread);
        client.close();
        assert_eq!(loaded, Ok(true));
        return;
    }
    let captured = tasty_utils::execution::ExecutionContext::capture("codex").unwrap();
    let context = CodexExecution {
        executable: captured.executable,
        search_path: captured.search_path.into_string().unwrap(),
    };
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "execution_probe_tests::a_real_proxy_works_when_only_the_caller_can_find_codex",
            "--ignored",
            "--nocapture",
        ])
        .env("PATH", empty.path())
        .env(
            "TASTY_CODEX_EXECUTION_PROBE_CONTEXT",
            serde_json::to_string(&context).unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
