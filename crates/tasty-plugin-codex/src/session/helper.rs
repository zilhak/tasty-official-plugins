//! 터미널 안에서 실행되는 세션 helper(`tasty-plugin-codex session …`).
//!
//! helper는 그 터미널의 cwd·환경에서 전용 `codex app-server`를 자식 프로세스로 띄우고, 준비를
//! 확인한 뒤 TUI를 `--remote`로 붙인다. TUI가 끝나면 서버를 종료한다. 서버와 TUI의 생명주기를
//! 이 프로세스가 소유하므로 셸 백그라운드 작업이나 PID 문자열을 쓰지 않는다.
//!
//! 서버를 시작하지 못하면 실패로 끝낸다. embedded 서버로 내려가 결과를 받을 수 없는 세션을
//! 정상 실행처럼 보이게 하지 않는다.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tasty_plugin_sdk::i18n::Translator;

use super::registry::{HelperPhase, HelperState, helper_file};
use crate::store;

/// 서버 준비를 기다리는 최대 시간.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_POLL: Duration = Duration::from_millis(100);
/// TUI 종료와 받은 신호를 확인하는 간격.
const TUI_POLL: Duration = Duration::from_millis(50);
/// 소유 서버에서 thread를 다시 여는 연결·요청의 기한.
const RESUME_TIMEOUT: Duration = Duration::from_secs(30);
/// 훅 프로세스가 세대를 확인하는 환경변수.
pub(crate) const GEN_ENV: &str = "TASTY_CODEX_GEN";
/// TUI가 WebSocket token을 읽는 환경변수 이름.
const REMOTE_TOKEN_ENV: &str = "TASTY_CODEX_REMOTE_TOKEN";

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Args {
    pub resume: Option<String>,
    pub prompt_file: Option<PathBuf>,
    pub tui_args: Vec<String>,
    pub native: Option<super::native_args::NativeArgs>,
}

pub(crate) fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--resume" => {
                let v = it.next().ok_or("missing value after --resume")?;
                if !crate::ids::is_uuid(v) {
                    return Err("--resume needs a thread UUID".into());
                }
                out.resume = Some(v.to_ascii_lowercase());
            }
            "--prompt-file" => {
                out.prompt_file = Some(PathBuf::from(
                    it.next().ok_or("missing value after --prompt-file")?,
                ));
            }
            "--" => {
                out.tui_args = it.by_ref().cloned().collect();
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    super::command::validate_tui_args(&out.tui_args)?;
    Ok(out)
}

/// 플러그인이 발급한 세대 등록.
#[derive(Debug)]
pub(crate) struct Opened {
    pub generation: String,
    pub state_dir: PathBuf,
    pub listen: String,
    pub token_file: Option<PathBuf>,
}

pub(crate) fn parse_opened(value: &Value) -> Result<Opened, String> {
    let generation = value["gen"]
        .as_str()
        .filter(|g| crate::ids::is_generation(g));
    let state_dir = value["state_dir"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let listen = value["listen"].as_str();
    match (generation, state_dir, listen) {
        (Some(generation), Some(state_dir), Some(listen)) => Ok(Opened {
            generation: generation.into(),
            state_dir,
            listen: listen.into(),
            token_file: value["token_file"].as_str().map(PathBuf::from),
        }),
        _ => Err(format!("unexpected session-open response: {value}")),
    }
}

/// Probes and mutations share the terminal-owned instance. An explicit parent home takes
/// precedence over the CLI profile default, even when PATH finds a differently built CLI.
fn host_cli() -> Command {
    let mut cmd = Command::new(if cfg!(windows) { "tasty.exe" } else { "tasty" });
    if let Some(home) = std::env::var_os("TASTY_PARENT_HOME").filter(|h| !h.is_empty()) {
        cmd.env("TASTY_HOME", home);
    }
    cmd
}

/// 같은 인스턴스의 Tasty CLI로 세대를 발급받는다. 터미널이 알려 준 인스턴스 홈만 쓴다.
fn open_via_cli(surface: u32, args: &Args) -> Result<Opened, String> {
    let mut cmd = host_cli();
    cmd.args(["codex", "session-open", "--surface", &surface.to_string()]);
    if let Some(thread) = &args.resume {
        cmd.args(["--resume", thread]);
    }
    let tui = serde_json::to_string(&args.tui_args).map_err(|e| e.to_string())?;
    cmd.args(["--tui-args", &tui]);
    if let Some(native) = &args.native {
        cmd.args([
            "--native-args",
            &serde_json::to_string(&native.restore).map_err(|e| e.to_string())?,
        ]);
    }
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("tasty CLI: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tasty codex session-open exited with {}",
            out.status
        ));
    }
    let value: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("session-open output: {e}"))?;
    parse_opened(&value)
}

fn write_state(state_dir: &Path, phase: HelperPhase, url: Option<String>, detail: Option<String>) {
    write_state_resumed(state_dir, phase, url, detail, None);
}

fn write_state_resumed(
    state_dir: &Path,
    phase: HelperPhase,
    url: Option<String>,
    detail: Option<String>,
    resumed: Option<String>,
) {
    let state = HelperState {
        phase,
        url,
        detail,
        resumed,
        updated_at_ms: store::now_ms(),
    };
    if let Err(e) = store::write_json(&helper_file(state_dir), &state) {
        tracing::warn!("codex session helper: state write failed: {e}");
    }
}

/// 정상 종료 요청 뒤 서버가 스스로 끝나기를 기다리는 시간.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// helper가 띄운 서버 프로세스. npm의 `codex`는 실제 서버를 다시 띄우는 래퍼이고 신호를 한 번만
/// 전달한다. 실제 서버는 첫 SIGTERM에 진행 중인 작업을 기다리며 종료하므로 래퍼만 끝내면 서버가
/// 고아로 남을 수 있다. Unix는 서버를 자기 프로세스 그룹으로 띄워 그룹 전체를 끝내고, Windows는
/// Job Object로 하위 프로세스까지 함께 끝낸다. 그룹을 나누므로 터미널의 Ctrl+C·hangup 신호는
/// 서버에 직접 가지 않는다. helper 자신은 [`main`]에서 그 신호를 기록만 하도록 바꿔 정리 도중
/// 끝나지 않는다. Linux는 helper가 강제 종료되면 서버 래퍼가 종료 신호를 받는다.
struct OwnedServer {
    child: Child,
    #[cfg(windows)]
    job: Option<tasty_reaper::JobObject>,
}

impl OwnedServer {
    fn spawn(cmd: Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        let cmd = {
            let mut cmd = cmd;
            std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
            cmd
        };
        let child = tasty_reaper::spawn_bound_to_host(cmd)?;
        #[cfg(windows)]
        let job = match tasty_reaper::JobObject::new().and_then(|job| {
            job.assign_pid(child.id())?;
            Ok(job)
        }) {
            Ok(job) => Some(job),
            Err(e) => {
                tracing::warn!("codex session helper: server job object: {e}");
                None
            }
        };
        Ok(Self {
            child,
            #[cfg(windows)]
            job,
        })
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// 서버를 정리한다. 이미 끝났으면 상태만 회수한다.
    fn stop(&mut self) {
        #[cfg(windows)]
        if let Some(job) = &self.job
            && let Err(e) = job.terminate()
        {
            tracing::warn!("codex session helper: server job terminate: {e}");
        }
        if let Err(e) = tasty_reaper::terminate_process_group(&mut self.child, STOP_GRACE) {
            tracing::warn!("codex session helper: server stop: {e}");
        }
    }
}

/// 서버 준비 판정. Some(url)이면 준비됐다.
pub(crate) type ReadyProbe<'a> = Box<dyn FnMut(&Path) -> Option<String> + 'a>;

#[derive(Debug, PartialEq)]
pub(crate) enum LifecycleError {
    /// 서버를 시작하지 못했거나 준비 전에 끝났다. TUI는 시작하지 않았다.
    Server(String),
    /// TUI를 실행하지 못했다.
    Tui(String),
    /// 서버 준비 전에 Ctrl+C·hangup·종료 요청을 받았다. 서버를 정리했고 TUI는 시작하지 않았다.
    Interrupted,
    /// 준비된 서버에서 thread를 다시 열지 못했거나 요청한 정책이 적용되지 않았다. 서버를
    /// 정리했고 TUI는 시작하지 않았다.
    Resume(String),
}

/// 서버 준비 뒤 TUI 전에 마친 작업.
#[derive(Default)]
pub(crate) struct Prepared {
    /// 서버가 다시 연 thread.
    pub resumed: Option<String>,
    /// TUI가 끝날 때까지 그 thread를 구독하는 연결의 읽기 스레드. 서버가 끝나면 함께 끝난다.
    pub hold: Option<std::thread::JoinHandle<()>>,
    pub relay: Option<super::relay::Relay>,
}

/// 준비 뒤 단계가 없는 [`run_lifecycle_with`].
#[cfg(all(test, unix))]
pub(crate) fn run_lifecycle(
    state_dir: &Path,
    server: Command,
    ready: ReadyProbe<'_>,
    ready_timeout: Duration,
    make_tui: impl FnOnce(&str) -> Command,
) -> Result<ExitStatus, LifecycleError> {
    run_lifecycle_with(
        state_dir,
        server,
        ready,
        ready_timeout,
        |_| Ok(Prepared::default()),
        make_tui,
    )
}

/// 서버 시작 → 준비 확인 → 준비 뒤 단계(`prepare`) → TUI 실행 → 서버 종료. TUI의 종료 상태를
/// 돌려준다. `prepare`가 실패하거나 그 사이 신호를 받으면 서버를 정리하고 `failed`를 기록한 뒤
/// TUI를 시작하지 않는다. `ready`는 `prepare`가 끝난 뒤 기록한다.
///
/// [`tasty_reaper::defer_termination_signals`]를 설치한 프로세스에서는 받은 신호를 단계별로
/// 처리한다. 준비 전에는 어떤 신호든 시작을 취소한다. TUI 실행 중에는 Ctrl+C를 TUI에 맡기고,
/// hangup·종료 요청은 TUI에 한 번 전달한 뒤 [`STOP_GRACE`] 안에 끝나지 않으면 강제로 끝낸다.
/// TUI가 끝난 뒤의 신호는 무시하고 서버 정리를 마친다.
pub(crate) fn run_lifecycle_with(
    state_dir: &Path,
    mut server: Command,
    mut ready: ReadyProbe<'_>,
    ready_timeout: Duration,
    prepare: impl FnOnce(&str) -> Result<Prepared, String>,
    make_tui: impl FnOnce(&str) -> Command,
) -> Result<ExitStatus, LifecycleError> {
    write_state(state_dir, HelperPhase::Starting, None, None);
    let log_path = state_dir.join("server.log");
    let fail = |detail: String| {
        write_state(state_dir, HelperPhase::Failed, None, Some(detail.clone()));
        LifecycleError::Server(detail)
    };
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| fail(format!("server log: {e}")))?;
    let log_err = log
        .try_clone()
        .map_err(|e| fail(format!("server log: {e}")))?;
    server.stdin(Stdio::null()).stdout(log).stderr(log_err);
    let mut child = OwnedServer::spawn(server)
        .map_err(|e| fail(format!("codex app-server could not start: {e}")))?;
    let deadline = Instant::now() + ready_timeout;
    let url = loop {
        if tasty_reaper::take_deferred_signals().any() {
            child.stop();
            write_state(
                state_dir,
                HelperPhase::Failed,
                None,
                Some("interrupted before the server was ready".into()),
            );
            return Err(LifecycleError::Interrupted);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(fail(format!(
                    "codex app-server exited before it was ready ({status})"
                )));
            }
            Ok(None) => {}
            Err(e) => {
                child.stop();
                return Err(fail(format!("codex app-server status: {e}")));
            }
        }
        if let Some(url) = ready(&log_path) {
            break url;
        }
        if Instant::now() >= deadline {
            child.stop();
            return Err(fail(format!(
                "codex app-server was not ready within {}s",
                ready_timeout.as_secs()
            )));
        }
        std::thread::sleep(READY_POLL);
    };
    let prepared = match prepare(&url) {
        Ok(p) => p,
        Err(detail) => {
            child.stop();
            write_state(state_dir, HelperPhase::Failed, None, Some(detail.clone()));
            tasty_reaper::take_deferred_signals();
            return Err(LifecycleError::Resume(detail));
        }
    };
    if tasty_reaper::take_deferred_signals().any() {
        child.stop();
        write_state(
            state_dir,
            HelperPhase::Failed,
            None,
            Some("interrupted before the server was ready".into()),
        );
        return Err(LifecycleError::Interrupted);
    }
    write_state_resumed(
        state_dir,
        HelperPhase::Ready,
        Some(url.clone()),
        None,
        prepared.resumed,
    );
    let tui_url = prepared
        .relay
        .as_ref()
        .map_or(url.as_str(), |relay| relay.url.as_str());
    let status = make_tui(tui_url).spawn().and_then(wait_tui);
    write_state(state_dir, HelperPhase::Exited, Some(url), None);
    child.stop();
    // 서버가 끝나 연결이 닫히면 읽기 스레드도 끝난다. 기다리지 않는다.
    drop(prepared.hold);
    // 정리 중에 받은 신호는 이미 처리를 마쳤으므로 버린다.
    tasty_reaper::take_deferred_signals();
    status.map_err(|e| LifecycleError::Tui(e.to_string()))
}

/// TUI가 끝나기를 기다린다. Ctrl+C는 같은 전경 그룹의 TUI가 직접 받으므로 helper는 무시한다.
/// hangup·종료 요청은 TUI(npm 래퍼면 래퍼가 실제 TUI에 전달한다)에 한 번 보내고, 기한 안에
/// 끝나지 않으면 강제로 끝낸다.
fn wait_tui(mut tui: Child) -> std::io::Result<ExitStatus> {
    let mut forwarded: Option<Instant> = None;
    loop {
        if let Some(status) = tui.try_wait()? {
            return Ok(status);
        }
        match forwarded {
            None => {
                if tasty_reaper::take_deferred_signals().forward_termination(&mut tui)? {
                    forwarded = Some(Instant::now());
                }
            }
            Some(at) if at.elapsed() >= STOP_GRACE => {
                if let Err(e) = tui.kill() {
                    tracing::warn!("codex session helper: tui kill: {e}");
                }
                return tui.wait();
            }
            Some(_) => {}
        }
        std::thread::sleep(TUI_POLL);
    }
}

#[cfg(unix)]
fn unix_ready(listen: &str) -> ReadyProbe<'_> {
    let path = listen.strip_prefix("unix://").map(PathBuf::from);
    Box::new(move |_log| {
        let path = path.as_ref()?;
        std::os::unix::net::UnixStream::connect(path)
            .ok()
            .map(|_| format!("unix://{}", path.display()))
    })
}

/// 서버 로그의 `listening on: ws://127.0.0.1:<port>` 줄에서 실제 포트를 읽는다.
pub(crate) fn listening_port(log: &str) -> Option<u16> {
    log.lines().find_map(|line| {
        let rest = line.split("listening on: ws://127.0.0.1:").nth(1)?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

fn ws_ready() -> ReadyProbe<'static> {
    Box::new(|log| {
        let text = std::fs::read_to_string(log).ok()?;
        let port = listening_port(&text)?;
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500))
            .ok()
            .map(|_| format!("ws://127.0.0.1:{port}"))
    })
}

#[cfg(test)]
fn server_command(opened: &Opened) -> Command {
    server_command_with(opened, Command::new("codex"))
}

fn server_command_with(opened: &Opened, mut cmd: Command) -> Command {
    cmd.args(["app-server", "--listen", &opened.listen]);
    if let Some(token_file) = &opened.token_file {
        cmd.args(["--ws-auth", "capability-token", "--ws-token-file"]);
        cmd.arg(token_file);
    }
    cmd.env(GEN_ENV, &opened.generation);
    cmd
}

#[cfg(test)]
pub(crate) fn tui_command(
    url: &str,
    token: Option<&str>,
    args: &Args,
    prompt: Option<&str>,
) -> Command {
    tui_command_with(url, token, args, prompt, Command::new("codex"))
}

fn tui_command_with(
    url: &str,
    token: Option<&str>,
    args: &Args,
    prompt: Option<&str>,
    mut cmd: Command,
) -> Command {
    if args.resume.is_some() {
        cmd.arg("resume");
    }
    cmd.args(["--remote", url]);
    let settings = super::command::ResumeSettings::from_tui_args(&args.tui_args);
    let token = token.or_else(|| {
        (args.resume.is_none() && (settings.model.is_some() || settings.effort.is_some()))
            .then_some("")
    });
    if let Some(token) = token {
        cmd.args(["--remote-auth-token-env", REMOTE_TOKEN_ENV]);
        cmd.env(REMOTE_TOKEN_ENV, token);
    }
    cmd.args(["-c", "check_for_update_on_startup=false"]);
    if args.resume.is_some() {
        // 재개 TUI는 정책 재정의를 받지 않는다. 정책·모델은 prepare_resume이 서버에 적용했다.
        cmd.args(super::command::resume_tui_args(&args.tui_args));
    } else {
        cmd.args(&args.tui_args);
    }
    if let Some(thread) = &args.resume {
        cmd.arg(thread);
    }
    if let Some(p) = prompt.filter(|p| !p.is_empty()) {
        cmd.arg(p);
    }
    cmd
}

fn translator() -> Translator {
    let lang_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("lang")))
        .unwrap_or_default();
    translator_from(&lang_dir, |name| std::env::var_os(name))
}

/// helper는 터미널 셸의 자식이라 plugin env(`TASTY_PLUGIN_ID` 등)를 받지 못해
/// `Translator::from_plugin_env`를 쓸 수 없다. 번역 층은 그 함수와 같게 맞춘다.
fn translator_from(lang_dir: &Path, env: impl Fn(&str) -> Option<OsString>) -> Translator {
    let locale = helper_locale(&env);
    // 터미널 셸의 값은 host의 TASTY_HOME 그대로라 상대 경로일 수 있다. helper의 cwd 기준으로
    // 해석하면 host가 아니라 작업 폴더의 파일을 읽으므로 값이 없는 것처럼 다룬다.
    let user_lang_dir = env("TASTY_PARENT_HOME")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
        .map(|root| root.join("lang"));
    Translator::load_with_overrides(
        lang_dir,
        &locale,
        crate::PLUGIN_ID,
        user_lang_dir.as_deref(),
    )
}

/// host가 `general.language`에서 확정해 터미널 셸까지 물려준 언어. OS 로케일(`LANG`)은 설정과
/// 다를 수 있어 보지 않는다. 값이 없으면 SDK·host-plugin과 같이 `en`이다.
fn helper_locale(env: &impl Fn(&str) -> Option<OsString>) -> String {
    env("TASTY_LOCALE")
        .and_then(|code| code.into_string().ok())
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| "en".to_string())
}

/// `session` 모드의 진입점. 프로세스 종료 코드를 돌려준다.
pub(crate) fn main(args: &[String]) -> i32 {
    entry(args, false)
}

pub(crate) fn terminal_main(args: &[String]) -> i32 {
    entry(args, true)
}

fn entry(raw: &[String], native: bool) -> i32 {
    let tr = translator();
    let report = |key: &str, detail: &str| {
        tracing::error!("{}", tr.t_replace(key, "{detail}", detail));
    };
    let args = match entry_args(raw, native, &report) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // 시작부터 설치해야 서버를 띄운 뒤 도착한 신호가 helper만 끝내고 서버를 남기지 않는다.
    if let Err(e) = tasty_reaper::defer_termination_signals() {
        tracing::warn!("codex session helper: terminal signals: {e}");
    }
    let Some(surface) = std::env::var("TASTY_SURFACE_ID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    else {
        report("codex.session.not_in_tasty", "TASTY_SURFACE_ID");
        return 2;
    };
    let prompt = match read_prompt(&args, &report) {
        Ok(prompt) => prompt,
        Err(code) => return code,
    };
    if let Err(code) = wait_for_host(&report) {
        return code;
    }
    let opened = match open_via_cli(surface, &args) {
        Ok(o) => o,
        Err(e) => {
            report("codex.session.open_failed", &e);
            return 1;
        }
    };
    let execution = match tasty_utils::execution::ExecutionContext::capture("codex") {
        Ok(execution) => execution,
        Err(e) => {
            write_state(
                &opened.state_dir,
                HelperPhase::Failed,
                None,
                Some(e.to_string()),
            );
            report("codex.session.server_failed", &e.to_string());
            return 1;
        }
    };
    let token = match &opened.token_file {
        Some(f) => match std::fs::read_to_string(f) {
            Ok(t) => Some(t.trim().to_string()),
            Err(e) => {
                report("codex.session.server_failed", &format!("token file: {e}"));
                return 1;
            }
        },
        None => None,
    };
    #[cfg(unix)]
    let probe = if opened.token_file.is_none() {
        unix_ready(&opened.listen)
    } else {
        ws_ready()
    };
    #[cfg(not(unix))]
    let probe = ws_ready();
    let relay_token = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let result = run_lifecycle_with(
        &opened.state_dir,
        server_command_with(&opened, execution.command()),
        probe,
        READY_TIMEOUT,
        |url| {
            if args.native.is_none() {
                let mut prepared = prepare_resume(url, opened.token_file.as_deref(), &args)?;
                let settings = super::command::ResumeSettings::from_tui_args(&args.tui_args);
                if args.resume.is_none() && (settings.model.is_some() || settings.effort.is_some())
                {
                    let endpoint =
                        super::registry::parse_endpoint(url, opened.token_file.as_deref())?;
                    let relay = super::relay::Relay::start(
                        endpoint,
                        settings,
                        std::sync::Arc::new(|_| Ok(())),
                    )
                    .map_err(|e| e.to_string())?;
                    *relay_token.lock().map_err(|_| "relay token poisoned")? =
                        Some(relay.token.clone());
                    prepared.relay = Some(relay);
                }
                return Ok(prepared);
            }
            let endpoint = super::registry::parse_endpoint(url, opened.token_file.as_deref())?;
            let generation = opened.generation.clone();
            let bind = std::sync::Arc::new(move |reply: &Value| {
                register_native_thread(surface, &generation, reply)
            });
            let relay = super::relay::Relay::start(
                endpoint,
                super::command::ResumeSettings::from_tui_args(&args.tui_args),
                bind,
            )
            .map_err(|e| e.to_string())?;
            *relay_token.lock().map_err(|_| "relay token poisoned")? = Some(relay.token.clone());
            Ok(Prepared {
                relay: Some(relay),
                ..Prepared::default()
            })
        },
        |url| {
            if let Some(native) = &args.native {
                let mut cmd = execution.command();
                cmd.args(["--remote", url, "--remote-auth-token-env", REMOTE_TOKEN_ENV]);
                set_relay_auth(&mut cmd, &relay_token);
                cmd.args(&native.tui);
                return cmd;
            }
            let mut cmd = tui_command_with(
                url,
                token.as_deref(),
                &args,
                prompt.as_deref(),
                execution.command(),
            );
            set_relay_auth(&mut cmd, &relay_token);
            cmd
        },
    );
    exit_code(result, &report)
}

static RELAY_TOKEN_POISON_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn set_relay_auth(cmd: &mut Command, relay_token: &std::sync::Mutex<Option<String>>) {
    // prepare는 완성한 String을 Some으로 한 번 대입한다. 부분 토큰이나 여러 필드의
    // 중간 상태가 없으므로 저장된 값을 복구해 전달한다. 진단에는 토큰 값을 넣지 않는다.
    let token = tasty_utils::poison::recover_mutex(
        relay_token.lock(),
        "codex relay auth slot",
        &RELAY_TOKEN_POISON_REPORTED,
    );
    if let Some(token) = token.as_ref() {
        cmd.env(REMOTE_TOKEN_ENV, token);
    }
}

/// 진입점의 인자. 실행을 끝내야 하면 종료 코드를 돌려준다. 해석할 수 없는 native 인자는 Tasty
/// 세션 복원 없이 native Codex 로 넘긴다.
fn entry_args(raw: &[String], native: bool, report: &dyn Fn(&str, &str)) -> Result<Args, i32> {
    let parsed = if native {
        match super::native_args::parse(raw) {
            Ok(Some(native)) => Ok(Args {
                tui_args: native.policy.clone(),
                native: Some(native),
                ..Args::default()
            }),
            Ok(None) => return Err(native_passthrough(raw)),
            Err(error) => {
                tracing::warn!("{error}; running native Codex without Tasty session restoration");
                return Err(native_passthrough(raw));
            }
        }
    } else {
        parse_args(raw)
    };
    parsed.map_err(|e| {
        report("codex.session.bad_args", &e);
        2
    })
}

/// 프롬프트 파일을 읽고 지운다. 읽지 못하면 종료 코드를 돌려준다.
fn read_prompt(args: &Args, report: &dyn Fn(&str, &str)) -> Result<Option<String>, i32> {
    let Some(file) = &args.prompt_file else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(file);
    if let Err(e) = std::fs::remove_file(file) {
        tracing::warn!("codex session helper: prompt file cleanup: {e}");
    }
    match text {
        Ok(t) => Ok(Some(t)),
        Err(e) => {
            report("codex.session.bad_args", &format!("prompt file: {e}"));
            Err(2)
        }
    }
}

/// Tasty IPC 가 준비될 때까지 기다린다. 취소됐거나 실패하면 종료 코드를 돌려준다.
fn wait_for_host(report: &dyn Fn(&str, &str)) -> Result<(), i32> {
    // Selected terminals are restored before boot starts the IPC listener. Only the read-only
    // readiness query is retried; session-open still runs once after a successful response.
    match super::readiness::wait(host_cli, READY_TIMEOUT, || {
        tasty_reaper::take_deferred_signals().any()
    }) {
        Ok(()) => Ok(()),
        Err(super::readiness::Error::Cancelled) => Err(130),
        Err(super::readiness::Error::Failed(detail)) => {
            report("codex.session.open_failed", &detail);
            Err(1)
        }
    }
}

/// 수명 주기 결과를 프로세스 종료 코드로 바꾼다.
fn exit_code(result: Result<ExitStatus, LifecycleError>, report: &dyn Fn(&str, &str)) -> i32 {
    match result {
        Ok(status) => status.code().unwrap_or(1),
        Err(LifecycleError::Server(detail)) => {
            report("codex.session.server_failed", &detail);
            1
        }
        Err(LifecycleError::Tui(detail)) => {
            report("codex.session.tui_failed", &detail);
            1
        }
        // 사용자가 취소했으므로 오류로 알리지 않는다. 128+SIGINT 관례를 따른다.
        Err(LifecycleError::Interrupted) => 130,
        Err(LifecycleError::Resume(detail)) => {
            report("codex.session.resume_failed", &detail);
            1
        }
    }
}

/// 재개할 thread가 있으면 소유 서버에서 요청한 정책·모델로 다시 열고 응답으로 확인한다. 그 연결은
/// TUI가 끝날 때까지 thread를 구독한다. TUI는 재개 때 정책 없이 `thread/resume`을 보내는데, 서버는
/// 구독자가 없는 idle thread에 다른 인자로 들어온 재개를 cold 재시작으로 처리해 helper가 적용한
/// 정책을 버린다. 구독자가 있으면 로드된 thread에 붙는다.
fn prepare_resume(url: &str, token_file: Option<&Path>, args: &Args) -> Result<Prepared, String> {
    let Some(thread) = &args.resume else {
        return Ok(Prepared::default());
    };
    let settings = super::command::ResumeSettings::from_tui_args(&args.tui_args);
    let endpoint = super::registry::parse_endpoint(url, token_file)?;
    let mut client = crate::rpc::Client::connect(&endpoint, RESUME_TIMEOUT)
        .map_err(|e| format!("connect to the owned server: {e}"))?;
    let reply = client
        .call("thread/resume", settings.params(thread))
        .map_err(|e| format!("thread/resume {thread}: {e}"))?;
    settings.verify(&reply)?;
    let hold = client
        .hold()
        .map_err(|e| format!("keep the thread subscribed: {e}"))?;
    Ok(Prepared {
        resumed: Some(thread.clone()),
        hold: Some(hold),
        relay: None,
    })
}

fn native_passthrough(args: &[String]) -> i32 {
    let result: std::io::Result<ExitStatus> =
        tasty_utils::execution::ExecutionContext::capture("codex").and_then(|execution| {
            let mut command = execution.command();
            command.args(args);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                Err(command.exec())
            }
            #[cfg(not(unix))]
            {
                command.status()
            }
        });
    match result {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            tracing::error!("Codex launch failed: {error}");
            1
        }
    }
}

fn register_native_thread(surface: u32, generation: &str, reply: &Value) -> Result<(), String> {
    let thread = reply["thread"]["id"]
        .as_str()
        .filter(|id| crate::ids::is_uuid(id))
        .ok_or("server returned no thread UUID")?;
    let payload = serde_json::json!({"session_id":thread,"model":reply["model"]});
    let mut cmd = host_cli();
    cmd.args([
        "codex",
        "session-bind",
        "--surface",
        &surface.to_string(),
        "--gen",
        generation,
    ]);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    let written = if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(payload.to_string().as_bytes())
    } else {
        Err(std::io::Error::other("registration stdin unavailable"))
    };
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    written.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("Tasty session registration failed: {}", out.status));
    }
    let result: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    if result.get("ignored").is_some() || result["host_call_failures"].as_u64() != Some(0) {
        return Err(format!(
            "Tasty did not register the selected conversation: {result}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_relay_token_still_reaches_the_native_tui_environment() {
        let relay_token = std::sync::Mutex::new(Some("fixture-capability".to_owned()));
        let poisoned = std::panic::catch_unwind(|| {
            let _held = relay_token.lock().unwrap();
            panic!("poison auth slot");
        });
        assert!(poisoned.is_err());
        assert!(relay_token.is_poisoned());
        let mut cmd = Command::new("unused-test-command");
        set_relay_auth(&mut cmd, &relay_token);
        assert!(cmd.get_envs().any(|(k, v)| k == REMOTE_TOKEN_ENV
            && v.is_some_and(|v| v == "fixture-capability")), "auth environment must survive poison");
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn helper_language_follows_the_setting_not_the_shell() {
        for (setting, shell, expected) in [
            ("en", "ko_KR.UTF-8", "en"),
            ("ja", "en_US.UTF-8", "ja"),
            ("ko", "ja_JP.UTF-8", "ko"),
            // 하이픈이 든 내장 코드도 값을 바꾸지 않고 번역기까지 전달한다.
            ("zh-Hans", "en_US.UTF-8", "zh-Hans"),
        ] {
            let vars = [("TASTY_LOCALE", setting), ("LANG", shell)];
            assert_eq!(helper_locale(&env_of(&vars)), expected, "{vars:?}");
        }
    }

    #[test]
    fn without_a_setting_the_helper_uses_english_whatever_the_shell_says() {
        for shell in [
            "ko_KR.UTF-8",
            "ja_JP.UTF-8",
            "zh_CN.UTF-8",
            "zh_TW.UTF-8",
            "zh_HK.UTF-8",
            "zh-Hant",
        ] {
            let unset = [("LANG", shell)];
            assert_eq!(helper_locale(&env_of(&unset)), "en", "{unset:?}");
            let blank = [("TASTY_LOCALE", ""), ("LANG", shell)];
            assert_eq!(helper_locale(&env_of(&blank)), "en", "{blank:?}");
        }
    }

    /// 설치본 카탈로그. 값은 어느 파일에서 왔는지만 구별한다.
    fn lang_fixture(root: &Path) -> PathBuf {
        let lang = root.join("installed");
        std::fs::create_dir_all(&lang).unwrap();
        for code in ["en", "ko", "ja", "zh-Hans"] {
            std::fs::write(
                lang.join(format!("{code}.toml")),
                format!("[codex.session]\nbad_args = \"installed {code}\"\n"),
            )
            .unwrap();
        }
        lang
    }

    #[test]
    fn the_translator_reads_the_catalog_of_the_setting() {
        const KEY: &str = "codex.session.bad_args";
        let dir = tempfile::tempdir().unwrap();
        let lang = lang_fixture(dir.path());
        for (setting, shell, expected) in [
            ("en", "ko_KR.UTF-8", "installed en"),
            ("ja", "en_US.UTF-8", "installed ja"),
            ("zh-Hans", "en_US.UTF-8", "installed zh-Hans"),
        ] {
            let vars = [("TASTY_LOCALE", setting), ("LANG", shell)];
            assert_eq!(
                translator_from(&lang, env_of(&vars)).t(KEY),
                expected,
                "{vars:?}"
            );
        }
        let unset = [("LANG", "zh_TW.UTF-8")];
        assert_eq!(
            translator_from(&lang, env_of(&unset)).t(KEY),
            "installed en"
        );
    }

    /// cwd는 프로세스 전역이므로 시험 실행 파일을 그 폴더에서 다시 띄워 확인한다.
    #[test]
    fn a_relative_parent_home_never_reads_files_under_the_working_directory() {
        const NAME: &str = "session::helper::tests::a_relative_parent_home_never_reads_files_under_the_working_directory";
        const CHILD_ENV: &str = "TASTY_TEST_HELPER_RELATIVE_HOME_LANG";
        const KEY: &str = "codex.session.bad_args";
        let plugin_id = crate::PLUGIN_ID;
        let rel = format!("rel/lang/plugins/{plugin_id}/ja.toml");
        let blank = format!("lang/plugins/{plugin_id}/ja.toml");
        if let Some(lang) = std::env::var_os(CHILD_ENV) {
            for (home, file) in [("rel", &rel), ("", &blank)] {
                assert!(Path::new(file).is_file(), "{file}가 cwd 아래에 있어야 한다");
                let vars = [("TASTY_LOCALE", "ja"), ("TASTY_PARENT_HOME", home)];
                assert_eq!(
                    translator_from(Path::new(&lang), env_of(&vars)).t(KEY),
                    "installed ja",
                    "{vars:?}"
                );
            }
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let lang = lang_fixture(dir.path());
        let cwd = dir.path().join("cwd");
        for file in [&rel, &blank] {
            let file = cwd.join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "[codex.session]\nbad_args = \"from cwd\"\n").unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME])
            .current_dir(&cwd)
            .env(CHILD_ENV, &lang)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(
            stdout.contains("1 passed"),
            "자식이 시험을 실행하지 않았다: {stdout}"
        );
    }

    /// 내장 언어의 사용자 override와 언어팩의 플러그인 번역을 `from_plugin_env`처럼 읽는다.
    #[test]
    fn the_translator_applies_the_user_files_of_the_parent_home() {
        const KEY: &str = "codex.session.bad_args";
        let dir = tempfile::tempdir().unwrap();
        let lang = lang_fixture(dir.path());
        let home = dir.path().join("home");
        let plugin_id = crate::PLUGIN_ID;
        let builtin = home.join(format!("lang/plugins/{plugin_id}/ja.toml"));
        let pack = home.join(format!("lang/fr/plugins/{plugin_id}.toml"));
        for (file, value) in [(&builtin, "user ja"), (&pack, "pack fr")] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, format!("[codex.session]\nbad_args = \"{value}\"\n")).unwrap();
        }
        let home = home.to_str().unwrap();
        for (setting, expected) in [("ja", "user ja"), ("fr", "pack fr"), ("ko", "installed ko")] {
            let vars = [("TASTY_LOCALE", setting), ("TASTY_PARENT_HOME", home)];
            assert_eq!(
                translator_from(&lang, env_of(&vars)).t(KEY),
                expected,
                "{vars:?}"
            );
        }
        let no_home = [("TASTY_LOCALE", "ja")];
        assert_eq!(
            translator_from(&lang, env_of(&no_home)).t(KEY),
            "installed ja",
            "부모 홈이 없으면 사용자 파일을 읽지 않는다"
        );
    }

    #[test]
    fn arguments_accept_only_a_thread_a_prompt_file_and_policy() {
        let a = parse_args(&s(&[
            "--resume",
            "01A12F74-DAB9-7A00-8000-0123456789AB",
            "--prompt-file",
            "/tmp/p",
            "--",
            "-a",
            "never",
        ]))
        .unwrap();
        assert_eq!(
            a.resume.as_deref(),
            Some("01a12f74-dab9-7a00-8000-0123456789ab")
        );
        assert_eq!(a.tui_args, s(&["-a", "never"]));
        assert!(parse_args(&s(&["--resume", "x"])).is_err());
        assert!(parse_args(&s(&["--", "-c", "x"])).is_err());
        assert!(parse_args(&s(&["--surface", "1"])).is_err());
    }

    #[test]
    fn the_tui_attaches_to_the_owned_endpoint_with_the_policy_and_thread() {
        let args = parse_args(&s(&[
            "--resume",
            "01a12f74-dab9-7a00-8000-0123456789ab",
            "--",
            "-a",
            "never",
            "-s",
            "read-only",
        ]))
        .unwrap();
        let cmd = tui_command("unix:///x/s.sock", None, &args, Some("go"));
        let argv: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            s(&[
                "resume",
                "--remote",
                "unix:///x/s.sock",
                "-c",
                "check_for_update_on_startup=false",
                "01a12f74-dab9-7a00-8000-0123456789ab",
                "go"
            ]),
            "재개 TUI에는 정책 재정의를 넘기지 않는다(0.154 remote resume이 거부한다)"
        );
        let fresh = parse_args(&s(&["--", "-a", "never", "-s", "read-only"])).unwrap();
        let argv: Vec<_> = tui_command("unix:///x/s.sock", None, &fresh, None)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv.ends_with(&s(&["-a", "never", "-s", "read-only"])),
            "새 세션은 정책을 TUI 인자로 적용한다: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.contains("bypass-hook-trust")),
            "훅 검토를 우회하지 않는다"
        );
        let ws = tui_command("ws://127.0.0.1:5", Some("t"), &Args::default(), None);
        let argv: Vec<_> = ws
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv.windows(2)
                .any(|w| w == ["--remote-auth-token-env", REMOTE_TOKEN_ENV])
        );
        assert!(
            !argv.iter().any(|a| a == "t"),
            "token은 인자로 넘기지 않는다"
        );
    }

    #[test]
    fn the_server_carries_the_generation_and_websocket_auth() {
        let opened = Opened {
            generation: "0123456789abcdef".into(),
            state_dir: "/x".into(),
            listen: "ws://127.0.0.1:0".into(),
            token_file: Some("/x/token".into()),
        };
        let cmd = server_command(&opened);
        let argv: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            s(&[
                "app-server",
                "--listen",
                "ws://127.0.0.1:0",
                "--ws-auth",
                "capability-token",
                "--ws-token-file",
                "/x/token"
            ])
        );
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == GEN_ENV && v == Some(std::ffi::OsStr::new("0123456789abcdef")))
        );
    }

    #[test]
    fn the_listening_port_is_read_from_the_observed_banner() {
        let log = "codex app-server (WebSockets)\n  listening on: ws://127.0.0.1:44949\n  readyz: http://127.0.0.1:44949/readyz\n";
        assert_eq!(listening_port(log), Some(44949));
        assert_eq!(listening_port("nothing"), None);
    }

    #[test]
    fn a_session_open_response_must_name_an_absolute_private_state() {
        // 절대 경로 형식은 OS마다 다르다(Windows 는 드라이브가 필요하다).
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().to_string_lossy();
        let ok =
            serde_json::json!({"gen":"0123456789abcdef","state_dir":abs,"listen":"unix:///a/s"});
        assert!(parse_opened(&ok).is_ok());
        let rel = serde_json::json!({"gen":"0123456789abcdef","state_dir":"a","listen":"x"});
        assert!(parse_opened(&rel).is_err());
        let generation = serde_json::json!({"gen":"../x","state_dir":abs,"listen":"x"});
        assert!(parse_opened(&generation).is_err());
    }

    #[cfg(unix)]
    fn phase(dir: &Path) -> HelperPhase {
        store::read_json::<HelperState>(&helper_file(dir))
            .unwrap()
            .unwrap()
            .phase
    }

    #[cfg(unix)]
    #[test]
    fn the_server_lives_exactly_as_long_as_the_tui() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Command::new("sleep");
        server.arg("30");
        let marker = dir.path().join("tui-saw-ready");
        let state = dir.path().to_path_buf();
        let status = run_lifecycle(
            dir.path(),
            server,
            Box::new(|_| Some("unix:///owned".into())),
            Duration::from_secs(5),
            |url| {
                assert_eq!(url, "unix:///owned");
                assert_eq!(phase(&state), HelperPhase::Ready);
                std::fs::write(&marker, "1").unwrap();
                let mut tui = Command::new("sh");
                tui.args(["-c", "exit 3"]);
                tui
            },
        )
        .unwrap();
        assert_eq!(status.code(), Some(3), "TUI 종료 코드를 그대로 돌려준다");
        assert!(marker.exists());
        assert_eq!(phase(dir.path()), HelperPhase::Exited);
    }

    #[cfg(unix)]
    #[test]
    fn a_server_that_never_gets_ready_fails_without_starting_the_tui() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Command::new("sh");
        server.args(["-c", "echo boom >&2; exit 4"]);
        let err = run_lifecycle(
            dir.path(),
            server,
            Box::new(|_| None),
            Duration::from_secs(5),
            |_| panic!("준비되지 않은 서버에 TUI를 붙이면 안 된다"),
        )
        .unwrap_err();
        assert!(
            matches!(err, LifecycleError::Server(ref d) if d.contains("exited")),
            "{err:?}"
        );
        assert_eq!(phase(dir.path()), HelperPhase::Failed);
        assert!(
            std::fs::read_to_string(dir.path().join("server.log"))
                .unwrap()
                .contains("boom")
        );

        let mut slow = Command::new("sleep");
        slow.arg("30");
        let started = Instant::now();
        let err = run_lifecycle(
            dir.path(),
            slow,
            Box::new(|_| None),
            Duration::from_millis(300),
            |_| panic!("시간 초과 뒤에도 TUI를 시작하면 안 된다"),
        )
        .unwrap_err();
        assert!(
            matches!(err, LifecycleError::Server(ref d) if d.contains("not ready")),
            "{err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "소유한 서버를 정리하고 돌아온다"
        );
    }

    /// 재개 준비(정책 적용)가 실패하면 서버를 정리하고 `failed`를 남기며 TUI를 띄우지 않는다.
    #[cfg(unix)]
    #[test]
    fn a_failed_resume_cleans_up_the_server_without_starting_the_tui() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("server.pid");
        let mut server = Command::new("sh");
        server.args(["-c", "echo $$ > \"$0\"; exec sleep 30"]);
        server.arg(&pid_file);
        let written = pid_file.clone();
        let err = run_lifecycle_with(
            dir.path(),
            server,
            // 서버가 pid를 기록한 뒤에 준비된 것으로 본다.
            Box::new(move |_| {
                std::fs::read_to_string(&written)
                    .ok()
                    .filter(|p| p.ends_with('\n'))
                    .map(|_| "unix:///owned".into())
            }),
            Duration::from_secs(5),
            |_| Err("the server kept approval policy on-request, requested never".into()),
            |_| panic!("재개를 확인하지 못했는데 TUI를 시작하면 안 된다"),
        )
        .unwrap_err();
        assert!(
            matches!(err, LifecycleError::Resume(ref d) if d.contains("approval policy")),
            "{err:?}"
        );
        let state = store::read_json::<HelperState>(&helper_file(dir.path()))
            .unwrap()
            .unwrap();
        assert_eq!(state.phase, HelperPhase::Failed);
        assert!(state.detail.unwrap_or_default().contains("approval policy"));
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "소유 서버가 남으면 안 된다"
        );
    }

    /// 재개를 확인한 thread는 `ready`와 함께 기록되고, TUI가 끝나면 `exited`가 된다.
    #[cfg(unix)]
    #[test]
    fn a_confirmed_resume_is_recorded_with_ready() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Command::new("sleep");
        server.arg("30");
        let state_dir = dir.path().to_path_buf();
        let status = run_lifecycle_with(
            dir.path(),
            server,
            Box::new(|_| Some("unix:///owned".into())),
            Duration::from_secs(5),
            |url| {
                assert_eq!(url, "unix:///owned");
                assert_eq!(
                    phase(&state_dir),
                    HelperPhase::Starting,
                    "준비 확인 전에는 ready가 아니다"
                );
                Ok(Prepared {
                    resumed: Some("019f55e7-3dfa-7a00-8000-0123456789ab".into()),
                    hold: None,
                    relay: None,
                })
            },
            |_| {
                let state = store::read_json::<HelperState>(&helper_file(&state_dir))
                    .unwrap()
                    .unwrap();
                assert_eq!(state.phase, HelperPhase::Ready);
                assert_eq!(
                    state.resumed.as_deref(),
                    Some("019f55e7-3dfa-7a00-8000-0123456789ab")
                );
                let mut tui = Command::new("sh");
                tui.args(["-c", "exit 0"]);
                tui
            },
        )
        .unwrap();
        assert_eq!(status.code(), Some(0));
        assert_eq!(phase(dir.path()), HelperPhase::Exited);
    }

    /// npm `codex`처럼 실제 서버를 자식으로 띄우고 신호를 전달하는 래퍼를 흉내 낸다. 정리 뒤
    /// 실제 서버 역할의 손자 프로세스도 남지 않아야 한다.
    #[cfg(target_os = "linux")]
    #[test]
    fn stopping_a_wrapper_also_ends_the_server_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("inner.pid");
        let mut server = Command::new("sh");
        server.args([
            "-c",
            "trap 'kill -TERM $c; wait $c; exit 0' TERM; sleep 300 & c=$!; echo $c > \"$1\"; wait $c",
            "wrapper",
        ]);
        server.arg(&pid_file);
        let pid_probe = pid_file.clone();
        run_lifecycle(
            dir.path(),
            server,
            Box::new(move |_| {
                std::fs::read_to_string(&pid_probe)
                    .ok()
                    .filter(|p| p.ends_with('\n'))
                    .map(|_| "unix:///owned".into())
            }),
            Duration::from_secs(5),
            |_| {
                let mut tui = Command::new("sh");
                tui.args(["-c", "exit 0"]);
                tui
            },
        )
        .unwrap();
        let inner = std::fs::read_to_string(&pid_file).unwrap();
        let proc_dir = std::path::PathBuf::from(format!("/proc/{}", inner.trim()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while proc_dir.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !proc_dir.exists(),
            "래퍼가 띄운 서버 {} 가 남았다",
            inner.trim()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_codex_binary_is_a_server_failure() {
        let dir = tempfile::tempdir().unwrap();
        let server = Command::new(dir.path().join("no-such-codex"));
        let err = run_lifecycle(
            dir.path(),
            server,
            Box::new(|_| None),
            Duration::from_secs(1),
            |_| panic!("no tui"),
        )
        .unwrap_err();
        assert!(matches!(err, LifecycleError::Server(_)));
        assert_eq!(phase(dir.path()), HelperPhase::Failed);
    }

    /// 실제 codex app-server를 helper와 같은 명령으로 띄우고, 플러그인의 RPC 클라이언트가 소유
    /// 전송로(unix 소켓, token을 요구하는 loopback WebSocket)로 대기열에 참조를 넣는지 확인한다.
    /// 모델 호출은 하지 않는다. 사용자 설정을 건드리지 않도록 격리된 CODEX_HOME을 받는다.
    #[cfg(unix)]
    #[test]
    #[ignore = "실제 codex CLI가 필요하다. TASTY_CODEX_REAL_HOME에 격리된 CODEX_HOME을 준다"]
    fn the_real_app_server_accepts_a_reference_over_owned_transports() {
        use crate::rpc::{Client, Endpoint, RpcError, queue_add, thread_loaded};
        let home = std::env::var_os("TASTY_CODEX_REAL_HOME").expect("TASTY_CODEX_REAL_HOME");
        let dir = tempfile::tempdir().unwrap();
        let mut checked = 0;
        for websocket in [false, true] {
            let token = crate::ids::random_hex(32).unwrap();
            let token_file = dir.path().join(format!("token-{websocket}"));
            std::fs::write(&token_file, &token).unwrap();
            let opened = Opened {
                generation: "0123456789abcdef".into(),
                state_dir: dir.path().into(),
                listen: if websocket {
                    "ws://127.0.0.1:0".into()
                } else {
                    format!("unix://{}/s.sock", dir.path().display())
                },
                token_file: websocket.then(|| token_file.clone()),
            };
            let log_path = dir.path().join(format!("server-{websocket}.log"));
            let log = std::fs::File::create(&log_path).unwrap();
            let mut cmd = server_command(&opened);
            cmd.env("CODEX_HOME", &home)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log);
            let mut server = OwnedServer::spawn(cmd).unwrap();
            let mut probe = if websocket {
                ws_ready()
            } else {
                unix_ready(&opened.listen)
            };
            let deadline = Instant::now() + Duration::from_secs(30);
            let url = loop {
                if let Some(url) = probe(&log_path) {
                    break url;
                }
                assert!(
                    Instant::now() < deadline,
                    "not ready: {}",
                    std::fs::read_to_string(&log_path).unwrap()
                );
                std::thread::sleep(READY_POLL);
            };
            let endpoint =
                super::super::registry::parse_endpoint(&url, opened.token_file.as_deref()).unwrap();
            if let Endpoint::Ws { port, .. } = &endpoint {
                let wrong = Endpoint::Ws {
                    port: *port,
                    token: "0".repeat(64),
                };
                assert!(
                    matches!(
                        Client::connect(&wrong, Duration::from_secs(5)),
                        Err(RpcError::NotSent(_))
                    ),
                    "틀린 token은 연결되지 않아야 한다"
                );
            }
            let mut client = Client::connect(&endpoint, Duration::from_secs(10)).unwrap();
            let started = client
                .call("thread/start", serde_json::json!({ "cwd": dir.path() }))
                .unwrap();
            let thread = started["thread"]["id"].as_str().unwrap().to_string();
            assert!(thread_loaded(&mut client, &thread).unwrap());
            let text =
                crate::delivery::send::reference_text("codex", "turn_ended", &"a".repeat(32));
            let submission = queue_add(&mut client, &thread, &text, "tasty-real-probe").unwrap();
            assert!(submission.is_some(), "대기열 수락 ID가 있어야 한다");
            client.close();
            server.stop();
            tracing::info!("PASS {url}: thread {thread}, submission {submission:?}");
            checked += 1;
        }
        assert_eq!(checked, 2);
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "helper_signal_tests.rs"]
mod signal_tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "helper_resume_tests.rs"]
mod resume_tests;
