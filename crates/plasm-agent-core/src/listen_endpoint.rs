//! TCP listen address resolution and client-facing HTTP origins for Plasm HTTP/MCP servers.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

const ENV_LISTEN_HOST: &str = "PLASM_LISTEN_HOST";
const LOOPBACK_V4: &str = "127.0.0.1";
const WILDCARD_V4: &str = "0.0.0.0";

/// Resolved HTTP/MCP bind target (`{host}:{port}`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcpListenEndpoint {
    pub host: String,
    pub port: u16,
}

impl TcpListenEndpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    pub fn from_cli(cli_listen_host: Option<&str>, port: u16) -> Result<Self, String> {
        let host = resolve_listen_host(cli_listen_host)?;
        Ok(Self { host, port })
    }

    /// Read `--listen-host` + `--port` from an MCP server [`clap::ArgMatches`].
    pub fn from_clap_matches(matches: &clap::ArgMatches) -> Result<Self, String> {
        let port = matches.get_one::<u16>("port").copied().unwrap_or(3000);
        let host = matches.get_one::<String>("listen_host").map(|s| s.as_str());
        Self::from_cli(host, port)
    }

    pub fn socket_addr(&self) -> Result<SocketAddr, String> {
        parse_socket_addr(&self.host, self.port)
    }

    pub async fn bind_tcp_listener(&self) -> std::io::Result<tokio::net::TcpListener> {
        let addr = self
            .socket_addr()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        tokio::net::TcpListener::bind(addr).await
    }

    /// Wire / boot / TUI label: `host:port`.
    pub fn display_addr(&self) -> String {
        display_listen_addr(&self.host, self.port)
    }

    /// HTTP origin for MCP JSON / `plasm init` copy (loopback when bind is wildcard).
    pub fn client_http_origin(&self) -> String {
        client_http_origin(&self.host, self.port)
    }

    pub fn client_mcp_streamable_url(&self) -> String {
        format!("{}/mcp", self.client_http_origin())
    }

    /// When bind is all-interfaces, local clients should use loopback — show in Status tab.
    pub fn local_client_hint_line(&self) -> Option<String> {
        if is_wildcard_bind_host(&self.host) {
            Some(format!(
                "  local clients: {}",
                client_http_origin(LOOPBACK_V4, self.port)
            ))
        } else {
            None
        }
    }
}

fn running_in_kubernetes() -> bool {
    std::env::var("KUBERNETES_SERVICE_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
}

fn default_listen_host() -> &'static str {
    if running_in_kubernetes() {
        WILDCARD_V4
    } else {
        LOOPBACK_V4
    }
}

fn normalize_host(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("listen host must not be empty".to_string());
    }
    if let Ok(sa) = SocketAddr::from_str(t) {
        return Err(format!(
            "listen host must not include a port (use --port for {}; got {sa})",
            sa.port()
        ));
    }
    Ok(t.to_string())
}

/// Precedence: CLI `--listen-host` → `PLASM_LISTEN_HOST` → default (loopback or wildcard in k8s).
pub fn resolve_listen_host(cli_override: Option<&str>) -> Result<String, String> {
    if let Some(cli) = cli_override {
        return normalize_host(cli);
    }
    if let Ok(env) = std::env::var(ENV_LISTEN_HOST) {
        let env = env.trim();
        if !env.is_empty() {
            return normalize_host(env);
        }
    }
    Ok(default_listen_host().to_string())
}

pub fn display_listen_addr(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

pub fn is_wildcard_bind_host(host: &str) -> bool {
    match host.trim() {
        "0.0.0.0" | "::" | "[::]" => true,
        s => s.parse::<IpAddr>().is_ok_and(|ip| ip.is_unspecified()),
    }
}

/// HTTP origin for client config copy (`http://host:port`).
pub fn client_http_origin(host: &str, port: u16) -> String {
    let client_host = if is_wildcard_bind_host(host) {
        LOOPBACK_V4
    } else {
        host.trim()
    };
    format!("http://{client_host}:{port}")
}

fn parse_socket_addr(host: &str, port: u16) -> Result<SocketAddr, String> {
    let host = host.trim();
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    format!("{host}:{port}")
        .parse::<SocketAddr>()
        .map_err(|e| format!("invalid listen address {host}:{port}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        keys: &'static [&'static str],
    }

    impl EnvGuard {
        fn new(keys: &'static [&'static str]) -> Self {
            for k in keys {
                std::env::remove_var(k);
            }
            Self { keys }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for k in self.keys {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    fn resolve_defaults_loopback_off_k8s() {
        let _g = EnvGuard::new(&[ENV_LISTEN_HOST, "KUBERNETES_SERVICE_HOST"]);
        assert_eq!(resolve_listen_host(None).unwrap(), LOOPBACK_V4);
    }

    #[test]
    fn resolve_defaults_wildcard_in_k8s() {
        let _g = EnvGuard::new(&[ENV_LISTEN_HOST]);
        std::env::set_var("KUBERNETES_SERVICE_HOST", "10.0.0.1");
        assert_eq!(resolve_listen_host(None).unwrap(), WILDCARD_V4);
    }

    #[test]
    fn resolve_cli_overrides_env() {
        let _g = EnvGuard::new(&[ENV_LISTEN_HOST, "KUBERNETES_SERVICE_HOST"]);
        std::env::set_var(ENV_LISTEN_HOST, "0.0.0.0");
        assert_eq!(resolve_listen_host(Some("127.0.0.1")).unwrap(), "127.0.0.1");
    }

    #[test]
    fn client_origin_maps_wildcard_to_loopback() {
        assert_eq!(client_http_origin("0.0.0.0", 3000), "http://127.0.0.1:3000");
        assert_eq!(
            client_http_origin("192.168.1.5", 8080),
            "http://192.168.1.5:8080"
        );
    }

    #[test]
    fn display_addr_formats_host_port() {
        let ep = TcpListenEndpoint::new("127.0.0.1", 3000);
        assert_eq!(ep.display_addr(), "127.0.0.1:3000");
    }

    #[test]
    fn socket_addr_parses_ipv4() {
        let ep = TcpListenEndpoint::new(LOOPBACK_V4, 0);
        assert_eq!(
            ep.socket_addr().unwrap(),
            SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0)
        );
    }
}
