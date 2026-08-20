//! Incoming (inbound) authentication for HTTP and MCP: JWT (`Authorization: Bearer`) and
//! API keys (`X-API-Key`). Outbound CGS/API credentials remain in [`plasm_runtime::auth`].

use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::http_problem_util::problem_response;
use crate::http_problem_util::problem_types;
use tracing::Instrument;

/// `PLASM_INCOMING_AUTH_MODE`: `off` | `optional` | `required`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum IncomingAuthMode {
    #[default]
    Off,
    Optional,
    Required,
}

impl IncomingAuthMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "" => Some(Self::Off),
            "optional" => Some(Self::Optional),
            "required" => Some(Self::Required),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IncomingAuthMethod {
    Jwt,
    ApiKey,
}

/// Verified caller identity for tenant-scoped execute sessions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TenantPrincipal {
    pub tenant_id: String,
    pub subject: String,
    pub method: IncomingAuthMethod,
}

/// Axum request extension: optional [`TenantPrincipal`] after incoming-auth middleware.
#[derive(Clone, Debug)]
pub struct IncomingPrincipal(pub Option<TenantPrincipal>);

#[derive(Debug, Deserialize)]
struct JwtClaims {
    sub: String,
    #[serde(default, alias = "tid")]
    tenant_id: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
}

/// API key entries loaded from `PLASM_AUTH_API_KEYS_FILE` (JSON array).
#[derive(Debug, Deserialize)]
struct ApiKeyFileEntry {
    key: String,
    tenant_id: String,
    #[serde(default = "default_api_key_subject")]
    subject: String,
}

fn default_api_key_subject() -> String {
    "api-key".into()
}

/// Configuration from environment (see plan / README).
#[derive(Clone)]
pub struct IncomingAuthConfig {
    pub mode: IncomingAuthMode,
    pub jwt_secret: Option<String>,
    pub jwt_issuer: Option<String>,
    pub jwt_audience: Option<String>,
    pub api_keys_file: Option<std::path::PathBuf>,
}

impl IncomingAuthConfig {
    /// Read env. Defaults: mode `off`, no JWT, no API key file.
    pub fn from_env() -> Self {
        let mode = std::env::var("PLASM_INCOMING_AUTH_MODE")
            .ok()
            .and_then(|s| IncomingAuthMode::parse(&s))
            .unwrap_or(IncomingAuthMode::Off);

        Self {
            mode,
            jwt_secret: std::env::var("PLASM_AUTH_JWT_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            jwt_issuer: std::env::var("PLASM_AUTH_JWT_ISSUER")
                .ok()
                .filter(|s| !s.is_empty()),
            jwt_audience: std::env::var("PLASM_AUTH_JWT_AUDIENCE")
                .ok()
                .filter(|s| !s.is_empty()),
            api_keys_file: std::env::var("PLASM_AUTH_API_KEYS_FILE")
                .ok()
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from),
        }
    }

    /// Fail fast when `required` is set but no verifier is configured.
    pub fn validate_startup(&self) -> Result<(), String> {
        if self.mode != IncomingAuthMode::Required {
            return Ok(());
        }
        let has_jwt = self.jwt_secret.is_some();
        let has_keys = self.api_keys_file.is_some();
        if !has_jwt && !has_keys {
            return Err(
                "PLASM_INCOMING_AUTH_MODE=required but neither PLASM_AUTH_JWT_SECRET nor PLASM_AUTH_API_KEYS_FILE is set"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn has_jwt(&self) -> bool {
        self.jwt_secret.is_some()
    }

    pub fn has_api_keys(&self) -> bool {
        self.api_keys_file.is_some()
    }
}

/// Loaded verifier state (JWT + optional API key map).
pub struct IncomingAuthVerifier {
    config: IncomingAuthConfig,
    /// Constant-time lookup: store raw keys hashed? We compare using ct_eq on bytes for each entry (dev-sized map).
    api_keys: Vec<(Vec<u8>, TenantPrincipal)>,
}

impl IncomingAuthVerifier {
    pub fn new(config: IncomingAuthConfig) -> Result<Self, String> {
        let api_keys = load_api_keys(config.api_keys_file.as_deref())?;
        Ok(Self { config, api_keys })
    }

    pub fn mode(&self) -> IncomingAuthMode {
        self.config.mode
    }

    #[allow(dead_code)]
    pub fn config(&self) -> &IncomingAuthConfig {
        &self.config
    }

    fn verify_jwt(&self, token: &str) -> Result<TenantPrincipal, String> {
        let secret = self
            .config
            .jwt_secret
            .as_deref()
            .ok_or_else(|| "JWT verification not configured".to_string())?;

        let header = decode_header(token).map_err(|e| e.to_string())?;
        if header.alg != Algorithm::HS256 {
            return Err("only HS256 JWTs are supported for incoming auth".into());
        }

        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        if let Some(ref iss) = self.config.jwt_issuer {
            validation.set_issuer(&[iss.as_str()]);
        }
        if let Some(ref aud) = self.config.jwt_audience {
            validation.set_audience(&[aud.as_str()]);
        }

        let key = DecodingKey::from_secret(secret.as_bytes());
        let data = decode::<JwtClaims>(token, &key, &validation).map_err(|e| e.to_string())?;
        let claims = data.claims;
        let tenant_id = claims
            .tenant_id
            .or(claims.tenant)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "JWT must include tenant_id (or tid) claim".to_string())?;
        if claims.sub.is_empty() {
            return Err("JWT must include non-empty sub claim".into());
        }
        Ok(TenantPrincipal {
            tenant_id,
            subject: claims.sub,
            method: IncomingAuthMethod::Jwt,
        })
    }

    /// Verify a Bearer JWT and return tenant principal.
    pub fn verify_bearer_token(&self, token: &str) -> Result<TenantPrincipal, String> {
        self.verify_jwt(token)
    }

    fn verify_api_key(&self, key: &str) -> Result<TenantPrincipal, String> {
        if self.api_keys.is_empty() {
            return Err("API key verification not configured".into());
        }
        let k = key.as_bytes();
        for (stored, principal) in &self.api_keys {
            if k.len() == stored.len() && k.ct_eq(stored).into() {
                return Ok(principal.clone());
            }
        }
        Err("invalid API key".into())
    }

    /// Parse `Authorization` / `X-API-Key` and return a principal, or error string.
    pub fn verify_headers(&self, headers: &HeaderMap) -> Result<Option<TenantPrincipal>, String> {
        if let Some(h) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
            let p = self.verify_api_key(h.trim())?;
            return Ok(Some(p));
        }
        if let Some(auth) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
            let auth = auth.trim();
            let prefix = "Bearer ";
            if auth.len() > prefix.len() && auth[..prefix.len()].eq_ignore_ascii_case(prefix) {
                let token = auth[prefix.len()..].trim();
                if token.is_empty() {
                    return Err("empty Bearer token".into());
                }
                let p = self.verify_jwt(token)?;
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// Apply mode: returns `Ok(Some(p))`, `Ok(None)` for anonymous where allowed, or `Err` for fatal parse errors.
    pub fn evaluate(
        &self,
        headers: &HeaderMap,
    ) -> Result<Option<TenantPrincipal>, IncomingAuthFailure> {
        match self.verify_headers(headers) {
            Ok(Some(p)) => Ok(Some(p)),
            Ok(None) => match self.config.mode {
                IncomingAuthMode::Required => Err(IncomingAuthFailure::Missing),
                IncomingAuthMode::Off | IncomingAuthMode::Optional => Ok(None),
            },
            Err(e) => Err(IncomingAuthFailure::Invalid(e)),
        }
    }
}

#[derive(Debug)]
pub enum IncomingAuthFailure {
    Missing,
    Invalid(String),
}

/// RFC 7807 response for HTTP (401 / 403).
pub fn incoming_auth_problem(
    failure: IncomingAuthFailure,
    forbidden: bool,
) -> axum::response::Response {
    let (status, typ, title, detail) = if forbidden {
        (
            ProblemStatus::FORBIDDEN,
            problem_types::INCOMING_AUTH_FORBIDDEN,
            "Forbidden",
            "this operation is not allowed for the authenticated principal".to_string(),
        )
    } else {
        match failure {
            IncomingAuthFailure::Missing => (
                ProblemStatus::UNAUTHORIZED,
                problem_types::INCOMING_AUTH_UNAUTHORIZED,
                "Unauthorized",
                "authentication required".to_string(),
            ),
            IncomingAuthFailure::Invalid(ref msg) => (
                ProblemStatus::UNAUTHORIZED,
                problem_types::INCOMING_AUTH_UNAUTHORIZED,
                "Unauthorized",
                msg.clone(),
            ),
        }
    };
    problem_response(
        Problem::custom(status, Uri::from_static(typ))
            .with_title(title)
            .with_detail(detail),
    )
}

/// Map principal to tenant scope string for session keys (`""` when anonymous).
pub fn tenant_scope(principal: Option<&TenantPrincipal>) -> String {
    principal.map(|p| p.tenant_id.clone()).unwrap_or_default()
}

fn load_api_keys(
    path: Option<&std::path::Path>,
) -> Result<Vec<(Vec<u8>, TenantPrincipal)>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read PLASM_AUTH_API_KEYS_FILE {}: {e}", path.display()))?;
    let entries: Vec<ApiKeyFileEntry> =
        serde_json::from_str(&raw).map_err(|e| format!("parse API keys file: {e}"))?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        if e.key.is_empty() || e.tenant_id.is_empty() {
            return Err("API key file: empty key or tenant_id".into());
        }
        out.push((
            e.key.into_bytes(),
            TenantPrincipal {
                tenant_id: e.tenant_id,
                subject: e.subject,
                method: IncomingAuthMethod::ApiKey,
            },
        ));
    }
    Ok(out)
}

/// Startup log line (no secrets).
pub fn log_incoming_auth_startup(config: &IncomingAuthConfig, verifier: &IncomingAuthVerifier) {
    tracing::info!(
        target: "plasm_agent::incoming_auth",
        mode = ?config.mode,
        jwt = config.has_jwt(),
        api_key_file = config.has_api_keys(),
        api_key_entries = verifier.api_keys.len(),
        "incoming auth"
    );
}

/// Returns `true` when the session may be accessed with this principal (anonymous ↔ empty tenant scope).
pub fn session_allows_principal(
    sess: &crate::execute_session::ExecuteSession,
    principal: Option<&TenantPrincipal>,
) -> bool {
    let req_tenant = principal.map(|p| p.tenant_id.as_str()).unwrap_or("");
    sess.tenant_scope == req_tenant
}

// --- HTTP middleware (Axum) ---

use axum::body::Body;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::server_state::PlasmHostState;

/// Layer order: add this **before** [`Extension`] of [`PlasmHostState`] so `Extension` runs first and populates request extensions.
pub async fn incoming_auth_http_middleware(
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let st = req
        .extensions()
        .get::<PlasmHostState>()
        .cloned()
        .ok_or_else(|| {
            tracing::error!(
                "incoming_auth_http_middleware: PlasmHostState missing from extensions"
            );
            incoming_auth_problem(
                IncomingAuthFailure::Invalid("internal server error".into()),
                false,
            )
        })?;

    let Some(ref verifier) = st.incoming_auth else {
        req.extensions_mut().insert(IncomingPrincipal(None));
        return Ok(next.run(req).await);
    };

    if verifier.mode() == IncomingAuthMode::Off {
        req.extensions_mut().insert(IncomingPrincipal(None));
        return Ok(next.run(req).await);
    }

    let principal = match verifier.evaluate(req.headers()) {
        Ok(Some(p)) => IncomingPrincipal(Some(p)),
        Ok(None) => IncomingPrincipal(None),
        Err(f) => return Err(incoming_auth_problem(f, false)),
    };
    let tenant_id = principal
        .0
        .as_ref()
        .map(|p| p.tenant_id.as_str())
        .unwrap_or("");
    let span = crate::spans::security_incoming_http(principal.0.is_some(), tenant_id);
    req.extensions_mut().insert(principal);
    Ok(next.run(req).instrument(span).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn jwt_round_trip_hs256() {
        use jsonwebtoken::{encode, EncodingKey, Header};
        use serde_json::json;

        let secret = "unit-test-secret";
        let config = IncomingAuthConfig {
            mode: IncomingAuthMode::Off,
            jwt_secret: Some(secret.into()),
            jwt_issuer: None,
            jwt_audience: None,
            api_keys_file: None,
        };
        let v = IncomingAuthVerifier::new(config).expect("verifier");

        let claims = json!({
            "sub": "u1",
            "tenant_id": "tenant-a",
            "exp": jsonwebtoken::get_current_timestamp() + 3600,
        });
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encode");

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let p = v.verify_headers(&headers).expect("ok").expect("some");
        assert_eq!(p.tenant_id, "tenant-a");
        assert_eq!(p.subject, "u1");
    }
}
