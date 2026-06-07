//! Cooperative cancellation signal for long-running execute paths.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared flag checked between pagination pages and hydrate batches.
#[derive(Clone, Default)]
pub struct CancelSignal(Arc<AtomicBool>);

impl CancelSignal {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for CancelSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelSignal")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[inline]
pub fn check_cancel(cancel: Option<&CancelSignal>) -> Result<(), crate::RuntimeError> {
    if cancel.is_some_and(CancelSignal::is_cancelled) {
        return Err(crate::RuntimeError::Cancelled);
    }
    Ok(())
}
