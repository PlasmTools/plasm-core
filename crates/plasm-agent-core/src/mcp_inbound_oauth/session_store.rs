use std::time::{Duration, SystemTime};

use auth_framework::oauth2_enhanced_storage::{EnhancedAuthorizationCode, RefreshToken};
use auth_framework::storage::core::AuthStorage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::error::McpOAuthError;

pub const OAUTH_AUTH_CODE_TTL_SECS: u64 = 600;
pub const OAUTH_REFRESH_TOKEN_TTL_SECS: u64 = 86400 * 30;

#[derive(Serialize, Deserialize)]
pub struct PlasmOAuthAuthCode {
    pub enhanced: EnhancedAuthorizationCode,
    pub tenant_id: String,
    pub resource: String,
}

#[derive(Serialize, Deserialize)]
pub struct PlasmOAuthRefreshRecord {
    pub enhanced: RefreshToken,
    pub tenant_id: String,
    pub resource: String,
}

pub struct OAuthSessionStore<'a> {
    storage: &'a dyn AuthStorage,
}

impl<'a> OAuthSessionStore<'a> {
    pub fn new(storage: &'a dyn AuthStorage) -> Self {
        Self { storage }
    }

    pub async fn store_auth_code(&self, row: &PlasmOAuthAuthCode) -> Result<(), McpOAuthError> {
        let key = auth_code_key(&row.enhanced.code);
        let bytes = serde_json::to_vec(row)
            .map_err(|_| McpOAuthError::server("OAuth authorization code encode failed"))?;
        self.storage
            .store_kv(
                &key,
                &bytes,
                Some(Duration::from_secs(OAUTH_AUTH_CODE_TTL_SECS)),
            )
            .await
            .map_err(|_| McpOAuthError::server("OAuth authorization code storage failed"))
    }

    pub async fn peek_auth_code(&self, code: &str) -> Result<PlasmOAuthAuthCode, McpOAuthError> {
        let key = auth_code_key(code);
        let bytes = self
            .storage
            .get_kv(&key)
            .await
            .map_err(|_| McpOAuthError::server("OAuth authorization code read failed"))?
            .ok_or_else(|| {
                McpOAuthError::bad_request(
                    "invalid_grant",
                    "authorization code is missing or expired",
                )
            })?;
        serde_json::from_slice(&bytes)
            .map_err(|_| McpOAuthError::server("OAuth authorization code decode failed"))
    }

    pub async fn delete_auth_code(&self, code: &str) -> Result<(), McpOAuthError> {
        self.storage
            .delete_kv(&auth_code_key(code))
            .await
            .map_err(|_| McpOAuthError::server("OAuth authorization code consume failed"))
    }

    pub async fn mint_and_store_refresh_token(
        &self,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        scope: &str,
        resource: &str,
    ) -> Result<String, McpOAuthError> {
        let wire = format!("plasm_rtok_{}", Uuid::new_v4().simple());
        let scopes: Vec<String> = scope.split_whitespace().map(str::to_string).collect();
        let enhanced = RefreshToken::new(
            client_id.to_string(),
            subject.to_string(),
            scopes,
            Duration::from_secs(OAUTH_REFRESH_TOKEN_TTL_SECS),
        );
        let record = PlasmOAuthRefreshRecord {
            enhanced,
            tenant_id: tenant_id.to_string(),
            resource: resource.to_string(),
        };
        self.store_refresh_token_record(&wire, &record).await?;
        Ok(wire)
    }

    pub async fn store_refresh_token_record(
        &self,
        wire: &str,
        record: &PlasmOAuthRefreshRecord,
    ) -> Result<(), McpOAuthError> {
        let key = refresh_key(&sha256_hex(wire.as_bytes()));
        let ttl = record
            .enhanced
            .expires_at
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .as_secs();
        if ttl == 0 {
            return Err(McpOAuthError::server("refresh token TTL computation failed"));
        }
        let bytes = serde_json::to_vec(record)
            .map_err(|_| McpOAuthError::server("OAuth refresh token encode failed"))?;
        self.storage
            .store_kv(&key, &bytes, Some(Duration::from_secs(ttl)))
            .await
            .map_err(|_| McpOAuthError::server("OAuth refresh token storage failed"))
    }

    pub async fn load_refresh_token(&self, wire: &str) -> Result<PlasmOAuthRefreshRecord, McpOAuthError> {
        let key = refresh_key(&sha256_hex(wire.as_bytes()));
        let bytes = self
            .storage
            .get_kv(&key)
            .await
            .map_err(|_| McpOAuthError::server("OAuth refresh token read failed"))?
            .ok_or_else(|| {
                McpOAuthError::bad_request(
                    "invalid_grant",
                    "refresh token is invalid or expired",
                )
            })?;
        serde_json::from_slice(&bytes)
            .map_err(|_| McpOAuthError::server("OAuth refresh token decode failed"))
    }

    pub async fn consume_refresh_token(&self, wire: &str) -> Result<(), McpOAuthError> {
        self.storage
            .delete_kv(&refresh_key(&sha256_hex(wire.as_bytes())))
            .await
            .map_err(|_| McpOAuthError::server("OAuth refresh token consume failed"))
    }
}

fn auth_code_key(code: &str) -> String {
    format!("oauth_auth_code:{code}")
}

fn refresh_key(token_hash: &str) -> String {
    format!("oauth_refresh:{token_hash}")
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
