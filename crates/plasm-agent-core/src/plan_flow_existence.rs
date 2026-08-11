//! `verify_existence_flow` — PLT guard for non-idempotent mutators with declared `identity_key`.

use crate::flow_catalog::FlowCatalogView;
use crate::plan_flow::{FlowViolation, FlowViolationKind, NodeDisposition, QualifiedCapabilityKey};
use crate::plan_flow_capability::resolve_alias_node;
use crate::plasm_plan::{
    Plan, PlanNodeKind, PlanPredicateOp, PlanResultUse, PlanValue, ValidatedPlanNode,
    ValidatedPlanState, ValidatedSurfaceNode,
};
use plasm_core::schema::{ViewDefinition, ViewNodeSpec};
use plasm_core::{
    CompOp, EntityKey, Expr, Predicate, SemanticEffect, TypedComparisonValue, Value,
    ViewNodeCondition, ViewNodeWhen,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistenceCheckOutcome {
    pub guarded: bool,
    pub reason: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn check_plan_mutation_existence(
    catalog: &FlowCatalogView,
    plan: &Plan<ValidatedPlanState>,
    topo_index: &BTreeMap<String, usize>,
    node_id: &str,
    entry_id: &str,
    entity: &str,
    capability: &str,
    template_expr: Option<&serde_json::Value>,
    uses_result: &[PlanResultUse],
) -> ExistenceCheckOutcome {
    if !catalog.workflow_identity_enabled(entry_id) {
        return guarded_ok();
    }
    let key = QualifiedCapabilityKey::from_parts(entry_id, entity, capability);
    let Some(meta) = catalog.capability_workflow_meta(&key) else {
        return guarded_ok();
    };
    if meta.idempotent {
        return guarded_ok();
    }
    let Some(identity_key) = meta.identity_key.as_ref().filter(|k| !k.is_empty()) else {
        return guarded_ok();
    };

    if let Some(view_key) = catalog.capability_view_key(&key) {
        if let Some(view) = catalog.view_definition(entry_id, view_key) {
            return check_view_existence_flow(catalog, entry_id, view, identity_key);
        }
    }

    let bindings = identity_bindings_from_template(template_expr, identity_key);
    let ancestors = ancestor_nodes(plan, topo_index, node_id, uses_result);
    let guarded = identity_key.iter().all(|param| {
        let expected = bindings.get(param.as_str());
        ancestors
            .iter()
            .any(|anc| read_node_covers_param(plan, entry_id, entity, anc, param, expected))
    });
    if guarded {
        guarded_ok()
    } else {
        ExistenceCheckOutcome {
            guarded: false,
            reason: Some(format!(
                "unguarded mutation: no dominating read covers identity_key [{}] on {entity}.{capability}",
                identity_key.join(", ")
            )),
        }
    }
}

pub fn check_view_existence_flow(
    catalog: &FlowCatalogView,
    entry_id: &str,
    view: &ViewDefinition,
    identity_key: &[String],
) -> ExistenceCheckOutcome {
    let mut prior_read_nodes: BTreeSet<String> = BTreeSet::new();
    for node in &view.nodes {
        let Some(meta) = catalog.capability_workflow_meta(&QualifiedCapabilityKey::from_parts(
            entry_id,
            view.entity.as_str(),
            node.capability.as_str(),
        )) else {
            if is_read_capability_name(catalog, entry_id, view.entity.as_str(), &node.capability) {
                prior_read_nodes.insert(node.id.clone());
            }
            continue;
        };
        if meta.effect == SemanticEffect::Read {
            prior_read_nodes.insert(node.id.clone());
            continue;
        }
        if !matches!(
            meta.effect,
            SemanticEffect::Write | SemanticEffect::SideEffect
        ) {
            continue;
        }
        if meta.idempotent || view_node_guarded_by_when(node, &prior_read_nodes) {
            continue;
        }
        let keys = meta
            .identity_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .unwrap_or(identity_key);
        let inner_guarded = keys
            .iter()
            .all(|param| view_read_covers_param(view, &prior_read_nodes, param, &node.bind));
        if !inner_guarded {
            return ExistenceCheckOutcome {
                guarded: false,
                reason: Some(format!(
                    "unguarded view mutation: view node `{}` ({}) lacks existence read for identity_key [{}]",
                    node.id,
                    node.capability,
                    keys.join(", ")
                )),
            };
        }
    }
    guarded_ok()
}

fn is_read_capability_name(
    catalog: &FlowCatalogView,
    entry_id: &str,
    entity: &str,
    capability: &str,
) -> bool {
    catalog
        .capability_workflow_meta(&QualifiedCapabilityKey::from_parts(
            entry_id, entity, capability,
        ))
        .is_some_and(|m| m.effect == SemanticEffect::Read)
}

fn guarded_ok() -> ExistenceCheckOutcome {
    ExistenceCheckOutcome {
        guarded: true,
        reason: None,
    }
}

fn view_node_guarded_by_when(node: &ViewNodeSpec, prior_reads: &BTreeSet<String>) -> bool {
    let condition = match node.when.as_ref() {
        Some(ViewNodeWhen::SkipIf { condition }) | Some(ViewNodeWhen::RunIf { condition }) => {
            condition
        }
        None => return false,
    };
    match condition {
        ViewNodeCondition::NodeRowCountPositive { node: read_id }
        | ViewNodeCondition::NodeRowCountZero { node: read_id } => prior_reads.contains(read_id),
    }
}

fn view_read_covers_param(
    view: &ViewDefinition,
    prior_reads: &BTreeSet<String>,
    param: &str,
    bind: &indexmap::IndexMap<String, plasm_core::schema::ViewParamBinding>,
) -> bool {
    for read_id in prior_reads {
        let Some(read_node) = view.nodes.iter().find(|n| n.id == *read_id) else {
            continue;
        };
        if read_node.bind.contains_key(param) {
            return true;
        }
    }
    if view.scope.iter().any(|s| s.name == param) {
        return bind.get(param).is_some_and(|b| {
            matches!(
                b,
                plasm_core::schema::ViewParamBinding::Scope { .. }
                    | plasm_core::schema::ViewParamBinding::Literal { .. }
            )
        });
    }
    false
}

fn read_node_covers_param(
    plan: &Plan<ValidatedPlanState>,
    entry_id: &str,
    entity: &str,
    node_id: &str,
    param: &str,
    expected: Option<&IdentityBinding>,
) -> bool {
    let Some(ValidatedPlanNode::Surface(surface)) =
        plan.nodes.iter().find(|n| n.id().as_str() == node_id)
    else {
        return false;
    };
    if !read_node_covers_entity(surface, entry_id, entity) {
        return false;
    }
    let constraints = surface_read_constraints(surface);
    match expected {
        Some(IdentityBinding::Literal(val)) => constraints.get(param) == Some(val),
        Some(IdentityBinding::FromAlias { alias, .. }) => {
            resolve_alias_node(&surface.uses_result, alias).is_some_and(|src| {
                read_node_covers_param(plan, entry_id, entity, &src, param, None)
            })
        }
        None | Some(IdentityBinding::Unknown) => constraints.contains_key(param),
    }
}

fn read_node_covers_entity(surface: &ValidatedSurfaceNode, entry_id: &str, entity: &str) -> bool {
    if !matches!(
        surface.kind,
        PlanNodeKind::Query | PlanNodeKind::Search | PlanNodeKind::Get
    ) {
        return false;
    }
    surface
        .qualified_entity
        .as_ref()
        .is_some_and(|q| q.entry_id.as_str() == entry_id && q.entity.as_str() == entity)
}

fn surface_read_constraints(surface: &ValidatedSurfaceNode) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for p in &surface.predicates {
        if p.op != PlanPredicateOp::Eq {
            continue;
        }
        let field = p
            .field_path
            .segments()
            .last()
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();
        if let PlanValue::Literal { value } = &p.value {
            if let Some(s) = json_scalar_to_string(value) {
                out.insert(field, s);
            }
        }
    }
    if let Some(ir) = &surface.ir {
        extract_expr_read_constraints(&ir.expr, &mut out);
    }
    out
}

fn extract_expr_read_constraints(expr: &Expr, out: &mut BTreeMap<String, String>) {
    match expr {
        Expr::Query(q) => {
            if let Some(pred) = &q.predicate {
                extract_predicate_eq_literals(pred, out);
            }
        }
        Expr::Get(g) => match &g.reference.key {
            EntityKey::Simple(id) => {
                out.insert("id".to_string(), id.to_string());
            }
            EntityKey::Compound(parts) => {
                for (k, v) in parts {
                    out.insert(k.clone(), v.clone());
                }
            }
        },
        _ => {}
    }
}

fn extract_predicate_eq_literals(pred: &Predicate, out: &mut BTreeMap<String, String>) {
    match pred {
        Predicate::Comparison {
            field,
            op: CompOp::Eq,
            value,
        } => {
            if let Some(s) = comparison_value_as_string(value) {
                out.insert(field.clone(), s);
            }
        }
        Predicate::And { args } => {
            for a in args {
                extract_predicate_eq_literals(a, out);
            }
        }
        _ => {}
    }
}

fn comparison_value_as_string(value: &TypedComparisonValue) -> Option<String> {
    match &value.to_value() {
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn json_scalar_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn ancestor_nodes(
    plan: &Plan<ValidatedPlanState>,
    topo_index: &BTreeMap<String, usize>,
    node_id: &str,
    uses_result: &[PlanResultUse],
) -> BTreeSet<String> {
    let Some(&self_idx) = topo_index.get(node_id) else {
        return BTreeSet::new();
    };
    let mut out = BTreeSet::new();
    let mut stack: Vec<String> = uses_result.iter().map(|u| u.node.clone()).collect();
    while let Some(id) = stack.pop() {
        if !out.insert(id.clone()) {
            continue;
        }
        if topo_index.get(&id).is_some_and(|i| *i >= self_idx) {
            continue;
        }
        if let Some(uses) = plan
            .nodes
            .iter()
            .find(|n| n.id().as_str() == id.as_str())
            .map(|n| match n {
                ValidatedPlanNode::Surface(s) => s.uses_result.clone(),
                ValidatedPlanNode::ForEach(f) => f.uses_result.clone(),
                _ => Vec::new(),
            })
        {
            for u in uses {
                stack.push(u.node);
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IdentityBinding {
    Literal(String),
    FromAlias { alias: String, path: Vec<String> },
    Unknown,
}

fn identity_bindings_from_template(
    template_expr: Option<&serde_json::Value>,
    identity_key: &[String],
) -> BTreeMap<String, IdentityBinding> {
    let mut out = BTreeMap::new();
    let Some(expr) = template_expr else {
        return out;
    };
    let input = expr
        .get("input")
        .or_else(|| expr.get("params"))
        .and_then(|v| v.as_object());
    let Some(input) = input else {
        return out;
    };
    for key in identity_key {
        let Some(v) = input.get(key.as_str()) else {
            continue;
        };
        out.insert(
            key.clone(),
            identity_binding_from_value(v).unwrap_or(IdentityBinding::Unknown),
        );
    }
    out
}

fn identity_binding_from_value(v: &serde_json::Value) -> Option<IdentityBinding> {
    if let Some(s) = v.as_str() {
        return Some(IdentityBinding::Literal(s.to_string()));
    }
    if let Some(n) = v.as_i64() {
        return Some(IdentityBinding::Literal(n.to_string()));
    }
    if let Some(hole) = v.get("__plasm_hole") {
        if hole.get("kind").and_then(|k| k.as_str()) == Some("node_input") {
            let alias = hole.get("alias").and_then(|a| a.as_str())?.to_string();
            let path = hole
                .get("path")
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            return Some(IdentityBinding::FromAlias { alias, path });
        }
    }
    None
}

pub(crate) fn apply_unguarded_mutation_review(
    disposition: &mut NodeDisposition,
    violations: &mut Vec<FlowViolation>,
    node_id: &str,
    outcome: ExistenceCheckOutcome,
) {
    if outcome.guarded {
        return;
    }
    let reason = outcome
        .reason
        .unwrap_or_else(|| "unguarded mutation".to_string());
    if !matches!(disposition, NodeDisposition::Deny) {
        *disposition = NodeDisposition::Review;
    }
    violations.push(FlowViolation {
        node: node_id.to_string(),
        kind: Some(FlowViolationKind::UnguardedMutation),
        sink_param: None,
        labels: BTreeSet::new(),
        reason,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_flow::{verify_plan_flow, FlowVerdict};
    use crate::plan_flow_policy::FlowPolicySnapshot;
    use plasm_core::load_schema_dir_unvalidated;
    use plasm_core::schema::{ViewDefinition, ViewNodeSpec};

    fn workflow_matrix_catalog() -> FlowCatalogView {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/schemas/workflow_matrix");
        let cgs = load_schema_dir_unvalidated(&dir).expect("workflow_matrix");
        FlowCatalogView::from_cgs("wf", &cgs)
    }

    #[test]
    fn view_skip_if_positive_guards_inner_create() {
        let view = ViewDefinition {
            description: "ensure".into(),
            capability: "workitem_ensure".into(),
            entity: "WorkItem".into(),
            scope: vec![],
            nodes: vec![
                ViewNodeSpec {
                    id: "check".into(),
                    capability: "workitem_query".into(),
                    bind: indexmap::IndexMap::new(),
                    when: None,
                },
                ViewNodeSpec {
                    id: "write".into(),
                    capability: "workitem_create".into(),
                    bind: indexmap::IndexMap::new(),
                    when: Some(ViewNodeWhen::SkipIf {
                        condition: ViewNodeCondition::NodeRowCountPositive {
                            node: "check".into(),
                        },
                    }),
                },
            ],
            output: indexmap::IndexMap::new(),
            relation_outputs: vec![],
        };
        let catalog = workflow_matrix_catalog();
        let outcome = check_view_existence_flow(&catalog, "wf", &view, &["title".into()]);
        assert!(outcome.guarded, "{:?}", outcome.reason);
    }

    #[test]
    fn guarded_query_then_create_passes_existence() {
        use crate::plasm_plan::parse_and_validate_plan_json;

        let catalog = workflow_matrix_catalog();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "lookup",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "wf", "entity": "WorkItem" },
                    "effect_class": "read",
                    "result_shape": "list",
                    "predicates": [{
                        "field_path": ["title"],
                        "op": "eq",
                        "value": { "kind": "literal", "value": "alpha" }
                    }],
                    "ir": { "expr": { "op": "query", "entity": "WorkItem", "capability": "workitem_query" } }
                },
                {
                    "id": "create",
                    "kind": "action",
                    "qualified_entity": { "entry_id": "wf", "entity": "WorkItem" },
                    "depends_on": ["lookup"],
                    "uses_result": [{ "node": "lookup", "as": "lookup" }],
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
                }
            ],
            "return": { "kind": "node", "node": "create" }
        });
        let validated = parse_and_validate_plan_json(&plan).expect("validate");
        let checked = verify_plan_flow(
            validated.artifact(),
            &["lookup".to_string(), "create".to_string()],
            &catalog,
            &FlowPolicySnapshot::Inactive,
        );
        assert!(
            matches!(checked.analysis.verdict, FlowVerdict::Clean),
            "{:?}",
            checked.analysis.violations
        );
    }

    #[test]
    fn workflow_identity_unguarded_create_needs_review() {
        use crate::plasm_plan::parse_and_validate_plan_json;

        let catalog = workflow_matrix_catalog();
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
                        "input": { "title": "demo" }
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
        assert!(checked
            .analysis
            .violations
            .iter()
            .any(|v| { v.kind == Some(FlowViolationKind::UnguardedMutation) }));
    }
}
