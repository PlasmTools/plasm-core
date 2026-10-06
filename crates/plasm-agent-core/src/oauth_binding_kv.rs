//! KV pointer from registry `entry_id` → outbound OAuth credential key (`plasm:outbound:v1:…`).
//!
//! Written when account linking completes so UIs can detect bindings without scanning KV.

use std::sync::Arc;

use auth_framework::storage::AuthStorage;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
#[error("failed to store outbound OAuth binding pointer")]
pub struct OAuthBindingWriteError(#[source] auth_framework::AuthError);

/// Stable KV key for “which outbound token row is bound to this catalog entry”.
pub fn oauth_binding_kv_key(entry_id: &str) -> String {
    format!("plasm:oauth_binding:v1:{}", entry_id.trim())
}

/// Store `{ "hosted_kv_key": "…" }` UTF-8 JSON at [`oauth_binding_kv_key`].
pub async fn write_oauth_binding_pointer(
    storage: &Arc<dyn AuthStorage>,
    entry_id: &str,
    hosted_kv_key: &str,
) -> Result<(), OAuthBindingWriteError> {
    let payload = json!({ "hosted_kv_key": hosted_kv_key }).to_string();
    storage
        .store_kv(
            oauth_binding_kv_key(entry_id).as_str(),
            payload.as_bytes(),
            None,
        )
        .await
        .map_err(OAuthBindingWriteError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_write_fault_preserves_auth_storage_source() {
        use std::error::Error;
        let error = OAuthBindingWriteError(auth_framework::AuthError::Storage(
            auth_framework::errors::StorageError::BackendUnavailable,
        ));
        let source = error.source().unwrap();
        assert!(matches!(
            source.downcast_ref::<auth_framework::AuthError>(),
            Some(auth_framework::AuthError::Storage(
                auth_framework::errors::StorageError::BackendUnavailable
            ))
        ));
        assert!(matches!(
            source
                .source()
                .unwrap()
                .downcast_ref::<auth_framework::errors::StorageError>(),
            Some(auth_framework::errors::StorageError::BackendUnavailable)
        ));
    }
}
