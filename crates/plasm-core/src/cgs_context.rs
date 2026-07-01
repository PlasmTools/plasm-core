//! [`Prefix`] and [`CgsContext`] — namespace provenance carried with a loaded [`CGS`](crate::schema::CGS).
//!
//! HTTP origin for execution is [`CGS::http_backend`] on the inner graph.

use crate::schema::CGS;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Logical namespace for a loaded capability graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Prefix {
    /// Single untagged domain (e.g. one `--schema` file).
    #[default]
    None,
    /// Identifies a row in a multi-entry catalog.
    Entry { id: String },
}

/// Schema plus its [`Prefix`], threaded through parse / typecheck / HTTP layers.
#[derive(Clone)]
pub struct CgsContext {
    pub prefix: Prefix,
    pub cgs: Arc<CGS>,
}

impl CgsContext {
    pub fn new(prefix: Prefix, cgs: Arc<CGS>) -> Self {
        Self { prefix, cgs }
    }

    pub fn none(cgs: Arc<CGS>) -> Self {
        Self {
            prefix: Prefix::None,
            cgs,
        }
    }

    pub fn entry(id: impl Into<String>, cgs: Arc<CGS>) -> Self {
        Self {
            prefix: Prefix::Entry { id: id.into() },
            cgs,
        }
    }

    /// Registry row id for this context — [`Prefix::Entry`] id or stamped [`CGS::entry_id`].
    pub fn registry_entry_id(&self) -> Option<&str> {
        match &self.prefix {
            Prefix::Entry { id } => Some(id.as_str()),
            Prefix::None => self.cgs.entry_id.as_deref(),
        }
    }

    /// [`CgsLayer`] view; fails when neither prefix nor CGS carries an `entry_id`.
    pub fn as_layer(&self) -> Option<crate::cgs_federation::CgsLayer<'_>> {
        Some(crate::cgs_federation::CgsLayer::new(
            self.registry_entry_id()?,
            self.cgs.as_ref(),
        ))
    }
}

impl std::ops::Deref for CgsContext {
    type Target = CGS;

    fn deref(&self) -> &Self::Target {
        &self.cgs
    }
}

impl AsRef<CGS> for CgsContext {
    fn as_ref(&self) -> &CGS {
        &self.cgs
    }
}

impl std::fmt::Debug for CgsContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CgsContext")
            .field("prefix", &self.prefix)
            .field("cgs", &"<CGS>")
            .finish()
    }
}
