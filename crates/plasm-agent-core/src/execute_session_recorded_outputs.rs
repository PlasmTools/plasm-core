//! Session-owned archive locators retain provenance, not historical row bodies.
use super::*;
use crate::run_artifacts::CompletedOutputError;
use indexmap::IndexSet;
use ruff_python_ast::visitor::{self, Visitor};

pub(super) type RecordedOutputs =
    HashMap<String, Result<Arc<SessionRunArtifact>, Arc<CompletedOutputError>>>;

#[derive(Clone)]
pub(super) struct ArchivedOutput {
    store: RunArtifactStore,
    prompt_hash: String,
    session_id: String,
    pub(super) resource_index: u64,
    seq: DeltaSeq,
    epoch: GraphEpoch,
    pub(super) flow: Option<crate::plan_flow::FlowFacts>,
}

impl std::fmt::Debug for ArchivedOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchivedOutput")
            .field("resource_index", &self.resource_index)
            .finish_non_exhaustive()
    }
}

impl ArchivedOutput {
    async fn load(
        &self,
        id: RunArtifactId,
    ) -> Result<Arc<SessionRunArtifact>, CompletedOutputError> {
        let payload = self
            .store
            .get_payload_result(&self.prompt_hash, &self.session_id, id)
            .await?
            .ok_or(CompletedOutputError::Unavailable)?;
        crate::run_artifacts::verify_payload_run_id(&payload, id)?;
        let document = crate::run_artifacts::parse_run_artifact_document_bytes(&payload.bytes)?;
        if document.prompt_hash != self.prompt_hash || document.session_id != self.session_id {
            return Err(CompletedOutputError::SessionMismatch);
        }
        // Digests identify plans, not observations. Never pair a replacement
        // payload with an older observation's flow proof.
        if document.resource_index != Some(self.resource_index) {
            return Err(CompletedOutputError::MissingFlowEvidence);
        }
        Ok(Arc::new(SessionRunArtifact {
            run_id: id,
            resource_index: self.resource_index,
            seq: self.seq,
            epoch: self.epoch,
            payload,
            flow: self.flow.clone(),
        }))
    }
}

impl SessionCore {
    #[cfg(test)]
    pub(crate) async fn evict_recorded_payloads_for_test(&self) {
        let mut state = self.state.lock().await;
        while !state.run_artifacts.map.is_empty() {
            state.run_artifacts.evict_one();
        }
    }

    pub(crate) async fn retain_output_archive(
        &self,
        artifact: &SessionRunArtifact,
        store: RunArtifactStore,
        prompt_hash: &str,
        session_id: &str,
    ) {
        let mut state = self.state.lock().await;
        if state
            .archived_outputs
            .get(&artifact.run_id)
            .is_some_and(|existing| existing.seq > artifact.seq)
            || state
                .run_artifacts
                .get(artifact.run_id)
                .is_some_and(|existing| existing.seq > artifact.seq)
        {
            return;
        }
        state.archived_outputs.insert(
            artifact.run_id,
            ArchivedOutput {
                store,
                prompt_hash: prompt_hash.into(),
                session_id: session_id.into(),
                resource_index: artifact.resource_index,
                seq: artifact.seq,
                epoch: artifact.epoch,
                flow: artifact.flow.clone(),
            },
        );
    }

    pub(crate) async fn resolve_recorded_output(
        &self,
        id: RunArtifactId,
    ) -> Result<Arc<SessionRunArtifact>, CompletedOutputError> {
        let archive = {
            let state = self.state.lock().await;
            if let Some(hot) = state.run_artifacts.get(id) {
                return Ok(hot);
            }
            state
                .archived_outputs
                .get(&id)
                .cloned()
                .ok_or(CompletedOutputError::Unavailable)?
        };
        archive.load(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefetch_only_literal_recorded_calls_and_deduplicate() {
        let id = RunArtifactId::from_bytes([7; 32]);
        let reference = id.to_wire();
        let source = format!(
            "class P(Program):\n    def build(self):\n        text = '{reference}'\n        other.recorded('{reference}')\n        self.recorded('{reference}')\n        return self.recorded('{reference}')\n"
        );
        let parsed = ruff_python_parser::parse_module(&source).unwrap();
        let mut references = References::default();
        for statement in parsed.suite() {
            references.visit_stmt(statement);
        }
        assert_eq!(references.0.into_iter().collect::<Vec<_>>(), vec![id]);
        let parsed = ruff_python_parser::parse_module(&format!(
            "text = '{reference}'\nother.recorded('{reference}')\nself.recorded(text)\n"
        ))
        .unwrap();
        let mut references = References::default();
        for statement in parsed.suite() {
            references.visit_stmt(statement);
        }
        assert!(references.0.is_empty());
    }
}

impl ExecuteSession {
    pub(crate) async fn recorded_output_snapshot(&self, source: &str) -> Self {
        let mut snapshot = self.preflight_snapshot().await;
        // Parse only to prefetch literal references. The normal frontend still
        // owns syntax, scope, eligibility and source-located diagnostics.
        let mut references = References::default();
        if let Ok(module) = ruff_python_parser::parse_module(source) {
            for statement in module.suite() {
                references.visit_stmt(statement);
            }
        }
        for reference in references.0 {
            let result = self
                .core
                .resolve_recorded_output(reference)
                .await
                .map_err(Arc::new);
            Arc::make_mut(&mut snapshot.recorded_outputs).insert(reference.to_wire(), result);
        }
        snapshot
    }
}

#[derive(Default)]
struct References(IndexSet<RunArtifactId>);
impl<'a> Visitor<'a> for References {
    fn visit_expr(&mut self, expression: &'a ruff_python_ast::Expr) {
        use ruff_python_ast::Expr;
        if let Expr::Call(call) = expression {
            if let Expr::Attribute(method) = call.func.as_ref() {
                if method.attr.as_str() == "recorded"
                    && matches!(method.value.as_ref(), Expr::Name(name) if name.id.as_str() == "self")
                {
                    if let [Expr::StringLiteral(literal)] = call.arguments.args.as_ref() {
                        if let Some(id) = RunArtifactId::from_wire(literal.value.to_str()) {
                            self.0.insert(id);
                        }
                    }
                }
            }
        }
        visitor::walk_expr(self, expression);
    }
}
