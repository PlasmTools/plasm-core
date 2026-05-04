//! Streamable HTTP MCP authentication:
//! - `Authorization: Bearer <api_key>` for tenant MCP transport (policy-controlled)
//! - `Authorization: Bearer <oauth_access_token>` for personal MCP inbound OAuth (dynamic registration)
//!
//! When no tenant MCP configurations are loaded, transport requests may omit `Authorization` (open local
//! dev) or send `Authorization: Bearer __plasm_mcp_anonymous__` (see [`PLASM_MCP_ANONYMOUS_BEARER_TOKEN`]:
//! `rust-mcp-sdk` cannot forward an empty bearer secret). Once tenant configs exist, every MCP request must authenticate
//! via API key or OAuth bearer token.

#![allow(clippy::result_large_err)]
// Err variants are full HTTP responses; boxing every OAuth helper would be high churn for little gain.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use http::header::{CONTENT_TYPE, LOCATION};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rust_mcp_sdk::auth::{AuthInfo, AuthProvider, AuthenticationError, OauthEndpoint};
use rust_mcp_sdk::mcp_http::{GenericBody, GenericBodyExt, McpAppState};
use rust_mcp_sdk::mcp_server::error::TransportServerError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use url::form_urlencoded;
use uuid::Uuid;

use crate::server_state::PlasmHostState;

const OAUTH_AUTHORIZE_PATH: &str = "/oauth/authorize";
const OAUTH_TOKEN_PATH: &str = "/oauth/token";
const OAUTH_REGISTER_PATH: &str = "/oauth/register";
const OAUTH_AS_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";
const OAUTH_PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource/mcp";
const MCP_OAUTH_PREFIX: &str = "/mcp";
/// Non-empty bearer value for **anonymous** Streamable HTTP MCP when no tenant configs exist.
///
/// `rust-mcp-sdk` trims the full `Authorization` header, then splits on the first ASCII space, so a
/// header of `Bearer ` (empty secret) becomes `Bearer` and never reaches [`AuthProvider::verify_token`]
/// with an empty string. Scripts and `curl` should send `Authorization: Bearer <this>` instead.
pub const PLASM_MCP_ANONYMOUS_BEARER_TOKEN: &str = "__plasm_mcp_anonymous__";
const OAUTH_SCOPE: &str = "mcp:tools";
const OAUTH_ACCESS_TOKEN_TTL_SECS: u64 = 3600;
const OAUTH_REFRESH_TOKEN_TTL_SECS: u64 = 86400 * 30;
const OAUTH_AUTH_CODE_TTL_SECS: u64 = 600;

type OauthHttpResponse = http::Response<GenericBody>;

#[derive(Debug, Clone, Copy)]
enum McpInboundTransportPolicy {
    ApiKeyOrOAuth,
    OAuthOnly,
    ApiKeyOnly,
}

impl McpInboundTransportPolicy {
    fn from_env() -> Self {
        match std::env::var("PLASM_MCP_TRANSPORT_AUTH_MODE")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("oauth_only") => Self::OAuthOnly,
            Some("api_key_only") => Self::ApiKeyOnly,
            _ => Self::ApiKeyOrOAuth,
        }
    }
}

fn auth_expires_at(seconds_from_now: u64) -> SystemTime {
    SystemTime::now() + Duration::from_secs(seconds_from_now)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn mcp_public_base_url() -> String {
    std::env::var("PLASM_MCP_PUBLIC_BASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:3001".to_string())
}

fn protected_resource_metadata_url(base: &str) -> String {
    format!("{base}{OAUTH_PROTECTED_RESOURCE_PATH}")
}

fn mcp_resource_base_url() -> String {
    let base = mcp_public_base_url();
    if base.ends_with(MCP_OAUTH_PREFIX) {
        base
    } else {
        format!("{base}{MCP_OAUTH_PREFIX}")
    }
}

fn oauth_endpoint_map() -> HashMap<String, OauthEndpoint> {
    let mut m = HashMap::new();
    let endpoints = [
        (
            OAUTH_AS_METADATA_PATH.to_string(),
            OauthEndpoint::AuthorizationServerMetadata,
        ),
        (
            OAUTH_PROTECTED_RESOURCE_PATH.to_string(),
            OauthEndpoint::ProtectedResourceMetadata,
        ),
        (
            OAUTH_AUTHORIZE_PATH.to_string(),
            OauthEndpoint::AuthorizationEndpoint,
        ),
        (OAUTH_TOKEN_PATH.to_string(), OauthEndpoint::TokenEndpoint),
        (
            OAUTH_REGISTER_PATH.to_string(),
            OauthEndpoint::RegistrationEndpoint,
        ),
    ];

    for (path, endpoint) in endpoints {
        m.insert(format!("{MCP_OAUTH_PREFIX}{path}"), endpoint);
    }
    m
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InboundOAuthClient {
    client_id: String,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: String,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    client_name: Option<String>,
    client_uri: Option<String>,
    scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InboundOAuthAuthCode {
    code: String,
    client_id: String,
    redirect_uri: String,
    tenant_id: String,
    subject: String,
    scope: String,
    code_challenge: String,
    code_challenge_method: String,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
struct InboundOAuthRegisterRequest {
    redirect_uris: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
    client_name: Option<String>,
    client_uri: Option<String>,
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
struct InboundOAuthRegisterResponse {
    client_id: String,
    client_secret: String,
    client_id_issued_at: u64,
    client_secret_expires_at: u64,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: String,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    scope: String,
    registration_client_uri: String,
    registration_access_token: String,
}

#[derive(Debug, Serialize)]
struct InboundOAuthTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

impl InboundOAuthTokenResponse {
    fn bearer(access_token: String, scope: String, refresh_token: Option<String>) -> Self {
        Self {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: OAUTH_ACCESS_TOKEN_TTL_SECS,
            scope,
            refresh_token,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InboundOAuthRefreshTokenRecord {
    token_hash: String,
    client_id: String,
    tenant_id: String,
    subject: String,
    scope: String,
    expires_at: u64,
}

#[derive(Debug, Serialize)]
struct InboundOAuthError {
    error: String,
    error_description: String,
}

#[derive(Debug, Serialize)]
struct IncomingOAuthJwtClaims {
    sub: String,
    tenant_id: String,
    exp: u64,
    iat: u64,
    scope: String,
    client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    iss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aud: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InboundOAuthTokenRequest {
    grant_type: Option<String>,
    client_id: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

pub struct PlasmMcpApiKeyAuthProvider {
    plasm: Arc<PlasmHostState>,
    oauth_endpoints: HashMap<String, OauthEndpoint>,
    protected_resource_metadata_url: String,
    transport_policy: McpInboundTransportPolicy,
}

impl PlasmMcpApiKeyAuthProvider {
    pub fn new(plasm: Arc<PlasmHostState>) -> Self {
        let base = mcp_resource_base_url();
        Self {
            plasm,
            oauth_endpoints: oauth_endpoint_map(),
            protected_resource_metadata_url: protected_resource_metadata_url(&base),
            transport_policy: McpInboundTransportPolicy::from_env(),
        }
    }

    fn oauth_client_key(client_id: &str) -> String {
        format!("plasm:incoming_oauth:v1:client:{client_id}")
    }

    fn oauth_code_key(code: &str) -> String {
        format!("plasm:incoming_oauth:v1:code:{code}")
    }

    fn oauth_refresh_key(token_hash: &str) -> String {
        format!("plasm:incoming_oauth:v1:refresh:{token_hash}")
    }

    fn sha256_hex(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    fn oauth_not_configured_error(&self) -> AuthenticationError {
        AuthenticationError::InvalidToken {
            description: "incoming OAuth is unavailable: configure incoming JWT and auth storage",
        }
    }

    fn mcp_repo(
        &self,
    ) -> Result<&crate::mcp_config_repository::McpConfigRepository, AuthenticationError> {
        self.plasm
            .mcp_config_repository()
            .map(|a| a.as_ref())
            .ok_or(AuthenticationError::InvalidToken {
                description: "MCP configuration store unavailable",
            })
    }

    async fn verify_anonymous_ok_async(&self) -> Result<AuthInfo, AuthenticationError> {
        let has_tenants = match self.plasm.mcp_config_repository() {
            None => false,
            Some(repo) => repo.has_tenant_configs().await.unwrap_or(false),
        };
        if has_tenants {
            return Err(AuthenticationError::InvalidToken {
                description: "MCP Authorization required: send `Authorization: Bearer <api_key>` or OAuth access token",
            });
        }
        let mut extra = serde_json::Map::new();
        extra.insert("plasm_mcp_anonymous".to_string(), json!(true));
        Ok(AuthInfo {
            token_unique_id: "plasm_mcp_anonymous".into(),
            client_id: None,
            user_id: None,
            scopes: None,
            expires_at: Some(auth_expires_at(3600)),
            audience: None,
            extra: Some(extra),
        })
    }

    async fn verify_api_key(&self, raw: &str) -> Result<AuthInfo, AuthenticationError> {
        let Some(mcp_auth) = self.plasm.mcp_transport_auth() else {
            return Err(AuthenticationError::InvalidToken {
                description: "MCP transport API key verification unavailable",
            });
        };
        let Some(config_id) = mcp_auth.verify_api_key(raw).await else {
            return Err(AuthenticationError::InvalidToken {
                description: "invalid or unknown MCP API key",
            });
        };
        let repo = self.mcp_repo()?;
        let Some(cfg) = repo.get_runtime_config(&config_id).await.map_err(|_| {
            AuthenticationError::InvalidToken {
                description: "MCP configuration store error",
            }
        })?
        else {
            return Err(AuthenticationError::InvalidToken {
                description: "MCP configuration for this API key is not available",
            });
        };

        let mut extra = serde_json::Map::new();
        extra.insert("plasm_mcp_config_id".to_string(), json!(cfg.id.to_string()));
        extra.insert(
            "plasm_space_type".to_string(),
            json!(cfg.space_type.clone()),
        );
        if let Some(owner_subject) = cfg.owner_subject.as_ref() {
            extra.insert("plasm_owner_subject".to_string(), json!(owner_subject));
        }

        Ok(AuthInfo {
            token_unique_id: format!("{:x}", Sha256::digest(raw.as_bytes())),
            client_id: Some(cfg.tenant_id.clone()),
            user_id: cfg
                .owner_subject
                .clone()
                .or_else(|| Some(cfg.id.to_string())),
            scopes: None,
            // SDK middleware rejects missing or past expiry; API keys do not expire server-side.
            expires_at: Some(auth_expires_at(86400 * 365)),
            audience: None,
            extra: Some(extra),
        })
    }

    async fn verify_oauth_bearer(&self, raw: &str) -> Result<AuthInfo, AuthenticationError> {
        let verifier = self
            .plasm
            .incoming_auth
            .as_deref()
            .ok_or_else(|| self.oauth_not_configured_error())?;

        let principal =
            verifier
                .verify_bearer_token(raw)
                .map_err(|_| AuthenticationError::InvalidToken {
                    description: "invalid OAuth bearer token",
                })?;

        let repo = self.mcp_repo()?;
        let Some(cfg) = repo
            .find_personal_runtime(&principal.tenant_id, &principal.subject)
            .await
            .map_err(|_| AuthenticationError::InvalidToken {
                description: "MCP configuration store error",
            })?
        else {
            return Err(AuthenticationError::InvalidToken {
                description:
                    "OAuth token subject is not bound to an active personal MCP configuration",
            });
        };

        let mut extra = serde_json::Map::new();
        extra.insert("plasm_mcp_config_id".to_string(), json!(cfg.id.to_string()));
        extra.insert("plasm_space_type".to_string(), json!("personal"));
        extra.insert(
            "plasm_owner_subject".to_string(),
            json!(principal.subject.clone()),
        );
        extra.insert("plasm_mcp_oauth".to_string(), json!(true));

        Ok(AuthInfo {
            token_unique_id: format!("{:x}", Sha256::digest(raw.as_bytes())),
            client_id: Some(principal.tenant_id.clone()),
            user_id: Some(principal.subject.clone()),
            scopes: Some(vec![OAUTH_SCOPE.to_string()]),
            expires_at: Some(auth_expires_at(OAUTH_ACCESS_TOKEN_TTL_SECS)),
            audience: None,
            extra: Some(extra),
        })
    }

    fn oauth_authorization_server_metadata_json(&self) -> serde_json::Value {
        let base = mcp_resource_base_url();
        json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}{OAUTH_AUTHORIZE_PATH}"),
            "token_endpoint": format!("{base}{OAUTH_TOKEN_PATH}"),
            "registration_endpoint": format!("{base}{OAUTH_REGISTER_PATH}"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_methods_supported": ["none"],
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": [OAUTH_SCOPE]
        })
    }

    fn oauth_protected_resource_metadata_json(&self) -> serde_json::Value {
        let resource = mcp_resource_base_url();
        json!({
            "resource": resource,
            "authorization_servers": [resource],
            "scopes_supported": [OAUTH_SCOPE],
            "bearer_methods_supported": ["header"]
        })
    }

    fn json_response(status: StatusCode, v: serde_json::Value) -> http::Response<GenericBody> {
        GenericBody::from_value(&v).into_json_response(status, None)
    }

    fn html_response(status: StatusCode, html: String) -> http::Response<GenericBody> {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        GenericBody::from_string(html).into_response(status, Some(headers))
    }

    fn redirect_response(location: &str) -> http::Response<GenericBody> {
        let mut headers = HeaderMap::new();
        if let Ok(hv) = HeaderValue::from_str(location) {
            headers.insert(LOCATION, hv);
        }
        GenericBody::empty().into_response(StatusCode::FOUND, Some(headers))
    }

    fn oauth_error_json(error: &str, description: &str) -> serde_json::Value {
        serde_json::to_value(InboundOAuthError {
            error: error.to_string(),
            error_description: description.to_string(),
        })
        .unwrap_or_else(
            |_| json!({"error":"server_error","error_description":"serialization failure"}),
        )
    }

    fn oauth_error_response(
        status: StatusCode,
        error: &str,
        description: &str,
    ) -> OauthHttpResponse {
        Self::json_response(status, Self::oauth_error_json(error, description))
    }

    fn require_auth_storage(
        &self,
    ) -> Result<&Arc<dyn auth_framework::storage::core::AuthStorage>, OauthHttpResponse> {
        self.plasm.auth_storage().ok_or_else(|| {
            Self::oauth_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "OAuth storage is unavailable on this node",
            )
        })
    }

    fn require_incoming_verifier(
        &self,
        error_description: &'static str,
    ) -> Result<&crate::incoming_auth::IncomingAuthVerifier, OauthHttpResponse> {
        self.plasm.incoming_auth.as_deref().ok_or_else(|| {
            Self::oauth_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                error_description,
            )
        })
    }

    fn parse_oauth_token_request(
        body: &str,
    ) -> Result<InboundOAuthTokenRequest, OauthHttpResponse> {
        if body.starts_with('{') {
            return serde_json::from_str::<InboundOAuthTokenRequest>(body).map_err(|_| {
                Self::oauth_error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "invalid token request body",
                )
            });
        }

        let form = Self::parse_form_body(body);
        Ok(InboundOAuthTokenRequest {
            grant_type: form.get("grant_type").cloned(),
            client_id: form.get("client_id").cloned(),
            code: form.get("code").cloned(),
            redirect_uri: form.get("redirect_uri").cloned(),
            code_verifier: form.get("code_verifier").cloned(),
            refresh_token: form.get("refresh_token").cloned(),
        })
    }

    fn redirect_uri_matches_client(client: &InboundOAuthClient, redirect_uri: &str) -> bool {
        client.redirect_uris.iter().any(|uri| uri == redirect_uri)
    }

    fn parse_form_body(body: &str) -> HashMap<String, String> {
        form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect::<HashMap<_, _>>()
    }

    fn parse_query(req: &http::Request<&str>) -> HashMap<String, String> {
        req.uri()
            .query()
            .map(|q| {
                form_urlencoded::parse(q.as_bytes())
                    .into_owned()
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default()
    }

    async fn load_client(&self, client_id: &str) -> Result<InboundOAuthClient, OauthHttpResponse> {
        let storage = self.require_auth_storage()?;
        let key = Self::oauth_client_key(client_id);
        let row = storage.get_kv(&key).await.map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth client storage read failed",
            )
        })?;
        let Some(bytes) = row else {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_client",
                "unknown client_id",
            ));
        };
        serde_json::from_slice::<InboundOAuthClient>(&bytes).map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth client row decode failed",
            )
        })
    }

    async fn store_client(&self, c: &InboundOAuthClient) -> Result<(), OauthHttpResponse> {
        let storage = self.require_auth_storage()?;
        let key = Self::oauth_client_key(&c.client_id);
        let bytes = serde_json::to_vec(c).map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth client row encode failed",
            )
        })?;
        storage.store_kv(&key, &bytes, None).await.map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth client storage write failed",
            )
        })
    }

    async fn store_auth_code(&self, c: &InboundOAuthAuthCode) -> Result<(), OauthHttpResponse> {
        let storage = self.require_auth_storage()?;
        let key = Self::oauth_code_key(&c.code);
        let bytes = serde_json::to_vec(c).map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth authorization code encode failed",
            )
        })?;
        storage
            .store_kv(
                &key,
                &bytes,
                Some(Duration::from_secs(OAUTH_AUTH_CODE_TTL_SECS)),
            )
            .await
            .map_err(|_| {
                Self::oauth_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "OAuth authorization code storage failed",
                )
            })
    }

    async fn load_and_consume_auth_code(
        &self,
        code: &str,
    ) -> Result<InboundOAuthAuthCode, OauthHttpResponse> {
        let storage = self.require_auth_storage()?;
        let key = Self::oauth_code_key(code);
        let row = storage.get_kv(&key).await.map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth authorization code read failed",
            )
        })?;
        let Some(bytes) = row else {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "authorization code is missing or expired",
            ));
        };
        let payload = serde_json::from_slice::<InboundOAuthAuthCode>(&bytes).map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth authorization code decode failed",
            )
        })?;
        storage.delete_kv(&key).await.map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth authorization code consume failed",
            )
        })?;
        Ok(payload)
    }

    async fn store_refresh_token_record(
        &self,
        record: &InboundOAuthRefreshTokenRecord,
    ) -> Result<(), OauthHttpResponse> {
        let storage = self.require_auth_storage()?;
        let key = Self::oauth_refresh_key(&record.token_hash);
        let ttl = record.expires_at.saturating_sub(now_secs());
        if ttl == 0 {
            return Err(Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "refresh token TTL computation failed",
            ));
        }
        let bytes = serde_json::to_vec(record).map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth refresh token encode failed",
            )
        })?;
        storage
            .store_kv(&key, &bytes, Some(Duration::from_secs(ttl)))
            .await
            .map_err(|_| {
                Self::oauth_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "OAuth refresh token storage failed",
                )
            })
    }

    async fn load_refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<InboundOAuthRefreshTokenRecord, OauthHttpResponse> {
        let storage = self.require_auth_storage()?;
        let token_hash = Self::sha256_hex(refresh_token.as_bytes());
        let key = Self::oauth_refresh_key(&token_hash);
        let row = storage.get_kv(&key).await.map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth refresh token read failed",
            )
        })?;
        let Some(bytes) = row else {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "refresh token is invalid or expired",
            ));
        };
        let payload =
            serde_json::from_slice::<InboundOAuthRefreshTokenRecord>(&bytes).map_err(|_| {
                Self::oauth_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "OAuth refresh token decode failed",
                )
            })?;
        Ok(payload)
    }

    async fn consume_refresh_token(&self, refresh_token: &str) -> Result<(), OauthHttpResponse> {
        let storage = self.require_auth_storage()?;
        let token_hash = Self::sha256_hex(refresh_token.as_bytes());
        let key = Self::oauth_refresh_key(&token_hash);
        storage.delete_kv(&key).await.map_err(|_| {
            Self::oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "OAuth refresh token consume failed",
            )
        })
    }

    fn mint_access_token(
        &self,
        verifier: &crate::incoming_auth::IncomingAuthVerifier,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        scope: &str,
    ) -> Result<String, http::Response<GenericBody>> {
        let jwt_secret = match verifier.config().jwt_secret.clone() {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                return Err(Self::json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    Self::oauth_error_json(
                        "temporarily_unavailable",
                        "incoming auth JWT secret is not configured",
                    ),
                ));
            }
        };
        let iat = now_secs();
        let exp = iat + OAUTH_ACCESS_TOKEN_TTL_SECS;
        let claims = IncomingOAuthJwtClaims {
            sub: subject.to_string(),
            tenant_id: tenant_id.to_string(),
            exp,
            iat,
            scope: scope.to_string(),
            client_id: client_id.to_string(),
            iss: verifier.config().jwt_issuer.clone(),
            aud: verifier.config().jwt_audience.clone(),
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(jwt_secret.as_bytes()),
        )
        .map_err(|_| {
            Self::json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                Self::oauth_error_json("server_error", "could not mint access token"),
            )
        })
    }

    async fn mint_and_store_refresh_token(
        &self,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        scope: &str,
    ) -> Result<String, http::Response<GenericBody>> {
        let refresh_token = format!("plasm_rtok_{}", Uuid::new_v4().simple());
        let record = InboundOAuthRefreshTokenRecord {
            token_hash: Self::sha256_hex(refresh_token.as_bytes()),
            client_id: client_id.to_string(),
            tenant_id: tenant_id.to_string(),
            subject: subject.to_string(),
            scope: scope.to_string(),
            expires_at: now_secs() + OAUTH_REFRESH_TOKEN_TTL_SECS,
        };
        self.store_refresh_token_record(&record).await?;
        Ok(refresh_token)
    }

    fn validate_pkce_s256(code_challenge: &str, code_verifier: &str) -> bool {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let hash = Sha256::digest(code_verifier.as_bytes());
        let encoded = URL_SAFE_NO_PAD.encode(hash);
        encoded == code_challenge
    }

    async fn verify_authorization_principal(
        &self,
        principal_token: &str,
    ) -> Result<crate::incoming_auth::TenantPrincipal, OauthHttpResponse> {
        let verifier = self
            .require_incoming_verifier("incoming auth is not configured for OAuth authorization")?;
        let principal = verifier.verify_bearer_token(principal_token).map_err(|_| {
            Self::oauth_error_response(
                StatusCode::UNAUTHORIZED,
                "access_denied",
                "principal token is invalid",
            )
        })?;
        let Some(repo) = self.plasm.mcp_config_repository() else {
            return Err(Self::oauth_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "MCP policy store unavailable",
            ));
        };
        let ok = repo
            .find_personal_runtime(&principal.tenant_id, &principal.subject)
            .await
            .map_err(|_| {
                Self::oauth_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily_unavailable",
                    "MCP policy store error",
                )
            })?
            .is_some();
        if !ok {
            return Err(Self::oauth_error_response(
                StatusCode::FORBIDDEN,
                "access_denied",
                "principal is not bound to an active personal MCP configuration",
            ));
        }
        Ok(principal)
    }

    fn authorization_prompt_html(params: &HashMap<String, String>, authorize_path: &str) -> String {
        let client_id = params.get("client_id").cloned().unwrap_or_default();
        let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
        let scope = params
            .get("scope")
            .cloned()
            .unwrap_or_else(|| OAUTH_SCOPE.to_string());
        let state = params.get("state").cloned().unwrap_or_default();
        let response_type = params
            .get("response_type")
            .cloned()
            .unwrap_or_else(|| "code".to_string());
        let code_challenge = params.get("code_challenge").cloned().unwrap_or_default();
        let code_challenge_method = params
            .get("code_challenge_method")
            .cloned()
            .unwrap_or_else(|| "S256".to_string());

        format!(
            r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Plasm MCP OAuth Authorization</title></head>
<body style="font-family: system-ui, sans-serif; margin: 2rem; max-width: 56rem;">
  <h1>Authorize personal MCP access</h1>
  <p>Paste your Plasm incoming-auth JWT to complete OAuth authorization for this client.</p>
  <form method="GET" action="{authz}">
    <input type="hidden" name="response_type" value="{response_type}" />
    <input type="hidden" name="client_id" value="{client_id}" />
    <input type="hidden" name="redirect_uri" value="{redirect_uri}" />
    <input type="hidden" name="scope" value="{scope}" />
    <input type="hidden" name="state" value="{state}" />
    <input type="hidden" name="code_challenge" value="{code_challenge}" />
    <input type="hidden" name="code_challenge_method" value="{code_challenge_method}" />
    <label for="principal_token"><strong>Principal token</strong></label><br/>
    <input id="principal_token" name="principal_token" style="width: 100%; margin-top: .5rem;" />
    <div style="margin-top: 1rem;">
      <button type="submit">Authorize</button>
    </div>
  </form>
</body></html>"#,
            authz = authorize_path,
            response_type = response_type,
            client_id = client_id,
            redirect_uri = redirect_uri,
            scope = scope,
            state = state,
            code_challenge = code_challenge,
            code_challenge_method = code_challenge_method,
        )
    }

    fn append_query_to_redirect(
        uri: &str,
        key: &str,
        value: &str,
        state: Option<&str>,
    ) -> Option<String> {
        let mut parsed = reqwest::Url::parse(uri).ok()?;
        {
            let mut q = parsed.query_pairs_mut();
            q.append_pair(key, value);
            if let Some(s) = state {
                if !s.is_empty() {
                    q.append_pair("state", s);
                }
            }
        }
        Some(parsed.to_string())
    }

    async fn oauth_register(
        &self,
        request: http::Request<&str>,
    ) -> Result<http::Response<GenericBody>, TransportServerError> {
        if request.method() != Method::POST {
            return Ok(GenericBody::create_405_response(
                request.method(),
                &[Method::POST, Method::OPTIONS],
            ));
        }
        let payload: InboundOAuthRegisterRequest = match serde_json::from_str(request.body().trim())
        {
            Ok(v) => v,
            Err(_) => {
                return Ok(Self::json_response(
                    StatusCode::BAD_REQUEST,
                    Self::oauth_error_json("invalid_request", "invalid registration JSON"),
                ));
            }
        };

        let redirect_uris = payload.redirect_uris.unwrap_or_default();
        if redirect_uris.is_empty()
            || redirect_uris
                .iter()
                .any(|u| reqwest::Url::parse(u).is_err())
        {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json(
                    "invalid_redirect_uri",
                    "redirect_uris must be non-empty valid URLs",
                ),
            ));
        }
        let token_endpoint_auth_method = payload
            .token_endpoint_auth_method
            .unwrap_or_else(|| "none".to_string())
            .trim()
            .to_ascii_lowercase();
        if token_endpoint_auth_method != "none" {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json(
                    "invalid_client_metadata",
                    "dynamic registration supports public clients only (token_endpoint_auth_method=none)",
                ),
            ));
        }

        let grant_types = payload
            .grant_types
            .unwrap_or_else(|| vec!["authorization_code".to_string()]);
        let normalized_grant_types: Vec<String> = grant_types
            .iter()
            .map(|g| g.trim().to_ascii_lowercase())
            .filter(|g| !g.is_empty())
            .collect();
        let supports_auth_code = normalized_grant_types
            .iter()
            .any(|g| g == "authorization_code");
        let grants_valid = normalized_grant_types
            .iter()
            .all(|g| g == "authorization_code" || g == "refresh_token");
        if normalized_grant_types.is_empty() || !supports_auth_code || !grants_valid {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json(
                    "invalid_client_metadata",
                    "grant_types must include authorization_code (optional refresh_token is allowed)",
                ),
            ));
        }
        let response_types = payload
            .response_types
            .unwrap_or_else(|| vec!["code".to_string()]);
        let normalized_response_types: Vec<String> = response_types
            .iter()
            .map(|r| r.trim().to_ascii_lowercase())
            .filter(|r| !r.is_empty())
            .collect();
        let supports_code = normalized_response_types.iter().any(|r| r == "code");
        let response_types_valid = normalized_response_types.iter().all(|r| r == "code");
        if normalized_response_types.is_empty() || !supports_code || !response_types_valid {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json(
                    "invalid_client_metadata",
                    "response_types must include code",
                ),
            ));
        }

        let client_id = format!("plasm_dcr_{}", Uuid::new_v4().simple());
        let client = InboundOAuthClient {
            client_id: client_id.clone(),
            redirect_uris: redirect_uris.clone(),
            token_endpoint_auth_method: "none".to_string(),
            grant_types: normalized_grant_types.clone(),
            response_types: normalized_response_types.clone(),
            client_name: payload.client_name,
            client_uri: payload.client_uri,
            scope: payload.scope.clone(),
        };
        if let Err(resp) = self.store_client(&client).await {
            return Ok(resp);
        }

        let now = now_secs();
        let reg_access = format!("plasm_reg_{}", Uuid::new_v4().simple());
        let response = InboundOAuthRegisterResponse {
            client_id,
            // Public clients use token_endpoint_auth_method=none; emit empty string for
            // compatibility with clients that require this field to be a JSON string.
            client_secret: String::new(),
            client_id_issued_at: now,
            client_secret_expires_at: 0,
            redirect_uris,
            token_endpoint_auth_method: "none".to_string(),
            grant_types: normalized_grant_types,
            response_types: normalized_response_types,
            scope: payload.scope.unwrap_or_else(|| OAUTH_SCOPE.to_string()),
            registration_client_uri: format!("{}{}", mcp_resource_base_url(), OAUTH_REGISTER_PATH),
            registration_access_token: reg_access,
        };
        let value = serde_json::to_value(response).unwrap_or_else(|_| {
            Self::oauth_error_json("server_error", "registration response serialization failed")
        });
        Ok(Self::json_response(StatusCode::CREATED, value))
    }

    async fn oauth_authorize(
        &self,
        request: http::Request<&str>,
    ) -> Result<http::Response<GenericBody>, TransportServerError> {
        let params = Self::parse_query(&request);
        let response_type = params
            .get("response_type")
            .map(String::as_str)
            .unwrap_or("");
        let client_id = params.get("client_id").map(String::as_str).unwrap_or("");
        let redirect_uri = params.get("redirect_uri").map(String::as_str).unwrap_or("");
        let state = params.get("state").map(String::as_str);
        let scope = params
            .get("scope")
            .cloned()
            .unwrap_or_else(|| OAUTH_SCOPE.to_string());
        let code_challenge = params
            .get("code_challenge")
            .map(String::as_str)
            .unwrap_or("");
        let code_challenge_method = params
            .get("code_challenge_method")
            .map(String::as_str)
            .unwrap_or("S256");

        if response_type != "code" || client_id.is_empty() || redirect_uri.is_empty() {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json(
                    "invalid_request",
                    "response_type=code, client_id, and redirect_uri are required",
                ),
            ));
        }
        if code_challenge.is_empty() || code_challenge_method != "S256" {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json(
                    "invalid_request",
                    "PKCE S256 is required (code_challenge + code_challenge_method=S256)",
                ),
            ));
        }

        let client = match self.load_client(client_id).await {
            Ok(c) => c,
            Err(resp) => return Ok(resp),
        };
        if !Self::redirect_uri_matches_client(&client, redirect_uri) {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json(
                    "invalid_request",
                    "redirect_uri is not registered for this client",
                ),
            ));
        }

        let principal_token = params
            .get("principal_token")
            .map(String::as_str)
            .unwrap_or("")
            .trim();
        if principal_token.is_empty() {
            let authorize_path = request.uri().path();
            return Ok(Self::html_response(
                StatusCode::OK,
                Self::authorization_prompt_html(&params, authorize_path),
            ));
        }

        let principal = match self.verify_authorization_principal(principal_token).await {
            Ok(p) => p,
            Err(resp) => return Ok(resp),
        };

        let code = Uuid::new_v4().simple().to_string();
        let row = InboundOAuthAuthCode {
            code: code.clone(),
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            tenant_id: principal.tenant_id,
            subject: principal.subject,
            scope,
            code_challenge: code_challenge.to_string(),
            code_challenge_method: code_challenge_method.to_string(),
            expires_at: now_secs() + OAUTH_AUTH_CODE_TTL_SECS,
        };
        if let Err(resp) = self.store_auth_code(&row).await {
            return Ok(resp);
        }
        let Some(location) = Self::append_query_to_redirect(redirect_uri, "code", &code, state)
        else {
            return Ok(Self::json_response(
                StatusCode::BAD_REQUEST,
                Self::oauth_error_json("invalid_request", "redirect_uri is invalid"),
            ));
        };
        Ok(Self::redirect_response(&location))
    }

    async fn oauth_token_authorization_code(
        &self,
        form: &InboundOAuthTokenRequest,
        verifier: &crate::incoming_auth::IncomingAuthVerifier,
    ) -> Result<InboundOAuthTokenResponse, OauthHttpResponse> {
        let client_id = form.client_id.as_deref().unwrap_or("").trim();
        let code = form.code.as_deref().unwrap_or("").trim();
        let redirect_uri = form.redirect_uri.as_deref().unwrap_or("").trim();
        let code_verifier = form.code_verifier.as_deref().unwrap_or("").trim();
        if client_id.is_empty()
            || code.is_empty()
            || redirect_uri.is_empty()
            || code_verifier.is_empty()
        {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "client_id, code, redirect_uri, and code_verifier are required",
            ));
        }

        let client = self.load_client(client_id).await?;
        if !Self::redirect_uri_matches_client(&client, redirect_uri) {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "redirect_uri mismatch",
            ));
        }

        let auth_code = self.load_and_consume_auth_code(code).await?;
        if auth_code.client_id != client_id || auth_code.redirect_uri != redirect_uri {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "authorization code mismatch",
            ));
        }
        if auth_code.expires_at <= now_secs() {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "authorization code expired",
            ));
        }
        if auth_code.code_challenge_method != "S256"
            || !Self::validate_pkce_s256(&auth_code.code_challenge, code_verifier)
        {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "PKCE verifier is invalid",
            ));
        }

        let access_token = self.mint_access_token(
            verifier,
            client_id,
            &auth_code.tenant_id,
            &auth_code.subject,
            &auth_code.scope,
        )?;

        let refresh_token = if client.grant_types.iter().any(|g| g == "refresh_token") {
            Some(
                self.mint_and_store_refresh_token(
                    client_id,
                    &auth_code.tenant_id,
                    &auth_code.subject,
                    &auth_code.scope,
                )
                .await?,
            )
        } else {
            None
        };

        Ok(InboundOAuthTokenResponse::bearer(
            access_token,
            auth_code.scope,
            refresh_token,
        ))
    }

    async fn oauth_token_refresh_grant(
        &self,
        form: &InboundOAuthTokenRequest,
        verifier: &crate::incoming_auth::IncomingAuthVerifier,
    ) -> Result<InboundOAuthTokenResponse, OauthHttpResponse> {
        let client_id = form.client_id.as_deref().unwrap_or("").trim();
        let refresh_token = form.refresh_token.as_deref().unwrap_or("").trim();
        if client_id.is_empty() || refresh_token.is_empty() {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "client_id and refresh_token are required",
            ));
        }

        let client = self.load_client(client_id).await?;
        if !client.grant_types.iter().any(|g| g == "refresh_token") {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "unauthorized_client",
                "client is not allowed to use refresh_token grant",
            ));
        }

        let refresh_record = self.load_refresh_token(refresh_token).await?;
        if refresh_record.expires_at <= now_secs() {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "refresh token is expired",
            ));
        }
        if refresh_record.client_id != client_id {
            return Err(Self::oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "refresh token client mismatch",
            ));
        }

        let access_token = self.mint_access_token(
            verifier,
            client_id,
            &refresh_record.tenant_id,
            &refresh_record.subject,
            &refresh_record.scope,
        )?;

        let rotated_refresh_token = self
            .mint_and_store_refresh_token(
                client_id,
                &refresh_record.tenant_id,
                &refresh_record.subject,
                &refresh_record.scope,
            )
            .await?;

        // Refresh rotation is failure-safe: old token is only consumed after successor is durably stored.
        self.consume_refresh_token(refresh_token).await?;

        Ok(InboundOAuthTokenResponse::bearer(
            access_token,
            refresh_record.scope,
            Some(rotated_refresh_token),
        ))
    }

    async fn oauth_token(
        &self,
        request: http::Request<&str>,
    ) -> Result<http::Response<GenericBody>, TransportServerError> {
        if request.method() != Method::POST {
            return Ok(GenericBody::create_405_response(
                request.method(),
                &[Method::POST, Method::OPTIONS],
            ));
        }

        let form = match Self::parse_oauth_token_request(request.body().trim()) {
            Ok(form) => form,
            Err(resp) => return Ok(resp),
        };
        let grant_type = form
            .grant_type
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let verifier = match self.require_incoming_verifier("incoming auth is unavailable") {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

        let response = match grant_type.as_str() {
            "authorization_code" => {
                match self.oauth_token_authorization_code(&form, verifier).await {
                    Ok(resp) => resp,
                    Err(resp) => return Ok(resp),
                }
            }
            "refresh_token" => match self.oauth_token_refresh_grant(&form, verifier).await {
                Ok(resp) => resp,
                Err(resp) => return Ok(resp),
            },
            _ => {
                return Ok(Self::oauth_error_response(
                    StatusCode::BAD_REQUEST,
                    "unsupported_grant_type",
                    "supported grant types are authorization_code and refresh_token",
                ));
            }
        };

        let payload = serde_json::to_value(response).unwrap_or_else(|_| {
            Self::oauth_error_json("server_error", "token serialization failed")
        });
        Ok(Self::json_response(StatusCode::OK, payload))
    }
}

#[async_trait]
impl AuthProvider for PlasmMcpApiKeyAuthProvider {
    async fn verify_token(&self, access_token: String) -> Result<AuthInfo, AuthenticationError> {
        let trimmed = access_token.trim();
        if trimmed.is_empty() || trimmed == PLASM_MCP_ANONYMOUS_BEARER_TOKEN {
            let r = self.verify_anonymous_ok_async().await;
            crate::metrics::record_mcp_transport_auth(
                if r.is_ok() {
                    "success"
                } else {
                    "invalid_token"
                },
                "anonymous",
            );
            return r;
        }
        match self.transport_policy {
            McpInboundTransportPolicy::OAuthOnly => {
                let r = self.verify_oauth_bearer(trimmed).await;
                crate::metrics::record_mcp_transport_auth(
                    if r.is_ok() {
                        "success"
                    } else {
                        "invalid_token"
                    },
                    "oauth",
                );
                r
            }
            McpInboundTransportPolicy::ApiKeyOnly => {
                let r = self.verify_api_key(trimmed).await;
                crate::metrics::record_mcp_transport_auth(
                    if r.is_ok() {
                        "success"
                    } else {
                        "invalid_token"
                    },
                    "api_key",
                );
                r
            }
            McpInboundTransportPolicy::ApiKeyOrOAuth => match self.verify_api_key(trimmed).await {
                Ok(info) => {
                    crate::metrics::record_mcp_transport_auth("success", "api_key");
                    Ok(info)
                }
                Err(_) => {
                    let r = self.verify_oauth_bearer(trimmed).await;
                    crate::metrics::record_mcp_transport_auth(
                        if r.is_ok() {
                            "success"
                        } else {
                            "invalid_token"
                        },
                        "oauth",
                    );
                    r
                }
            },
        }
    }

    fn auth_endpoints(&self) -> Option<&HashMap<String, OauthEndpoint>> {
        Some(&self.oauth_endpoints)
    }

    async fn handle_request(
        &self,
        request: http::Request<&str>,
        _state: Arc<McpAppState>,
    ) -> Result<http::Response<GenericBody>, TransportServerError> {
        let Some(endpoint) = self.endpoint_type(&request) else {
            return Ok(GenericBody::create_404_response());
        };
        if let Some(response) = self.validate_allowed_methods(endpoint, request.method()) {
            return Ok(response);
        }
        match endpoint {
            OauthEndpoint::AuthorizationServerMetadata => Ok(Self::json_response(
                StatusCode::OK,
                self.oauth_authorization_server_metadata_json(),
            )),
            OauthEndpoint::ProtectedResourceMetadata => Ok(Self::json_response(
                StatusCode::OK,
                self.oauth_protected_resource_metadata_json(),
            )),
            OauthEndpoint::RegistrationEndpoint => self.oauth_register(request).await,
            OauthEndpoint::AuthorizationEndpoint => self.oauth_authorize(request).await,
            OauthEndpoint::TokenEndpoint => self.oauth_token(request).await,
            _ => Ok(GenericBody::create_404_response()),
        }
    }

    fn protected_resource_metadata_url(&self) -> Option<&str> {
        Some(self.protected_resource_metadata_url.as_str())
    }
}

pub(crate) fn config_id_from_auth_info(info: &AuthInfo) -> Option<Uuid> {
    let extra = info.extra.as_ref()?;
    let v = extra.get("plasm_mcp_config_id")?;
    let s = v.as_str()?;
    Uuid::parse_str(s).ok()
}

pub(crate) fn is_anonymous_mcp_auth(info: &AuthInfo) -> bool {
    info.extra
        .as_ref()
        .and_then(|m| m.get("plasm_mcp_anonymous"))
        .and_then(|v| v.as_bool())
        == Some(true)
}
