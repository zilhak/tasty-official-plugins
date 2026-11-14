use super::*;

fn browse(doc: &mut ImageDoc, sid: u32, id: u64) -> Value {
    doc.browse.queue();
    let mut request = Value::Null;
    dispatch(doc, sid, |method, params| {
        assert_eq!(method, "file_picker.trigger");
        assert!(request.is_null(), "one call per browse intent");
        request = params;
        Ok(json!({"request_id": id}))
    });
    dispatch(doc, sid, |_, _| panic!("intent consumed"));
    request
}

fn result(id: u64, path: &str, cancelled: bool) -> Value {
    json!({"request_id": id, "paths": if cancelled { vec![] } else { vec![path] }, "cancelled": cancelled})
}

#[test]
fn browse_result_fills_save_path_and_cancel_keeps_it() {
    let dir = std::env::current_dir().unwrap();
    let mut doc = ImageDoc::new(Some(dir.join("input.png").to_string_lossy().into_owned()));
    doc.open_save_as();
    doc.save_path_buffer = "edited.png".into();
    let params = browse(&mut doc, 42, 100);
    assert_eq!(params["mode"], "save");
    assert_eq!(params["default_name"], "edited.png");
    assert_eq!(params["location"], "local");
    assert_eq!(params["start_dir"], dir.to_string_lossy().as_ref());
    assert_eq!(params["origin_surface_id"], 42);
    assert_eq!(params["filters"], json!(["png"]));
    assert!(params.get("owner_popup_instance").is_none());
    receive(&mut doc, &result(99, "wrong.png", false));
    assert_eq!(doc.save_path_buffer, "edited.png");
    receive(&mut doc, &result(100, "/chosen/out.png", false));
    assert_eq!(doc.save_path_buffer, "/chosen/out.png");
    receive(&mut doc, &result(100, "replay.png", false));
    assert_eq!(doc.save_path_buffer, "/chosen/out.png");
    browse(&mut doc, 42, 101);
    receive(&mut doc, &result(101, "", true));
    receive(&mut doc, &result(101, "after-cancel.png", false));
    assert_eq!(doc.save_path_buffer, "/chosen/out.png");
}

#[test]
fn closed_reopened_or_newer_card_rejects_late_results() {
    let mut doc = ImageDoc::new(None);
    doc.open_save_as();
    assert_eq!(browse(&mut doc, 1, 10)["default_name"], "image.png");
    doc.close_save_as();
    receive(&mut doc, &result(10, "closed.png", false));
    doc.open_save_as();
    receive(&mut doc, &result(10, "reopened.png", false));
    assert!(doc.save_path_buffer.is_empty());
    browse(&mut doc, 1, 11);
    browse(&mut doc, 1, 12);
    receive(&mut doc, &result(11, "older.png", false));
    assert!(doc.save_path_buffer.is_empty());
    receive(&mut doc, &result(12, "new.png", false));
    assert_eq!(doc.save_path_buffer, "new.png");
    browse(&mut doc, 1, 13);
    doc.open_save_as_for_clash("taken.png");
    let suggested = doc.save_path_buffer.clone();
    receive(&mut doc, &result(13, "old-card.png", false));
    assert_eq!(doc.save_path_buffer, suggested);
}

#[test]
fn surfaces_and_recreated_surfaces_do_not_share_receipts() {
    let mut first = ImageDoc::new(None);
    let mut second = ImageDoc::new(None);
    first.open_save_as();
    second.open_save_as();
    browse(&mut first, 1, 20);
    browse(&mut second, 2, 21);
    receive(&mut second, &result(20, "first.png", false));
    assert!(second.save_path_buffer.is_empty());
    // Reusing a surface ID creates a fresh doc, never its old pending receipt.
    first = ImageDoc::new(None);
    first.open_save_as();
    receive(&mut first, &result(20, "destroyed.png", false));
    assert!(first.save_path_buffer.is_empty());
    receive(&mut second, &result(21, "second.png", false));
    assert_eq!(second.save_path_buffer, "second.png");
}

#[test]
fn busy_or_malformed_admission_keeps_buffer_and_discards_old_request() {
    let mut doc = ImageDoc::new(None);
    doc.open_save_as();
    doc.save_path_buffer = "keep.png".into();
    browse(&mut doc, 7, 30);
    doc.browse.queue();
    dispatch(&mut doc, 7, |_, _| Err("-32000: already open".into()));
    receive(&mut doc, &result(30, "old.png", false));
    assert_eq!(doc.save_path_buffer, "keep.png");
    assert!(doc.browse.pending.is_none());
    doc.browse.queue();
    dispatch(&mut doc, 7, |_, _| Ok(json!({})));
    assert!(doc.browse.pending.is_none());
    assert_eq!(doc.save_path_buffer, "keep.png");
}

#[test]
fn path_selection_does_not_save_pixels_or_accept_multiple_paths() {
    let mut doc = ImageDoc::new(None);
    doc.open_save_as();
    browse(&mut doc, 1, 40);
    receive(
        &mut doc,
        &json!({"request_id":40,"cancelled":false,"paths":["a.png","b.png"]}),
    );
    assert!(doc.save_path_buffer.is_empty());
    assert!(doc.file_path.is_none());
    browse(&mut doc, 1, 41);
    receive(&mut doc, &result(41, "chosen.png", false));
    assert_eq!(doc.save_path_buffer, "chosen.png");
    assert!(doc.file_path.is_none());
    assert!(doc.save_path_popup);
}
