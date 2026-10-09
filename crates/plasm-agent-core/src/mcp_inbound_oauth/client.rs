use super::records::RegisteredClient;
use crate::secret_store::SecretStore;

use super::error::McpOAuthError;

pub fn registration_key(client_id: &str) -> String {
    format!("client_registration:{client_id}")
}

pub async fn load_registered_client(
    storage: &dyn SecretStore,
    client_id: &str,
) -> Result<RegisteredClient, McpOAuthError> {
    if url::Url::parse(client_id).is_ok_and(|url| url.scheme() == "https") {
        return super::metadata_client::load(storage, client_id).await;
    }
    let key = registration_key(client_id);
    let bytes = storage
        .get_kv(&key)
        .await
        .map_err(McpOAuthError::from)?
        .ok_or_else(|| McpOAuthError::bad_request("invalid_client", "unknown client_id"))?;
    let client: RegisteredClient = serde_json::from_slice(&bytes).map_err(McpOAuthError::from)?;
    if !client.is_active {
        return Err(McpOAuthError::bad_request(
            "invalid_client",
            "client registration is inactive",
        ));
    }
    Ok(client)
}

pub fn redirect_uri_allowed(client: &RegisteredClient, redirect_uri: &str) -> bool {
    client.metadata.redirect_uris.as_ref().is_some_and(|uris| {
        uris.iter().any(|uri| {
            uri == redirect_uri
                || loopback_endpoint(uri)
                    .zip(loopback_endpoint(redirect_uri))
                    .is_some_and(|(registered, requested)| registered == requested)
        })
    })
}

/// Ignore only the authority's port for native HTTP loopback IP redirects.
/// Compare the remaining wire components without URL normalization.
fn loopback_endpoint(uri: &str) -> Option<(&str, &str)> {
    if uri.contains('\\') || uri.bytes().any(|byte| byte.is_ascii_control()) {
        return None;
    }
    let url = url::Url::parse(uri).ok()?;
    let loopback = match url.host()? {
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
        url::Host::Domain(_) => false,
    };
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let start = uri.find("://")? + 3;
    let end = uri[start..]
        .find(['/', '?', '#'])
        .map_or(uri.len(), |offset| start + offset);
    let authority = &uri[start..end];
    let host_end = if authority.starts_with('[') {
        authority.find(']')? + 1
    } else {
        authority.find(':').unwrap_or(authority.len())
    };
    Some((&uri[..start + host_end], &uri[end..]))
}

pub fn authorization_redirect(
    client: &RegisteredClient,
    requested: Option<&str>,
) -> Result<String, McpOAuthError> {
    match requested.filter(|value| !value.is_empty()) {
        Some(uri) if redirect_uri_allowed(client, uri) => Ok(uri.to_owned()),
        Some(_) => Err(McpOAuthError::bad_request(
            "invalid_request",
            "redirect_uri is not registered for this client",
        )),
        None => match client.metadata.redirect_uris.as_deref() {
            Some([uri]) => Ok(uri.clone()),
            _ => Err(McpOAuthError::bad_request(
                "invalid_request",
                "redirect_uri is required when the client has multiple registered redirects",
            )),
        },
    }
}

pub fn grant_type_allowed(client: &RegisteredClient, grant_type: &str) -> bool {
    client
        .metadata
        .grant_types
        .as_ref()
        .is_some_and(|grants| grants.iter().any(|g| g == grant_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_exception_changes_only_the_port() {
        for (registered, requested) in [
            (
                "http://127.0.0.1:1234/callback?x=1",
                "http://127.0.0.1:4567/callback?x=1",
            ),
            ("http://[::1]:1234/callback", "http://[::1]:4567/callback"),
            (
                "http://127.0.0.1/callback",
                "http://127.0.0.1:4567/callback",
            ),
        ] {
            assert_eq!(loopback_endpoint(registered), loopback_endpoint(requested));
        }
        let registered = loopback_endpoint("http://127.0.0.1:1234/callback?x=1");
        for requested in [
            "http://127.0.0.1:4567\\other/callback?x=1",
            "http://127.0.0.2:4567/callback?x=1",
            "http://127.0.0.1:4567/other?x=1",
            "http://127.0.0.1:4567/callback?x=2",
            "http://127.0.0.1:4567/call%62ack?x=1",
            "https://127.0.0.1:4567/callback?x=1",
            "http://localhost:4567/callback?x=1",
            "http://user@127.0.0.1:4567/callback?x=1",
            "http://127.0.0.1:4567/callback?x=1#fragment",
        ] {
            assert_ne!(
                registered,
                loopback_endpoint(requested),
                "accepted {requested}"
            );
        }
        for uri in [
            "http://localhost:1234/cb",
            "http://192.0.2.1:1234/cb",
            "https://[::1]:1234/cb",
        ] {
            assert!(loopback_endpoint(uri).is_none());
        }
    }
}
