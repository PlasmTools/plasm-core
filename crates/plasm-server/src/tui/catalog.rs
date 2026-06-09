//! API catalogue row helpers

use super::*;

pub(crate) fn row_enabled(state: &RunState, snap: &UiSnapshot, entry_id: &str) -> bool {
    if let Some(ref staged) = state.api.staged_allowed {
        return staged.contains(entry_id);
    }
    snap.db_allowed.contains(entry_id)
}

/// `(entry_id, missing)` pairs for enabled MCP allowlist rows (secret and/or binding slots).
pub(crate) fn catalog_policy_readiness_gaps(
    rows: &[McpConfigCatalogRow],
    allowed: &std::collections::HashSet<String>,
) -> Vec<(String, String)> {
    rows.iter()
        .filter(|r| allowed.contains(&r.entry_id))
        .flat_map(|r| {
            let mut gaps = Vec::new();
            if r.connect_profile.has_api_key && !r.auth_optional && !r.api_secret_present {
                gaps.push((r.entry_id.clone(), "secret".to_string()));
            }
            if r.bindings_required && !r.bindings_complete {
                gaps.push((r.entry_id.clone(), "binding".to_string()));
            }
            gaps
        })
        .collect()
}

pub(crate) fn oauth_surface_status(snap: &UiSnapshot) -> Option<&str> {
    snap.oauth_surface.status_message()
}

pub(crate) fn input_mode_label(mode: &InputMode) -> Option<&'static str> {
    match mode {
        InputMode::Normal => None,
        InputMode::ApiFilter => Some("API filter"),
        InputMode::ApiSecretEdit { .. } => Some("API key secret"),
        InputMode::CatalogConnect { .. } => Some("Catalog connect"),
        InputMode::AddKeyLabel { .. } => Some("Add key"),
        InputMode::ConfirmKeyRevoke { .. } => Some("Confirm revoke"),
        InputMode::OAuthWizard(_) => Some("OAuth wizard"),
        InputMode::OAuthDeviceScopePick(_) => Some("OAuth scopes"),
        InputMode::ConfirmOAuthDisable { .. } => Some("Confirm disable"),
    }
}
