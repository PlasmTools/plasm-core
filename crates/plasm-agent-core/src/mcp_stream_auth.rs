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
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use http::header::{CONTENT_TYPE, LOCATION};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use rust_mcp_sdk::auth::{AuthInfo, AuthProvider, AuthenticationError, OauthEndpoint};
use rust_mcp_sdk::mcp_http::{GenericBody, GenericBodyExt, McpAppState};
use rust_mcp_sdk::mcp_server::error::TransportServerError;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;
use url::form_urlencoded;
use uuid::Uuid;

use crate::mcp_inbound_oauth::{
    mcp_resource_base_url, AuthorizeOutcome, McpInboundOAuthService, McpOAuthError,
    McpOAuthTokenRequest, OAUTH_SCOPE,
};
use crate::server_state::PlasmHostState;

const OAUTH_AUTHORIZE_PATH: &str = "/oauth/authorize";
const OAUTH_TOKEN_PATH: &str = "/oauth/token";
const OAUTH_REGISTER_PATH: &str = "/oauth/register";
const OAUTH_AS_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";
const OAUTH_OPENID_CONFIGURATION_PATH: &str = "/.well-known/openid-configuration";
const OAUTH_PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource/mcp";
const MCP_OAUTH_PREFIX: &str = "/mcp";
/// Non-empty bearer value for **anonymous** Streamable HTTP MCP when no tenant configs exist.
///
/// `rust-mcp-sdk` trims the full `Authorization` header, then splits on the first ASCII space, so a
/// header of `Bearer ` (empty secret) becomes `Bearer` and never reaches [`AuthProvider::verify_token`]
/// with an empty string. Scripts and `curl` should send `Authorization: Bearer <this>` instead.
pub const PLASM_MCP_ANONYMOUS_BEARER_TOKEN: &str = "__plasm_mcp_anonymous__";
const OAUTH_ACCESS_TOKEN_TTL_SECS: u64 = 3600;

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

fn protected_resource_metadata_url(base: &str) -> String {
    format!("{base}{OAUTH_PROTECTED_RESOURCE_PATH}")
}

fn oauth_endpoint_map() -> HashMap<String, OauthEndpoint> {
    let mut m = HashMap::new();
    let endpoints = [
        (
            OAUTH_AS_METADATA_PATH.to_string(),
            OauthEndpoint::AuthorizationServerMetadata,
        ),
        (
            OAUTH_OPENID_CONFIGURATION_PATH.to_string(),
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

#[derive(Debug, Serialize)]
struct InboundOAuthError {
    error: String,
    error_description: String,
}

pub struct PlasmMcpApiKeyAuthProvider {
    plasm: Arc<PlasmHostState>,
    oauth: OnceCell<McpInboundOAuthService>,
    oauth_endpoints: HashMap<String, OauthEndpoint>,
    protected_resource_metadata_url: String,
    transport_policy: McpInboundTransportPolicy,
}

impl PlasmMcpApiKeyAuthProvider {
    pub fn new(plasm: Arc<PlasmHostState>) -> Self {
        let base = mcp_resource_base_url();
        Self {
            plasm,
            oauth: OnceCell::new(),
            oauth_endpoints: oauth_endpoint_map(),
            protected_resource_metadata_url: protected_resource_metadata_url(&base),
            transport_policy: McpInboundTransportPolicy::from_env(),
        }
    }

    async fn oauth_service(&self) -> Result<&McpInboundOAuthService, OauthHttpResponse> {
        self.oauth
            .get_or_try_init(|| async {
                McpInboundOAuthService::try_from_host(self.plasm.as_ref())
                    .await
                    .ok_or_else(|| {
                        Self::oauth_error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "temporarily_unavailable",
                            "incoming OAuth is unavailable: configure incoming JWT and auth storage",
                        )
                    })
            })
            .await
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
            expires_at: Some(auth_expires_at(86400 * 365)),
            audience: None,
            extra: Some(extra),
        })
    }

    async fn verify_oauth_bearer(&self, raw: &str) -> Result<AuthInfo, AuthenticationError> {
        let oauth = self.oauth_service().await.map_err(|_| {
            tracing::warn!("mcp oauth bearer rejected: inbound oauth service unavailable");
            self.oauth_not_configured_error()
        })?;
        let principal = oauth.verify_access_token(raw).map_err(|_| {
            tracing::warn!("mcp oauth bearer rejected: invalid or expired access token");
            AuthenticationError::InvalidToken {
                description: "invalid OAuth bearer token",
            }
        })?;

        let repo = self.mcp_repo()?;
        let Some(cfg) = repo
            .find_personal_runtime(&principal.tenant_id, &principal.subject)
            .await
            .map_err(|_| {
                tracing::warn!(
                    tenant_id = %principal.tenant_id,
                    subject = %principal.subject,
                    "mcp oauth bearer rejected: personal MCP config store error"
                );
                AuthenticationError::InvalidToken {
                    description: "MCP configuration store error",
                }
            })?
        else {
            tracing::warn!(
                tenant_id = %principal.tenant_id,
                subject = %principal.subject,
                client_id = %principal.client_id,
                "mcp oauth bearer rejected: no active personal MCP configuration"
            );
            return Err(AuthenticationError::InvalidToken {
                description:
                    "OAuth token subject is not bound to an active personal MCP configuration",
            });
        };

        tracing::info!(
            tenant_id = %principal.tenant_id,
            subject = %principal.subject,
            client_id = %principal.client_id,
            config_id = %cfg.id,
            "mcp oauth bearer accepted"
        );

        let mut extra = serde_json::Map::new();
        extra.insert("plasm_mcp_config_id".to_string(), json!(cfg.id.to_string()));
        extra.insert("plasm_space_type".to_string(), json!("personal"));
        extra.insert(
            "plasm_owner_subject".to_string(),
            json!(principal.subject.clone()),
        );
        extra.insert("plasm_mcp_oauth".to_string(), json!(true));

        let scopes: Vec<String> = principal
            .scope
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let scopes = if scopes.is_empty() {
            vec![OAUTH_SCOPE.to_string()]
        } else {
            scopes
        };

        Ok(AuthInfo {
            token_unique_id: format!("{:x}", Sha256::digest(raw.as_bytes())),
            client_id: Some(principal.client_id.clone()),
            user_id: Some(principal.subject.clone()),
            scopes: Some(scopes),
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

    fn oauth_error_from_domain(err: McpOAuthError) -> OauthHttpResponse {
        let status = match &err {
            McpOAuthError::OAuth { .. } | McpOAuthError::InvalidTarget { .. } => {
                StatusCode::BAD_REQUEST
            }
            McpOAuthError::AccessDenied { description } => {
                if description.contains("principal token is invalid") {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::FORBIDDEN
                }
            }
            McpOAuthError::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            McpOAuthError::Server { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            McpOAuthError::RateLimited { .. } => StatusCode::BAD_REQUEST,
        };
        Self::oauth_error_response(status, err.oauth_error_code(), err.description())
    }

    fn authorization_prompt_html(params: &HashMap<String, String>, authorize_path: &str) -> String {
        let client_id = params.get("client_id").cloned().unwrap_or_default();
        let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
        let scope = params
            .get("scope")
            .cloned()
            .unwrap_or_else(|| OAUTH_SCOPE.to_string());
        let state = params.get("state").cloned().unwrap_or_default();
        let resource = params.get("resource").cloned().unwrap_or_default();
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
    <input type="hidden" name="resource" value="{resource}" />
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
            resource = resource,
            code_challenge = code_challenge,
            code_challenge_method = code_challenge_method,
        )
    }

    fn parse_oauth_token_request(body: &str) -> Result<McpOAuthTokenRequest, OauthHttpResponse> {
        if body.starts_with('{') {
            return serde_json::from_str::<McpOAuthTokenRequest>(body).map_err(|_| {
                Self::oauth_error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "invalid token request body",
                )
            });
        }

        let form = Self::parse_form_body(body);
        Ok(McpOAuthTokenRequest {
            grant_type: form.get("grant_type").cloned(),
            client_id: form.get("client_id").cloned(),
            code: form.get("code").cloned(),
            redirect_uri: form.get("redirect_uri").cloned(),
            code_verifier: form.get("code_verifier").cloned(),
            refresh_token: form.get("refresh_token").cloned(),
            resource: form.get("resource").cloned(),
        })
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
        let oauth = match self.oauth_service().await {
            Ok(o) => o,
            Err(resp) => return Ok(resp),
        };
        match oauth.register_client(request.body(), None).await {
            Ok(response) => {
                let value = serde_json::to_value(response).unwrap_or_else(|_| {
                    Self::oauth_error_json(
                        "server_error",
                        "registration response serialization failed",
                    )
                });
                Ok(Self::json_response(StatusCode::CREATED, value))
            }
            Err(err) => Ok(Self::oauth_error_from_domain(err)),
        }
    }

    async fn oauth_authorize(
        &self,
        request: http::Request<&str>,
    ) -> Result<http::Response<GenericBody>, TransportServerError> {
        let oauth = match self.oauth_service().await {
            Ok(o) => o,
            Err(resp) => return Ok(resp),
        };
        let params = Self::parse_query(&request);
        let authorize_path = request.uri().path();
        match oauth.authorize(&self.plasm, &params).await {
            Ok(AuthorizeOutcome::AwaitingPrincipal) => Ok(Self::html_response(
                StatusCode::OK,
                Self::authorization_prompt_html(&params, authorize_path),
            )),
            Ok(AuthorizeOutcome::Redirect { location }) => Ok(Self::redirect_response(&location)),
            Err(err) => Ok(Self::oauth_error_from_domain(err)),
        }
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

        let oauth = match self.oauth_service().await {
            Ok(o) => o,
            Err(resp) => return Ok(resp),
        };

        let form = match Self::parse_oauth_token_request(request.body().trim()) {
            Ok(form) => form,
            Err(resp) => return Ok(resp),
        };

        match oauth.exchange_token(&form).await {
            Ok(response) => {
                let payload = serde_json::to_value(response).unwrap_or_else(|_| {
                    Self::oauth_error_json("server_error", "token serialization failed")
                });
                Ok(Self::json_response(StatusCode::OK, payload))
            }
            Err(err) => {
                tracing::warn!(
                    oauth_error = err.oauth_error_code(),
                    description = err.description(),
                    "mcp inbound oauth token exchange failed"
                );
                Ok(Self::oauth_error_from_domain(err))
            }
        }
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
