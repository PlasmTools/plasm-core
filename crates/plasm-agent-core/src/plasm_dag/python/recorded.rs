//! Historical values are admitted as data, never as executable producer graphs.
use super::*;
use crate::run_artifacts::{
    parse_run_artifact_document_bytes, verify_payload_run_id, CompletedOutputError,
};

impl Lower<'_> {
    pub(super) fn recorded(
        &mut self,
        site: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        if self.frame.depth != 0 {
            return Err(at(site, PythonSourceError::RecordedOutputScope));
        }
        let [PyExpr::StringLiteral(reference)] = call.arguments.args.as_ref() else {
            return Err(at(site, PythonSourceError::RecordedOutputReference));
        };
        if !call.arguments.keywords.is_empty() {
            return Err(at(site, PythonSourceError::RecordedOutputReference));
        }
        let reference = reference.value.to_str();
        let failed = |error| {
            at(
                site,
                PythonSourceError::RecordedOutput(std::sync::Arc::new(error)),
            )
        };
        let stored = self
            .es
            .recorded_outputs
            .get(reference)
            .ok_or_else(|| failed(CompletedOutputError::Unavailable))?;
        let flow = stored
            .flow
            .as_ref()
            .ok_or_else(|| failed(CompletedOutputError::MissingFlowEvidence))?;
        verify_payload_run_id(&stored.payload, stored.run_id)
            .map_err(|error| failed(error.into()))?;
        let document = parse_run_artifact_document_bytes(&stored.payload.bytes)
            .map_err(|error| failed(error.into()))?;
        if document.prompt_hash != self.es.prompt_hash || document.principal != self.es.principal {
            return Err(failed(CompletedOutputError::SessionMismatch));
        }
        let collection = document
            .recorded_collection()
            .map_err(|error| failed(error.into()))?;
        if collection.coverage() != plasm_runtime::ResultCoverage::Complete {
            return Err(failed(CompletedOutputError::Incomplete));
        }
        if collection.row_materialization() != plasm_runtime::execution::RowMaterialization::Catalog
            || !self
                .es
                .contexts_by_entry
                .get(&document.entry_id)
                .is_some_and(|context| context.cgs.entities.contains_key(document.entity.as_str()))
        {
            return Err(at(site, PythonSourceError::RecordedOutputContract));
        }
        let rows = collection
            .resident_entities()
            .iter()
            .map(|row| {
                plasm_core::Value::Object(
                    row.fields
                        .iter()
                        .map(|(key, value)| (key.clone(), value.to_value()))
                        .collect(),
                )
            })
            .collect();
        let value =
            plasm_core::operand_binding::ResolvedValue::try_from(plasm_core::Value::Array(rows))
                .map_err(|_| at(site, PythonSourceError::RecordedOutputContract))?;
        self.recorded_outputs.insert(
            id.into(),
            RecordedOutputOrigin {
                run_id: stored.run_id.to_wire(),
                entry_id: document.entry_id,
                entity: document.entity,
                labels: flow.labels.clone(),
                provenance: flow.provenance.clone(),
            },
        );
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: false,
            page_size: None,
            source: super::super::types::DagNodeSource::Data(PlanValue::Literal { value }),
        })
    }
}
