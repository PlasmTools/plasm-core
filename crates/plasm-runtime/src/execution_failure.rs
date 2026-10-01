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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectOccurrence {
    pub source_index: usize,
    pub source_identity: Option<String>,
    pub status: crate::OperationInvocationStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectReceipt {
    pub entry_id: String,
    pub capability: String,
    pub completed: usize,
    pub failed: usize,
    /// Failed invocations do not establish absence of an effect.
    pub occurrences: Vec<EffectOccurrence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionFailure {
    pub cause: FailureCause,
    pub recovery: RecoveryDisposition,
    pub code: String,
    pub node: Option<String>,
    pub occurrence_path: Vec<usize>,
    pub catalog_digest: Option<String>,
    pub effects: Vec<EffectReceipt>,
    pub dispatches: Vec<crate::MutationDispatch>,
    /// A failure after dispatch is not evidence that no effect occurred.
    pub effects_unresolved: bool,
    /// Agent-visible diagnostic prose; never grants recovery authority.
    diagnostic: String,
}

impl From<plasm_core::collection_codec::CollectionFault> for ExecutionFailure {
    fn from(fault: plasm_core::collection_codec::CollectionFault) -> Self {
        use plasm_core::collection_codec::CollectionFault;
        let code = match &fault {
            CollectionFault::NotResident => "collection_not_resident",
            CollectionFault::Conservation => "collection_conservation",
            CollectionFault::Arity => "collection_arity",
            CollectionFault::InputMismatch { .. } => "collection_input_mismatch",
            CollectionFault::IdentityMismatch => "collection_identity_mismatch",
            CollectionFault::Incomplete { .. } => "collection_incomplete",
            CollectionFault::Frame(_) => "collection_frame",
        };
        Self::new(FailureCause::ResponseContract, code, fault.to_string())
    }
}
impl ExecutionFailure {
    pub fn new(cause: FailureCause, code: &str, diagnostic: impl Into<String>) -> Self {
        Self {
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
            effects: Vec::new(),
            dispatches: Vec::new(),
            effects_unresolved: false,
            diagnostic: diagnostic.into(),
        }
    }
    /// Guidance follows typed recovery authority, never a service diagnostic.
    pub fn recovery_instructions(&self) -> Option<&'static str> {
        (self.recovery == RecoveryDisposition::ReconcileEffects).then_some(
            "This execution stopped. Inspect its completed writes and unresolved dispatches before choosing the next program. Failed dispatches may have taken effect; service error bodies prove neither success nor absence. A new execution is permitted and does not resume or roll back this execution."
        )
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
                entry_id: ack.entry_id.clone(),
                capability: ack.capability.clone(),
                completed: ack.completed,
                failed: ack.failed,
                occurrences: ack
                    .outcomes
                    .iter()
                    .map(|outcome| EffectOccurrence {
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
    /// Drain concurrent failures without dropping any branch's effect evidence.
    pub fn merge(mut self, mut other: Self) -> Self {
        if self.recovery == RecoveryDisposition::RepairProgram
            && other.recovery != RecoveryDisposition::RepairProgram
        {
            std::mem::swap(&mut self, &mut other);
        }
        self.effects.extend(other.effects);
        self.dispatches.extend(other.dispatches);
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
impl From<String> for ExecutionFailure {
    fn from(detail: String) -> Self {
        Self::new(
            FailureCause::Unclassified,
            "unclassified_execution_failure",
            detail,
        )
    }
}
impl From<crate::RuntimeError> for ExecutionFailure {
    fn from(error: crate::RuntimeError) -> Self {
        use crate::RuntimeError::*;
        if let HydrationGet { source, .. } = error {
            return Self::from(*source);
        }
        if let Collection(fault) = error {
            return Self::from(fault);
        }
        let (cause, code) = match &error {
            FieldUnavailable { .. } => (FailureCause::ResponseContract, "field_unavailable"),
            DecodeError { .. } => (
                FailureCause::ResponseContract,
                "response_contract_violation",
            ),
            CompilationError { .. } | CmlError { .. } | CapabilityNotFound { .. } => {
                (FailureCause::Catalog, "catalog_contract_violation")
            }
            TypeError { .. } => (FailureCause::Runtime, "runtime_type_violation"),
            RequestError {
                status: Some(_), ..
            }
            | RateLimited { .. } => (FailureCause::Upstream, "upstream_rejection"),
            RequestError { .. } => (FailureCause::Transport, "transport_failure"),
            AuthenticationError { .. } => (FailureCause::Authorization, "authorization_required"),
            Cancelled => (FailureCause::Cancelled, "execution_cancelled"),
            _ => (FailureCause::Unclassified, "unclassified_execution_failure"),
        };
        let diagnostic = match &error {
            // Decoder errors may embed whole response values. Explain the contract
            // failure without promoting those values into the diagnostic channel.
            DecodeError { source } => match source {
                plasm_compile::DecodeError::FieldContract { field, .. } => {
                    format!("Response field '{field}' violates its declared type")
                }
                plasm_compile::DecodeError::TransformFailed { transform, .. } => {
                    format!("Response transform '{transform}' failed")
                }
                _ => source.to_string(),
            },
            _ => error.to_string(),
        };
        Self::new(cause, code, diagnostic)
    }
}
impl std::fmt::Display for ExecutionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:?}", self.code, self.recovery)
    }
}
impl std::error::Error for ExecutionFailure {}

#[cfg(test)]
mod tests {
    use super::*;
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
            outcomes: vec![
                crate::OperationInvocationOutcome {
                    source_index: 0,
                    source_identity: Some("Item/a".into()),
                    status: crate::OperationInvocationStatus::Completed,
                    error: None,
                },
                crate::OperationInvocationOutcome {
                    source_index: 1,
                    source_identity: Some("Item/b".into()),
                    status: crate::OperationInvocationStatus::Failed,
                    error: Some("opaque service private-password-sentinel".into()),
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
        assert!(!serde_json::to_string(&failure)
            .unwrap()
            .contains("private-password"));
    }
    #[test]
    fn decoder_diagnostics_preserve_structure_but_not_raw_field_values() {
        let failure = ExecutionFailure::from(crate::RuntimeError::from(
            plasm_compile::DecodeError::FieldContract {
                field: "amount".into(),
                reason: "invalid raw-secret-value".into(),
            },
        ));
        let wire = serde_json::to_string(&failure).unwrap();
        assert!(wire.contains("amount"));
        assert!(!wire.contains("raw-secret-value"));
        let missing = ExecutionFailure::from(crate::RuntimeError::from(
            plasm_compile::DecodeError::InvalidStructure {
                message: "No valid ID field found in source object".into(),
            },
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
                message: message.into(),
                attempts: 1,
                status: Some(422),
                body: Some(serde_json::json!({"message": message})),
            });
            let wire = serde_json::to_value(&failure).unwrap();
            assert!(wire["diagnostic"].as_str().unwrap().contains(message));
            assert_eq!(failure.cause, FailureCause::Upstream);
            assert_eq!(failure.recovery, RecoveryDisposition::Stop);
            assert!(!failure.effects_unresolved);
        }
    }

    #[test]
    fn failure_wire_preserves_diagnostic_without_granting_repair_by_message() {
        let failure = ExecutionFailure::from(
            "repair program: service rejected the requested state".to_string(),
        );
        let wire = serde_json::to_string(&failure).unwrap();
        assert!(wire.contains("service rejected the requested state"));
        let decoded: ExecutionFailure = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.recovery, RecoveryDisposition::Stop);
        assert_eq!(decoded.diagnostic(), failure.diagnostic());
    }
}

impl From<&str> for ExecutionFailure {
    fn from(detail: &str) -> Self {
        detail.to_owned().into()
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
