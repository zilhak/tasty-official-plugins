//! NSPasteboard에서 기타 클립보드 포맷을 열거하고 읽는다.

use objc2_app_kit::NSPasteboard;

use super::MAX_RAW_BYTES;
use crate::clipboard::OtherFormatEntry;

/// 다른 타입에서 표시하는 흔한 포맷을 제외한다. 모든 변형을 포함하는 목록은 아니다.
const CONSUMED_TYPE_NAMES: &[&str] = &[
    // 텍스트의 기본·레거시 표현.
    "public.utf8-plain-text",
    "public.utf16-plain-text",
    "public.plain-text",
    "NSStringPboardType",
    // 파일 URL의 기본·레거시 표현.
    "public.file-url",
    "NSFilenamesPboardType",
    // 이미지 포맷.
    "public.tiff",
    "public.png",
    "public.jpeg",
    "NSTIFFPboardType",
    "NSPICTPboardType",
    // Html
    "public.html",
    "NSHTMLPboardType",
];

pub(super) fn read_other() -> Vec<OtherFormatEntry> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let Some(types) = pasteboard.types() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for ty in types.iter() {
        let name = ty.to_string();
        if CONSUMED_TYPE_NAMES.contains(&name.as_str()) {
            continue;
        }
        match pasteboard.dataForType(&ty) {
            Some(data) => {
                let bytes = data.to_vec();
                out.push(OtherFormatEntry::from_bytes(name, &bytes, MAX_RAW_BYTES));
            }
            // 조회 중 내용이 바뀌거나 읽을 수 없으면 이 포맷만 생략한다.
            None => tracing::debug!("clipboard pasteboard dataForType({name}) returned nil"),
        }
    }
    out
}
