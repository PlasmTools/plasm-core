use std::time::Duration;

use super::records::AuthorizationCode;
use crate::secret_store::SecretStore;
use serde::{Deserialize, Serialize};

use super::error::McpOAuthError;

pub const OAUTH_AUTH_CODE_TTL_SECS: u64 = 600;
pub const OAUTH_REFRESH_TOKEN_TTL_SECS: u64 = 86400 * 30;

#[derive(Serialize, Deserialize)]
pub struct PlasmOAuthAuthCode {
    pub enhanced: AuthorizationCode,
    pub tenant_id: String,
    pub resource: String,
}

pub struct OAuthSessionStore<'a> {
    pub(super) storage: &'a dyn SecretStore,
}

impl<'a> OAuthSessionStore<'a> {
    pub fn new(storage: &'a dyn SecretStore) -> Self {
        Self { storage }
    }

    pub async fn store_auth_code(&self, row: &PlasmOAuthAuthCode) -> Result<(), McpOAuthError> {
        let key = auth_code_key(&row.enhanced.code);
        let bytes = serde_json::to_vec(row).map_err(McpOAuthError::from)?;
        self.storage
            .store_kv(
                &key,
                &bytes,
                Some(Duration::from_secs(OAUTH_AUTH_CODE_TTL_SECS)),
            )
            .await
            .map_err(McpOAuthError::from)
    }

    pub async fn peek_auth_code(
        &self,
        code: &str,
    ) -> Result<(PlasmOAuthAuthCode, Vec<u8>), McpOAuthError> {
        let key = auth_code_key(code);
        let bytes = self
            .storage
            .get_kv(&key)
            .await
            .map_err(McpOAuthError::from)?
            .ok_or_else(|| {
                McpOAuthError::bad_request(
                    "invalid_grant",
                    "authorization code is missing or expired",
                )
            })?;
        let record = serde_json::from_slice(&bytes).map_err(McpOAuthError::from)?;
        Ok((record, bytes))
    }

    pub async fn consume_auth_code(
        &self,
        code: &str,
        expected: &[u8],
    ) -> Result<(), McpOAuthError> {
        let consumed = self
            .storage
            .consume_kv(&auth_code_key(code), expected)
            .await
            .map_err(McpOAuthError::from)?;
        if !consumed {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "grant already consumed",
            ));
        }
        Ok(())
    }
}

fn auth_code_key(code: &str) -> String {
    format!("oauth_auth_code:{code}")
}
