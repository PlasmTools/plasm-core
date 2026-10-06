//! Page collectors for paginated query streams (standard, row-match, top-k).

use crate::cache::CachedEntity;
use crate::row_predicate::entity_matches_predicates;
use crate::top_k::TopKHeap;
use crate::{RowMatchBudget, StreamConsumeOpts};

pub struct PageIngestOutcome {
    pub merge_into_mat: Vec<CachedEntity>,
    pub yield_entities: Vec<CachedEntity>,
    pub progress_rows: usize,
    pub row_match_budget_satisfied: bool,
}

pub enum PageCollector {
    Standard,
    RowMatch {
        budget: RowMatchBudget,
        matching_total: usize,
    },
    TopK(TopKHeap),
}

impl PageCollector {
    pub fn new(
        consume: &StreamConsumeOpts,
        cgs: &plasm_core::CGS,
        entity: &str,
    ) -> Result<Self, crate::RuntimeError> {
        Ok(if let Some(ref spec) = consume.top_k {
            let (field, path) = spec
                .sort_key
                .split_first()
                .ok_or(crate::RuntimeError::TopKFieldEmpty)?;
            let field = cgs
                .get_entity(entity)
                .and_then(|e| e.fields.get(field.as_str()))
                .ok_or_else(|| crate::RuntimeError::FieldUnknown {
                    entity: entity.to_owned(),
                    field: field.clone(),
                })?;
            let mut contract = plasm_core::value_contract::ValueContract::from_domain(
                cgs,
                "",
                field.kind.registry_key(),
            )?;
            contract.nullable = !field.required;
            for segment in path {
                contract = contract.field(segment)?;
            }
            Self::TopK(TopKHeap::new(spec.clone(), contract)?)
        } else if let Some(ref budget) = consume.row_match_budget {
            Self::RowMatch {
                budget: budget.clone(),
                matching_total: 0,
            }
        } else {
            Self::Standard
        })
    }

    pub fn skips_pre_page_merge(&self) -> bool {
        !matches!(self, Self::Standard)
    }

    pub fn ingest_page(
        &mut self,
        entities: Vec<CachedEntity>,
    ) -> Result<PageIngestOutcome, crate::RuntimeError> {
        Ok(match self {
            Self::Standard => {
                let progress_rows = entities.len();
                PageIngestOutcome {
                    merge_into_mat: Vec::new(),
                    yield_entities: entities,
                    progress_rows,
                    row_match_budget_satisfied: false,
                }
            }
            Self::RowMatch {
                budget,
                matching_total,
            } => {
                let mut filtered = Vec::new();
                for entity in entities {
                    if entity_matches_predicates(&entity, &budget.predicates)? {
                        filtered.push(entity);
                    }
                }
                *matching_total = matching_total.saturating_add(filtered.len());
                let satisfied = *matching_total >= budget.count;
                PageIngestOutcome {
                    progress_rows: filtered.len(),
                    merge_into_mat: filtered.clone(),
                    yield_entities: filtered,
                    row_match_budget_satisfied: satisfied,
                }
            }
            Self::TopK(heap) => {
                let progress_rows = entities.len();
                for entity in entities {
                    heap.insert(entity)?;
                }
                PageIngestOutcome {
                    merge_into_mat: Vec::new(),
                    yield_entities: Vec::new(),
                    progress_rows,
                    row_match_budget_satisfied: false,
                }
            }
        })
    }

    pub fn finish(self) -> Option<Vec<CachedEntity>> {
        match self {
            Self::TopK(heap) => {
                let entities = heap.into_sorted_entities();
                if entities.is_empty() {
                    None
                } else {
                    Some(entities)
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn top_k_resolves_declared_catalog_domain_before_any_page() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs =
            plasm_core::load_schema(&root.join("../../fixtures/schemas/plasm_pagination_matrix"))
                .unwrap();
        let mut consume = StreamConsumeOpts {
            top_k: Some(crate::TopKSpec {
                count: 1,
                sort_key: vec!["n".into()],
                descending: false,
                row_filter: vec![],
            }),
            ..Default::default()
        };
        let mut collector = PageCollector::new(&consume, &cgs, "Item").unwrap();
        // A numeric declaration cannot be inferred as a string from its first page.
        let entity = CachedEntity {
            reference: plasm_core::Ref::new("Item", "a"),
            fields: [(
                "n".into(),
                plasm_core::TypedFieldValue::from(plasm_core::Value::String("2024-01-01".into())),
            )]
            .into(),
            relations: Default::default(),
            last_updated: 0,
            version: 0,
            completeness: crate::EntityCompleteness::Summary,
            unavailable_fields: Default::default(),
        };
        assert!(collector.ingest_page(vec![entity]).is_err());
        consume.top_k.as_mut().unwrap().sort_key = vec!["absent".into()];
        assert!(PageCollector::new(&consume, &cgs, "Item").is_err());
    }
}
