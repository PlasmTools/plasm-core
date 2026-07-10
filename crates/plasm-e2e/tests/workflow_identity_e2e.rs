//! Workflow identity matrix: schema validation, PLT existence flow, reconcile semantics.

use plasm_agent::{verify_plan_flow, FlowCatalogView, FlowPolicySnapshot, FlowVerdict};
use plasm_compile::CmlEnv;
use plasm_core::load_schema_dir_unvalidated;
use plasm_core::preflight::PLASM_EXISTENCE_SKIP_WRITE_ENV;
use plasm_core::{
    conflict_rules_from_mapping_template, match_conflict_rule, plasm_value_to_json, Value,
    WorkflowConflictKind,
};
use plasm_runtime::workflow_reconcile::{
    detect_identity_mismatch, should_skip_write_after_preflight, skipped_write_result,
};

fn workflow_matrix_dir() -> std::path::PathBuf {
    let crate_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        crate_root.join("../../fixtures/schemas/workflow_matrix"),
        crate_root.join("../../../fixtures/schemas/workflow_matrix"),
    ];
    for p in &candidates {
        if p.join("domain.yaml").exists() {
            return p.clone();
        }
    }
    panic!("workflow_matrix fixture not found (tried {candidates:?})");
}

#[test]
fn workflow_matrix_schema_loads_with_workflow_identity() {
    let cgs = load_schema_dir_unvalidated(&workflow_matrix_dir()).expect("load");
    assert!(cgs.workflow_identity);
    let cap = cgs.get_capability("workitem_create").expect("create");
    assert_eq!(
        cap.identity_key.as_deref(),
        Some(&["title".to_string()][..])
    );
    assert!(cgs.views.contains_key("workitem_ensure"));
}

#[test]
fn workflow_matrix_view_ensure_passes_plt_existence() {
    let cgs = load_schema_dir_unvalidated(&workflow_matrix_dir()).expect("load");
    let catalog = FlowCatalogView::from_cgs("wf", &cgs);
    let view = cgs.views.get("workitem_ensure").expect("view");
    let outcome = plasm_agent::plan_flow_existence::check_view_existence_flow(
        &catalog,
        "wf",
        view,
        &["title".to_string()],
    );
    assert!(
        outcome.guarded,
        "conditional write view should be guarded: {:?}",
        outcome.reason
    );
}

#[test]
fn workflow_matrix_unguarded_plan_needs_review() {
    use plasm_agent::plasm_plan::parse_and_validate_plan_json;

    let cgs = load_schema_dir_unvalidated(&workflow_matrix_dir()).expect("load");
    let catalog = FlowCatalogView::from_cgs("wf", &cgs);
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "create",
            "kind": "action",
            "qualified_entity": { "entry_id": "wf", "entity": "WorkItem" },
            "effect_class": "side_effect",
            "result_shape": "side_effect_ack",
            "ir_template": {
                "expr": {
                    "op": "invoke",
                    "capability": "workitem_create",
                    "target": { "entity_type": "WorkItem", "key": {} },
                    "input": { "title": "alpha" }
                }
            }
        }],
        "return": { "kind": "node", "node": "create" }
    });
    let validated = parse_and_validate_plan_json(&plan).expect("validate");
    let checked = verify_plan_flow(
        validated.artifact(),
        &["create".to_string()],
        &catalog,
        &FlowPolicySnapshot::Inactive,
    );
    assert!(matches!(checked.analysis.verdict, FlowVerdict::NeedsReview));
}

#[test]
fn workflow_matrix_idempotent_cap_declares_reconcile_via_query() {
    let cgs = load_schema_dir_unvalidated(&workflow_matrix_dir()).expect("load");
    let cap = cgs
        .get_capability("workitem_create_idempotent")
        .expect("idempotent create");
    let output = cap.output_schema.as_ref().expect("output");
    assert!(output.idempotent);
    let reconcile = output.reconcile.as_ref().expect("reconcile");
    assert_eq!(reconcile.via, "workitem_query");
    assert_eq!(reconcile.on, WorkflowConflictKind::ResourceExists);
}

#[test]
fn workflow_matrix_conflict_rule_matches_resource_exists() {
    let cgs = load_schema_dir_unvalidated(&workflow_matrix_dir()).expect("load");
    let cap = cgs
        .get_capability("workitem_create_idempotent")
        .expect("cap");
    let rules = conflict_rules_from_mapping_template(&cap.mapping.template.0);
    let body = serde_json::json!({ "message": "title already exists", "title": "alpha" });
    let conflict = match_conflict_rule(&rules, 422, &body).expect("match");
    assert_eq!(conflict.kind, WorkflowConflictKind::ResourceExists);
}

#[test]
fn workflow_matrix_identity_mismatch_detected_on_extra_field() {
    let cgs = load_schema_dir_unvalidated(&workflow_matrix_dir()).expect("load");
    let cap = cgs
        .get_capability("workitem_create_idempotent")
        .expect("cap");
    let input = Value::Object(indexmap::IndexMap::from([
        ("title".into(), Value::String("alpha".into())),
        ("extra".into(), Value::String("new".into())),
    ]));
    let fetched = plasm_runtime::execution::ExecutionResult {
        entities: vec![plasm_runtime::cache::CachedEntity::from_decoded(
            plasm_core::Ref::new("WorkItem", ""),
            indexmap::IndexMap::from([
                ("title".into(), Value::String("alpha".into())),
                ("extra".into(), Value::String("old".into())),
            ]),
            indexmap::IndexMap::new(),
            0,
            plasm_runtime::cache::EntityCompleteness::Complete,
        )],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: plasm_runtime::execution::ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: Vec::new(),
    };
    let conflict = detect_identity_mismatch(cap, &input, &fetched).expect("mismatch");
    assert_eq!(conflict.kind, WorkflowConflictKind::IdentityMismatch);
    assert!(conflict.existing.is_some());
}

#[test]
fn workflow_matrix_skip_write_preflight_sets_env_flag() {
    let mut env = CmlEnv::new();
    env.insert(
        PLASM_EXISTENCE_SKIP_WRITE_ENV.to_string(),
        Value::Bool(true),
    );
    assert!(should_skip_write_after_preflight(&env));
    let skipped = skipped_write_result("WorkItem");
    let outcome = skipped
        .entities
        .first()
        .and_then(|e| e.fields.get("outcome"))
        .map(|v| plasm_value_to_json(&v.to_value()));
    assert_eq!(outcome, Some(serde_json::json!("skipped")));
}
