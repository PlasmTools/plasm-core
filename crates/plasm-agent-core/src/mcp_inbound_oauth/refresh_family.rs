//! One durable record owns an authorization grant and its refresh-token lineage.
use std::{collections::BTreeSet, time::Duration};

use serde::{Deserialize, Serialize};

use super::{
    error::McpOAuthError,
    jwt::OAUTH_ACCESS_TOKEN_TTL_SECS,
    records::RefreshToken,
    session_store::{OAuthSessionStore, OAUTH_REFRESH_TOKEN_TTL_SECS},
    types::OAUTH_SCOPE,
};

// Bound retained replay evidence as well as the family's absolute lifetime.
const MAX_REFRESH_ROTATIONS: usize = 4096;
#[cfg(test)]
#[path = "refresh_family_tests.rs"]
mod tests;

#[derive(Serialize, Deserialize)]
struct RefreshFamily {
    enhanced: RefreshToken,
    tenant_id: String,
    resource: String,
    current_hash: Option<String>,
    spent_hashes: BTreeSet<String>,
}

pub(super) struct RotatedGrant {
    pub grant_id: String,
    pub tenant_id: String,
    pub subject: String,
    pub refresh_token: String,
}

fn invalid() -> McpOAuthError {
    McpOAuthError::bad_request("invalid_grant", "authorization grant is invalid or revoked")
}

fn grant_key(id: &str) -> String {
    format!("oauth_refresh_family:{id}")
}

fn valid_component(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

fn grant_id(wire: &str) -> Result<&str, McpOAuthError> {
    let (id, secret) = wire
        .strip_prefix("plasm_rtok_")
        .and_then(|value| value.split_once('.'))
        .ok_or_else(invalid)?;
    if !valid_component(id) || !valid_component(secret) {
        return Err(invalid());
    }
    Ok(id)
}

fn fresh_wire(id: &str) -> Result<String, McpOAuthError> {
    Ok(format!(
        "plasm_rtok_{id}.{}",
        crate::secret_store::random_token().map_err(McpOAuthError::from)?
    ))
}

fn token_hash(wire: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(wire.as_bytes()))
}

impl OAuthSessionStore<'_> {
    pub async fn mint_grant(
        &self,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        resource: &str,
        with_refresh: bool,
    ) -> Result<(String, Option<String>), McpOAuthError> {
        let ttl = if with_refresh {
            OAUTH_REFRESH_TOKEN_TTL_SECS
        } else {
            OAUTH_ACCESS_TOKEN_TTL_SECS
        };
        let enhanced = RefreshToken::new(
            client_id.into(),
            subject.into(),
            vec![OAUTH_SCOPE.into()],
            Duration::from_secs(ttl),
        )
        .map_err(McpOAuthError::from)?;
        let id = enhanced.token_id.clone();
        let wire = with_refresh.then(|| fresh_wire(&id)).transpose()?;
        let family = RefreshFamily {
            enhanced,
            tenant_id: tenant_id.into(),
            resource: resource.into(),
            current_hash: wire.as_deref().map(token_hash),
            spent_hashes: BTreeSet::new(),
        };
        let bytes = serde_json::to_vec(&family).map_err(McpOAuthError::from)?;
        self.storage
            .store_kv(&grant_key(&id), &bytes, Some(Duration::from_secs(ttl)))
            .await
            .map_err(McpOAuthError::from)?;
        Ok((id, wire))
    }

    /// Rotation and replay revocation compete on the same expiring record.
    pub async fn rotate_refresh_token(
        &self,
        wire: &str,
        client_id: &str,
        resource: &str,
    ) -> Result<RotatedGrant, McpOAuthError> {
        let id = grant_id(wire)?;
        let key = grant_key(id);
        let supplied = token_hash(wire);
        loop {
            let expected = self
                .storage
                .get_kv(&key)
                .await
                .map_err(McpOAuthError::from)?
                .ok_or_else(invalid)?;
            let mut family: RefreshFamily =
                serde_json::from_slice(&expected).map_err(McpOAuthError::from)?;
            if !family.enhanced.is_valid()
                || family.enhanced.client_id != client_id
                || family.resource != resource
                || family.enhanced.scopes != [OAUTH_SCOPE]
            {
                return Err(invalid());
            }

            let reused = family.spent_hashes.contains(&supplied);
            if !reused && family.current_hash.as_deref() != Some(&supplied) {
                return Err(invalid());
            }
            let revoke = reused || family.spent_hashes.len() >= MAX_REFRESH_ROTATIONS;
            let next = if revoke {
                family.enhanced.is_revoked = true;
                family.current_hash = None;
                None
            } else {
                let next = fresh_wire(id)?;
                family.spent_hashes.insert(supplied.clone());
                family.current_hash = Some(token_hash(&next));
                Some(next)
            };
            let replacement = serde_json::to_vec(&family).map_err(McpOAuthError::from)?;
            if !self
                .storage
                .compare_exchange_kv(&key, &expected, &replacement)
                .await
                .map_err(McpOAuthError::from)?
            {
                // A competing rotation may have made this request a replay.
                tokio::task::yield_now().await;
                continue;
            }
            let refresh_token = next.ok_or_else(invalid)?;
            return Ok(RotatedGrant {
                grant_id: id.into(),
                tenant_id: family.tenant_id,
                subject: family.enhanced.user_id,
                refresh_token,
            });
        }
    }

    pub async fn validate_access_grant(
        &self,
        id: &str,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        resource: &str,
    ) -> Result<(), McpOAuthError> {
        if !valid_component(id) {
            return Err(invalid());
        }
        let bytes = self
            .storage
            .get_kv(&grant_key(id))
            .await
            .map_err(McpOAuthError::from)?
            .ok_or_else(invalid)?;
        let family: RefreshFamily = serde_json::from_slice(&bytes).map_err(McpOAuthError::from)?;
        if !family.enhanced.is_valid()
            || family.enhanced.client_id != client_id
            || family.tenant_id != tenant_id
            || family.enhanced.user_id != subject
            || family.resource != resource
            || family.enhanced.scopes != [OAUTH_SCOPE]
        {
            return Err(invalid());
        }
        Ok(())
    }
}
