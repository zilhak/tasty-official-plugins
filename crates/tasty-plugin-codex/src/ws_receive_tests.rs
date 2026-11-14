use super::*;
use std::io::{self, Cursor};

struct SmallReads {
    bytes: Cursor<Vec<u8>>,
}
impl Read for SmallReads {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        assert!(
            out.len() <= 8192,
            "must not allocate/read the advertised payload at once"
        );
        let n = out.len().min(7);
        self.bytes.read(&mut out[..n])
    }
}
impl Write for SmallReads {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_huge_advertised_frame_with_a_short_body_is_eof_not_a_size_refusal() {
    let mut bytes = vec![0x81, 127];
    bytes.extend_from_slice(&(i64::MAX as u64).to_be_bytes());
    bytes.extend_from_slice(b"short body");
    let mut ws = WsConn::established(
        SmallReads {
            bytes: Cursor::new(bytes),
        },
        false,
    );
    assert_eq!(
        ws.recv_text().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn masking_and_utf8_survive_chunk_and_fragment_boundaries() {
    let text = "한글\u{1f349}".repeat(2500);
    // Deliberately split inside UTF-8 and at a non-mask-aligned offset.
    let split = 8193;
    let mut bytes = Vec::new();
    for (opcode, fin, body) in [
        (1, false, &text.as_bytes()[..split]),
        (0, true, &text.as_bytes()[split..]),
    ] {
        bytes.extend_from_slice(&[opcode | if fin { 128 } else { 0 }, 0xFE]);
        bytes.extend_from_slice(&(body.len() as u16).to_be_bytes());
        let mask = [1, 17, 39, 200];
        bytes.extend_from_slice(&mask);
        bytes.extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    }
    let mut ws = WsConn::established(
        SmallReads {
            bytes: Cursor::new(bytes),
        },
        true,
    );
    assert_eq!(ws.recv_text().unwrap(), text);
}

#[test]
fn invalid_control_lengths_and_reserved_length_bit_are_protocol_errors() {
    for bytes in [
        vec![0x89, 126, 0, 126],
        vec![0x09, 0],
        vec![0x81, 127, 128, 0, 0, 0, 0, 0, 0, 0],
    ] {
        let mut ws = WsConn::established(
            SmallReads {
                bytes: Cursor::new(bytes),
            },
            false,
        );
        assert_eq!(
            ws.recv_text().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
