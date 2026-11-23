#![forbid(unsafe_code)]

//! Codex 기동·훅·설정 설치와 Codex 부모로의 결과 반환을 제공한다. 자식 관리는 호스트의
//! terminal.* IPC에 맡긴다. 같은 실행 파일의 `session` 모드는 터미널 안에서 전용 app-server와
//! TUI를 실행하는 세션 helper다.

// 이유: 시험의 let _는 제품 코드에서 반환값을 버리는 목록에 포함하지 않는다.
#![cfg_attr(test, allow(clippy::let_underscore_must_use))]

mod caller;
mod child_return;
mod delivery;
mod execution;
#[cfg(all(test, target_os = "linux"))]
mod execution_probe_tests;
mod handlers;
mod ids;
mod install;
mod notice;
mod ownership;
mod reboot;
mod reboot_watch;
mod rpc;
mod session;
mod spawn_options;
mod store;
mod task_turn;
mod ws;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tasty_plugin_sdk::{
    HostHandle, IpcMethodCtx, IpcMethodError, Plugin, PluginEnv, SurfaceCreateCtx, SurfaceResult,
    bus::BusHandle, i18n::Translator,
};

const PLUGIN_ID: &str = "com.tasty.codex";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct CodexPlugin {
    /// reboot 시퀀스 진행 중인 surface 집합 — 같은 surface 중복 reboot 가드.
    rebooting: Arc<Mutex<HashSet<u32>>>,
    /// 이 플러그인의 lang 디렉터리에서 읽은 번역 문구.
    translator: Translator,
    /// 세션 세대와 결과 원장을 두는 플러그인 데이터 폴더. 없으면 반환 기능을 쓰지 않는다.
    state_root: Option<PathBuf>,
    /// 이 프로세스의 백그라운드 송신 결과만 받기 위한 난수.
    outcome_token: String,
    /// 진행 중인 송신과 예약한 재시도.
    pump: delivery::Pump,
    /// 턴 후보 재확인 깨우기 스케줄러.
    turn_scheduler: Arc<delivery::turn_scheduler::TurnScheduler>,
}

impl CodexPlugin {
    fn root(&self) -> Result<&std::path::Path, IpcMethodError> {
        self.state_root
            .as_deref()
            .ok_or_else(|| IpcMethodError::new(self.translator.t("codex.state.no_data_dir")))
    }

    fn with_delivery<T>(
        &self,
        host: &HostHandle,
        f: impl FnOnce(&delivery::Ctx<'_>) -> Result<T, IpcMethodError>,
    ) -> Result<T, IpcMethodError> {
        let dispatch = delivery::send::ThreadDispatch {
            host: host.clone(),
            token: self.outcome_token.clone(),
            turn_scheduler: self.turn_scheduler.clone(),
        };
        let ctx = delivery::Ctx {
            root: self.root()?,
            dispatch: &dispatch,
            loaded: &delivery::proxy_loaded,
            live: &delivery::owned_thread_active,
            tr: &self.translator,
            token: &self.outcome_token,
            pump: &self.pump,
        };
        f(&ctx)
    }
}

impl Plugin for CodexPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn on_start(&mut self, host: HostHandle, _bus: BusHandle) {
        if self.state_root.is_none() {
            return;
        }
        if let Err(e) = self.with_delivery(&host, |ctx| {
            delivery::recover(ctx);
            Ok(())
        }) {
            tracing::warn!("codex delivery recovery: {e:?}");
        }
    }

    fn create_surface(&mut self, _ctx: SurfaceCreateCtx) -> SurfaceResult {
        // 자식은 호스트가 만든 일반 터미널에서 실행한다.
        SurfaceResult::default()
    }

    fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> Result<Value, IpcMethodError> {
        let IpcMethodCtx {
            method,
            params,
            host,
            ..
        } = ctx;
        let tr = &self.translator;
        if let Some(name) = method.strip_prefix("codex.") {
            handlers::remote::refuse_public(&host, name, &params, tr)?;
        }
        match method.as_str() {
            "codex.launch" => handlers::handle_launch(&host, &params, tr),
            "codex.spawn" => {
                self.with_delivery(&host, |d| handlers::handle_spawn(&host, &params, tr, d))
            }
            "codex.children" => {
                self.with_delivery(&host, |d| handlers::handle_children(&host, &params, tr, d))
            }
            "codex.parent" => self.with_delivery(&host, |d| delivery::handle_parent(d, &params)),
            "codex.parent_of" => handlers::handle_parent_of(&host, &params, tr),
            "codex.deliver" => self.with_delivery(&host, |d| delivery::handle_deliver(d, &params)),
            "codex.remote_receive" => {
                self.with_delivery(&host, |d| handlers::remote::receive(&host, d, &params, tr))
            }
            "codex.result" => self.with_delivery(&host, |d| delivery::handle_result(d, &params)),
            "codex.remote_results" => {
                self.with_delivery(&host, |d| handlers::remote::results(d, &params, tr))
            }
            "codex.delivery_outcome" => {
                self.with_delivery(&host, |d| delivery::handle_outcome(d, &params))
            }
            "codex.delivery_wake" => {
                self.with_delivery(&host, |d| delivery::handle_wake(d, &params))
            }
            "codex.turn_outcome" => {
                self.with_delivery(&host, |d| delivery::turns::handle_turn_outcome(d, &params))
            }
            "codex.turn_wake" => {
                self.with_delivery(&host, |d| delivery::turns::handle_turn_wake(d, &params))
            }
            "codex.session_open" => session::handle_open(self.root()?, &params, tr),
            "codex.session_bind" => self.with_delivery(&host, |d| {
                let result = session::handle_bind(&host, d.root, &params, tr)?;
                if let (Some(generation), Some(thread)) =
                    (params["gen"].as_str(), params["session"].as_str())
                {
                    let thread = thread.to_ascii_lowercase();
                    if let Err(error) =
                        delivery::turns::retire_unloaded_exits(d, generation, &thread)
                    {
                        tracing::warn!("codex selection: retire unloaded exits: {error}");
                    }
                    delivery::flush_thread(d, &thread);
                }
                Ok(result)
            }),
            "codex.state" => handlers::handle_state(&host, &params, tr),
            "codex.tell" => {
                self.with_delivery(&host, |d| handlers::handle_tell(&host, &params, tr, d))
            }
            "codex.notify_caller" => handlers::handle_notify_caller(&host, &params, tr),
            "codex.broadcast" => {
                self.with_delivery(&host, |d| handlers::handle_broadcast(&host, &params, tr, d))
            }
            "codex.kill" => {
                self.with_delivery(&host, |d| handlers::handle_kill(&host, &params, tr, d))
            }
            "codex.respawn" => {
                self.with_delivery(&host, |d| handlers::handle_respawn(&host, &params, tr, d))
            }
            "codex.install" => install::handle_install(&params, tr),
            "codex.uninstall" => install::handle_uninstall(&params, tr),
            "codex.hook" => {
                self.with_delivery(&host, |d| handlers::handle_hook(&host, &params, tr, d))
            }
            "codex.reboot" => reboot::handle_reboot(
                &self.rebooting,
                &host,
                &params,
                tr,
                self.state_root.as_deref(),
            ),
            other => Err(IpcMethodError::not_found(other)),
        }
    }
}

/// UTF-8이 아닌 첫 인자의 위치.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NonUtf8Arg {
    /// argv[0](실행 이름).
    Program,
    /// 실행 이름 뒤 n번째 인자. 셸의 `$n`처럼 1부터 센다.
    Argument(usize),
}

/// 실행 이름을 포함한 인자들을 UTF-8 문자열로 바꾼다. 손실 변환(U+FFFD 치환)은 하지 않는다.
/// 바뀐 ID·경로·옵션 값이 다른 대상에 작용할 수 있다.
fn utf8_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Vec<String>, NonUtf8Arg> {
    args.into_iter()
        .enumerate()
        .map(|(i, arg)| {
            arg.into_string().map_err(|_| match i {
                0 => NonUtf8Arg::Program,
                n => NonUtf8Arg::Argument(n),
            })
        })
        .collect()
}

/// 프로세스 인자를 읽어 실행 이름 뒤 인자들을 돌려준다. 실행 이름이나 인자가 UTF-8이 아니면 그
/// 위치만 stderr에 알리고 사용 오류(종료 코드 2)로 끝낸다. 인자 원문은 메시지 본문이나 토큰일 수
/// 있어 쓰지 않는다. `std::env::args`는 건너뛰는 실행 이름도 문자열로 바꾸므로 실행 이름도 함께
/// 검사한다.
fn process_args_or_exit() -> Vec<String> {
    let raw = std::env::args_os();
    match utf8_args(raw) {
        Ok(args) => args.into_iter().skip(1).collect(),
        Err(err) => {
            let message = match err {
                NonUtf8Arg::Program => "Error: the program name (argv[0]) contains invalid UTF-8. Nothing was executed or sent.".to_string(),
                NonUtf8Arg::Argument(n) => format!(
                    "Error: argument {n} contains invalid UTF-8. The first argument after the program name is numbered 1. Nothing was executed or sent."
                ),
            };
            report_usage_error(&message);
            std::process::exit(2);
        }
    }
}

/// stderr에 한 줄을 쓴다. stderr가 닫혔거나 가득 차도 알릴 곳이 없으므로 쓰기 오류는 버리고,
/// 종료 코드는 호출자가 정한다. `eprintln!`은 이때 패닉해 종료 코드 2에 닿지 못한다.
fn report_usage_error(message: &str) {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    let written = writeln!(stderr, "{message}");
    drop(written);
}

fn main() -> anyhow::Result<()> {
    let args = process_args_or_exit();
    if matches!(
        args.first().map(String::as_str),
        Some("session" | "terminal")
    ) {
        // 세션 helper는 사용자의 터미널에서 실행된다. 경고 이상만 시각·모듈 없이 stderr로 내
        // 사용자가 안내 문장을 그대로 읽게 한다.
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
            .with_writer(std::io::stderr)
            .without_time()
            .with_target(false)
            .with_ansi(false)
            .init();
        std::process::exit(if args[0] == "terminal" {
            session::helper::terminal_main(&args[1..])
        } else {
            session::helper::main(&args[1..])
        });
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    // 환경을 읽지 못하면 번역 키를 그대로 반환한다.
    let env = PluginEnv::load().ok();
    let translator = env
        .as_ref()
        .map(Translator::from_plugin_env)
        .unwrap_or_default();
    let state_root = env
        .as_ref()
        .and_then(|e| e.data_dir.clone())
        .map(|d| d.join("return"));
    if let Some(root) = &state_root {
        store::private_dir(root)?;
    }
    tasty_plugin_sdk::run(CodexPlugin {
        translator,
        state_root,
        outcome_token: ids::new_id()?,
        ..Default::default()
    })
}

#[cfg(test)]
mod install_tests;
#[cfg(test)]
mod ownership_tests;
