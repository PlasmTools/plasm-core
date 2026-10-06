//! OAuth 2.0 authorization-code + PKCE helpers for third-party "connect account" flows.
//!
//! Building block for a brokered credential model: user opens an authorize URL, returns with a
//! `code`, then [`exchange_authorization_code`] trades the code (+ PKCE verifier) for tokens.
//!
//! Uses [`oauth2`] with async [`reqwest::Client`]. Use a redirect policy of **no** follows on the
//! HTTP client for token requests (see `oauth2` crate security notes on SSRF).
//!
//! **Refresh tokens:** Google (`accounts.google.com`) authorize URLs get `access_type=offline` and
//! `prompt=consent` so the token response can include a `refresh_token`. Other IdPs use different
//! rules (e.g. Microsoft Entra expects the `offline_access` **scope**, not extra query parameters).

use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use reqwest::redirect::Policy;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;
use url::Url;

/// Serializable start state: redirect the resource owner to [`Self::authorize_url`].
#[derive(Debug, Clone)]
pub struct OAuthAuthorizationStart {
    /// Full authorization URL (includes `state` and PKCE challenge).
    pub authorize_url: String,
    /// CSRF `state` returned by the provider; must match the callback.
    pub csrf_state: String,
    /// PKCE verifier; store server-side until the callback exchanges the code.
    pub pkce_verifier: String,
}

#[derive(Debug, Error)]
pub enum OAuthConnectError {
    #[error("invalid OAuth URL: {0}")]
    InvalidUrl(#[source] url::ParseError),
    #[error("OAuth token exchange failed: {0}")]
    TokenExchange(#[source] OAuthEndpointError),
    #[error("OAuth device authorization failed: {0}")]
    DeviceAuthorization(#[source] OAuthEndpointError),
}

/// Concrete endpoint failures. Formatting deliberately excludes URLs, response bodies,
/// and provider descriptions; callers may inspect the source explicitly when needed.
#[derive(Error)]
pub enum OAuthEndpointError {
    #[error("HTTP client construction failed")]
    ClientBuild(#[source] reqwest::Error),
    #[error("HTTP request failed")]
    Request(#[source] reqwest::Error),
    #[error("HTTP {status}: response body read failed")]
    ResponseBody {
        status: reqwest::StatusCode,
        #[source]
        source: reqwest::Error,
    },
    #[error("HTTP {status}: invalid JSON response")]
    Json {
        status: reqwest::StatusCode,
        #[source]
        source: serde_json::Error,
    },
    #[error("OAuth2 token request failed")]
    OAuth2(
        #[source]
        oauth2::RequestTokenError<
            oauth2::HttpClientError<reqwest::Error>,
            oauth2::basic::BasicErrorResponse,
        >,
    ),
    #[error("HTTP {status}: OAuth endpoint rejected the request")]
    Protocol {
        status: reqwest::StatusCode,
        error: Option<String>,
        error_description: Option<String>,
        error_uri: Option<String>,
    },
    #[error("missing string field {field}")]
    MissingString { field: &'static str },
    #[error("empty string field {field}")]
    EmptyString { field: &'static str },
    #[error("token response missing access_token")]
    MissingAccessToken,
}

impl std::fmt::Debug for OAuthEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

async fn endpoint_response_json(
    response: reqwest::Response,
) -> Result<(reqwest::StatusCode, Value), OAuthEndpointError> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|source| OAuthEndpointError::ResponseBody { status, source })?;
    let body = serde_json::from_slice(&bytes)
        .map_err(|source| OAuthEndpointError::Json { status, source })?;
    Ok((status, body))
}

fn endpoint_protocol_error(status: reqwest::StatusCode, body: &Value) -> OAuthEndpointError {
    let string = |key| body.get(key).and_then(Value::as_str).map(str::to_owned);
    OAuthEndpointError::Protocol {
        status,
        error: string("error"),
        error_description: string("error_description"),
        error_uri: string("error_uri"),
    }
}

/// Build an OAuth2 Basic client and produce an authorization URL with PKCE (S256).
pub fn begin_authorization_code_pkce(
    client_id: &str,
    client_secret: Option<&str>,
    auth_url: &str,
    token_url: &str,
    redirect_uri: &str,
    scopes: &[String],
) -> Result<OAuthAuthorizationStart, OAuthConnectError> {
    let auth = AuthUrl::new(auth_url.to_string()).map_err(OAuthConnectError::InvalidUrl)?;
    let token = TokenUrl::new(token_url.to_string()).map_err(OAuthConnectError::InvalidUrl)?;
    let redirect =
        RedirectUrl::new(redirect_uri.to_string()).map_err(OAuthConnectError::InvalidUrl)?;

    let mut client = BasicClient::new(ClientId::new(client_id.to_string()))
        .set_auth_uri(auth)
        .set_token_uri(token)
        .set_redirect_uri(redirect);

    if let Some(secret) = client_secret {
        client = client.set_client_secret(ClientSecret::new(secret.to_string()));
    }

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let mut req = client.authorize_url(CsrfToken::new_random);
    for s in scopes {
        req = req.add_scope(Scope::new(s.clone()));
    }
    let (url, csrf) = req.set_pkce_challenge(pkce_challenge).url();
    let url = apply_google_refresh_token_authorize_params(url);

    Ok(OAuthAuthorizationStart {
        authorize_url: url.to_string(),
        csrf_state: csrf.secret().to_string(),
        pkce_verifier: pkce_verifier.secret().to_string(),
    })
}

/// Google OAuth web server flow: request a refresh token (`access_type=offline`) and show the
/// consent screen on re-link so a new `refresh_token` is issued (`prompt=consent`).
///
/// Without these, Google often omits `refresh_token` from the token response; outbound KV then
/// cannot refresh expired access tokens. See Google Identity: "Using OAuth 2.0 for Web Server Apps".
fn apply_google_refresh_token_authorize_params(url: Url) -> Url {
    if url.host_str() != Some("accounts.google.com") {
        return url;
    }
    let mut url = url;
    ensure_oauth_query_param(&mut url, "access_type", "offline");
    ensure_oauth_query_param(&mut url, "prompt", "consent");
    url
}

fn ensure_oauth_query_param(url: &mut Url, key: &str, value: &str) {
    if url.query_pairs().any(|(k, _)| k == key) {
        return;
    }
    url.query_pairs_mut().append_pair(key, value);
}

fn oauth_reqwest_client() -> Result<reqwest::Client, OAuthConnectError> {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|e| OAuthConnectError::TokenExchange(OAuthEndpointError::ClientBuild(e)))
}

/// Exchange an authorization code for tokens (async). `pkce_verifier` must be the secret saved from
/// [`begin_authorization_code_pkce`].
pub async fn exchange_authorization_code(
    client_id: &str,
    client_secret: Option<&str>,
    auth_url: &str,
    token_url: &str,
    redirect_uri: &str,
    code: &str,
    pkce_verifier: &str,
) -> Result<String, OAuthConnectError> {
    let auth = AuthUrl::new(auth_url.to_string()).map_err(OAuthConnectError::InvalidUrl)?;
    let token = TokenUrl::new(token_url.to_string()).map_err(OAuthConnectError::InvalidUrl)?;
    let redirect =
        RedirectUrl::new(redirect_uri.to_string()).map_err(OAuthConnectError::InvalidUrl)?;

    let mut client = BasicClient::new(ClientId::new(client_id.to_string()))
        .set_auth_uri(auth)
        .set_token_uri(token)
        .set_redirect_uri(redirect);

    if let Some(secret) = client_secret {
        client = client.set_client_secret(ClientSecret::new(secret.to_string()));
    }

    let verifier = PkceCodeVerifier::new(pkce_verifier.to_string());
    let http = oauth_reqwest_client()?;

    let token = client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .set_pkce_verifier(verifier)
        .request_async(&http)
        .await
        .map_err(|e| OAuthConnectError::TokenExchange(OAuthEndpointError::OAuth2(e)))?;

    Ok(token.access_token().secret().to_string())
}

/// RFC 8628 device authorization response (fields used by Plasm outbound device flows).
#[derive(Debug, Clone)]
pub struct OAuthDeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: Option<u64>,
}

/// Single token-endpoint poll for the device code grant ([RFC 8628 §3.4](https://www.rfc-editor.org/rfc/rfc8628#section-3.4)).
#[derive(Debug, Clone)]
pub enum OAuthDeviceTokenPoll {
    Success(Value),
    AuthorizationPending,
    SlowDown {
        interval_secs: u64,
    },
    OAuthError {
        error: String,
        error_description: Option<String>,
    },
}

/// `POST` the device authorization endpoint (`application/x-www-form-urlencoded`).
pub async fn request_oauth_device_authorization(
    http: &reqwest::Client,
    device_authorization_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    scopes: &[String],
    timeout: Duration,
) -> Result<OAuthDeviceAuthorizationResponse, OAuthConnectError> {
    let mut form = HashMap::new();
    form.insert("client_id".to_string(), client_id.to_string());
    if let Some(sec) = client_secret {
        form.insert("client_secret".to_string(), sec.to_string());
    }
    if !scopes.is_empty() {
        form.insert("scope".to_string(), scopes.join(" "));
    }

    let response = http
        .post(device_authorization_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| OAuthConnectError::DeviceAuthorization(OAuthEndpointError::Request(e)))?;

    let (status, body) = endpoint_response_json(response)
        .await
        .map_err(OAuthConnectError::DeviceAuthorization)?;

    if !status.is_success() {
        return Err(OAuthConnectError::DeviceAuthorization(
            endpoint_protocol_error(status, &body),
        ));
    }

    let device_code = json_required_string(&body, "device_code")?;
    let user_code = json_required_string(&body, "user_code")?;
    let verification_uri = json_required_string(&body, "verification_uri")?;
    let verification_uri_complete = body
        .get("verification_uri_complete")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            body.get("expires_in")
                .and_then(|v| v.as_i64())
                .map(|v| v.max(0) as u64)
        })
        .unwrap_or(1800);
    let interval = body.get("interval").and_then(|v| v.as_u64()).or_else(|| {
        body.get("interval")
            .and_then(|v| v.as_i64())
            .map(|v| v.max(0) as u64)
    });

    Ok(OAuthDeviceAuthorizationResponse {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in,
        interval,
    })
}

fn json_required_string(body: &Value, key: &'static str) -> Result<String, OAuthConnectError> {
    let Some(v) = body.get(key).and_then(|v| v.as_str()) else {
        return Err(OAuthConnectError::DeviceAuthorization(
            OAuthEndpointError::MissingString { field: key },
        ));
    };
    let t = v.trim();
    if t.is_empty() {
        return Err(OAuthConnectError::DeviceAuthorization(
            OAuthEndpointError::EmptyString { field: key },
        ));
    }
    Ok(t.to_string())
}

/// Poll the token endpoint once using `grant_type=urn:ietf:params:oauth:grant-type:device_code`.
pub async fn poll_oauth_device_token_once(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    device_code: &str,
    timeout: Duration,
) -> Result<OAuthDeviceTokenPoll, OAuthConnectError> {
    let mut form = HashMap::new();
    form.insert(
        "grant_type".to_string(),
        "urn:ietf:params:oauth:grant-type:device_code".to_string(),
    );
    form.insert("device_code".to_string(), device_code.to_string());
    form.insert("client_id".to_string(), client_id.to_string());
    if let Some(sec) = client_secret {
        form.insert("client_secret".to_string(), sec.to_string());
    }

    let response = http
        .post(token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| OAuthConnectError::TokenExchange(OAuthEndpointError::Request(e)))?;

    let (status, body) = endpoint_response_json(response)
        .await
        .map_err(OAuthConnectError::TokenExchange)?;

    if status.is_success() && body.get("access_token").is_some() {
        return Ok(OAuthDeviceTokenPoll::Success(body));
    }

    let err_code = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    match err_code {
        "" if status.is_success() => Err(OAuthConnectError::TokenExchange(
            OAuthEndpointError::MissingAccessToken,
        )),
        "authorization_pending" => Ok(OAuthDeviceTokenPoll::AuthorizationPending),
        "slow_down" => {
            let interval_secs = body
                .get("interval")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    body.get("interval")
                        .and_then(|v| v.as_i64())
                        .map(|i| i.max(1) as u64)
                })
                .unwrap_or(5);
            Ok(OAuthDeviceTokenPoll::SlowDown { interval_secs })
        }
        other if !other.is_empty() => {
            let desc = body
                .get("error_description")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Ok(OAuthDeviceTokenPoll::OAuthError {
                error: other.to_string(),
                error_description: desc,
            })
        }
        _ => Err(OAuthConnectError::TokenExchange(endpoint_protocol_error(
            status, &body,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use url::Url;

    #[test]
    fn invalid_url_preserves_parse_source() {
        let error = begin_authorization_code_pkce(
            "cid",
            None,
            "not a URL",
            "https://example.com/token",
            "https://example.com/callback",
            &[],
        )
        .unwrap_err();
        assert!(matches!(error, OAuthConnectError::InvalidUrl(_)));
        assert!(error.source().unwrap().is::<url::ParseError>());
    }

    #[test]
    fn endpoint_errors_preserve_concrete_sources() {
        let request = reqwest::Client::new().get("not a URL").build().unwrap_err();
        let error = OAuthConnectError::TokenExchange(OAuthEndpointError::Request(request));
        let endpoint = error.source().unwrap();
        assert!(endpoint.is::<OAuthEndpointError>());
        assert!(endpoint.source().unwrap().is::<reqwest::Error>());

        let source = serde_json::from_str::<Value>("{").unwrap_err();
        let error = OAuthConnectError::DeviceAuthorization(OAuthEndpointError::Json {
            status: reqwest::StatusCode::BAD_GATEWAY,
            source,
        });
        assert!(error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .is::<serde_json::Error>());
    }

    #[test]
    fn oauth2_source_retains_provider_metadata_without_formatting_it() {
        let response = oauth2::basic::BasicErrorResponse::new(
            oauth2::basic::BasicErrorResponseType::InvalidGrant,
            Some("sensitive-provider-description".into()),
            Some("https://example.com/error?secret=sensitive".into()),
        );
        let error = OAuthConnectError::TokenExchange(OAuthEndpointError::OAuth2(
            oauth2::RequestTokenError::ServerResponse(response),
        ));
        let source = error.source().unwrap().source().unwrap();
        type TokenError = oauth2::RequestTokenError<
            oauth2::HttpClientError<reqwest::Error>,
            oauth2::basic::BasicErrorResponse,
        >;
        let Some(TokenError::ServerResponse(response)) = source.downcast_ref::<TokenError>() else {
            panic!("expected the original OAuth2 source");
        };
        assert_eq!(
            response.error_description().unwrap(),
            "sensitive-provider-description"
        );
        assert!(!error.to_string().contains("sensitive"));
        assert!(!format!("{error:?}").contains("sensitive"));
    }

    #[test]
    fn protocol_metadata_is_retained_but_redacted_from_diagnostics() {
        let body = serde_json::json!({
            "error": "invalid_client",
            "error_description": "sensitive-description",
            "error_uri": "https://example.com/?secret=sensitive",
            "access_token": "sensitive-token",
        });
        let error = endpoint_protocol_error(reqwest::StatusCode::UNAUTHORIZED, &body);
        assert!(!error.to_string().contains("sensitive"));
        assert!(!format!("{error:?}").contains("sensitive"));
        let OAuthEndpointError::Protocol {
            status,
            error,
            error_description,
            error_uri,
        } = error
        else {
            panic!("expected protocol metadata");
        };
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(error.as_deref(), Some("invalid_client"));
        assert_eq!(error_description.as_deref(), Some("sensitive-description"));
        assert_eq!(
            error_uri.as_deref(),
            Some("https://example.com/?secret=sensitive")
        );
    }

    #[test]
    fn required_device_fields_have_semantic_errors() {
        let missing = json_required_string(&serde_json::json!({}), "device_code").unwrap_err();
        assert!(matches!(
            missing,
            OAuthConnectError::DeviceAuthorization(OAuthEndpointError::MissingString {
                field: "device_code"
            })
        ));
        let empty = json_required_string(&serde_json::json!({"device_code": "  "}), "device_code")
            .unwrap_err();
        assert!(matches!(
            empty,
            OAuthConnectError::DeviceAuthorization(OAuthEndpointError::EmptyString {
                field: "device_code"
            })
        ));
    }

    #[tokio::test]
    async fn poll_device_token_authorization_pending() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.expect("read");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains("device_code"));
            let body = r#"{"error":"authorization_pending"}"#;
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
        });
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let token_url = format!("http://{}", addr);
        let out = poll_oauth_device_token_once(
            &http,
            &token_url,
            "cid",
            Some("sec"),
            "dc",
            Duration::from_secs(5),
        )
        .await
        .expect("poll");
        assert!(matches!(out, OAuthDeviceTokenPoll::AuthorizationPending));
    }

    #[test]
    fn begin_pkce_produces_https_urls() {
        let start = begin_authorization_code_pkce(
            "cid",
            Some("sec"),
            "https://example.com/oauth/authorize",
            "https://example.com/oauth/token",
            "https://app.example/callback",
            &["read".into()],
        )
        .expect("begin");
        assert!(start.authorize_url.contains("code_challenge="));
        assert!(start.authorize_url.contains("state="));
        assert!(!start.pkce_verifier.is_empty());
    }

    #[test]
    fn begin_pkce_google_adds_offline_and_consent() {
        let start = begin_authorization_code_pkce(
            "cid",
            Some("sec"),
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://app.example/callback",
            &["https://www.googleapis.com/auth/calendar.readonly".into()],
        )
        .expect("begin");
        let u = Url::parse(&start.authorize_url).expect("parse authorize_url");
        let q: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(
            q.iter().any(|(k, v)| k == "access_type" && v == "offline"),
            "expected access_type=offline in {:?}",
            q
        );
        assert!(
            q.iter().any(|(k, v)| k == "prompt" && v == "consent"),
            "expected prompt=consent in {:?}",
            q
        );
    }

    #[test]
    fn begin_pkce_non_google_does_not_inject_google_params() {
        let start = begin_authorization_code_pkce(
            "cid",
            Some("sec"),
            "https://example.com/oauth/authorize",
            "https://example.com/oauth/token",
            "https://app.example/callback",
            &["read".into()],
        )
        .expect("begin");
        assert!(
            !start.authorize_url.contains("access_type="),
            "non-Google IdP should not get access_type"
        );
        assert!(
            !start.authorize_url.contains("prompt=consent"),
            "non-Google IdP should not get prompt=consent"
        );
    }
}
