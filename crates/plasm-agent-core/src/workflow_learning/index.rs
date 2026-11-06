//! Bounded intent recall. Similarity proposes candidates; it never grants admission.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedIntent {
    pub purpose: String,
    pub facets: Vec<String>,
    pub aliases: Vec<String>,
}

pub(super) fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl NormalizedIntent {
    /// Provider-proposed facets must already distinguish guards, direction and
    /// preserved properties. Literal removal is not performed by this function.
    pub fn family_key(&self) -> String {
        let facets: BTreeSet<_> = self
            .facets
            .iter()
            .map(|s| normalize(s))
            .filter(|s| !s.is_empty())
            .collect();
        let bytes = serde_json::to_vec(&(normalize(&self.purpose), facets))
            .expect("intent strings serialize");
        hex::encode(Sha256::digest(bytes))
    }

    fn terms(&self) -> BTreeSet<String> {
        std::iter::once(&self.purpose)
            .chain(&self.facets)
            .chain(&self.aliases)
            .flat_map(|text| text.split_whitespace().map(str::to_lowercase))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub id: String,
    pub intent: NormalizedIntent,
    pub catalogs: BTreeSet<String>,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedMatch {
    pub id: String,
    pub score: f64,
}

fn cosine(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty()
        || left.len() != right.len()
        || left.iter().chain(right).any(|v| !v.is_finite())
    {
        return None;
    }
    let dot: f64 = left
        .iter()
        .zip(right)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum();
    let norm = |values: &[f32]| {
        values
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    let denom = norm(left) * norm(right);
    (denom > 0.0).then(|| (dot / denom).clamp(-1.0, 1.0))
}

/// Input is a caller-bounded, tenant-scoped active index snapshot. Provider
/// embeddings are optional; only compatible finite vectors contribute.
pub fn rank(
    intent: &NormalizedIntent,
    query_embedding: Option<&[f32]>,
    allowed_catalogs: &BTreeSet<String>,
    requested_catalog: Option<&str>,
    entries: &[IndexEntry],
    limit: usize,
) -> Vec<RankedMatch> {
    let terms = intent.terms();
    let mut matches: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            if !entry.catalogs.is_subset(allowed_catalogs)
                || requested_catalog.is_some_and(|catalog| !entry.catalogs.contains(catalog))
            {
                return None;
            }
            let other = entry.intent.terms();
            let union = terms.union(&other).count();
            let lexical = if union == 0 {
                0.0
            } else {
                terms.intersection(&other).count() as f64 / union as f64
            };
            let semantic = query_embedding
                .zip(entry.embedding.as_deref())
                .and_then(|(a, b)| cosine(a, b));
            let score = semantic.map_or(lexical, |s| 0.5 * lexical + 0.5 * s.max(0.0));
            (score > 0.0).then(|| RankedMatch {
                id: entry.id.clone(),
                score,
            })
        })
        .collect();
    matches.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    matches.truncate(limit);
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    fn intent() -> NormalizedIntent {
        NormalizedIntent {
            purpose: "pay person".into(),
            facets: vec!["outgoing".into()],
            aliases: vec![],
        }
    }
    #[test]
    fn family_ignores_aliases_and_facet_order_but_preserves_conditions() {
        let a = intent();
        let mut b = a.clone();
        b.aliases.push("send funds".into());
        assert_eq!(a.family_key(), b.family_key());
        b.facets.push("only if balance positive".into());
        assert_ne!(a.family_key(), b.family_key());
    }
    #[test]
    fn catalog_permissions_precede_similarity_and_limits() {
        let a = IndexEntry {
            id: "a".into(),
            intent: intent(),
            catalogs: ["private".into()].into(),
            embedding: Some(vec![1.0]),
        };
        let b = IndexEntry {
            id: "b".into(),
            catalogs: ["allowed".into()].into(),
            ..a.clone()
        };
        let rows = rank(
            &intent(),
            Some(&[1.0]),
            &["allowed".into()].into(),
            None,
            &[a, b],
            1,
        );
        assert_eq!(rows[0].id, "b");
    }
    #[test]
    fn invalid_embeddings_cannot_poison_ordering() {
        assert_eq!(cosine(&[f32::NAN], &[1.0]), None);
        assert_eq!(cosine(&[0.0], &[1.0]), None);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), None);
    }
}
