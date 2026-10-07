use std::collections::HashMap;
use std::time::Duration;

use super::records::{AuthorizationCode, RegisteredClient};

use crate::server_state::PlasmHostState;

use super::client::{
    authorization_redirect, grant_type_allowed, load_registered_client, redirect_uri_allowed,
};
use super::error::McpOAuthError;
use super::jwt::OAUTH_ACCESS_TOKEN_TTL_SECS;
use super::pkce::validate_pkce_s256;
use super::principal::verify_authorization_principal;
use super::resource::resolve_resource_param;
use super::session_store::{OAuthSessionStore, PlasmOAuthAuthCode, OAUTH_AUTH_CODE_TTL_SECS};
use super::types::{
    AuthorizationRequest, AuthorizeOutcome, McpOAuthTokenRequest, McpOAuthTokenResponse,
    OAUTH_SCOPE,
};
use super::McpInboundOAuthService;

impl McpInboundOAuthService {
    pub async fn authorize(
        &self,
        plasm: &PlasmHostState,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizeOutcome, McpOAuthError> {
        if request.ambiguous_context {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "client_id and redirect_uri must be unambiguous",
            ));
        }
        let mut params = request.params.clone();
        let client_id = params.get("client_id").map(String::as_str).unwrap_or("");
        if client_id.is_empty() {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "client_id is required",
            ));
        }
        let client = load_registered_client(self.storage.as_ref(), client_id).await?;
        let redirect_uri =
            authorization_redirect(&client, params.get("redirect_uri").map(String::as_str))?;
        params.insert("redirect_uri".into(), redirect_uri.clone());
        let result = match &request.error {
            Some(error) => Err(error.clone()),
            None => self.authorize_validated(plasm, &params, &client).await,
        };
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => Ok(AuthorizeOutcome::Redirect {
                location: build_redirect_with_error(
                    &redirect_uri,
                    &error,
                    params.get("state").map(String::as_str),
                    &self.canonical_resource,
                )?,
            }),
        }
    }

    async fn authorize_validated(
        &self,
        plasm: &PlasmHostState,
        params: &HashMap<String, String>,
        client: &RegisteredClient,
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
            .filter(|scope| !scope.is_empty())
            .cloned()
            .unwrap_or_else(|| OAUTH_SCOPE.to_string());
        let code_challenge = params
            .get("code_challenge")
            .map(String::as_str)
            .unwrap_or("");
        if scope.split_whitespace().collect::<Vec<_>>() != [OAUTH_SCOPE] {
            return Err(McpOAuthError::bad_request(
                "invalid_scope",
                "only mcp:tools is supported",
            ));
        }
        let code_challenge_method = params
            .get("code_challenge_method")
            .map(String::as_str)
            .unwrap_or("");

        if response_type.is_empty() {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "response_type is required",
            ));
        }
        if response_type != "code" {
            return Err(McpOAuthError::bad_request(
                "unsupported_response_type",
                "only response_type=code is supported",
            ));
        }
        if code_challenge.len() != 43
            || !code_challenge
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            || code_challenge_method != "S256"
        {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "PKCE S256 is required (code_challenge + code_challenge_method=S256)",
            ));
        }

        let resource = resolve_resource_param(
            &self.canonical_resource,
            params.get("resource").map(String::as_str),
        )?;

        if !grant_type_allowed(client, "authorization_code") {
            return Err(McpOAuthError::bad_request(
                "unauthorized_client",
                "client is not allowed to use authorization_code grant",
            ));
        }

        let principal_token = params
            .get("principal_token")
            .map(String::as_str)
            .unwrap_or("")
            .trim();
        if principal_token.is_empty() {
            return Ok(AuthorizeOutcome::AwaitingPrincipal {
                params: params.clone(),
            });
        }

        let principal = verify_authorization_principal(plasm, principal_token).await?;
        let scopes: Vec<String> = scope.split_whitespace().map(str::to_string).collect();
        let auth_code = AuthorizationCode::new(
            client_id.to_string(),
            principal.subject.clone(),
            redirect_uri.to_string(),
            scopes,
            Some(code_challenge.to_string()),
            Some(code_challenge_method.to_string()),
            Duration::from_secs(OAUTH_AUTH_CODE_TTL_SECS),
        )
        .map_err(McpOAuthError::from)?;
        let code = auth_code.code.clone();
        OAuthSessionStore::new(self.storage.as_ref())
            .store_auth_code(&PlasmOAuthAuthCode {
                enhanced: auth_code,
                tenant_id: principal.tenant_id,
                resource,
            })
            .await?;

        Ok(AuthorizeOutcome::Redirect {
            location: build_redirect_with_code(
                redirect_uri,
                &code,
                state,
                &self.canonical_resource,
            )?,
        })
    }

    pub async fn exchange_token(
        &self,
        form: &McpOAuthTokenRequest,
    ) -> Result<McpOAuthTokenResponse, McpOAuthError> {
        let grant_type = form.grant_type.as_deref().unwrap_or("");
        match grant_type {
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
        let client_id = form.client_id.as_deref().unwrap_or("");
        let code = form.code.as_deref().unwrap_or("");
        let requested_redirect = form.redirect_uri.as_deref().filter(|uri| !uri.is_empty());
        let code_verifier = form.code_verifier.as_deref().unwrap_or("");
        if client_id.is_empty() || code.is_empty() || code_verifier.is_empty() {
            return Err(McpOAuthError::bad_request(
                "invalid_request",
                "client_id, code, and code_verifier are required",
            ));
        }

        let resource = resolve_resource_param(&self.canonical_resource, form.resource.as_deref())?;

        let client = load_registered_client(self.storage.as_ref(), client_id).await?;
        if !grant_type_allowed(&client, "authorization_code") {
            return Err(McpOAuthError::bad_request(
                "unauthorized_client",
                "client is not allowed to use authorization_code grant",
            ));
        }
        if requested_redirect.is_some_and(|uri| !redirect_uri_allowed(&client, uri)) {
            return Err(McpOAuthError::bad_request(
                "invalid_grant",
                "redirect_uri mismatch",
            ));
        }

        let sessions = OAuthSessionStore::new(self.storage.as_ref());
        let (stored, expected) = sessions.peek_auth_code(code).await?;
        let redirect_uri = requested_redirect.unwrap_or(&stored.enhanced.redirect_uri);
        Self::validate_auth_code_for_exchange(
            &stored,
            client_id,
            redirect_uri,
            &resource,
            code_verifier,
        )?;
        sessions.consume_auth_code(code, &expected).await?;

        let scope = stored.enhanced.scopes.join(" ");
        let (grant_id, refresh_token) = sessions
            .mint_grant(
                client_id,
                &stored.tenant_id,
                &stored.enhanced.user_id,
                &stored.resource,
                grant_type_allowed(&client, "refresh_token"),
            )
            .await?;
        self.issue_token_response(
            "authorization_code",
            client_id,
            &stored.tenant_id,
            &stored.enhanced.user_id,
            &scope,
            &stored.resource,
            &grant_id,
            refresh_token,
        )
    }

    async fn exchange_refresh_token(
        &self,
        form: &McpOAuthTokenRequest,
    ) -> Result<McpOAuthTokenResponse, McpOAuthError> {
        let client_id = form.client_id.as_deref().unwrap_or("");
        let refresh_token = form.refresh_token.as_deref().unwrap_or("");
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

        if form
            .scope
            .as_deref()
            .filter(|scope| !scope.is_empty())
            .is_some_and(|scope| scope.split_whitespace().collect::<Vec<_>>() != [OAUTH_SCOPE])
        {
            return Err(McpOAuthError::bad_request(
                "invalid_scope",
                "only the granted mcp:tools scope is supported",
            ));
        }
        let rotated = OAuthSessionStore::new(self.storage.as_ref())
            .rotate_refresh_token(refresh_token, client_id, &resource)
            .await?;
        self.issue_token_response(
            "refresh_token",
            client_id,
            &rotated.tenant_id,
            &rotated.subject,
            OAUTH_SCOPE,
            &resource,
            &rotated.grant_id,
            Some(rotated.refresh_token),
        )
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
        if stored.enhanced.scopes != [OAUTH_SCOPE] || !stored.enhanced.is_valid() {
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

    #[allow(clippy::too_many_arguments)]
    fn issue_token_response(
        &self,
        grant_type: &str,
        client_id: &str,
        tenant_id: &str,
        subject: &str,
        scope: &str,
        resource: &str,
        grant_id: &str,
        refresh_token: Option<String>,
    ) -> Result<McpOAuthTokenResponse, McpOAuthError> {
        let access_token = self
            .jwt
            .mint_access_token(client_id, tenant_id, subject, scope, resource, grant_id)?;
        tracing::info!(
            grant_type = grant_type,
            client_id = client_id,
            tenant_id = tenant_id,
            subject = subject,
            resource = resource,
            "mcp inbound oauth access token issued"
        );
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
    issuer: &str,
) -> Result<String, McpOAuthError> {
    let mut parsed = reqwest::Url::parse(redirect_uri)
        .map_err(|_| McpOAuthError::bad_request("invalid_request", "redirect_uri is invalid"))?;
    {
        let mut q = parsed.query_pairs_mut();
        q.append_pair("code", code);
        q.append_pair("iss", issuer);
        if let Some(s) = state {
            q.append_pair("state", s);
        }
    }
    Ok(parsed.to_string())
}

fn build_redirect_with_error(
    redirect_uri: &str,
    error: &McpOAuthError,
    state: Option<&str>,
    issuer: &str,
) -> Result<String, McpOAuthError> {
    let mut parsed = reqwest::Url::parse(redirect_uri)
        .map_err(|_| McpOAuthError::bad_request("invalid_request", "redirect_uri is invalid"))?;
    let mut query = parsed.query_pairs_mut();
    query.append_pair("error", error.oauth_error_code());
    query.append_pair("error_description", error.description());
    query.append_pair("iss", issuer);
    if let Some(state) = state {
        query.append_pair("state", state);
    }
    drop(query);
    Ok(parsed.to_string())
}
