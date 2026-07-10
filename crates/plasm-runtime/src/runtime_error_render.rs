//! Map [`RuntimeError`](crate::RuntimeError) into [`StepError`](plasm_core::step_semantics::StepError) for unified UX.

use plasm_core::error_render::render_type_error;
use plasm_core::schema::CGS;
use plasm_core::step_semantics::{append_correction_lines, StepError, StepErrorCategory};

use crate::RuntimeError;

/// Convert a runtime failure into a structured [`StepError`].
pub fn step_error_from_runtime(err: &RuntimeError, cgs: &CGS) -> StepError {
    match err {
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
        RuntimeError::RequestError { message, .. } => StepError::new(
            StepErrorCategory::Network,
            append_correction_lines(
                message.clone(),
                vec!["Confirm --backend base URL, network reachability, and TLS.".into()],
            ),
            None,
        ),
        RuntimeError::WorkflowConflict { conflict, message, .. } => StepError::new(
            StepErrorCategory::Network,
            append_correction_lines(
                format!("{message}\n\n{}", conflict.markdown_block()),
                vec!["Resolve the workflow conflict before retrying the mutator.".into()],
            ),
            None,
        ),
        RuntimeError::RateLimited {
            message,
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
                append_correction_lines(message.clone(), hints),
                None,
            )
        }
        RuntimeError::CacheError { message } => {
            StepError::new(StepErrorCategory::Runtime, message.clone(), None)
        }
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
        RuntimeError::ReplayStoreError { message } => {
            StepError::new(StepErrorCategory::Config, message.clone(), None)
        }
        RuntimeError::ConfigurationError { message } => {
            StepError::new(StepErrorCategory::Config, message.clone(), None)
        }
        RuntimeError::SerializationError { message } => {
            StepError::new(StepErrorCategory::Runtime, message.clone(), None)
        }
        RuntimeError::AuthenticationError { message } => StepError::new(
            StepErrorCategory::Auth,
            append_correction_lines(
                message.clone(),
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
    }
}
