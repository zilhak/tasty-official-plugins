#![forbid(unsafe_code)]

//! Claude Code의 기동, 훅, 오류 감시와 텔레메트리를 제공한다.
//! 자식 터미널 관리는 호스트의 `terminal.*` IPC에 맡긴다.
//! 호스트 코드에 직접 의존하지 않고 `tasty-plugin-sdk`로 통신한다.

mod auto_resume;
mod background_tasks;
mod checklist;
mod codex_return;
mod error_scan;
mod gate;
mod handlers;
mod hook;
mod install;
mod monitor_tasks;
mod notice;
mod notifications;
mod notify_mod;
mod profile;
mod profile_attach;
mod profile_merge;
mod quiet_notice;
mod reboot;
mod remote;
mod retrack;
mod session_options;
mod session_start;
mod state;
mod stop_pairing;
mod task_turn;
mod task_watch;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use error_scan::{ErrorScanner, scan_target_is_alive};
use handlers::*;
use serde_json::{Value, json};
use state::ClaudeState;
use tasty_plugin_sdk::{
    HostHandle, IpcMethodCtx, IpcMethodError, Plugin, PluginEnv, SurfaceCreateCtx, SurfaceResult,
    i18n::Translator,
};

const PLUGIN_ID: &str = "com.tasty.claude";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 폴링 사이의 대기 시간. 한 회차의 IPC 처리 시간은 별도로 걸린다.
const ERROR_SCAN_INTERVAL: Duration = Duration::from_millis(800);

/// 보류한 Stop 의 시간 초과를 확인하는 간격. 확정이 이만큼 늦어질 수 있다.
const STOP_PAIRING_TICK: Duration = Duration::from_millis(250);

struct ClaudePlugin {
    /// 훅에서 사용하는 Claude 실행 시간 기록. 자식 목록은 호스트가 관리한다.
    state: Arc<Mutex<ClaudeState>>,
    /// 게이트가 붙은 Stop 과 그 판정의 짝. `claude-stop-pairing` 스레드가 시간 초과를 확정한다.
    pairing: Arc<Mutex<stop_pairing::StopPairing>>,
    /// Codex 부모에게 돌려보낼 결과의 경로. 훅 워커·Stop 확정 스레드·송신 스레드가 함께 쓴다.
    returns: Arc<codex_return::Returns>,
    /// 결과 송신 스레드. `on_start` 에서 시작하고, 플러그인 값이 사라질 때 멈추고 회수한다.
    return_pump: Option<codex_return::ReturnPump>,
    scanner: Arc<Mutex<ErrorScanner>>,
    /// API 오류 후 재개 예약. `claude-auto-resume` 스레드가 만기를 처리한다.
    resume: Arc<Mutex<auto_resume::ResumeTable>>,
    /// 그 스레드가 쓸 문구. `on_start` 에서 스레드로 넘긴다.
    resume_texts: auto_resume::Texts,
    /// reboot 시퀀스 진행 중인 surface 집합 — 같은 surface 중복 reboot 가드.
    rebooting: Arc<Mutex<HashSet<u32>>>,
    /// 프로필·게이트·라운드 상태를 저장하는 `TASTY_PLUGIN_DATA_DIR`.
    /// 없으면 다른 경로에 쓰지 않고 각 처리기에서 오류 또는 통과로 처리한다.
    plugin_data_dir: Option<PathBuf>,
    /// 기본 게이트의 번역된 본문. 시작할 때 한 번 읽는다.
    /// 사용자 게이트 본문은 별도 파일에서 훅 실행 때마다 읽는다.
    checklist_body: String,
    /// 사람이 읽는 IPC 에러/응답 문자열 번역용. `main()` 에서 1 회 로드해 재사용한다.
    translator: Translator,
}

impl ClaudePlugin {
    fn new(
        plugin_data_dir: Option<PathBuf>,
        checklist_body: String,
        translator: Translator,
    ) -> Self {
        let resume_texts = auto_resume::Texts {
            message: translator.t("claude.auto_resume.message").to_string(),
            limit_title: translator.t("claude.auto_resume.limit_title").to_string(),
            limit_body: translator.t("claude.auto_resume.limit_body").to_string(),
            limit_body_one: translator
                .t_count_template("claude.auto_resume.limit_body", 1)
                .to_string(),
        };
        Self {
            state: Arc::new(Mutex::new(ClaudeState::new())),
            pairing: Arc::new(Mutex::new(stop_pairing::StopPairing::default())),
            returns: Arc::new(codex_return::Returns::new(plugin_data_dir.as_deref())),
            return_pump: None,
            scanner: Arc::new(Mutex::new(ErrorScanner::new())),
            resume: Arc::new(Mutex::new(auto_resume::ResumeTable::default())),
            resume_texts,
            rebooting: Arc::new(Mutex::new(HashSet::new())),
            plugin_data_dir,
            checklist_body,
            translator,
        }
    }
}

impl ClaudePlugin {
    /// 게이트 판정을 같은 Stop 의 상태 훅과 짝짓는다. 짝이 모이면 보류한 idle 처리를 실행한다.
    fn observe_gate_verdict(&self, host: &HostHandle, params: &Value, response: &Value) {
        let Some(session) = params.get("session_id").and_then(Value::as_str) else {
            return;
        };
        let prompt_id = params
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let settled = stop_pairing::lock_pairing(&self.pairing).verdict(
            session,
            prompt_id,
            stop_pairing::verdict_of(response),
            stop_pairing::block_cap_param(params),
            std::time::Instant::now(),
        );
        self.stop_tail(host).finish(settled);
    }

    fn stop_tail<'a>(&'a self, host: &'a HostHandle) -> hook::StopTail<'a, HostHandle> {
        hook::StopTail {
            state: &self.state,
            scanner: &self.scanner,
            resume: &self.resume,
            pairing: &self.pairing,
            returns: &self.returns,
            host,
        }
    }
}

impl Plugin for ClaudePlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn create_surface(&mut self, _ctx: SurfaceCreateCtx) -> SurfaceResult {
        // 자식 Claude는 호스트가 만든 일반 터미널에서 실행한다.
        SurfaceResult::default()
    }

    fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> Result<Value, IpcMethodError> {
        if let Some(name) = ctx.method.strip_prefix("claude.") {
            remote::refuse_public(&ctx.host, name, &ctx.params, &self.translator)?;
        }
        match ctx.method.as_str() {
            "claude.hook" => hook::handle_claude_hook(
                &self.state,
                &self.scanner,
                &self.resume,
                &self.pairing,
                &self.returns,
                &ctx.host,
                &ctx.params,
                self.plugin_data_dir.as_deref(),
                &self.translator,
            ),
            "claude.checklist_hook" => {
                let response = checklist::handle_checklist_hook(
                    &ctx.host,
                    self.plugin_data_dir.as_deref(),
                    &self.checklist_body,
                    &ctx.params,
                    &self.translator,
                )?;
                self.observe_gate_verdict(&ctx.host, &ctx.params, &response);
                Ok(response)
            }
            "claude.checklist_enable" => checklist::handle_enable(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.checklist_disable" => checklist::handle_disable(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.checklist_status" => checklist::handle_status(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.install" => match install::run_install(&self.translator) {
                Ok(changes) => Ok(
                    json!({ "installed": changes.added, "updated": changes.updated, "mod_updated": changes.mod_updated }),
                ),
                Err(e) => Err(IpcMethodError::new(
                    self.translator
                        .t_fmt("claude.install.install_failed", &e.to_string()),
                )),
            },
            "claude.uninstall" => match install::run_uninstall(&self.translator) {
                Ok(removed) => Ok(json!({ "uninstalled": removed })),
                Err(e) => Err(IpcMethodError::new(
                    self.translator
                        .t_fmt("claude.install.uninstall_failed", &e.to_string()),
                )),
            },
            "claude.parent" => handle_parent(&ctx.host, &ctx.params, &self.translator),
            "claude.state" => handle_state(&ctx.host, &ctx.params, &self.translator),
            "claude.children" => {
                handle_children(&self.returns, &ctx.host, &ctx.params, &self.translator)
            }
            "claude.kill" => handle_kill(
                &self.scanner,
                &self.returns,
                &ctx.host,
                &ctx.params,
                &self.translator,
            ),
            "claude.broadcast" => {
                handle_broadcast(&self.returns, &ctx.host, &ctx.params, &self.translator)
            }
            "claude.tell" => handle_tell(&self.returns, &ctx.host, &ctx.params, &self.translator),
            // attach 서버에서 온 원격 자식의 턴 결과. 이 처리 안에서 claude namespace 를 다시 부르지 않는다.
            "claude.remote_receive" => {
                remote::receive(&self.returns, &ctx.host, &ctx.params, &self.translator)
            }
            "claude.codex_returns" => {
                codex_return::handle_returns(&self.returns, &ctx.params, &self.translator)
            }
            "claude.remote_results" => {
                codex_return::handle_remote_results(&self.returns, &ctx.params, &self.translator)
            }
            "claude.notify_done" => {
                handle_notify_done(&self.scanner, &ctx.host, &ctx.params, &self.translator)
            }
            "claude.notify_error" => {
                handle_notify_error(&self.scanner, &ctx.host, &ctx.params, &self.translator)
            }
            "claude.notify_stop_failure" => {
                handle_notify_stop_failure(&self.scanner, &ctx.host, &ctx.params, &self.translator)
            }
            "claude.notify_quiet_release" => quiet_notice::handle_notify_quiet_release(
                &self.scanner,
                &ctx.host,
                &ctx.params,
                &self.translator,
            ),
            "claude.quiet" => {
                quiet_notice::handle_quiet(&self.scanner, &ctx.host, &ctx.params, &self.translator)
            }
            "claude.launch" => handle_launch(
                &self.scanner,
                &ctx.host,
                &ctx.params,
                self.plugin_data_dir.as_deref(),
                &self.translator,
            ),
            "claude.respawn" => handle_respawn(
                &self.scanner,
                &self.returns,
                &ctx.host,
                &ctx.params,
                self.plugin_data_dir.as_deref(),
                &self.translator,
            ),
            "claude.spawn" => handle_spawn(
                &self.scanner,
                &self.returns,
                &ctx.host,
                &ctx.params,
                self.plugin_data_dir.as_deref(),
                &self.translator,
            ),
            "claude.reboot" => reboot::handle_reboot(
                &self.rebooting,
                &ctx.host,
                &ctx.params,
                self.plugin_data_dir.as_deref(),
                &self.translator,
            ),
            // reboot과 같은 진행 중 집합을 사용해 같은 자식의 중복 실행을 막는다.
            "claude.child_profile" => handle_child_profile(
                &self.rebooting,
                &ctx.host,
                &ctx.params,
                self.plugin_data_dir.as_deref(),
                &self.translator,
            ),
            "claude.profile_register" => profile::handle_register(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.profile_unregister" => profile::handle_unregister(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.profile_list" => profile::handle_list(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.profile_show" => profile::handle_show(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.profile_current" => profile::handle_current(
                self.plugin_data_dir.as_deref(),
                &ctx.host,
                &ctx.params,
                &self.translator,
            ),
            "claude.gate_register" => gate::handle_register(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.gate_unregister" => gate::handle_unregister(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.gate_list" => gate::handle_list(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            "claude.gate_show" => gate::handle_show(
                self.plugin_data_dir.as_deref(),
                &ctx.params,
                &self.translator,
            ),
            other => Err(IpcMethodError::not_found(other)),
        }
    }

    fn on_start(&mut self, host: HostHandle, _bus: tasty_plugin_sdk::BusHandle) {
        let input_host = host.clone();
        std::thread::Builder::new()
            .name("claude-input-default".into())
            .spawn(move || {
                if let Err(error) = input_host.call(
                    "settings.initialize_input_rule",
                    serde_json::json!({ "app": "claude", "shift_enter_newline": true }),
                ) {
                    tracing::warn!("could not register Claude input default: {error}");
                }
            })
            .expect("spawn claude-input-default thread");
        let (state, scanner, resume, pairing, returns) = (
            self.state.clone(),
            self.scanner.clone(),
            self.resume.clone(),
            self.pairing.clone(),
            self.returns.clone(),
        );
        let pairing_host = host.clone();
        // 송신 스레드는 응답 상한을 짧게 둔 핸들을 쓴다. 느린 Codex 가 한 요청에 오래 묶어 두지 않는다.
        let mut deliver_host = host.clone();
        deliver_host.timeout = codex_return::DELIVER_CALL_TIMEOUT;
        self.return_pump = codex_return::ReturnPump::start(self.returns.clone(), deliver_host);
        std::thread::Builder::new()
            .name("claude-stop-pairing".into())
            .spawn(move || {
                stop_pairing_loop(&state, &scanner, &resume, &pairing, &returns, &pairing_host)
            })
            .expect("spawn claude-stop-pairing thread");
        // 등록된 최상위·자식 터미널을 별도 스레드에서 주기적으로 확인한다.
        let scanner = self.scanner.clone();
        let resume = self.resume.clone();
        let resume_host = host.clone();
        let resume_texts = self.resume_texts.clone();
        // 스레드 생성 실패 시 이 플러그인 프로세스를 패닉으로 종료한다.
        std::thread::Builder::new()
            .name("claude-error-scan".into())
            .spawn(move || error_scan_loop(scanner, host))
            .expect("spawn claude-error-scan thread");
        // terminal.tell 응답을 기다리는 동안 오류 감시가 멈추지 않도록 스레드를 나눈다.
        std::thread::Builder::new()
            .name("claude-auto-resume".into())
            .spawn(move || auto_resume::run_loop(resume, resume_host, resume_texts))
            .expect("spawn claude-auto-resume thread");
    }
}

/// 판정이 [`stop_pairing::GATE_DECISION_TIMEOUT`] 안에 모이지 않은 Stop 을 확정한다.
fn stop_pairing_loop(
    state: &Mutex<ClaudeState>,
    scanner: &Arc<Mutex<ErrorScanner>>,
    resume: &Mutex<auto_resume::ResumeTable>,
    pairing: &Mutex<stop_pairing::StopPairing>,
    returns: &codex_return::Returns,
    host: &HostHandle,
) {
    let tail = hook::StopTail {
        state,
        scanner,
        resume,
        pairing,
        returns,
        host,
    };
    loop {
        std::thread::sleep(STOP_PAIRING_TICK);
        let expired = stop_pairing::lock_pairing(tail.pairing).expire(std::time::Instant::now());
        tail.finish_expired(expired);
    }
}

fn error_scan_loop(scanner: Arc<Mutex<ErrorScanner>>, host: HostHandle) {
    // 재시작 전부터 있던 Claude 터미널을 다시 감시한다.
    let restored = retrack::retrack(&host, &scanner);
    tracing::info!("claude retrack: {restored} surface(s) watched again after start");
    loop {
        std::thread::sleep(ERROR_SCAN_INTERVAL);
        // 대상 목록을 복사한 뒤 잠금을 풀고, 개별 스캔 때 다시 잠근다.
        // poison은 lock_scanner가 한 번 보고하고 내부 상태를 계속 사용한다.
        let surfaces = crate::error_scan::lock_scanner(&scanner).enabled_snapshot();
        if !surfaces.is_empty() {
            crate::error_scan::lock_scanner(&scanner)
                .refresh_no_output_notice(&host, std::time::Instant::now());
        }
        for (sid, target) in surfaces {
            // 등록 경로별 조회 결과로 제거 여부를 판단한다. 조회 실패 처리도 경로마다 다르다.
            if !scan_target_is_alive(&host, sid, target) {
                crate::error_scan::lock_scanner(&scanner).stop_tracking(&host, sid);
                continue;
            }
            let mut scanner = crate::error_scan::lock_scanner(&scanner);
            // 끝 시각이 지난 무출력 선언을 먼저 끝낸다. 정지 판정은 조용하지 않은 surface 에서 일찍 돌아가 만료를 놓친다.
            scanner.release_expired(&host, sid, crate::error_scan::unix_now_ms());
            // 반환된 오류 문구는 시험에서만 사용한다.
            scanner.scan_one(&host, sid);
        }
        crate::error_scan::lock_scanner(&scanner).prune_quiet(crate::error_scan::unix_now_ms());
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    // SDK 연결에 쓰는 환경에서 여기서는 데이터 경로와 번역 설정을 읽는다.
    // 데이터 경로가 없으면 None을 넘겨 저장 요청을 거부한다.
    let env = PluginEnv::load().ok();
    let plugin_data_dir = env.as_ref().and_then(|e| e.data_dir.clone());
    // 환경을 읽지 못하면 번역 키를 그대로 반환한다.
    let translator = env
        .as_ref()
        .map(Translator::from_plugin_env)
        .unwrap_or_default();
    // 기본 게이트 본문은 활성 언어로 한 번만 읽는다.
    let checklist_body = translator.t("claude.checklist.body").to_string();
    tasty_plugin_sdk::run(ClaudePlugin::new(
        plugin_data_dir,
        checklist_body,
        translator,
    ))
}
