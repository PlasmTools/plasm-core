use serde::{Deserialize, Serialize};

pub const OAUTH_SCOPE: &str = "mcp:tools";
pub const OAUTH_REGISTER_PATH: &str = "/oauth/register";
const MCP_OAUTH_PREFIX: &str = "/mcp";

#[derive(Debug, Clone, Serialize)]
pub struct McpOAuthRegisterResponse {
    pub client_id: String,
    pub client_secret: String,
    pub client_id_issued_at: u64,
    pub client_secret_expires_at: u64,
    pub redirect_uris: Vec<String>,
    pub token_endpoint_auth_method: String,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    pub scope: String,
    pub registration_client_uri: String,
    pub registration_access_token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpOAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpOAuthTokenRequest {
    pub grant_type: Option<String>,
    pub client_id: Option<String>,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
    pub resource: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AuthorizeOutcome {
    AwaitingPrincipal,
    Redirect { location: String },
}

pub fn mcp_public_base_url() -> String {
    std::env::var("PLASM_MCP_PUBLIC_BASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:3001".to_string())
}

pub fn mcp_resource_base_url() -> String {
    let base = mcp_public_base_url();
    if base.ends_with(MCP_OAUTH_PREFIX) {
        base
    } else {
        format!("{base}{MCP_OAUTH_PREFIX}")
    }
}
