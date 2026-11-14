//! Codex 호출자 판별과 부모 해석. 수신처 준비는 Codex 플러그인의 `codex.parent` 가 맡고, 자식 소속과
//! 지금 위치는 호스트에 묻는다.

use serde_json::{Map, Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

/// CLI 가 모든 dynamic plugin 호출에 싣는 호출자 정보의 키.
pub(crate) const CONTEXT_KEY: &str = "_tasty_context";

/// Codex 플러그인을 거쳐 호출한 Codex 대화. 신원 증명이 아니라 결과를 엉뚱한 곳에 보내지 않기 위한 값이다.
#[derive(Debug, Clone)]
pub(crate) struct CodexCaller {
    context: Value,
}

/// 호출이 Codex 대화에서 왔는가. 다른 호출자는 `None` 이며 기존 surface 흐름을 그대로 쓴다.
/// Codex 라고 했는데 thread id 가 잘못됐으면 다른 부모로 대신하지 않고 거절한다.
pub(crate) fn codex_caller(
    params: &Value,
    tr: &Translator,
) -> Result<Option<CodexCaller>, IpcMethodError> {
    let Some(context) = params.get(CONTEXT_KEY) else {
        return Ok(None);
    };
    if context.get("provider").and_then(Value::as_str) != Some("codex") {
        return Ok(None);
    }
    let thread = context
        .get("thread_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !is_uuid(thread) {
        return Err(IpcMethodError::invalid_params(
            tr.t("claude.codex.bad_context"),
        ));
    }
    Ok(Some(CodexCaller {
        context: context.clone(),
    }))
}

pub(super) fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// 호스트 child registry 가 쓰는 부모. 물리 surface 이거나 surface 가 없는 외부 부모 키다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalParent {
    Surface(u32),
    External(String),
}

impl TerminalParent {
    /// 호스트 요청에 부모를 싣는다. 물리 부모 키는 메서드마다 다르다(spawn 은 `parent`, 나머지는 `surface`).
    pub(crate) fn insert(&self, map: &mut Map<String, Value>, physical_key: &str) {
        match self {
            Self::Surface(id) => map.insert(physical_key.into(), json!(id)),
            Self::External(key) => map.insert("parent_external".into(), json!(key)),
        };
    }

    /// 응답에 싣는 부모. 물리 부모는 기존 `parent_surface_id`, 외부 부모는 `parent_external` 이다.
    pub(crate) fn describe(&self, out: &mut Map<String, Value>) {
        match self {
            Self::Surface(id) => out.insert("parent_surface_id".into(), json!(id)),
            Self::External(key) => out.insert("parent_external".into(), json!(key)),
        };
    }
}

/// `codex.parent` 가 준비한 수신처와 그 대화의 터미널 부모.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedParent {
    pub receiver_id: String,
    pub parent: TerminalParent,
}

/// Codex 플러그인에 수신처 준비를 맡긴다. 플러그인이 없거나 꺼져 있으면 그 오류를 그대로 알린다.
pub(crate) fn resolve_parent<H: HostCall>(
    host: &H,
    caller: &CodexCaller,
    tr: &Translator,
) -> Result<ResolvedParent, IpcMethodError> {
    let response = host
        .call("codex.parent", json!({ CONTEXT_KEY: caller.context }))
        .map_err(|e| {
            let mut error = IpcMethodError::from(e);
            error.message = tr.t_fmt("claude.codex.parent_failed", &error.message);
            error
        })?;
    parse_parent(&response).ok_or_else(|| {
        IpcMethodError::new(tr.t_fmt("claude.codex.parent_malformed", &response.to_string()))
    })
}

pub(super) fn parse_parent(response: &Value) -> Option<ResolvedParent> {
    let receiver_id = response
        .get("receiver_id")
        .and_then(Value::as_str)
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))?
        .to_string();
    let parent = response.get("terminal_parent")?;
    let surface = parent.get("parent");
    let external = parent.get("parent_external");
    let parent = match (surface, external) {
        (Some(id), None) => TerminalParent::Surface(u32::try_from(id.as_u64()?).ok()?),
        (None, Some(key)) => {
            TerminalParent::External(key.as_str().filter(|s| !s.is_empty())?.to_string())
        }
        _ => return None,
    };
    Some(ResolvedParent {
        receiver_id,
        parent,
    })
}

/// 자식을 관리할 부모. 일반 호출은 기존처럼 명시값이나 `TASTY_SURFACE_ID` 기본값을 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParentTarget {
    Legacy(Option<u32>),
    Codex(ResolvedParent),
}

impl ParentTarget {
    pub(crate) fn insert(&self, map: &mut Map<String, Value>, physical_key: &str) {
        match self {
            Self::Legacy(Some(id)) => {
                map.insert(physical_key.into(), json!(id));
            }
            Self::Legacy(None) => {}
            Self::Codex(resolved) => resolved.parent.insert(map, physical_key),
        }
    }
}

/// 요청의 부모를 정한다. Codex 호출이면 자기 id 인자 없이 `codex.parent` 로 부모를 구한다.
/// 이때 명시한 `--surface` 가 그 부모와 다르면 다른 부모의 자식을 건드리지 않도록 거절한다.
pub(crate) fn parent_target<H: HostCall>(
    host: &H,
    params: &Value,
    tr: &Translator,
) -> Result<ParentTarget, IpcMethodError> {
    let explicit = crate::handlers::optional_target_surface(params, tr)?;
    let Some(caller) = codex_caller(params, tr)? else {
        return Ok(ParentTarget::Legacy(explicit));
    };
    let resolved = resolve_parent(host, &caller, tr)?;
    if let Some(surface) = explicit
        && resolved.parent != TerminalParent::Surface(surface)
    {
        return Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.codex.parent_mismatch", &surface.to_string()),
        ));
    }
    Ok(ParentTarget::Codex(resolved))
}

/// 자식 surface 가 이 부모의 자식인가. 다른 부모의 자식에게 결과 수신처를 걸지 않기 위해 확인한다.
pub(crate) fn is_child_of<H: HostCall>(
    host: &H,
    parent: Map<String, Value>,
    child_surface: u32,
) -> Result<bool, IpcMethodError> {
    let params = parent;
    let response = host
        .call("terminal.children", Value::Object(params))
        .map_err(IpcMethodError::from)?;
    Ok(response
        .get("children")
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children
                .iter()
                .any(|c| c.get("surface_id").and_then(Value::as_u64) == Some(child_surface as u64))
        }))
}

/// 호스트가 지금 이 surface 를 둔 workspace id. 호스트의 `surface.list` 는 모든 창의 목록을 모아 돌려주므로
/// 그 안에서 id 가 같은 항목을 찾는다. `surface_id` 인자는 호스트가 읽지 않는다.
/// surface 가 없거나 조회에 실패하면 `None` 이다.
pub(super) fn current_workspace<H: HostCall>(host: &H, surface: u32) -> Option<String> {
    let list = match host.call("surface.list", json!({ "surface_id": surface })) {
        Ok(list) => list,
        Err(e) => {
            tracing::debug!("claude codex return: cannot locate the workspace of s{surface}: {e}");
            return None;
        }
    };
    list.as_array()?
        .iter()
        .find(|s| s.get("id").and_then(Value::as_u64) == Some(u64::from(surface)))?
        .get("workspace_id")?
        .as_u64()
        .map(|w| w.to_string())
}
