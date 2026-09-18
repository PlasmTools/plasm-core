//! HTTP transport helpers (compiled request + absolute URL GET).

use super::*;
use crate::http_transport::compiled_method_label;
use tracing::Instrument;

impl ExecutionEngine {
    /// Execute request and capture `Link` header (`rel="next"`) when present.
    pub(crate) async fn execute_http_request_full(
        &self,
        request: &CompiledRequest,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let method = compiled_method_label(&request.method);
        let url_len = request.url_path().len();
        self.execute_http_request_full_inner(request)
            .instrument(crate::spans::http_compiled_request(method, url_len))
            .await
    }

    async fn execute_http_request_full_inner(
        &self,
        request: &CompiledRequest,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let base_url = self.effective_http_base_for_request();
        let auth = self.resolve_compiled_http_auth(request).await?;
        let destination = crate::http_transport::compiled_http_url(base_url.as_ref(), request);
        let _permit = self.acquire_backend_http_permit(&destination).await?;
        annotate_http_401_login_tail(
            self.transport
                .send_compiled_http(base_url.as_ref(), request, auth)
                .await,
        )
    }

    pub(super) async fn resolve_compiled_http_auth(
        &self,
        request: &CompiledRequest,
    ) -> Result<Option<crate::auth::ResolvedAuth>, RuntimeError> {
        let base_url = self.effective_http_base_for_request();
        match &request.credential {
            Some(credential) => {
                let (store, scope) = credential_scope(&credential.slot, &credential.resource, base_url.as_ref())?;
                let destination = crate::http_transport::compiled_http_url(base_url.as_ref(), request);
                let destination = url::Url::parse(&destination).map_err(|_| crate::credentials::credential_error("invalid credential request destination"))?;
                if destination.origin().ascii_serialization() != scope.origin || !destination.username().is_empty() || destination.password().is_some() {
                    return Err(crate::credentials::credential_error("credential request destination is outside its scope"));
                }
                let reference = crate::credentials::CredentialReference::parse(&credential.reference)?;
                match store.resolve(&reference, &scope).await? {
                    plasm_compile::CredentialSource::Host {} => {
                        let auth = self.resolve_auth_http().await?;
                        if !self.transport.injects_host_auth()
                            && !auth.as_ref().is_some_and(|auth| {
                                auth.headers.iter().chain(&auth.query_params).any(|(_, value)| !value.trim().is_empty())
                            })
                        {
                            return Err(crate::credentials::credential_error("scoped host authentication is not configured"));
                        }
                        Ok(auth)
                    }
                }
            }
            None if request.headers.as_ref().is_some_and(|headers| matches!(headers, plasm_core::Value::Object(fields) if fields.keys().any(|key| key.eq_ignore_ascii_case("Authorization")))) => Ok(None),
            None => self.resolve_auth_http().await,
        }
    }

    /// GET absolute URL (used for `link_header` continuation pages).
    pub(crate) async fn get_json_absolute(
        &self,
        url: &str,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.get_json_absolute_inner(url)
            .instrument(crate::spans::http_absolute_get(url.len()))
            .await
    }

    async fn get_json_absolute_inner(
        &self,
        url: &str,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let auth = self.resolve_auth_http().await?;
        let _permit = self.acquire_backend_http_permit(url).await?;
        annotate_http_401_login_tail(self.transport.get_json_absolute(url, auth).await)
    }
}

fn annotate_http_401_login_tail<T>(result: Result<T, RuntimeError>) -> Result<T, RuntimeError> {
    let Err(RuntimeError::RequestError {
        status: Some(401),
        message,
        attempts,
        body,
    }) = result
    else {
        return result;
    };
    let Some(material) = super::session::try_current_execute_session_material() else {
        return Err(RuntimeError::RequestError {
            message,
            attempts,
            status: Some(401),
            body,
        });
    };
    let login_tail = material
        .login_access_token_tail
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    let Some(login_tail) = login_tail else {
        return Err(RuntimeError::RequestError {
            message,
            attempts,
            status: Some(401),
            body,
        });
    };
    Err(RuntimeError::RequestError {
        message: crate::http_auth_failure::append_login_token_compare(&message, &login_tail),
        attempts,
        status: Some(401),
        body,
    })
}

pub(super) fn credential_scope(
    slot: &str,
    resource: &serde_json::Value,
    base: &str,
) -> Result<
    (
        Arc<dyn crate::credentials::SessionCredentialStore>,
        crate::credentials::CredentialScope,
    ),
    RuntimeError,
> {
    let material = super::session::try_current_execute_session_material().ok_or_else(|| {
        crate::credentials::credential_error("credential effects require an execute session")
    })?;
    let store = material.credential_store.clone().ok_or_else(|| {
        crate::credentials::credential_error(
            "execute session has no credential persistence adapter",
        )
    })?;
    let origin = url::Url::parse(base)
        .map_err(|_| crate::credentials::credential_error("invalid credential transport origin"))?
        .origin()
        .ascii_serialization();
    Ok((
        store,
        crate::credentials::CredentialScope {
            session: format!("{}:{}", material.prompt_hash, material.session_id),
            catalog_revision: material.catalog_revision.clone(),
            origin,
            slot: slot.into(),
            resource: resource.clone(),
        },
    ))
}
