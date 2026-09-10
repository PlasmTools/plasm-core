//! Type-state modeling for the outbound OAuth browser link flow (`/internal/oauth-link/v1/start`
//! → IdP → `/oauth/link/callback`).
//!
//! - **`OauthLinkSession<AwaitingIdpRedirect>`**: pending row stored in KV; may build authorize URL
//!   or consume an IdP callback.
//! - **`OauthLinkSession<AwaitingTokenExchange>`**: authorization `code` received; must run token
//!   exchange (consuming the PKCE verifier) before storage.
//!
//! KV JSON is **`OauthPendingRecordV1`**: same flattened fields as the historical pending struct, plus
//! optional `pending_version` (defaults to `1` on deserialize for backward compatibility).
//!
//! The **authorize URL and PKCE verifier** for new sessions come from
//! [`plasm_runtime::begin_authorization_code_pkce`] (see `http_oauth_link` start handler);
//! token exchange still uses the same verifier via [`OauthLinkSession::exchange_and_store`].

use plasm_runtime::{
    build_oauth_token_http_client, post_oauth_token_form_json, ApplyTokenError, OutboundOAuthKvV1,
    TokenEndpointResponseSummary,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::Instrument;

/// KV key prefix for in-flight OAuth link sessions (`{PENDING_PREFIX}{csrf_state}`).
pub const PENDING_PREFIX: &str = "plasm:oauth_link:pending:";
pub const PENDING_TTL: Duration = Duration::from_secs(900);

mod sealed {
    pub trait Sealed {}
}

/// Phase marker for [`OauthLinkSession`].
pub trait Phase: sealed::Sealed {}

/// Pending session stored in KV; browser has not yet returned with `code` / `error`.
#[derive(Debug, Clone, Copy)]
pub struct AwaitingIdpRedirect;

impl sealed::Sealed for AwaitingIdpRedirect {}
impl Phase for AwaitingIdpRedirect {}

/// Authorization code received; PKCE verifier must be used exactly once in token exchange.
#[derive(Debug, Clone)]
pub struct AwaitingTokenExchange {
    pub authorization_code: AuthorizationCode,
}

impl sealed::Sealed for AwaitingTokenExchange {}
impl Phase for AwaitingTokenExchange {}

/// Opaque CSRF `state` value sent to the IdP and used as the pending KV key suffix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CsrfState(String);

impl CsrfState {
    /// CSRF value from [`plasm_runtime::OAuthAuthorizationStart`] (must match pending KV key suffix).
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Full pending KV key (`plasm:oauth_link:pending:{uuid}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingKvKey(String);

impl PendingKvKey {
    pub fn from_csrf(csrf: &CsrfState) -> Self {
        Self(format!("{PENDING_PREFIX}{}", csrf.as_str()))
    }

    pub fn from_state_query_param(state: &str) -> Option<Self> {
        let t = state.trim();
        if t.is_empty() {
            return None;
        }
        Some(Self(format!("{PENDING_PREFIX}{t}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn csrf_state(&self) -> CsrfState {
        CsrfState(
            self.0
                .strip_prefix(PENDING_PREFIX)
                .unwrap_or("")
                .to_string(),
        )
    }
}

/// Authorization code returned by the IdP (opaque string).
#[derive(Debug, Clone)]
pub struct AuthorizationCode(String);

impl AuthorizationCode {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Shared fields persisted in KV for the pending OAuth link session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OauthPendingCore {
    pub code_verifier: String,
    pub hosted_kv_key: String,
    pub entry_id: String,
    pub return_url: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub client_secret: String,
    /// When the control plane ties this link to an outbound auth row (e.g. Postgres `auth_config_id`).
    #[serde(default)]
    pub auth_config_id: Option<String>,
    /// SHA-256 (hex) of sorted requested scopes from the authorize step — for logs without echoing full scope URLs.
    #[serde(default)]
    pub requested_scopes_sha256: Option<String>,
}

fn default_pending_version() -> u32 {
    1
}

/// Versioned pending blob stored at `{PENDING_PREFIX}{csrf}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OauthPendingRecordV1 {
    /// Forward-compatible KV blob version (`1` = current layout).
    #[serde(default = "default_pending_version")]
    pub(crate) pending_version: u32,
    #[serde(flatten)]
    pub core: OauthPendingCore,
}

impl OauthPendingRecordV1 {
    pub fn from_core(core: OauthPendingCore) -> Self {
        Self {
            pending_version: 1,
            core,
        }
    }
}

/// Raw IdP callback query (Axum); convert immediately with [`IdpCallback::parse`].
#[derive(Debug, Deserialize)]
pub struct CallbackQueryRaw {
    pub state: Option<String>,
    pub code: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Query string could not be classified as IdP success or error callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdpCallbackParseError {
    MissingState,
    MissingCodeOrError,
}

/// CSRF `state` from the callback does not match the loaded pending session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OauthCallbackStateMismatch;

/// IdP callback after the type-state boundary: invalid query combinations are rejected here.
#[derive(Debug, Clone)]
pub enum IdpCallback {
    AuthorizationError {
        state: CsrfState,
        error: String,
        error_description: Option<String>,
    },
    AuthorizationSuccess {
        state: CsrfState,
        code: AuthorizationCode,
    },
}

impl IdpCallback {
    /// Parse callback query. If both `error` and `code` are present, **`error` wins** (matches prior
    /// handler behavior and typical IdP error redirects).
    pub fn parse(q: &CallbackQueryRaw) -> Result<Self, IdpCallbackParseError> {
        let state_raw = q.state.as_deref().unwrap_or("").trim();
        if state_raw.is_empty() {
            return Err(IdpCallbackParseError::MissingState);
        }
        let state = CsrfState(state_raw.to_string());

        let err_trimmed = q.error.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());

        if let Some(err) = err_trimmed {
            let desc = q
                .error_description
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            return Ok(Self::AuthorizationError {
                state,
                error: err.to_string(),
                error_description: desc,
            });
        }

        let code_trimmed = q.code.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());

        if let Some(c) = code_trimmed {
            return Ok(Self::AuthorizationSuccess {
                state,
                code: AuthorizationCode::new(c.to_string()),
            });
        }

        Err(IdpCallbackParseError::MissingCodeOrError)
    }
}

/// Outcome of [`OauthLinkSession::on_idp_callback`] when the IdP returns an error (no token exchange).
#[derive(Debug, Clone)]
pub struct OauthTerminalErrorRedirect {
    pub return_url: String,
    pub oauth_error: String,
}

pub struct OauthLinkSession<P: Phase> {
    pub pending_key: PendingKvKey,
    pub core: OauthPendingCore,
    pub phase: P,
}

impl OauthLinkSession<AwaitingIdpRedirect> {
    pub fn begin(csrf: CsrfState, core: OauthPendingCore) -> Self {
        let pending_key = PendingKvKey::from_csrf(&csrf);
        Self {
            pending_key,
            core,
            phase: AwaitingIdpRedirect,
        }
    }

    /// Reconstruct after loading KV; `pending_key` must match the callback `state` query param.
    pub fn from_loaded(pending_key: PendingKvKey, core: OauthPendingCore) -> Self {
        Self {
            pending_key,
            core,
            phase: AwaitingIdpRedirect,
        }
    }

    pub fn csrf_state(&self) -> CsrfState {
        self.pending_key.csrf_state()
    }

    pub fn to_pending_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&OauthPendingRecordV1::from_core(self.core.clone()))
    }

    /// Consumes the awaiting-redirect session. On IdP error, returns a terminal redirect for the SaaS.
    /// On success, returns the next phase (token exchange). CSRF `state` must match this session.
    pub fn on_idp_callback(
        self,
        cb: IdpCallback,
    ) -> Result<
        Result<OauthLinkSession<AwaitingTokenExchange>, OauthTerminalErrorRedirect>,
        OauthCallbackStateMismatch,
    > {
        let expected = self.csrf_state();
        match &cb {
            IdpCallback::AuthorizationError { state, .. }
            | IdpCallback::AuthorizationSuccess { state, .. } => {
                if state.as_str() != expected.as_str() {
                    return Err(OauthCallbackStateMismatch);
                }
            }
        }

        match cb {
            IdpCallback::AuthorizationError {
                error,
                error_description,
                ..
            } => {
                let msg = error_description.unwrap_or(error);
                Ok(Err(OauthTerminalErrorRedirect {
                    return_url: self.core.return_url.clone(),
                    oauth_error: msg,
                }))
            }
            IdpCallback::AuthorizationSuccess { code, .. } => Ok(Ok(OauthLinkSession {
                pending_key: self.pending_key,
                core: self.core,
                phase: AwaitingTokenExchange {
                    authorization_code: code,
                },
            })),
        }
    }
}

impl OauthLinkSession<AwaitingTokenExchange> {
    /// Run the authorization-code + PKCE token exchange, store [`OutboundOAuthKvV1`] at
    /// `core.hosted_kv_key`, and return redirect parameters for the SaaS.
    pub async fn exchange_and_store(
        self,
        redirect_uri: String,
        http_timeout: Duration,
    ) -> Result<OauthLinkCompleted, OauthExchangeError> {
        let span = crate::spans::oauth_link_token_exchange(&self.core.entry_id);
        self.exchange_and_store_inner(redirect_uri, http_timeout)
            .instrument(span)
            .await
    }

    async fn exchange_and_store_inner(
        self,
        redirect_uri: String,
        http_timeout: Duration,
    ) -> Result<OauthLinkCompleted, OauthExchangeError> {
        let http = build_oauth_token_http_client(http_timeout)
            .map_err(|_| OauthExchangeError::HttpClient)?;
        let mut form = HashMap::new();
        form.insert("grant_type".into(), "authorization_code".into());
        form.insert("client_id".into(), self.core.client_id.clone());
        form.insert("client_secret".into(), self.core.client_secret.clone());
        form.insert(
            "code".into(),
            self.phase.authorization_code.as_str().to_string(),
        );
        form.insert("redirect_uri".into(), redirect_uri);
        form.insert("code_verifier".into(), self.core.code_verifier.clone());

        let token_json = post_oauth_token_form_json(
            &http,
            self.core.token_endpoint.trim(),
            form,
            http_timeout,
            "OAuth authorization code exchange",
        )
        .await
        .map_err(|e| {
            tracing::warn!(
                target: "plasm_agent::oauth_link",
                entry_id = %self.core.entry_id,
                oauth.phase = "token_exchange",
                token_endpoint = %self.core.token_endpoint.trim(),
                error = %e,
                "oauth token exchange: POST token endpoint failed (see error for IdP error_code / error_description)"
            );
            OauthExchangeError::TokenExchange
        })?;

        let summary = TokenEndpointResponseSummary::from_value(&token_json);

        tracing::info!(
            target: "plasm_agent::oauth_link",
            entry_id = %self.core.entry_id,
            oauth.phase = "token_exchange",
            token.top_level_keys = ?summary.top_level_keys,
            token.has_access_token = summary.has_access_token,
            token.access_token_len = ?summary.access_token_len,
            token.has_refresh_token = summary.has_refresh_token,
            token.refresh_token_len = ?summary.refresh_token_len,
            token.has_id_token = summary.has_id_token,
            token.id_token_len = ?summary.id_token_len,
            token.token_type = ?summary.token_type,
            token.scope = ?summary.scope,
            token.expires_in = ?summary.expires_in,
            token.rfc6749_error = ?summary.rfc6749_error,
            token.rfc6749_error_description = ?summary.rfc6749_error_description,
            "oauth token exchange: token endpoint JSON summary (redacted)"
        );

        let envelope = match OutboundOAuthKvV1::from_token_json_for_entry(
            self.core.entry_id.clone(),
            &token_json,
        ) {
            Ok(e) => e,
            Err(e) => {
                let kind = apply_token_error_kind(&e);
                let (rfc_error, rfc_desc) = match &e {
                    ApplyTokenError::OAuthTokenEndpoint(oe) => {
                        (Some(oe.error.as_str()), oe.error_description.as_deref())
                    }
                    _ => (None, None),
                };
                tracing::warn!(
                    target: "plasm_agent::oauth_link",
                    entry_id = %self.core.entry_id,
                    oauth.phase = "token_exchange",
                    apply_error_kind = kind,
                    apply_error = %e,
                    rfc6749_error = rfc_error,
                    rfc6749_error_description = rfc_desc,
                    token.top_level_keys = ?summary.top_level_keys,
                    token.has_access_token = summary.has_access_token,
                    token.has_id_token = summary.has_id_token,
                    token.scope = ?summary.scope,
                    token.rfc6749_error = ?summary.rfc6749_error,
                    "oauth token exchange: envelope parse failed (ApplyTokenError)"
                );
                return Err(OauthExchangeError::TokenResponse(e));
            }
        };

        let envelope_bytes =
            serde_json::to_vec(&envelope).map_err(|_| OauthExchangeError::SerializeEnvelope)?;

        Ok(OauthLinkCompleted {
            return_url: self.core.return_url,
            hosted_kv_key: self.core.hosted_kv_key,
            entry_id: self.core.entry_id,
            envelope_bytes,
            pending_key: self.pending_key,
            auth_config_id: self.core.auth_config_id.clone(),
            requested_scopes_sha256: self.core.requested_scopes_sha256.clone(),
            granted_scope: summary.scope.clone(),
        })
    }
}

fn apply_token_error_kind(e: &ApplyTokenError) -> &'static str {
    match e {
        ApplyTokenError::OAuthTokenEndpoint(_) => "oauth_token_endpoint",
        ApplyTokenError::OpenIdConnectIdTokenWithoutAccessToken => {
            "oidc_id_token_without_access_token"
        }
        ApplyTokenError::MissingAccessToken => "missing_access_token",
        ApplyTokenError::AccessTokenNotJsonString => "access_token_not_json_string",
    }
}

/// Successful token exchange; caller stores `envelope_bytes` at `hosted_kv_key` and deletes `pending_key`.
#[derive(Debug)]
pub struct OauthLinkCompleted {
    pub return_url: String,
    pub hosted_kv_key: String,
    pub entry_id: String,
    pub envelope_bytes: Vec<u8>,
    pub pending_key: PendingKvKey,
    pub auth_config_id: Option<String>,
    pub requested_scopes_sha256: Option<String>,
    /// `scope` field from the token endpoint response (granted scopes).
    pub granted_scope: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OauthExchangeError {
    #[error("token request failed")]
    HttpClient,
    #[error("token exchange failed")]
    TokenExchange,
    #[error(transparent)]
    TokenResponse(#[from] ApplyTokenError),
    #[error("failed to serialize token")]
    SerializeEnvelope,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn idp_callback_error_wins_over_code() {
        let q = CallbackQueryRaw {
            state: Some("abc".into()),
            code: Some("should_ignore".into()),
            error: Some("access_denied".into()),
            error_description: Some("nope".into()),
        };
        match IdpCallback::parse(&q).unwrap() {
            IdpCallback::AuthorizationError {
                state,
                error,
                error_description,
            } => {
                assert_eq!(state.as_str(), "abc");
                assert_eq!(error, "access_denied");
                assert_eq!(error_description.as_deref(), Some("nope"));
            }
            IdpCallback::AuthorizationSuccess { .. } => panic!("expected error branch"),
        }
    }

    #[test]
    fn idp_callback_success_requires_code() {
        let q = CallbackQueryRaw {
            state: Some("s".into()),
            code: Some("  c0de  ".into()),
            error: None,
            error_description: None,
        };
        match IdpCallback::parse(&q).unwrap() {
            IdpCallback::AuthorizationSuccess { code, .. } => {
                assert_eq!(code.as_str(), "c0de");
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn idp_callback_missing_state() {
        let q = CallbackQueryRaw {
            state: None,
            code: Some("x".into()),
            error: None,
            error_description: None,
        };
        assert!(matches!(
            IdpCallback::parse(&q),
            Err(IdpCallbackParseError::MissingState)
        ));
    }

    #[test]
    fn pending_record_legacy_json_roundtrip() {
        let core = OauthPendingCore {
            code_verifier: "v".into(),
            hosted_kv_key: "plasm:outbound:v1:u".into(),
            entry_id: "e".into(),
            return_url: "https://app/cb".into(),
            token_endpoint: "https://idp/token".into(),
            client_id: "cid".into(),
            client_secret: "sec".into(),
            auth_config_id: None,
            requested_scopes_sha256: None,
        };
        let legacy = serde_json::json!({
            "code_verifier": "v",
            "hosted_kv_key": "plasm:outbound:v1:u",
            "entry_id": "e",
            "return_url": "https://app/cb",
            "token_endpoint": "https://idp/token",
            "client_id": "cid",
            "client_secret": "sec",
        });
        let parsed: OauthPendingRecordV1 = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.pending_version, 1);
        assert_eq!(parsed.core, core);
    }

    #[test]
    fn session_transition_success() {
        let csrf = CsrfState::new(Uuid::new_v4().to_string());
        let core = OauthPendingCore {
            code_verifier: "ver".into(),
            hosted_kv_key: "kv".into(),
            entry_id: "ent".into(),
            return_url: "https://r".into(),
            token_endpoint: "https://t".into(),
            client_id: "c".into(),
            client_secret: "s".into(),
            auth_config_id: None,
            requested_scopes_sha256: None,
        };
        let sess = OauthLinkSession::begin(csrf.clone(), core);
        let cb = IdpCallback::AuthorizationSuccess {
            state: csrf,
            code: AuthorizationCode::new("co"),
        };
        let next = sess.on_idp_callback(cb).unwrap().unwrap();
        assert_eq!(next.phase.authorization_code.as_str(), "co");
    }

    #[test]
    fn session_transition_rejects_state_mismatch() {
        let sess = OauthLinkSession::begin(
            CsrfState("one".into()),
            OauthPendingCore {
                code_verifier: "v".into(),
                hosted_kv_key: "k".into(),
                entry_id: "e".into(),
                return_url: "r".into(),
                token_endpoint: "t".into(),
                client_id: "c".into(),
                client_secret: "s".into(),
                auth_config_id: None,
                requested_scopes_sha256: None,
            },
        );
        let cb = IdpCallback::AuthorizationSuccess {
            state: CsrfState("two".into()),
            code: AuthorizationCode::new("x"),
        };
        assert!(matches!(
            sess.on_idp_callback(cb),
            Err(OauthCallbackStateMismatch)
        ));
    }
}
