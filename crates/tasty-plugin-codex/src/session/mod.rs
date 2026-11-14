//! Tasty가 실행하는 Codex 세션: 전용 app-server와 `--remote` TUI.
//!
//! launch·spawn·respawn·reboot·복원은 모두 [`command::command_line`]으로 helper를 실행한다.
//! helper는 시작할 때 `codex.session_open`으로 새 세대를 발급받고, 그 세대의 서버 환경에
//! `TASTY_CODEX_GEN`을 넣는다. 설치된 훅은 이 값과 터미널의 surface를 함께 보내며, 플러그인은
//! surface의 현재 세대와 같을 때만 훅을 적용한다.

pub(crate) mod command;
pub(crate) mod helper;
pub(crate) mod native_args;
mod readiness;
pub(crate) mod registry;
pub(crate) mod relay;

use std::path::Path;

use serde_json::{Value, json};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

/// `codex.session_open`: helper가 자기 터미널의 새 세대를 발급받는다.
pub(crate) fn handle_open(
    root: &Path,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let surface = crate::handlers::require_u32(params, "surface", tr)?;
    let resume = match params.get("resume").and_then(Value::as_str) {
        Some(t) if crate::ids::is_uuid(t) => Some(t.to_ascii_lowercase()),
        Some(_) => {
            return Err(IpcMethodError::invalid_params(
                "resume must be a thread UUID",
            ));
        }
        None => None,
    };
    let tui_args: Vec<String> = match params.get("tui_args").and_then(Value::as_str) {
        Some(raw) => serde_json::from_str(raw)
            .map_err(|e| IpcMethodError::invalid_params(&format!("tui_args: {e}")))?,
        None => Vec::new(),
    };
    let native_args: Option<Vec<String>> = params
        .get("native_args")
        .and_then(Value::as_str)
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| IpcMethodError::invalid_params(&format!("native_args: {e}")))?;
    if let Some(args) = &native_args {
        let parsed = native_args::parse(args)
            .map_err(|e| IpcMethodError::invalid_params(&e))?
            .ok_or_else(|| IpcMethodError::invalid_params("native_args must be interactive"))?;
        if parsed.restore != *args {
            return Err(IpcMethodError::invalid_params(
                "native_args must contain only normalized restore options",
            ));
        }
    }
    command::validate_tui_args(&tui_args).map_err(|e| IpcMethodError::invalid_params(&e))?;
    let mut session = registry::open_session(root, surface, resume.as_deref(), tui_args)
        .map_err(|e| IpcMethodError::new(format!("codex session registry: {e}")))?;
    session.native_args = native_args;
    registry::write_session(root, &session).map_err(|e| IpcMethodError::new(e.to_string()))?;
    Ok(json!({
        "gen": session.generation,
        "state_dir": registry::session_dir(root, &session.generation),
        "listen": session.listen,
        "token_file": session.token_file,
    }))
}

/// 터미널에 보낼 helper 명령(제출 문자 포함).
pub(crate) fn submit_line(
    resume: Option<&str>,
    prompt_file: Option<&Path>,
    tui_args: &[String],
) -> std::io::Result<String> {
    Ok(format!(
        "{}\r",
        command::command_line(&command::helper_program()?, resume, prompt_file, tui_args)
    ))
}

/// 복원·reboot가 같은 thread를 같은 정책과 모델로 다시 여는 명령(제출 문자 없음).
pub(crate) fn restore_line(session: &registry::Session, thread: &str) -> std::io::Result<String> {
    if let Some(args) = &session.native_args {
        let mut parts = vec![
            command::sh_quote(&command::helper_program()?),
            "terminal".into(),
            "resume".into(),
            command::sh_quote(thread),
        ];
        let args = command::with_model(args.clone(), session.model.as_deref());
        parts.extend(args.iter().map(|a| command::sh_quote(a)));
        return Ok(parts.join(" "));
    }
    let args = command::with_model(session.tui_args.clone(), session.model.as_deref());
    Ok(command::command_line(
        &command::helper_program()?,
        Some(thread),
        None,
        &args,
    ))
}

/// The launch-scoped relay reports the successful thread response it delivered to its TUI.
/// This is registration, independent of native hook approval and turn completion reporting.
pub(crate) fn handle_bind<H: tasty_plugin_agent_common::host_call::HostCall>(
    host: &H,
    root: &Path,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    handle_bind_with(host, root, params, tr, &|endpoint, thread| {
        let mut client = crate::rpc::Client::connect(endpoint, std::time::Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        let loaded = crate::rpc::thread_loaded(&mut client, thread).map_err(|e| e.to_string());
        client.close();
        loaded
    })
}

fn handle_bind_with<H: tasty_plugin_agent_common::host_call::HostCall>(
    host: &H,
    root: &Path,
    params: &Value,
    tr: &Translator,
    loaded: &dyn Fn(&crate::rpc::Endpoint, &str) -> Result<bool, String>,
) -> Result<Value, IpcMethodError> {
    let surface = crate::handlers::require_u32(params, "surface", tr)?;
    let generation = params["gen"]
        .as_str()
        .filter(|g| crate::ids::is_generation(g))
        .ok_or_else(|| IpcMethodError::invalid_params("invalid generation"))?;
    let thread = params["session"]
        .as_str()
        .filter(|t| crate::ids::is_uuid(t))
        .ok_or_else(|| IpcMethodError::invalid_params("invalid thread"))?
        .to_ascii_lowercase();
    let io_error = |e: std::io::Error| IpcMethodError::new(e.to_string());
    let mut session = registry::current_session(root, surface, generation)
        .map_err(io_error)?
        .filter(|s| s.native_args.is_some())
        .ok_or_else(|| IpcMethodError::invalid_params("stale native session"))?;
    let endpoint = registry::ready_endpoint(root, &session).map_err(IpcMethodError::new)?;
    if !loaded(&endpoint, &thread).map_err(IpcMethodError::new)? {
        return Err(IpcMethodError::invalid_params(
            "selected thread is not loaded in the owned server",
        ));
    }
    if let Some(old) = &session.thread
        && old != &thread
    {
        registry::unbind_thread(root, old, generation).map_err(io_error)?;
    }
    registry::bind_thread(root, &thread, generation).map_err(io_error)?;
    session.thread = Some(thread.clone());
    session.model = params["model"].as_str().map(str::to_owned);
    registry::write_session(root, &session).map_err(io_error)?;
    let restore = restore_line(&session, &thread).map_err(io_error)?;
    for (key, value) in [("codex-session-id", thread), ("restore.command", restore)] {
        host.call(
            "surface.meta.set",
            json!({"surface_id":surface,"key":key,"value":value}),
        )
        .map_err(IpcMethodError::from)?;
    }
    Ok(json!({"host_call_failures":0}))
}

#[cfg(test)]
#[path = "native_bind_tests.rs"]
mod native_bind_tests;
