//! Decode once at the durable checkpoint. Each assessment carries the provider
//! answer and the host's disposition through receipt and teaching projection.
use super::references::References;
use super::*;
use crate::discovery_matcher::{CapabilityIntentMatch, MatchChoice};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Include,
    Exclude,
    Uncertain,
    UncertainUnit,
    UncertainOutput,
    UncertainConstraint,
    UncertainInput,
}

impl Verdict {
    fn gap(self) -> Option<&'static str> {
        match self {
            Self::UncertainUnit => Some("measurement unit"),
            Self::UncertainOutput => Some("output field or representation"),
            Self::UncertainConstraint => Some("scope, filter, condition or ordering"),
            Self::UncertainInput => Some("required input or binding"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Warning {
    Model,
    QualifiedKey,
    ParallelKey,
    /// Recorded by v33. Persisted receipts retain their original explanation.
    SharedIdentifier,
    Collection,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Teach,
    Warn(Warning),
    Defer,
    Exclude,
    Subsumed,
}

impl Disposition {
    pub(crate) fn teaches(self) -> bool {
        matches!(self, Self::Teach | Self::Warn(_))
    }
    pub(crate) fn uncertain(self) -> bool {
        matches!(self, Self::Defer | Self::Warn(_))
    }
    fn choice(self) -> Option<MatchChoice> {
        match self {
            Self::Teach => Some(MatchChoice::Relevant),
            Self::Warn(_) | Self::Defer => Some(MatchChoice::Uncertain),
            Self::Exclude => Some(MatchChoice::Unrelated),
            Self::Subsumed => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    pub verdict: Verdict,
    pub confidence: f64,
    pub disposition: Disposition,
}

pub(crate) struct Selection {
    pub(crate) assessments: BTreeMap<CapabilityRef, Assessment>,
    pub(crate) tokens: Option<u64>,
}

fn is_read(candidate: &RetrievedCapability) -> bool {
    matches!(
        candidate.document.operation.kind,
        plasm_core::CapabilityKind::Get
            | plasm_core::CapabilityKind::Query
            | plasm_core::CapabilityKind::Search
    )
}

impl WholeIntentPacket {
    pub(crate) fn decode(&self, raw: &str) -> Result<Selection, WholeIntentError> {
        let questions = (0..self.candidates.len())
            .map(|i| {
                (
                    format!("c{i}"),
                    vec![
                        "include",
                        "exclude",
                        "uncertain",
                        "uncertain_unit",
                        "uncertain_output",
                        "uncertain_constraint",
                        "uncertain_input",
                    ],
                )
            })
            .collect();
        let (answers, tokens) =
            crate::decision_codec::decode_with_usage(&self.model, &questions, raw)
                .map_err(|_| WholeIntentError::InvalidResponse)?;
        let mut assessments = BTreeMap::new();
        for (i, candidate) in self.candidates.iter().enumerate() {
            let answer = &answers[&format!("c{i}")];
            let verdict: Verdict = Verdict::deserialize(serde::de::value::StrDeserializer::<
                serde::de::value::Error,
            >::new(&answer.choice))
            .map_err(|_| WholeIntentError::InvalidResponse)?;
            let confidence = answer.confidence;
            let disposition = match verdict {
                Verdict::Include if confidence >= 0.5 => Disposition::Teach,
                Verdict::Exclude if confidence >= if is_read(candidate) { 0.60 } else { 0.50 } => {
                    Disposition::Exclude
                }
                Verdict::Exclude => Disposition::Defer,
                _ if is_read(candidate) => Disposition::Warn(Warning::Model),
                _ => Disposition::Defer,
            };
            assessments.insert(
                candidate.reference.clone(),
                Assessment {
                    verdict,
                    confidence,
                    disposition,
                },
            );
        }
        Ok(Selection {
            assessments,
            tokens,
        })
    }
}

impl DecisionPacket for WholeIntentPacket {
    type Response = Selection;
    fn cache_key(&self) -> &str {
        &self.cache_key
    }
    fn validate_response(
        &self,
        raw: &str,
    ) -> Result<Selection, Box<dyn std::error::Error + Send + Sync + 'static>> {
        Ok(self.decode(raw)?)
    }
}

impl Selection {
    pub(crate) fn apply(
        &mut self,
        candidates: &[RetrievedCapability],
        intent: &str,
        catalogs: &BTreeMap<String, CGS>,
        bindings: &DeploymentBindings,
    ) {
        let references = References::bind(intent, catalogs);
        let qualified = QualifiedSubjects::new(&references.plain);
        let terms = intent_terms(&references.plain);
        let positive: BTreeSet<_> = self
            .assessments
            .iter()
            .filter(|(_, a)| a.disposition == Disposition::Teach)
            .map(|(r, _)| r.clone())
            .collect();
        let mut selected = positive.clone();
        crate::discovery_matcher::read_subsumption::remove_subsumed(
            &mut selected,
            catalogs,
            bindings,
        );

        let mut numbered = BTreeMap::<_, Vec<_>>::new();
        let mut properties = BTreeMap::<_, BTreeSet<_>>::new();
        let mut collections = BTreeMap::<_, Vec<_>>::new();
        for c in candidates {
            if qualified.names_numbered_record(c) {
                numbered
                    .entry((&c.reference.catalog, &c.document.entity))
                    .or_default()
                    .push(&c.reference);
            }
            let verdict = self.assessments[&c.reference].verdict;
            if let Some(key) = record_identity(c, catalogs) {
                if is_read(c)
                    && matches!(verdict, Verdict::UncertainUnit | Verdict::UncertainOutput)
                {
                    properties
                        .entry(key.clone())
                        .or_default()
                        .insert(&c.document.entity);
                }
                if c.document.operation.kind == plasm_core::CapabilityKind::Query
                    && names_entity(&terms, &c.document.entity)
                {
                    collections.entry(key).or_default().push(c);
                }
            }
        }
        let numbered: BTreeSet<_> = numbered
            .values()
            .filter_map(|v| match v.as_slice() {
                [r] => Some(*r),
                _ => None,
            })
            .collect();
        let collections: BTreeSet<_> = collections
            .iter()
            .filter_map(|(k, v)| match v.as_slice() {
                [c] if properties
                    .get(k)
                    .is_some_and(|entities| entities.iter().any(|e| *e != &c.document.entity)) =>
                {
                    Some(&c.reference)
                }
                _ => None,
            })
            .collect();
        let mut parallel = BTreeSet::new();
        let mut corroborated = BTreeSet::new();
        for list in references.lists {
            if list.iter().any(|r| positive.contains(r)) {
                corroborated.extend(list.iter().cloned());
            }
            parallel.extend(list);
        }
        for c in candidates {
            let r = &c.reference;
            let a = self.assessments.get_mut(r).expect("bound candidate");
            a.disposition = match a.disposition {
                Disposition::Teach if !selected.contains(r) => Disposition::Subsumed,
                Disposition::Defer | Disposition::Exclude if collections.contains(r) => {
                    Disposition::Warn(Warning::Collection)
                }
                Disposition::Defer if parallel.contains(r) => {
                    Disposition::Warn(Warning::ParallelKey)
                }
                Disposition::Exclude if corroborated.contains(r) => {
                    Disposition::Warn(Warning::ParallelKey)
                }
                Disposition::Defer if numbered.contains(r) || references.keys.contains(r) => {
                    Disposition::Warn(Warning::QualifiedKey)
                }
                disposition => disposition,
            };
        }
    }

    pub(crate) fn matches(&self, candidates: &[RetrievedCapability]) -> Vec<CapabilityIntentMatch> {
        candidates
            .iter()
            .filter_map(|c| {
                let a = &self.assessments[&c.reference];
                Some(CapabilityIntentMatch {
                    capability_id: c.id.clone(),
                    choice: a.disposition.choice()?,
                    assessment: Some(a.clone()),
                })
            })
            .collect()
    }
    pub(crate) fn gaps(&self) -> Vec<String> {
        self.assessments
            .iter()
            .filter_map(|(r, a)| {
                Some(format!(
                    "{} for {}/{}",
                    a.verdict.gap()?,
                    r.catalog,
                    r.capability
                ))
            })
            .collect()
    }
}
