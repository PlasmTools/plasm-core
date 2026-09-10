//! CGS `oauth.scopes` / `default_scope_sets` selection for device-code binding (not provider `default_scopes`).

use std::collections::BTreeSet;
use std::sync::Arc;

use auth_framework::storage::AuthStorage;
use plasm_agent_core::oauth_link_catalog::OauthLinkCatalog;
use plasm_core::discovery::{CgsCatalog, CgsRegistry, DiscoveryError};
use plasm_core::schema::{OauthDefaultScopeSet, OauthScopeEntry};

#[derive(Clone)]
pub struct OAuthDeviceScopePickState {
    pub entry_id: String,
    /// `(scope_id, label)` sorted by `scope_id`.
    pub scope_rows: Vec<(String, String)>,
    /// Named bundles from CGS `default_scope_sets` (order preserved).
    pub default_sets: Vec<(String, Vec<String>)>,
    pub selected: BTreeSet<String>,
    pub cursor: usize,
    pub link_catalog: Arc<OauthLinkCatalog>,
    pub storage: Arc<dyn AuthStorage>,
}

impl std::fmt::Debug for OAuthDeviceScopePickState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthDeviceScopePickState")
            .field("entry_id", &self.entry_id)
            .field("scope_rows", &self.scope_rows)
            .field("default_sets", &self.default_sets)
            .field("selected", &self.selected)
            .field("cursor", &self.cursor)
            .field("link_catalog", &"<OauthLinkCatalog>")
            .field("storage", &"<AuthStorage>")
            .finish()
    }
}

impl OAuthDeviceScopePickState {
    /// When the loaded CGS has a non-empty `oauth.scopes` map, returns a picker pre-filled from the
    /// first `default_scope_sets` entry (if any). Otherwise `Ok(None)` so callers fall back to
    /// empty scopes (legacy provider defaults).
    pub fn try_open(
        reg: &CgsRegistry,
        entry_id: String,
        link_catalog: Arc<OauthLinkCatalog>,
        storage: Arc<dyn AuthStorage>,
    ) -> Result<Option<Self>, DiscoveryError> {
        let ctx = reg.load_context(entry_id.trim())?;
        let Some(oauth) = ctx.oauth.as_ref() else {
            return Ok(None);
        };
        if oauth.scopes.is_empty() {
            return Ok(None);
        }

        let mut scope_rows: Vec<(String, String)> = oauth
            .scopes
            .iter()
            .map(|(id, e): (&String, &OauthScopeEntry)| {
                let label = e.label.trim();
                let display = if label.is_empty() {
                    id.clone()
                } else {
                    label.to_string()
                };
                (id.clone(), display)
            })
            .collect();
        scope_rows.sort_by(|a, b| a.0.cmp(&b.0));

        let default_sets: Vec<(String, Vec<String>)> = oauth
            .default_scope_sets
            .iter()
            .map(|(n, b): (&String, &OauthDefaultScopeSet)| (n.clone(), b.scopes.clone()))
            .collect();

        let mut selected = BTreeSet::new();
        if let Some((_, scopes)) = default_sets.first() {
            for s in scopes {
                if oauth.scopes.contains_key(s) {
                    selected.insert(s.clone());
                }
            }
        }

        Ok(Some(Self {
            entry_id,
            scope_rows,
            default_sets,
            selected,
            cursor: 0,
            link_catalog,
            storage,
        }))
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.scope_rows.is_empty() {
            return;
        }
        let n = self.scope_rows.len() as isize;
        let cur = self.cursor as isize;
        self.cursor = (cur + delta).rem_euclid(n) as usize;
    }

    pub fn toggle_cursor_row(&mut self) {
        if let Some((id, _)) = self.scope_rows.get(self.cursor) {
            if self.selected.contains(id) {
                self.selected.remove(id);
            } else {
                self.selected.insert(id.clone());
            }
        }
    }

    /// `idx` is zero-based (key `1` → `idx` 0). Returns the set name when a bundle existed.
    pub fn apply_default_set(&mut self, idx: usize) -> Option<String> {
        if let Some((name, scopes)) = self.default_sets.get(idx) {
            self.selected = scopes.iter().cloned().collect();
            return Some(name.clone());
        }
        None
    }

    pub fn selected_scope_strings(&self) -> Vec<String> {
        self.selected.iter().cloned().collect()
    }
}
