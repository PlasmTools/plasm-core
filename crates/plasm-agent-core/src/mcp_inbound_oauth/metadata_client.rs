//! Bounded Client ID Metadata Document retrieval with pinned public DNS results.
use super::{
    error::{CimdRetrievalError, McpOAuthError},
    records::{ClientMetadata, RegisteredClient},
    types::OAUTH_SCOPE,
};
use crate::secret_store::SecretStore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

const MAX_DOCUMENT_BYTES: usize = 65536;
#[cfg(test)]
#[path = "metadata_retrieval_tests.rs"]
mod retrieval_tests;
#[derive(Deserialize)]
struct Document {
    client_id: String,
    client_name: String,
    #[serde(flatten)]
    metadata: ClientMetadata,
}
fn invalid() -> McpOAuthError {
    McpOAuthError::bad_request(
        "invalid_client_metadata",
        "client metadata must be a bounded public HTTPS JSON document with matching client_id",
    )
}

pub(super) async fn load(
    storage: &dyn SecretStore,
    client_id: &str,
) -> Result<RegisteredClient, McpOAuthError> {
    static LOADER: OnceLock<MetadataLoader> = OnceLock::new();
    LOADER
        .get_or_init(MetadataLoader::default)
        .load(storage, client_id)
        .await
}

struct MetadataLoader {
    slots: Arc<tokio::sync::Semaphore>,
    keys: tokio::sync::Mutex<HashMap<String, Instant>>,
    lookup_timeout: Duration,
    request_timeout: Duration,
    cache_ttl: Duration,
    #[cfg(test)]
    fixture: Option<(SocketAddr, reqwest::Certificate)>,
    #[cfg(test)]
    lookup_delay: Duration,
    #[cfg(test)]
    resolved_addresses: Option<Vec<SocketAddr>>,
    #[cfg(test)]
    resolver_error: Option<std::io::ErrorKind>,
}
impl Default for MetadataLoader {
    fn default() -> Self {
        Self {
            slots: Arc::new(tokio::sync::Semaphore::new(16)),
            keys: Default::default(),
            lookup_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            cache_ttl: Duration::from_secs(300),
            #[cfg(test)]
            fixture: None,
            #[cfg(test)]
            lookup_delay: Duration::ZERO,
            #[cfg(test)]
            resolved_addresses: None,
            #[cfg(test)]
            resolver_error: None,
        }
    }
}
impl MetadataLoader {
    async fn load(
        &self,
        storage: &dyn SecretStore,
        client_id: &str,
    ) -> Result<RegisteredClient, McpOAuthError> {
        let url = url::Url::parse(client_id).map_err(|_| invalid())?;
        if url.path() == "/"
            || url.path().is_empty()
            || url.scheme() != "https"
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(invalid());
        }
        let host = url.host_str().ok_or_else(invalid)?;
        let key = format!(
            "oauth_cimd:{}",
            hex::encode(Sha256::digest(client_id.as_bytes()))
        );
        if let Some(bytes) = storage.get_kv(&key).await.map_err(McpOAuthError::from)? {
            return validate_document(client_id, &bytes);
        }
        let permit = tokio::time::timeout(self.lookup_timeout, self.slots.clone().acquire_owned())
            .await
            .map_err(CimdRetrievalError::CapacityTimeout)?
            .map_err(CimdRetrievalError::CapacityClosed)?;
        let port = url.port_or_known_default().ok_or_else(invalid)?;
        let resolver_host = host.trim_matches(['[', ']']).to_owned();
        // A timeout cannot cancel the OS resolver. Keep the fetch permit inside
        // that task until it finishes, so timed-out callers cannot multiply it.
        #[cfg(test)]
        let fixture_address = self.fixture.as_ref().map(|fixture| fixture.0);
        #[cfg(test)]
        let delay = self.lookup_delay;
        #[cfg(test)]
        let resolved_addresses = self.resolved_addresses.clone();
        #[cfg(test)]
        let resolver_error = self.resolver_error;
        let lookup = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(kind) = resolver_error {
                return Err(std::io::Error::from(kind));
            }
            #[cfg(test)]
            if let Some(address) = fixture_address {
                std::thread::sleep(delay);
                return Ok((resolved_addresses.unwrap_or_else(|| vec![address]), permit));
            }
            (resolver_host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| (addresses.take(17).collect::<Vec<SocketAddr>>(), permit))
        });
        let (addresses, _permit) = tokio::time::timeout(self.lookup_timeout, lookup)
            .await
            .map_err(CimdRetrievalError::ResolutionTimeout)?
            .map_err(CimdRetrievalError::ResolutionTask)?
            .map_err(CimdRetrievalError::Resolve)?;
        if addresses.is_empty()
            || addresses.len() > 16
            || addresses
                .iter()
                .any(|address| !self.allowed_address(*address))
        {
            return Err(invalid());
        }
        let builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.request_timeout)
            .resolve_to_addrs(host, &addresses);
        #[cfg(test)]
        let builder = if let Some((_, certificate)) = &self.fixture {
            builder.add_root_certificate(certificate.clone())
        } else {
            builder
        };
        let client = builder.build().map_err(McpOAuthError::from)?;
        let mut response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(CimdRetrievalError::Http)?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if response.status() != reqwest::StatusCode::OK
            || content_type != "application/json"
            || response
                .content_length()
                .is_some_and(|length| length > MAX_DOCUMENT_BYTES as u64)
        {
            return Err(invalid());
        }
        let ttl = cache_duration(response.headers(), self.cache_ttl);
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(CimdRetrievalError::Http)? {
            if bytes.len() + chunk.len() > MAX_DOCUMENT_BYTES {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        let registered = validate_document(client_id, &bytes)?;
        let cache_allowed = {
            let now = Instant::now();
            let mut keys = self.keys.lock().await;
            keys.retain(|_, expires| *expires > now);
            if !ttl.is_zero() && (keys.contains_key(&key) || keys.len() < 256) {
                keys.insert(key.clone(), now + ttl);
                true
            } else {
                false
            }
        };
        if cache_allowed {
            storage
                .store_kv(&key, &bytes, Some(ttl))
                .await
                .map_err(McpOAuthError::from)?;
        }
        Ok(registered)
    }

    fn allowed_address(&self, address: SocketAddr) -> bool {
        #[cfg(test)]
        if self
            .fixture
            .as_ref()
            .is_some_and(|fixture| fixture.0 == address)
        {
            return true;
        }
        public_address(address.ip())
    }
}

fn cache_duration(headers: &reqwest::header::HeaderMap, maximum: Duration) -> Duration {
    let mut ttl = maximum;
    for value in headers.get_all(reqwest::header::CACHE_CONTROL) {
        let Ok(value) = value.to_str() else {
            return Duration::ZERO;
        };
        for directive in value.split(',').map(str::trim) {
            if directive.eq_ignore_ascii_case("no-store")
                || directive.eq_ignore_ascii_case("no-cache")
            {
                return Duration::ZERO;
            }
            if let Some((name, value)) = directive.split_once('=') {
                if name.trim().eq_ignore_ascii_case("max-age") {
                    let Ok(seconds) = value.trim().trim_matches('"').parse::<u64>() else {
                        return Duration::ZERO;
                    };
                    ttl = ttl.min(Duration::from_secs(seconds));
                }
            }
        }
    }
    if let Some(expires) = headers.get(reqwest::header::EXPIRES) {
        let Some(expires) = expires
            .to_str()
            .ok()
            .and_then(|value| chrono::DateTime::parse_from_rfc2822(value).ok())
        else {
            return Duration::ZERO;
        };
        let date = headers
            .get(reqwest::header::DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| chrono::DateTime::parse_from_rfc2822(value).ok())
            .map(|date| date.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        ttl = ttl.min(
            (expires.with_timezone(&chrono::Utc) - date)
                .to_std()
                .unwrap_or(Duration::ZERO),
        );
    }
    let age = headers
        .get(reqwest::header::AGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    ttl.saturating_sub(Duration::from_secs(age))
}

fn validate_document(client_id: &str, bytes: &[u8]) -> Result<RegisteredClient, McpOAuthError> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid());
    }
    let mut document: Document = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if document.client_name.trim().is_empty()
        || document.client_id != client_id
        || document.metadata.redirect_uris.as_ref().is_none_or(|uris| {
            uris.is_empty()
                || uris.len() > 10
                || uris.iter().any(|uri| !super::dcr::valid_redirect(uri))
        })
    {
        return Err(invalid());
    }
    let metadata = &mut document.metadata;
    if metadata
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|method| method != "none")
        || metadata
            .scope
            .as_deref()
            .is_some_and(|scope| scope != OAUTH_SCOPE)
    {
        return Err(invalid());
    }
    // Public clients must not publish credentials or private key material.
    if metadata.additional_metadata.contains_key("client_secret")
        || metadata
            .additional_metadata
            .contains_key("client_secret_expires_at")
        || metadata.additional_metadata.contains_key("jwks")
        || metadata.additional_metadata.contains_key("jwks_uri")
    {
        return Err(invalid());
    }
    let grants = metadata
        .grant_types
        .get_or_insert_with(|| vec!["authorization_code".into()]);
    if !grants.iter().any(|grant| grant == "authorization_code")
        || grants
            .iter()
            .any(|grant| grant != "authorization_code" && grant != "refresh_token")
    {
        return Err(invalid());
    }
    let responses = metadata
        .response_types
        .get_or_insert_with(|| vec!["code".into()]);
    if responses.is_empty() || responses.iter().any(|response| response != "code") {
        return Err(invalid());
    }
    metadata.token_endpoint_auth_method = Some("none".into());
    metadata.scope = Some(OAUTH_SCOPE.into());
    let now = chrono::Utc::now();
    Ok(RegisteredClient {
        client_id: client_id.into(),
        client_secret_hash: None,
        registration_access_token_hash: String::new(),
        metadata: document.metadata,
        registered_at: now,
        updated_at: now,
        client_secret_expires_at: None,
        is_active: true,
    })
}

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Permit native global unicast only; exclude mapped IPv4, special-use and documentation.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] < 0x200 || segments[1] == 0xdb8))
                && !(segments[0] == 0x2002)
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_special_use_and_private_targets() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::1",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "allowed {ip}");
        }
        for ip in ["8.8.8.8", "2606:4700:4700::1111"] {
            assert!(public_address(ip.parse().unwrap()));
        }
    }
    #[test]
    fn metadata_binds_identity_and_redirects() {
        let id = "https://example.com/client.json";
        let valid = serde_json::json!({"client_id": id, "client_name": "Test client", "redirect_uris": ["http://127.0.0.1:1234/callback"]});
        assert!(validate_document(id, &serde_json::to_vec(&valid).unwrap()).is_ok());
        for field in [
            "client_secret",
            "client_secret_expires_at",
            "jwks",
            "jwks_uri",
        ] {
            let mut credential_metadata = valid.clone();
            credential_metadata[field] = serde_json::json!("forbidden");
            assert!(
                validate_document(id, &serde_json::to_vec(&credential_metadata).unwrap()).is_err(),
                "accepted {field}"
            );
        }
        assert!(validate_document(
            "https://other.example/client",
            &serde_json::to_vec(&valid).unwrap()
        )
        .is_err());
        for uri in [
            "http://10.0.0.1/callback",
            "https://example.com/cb#fragment",
            "https://user:password@example.com/cb",
        ] {
            let invalid = serde_json::json!({"client_id": id, "client_name": "Test client", "redirect_uris": [uri]});
            assert!(validate_document(id, &serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
}
