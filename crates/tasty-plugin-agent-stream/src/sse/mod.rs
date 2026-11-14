//! 외부 소비자가 연결해 이벤트를 받는 SSE 서버.
//! frame은 프레임 작성, hub는 구독 큐, request는 요청 해석, server는 HTTP 연결을 맡는다.
//! 노출 정책은 docs/plugins/agent-stream/index.md#sse-엔드포인트 참고.

pub mod frame;
pub mod hub;
pub mod request;
pub mod server;

use std::net::IpAddr;

use serde_json::{Value, json};

/// SSE 서버 기동 설정.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServeConfig {
    pub bind: String,
    pub port: u16,
    /// 구독 토큰. `None` 이면 무인증(loopback 에서만 허용).
    pub token: Option<String>,
}

/// 설정이 거부되는 이유. 사람이 읽는 문자열은 호출자가 i18n 키로 만든다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// 포트 0 = "아무 포트나" 는 받지 않는다.
    PortRequired,
    /// bind 주소가 IP 로 해석되지 않는다.
    InvalidBind,
    /// loopback 이 아닌 주소에 토큰 없이 열려 했다.
    RemoteBindNeedsToken,
}

impl ServeConfig {
    /// 기본 바인딩 주소는 loopback이다.
    pub const DEFAULT_BIND: &'static str = "127.0.0.1";

    /// 포트를 명시해야 하며 loopback 외의 주소는 토큰이 있어야 한다.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::PortRequired);
        }
        let loopback = is_loopback(&self.bind).ok_or(ConfigError::InvalidBind)?;
        if !loopback && self.token.is_none() {
            return Err(ConfigError::RemoteBindNeedsToken);
        }
        Ok(())
    }

    /// 토큰을 제외한 조회용 JSON. 토큰 설정 여부만 포함한다.
    pub fn to_public_json(&self) -> Value {
        json!({
            "bind": self.bind,
            "port": self.port,
            "auth": self.token.is_some(),
        })
    }
}

/// bind 주소가 loopback 인가. IP 로 해석되지 않으면 `None`.
fn is_loopback(bind: &str) -> Option<bool> {
    if bind.eq_ignore_ascii_case("localhost") {
        return Some(true);
    }
    let addr: IpAddr = bind.parse().ok()?;
    Some(addr.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(bind: &str, port: u16, token: Option<&str>) -> ServeConfig {
        ServeConfig {
            bind: bind.to_string(),
            port,
            token: token.map(str::to_string),
        }
    }

    #[test]
    fn a_port_is_required_there_is_no_automatic_allocation() {
        assert_eq!(
            config("127.0.0.1", 0, None).validate(),
            Err(ConfigError::PortRequired)
        );
        assert!(config("127.0.0.1", 8123, None).validate().is_ok());
    }

    #[test]
    fn loopback_may_run_without_a_token_but_a_wider_bind_may_not() {
        assert!(config("127.0.0.1", 1, None).validate().is_ok());
        assert!(config("localhost", 1, None).validate().is_ok());
        assert!(config("::1", 1, None).validate().is_ok());
        assert_eq!(
            config("0.0.0.0", 1, None).validate(),
            Err(ConfigError::RemoteBindNeedsToken)
        );
        assert!(config("0.0.0.0", 1, Some("t")).validate().is_ok());
    }

    #[test]
    fn a_bind_address_that_is_not_an_ip_is_rejected() {
        assert_eq!(
            config("example.com", 1, Some("t")).validate(),
            Err(ConfigError::InvalidBind)
        );
    }

    #[test]
    fn the_public_view_never_carries_the_token() {
        let view = config("127.0.0.1", 9, Some("super-secret")).to_public_json();
        assert!(!view.to_string().contains("super-secret"));
        assert_eq!(view["auth"], json!(true));
    }
}
