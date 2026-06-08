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
    pub fn new(consume: &StreamConsumeOpts) -> Self {
        if let Some(ref spec) = consume.top_k {
            Self::TopK(TopKHeap::new(spec.clone()))
        } else if let Some(ref budget) = consume.row_match_budget {
            Self::RowMatch {
                budget: budget.clone(),
                matching_total: 0,
            }
        } else {
            Self::Standard
        }
    }

    pub fn skips_pre_page_merge(&self) -> bool {
        !matches!(self, Self::Standard)
    }

    pub fn ingest_page(&mut self, entities: Vec<CachedEntity>) -> PageIngestOutcome {
        match self {
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
                let filtered: Vec<_> = entities
                    .into_iter()
                    .filter(|e| entity_matches_predicates(e, &budget.predicates))
                    .collect();
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
                    heap.insert(entity);
                }
                PageIngestOutcome {
                    merge_into_mat: Vec::new(),
                    yield_entities: Vec::new(),
                    progress_rows,
                    row_match_budget_satisfied: false,
                }
            }
        }
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
