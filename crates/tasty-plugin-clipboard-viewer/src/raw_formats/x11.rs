//! X11/XWayland에서 TARGETS로 포맷 목록을 조회한 뒤 각 포맷을 읽는다.
//! arboard의 연결을 공유하지 않고 별도 연결과 임시 창을 사용한다.
//! X11 연결이 없는 순수 Wayland 환경에서는 빈 목록과 debug 로그를 남긴다.

use std::time::{Duration, Instant};

use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, NONE,
    connection::Connection,
    protocol::{
        Event,
        xproto::{
            AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Property, Time, WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

use super::MAX_RAW_BYTES;
use crate::clipboard::OtherFormatEntry;

x11rb::atom_manager! {
    Atoms: AtomCookies {
        CLIPBOARD,
        TARGETS,
        INCR,
        MULTIPLE,
        SAVE_TARGETS,
        TIMESTAMP,

        // 텍스트의 알려진 변형.
        UTF8_STRING,
        STRING,
        TEXT,
        UTF8_MIME_0: b"text/plain;charset=utf-8",
        UTF8_MIME_1: b"text/plain;charset=UTF-8",
        TEXT_MIME_UNKNOWN: b"text/plain",

        // files/html/image 로 이미 소비.
        URI_LIST: b"text/uri-list",
        HTML: b"text/html",
        PNG_MIME: b"image/png",

        // 이 모듈의 응답을 받을 임시 프로퍼티.
        TASTY_OTHER_REPLY,
    }
}

/// 최초 SelectionNotify를 기다리는 제한 시간.
const CONVERT_TIMEOUT: Duration = Duration::from_millis(3000);
/// INCR 조각 사이의 대기 제한 시간.
const INCR_SEGMENT_TIMEOUT: Duration = Duration::from_millis(500);

pub(super) fn read_other() -> Vec<OtherFormatEntry> {
    match try_read_other() {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!("clipboard other-format X11 enumeration skipped: {e}");
            Vec::new()
        }
    }
}

fn try_read_other() -> Result<Vec<OtherFormatEntry>, String> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| format!("X11 connect failed: {e}"))?;
    let screen = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| "X11: no screen found".to_string())?;
    let win = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        screen.root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::COPY_FROM_PARENT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;

    let atoms = Atoms::new(&conn)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;

    let Some(targets_bytes) = convert_and_read(&conn, win, &atoms, atoms.TARGETS)? else {
        // 소유자가 없거나 TARGETS를 지원하지 않으면 빈 목록으로 처리한다.
        return Ok(Vec::new());
    };
    let target_atoms: Vec<u32> = targets_bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let consumed = [
        atoms.TARGETS,
        atoms.MULTIPLE,
        atoms.SAVE_TARGETS,
        atoms.TIMESTAMP,
        atoms.UTF8_STRING,
        atoms.STRING,
        atoms.TEXT,
        atoms.UTF8_MIME_0,
        atoms.UTF8_MIME_1,
        atoms.TEXT_MIME_UNKNOWN,
        atoms.URI_LIST,
        atoms.HTML,
        atoms.PNG_MIME,
    ];

    let mut out = Vec::new();
    for atom in target_atoms {
        if consumed.contains(&atom) {
            continue;
        }
        let name = match conn.get_atom_name(atom).map_err(|e| e.to_string())?.reply() {
            Ok(reply) => String::from_utf8_lossy(&reply.name).into_owned(),
            // 이름 조회에 실패하면 이 포맷만 생략한다.
            Err(_) => continue,
        };
        match convert_and_read(&conn, win, &atoms, atom) {
            Ok(Some(bytes)) => out.push(OtherFormatEntry::from_bytes(name, &bytes, MAX_RAW_BYTES)),
            // 목록 조회 후 소유자가 바뀌는 등의 이유로 요청을 거절할 수 있다.
            Ok(None) => tracing::debug!("clipboard other-format {name}: owner declined"),
            Err(e) => tracing::debug!("clipboard other-format {name} read failed: {e}"),
        }
    }
    Ok(out)
}

/// CLIPBOARD에 포맷을 요청하고 일반 응답 또는 INCR 조각을 모은다.
/// 소유자가 거절하면 None, 연결·프로토콜·대기 오류는 Err를 반환한다.
fn convert_and_read(
    conn: &RustConnection,
    win: u32,
    atoms: &Atoms,
    target: u32,
) -> Result<Option<Vec<u8>>, String> {
    conn.delete_property(win, atoms.TASTY_OTHER_REPLY)
        .map_err(|e| e.to_string())?;
    conn.convert_selection(
        win,
        atoms.CLIPBOARD,
        target,
        atoms.TASTY_OTHER_REPLY,
        Time::CURRENT_TIME,
    )
    .map_err(|e| e.to_string())?;
    conn.sync().map_err(|e| e.to_string())?;

    let mut incr_data: Vec<u8> = Vec::new();
    let mut using_incr = false;
    let mut deadline = Instant::now() + CONVERT_TIMEOUT;

    loop {
        if Instant::now() > deadline {
            return Err("timed out waiting for SelectionNotify/PropertyNotify".to_string());
        }
        let Some(event) = conn.poll_for_event().map_err(|e| e.to_string())? else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        match event {
            Event::SelectionNotify(ev) => {
                if ev.property == NONE {
                    return Ok(None);
                }
                let reply = conn
                    .get_property(
                        true,
                        win,
                        atoms.TASTY_OTHER_REPLY,
                        AtomEnum::ANY,
                        0,
                        u32::MAX / 4,
                    )
                    .map_err(|e| e.to_string())?
                    .reply()
                    .map_err(|e| e.to_string())?;
                if reply.type_ == atoms.INCR {
                    using_incr = true;
                    deadline = Instant::now() + INCR_SEGMENT_TIMEOUT;
                    continue;
                }
                return Ok(Some(reply.value));
            }
            Event::PropertyNotify(ev)
                if using_incr
                    && ev.atom == atoms.TASTY_OTHER_REPLY
                    && ev.state == Property::NEW_VALUE =>
            {
                let reply = conn
                    .get_property(
                        true,
                        win,
                        atoms.TASTY_OTHER_REPLY,
                        AtomEnum::ANY,
                        0,
                        u32::MAX / 4,
                    )
                    .map_err(|e| e.to_string())?
                    .reply()
                    .map_err(|e| e.to_string())?;
                if reply.value_len == 0 {
                    return Ok(Some(incr_data));
                }
                incr_data.extend(reply.value);
                deadline = Instant::now() + INCR_SEGMENT_TIMEOUT;
            }
            _ => {}
        }
    }
}
