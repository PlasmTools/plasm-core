//! In-memory tenant workflow registry (OSS tests + local dev).

use std::collections::BTreeMap;
use std::sync::RwLock;

use crate::workflow_manifest::WorkflowManifest;

#[derive(Default)]
pub struct WorkflowRegistry {
    inner: RwLock<BTreeMap<String, WorkflowManifest>>,
}

impl WorkflowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, manifest: WorkflowManifest) {
        let id = manifest.id.clone();
        self.inner.write().expect("workflow registry lock").insert(id, manifest);
    }

    pub fn get(&self, id: &str) -> Option<WorkflowManifest> {
        self.inner.read().expect("workflow registry lock").get(id).cloned()
    }

    pub fn list_ids(&self) -> Vec<String> {
        self.inner
            .read()
            .expect("workflow registry lock")
            .keys()
            .cloned()
            .collect()
    }
}

pub fn demo_workflow_manifests() -> Vec<WorkflowManifest> {
    vec![
        WorkflowManifest {
            schema_version: crate::workflow_manifest::WORKFLOW_MANIFEST_SCHEMA_VERSION,
            id: "dossier_linear".into(),
            title: "Specimen dossier → Linear comment".into(),
            description: "Cross-catalog read + template + write (demo shape)".into(),
            program_template: concat!(
                "source = {{sym:pokeapi.Berry}}.limit(1)\n",
                "target_id = {{param:linear_issue_id}}\n",
                "body = {{param:dossier_body}}\n",
                "source\n"
            )
            .into(),
            seeds: vec![
                crate::workflow_manifest::WorkflowSeed {
                    entry_id: "pokeapi".into(),
                    entity: "Berry".into(),
                },
                crate::workflow_manifest::WorkflowSeed {
                    entry_id: "linear".into(),
                    entity: "Issue".into(),
                },
            ],
            parameters: vec![
                crate::workflow_manifest::WorkflowParameter {
                    name: "linear_issue_id".into(),
                    description: "Linear issue id".into(),
                    bind: crate::workflow_manifest::WorkflowSemanticBind {
                        kind: crate::workflow_manifest::WorkflowBindKind::EntityField,
                        entry_id: "linear".into(),
                        entity: "Issue".into(),
                        capability: None,
                        param: None,
                        field: Some("id".into()),
                        value_ref: None,
                    },
                    required: true,
                },
                crate::workflow_manifest::WorkflowParameter {
                    name: "dossier_body".into(),
                    description: "Rendered dossier markdown".into(),
                    bind: crate::workflow_manifest::WorkflowSemanticBind {
                        kind: crate::workflow_manifest::WorkflowBindKind::TemplateString,
                        entry_id: "linear".into(),
                        entity: "Issue".into(),
                        capability: Some("comment_create".into()),
                        param: Some("body".into()),
                        field: None,
                        value_ref: None,
                    },
                    required: true,
                },
            ],
            catalog_pins: vec!["pokeapi".into(), "linear".into()],
        },
        workflow_matrix_manifest(),
    ]
}

pub fn workflow_matrix_manifest() -> WorkflowManifest {
    WorkflowManifest {
        schema_version: crate::workflow_manifest::WORKFLOW_MANIFEST_SCHEMA_VERSION,
        id: "workflow_matrix_parallel".into(),
        title: "Workflow matrix parallel reads".into(),
        description: "Two-catalog parallel read fixture program".into(),
        program_template: concat!(
            "a = {{sym:catalog_a.WorkItem}}.limit({{param:limit}})\n",
            "b = {{sym:catalog_b.WorkItem}}.limit({{param:limit}})\n",
            "a\n"
        )
        .into(),
        seeds: vec![
            crate::workflow_manifest::WorkflowSeed {
                entry_id: "catalog_a".into(),
                entity: "WorkItem".into(),
            },
            crate::workflow_manifest::WorkflowSeed {
                entry_id: "catalog_b".into(),
                entity: "WorkItem".into(),
            },
        ],
        parameters: vec![crate::workflow_manifest::WorkflowParameter {
            name: "limit".into(),
            description: "Row limit".into(),
            bind: crate::workflow_manifest::WorkflowSemanticBind {
                kind: crate::workflow_manifest::WorkflowBindKind::CapabilityParam,
                entry_id: "catalog_a".into(),
                entity: "WorkItem".into(),
                capability: Some("workitem_query".into()),
                param: Some("limit".into()),
                field: None,
                value_ref: None,
            },
            required: false,
        }],
        catalog_pins: vec!["catalog_a".into(), "catalog_b".into()],
    }
}
