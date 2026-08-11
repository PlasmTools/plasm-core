//! Typed CGS → flow-catalog projection (no JSON round-trip).

use crate::plan_flow::{QualifiedCapabilityKey, SinkParamRef};
use plasm_core::schema::ViewDefinition;
use plasm_core::{
    flow_control_param_names, CapabilityKind, CapabilitySchema, DataClassName, SemanticEffect, CGS,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One pinned catalog graph (session-agnostic).
pub struct CatalogPin<'a> {
    pub entry_id: &'a str,
    pub cgs: &'a CGS,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityWorkflowMeta {
    pub kind: CapabilityKind,
    pub effect: SemanticEffect,
    pub identity_key: Option<Vec<String>>,
    pub idempotent: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct FlowCatalogView {
    pub capability_output_labels: BTreeMap<QualifiedCapabilityKey, BTreeSet<DataClassName>>,
    pub capability_sink_params: BTreeMap<QualifiedCapabilityKey, Vec<SinkParamRef>>,
    pub capability_sanitizers: BTreeMap<QualifiedCapabilityKey, BTreeSet<DataClassName>>,
    /// Behavior-controlling parameter names per capability (robust-declass: taint here voids clearance).
    pub capability_control_params: BTreeMap<QualifiedCapabilityKey, BTreeSet<String>>,
    /// Per-entry `workflow_identity: true` from domain.yaml.
    #[serde(default)]
    pub entry_workflow_identity: BTreeMap<String, bool>,
    /// Declared workflow metadata per capability (kind + identity + idempotent).
    #[serde(default)]
    pub capability_workflow: BTreeMap<QualifiedCapabilityKey, CapabilityWorkflowMeta>,
    /// View DAG definitions keyed by `(entry_id, view_key)`.
    #[serde(default)]
    pub views: BTreeMap<String, BTreeMap<String, ViewDefinition>>,
    /// Outer capability → composed view key (`transport: view` mappings).
    #[serde(default)]
    pub capability_view_key: BTreeMap<QualifiedCapabilityKey, String>,
}

impl FlowCatalogView {
    pub fn output_labels_for(&self, key: &QualifiedCapabilityKey) -> BTreeSet<DataClassName> {
        self.capability_output_labels
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn output_labels_for_entity(
        &self,
        entry_id: &str,
        entity: &str,
    ) -> BTreeSet<DataClassName> {
        let mut out = BTreeSet::new();
        for (key, labels) in &self.capability_output_labels {
            if key.entry_id.as_str() == entry_id && key.entity.as_str() == entity {
                out.extend(labels.iter().cloned());
            }
        }
        out
    }

    pub fn sink_params_for(&self, key: &QualifiedCapabilityKey) -> &[SinkParamRef] {
        self.capability_sink_params
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn sanitizers_for(&self, key: &QualifiedCapabilityKey) -> BTreeSet<DataClassName> {
        self.capability_sanitizers
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn control_params_for(&self, key: &QualifiedCapabilityKey) -> BTreeSet<String> {
        self.capability_control_params
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn workflow_identity_enabled(&self, entry_id: &str) -> bool {
        self.entry_workflow_identity
            .get(entry_id)
            .copied()
            .unwrap_or(false)
    }

    pub fn capability_workflow_meta(
        &self,
        key: &QualifiedCapabilityKey,
    ) -> Option<&CapabilityWorkflowMeta> {
        self.capability_workflow.get(key)
    }

    pub fn capability_view_key(&self, key: &QualifiedCapabilityKey) -> Option<&str> {
        self.capability_view_key.get(key).map(String::as_str)
    }

    pub fn view_definition(&self, entry_id: &str, view_key: &str) -> Option<&ViewDefinition> {
        self.views.get(entry_id)?.get(view_key)
    }

    pub fn from_cgs(entry_id: &str, cgs: &CGS) -> Self {
        let mut view = Self::default();
        view.merge_cgs(entry_id, cgs);
        view
    }

    pub fn from_pins<'a>(pins: impl IntoIterator<Item = CatalogPin<'a>>) -> Self {
        let mut view = Self::default();
        for pin in pins {
            view.merge_cgs(pin.entry_id, pin.cgs);
        }
        view
    }

    pub fn merge_cgs(&mut self, entry_id: &str, cgs: &CGS) {
        if cgs.workflow_identity {
            self.entry_workflow_identity
                .insert(entry_id.to_string(), true);
        }
        for (view_key, view_def) in &cgs.views {
            self.views
                .entry(entry_id.to_string())
                .or_default()
                .insert(view_key.clone(), view_def.clone());
            let cap_key = QualifiedCapabilityKey::from_parts(
                entry_id,
                view_def.entity.as_str(),
                view_def.capability.as_str(),
            );
            self.capability_view_key.insert(cap_key, view_key.clone());
        }
        for (cap_name, cap) in &cgs.capabilities {
            ingest_capability(self, entry_id, cap_name.as_str(), cap, cgs);
        }
    }
}

fn ingest_capability(
    view: &mut FlowCatalogView,
    entry_id: &str,
    cap_name: &str,
    cap: &CapabilitySchema,
    cgs: &CGS,
) {
    let entity_name = cap.domain.as_str();
    if entity_name.is_empty() {
        return;
    }
    let key = QualifiedCapabilityKey::from_parts(entry_id, entity_name, cap_name);

    let idempotent = cap.output_schema.as_ref().is_some_and(|o| o.idempotent);
    let effect = cap.effective_effect();
    if cap.is_remote_mutation() || cap.is_read() || cap.identity_key.is_some() || idempotent {
        view.capability_workflow.insert(
            key.clone(),
            CapabilityWorkflowMeta {
                kind: cap.kind,
                effect,
                identity_key: cap.identity_key.clone(),
                idempotent,
            },
        );
    }

    let labels: BTreeSet<DataClassName> = cgs
        .capability_output_data_classes(cap)
        .into_iter()
        .cloned()
        .collect();
    if !labels.is_empty() {
        view.capability_output_labels.insert(key.clone(), labels);
    }

    let sinks: Vec<SinkParamRef> = cgs
        .capability_sink_params(cap)
        .into_iter()
        .filter_map(|param| {
            param.sink_class.as_ref().map(|sink_class| SinkParamRef {
                param: plasm_core::CapabilityParamName::from(param.name.as_str()),
                sink_class: Some(sink_class.clone()),
            })
        })
        .collect();
    if !sinks.is_empty() {
        view.capability_sink_params.insert(key.clone(), sinks);
    }

    let sanitizers: BTreeSet<DataClassName> = cap.sanitizes.iter().cloned().collect();
    if !sanitizers.is_empty() {
        view.capability_sanitizers.insert(key.clone(), sanitizers);
    }

    let control_params: BTreeSet<String> = cap
        .input_schema
        .as_ref()
        .map(flow_control_param_names)
        .unwrap_or_default()
        .into_iter()
        .collect();
    if !control_params.is_empty() {
        view.capability_control_params.insert(key, control_params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::load_schema_dir_unvalidated;

    #[test]
    fn from_cgs_uses_typed_sink_and_output_helpers() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/flow_matrix");
        let cgs = load_schema_dir_unvalidated(&dir).expect("load flow_matrix fixture");
        let view = FlowCatalogView::from_cgs("flow", &cgs);
        let send_key = QualifiedCapabilityKey::from_parts("flow", "Message", "send");
        assert!(
            view.sink_params_for(&send_key).iter().any(|p| p
                .sink_class
                .as_ref()
                .is_some_and(|s| s.as_str() == "outbound_body")),
            "send capability should expose outbound_body sink param"
        );
    }
}
