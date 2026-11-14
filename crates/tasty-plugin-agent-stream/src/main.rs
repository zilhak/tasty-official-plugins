// Windows의 파일 식별자 조회만 Win32 FFI를 허용하고 나머지 unsafe 코드는 금지한다.
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(windows, deny(unsafe_code))]

//! 에이전트 세션 기록을 읽어 text·thinking·tool_use·turn_end 이벤트로 제공한다.
//! 현재 Claude Code 기록을 지원하며 CLI·IPC 조회와 SSE 구독을 제공한다.
//! 외부 요청을 받는 웹훅은 호스트의 별도 기능이다.
//! 세션 id는 호스트 IPC로 surface 메타데이터를 읽어 확인한다.

mod handlers;
mod pump;
mod record;
mod registry;
mod resolve;
mod sse;
mod tail;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use registry::StreamRegistry;
use serde_json::Value;
use sse::server::SseServer;
use tasty_plugin_sdk::{
    BusHandle, HostHandle, IpcMethodCtx, IpcMethodError, Plugin, PluginEnv, SurfaceCreateCtx,
    SurfaceResult, i18n::Translator,
};

const PLUGIN_ID: &str = "com.tasty.agent-stream";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

struct AgentStreamPlugin {
    /// watch 대상 · 수집 이벤트 버퍼. IPC 핸들러(worker 스레드)와 tail 스레드가 공유한다.
    registry: Arc<Mutex<StreamRegistry>>,
    /// 사람이 읽는 IPC 에러 문자열 번역용. `main()` 에서 1 회 로드해 재사용한다.
    translator: Translator,
    /// 떠 있는 SSE 엔드포인트. dispatch 스레드에서만 만지므로 별도 락이 필요 없다.
    server: Option<SseServer>,
}

impl AgentStreamPlugin {
    fn new(data_dir: Option<PathBuf>, translator: Translator) -> Self {
        let mut registry = StreamRegistry::new(data_dir.as_deref());
        // 재시작 뒤에도 기존 추적 대상을 복원한다.
        registry.restore();
        Self {
            registry: Arc::new(Mutex::new(registry)),
            translator,
            server: None,
        }
    }
}

impl AgentStreamPlugin {
    /// 저장된 SSE 설정으로 서버를 다시 연다. bind 실패는 로그에 남기고 수집을 계속한다.
    /// 서버 없이도 poll로 이벤트를 읽을 수 있다.
    fn restore_endpoint(&mut self) {
        self.restore_endpoint_with(sse::server::start);
    }

    fn restore_endpoint_with(
        &mut self,
        start: impl FnOnce(
            sse::ServeConfig,
            Arc<sse::hub::SseHub>,
            Arc<Mutex<StreamRegistry>>,
        ) -> Result<SseServer, String>,
    ) {
        let reg = match self.registry.lock() {
            Ok(reg) => reg,
            Err(e) => {
                tracing::warn!(
                    "agent-stream: registry lock poisoned at start — the SSE endpoint is not reopened: {e}"
                );
                return;
            }
        };
        let Some(config) = reg.serve_config() else {
            return;
        };
        let hub = reg.hub();
        drop(reg);
        match start(config.clone(), hub, self.registry.clone()) {
            Ok(server) => self.server = Some(server),
            Err(e) => tracing::warn!(
                "agent-stream: cannot reopen the SSE endpoint on {}:{} after restart: {e} — collection continues, subscribe again after `tasty agent-stream serve`",
                config.bind,
                config.port
            ),
        }
    }
}

impl Plugin for AgentStreamPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn create_surface(&mut self, _ctx: SurfaceCreateCtx) -> SurfaceResult {
        // 화면을 그리지 않으므로 매니페스트에 surface_kind를 선언하지 않는다.
        SurfaceResult::default()
    }

    fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> Result<Value, IpcMethodError> {
        let IpcMethodCtx {
            method,
            params,
            host,
            ..
        } = ctx;
        match method.as_str() {
            "agent_stream.watch" => {
                handlers::handle_watch(&host, &self.registry, &self.translator, params)
            }
            "agent_stream.turn_start" => {
                handlers::handle_turn_start(&host, &self.registry, &self.translator, params)
            }
            "agent_stream.unwatch" => {
                handlers::handle_unwatch(&host, &self.registry, &self.translator, params)
            }
            "agent_stream.list" => handlers::handle_list(&self.registry, &self.translator),
            "agent_stream.poll" => handlers::handle_poll(&self.registry, &self.translator, params),
            "agent_stream.serve" => {
                handlers::handle_serve(&self.registry, &mut self.server, &self.translator, params)
            }
            "agent_stream.serve_stop" => {
                handlers::handle_serve_stop(&self.registry, &mut self.server, &self.translator)
            }
            "agent_stream.serve_info" => handlers::handle_serve_info(&self.server),
            other => Err(IpcMethodError::not_found(other)),
        }
    }

    fn on_start(&mut self, host: HostHandle, _bus: BusHandle) {
        self.restore_endpoint();
        let registry = self.registry.clone();
        // 수집 스레드를 시작하지 못하면 이 플러그인 프로세스를 종료한다. 호스트와는 별도 프로세스다.
        std::thread::Builder::new()
            .name("agent-stream-tail".into())
            .spawn(move || pump::tail_loop(registry, host))
            .expect("spawn agent-stream-tail thread");
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    // 환경 정보가 없으면 번역은 키로 표시하고 데이터 디렉터리에는 쓰지 않는다.
    let env = PluginEnv::load().ok();
    let data_dir = env.as_ref().and_then(|e| e.data_dir.clone());
    let translator = env
        .as_ref()
        .map(Translator::from_plugin_env)
        .unwrap_or_default();
    tasty_plugin_sdk::run(AgentStreamPlugin::new(data_dir, translator))
}

#[cfg(test)]
mod tests {
    use super::*;

    use sse::server::test_support::ReservedEndpoint;

    #[test]
    fn a_persisted_endpoint_is_reopened_on_the_same_address_after_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reservation = ReservedEndpoint::new();
        let port = reservation.port();

        // 1 회차: 설정을 영속한다. (예약 리스너를 쥔 채라 이 사이 포트를 남이 못 가져간다.)
        {
            let mut reg = StreamRegistry::new(Some(dir.path()));
            reg.set_serve_config(Some(sse::ServeConfig {
                bind: "127.0.0.1".into(),
                port,
                token: Some("t".into()),
            }));
            reg.save_if_dirty();
        }

        // Restore the persisted address using the listener that still owns that address.
        let mut plugin =
            AgentStreamPlugin::new(Some(dir.path().to_path_buf()), Translator::default());
        let starts = std::cell::Cell::new(0);
        plugin.restore_endpoint_with(|config, hub, registry| {
            starts.set(starts.get() + 1);
            assert_eq!(config.port, port, "restore uses the saved address");
            assert_eq!(config.token.as_deref(), Some("t"));
            reservation.start(config, hub, registry)
        });
        assert_eq!(starts.get(), 1);
        let info = plugin
            .server
            .as_ref()
            .expect("the endpoint is reopened")
            .to_json();
        assert_eq!(
            info["url"],
            Value::from(format!("http://127.0.0.1:{port}/events")),
            "구독자가 다시 연결할 수 있도록 저장한 주소를 복원해야 한다"
        );
        assert_eq!(info["port"], Value::from(port));
        assert_eq!(
            plugin
                .registry
                .lock()
                .expect("lock")
                .serve_config()
                .unwrap()
                .port,
            port,
        );
        // 토큰은 공개 뷰에 실리지 않는다.
        assert!(!info.to_string().contains("\"t\""), "{info}");
        plugin.server.take().expect("server").shutdown();
    }

    #[test]
    fn an_occupied_restore_address_is_not_replaced_or_forgotten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let occupied = ReservedEndpoint::new();
        {
            let mut registry = StreamRegistry::new(Some(dir.path()));
            registry.set_serve_config(Some(sse::ServeConfig {
                bind: "127.0.0.1".into(),
                port: occupied.port(),
                token: None,
            }));
            registry.save_if_dirty();
        }
        let saved = std::fs::read(dir.path().join("watches.json")).unwrap();
        let mut plugin =
            AgentStreamPlugin::new(Some(dir.path().to_path_buf()), Translator::default());
        // Use the production starter while another owned listener holds the address.
        plugin.restore_endpoint();
        assert!(
            plugin.server.is_none(),
            "restore must not fall back to another port"
        );
        assert_eq!(
            plugin.registry.lock().unwrap().serve_config().unwrap().port,
            occupied.port(),
        );
        assert_eq!(
            std::fs::read(dir.path().join("watches.json")).unwrap(),
            saved
        );
    }

    #[test]
    fn no_persisted_endpoint_means_nothing_is_opened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut plugin =
            AgentStreamPlugin::new(Some(dir.path().to_path_buf()), Translator::default());
        plugin.restore_endpoint();
        assert!(plugin.server.is_none());
    }
}
