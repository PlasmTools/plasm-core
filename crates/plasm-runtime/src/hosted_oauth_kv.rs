//! Structured OAuth credentials stored in auth-framework KV at `plasm:outbound:*` keys.
//!
//! Values are JSON [`OutboundOAuthKvV1`] with `entry_id` for provider resolution and optional `refresh_token`.

use crate::RuntimeError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// JSON `version` field for [`OutboundOAuthKvV1`].
pub const OUTBOUND_OAUTH_KV_VERSION: u32 = 1;

/// Skew (seconds) before nominal expiry when we treat an access token as expired for proactive refresh
/// and for in-memory client-credentials cache (see `auth.rs` `resolve_oauth2` margin).
pub const HOSTED_OAUTH_EXPIRY_SKEW_SECS: u64 = 30;

/// Wall-clock Unix seconds (UTC).
pub fn unix_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build a `reqwest` client for OAuth token endpoint calls.
pub fn build_oauth_token_http_client(
    default_timeout: Duration,
) -> Result<reqwest::Client, RuntimeError> {
    reqwest::Client::builder()
        .timeout(default_timeout)
        .build()
        .map_err(|e| RuntimeError::AuthenticationError {
            message: format!("Failed to build HTTP client for OAuth token: {e}"),
        })
}

/// POST `application/x-www-form-urlencoded` to a token URL; expect JSON. Used for refresh and client credentials.
pub async fn post_oauth_token_form_json(
    http: &reqwest::Client,
    token_url: &str,
    form: HashMap<String, String>,
    per_request_timeout: Duration,
    error_context: &str,
) -> Result<serde_json::Value, RuntimeError> {
    let response = http
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .timeout(per_request_timeout)
        .send()
        .await
        .map_err(|e| RuntimeError::AuthenticationError {
            message: format!("{error_context} request failed: {e}"),
        })?;
    let status = response.status();
    let body: serde_json::Value =
        response
            .json()
            .await
            .map_err(|e| RuntimeError::AuthenticationError {
                message: format!("{error_context} response is not valid JSON: {e}"),
            })?;
    if !status.is_success() {
        return Err(oauth_token_json_error(status, &body, error_context));
    }
    Ok(body)
}

fn oauth_token_json_error(
    status: reqwest::StatusCode,
    body: &serde_json::Value,
    context: &str,
) -> RuntimeError {
    let err_code = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let err_desc = body
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    RuntimeError::AuthenticationError {
        message: format!("{context} (HTTP {status}): {err_code} {err_desc}")
            .trim()
            .to_string(),
    }
}

/// True when `err` is an OAuth 2.0 token endpoint failure with `error=invalid_grant`
/// (refresh token revoked, re-authorization required).
///
/// Prefer this over substring checks on [`RuntimeError`] display output: it keys off
/// [`RuntimeError::AuthenticationError`] messages produced by this module’s token helpers.
pub fn runtime_error_is_oauth_invalid_grant(err: &RuntimeError) -> bool {
    match err {
        RuntimeError::AuthenticationError { message } => {
            authentication_error_message_is_oauth_invalid_grant(message)
        }
        _ => false,
    }
}

fn authentication_error_message_is_oauth_invalid_grant(message: &str) -> bool {
    // oauth_token_json_error: "{context} (HTTP {status}): {err_code} {err_desc}"
    if let Some(idx) = message.rfind("): ") {
        let tail = message[idx + 3..].trim_start();
        let code = tail.split_whitespace().next().unwrap_or("");
        if code == "invalid_grant" {
            return true;
        }
    }
    message.contains("invalid_grant")
}

/// OAuth-linked outbound credential blob (v1) stored in KV.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundOAuthKvV1 {
    pub version: u32,
    /// Registry `entry_id` used to resolve `token_endpoint`, `client_id`, and client secret at refresh time.
    pub entry_id: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    /// Unix seconds when `access_token` is treated as expired (refresh before this minus skew).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Account-specific HTTP origin (e.g. Fibery workspace host) when catalog `http_backend` is a placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_backend: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OutboundOAuthKvParseError {
    #[error("hosted outbound credential is empty")]
    Empty,
    #[error("hosted outbound credential must be JSON object (OutboundOAuthKvV1), not a bare token string")]
    NotJsonObject,
    #[error("invalid JSON for hosted outbound credential: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported hosted outbound credential version: {0} (expected {1})")]
    UnsupportedVersion(u32, u32),
}

/// OAuth 2.0 token endpoint error parameters ([RFC 6749 Section 5.2](https://www.rfc-editor.org/rfc/rfc6749#section-5.2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokenEndpointError {
    pub error: String,
    pub error_description: Option<String>,
}

impl std::fmt::Display for OAuthTokenEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "OAuth 2.0 token error (RFC 6749 Section 5.2): {}",
            self.error
        )?;
        if let Some(d) = &self.error_description {
            write!(f, " — {}", d)?;
        }
        Ok(())
    }
}

impl std::error::Error for OAuthTokenEndpointError {}

#[derive(Debug, thiserror::Error)]
pub enum ApplyTokenError {
    #[error(transparent)]
    OAuthTokenEndpoint(#[from] OAuthTokenEndpointError),
    /// [OpenID Connect Core §3.1.3.3](https://openid.net/specs/openid-connect-core-1_0.html#TokenResponse):
    /// `access_token` is REQUIRED unless only the ID Token is returned. Google often does this when
    /// the consent only granted OpenID / `openid` scopes — **not** a missing refresh token; add Gmail API scopes.
    #[error("OpenID Connect token response contained id_token but no access_token (OIDC Core §3.1.3.3); request resource-server OAuth scopes, not OpenID-only")]
    OpenIdConnectIdTokenWithoutAccessToken,
    #[error(
        "OAuth 2.0 successful token response missing non-empty access_token (RFC 6749 Section 5.1)"
    )]
    MissingAccessToken,
    #[error("OAuth 2.0 access_token is not a JSON string (RFC 6749 Section 5.1)")]
    AccessTokenNotJsonString,
}

fn oauth_token_endpoint_error_from_json(
    body: &serde_json::Value,
) -> Option<OAuthTokenEndpointError> {
    let err = body.get("error")?;
    if err.is_null() {
        return None;
    }
    let code = err.as_str()?.trim();
    if code.is_empty() {
        return None;
    }
    Some(OAuthTokenEndpointError {
        error: code.to_string(),
        error_description: body
            .get("error_description")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// RFC 6749 `expires_in` is a number; some providers send a decimal string — accept both.
fn expires_in_seconds(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

impl From<ApplyTokenError> for RuntimeError {
    fn from(e: ApplyTokenError) -> Self {
        RuntimeError::AuthenticationError {
            message: e.to_string(),
        }
    }
}

/// Non-empty refresh token, if the provider returned one.
pub fn usable_refresh_token(env: &OutboundOAuthKvV1) -> Option<&str> {
    env.refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// After parsing hosted KV UTF-8: either return a bearer token, or an envelope that needs refresh.
#[derive(Debug, Clone)]
pub enum HostedBearerResolution {
    Ready(String),
    NeedsRefresh(OutboundOAuthKvV1),
}

/// Classify trimmed UTF-8 credential payload: JSON [`OutboundOAuthKvV1`] only.
pub fn classify_hosted_bearer_utf8(
    trimmed: &str,
    skew_secs: u64,
) -> Result<HostedBearerResolution, RuntimeError> {
    if trimmed.is_empty() {
        return Err(RuntimeError::AuthenticationError {
            message: "Hosted bearer credential is empty or whitespace-only.".to_string(),
        });
    }
    let env =
        parse_outbound_oauth_kv_v1(trimmed).map_err(|e| RuntimeError::AuthenticationError {
            message: format!("Invalid hosted OAuth credential: {e}"),
        })?;
    let now = unix_secs_now();
    if !env.needs_proactive_refresh(now, skew_secs) {
        return Ok(HostedBearerResolution::Ready(env.access_token.clone()));
    }
    if usable_refresh_token(&env).is_none() {
        return Err(RuntimeError::AuthenticationError {
            message:
                "OAuth access token expired and no refresh_token is stored; re-link the account."
                    .to_string(),
        });
    }
    Ok(HostedBearerResolution::NeedsRefresh(env))
}

/// Default [`SecretProvider::resolve_hosted_bearer`] path: no network refresh.
pub fn resolve_hosted_bearer_default_no_refresh(raw: &str) -> Result<String, RuntimeError> {
    let trimmed = raw.trim();
    match classify_hosted_bearer_utf8(trimmed, HOSTED_OAUTH_EXPIRY_SKEW_SECS)? {
        HostedBearerResolution::Ready(t) => Ok(t),
        HostedBearerResolution::NeedsRefresh(_) => Err(RuntimeError::AuthenticationError {
            message: "OAuth access token expired; hosted refresh requires plasm.".to_string(),
        }),
    }
}

/// Parse KV UTF-8 payload as JSON [`OutboundOAuthKvV1`] (version [`OUTBOUND_OAUTH_KV_VERSION`] only).
pub fn parse_outbound_oauth_kv_v1(
    raw: &str,
) -> Result<OutboundOAuthKvV1, OutboundOAuthKvParseError> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(OutboundOAuthKvParseError::Empty);
    }
    if !t.starts_with('{') {
        return Err(OutboundOAuthKvParseError::NotJsonObject);
    }
    let v: serde_json::Value = serde_json::from_str(t)?;
    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    if version != OUTBOUND_OAUTH_KV_VERSION {
        return Err(OutboundOAuthKvParseError::UnsupportedVersion(
            version,
            OUTBOUND_OAUTH_KV_VERSION,
        ));
    }
    Ok(serde_json::from_value(v)?)
}

/// Optional per-account HTTP origin stored alongside a hosted outbound credential (Fibery workspace URL).
pub fn hosted_outbound_http_backend_from_kv(raw: &str) -> Option<String> {
    let env = parse_outbound_oauth_kv_v1(raw).ok()?;
    env.http_backend
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

impl OutboundOAuthKvV1 {
    /// `true` if a proactive refresh should run (access past expiry minus skew).
    pub fn needs_proactive_refresh(&self, now_unix: u64, skew_secs: u64) -> bool {
        match self.expires_at_unix {
            None => false,
            Some(exp) => now_unix.saturating_add(skew_secs) >= exp,
        }
    }

    /// Build a v1 envelope from an authorization-code or refresh token JSON body.
    pub fn from_token_json_for_entry(
        entry_id: impl Into<String>,
        body: &serde_json::Value,
    ) -> Result<Self, ApplyTokenError> {
        let mut env = Self {
            version: OUTBOUND_OAUTH_KV_VERSION,
            entry_id: entry_id.into(),
            access_token: String::new(),
            refresh_token: None,
            token_type: None,
            expires_at_unix: None,
            scope: None,
            http_backend: None,
        };
        env.apply_token_response(body)?;
        Ok(env)
    }

    /// Merge a successful token JSON response (access + optional refresh rotation + expires_in).
    ///
    /// Distinguishes:
    /// - [RFC 6749 Section 5.2](https://www.rfc-editor.org/rfc/rfc6749#section-5.2) error responses (`error`, …)
    ///   even if a non-conformant server returned HTTP 2xx.
    /// - [RFC 6749 Section 5.1](https://www.rfc-editor.org/rfc/rfc6749#section-5.1) success parameters.
    /// - [OpenID Connect Core §3.1.3.3](https://openid.net/specs/openid-connect-core-1_0.html#TokenResponse)
    ///   (`id_token` without `access_token` when only the ID Token is returned).
    pub fn apply_token_response(
        &mut self,
        body: &serde_json::Value,
    ) -> Result<(), ApplyTokenError> {
        if let Some(e) = oauth_token_endpoint_error_from_json(body) {
            return Err(ApplyTokenError::OAuthTokenEndpoint(e));
        }

        match body.get("access_token") {
            None => {
                if body.get("id_token").is_some() {
                    return Err(ApplyTokenError::OpenIdConnectIdTokenWithoutAccessToken);
                }
                return Err(ApplyTokenError::MissingAccessToken);
            }
            Some(v) if v.is_null() => {
                if body.get("id_token").is_some() {
                    return Err(ApplyTokenError::OpenIdConnectIdTokenWithoutAccessToken);
                }
                return Err(ApplyTokenError::MissingAccessToken);
            }
            Some(v) => {
                if let Some(s) = v.as_str() {
                    if s.is_empty() {
                        return Err(ApplyTokenError::MissingAccessToken);
                    }
                    self.access_token = s.to_string();
                } else {
                    return Err(ApplyTokenError::AccessTokenNotJsonString);
                }
            }
        }

        if let Some(rt) = body
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            self.refresh_token = Some(rt.to_string());
        }
        if let Some(tt) = body
            .get("token_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            self.token_type = Some(tt.to_string());
        }
        if let Some(sc) = body
            .get("scope")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            self.scope = Some(sc.to_string());
        }
        if let Some(secs) = body.get("expires_in").and_then(expires_in_seconds) {
            self.expires_at_unix = Some(unix_secs_now().saturating_add(secs));
        } else {
            self.expires_at_unix = None;
        }
        Ok(())
    }
}

/// OAuth2 `refresh_token` grant.
pub async fn oauth_refresh_token_request(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<serde_json::Value, RuntimeError> {
    let mut form = HashMap::new();
    form.insert("grant_type".into(), "refresh_token".into());
    form.insert("refresh_token".into(), refresh_token.to_string());
    form.insert("client_id".into(), client_id.to_string());
    form.insert("client_secret".into(), client_secret.to_string());
    post_oauth_token_form_json(
        http,
        token_endpoint,
        form,
        Duration::from_secs(30),
        "OAuth refresh",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_bare_jwt_string() {
        let t = "eyJhbGciOiJIUzI1NiJ9.xyz";
        let err = parse_outbound_oauth_kv_v1(t).unwrap_err();
        assert!(
            matches!(err, OutboundOAuthKvParseError::NotJsonObject),
            "{err}"
        );
    }

    #[test]
    fn parse_v1_round_trip() {
        let env = OutboundOAuthKvV1 {
            version: OUTBOUND_OAUTH_KV_VERSION,
            entry_id: "linear".into(),
            access_token: "acc".into(),
            refresh_token: Some("ref".into()),
            token_type: Some("Bearer".into()),
            expires_at_unix: Some(1_700_000_000),
            scope: Some("read".into()),
            http_backend: Some("https://acme.fibery.io".into()),
        };
        let json = serde_json::to_string(&env).unwrap();
        let parsed = parse_outbound_oauth_kv_v1(&json).unwrap();
        assert_eq!(parsed, env);
        assert_eq!(
            hosted_outbound_http_backend_from_kv(&json).as_deref(),
            Some("https://acme.fibery.io")
        );
    }

    #[test]
    fn hosted_http_backend_trims_slash() {
        let raw = r#"{"version":1,"entry_id":"fibery","access_token":"Token x","http_backend":"https://acme.fibery.io/"}"#;
        assert_eq!(
            hosted_outbound_http_backend_from_kv(raw).as_deref(),
            Some("https://acme.fibery.io")
        );
    }

    #[test]
    fn parse_v1_round_trip_legacy_without_http_backend() {
        let env = OutboundOAuthKvV1 {
            version: OUTBOUND_OAUTH_KV_VERSION,
            entry_id: "linear".into(),
            access_token: "acc".into(),
            refresh_token: Some("ref".into()),
            token_type: Some("Bearer".into()),
            expires_at_unix: Some(1_700_000_000),
            scope: Some("read".into()),
            http_backend: None,
        };
        let json = serde_json::to_string(&env).unwrap();
        let parsed = parse_outbound_oauth_kv_v1(&json).unwrap();
        assert_eq!(parsed, env);
    }

    #[test]
    fn needs_proactive_refresh_respects_expiry() {
        let mut env = OutboundOAuthKvV1 {
            version: 1,
            entry_id: "e".into(),
            access_token: "a".into(),
            refresh_token: None,
            token_type: None,
            expires_at_unix: Some(1000),
            scope: None,
            http_backend: None,
        };
        assert!(!env.needs_proactive_refresh(900, HOSTED_OAUTH_EXPIRY_SKEW_SECS));
        assert!(env.needs_proactive_refresh(970, HOSTED_OAUTH_EXPIRY_SKEW_SECS));
        env.expires_at_unix = None;
        assert!(!env.needs_proactive_refresh(u64::MAX, HOSTED_OAUTH_EXPIRY_SKEW_SECS));
    }

    #[test]
    fn apply_token_response_rfc6749_error_takes_precedence() {
        let body = serde_json::json!({
            "error": "invalid_grant",
            "error_description": "Code was already redeemed"
        });
        let mut env = OutboundOAuthKvV1 {
            version: 1,
            entry_id: "e".into(),
            access_token: "old".into(),
            refresh_token: None,
            token_type: None,
            expires_at_unix: None,
            scope: None,
            http_backend: None,
        };
        let err = env.apply_token_response(&body).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid_grant"), "{msg}");
        assert!(msg.contains("already redeemed"), "{msg}");
    }

    #[test]
    fn apply_token_response_oidc_id_token_without_access_token() {
        let body = serde_json::json!({
            "id_token": "eyJ.header.payload.sig",
            "token_type": "Bearer",
            "expires_in": 3600
        });
        let mut env = OutboundOAuthKvV1 {
            version: 1,
            entry_id: "e".into(),
            access_token: "".into(),
            refresh_token: None,
            token_type: None,
            expires_at_unix: None,
            scope: None,
            http_backend: None,
        };
        assert!(matches!(
            env.apply_token_response(&body),
            Err(ApplyTokenError::OpenIdConnectIdTokenWithoutAccessToken)
        ));
    }

    #[test]
    fn apply_token_response_expires_in_accepts_string_seconds() {
        let mut env = OutboundOAuthKvV1 {
            version: 1,
            entry_id: "e".into(),
            access_token: "a".into(),
            refresh_token: None,
            token_type: None,
            expires_at_unix: None,
            scope: None,
            http_backend: None,
        };
        let body = serde_json::json!({
            "access_token": "a",
            "expires_in": "3600"
        });
        env.apply_token_response(&body).unwrap();
        assert!(env.expires_at_unix.is_some());
    }

    #[test]
    fn apply_token_response_updates_fields() {
        let mut env = OutboundOAuthKvV1 {
            version: 1,
            entry_id: "e".into(),
            access_token: "old".into(),
            refresh_token: Some("r1".into()),
            token_type: None,
            expires_at_unix: None,
            scope: None,
            http_backend: None,
        };
        let body = serde_json::json!({
            "access_token": "new",
            "refresh_token": "r2",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "a b"
        });
        env.apply_token_response(&body).unwrap();
        assert_eq!(env.access_token, "new");
        assert_eq!(env.refresh_token.as_deref(), Some("r2"));
        assert_eq!(env.token_type.as_deref(), Some("Bearer"));
        assert_eq!(env.scope.as_deref(), Some("a b"));
        assert!(env.expires_at_unix.is_some());
    }

    #[test]
    fn from_token_json_for_entry_matches_manual_envelope() {
        let body = serde_json::json!({
            "access_token": "a",
            "refresh_token": "r",
            "token_type": "Bearer",
            "expires_in": 60,
            "scope": "s"
        });
        let env = OutboundOAuthKvV1::from_token_json_for_entry("gh", &body).unwrap();
        assert_eq!(env.entry_id, "gh");
        assert_eq!(env.access_token, "a");
        assert_eq!(env.refresh_token.as_deref(), Some("r"));
        assert!(env.expires_at_unix.is_some());
    }

    #[test]
    fn classify_empty_refresh_string_requires_relink() {
        let env = OutboundOAuthKvV1 {
            version: 1,
            entry_id: "e".into(),
            access_token: "x".into(),
            refresh_token: Some("   ".into()),
            token_type: None,
            expires_at_unix: Some(1),
            scope: None,
            http_backend: None,
        };
        let j = serde_json::to_string(&env).unwrap();
        let err = classify_hosted_bearer_utf8(&j, HOSTED_OAUTH_EXPIRY_SKEW_SECS).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("re-link"), "{msg}");
    }

    #[tokio::test]
    async fn oauth_refresh_token_request_parses_success_json() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = r#"{"access_token":"newtok","expires_in":120,"refresh_token":"r2"}"#;
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains("grant_type=refresh_token"));
            assert!(req.contains("refresh_token=rt"));
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
        });
        let client = reqwest::Client::new();
        let v = oauth_refresh_token_request(
            &client,
            &format!("http://{addr}/token"),
            "cid",
            "csec",
            "rt",
        )
        .await
        .unwrap();
        assert_eq!(v["access_token"], "newtok");
        assert_eq!(v["refresh_token"], "r2");
    }

    #[tokio::test]
    async fn oauth_refresh_token_request_maps_http_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = r#"{"error":"invalid_grant","error_description":"revoked"}"#;
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let _req = String::from_utf8_lossy(&buf[..n]);
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
        });
        let client = reqwest::Client::new();
        let err = oauth_refresh_token_request(
            &client,
            &format!("http://{addr}/token"),
            "cid",
            "csec",
            "rt",
        )
        .await
        .unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("invalid_grant"), "{msg}");
        assert!(runtime_error_is_oauth_invalid_grant(&err), "{msg}");
    }
}
