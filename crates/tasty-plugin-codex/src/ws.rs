//! Codex app-server 연결에 쓰는 최소 WebSocket 클라이언트(RFC 6455).
//!
//! app-server는 unix socket·loopback TCP·`app-server proxy`의 stdio 모두에서 WebSocket 프레임에
//! JSON-RPC 텍스트를 싣는다. 텍스트 메시지 송수신, ping 응답, close 처리만 구현한다.

use std::io::{Read, Write};

pub(crate) struct WsConn<S: Read + Write> {
    stream: S,
    server: bool,
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(buf)
}

impl<S: Read + Write> WsConn<S> {
    /// HTTP Upgrade를 보내고 101 응답을 확인한다. bearer가 있으면 Authorization 헤더에 싣는다.
    pub(crate) fn handshake(
        mut stream: S,
        host: &str,
        bearer: Option<&str>,
    ) -> std::io::Result<Self> {
        let key = base64(&random_bytes::<16>()?);
        let mut request = format!(
            "GET / HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
        );
        if let Some(token) = bearer {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte)? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed during the websocket handshake",
                ));
            }
            head.push(byte[0]);
            if head.len() > 16 * 1024 {
                return Err(std::io::Error::other(
                    "websocket handshake response too large",
                ));
            }
        }
        let status = String::from_utf8_lossy(&head);
        let first = status.lines().next().unwrap_or_default().to_string();
        if !first.contains(" 101 ") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("websocket upgrade refused: {first}"),
            ));
        }
        Ok(Self {
            stream,
            server: false,
        })
    }

    pub(crate) fn established(stream: S, server: bool) -> Self {
        Self { stream, server }
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        let mask = random_bytes::<4>()?;
        let mask_bit = if self.server { 0 } else { 0x80 };
        let mut frame = Vec::new();
        let capacity = payload.len().checked_add(14).ok_or_else(|| {
            std::io::Error::other("websocket frame length exceeds the address space")
        })?;
        frame.try_reserve(capacity).map_err(std::io::Error::other)?;
        frame.push(0x80 | opcode);
        let n = payload.len();
        if n < 126 {
            frame.push(mask_bit | n as u8);
        } else if n <= usize::from(u16::MAX) {
            frame.push(mask_bit | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            frame.push(mask_bit | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
        if self.server {
            frame.extend_from_slice(payload);
        } else {
            frame.extend_from_slice(&mask);
            frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        }
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    pub(crate) fn send_text(&mut self, text: &str) -> std::io::Result<()> {
        self.send_frame(0x1, text.as_bytes())
    }

    /// close 프레임을 보낸다. 실패는 연결을 끝내는 중이라 호출자가 무시해도 된다.
    pub(crate) fn close(&mut self) -> std::io::Result<()> {
        self.send_frame(0x8, &[])
    }

    fn read_exact(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.stream.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Grow from bytes actually received, not the peer's advertised frame length. Messages are
    /// still assembled for JSON-RPC; allocation failure is an error, not a size-based refusal.
    fn read_payload(
        &mut self,
        mut remaining: u64,
        mask: Option<&[u8]>,
        output: &mut Vec<u8>,
    ) -> std::io::Result<()> {
        let mut chunk = [0u8; 8192];
        let mut offset = 0;
        while remaining != 0 {
            let count = remaining.min(chunk.len() as u64) as usize;
            self.stream.read_exact(&mut chunk[..count])?;
            if let Some(mask) = mask {
                for (index, byte) in chunk[..count].iter_mut().enumerate() {
                    *byte ^= mask[(offset + index) % 4];
                }
            }
            output.try_reserve(count).map_err(std::io::Error::other)?;
            output.extend_from_slice(&chunk[..count]);
            remaining -= count as u64;
            offset = (offset + count) % 4;
        }
        Ok(())
    }

    /// 다음 텍스트 메시지를 읽는다. ping에는 pong으로 답하고 close는 EOF 오류로 돌려준다.
    pub(crate) fn recv_text(&mut self) -> std::io::Result<String> {
        let mut message = Vec::new();
        loop {
            let head = self.read_exact(2)?;
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 0x0F;
            let masked = head[1] & 0x80 != 0;
            let mut len = u64::from(head[1] & 0x7F);
            if len == 126 {
                let b = self.read_exact(2)?;
                len = u64::from(u16::from_be_bytes([b[0], b[1]]));
            } else if len == 127 {
                let b = self.read_exact(8)?;
                len = u64::from_be_bytes(b.try_into().unwrap_or([0; 8]));
            }
            if len > i64::MAX as u64 || (opcode >= 8 && (!fin || len > 125)) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid websocket frame length or control frame",
                ));
            }
            let mask = if masked {
                Some(self.read_exact(4)?)
            } else {
                None
            };
            match opcode {
                0x8..=0xA => {
                    let mut data = Vec::new();
                    self.read_payload(len, mask.as_deref(), &mut data)?;
                    if opcode == 0x9 {
                        self.send_frame(0xA, &data)?;
                    } else if opcode == 0x8 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "websocket closed by the server",
                        ));
                    }
                }
                0x0..=0x2 => {
                    self.read_payload(len, mask.as_deref(), &mut message)?;
                    if fin {
                        return String::from_utf8(message).map_err(std::io::Error::other);
                    }
                }
                _ => return Err(std::io::Error::other("unknown websocket opcode")),
            }
        }
    }
}

#[cfg(test)]
#[path = "ws_receive_tests.rs"]
mod receive_tests;

#[cfg(test)]
pub(crate) mod test_server {
    //! 시험용 서버 쪽 프레임 처리. 클라이언트 프레임은 마스크되어 있다.
    use std::io::{Read, Write};

    pub(crate) fn accept_handshake<S: Read + Write>(s: &mut S) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            .unwrap();
        String::from_utf8_lossy(&head).into_owned()
    }

    /// 텍스트 프레임을 읽는다. pong은 건너뛰고 close면 None이다.
    pub(crate) fn read_text<S: Read>(s: &mut S) -> Option<String> {
        loop {
            let (opcode, data) = read_frame(s)?;
            match opcode {
                0xA => continue,
                0x8 => return None,
                _ => return String::from_utf8(data).ok(),
            }
        }
    }

    /// 클라이언트 프레임 하나의 opcode와 마스크를 푼 내용.
    pub(crate) fn read_frame<S: Read>(s: &mut S) -> Option<(u8, Vec<u8>)> {
        let mut head = [0u8; 2];
        s.read_exact(&mut head).ok()?;
        let mut len = u64::from(head[1] & 0x7F);
        if len == 126 {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).ok()?;
            len = u64::from(u16::from_be_bytes(b));
        } else if len == 127 {
            let mut b = [0u8; 8];
            s.read_exact(&mut b).ok()?;
            len = u64::from_be_bytes(b);
        }
        let mut mask = [0u8; 4];
        s.read_exact(&mut mask).ok()?;
        let mut data = vec![0u8; len as usize];
        s.read_exact(&mut data).ok()?;
        for (i, b) in data.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        Some((head[0] & 0x0F, data))
    }

    pub(crate) fn write_text<S: Write>(s: &mut S, text: &str) {
        let mut frame = vec![0x81];
        let n = text.len();
        if n < 126 {
            frame.push(n as u8);
        } else if n <= 65535 {
            frame.push(126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            frame.push(127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
        frame.extend_from_slice(text.as_bytes());
        s.write_all(&frame).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn text_round_trips_through_masked_frames_of_every_length_form() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let head = test_server::accept_handshake(&mut s);
            assert!(head.contains("Authorization: Bearer tok\r\n"), "{head}");
            // ping에 pong이 오는지는 다음 메시지 순서로 확인한다.
            s.write_all(&[0x89, 0x01, b'p']).unwrap();
            let mut pongs = Vec::new();
            let mut echoed = 0;
            while echoed < 3 {
                let (opcode, data) = test_server::read_frame(&mut s).unwrap();
                if opcode == 0xA {
                    pongs.push(data);
                    continue;
                }
                test_server::write_text(&mut s, &String::from_utf8(data).unwrap());
                echoed += 1;
            }
            assert_eq!(
                pongs,
                vec![b"p".to_vec()],
                "ping에는 같은 내용의 pong으로 답한다"
            );
        });
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut ws = WsConn::handshake(stream, "127.0.0.1", Some("tok")).unwrap();
        for text in ["a".to_string(), "b".repeat(300), "c".repeat(70_000)] {
            ws.send_text(&text).unwrap();
            assert_eq!(ws.recv_text().unwrap(), text);
        }
        server.join().unwrap();
    }

    #[test]
    fn a_refused_upgrade_is_an_error_not_a_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let read = s.read(&mut buf).unwrap();
            assert!(read > 0, "업그레이드 요청을 받아야 한다");
            s.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n").unwrap();
        });
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let err = WsConn::handshake(stream, "127.0.0.1", None).err().unwrap();
        assert!(err.to_string().contains("401"), "{err}");
        server.join().unwrap();
    }
}
