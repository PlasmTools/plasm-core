//! Catalog-derived vocabulary for project flow policy authoring.

use std::collections::BTreeMap;

use plasm_core::CgsCatalog;
use plasm_core::{flow_control_param_names, DataClassSeverity, CGS};
use serde::Serialize;

use crate::catalog_runtime::CatalogRuntime;
use crate::flow_catalog::FlowCatalogView;
use crate::flow_policy_repository::FlowPolicyRepository;
use crate::plan_flow::QualifiedCapabilityKey;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DataClassVocabEntry {
    pub id: String,
    pub severity: String,
    /// `confidentiality` | `integrity` — derived from severity when absent in catalog.
    pub dimension: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SinkClassVocabEntry {
    pub id: String,
    /// Lattice drained by this sink class.
    pub dimension: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CapabilityVocabEntry {
    pub entity: String,
    pub name: String,
    pub kind: String,
    pub effect_class: String,
    /// False for LLM/agentic transforms — cannot be trusted declassifiers.
    pub deterministic: bool,
    pub output_labels: Vec<String>,
    pub sink_classes: Vec<String>,
    /// Behavior-controlling parameter names (robust-declass: taint here voids clearance).
    pub control_params: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogVocabulary {
    pub entry_id: String,
    pub catalog_has_labels: bool,
    pub data_classes: Vec<DataClassVocabEntry>,
    pub sink_classes: Vec<SinkClassVocabEntry>,
    pub entities: Vec<String>,
    pub capabilities: Vec<CapabilityVocabEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectFlowVocabulary {
    pub catalogs: Vec<CatalogVocabulary>,
}

pub fn vocabulary_for_entry(catalog: &CatalogRuntime, entry_id: &str) -> Option<CatalogVocabulary> {
    let reg = catalog.snapshot();
    let ctx = reg.load_context(entry_id).ok()?;
    Some(vocabulary_from_cgs(entry_id, ctx.cgs.as_ref()))
}

pub fn vocabulary_from_cgs(entry_id: &str, cgs: &CGS) -> CatalogVocabulary {
    let mut data_classes: Vec<DataClassVocabEntry> = cgs
        .data_classes
        .iter()
        .map(|(name, schema)| DataClassVocabEntry {
            id: name.as_str().to_string(),
            severity: severity_wire(schema.severity),
            dimension: dimension_wire(schema.dimension_or_default()),
            description: (!schema.description.is_empty()).then(|| schema.description.clone()),
        })
        .collect();
    data_classes.sort_by(|a, b| a.id.cmp(&b.id));

    let label_dim: BTreeMap<&str, &str> = data_classes
        .iter()
        .map(|d| (d.id.as_str(), d.dimension.as_str()))
        .collect();

    let view = FlowCatalogView::from_cgs(entry_id, cgs);
    let mut sink_map: BTreeMap<String, String> = BTreeMap::new();
    for sinks in view.capability_sink_params.values() {
        for s in sinks {
            if let Some(sc) = s.sink_class.as_ref() {
                let id = sc.as_str().to_string();
                let dim = label_dim
                    .get(id.as_str())
                    .copied()
                    .unwrap_or_else(|| sink_dimension_heuristic(id.as_str()))
                    .to_string();
                sink_map.entry(id).or_insert(dim);
            }
        }
    }
    let sink_classes: Vec<SinkClassVocabEntry> = sink_map
        .into_iter()
        .map(|(id, dimension)| SinkClassVocabEntry { id, dimension })
        .collect();

    let mut entities: Vec<String> = cgs.entities.keys().map(|k| k.to_string()).collect();
    entities.sort();

    let mut capabilities: Vec<CapabilityVocabEntry> = Vec::new();
    for (cap_name, cap) in &cgs.capabilities {
        let entity = cap.domain.as_str().to_string();
        let key = QualifiedCapabilityKey::from_parts(entry_id, &entity, cap_name.as_str());
        let mut output_labels: Vec<String> = view
            .output_labels_for(&key)
            .into_iter()
            .map(|l| l.as_str().to_string())
            .collect();
        output_labels.sort();
        let mut sink_classes_cap: Vec<String> = view
            .sink_params_for(&key)
            .iter()
            .filter_map(|p| p.sink_class.as_ref().map(|s| s.as_str().to_string()))
            .collect();
        sink_classes_cap.sort();
        sink_classes_cap.dedup();
        let control_params = cap
            .input_schema
            .as_ref()
            .map(flow_control_param_names)
            .unwrap_or_default();
        let kind = format!("{:?}", cap.kind).to_ascii_lowercase();
        let effect_class = match kind.as_str() {
            "query" | "get" | "search" => "read",
            "create" | "update" | "delete" => "write",
            _ => "side_effect",
        }
        .to_string();
        capabilities.push(CapabilityVocabEntry {
            entity,
            name: cap_name.as_str().to_string(),
            kind,
            effect_class,
            deterministic: cap.is_deterministic(),
            output_labels,
            sink_classes: sink_classes_cap,
            control_params,
        });
    }
    capabilities.sort_by(|a, b| (&a.entity, &a.name).cmp(&(&b.entity, &b.name)));

    CatalogVocabulary {
        entry_id: entry_id.to_string(),
        catalog_has_labels: !data_classes.is_empty(),
        data_classes,
        sink_classes,
        entities,
        capabilities,
    }
}

fn sink_dimension_heuristic(id: &str) -> &'static str {
    match id {
        "permission_grant" | "acl_write" | "authz" | "policy_write" => "integrity",
        _ => "confidentiality",
    }
}

pub async fn project_vocabulary(
    repo: &FlowPolicyRepository,
    catalog: &CatalogRuntime,
    tenant_id: &str,
    workspace_slug: &str,
    project_slug: &str,
) -> Result<ProjectFlowVocabulary, crate::flow_policy_repository::FlowPolicyRepositoryError> {
    let entry_ids = repo
        .enabled_entry_ids_for_project(tenant_id, workspace_slug, project_slug)
        .await?;
    let mut catalogs = Vec::new();
    for eid in entry_ids {
        if let Some(v) = vocabulary_for_entry(catalog, eid.as_str()) {
            catalogs.push(v);
        }
    }
    Ok(ProjectFlowVocabulary { catalogs })
}

fn severity_wire(severity: DataClassSeverity) -> String {
    match severity {
        DataClassSeverity::Info => "info".into(),
        DataClassSeverity::Sensitive => "sensitive".into(),
        DataClassSeverity::Untrusted => "untrusted".into(),
        DataClassSeverity::Critical => "critical".into(),
    }
}

fn dimension_wire(dim: plasm_core::DataClassDimension) -> String {
    match dim {
        plasm_core::DataClassDimension::Confidentiality => "confidentiality".into(),
        plasm_core::DataClassDimension::Integrity => "integrity".into(),
    }
}
