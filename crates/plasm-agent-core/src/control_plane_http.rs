//! Shared control-plane HTTP auth: `x-plasm-control-plane-secret` vs `PLASM_MCP_CONTROL_PLANE_SECRET`,
//! plus optional OSS local setup via `x-plasm-outbound-setup-secret` / `PLASM_OUTBOUND_SETUP_SECRET`.

use axum::http::HeaderMap;
use subtle::ConstantTimeEq;

pub(crate) const X_PLASM_CONTROL_PLANE_SECRET: &str = "x-plasm-control-plane-secret";
pub const X_PLASM_OUTBOUND_SETUP_SECRET: &str = "x-plasm-outbound-setup-secret";

const DEV_PLANE_SECRET_FALLBACK: &str = "dev-plasm-mcp-control-plane-secret-32chars-min!!";

/// Secret used to authorize internal HTTP handlers (dev default when env unset).
/// Returns `None` when configured secret is too short (handlers must reject).
pub(crate) fn control_plane_secret_for_internal_http(
    too_short_log: &'static str,
) -> Option<String> {
    let expected = std::env::var("PLASM_MCP_CONTROL_PLANE_SECRET")
        .unwrap_or_else(|_| DEV_PLANE_SECRET_FALLBACK.to_string());
    if expected.len() < 16 {
        tracing::warn!(
            "PLASM_MCP_CONTROL_PLANE_SECRET too short; rejecting {}",
            too_short_log
        );
        return None;
    }
    Some(expected)
}

/// Strict secret from env only (no dev default). For outbound callbacks that must not use implicit dev keys.
pub(crate) fn control_plane_secret_from_env_strict() -> Option<String> {
    let secret = std::env::var("PLASM_MCP_CONTROL_PLANE_SECRET").ok()?;
    let secret = secret.trim().to_string();
    (secret.len() >= 16).then_some(secret)
}

pub fn control_plane_headers_authorized(headers: &HeaderMap, too_short_log: &'static str) -> bool {
    let Some(expected) = control_plane_secret_for_internal_http(too_short_log) else {
        return false;
    };
    let Some(got) = headers
        .get(X_PLASM_CONTROL_PLANE_SECRET)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    if got.len() != expected.len() {
        return false;
    }
    got.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Strict secret from env only (no dev default). For OSS/desktop outbound OAuth admin routes.
fn outbound_setup_secret_from_env_strict() -> Option<String> {
    let secret = std::env::var("PLASM_OUTBOUND_SETUP_SECRET").ok()?;
    let secret = secret.trim().to_string();
    (secret.len() >= 16).then_some(secret)
}

pub fn outbound_setup_headers_authorized(headers: &HeaderMap) -> bool {
    let Some(expected) = outbound_setup_secret_from_env_strict() else {
        return false;
    };
    let Some(got) = headers
        .get(X_PLASM_OUTBOUND_SETUP_SECRET)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    if got.len() != expected.len() {
        return false;
    }
    got.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Hosted control-plane secret **or** OSS outbound setup secret (when `PLASM_OUTBOUND_SETUP_SECRET` is set).
pub fn internal_or_outbound_setup_authorized(
    headers: &HeaderMap,
    too_short_log: &'static str,
) -> bool {
    control_plane_headers_authorized(headers, too_short_log)
        || outbound_setup_headers_authorized(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    fn with_plasm_outbound_setup_secret<R>(secret: &str, f: impl FnOnce() -> R) -> R {
        const KEY: &str = "PLASM_OUTBOUND_SETUP_SECRET";
        let old = std::env::var_os(KEY);
        std::env::set_var(KEY, secret);
        let out = f();
        match old {
            Some(v) => std::env::set_var(KEY, v),
            None => std::env::remove_var(KEY),
        }
        out
    }

    #[test]
    fn internal_or_outbound_setup_rejects_empty_headers() {
        let headers = HeaderMap::new();
        assert!(!internal_or_outbound_setup_authorized(&headers, "test"));
    }

    #[test]
    fn internal_or_outbound_setup_accepts_dev_control_plane_secret_by_default() {
        let mut headers = HeaderMap::new();
        let control_hdr: HeaderName = X_PLASM_CONTROL_PLANE_SECRET.parse().unwrap();
        headers.insert(
            control_hdr,
            HeaderValue::from_static("dev-plasm-mcp-control-plane-secret-32chars-min!!"),
        );
        assert!(internal_or_outbound_setup_authorized(&headers, "test"));
    }

    #[test]
    fn outbound_setup_secret_authorizes_when_env_and_header_match() {
        let secret = "0123456789012345678901234567890ab";
        with_plasm_outbound_setup_secret(secret, || {
            let mut headers = HeaderMap::new();
            let outbound_hdr: HeaderName = X_PLASM_OUTBOUND_SETUP_SECRET.parse().unwrap();
            headers.insert(outbound_hdr, HeaderValue::from_str(secret).unwrap());
            assert!(internal_or_outbound_setup_authorized(&headers, "test"));
        });
    }

    #[test]
    fn outbound_setup_secret_rejects_mismatch() {
        let secret = "0123456789012345678901234567890ab";
        with_plasm_outbound_setup_secret(secret, || {
            let mut headers = HeaderMap::new();
            let outbound_hdr: HeaderName = X_PLASM_OUTBOUND_SETUP_SECRET.parse().unwrap();
            headers.insert(
                outbound_hdr,
                HeaderValue::from_static("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            );
            assert!(!internal_or_outbound_setup_authorized(&headers, "test"));
        });
    }
}
