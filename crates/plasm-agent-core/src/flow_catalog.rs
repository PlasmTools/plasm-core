//! Typed CGS → flow-catalog projection (no JSON round-trip).

use crate::plan_flow::{QualifiedCapabilityKey, SinkParamRef};
use plasm_core::{CapabilitySchema, DataClassName, CGS};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One pinned catalog graph (session-agnostic).
pub struct CatalogPin<'a> {
    pub entry_id: &'a str,
    pub cgs: &'a CGS,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct FlowCatalogView {
    pub capability_output_labels: BTreeMap<QualifiedCapabilityKey, BTreeSet<DataClassName>>,
    pub capability_sink_params: BTreeMap<QualifiedCapabilityKey, Vec<SinkParamRef>>,
    pub capability_sanitizers: BTreeMap<QualifiedCapabilityKey, BTreeSet<DataClassName>>,
}

impl FlowCatalogView {
    pub fn output_labels_for(&self, key: &QualifiedCapabilityKey) -> BTreeSet<DataClassName> {
        self.capability_output_labels
            .get(key)
            .cloned()
            .unwrap_or_default()
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
        view.capability_sanitizers.insert(key, sanitizers);
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

    #[test]
    fn from_pins_federates_multiple_entries() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/flow_matrix");
        let cgs = load_schema_dir_unvalidated(&dir).expect("load flow_matrix fixture");
        let view = FlowCatalogView::from_pins([
            CatalogPin {
                entry_id: "flow_a",
                cgs: &cgs,
            },
            CatalogPin {
                entry_id: "flow_b",
                cgs: &cgs,
            },
        ]);
        let send_key = QualifiedCapabilityKey::from_parts("flow_b", "Message", "send");
        assert!(!view.sink_params_for(&send_key).is_empty());
    }
}
