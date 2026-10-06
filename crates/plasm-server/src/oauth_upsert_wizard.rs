//! Multi-step OAuth provider upsert (TUI) — mirrors `plasm-server oauth provider upsert` fields.

use plasm_agent_core::mcp_config_admin::McpConfigCatalogRow;

use crate::appliance_oauth_admin::{appliance_oauth_client_secret_kv_key, ApplianceOauthUpsert};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OauthWizardError {
    #[error("No registry API matches the current search")]
    NoMatchingRegistryApi,
    #[error("entry_id required")]
    MissingEntryId,
    #[error("token_endpoint required")]
    MissingTokenEndpoint,
    #[error("client_id required")]
    MissingClientId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OAuthUpsertStep {
    EntryId,
    TokenEndpoint,
    AuthorizationEndpoint,
    DeviceAuthorizationEndpoint,
    ClientId,
    ClientSecret,
    Scopes,
    Enabled,
    Confirm,
}

#[derive(Clone, Debug)]
pub(crate) struct OAuthUpsertWizard {
    pub step: OAuthUpsertStep,
    pub buf: String,
    pub entry_sel: usize,
    pub entry_id: String,
    pub token_endpoint: String,
    pub authorization_endpoint: Option<String>,
    pub device_authorization_endpoint: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
    pub enabled: bool,
}

impl OAuthUpsertWizard {
    pub fn new() -> Self {
        Self {
            step: OAuthUpsertStep::EntryId,
            buf: String::new(),
            entry_sel: 0,
            entry_id: String::new(),
            token_endpoint: String::new(),
            authorization_endpoint: None,
            device_authorization_endpoint: None,
            client_id: String::new(),
            client_secret: None,
            scopes: Vec::new(),
            enabled: true,
        }
    }

    pub fn for_entry(entry_id: &str) -> Self {
        let mut wizard = Self::new();
        wizard.entry_id = entry_id.trim().to_string();
        wizard.step = OAuthUpsertStep::TokenEndpoint;
        wizard
    }

    pub fn prompt_title(&self) -> &'static str {
        match self.step {
            OAuthUpsertStep::EntryId => "entry_id (search registry APIs, then Enter selects)",
            OAuthUpsertStep::TokenEndpoint => "token_endpoint (URL, required)",
            OAuthUpsertStep::AuthorizationEndpoint => {
                "authorization_endpoint (optional — Enter empty to skip)"
            }
            OAuthUpsertStep::DeviceAuthorizationEndpoint => {
                "device_authorization_endpoint (optional — Enter empty to skip)"
            }
            OAuthUpsertStep::ClientId => "client_id (required)",
            OAuthUpsertStep::ClientSecret => {
                "client_secret (optional — echoes in raw TTY; Enter empty to skip; prefer CLI stdin for secrets)"
            }
            OAuthUpsertStep::Scopes => "scopes (comma-separated, Enter empty for none)",
            OAuthUpsertStep::Enabled => "enabled — Space toggles, Enter continues",
            OAuthUpsertStep::Confirm => "Confirm — Enter save, Esc cancel wizard",
        }
    }

    pub fn filtered_entry_indices(&self, rows: &[McpConfigCatalogRow]) -> Vec<usize> {
        let filter = self.buf.trim().to_ascii_lowercase();
        rows.iter()
            .enumerate()
            .filter_map(|(i, row)| {
                if filter.is_empty()
                    || row.entry_id.to_ascii_lowercase().contains(&filter)
                    || row.label.to_ascii_lowercase().contains(&filter)
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn move_entry_selection(&mut self, rows: &[McpConfigCatalogRow], delta: isize) {
        let matches = self.filtered_entry_indices(rows);
        if matches.is_empty() {
            self.entry_sel = 0;
            return;
        }
        let max = matches.len().saturating_sub(1) as isize;
        let cur = self.entry_sel.min(matches.len().saturating_sub(1)) as isize;
        self.entry_sel = (cur + delta).clamp(0, max) as usize;
    }

    pub fn reset_entry_selection(&mut self) {
        self.entry_sel = 0;
    }

    pub fn commit_entry_selection(
        &mut self,
        rows: &[McpConfigCatalogRow],
    ) -> Result<(), OauthWizardError> {
        let matches = self.filtered_entry_indices(rows);
        let Some(row_ix) = matches
            .get(self.entry_sel.min(matches.len().saturating_sub(1)))
            .copied()
        else {
            return Err(OauthWizardError::NoMatchingRegistryApi);
        };
        self.entry_id = rows[row_ix].entry_id.trim().to_string();
        self.buf.clear();
        self.entry_sel = 0;
        self.step = OAuthUpsertStep::TokenEndpoint;
        Ok(())
    }

    pub fn summary_lines(&self) -> Vec<String> {
        vec![
            format!("entry_id: {}", self.entry_id),
            format!("token_endpoint: {}", self.token_endpoint),
            format!(
                "authorization_endpoint: {}",
                self.authorization_endpoint.as_deref().unwrap_or("(none)")
            ),
            format!(
                "device_authorization_endpoint: {}",
                self.device_authorization_endpoint
                    .as_deref()
                    .unwrap_or("(none)")
            ),
            format!("client_id: {}", self.client_id),
            format!(
                "client_secret: {}",
                if self.client_secret.is_some() {
                    "(set)"
                } else {
                    "(none)"
                }
            ),
            format!(
                "scopes: {}",
                if self.scopes.is_empty() {
                    "(none)".into()
                } else {
                    self.scopes.join(", ")
                }
            ),
            format!("enabled: {}", self.enabled),
        ]
    }

    /// `Enter` on a field step (not `Enabled`, not `Confirm`).
    pub fn commit_buf_and_advance(&mut self) -> Result<(), OauthWizardError> {
        let t = self.buf.trim();
        match self.step {
            OAuthUpsertStep::EntryId => {
                if t.is_empty() && self.entry_id.is_empty() {
                    return Err(OauthWizardError::MissingEntryId);
                }
                if !t.is_empty() {
                    self.entry_id = t.to_string();
                }
                self.buf.clear();
                self.entry_sel = 0;
                self.step = OAuthUpsertStep::TokenEndpoint;
            }
            OAuthUpsertStep::TokenEndpoint => {
                if t.is_empty() {
                    return Err(OauthWizardError::MissingTokenEndpoint);
                }
                self.token_endpoint = t.to_string();
                self.buf.clear();
                self.step = OAuthUpsertStep::AuthorizationEndpoint;
            }
            OAuthUpsertStep::AuthorizationEndpoint => {
                self.authorization_endpoint = if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                };
                self.buf.clear();
                self.step = OAuthUpsertStep::DeviceAuthorizationEndpoint;
            }
            OAuthUpsertStep::DeviceAuthorizationEndpoint => {
                self.device_authorization_endpoint = if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                };
                self.buf.clear();
                self.step = OAuthUpsertStep::ClientId;
            }
            OAuthUpsertStep::ClientId => {
                if t.is_empty() {
                    return Err(OauthWizardError::MissingClientId);
                }
                self.client_id = t.to_string();
                self.buf.clear();
                self.step = OAuthUpsertStep::ClientSecret;
            }
            OAuthUpsertStep::ClientSecret => {
                self.client_secret = if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                };
                self.buf.clear();
                self.step = OAuthUpsertStep::Scopes;
            }
            OAuthUpsertStep::Scopes => {
                self.scopes = t
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                self.buf.clear();
                self.step = OAuthUpsertStep::Enabled;
            }
            OAuthUpsertStep::Enabled | OAuthUpsertStep::Confirm => {}
        }
        Ok(())
    }

    pub fn advance_enabled_to_confirm(&mut self) {
        if self.step == OAuthUpsertStep::Enabled {
            self.buf.clear();
            self.step = OAuthUpsertStep::Confirm;
        }
    }

    pub fn try_build_upsert(
        &self,
    ) -> Result<ApplianceOauthUpsert, crate::appliance_oauth_admin::OauthClientSecretKeyError> {
        let client_secret_key = appliance_oauth_client_secret_kv_key(&self.entry_id)?;
        Ok(ApplianceOauthUpsert {
            entry_id: self.entry_id.clone(),
            authorization_endpoint: self.authorization_endpoint.clone(),
            token_endpoint: self.token_endpoint.clone(),
            device_authorization_endpoint: self.device_authorization_endpoint.clone(),
            default_scopes: self.scopes.clone(),
            client_id: self.client_id.clone(),
            client_secret_key,
            client_secret_value: self.client_secret.clone(),
            enabled: self.enabled,
        })
    }
}
