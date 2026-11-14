//! attach mirror 로 원격 자식을 다루는 host 호출(`attach.*`)과, 서버에서 받은 원격 출처 호출의 판별.
//!
//! - client(부모 쪽): 대상이 mirror 인지 묻고([`target`]), 같은 namespace 메서드를 서버에서 실행하게
//!   하며([`call`]), 부모 경로를 묶은 뒤 통지를 풀어 준다([`commit`]). 부모 관계·경로·알림 훅은 client 에
//!   두고 서버로 보내지 않는다.
//! - 서버(자식 쪽): host 가 넣은 `parent_external = attach:o<n>` 와 `attach_context` 로 원격 출처 호출을
//!   알아본다([`AttachOrigin::of`]). 이 부모에게는 알림 훅을 걸지 않고, 턴 결과는 [`child_notice`] 로 넘긴다.
//!
//! 계약은 docs/plugins/index.md 의 원격 attach 절에 있다. 메시지와 번역 키는 각 plugin 이 정한다.

use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::host_call::HostCall;

/// `attach.agent_call` 한 번의 응답 상한. host 가 결과를 미루는 상한(35초)보다 길고, 같은 worker 가
/// ping 에 답하지 못해 재시작되는 healthcheck 상한(60초)보다 짧다.
pub const AGENT_CALL_TIMEOUT: Duration = Duration::from_secs(40);
/// host 가 결과를 알 수 없을 때 쓰는 오류 코드.
pub const OUTCOME_UNKNOWN_CODE: i32 = -32010;
/// 이 host 가 원격 자식 호출을 모를 때 오는 오류 코드(JSON-RPC method not found).
const METHOD_NOT_FOUND: i32 = -32601;

/// host 가 원격 출처 호출에 넣는 키. client 가 보낸 같은 이름은 서버 host 가 지운다.
pub const CONTEXT_KEY: &str = "attach_context";
/// host 가 확인해 넣는 새 자식 cwd 상속 원본. client View 가 고른 원격 surface 또는 상속 안 함이다.
pub const INHERIT_CWD_KEY: &str = "inherit_cwd";
/// tell·kill 대상으로 host 가 확인한 서버 쪽 자식 surface.
pub const TARGET_KEY: &str = "attach_target_surface";
/// client plugin 이 서버 plugin 에 턴 결과를 돌려보내 달라고 알리는 plugin 인자. 부모가 Codex 대화일 때만 넣는다.
/// 값은 client 가 그 spawn·tell 에 발급한 불투명 route 토큰이다. 서버는 그 지시로 시작한 턴의 결과에 토큰을 그대로
/// 실어 돌려보내고, client 는 토큰으로 그때의 수신처를 찾는다. 수신처 자체는 서버로 보내지 않는다.
pub const RETURN_ROUTE_KEY: &str = "return_route";
/// 돌려보내는 결과 본문에서 route 토큰을 싣는 키.
pub const ROUTE_FIELD: &str = "route";

static TOKEN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 새 route 토큰(32 hex). 추측을 막는 비밀이 아니라 다른 spawn·tell 과 겹치지 않는 이름이다.
pub fn new_route_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TOKEN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let half = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u64(seq);
        h.write_u32(std::process::id());
        h.write_u64(salt);
        h.finish()
    };
    format!("{:016x}{:016x}", half(1), half(2))
}

/// route 토큰 형식인가. 서버에서 받은 값을 파일 이름·키로 쓰기 전에 확인한다.
pub fn is_route_token(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 대상의 위치.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 이 Tasty 의 대상이다. 기존 경로로 처리한다.
    Local,
    /// attach mirror 다. `anchor_surface_id` 는 실행 위치를 정하는 mirror surface 이고, `child` 는 질의한
    /// surface 가 이 client 에 등록된 원격 자식인지다.
    Remote { anchor_surface_id: u32, child: bool },
}

/// 원격 대상 확인 실패.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// 서버 Tasty 가 원격 자식 기능을 지원하지 않는다(구버전).
    Unsupported,
    /// host 질의가 실패했다.
    Host(String),
}

/// `attach.agent_target` 로 대상이 mirror 인지 묻는다. `query` 는 workspace·pane·surface_id 중 하나 이상이다.
/// 이 호출을 모르는 host 에는 mirror 원격 실행이 없으므로 로컬로 본다.
pub fn target<H: HostCall>(host: &H, query: Map<String, Value>) -> Result<Target, TargetError> {
    let response = match host.call("attach.agent_target", Value::Object(query)) {
        Ok(r) => r,
        Err(tasty_plugin_sdk::PluginError::HostCall {
            code: Some(METHOD_NOT_FOUND),
            ..
        }) => return Ok(Target::Local),
        Err(e) => return Err(TargetError::Host(e.to_string())),
    };
    parse_target(&response)
}

fn parse_target(response: &Value) -> Result<Target, TargetError> {
    if response.get("remote").and_then(Value::as_bool) != Some(true) {
        return Ok(Target::Local);
    }
    if response.get("capable").and_then(Value::as_bool) != Some(true) {
        return Err(TargetError::Unsupported);
    }
    let anchor = u32_of(response, "anchor_surface_id")
        .ok_or_else(|| TargetError::Host(format!("malformed attach.agent_target: {response}")))?;
    Ok(Target::Remote {
        anchor_surface_id: anchor,
        child: response.get("child").and_then(Value::as_bool) == Some(true),
    })
}

fn u32_of(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// client 에서 등록할 새 자식의 부모.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parent {
    Surface(u32),
    External(String),
}

impl Parent {
    fn json(&self) -> Value {
        match self {
            Self::Surface(id) => json!({ "surface": id }),
            Self::External(key) => json!({ "external": key }),
        }
    }
}

/// 원격 실행 요청 하나.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// 접두사 없는 메서드 이름. namespace 는 host 가 호출한 plugin 으로 정한다.
    pub method: &'static str,
    pub anchor_surface_id: u32,
    /// tell·kill 이 다룰 기존 원격 자식(client surface).
    pub target_surface_id: Option<u32>,
    /// 서버 plugin 메서드 인자. 각 plugin 의 허용 목록으로 만든다.
    pub params: Map<String, Value>,
    /// spawn 에서 새 자식을 등록할 부모.
    pub parent: Option<Parent>,
    pub role: Option<Value>,
    pub nickname: Option<Value>,
}

/// 원격 call 로 생긴 자식. `surface_id` 는 client mirror surface 다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub surface_id: u32,
    pub remote_surface_id: u32,
    pub index: Option<u32>,
}

/// 원격 call 의 성공 결과.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub call_id: u64,
    /// 서버 plugin 응답 그대로. surface 번호는 서버 기준이다.
    pub result: Value,
    pub children: Vec<Child>,
}

/// 원격 call 실패.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// 서버에서 실행됐는지 알 수 없다(상한 초과·재연결 실패·서버 재시작). 성공이나 실패로 단정하지 않는다.
    Unknown(String),
    /// 서버 plugin 이나 holder 검사가 거절했다.
    Failed { code: Option<i32>, message: String },
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(m) | Self::Failed { message: m, .. } => f.write_str(m),
        }
    }
}

/// 서버에서 같은 namespace 메서드를 실행하고 결과를 기다린다. [`AGENT_CALL_TIMEOUT`] 안에 답이 없으면
/// 결과불명이다.
pub fn call<H: HostCall>(host: &H, request: Request) -> Result<Reply, CallError> {
    let mut body = json!({
        "method": request.method,
        "anchor_surface_id": request.anchor_surface_id,
        "params": Value::Object(request.params),
    });
    if let Some(t) = request.target_surface_id {
        body["target_surface_id"] = json!(t);
    }
    if let Some(p) = &request.parent {
        body["parent"] = p.json();
    }
    for (key, value) in [("role", request.role), ("nickname", request.nickname)] {
        if let Some(v) = value.filter(|v| !v.is_null()) {
            body[key] = v;
        }
    }
    match host.call_within("attach.agent_call", body, AGENT_CALL_TIMEOUT) {
        Ok(response) => parse_reply(&response),
        Err(tasty_plugin_sdk::PluginError::HostCall {
            code: Some(OUTCOME_UNKNOWN_CODE),
            message,
            ..
        }) => Err(CallError::Unknown(message)),
        Err(tasty_plugin_sdk::PluginError::HostCall { code, message, .. }) => {
            Err(CallError::Failed { code, message })
        }
        Err(e) => Err(CallError::Unknown(e.to_string())),
    }
}

fn parse_reply(response: &Value) -> Result<Reply, CallError> {
    let malformed = || CallError::Unknown(format!("malformed attach.agent_call reply: {response}"));
    let call_id = response
        .get("call_id")
        .and_then(Value::as_u64)
        .ok_or_else(malformed)?;
    let children = response
        .get("children")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| {
                    Some(Child {
                        surface_id: u32_of(c, "surface_id")?,
                        remote_surface_id: u32_of(c, "remote_surface_id")?,
                        index: u32_of(c, "index"),
                    })
                })
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or(Some(Vec::new()))
        .ok_or_else(malformed)?;
    Ok(Reply {
        call_id,
        result: response.get("result").cloned().unwrap_or(Value::Null),
        children,
    })
}

/// 부모 경로를 묶었으니 이 call 의 자식 통지를 보내도 된다고 알린다. 묶을 것이 없어도 불러야 한다.
/// 실패하면 통지가 서버에 보관만 되므로 호출자는 결과에 경고를 싣는다.
pub fn commit<H: HostCall>(host: &H, call_id: u64) -> Result<(), String> {
    host.call("attach.agent_commit", json!({ "call_id": call_id }))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 서버 plugin 이 받은 원격 출처 호출.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachOrigin {
    /// 공개 부모 키(`attach:o<n>`). host child registry 에서 이 출처의 자식을 찾는 데 쓴다.
    pub parent_key: String,
    /// host 가 이 출처의 자식으로 확인한 tell·kill 대상.
    pub target_surface: Option<u32>,
    /// 부모가 Codex 대화라 턴 결과를 돌려보내야 할 때 client 가 준 route 토큰. 결과에 그대로 싣는다.
    pub return_route: Option<String>,
}

impl AttachOrigin {
    /// 원격 출처 호출이면 그 정보다. `attach:` 부모 키와 host 문맥 토큰이 함께 있어야 한다. 공개 IPC 는
    /// `attach:` 부모를 쓸 수 없으므로 문맥 없이 키만 온 호출은 원격 출처로 보지 않는다.
    pub fn of(params: &Value) -> Option<Self> {
        let key = params.get("parent_external").and_then(Value::as_str)?;
        if !key.starts_with("attach:") {
            return None;
        }
        params
            .get(CONTEXT_KEY)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?;
        Some(Self {
            parent_key: key.to_string(),
            target_surface: u32_of(params, TARGET_KEY),
            return_route: params
                .get(RETURN_ROUTE_KEY)
                .and_then(Value::as_str)
                .filter(|t| is_route_token(t))
                .map(str::to_string),
        })
    }

    /// host `terminal.spawn` 에 원격 출처 부모·위치·문맥과 새 자식 cwd 상속 원본을 그대로 싣는다.
    pub fn spawn_request(params: &Value, map: &mut Map<String, Value>) {
        for key in [
            "parent_external",
            "workspace",
            "pane",
            CONTEXT_KEY,
            INHERIT_CWD_KEY,
        ] {
            if let Some(v) = params.get(key).filter(|v| !v.is_null()) {
                map.insert(key.into(), v.clone());
            }
        }
    }
}

/// `attach.child_notice` 의 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeStatus {
    /// 쌓였거나 아직 client 가 확인하지 않았다. 같은 key 로 다시 물어야 한다.
    Pending,
    /// client plugin 이 기록(또는 이미 기록)했다.
    Delivered,
    /// 받을 수 없는 통지다. 다시 보내지 않는다.
    Rejected(String),
}

/// 원격 부모에게 갈 결과를 서버 host 에 맡기거나 그 상태를 묻는다. 같은 key 는 새로 쌓이지 않는다.
/// 영속 기록은 [`NoticeStatus::Delivered`]·[`NoticeStatus::Rejected`] 를 받은 뒤에만 지운다.
/// `attach.child_notice` 한 번의 상한. 이 호출은 host 에 기록을 맡기거나 상태만 물으므로 오래 걸릴 일이 없다.
/// worker 에서 불러도 healthcheck(60초)에 닿지 않도록 짧게 둔다. 넘으면 pending 과 같이 다시 묻는다.
pub const NOTICE_TIMEOUT: Duration = Duration::from_secs(15);

/// 원격 kill 응답이 자식을 닫았다고 보고했는가. `Ok` 응답이어도 대상을 찾지 못했으면 거짓이다.
pub fn killed(result: &Value) -> bool {
    result.get("killed").and_then(Value::as_bool) == Some(true)
        || result
            .get("killed_surface_id")
            .is_some_and(|v| !v.is_null())
}

pub fn child_notice<H: HostCall>(
    host: &H,
    surface_id: u32,
    key: &str,
    payload: Value,
) -> Result<NoticeStatus, String> {
    let response = host
        .call_within(
            "attach.child_notice",
            json!({ "surface_id": surface_id, "key": key, "payload": payload }),
            NOTICE_TIMEOUT,
        )
        .map_err(|e| e.to_string())?;
    parse_notice_status(&response)
}

fn parse_notice_status(response: &Value) -> Result<NoticeStatus, String> {
    match response.get("status").and_then(Value::as_str) {
        Some("pending") => Ok(NoticeStatus::Pending),
        Some("delivered") => Ok(NoticeStatus::Delivered),
        Some("rejected") => Ok(NoticeStatus::Rejected(
            response
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )),
        _ => Err(format!("malformed attach.child_notice reply: {response}")),
    }
}

/// client plugin 의 수신 메서드 응답.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    Recorded,
    Duplicate,
    Rejected(String),
}

impl Received {
    pub fn json(&self) -> Value {
        match self {
            Self::Recorded => json!({ "status": "recorded" }),
            Self::Duplicate => json!({ "status": "duplicate" }),
            Self::Rejected(reason) => json!({ "status": "rejected", "reason": reason }),
        }
    }
}

/// 원격 결과의 client 쪽 완료 키. 서버가 만든 안정 키에 접두사를 붙여 client 로컬 키와 겹치지 않게 한다.
/// 재전송은 같은 키가 되어 원장이 중복으로 거른다.
pub fn local_completion_key(remote_key: &str) -> String {
    format!("r.{remote_key}")
}

/// 수신 메서드 인자: 받은 mirror surface 와 plugin 결과.
pub fn received(params: &Value) -> Option<(u32, &Value)> {
    Some((u32_of(params, "surface_id")?, params.get("payload")?))
}

/// 원격 출처 spawn 의 경로 인자가 서버에서 쓸 수 없는 이유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProblem {
    /// 절대 경로가 아니다. 서버 프로세스의 작업 위치로 풀면 다른 파일을 가리킬 수 있다.
    NotAbsolute { key: &'static str, path: String },
    /// 이 서버에 그 디렉터리(`cwd`)나 파일(`profile_file`)이 없다.
    Missing { key: &'static str, path: String },
}

/// 원격 출처 spawn 의 `cwd`·`profile_file` 은 서버 기준 절대 경로여야 하고 서버에 있어야 한다. 서버가 터미널을
/// 만들기 전에 확인한다.
pub fn server_path_problem(params: &Value) -> Option<PathProblem> {
    for (key, want_dir) in [("cwd", true), ("profile_file", false)] {
        let Some(path) = params
            .get(key)
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
        else {
            continue;
        };
        let p = std::path::Path::new(path);
        let path = path.to_string();
        if !p.is_absolute() {
            return Some(PathProblem::NotAbsolute { key, path });
        }
        if (want_dir && !p.is_dir()) || (!want_dir && !p.is_file()) {
            return Some(PathProblem::Missing { key, path });
        }
    }
    None
}

/// attach 원격 부모의 공개 키 접두어.
pub const PARENT_PREFIX: &str = "attach:";

/// 공개 호출로 자식을 바꾸는 메서드(namespace 뒤 이름). attach 원격 자식에는 host 가 넘긴 원격 출처 호출만 쓸 수 있다.
pub const GUARDED_METHODS: &[&str] = &[
    "tell",
    "kill",
    "respawn",
    "reboot",
    "child_profile",
    "broadcast",
];

/// 공개 호출이 attach 원격 자식을 바꾸려 해서 거절하는 이유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// 대상 surface 의 부모가 attach 원격 부모다.
    Child(u32),
    /// attach 원격 부모 키를 직접 지정했다.
    Parent,
}

fn names_attach_parent(params: &Value) -> bool {
    params
        .get("parent_external")
        .and_then(Value::as_str)
        .is_some_and(|k| k.starts_with(PARENT_PREFIX))
}

/// 원격 출처 호출의 대상 자식. host 가 확인한 대상(`attach_target_surface`)이 지금도 그 출처 부모의 자식일 때만 준다.
/// 공개 호출이 출처 키를 꾸며 넣어도 그 부모의 자식이 아닌 surface 에는 닿지 않는다.
pub fn origin_child<H: HostCall>(
    host: &H,
    origin: &AttachOrigin,
) -> Result<Option<u32>, tasty_plugin_sdk::PluginError> {
    let Some(target) = origin.target_surface else {
        return Ok(None);
    };
    let parent = host.call("terminal.parent", json!({ "surface": target }))?;
    let key = parent.get("parent_external").and_then(Value::as_str);
    Ok((key == Some(origin.parent_key.as_str())).then_some(target))
}

/// `surface` 의 host 부모가 attach 원격 부모인가.
pub fn is_attach_child<H: HostCall>(
    host: &H,
    surface: u32,
) -> Result<bool, tasty_plugin_sdk::PluginError> {
    let parent = host.call("terminal.parent", json!({ "surface": surface }))?;
    Ok(names_attach_parent(&parent))
}

/// 서버의 공개 호출(사용자 CLI·다른 도구)이 attach 원격 자식을 바꾸는지 본다. host 가 attach 점유를 생성 plugin 에만 풀어
/// 주므로, 같은 plugin 을 거친 서버 로컬 호출이 점유를 우회하지 못하게 plugin 이 막는다. 원격 출처(`AttachOrigin`)의
/// tell·kill 은 [`AttachOrigin`] 처리 쪽이 따로 대상을 확인하므로 여기서는 통과시킨다. plugin 의 내부 동작(auto_resume·
/// 훅)은 공개 호출이 아니므로 대상이 아니다.
pub fn refuse_public<H: HostCall>(
    host: &H,
    method: &str,
    params: &Value,
) -> Result<Option<Refusal>, tasty_plugin_sdk::PluginError> {
    if !GUARDED_METHODS.contains(&method) {
        return Ok(None);
    }
    if matches!(method, "tell" | "kill") && AttachOrigin::of(params).is_some() {
        return Ok(None);
    }
    if names_attach_parent(params) {
        return Ok(Some(Refusal::Parent));
    }
    if matches!(method, "tell" | "reboot") {
        for key in ["surface", "surface_id"] {
            if let Some(surface) = u32_of(params, key)
                && is_attach_child(host, surface)?
            {
                return Ok(Some(Refusal::Child(surface)));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tasty_plugin_sdk::PluginError;

    type Reply = Box<dyn Fn(&str) -> Result<Value, PluginError>>;

    struct Host {
        calls: RefCell<Vec<(String, Value)>>,
        reply: Reply,
    }

    impl HostCall for Host {
        fn call(&self, method: &str, params: Value) -> Result<Value, PluginError> {
            self.calls.borrow_mut().push((method.into(), params));
            (self.reply)(method)
        }
    }

    fn host(reply: impl Fn(&str) -> Result<Value, PluginError> + 'static) -> Host {
        Host {
            calls: RefCell::new(Vec::new()),
            reply: Box::new(reply),
        }
    }

    fn rpc(code: i32, message: &str) -> PluginError {
        PluginError::HostCall {
            method: "m".into(),
            message: message.into(),
            code: Some(code),
        }
    }

    #[test]
    fn a_host_without_the_call_and_a_local_target_stay_local() {
        let old = host(|_| Err(rpc(METHOD_NOT_FOUND, "unknown method")));
        assert_eq!(target(&old, Map::new()), Ok(Target::Local));
        let local = host(|_| Ok(json!({ "remote": false })));
        assert_eq!(target(&local, Map::new()), Ok(Target::Local));
    }

    #[test]
    fn an_old_server_is_refused_before_anything_is_created() {
        let h = host(|_| Ok(json!({ "remote": true, "capable": false, "anchor_surface_id": 4 })));
        assert_eq!(target(&h, Map::new()), Err(TargetError::Unsupported));
        let broken = host(|_| Err(rpc(-32000, "boom")));
        assert!(matches!(
            target(&broken, Map::new()),
            Err(TargetError::Host(_))
        ));
    }

    #[test]
    fn a_mirror_target_names_its_anchor_and_child_flag() {
        let h = host(|_| {
            Ok(
                json!({ "remote": true, "capable": true, "workspace": 2, "anchor_surface_id": 12, "child": true }),
            )
        });
        assert_eq!(
            target(&h, Map::new()),
            Ok(Target::Remote {
                anchor_surface_id: 12,
                child: true
            })
        );
    }

    fn spawn_request() -> Request {
        Request {
            method: "spawn",
            anchor_surface_id: 12,
            target_surface_id: None,
            params: Map::from_iter([("prompt".to_string(), json!("hi"))]),
            parent: Some(Parent::External("codex:t".into())),
            role: Some(json!("r")),
            nickname: None,
        }
    }

    #[test]
    fn a_call_sends_the_parent_and_reads_the_mirror_children() {
        let h = host(|_| {
            Ok(json!({
                "call_id": 7,
                "result": { "child_surface_id": 99 },
                "children": [{ "surface_id": 15, "remote_surface_id": 99, "index": 2 }],
            }))
        });
        let reply = call(&h, spawn_request()).unwrap();
        assert_eq!(reply.call_id, 7);
        assert_eq!(
            reply.children,
            vec![Child {
                surface_id: 15,
                remote_surface_id: 99,
                index: Some(2)
            }]
        );
        let (method, body) = h.calls.borrow()[0].clone();
        assert_eq!(method, "attach.agent_call");
        assert_eq!(body["parent"], json!({ "external": "codex:t" }));
        assert_eq!(body["role"], json!("r"));
        assert!(body.get("nickname").is_none());
        assert!(body.get("target_surface_id").is_none());
    }

    #[test]
    fn an_unknown_outcome_is_not_a_failure_or_a_success() {
        let h = host(|_| Err(rpc(OUTCOME_UNKNOWN_CODE, "call 7 outcome unknown")));
        assert_eq!(
            call(&h, spawn_request()),
            Err(CallError::Unknown("call 7 outcome unknown".into()))
        );
        let timeout = host(|_| {
            Err(PluginError::HostCallTimeout {
                method: "attach.agent_call".into(),
                timeout: AGENT_CALL_TIMEOUT,
            })
        });
        assert!(matches!(
            call(&timeout, spawn_request()),
            Err(CallError::Unknown(_))
        ));
        let refused = host(|_| Err(rpc(-32602, "not holder")));
        assert!(matches!(
            call(&refused, spawn_request()),
            Err(CallError::Failed { .. })
        ));
        let malformed = host(|_| Ok(json!({ "result": {} })));
        assert!(matches!(
            call(&malformed, spawn_request()),
            Err(CallError::Unknown(_))
        ));
    }

    /// host 가 넣은 원격 출처 값과 cwd 상속 원본을 `terminal.spawn` 에 그대로 넘긴다. 빈 값은 싣지 않는다.
    #[test]
    fn the_spawn_request_carries_the_host_cwd_source() {
        let params = json!({
            "parent_external": "attach:o3",
            "workspace": "2",
            "pane": 5,
            "attach_context": "tok",
            "inherit_cwd": {"surface": 9},
            "prompt": "hi",
        });
        let mut map = Map::new();
        AttachOrigin::spawn_request(&params, &mut map);
        assert_eq!(
            Value::Object(map),
            json!({
                "parent_external": "attach:o3",
                "workspace": "2",
                "pane": 5,
                "attach_context": "tok",
                "inherit_cwd": {"surface": 9},
            })
        );
        let mut map = Map::new();
        AttachOrigin::spawn_request(&json!({ "inherit_cwd": null }), &mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn only_a_parent_key_with_a_host_context_is_an_attach_origin() {
        let p = json!({
            "parent_external": "attach:o3",
            "attach_context": "tok",
            "attach_target_surface": 9,
            "return_route": "0123456789abcdef0123456789abcdef",
        });
        assert_eq!(
            AttachOrigin::of(&p),
            Some(AttachOrigin {
                parent_key: "attach:o3".into(),
                target_surface: Some(9),
                return_route: Some("0123456789abcdef0123456789abcdef".into()),
            })
        );
        let mut bad = p.clone();
        bad["return_route"] = json!("../x");
        assert_eq!(AttachOrigin::of(&bad).unwrap().return_route, None);
        let a = new_route_token();
        assert!(is_route_token(&a));
        assert_ne!(a, new_route_token());
        assert_eq!(
            AttachOrigin::of(&json!({ "parent_external": "attach:o3" })),
            None
        );
        assert_eq!(
            AttachOrigin::of(&json!({ "parent_external": "codex:t", "attach_context": "tok" })),
            None
        );
    }

    #[test]
    fn notice_statuses_are_read_strictly() {
        assert_eq!(
            parse_notice_status(&json!({ "status": "pending" })),
            Ok(NoticeStatus::Pending)
        );
        assert_eq!(
            parse_notice_status(&json!({ "status": "delivered" })),
            Ok(NoticeStatus::Delivered)
        );
        assert_eq!(
            parse_notice_status(&json!({ "status": "rejected", "reason": "gone" })),
            Ok(NoticeStatus::Rejected("gone".into()))
        );
        assert!(parse_notice_status(&json!({})).is_err());
    }

    #[test]
    fn the_local_key_is_prefixed_and_stable() {
        assert_eq!(local_completion_key("claude:a:t1"), "r.claude:a:t1");
    }

    #[test]
    fn public_calls_cannot_change_attach_children() {
        let attach_child = host(|m| {
            assert_eq!(m, "terminal.parent");
            Ok(json!({ "parent_external": "attach:o3", "status": "active" }))
        });
        assert_eq!(
            refuse_public(
                &attach_child,
                "tell",
                &json!({ "surface": 9, "message": "x" })
            )
            .unwrap(),
            Some(Refusal::Child(9))
        );
        assert_eq!(
            refuse_public(&attach_child, "reboot", &json!({ "surface": 9 })).unwrap(),
            Some(Refusal::Child(9))
        );
        for m in ["kill", "respawn", "child_profile", "broadcast"] {
            assert_eq!(
                refuse_public(
                    &attach_child,
                    m,
                    &json!({ "parent_external": "attach:o3", "child": 1 })
                )
                .unwrap(),
                Some(Refusal::Parent),
                "{m}"
            );
        }
        // host 가 넣은 원격 출처의 tell·kill 은 출처 처리 쪽이 대상을 확인한다.
        let origin = json!({ "parent_external": "attach:o3", "attach_context": "c", "attach_target_surface": 9 });
        assert_eq!(refuse_public(&attach_child, "tell", &origin).unwrap(), None);
        assert_eq!(refuse_public(&attach_child, "kill", &origin).unwrap(), None);
        // 출처 키를 실어도 respawn·reboot 등은 원격 출처 메서드가 아니다.
        assert_eq!(
            refuse_public(&attach_child, "respawn", &origin).unwrap(),
            Some(Refusal::Parent)
        );
        // 다른 메서드는 보지 않는다.
        assert_eq!(
            refuse_public(&attach_child, "children", &json!({ "surface": 9 })).unwrap(),
            None
        );

        let local_child = host(|_| Ok(json!({ "parent_surface_id": 5, "parent_external": null })));
        assert_eq!(
            refuse_public(
                &local_child,
                "tell",
                &json!({ "surface": 9, "message": "x" })
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn server_paths_must_be_absolute_and_present() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.json");
        std::fs::write(&file, "{}").unwrap();
        let d = dir.path().to_string_lossy().to_string();
        let f = file.to_string_lossy().to_string();
        assert_eq!(
            server_path_problem(&json!({ "cwd": d, "profile_file": f })),
            None
        );
        assert_eq!(server_path_problem(&json!({})), None);
        assert!(matches!(
            server_path_problem(&json!({ "cwd": "work" })),
            Some(PathProblem::NotAbsolute { key: "cwd", .. })
        ));
        assert!(matches!(
            server_path_problem(&json!({ "profile_file": "p.json" })),
            Some(PathProblem::NotAbsolute {
                key: "profile_file",
                ..
            })
        ));
        assert!(matches!(
            server_path_problem(&json!({ "cwd": f })),
            Some(PathProblem::Missing { key: "cwd", .. })
        ));
        assert!(matches!(
            server_path_problem(&json!({ "profile_file": d })),
            Some(PathProblem::Missing {
                key: "profile_file",
                ..
            })
        ));
    }
}
