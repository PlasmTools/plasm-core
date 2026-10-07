//! Persisted OAuth record shapes; serde field names preserve existing storage.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct ClientMetadata {
    pub redirect_uris: Option<Vec<String>>,
    pub token_endpoint_auth_method: Option<String>,
    pub grant_types: Option<Vec<String>>,
    pub response_types: Option<Vec<String>>,
    pub scope: Option<String>,
    #[serde(flatten)]
    pub additional_metadata: HashMap<String, serde_json::Value>,
}
#[derive(Serialize, Deserialize)]
pub struct RegisteredClient {
    pub client_id: String,
    pub client_secret_hash: Option<String>,
    pub registration_access_token_hash: String,
    pub metadata: ClientMetadata,
    pub registered_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub client_secret_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub is_active: bool,
}
#[derive(Serialize, Deserialize)]
pub struct AuthorizationCode {
    pub code: String,
    pub client_id: String,
    pub user_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub issued_at: SystemTime,
    pub expires_at: SystemTime,
    pub is_used: bool,
}
impl AuthorizationCode {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client_id: String,
        user_id: String,
        redirect_uri: String,
        scopes: Vec<String>,
        code_challenge: Option<String>,
        code_challenge_method: Option<String>,
        ttl: Duration,
    ) -> Result<Self, crate::secret_store::SecretStoreError> {
        let issued_at = SystemTime::now();
        Ok(Self {
            code: crate::secret_store::random_token()?,
            client_id,
            user_id,
            redirect_uri,
            scopes,
            code_challenge,
            code_challenge_method,
            issued_at,
            expires_at: issued_at + ttl,
            is_used: false,
        })
    }
    pub fn is_valid(&self) -> bool {
        !self.is_used && self.expires_at > SystemTime::now()
    }
}
#[derive(Serialize, Deserialize)]
pub struct RefreshToken {
    pub token_id: String,
    pub client_id: String,
    pub user_id: String,
    pub scopes: Vec<String>,
    pub issued_at: SystemTime,
    pub expires_at: SystemTime,
    pub is_revoked: bool,
}
impl RefreshToken {
    pub fn new(
        client_id: String,
        user_id: String,
        scopes: Vec<String>,
        ttl: Duration,
    ) -> Result<Self, crate::secret_store::SecretStoreError> {
        let issued_at = SystemTime::now();
        Ok(Self {
            token_id: crate::secret_store::random_token()?,
            client_id,
            user_id,
            scopes,
            issued_at,
            expires_at: issued_at + ttl,
            is_revoked: false,
        })
    }
    pub fn is_valid(&self) -> bool {
        !self.is_revoked && self.expires_at > SystemTime::now()
    }
}
