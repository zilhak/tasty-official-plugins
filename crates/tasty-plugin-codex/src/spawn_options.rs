//! Narrow, invocation-only model overrides. Provider model availability belongs to Codex.
use serde_json::Value;
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

pub(crate) const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];
pub(crate) const EFFORT_KEY: &str = "model_reasoning_effort";

pub(crate) fn model_ok(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':'))
}

pub(crate) fn effort_config(value: &str) -> Option<&str> {
    value
        .strip_prefix("model_reasoning_effort=")
        .filter(|e| EFFORTS.contains(e))
}

pub(crate) fn parse(params: &Value, tr: &Translator) -> Result<Vec<String>, IpcMethodError> {
    let mut args = Vec::new();
    for key in ["model", "effort"] {
        let Some(raw) = params.get(key) else { continue };
        let value = raw
            .as_str()
            .filter(|v| {
                if key == "model" {
                    model_ok(v)
                } else {
                    EFFORTS.contains(v)
                }
            })
            .ok_or_else(|| {
                IpcMethodError::invalid_params(&tr.t_replace(
                    "codex.params.invalid_model_option",
                    "{key}",
                    key,
                ))
            })?;
        if key == "model" {
            args.extend(["-m".into(), value.into()]);
        } else {
            args.extend(["-c".into(), format!("{EFFORT_KEY}={value}")]);
        }
    }
    Ok(args)
}

/// Preserve only explicit session overrides when reboot replaces the permission policy.
pub(crate) fn saved_tokens(session: &crate::session::registry::Session) -> Vec<String> {
    let settings = crate::session::command::ResumeSettings::from_tui_args(&session.tui_args);
    let mut args = Vec::new();
    if let Some(model) = settings.model.filter(|m| model_ok(m)) {
        args.extend(["-m".into(), model]);
    }
    if let Some(effort) = settings.effort {
        args.extend(["-c".into(), format!("{EFFORT_KEY}={effort}")]);
    }
    args
}

#[cfg(test)]
#[path = "spawn_options_tests.rs"]
mod tests;
