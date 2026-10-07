use super::records::{ClientMetadata, RegisteredClient};
use crate::secret_store::SecretStore;
use std::net::IpAddr;
use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};

use super::error::McpOAuthError;
use super::types::{McpOAuthRegisterResponse, OAUTH_SCOPE};

pub struct DcrHandle<'a> {
    dcr: &'a dyn SecretStore,
}

impl<'a> DcrHandle<'a> {
    pub fn new(dcr: &'a dyn SecretStore) -> Self {
        Self { dcr }
    }

    pub async fn register_client(
        &self,
        body: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<McpOAuthRegisterResponse, McpOAuthError> {
        enforce_registration_rate(client_ip).await?;
        if body.len() > 65536 {
            return Err(McpOAuthError::bad_request(
                "invalid_client_metadata",
                "registration body exceeds 64 KiB",
            ));
        }
        let payload: ClientMetadata = serde_json::from_str(body.trim()).map_err(|_| {
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
        if !normalized_grant_types
            .iter()
            .any(|g| g == "authorization_code")
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

        if body.len() > 65536 {
            return Err(McpOAuthError::bad_request(
                "invalid_client_metadata",
                "registration body exceeds 64 KiB",
            ));
        }
        let uris = request
            .redirect_uris
            .as_ref()
            .expect("validated redirect list");
        if uris.len() > 10 || uris.iter().any(|uri| !valid_redirect(uri)) {
            return Err(McpOAuthError::bad_request(
                "invalid_redirect_uri",
                "redirect URIs must use HTTPS or HTTP loopback and contain no fragments",
            ));
        }
        if request
            .scope
            .as_deref()
            .is_some_and(|scope| scope != OAUTH_SCOPE)
        {
            return Err(McpOAuthError::bad_request(
                "invalid_scope",
                "only mcp:tools is supported",
            ));
        }
        request.scope = Some(OAUTH_SCOPE.into());
        let client_id = format!("plasm_client_{}", uuid::Uuid::new_v4().simple());
        let now = chrono::Utc::now();
        let registered = RegisteredClient {
            client_id: client_id.clone(),
            client_secret_hash: None,
            registration_access_token_hash: String::new(),
            metadata: request,
            registered_at: now,
            updated_at: now,
            client_secret_expires_at: None,
            is_active: true,
        };
        self.dcr
            .store_kv(
                &super::client::registration_key(&client_id),
                &serde_json::to_vec(&registered).map_err(McpOAuthError::from)?,
                None,
            )
            .await
            .map_err(McpOAuthError::from)?;

        let scope = registered
            .metadata
            .scope
            .clone()
            .unwrap_or_else(|| OAUTH_SCOPE.to_string());

        Ok(McpOAuthRegisterResponse {
            client_id: registered.client_id,
            client_id_issued_at: now.timestamp() as u64,
            redirect_uris: registered.metadata.redirect_uris.unwrap_or_default(),
            token_endpoint_auth_method: "none".to_string(),
            grant_types: normalized_grant_types,
            response_types: normalized_response_types,
            scope,
        })
    }
}

pub(super) fn valid_redirect(uri: &str) -> bool {
    let Ok(url) = url::Url::parse(uri) else {
        return false;
    };
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => url.host_str().is_some_and(|host| {
            host == "localhost"
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
                || host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }),
        _ => false,
    }
}

async fn enforce_registration_rate(ip: Option<IpAddr>) -> Result<(), McpOAuthError> {
    type RegistrationAttempts = HashMap<Option<IpAddr>, (Instant, u32)>;
    static LIMITS: OnceLock<tokio::sync::Mutex<RegistrationAttempts>> = OnceLock::new();
    let mut limits = LIMITS.get_or_init(Default::default).lock().await;
    let now = Instant::now();
    limits.retain(|_, (started, _)| now.duration_since(*started) < Duration::from_secs(3600));
    if limits.len() >= 10000 && !limits.contains_key(&ip) {
        return Err(McpOAuthError::RateLimited {
            description: "registration rate capacity reached".into(),
        });
    }
    let (_, count) = limits.entry(ip).or_insert((now, 0));
    if *count >= 1000 {
        return Err(McpOAuthError::RateLimited {
            description: "registration rate limit exceeded".into(),
        });
    }
    *count += 1;
    Ok(())
}
