//! Codex 호출자가 관리할 수 있는 자식과 그 반환 경로를 정한다.
//!
//! Codex 대화는 호스트가 그 대화(`codex:<thread>`)의 자식으로 기록한 surface만 관리한다. 같은
//! helper surface에서 `/new`로 바뀐 다른 대화도 다른 부모다. 다른 부모를 명시하면 거절한다. 반환 경로는 자식에게 지시를 보내기 전에 기록하고, Codex 대화가 아닌 호출자가 새
//! 지시를 보내면 지시 전에 지운다. 이미 기록한 결과는 `events/`에 남는다.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::caller::CallerContext;
use crate::delivery::Ctx as DeliveryCtx;
use crate::delivery::ledger::{self, ChildReturn};
use crate::handlers::optional_target_surface;

/// Codex 부모의 호스트 부모 키 `codex:<thread>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentKey(String);

impl ParentKey {
    /// terminal.children·kill·respawn·broadcast·spawn에 넣는 부모 인자.
    pub(crate) fn put(&self, dst: &mut Map<String, Value>) {
        dst.insert("parent_external".into(), json!(self.0));
    }

    /// terminal.parent 응답이 이 부모를 가리키는지. 응답에 `parent_external` 필드가 없는 호스트는
    /// 소유를 증명하지 못하므로 거짓이다. 같은 surface에서 실행 중이어도 숫자 부모는 이 부모가 아니다.
    fn is_parent_in(&self, resp: &Value) -> bool {
        resp.get("status").and_then(Value::as_str) == Some("active")
            && resp.get("parent_external").and_then(Value::as_str) == Some(self.0.as_str())
    }
}

fn host_err(e: tasty_plugin_sdk::PluginError) -> IpcMethodError {
    IpcMethodError::from(e)
}

/// Codex 호출자의 호스트 부모 키. 언제나 `codex:<thread>` 외부 부모다.
pub(crate) fn caller_key(caller: &CallerContext) -> ParentKey {
    ParentKey(crate::delivery::external_parent_key(&caller.thread))
}

/// 사용자가 명시한 부모 surface가 호출자가 실행 중인 helper surface가 아니면 거절한다. 자기
/// surface를 명시해도 호스트에는 thread 부모 키를 넘긴다.
pub(crate) fn reject_other_parent(
    delivery: &DeliveryCtx<'_>,
    params: &Value,
    caller: &CallerContext,
    tr: &Translator,
) -> Result<(), IpcMethodError> {
    if !caller.is_explicit(params, "surface") && !caller.is_explicit(params, "surface_id") {
        return Ok(());
    }
    let Some(explicit) = optional_target_surface(params, tr)? else {
        return Ok(());
    };
    let own = crate::delivery::caller_surface(delivery.root, caller)
        .map_err(|e| IpcMethodError::new(format!("codex session registry: {e}")))?;
    match own {
        Some(own) if own == explicit => Ok(()),
        _ => Err(IpcMethodError::invalid_params(&tr.t_replace(
            "codex.owner.other_parent",
            "{surface}",
            &explicit.to_string(),
        ))),
    }
}

/// 호스트가 child를 이 부모의 자식으로 기록하고 있는지.
pub(crate) fn owns_child<H: HostCall>(
    host: &H,
    key: &ParentKey,
    child: u32,
) -> Result<bool, tasty_plugin_sdk::PluginError> {
    let resp = host.call("terminal.parent", json!({ "surface": child }))?;
    Ok(key.is_parent_in(&resp))
}

/// 결과를 Codex 훅으로 돌려줄 수 있는 surface인지. helper 세션이 열린 적 있거나 Codex 반환
/// 경로가 이미 있으면 Codex 자식이다.
pub(crate) fn is_codex_child(delivery: &DeliveryCtx<'_>, surface: u32) -> bool {
    let session = crate::session::registry::current_gen(delivery.root, surface);
    let record = ledger::child_receiver(delivery.root, surface);
    matches!(session, Ok(Some(_))) || matches!(record, Ok(Some(_)))
}

fn record_err(e: std::io::Error) -> IpcMethodError {
    IpcMethodError::new(format!("codex child return record: {e}"))
}

/// Codex 대화가 아닌 호출자가 새 지시를 보내기 전에 이전 Codex 반환 경로를 지운다.
pub(crate) fn invalidate_route(
    delivery: &DeliveryCtx<'_>,
    child: u32,
) -> Result<(), IpcMethodError> {
    // 경로보다 먼저 아직 기록하지 않은 후보를 무효화한다. 후보는 Stop 때 고정한 수신자를
    // 가지므로 경로만 지우면 확인이 끝난 뒤 옛 부모에게 기록된다.
    crate::delivery::turns::invalidate_surface(delivery, child).map_err(record_err)?;
    ledger::clear_child_receiver(delivery.root, child).map_err(record_err)
}

/// tell 전에 경로를 기록한다. 전송이 실패하면 [`restore_route`]로 되돌린다.
pub(crate) fn bind_route(
    delivery: &DeliveryCtx<'_>,
    child: u32,
    receiver_id: &str,
) -> Result<Option<ChildReturn>, IpcMethodError> {
    let previous = ledger::child_receiver(delivery.root, child).map_err(record_err)?;
    let index = previous.as_ref().and_then(|p| p.child_index);
    ledger::set_child_receiver(delivery.root, child, receiver_id, index).map_err(record_err)?;
    Ok(previous)
}

pub(crate) fn restore_route(delivery: &DeliveryCtx<'_>, child: u32, previous: Option<ChildReturn>) {
    let restored = match previous {
        Some(p) => ledger::write_child_return(delivery.root, child, &p),
        None => ledger::clear_child_receiver(delivery.root, child),
    };
    if let Err(e) = restored {
        tracing::warn!("codex child return record restore for surface {child}: {e}");
    }
}

/// 외부 부모의 자식이 지금 있는 workspace들. 이 thread로 반환 경로를 기록한 surface 중 호스트가
/// 지금도 그 부모의 자식으로 기록한 것만 센다. index를 주면 그 번호의 자식만 본다.
fn child_workspaces<H: HostCall>(
    host: &H,
    delivery: &DeliveryCtx<'_>,
    key: &ParentKey,
    thread: &str,
    index: Option<u32>,
) -> Result<BTreeSet<u64>, IpcMethodError> {
    let mut receivers: HashMap<String, bool> = HashMap::new();
    let mine: Vec<(u32, ChildReturn)> = ledger::child_records(delivery.root)
        .into_iter()
        .filter(|(_, r)| {
            *receivers.entry(r.receiver_id.clone()).or_insert_with(|| {
                matches!(ledger::read_receiver(delivery.root, &r.receiver_id),
                    Ok(Some(rec)) if rec.thread == thread)
            })
        })
        .filter(|(_, r)| index.is_none() || r.child_index == index)
        .collect();
    if mine.is_empty() {
        return Ok(BTreeSet::new());
    }
    let list = host.call("surface.list", json!({})).map_err(host_err)?;
    let workspace_of: HashMap<u64, u64> = list
        .as_array()
        .or_else(|| list.get("surfaces").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|s| Some((s.get("id")?.as_u64()?, s.get("workspace_id")?.as_u64()?)))
        .collect();
    let mut out = BTreeSet::new();
    for (surface, _) in mine {
        let Some(ws) = workspace_of.get(&u64::from(surface)) else {
            continue;
        };
        if owns_child(host, key, surface).map_err(host_err)? {
            out.insert(*ws);
        }
    }
    Ok(out)
}

/// 외부 부모 요청에 넣을 workspace를 정한다. 호스트의 workspace는 창을 고르는 데만 쓰이고 자식을
/// 거르지 않으므로 요청은 한 번만 보낸다.
///
/// 1. 사용자가 명시한 workspace.
/// 2. 대상 자식(`child` index)이 지금 있는 workspace.
/// 3. 이 부모의 자식이 모두 한 workspace에 있으면 그 workspace.
///
/// 후보가 둘 이상이면 어느 창의 자식인지 정할 수 없어 거절한다. 사용자 포커스나 마지막으로 쓴
/// workspace로 정하지 않는다.
fn workspace_for<H: HostCall>(
    host: &H,
    delivery: &DeliveryCtx<'_>,
    caller: &CallerContext,
    key: &ParentKey,
    params: &Value,
    child: Option<u32>,
    tr: &Translator,
) -> Result<Option<String>, IpcMethodError> {
    if let Some(w) = params
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|w| !w.is_empty())
    {
        return Ok(Some(w.to_string()));
    }
    let mut candidates = BTreeSet::new();
    if child.is_some() {
        candidates = child_workspaces(host, delivery, key, &caller.thread, child)?;
    }
    if candidates.is_empty() {
        candidates = child_workspaces(host, delivery, key, &caller.thread, None)?;
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next().map(|w| w.to_string())),
        _ => {
            let list: Vec<String> = candidates.iter().map(u64::to_string).collect();
            Err(IpcMethodError::invalid_params(&tr.t_replace(
                "codex.owner.ambiguous_workspace",
                "{workspaces}",
                &list.join(", "),
            )))
        }
    }
}

/// Codex 호출자가 부모를 지정하는 요청의 인자. 다른 부모를 명시하면 거절하고, 외부 부모면
/// [`workspace_for`]로 정한 workspace를 넣는다.
pub(crate) fn parent_request<H: HostCall>(
    host: &H,
    delivery: &DeliveryCtx<'_>,
    caller: &CallerContext,
    params: &Value,
    child: Option<u32>,
    base: &Map<String, Value>,
    tr: &Translator,
) -> Result<Map<String, Value>, IpcMethodError> {
    let key = caller_key(caller);
    reject_other_parent(delivery, params, caller, tr)?;
    let mut p = base.clone();
    key.put(&mut p);
    if let Some(ws) = workspace_for(host, delivery, caller, &key, params, child, tr)? {
        p.insert("workspace".into(), json!(ws));
    }
    Ok(p)
}
