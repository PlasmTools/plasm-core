//! OAuth 2.0 authorization-code linking for outbound `hosted_kv` secrets (browser redirect flow).
//!
//! - `POST /internal/oauth-link/v1/start` — control-plane auth; returns `{ "authorize_url": "…" }`.
//! - `GET /oauth/link/callback` — public redirect URI; exchanges code, stores a JSON v1 credential
//!   envelope (access + optional refresh + expiry + `entry_id`) at a preallocated `plasm:outbound:…` key,
//!   redirects back to the SaaS `return_url`.
//!
//! Flow state is modeled in [`crate::oauth_link_session`] (type-state phases + [`IdpCallback`] parsing).

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use plasm_runtime::{begin_authorization_code_pkce, OAuthAuthorizationStart, OAuthConnectError};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tracing::instrument;

use crate::control_plane_http::internal_or_outbound_setup_authorized;
use crate::oauth_link_catalog::{OauthResolveError, RuntimeOauthProviderMeta};
use crate::oauth_link_session::{
    CallbackQueryRaw, CsrfState, IdpCallback, IdpCallbackParseError, OauthCallbackStateMismatch,
    OauthExchangeError, OauthLinkSession, OauthPendingCore, OauthPendingRecordV1, PendingKvKey,
    PENDING_TTL,
};
use crate::server_state::PlasmHostState;
fn oauth_resolve_error_for_start_json(e: &OauthResolveError) -> (StatusCode, &'static str) {
    let status = match e {
        OauthResolveError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::NOT_FOUND,
    };
    let code = match e {
        OauthResolveError::UnknownEntry => "unknown_entry",
        OauthResolveError::SecretNotInKv => "secret_not_in_kv",
        OauthResolveError::BadSecretUtf8 => "bad_secret_utf8",
        OauthResolveError::Storage(_) => "storage_error",
    };
    (status, code)
}

type OauthStartJsonError = (StatusCode, Json<serde_json::Value>);

fn oauth_start_json_err(
    status: StatusCode,
    code: &str,
    message: &str,
    entry_id: Option<&str>,
) -> OauthStartJsonError {
    let mut o = json!({
        "error": code,
        "message": message,
    });
    if let Some(e) = entry_id {
        o["entry_id"] = json!(e);
    }
    (status, Json(o))
}

#[derive(Debug, Deserialize)]
struct ProviderUpsertBody {
    entry_id: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    default_scopes: Vec<String>,
    client_id: String,
    client_secret_key: String,
    #[serde(default = "default_provider_enabled")]
    enabled: bool,
}

fn default_provider_enabled() -> bool {
    true
}

async fn provider_upsert_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ProviderUpsertBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !internal_or_outbound_setup_authorized(&headers, "oauth-link provider-upsert") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let entry_id = body.entry_id.trim();
    if entry_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let Some(catalog) = st.oauth_link_catalog() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if body.enabled {
        let meta = match RuntimeOauthProviderMeta::try_new(
            &body.authorization_endpoint,
            &body.token_endpoint,
            body.default_scopes,
            &body.client_id,
            &body.client_secret_key,
        ) {
            Ok(m) => m,
            Err(_) => return Err(StatusCode::BAD_REQUEST),
        };
        catalog.upsert_runtime(entry_id.to_string(), meta).await;
        tracing::info!(
            target: "plasm_agent::oauth_link",
            entry_id = %entry_id,
            "oauth link provider-upsert: runtime OAuth provider enabled in catalog"
        );
    } else {
        catalog.remove_runtime(entry_id).await;
        tracing::info!(
            target: "plasm_agent::oauth_link",
            entry_id = %entry_id,
            "oauth link provider-upsert: runtime OAuth provider removed from catalog"
        );
    }
    Ok(Json(json!({ "ok": true })))
}

/// Stable hex(SHA256) of sorted scope strings joined by `\n` (for logs without listing scopes).
fn oauth_scope_list_sha256_hex(scopes: &[String]) -> String {
    let mut parts: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();
    parts.sort_unstable();
    let joined = parts.join("\n");
    hex::encode(Sha256::digest(joined.as_bytes()))
}

fn append_query_params(base: &str, pairs: &[(&str, &str)]) -> Result<String, ()> {
    let mut u = reqwest::Url::parse(base).map_err(|_| ())?;
    for (k, v) in pairs {
        u.query_pairs_mut().append_pair(k, v);
    }
    Ok(u.to_string())
}

#[derive(Debug, Deserialize)]
struct StartBody {
    entry_id: String,
    return_url: String,
    /// Optional space-delimited scope override (CGS / control-plane). When omitted or empty, uses provider `default_scopes` from the OAuth link catalog.
    #[serde(default)]
    scopes: Option<Vec<String>>,
    /// When set, correlates this flow with the control-plane outbound auth row (e.g. Postgres `auth_config_id`) for structured logs after token exchange.
    #[serde(default)]
    auth_config_id: Option<String>,
}

#[instrument(
    skip(st, headers, body),
    target = "plasm_agent::oauth_link",
    fields(oauth.phase = "start")
)]
async fn start_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<StartBody>,
) -> Result<Json<serde_json::Value>, OauthStartJsonError> {
    if !internal_or_outbound_setup_authorized(&headers, "oauth-link start") {
        return Err(oauth_start_json_err(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid or missing x-plasm-control-plane-secret / x-plasm-outbound-setup-secret",
            None,
        ));
    }
    let Some(catalog) = st.oauth_link_catalog() else {
        return Err(oauth_start_json_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "OAuth link catalog not configured",
            None,
        ));
    };
    let Some(storage) = st.auth_storage() else {
        return Err(oauth_start_json_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            "auth storage not configured",
            None,
        ));
    };
    let entry_id = body.entry_id.trim();
    if entry_id.is_empty() {
        return Err(oauth_start_json_err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "entry_id is required",
            None,
        ));
    }
    let return_url = body.return_url.trim();
    if return_url.is_empty() {
        return Err(oauth_start_json_err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "return_url is required",
            Some(entry_id),
        ));
    }
    if !catalog.return_url_allowed(return_url) {
        tracing::warn!(
            target: "plasm_agent::oauth_link",
            return_url = %return_url,
            "oauth link start: return_url not allowlisted (set PLASM_OAUTH_LINK_ALLOWED_RETURN_PREFIXES to include this origin)"
        );
        return Err(oauth_start_json_err(
            StatusCode::BAD_REQUEST,
            "return_url_not_allowed",
            "return_url does not match PLASM_OAUTH_LINK_ALLOWED_RETURN_PREFIXES",
            Some(entry_id),
        ));
    }
    let cfg = match catalog.resolve_for_oauth_start(storage, entry_id).await {
        Ok(c) => c,
        Err(e) => {
            let (status, code) = oauth_resolve_error_for_start_json(&e);
            if status == StatusCode::NOT_FOUND {
                tracing::warn!(entry_id = %entry_id, ?e, "oauth link start: provider resolve failed");
            } else {
                tracing::warn!(entry_id = %entry_id, ?e, "oauth link start: storage error resolving provider");
            }
            let msg = e.refresh_failure_message();
            return Err(oauth_start_json_err(status, code, &msg, Some(entry_id)));
        }
    };

    let (scope_list, scopes_source): (Vec<String>, &'static str) = match &body.scopes {
        Some(v) if !v.is_empty() => {
            let sl: Vec<String> = v
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if sl.is_empty() {
                (cfg.default_scopes.clone(), "catalog_default")
            } else {
                (sl, "request_body")
            }
        }
        _ => (cfg.default_scopes.clone(), "catalog_default"),
    };

    let scopes_sha256 = oauth_scope_list_sha256_hex(&scope_list);

    let oauth_start = match begin_authorization_code_pkce(
        &cfg.client_id,
        Some(cfg.client_secret.as_str()),
        cfg.authorization_endpoint.trim(),
        cfg.token_endpoint.trim(),
        catalog.redirect_uri.as_str(),
        &scope_list,
    ) {
        Ok(s) => s,
        Err(e) => {
            let msg = match &e {
                OAuthConnectError::InvalidUrl(m) => m.as_str(),
                OAuthConnectError::TokenExchange(m) => m.as_str(),
            };
            tracing::warn!(error = %e, "oauth link: begin_authorization_code_pkce failed");
            return Err(oauth_start_json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                msg,
                Some(entry_id),
            ));
        }
    };

    let OAuthAuthorizationStart {
        authorize_url,
        csrf_state,
        pkce_verifier,
    } = oauth_start;

    let csrf = CsrfState::new(csrf_state);
    let hosted_kv_key = format!("plasm:outbound:v1:{}", uuid::Uuid::new_v4());

    let auth_config_id = body
        .auth_config_id
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let core = OauthPendingCore {
        code_verifier: pkce_verifier,
        hosted_kv_key: hosted_kv_key.clone(),
        entry_id: entry_id.to_string(),
        return_url: return_url.to_string(),
        token_endpoint: cfg.token_endpoint.clone(),
        client_id: cfg.client_id.clone(),
        client_secret: cfg.client_secret.clone(),
        auth_config_id,
        requested_scopes_sha256: Some(scopes_sha256.clone()),
    };

    let session = OauthLinkSession::begin(csrf, core);
    let pending_json = session.to_pending_bytes().map_err(|_| {
        oauth_start_json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "failed to serialize pending session",
            Some(entry_id),
        )
    })?;

    storage
        .store_kv(
            session.pending_key.as_str(),
            &pending_json,
            Some(PENDING_TTL),
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "oauth link: store pending failed");
            oauth_start_json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                &format!("store pending failed: {e}"),
                Some(entry_id),
            )
        })?;

    tracing::info!(
        target: "plasm_agent::oauth_link",
        entry_id = %entry_id,
        oauth.phase = "start",
        scopes.source = scopes_source,
        scope_count = scope_list.len(),
        scopes_sha256 = %scopes_sha256,
        "oauth link start: pending session stored (oauth2 authorize URL + PKCE from plasm_runtime::begin_authorization_code_pkce)"
    );

    tracing::debug!(
        target: "plasm_agent::oauth_link",
        entry_id = %entry_id,
        scopes = ?scope_list,
        "oauth link start: scope list (debug)"
    );

    if std::env::var("PLASM_OAUTH_LINK_LOG_SCOPES").ok().as_deref() == Some("1") {
        tracing::info!(
            target: "plasm_agent::oauth_link",
            entry_id = %entry_id,
            scopes = ?scope_list,
            "oauth link start: scope list (PLASM_OAUTH_LINK_LOG_SCOPES=1)"
        );
    }

    Ok(Json(json!({ "authorize_url": authorize_url })))
}

async fn runtime_providers_list_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !internal_or_outbound_setup_authorized(&headers, "oauth-link runtime-providers list") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(catalog) = st.oauth_link_catalog() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let ids = catalog.runtime_entry_ids().await;
    Ok(Json(json!({ "entry_ids": ids })))
}

async fn callback_handler(
    Extension(st): Extension<PlasmHostState>,
    axum::extract::Query(raw): axum::extract::Query<CallbackQueryRaw>,
) -> Response {
    let Some(catalog) = st.oauth_link_catalog() else {
        return plain_status(StatusCode::SERVICE_UNAVAILABLE, "OAuth link unavailable");
    };
    let Some(storage) = st.auth_storage() else {
        return plain_status(StatusCode::SERVICE_UNAVAILABLE, "OAuth link unavailable");
    };

    let state_trim = raw.state.as_deref().unwrap_or("").trim();
    if state_trim.is_empty() {
        return plain_status(StatusCode::BAD_REQUEST, "missing state");
    }

    let Some(pending_key) = PendingKvKey::from_state_query_param(state_trim) else {
        return plain_status(StatusCode::BAD_REQUEST, "missing state");
    };

    let pending_raw = match storage.get_kv(pending_key.as_str()).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return plain_status(StatusCode::BAD_REQUEST, "unknown or expired OAuth state");
        }
        Err(e) => {
            tracing::warn!(error = %e, "oauth link: get pending failed");
            return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "storage error");
        }
    };

    let record: OauthPendingRecordV1 = match serde_json::from_slice(&pending_raw) {
        Ok(p) => p,
        Err(_) => {
            return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "invalid pending session");
        }
    };

    let session = OauthLinkSession::from_loaded(pending_key.clone(), record.core);

    let cb = match IdpCallback::parse(&raw) {
        Ok(c) => c,
        Err(IdpCallbackParseError::MissingState) => {
            return plain_status(StatusCode::BAD_REQUEST, "missing state");
        }
        Err(IdpCallbackParseError::MissingCodeOrError) => {
            let _ = storage.delete_kv(pending_key.as_str()).await;
            return redirect_or_plain(
                &session.core.return_url,
                &[("oauth_status", "error"), ("oauth_error", "missing code")],
            );
        }
    };

    match session.on_idp_callback(cb) {
        Err(OauthCallbackStateMismatch) => {
            let _ = storage.delete_kv(pending_key.as_str()).await;
            plain_status(StatusCode::BAD_REQUEST, "OAuth state mismatch")
        }
        Ok(Err(terminal)) => {
            let _ = storage.delete_kv(pending_key.as_str()).await;
            tracing::info!(
                target: "plasm_agent::oauth_link",
                oauth_error = %terminal.oauth_error,
                "oauth link callback: IdP returned error (user will be redirected to SaaS with oauth_status=error)"
            );
            redirect_or_plain(
                &terminal.return_url,
                &[
                    ("oauth_status", "error"),
                    ("oauth_error", terminal.oauth_error.as_str()),
                ],
            )
        }
        Ok(Ok(exchange)) => {
            let redirect_uri = catalog.redirect_uri.clone();
            let http_timeout = Duration::from_secs(30);
            let return_url_on_exchange_err = exchange.core.return_url.clone();
            let pending_key_on_exchange_err = exchange.pending_key.clone();
            match exchange
                .exchange_and_store(redirect_uri, http_timeout)
                .await
            {
                Ok(completed) => {
                    if let Err(e) = storage
                        .store_kv(&completed.hosted_kv_key, &completed.envelope_bytes, None)
                        .await
                    {
                        tracing::warn!(error = %e, "oauth link: store oauth credential failed");
                        let _ = storage.delete_kv(completed.pending_key.as_str()).await;
                        return redirect_or_plain(
                            &completed.return_url,
                            &[
                                ("oauth_status", "error"),
                                ("oauth_error", "failed to store token"),
                            ],
                        );
                    }

                    let _ = storage.delete_kv(completed.pending_key.as_str()).await;

                    tracing::info!(
                        target: "plasm_agent::oauth_link",
                        entry_id = %completed.entry_id,
                        hosted_kv_key = %completed.hosted_kv_key,
                        auth_config_id = ?completed.auth_config_id,
                        requested_scopes_sha256 = ?completed.requested_scopes_sha256,
                        granted_scope = ?completed.granted_scope,
                        "oauth link callback: token exchange succeeded, credential stored in KV (redirect to return_url); OAuth requested (sha256) + granted scope"
                    );

                    redirect_or_plain(
                        &completed.return_url,
                        &[
                            ("oauth_status", "ok"),
                            ("hosted_kv_key", &completed.hosted_kv_key),
                            ("entry_id", &completed.entry_id),
                        ],
                    )
                }
                Err(e) => {
                    let return_url = return_url_on_exchange_err;
                    let _ = storage
                        .delete_kv(pending_key_on_exchange_err.as_str())
                        .await;
                    let msg = oauth_link_callback_error_query_value(&e);
                    tracing::warn!(error = %e, "oauth link: exchange or envelope failed");
                    redirect_oauth_exchange_error(&return_url, &msg)
                }
            }
        }
    }
}

fn plain_status(status: StatusCode, msg: &'static str) -> Response {
    (status, msg).into_response()
}

fn redirect_or_plain(return_url: &str, pairs: &[(&str, &str)]) -> Response {
    match append_query_params(return_url, pairs) {
        Ok(u) => Redirect::temporary(&u).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "invalid return_url").into_response(),
    }
}

fn redirect_oauth_exchange_error(return_url: &str, oauth_error: &str) -> Response {
    let Ok(mut u) = reqwest::Url::parse(return_url) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "invalid return_url").into_response();
    };
    u.query_pairs_mut()
        .append_pair("oauth_status", "error")
        .append_pair("oauth_error", oauth_error);
    Redirect::temporary(u.as_str()).into_response()
}

/// Keep redirect query strings bounded; messages are spec-accurate from [`OauthExchangeError`].
fn oauth_link_callback_error_query_value(e: &OauthExchangeError) -> String {
    const MAX_CHARS: usize = 900;
    let s = e.to_string();
    let mut it = s.chars();
    let prefix: String = it.by_ref().take(MAX_CHARS).collect();
    if it.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

pub fn oauth_link_routes() -> Router {
    Router::new()
        .route("/oauth/link/callback", get(callback_handler))
        .route("/internal/oauth-link/v1/start", post(start_handler))
        .route(
            "/internal/oauth-link/v1/provider-upsert",
            post(provider_upsert_handler),
        )
        .route(
            "/internal/oauth-link/v1/runtime-providers",
            get(runtime_providers_list_handler),
        )
}
