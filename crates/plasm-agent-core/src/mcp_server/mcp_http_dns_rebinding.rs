//! DNS rebinding protection for Streamable HTTP MCP (MCP security best practices).
//!
//! Loopback-only servers reject non-local `Host` / `Origin`. When
//! `PLASM_MCP_PUBLIC_BASE_URL` is set (ingress / SaaS), the configured public host is
//! also allowed so clients can connect via the public hostname.

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// Axum middleware: reject MCP requests whose `Host` or `Origin` are not on the allowlist.
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
    let allowed = allowed_mcp_hostnames();
    if let Some(host) = header_value(headers, header::HOST) {
        if !host_matches_allowlist(&host, &allowed) {
            return Some(deny_response("invalid Host header for MCP server"));
        }
    }
    if let Some(origin) = header_value(headers, header::ORIGIN) {
        if !origin_matches_allowlist(&origin, &allowed) {
            return Some(deny_response("invalid Origin header for MCP server"));
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

fn push_host_from_url(out: &mut Vec<String>, raw: &str) {
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }
    if let Ok(url) = url::Url::parse(raw) {
        if let Some(host) = url.host_str() {
            push_hostname(out, host);
        }
    }
}

fn push_hostname(out: &mut Vec<String>, host: &str) {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() || out.iter().any(|h| h == &host) {
        return;
    }
    out.push(host);
}

fn push_host_list(out: &mut Vec<String>, raw: &str) {
    for part in raw.split(',') {
        push_hostname(out, part);
    }
}

/// Hostnames permitted on `Host` / `Origin` for MCP HTTP.
fn allowed_mcp_hostnames() -> Vec<String> {
    let mut out: Vec<String> = LOOPBACK_HOSTS.iter().map(|h| (*h).to_string()).collect();
    if let Ok(v) = std::env::var("PLASM_MCP_PUBLIC_BASE_URL") {
        push_host_from_url(&mut out, &v);
    }
    if let Ok(v) = std::env::var("PLASM_PUBLIC_WEB_ORIGIN") {
        push_host_from_url(&mut out, &v);
    }
    if let Ok(v) = std::env::var("PLASM_MCP_ALLOWED_HOSTS") {
        push_host_list(&mut out, &v);
    }
    out
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

fn host_matches_allowlist(host: &str, allowed: &[String]) -> bool {
    let Some(hostname) = host_header_hostname(host) else {
        return false;
    };
    let hostname = hostname.to_ascii_lowercase();
    allowed.iter().any(|a| a == &hostname)
}

fn origin_matches_allowlist(origin: &str, allowed: &[String]) -> bool {
    let origin = origin.trim();
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(name)) => host_matches_allowlist(name, allowed),
        Some(url::Host::Ipv4(ip)) => {
            ip.is_loopback() || host_matches_allowlist(&ip.to_string(), allowed)
        }
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback() || host_matches_allowlist(&format!("{ip}"), allowed)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback_allowlist() -> Vec<String> {
        LOOPBACK_HOSTS.iter().map(|h| (*h).to_string()).collect()
    }

    #[test]
    fn allows_localhost_hosts_with_port() {
        let allowed = loopback_allowlist();
        assert!(host_matches_allowlist("127.0.0.1:3000", &allowed));
        assert!(host_matches_allowlist("localhost:8080", &allowed));
        assert!(host_matches_allowlist("[::1]:3000", &allowed));
    }

    #[test]
    fn rejects_non_localhost_host() {
        let allowed = loopback_allowlist();
        assert!(!host_matches_allowlist("evil.example.com", &allowed));
        assert!(!host_matches_allowlist("evil.example.com:443", &allowed));
    }

    #[test]
    fn allows_localhost_origin() {
        let allowed = loopback_allowlist();
        assert!(origin_matches_allowlist("http://127.0.0.1:3000", &allowed));
        assert!(origin_matches_allowlist("http://localhost/mcp", &allowed));
    }

    #[test]
    fn rejects_attacker_origin() {
        let allowed = loopback_allowlist();
        assert!(!origin_matches_allowlist("http://evil.example.com", &allowed));
    }

    #[test]
    fn allows_public_host_when_on_allowlist() {
        let allowed = vec![
            "localhost".into(),
            "127.0.0.1".into(),
            "::1".into(),
            "platform.plasm.tools".into(),
        ];
        assert!(host_matches_allowlist("platform.plasm.tools", &allowed));
        assert!(origin_matches_allowlist(
            "https://platform.plasm.tools/plasm/mcp",
            &allowed
        ));
    }

    #[test]
    fn rejects_unlisted_host_even_with_public_on_allowlist() {
        let allowed = vec![
            "localhost".into(),
            "127.0.0.1".into(),
            "::1".into(),
            "platform.plasm.tools".into(),
        ];
        assert!(!host_matches_allowlist("evil.example.com", &allowed));
    }

    #[test]
    fn denies_evil_host_header() {
        let allowed = loopback_allowlist();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "evil.example.com".parse().unwrap());
        assert!(!host_matches_allowlist(
            headers.get(header::HOST).unwrap().to_str().unwrap(),
            &allowed
        ));
    }

    #[test]
    fn allows_valid_host_header() {
        let allowed = loopback_allowlist();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:3000".parse().unwrap());
        assert!(host_matches_allowlist(
            headers.get(header::HOST).unwrap().to_str().unwrap(),
            &allowed
        ));
    }
}
