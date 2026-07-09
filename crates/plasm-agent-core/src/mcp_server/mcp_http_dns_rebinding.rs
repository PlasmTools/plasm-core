//! DNS rebinding protection for Streamable HTTP MCP on loopback (MCP security best practices).

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Axum middleware: reject MCP requests whose `Host` or `Origin` name a non-localhost host.
pub async fn reject_dns_rebinding(req: Request<Body>, next: Next) -> Response {
    if let Some(deny) = dns_rebinding_denial(req.headers()) {
        return deny;
    }
    next.run(req).await
}

fn dns_rebinding_denial(headers: &HeaderMap) -> Option<Response> {
    if dns_rebinding_disabled() {
        return None;
    }
    if let Some(host) = header_value(headers, header::HOST) {
        if !is_allowed_localhost_host(&host) {
            return Some(deny_response(
                "invalid Host header for localhost MCP server",
            ));
        }
    }
    if let Some(origin) = header_value(headers, header::ORIGIN) {
        if !is_allowed_localhost_origin(&origin) {
            return Some(deny_response(
                "invalid Origin header for localhost MCP server",
            ));
        }
    }
    None
}

fn dns_rebinding_disabled() -> bool {
    matches!(
        std::env::var("PLASM_MCP_DNS_REBINDING_PROTECTION")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

fn header_value(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn deny_response(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({
            "error": "forbidden",
            "error_description": message,
        })),
    )
        .into_response()
}

/// Returns true when `host` is `localhost`, `127.0.0.1`, or IPv6 loopback, optionally with port.
pub fn is_allowed_localhost_host(host: &str) -> bool {
    let Some(hostname) = host_header_hostname(host) else {
        return false;
    };
    let hostname = hostname.to_ascii_lowercase();
    matches!(hostname.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn host_header_hostname(host: &str) -> Option<&str> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    if host.starts_with('[') {
        let end = host.find(']')?;
        return Some(&host[1..end]);
    }
    if let Some((name, port)) = host.rsplit_once(':') {
        if port.chars().all(|c| c.is_ascii_digit()) {
            return Some(name);
        }
    }
    Some(host)
}

/// Returns true when `origin` is an `http`/`https` URL whose host is loopback.
pub fn is_allowed_localhost_origin(origin: &str) -> bool {
    let origin = origin.trim();
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(name)) => is_allowed_localhost_host(name),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_localhost_hosts_with_port() {
        assert!(is_allowed_localhost_host("127.0.0.1:3000"));
        assert!(is_allowed_localhost_host("localhost:8080"));
        assert!(is_allowed_localhost_host("[::1]:3000"));
    }

    #[test]
    fn rejects_non_localhost_host() {
        assert!(!is_allowed_localhost_host("evil.example.com"));
        assert!(!is_allowed_localhost_host("evil.example.com:443"));
    }

    #[test]
    fn allows_localhost_origin() {
        assert!(is_allowed_localhost_origin("http://127.0.0.1:3000"));
        assert!(is_allowed_localhost_origin("http://localhost/mcp"));
    }

    #[test]
    fn rejects_attacker_origin() {
        assert!(!is_allowed_localhost_origin("http://evil.example.com"));
    }

    #[test]
    fn denies_evil_host_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "evil.example.com".parse().unwrap());
        assert!(dns_rebinding_denial(&headers).is_some());
    }

    #[test]
    fn allows_valid_host_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:3000".parse().unwrap());
        assert!(dns_rebinding_denial(&headers).is_none());
    }
}
