//! Inspect the envelope without allocating a JSON tree for catalogs, tool results or history.
use super::{Bind, Chosen, Pending, ResumeSettings, apply_settings, is_permission_choice};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::io;
use std::sync::atomic::Ordering;

#[derive(Deserialize)]
struct Envelope<'a> {
    id: Option<Value>,
    method: Option<String>,
    #[serde(borrow)]
    params: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    result: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    error: Option<&'a RawValue>,
}

fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(deserializer).map(Some)
}

fn invalid_json(error: serde_json::Error) -> io::Error {
    // Do not log the payload or a deserializer's value-bearing error text.
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "invalid relay JSON at line {}, column {} ({:?})",
            error.line(),
            error.column(),
            error.classify()
        ),
    )
}

pub(super) fn request(
    text: &str,
    defaults: &ResumeSettings,
    chosen: &Chosen,
    pending: &Pending,
) -> io::Result<Option<String>> {
    let envelope: Envelope<'_> = serde_json::from_str(text).map_err(invalid_json)?;
    let method = envelope.method.as_deref();
    if !matches!(
        method,
        Some("thread/settings/update" | "thread/start" | "thread/resume" | "thread/fork")
    ) {
        return Ok(None);
    }
    let mut params: Value = match envelope.params {
        Some(raw) => serde_json::from_str(raw.get()).map_err(invalid_json)?,
        None => Value::Null,
    };
    if method == Some("thread/settings/update") {
        if is_permission_choice(&params) {
            chosen.store(true, Ordering::Release);
        }
        return Ok(None);
    }
    if params["ephemeral"] == true {
        return Ok(None);
    }
    let params = params
        .as_object_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing thread parameters"))?;
    let effective = apply_settings(params, defaults, chosen.load(Ordering::Acquire));
    pending
        .lock()
        .map_err(|_| io::Error::other("relay pending poisoned"))?
        .insert(
            envelope.id.as_ref().unwrap_or(&Value::Null).to_string(),
            effective,
        );
    // Only policy-bearing thread requests need rewriting. Preserve other top-level fields.
    let mut message: Value = serde_json::from_str(text).map_err(invalid_json)?;
    message["params"] = Value::Object(std::mem::take(params));
    Ok(Some(message.to_string()))
}

pub(super) fn reply(text: &str, pending: &Pending, bind: &Bind) -> io::Result<Option<String>> {
    let envelope: Envelope<'_> = serde_json::from_str(text).map_err(invalid_json)?;
    if envelope.result.is_none() && envelope.error.is_none() {
        return Ok(None);
    }
    let settings = pending
        .lock()
        .map_err(|_| io::Error::other("relay pending poisoned"))?
        .remove(&envelope.id.as_ref().unwrap_or(&Value::Null).to_string());
    if let (Some(settings), Some(raw)) = (settings, envelope.result) {
        let result: Value = serde_json::from_str(raw.get()).map_err(invalid_json)?;
        if let Err(error) = settings.verify(&result).and_then(|()| bind(&result)) {
            return Ok(Some(
                json!({"id":envelope.id,"error":{"code":-32000,"message":error}}).to_string(),
            ));
        }
    }
    Ok(None)
}
