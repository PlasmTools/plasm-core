//! Request-bound operations and validated provider proposals.
//!
//! These values describe request coverage and judgments, not authority to expose or execute
//! a capability. Operation identifiers are scoped to the exact intent revision.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_OPERATIONS: usize = 513;
const MAX_OPERATION_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestedOperation {
    id: OperationId,
    /// Exact text from the authorized request, not an invented paraphrase.
    text: String,
}

impl RequestedOperation {
    pub fn id(&self) -> &OperationId {
        &self.id
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationSet {
    intent_digest: String,
    operations: Vec<RequestedOperation>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OperationError {
    #[error("intent must not be empty")]
    EmptyIntent,
    #[error("operation proposal count is outside the permitted bounds")]
    InvalidCount,
    #[error("operation proposal is empty, oversized, or not an exact intent excerpt")]
    InvalidExcerpt,
    #[error("operation proposal repeats an excerpt")]
    DuplicateExcerpt,
    #[error("operation set belongs to another intent revision")]
    IntentMismatch,
    #[error("judgment names an operation or capability outside the issued question set")]
    UnknownQuestion,
    #[error("provider returned a duplicate operation-capability judgment")]
    DuplicateJudgment,
    #[error("provider did not answer every issued operation-capability question")]
    IncompleteJudgments,
    #[error("request coverage contains too many fragments")]
    RequestTooLarge,
    #[error("operation decision packet is invalid")]
    InvalidDecisionPacket,
    #[error("operation decision references an invalid connected catalogue witness")]
    InvalidGraphWitness,
    #[error("operation decision packet requires {bytes} bytes; limit is {limit}")]
    DecisionPacketTooLarge { bytes: usize, limit: usize },
    #[error("operation decision response is invalid")]
    InvalidDecisionResponse,
    #[error("operation relevance requires the configured Luna Decisions model")]
    UnsupportedDecisionModel,
    #[error("operation matrix contains an unauthorized or duplicate candidate")]
    InvalidCandidates,
    #[error("operation decision packet exceeds the permitted question count")]
    InvalidPacketSize,
    #[error("an operation-capability pair has already been judged")]
    AlreadyJudged,
    #[error("operation matrix belongs to another session, generation or authorization")]
    ScopeMismatch,
}

/// A single authorized, catalogue-qualified question. The contract comes from
/// the pinned catalogue; the provider cannot invent capability identities.
#[derive(Debug, Clone)]
pub(crate) struct DecisionQuestion {
    pub operation: OperationId,
    pub capability: plasm_core::prerequisites::CapabilityRef,
}

#[derive(Clone)]
pub struct DecisionBatch {
    model: String,
    body: String,
    cache_key: String,
    scope: MatrixScope,
    bindings: BTreeMap<String, (OperationId, plasm_core::prerequisites::CapabilityRef)>,
    contextual_questions: BTreeSet<String>,
    members: BTreeSet<plasm_core::prerequisites::CapabilityRef>,
}

#[cfg(test)]
#[derive(Serialize)]
pub(super) struct DecisionState<'a> {
    intent: &'a str,
    operations: &'a BTreeMap<String, String>,
    capabilities: &'a BTreeMap<String, CapabilityWitness<'a>>,
}

/// Each question carries the same projected, authorized graph evidence used by
/// ordinary discovery. A route is evidence for composition, not permission.
#[derive(Serialize)]
pub(super) struct GraphDecisionState<'a> {
    intent: &'a str,
    guidance: &'static str,
    operations: BTreeMap<String, &'a str>,
    question_evidence: BTreeMap<String, GraphWitness>,
}

#[derive(Serialize)]
struct GraphWitness {
    capabilities: super::witnesses::OrderedCards,
    types: super::witnesses::ProjectedDefinitions,
    type_refs: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bindings: Vec<serde_json::Value>,
}

/// One planning wave can try several packet sizes; project each exact route once.
pub(super) type GraphWitnessCache = BTreeMap<
    (
        plasm_core::prerequisites::CapabilityRef,
        Vec<super::search_graph::Edge<plasm_core::prerequisites::CapabilityRef>>,
    ),
    (
        super::witnesses::Scope,
        BTreeSet<plasm_core::prerequisites::CapabilityRef>,
    ),
>;

#[cfg(test)]
#[derive(Serialize)]
struct CapabilityWitness<'a> {
    reference: &'a plasm_core::prerequisites::CapabilityRef,
    operation: &'a plasm_core::catalog_discovery::OperationEvidence,
    collection: &'a plasm_core::catalog_discovery::CollectionEvidence,
}

impl DecisionBatch {
    fn validate_questions(
        model: &str,
        intent: &str,
        operations: &OperationSet,
        scope: &MatrixScope,
        authorized: &crate::discovery_store::DiscoveryAuthorization,
        index: &super::catalogue_search::CatalogueSearch,
        questions: &[DecisionQuestion],
    ) -> Result<
        BTreeMap<String, (OperationId, plasm_core::prerequisites::CapabilityRef)>,
        OperationError,
    > {
        operations.validate_revision(intent)?;
        let permitted: BTreeSet<_> = index
            .candidates
            .keys()
            .filter(|reference| authorized.permits(reference))
            .cloned()
            .collect();
        if scope.authorization_digest != authorization_digest(&permitted)
            || scope.request.intent_digest != operations.intent_digest
        {
            return Err(OperationError::ScopeMismatch);
        }
        if !crate::decision_codec::model_matches(super::RELEVANCE_MODEL, model) {
            return Err(OperationError::UnsupportedDecisionModel);
        }
        if questions.is_empty() || questions.len() > super::request_packing::MAX_QUESTIONS {
            return Err(OperationError::InvalidDecisionPacket);
        }
        let known: BTreeSet<_> = operations.operations.iter().map(|item| &item.id).collect();
        let mut unique = BTreeSet::new();
        let mut bindings = BTreeMap::new();
        for (ordinal, question) in questions.iter().enumerate() {
            let pair = (question.operation.clone(), question.capability.clone());
            if !known.contains(&question.operation)
                || !authorized.permits(&question.capability)
                || !index.candidates.contains_key(&question.capability)
                || !unique.insert(pair.clone())
            {
                return Err(OperationError::InvalidDecisionPacket);
            }
            bindings.insert(format!("q{}", ordinal + 1), pair);
        }
        Ok(bindings)
    }
    /// Bind operation questions to the connected planner's exact paths. The
    /// flat constructor checks the common identity and authorization contract;
    /// this constructor replaces its evidence with the planner's typed graph
    /// projection before a packet is issued to the provider.
    pub(super) fn from_graph(
        context: super::online::SearchContext<'_>,
        operations: &OperationSet,
        scope: &MatrixScope,
        index: &super::catalogue_search::CatalogueSearch,
        wave: &[(
            plasm_core::prerequisites::CapabilityRef,
            Vec<super::search_graph::Edge<plasm_core::prerequisites::CapabilityRef>>,
        )],
        questions: &[DecisionQuestion],
        witness_cache: &mut GraphWitnessCache,
    ) -> Result<Self, OperationError> {
        let selection_request = context.intent.selection_request();
        let intent = selection_request.as_str();
        if scope.generation != context.generation {
            return Err(OperationError::ScopeMismatch);
        }
        let bindings = Self::validate_questions(
            context.model,
            intent,
            operations,
            scope,
            context.allowed,
            index,
            questions,
        )?;
        let operation_labels: BTreeMap<_, _> = operations
            .operations
            .iter()
            .enumerate()
            .map(|(n, operation)| (operation.id.clone(), format!("o{}", n + 1)))
            .collect();
        let wire_operations = operations
            .operations
            .iter()
            .filter(|operation| {
                questions
                    .iter()
                    .any(|question| question.operation == operation.id)
            })
            .map(|operation| {
                let text = if operation.text == intent {
                    "The complete request in intent"
                } else {
                    operation.text.as_str()
                };
                (operation_labels[&operation.id].clone(), text)
            })
            .collect();
        let mut evidence = BTreeMap::new();
        let mut wire_questions = BTreeMap::new();
        let mut contextual_questions = BTreeSet::new();
        let mut members = BTreeSet::new();
        let mut witness_labels: BTreeMap<plasm_core::prerequisites::CapabilityRef, String> =
            BTreeMap::new();
        for (n, question) in questions.iter().enumerate() {
            let key = format!("q{}", n + 1);
            let (_, path) = wave
                .iter()
                .find(|(reference, _)| reference == &question.capability)
                .ok_or(OperationError::InvalidGraphWitness)?;
            let target = index
                .labels
                .get(&question.capability)
                .ok_or(OperationError::InvalidGraphWitness)?;
            let witness_label = if let Some(label) = witness_labels.get(&question.capability) {
                label.clone()
            } else {
                let label = format!("w{}", witness_labels.len() + 1);
                let cache_key = (question.capability.clone(), path.clone());
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    witness_cache.entry(cache_key.clone())
                {
                    let (witness, _, scope_members) =
                        super::online::packet_scope(index, &question.capability, path, context)
                            .map_err(|_| OperationError::InvalidGraphWitness)?;
                    entry.insert((witness, scope_members));
                }
                let (witness, scope_members) = &witness_cache[&cache_key];
                members.extend(scope_members.iter().cloned());
                evidence.insert(
                    label.clone(),
                    GraphWitness {
                        capabilities: witness.cards.clone(),
                        types: witness.types.clone(),
                        type_refs: witness.references.clone(),
                        bindings: witness.bindings.clone(),
                    },
                );
                witness_labels.insert(question.capability.clone(), label.clone());
                label
            };
            let is_fragment = question.operation != operations.operations[0].id;
            let mut criteria = BTreeMap::from([
                ("relevant", "Contributes to the active requirement."),
                ("unrelated", "Does not contribute."),
                ("uncertain", "Contract or active status is uncertain."),
            ]);
            let instructions = format!(
                "Classify `question_evidence.{witness_label}.capabilities.{target}` for operation `{}` using state.guidance.",
                operation_labels[&question.operation]
            );
            if is_fragment {
                contextual_questions.insert(key.clone());
                criteria.insert("contextual", "Clearly not an active requirement.");
            }
            wire_questions.insert(
                key.clone(),
                super::decisions::Question {
                    kind: "choice",
                    instructions,
                    criteria,
                },
            );
        }
        let state = GraphDecisionState {
            intent,
            guidance: "Classify each capability against the complete ordered request and named operation. Respect revisions, constraints and effect direction. A component of a composition or a requested relationship, membership, ownership or target-selection condition may contribute. Do not invent relationships; authentication and transport prerequisites alone are not business contributions. For fragments, choose contextual only for commentary, superseded requirements or other text that clearly does not independently request a current action or selection condition; when unsure, choose uncertain. The complete request remains authoritative.",
            operations: wire_operations,
            question_evidence: evidence,
        };
        use crate::decision_codec::DecisionCodec;
        let body = super::RelevanceCodec::encode(&super::decisions::Request {
            model: context.model,
            state: super::decisions::State::OperationGraph(&state),
            questions: wire_questions,
        })
        .map_err(|_| OperationError::InvalidDecisionPacket)?;
        if body.len() > super::search_budget::MAX_INPUT_BYTES {
            return Err(OperationError::DecisionPacketTooLarge {
                bytes: body.len(),
                limit: super::search_budget::MAX_INPUT_BYTES,
            });
        }
        let cache_key = plasm_core::catalog_discovery::content_hash(
            &serde_json::to_vec(&("operation-graph-decisions-v1", scope, &body))
                .map_err(|_| OperationError::InvalidDecisionPacket)?,
        );
        Ok(Self {
            model: context.model.into(),
            body,
            cache_key,
            scope: scope.clone(),
            bindings,
            contextual_questions,
            members,
        })
    }

    #[cfg(test)]
    pub(crate) fn new(
        model: &str,
        intent: &str,
        operations: &OperationSet,
        scope: &MatrixScope,
        authorized: &crate::discovery_store::DiscoveryAuthorization,
        index: &super::catalogue_search::CatalogueSearch,
        questions: &[DecisionQuestion],
    ) -> Result<Self, OperationError> {
        let bindings = Self::validate_questions(
            model, intent, operations, scope, authorized, index, questions,
        )?;
        let mut wire_questions = BTreeMap::new();
        let operation_labels: BTreeMap<_, _> = operations
            .operations
            .iter()
            .enumerate()
            .map(|(index, operation)| (operation.id.clone(), format!("o{}", index + 1)))
            .collect();
        let wire_operations: BTreeMap<_, _> = operations
            .operations
            .iter()
            .map(|operation| {
                (
                    operation_labels[&operation.id].clone(),
                    operation.text.clone(),
                )
            })
            .collect();
        let mut capability_labels: BTreeMap<plasm_core::prerequisites::CapabilityRef, String> =
            BTreeMap::new();
        let mut wire_capabilities: BTreeMap<String, CapabilityWitness<'_>> = BTreeMap::new();
        for (ordinal, question) in questions.iter().enumerate() {
            let candidate = index
                .candidates
                .get(&question.capability)
                .ok_or(OperationError::InvalidDecisionPacket)?;
            let key = format!("q{}", ordinal + 1);
            let capability_label = if let Some(label) = capability_labels.get(&question.capability)
            {
                label.clone()
            } else {
                let label = format!("c{}", capability_labels.len() + 1);
                capability_labels.insert(question.capability.clone(), label.clone());
                wire_capabilities.insert(
                    label.clone(),
                    CapabilityWitness {
                        reference: &question.capability,
                        operation: &candidate.document.operation,
                        collection: &candidate.document.collection,
                    },
                );
                label
            };
            wire_questions.insert(key, super::decisions::Question {
                kind: "choice",
                instructions: format!("Classify whether capability `{capability_label}` contributes to operation `{}`. A capability may perform one necessary component of a composed operation without completing it alone. A prerequisite alone is not a business contribution. Respect effect direction and the complete request.", operation_labels[&question.operation]),
                criteria: BTreeMap::from([
                    ("relevant", "Its documented information, output, or effect performs the requested operation, establishes a requested target-selection condition, or contributes a needed component of a plausible composition."),
                    ("unrelated", "Its documented purpose or effect does not contribute to the request, including an unrequested destructive effect."),
                    ("uncertain", "The documented contract does not establish the contribution."),
                ]),
            });
        }
        let state = DecisionState {
            intent,
            operations: &wire_operations,
            capabilities: &wire_capabilities,
        };
        use crate::decision_codec::DecisionCodec;
        let body = super::RelevanceCodec::encode(&super::decisions::Request {
            model,
            state: super::decisions::State::Operation(&state),
            questions: wire_questions,
        })
        .map_err(|_| OperationError::InvalidDecisionPacket)?;
        if body.len() > super::search_budget::MAX_INPUT_BYTES {
            return Err(OperationError::DecisionPacketTooLarge {
                bytes: body.len(),
                limit: super::search_budget::MAX_INPUT_BYTES,
            });
        }
        Ok(Self {
            model: model.into(),
            cache_key: plasm_core::catalog_discovery::content_hash(
                &serde_json::to_vec(&("operation-decisions-v2", scope, &body))
                    .map_err(|_| OperationError::InvalidDecisionPacket)?,
            ),
            body,
            scope: scope.clone(),
            bindings,
            contextual_questions: BTreeSet::new(),
            members: BTreeSet::new(),
        })
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    pub(super) fn members(&self) -> &BTreeSet<plasm_core::prerequisites::CapabilityRef> {
        &self.members
    }

    pub(crate) fn issued_pairs(&self) -> IssuedPairs {
        IssuedPairs {
            scope: self.scope.clone(),
            pairs: self.bindings.values().cloned().collect(),
        }
    }

    pub(crate) fn decode(&self, raw: &str) -> Result<Vec<OperationJudgment>, OperationError> {
        let choices: BTreeMap<String, Vec<&str>> = self
            .bindings
            .keys()
            .map(|key| {
                let mut values = vec!["relevant", "unrelated", "uncertain"];
                if self.contextual_questions.contains(key) {
                    values.push("contextual");
                }
                (key.clone(), values)
            })
            .collect();
        let mut answers = crate::decision_codec::decode_choice_sets(&self.model, &choices, raw)
            .map_err(|_| OperationError::InvalidDecisionResponse)?;
        let mut judgments = Vec::with_capacity(self.bindings.len());
        for (key, (operation, capability)) in &self.bindings {
            let answer = answers
                .remove(key)
                .ok_or(OperationError::InvalidDecisionResponse)?;
            let contribution = match answer.choice.as_str() {
                "relevant" => DirectContribution::Relevant,
                "unrelated" => DirectContribution::Unrelated,
                "uncertain" => DirectContribution::Uncertain,
                "contextual" => DirectContribution::Contextual,
                _ => return Err(OperationError::InvalidDecisionResponse),
            };
            judgments.push(OperationJudgment {
                operation: operation.clone(),
                capability: capability.clone(),
                contribution,
            });
        }
        let issued = self.bindings.values().cloned().collect();
        validate_judgments(&issued, &judgments)?;
        Ok(judgments)
    }
}

impl crate::decision_codec::DecisionPacket for DecisionBatch {
    fn cache_key(&self) -> &str {
        &self.cache_key
    }

    fn validate_response(
        &self,
        raw: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.decode(raw)
            .map(|_| ())
            .map_err(|error| Box::new(error) as _)
    }
}

/// Identity of the complete ordered request used by a discovery revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestIdentity {
    intent_digest: String,
}

impl RequestIdentity {
    pub(crate) fn from_request(intent: &str) -> Self {
        Self {
            intent_digest: plasm_core::catalog_discovery::content_hash(intent.as_bytes()),
        }
    }
}

/// Deterministically issue source spans; the model selects identifiers and
/// cannot alter the text used by downstream judgment.
fn source_spans(intent: &str) -> Result<BTreeMap<String, String>, OperationError> {
    let intent = intent
        .strip_prefix(crate::intent_provenance::SELECTION_HISTORY_HEADER)
        .unwrap_or(intent);
    let mut spans = Vec::<String>::new();
    let mut start = 0;
    let mut boundaries = Vec::new();
    for (offset, ch) in intent.char_indices() {
        let next = offset + ch.len_utf8();
        if ch == ';'
            || ch == '\n'
            || (ch == '.'
                && intent[next..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace))
        {
            boundaries.push(next);
        }
    }
    boundaries.push(intent.len());
    for end in boundaries {
        let clause = intent[start..end].trim();
        start = end;
        if clause.is_empty() {
            continue;
        }
        let parts: Vec<&str> = if clause.len() > MAX_OPERATION_BYTES {
            vec![clause]
        } else {
            clause.split(" and ").collect()
        };
        for part in &parts {
            let mut rest = part.trim();
            while !rest.is_empty() {
                let mut boundary = rest.len().min(MAX_OPERATION_BYTES);
                while !rest.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                if boundary < rest.len() {
                    if let Some(split) = rest[..boundary].rfind(char::is_whitespace) {
                        if split > 0 {
                            boundary = split;
                        }
                    }
                }
                let text = rest[..boundary].trim();
                if !text.is_empty() {
                    spans.push(text.to_owned());
                }
                rest = rest[boundary..].trim_start();
            }
        }
        if parts.len() > 1 && clause.len() <= MAX_OPERATION_BYTES {
            spans.push(clause.to_owned());
        }
        if spans.len() > 512 {
            return Err(OperationError::RequestTooLarge);
        }
    }
    if spans.is_empty() {
        return Err(OperationError::EmptyIntent);
    }
    Ok(spans
        .into_iter()
        .enumerate()
        .map(|(i, text)| (format!("s{i}"), text))
        .collect())
}

/// The model judges only direct contribution. Required acquisitions are
/// resolved independently by the authoritative catalogue graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectContribution {
    Relevant,
    Unrelated,
    Uncertain,
    Contextual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationJudgment {
    pub operation: OperationId,
    pub capability: plasm_core::prerequisites::CapabilityRef,
    pub contribution: DirectContribution,
}

/// Exact pairs admitted to a provider packet. Only the packet builder can
/// construct this token during live operation.
pub struct IssuedPairs {
    scope: MatrixScope,
    pairs: BTreeSet<(OperationId, plasm_core::prerequisites::CapabilityRef)>,
}

/// Bind provider responses to the exact issued question set. Packet deferral is
/// represented by questions not issued, never as a negative model judgment.
pub fn validate_judgments(
    issued: &BTreeSet<(OperationId, plasm_core::prerequisites::CapabilityRef)>,
    judgments: &[OperationJudgment],
) -> Result<(), OperationError> {
    let mut answered = BTreeSet::new();
    for judgment in judgments {
        let key = (judgment.operation.clone(), judgment.capability.clone());
        if !issued.contains(&key) {
            return Err(OperationError::UnknownQuestion);
        }
        if !answered.insert(key) {
            return Err(OperationError::DuplicateJudgment);
        }
    }
    if answered != *issued {
        return Err(OperationError::IncompleteJudgments);
    }
    Ok(())
}

/// The evidence accumulated for a candidate. An incomplete matrix cannot be
/// mistaken for an unrelated judgment, and uncertainty remains explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateProgress {
    Relevant {
        operations: BTreeSet<OperationId>,
        pending: BTreeSet<OperationId>,
        uncertain: BTreeSet<OperationId>,
    },
    Incomplete {
        pending: BTreeSet<OperationId>,
        uncertain: BTreeSet<OperationId>,
    },
    Uncertain {
        operations: BTreeSet<OperationId>,
    },
    Unrelated,
}

/// Per-intent operation/candidate work. This owns scheduling and judgment
/// accounting only; catalogue authorization and closure remain host decisions.
pub struct OperationMatrix {
    scope: MatrixScope,
    operations: OperationSet,
    candidates: Vec<plasm_core::prerequisites::CapabilityRef>,
    answers: BTreeMap<(OperationId, plasm_core::prerequisites::CapabilityRef), DirectContribution>,
    /// Request-local retrieval ranks guide scheduling; they are not judgments.
    recall_ranks: BTreeMap<(OperationId, plasm_core::prerequisites::CapabilityRef), usize>,
    /// Current-turn priority does not change historical obligations or judgments.
    current_operations: BTreeSet<OperationId>,
}

/// Identity of the trusted discovery session and its immutable catalogue and
/// authorization view. It is kept out of the model packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixScope {
    session: String,
    generation: String,
    authorization_digest: String,
    request: RequestIdentity,
}

impl MatrixScope {
    pub fn new(
        session: impl Into<String>,
        generation: impl Into<String>,
        authorized: &BTreeSet<plasm_core::prerequisites::CapabilityRef>,
        request: RequestIdentity,
    ) -> Result<Self, OperationError> {
        let session = session.into();
        let generation = generation.into();
        if session.trim().is_empty() || generation.trim().is_empty() {
            return Err(OperationError::ScopeMismatch);
        }
        Ok(Self {
            session,
            generation,
            authorization_digest: authorization_digest(authorized),
            request,
        })
    }

    /// A changed request identity is an ordinary intent revision;
    /// session, generation and authorization changes are different scopes.
    pub(crate) fn same_host_scope(&self, other: &Self) -> bool {
        self.session == other.session
            && self.generation == other.generation
            && self.authorization_digest == other.authorization_digest
    }
}

fn authorization_digest(authorized: &BTreeSet<plasm_core::prerequisites::CapabilityRef>) -> String {
    plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(authorized).expect("capability references serialize"),
    )
}

/// Persistable evidence for one exact request revision. Restoring it always
/// revalidates the operation identities, authorization and answer bindings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixCheckpoint {
    scope: MatrixScope,
    operations: OperationSet,
    candidates: Vec<plasm_core::prerequisites::CapabilityRef>,
    answers: Vec<OperationJudgment>,
}

impl MatrixCheckpoint {
    pub(crate) fn scope(&self) -> &MatrixScope {
        &self.scope
    }

    pub(crate) fn answered_pairs(
        &self,
    ) -> BTreeSet<(OperationId, plasm_core::prerequisites::CapabilityRef)> {
        self.answers
            .iter()
            .map(|answer| (answer.operation.clone(), answer.capability.clone()))
            .collect()
    }
}

impl OperationMatrix {
    /// Scheduling signal only: a provider judgment does not authorize exposure.
    /// Request fragments without an affirmative, authorized contribution judgment.
    /// This is bounded search evidence, not proof that no capability exists.
    pub(crate) fn uncovered_operations(&self) -> impl Iterator<Item = &str> {
        let has_fragments = self.operations.operations.len() > 1;
        self.operations
            .operations
            .iter()
            .enumerate()
            .filter(move |(index, operation)| {
                if *index == 0 {
                    // Whole-request relevance is not independent coverage evidence.
                    return !has_fragments;
                }
                let primary = &self.operations.operations[0].id;
                !self.answers.iter().any(|((id, candidate), contribution)| {
                    id == &operation.id
                        && *contribution == DirectContribution::Relevant
                        && self.answers.get(&(primary.clone(), candidate.clone()))
                            == Some(&DirectContribution::Relevant)
                })
            })
            .map(|(_, operation)| operation.text.as_str())
    }
    pub(crate) fn contextual_fragment(&self, text: &str) -> bool {
        self.operations
            .operations
            .iter()
            .skip(1)
            .filter(|op| op.text == text)
            .any(|op| {
                let mut contextual = false;
                for ((id, _), value) in &self.answers {
                    if id == &op.id {
                        if *value != DirectContribution::Contextual {
                            return false;
                        }
                        contextual = true;
                    }
                }
                contextual
            })
    }
    pub(crate) fn has_contextual_fragments(&self) -> bool {
        self.operations
            .operations
            .iter()
            .skip(1)
            .any(|op| self.contextual_fragment(&op.text))
    }
    pub(super) fn scope(&self) -> &MatrixScope {
        &self.scope
    }

    pub(crate) fn operations(&self) -> &OperationSet {
        &self.operations
    }

    pub fn new(
        scope: MatrixScope,
        intent: &str,
        operations: OperationSet,
        authorized: &BTreeSet<plasm_core::prerequisites::CapabilityRef>,
        candidates: Vec<plasm_core::prerequisites::CapabilityRef>,
    ) -> Result<Self, OperationError> {
        operations.validate_revision(intent)?;
        if operations
            .operations
            .first()
            .is_none_or(|operation| operation.text != intent)
        {
            return Err(OperationError::IntentMismatch);
        }
        if scope.authorization_digest != authorization_digest(authorized)
            || scope.request.intent_digest != operations.intent_digest
            || scope.session.trim().is_empty()
            || scope.generation.trim().is_empty()
        {
            return Err(OperationError::ScopeMismatch);
        }
        let unique: BTreeSet<_> = candidates.iter().collect();
        if unique.len() != candidates.len() || !candidates.iter().all(|c| authorized.contains(c)) {
            return Err(OperationError::InvalidCandidates);
        }
        Ok(Self {
            scope,
            operations,
            candidates,
            answers: BTreeMap::new(),
            recall_ranks: BTreeMap::new(),
            current_operations: BTreeSet::new(),
        })
    }

    pub(crate) fn prioritize_current_request(&mut self, current: &str) {
        self.current_operations = self
            .operations
            .operations
            .iter()
            .filter(|operation| {
                crate::intent_provenance::current_request_contains_span(current, &operation.text)
            })
            .map(|operation| operation.id.clone())
            .collect();
    }

    /// Install authorized, revision-bound operation recall ranks. These ranks
    /// nominate questions only; they cannot authorize or select a capability.
    pub(crate) fn set_recall_ranks(
        &mut self,
        authorized: &BTreeSet<plasm_core::prerequisites::CapabilityRef>,
        ranks: BTreeMap<(OperationId, plasm_core::prerequisites::CapabilityRef), usize>,
    ) -> Result<(), OperationError> {
        if self.scope.authorization_digest != authorization_digest(authorized) {
            return Err(OperationError::ScopeMismatch);
        }
        if ranks.keys().any(|(operation, capability)| {
            !authorized.contains(capability)
                || !self
                    .operations
                    .operations
                    .iter()
                    .any(|item| &item.id == operation)
        }) {
            return Err(OperationError::InvalidCandidates);
        }
        self.recall_ranks = ranks;
        Ok(())
    }

    pub fn checkpoint(&self) -> MatrixCheckpoint {
        MatrixCheckpoint {
            scope: self.scope.clone(),
            operations: self.operations.clone(),
            candidates: self.candidates.clone(),
            answers: self
                .answers
                .iter()
                .map(
                    |((operation, capability), contribution)| OperationJudgment {
                        operation: operation.clone(),
                        capability: capability.clone(),
                        contribution: *contribution,
                    },
                )
                .collect(),
        }
    }

    /// Add newly discovered candidates without losing prior judgments. All
    /// existing and new references must still be authorized under this request.
    pub fn add_candidates(
        &mut self,
        authorized: &BTreeSet<plasm_core::prerequisites::CapabilityRef>,
        incoming: &[plasm_core::prerequisites::CapabilityRef],
    ) -> Result<(), OperationError> {
        if self.scope.authorization_digest != authorization_digest(authorized) {
            return Err(OperationError::ScopeMismatch);
        }
        if !self
            .candidates
            .iter()
            .chain(incoming)
            .all(|candidate| authorized.contains(candidate))
        {
            return Err(OperationError::InvalidCandidates);
        }
        let mut known: BTreeSet<_> = self.candidates.iter().cloned().collect();
        for candidate in incoming {
            if known.insert(candidate.clone()) {
                self.candidates.push(candidate.clone());
            }
        }
        Ok(())
    }

    pub fn restore(
        expected_scope: &MatrixScope,
        intent: &str,
        authorized: &BTreeSet<plasm_core::prerequisites::CapabilityRef>,
        checkpoint: MatrixCheckpoint,
    ) -> Result<Self, OperationError> {
        if &checkpoint.scope != expected_scope {
            return Err(OperationError::ScopeMismatch);
        }
        let mut matrix = Self::new(
            checkpoint.scope,
            intent,
            checkpoint.operations,
            authorized,
            checkpoint.candidates,
        )?;
        let issued = checkpoint
            .answers
            .iter()
            .map(|answer| (answer.operation.clone(), answer.capability.clone()))
            .collect();
        matrix.record_packet(
            &IssuedPairs {
                scope: matrix.scope.clone(),
                pairs: issued,
            },
            &checkpoint.answers,
        )?;
        Ok(matrix)
    }

    /// Select at most four distinct pending pairs, balancing operation coverage
    /// and candidate coverage while retaining the host's candidate ranking.
    pub fn next_packet(
        &self,
        limit: usize,
    ) -> Result<Vec<(OperationId, plasm_core::prerequisites::CapabilityRef)>, OperationError> {
        self.next_packet_for(&self.candidates, limit)
    }

    /// Schedule only candidates admitted by the connected graph planner.
    /// Unadmitted candidates retain their pending state for a later wave.
    pub(crate) fn next_packet_for(
        &self,
        admitted: &[plasm_core::prerequisites::CapabilityRef],
        limit: usize,
    ) -> Result<Vec<(OperationId, plasm_core::prerequisites::CapabilityRef)>, OperationError> {
        if limit == 0 || limit > super::request_packing::MAX_QUESTIONS {
            return Err(OperationError::InvalidPacketSize);
        }
        let admitted_set: BTreeSet<_> = admitted.iter().collect();
        if admitted_set.len() != admitted.len()
            || !admitted
                .iter()
                .all(|candidate| self.candidates.contains(candidate))
        {
            return Err(OperationError::InvalidCandidates);
        }
        let mut selected = Vec::new();
        let primary = &self.operations.operations[0].id;
        let mut fragments: Vec<_> = self
            .operations
            .operations
            .iter()
            .skip(1)
            .enumerate()
            .collect();
        fragments.sort_by_key(|(rank, operation)| {
            (!self.current_operations.contains(&operation.id), *rank)
        });
        // Reserve a slot for coverage when possible, so a newly exposed candidate
        // can be assessed in the same bounded packet as whole-request relevance.
        let has_uncovered = self.uncovered_operations().next().is_some();
        let primary_limit = if has_uncovered && limit > 1 {
            limit - 1
        } else {
            limit
        };
        for candidate in admitted {
            let pair = (primary.clone(), candidate.clone());
            if !self.answers.contains_key(&pair) && selected.len() < primary_limit {
                selected.push(pair);
            }
        }
        for (_, operation) in fragments {
            if selected.len() == limit {
                break;
            }
            if self.answers.iter().any(|((id, candidate), value)| {
                id == &operation.id
                    && *value == DirectContribution::Relevant
                    && self.answers.get(&(primary.clone(), candidate.clone()))
                        == Some(&DirectContribution::Relevant)
            }) {
                continue;
            }
            let best = admitted
                .iter()
                .filter(|candidate| {
                    let primary_pair = (primary.clone(), (*candidate).clone());
                    (self.answers.get(&primary_pair) == Some(&DirectContribution::Relevant)
                        || selected.contains(&primary_pair))
                        && !self
                            .answers
                            .contains_key(&(operation.id.clone(), (*candidate).clone()))
                        && !selected.contains(&(operation.id.clone(), (*candidate).clone()))
                })
                .min_by_key(|candidate| {
                    self.recall_ranks
                        .get(&(operation.id.clone(), (*candidate).clone()))
                        .copied()
                        .unwrap_or(usize::MAX)
                });
            if let Some(candidate) = best {
                selected.push((operation.id.clone(), candidate.clone()));
                if selected.len() == limit {
                    break;
                }
            }
        }
        // A packet with no eligible coverage must still assess remaining candidates.
        if selected.len() < limit {
            for candidate in admitted {
                let pair = (primary.clone(), candidate.clone());
                if !self.answers.contains_key(&pair) && !selected.contains(&pair) {
                    selected.push(pair);
                    if selected.len() == limit {
                        break;
                    }
                }
            }
        }
        Ok(selected)
    }

    /// Commit only a complete response for the exact packet. A malformed,
    /// duplicated, or replayed response cannot partially change the matrix.
    pub fn record_packet(
        &mut self,
        issued: &IssuedPairs,
        judgments: &[OperationJudgment],
    ) -> Result<(), OperationError> {
        if issued.scope != self.scope {
            return Err(OperationError::ScopeMismatch);
        }
        validate_judgments(&issued.pairs, judgments)?;
        let known_ops: BTreeSet<_> = self.operations.operations.iter().map(|o| &o.id).collect();
        let known_candidates: BTreeSet<_> = self.candidates.iter().collect();
        for (operation, candidate) in &issued.pairs {
            if !known_ops.contains(operation) || !known_candidates.contains(candidate) {
                return Err(OperationError::UnknownQuestion);
            }
            if self
                .answers
                .contains_key(&(operation.clone(), candidate.clone()))
            {
                return Err(OperationError::AlreadyJudged);
            }
        }
        for judgment in judgments {
            if judgment.operation == self.operations.operations[0].id
                && judgment.contribution == DirectContribution::Contextual
            {
                return Err(OperationError::InvalidDecisionResponse);
            }
        }
        for judgment in judgments {
            self.answers.insert(
                (judgment.operation.clone(), judgment.capability.clone()),
                judgment.contribution,
            );
        }
        Ok(())
    }

    pub fn progress(
        &self,
        candidate: &plasm_core::prerequisites::CapabilityRef,
    ) -> Result<CandidateProgress, OperationError> {
        if !self.candidates.contains(candidate) {
            return Err(OperationError::UnknownQuestion);
        }
        let operation = &self.operations.operations[0].id;
        let relevant = BTreeSet::from([operation.clone()]);
        let uncertain_fragments: BTreeSet<_> = self
            .operations
            .operations
            .iter()
            .skip(1)
            .filter(|fragment| {
                self.answers.get(&(fragment.id.clone(), candidate.clone()))
                    == Some(&DirectContribution::Uncertain)
            })
            .map(|fragment| fragment.id.clone())
            .collect();
        if !uncertain_fragments.is_empty()
            && self.answers.get(&(operation.clone(), candidate.clone()))
                == Some(&DirectContribution::Relevant)
        {
            return Ok(CandidateProgress::Uncertain {
                operations: uncertain_fragments,
            });
        }
        let contradictory: BTreeSet<_> = self
            .operations
            .operations
            .iter()
            .skip(1)
            .filter(|fragment| {
                self.answers.get(&(fragment.id.clone(), candidate.clone()))
                    == Some(&DirectContribution::Relevant)
            })
            .map(|fragment| fragment.id.clone())
            .collect();
        if !contradictory.is_empty()
            && matches!(
                self.answers.get(&(operation.clone(), candidate.clone())),
                Some(DirectContribution::Unrelated | DirectContribution::Uncertain)
            )
        {
            return Ok(CandidateProgress::Uncertain {
                operations: contradictory,
            });
        }
        Ok(
            match self.answers.get(&(operation.clone(), candidate.clone())) {
                Some(DirectContribution::Relevant) => CandidateProgress::Relevant {
                    operations: relevant,
                    pending: BTreeSet::new(),
                    uncertain: BTreeSet::new(),
                },
                Some(DirectContribution::Unrelated) => CandidateProgress::Unrelated,
                Some(DirectContribution::Contextual) => {
                    return Err(OperationError::InvalidDecisionResponse);
                }
                Some(DirectContribution::Uncertain) => CandidateProgress::Uncertain {
                    operations: relevant,
                },
                None => CandidateProgress::Incomplete {
                    pending: relevant,
                    uncertain: BTreeSet::new(),
                },
            },
        )
    }

    /// Directly supported candidates in the host's original rank order.
    /// Incomplete and uncertain candidates remain in the matrix for later
    /// judgment; neither state is silently converted to unrelated or exposed.
    pub fn directly_supported(&self) -> Vec<plasm_core::prerequisites::CapabilityRef> {
        self.candidates
            .iter()
            .filter(|candidate| {
                matches!(
                    self.progress(candidate),
                    Ok(CandidateProgress::Relevant { .. })
                )
            })
            .cloned()
            .collect()
    }
}

impl OperationSet {
    /// Admit bounded source spans as coverage questions, without inferring intent.
    pub(crate) fn from_request(intent: &str) -> Result<Self, OperationError> {
        let mut spans: Vec<_> = source_spans(intent)?.into_iter().collect();
        spans.sort_by_key(|(id, _)| id[1..].parse::<usize>().expect("host span id"));
        let mut seen = BTreeSet::new();
        let mut excerpts = vec![intent.to_owned()];
        excerpts.extend(
            spans
                .into_iter()
                .map(|(_, text)| text)
                .filter(|text| seen.insert(text.clone())),
        );
        Self::from_excerpts(intent, &excerpts)
    }

    /// Validate exact request excerpts and assign stable, revision-bound IDs.
    /// This establishes provenance only; it does not prove semantic completeness.
    pub fn from_excerpts(intent: &str, excerpts: &[String]) -> Result<Self, OperationError> {
        if intent.trim().is_empty() {
            return Err(OperationError::EmptyIntent);
        }
        if excerpts.is_empty() || excerpts.len() > MAX_OPERATIONS {
            return Err(OperationError::InvalidCount);
        }
        let intent_digest = plasm_core::catalog_discovery::content_hash(intent.as_bytes());
        let mut seen = BTreeSet::new();
        let mut primary_duplicated = false;
        let mut operations = Vec::with_capacity(excerpts.len());
        for (index, excerpt) in excerpts.iter().enumerate() {
            if excerpt.trim().is_empty()
                || (excerpt.len() > MAX_OPERATION_BYTES && excerpt != intent)
                || !intent.contains(excerpt)
            {
                return Err(OperationError::InvalidExcerpt);
            }
            if !seen.insert(excerpt) {
                if excerpt == intent && !primary_duplicated {
                    primary_duplicated = true;
                } else {
                    return Err(OperationError::DuplicateExcerpt);
                }
            }
            let identity = plasm_core::catalog_discovery::content_hash(
                &serde_json::to_vec(&(&intent_digest, index == 0, excerpt))
                    .expect("strings serialize"),
            );
            operations.push(RequestedOperation {
                id: OperationId(identity),
                text: excerpt.clone(),
            });
        }
        Ok(Self {
            intent_digest,
            operations,
        })
    }

    pub fn validate_revision(&self, intent: &str) -> Result<(), OperationError> {
        if self.intent_digest != plasm_core::catalog_discovery::content_hash(intent.as_bytes()) {
            return Err(OperationError::IntentMismatch);
        }
        // Deserialization is not authority: revalidate all IDs and excerpts.
        let excerpts: Vec<_> = self.operations.iter().map(|op| op.text.clone()).collect();
        if Self::from_excerpts(intent, &excerpts)? != *self {
            return Err(OperationError::InvalidExcerpt);
        }
        Ok(())
    }

    pub fn operations(&self) -> &[RequestedOperation] {
        &self.operations
    }
}

#[cfg(test)]
#[path = "operations/tests.rs"]
mod tests;
