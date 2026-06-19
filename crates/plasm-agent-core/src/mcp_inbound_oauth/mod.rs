//! MCP inbound OAuth backed by auth-framework (DCR, session KV, JwtManager).

mod client;
mod dcr;
mod error;
mod grants;
mod jwt;
mod pkce;
mod principal;
mod resource;
mod session_store;
mod types;

use std::net::IpAddr;
use std::sync::Arc;

use auth_framework::server::core::client_registration::ClientRegistrationManager;
use auth_framework::storage::core::AuthStorage;
use auth_framework::AuthError;

use crate::auth_framework_host::resolve_jwt_signing_secret;
use crate::server_state::PlasmHostState;

pub use error::McpOAuthError;
pub use jwt::VerifiedMcpOAuthAccess;
pub use types::{
    mcp_resource_base_url, AuthorizeOutcome, McpOAuthRegisterResponse, McpOAuthTokenRequest,
    OAUTH_SCOPE,
};

pub struct McpInboundOAuthService {
    storage: Arc<dyn AuthStorage>,
    dcr: ClientRegistrationManager,
    jwt: jwt::McpOAuthJwt,
    canonical_resource: String,
}

impl McpInboundOAuthService {
    pub async fn try_from_host(plasm: &PlasmHostState) -> Option<Self> {
        let storage = plasm.auth_storage()?.clone();
        let jwt_secret = plasm
            .incoming_auth
            .as_deref()
            .and_then(|v| v.config().jwt_secret.clone())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| resolve_jwt_signing_secret().ok())?;
        Self::new(storage, jwt_secret).await.ok()
    }

    pub async fn new(storage: Arc<dyn AuthStorage>, jwt_secret: String) -> Result<Self, AuthError> {
        let canonical_resource = mcp_resource_base_url();
        let dcr =
            ClientRegistrationManager::new(dcr::dcr_config(&canonical_resource), storage.clone());
        let jwt = jwt::McpOAuthJwt::new(&jwt_secret, &canonical_resource);
        Ok(Self {
            storage,
            dcr,
            jwt,
            canonical_resource,
        })
    }

    pub async fn register_client(
        &self,
        body: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<McpOAuthRegisterResponse, McpOAuthError> {
        dcr::DcrHandle::new(&self.dcr, &self.canonical_resource)
            .register_client(body, client_ip)
            .await
    }

    pub fn verify_access_token(&self, token: &str) -> Result<VerifiedMcpOAuthAccess, AuthError> {
        self.jwt.verify_access_token(token)
    }
}
