//! clipboard-win으로 기타 클립보드 포맷을 열거하고 읽는다.

use clipboard_win::formats::{CF_DIB, CF_DIBV5, CF_HDROP, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT};
use clipboard_win::{Clipboard, EnumFormats, raw};

use super::MAX_RAW_BYTES;
use crate::clipboard::OtherFormatEntry;

/// 다른 타입에서 표시하는 텍스트·파일·이미지 포맷을 제외한다.
const CONSUMED_FIXED_IDS: &[u32] = &[
    CF_TEXT,
    CF_UNICODETEXT,
    CF_OEMTEXT,
    CF_HDROP,
    CF_DIB,
    CF_DIBV5,
];

/// 등록 포맷의 ID는 달라질 수 있으므로 HTML·PNG는 이름으로 구분한다.
fn is_consumed_by_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("HTML Format") || name.eq_ignore_ascii_case("PNG")
}

pub(super) fn read_other() -> Vec<OtherFormatEntry> {
    let _clip = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("clipboard-win open failed, skipping other-format enumeration: {e}");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for id in EnumFormats::new() {
        if CONSUMED_FIXED_IDS.contains(&id) {
            continue;
        }
        let name = raw::format_name_big(id).unwrap_or_else(|| format!("Format #{id}"));
        if is_consumed_by_name(&name) {
            continue;
        }
        let mut buf = Vec::new();
        match raw::get_vec(id, &mut buf) {
            Ok(_) => out.push(OtherFormatEntry::from_bytes(name, &buf, MAX_RAW_BYTES)),
            // 읽지 못하는 포맷만 생략한다.
            Err(e) => tracing::debug!("clipboard-win get_vec({id}, {name}) failed: {e}"),
        }
    }
    out
}
