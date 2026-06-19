use auth_framework::server::core::client_registration::RegisteredClient;
use auth_framework::storage::core::AuthStorage;

use super::error::McpOAuthError;

pub fn registration_key(client_id: &str) -> String {
    format!("client_registration:{client_id}")
}

pub async fn load_registered_client(
    storage: &dyn AuthStorage,
    client_id: &str,
) -> Result<RegisteredClient, McpOAuthError> {
    let key = registration_key(client_id);
    let bytes = storage
        .get_kv(&key)
        .await
        .map_err(|e| McpOAuthError::server(&e.to_string()))?
        .ok_or_else(|| McpOAuthError::bad_request("invalid_client", "unknown client_id"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| McpOAuthError::server("registered client decode failed"))
}

pub fn redirect_uri_allowed(client: &RegisteredClient, redirect_uri: &str) -> bool {
    client
        .metadata
        .redirect_uris
        .as_ref()
        .is_some_and(|uris| uris.iter().any(|uri| uri == redirect_uri))
}

pub fn grant_type_allowed(client: &RegisteredClient, grant_type: &str) -> bool {
    client
        .metadata
        .grant_types
        .as_ref()
        .is_some_and(|grants| grants.iter().any(|g| g == grant_type))
}
