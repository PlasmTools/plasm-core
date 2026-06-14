//! Exclusive session graph-cache guard for async execute scopes.
//!
//! Tokio's session graph mutex is **not reentrant**. [`GraphCacheGuard`] is the only
//! proof that hot-cache reads may borrow the live [`SessionMaterialization`] without
//! acquiring the mutex again on the same task.
//!
//! Live execute/projection runs on a [`crate::graph_execute::GraphExecuteBranch`] fork;
//! the session mutex is held only for brief fork/commit and epoch reads — not across HTTP.

use std::ops::{Deref, DerefMut};

use plasm_runtime::SessionMaterialization;
use tokio::sync::MutexGuard;

/// Holds `ExecuteSession::graph_cache`'s mutex guard.
pub struct GraphCacheGuard<'a> {
    inner: MutexGuard<'a, SessionMaterialization>,
}

impl GraphCacheGuard<'_> {
    pub(crate) fn from_guard(inner: MutexGuard<'_, SessionMaterialization>) -> GraphCacheGuard<'_> {
        GraphCacheGuard { inner }
    }

    /// Hot materialization view while the guard is held.
    pub(crate) fn materialization(&self) -> &SessionMaterialization {
        &self.inner
    }
}

impl Deref for GraphCacheGuard<'_> {
    type Target = SessionMaterialization;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for GraphCacheGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
