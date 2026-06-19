use std::net::IpAddr;
use std::time::Duration;

use auth_framework::server::core::client_registration::{
    ClientRegistrationConfig, ClientRegistrationManager, ClientRegistrationRequest,
};

use super::error::{map_auth_error, McpOAuthError};
use super::types::{McpOAuthRegisterResponse, OAUTH_REGISTER_PATH, OAUTH_SCOPE};

pub struct DcrHandle<'a> {
    dcr: &'a ClientRegistrationManager,
    canonical_resource: &'a str,
}

impl<'a> DcrHandle<'a> {
    pub fn new(dcr: &'a ClientRegistrationManager, canonical_resource: &'a str) -> Self {
        Self {
            dcr,
            canonical_resource,
        }
    }

    pub async fn register_client(
        &self,
        body: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<McpOAuthRegisterResponse, McpOAuthError> {
        let payload: ClientRegistrationRequest = serde_json::from_str(body.trim()).map_err(|_| {
            McpOAuthError::bad_request("invalid_request", "invalid registration JSON")
        })?;

        if payload
            .redirect_uris
            .as_ref()
            .is_none_or(|uris| uris.is_empty())
        {
            return Err(McpOAuthError::bad_request(
                "invalid_redirect_uri",
                "redirect_uris must be non-empty valid URLs",
            ));
        }
        let token_endpoint_auth_method = payload
            .token_endpoint_auth_method
            .clone()
            .unwrap_or_else(|| "none".to_string())
            .trim()
            .to_ascii_lowercase();
        if token_endpoint_auth_method != "none" {
            return Err(McpOAuthError::bad_request(
                "invalid_client_metadata",
                "dynamic registration supports public clients only (token_endpoint_auth_method=none)",
            ));
        }

        let grant_types = payload
            .grant_types
            .clone()
            .unwrap_or_else(|| vec!["authorization_code".to_string()]);
        let normalized_grant_types: Vec<String> = grant_types
            .iter()
            .map(|g| g.trim().to_ascii_lowercase())
            .filter(|g| !g.is_empty())
            .collect();
        if !normalized_grant_types.iter().any(|g| g == "authorization_code")
            || !normalized_grant_types
                .iter()
                .all(|g| g == "authorization_code" || g == "refresh_token")
        {
            return Err(McpOAuthError::bad_request(
                "invalid_client_metadata",
                "grant_types must include authorization_code (optional refresh_token is allowed)",
            ));
        }

        let response_types = payload
            .response_types
            .clone()
            .unwrap_or_else(|| vec!["code".to_string()]);
        let normalized_response_types: Vec<String> = response_types
            .iter()
            .map(|r| r.trim().to_ascii_lowercase())
            .filter(|r| !r.is_empty())
            .collect();
        if !normalized_response_types.iter().any(|r| r == "code")
            || !normalized_response_types.iter().all(|r| r == "code")
        {
            return Err(McpOAuthError::bad_request(
                "invalid_client_metadata",
                "response_types must include code",
            ));
        }

        let mut request = payload;
        request.token_endpoint_auth_method = Some("none".to_string());
        request.grant_types = Some(normalized_grant_types.clone());
        request.response_types = Some(normalized_response_types.clone());

        let registered = self
            .dcr
            .register_client(request, client_ip)
            .await
            .map_err(map_auth_error)?;

        let scope = registered
            .scope
            .clone()
            .unwrap_or_else(|| OAUTH_SCOPE.to_string());

        Ok(McpOAuthRegisterResponse {
            client_id: registered.client_id,
            client_secret: registered.client_secret.unwrap_or_default(),
            client_id_issued_at: registered.client_id_issued_at.unwrap_or(0) as u64,
            client_secret_expires_at: registered.client_secret_expires_at.unwrap_or(0) as u64,
            redirect_uris: registered.redirect_uris.unwrap_or_default(),
            token_endpoint_auth_method: "none".to_string(),
            grant_types: normalized_grant_types,
            response_types: normalized_response_types,
            scope,
            registration_client_uri: format!("{}{}", self.canonical_resource, OAUTH_REGISTER_PATH),
            registration_access_token: registered.registration_access_token,
        })
    }
}

pub fn dcr_config(base_url: &str) -> ClientRegistrationConfig {
    ClientRegistrationConfig {
        base_url: base_url.to_string(),
        require_authentication: false,
        default_secret_expiration: Some(86400 * 365),
        max_redirect_uris: 10,
        allowed_grant_types: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ],
        allowed_response_types: vec!["code".to_string()],
        allowed_auth_methods: vec!["none".to_string()],
        allow_public_clients: true,
        rate_limit_per_ip: 1000,
        rate_limit_window: Duration::from_secs(3600),
    }
}
