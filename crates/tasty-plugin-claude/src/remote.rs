//! attach mirror 의 원격 자식 Claude.
//!
//! - client(부모 쪽): 부모 해석·Codex 결과 경로·완료 알림 훅은 지금처럼 이 Tasty 에서 하고, 자식 생성·입력·종료만
//!   서버의 같은 메서드(`claude.spawn`·`tell`·`kill`)로 보낸다. 서버로 보내는 인자는 허용 목록으로 고른다.
//!   이 Tasty 의 호출 문맥(`_tasty_context`)·부모·수신처는 보내지 않는다.
//! - 서버(자식 쪽): host 가 넣은 원격 출처(`attach:o<n>` + 문맥 토큰)로 받은 호출이다. 서버에 설치된 CLI·설정·
//!   프로필·토큰으로 Claude 를 실행하고 훅도 서버로 보고된다. 이 부모에게는 알림 훅을 걸지 않는다(상태·이벤트는
//!   host 가 중계한다). client 가 route 토큰을 보냈으면 턴 결과를 [`crate::codex_return`] 의 attach 경로로 돌려보낸다.
//! - client 는 부모가 Codex 대화든 물리 surface 든 spawn·tell 마다 route 토큰을 발급한다. 물리 부모의 결과는 본문을
//!   저장한 뒤 그 부모의 완료 로그에 결과 ID 와 조회 명령(`tasty claude remote-results --id`)을 남긴다.
//!
//! 계약: docs/plugins/index.md 의 "원격 attach 자식" 절.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_agent_common::params::forward;
use tasty_plugin_agent_common::remote_attach::{
    self, AttachOrigin, CallError, Parent, RETURN_ROUTE_KEY, Request, Target, TargetError,
};
use tasty_plugin_agent_common::remote_owner::{self, Admit};
use tasty_plugin_agent_common::remote_results;
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::codex_return::{Binding, ParentTarget, Returns, RouteTo, TerminalParent};
use crate::error_scan::{ErrorScanner, ScanTarget};

/// 서버 `claude.spawn` 으로 보내는 인자. 사용자가 명시한 경로(`cwd`·`profile_file`)와 등록 이름(`profile`)은
/// 서버에서 해석·확인한다. 이 Tasty 에서 해석한 설정 경로를 대신 넣지는 않는다.
const SPAWN_KEYS: &[&str] = &[
    "cwd",
    "prompt",
    "profile",
    "profile_file",
    "permission_mode",
    "model",
    "effort",
];

/// 결과불명으로 끝난 spawn 의 부모. host 가 그 call 의 늦은 결과(`late_spawn`)를 주면 이 정보로 경로를 묶는다.
/// host 는 늦은 결과를 최대 10분 다시 주므로 그보다 조금 길게 둔다. plugin 이 다시 시작하면 사라지고, 그때
/// 늦게 생긴 자식은 묶을 부모를 모르므로 닫는다.
struct PendingSpawn {
    target: ParentTarget,
    with_prompt: bool,
    owner: Option<String>,
    at: Instant,
}

const PENDING_KEPT: Duration = Duration::from_secs(15 * 60);

static PENDING: LazyLock<Mutex<HashMap<u64, PendingSpawn>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pending() -> std::sync::MutexGuard<'static, HashMap<u64, PendingSpawn>> {
    PENDING.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("claude remote: pending spawn table was poisoned; continuing with its data");
        poisoned.into_inner()
    })
}

/// host 의 결과불명 메시지에 실린 call 번호(`call <id>`).
fn call_id_in(message: &str) -> Option<u64> {
    let rest = &message[message.find("call ")? + "call ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// 원격 대상 확인 실패를 사용자 오류로 바꾼다.
fn target_error(e: TargetError, tr: &Translator) -> IpcMethodError {
    match e {
        TargetError::Unsupported => IpcMethodError::new(tr.t("claude.remote.unsupported")),
        TargetError::Host(m) => IpcMethodError::new(tr.t_fmt("claude.remote.lookup_failed", &m)),
    }
}

fn call_error(e: CallError, tr: &Translator) -> IpcMethodError {
    match e {
        CallError::Unknown(m) => IpcMethodError::new(tr.t_fmt("claude.remote.outcome_unknown", &m)),
        CallError::Failed { code, message } => {
            let mut error = IpcMethodError::new(message);
            if let Some(code) = code {
                error.code = code;
            }
            error
        }
    }
}

/// spawn 대상(workspace·pane)이 attach mirror 면 실행 위치 surface 를, 이 Tasty 의 대상이면 `None` 을 준다.
/// workspace 이름은 mirror 를 가리킬 수 없으므로 숫자 id 만 묻는다.
pub(crate) fn spawn_anchor<H: HostCall>(
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Option<u32>, IpcMethodError> {
    let mut query = Map::new();
    if let Some(ws) = params
        .get("workspace")
        .and_then(|w| w.as_u64().or_else(|| w.as_str()?.parse().ok()))
    {
        query.insert("workspace".into(), json!(ws));
    }
    if let Some(pane) = params.get("pane").and_then(Value::as_u64) {
        query.insert("pane".into(), json!(pane));
    }
    if query.is_empty() {
        return Ok(None);
    }
    match remote_attach::target(host, query).map_err(|e| target_error(e, tr))? {
        Target::Local => Ok(None),
        Target::Remote {
            anchor_surface_id, ..
        } => Ok(Some(anchor_surface_id)),
    }
}

/// 이 surface 가 이 Tasty 에 등록된 원격 자식이면 실행 위치 surface 를 준다.
pub(crate) fn child_anchor<H: HostCall>(
    host: &H,
    surface: u32,
    tr: &Translator,
) -> Result<Option<u32>, IpcMethodError> {
    let query = Map::from_iter([("surface_id".to_string(), json!(surface))]);
    match remote_attach::target(host, query).map_err(|e| target_error(e, tr))? {
        Target::Remote {
            anchor_surface_id,
            child: true,
        } => Ok(Some(anchor_surface_id)),
        _ => Ok(None),
    }
}

fn parent_of(target: &ParentTarget) -> Option<Parent> {
    match target {
        ParentTarget::Legacy(Some(id)) => Some(Parent::Surface(*id)),
        ParentTarget::Legacy(None) => None,
        ParentTarget::Codex(resolved) => Some(match &resolved.parent {
            TerminalParent::Surface(id) => Parent::Surface(*id),
            TerminalParent::External(key) => Parent::External(key.clone()),
        }),
    }
}

/// client: mirror workspace 에 자식을 만든다. 서버에서 실행한 뒤 이 Tasty 의 mirror 자식에 결과 경로·알림 훅을
/// 묶고, 그다음 통지를 풀어 준다(commit). 응답의 surface·index 는 이 Tasty 기준이다.
pub(crate) fn spawn<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    target: &ParentTarget,
    anchor: u32,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let mut remote = forward(params, SPAWN_KEYS);
    let to = match target {
        ParentTarget::Codex(resolved) => Some(RouteTo::Codex(resolved.receiver_id.clone())),
        ParentTarget::Legacy(Some(parent)) => Some(RouteTo::Surface {
            surface: *parent,
            session: remote_results::parent_session(host, *parent)
                .map_err(|e| IpcMethodError::new(tr.t_fmt("claude.codex.route_not_saved", &e)))?,
        }),
        ParentTarget::Legacy(None) => None,
    };
    let with_prompt = params.get("prompt").and_then(Value::as_str).is_some();
    // 새 자식의 소유자는 만든 부모다. 부모 없이 만든 자식은 prompt 로 이미 턴을 열었으면 소유자 없음으로 고정하고,
    // 아니면 첫 tell 이 맡는다.
    let owner = match &to {
        Some(to) => Some(to.owner()),
        None => with_prompt.then(|| remote_owner::NOBODY.to_string()),
    };
    if let Some(to) = to {
        let token = returns
            .issue_attach_route(&to)
            .map_err(|e| IpcMethodError::new(tr.t_fmt("claude.codex.route_not_saved", &e)))?;
        remote.insert(RETURN_ROUTE_KEY.into(), json!(token));
    }
    let reply = match remote_attach::call(
        host,
        Request {
            method: "spawn",
            anchor_surface_id: anchor,
            target_surface_id: None,
            params: remote,
            parent: parent_of(target),
            role: params.get("role").cloned(),
            nickname: params.get("nickname").cloned(),
        },
    ) {
        Ok(reply) => reply,
        Err(e) => {
            if let CallError::Unknown(message) = &e
                && let Some(call_id) = call_id_in(message)
            {
                let mut table = pending();
                table.retain(|_, p| p.at.elapsed() < PENDING_KEPT);
                table.insert(
                    call_id,
                    PendingSpawn {
                        target: target.clone(),
                        with_prompt,
                        owner: owner.clone(),
                        at: Instant::now(),
                    },
                );
            }
            return Err(call_error(e, tr));
        }
    };
    let Some(child) = reply.children.first().cloned() else {
        // 묶을 자식이 없어도 commit 해 서버가 이 call 의 보관분을 붙들지 않게 한다.
        if let Err(e) = remote_attach::commit(host, reply.call_id) {
            tracing::warn!(
                "claude remote spawn: commit of call {} failed: {e}",
                reply.call_id
            );
        }
        return Err(IpcMethodError::new(
            tr.t_fmt("claude.remote.no_child", &reply.result.to_string()),
        ));
    };
    // 경로를 묶은 뒤에만 통지를 풀어야 첫 완료가 수신처 없이 지나가지 않는다. 묶지 못했으면 commit 하지 않고
    // (통지는 서버에 보관된 채 남는다) 받을 곳이 없는 자식을 닫는다.
    if let Err(e) = bind(
        returns,
        host,
        target,
        child.surface_id,
        with_prompt,
        owner.as_deref(),
        tr,
    ) {
        let cleanup = match remote_attach::call(
            host,
            child_request("kill", anchor, child.surface_id, Map::new()),
        ) {
            Ok(r) if remote_attach::killed(&r.result) => {
                tr.t("claude.remote.child_closed").to_string()
            }
            Ok(r) => tr.t_fmt("claude.remote.child_not_closed", &r.result.to_string()),
            Err(k) => tr.t_fmt("claude.remote.child_not_closed", &k.to_string()),
        };
        return Err(IpcMethodError::new(tr.t_args(
            "claude.remote.bind_failed",
            &[&child.surface_id.to_string(), &e.message, &cleanup],
        )));
    }
    let committed = remote_attach::commit(host, reply.call_id);
    let mut out = Map::new();
    out.insert("child_surface_id".into(), json!(child.surface_id));
    if let Some(index) = child.index {
        out.insert("child_index".into(), json!(index));
    }
    out.insert("remote".into(), json!(true));
    out.insert(
        "remote_child_surface_id".into(),
        json!(child.remote_surface_id),
    );
    for key in ["requested_session_options", "warning"] {
        if let Some(v) = reply.result.get(key).filter(|v| !v.is_null()) {
            out.insert(key.into(), v.clone());
        }
    }
    crate::handlers::describe_parent(target, &mut out);
    if let Err(e) = committed {
        out.insert(
            "warning".into(),
            json!(tr.t_fmt("claude.remote.commit_failed", &e)),
        );
    }
    Ok(Value::Object(out))
}

/// client: 새 mirror 자식에 부모 경로를 묶는다. Codex 부모는 결과 경로, 물리 부모는 완료 알림 훅이다.
fn bind<H: HostCall>(
    returns: &Returns,
    host: &H,
    target: &ParentTarget,
    child: u32,
    with_prompt: bool,
    owner: Option<&str>,
    tr: &Translator,
) -> Result<(), IpcMethodError> {
    // 새 자식이므로 같은 surface id 에 남은 기록과 관계없이 소유자를 새로 쓴다.
    returns
        .owners_dir()
        .ok_or_else(|| tr.t("claude.codex.no_data_dir").to_string())
        .and_then(|dir| remote_owner::reset(&dir, child, owner))
        .map_err(|e| IpcMethodError::new(tr.t_fmt("claude.codex.route_not_saved", &e)))?;
    match target {
        ParentTarget::Codex(resolved) => returns
            .bind_new_child(child, &Binding::new(resolved), with_prompt)
            .map_err(|e| {
                IpcMethodError::new(tr.t_args(
                    "claude.codex.child_route_not_saved",
                    &[&child.to_string(), &e],
                ))
            }),
        ParentTarget::Legacy(Some(parent)) => {
            crate::notifications::register_notify_hooks(host, *parent, child, "spawn");
            Ok(())
        }
        ParentTarget::Legacy(None) => Ok(()),
    }
}

/// client: 원격 자식에게 보내는 tell 을 판정한다. 자식의 소유자가 다른 부모면 그 턴의 결과가 이 지시의 토큰으로
/// 가지 않도록 경로를 바꾸거나 보내기 전에 거절한다. 소유자가 없으면 이 부모가 맡는다.
pub(crate) fn admit(
    returns: &Returns,
    child: u32,
    owner: &str,
    tr: &Translator,
) -> Result<Admit, IpcMethodError> {
    let dir = returns
        .owners_dir()
        .ok_or_else(|| IpcMethodError::new(tr.t("claude.codex.no_data_dir")))?;
    match remote_owner::admit(&dir, child, owner) {
        Ok(Admit::Other(current)) => Err(IpcMethodError::invalid_params(&tr.t_args(
            "claude.remote.child_of_other_parent",
            &[&child.to_string(), &current],
        ))),
        Ok(Admit::Unrecorded) => Err(IpcMethodError::invalid_params(
            &tr.t_args("claude.remote.child_without_owner", &[&child.to_string()]),
        )),
        Ok(admit) => Ok(admit),
        Err(e) => Err(IpcMethodError::new(
            tr.t_fmt("claude.codex.route_not_saved", &e),
        )),
    }
}

/// 이 tell 로 맡은 자식의 전송이 확정 실패했다. 되돌리지 못하면 경고만 남긴다(그 부모가 계속 소유한다).
pub(crate) fn unclaim(returns: &Returns, child: u32, owner: &str) {
    let Some(dir) = returns.owners_dir() else {
        return;
    };
    if let Err(e) = remote_owner::unclaim(&dir, child, owner) {
        tracing::warn!("claude remote owner: could not release s{child}: {e}");
    }
}

/// client: `claude.remote_receive`. 원격 결과(`notice`)는 결과 경로로 기록하고, 결과불명 뒤 늦게 생긴
/// 자식(`late_spawn`)은 경로를 묶거나 닫는다. 이 처리 안에서 claude namespace 를 다시 부르지 않는다.
pub(crate) fn receive<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    match params.get("kind").and_then(Value::as_str) {
        Some("late_spawn") => Ok(late_spawn(returns, host, params, tr).json()),
        _ => returns.remote_receive(host, params, tr),
    }
}

fn late_spawn<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    tr: &Translator,
) -> remote_attach::Received {
    use remote_attach::Received;
    let children: Vec<u32> = params
        .get("children")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("surface_id").and_then(Value::as_u64))
                .filter_map(|n| u32::try_from(n).ok())
                .collect()
        })
        .unwrap_or_default();
    let call_id = params.get("call_id").and_then(Value::as_u64);
    let waiting = call_id.and_then(|id| pending().remove(&id));
    let failed = params.get("error").is_some_and(|e| !e.is_null());
    let reason = match (&waiting, failed, children.as_slice()) {
        (_, _, []) => Some("the late result has no child".to_string()),
        (None, _, _) => {
            Some("no parent is waiting for this call (plugin restarted or expired)".into())
        }
        (Some(_), true, _) => Some("the server reported an error for this call".into()),
        (Some(_), false, [_]) => None,
        (Some(_), false, _) => Some("a spawn call cannot create several children".into()),
    };
    if let (None, Some(p)) = (&reason, &waiting) {
        match bind(
            returns,
            host,
            &p.target,
            children[0],
            p.with_prompt,
            p.owner.as_deref(),
            tr,
        ) {
            Ok(()) => return Received::Recorded,
            Err(e) => return discard(host, &children, e.message, tr),
        }
    }
    discard(host, &children, reason.unwrap_or_default(), tr)
}

/// 묶을 수 없는 늦은 자식을 닫고 거절한다. host 는 commit 하지 않는다.
fn discard<H: HostCall>(
    host: &H,
    children: &[u32],
    reason: String,
    tr: &Translator,
) -> remote_attach::Received {
    for &child in children {
        let closed = child_anchor(host, child, tr).and_then(|anchor| match anchor {
            Some(anchor) => {
                remote_attach::call(host, child_request("kill", anchor, child, Map::new()))
                    .map(|_| ())
                    .map_err(|e| call_error(e, tr))
            }
            None => Ok(()),
        });
        if let Err(e) = closed {
            tracing::warn!(
                "claude remote: late child s{child} could not be closed: {}",
                e.message
            );
        }
    }
    tracing::warn!("claude remote: late spawn refused: {reason}");
    remote_attach::Received::Rejected(reason)
}

/// client: 원격 자식에게 입력을 보낸다. 결과 경로는 호출자가 이미 정했다(경로 먼저, 입력 나중).
/// 원격 tell 의 실패. 서버가 실행했는지 모르는 결과불명은 확정된 실패와 구분한다.
pub(crate) struct TellError {
    pub error: IpcMethodError,
    /// 서버에서 실행됐을 수 있다. 호출자는 새로 정한 결과 경로와 턴 소유를 되돌리지 않는다.
    pub unknown: bool,
}

pub(crate) fn tell<H: HostCall>(
    host: &H,
    anchor: u32,
    child: u32,
    message: &str,
    route: Option<String>,
    tr: &Translator,
) -> Result<Value, TellError> {
    let mut params = Map::from_iter([("message".to_string(), json!(message))]);
    if let Some(token) = route {
        params.insert(RETURN_ROUTE_KEY.to_string(), json!(token));
    }
    let reply =
        remote_attach::call(host, child_request("tell", anchor, child, params)).map_err(|e| {
            TellError {
                unknown: matches!(e, CallError::Unknown(_)),
                error: call_error(e, tr),
            }
        })?;
    let mut out = match reply.result {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    out.insert("surface".into(), json!(child));
    out.insert("remote".into(), json!(true));
    Ok(Value::Object(out))
}

fn child_request(
    method: &'static str,
    anchor: u32,
    child: u32,
    params: Map<String, Value>,
) -> Request {
    Request {
        method,
        anchor_surface_id: anchor,
        target_surface_id: Some(child),
        params,
        parent: None,
        role: None,
        nickname: None,
    }
}

/// client: 원격 자식을 닫는다. 결과 경로는 서버에 남은 결과가 늦게 와도 기록되도록 끝난 경로로만 둔다.
pub(crate) fn kill<H: HostCall>(
    returns: &Returns,
    host: &H,
    anchor: u32,
    child: u32,
    index: u32,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let reply = remote_attach::call(host, child_request("kill", anchor, child, Map::new()))
        .map_err(|e| call_error(e, tr))?;
    let killed = reply.result.get("killed").and_then(Value::as_bool) == Some(true);
    if killed {
        returns.end_remote_child(child);
        if let Some(dir) = returns.owners_dir()
            && let Err(e) = remote_owner::forget(&dir, child)
        {
            tracing::warn!("claude remote owner: could not forget the closed s{child}: {e}");
        }
    }
    let mut out = Map::new();
    out.insert("killed".into(), json!(killed));
    if killed {
        out.insert("killed_surface_id".into(), json!(child));
    }
    out.insert("child_index".into(), json!(index));
    out.insert("remote".into(), json!(true));
    Ok(Value::Object(out))
}

/// 이 index 의 자식 surface. 부모 인자는 `terminal.kill` 에 보낼 것과 같다.
pub(crate) fn child_by_index<H: HostCall>(
    host: &H,
    parent: &Map<String, Value>,
    index: u32,
) -> Option<u32> {
    let mut p = parent.clone();
    p.remove("child");
    let resp = host.call("terminal.children", Value::Object(p)).ok()?;
    resp.get("children")?.as_array()?.iter().find_map(|c| {
        (c.get("index").and_then(Value::as_u64) == Some(u64::from(index)))
            .then(|| c.get("surface_id").and_then(Value::as_u64))
            .flatten()
            .and_then(|n| u32::try_from(n).ok())
    })
}

fn path_error(problem: remote_attach::PathProblem, tr: &Translator) -> IpcMethodError {
    let (key, flag, path) = match problem {
        remote_attach::PathProblem::NotAbsolute { key, path } => {
            ("claude.remote.path_not_absolute", key, path)
        }
        remote_attach::PathProblem::Missing { key, path } => {
            ("claude.remote.path_missing", key, path)
        }
    };
    let flag = format!("--{}", flag.replace('_', "-"));
    IpcMethodError::invalid_params(&tr.t_args(key, &[&flag, &path]))
}

/// 서버: 원격 출처 부모의 새 자식을 이 Tasty 의 설정·CLI 로 실행한다.
pub(crate) fn serve_spawn<H: HostCall>(
    scanner: &Arc<Mutex<ErrorScanner>>,
    returns: &Returns,
    host: &H,
    params: &Value,
    origin: &AttachOrigin,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    // 원격 부모가 준 경로는 이 서버 기준 절대 경로여야 한다. 서버 프로세스의 작업 위치로 풀지 않는다.
    if let Some(problem) = remote_attach::server_path_problem(params) {
        return Err(path_error(problem, tr));
    }
    let options = crate::session_options::SessionOptions::parse(params, tr)?;
    let prompt = params.get("prompt").and_then(Value::as_str);
    let profile_file = crate::handlers::resolve_profile_file_param(data_dir, params, tr)?;
    let permission_mode =
        crate::handlers::resolve_permission_mode(host, params, profile_file.as_deref(), tr)?;
    if origin.return_route.is_some() && !returns.is_available() {
        return Err(IpcMethodError::new(tr.t("claude.codex.no_data_dir")));
    }
    let mut sp = forward(params, &["cwd", "role", "nickname"]);
    AttachOrigin::spawn_request(params, &mut sp);
    let resp = host
        .call("terminal.spawn", Value::Object(sp))
        .map_err(IpcMethodError::from)?;
    let child = resp
        .get("child_surface_id")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| {
            IpcMethodError::new(
                tr.t_fmt("claude.spawn.missing_child_surface_id", &resp.to_string()),
            )
        })?;
    if let Some(route) = &origin.return_route {
        returns
            .bind_new_child(
                child,
                &Binding::attach(&origin.parent_key, route),
                prompt.is_some(),
            )
            .map_err(|e| {
                IpcMethodError::new(tr.t_args(
                    "claude.codex.child_route_not_saved",
                    &[&child.to_string(), &e],
                ))
            })?;
    }
    crate::handlers::start_claude_with_options(
        host,
        child,
        prompt,
        profile_file.as_deref(),
        permission_mode.as_deref(),
        &options,
    )?;
    crate::retrack::enable_and_record(host, scanner, child, ScanTarget::Child);
    let mut out = resp;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("parent_external".into(), json!(origin.parent_key));
        obj.insert("requested_session_options".into(), options.json());
        if !matches!(crate::install::completion_hooks_installed(tr), Ok(true)) {
            obj.insert("warning".into(), json!(tr.t("claude.remote.hooks_missing")));
        }
    }
    Ok(out)
}

fn origin_target<H: HostCall>(
    host: &H,
    origin: &AttachOrigin,
    tr: &Translator,
) -> Result<u32, IpcMethodError> {
    remote_attach::origin_child(host, origin)
        .map_err(IpcMethodError::from)?
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.remote.no_target")))
}

/// 서버: 원격 출처 부모가 자기 자식에게 보낸 입력. 결과 경로를 먼저 정한 뒤 보낸다.
pub(crate) fn serve_tell<H: HostCall>(
    returns: &Returns,
    host: &H,
    params: &Value,
    origin: &AttachOrigin,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let child = origin_target(host, origin, tr)?;
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_message")))?;
    if let Some(route) = &origin.return_route {
        if !returns.is_available() {
            return Err(IpcMethodError::new(tr.t("claude.codex.no_data_dir")));
        }
        returns
            .bind_tell(child, &Binding::attach(&origin.parent_key, route))
            .map_err(|e| IpcMethodError::new(tr.t_fmt("claude.codex.route_not_saved", &e)))?;
    } else {
        crate::handlers::release_codex_route(returns, child, tr)?;
    }
    host.call(
        "terminal.tell",
        json!({ "surface": child, "text": message }),
    )
    .map_err(IpcMethodError::from)
}

/// 서버: 원격 출처 부모의 자식을 닫는다. 이미 claim 한 결과는 보낼 요청으로 남아 계속 전달된다.
pub(crate) fn serve_kill<H: HostCall>(
    scanner: &Arc<Mutex<ErrorScanner>>,
    returns: &Returns,
    host: &H,
    params: &Value,
    origin: &AttachOrigin,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let child = origin_target(host, origin, tr)?;
    let mut parent = forward(params, &["workspace"]);
    parent.insert("parent_external".into(), json!(origin.parent_key));
    let index = child_by_index_of_surface(host, &parent, child)
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.remote.no_target")))?;
    let mut kp = parent;
    kp.insert("child".into(), json!(index));
    let resp = host
        .call("terminal.kill", Value::Object(kp))
        .map_err(IpcMethodError::from)?;
    let killed = resp
        .get("killed_surface_id")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    if let Some(killed) = killed {
        crate::error_scan::lock_scanner(scanner).stop_tracking(host, killed);
        returns.forget_child(killed);
    }
    let mut out = resp;
    if let Value::Object(m) = &mut out {
        m.insert("killed".into(), json!(killed.is_some()));
    }
    Ok(out)
}

/// 이 부모 아래에서 surface 의 index.
fn child_by_index_of_surface<H: HostCall>(
    host: &H,
    parent: &Map<String, Value>,
    surface: u32,
) -> Option<u32> {
    let resp = host
        .call("terminal.children", Value::Object(parent.clone()))
        .ok()?;
    resp.get("children")?.as_array()?.iter().find_map(|c| {
        (c.get("surface_id").and_then(Value::as_u64) == Some(u64::from(surface)))
            .then(|| c.get("index").and_then(Value::as_u64))
            .flatten()
            .and_then(|n| u32::try_from(n).ok())
    })
}

/// 서버의 공개 호출이 attach 원격 자식을 바꾸려 하면 거절한다([`remote_attach::refuse_public`]).
pub(crate) fn refuse_public<H: HostCall>(
    host: &H,
    method: &str,
    params: &Value,
    tr: &Translator,
) -> Result<(), IpcMethodError> {
    match remote_attach::refuse_public(host, method, params) {
        Ok(None) => Ok(()),
        Ok(Some(remote_attach::Refusal::Child(surface))) => Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.remote.attach_child", &surface.to_string()),
        )),
        Ok(Some(remote_attach::Refusal::Parent)) => Err(IpcMethodError::invalid_params(
            tr.t("claude.remote.attach_parent"),
        )),
        Err(e) => Err(IpcMethodError::new(
            tr.t_fmt("claude.remote.lookup_failed", &e.to_string()),
        )),
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
