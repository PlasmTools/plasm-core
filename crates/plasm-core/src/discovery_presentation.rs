//! Discovery presentation contract: catalog routing surface and agent-facing decision branch.

use serde::{Deserialize, Serialize};

use crate::discovery::DiscoveryResult;

/// Sorted catalog `entry_id`s matched by intent routing (empty when routing scanned all catalogs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CatalogRoute(pub Vec<String>);

impl CatalogRoute {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_multi(&self) -> bool {
        self.0.len() >= 2
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    pub fn join_display(&self) -> String {
        self.0.join(", ")
    }
}

impl From<Vec<String>> for CatalogRoute {
    fn from(v: Vec<String>) -> Self {
        Self(v)
    }
}

impl AsRef<[String]> for CatalogRoute {
    fn as_ref(&self) -> &[String] {
        &self.0
    }
}

impl std::ops::Deref for CatalogRoute {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Structured discovery branch encoded in TSV `# decision:` lines and `_meta.plasm.discovery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscoveryDecision {
    #[default]
    Match,
    Clarify,
    NoMatch,
}

impl DiscoveryDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => crate::prompt_render::DISCOVER_DECISION_MATCH,
            Self::Clarify => crate::prompt_render::DISCOVER_DECISION_CLARIFY,
            Self::NoMatch => crate::prompt_render::DISCOVER_DECISION_NO_MATCH,
        }
    }

    /// Agent-facing decision from discovery result plus the rows shown in the TSV.
    pub fn for_presentation(result: &DiscoveryResult, shown_rows: &[(String, String)]) -> Self {
        if shown_rows.is_empty() {
            Self::NoMatch
        } else if !result.ambiguities.is_empty() {
            Self::Clarify
        } else if result.catalog_route.is_multi() {
            let shown_apis: std::collections::BTreeSet<&str> =
                shown_rows.iter().map(|(api, _)| api.as_str()).collect();
            if shown_apis.len() < 2 {
                Self::Clarify
            } else {
                Self::Match
            }
        } else {
            Self::Match
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{CapabilityQuery, DiscoveryResult, RankedCandidate};

    #[test]
    fn clarify_when_multi_route_shown_single_api() {
        let result = DiscoveryResult {
            contexts: vec![],
            candidates: vec![RankedCandidate {
                entry_id: "proof".into(),
                entity: "Document".into(),
                capability_name: "document_get".into(),
                score: 10,
                reason_codes: vec![],
                capability_description: String::new(),
            }],
            ambiguities: vec![],
            applied_query_echo: CapabilityQuery::default(),
            closure_stats: None,
            schema_neighborhoods: vec![],
            entity_summaries: vec![],
            catalog_route: CatalogRoute::from(vec!["pokeapi".into(), "proof".into()]),
        };
        let shown = vec![("proof".into(), "Document".into())];
        assert_eq!(
            DiscoveryDecision::for_presentation(&result, &shown),
            DiscoveryDecision::Clarify
        );
    }
}
