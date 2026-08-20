//! Catalog vs row filter newtypes — cannot be substituted.

use crate::plasm_monad::payload::PlanPredicate;
use serde::{Deserialize, Serialize};

use super::error::RowFilterError;

/// Fetch-plane predicates (`e1{…}`). Not a row-compute input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogFilter(Vec<PlanPredicate>);

impl CatalogFilter {
    #[must_use]
    pub fn new(predicates: Vec<PlanPredicate>) -> Self {
        Self(predicates)
    }

    #[must_use]
    pub fn predicates(&self) -> &[PlanPredicate] {
        &self.0
    }
}

/// Row-plane AND-filter. No conversion to [`CatalogFilter`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowFilter {
    predicates: Vec<PlanPredicate>,
}

impl RowFilter {
    pub fn new(predicates: Vec<PlanPredicate>) -> Result<Self, RowFilterError> {
        if predicates.is_empty() {
            return Err(RowFilterError::Empty);
        }
        Ok(Self { predicates })
    }

    #[must_use]
    pub fn predicates(&self) -> &[PlanPredicate] {
        &self.predicates
    }
}

impl TryFrom<Vec<PlanPredicate>> for RowFilter {
    type Error = RowFilterError;

    fn try_from(predicates: Vec<PlanPredicate>) -> Result<Self, Self::Error> {
        Self::new(predicates)
    }
}
