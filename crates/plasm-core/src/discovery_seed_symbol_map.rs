//! Closed `s#` vocabulary for LLM seed selection.

use std::collections::{HashMap, HashSet};

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_seed_catalog::CatalogWorkflowContext;
use crate::discovery_seed_select::SeedSelectionValidationError;

/// One entity row presented to the selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSymbolRow {
    pub symbol: String,
    pub candidate_id: String,
    pub catalog: String,
    pub entity: String,
    pub entity_description: String,
    pub capability_kinds: String,
    pub relation_coverage: String,
    pub entity_aliases: String,
}

/// Host-assigned symbol table for a single selector call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSymbolMap {
    rows: Vec<SeedSymbolRow>,
    symbol_to_candidate: HashMap<String, String>,
}

impl SeedSymbolMap {
    pub fn build(
        bundles: &[EntityCandidateBundle],
        catalog_context: Option<&CatalogWorkflowContext>,
    ) -> Self {
        let mut ordered: Vec<&EntityCandidateBundle> = bundles
            .iter()
            .filter(|bundle| !bundle.entity.ends_with("Context"))
            .filter(|bundle| !bundle.capabilities.is_empty())
            .collect();
        ordered.sort_by(|left, right| {
            right
                .max_lexical_score
                .cmp(&left.max_lexical_score)
                .then_with(|| left.entry_id.cmp(&right.entry_id))
                .then_with(|| left.entity.cmp(&right.entity))
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });

        let mut rows = Vec::with_capacity(ordered.len());
        let mut symbol_to_candidate = HashMap::new();
        for (index, bundle) in ordered.into_iter().enumerate() {
            let symbol = format!("s{}", index + 1);
            symbol_to_candidate.insert(symbol.clone(), bundle.candidate_id.clone());
            rows.push(SeedSymbolRow {
                symbol,
                candidate_id: bundle.candidate_id.clone(),
                catalog: bundle.entry_id.clone(),
                entity: bundle.entity.clone(),
                entity_description: bundle.entity_description.clone(),
                capability_kinds: capability_kind_summary(bundle),
                relation_coverage: if bundle.relation_hints.is_empty() {
                    "(none)".into()
                } else {
                    bundle.relation_hints.clone()
                },
                entity_aliases: entity_aliases_for(
                    catalog_context,
                    &bundle.entry_id,
                    &bundle.entity,
                ),
            });
        }

        Self {
            rows,
            symbol_to_candidate,
        }
    }

    pub fn rows(&self) -> &[SeedSymbolRow] {
        &self.rows
    }

    pub fn symbol_count(&self) -> usize {
        self.rows.len()
    }

    pub fn candidate_for_symbol(&self, symbol: &str) -> Option<&str> {
        self.symbol_to_candidate.get(symbol).map(String::as_str)
    }

    pub fn resolve_symbols(
        &self,
        symbols: &[String],
    ) -> Result<Vec<String>, SeedSelectionValidationError> {
        let mut resolved = Vec::with_capacity(symbols.len());
        let mut seen = HashSet::new();
        for symbol in symbols {
            if symbol.contains(':') {
                return Err(SeedSelectionValidationError::RawIdHallucination(
                    symbol.clone(),
                ));
            }
            let Some(candidate_id) = self.symbol_to_candidate.get(symbol) else {
                return Err(SeedSelectionValidationError::UnknownSymbol(symbol.clone()));
            };
            if !seen.insert(candidate_id.clone()) {
                return Err(SeedSelectionValidationError::DuplicateSelectedId);
            }
            resolved.push(candidate_id.clone());
        }
        Ok(resolved)
    }

    pub fn resolve_alternative_symbol_sets(
        &self,
        sets: &[Vec<String>],
    ) -> Result<Vec<Vec<String>>, SeedSelectionValidationError> {
        sets.iter()
            .map(|symbols| self.resolve_symbols(symbols))
            .collect()
    }
}

pub(crate) fn entity_aliases_for(
    catalog_context: Option<&CatalogWorkflowContext>,
    entry_id: &str,
    entity: &str,
) -> String {
    catalog_context
        .and_then(|context| context.index(entry_id))
        .and_then(|index| index.entity_phrases.get(entity))
        .map(|phrases| phrases.join("|"))
        .unwrap_or_default()
}

fn capability_kind_summary(bundle: &EntityCandidateBundle) -> String {
    let mut kinds: Vec<String> = bundle
        .capabilities
        .iter()
        .map(|capability| capability.witness_kind().to_string())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    if kinds.is_empty() {
        "(none)".into()
    } else {
        kinds.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_auto_seed::EntityCapabilityEvidence;

    fn bundle(id: &str, catalog: &str, entity: &str) -> EntityCandidateBundle {
        EntityCandidateBundle {
            candidate_id: id.into(),
            entry_id: catalog.into(),
            entity: entity.into(),
            entity_description: String::new(),
            max_lexical_score: 1,
            capabilities: vec![EntityCapabilityEvidence {
                capability_id: format!("{catalog}:{entity}:query"),
                capability_name: "query".into(),
                kind: "Query".into(),
                effect: crate::SemanticEffect::Read,
                description: String::new(),
                reason_codes: vec![],
                lexical_score: 1,
            }],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        }
    }

    #[test]
    fn symbols_are_stable_and_resolve() {
        let bundles = vec![
            bundle("catalog_a:TagLeaf", "catalog_a", "TagLeaf"),
            bundle("catalog_a:ParentNav", "catalog_a", "ParentNav"),
        ];
        let map = SeedSymbolMap::build(&bundles, None);
        assert_eq!(map.rows()[0].entity, "ParentNav");
        assert_eq!(
            map.resolve_symbols(&["s1".into()]).expect("resolve"),
            vec!["catalog_a:ParentNav"]
        );
    }

    #[test]
    fn raw_ids_and_unknown_symbols_are_rejected() {
        let bundles = vec![bundle("catalog_a:ParentNav", "catalog_a", "ParentNav")];
        let map = SeedSymbolMap::build(&bundles, None);
        assert!(matches!(
            map.resolve_symbols(&["catalog_a:ParentNav".into()]),
            Err(SeedSelectionValidationError::RawIdHallucination(_))
        ));
        assert!(matches!(
            map.resolve_symbols(&["s99".into()]),
            Err(SeedSelectionValidationError::UnknownSymbol(_))
        ));
    }
}
