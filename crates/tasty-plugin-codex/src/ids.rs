//! 난수 식별자와 외부에서 받은 식별자의 형식 검사.
//!
//! 수신자·이벤트 ID는 추측할 수 없어야 하므로 OS 난수로 만든다. 형식 검사는 파일 이름과
//! 셸 인자로 쓰기 전에 한다.

/// 바이트 수만큼의 OS 난수를 소문자 16진수로 반환한다.
pub(crate) fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// 수신자·이벤트 ID(128비트)의 16진 길이.
pub(crate) const ID_HEX_LEN: usize = 32;

/// 새 수신자·이벤트 ID.
pub(crate) fn new_id() -> std::io::Result<String> {
    random_hex(ID_HEX_LEN / 2)
}

/// 세션 세대. 같은 surface의 이전 실행과 구분하는 값이라 64비트면 충분하다.
pub(crate) fn new_generation() -> std::io::Result<String> {
    random_hex(8)
}

pub(crate) fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn is_id(value: &str) -> bool {
    is_hex(value, ID_HEX_LEN)
}

pub(crate) fn is_generation(value: &str) -> bool {
    is_hex(value, 16)
}

/// 하이픈 위치를 포함한 UUID 형식. 대소문자를 모두 받는다.
pub(crate) fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// 완료 키의 최대 바이트 수.
pub(crate) const COMPLETION_KEY_MAX: usize = 160;

/// 완료 키가 계약 형식(1..160바이트의 `[A-Za-z0-9._:-]`)인지 검사한다.
pub(crate) fn is_completion_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= COMPLETION_KEY_MAX
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// 값의 SHA-256을 소문자 16진수로 돌려준다.
pub(crate) fn sha256_hex(value: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 완료 키를 claim 파일 이름으로 바꾼다. 키를 그대로 쓰면 대소문자를 구분하지 않는 파일
/// 시스템(Windows·macOS 기본값)에서 다른 키가 같은 파일이 되고, Windows는 끝의 `.`을
/// 버리고 `CON` 같은 예약 이름을 장치로 연다. 그래서 키의 SHA-256 16진수를 쓴다. 원래 키는
/// 이벤트에 남는다.
pub(crate) fn completion_key_file_name(key: &str) -> Option<String> {
    is_completion_key(key).then(|| sha256_hex(key))
}

/// 셸·파일에 넣을 수 있는 짧은 토큰(턴 ID 등).
pub(crate) fn is_plain_token(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_long_random_hex_and_differ() {
        let a = new_id().unwrap();
        let b = new_id().unwrap();
        assert!(is_id(&a) && is_id(&b), "{a} {b}");
        assert_ne!(a, b);
        assert!(is_generation(&new_generation().unwrap()));
        assert!(!is_id("ABCDEF0123456789abcdef0123456789"));
        assert!(!is_id("ev-7f3a"));
    }

    #[test]
    fn uuid_shape_is_checked_with_hyphens() {
        assert!(is_uuid("01a12f74-dab9-7a00-8000-0123456789ab"));
        assert!(is_uuid("01A12F74-DAB9-7A00-8000-0123456789AB"));
        assert!(!is_uuid("01a12f74dab97a0080000123456789ab0000"));
        assert!(!is_uuid("01a12f74-dab9-7a00-8000-0123456789a"));
        assert!(!is_uuid("../../../../etc/passwd-0000000000000"));
    }

    #[test]
    fn completion_keys_map_to_fixed_lowercase_hex_names() {
        let name = completion_key_file_name("codex:abc:t1").unwrap();
        assert!(is_hex(&name, 64), "{name}");
        // 대소문자만 다른 키, 끝의 `.`만 다른 키, Windows 예약 이름도 서로 다른 파일이다.
        let names: std::collections::HashSet<String> =
            ["k:A", "k:a", "a.", "a", "CON", "con", "NUL", "COM1"]
                .iter()
                .map(|k| completion_key_file_name(k).unwrap())
                .collect();
        assert_eq!(names.len(), 8);
        for bad in ["", "a/b", "a\\b", "a b", "a\nb", "키", "codex+abc"] {
            assert_eq!(completion_key_file_name(bad), None, "{bad:?}");
        }
        assert!(completion_key_file_name(&"a".repeat(COMPLETION_KEY_MAX)).is_some());
        assert!(completion_key_file_name(&"a".repeat(COMPLETION_KEY_MAX + 1)).is_none());
    }
}
