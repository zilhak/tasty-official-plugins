//! Asynchronous host picker: a receipt belongs to one surface's open save card.
use crate::doc::ImageDoc;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Default)]
pub(crate) struct BrowseState {
    generation: u64,
    queued: bool,
    pending: Option<(u64, u64)>,
}

impl BrowseState {
    pub(crate) fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.queued = false;
        self.pending = None;
    }

    pub(crate) fn queue(&mut self) {
        self.invalidate();
        self.queued = true;
    }
}

/// Runs after painting, and waits only for admission, never for a user's selection.
/// A busy/error reply leaves the card and its buffer unchanged and drops the old receipt.
pub(crate) fn dispatch(
    doc: &mut ImageDoc,
    surface_id: u32,
    mut call: impl FnMut(&str, Value) -> Result<Value, String>,
) {
    if !std::mem::take(&mut doc.browse.queued) || !doc.save_path_popup {
        return;
    }
    let buffer = Path::new(&doc.save_path_buffer);
    let default_name = buffer
        .file_name()
        .filter(|n| !n.is_empty())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image.png".to_string());
    let start_dir = buffer
        .parent()
        .filter(|p| p.is_absolute())
        .or_else(|| {
            doc.file_path
                .as_deref()
                .and_then(|p| Path::new(p).parent())
                .filter(|p| p.is_absolute())
        })
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        });
    let params = json!({ "mode": "save", "default_name": default_name,
        "filters": ["png"], "origin_surface_id": surface_id, "location": "local", "start_dir": start_dir });
    match call("file_picker.trigger", params) {
        Ok(reply) => {
            if let Some(id) = reply.get("request_id").and_then(Value::as_u64) {
                doc.browse.pending = Some((id, doc.browse.generation));
            } else {
                tracing::warn!("image browse: host returned no request_id");
            }
        }
        Err(error) => tracing::warn!("image browse (file_picker.trigger) failed: {error}"),
    }
}

/// Unknown, closed-card and old-generation results cannot alter a newer card.
/// Matching cancellation and confirmation both consume the receipt exactly once.
pub(crate) fn receive(doc: &mut ImageDoc, payload: &Value) {
    let (Some(id), Some(cancelled), Some(paths)) = (
        payload.get("request_id").and_then(Value::as_u64),
        payload.get("cancelled").and_then(Value::as_bool),
        payload.get("paths").and_then(Value::as_array),
    ) else {
        return;
    };
    if doc.browse.pending != Some((id, doc.browse.generation)) {
        return;
    }
    doc.browse.pending = None;
    if doc.save_path_popup
        && !cancelled
        && let [path] = paths.as_slice()
        && let Some(path) = path.as_str()
        && !path.is_empty()
    {
        doc.save_path_buffer = path.to_string();
        doc.save_path_select_stem = false;
    }
}

#[cfg(test)]
mod tests;
