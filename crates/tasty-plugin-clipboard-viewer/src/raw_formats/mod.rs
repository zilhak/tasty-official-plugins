//! arboard가 열거하지 않는 기타 클립보드 포맷을 플랫폼 API로 읽는다.
//! 텍스트·파일·이미지·HTML의 알려진 변형은 플랫폼별 목록으로 제외한다.
//! 개별 읽기 실패는 빈 클립보드와 구분되지 않을 수 있다.

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

// 별도 Wayland 백엔드는 없으므로 X11/XWayland 연결이 필요하다.
#[cfg(all(unix, not(target_os = "macos")))]
mod x11;

use crate::clipboard::OtherFormatEntry;

/// 포맷별 미리보기 변환에 사용할 최대 바이트 수. OS에서 읽는 양의 상한은 아니다.
/// 화면에 표시할 줄 수 제한과는 별개다.
pub(crate) const MAX_RAW_BYTES: usize = 16 * 1024;

/// 지원하는 플랫폼에서 기타 포맷을 읽는다. 조회 실패 시 비거나 일부만 남을 수 있다.
pub(crate) fn read_other() -> Vec<OtherFormatEntry> {
    #[cfg(target_os = "windows")]
    {
        windows::read_other()
    }
    #[cfg(target_os = "macos")]
    {
        macos::read_other()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        x11::read_other()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        Vec::new()
    }
}
