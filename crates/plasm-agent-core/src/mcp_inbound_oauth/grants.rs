use std::collections::HashMap;
use std::time::Duration;

use auth_framework::oauth2_enhanced_storage::EnhancedAuthorizationCode;

use crate::server_state::PlasmHostState;

use super::client::{grant_type_allowed, load_registered_client, redirect_uri_allowed};
use super::error::McpOAuthError;
use super::jwt::OAUTH_ACCESS_TOKEN_TTL_SECS;
use super::pkce::validate_pkce_s256;
use super::principal::verify_authorization_principal;
use super::resource::resolve_resource_param;
use super::session_store::{OAuthSessionStore, PlasmOAuthAuthCode, OAUTH_AUTH_CODE_TTL_SECS};
use super::types::{AuthorizeOutcome, McpOAuthTokenRequest, McpOAuthTokenResponse, OAUTH_SCOPE};
use super::McpInboundOAuthService;

impl McpInboundOAuthService {
    pub async fn authorize(
        &self,
        plasm: &PlasmHostState,
        params: &HashMap<String, String>,
    ) -> Result<AuthorizeOutcome, McpOAuthError> {
        let response_type = params
            .get("response_type")
            .map(String::as_str)
            .unwrap_or("");
        let client_id = params.get("client_id").map(String::as_str).unwrap_or("");
        let redirect_uri = params.get("redirect_uri").map(String::as_str).unwrap_or("");
        let state = params.get("state").map(String::as_str);
        let scope = params
            .get("scope")
            .cloned()
            .unwrap_or_else(|| OAUTH_SCOPE.to_string());
        let code_challenge = params
            .get("code_challenge")
            .map(String::as_str)
            .unwrap_or("");
        let code_challenge_method = params
            .get("code_challenge_method")
            .map(String::as_str)
            .unwrap_or("S256");

        if response_type != "code" || client_id.is_empty() || redirect_uri.is_empty() {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "response_type=code, client_id, and redirect_uri are required",
            ));
        }
        if code_challenge.is_empty() || code_challenge_method != "S256" {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "PKCE S256 is required (code_challenge + code_challenge_method=S256)",
            ));
        }

        let resource = resolve_resource_param(
            &self.canonical_resource,
            params.get("resource").map(String::as_str),
        )?;

        let client = load_registered_client(self.storage.as_ref(), client_id).await?;
        if !redirect_uri_allowed(&client, redirect_uri) {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "redirect_uri is not registered for this client",
            ));
        }

        let principal_token = params
            .get("principal_token")
            .map(String::as_str)
            .unwrap_or("")
            .trim();
        if principal_token.is_empty() {
            return Ok(AuthorizeOutcome::AwaitingPrincipal);
        }

        let principal = verify_authorization_principal(plasm, principal_token).await?;
        let scopes: Vec<String> = scope.split_whitespace().map(str::to_string).collect();
        let auth_code = EnhancedAuthorizationCode::new(
            client_id.to_string(),
            principal.subject.clone(),
            redirect_uri.to_string(),
            scopes,
            Some(code_challenge.to_string()),
            Some(code_challenge_method.to_string()),
            Duration::from_secs(OAUTH_AUTH_CODE_TTL_SECS),
        );
        let code = auth_code.code.clone();
        OAuthSessionStore::new(self.storage.as_ref())
            .store_auth_code(&PlasmOAuthAuthCode {
                enhanced: auth_code,
                tenant_id: principal.tenant_id,
                resource,
            })
            .await?;

        Ok(AuthorizeOutcome::Redirect {
            location: build_redirect_with_code(redirect_uri, &code, state)?,
        })
    }

    pub async fn exchange_token(
        &self,
        form: &McpOAuthTokenRequest,
    ) -> Result<McpOAuthTokenResponse, McpOAuthError> {
        let grant_type = form
            .grant_type
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match grant_type.as_str() {
            "authorization_code" => self.exchange_authorization_code(form).await,
            "refresh_token" => self.exchange_refresh_token(form).await,
            _ => Err(McpOAuthError::bad_request(
                "unsupported_grant_type",
                "supported grant types are authorization_code and refresh_token",
            )),
        }
    }

    async fn exchange_authorization_code(
        &self,
        form: &McpOAuthTokenRequest,
    ) -> Result<McpOAuthTokenResponse, McpOAuthError> {
        let client_id = form.client_id.as_deref().unwrap_or("").trim();
        let code = form.code.as_deref().unwrap_or("").trim();
        let redirect_uri = form.redirect_uri.as_deref().unwrap_or("").trim();
        let code_verifier = form.code_verifier.as_deref().unwrap_or("").trim();
        if client_id.is_empty()
            || code.is_empty()
            || redirect_uri.is_empty()
            || code_verifier.is_empty()
        {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "client_id, code, redirect_uri, and code_verifier are required",
            ));
        }

        let resource = resolve_resource_param(&self.canonical_resource, form.resource.as_deref())?;

        let client = load_registered_client(self.storage.as_ref(), client_id).await?;
        if !redirect_uri_allowed(&client, redirect_uri) {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "redirect_uri mismatch",
            ));
        }

        let sessions = OAuthSessionStore::new(self.storage.as_ref());
        let stored = sessions.peek_auth_code(code).await?;
        Self::validate_auth_code_for_exchange(
            &stored,
            client_id,
            redirect_uri,
            &resource,
            code_verifier,
        )?;
        sessions.delete_auth_code(code).await?;

        let scope = stored.enhanced.scopes.join(" ");
        self.issue_token_response(
            client_id,
            &stored.tenant_id,
            &stored.enhanced.user_id,
            &scope,
            &stored.resource,
            grant_type_allowed(&client, "refresh_token"),
        )
        .await
    }

    async fn exchange_refresh_token(
        &self,
        form: &McpOAuthTokenRequest,
    ) -> Result<McpOAuthTokenResponse, McpOAuthError> {
        let client_id = form.client_id.as_deref().unwrap_or("").trim();
        let refresh_token = form.refresh_token.as_deref().unwrap_or("").trim();
        if client_id.is_empty() || refresh_token.is_empty() {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "client_id and refresh_token are required",
            ));
        }

        let resource = resolve_resource_param(&self.canonical_resource, form.resource.as_deref())?;

        let client = load_registered_client(self.storage.as_ref(), client_id).await?;
        if !grant_type_allowed(&client, "refresh_token") {
            return Err(McpOAuthError::bad_request(
                "unauthorized_client",
                "client is not allowed to use refresh_token grant",
            ));
        }

        let sessions = OAuthSessionStore::new(self.storage.as_ref());
        let stored = sessions.load_refresh_token(refresh_token).await?;
        if stored.resource != resource {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "resource parameter mismatch",
            ));
        }
        if !stored.enhanced.is_valid() {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "refresh token is expired",
            ));
        }
        if stored.enhanced.client_id != client_id {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "refresh token client mismatch",
            ));
        }

        let scope = stored.enhanced.scopes.join(" ");
        let response = self
            .issue_token_response(
                client_id,
                &stored.tenant_id,
                &stored.enhanced.user_id,
                &scope,
                &stored.resource,
                true,
            )
            .await?;
        sessions.consume_refresh_token(refresh_token).await?;
        Ok(response)
    }

    fn validate_auth_code_for_exchange(
        stored: &PlasmOAuthAuthCode,
        client_id: &str,
        redirect_uri: &str,
        resource: &str,
        code_verifier: &str,
    ) -> Result<(), McpOAuthError> {
        if stored.enhanced.client_id != client_id || stored.enhanced.redirect_uri != redirect_uri {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "authorization code mismatch",
            ));
        }
        if stored.resource != resource {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "resource parameter mismatch",
            ));
        }
        if !stored.enhanced.is_valid() {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "authorization code expired",
            ));
        }
        if stored.enhanced.code_challenge_method.as_deref() != Some("S256")
            || !validate_pkce_s256(
                stored.enhanced.code_challenge.as_deref().unwrap_or(""),
                code_verifier,
            )
        {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "PKCE verifier is invalid",
            ));
        }
        Ok(())
    }

    async fn issue_token_response(
        &self,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        scope: &str,
        resource: &str,
        with_refresh: bool,
    ) -> Result<McpOAuthTokenResponse, McpOAuthError> {
        let access_token = self
            .jwt
            .mint_access_token(client_id, tenant_id, subject, scope, resource)?;
        let refresh_token = if with_refresh {
            Some(
                OAuthSessionStore::new(self.storage.as_ref())
                    .mint_and_store_refresh_token(client_id, tenant_id, subject, scope, resource)
                    .await?,
            )
        } else {
            None
        };
        Ok(McpOAuthTokenResponse {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: OAUTH_ACCESS_TOKEN_TTL_SECS,
            scope: scope.to_string(),
            refresh_token,
        })
    }
}

fn build_redirect_with_code(
    redirect_uri: &str,
    code: &str,
    state: Option<&str>,
) -> Result<String, McpOAuthError> {
    let mut parsed = reqwest::Url::parse(redirect_uri)
        .map_err(|_| McpOAuthError::bad_request("invalid_request", "redirect_uri is invalid"))?;
    {
        let mut q = parsed.query_pairs_mut();
        q.append_pair("code", code);
        if let Some(s) = state {
            if !s.is_empty() {
                q.append_pair("state", s);
            }
        }
    }
    Ok(parsed.to_string())
}
