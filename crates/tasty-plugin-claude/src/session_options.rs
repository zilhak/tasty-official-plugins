//! Per-spawn Claude invocation options, kept with the session for hook-driven restoration.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::hook::{HostCall as PlannedCall, RESTORE_COMMAND_META_KEY};

pub(crate) const META_KEY: &str = "claude-session-options";
/// The CLI help omits ultracode; the official CLI reference documents it since 2.1.203.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultracode"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl SessionOptions {
    /// A supplied value must be a single non-option token. Model availability belongs to the
    /// provider, so aliases, full IDs, deployment names and inference-profile ARNs are accepted.
    pub fn parse(params: &Value, tr: &Translator) -> Result<Self, IpcMethodError> {
        let read = |key: &str| -> Result<Option<String>, IpcMethodError> {
            let Some(value) = params.get(key) else {
                return Ok(None);
            };
            let Some(s) = value.as_str().filter(|s| {
                !s.is_empty()
                    && !s.starts_with('-')
                    && !s.chars().any(|c| c.is_whitespace() || c.is_control())
            }) else {
                return Err(IpcMethodError::invalid_params(
                    &tr.t_fmt("claude.params.invalid_session_option", key),
                ));
            };
            if key == "effort" && !EFFORTS.contains(&s) {
                return Err(IpcMethodError::invalid_params(
                    &tr.t_fmt("claude.params.invalid_effort", &EFFORTS.join(", ")),
                ));
            }
            Ok(Some(s.to_string()))
        };
        Ok(Self {
            model: read("model")?,
            effort: read("effort")?,
        })
    }

    pub fn json(&self) -> Value {
        let mut out = json!({});
        if let Some(model) = &self.model {
            out["model"] = json!(model);
        }
        if let Some(effort) = &self.effort {
            out["effort"] = json!(effort);
        }
        out
    }

    /// Values are shell data, including provider IDs with punctuation or quotes.
    pub fn flags(&self) -> String {
        let mut out = String::new();
        for (key, value) in [("model", &self.model), ("effort", &self.effort)] {
            if let Some(value) = value {
                out.push_str(&format!(" --{key} {}", posix_single_quote(value)));
            }
        }
        out
    }

    /// Bind pending invocation options to the exact session ID passed to Claude.
    pub fn record<H: HostCall>(
        &self,
        host: &H,
        surface_id: u32,
        session: &str,
    ) -> Result<(), IpcMethodError> {
        host.call(
            "surface.meta.set",
            json!({
                "surface_id": surface_id, "key": META_KEY,
                "value": bound_json(session, true, self).to_string(),
            }),
        )
        .map_err(IpcMethodError::from)?;
        Ok(())
    }
}

fn bound_json(session: &str, pending: bool, options: &SessionOptions) -> Value {
    json!({"schema": 2, "session": session, "pending": pending, "options": options.json()})
}

#[derive(Debug)]
enum SurfaceOptions {
    Missing,
    Legacy(SessionOptions),
    Bound {
        session: String,
        pending: bool,
        options: SessionOptions,
    },
}

fn read_meta<H: HostCall>(
    host: &H,
    surface: u32,
    key: &str,
    tr: &Translator,
) -> Result<Option<String>, IpcMethodError> {
    let reply = host
        .call(
            "surface.meta.get",
            json!({"surface_id": surface, "key": key}),
        )
        .map_err(IpcMethodError::from)?;
    match reply.get("value") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        _ => Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.params.invalid_session_option", key),
        )),
    }
}

fn surface_options<H: HostCall>(
    host: &H,
    surface: u32,
    tr: &Translator,
) -> Result<SurfaceOptions, IpcMethodError> {
    let Some(text) = read_meta(host, surface, META_KEY, tr)? else {
        return Ok(SurfaceOptions::Missing);
    };
    let invalid = || {
        IpcMethodError::invalid_params(&tr.t_fmt("claude.params.invalid_session_option", META_KEY))
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| IpcMethodError::new(e.to_string()))?;
    if !value.is_object() {
        return Err(invalid());
    }
    if let Some(schema) = value.get("schema") {
        if schema != 2 {
            return Err(invalid());
        }
        let session = value
            .get("session")
            .and_then(Value::as_str)
            .filter(|id| tasty_plugin_agent_common::reboot::is_safe_session_id(id))
            .ok_or_else(invalid)?;
        let pending = value
            .get("pending")
            .and_then(Value::as_bool)
            .ok_or_else(invalid)?;
        let options = value
            .get("options")
            .filter(|v| v.is_object())
            .ok_or_else(invalid)?;
        return Ok(SurfaceOptions::Bound {
            session: session.into(),
            pending,
            options: SessionOptions::parse(options, tr)?,
        });
    }
    Ok(SurfaceOptions::Legacy(SessionOptions::parse(&value, tr)?))
}

/// A previous process must not consume a fresh launch's pending selection, including {}.
pub(crate) fn stale_start<H: HostCall>(
    host: &H,
    surface: u32,
    session: &str,
    tr: &Translator,
) -> Result<bool, IpcMethodError> {
    Ok(
        matches!(surface_options(host, surface, tr)?, SurfaceOptions::Bound {session: owner, pending: true, ..} if owner != session),
    )
}

fn record_path(data_dir: Option<&Path>, session: &str) -> Option<PathBuf> {
    if !tasty_plugin_agent_common::reboot::is_safe_session_id(session) {
        return None;
    }
    Some(
        data_dir?
            .join("session-options")
            .join(format!("{session}.json")),
    )
}

/// Only matching session metadata can override a durable session record. Old unbound metadata
/// is trusted only when the pre-existing settings-owner marker identifies this same session.
/// Host/file read errors propagate; only a genuinely absent record means omitted options.
pub(crate) fn load<H: HostCall>(
    host: &H,
    surface: u32,
    data_dir: Option<&Path>,
    session: &str,
    tr: &Translator,
) -> Result<SessionOptions, IpcMethodError> {
    let mut ambiguous_legacy = false;
    match surface_options(host, surface, tr)? {
        SurfaceOptions::Bound {
            session: owner,
            options,
            ..
        } if owner == session => return Ok(options),
        SurfaceOptions::Legacy(options) => {
            let owner = read_meta(
                host,
                surface,
                crate::stop_pairing::SETTINGS_SESSION_META_KEY,
                tr,
            )?;
            if owner
                .as_deref()
                .is_some_and(|id| !tasty_plugin_agent_common::reboot::is_safe_session_id(id))
            {
                return Err(IpcMethodError::invalid_params(
                    tr.t("claude.params.unowned_session_options"),
                ));
            }
            if owner.as_deref() == Some(session) {
                return Ok(options);
            }
            ambiguous_legacy = owner.is_none() && options != SessionOptions::default();
        }
        _ => {}
    }
    let absent = || {
        if ambiguous_legacy {
            Err(IpcMethodError::invalid_params(
                tr.t("claude.params.unowned_session_options"),
            ))
        } else {
            Ok(SessionOptions::default())
        }
    };
    let Some(path) = record_path(data_dir, session) else {
        return absent();
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return absent(),
        Err(e) => return Err(IpcMethodError::new(e.to_string())),
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| IpcMethodError::new(e.to_string()))?;
    if !value.is_object() {
        return Err(IpcMethodError::invalid_params(
            &tr.t_fmt("claude.params.invalid_session_option", META_KEY),
        ));
    }
    SessionOptions::parse(&value, tr)
}

pub(crate) fn store(data_dir: Option<&Path>, session: &str, options: &SessionOptions) {
    let Some(path) = record_path(data_dir, session) else {
        return;
    };
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, options.json().to_string()));
    if let Err(e) = result {
        tracing::warn!(
            "claude session options: cannot save {}: {e}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests;

/// Quote one shell word for a POSIX shell, including Git Bash on Windows.
///
/// Inside single quotes `\`, `$`, `` ` `` and `"` stay literal, so Windows paths keep their
/// backslashes, UNC prefixes and `D$` shares. `'` is moved outside the quotes as `'\''`. Following
/// the shell-escape rule, `!` is also written outside as `'\!'`; bash gives the same word if it
/// stays inside the single quotes. The result does not depend on the `MSYSTEM` variable.
pub(crate) fn posix_single_quote(value: &str) -> String {
    shell_escape::unix::escape(value.into()).into_owned()
}

/// Keep the established double-quoted settings path syntax while escaping shell expansion.
#[cfg(not(windows))]
pub(crate) fn quote_path(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Windows terminals run only Git Bash, which reads the command line with POSIX rules.
#[cfg(windows)]
pub(crate) fn quote_path(path: &str) -> String {
    posix_single_quote(path)
}

/// Refresh the restore command after profile resolution, preserving invocation-only selections.
pub(crate) fn session_start<H: HostCall>(
    calls: &mut Vec<PlannedCall>,
    host: &H,
    surface_id: u32,
    data_dir: Option<&Path>,
    session_id: &str,
    tr: &Translator,
) {
    match load(host, surface_id, data_dir, session_id, tr) {
        Ok(options) => {
            for call in calls.iter_mut() {
                if let PlannedCall::MetaSet { key, value, .. } = call
                    && *key == RESTORE_COMMAND_META_KEY
                {
                    value.push_str(&options.flags());
                }
            }
            calls.push(PlannedCall::MetaSet {
                surface_id,
                key: META_KEY,
                value: bound_json(session_id, false, &options).to_string(),
            });
            store(data_dir, session_id, &options);
        }
        Err(e) => {
            // Do not replace a previously explicit restoration with an unqualified command.
            calls.retain(|call| {
                !matches!(call, PlannedCall::MetaSet { key, .. }
                    if *key == RESTORE_COMMAND_META_KEY)
            });
            tracing::warn!(
                "claude session-start: cannot restore session options: {}",
                e.message
            );
        }
    }
}
