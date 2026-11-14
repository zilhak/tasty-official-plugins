//! The TUI peer deliberately does not use WsConn's receiver: it checks the relay's wire output.
use super::*;
use crate::ws::test_server;

fn frame(socket: &mut TcpStream, opcode: u8, fin: bool, body: &[u8]) {
    let mut header = vec![opcode | if fin { 0x80 } else { 0 }, 127];
    header.extend_from_slice(&(body.len() as u64).to_be_bytes());
    socket.write_all(&header).unwrap();
    socket.write_all(body).unwrap();
}

fn receive(socket: &mut TcpStream) -> String {
    let mut header = [0; 2];
    socket.read_exact(&mut header).unwrap();
    assert_eq!(header[0], 0x81);
    assert_eq!(header[1] & 0x80, 0);
    let length = match header[1] {
        126 => {
            let mut bytes = [0; 2];
            socket.read_exact(&mut bytes).unwrap();
            u64::from(u16::from_be_bytes(bytes))
        }
        127 => {
            let mut bytes = [0; 8];
            socket.read_exact(&mut bytes).unwrap();
            u64::from_be_bytes(bytes)
        }
        n => u64::from(n),
    };
    assert!(length < 32 * 1024 * 1024, "bounded synthetic fixture");
    let mut body = vec![0; length as usize];
    socket.read_exact(&mut body).unwrap();
    String::from_utf8(body).unwrap()
}

fn catalog(size: usize) -> String {
    let prefix = "{ \"result\": {\"marketplaces\": [{\"description\": \"";
    let suffix = "\"}]}, \"id\": 1 }";
    format!(
        "{prefix}{}{suffix}",
        "x".repeat(size - prefix.len() - suffix.len())
    )
}

#[test]
fn catalogs_cross_the_old_limit_intact_and_keep_the_connection_usable() {
    let sizes = [
        17_889_960,
        16 * 1024 * 1024 - 1,
        16 * 1024 * 1024,
        16 * 1024 * 1024 + 1,
    ];
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        test_server::accept_handshake(&mut socket);
        for (index, size) in sizes.into_iter().enumerate() {
            let request = test_server::read_text(&mut socket).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["method"],
                "plugin/list"
            );
            let text = catalog(size);
            if index == 3 {
                let mid = text.len() / 2;
                frame(&mut socket, 1, false, &text.as_bytes()[..mid]);
                socket.write_all(&[0x89, 1, b'p']).unwrap();
                assert_eq!(
                    test_server::read_frame(&mut socket),
                    Some((0xA, vec![b'p']))
                );
                frame(&mut socket, 0, true, &text.as_bytes()[mid..]);
            } else {
                frame(&mut socket, 1, true, text.as_bytes());
            }
        }
        // A large TUI request also passes through, without rewriting its JSON or widening policy.
        let request = test_server::read_text(&mut socket).unwrap();
        assert_eq!(request, large_turn());
        test_server::write_text(&mut socket, "{\"id\":2,\"result\":{}}");
        let request = test_server::read_text(&mut socket).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&request).unwrap()["method"],
            "account/read"
        );
        test_server::write_text(&mut socket, "{ \"id\": 3, \"result\": {\"ok\":true} }");
        test_server::read_text(&mut socket);
    });
    let relay = Relay::start(
        Endpoint::Ws {
            port,
            token: "backend".into(),
        },
        ResumeSettings::default(),
        Arc::new(|_| panic!("catalogs must not bind a conversation")),
    )
    .unwrap();
    let port: u16 = relay.url.rsplit(':').next().unwrap().parse().unwrap();
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut sender =
        WsConn::handshake(socket.try_clone().unwrap(), "localhost", Some(&relay.token)).unwrap();
    for size in sizes {
        sender
            .send_text("{\"id\":1,\"method\":\"plugin/list\",\"params\":{}}")
            .unwrap();
        assert_eq!(receive(&mut socket), catalog(size));
    }
    sender.send_text(&large_turn()).unwrap();
    assert_eq!(receive(&mut socket), "{\"id\":2,\"result\":{}}");
    sender
        .send_text("{\"id\":3,\"method\":\"account/read\",\"params\":{}}")
        .unwrap();
    assert_eq!(
        receive(&mut socket),
        "{ \"id\": 3, \"result\": {\"ok\":true} }"
    );
    sender.close().unwrap();
    drop(sender);
    drop(socket);
    drop(relay);
    server.join().unwrap();
}

fn large_turn() -> String {
    format!(
        "{{ \"id\": 2, \"method\": \"turn/start\", \"params\": {{\"input\":\"{}\"}} }}",
        "y".repeat(17 * 1024 * 1024)
    )
}

#[test]
fn a_null_thread_reply_still_resolves_the_pending_policy_check() {
    let pending: Pending = Arc::new(Mutex::new(HashMap::from([(
        "1".into(),
        ResumeSettings {
            approval: Some("never".into()),
            ..Default::default()
        },
    )])));
    let bind: Bind = Arc::new(|_| panic!("invalid policy cannot bind"));
    let error = message::reply(r#"{"id":1,"result":null}"#, &pending, &bind)
        .unwrap()
        .unwrap();
    assert!(serde_json::from_str::<Value>(&error).unwrap()["error"].is_object());
    assert!(pending.lock().unwrap().is_empty());
}

#[test]
fn malformed_json_is_diagnosed_without_copying_its_content() {
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let chosen: Chosen = Arc::new(AtomicBool::new(false));
    let text = r#"{"id":1,"method":{"secret":"do-not-log"}}"#;
    let error = message::request(text, &ResumeSettings::default(), &chosen, &pending).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!error.to_string().contains("do-not-log"));
    let bind: Bind = Arc::new(|_| panic!("not a thread response"));
    assert_eq!(
        message::reply("{", &pending, &bind).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
