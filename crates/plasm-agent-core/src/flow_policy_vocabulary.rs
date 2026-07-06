//! Catalog-derived vocabulary for project flow policy authoring.

use std::collections::BTreeSet;

use plasm_core::CgsCatalog;
use plasm_core::DataClassSeverity;
use plasm_core::CGS;
use serde::Serialize;

use crate::catalog_runtime::CatalogRuntime;
use crate::flow_catalog::FlowCatalogView;
use crate::flow_policy_repository::FlowPolicyRepository;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DataClassVocabEntry {
    pub id: String,
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogVocabulary {
    pub entry_id: String,
    pub catalog_has_labels: bool,
    pub data_classes: Vec<DataClassVocabEntry>,
    pub sink_classes: Vec<String>,
    pub entities: Vec<String>,
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
            description: (!schema.description.is_empty()).then(|| schema.description.clone()),
        })
        .collect();
    data_classes.sort_by(|a, b| a.id.cmp(&b.id));

    let view = FlowCatalogView::from_cgs(entry_id, cgs);
    let mut sink_classes: BTreeSet<String> = BTreeSet::new();
    for sinks in view.capability_sink_params.values() {
        for s in sinks {
            if let Some(sc) = s.sink_class.as_ref() {
                sink_classes.insert(sc.as_str().to_string());
            }
        }
    }

    let mut entities: Vec<String> = cgs.entities.keys().map(|k| k.to_string()).collect();
    entities.sort();

    let catalog_has_labels = !data_classes.is_empty();
    CatalogVocabulary {
        entry_id: entry_id.to_string(),
        catalog_has_labels,
        data_classes,
        sink_classes: sink_classes.into_iter().collect(),
        entities,
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
