//! 클립보드 내용을 타입별로 읽는다.
//! 텍스트·파일·이미지·HTML은 arboard로, 나머지 포맷은 플랫폼 API로 읽는다.

use std::path::PathBuf;

/// 클립보드에 담길 수 있는 콘텐츠 타입. 좌측 목록의 키이자 선택 단위.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClipboardType {
    Text,
    Files,
    Image,
    Html,
    /// 알려진 텍스트·파일·이미지·HTML 포맷을 제외한 나머지 목록.
    Other,
}

impl ClipboardType {
    /// 타입 라벨 i18n 키.
    pub fn label_i18n_key(self) -> &'static str {
        match self {
            ClipboardType::Text => "clipboard_viewer.type.text",
            ClipboardType::Files => "clipboard_viewer.type.files",
            ClipboardType::Image => "clipboard_viewer.type.image",
            ClipboardType::Html => "clipboard_viewer.type.html",
            ClipboardType::Other => "clipboard_viewer.type.other",
        }
    }

    /// 푸터에 쓸 MIME 문자열. 기술 표기이므로 번역하지 않는다.
    pub fn mime_str(self) -> &'static str {
        match self {
            ClipboardType::Text => "text/plain",
            ClipboardType::Files => "text/uri-list",
            // arboard는 원본 인코딩 대신 RGBA8 픽셀을 반환한다.
            ClipboardType::Image => "image/rgba8",
            ClipboardType::Html => "text/html",
            // 실제 푸터에서는 단일 MIME 대신 포맷 개수를 표시한다.
            ClipboardType::Other => "application/octet-stream",
        }
    }
}

/// 타입별 표시 데이터.
#[derive(Clone, Debug)]
pub enum ContentRepr {
    Text(String),
    Files(Vec<PathBuf>),
    /// 이미지 미리보기는 그리지 않으므로 픽셀 대신 치수와 바이트 수만 보관한다.
    Image {
        width: usize,
        height: usize,
        byte_len: usize,
    },
    Html(String),
    /// 포맷 이름과 미리보기 목록. read_available은 빈 목록을 추가하지 않는다.
    Other(Vec<OtherFormatEntry>),
}

impl ContentRepr {
    /// 이미지의 치수·크기 요약. 다른 타입은 None을 반환한다.
    pub fn meta_text(&self) -> Option<String> {
        match self {
            ContentRepr::Text(_) => None,
            ContentRepr::Files(_) => None,
            ContentRepr::Image {
                width,
                height,
                byte_len,
            } => Some(format!("{width}×{height} · {}", format_bytes(*byte_len))),
            ContentRepr::Html(_) => None,
            ContentRepr::Other(_) => None,
        }
    }
}

/// 기타 포맷의 이름·크기·미리보기.
#[derive(Clone, Debug)]
pub struct OtherFormatEntry {
    /// 포맷 이름(OS 가 보고하는 사람이 읽는 이름, 없으면 ID 기반 fallback).
    pub name: String,
    /// 미리보기를 자르기 전의 바이트 수.
    pub byte_len: usize,
    /// 손실 허용 UTF-8 변환 또는 바이너리의 16진수 요약. 내용을 로그에 남기지 않는다.
    pub preview: String,
    /// `preview`가 hex 요약(바이너리 fallback)인지 — 뷰가 스타일을 달리할 수 있게.
    pub is_binary: bool,
}

impl OtherFormatEntry {
    /// cap 바이트까지만 미리보기로 변환한다. 치환 문자 비율이 높으면 16진수로 표시한다.
    pub(crate) fn from_bytes(name: String, bytes: &[u8], cap: usize) -> Self {
        let byte_len = bytes.len();
        let capped = &bytes[..byte_len.min(cap)];
        let lossy = String::from_utf8_lossy(capped);
        let replacement_ratio = if lossy.is_empty() {
            0.0
        } else {
            lossy.chars().filter(|&c| c == '\u{FFFD}').count() as f64 / lossy.chars().count() as f64
        };
        let is_binary = replacement_ratio > 0.05;
        let preview = if is_binary {
            hex_summary(capped)
        } else {
            lossy.into_owned()
        };
        Self {
            name,
            byte_len,
            preview,
            is_binary,
        }
    }
}

/// 바이너리 앞부분을 공백으로 나눈 16진수 바이트로 표시한다.
fn hex_summary(bytes: &[u8]) -> String {
    const HEX_PREVIEW_BYTES: usize = 256;
    bytes[..bytes.len().min(HEX_PREVIEW_BYTES)]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 바이트 수를 사람이 읽기 쉬운 단위로 표시한다. 이미지는 RGBA8 픽셀 데이터 크기다.
pub(crate) fn format_bytes(n: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut s = n as f64;
    let mut u = 0;
    while s >= 1024.0 && u < UNITS.len() - 1 {
        s /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{s:.1} {}", UNITS[u])
    }
}

/// Text 리더 — 비어있지 않은 텍스트가 있을 때만 `Some`.
fn read_text(clip: &mut arboard::Clipboard) -> Option<(ClipboardType, ContentRepr)> {
    match clip.get_text() {
        Ok(text) if !text.is_empty() => Some((ClipboardType::Text, ContentRepr::Text(text))),
        Ok(_) => None,
        Err(e) => {
            // 이 타입을 읽지 못하면 다른 타입 조회를 계속한다.
            tracing::debug!("clipboard get_text: {e}");
            None
        }
    }
}

/// Files 리더 — 비어있지 않은 파일 목록이 있을 때만 `Some`.
fn read_files(clip: &mut arboard::Clipboard) -> Option<(ClipboardType, ContentRepr)> {
    match clip.get().file_list() {
        Ok(files) if !files.is_empty() => Some((ClipboardType::Files, ContentRepr::Files(files))),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("clipboard get file_list: {e}");
            None
        }
    }
}

/// 이미지 픽셀은 버리고 치수·크기만 보관한다.
fn read_image(clip: &mut arboard::Clipboard) -> Option<(ClipboardType, ContentRepr)> {
    match clip.get_image() {
        Ok(img) => Some((
            ClipboardType::Image,
            ContentRepr::Image {
                width: img.width,
                height: img.height,
                byte_len: img.bytes.len(),
            },
        )),
        Err(e) => {
            tracing::debug!("clipboard get_image: {e}");
            None
        }
    }
}

/// 비어 있지 않은 HTML을 읽는다.
fn read_html(clip: &mut arboard::Clipboard) -> Option<(ClipboardType, ContentRepr)> {
    match clip.get().html() {
        Ok(html) if !html.is_empty() => Some((ClipboardType::Html, ContentRepr::Html(html))),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("clipboard get_html: {e}");
            None
        }
    }
}

/// 플랫폼 API로 기타 포맷을 열거한다. arboard에는 열거 기능이 없다.
fn read_other() -> Option<(ClipboardType, ContentRepr)> {
    let entries = crate::raw_formats::read_other();
    if entries.is_empty() {
        None
    } else {
        Some((ClipboardType::Other, ContentRepr::Other(entries)))
    }
}

/// 읽을 수 있는 타입과 내용을 반환한다. 핸들을 열지 못하면 오류다.
/// 개별 타입의 읽기 실패는 생략하므로 빈 결과만으로 클립보드가 비었다고 단정할 수 없다.
pub(crate) fn read_available() -> Result<Vec<(ClipboardType, ContentRepr)>, String> {
    let mut clip = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    out.extend(read_text(&mut clip));
    out.extend(read_files(&mut clip));
    out.extend(read_image(&mut clip));
    out.extend(read_html(&mut clip));
    out.extend(read_other());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(4096), "4.0 KB");
        assert_eq!(format_bytes(2 * 1024 * 1024 + 300 * 1024), "2.3 MB");
    }

    #[test]
    fn image_meta_text_formats_dimensions_and_size() {
        let repr = ContentRepr::Image {
            width: 1920,
            height: 1080,
            byte_len: 1920 * 1080 * 4,
        };
        assert_eq!(repr.meta_text().as_deref(), Some("1920×1080 · 7.9 MB"));
    }

    #[test]
    fn text_meta_text_is_none() {
        assert_eq!(ContentRepr::Text("x".into()).meta_text(), None);
    }

    #[test]
    fn html_meta_text_is_none() {
        assert_eq!(ContentRepr::Html("<p>x</p>".into()).meta_text(), None);
    }

    #[test]
    fn image_type_uses_own_label_and_mime() {
        assert_eq!(
            ClipboardType::Image.label_i18n_key(),
            "clipboard_viewer.type.image"
        );
        assert_eq!(ClipboardType::Image.mime_str(), "image/rgba8");
    }

    #[test]
    fn html_type_uses_own_label_and_mime() {
        assert_eq!(
            ClipboardType::Html.label_i18n_key(),
            "clipboard_viewer.type.html"
        );
        assert_eq!(ClipboardType::Html.mime_str(), "text/html");
    }

    #[test]
    fn other_type_uses_own_label_and_mime() {
        assert_eq!(
            ClipboardType::Other.label_i18n_key(),
            "clipboard_viewer.type.other"
        );
        assert_eq!(ClipboardType::Other.mime_str(), "application/octet-stream");
    }

    #[test]
    fn other_meta_text_is_none() {
        assert_eq!(ContentRepr::Other(Vec::new()).meta_text(), None);
    }

    #[test]
    fn other_format_entry_plain_text_is_not_binary() {
        let entry = OtherFormatEntry::from_bytes("Custom Format".into(), b"hello world", 4096);
        assert_eq!(entry.byte_len, 11);
        assert_eq!(entry.preview, "hello world");
        assert!(!entry.is_binary);
    }

    #[test]
    fn other_format_entry_binary_falls_back_to_hex() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let entry = OtherFormatEntry::from_bytes("Binary Format".into(), &bytes, 4096);
        assert!(entry.is_binary);
        assert_eq!(entry.byte_len, 256);
        assert!(entry.preview.starts_with("00 01 02"));
    }

    #[test]
    fn other_format_entry_caps_before_previewing() {
        let bytes = vec![b'a'; 10_000];
        let entry = OtherFormatEntry::from_bytes("Big Format".into(), &bytes, 100);
        // 미리보기만 자르고 원래 바이트 수는 보존한다.
        assert_eq!(entry.byte_len, 10_000);
        assert_eq!(entry.preview.len(), 100);
        assert!(!entry.is_binary);
    }
}
