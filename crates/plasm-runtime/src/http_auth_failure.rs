//! Domain-general HTTP auth-failure diagnostics (HTTP-1).
//!
//! A 401 names the outbound wire: method, path, query, Authorization present/absent.
//! When a credential is present, only a short tail is quoted. When a session login
//! `access_token` also exists, its tail is quoted and compared. Full secrets are never
//! printed (credential erasure is the narrow exception).

use crate::api_error_detail::{summarize_json_api_error_for_http, summarize_text_error_body};
use crate::auth::ResolvedAuth;
use crate::error::RuntimeError;
use plasm_compile::CompiledRequest;

/// Secret-safe Authorization fact for one outbound request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundAuthorizationFact {
    pub present: bool,
    pub scheme: Option<String>,
    pub token_tail: Option<String>,
}

impl Default for OutboundAuthorizationFact {
    fn default() -> Self {
        Self::absent()
    }
}

impl OutboundAuthorizationFact {
    #[must_use]
    pub fn absent() -> Self {
        Self {
            present: false,
            scheme: None,
            token_tail: None,
        }
    }

    #[must_use]
    pub fn from_header(value: Option<&str>) -> Self {
        let Some(raw) = value.map(str::trim).filter(|s| !s.is_empty()) else {
            return Self::absent();
        };
        let (scheme, token) = strip_auth_scheme(raw);
        let tail = credential_tail(token);
        Self {
            present: true,
            scheme: if scheme.is_empty() {
                None
            } else {
                Some(scheme.to_string())
            },
            token_tail: if tail.is_empty() { None } else { Some(tail) },
        }
    }
}

/// Token bytes after stripping a `Bearer` / `Token` scheme, if present.
#[must_use]
pub fn token_secret(header_or_token: &str) -> &str {
    strip_auth_scheme(header_or_token.trim()).1
}

/// Append login-result tail compare when a 401 line already named the request wire.
#[must_use]
pub fn append_login_token_compare(message: &str, login_tail: &str) -> String {
    let login_tail = login_tail.trim();
    if login_tail.is_empty() || message.contains("login access_token tail") {
        return message.to_string();
    }
    let mut out = message.to_string();
    out.push_str("; login access_token tail …");
    out.push_str(login_tail);
    if let Some(request_tail) = message
        .split("tail …")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
    {
        if request_tail == login_tail {
            out.push_str(" (same)");
        } else {
            out.push_str(" (different)");
        }
    }
    out
}

/// Last four Unicode scalars of a secret, for match/mismatch only.
#[must_use]
pub fn credential_tail(secret: &str) -> String {
    let trimmed = secret.trim();
    let count = trimmed.chars().count();
    if count == 0 {
        return String::new();
    }
    let take = count.min(4);
    trimmed.chars().skip(count - take).collect()
}

fn strip_auth_scheme(header: &str) -> (&str, &str) {
    for prefix in ["Bearer ", "bearer ", "Token ", "token "] {
        if let Some(rest) = header.strip_prefix(prefix) {
            return (prefix.trim(), rest.trim());
        }
    }
    ("", header.trim())
}

/// Path and query of an absolute or relative URL (origin omitted).
#[must_use]
pub fn url_path_and_query(url: &str) -> (String, String) {
    let trimmed = url.trim();
    let after_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let path_and_query = after_scheme
        .find('/')
        .map(|i| &after_scheme[i..])
        .unwrap_or("/");
    match path_and_query.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (path_and_query.to_string(), String::new()),
    }
}

fn query_slot(query: &str) -> &str {
    if query.is_empty() {
        "(none)"
    } else {
        query
    }
}

/// User-facing HTTP failure line. Status 401 is HTTP-1 (names the auth wire).
#[must_use]
pub fn format_http_status_error(
    method: &str,
    url: &str,
    status: u16,
    detail: &str,
    authorization: &OutboundAuthorizationFact,
    login_access_token_tail: Option<&str>,
) -> String {
    if status == 401 {
        return format_http_401_error(method, url, detail, authorization, login_access_token_tail);
    }
    if detail.is_empty() {
        format!("{method} {url} — HTTP {status}")
    } else {
        format!("{method} {url} — HTTP {status} from API: {detail}")
    }
}

fn format_http_401_error(
    method: &str,
    url: &str,
    detail: &str,
    authorization: &OutboundAuthorizationFact,
    login_access_token_tail: Option<&str>,
) -> String {
    let (path, query) = url_path_and_query(url);
    let query = query_slot(&query);
    let mut message = format!("{method} path={path} query={query} — HTTP 401 from API: {detail}");
    message.push_str("; Authorization: ");
    if !authorization.present {
        message.push_str("absent");
        return message;
    }
    message.push_str("present");
    if let Some(scheme) = authorization.scheme.as_deref() {
        message.push(' ');
        message.push('(');
        message.push_str(scheme);
        if let Some(tail) = authorization.token_tail.as_deref() {
            message.push_str(" tail …");
            message.push_str(tail);
        }
        message.push(')');
    } else if let Some(tail) = authorization.token_tail.as_deref() {
        message.push_str(" (tail …");
        message.push_str(tail);
        message.push(')');
    }
    let Some(login_tail) = login_access_token_tail
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return message;
    };
    message.push_str("; login access_token tail …");
    message.push_str(login_tail);
    match authorization.token_tail.as_deref() {
        Some(request_tail) if request_tail == login_tail => message.push_str(" (same)"),
        Some(_) => message.push_str(" (different)"),
        None => {}
    }
    message
}

#[must_use]
pub fn outbound_authorization_fact(
    request: &CompiledRequest,
    auth: Option<&ResolvedAuth>,
) -> OutboundAuthorizationFact {
    match crate::http_transport::compiled_template_headers(request, auth) {
        Ok(headers) => authorization_fact_from_header_pairs(&headers, auth),
        Err(_) => authorization_fact_from_header_pairs(&[], auth),
    }
}

#[must_use]
pub fn authorization_fact_from_resolved(auth: Option<&ResolvedAuth>) -> OutboundAuthorizationFact {
    authorization_fact_from_header_pairs(&[], auth)
}

fn authorization_fact_from_header_pairs(
    template_headers: &[(String, String)],
    auth: Option<&ResolvedAuth>,
) -> OutboundAuthorizationFact {
    for (key, value) in template_headers {
        if key.eq_ignore_ascii_case("authorization") {
            return OutboundAuthorizationFact::from_header(Some(value.as_str()));
        }
    }
    if let Some(resolved) = auth {
        for (key, value) in &resolved.headers {
            if key.eq_ignore_ascii_case("authorization") {
                return OutboundAuthorizationFact::from_header(Some(value.as_str()));
            }
        }
    }
    OutboundAuthorizationFact::absent()
}

fn host_body_detail(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "empty body".into();
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
        summarize_json_api_error_for_http(&json)
    } else {
        summarize_text_error_body(trimmed.as_bytes(), None)
    }
}

/// Host-callback / NAPI non-2xx → the same [`RuntimeError::RequestError`] liturgy as reqwest.
#[must_use]
pub fn request_error_from_host_http(
    method: &str,
    url: &str,
    authorization_header: Option<&str>,
    status: u16,
    body: &str,
) -> RuntimeError {
    let detail = host_body_detail(body);
    let authorization = OutboundAuthorizationFact::from_header(authorization_header);
    let message = format_http_status_error(method, url, status, &detail, &authorization, None);
    RuntimeError::RequestError {
        message,
        attempts: 1,
        status: Some(status),
        body: serde_json::from_str(body).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.signatureTAIL";

    #[test]
    fn http_401_bearer_present_names_wire_and_hides_token() {
        let auth = OutboundAuthorizationFact::from_header(Some(&format!("Bearer {JWT}")));
        let message = format_http_status_error(
            "GET",
            "https://api.example.com/users?query=alice",
            401,
            "Invalid credentials",
            &auth,
            None,
        );
        assert!(
            message.contains("GET path=/users query=query=alice"),
            "{message}"
        );
        assert!(
            message.contains("HTTP 401 from API: Invalid credentials"),
            "{message}"
        );
        assert!(
            message.contains("Authorization: present (Bearer tail …TAIL)"),
            "{message}"
        );
        assert!(!message.contains(JWT), "full JWT leaked: {message}");
        assert!(!message.contains("eyJhbGciOiJIUzI1NiJ9"), "{message}");
    }

    #[test]
    fn http_401_authorization_absent() {
        let message = format_http_status_error(
            "POST",
            "https://api.example.com/notes",
            401,
            "missing token",
            &OutboundAuthorizationFact::absent(),
            None,
        );
        assert!(
            message.contains("POST path=/notes query=(none)"),
            "{message}"
        );
        assert!(message.contains("Authorization: absent"), "{message}");
        assert!(!message.contains("tail"), "{message}");
        assert!(!message.contains("login access_token"), "{message}");
    }

    #[test]
    fn http_401_compares_login_tail_when_both_exist() {
        let auth = OutboundAuthorizationFact::from_header(Some(&format!("Bearer {JWT}")));
        let same = format_http_status_error(
            "GET",
            "https://api.example.com/users?q=x",
            401,
            "denied",
            &auth,
            Some("TAIL"),
        );
        assert!(
            same.contains("login access_token tail …TAIL (same)"),
            "{same}"
        );
        assert!(!same.contains(JWT), "{same}");

        let different = format_http_status_error(
            "GET",
            "https://api.example.com/users?q=x",
            401,
            "denied",
            &auth,
            Some("OTHR"),
        );
        assert!(
            different.contains("login access_token tail …OTHR (different)"),
            "{different}"
        );
    }

    #[test]
    fn host_401_uses_shared_liturgy() {
        let err = request_error_from_host_http(
            "GET",
            "https://api.example.com/users?query=alice",
            Some(&format!("Bearer {JWT}")),
            401,
            r#"{"message":"Invalid credentials"}"#,
        );
        let displayed = err.to_string();
        assert!(
            displayed.starts_with("HTTP request failed: GET path=/users"),
            "{displayed}"
        );
        assert!(displayed.contains("Authorization: present"), "{displayed}");
        assert!(!displayed.contains(JWT), "{displayed}");
        match err {
            RuntimeError::RequestError {
                status: Some(401), ..
            } => {}
            other => panic!("expected RequestError 401, got {other:?}"),
        }
    }

    #[test]
    fn append_login_compare_is_idempotent_and_secret_free() {
        let auth = OutboundAuthorizationFact::from_header(Some(&format!("Bearer {JWT}")));
        let base = format_http_status_error(
            "GET",
            "https://api.example.com/search?query=q",
            401,
            "denied",
            &auth,
            None,
        );
        let once = append_login_token_compare(&base, "TAIL");
        assert!(
            once.contains("login access_token tail …TAIL (same)"),
            "{once}"
        );
        assert_eq!(append_login_token_compare(&once, "TAIL"), once);
        assert!(!once.contains(JWT), "{once}");
    }
}
