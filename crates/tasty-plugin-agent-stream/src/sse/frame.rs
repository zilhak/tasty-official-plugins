//! SSE 프레임: id·event·data 줄 뒤 빈 줄로 끝낸다.
//! data 안의 개행은 줄마다 data:를 붙여 전송해 프레임 경계와 구분한다.

/// 유휴 연결에 보내는 주석 줄. 이벤트로 처리되지 않으며 프록시의 유휴 연결 종료를 줄인다.
pub const KEEP_ALIVE: &str = ": keep-alive\n\n";

/// 이벤트 하나를 프레임으로 만든다. event에는 개행 없는 이벤트 이름을 전달해야 한다.
/// data의 LF·CRLF·CR은 각각 data: 줄로 나눈다.
pub fn encode(id: u64, event: &str, data: &str) -> String {
    let mut out = String::with_capacity(data.len() + 48);
    out.push_str("id: ");
    out.push_str(&id.to_string());
    out.push('\n');
    out.push_str("event: ");
    out.push_str(event);
    out.push('\n');
    for line in split_lines(data) {
        out.push_str("data:");
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

/// SSE 규격의 줄 구분자 3 종(`\r\n` / `\r` / `\n`)으로 자른다. 빈 문자열은 빈 줄 하나로
/// 본다 — `data:` 한 줄이 나가고 소비자의 data 버퍼는 빈 문자열이 된다.
fn split_lines(data: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = data.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&data[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                lines.push(&data[start..i]);
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            _ => i += 1,
        }
    }
    lines.push(&data[start..]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_line_payload_becomes_one_data_line_and_a_blank_terminator() {
        assert_eq!(
            encode(7, "text", "{\"a\":1}"),
            "id: 7\nevent: text\ndata: {\"a\":1}\n\n"
        );
    }

    #[test]
    fn embedded_newlines_are_split_into_one_data_line_each() {
        // 개행을 그대로 흘리면 첫 줄 뒤의 빈 줄이 프레임을 조기 종료시킨다.
        let frame = encode(1, "text", "first\nsecond\nthird");
        assert_eq!(
            frame,
            "id: 1\nevent: text\ndata: first\ndata: second\ndata: third\n\n"
        );
        // 종결 빈 줄은 정확히 하나 — 프레임 중간에 빈 줄이 없다.
        assert_eq!(frame.matches("\n\n").count(), 1);
    }

    #[test]
    fn crlf_and_bare_cr_count_as_line_breaks_too() {
        assert_eq!(
            encode(2, "text", "a\r\nb\rc"),
            "id: 2\nevent: text\ndata: a\ndata: b\ndata: c\n\n"
        );
    }

    #[test]
    fn an_empty_payload_still_emits_one_data_line() {
        assert_eq!(
            encode(3, "turn_end", ""),
            "id: 3\nevent: turn_end\ndata:\n\n"
        );
    }

    #[test]
    fn a_trailing_newline_produces_a_trailing_empty_data_line() {
        // 소비자가 다시 이어붙이면 원문의 마지막 개행이 보존된다.
        assert_eq!(
            encode(4, "text", "a\n"),
            "id: 4\nevent: text\ndata: a\ndata:\n\n"
        );
    }

    #[test]
    fn keep_alive_is_a_comment_line_terminated_like_a_frame() {
        assert!(KEEP_ALIVE.starts_with(':'));
        assert!(KEEP_ALIVE.ends_with("\n\n"));
    }
}
