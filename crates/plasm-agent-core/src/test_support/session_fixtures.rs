//! Construct [`ExecuteSession`] values for unit/integration tests without positional-arg churn.

use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::{CgsContext, CGS};

use crate::execute_session::ExecuteSession;

/// Defaults for [`ExecuteSessionFixture::build`].
#[derive(Clone, Debug)]
pub struct ExecuteSessionFixture {
    pub prompt_hash: String,
    pub prompt_text: String,
    pub entry_id: String,
    pub tenant_scope: String,
    pub principal_subject: String,
    pub http_backend: Option<String>,
    pub entities: Vec<String>,
    pub principal: Option<String>,
    pub catalog_cgs_hash: Option<String>,
    pub context_intent: Option<String>,
    pub ranked_capabilities: Option<Vec<String>>,
}

impl ExecuteSessionFixture {
    pub fn new() -> Self {
        Self {
            prompt_hash: "ph".into(),
            prompt_text: String::new(),
            entry_id: "default".into(),
            tenant_scope: String::new(),
            principal_subject: String::new(),
            http_backend: None,
            entities: vec!["Pet".into()],
            principal: None,
            catalog_cgs_hash: None,
            context_intent: None,
            ranked_capabilities: None,
        }
    }

    pub fn prompt_hash(mut self, v: impl Into<String>) -> Self {
        self.prompt_hash = v.into();
        self
    }

    pub fn entry_id(mut self, v: impl Into<String>) -> Self {
        self.entry_id = v.into();
        self
    }

    pub fn entities(mut self, entities: Vec<String>) -> Self {
        self.entities = entities;
        self
    }

    pub fn catalog_cgs_hash(mut self, v: impl Into<String>) -> Self {
        self.catalog_cgs_hash = Some(v.into());
        self
    }

    pub fn build(self, cgs: Arc<CGS>) -> ExecuteSession {
        let entry_id = self.entry_id.clone();
        let mut contexts = IndexMap::new();
        contexts.insert(
            entry_id.clone(),
            Arc::new(CgsContext::entry(entry_id.as_str(), cgs.clone())),
        );
        let catalog_cgs_hash = self
            .catalog_cgs_hash
            .unwrap_or_else(|| cgs.catalog_cgs_hash_hex());
        ExecuteSession::new(
            self.prompt_hash,
            self.prompt_text,
            cgs,
            contexts,
            entry_id,
            self.tenant_scope,
            self.principal_subject,
            self.http_backend,
            self.entities,
            None,
            self.principal,
            catalog_cgs_hash,
            self.context_intent,
            self.ranked_capabilities,
        )
    }
}

impl Default for ExecuteSessionFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Minimal session: one `default` context, empty tenant/principal, `Pet` entity.
pub fn minimal_execute_session(cgs: Arc<CGS>) -> ExecuteSession {
    ExecuteSessionFixture::new().build(cgs)
}

/// Like [`minimal_execute_session`] but with a custom prompt hash.
pub fn test_execute_session(cgs: Arc<CGS>, prompt_hash: &str) -> ExecuteSession {
    ExecuteSessionFixture::new()
        .prompt_hash(prompt_hash)
        .build(cgs)
}
