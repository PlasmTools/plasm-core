//! RFC 8628-style device authorization for **incoming** (platform) auth — used by the `plasm` CLI.
//!
//! - `POST /v1/incoming-auth/device/start` — public; returns `device_code`, `user_code`, `verification_uri`.
//! - `POST /v1/incoming-auth/device/poll` — public; CLI polls with `device_code`.
//! - `POST /internal/incoming-auth/v1/device/complete` — control-plane; Phoenix after GitHub sign-in.
//!
//! Sessions are stored in auth-framework KV (`AuthStorage`) so multi-replica `plasm-mcp` works.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use auth_framework::storage::core::AuthStorage;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::control_plane_http::control_plane_headers_authorized;
use crate::incoming_auth::IncomingAuthVerifier;
use crate::server_state::PlasmHostState;

const DEFAULT_DEVICE_TTL_SECS: u64 = 900;
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// Marker type — device sessions live in [`AuthStorage`], not in-process maps.
#[derive(Clone, Copy, Debug, Default)]
pub struct IncomingAuthDeviceStore;

impl IncomingAuthDeviceStore {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDeviceSession {
    user_code: String,
    expires_at_unix: u64,
    poll_interval_secs: u64,
    status: StoredDeviceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredDeviceStatus {
    Pending,
    Approved { access_token: String },
}

fn device_code_key(device_code: &str) -> String {
    format!("plasm:incoming_auth_device:v1:code:{device_code}")
}

fn user_code_index_key(user_code: &str) -> String {
    format!("plasm:incoming_auth_device:v1:user:{user_code}")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn public_web_origin() -> String {
    std::env::var("PLASM_PUBLIC_WEB_ORIGIN")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://platform.plasm.tools".to_string())
}

fn mint_user_code() -> String {
    let raw = uuid::Uuid::new_v4().to_string().replace('-', "");
    let s = raw[..8].to_uppercase();
    format!("{}-{}", &s[..4], &s[4..8])
}

fn mint_device_code() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Canonical `XXXX-XXXX` user code for KV index and session lookup.
pub fn normalize_user_code(raw: &str) -> Option<String> {
    let alnum: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    match alnum.len() {
        8 => Some(format!("{}-{}", &alnum[..4], &alnum[4..8])),
        n if n > 8 => Some(format!("{}-{}", &alnum[..4], &alnum[4..8])),
        _ => None,
    }
}

fn jwt_ttl_secs() -> u64 {
    std::env::var("PLASM_INCOMING_AUTH_JWT_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(30 * 86_400)
}

fn storage_unavailable() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "device_auth_storage_unavailable",
            "message": "device login requires auth-framework KV (PLASM_AUTH_STORAGE_URL)",
        })),
    )
}

fn require_auth_storage(
    st: &PlasmHostState,
) -> Result<&std::sync::Arc<dyn AuthStorage>, (StatusCode, Json<serde_json::Value>)> {
    st.auth_storage().ok_or_else(storage_unavailable)
}

async fn store_session(
    storage: &dyn AuthStorage,
    device_code: &str,
    user_code: &str,
    sess: &StoredDeviceSession,
    ttl_secs: u64,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let bytes = serde_json::to_vec(sess).map_err(|_| storage_kv_error("encode"))?;
    storage
        .store_kv(
            &device_code_key(device_code),
            &bytes,
            Some(Duration::from_secs(ttl_secs)),
        )
        .await
        .map_err(|_| storage_kv_error("write"))?;
    storage
        .store_kv(
            &user_code_index_key(user_code),
            device_code.as_bytes(),
            Some(Duration::from_secs(ttl_secs)),
        )
        .await
        .map_err(|_| storage_kv_error("write index"))?;
    Ok(())
}

async fn load_session(
    storage: &dyn AuthStorage,
    device_code: &str,
) -> Result<Option<StoredDeviceSession>, (StatusCode, Json<serde_json::Value>)> {
    let row = storage
        .get_kv(&device_code_key(device_code))
        .await
        .map_err(|_| storage_kv_error("read"))?;
    let Some(bytes) = row else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| storage_kv_error("decode"))
}

async fn save_session(
    storage: &dyn AuthStorage,
    device_code: &str,
    user_code: &str,
    sess: &StoredDeviceSession,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let ttl = sess.expires_at_unix.saturating_sub(now_unix()).max(1);
    store_session(storage, device_code, user_code, sess, ttl).await
}

fn storage_kv_error(op: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": "server_error",
            "message": format!("device session storage {op} failed"),
        })),
    )
}

fn session_expired(sess: &StoredDeviceSession) -> bool {
    sess.expires_at_unix <= now_unix()
}

/// Mint an HS256 incoming JWT compatible with [`IncomingAuthVerifier`].
#[derive(Debug, thiserror::Error)]
pub enum IncomingTokenMintError {
    #[error("JWT minting not configured (PLASM_AUTH_JWT_SECRET)")]
    NotConfigured,
    #[error("incoming token encoding failed: {0}")]
    Encode(#[source] jsonwebtoken::errors::Error),
}

pub fn mint_incoming_access_token(
    verifier: &IncomingAuthVerifier,
    sub: &str,
    tenant_id: &str,
) -> Result<String, IncomingTokenMintError> {
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;

    let config = verifier.config();
    let secret = config
        .jwt_secret
        .as_deref()
        .ok_or(IncomingTokenMintError::NotConfigured)?;

    let exp = now_unix() + jwt_ttl_secs();

    let mut claims = json!({
        "sub": sub,
        "tenant_id": tenant_id,
        "exp": exp,
    });
    if let Some(ref iss) = config.jwt_issuer {
        claims["iss"] = json!(iss);
    }
    if let Some(ref aud) = config.jwt_audience {
        claims["aud"] = json!(aud);
    }

    encode(
        &Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(IncomingTokenMintError::Encode)
}

#[derive(Debug, Deserialize)]
struct DeviceStartBody {
    #[serde(default)]
    _client_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeviceStartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

async fn device_start_handler(
    Extension(st): Extension<PlasmHostState>,
    Json(_body): Json<DeviceStartBody>,
) -> Result<Json<DeviceStartResponse>, (StatusCode, Json<serde_json::Value>)> {
    let Some(verifier) = st.incoming_auth.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "incoming_auth_disabled",
                "message": "incoming auth is not configured on this server",
            })),
        ));
    };
    if !verifier.config().has_jwt() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "jwt_not_configured",
                "message": "PLASM_AUTH_JWT_SECRET is required for CLI device login",
            })),
        ));
    }

    let storage = require_auth_storage(&st)?;

    let device_code = mint_device_code();
    let user_code = mint_user_code();
    let ttl = DEFAULT_DEVICE_TTL_SECS;
    let interval = DEFAULT_POLL_INTERVAL_SECS;
    let web = public_web_origin();
    let verification_uri = format!("{web}/device?user_code={user_code}");
    let verification_uri_complete = verification_uri.clone();

    let sess = StoredDeviceSession {
        user_code: user_code.clone(),
        expires_at_unix: now_unix() + ttl,
        poll_interval_secs: interval,
        status: StoredDeviceStatus::Pending,
    };
    store_session(storage.as_ref(), &device_code, &user_code, &sess, ttl).await?;

    Ok(Json(DeviceStartResponse {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in: ttl,
        interval,
    }))
}

#[derive(Debug, Deserialize)]
struct DevicePollBody {
    device_code: String,
}

#[derive(Debug, Serialize)]
struct DevicePollSuccess {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
}

#[derive(Debug, Serialize)]
struct DevicePollPending {
    error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval: Option<u64>,
}

async fn device_poll_handler(
    Extension(st): Extension<PlasmHostState>,
    Json(body): Json<DevicePollBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let device_code = body.device_code.trim();
    if device_code.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "error": "invalid_request", "message": "device_code is required" }),
            ),
        ));
    }

    let storage = require_auth_storage(&st)?;
    let Some(sess) = load_session(storage.as_ref(), device_code).await? else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "error": "expired_token", "message": "unknown or expired device_code" }),
            ),
        ));
    };

    if session_expired(&sess) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "expired_token" })),
        ));
    }

    match &sess.status {
        StoredDeviceStatus::Pending => Ok(Json(
            serde_json::to_value(DevicePollPending {
                error: "authorization_pending",
                interval: Some(sess.poll_interval_secs),
            })
            .expect("serialize"),
        )),
        StoredDeviceStatus::Approved { access_token } => {
            let resp = DevicePollSuccess {
                access_token: access_token.clone(),
                token_type: "Bearer",
                expires_in: jwt_ttl_secs(),
            };
            Ok(Json(serde_json::to_value(resp).expect("serialize")))
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCompleteBody {
    user_code: String,
    subject: String,
    #[serde(default)]
    github_login: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeviceCompleteResponse {
    ok: bool,
}

async fn device_complete_handler(
    Extension(st): Extension<PlasmHostState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<DeviceCompleteBody>,
) -> Result<Json<DeviceCompleteResponse>, StatusCode> {
    if !control_plane_headers_authorized(&headers, "incoming-auth device complete") {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_code = match normalize_user_code(&body.user_code) {
        Some(c) => c,
        None => return Err(StatusCode::BAD_REQUEST),
    };
    let subject = body.subject.trim();
    if subject.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let Some(verifier) = st.incoming_auth.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(tenant_store) = st.tenant_binding() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };

    let storage = match st.auth_storage() {
        Some(s) => s,
        None => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };

    let index_row = storage
        .get_kv(&user_code_index_key(&user_code))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(index_bytes) = index_row else {
        return Err(StatusCode::NOT_FOUND);
    };
    let device_code =
        String::from_utf8(index_bytes).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(mut sess) = load_session(storage.as_ref(), &device_code)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    if session_expired(&sess) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let login = body
        .github_login
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let row = tenant_store
        .resolve_or_insert(subject, login)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let token = mint_incoming_access_token(verifier, subject, &row.tenant_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    sess.status = StoredDeviceStatus::Approved {
        access_token: token,
    };
    save_session(storage.as_ref(), &device_code, &user_code, &sess)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(DeviceCompleteResponse { ok: true }))
}

/// Public device authorization routes (no incoming-auth middleware).
pub fn incoming_auth_device_public_routes() -> Router {
    Router::new()
        .route("/v1/incoming-auth/device/start", post(device_start_handler))
        .route("/v1/incoming-auth/device/poll", post(device_poll_handler))
}

/// Control-plane completion after browser GitHub sign-in.
pub fn incoming_auth_device_internal_routes() -> Router {
    Router::new().route(
        "/internal/incoming-auth/v1/device/complete",
        post(device_complete_handler),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incoming_auth::{IncomingAuthConfig, IncomingAuthMode, IncomingAuthVerifier};

    #[test]
    fn normalize_user_code_accepts_paste_variants() {
        assert_eq!(
            normalize_user_code("abcd-ef12").as_deref(),
            Some("ABCD-EF12")
        );
        assert_eq!(
            normalize_user_code("abcdef12").as_deref(),
            Some("ABCD-EF12")
        );
        assert_eq!(
            normalize_user_code("ABCD EFGH").as_deref(),
            Some("ABCD-EFGH")
        );
        assert_eq!(
            normalize_user_code("  abcd-efgh  ").as_deref(),
            Some("ABCD-EFGH")
        );
        assert!(normalize_user_code("short").is_none());
    }

    #[test]
    fn mint_incoming_access_token_round_trip() {
        let config = IncomingAuthConfig {
            mode: IncomingAuthMode::Off,
            jwt_secret: Some("unit-test-secret-long-enough".into()),
            jwt_issuer: None,
            jwt_audience: None,
            api_keys_file: None,
        };
        let v = IncomingAuthVerifier::new(config).unwrap();
        let tok = mint_incoming_access_token(&v, "github:1", "tenant-a").unwrap();
        let p = v.verify_bearer_token(&tok).unwrap();
        assert_eq!(p.tenant_id, "tenant-a");
        assert_eq!(p.subject, "github:1");
    }
}
