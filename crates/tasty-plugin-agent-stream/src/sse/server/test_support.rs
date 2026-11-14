//! Test fixtures keep the listening socket owned through server startup.

use std::net::TcpListener;
use std::sync::Arc;

use super::{ServeConfig, Server, Shared, SseHub, SseServer, start_bound};

pub(crate) struct ReservedEndpoint {
    listener: TcpListener,
}

impl ReservedEndpoint {
    pub(crate) fn new() -> Self {
        Self {
            listener: TcpListener::bind("127.0.0.1:0").expect("reserve endpoint"),
        }
    }

    pub(crate) fn port(&self) -> u16 {
        self.listener.local_addr().expect("reserved address").port()
    }

    pub(crate) fn start(
        self,
        config: ServeConfig,
        hub: Arc<SseHub>,
        registry: Shared,
    ) -> Result<SseServer, String> {
        let expected = self.listener.local_addr().expect("reserved address");
        assert_eq!(config.bind, expected.ip().to_string());
        assert_eq!(config.port, expected.port());
        let server = Server::from_listener(self.listener, None).map_err(|e| e.to_string())?;
        start_bound(config, server, hub, registry)
    }
}

#[test]
fn ownership_transfer_keeps_the_reserved_address_occupied() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex;
    use std::time::Duration;

    let endpoint = ReservedEndpoint::new();
    let port = endpoint.port();
    let address = ("127.0.0.1", port);
    assert!(
        TcpListener::bind(address).is_err(),
        "reservation owns the port"
    );
    let registry = Arc::new(Mutex::new(crate::registry::StreamRegistry::new(None)));
    let config = ServeConfig {
        bind: address.0.into(),
        port,
        token: None,
    };
    let mut server = endpoint
        .start(config, Arc::new(SseHub::default()), registry)
        .expect("transfer listener ownership");
    assert!(
        TcpListener::bind(address).is_err(),
        "server still owns the port"
    );
    assert_eq!(server.to_json()["port"], port);
    assert_eq!(
        server.to_json()["url"],
        format!("http://127.0.0.1:{port}/events")
    );
    let mut client = TcpStream::connect(address).expect("connect to transferred listener");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    server.shutdown();
}
