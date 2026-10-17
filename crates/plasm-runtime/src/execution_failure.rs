//! Public execution failures carry recovery authority independently of diagnostic prose.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCause {
    Program,
    ResponseContract,
    Catalog,
    Runtime,
    Upstream,
    Transport,
    Authorization,
    Cancelled,
    Unclassified,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDisposition {
    RepairProgram,
    ObtainAuthorization,
    ReconcileEffects,
    Stop,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputBudgetUnit {
    Rows,
    /// Native value accounting, including node/key overhead; not wire bytes.
    AccountedBytes,
}

/// Bounded evidence: counting stops on violation, so totals are lower bounds.
/// Input names identify plan sources or materialized packet fields, never values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputBudgetEvidence {
    pub unit: InputBudgetUnit,
    pub limit: usize,
    pub observed_at_least: usize,
    pub input: String,
}

/// Compiler-owned evidence; presentation text never selects these variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgramRejectionDetail {
    UnionColumnsDiffer {
        left: Vec<String>,
        right: Vec<String>,
    },
    ReservedBinding {
        name: String,
    },
    StaticIterationRequired,
    BuildMutation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramRejectionEvidence {
    pub category: String,
    /// UTF-8 byte offset in submitted Python, not a line or character index.
    pub span_offset: Option<usize>,
    pub detail: Option<ProgramRejectionDetail>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectOccurrence {
    pub scope_path: Vec<String>,
    pub occurrence_path: Vec<usize>,
    pub source_index: usize,
    pub source_identity: Option<String>,
    pub status: crate::OperationInvocationStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectReceipt {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_effects: Vec<crate::ResolvedEffectEvidence>,
    pub entry_id: String,
    pub capability: String,
    pub completed: usize,
    pub failed: usize,
    /// Failed invocations do not establish absence of an effect.
    pub occurrences: Vec<EffectOccurrence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionFailure(Box<ExecutionFailureDetails>);

/// Owned failure evidence, allocated only on the failure path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionFailureDetails {
    pub cause: FailureCause,
    pub recovery: RecoveryDisposition,
    pub code: String,
    pub node: Option<String>,
    pub occurrence_path: Vec<usize>,
    pub catalog_digest: Option<String>,
    /// Innermost input occurrence of the primary failure. Concurrent merge keeps
    /// the primary diagnostic and this address together. Not necessarily the
    /// receiver of a nested operation, so presentation must label it as input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_occurrence: Option<crate::OperationInvocationAddress>,
    pub effects: Vec<EffectReceipt>,
    pub dispatches: Vec<crate::MutationDispatch>,
    /// A failure after dispatch is not evidence that no effect occurred.
    pub effects_unresolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_budget: Option<InputBudgetEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_rejection: Option<ProgramRejectionEvidence>,
    /// Agent-visible diagnostic prose; never grants recovery authority.
    diagnostic: String,
}

impl std::ops::Deref for ExecutionFailure {
    type Target = ExecutionFailureDetails;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ExecutionFailure {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<plasm_core::collection_codec::CollectionFault> for ExecutionFailure {
    fn from(fault: plasm_core::collection_codec::CollectionFault) -> Self {
        use plasm_core::collection_codec::CollectionFault;
        let code = match &fault {
            CollectionFault::RowMaterializationMismatch => {
                "collection_row_materialization_mismatch"
            }
            CollectionFault::NotResident => "collection_not_resident",
            CollectionFault::Conservation => "collection_conservation",
            CollectionFault::Arity => "collection_arity",
            CollectionFault::InputMismatch { .. } => "collection_input_mismatch",
            CollectionFault::IdentityMismatch => "collection_identity_mismatch",
            CollectionFault::Incomplete { .. } => "collection_incomplete",
            CollectionFault::ObservationJson { .. }
            | CollectionFault::DerivationJson { .. }
            | CollectionFault::ExpressionJson { .. }
            | CollectionFault::CatalogDigestHex { .. }
            | CollectionFault::FrameEncodeJson { .. }
            | CollectionFault::FrameDecodeJson { .. }
            | CollectionFault::FrameTooShort { .. }
            | CollectionFault::FrameHeader
            | CollectionFault::FrameDigestMismatch
            | CollectionFault::FrameEvidenceInconsistent => "collection_frame",
        };
        Self::new(FailureCause::ResponseContract, code, fault.to_string())
    }
}

impl From<plasm_core::plasm_monad::CorrelatedBodyError> for ExecutionFailure {
    fn from(error: plasm_core::plasm_monad::CorrelatedBodyError) -> Self {
        use plasm_core::plasm_monad::CorrelatedBodyError as E;
        let code = match &error {
            E::NestingDepthExceeded => "scope_nesting_depth_exceeded",
            E::ParentBoundExceeded | E::ParentCountExceeded { .. } => "scope_parent_bound_exceeded",
            E::InvalidParentCapture | E::InvalidCapture => "scope_capture_invalid",
            E::UnsupportedWireVersion => "scope_wire_version_invalid",
            E::StepsTopologyMismatch => "scope_topology_mismatch",
            E::EffectfulPredicateScope => "scope_predicate_effect_forbidden",
            E::ReadEffectMismatch | E::MutationEffectMismatch => "scope_effect_mismatch",
            E::UndeclaredDependency { .. } => "scope_dependency_undeclared",
            E::ParallelReturn => "scope_parallel_return_forbidden",
            E::ReturnOutsideScope | E::ReturnNotLocal => "scope_return_invalid",
            E::OutputCardinalityMismatch | E::OutputCountMismatch { .. } => {
                "scope_output_cardinality_invalid"
            }
            E::OutputHasEntityAuthority => "scope_output_authority_invalid",
            E::BindGraph(_) => "scope_bind_graph_invalid",
            E::Scope(_) => "scope_operand_invalid",
        };
        Self::new(FailureCause::Program, code, error.to_string())
    }
}

impl From<plasm_core::plasm_monad::PlasmCompValidationError> for ExecutionFailure {
    fn from(error: plasm_core::plasm_monad::PlasmCompValidationError) -> Self {
        use plasm_core::plasm_monad::PlasmCompValidationError as E;
        let code = match &error {
            E::UnsupportedVersion { .. } => "comp_version_unsupported",
            E::EmptySteps => "comp_steps_empty",
            E::EmptyTopology => "comp_topology_empty",
            E::UnknownTopologicalStep { .. } => "comp_topology_step_unknown",
            E::IterationEffectContractMismatch => "comp_iteration_contract_mismatch",
            E::MissingIterationDependency { .. } => "comp_iteration_dependency_missing",
            E::InvalidIterationPredicate => "comp_iteration_predicate_invalid",
            E::MissingMapBodyParentDependency { .. } => "comp_map_parent_dependency_missing",
            E::BindGraph(_) => "comp_bind_graph_invalid",
            E::CorrelatedBody(_) => "comp_correlated_body_invalid",
            E::IterationEffect(_) => "comp_iteration_effect_invalid",
        };
        Self::new(FailureCause::Program, code, error.to_string())
    }
}
impl From<plasm_core::plasm_monad::BindGraphError> for ExecutionFailure {
    fn from(error: plasm_core::plasm_monad::BindGraphError) -> Self {
        Self::new(
            FailureCause::Program,
            "bind_graph_invalid",
            error.to_string(),
        )
    }
}
impl From<plasm_core::plasm_monad::IterationStepEffectError> for ExecutionFailure {
    fn from(error: plasm_core::plasm_monad::IterationStepEffectError) -> Self {
        Self::new(
            FailureCause::Program,
            "iteration_step_effect_invalid",
            error.to_string(),
        )
    }
}
impl From<plasm_core::plasm_monad::StepIdError> for ExecutionFailure {
    fn from(error: plasm_core::plasm_monad::StepIdError) -> Self {
        Self::new(FailureCause::Program, "step_id_invalid", error.to_string())
    }
}
impl ExecutionFailure {
    pub fn new(cause: FailureCause, code: &str, diagnostic: impl Into<String>) -> Self {
        Self(Box::new(ExecutionFailureDetails {
            cause,
            recovery: match cause {
                FailureCause::Program => RecoveryDisposition::RepairProgram,
                FailureCause::Authorization => RecoveryDisposition::ObtainAuthorization,
                _ => RecoveryDisposition::Stop,
            },
            code: code.into(),
            node: None,
            occurrence_path: Vec::new(),
            catalog_digest: None,
            input_occurrence: None,
            effects: Vec::new(),
            dispatches: Vec::new(),
            effects_unresolved: false,
            input_budget: None,
            program_rejection: None,
            diagnostic: diagnostic.into(),
        }))
    }
    pub fn with_input_budget(mut self, evidence: InputBudgetEvidence) -> Self {
        self.input_budget = Some(evidence);
        self
    }
    /// Guidance follows typed recovery authority, never a service diagnostic.
    pub fn recovery_instructions(&self) -> Option<&'static str> {
        if self.recovery != RecoveryDisposition::ReconcileEffects {
            return None;
        }
        if self.effects_unresolved || !self.dispatches.is_empty() {
            Some("Some dispatched effects have an unknown outcome. Reconcile them before retrying; this execution did not roll back.")
        } else {
            Some("A prior effect completed. Check its receipt before retrying; this execution did not roll back.")
        }
    }
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
    pub fn at(mut self, node: impl Into<String>, occurrence_path: Vec<usize>) -> Self {
        if self.node.is_none() {
            self.node = Some(node.into());
            self.occurrence_path = occurrence_path;
        }
        self
    }
    pub fn with_catalog(mut self, digest: &str) -> Self {
        if self.catalog_digest.is_none() {
            self.catalog_digest = Some(digest.to_owned());
        }
        self
    }
    pub fn with_effects(mut self, ledger: &crate::OperationLedger) -> Self {
        for ack in ledger.entries() {
            self.effects.push(EffectReceipt {
                resolved_effects: ack.resolved_effects.clone(),
                entry_id: ack.entry_id.clone(),
                capability: ack.capability.clone(),
                completed: ack.completed,
                failed: ack.failed,
                occurrences: ack
                    .outcomes
                    .iter()
                    .map(|outcome| EffectOccurrence {
                        scope_path: outcome.scope_path.clone(),
                        occurrence_path: outcome.occurrence_path.clone(),
                        source_index: outcome.source_index,
                        source_identity: outcome.source_identity.clone(),
                        status: outcome.status,
                    })
                    .collect(),
            });
        }
        if self.effects.iter().any(|x| x.completed > 0) || self.effects_unresolved {
            self.recovery = RecoveryDisposition::ReconcileEffects;
        }
        self
    }
    /// Qualify scalar receipts at the same scheduler boundary as successful acks.
    /// This changes attribution only, never counts, certainty or recovery authority.
    pub fn qualify_effects(mut self, address: &crate::OperationInvocationAddress) -> Self {
        if self.input_occurrence.is_none() {
            self.input_occurrence = Some(address.clone());
        }
        for receipt in &mut self.effects {
            if !receipt.occurrences.is_empty() {
                continue;
            }
            if let Some(status) =
                crate::OperationInvocationAddress::scalar_status(receipt.completed, receipt.failed)
            {
                receipt.occurrences.push(EffectOccurrence {
                    scope_path: address.scope_path.clone(),
                    occurrence_path: address.occurrence_path.clone(),
                    source_index: address.source_index,
                    source_identity: address.source_identity.clone(),
                    status,
                });
            }
        }
        self
    }
    /// Drain concurrent failures without dropping any branch's effect evidence.
    pub fn merge(mut self, mut other: Self) -> Self {
        if self.recovery == RecoveryDisposition::RepairProgram
            && other.recovery != RecoveryDisposition::RepairProgram
        {
            std::mem::swap(&mut self, &mut other);
        }
        self.effects.append(&mut other.effects);
        self.dispatches.append(&mut other.dispatches);
        self.effects_unresolved |= other.effects_unresolved;
        if self.effects_unresolved
            || !self.dispatches.is_empty()
            || self.effects.iter().any(|e| e.completed > 0)
        {
            self.recovery = RecoveryDisposition::ReconcileEffects;
        }
        self
    }
    pub fn with_dispatches(mut self, dispatches: Vec<crate::MutationDispatch>) -> Self {
        if !dispatches.is_empty() {
            self.effects_unresolved = true;
            self.recovery = RecoveryDisposition::ReconcileEffects;
            self.dispatches.extend(dispatches);
        }
        self
    }
}
impl From<crate::RuntimeError> for ExecutionFailure {
    fn from(mut error: crate::RuntimeError) -> Self {
        use crate::RuntimeError::*;
        let mut locations = Vec::new();
        loop {
            match error {
                HydrationGet { source, .. } => error = *source,
                ViewNode {
                    view, node, source, ..
                } => {
                    locations.push(view);
                    locations.push(node);
                    error = *source;
                }
                inner => {
                    error = inner;
                    break;
                }
            }
        }
        if let Collection(fault) = error {
            let failure = Self::from(fault);
            return if locations.is_empty() {
                failure
            } else {
                failure.at(locations.join("/"), vec![])
            };
        }
        let (cause, code) = match &error {
            FieldUnavailable { .. } => (FailureCause::ResponseContract, "field_unavailable"),
            DecodeError { .. } | DeclaredOutputCardinality { .. } => (
                FailureCause::ResponseContract,
                "response_contract_violation",
            ),
            PaginationProgress { .. } => (
                FailureCause::ResponseContract,
                "pagination_progress_violation",
            ),
            CatalogTemplate(_)
            | CompilationError { .. }
            | CmlError { .. }
            | CapabilityNotFound { .. } => (FailureCause::Catalog, "catalog_contract_violation"),
            TypeError { .. } => (FailureCause::Runtime, "runtime_type_violation"),
            RequestError {
                status: Some(_), ..
            }
            | RateLimited { .. } => (FailureCause::Upstream, "upstream_rejection"),
            HostTransport { .. } | HttpTransport { .. } | RequestError { .. } => {
                (FailureCause::Transport, "transport_failure")
            }
            CredentialProvider { .. } | AuthenticationError(_) => {
                (FailureCause::Authorization, "authorization_required")
            }
            Cancelled => (FailureCause::Cancelled, "execution_cancelled"),
            _ => (FailureCause::Unclassified, "unclassified_execution_failure"),
        };
        let diagnostic = match &error {
            HostTransport { .. } => "Provider transport failed without a usable response. Inspect the host transport evidence before another execution.".into(),
            Evm(source) => source.agent_diagnostic(),
            HttpTransport { source, .. } if source.is_timeout() => "Provider transport timed out. Check provider availability and reconcile any dispatched effects before another execution.".into(),
            HttpTransport { source, .. } if source.is_connect() => "Could not connect to the provider. Check provider availability and the configured connection.".into(),
            HttpTransport { .. } => "Provider transport could not complete the exchange. Inspect the transport evidence and reconcile any dispatched effects before another execution.".into(),
            RequestError {
                source: crate::RequestFailure::HttpStatus(failure),
                ..
            } => failure.agent_diagnostic().to_string(),
            RateLimited {
                source: crate::RateLimitCause::Upstream(failure),
                ..
            } => failure.agent_diagnostic().to_string(),
            // Decoder errors may embed whole response values. Explain the contract
            // failure without promoting those values into the diagnostic channel.
            DecodeError { source } => match source {
                plasm_compile::DecodeError::FieldContract { field, .. } => {
                    format!("Response field '{field}' violates its declared type")
                }
                plasm_compile::DecodeError::JsonPath(_)
                | plasm_compile::DecodeError::PathNotFound { .. }
                | plasm_compile::DecodeError::TypeMismatch { .. }
                | plasm_compile::DecodeError::StringTransformInput
                | plasm_compile::DecodeError::NumberTransformInput
                | plasm_compile::DecodeError::NumberRepresentation
                | plasm_compile::DecodeError::NumberParse { .. }
                | plasm_compile::DecodeError::BooleanTransformInput
                | plasm_compile::DecodeError::BooleanLiteral
                | plasm_compile::DecodeError::EnumTransformInput
                | plasm_compile::DecodeError::SegmentDeriveInput
                | plasm_compile::DecodeError::SegmentPrefixMissing
                | plasm_compile::DecodeError::SegmentIndex { .. }
                | plasm_compile::DecodeError::NameValueDeriveInput
                | plasm_compile::DecodeError::ObjectKeyDeriveInput
                | plasm_compile::DecodeError::NestedRelationDecoder { .. }
                | plasm_compile::DecodeError::IdentityPathEmpty
                | plasm_compile::DecodeError::IdentityScalarRequired
                | plasm_compile::DecodeError::EntitySourceShape
                | plasm_compile::DecodeError::IdentityFieldTypeMissing { .. }
                | plasm_compile::DecodeError::IdentityFieldContract { .. }
                | plasm_compile::DecodeError::CompoundKeyPartMissing { .. }
                | plasm_compile::DecodeError::ExhaustiveEmbedPathEmpty { .. }
                | plasm_compile::DecodeError::ExhaustiveEmbedBranch { .. }
                | plasm_compile::DecodeError::RelationMembership { .. }
                | plasm_compile::DecodeError::IdentityMissing => source.to_string(),
            },
            RequestError { source, .. } => source.agent_diagnostic(),
            RateLimited { .. } => "Provider concurrency or retry capacity was exhausted. Check provider availability before another execution.".into(),
            CacheSource(_) | CacheError(_) => "Observation storage failed. The host must repair storage before execution can continue.".into(),
            CredentialProvider { .. } | Credential(_) | AuthenticationError(_) => "Configured authorization could not be resolved or used. Check the configured account and inspect the authorization evidence before another execution.".into(),
            // Explicit semantic-display whitelist: adding a runtime variant must
            // select its agent projection; no wildcard can promote a new payload.
            RequestIdentityAuthOpaque
            | RequestIdentityCardinality { .. }
            | DeclaredOutputCardinality { .. }
            | ResponseNarrowing(_)
            | HttpWire(_)
            | Chain(_)
            | ViewPlan(_)
            | Preflight(_)
            | HttpLimiter(_)
            | PaginationFault(_)
            | RelationParametersMissing { .. }
            | MaterializeBindingMissing { .. }
            | EmbeddedHydrationPlanRequired
            | ReadIdentityType
            | CompiledCatalogMissing
            | CompiledCatalogScopeMissing
            | ViewQueryDispatchRequired
            | EvmRpcUrlMissing
            | ContinuationDispatchRequired { .. }
            | TeachingValueNotExecutable
            | CapabilityUnknown { .. }
            | ReadCapabilityRequired { .. }
            | GetCapabilityRequired { .. }
            | DerivedGetNestingForbidden { .. }
            | ViewGetNestingForbidden
            | GetIdentityMismatch { .. }
            | HttpQueryTemplateRequired
            | ViewPaginationUnsupported
            | PaginationPageLimit { .. }
            | LiveAbsolutePaginationRequired
            | Ordering(_)
            | TopKFieldUnobserved
            | TopKSequenceOverflow
            | TopKFieldEmpty
            | FieldUnknown { .. }
            | EntityUnknown { .. }
            | ViewNodeMissing { .. }
            | ComputedOutputPhaseRequired
            | TraversalParentTypeMismatch { .. }
            | PaginationContract(_)
            | CatalogTemplate(_)
            | IdentityProjection(_)
            | EntityRefScope(_)
            | QueryResolution(_)
            | SchemaContract(_)
            | OperandResolution(_)
            | ViewTemplate { .. }
            | ViewTemplateEmpty
            | ViewTemplateTooLong { .. }
            | ViewNode { .. }
            | ViewNodeResolution(_)
            | FieldUnavailable { .. }
            | Collection(_)
            | ValueContract(_)
            | TemporalInput(_)
            | ValueCoercion(_)
            | RowPredicate(_)
            | CompilationError { .. }
            | TypeError { .. }
            | CmlError { .. }
            | WorkflowConflict { .. }
            | CacheValueRow(_)
            | CacheRowDecode(_)
            | UnsupportedExecutionMode { .. }
            | CapabilityNotFound { .. }
            | FingerprintNotFound
            | ReplayEntryNotFound { .. }
            | ReplayStoreError(_)
            | DerivedGetNotFound { .. }
            | DerivedGetNonUnique { .. }
            | DerivedGetSourceFieldMissing { .. }
            | DerivedGetIncompleteSource { .. }
            | PaginationProgress { .. }
            | SerializationError(_)
            | Cancelled
            | HydrationGet { .. } => error.to_string(),
        };
        let failure = Self::new(cause, code, diagnostic);
        if locations.is_empty() {
            failure
        } else {
            failure.at(locations.join("/"), vec![])
        }
    }
}

impl From<plasm_core::ValueCodecError> for ExecutionFailure {
    fn from(source: plasm_core::ValueCodecError) -> Self {
        crate::RuntimeError::from(source).into()
    }
}
impl std::fmt::Display for ExecutionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.diagnostic())?;
        if let Some(correction) = self.recovery_instructions() {
            write!(f, " {correction}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ExecutionFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_without_receipt_retains_innermost_input() {
        let inner = crate::OperationInvocationAddress {
            scope_path: vec!["items".into(), "write".into()],
            occurrence_path: vec![2, 7],
            source_index: 7,
            source_identity: Some("Item/b".into()),
        };
        let outer = crate::OperationInvocationAddress {
            occurrence_path: vec![2],
            source_identity: Some("Group/a".into()),
            ..inner.clone()
        };
        let failure =
            ExecutionFailure::new(FailureCause::Upstream, "upstream_rejection", "already set")
                .qualify_effects(&inner)
                .qualify_effects(&outer);
        assert_eq!(failure.input_occurrence, Some(inner));
        assert!(failure.effects.is_empty());
        let restored: ExecutionFailure =
            serde_json::from_value(serde_json::to_value(&failure).unwrap()).unwrap();
        assert_eq!(restored.input_occurrence, failure.input_occurrence);
        let merged = failure.clone().merge(
            ExecutionFailure::new(FailureCause::Upstream, "other_rejection", "other rejected")
                .qualify_effects(&outer),
        );
        assert_eq!(merged.diagnostic(), failure.diagnostic());
        assert_eq!(merged.input_occurrence, failure.input_occurrence);
    }
    #[test]
    fn occurrence_qualification_preserves_counts_and_nested_addresses() {
        let address = crate::OperationInvocationAddress {
            scope_path: vec!["outer".into(), "write".into()],
            occurrence_path: vec![2, 1],
            source_index: 1,
            source_identity: Some("Item/a".into()),
        };
        for (completed, failed) in [(0, 0), (1, 0), (0, 1), (2, 0), (1, 1)] {
            let mut ack = crate::OperationAck {
                entry_id: "fixture".into(),
                capability: "set_state".into(),
                entity: "Item".into(),
                logical_invocations: completed + failed,
                completed,
                failed,
                source: crate::ExecutionSource::Live,
                description: String::new(),
                resolved_effects: Vec::new(),
                outcomes: vec![],
            };
            let original = ExecutionFailure::new(FailureCause::Cancelled, "cancelled", "stopped")
                .with_effects(&crate::OperationLedger::from_ack(ack.clone()));
            let qualified = original.clone().qualify_effects(&address);
            address.qualify_ack(&mut ack);
            assert_eq!(qualified.effects[0].completed, completed);
            assert_eq!(qualified.effects[0].failed, failed);
            assert_eq!(qualified.recovery, original.recovery);
            assert_eq!(qualified.effects_unresolved, original.effects_unresolved);
            assert_eq!(qualified.effects[0].occurrences.len(), ack.outcomes.len());
            if let Some(occurrence) = qualified.effects[0].occurrences.first() {
                assert_eq!(occurrence.occurrence_path, [2, 1]);
                assert_eq!(occurrence.source_identity.as_deref(), Some("Item/a"));
                assert_eq!(occurrence.status, ack.outcomes[0].status);
            }
            let parent = crate::OperationInvocationAddress {
                scope_path: vec!["parent".into()],
                occurrence_path: vec![9],
                source_index: 9,
                source_identity: None,
            };
            assert_eq!(qualified, qualified.clone().qualify_effects(&parent));
            let before = ack.clone();
            parent.qualify_ack(&mut ack);
            assert_eq!(ack, before);
            let wire = serde_json::to_vec(&qualified).unwrap();
            assert_eq!(
                qualified,
                serde_json::from_slice::<ExecutionFailure>(&wire).unwrap()
            );
        }
    }
    #[test]
    fn effect_occurrences_survive_without_opaque_error_details() {
        let ack = crate::OperationAck {
            entry_id: "fixture".into(),
            capability: "set_state".into(),
            entity: "Item".into(),
            logical_invocations: 2,
            completed: 1,
            failed: 1,
            source: crate::ExecutionSource::Live,
            description: String::new(),
            resolved_effects: Vec::new(),
            outcomes: vec![
                crate::OperationInvocationOutcome {
                    scope_path: vec!["write".into()],
                    occurrence_path: vec![0],
                    source_index: 0,
                    source_identity: Some("Item/a".into()),
                    status: crate::OperationInvocationStatus::Completed,
                    error: None,
                },
                crate::OperationInvocationOutcome {
                    scope_path: vec!["write".into()],
                    occurrence_path: vec![1],
                    source_index: 1,
                    source_identity: Some("Item/b".into()),
                    status: crate::OperationInvocationStatus::Failed,
                    error: Some(ExecutionFailure::new(
                        FailureCause::Upstream,
                        "fixture_rejection",
                        "opaque service private-password-sentinel",
                    )),
                },
            ],
        };
        let failure = ExecutionFailure::new(FailureCause::Upstream, "upstream_rejection", "opaque")
            .with_effects(&crate::OperationLedger::from_ack(ack));
        assert_eq!(failure.effects[0].occurrences.len(), 2);
        assert_eq!(
            failure.effects[0].occurrences[0].source_identity.as_deref(),
            Some("Item/a")
        );
        assert_eq!(failure.recovery, RecoveryDisposition::ReconcileEffects);
        assert_eq!(
            failure.recovery_instructions(),
            Some("A prior effect completed. Check its receipt before retrying; this execution did not roll back.")
        );
        assert!(!serde_json::to_string(&failure)
            .unwrap()
            .contains("private-password"));
    }
    #[test]
    fn decoder_diagnostics_preserve_structure_but_not_raw_field_values() {
        let error = plasm_compile::DecodeError::FieldContract {
            field: "email".into(),
            source: plasm_core::DecodeFieldCause::Domain(
                plasm_core::ValueDomainViolation::InvalidProfile(
                    plasm_core::value_domain::ProfileId::Email,
                ),
            ),
        };
        assert!(matches!(
            &error,
            plasm_compile::DecodeError::FieldContract {
                source: plasm_core::DecodeFieldCause::Domain(
                    plasm_core::ValueDomainViolation::InvalidProfile(
                        plasm_core::value_domain::ProfileId::Email
                    )
                ),
                ..
            }
        ));
        let failure = ExecutionFailure::from(crate::RuntimeError::from(error));
        let wire = serde_json::to_string(&failure).unwrap();
        assert!(wire.contains("email"));
        assert!(!wire.contains("raw-secret-value"));
        let missing = ExecutionFailure::from(crate::RuntimeError::from(
            plasm_compile::DecodeError::IdentityMissing,
        ));
        assert!(missing.diagnostic().contains("No valid ID"));
        assert_eq!(missing.recovery, RecoveryDisposition::Stop);
    }

    #[test]
    fn upstream_service_message_survives_wire_without_authorizing_retry() {
        for message in [
            "Already applied",
            "Insufficient balance",
            "retry immediately",
            "repair program",
        ] {
            let failure = ExecutionFailure::from(crate::RuntimeError::RequestError {
                source: crate::HttpStatusFailure::without_request(422, message.into()).into(),
                attempts: 1,
                status: Some(422),
                body: Some(Box::new(serde_json::json!({"message": message}))),
            });
            let wire = serde_json::to_value(&failure).unwrap();
            assert!(wire["diagnostic"].as_str().unwrap().contains(message));
            assert_eq!(failure.cause, FailureCause::Upstream);
            assert_eq!(failure.recovery, RecoveryDisposition::Stop);
            assert!(!failure.effects_unresolved);
        }
    }

    #[test]
    fn provider_presentation_is_transport_independent_and_never_empty() {
        let mut evidence =
            crate::HttpStatusFailure::without_request(422, "Validation error. Reason: ".into());
        evidence.method = "POST".into();
        evidence.url = "https://provider.invalid/request?private=do-not-render".into();
        let failure = ExecutionFailure::from(crate::RuntimeError::RequestError {
            source: evidence.into(),
            attempts: 1,
            status: Some(422),
            body: Some(Box::new(
                serde_json::json!({"detail":"Validation error. Reason: "}),
            )),
        });
        assert_eq!(failure.cause, FailureCause::Upstream);
        assert_eq!(failure.recovery, RecoveryDisposition::Stop);
        assert!(failure.diagnostic().contains("no specific correction"));
        assert!(!failure.diagnostic().contains("do-not-render"));
        assert!(!failure.diagnostic().contains("POST"));
    }

    #[test]
    fn nested_view_failures_use_the_same_provider_projection_without_recursion() {
        let mut failure = crate::RuntimeError::RequestError {
            source: crate::HttpStatusFailure::without_request(422, "Insufficient balance".into())
                .into(),
            attempts: 1,
            status: Some(422),
            body: Some(Box::new(serde_json::json!({"private":"do-not-render"}))),
        };
        for index in 0..128 {
            failure = crate::RuntimeError::ViewNode {
                view: "ReadView".into(),
                node: format!("__py{index}"),
                capability: "read".into(),
                source: Box::new(failure),
            };
        }
        let failure = ExecutionFailure::from(failure);
        assert_eq!(failure.cause, FailureCause::Upstream);
        assert!(failure.diagnostic().contains("Insufficient balance"));
        assert!(!failure.diagnostic().contains("do-not-render"));
        assert!(!failure.diagnostic().contains("__py"));
        assert!(
            failure.node.as_ref().unwrap().contains("__py"),
            "machine scope survives projection"
        );
    }

    #[test]
    fn failure_wire_preserves_diagnostic_without_granting_repair_by_message() {
        let failure = ExecutionFailure::new(
            FailureCause::Runtime,
            "service_rejected",
            "repair program: service rejected the requested state",
        );
        let wire = serde_json::to_string(&failure).unwrap();
        assert!(wire.contains("service rejected the requested state"));
        let decoded: ExecutionFailure = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.recovery, RecoveryDisposition::Stop);
        assert_eq!(decoded.diagnostic(), failure.diagnostic());
    }

    #[test]
    fn pagination_guard_failure_is_a_response_contract_violation() {
        let failure = ExecutionFailure::from(crate::RuntimeError::PaginationProgress {
            reason: crate::execution::PaginationTerminalReason::DuplicateIdentityOverlap,
        });
        assert_eq!(failure.cause, FailureCause::ResponseContract);
        assert_eq!(failure.code, "pagination_progress_violation");
        assert_eq!(failure.recovery, RecoveryDisposition::Stop);
        assert!(failure.diagnostic().contains("duplicate identity overlap"));
        assert_eq!(failure.recovery_instructions(), None);
    }

    #[test]
    fn unresolved_dispatch_uses_unknown_outcome_guidance() {
        let mut failure =
            ExecutionFailure::new(FailureCause::Transport, "transport_failure", "lost reply");
        failure.recovery = RecoveryDisposition::ReconcileEffects;
        failure.effects_unresolved = true;
        assert_eq!(
            failure.recovery_instructions(),
            Some("Some dispatched effects have an unknown outcome. Reconcile them before retrying; this execution did not roll back.")
        );
    }
}

impl PartialEq for ExecutionFailure {
    fn eq(&self, other: &Self) -> bool {
        self.cause == other.cause
            && self.recovery == other.recovery
            && self.code == other.code
            && self.node == other.node
            && self.occurrence_path == other.occurrence_path
            && self.catalog_digest == other.catalog_digest
            && self.effects == other.effects
            && self.dispatches == other.dispatches
            && self.effects_unresolved == other.effects_unresolved
    }
}
impl Eq for ExecutionFailure {}

#[cfg(test)]
mod collection_fault_tests {
    use super::*;
    use plasm_core::collection_codec::CollectionFault;

    #[test]
    fn collection_fault_has_typed_contract_authority_without_invented_effects() {
        for failure in [
            ExecutionFailure::from(CollectionFault::Conservation),
            ExecutionFailure::from(crate::RuntimeError::from(CollectionFault::Conservation)),
        ] {
            assert_eq!(failure.cause, FailureCause::ResponseContract);
            assert_eq!(failure.code, "collection_conservation");
            assert_eq!(failure.recovery, RecoveryDisposition::Stop);
            assert!(!failure.effects_unresolved);
            assert!(failure.effects.is_empty());
            assert!(failure.dispatches.is_empty());
        }
    }
}
