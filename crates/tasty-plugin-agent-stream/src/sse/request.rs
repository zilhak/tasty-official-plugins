//! HTTP 라이브러리와 별도로 경로·쿼리·인증·구독 옵션을 해석한다.

use std::collections::BTreeMap;

use crate::sse::hub::SubOptions;

/// 스트림 경로. 이 경로가 아니면 404 — 다른 경로를 열지 않는다.
pub const STREAM_PATH: &str = "/events";

/// 구독 토큰을 실을 수 있는 쿼리 파라미터 이름.
pub const TOKEN_QUERY_KEY: &str = "token";

/// URL 에서 경로부만 뽑는다(쿼리 제외).
pub fn path_of(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// `?a=1&b=2` 를 파싱한다. `+` 는 공백, `%XX` 는 바이트로 디코드한다.
pub fn parse_query(url: &str) -> BTreeMap<String, String> {
    let Some((_, raw)) = url.split_once('?') else {
        return BTreeMap::new();
    };
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// form-urlencoded를 디코드한다. 잘못된 % 시퀀스는 그대로 남긴다.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match hex_pair(bytes[i + 1], bytes[i + 2]) {
                Some(byte) => {
                    out.push(byte);
                    i += 3;
                }
                None => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_pair(hi: u8, lo: u8) -> Option<u8> {
    let hi = (hi as char).to_digit(16)?;
    let lo = (lo as char).to_digit(16)?;
    Some((hi * 16 + lo) as u8)
}

/// 길이가 다르면 거절하고 같은 길이는 모든 바이트의 XOR 결과를 합쳐 비교한다.
/// 이 소스만으로 컴파일된 코드의 실행 시간이 일정하다고 보장하지는 않는다.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 구독 인증. `expected` 가 `None` 이면(토큰 미설정) 무인증 통과다.
///
/// 토큰 위치는 `Authorization: Bearer <t>` 와 `?token=<t>` 두 곳 — 브라우저
/// `EventSource` 가 커스텀 헤더를 못 붙이므로 쿼리 경로가 필요하고, 서버 대 서버
/// 구독자는 헤더 쪽이 로그에 덜 남는다.
pub fn authorize(
    expected: Option<&str>,
    headers: &BTreeMap<String, String>,
    query: &BTreeMap<String, String>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let presented = headers
        .get("authorization")
        .and_then(|raw| strip_bearer(raw))
        .map(str::to_string)
        .or_else(|| query.get(TOKEN_QUERY_KEY).cloned());
    match presented {
        Some(token) => ct_eq(token.as_bytes(), expected.as_bytes()),
        None => false,
    }
}

fn strip_bearer(raw: &str) -> Option<&str> {
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    Some(rest.trim())
}

/// 쿼리에서 구독 옵션을 읽는다. 없는 값은 기본(모든 surface, thinking 제외).
pub fn sub_options(query: &BTreeMap<String, String>) -> SubOptions {
    SubOptions {
        filter_surface: query.get("surface").and_then(|v| v.parse::<u32>().ok()),
        include_thinking: query.get("thinking").is_some_and(|v| is_truthy(v)),
    }
}

fn is_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Last-Event-ID를 우선 읽고 유효한 값이 없으면 after_seq를 읽는다. 둘 다 없으면 재전송하지 않는다.
pub fn resume_from(
    headers: &BTreeMap<String, String>,
    query: &BTreeMap<String, String>,
) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or_else(|| query.get("after_seq").and_then(|v| v.parse::<u64>().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_path_is_split_from_the_query() {
        assert_eq!(path_of("/events?token=a"), "/events");
        assert_eq!(path_of("/events"), "/events");
        assert_eq!(path_of("/"), "/");
    }

    #[test]
    fn query_pairs_are_percent_and_plus_decoded() {
        let q = parse_query("/events?token=a%2Fb+c&thinking=1&flag");
        assert_eq!(q.get("token").map(String::as_str), Some("a/b c"));
        assert_eq!(q.get("thinking").map(String::as_str), Some("1"));
        assert_eq!(q.get("flag").map(String::as_str), Some(""));
        assert!(parse_query("/events").is_empty());
    }

    #[test]
    fn no_configured_token_means_no_authentication() {
        assert!(authorize(None, &map(&[]), &map(&[])));
    }

    #[test]
    fn a_configured_token_is_accepted_from_the_header_or_the_query_only() {
        let expected = Some("s3cret");
        assert!(authorize(
            expected,
            &map(&[("authorization", "Bearer s3cret")]),
            &map(&[])
        ));
        assert!(authorize(
            expected,
            &map(&[("authorization", "bearer s3cret")]),
            &map(&[])
        ));
        assert!(authorize(expected, &map(&[]), &map(&[("token", "s3cret")])));
        // 미제시·오토큰·스킴 없는 헤더는 전부 거부.
        assert!(!authorize(expected, &map(&[]), &map(&[])));
        assert!(!authorize(expected, &map(&[]), &map(&[("token", "wrong")])));
        assert!(!authorize(
            expected,
            &map(&[("authorization", "s3cret")]),
            &map(&[])
        ));
    }

    #[test]
    fn ct_eq_matches_only_on_equal_content_and_length() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn subscription_options_default_to_every_surface_without_thinking() {
        let opts = sub_options(&map(&[]));
        assert_eq!(opts.filter_surface, None);
        assert!(!opts.include_thinking);

        let opts = sub_options(&map(&[("surface", "42"), ("thinking", "true")]));
        assert_eq!(opts.filter_surface, Some(42));
        assert!(opts.include_thinking);

        // 알아볼 수 없는 값은 기본으로 떨어진다(조용히 켜지지 않는다).
        assert!(!sub_options(&map(&[("thinking", "maybe")])).include_thinking);
        assert_eq!(sub_options(&map(&[("surface", "x")])).filter_surface, None);
    }

    #[test]
    fn resume_prefers_the_header_and_defaults_to_live_only() {
        assert_eq!(resume_from(&map(&[]), &map(&[])), None);
        assert_eq!(
            resume_from(
                &map(&[("last-event-id", "12")]),
                &map(&[("after_seq", "3")])
            ),
            Some(12)
        );
        assert_eq!(resume_from(&map(&[]), &map(&[("after_seq", "3")])), Some(3));
        // 숫자가 아니면 재개하지 않는다(임의 커서로 되감지 않는다).
        assert_eq!(
            resume_from(&map(&[("last-event-id", "abc")]), &map(&[])),
            None
        );
    }
}
