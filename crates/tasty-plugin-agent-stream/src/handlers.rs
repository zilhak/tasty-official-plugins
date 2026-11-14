//! agent_stream.* IPC 핸들러. 대화 기록은 surface별로 명시해서 추적한다.
//! 요청하지 않은 세션까지 중계하지 않도록 전체 자동 등록은 제공하지 않는다.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::registry::{
    DEFAULT_TURN_TIMEOUT_SECS, MAX_TURN_TIMEOUT_SECS, MIN_TURN_TIMEOUT_SECS, StreamRegistry,
    TurnError, new_watch,
};
use crate::resolve::{self, CLAUDE_SESSION_META_KEY, HostCall, ResolveError};
use crate::sse::hub::SseHub;
use crate::sse::server::{self, SseServer};
use crate::sse::{ConfigError, ServeConfig};

/// `poll` 의 기본/최대 반환 개수.
const POLL_DEFAULT_LIMIT: u64 = 100;
const POLL_MAX_LIMIT: u64 = 1000;

/// request_id의 바이트 상한. 턴의 모든 이벤트에 복사되므로 긴 외부 입력은 거절한다.
const MAX_REQUEST_ID_LEN: usize = 512;

type Shared = Arc<Mutex<StreamRegistry>>;

/// 잠금을 보유한 스레드가 패닉했으면 오류로 반환한다.
fn lock<'a>(
    registry: &'a Shared,
    tr: &Translator,
) -> Result<std::sync::MutexGuard<'a, StreamRegistry>, IpcMethodError> {
    registry.lock().map_err(|e| {
        IpcMethodError::new(tr.t_replace(
            "agent_stream.error.registry_poisoned",
            "{detail}",
            &e.to_string(),
        ))
    })
}

/// 대상 id의 형식을 확인한다. 누락과 잘못된 값을 구분하며 실제 존재 여부는 별도로 확인한다.
fn require_surface(params: &Value, tr: &Translator) -> Result<u32, IpcMethodError> {
    let Some(raw) = params.get("surface").or_else(|| params.get("surface_id")) else {
        return Err(IpcMethodError::invalid_params(
            tr.t("agent_stream.error.missing_surface"),
        ));
    };
    raw.as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| {
            IpcMethodError::invalid_params(&tr.t_replace(
                "agent_stream.error.invalid_surface",
                "{value}",
                &raw.to_string(),
            ))
        })
}

/// 추적하지 않는 대상이 실제로 존재하는지 확인한다.
/// 없는 대상이면 공용 오류 형식에 호출자가 사용한 메서드 이름을 넣는다.
/// 호스트 조회 자체가 실패하면 원래의 미등록 오류를 유지한다.
fn require_live_surface<H: HostCall>(
    host: &H,
    surface_id: u32,
    method: &str,
    fallback: IpcMethodError,
) -> IpcMethodError {
    if crate::resolve::surface_exists(host, surface_id) {
        return fallback;
    }
    IpcMethodError::new(tasty_utils::target::unowned_target_message(
        "surface",
        u64::from(surface_id),
        method,
    ))
}

/// 기록 경로 조회 오류를 IPC 오류로 바꾼다. 대상 부재 오류에는 내부 host 호출 대신
/// 호출자가 사용한 메서드 이름을 넣고, 다른 호스트 오류는 그대로 전달한다.
fn resolve_error_message(
    tr: &Translator,
    err: &ResolveError,
    surface_id: u32,
    method: &str,
) -> IpcMethodError {
    match err {
        ResolveError::NoSessionMeta { surface_id } => IpcMethodError::new(
            tr.t_replace(
                "agent_stream.error.no_session_meta",
                "{surface}",
                &surface_id.to_string(),
            )
            .replace("{key}", CLAUDE_SESSION_META_KEY),
        ),
        ResolveError::TranscriptRootMissing => {
            IpcMethodError::new(tr.t("agent_stream.error.transcript_root_missing"))
        }
        ResolveError::TranscriptNotFound { session_id } => IpcMethodError::new(tr.t_replace(
            "agent_stream.error.transcript_not_found",
            "{session}",
            session_id,
        )),
        ResolveError::HostCall { message } => {
            if tasty_utils::target::says_no_live_target(message, "surface", u64::from(surface_id)) {
                IpcMethodError::new(tasty_utils::target::unowned_target_message(
                    "surface",
                    u64::from(surface_id),
                    method,
                ))
            } else {
                IpcMethodError::new(message.clone())
            }
        }
    }
}

/// 지정한 surface의 대화 기록 추적을 시작한다. 세션 id가 없으면 거절한다.
/// id는 있지만 파일이 아직 없으면 awaiting_transcript로 등록해 계속 찾는다.
pub(crate) fn handle_watch<H: HostCall>(
    host: &H,
    registry: &Shared,
    tr: &Translator,
    params: Value,
) -> Result<Value, IpcMethodError> {
    let surface_id = require_surface(&params, tr)?;
    let from_start = params
        .get("from_start")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let session_id = resolve::session_id_for_surface(host, surface_id)
        .map_err(|e| resolve_error_message(tr, &e, surface_id, "agent_stream.watch"))?;

    let transcript = match resolve::transcript_path(&session_id) {
        Ok(path) => Some(path),
        Err(ResolveError::TranscriptNotFound { .. }) => None,
        Err(e) => {
            return Err(resolve_error_message(
                tr,
                &e,
                surface_id,
                "agent_stream.watch",
            ));
        }
    };

    let mut reg = lock(registry, tr)?;
    let replaced = reg.insert(new_watch(
        surface_id,
        session_id.clone(),
        transcript.clone().unwrap_or_default(),
        from_start,
    ));
    let offset = reg.watch_mut(surface_id).map(|w| w.offset()).unwrap_or(0);
    reg.save_if_dirty();
    Ok(json!({
        "surface_id": surface_id,
        "session_id": session_id,
        "transcript": transcript.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "offset": offset,
        "status": if transcript.is_some() { "tailing" } else { "awaiting_transcript" },
        "replaced": replaced,
    }))
}

/// request_id 문자열이나 숫자를 문자열로 읽는다.
/// 웹훅의 응답은 고정 ACK이므로 이벤트와 요청을 연결할 id는 호출자가 제공해야 한다.
fn require_request_id(params: &Value, tr: &Translator) -> Result<String, IpcMethodError> {
    let raw = match params.get("request_id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(IpcMethodError::invalid_params(
            tr.t("agent_stream.error.missing_request_id"),
        ));
    }
    if trimmed.len() > MAX_REQUEST_ID_LEN {
        return Err(IpcMethodError::invalid_params(&tr.t_replace(
            "agent_stream.error.request_id_too_long",
            "{max}",
            &MAX_REQUEST_ID_LEN.to_string(),
        )));
    }
    Ok(trimmed.to_string())
}

/// 추적 중인 surface의 다음 이벤트에 request_id를 붙일 턴을 연다.
/// 웹훅 순차 실행에서 claude.tell보다 먼저 호출해야 응답 기록에 id를 붙일 수 있다.
/// 턴은 기록의 turn_end 등으로 닫으며, 별도의 claude-idle 훅에는 의존하지 않는다.
pub(crate) fn handle_turn_start<H: HostCall>(
    host: &H,
    registry: &Shared,
    tr: &Translator,
    params: Value,
) -> Result<Value, IpcMethodError> {
    let surface_id = require_surface(&params, tr)?;
    let request_id = require_request_id(&params, tr)?;
    let timeout_secs = params
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TURN_TIMEOUT_SECS)
        .clamp(MIN_TURN_TIMEOUT_SECS, MAX_TURN_TIMEOUT_SECS);

    let mut reg = lock(registry, tr)?;
    reg.start_turn(
        surface_id,
        request_id.clone(),
        Duration::from_secs(timeout_secs),
    )
    .map_err(|e| {
        let msg = turn_error_message(tr, surface_id, &e);
        // 미등록인 경우에만 대상 존재 여부를 확인한다.
        match e {
            TurnError::NotWatched => {
                require_live_surface(host, surface_id, "agent_stream.turn_start", msg)
            }
            TurnError::AlreadyOpen { .. } => msg,
        }
    })?;
    Ok(json!({
        "surface_id": surface_id,
        "request_id": request_id,
        "timeout_secs": timeout_secs,
        "turn_open": true,
    }))
}

fn turn_error_message(tr: &Translator, surface_id: u32, err: &TurnError) -> IpcMethodError {
    match err {
        TurnError::NotWatched => IpcMethodError::new(tr.t_replace(
            "agent_stream.error.turn_not_watched",
            "{surface}",
            &surface_id.to_string(),
        )),
        TurnError::AlreadyOpen { request_id } => IpcMethodError::new(
            tr.t_replace(
                "agent_stream.error.turn_already_open",
                "{surface}",
                &surface_id.to_string(),
            )
            .replace("{request}", request_id),
        ),
    }
}

/// `agent_stream.unwatch` — tail 을 멈추고 종료 이벤트를 남긴다.
pub(crate) fn handle_unwatch<H: HostCall>(
    host: &H,
    registry: &Shared,
    tr: &Translator,
    params: Value,
) -> Result<Value, IpcMethodError> {
    let surface_id = require_surface(&params, tr)?;
    let mut reg = lock(registry, tr)?;
    if !reg.remove(surface_id, crate::record::REASON_UNWATCHED) {
        // 등록된 대상은 이미 종료됐어도 정리한다. 미등록인 경우에만 존재 여부를 확인한다.
        let fallback = IpcMethodError::new(tr.t_replace(
            "agent_stream.error.not_watched",
            "{surface}",
            &surface_id.to_string(),
        ));
        return Err(require_live_surface(
            host,
            surface_id,
            "agent_stream.unwatch",
            fallback,
        ));
    }
    reg.save_if_dirty();
    Ok(json!({ "surface_id": surface_id, "unwatched": true }))
}

/// `agent_stream.list` — 현재 tail 중인 대상 전부. 포커스와 무관하게 전 대상을 돌려준다.
pub(crate) fn handle_list(registry: &Shared, tr: &Translator) -> Result<Value, IpcMethodError> {
    Ok(lock(registry, tr)?.list_json())
}

/// `agent_stream.poll` — seq 커서로 수집 이벤트를 읽는다(비파괴).
pub(crate) fn handle_poll(
    registry: &Shared,
    tr: &Translator,
    params: Value,
) -> Result<Value, IpcMethodError> {
    let filter_surface = params
        .get("filter_surface")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    let after_seq = params
        .get("after_seq")
        .and_then(Value::as_i64)
        .map(|v| v.max(0) as u64)
        .unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(POLL_DEFAULT_LIMIT)
        .clamp(1, POLL_MAX_LIMIT) as usize;
    Ok(lock(registry, tr)?.poll_json(filter_surface, after_seq, limit))
}

/// SSE 엔드포인트를 연다. 기존 서버는 닫고 새 설정으로 다시 연다.
/// 처음 잠금 확인에 실패하면 기존 서버를 유지한다. 이미 서버를 바꾼 뒤에는
/// poison 상태라도 메모리 설정을 갱신하고 저장을 시도한다.
/// 디스크 쓰기에 실패하면 다음 재시작이 이전 설정을 읽을 수 있다.
pub(crate) fn handle_serve(
    registry: &Shared,
    server: &mut Option<SseServer>,
    tr: &Translator,
    params: Value,
) -> Result<Value, IpcMethodError> {
    handle_serve_with(registry, server, tr, params, server::start)
}

// The starter owns the bind operation; validation and persistence stay on this path.
fn handle_serve_with(
    registry: &Shared,
    server: &mut Option<SseServer>,
    tr: &Translator,
    params: Value,
    start: impl FnOnce(ServeConfig, Arc<SseHub>, Shared) -> Result<SseServer, String>,
) -> Result<Value, IpcMethodError> {
    let config = serve_config_from(&params, tr)?;
    let replaced = server.is_some();
    // 기존 서버를 닫기 전에 잠금을 확인해, 여기서 실패하면 서버와 설정을 그대로 둔다.
    let hub = lock(registry, tr)?.hub();
    // 같은 주소로 다시 열 수 있도록 기존 서버를 먼저 닫는다.
    if let Some(mut old) = server.take() {
        old.shutdown();
    }
    let started = match start(config.clone(), hub, registry.clone()) {
        Ok(started) => started,
        Err(e) => {
            // 기존 서버도 닫혔으므로 설정을 비우고 저장을 시도한다.
            persist_serve_config(registry, None);
            return Err(bind_failed_message(tr, &config, &e));
        }
    };
    let info = started.to_json();
    *server = Some(started);
    // 이미 연 서버의 설정을 메모리에 반영하고 저장을 시도한다.
    persist_serve_config(registry, Some(config));
    let mut info = info;
    if let Some(map) = info.as_object_mut() {
        map.insert("replaced".into(), Value::from(replaced));
    }
    Ok(info)
}

fn bind_failed_message(tr: &Translator, config: &ServeConfig, detail: &str) -> IpcMethodError {
    IpcMethodError::new(
        tr.t_replace(
            "agent_stream.error.serve_bind_failed",
            "{addr}",
            &format!("{}:{}", config.bind, config.port),
        )
        .replace("{detail}", detail),
    )
}

/// 서버 변경 뒤에는 poison 상태라도 serve 설정을 현재 값으로 교체하고 저장을 시도한다.
/// 이 복구가 다른 필드의 부분 갱신을 되돌리거나 디스크 저장 성공을 보장하지는 않는다.
fn persist_serve_config(registry: &Shared, config: Option<ServeConfig>) {
    let mut reg = match registry.lock() {
        Ok(reg) => reg,
        Err(poisoned) => {
            tracing::warn!(
                "agent-stream: recovering the poisoned registry lock to update the SSE endpoint state"
            );
            poisoned.into_inner()
        }
    };
    reg.set_serve_config(config);
    reg.save_if_dirty();
}

/// 서버를 닫고 설정을 비운 뒤 저장을 시도한다. 디스크 실패는 save_if_dirty가 기록한다.
pub(crate) fn handle_serve_stop(
    registry: &Shared,
    server: &mut Option<SseServer>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let Some(mut running) = server.take() else {
        return Err(IpcMethodError::new(
            tr.t("agent_stream.error.serve_not_running"),
        ));
    };
    running.shutdown();
    persist_serve_config(registry, None);
    Ok(json!({ "running": false, "stopped": true }))
}

/// `agent_stream.serve_info` — 엔드포인트 상태와 구독자 통계. 토큰은 싣지 않는다.
pub(crate) fn handle_serve_info(server: &Option<SseServer>) -> Result<Value, IpcMethodError> {
    match server {
        Some(running) => Ok(running.to_json()),
        None => Ok(json!({ "running": false })),
    }
}

fn serve_config_from(params: &Value, tr: &Translator) -> Result<ServeConfig, IpcMethodError> {
    // 범위 밖 포트는 미지정과 다른 오류로 알린다.
    let port = match params.get("port").and_then(Value::as_u64) {
        Some(raw) => u16::try_from(raw).map_err(|_| {
            IpcMethodError::invalid_params(&tr.t_replace(
                "agent_stream.error.serve_port_out_of_range",
                "{port}",
                &raw.to_string(),
            ))
        })?,
        // 미지정 — 아래 `validate()` 가 `PortRequired` 로 거른다.
        None => 0,
    };
    let bind = params
        .get("bind")
        .and_then(Value::as_str)
        .unwrap_or(ServeConfig::DEFAULT_BIND)
        .to_string();
    let token = params
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let config = ServeConfig { bind, port, token };
    config
        .validate()
        .map_err(|e| config_error_message(tr, &config, e))?;
    Ok(config)
}

fn config_error_message(tr: &Translator, config: &ServeConfig, err: ConfigError) -> IpcMethodError {
    match err {
        ConfigError::PortRequired => {
            IpcMethodError::invalid_params(tr.t("agent_stream.error.serve_port_required"))
        }
        ConfigError::InvalidBind => IpcMethodError::invalid_params(&tr.t_replace(
            "agent_stream.error.serve_invalid_bind",
            "{bind}",
            &config.bind,
        )),
        ConfigError::RemoteBindNeedsToken => IpcMethodError::invalid_params(&tr.t_replace(
            "agent_stream.error.serve_remote_bind_needs_token",
            "{bind}",
            &config.bind,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tasty_plugin_sdk::PluginError;

    struct StubHost {
        session: Option<&'static str>,
        /// surface.locate 응답의 exists. 기본 시험 대상은 존재한다.
        exists: bool,
    }

    /// 존재하는 surface를 반환하는 시험용 호스트.
    fn live() -> StubHost {
        StubHost {
            session: None,
            exists: true,
        }
    }

    impl HostCall for StubHost {
        fn call(&self, method: &str, _params: Value) -> Result<Value, PluginError> {
            match method {
                "surface.meta.get" => Ok(json!({ "value": self.session })),
                "surface.locate" => Ok(json!({ "exists": self.exists })),
                other => panic!("unexpected host call {other}"),
            }
        }
    }

    fn shared() -> Shared {
        Arc::new(Mutex::new(StreamRegistry::new(None)))
    }

    use crate::sse::server::test_support::ReservedEndpoint;

    /// 잠금을 보유한 스레드에서 의도적으로 패닉을 일으켜 poison 상태를 만든다.
    fn poison_registry(registry: &Shared) {
        let target = registry.clone();
        let joined = std::thread::spawn(move || {
            let _guard = target.lock().expect("lock");
            panic!("intentional: poisoning the registry lock for a regression test");
        })
        .join();
        assert!(joined.is_err(), "패닉이 나야 락이 poisoned 가 된다");
        assert!(registry.lock().is_err(), "락이 poisoned 여야 한다");
    }

    fn snapshot_serve(dir: &std::path::Path) -> Value {
        let text = std::fs::read_to_string(dir.join("watches.json")).expect("snapshot");
        serde_json::from_str::<Value>(&text).expect("json")["serve"].clone()
    }

    #[test]
    fn the_snapshot_records_the_closed_endpoint_even_when_the_lock_is_poisoned() {
        // 잠금이 poison 상태여도 서버를 닫은 뒤의 설정을 저장해야 한다.
        let dir = tempfile::tempdir().expect("tempdir");
        let registry: Shared = Arc::new(Mutex::new(StreamRegistry::new(Some(dir.path()))));
        let tr = Translator::default();
        let mut server = None;

        let reservation = ReservedEndpoint::new();
        let port = reservation.port();
        handle_serve_with(
            &registry,
            &mut server,
            &tr,
            json!({"port": port, "bind": "127.0.0.1"}),
            |config, hub, registry| reservation.start(config, hub, registry),
        )
        .expect("the first bind succeeds");
        assert_ne!(snapshot_serve(dir.path()), Value::Null);

        poison_registry(&registry);

        handle_serve_stop(&registry, &mut server, &tr).expect("stopping must not fail");
        assert!(server.is_none(), "런타임은 닫혔다");
        assert_eq!(
            snapshot_serve(dir.path()),
            Value::Null,
            "스냅샷도 닫힘이어야 한다 — 남으면 다음 재시작이 되살린다"
        );
    }

    #[test]
    fn persisting_the_serve_clause_survives_a_poisoned_lock_in_both_directions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry: Shared = Arc::new(Mutex::new(StreamRegistry::new(Some(dir.path()))));
        poison_registry(&registry);

        let config = ServeConfig {
            bind: "127.0.0.1".into(),
            port: 8787,
            token: Some("t".into()),
        };
        persist_serve_config(&registry, Some(config));
        assert_eq!(snapshot_serve(dir.path())["port"], json!(8787));

        persist_serve_config(&registry, None);
        assert_eq!(snapshot_serve(dir.path()), Value::Null);
    }

    #[test]
    fn a_poisoned_lock_before_anything_changes_is_reported_without_touching_the_endpoint() {
        // 서버 변경 전 잠금 확인에 실패하면 기존 서버·설정을 유지한다.
        let dir = tempfile::tempdir().expect("tempdir");
        let registry: Shared = Arc::new(Mutex::new(StreamRegistry::new(Some(dir.path()))));
        let tr = Translator::default();
        let mut server = None;

        let reservation = ReservedEndpoint::new();
        let first_port = reservation.port();
        handle_serve_with(
            &registry,
            &mut server,
            &tr,
            json!({"port": first_port, "bind": "127.0.0.1"}),
            |config, hub, registry| reservation.start(config, hub, registry),
        )
        .expect("the first bind succeeds");

        poison_registry(&registry);

        // Rejection must happen before the starter or the old endpoint is touched.
        let starts = std::cell::Cell::new(0);
        let err = handle_serve_with(
            &registry,
            &mut server,
            &tr,
            json!({"port": first_port, "bind": "127.0.0.1"}),
            |config, hub, registry| {
                starts.set(starts.get() + 1);
                server::start(config, hub, registry)
            },
        )
        .expect_err("a poisoned registry must be reported, not swallowed");
        assert_eq!(starts.get(), 0, "poison rejection must precede the starter");
        assert!(
            err.message.contains("registry_poisoned") || err.message.contains("poisoned"),
            "{}",
            err.message
        );

        // 옛 엔드포인트는 그대로 떠 있고 스냅샷도 그 주소를 가리킨다.
        let info = server
            .as_ref()
            .expect("the old endpoint stays up")
            .to_json();
        assert_eq!(info["port"], json!(first_port), "{info}");
        assert_eq!(snapshot_serve(dir.path())["port"], json!(first_port));

        server.take().expect("server").shutdown();
    }

    #[test]
    fn a_failed_rebind_clears_the_persisted_endpoint_so_a_restart_does_not_reopen_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry: Shared = Arc::new(Mutex::new(StreamRegistry::new(Some(dir.path()))));
        let tr = Translator::default();
        let mut server = None;

        let reservation = ReservedEndpoint::new();
        let port = reservation.port();
        handle_serve_with(
            &registry,
            &mut server,
            &tr,
            json!({"port": port, "bind": "127.0.0.1"}),
            |config, hub, registry| reservation.start(config, hub, registry),
        )
        .expect("the first bind succeeds");
        assert!(registry.lock().expect("lock").serve_config().is_some());

        // Keep a competing listener alive: production bind must fail, without a retry.
        let occupied = ReservedEndpoint::new();
        let err = handle_serve(
            &registry,
            &mut server,
            &tr,
            json!({"port": occupied.port(), "bind": "127.0.0.1"}),
        )
        .expect_err("binding an occupied address must fail");
        assert!(
            err.message.contains("serve_bind_failed")
                || err
                    .message
                    .contains(&format!("127.0.0.1:{}", occupied.port())),
            "{}",
            err.message
        );

        // 새 bind가 실패하면 서버와 저장 설정 모두 닫힘 상태여야 한다.
        assert!(server.is_none(), "the runtime endpoint is closed");
        assert!(registry.lock().expect("lock").serve_config().is_none());
        let snapshot: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("watches.json")).expect("snapshot"),
        )
        .expect("json");
        assert_eq!(snapshot["serve"], Value::Null, "{snapshot}");
    }

    #[test]
    fn missing_or_zero_port_is_rejected_before_starting() {
        for params in [json!({}), json!({"port": 0})] {
            let starts = std::cell::Cell::new(0);
            let err = handle_serve_with(
                &shared(),
                &mut None,
                &Translator::default(),
                params.clone(),
                |config, hub, registry| {
                    starts.set(starts.get() + 1);
                    server::start(config, hub, registry)
                },
            )
            .expect_err("an explicit nonzero port is required");
            assert_eq!(err.code, -32602);
            assert_eq!(starts.get(), 0);
            let public_err = handle_serve(&shared(), &mut None, &Translator::default(), params)
                .expect_err("the public path also rejects zero");
            assert_eq!(err.code, public_err.code);
        }
    }

    #[test]
    fn a_port_outside_the_u16_range_reports_its_real_cause() {
        let err = handle_serve(
            &shared(),
            &mut None,
            &Translator::default(),
            json!({"port": 70000}),
        )
        .expect_err("must reject");
        assert_eq!(err.code, -32602);
        assert!(
            err.message.contains("serve_port_out_of_range") || err.message.contains("70000"),
            "{}",
            err.message
        );
        assert!(
            !err.message.contains("serve_port_required"),
            "범위 밖 포트를 미지정으로 안내해서는 안 된다: {}",
            err.message
        );
    }

    #[test]
    fn watch_without_a_target_surface_is_rejected() {
        let host = StubHost {
            session: Some("s"),
            exists: true,
        };
        let err = handle_watch(&host, &shared(), &Translator::default(), json!({}))
            .expect_err("must reject");
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("missing_surface") || err.message.contains("--surface"));
    }

    #[test]
    fn watch_without_session_meta_is_rejected_loudly() {
        let host = StubHost {
            session: None,
            exists: true,
        };
        let registry = shared();
        let err = handle_watch(
            &host,
            &registry,
            &Translator::default(),
            json!({ "surface": 7 }),
        )
        .expect_err("must reject");
        // Translator::default() 는 키를 그대로 돌려주므로 키가 보이면 그 분기다.
        assert!(err.message.contains("no_session_meta"), "{}", err.message);
        assert!(
            !registry.lock().expect("lock").is_watched(7),
            "a rejected watch must not leave a registration behind"
        );
    }

    #[test]
    fn unwatch_of_an_unknown_surface_is_an_error_not_a_silent_ok() {
        let err = handle_unwatch(
            &live(),
            &shared(),
            &Translator::default(),
            json!({ "surface": 3 }),
        )
        .expect_err("must reject");
        assert!(err.message.contains("not_watched"), "{}", err.message);
    }

    #[test]
    fn poll_clamps_the_limit_and_defaults_the_cursor() {
        let registry = shared();
        {
            let mut reg = registry.lock().expect("lock");
            for _ in 0..5 {
                reg.push_event(1, "s", crate::record::StreamEvent::turn_end("end_turn"));
            }
        }
        let all = handle_poll(&registry, &Translator::default(), json!({})).expect("poll");
        assert_eq!(all["events"].as_array().expect("array").len(), 5);

        let capped = handle_poll(&registry, &Translator::default(), json!({ "limit": 99999 }))
            .expect("poll");
        assert_eq!(capped["events"].as_array().expect("array").len(), 5);

        let one = handle_poll(
            &registry,
            &Translator::default(),
            json!({ "limit": 1, "after_seq": 2 }),
        )
        .expect("poll");
        let events = one["events"].as_array().expect("array");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["seq"], 3);
    }

    #[test]
    fn poll_filter_surface_is_not_auto_filled_from_the_surface_key() {
        let registry = shared();
        {
            let mut reg = registry.lock().expect("lock");
            reg.push_event(1, "s", crate::record::StreamEvent::turn_end("end_turn"));
            reg.push_event(2, "s", crate::record::StreamEvent::turn_end("end_turn"));
        }
        // CLI 가 TASTY_SURFACE_ID 로 자동 주입하는 `surface`/`surface_id` 키는 poll 의
        // 필터로 쓰이지 않는다 — 필터는 오직 `filter_surface` 다.
        let unfiltered = handle_poll(
            &registry,
            &Translator::default(),
            json!({ "surface": 1, "surface_id": 1 }),
        )
        .expect("poll");
        assert_eq!(unfiltered["events"].as_array().expect("array").len(), 2);

        let filtered = handle_poll(
            &registry,
            &Translator::default(),
            json!({ "filter_surface": 2 }),
        )
        .expect("poll");
        assert_eq!(filtered["events"].as_array().expect("array").len(), 1);
    }

    #[test]
    fn list_is_empty_before_anything_is_watched() {
        let list = handle_list(&shared(), &Translator::default()).expect("list");
        assert!(list["watches"].as_array().expect("array").is_empty());
    }

    fn watched_shared(surface_id: u32) -> Shared {
        let registry = shared();
        registry.lock().expect("lock").insert(new_watch(
            surface_id,
            "s".into(),
            std::path::PathBuf::new(),
            false,
        ));
        registry
    }

    #[test]
    fn turn_start_opens_a_turn_on_a_watched_surface() {
        let registry = watched_shared(3);
        let out = handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3, "request_id": "abc" }),
        )
        .expect("opens");
        assert_eq!(out["surface_id"], 3);
        assert_eq!(out["request_id"], "abc");
        assert_eq!(out["timeout_secs"], DEFAULT_TURN_TIMEOUT_SECS);
        assert!(registry.lock().expect("lock").has_open_turn(3));
    }

    #[test]
    fn turn_start_accepts_a_numeric_request_id_as_a_string() {
        // `${body.request_id}` 는 전체 플레이스홀더면 타입을 보존한다 — 숫자로 와도 받는다.
        let registry = watched_shared(3);
        let out = handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3, "request_id": 42 }),
        )
        .expect("opens");
        assert_eq!(out["request_id"], "42");
    }

    #[test]
    fn turn_start_without_a_request_id_is_rejected() {
        let registry = watched_shared(3);
        let err = handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3 }),
        )
        .expect_err("must reject");
        assert_eq!(err.code, -32602);
        assert!(
            err.message.contains("missing_request_id"),
            "{}",
            err.message
        );
        assert!(!registry.lock().expect("lock").has_open_turn(3));
    }

    #[test]
    fn turn_start_rejects_an_oversized_request_id() {
        let registry = watched_shared(3);
        let huge = "x".repeat(MAX_REQUEST_ID_LEN + 1);
        let err = handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3, "request_id": huge }),
        )
        .expect_err("must reject an oversized request_id");
        assert_eq!(err.code, -32602);
        assert!(
            err.message.contains("request_id_too_long"),
            "{}",
            err.message
        );
        // 거절한 요청으로 턴을 만들지 않는다.
        assert!(!registry.lock().expect("lock").has_open_turn(3));
    }

    #[test]
    fn turn_start_accepts_a_request_id_at_the_cap() {
        let registry = watched_shared(3);
        let at_cap = "x".repeat(MAX_REQUEST_ID_LEN);
        handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3, "request_id": at_cap }),
        )
        .expect("a request_id exactly at the cap is accepted");
        assert!(registry.lock().expect("lock").has_open_turn(3));
    }

    /// 존재하지 않는 대상을 호스트의 공용 오류로 거절하는 stub.
    struct RejectingHost;

    impl HostCall for RejectingHost {
        fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
            let sid = params
                .get("surface_id")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            match method {
                // 두 조회 모두 공용 대상 부재 오류를 사용한다.
                "surface.locate" | "surface.meta.get" => Err(PluginError::HostCall {
                    method: method.to_string(),
                    message: tasty_utils::target::unowned_target_message("surface", sid, method),
                    code: None,
                }),
                other => panic!("unexpected host call {other}"),
            }
        }
    }

    /// 대상 부재 오류는 내부 조회가 아닌 호출자의 메서드 이름을 알려야 한다.
    #[test]
    fn watch_names_the_method_the_caller_called() {
        let tr = Translator::default();
        let err = handle_watch(
            &RejectingHost,
            &shared(),
            &tr,
            json!({ "surface": 424_242 }),
        )
        .expect_err("없는 surface 는 거절");
        assert_eq!(
            err.message,
            tasty_utils::target::unowned_target_message("surface", 424_242, "agent_stream.watch"),
            "내부 호출 이름이 새어 나왔다: {}",
            err.message
        );
    }

    /// 잘못된 id와 누락한 id를 다른 오류로 알려야 한다.
    #[test]
    fn a_surface_value_out_of_u32_range_is_not_reported_as_missing() {
        let tr = Translator::default();
        let absent = require_surface(&json!({}), &tr).expect_err("키 없음은 거절");
        let too_big = require_surface(&json!({ "surface": 999_999_999_999u64 }), &tr)
            .expect_err("범위 초과는 거절");
        assert!(
            absent.message.contains("missing_surface"),
            "키가 없으면 missing: {}",
            absent.message
        );
        assert!(
            too_big.message.contains("invalid_surface"),
            "값이 있으면 invalid: {}",
            too_big.message
        );
        assert_ne!(
            absent.message, too_big.message,
            "누락한 값과 잘못된 값은 다른 오류로 안내해야 한다"
        );
        // 양방향 — 정상 값은 그대로 통과한다.
        assert_eq!(
            require_surface(&json!({ "surface": 7 }), &tr).expect("정상"),
            7
        );
    }

    /// 존재하지 않는 대상은 단순 미등록과 구분한다.
    #[test]
    fn a_missing_surface_is_not_reported_as_merely_unwatched() {
        let tr = Translator::default();

        let dead_turn = handle_turn_start(
            &RejectingHost,
            &shared(),
            &tr,
            json!({ "surface": 424_242, "request_id": "abc" }),
        )
        .expect_err("없는 surface 는 거절");
        let dead_unwatch = handle_unwatch(
            &RejectingHost,
            &shared(),
            &tr,
            json!({ "surface": 424_242 }),
        )
        .expect_err("없는 surface 는 거절");
        // 공용 오류의 정확한 출력과 메서드 이름을 확인한다.
        assert_eq!(
            dead_turn.message,
            tasty_utils::target::unowned_target_message(
                "surface",
                424_242,
                "agent_stream.turn_start"
            )
        );
        assert_eq!(
            dead_unwatch.message,
            tasty_utils::target::unowned_target_message("surface", 424_242, "agent_stream.unwatch")
        );

        // 존재하는 대상의 미등록 오류는 그대로 유지한다.
        let live_turn = handle_turn_start(
            &live(),
            &shared(),
            &tr,
            json!({ "surface": 3, "request_id": "abc" }),
        )
        .expect_err("watch 안 했으면 거절");
        let live_unwatch = handle_unwatch(&live(), &shared(), &tr, json!({ "surface": 3 }))
            .expect_err("watch 안 했으면 거절");
        assert!(
            live_turn.message.contains("turn_not_watched"),
            "{}",
            live_turn.message
        );
        assert!(
            live_unwatch.message.contains("not_watched"),
            "{}",
            live_unwatch.message
        );
    }

    #[test]
    fn turn_start_on_an_unwatched_surface_is_rejected_loudly() {
        let err = handle_turn_start(
            &live(),
            &shared(),
            &Translator::default(),
            json!({ "surface": 3, "request_id": "abc" }),
        )
        .expect_err("must reject");
        assert!(err.message.contains("turn_not_watched"), "{}", err.message);
    }

    #[test]
    fn turn_start_rejects_an_overlapping_turn() {
        let registry = watched_shared(3);
        handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3, "request_id": "first" }),
        )
        .expect("first opens");
        let err = handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3, "request_id": "second" }),
        )
        .expect_err("overlap rejected");
        assert!(err.message.contains("turn_already_open"), "{}", err.message);
    }

    #[test]
    fn turn_start_clamps_the_timeout_into_range() {
        let registry = watched_shared(3);
        let out = handle_turn_start(
            &live(),
            &registry,
            &Translator::default(),
            json!({ "surface": 3, "request_id": "abc", "timeout_secs": 1 }),
        )
        .expect("opens");
        assert_eq!(out["timeout_secs"], MIN_TURN_TIMEOUT_SECS);
    }
}
