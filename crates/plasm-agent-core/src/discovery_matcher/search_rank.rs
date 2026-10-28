//! Deterministic fielded lexical ranking. Authorization precedes construction.
use std::collections::{BTreeMap, BTreeSet};

const WEIGHTS: [f64; 3] = [2.0, 1.0, 0.5];
const K1: f64 = 1.2;
const B: f64 = 0.75;
const RRF: f64 = 60.0;

/// Field order: operation/effect, output/entity, input descriptions.
pub(super) struct RankedDocument<K> {
    pub key: K,
    pub fields: [String; 3],
}

pub(super) struct LexicalIndex<K: Ord> {
    lengths: BTreeMap<K, [usize; 3]>,
    postings: BTreeMap<String, BTreeMap<K, [usize; 3]>>,
    averages: [f64; 3],
}

fn terms(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
}

impl<K: Ord + Clone> LexicalIndex<K> {
    /// Reject conflicting identities instead of silently merging catalogues.
    pub fn new(documents: impl IntoIterator<Item = RankedDocument<K>>) -> Result<Self, K> {
        let mut index = Self {
            lengths: BTreeMap::new(),
            postings: BTreeMap::new(),
            averages: [0.0; 3],
        };
        for document in documents {
            if index.lengths.contains_key(&document.key) {
                return Err(document.key);
            }
            let mut lengths = [0; 3];
            for (field, text) in document.fields.iter().enumerate() {
                for term in terms(text) {
                    lengths[field] += 1;
                    index
                        .postings
                        .entry(term)
                        .or_default()
                        .entry(document.key.clone())
                        .or_insert([0; 3])[field] += 1;
                }
            }
            for (field, length) in lengths.iter().enumerate() {
                index.averages[field] += *length as f64;
            }
            index.lengths.insert(document.key, lengths);
        }
        let count = index.lengths.len().max(1) as f64;
        for average in &mut index.averages {
            *average = (*average / count).max(1.0);
        }
        Ok(index)
    }

    fn query(&self, query: &str) -> Vec<K> {
        let mut scores: BTreeMap<K, f64> = BTreeMap::new();
        for term in terms(query).collect::<BTreeSet<_>>() {
            let Some(posting) = self.postings.get(&term) else {
                continue;
            };
            let n = self.lengths.len() as f64;
            let df = posting.len() as f64;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for (key, frequencies) in posting {
                let length = self.lengths[key];
                let weighted: f64 = (0..3)
                    .map(|field| {
                        WEIGHTS[field] * frequencies[field] as f64
                            / (1.0 - B + B * length[field] as f64 / self.averages[field])
                    })
                    .sum();
                *scores.entry(key.clone()).or_default() +=
                    idf * weighted * (K1 + 1.0) / (K1 + weighted);
            }
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then_with(|| a.cmp(b)));
        ranked.into_iter().map(|(key, _)| key).collect()
    }

    /// Separate queries, deduplicated before RRF; zero lexical matches are not
    /// invented matches. They remain eligible for typed relationship nomination.
    pub fn rank(&self, outcome: &str, need: &str) -> Vec<K> {
        let queries: BTreeSet<_> = [outcome, need]
            .into_iter()
            .map(|query| terms(query).collect::<BTreeSet<_>>())
            .collect();
        let mut scores = BTreeMap::<K, f64>::new();
        for query in queries {
            let text = query.into_iter().collect::<Vec<_>>().join(" ");
            for (rank, key) in self.query(&text).into_iter().enumerate() {
                *scores.entry(key).or_default() += 1.0 / (RRF + rank as f64 + 1.0);
            }
        }
        let mut ranked: Vec<_> = scores.into_iter().collect();
        ranked.sort_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then_with(|| a.cmp(b)));
        ranked.into_iter().map(|(key, _)| key).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(key: &str, fields: [&str; 3]) -> RankedDocument<String> {
        RankedDocument {
            key: key.into(),
            fields: fields.map(str::to_owned),
        }
    }

    #[test]
    fn field_roles_and_qualified_ties_are_stable_under_permutation() {
        let make = || {
            vec![
                document("a/create", ["produce spectrum", "result", "input"]),
                document("b/create", ["produce spectrum", "result", "input"]),
                document("c/query", ["inspect archive", "result", "spectrum"]),
            ]
        };
        let forward = LexicalIndex::new(make()).ok().unwrap();
        let reverse = LexicalIndex::new(make().into_iter().rev()).ok().unwrap();
        assert_eq!(
            forward.rank("spectrum", "spectrum"),
            vec!["a/create", "b/create", "c/query"]
        );
        assert_eq!(
            forward.rank("spectrum", "archive"),
            reverse.rank("spectrum", "archive")
        );
        assert_eq!(
            forward.rank("spectrum", "spectrum"),
            forward.rank("spectrum", "SPECTRUM spectrum")
        );
    }

    #[test]
    fn neutral_growth_does_not_consume_model_work_or_invent_lexical_hits() {
        for count in [0, 100, 1_000, 10_000] {
            let documents =
                std::iter::once(document("target", ["produce spectrum", "result", "input"])).chain(
                    (0..count).map(|i| {
                        document(
                            &format!("neutral/{i}"),
                            ["archive weather", "record", "date"],
                        )
                    }),
                );
            let index = LexicalIndex::new(documents).ok().unwrap();
            assert_eq!(index.rank("spectrum", "spectrum"), vec!["target"]);
            assert!(index.rank("unmentioned", "unknown").is_empty());
        }
    }

    #[test]
    fn input_nomination_must_not_treat_primitive_similarity_as_a_lexical_hit() {
        let index = LexicalIndex::new([document(
            "opaque",
            ["unspecified operation", "opaque", "string"],
        )])
        .ok()
        .unwrap();
        assert!(index
            .rank("publish spectrum", "publish spectrum")
            .is_empty());
        // This is not an unrelated judgment: typed reachability is a separate phase.
        assert_eq!(index.lengths.len(), 1);
    }
}
