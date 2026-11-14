//! Per-child launch after validating invocation overrides.
use super::*;

pub(crate) fn handle_spawn<H: HostCall>(
    host: &H,
    params: &Value,
    tr: &Translator,
    delivery: &DeliveryCtx<'_>,
) -> Result<Value, IpcMethodError> {
    if let Some(origin) = AttachOrigin::of(params) {
        return super::remote::serve_spawn(host, params, tr, delivery, &origin);
    }
    let mut args = crate::spawn_options::parse(params, tr)?;
    let policy = resolve_policy_args(host, params, tr)?;
    let mut policy = crate::session::command::policy_tokens(&policy);
    policy.append(&mut args);
    let caller = codex_caller_of(params)?;
    let prompt = optional_str(params, "prompt");

    // 먼저 자식 터미널을 만들고 아래에서 Codex 기동 명령을 보낸다.
    let mut sp = forward(params, &["workspace", "pane", "cwd", "role", "nickname"]);
    // Codex 부모는 spawn 전에 수신처를 준비한다. 받을 수 없는 부모면 자식을 만들지 않는다.
    // parent는 경고 집계에 쓰는 terminal.children 부모 인자다.
    let (receiver, mut parent) = match &caller {
        Some(c) => {
            let (receiver, terminal_parent) = crate::delivery::prepare_receiver(delivery, c)?;
            // 호스트 관계와 반환 대상이 갈리지 않도록 다른 부모의 명시는 받지 않는다.
            ownership::reject_other_parent(delivery, params, c, tr)?;
            let parent = terminal_parent.as_object().cloned().unwrap_or_default();
            sp.extend(parent.clone());
            (Some(receiver), parent)
        }
        None => {
            let parent = require_target_surface(params, tr)?;
            sp.insert("parent".into(), json!(parent));
            (
                None,
                Map::from_iter([("surface".to_string(), json!(parent))]),
            )
        }
    };
    if let Some(anchor) = super::remote::spawn_anchor(host, params, tr)? {
        let receiver_id = receiver.as_ref().map(|r| r.receiver_id.as_str());
        return super::remote::spawn(host, params, tr, delivery, anchor, receiver_id, &parent);
    }
    let resp = host_call(host, "terminal.spawn", Value::Object(sp))?;
    let child_sid = resp
        .get("child_surface_id")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| {
            IpcMethodError::new(tr.t_replace(
                "codex.spawn.missing_child_surface_id",
                "{resp}",
                &resp.to_string(),
            ))
        })?;
    match &receiver {
        Some(receiver) => set_child_return(delivery, child_sid, child_index_of(&resp), receiver)?,
        None => ownership::invalidate_route(delivery, child_sid)?,
    }

    let cmd = make_codex_command_with_args(child_sid, prompt.as_deref(), &policy)?;
    host_call(
        host,
        "surface.send",
        json!({"surface_id": child_sid, "text": cmd}),
    )?;

    // 수신자가 있으면 결과는 부모 대화로 간다. 상태 로그 훅은 surface 부모의 기존 경로다.
    if receiver.is_none()
        && let Some(parent) = parent.get("surface").and_then(Value::as_u64)
    {
        register_notify_hooks(host, parent as u32, child_sid, "spawn");
    }

    // 경고 기준을 넘어도 spawn은 성공으로 반환한다. 외부 부모의 자식 목록은 창을 고르도록
    // spawn한 workspace를 함께 넘긴다.
    if receiver.is_some()
        && let Some(ws) = params.get("workspace").filter(|w| !w.is_null())
    {
        parent.insert("workspace".into(), ws.clone());
    }
    let mut out = resp;
    if let Some(warning) = compute_spawn_warning(host, Value::Object(parent), tr)
        && let Some(obj) = out.as_object_mut()
    {
        obj.insert("warning".into(), json!(warning));
    }

    Ok(out)
}
