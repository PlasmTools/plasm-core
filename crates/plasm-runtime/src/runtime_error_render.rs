//! Map [`RuntimeError`](crate::RuntimeError) into [`StepError`](plasm_core::step_semantics::StepError) for unified UX.

use plasm_core::error_render::render_type_error;
use plasm_core::schema::CGS;
use plasm_core::step_semantics::{append_correction_lines, StepError, StepErrorCategory};

use crate::RuntimeError;

/// Convert a runtime failure into a structured [`StepError`].
pub fn step_error_from_runtime(err: &RuntimeError, cgs: &CGS) -> StepError {
    match err {
        RuntimeError::RequestIdentityAuthOpaque => {
            StepError::new(StepErrorCategory::Config, err.to_string(), None)
        }
        RuntimeError::RequestIdentityCardinality { .. } => {
            StepError::new(StepErrorCategory::Runtime, err.to_string(), None)
        }
        RuntimeError::Evm(_) => StepError::new(StepErrorCategory::Runtime, err.to_string(), None),
        RuntimeError::CatalogTemplate(source) => {
            StepError::new(StepErrorCategory::Config, source.to_string(), None)
        }
        RuntimeError::ResponseNarrowing(_)
        | RuntimeError::HttpWire(_)
        | RuntimeError::Chain(_)
        | RuntimeError::ViewPlan(_)
        | RuntimeError::Preflight(_)
        | RuntimeError::HttpLimiter(_)
        | RuntimeError::PaginationFault(_)
        | RuntimeError::RelationParametersMissing { .. }
        | RuntimeError::MaterializeBindingMissing { .. }
        | RuntimeError::EmbeddedHydrationPlanRequired
        | RuntimeError::ReadIdentityType
        | RuntimeError::CompiledCatalogMissing
        | RuntimeError::CompiledCatalogScopeMissing
        | RuntimeError::ViewQueryDispatchRequired
        | RuntimeError::EvmRpcUrlMissing
        | RuntimeError::ContinuationDispatchRequired { .. }
        | RuntimeError::TeachingValueNotExecutable
        | RuntimeError::CapabilityUnknown { .. }
        | RuntimeError::ReadCapabilityRequired { .. }
        | RuntimeError::GetCapabilityRequired { .. }
        | RuntimeError::DerivedGetNestingForbidden { .. }
        | RuntimeError::ViewGetNestingForbidden
        | RuntimeError::GetIdentityMismatch { .. }
        | RuntimeError::HttpQueryTemplateRequired
        | RuntimeError::ViewPaginationUnsupported
        | RuntimeError::PaginationPageLimit { .. }
        | RuntimeError::LiveAbsolutePaginationRequired
        | RuntimeError::Ordering(_)
        | RuntimeError::TopKFieldUnobserved
        | RuntimeError::TopKSequenceOverflow
        | RuntimeError::TopKFieldEmpty
        | RuntimeError::FieldUnknown { .. }
        | RuntimeError::EntityUnknown { .. }
        | RuntimeError::ViewNodeMissing { .. }
        | RuntimeError::ComputedOutputPhaseRequired
        | RuntimeError::TraversalParentTypeMismatch { .. }
        | RuntimeError::Credential(_)
        | RuntimeError::PaginationContract(_)
        | RuntimeError::IdentityProjection(_)
        | RuntimeError::EntityRefScope(_)
        | RuntimeError::QueryResolution(_)
        | RuntimeError::SchemaContract(_)
        | RuntimeError::OperandResolution(_)
        | RuntimeError::ViewTemplate { .. }
        | RuntimeError::ViewTemplateEmpty
        | RuntimeError::ViewTemplateTooLong { .. }
        | RuntimeError::ViewNode { .. } => {
            StepError::new(StepErrorCategory::Config, err.to_string(), None)
        }
        RuntimeError::ViewNodeResolution(error) => {
            StepError::new(StepErrorCategory::Config, error.to_string(), None)
        }
        RuntimeError::FieldUnavailable { .. } => {
            StepError::new(StepErrorCategory::Runtime, err.to_string(), None)
        }
        RuntimeError::Collection(fault) => {
            StepError::new(StepErrorCategory::Runtime, fault.to_string(), None)
        }
        RuntimeError::ValueContract(error) => {
            StepError::new(StepErrorCategory::Runtime, error.to_string(), None)
        }
        RuntimeError::RowPredicate(error) => {
            StepError::new(StepErrorCategory::Runtime, error.to_string(), None)
        }
        RuntimeError::TemporalInput(error) => {
            StepError::new(StepErrorCategory::Runtime, error.to_string(), None)
        }
        RuntimeError::ValueCoercion(error) => {
            StepError::new(StepErrorCategory::Runtime, error.to_string(), None)
        }
        RuntimeError::TypeError { source } => render_type_error(source, cgs),
        RuntimeError::CompilationError { source } => {
            let msg = source.to_string();
            let hints = vec![
                "Verify the expression matches a capability on the entity (query vs search vs get)."
                    .into(),
                "If multiple query capabilities exist, the expression may need an explicit capability parameter."
                    .into(),
            ];
            StepError::new(
                StepErrorCategory::Config,
                append_correction_lines(msg, hints),
                None,
            )
        }
        RuntimeError::DecodeError { source } => StepError::new(
            StepErrorCategory::Runtime,
            append_correction_lines(
                source.to_string(),
                vec![
                    "Check CML `response` / `items` mapping matches the live API envelope.".into(),
                ],
            ),
            None,
        ),
        RuntimeError::CmlError { source } => StepError::new(
            StepErrorCategory::Config,
            append_correction_lines(
                source.to_string(),
                vec!["Check path/query/body templates and env var names in mappings.yaml.".into()],
            ),
            None,
        ),
        RuntimeError::HostTransport { .. } | RuntimeError::HttpTransport { .. } => StepError::new(
            StepErrorCategory::Network,
            err.to_string(),
            None,
        ),
        RuntimeError::RequestError { source, .. } => StepError::new(
            StepErrorCategory::Network,
            append_correction_lines(
                source.to_string(),
                vec!["Confirm --backend base URL, network reachability, and TLS.".into()],
            ),
            None,
        ),
        RuntimeError::WorkflowConflict {
            conflict, ..
        } => StepError::new(
            StepErrorCategory::Network,
            append_correction_lines(
                conflict.markdown_block(),
                vec!["Resolve the workflow conflict before retrying the mutator.".into()],
            ),
            None,
        ),
        RuntimeError::RateLimited {
            source,
            retry_after,
            ..
        } => {
            let mut hints = vec![
                "Upstream rate limited (HTTP 429 or quota-exhausted 403); reduce concurrency or retry after the rate-limit window."
                    .into(),
            ];
            if let Some(d) = retry_after {
                hints.push(format!("Retry-After hint: {}s.", d.as_secs()));
            }
            StepError::new(
                StepErrorCategory::Network,
                append_correction_lines(source.to_string(), hints),
                None,
            )
        }
        RuntimeError::CacheError(_) => {
            StepError::new(StepErrorCategory::Runtime, err.to_string(), None)
        }
        RuntimeError::CacheSource(_) | RuntimeError::CacheValueRow(_) | RuntimeError::CacheRowDecode(_) => StepError::new(
            StepErrorCategory::Runtime,
            err.to_string(),
            None,
        ),
        RuntimeError::UnsupportedExecutionMode { mode } => StepError::new(
            StepErrorCategory::Config,
            format!("Execution mode '{mode}' not supported"),
            None,
        ),
        RuntimeError::CapabilityNotFound { capability, entity } => StepError::new(
            StepErrorCategory::Config,
            append_correction_lines(
                err.to_string(),
                vec![format!(
                    "Check domain.yaml: capability '{capability}' on entity '{entity}' must exist."
                )],
            ),
            None,
        ),
        RuntimeError::FingerprintNotFound => StepError::new(
            StepErrorCategory::Runtime,
            append_correction_lines(
                err.to_string(),
                vec!["Replay/hybrid mode: record a matching request first.".into()],
            ),
            None,
        ),
        RuntimeError::ReplayEntryNotFound { fingerprint } => StepError::new(
            StepErrorCategory::Runtime,
            format!("Replay miss for fingerprint {fingerprint}"),
            None,
        ),
        RuntimeError::ReplayStoreError(_) => {
            StepError::new(StepErrorCategory::Config, err.to_string(), None)
        }
        RuntimeError::PaginationProgress { .. } => {
            StepError::new(StepErrorCategory::Runtime, err.to_string(), None)
        }
        RuntimeError::SerializationError(_) => {
            StepError::new(StepErrorCategory::Runtime, err.to_string(), None)
        }
        RuntimeError::CredentialProvider { .. } | RuntimeError::AuthenticationError(_) => StepError::new(
            StepErrorCategory::Auth,
            append_correction_lines(
                err.to_string(),
                vec![
                    "Set the env vars declared in the CGS `auth` block (see schema README).".into(),
                ],
            ),
            None,
        ),
        RuntimeError::Cancelled => StepError::new(
            StepErrorCategory::Runtime,
            "operation cancelled".to_string(),
            None,
        ),
        RuntimeError::HydrationGet {
            cap_name,
            entity_type,
            source,
        } => {
            let inner = step_error_from_runtime(source, cgs);
            StepError::new(
                inner.category,
                format!(
                    "synthesized GET `{cap_name}` during {entity_type} hydration\n\n{}",
                    inner.correction
                ),
                inner.span_offset,
            )
        }
        RuntimeError::DerivedGetNotFound { .. }
        | RuntimeError::DerivedGetNonUnique { .. }
        | RuntimeError::DerivedGetSourceFieldMissing { .. }
        | RuntimeError::DerivedGetIncompleteSource { .. } => StepError::new(
            StepErrorCategory::Runtime,
            append_correction_lines(
                err.to_string(),
                vec![
                    "Derived Get runs a source Query then unique-matches the identity; check the key and that the list materializes fully."
                        .into(),
                ],
            ),
            None,
        ),
    }
}

#[cfg(test)]
mod collection_fault_tests {
    use super::*;

    #[test]
    fn collection_contract_fault_does_not_instruct_python_repair_or_write_retry() {
        let fault = plasm_core::collection_codec::CollectionFault::Conservation;
        let expected = fault.to_string();
        let error = step_error_from_runtime(&RuntimeError::from(fault), &CGS::new());
        assert_eq!(error.category, StepErrorCategory::Runtime);
        assert_eq!(error.correction, expected);
        assert_eq!(error.span_offset, None);
    }
}
