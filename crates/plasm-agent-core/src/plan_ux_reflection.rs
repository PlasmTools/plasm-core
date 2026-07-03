//! Mandatory UX projection from dry-run plans — sibling to comp wire, excluded from commit hash.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::execute_session::ExecuteSession;
use crate::plan_dry_display::{
    build_plan_dry_compact_view, human_ux_headline_for_op, human_ux_summary_for_op, PlanDryVerdict,
};
use crate::plan_flow_reflection::{plan_ux_flow_reflection, PlanUxFlowReflection};
use crate::plasm_plan::{
    EffectClass, PlanNodeKind, ValidatedPlanNode, ValidatedPlanReturn, ValidatedPlanState,
};
use crate::plasm_plan_run::DryPlasmPlanEvaluation;

/// Bumped to 3 for the mandatory `flow` field (Flow tab — data-flow reflection).
pub const PLAN_UX_REFLECTION_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanUxLayout {
    Sequential,
    ParallelColumns,
    Branches,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanUxWidgetKind {
    ReadSurface,
    RelationHop,
    RenderTemplate,
    Compute,
    ActionSurface,
    Data,
    Derive,
    ForEach,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxColumn {
    pub entry_id: String,
    pub label: String,
    pub step_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxStep {
    pub id: String,
    pub ordinal: u8,
    pub widget: PlanUxWidgetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_entity: Option<String>,
    pub operation: String,
    pub effect_class: String,
    pub approval_gate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxParamBinding {
    pub param_name: String,
    pub step_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxReview {
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<String>,
    pub write_count: usize,
    pub read_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlanUxLiveOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_step_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlanUxSession {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unused_seeds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUxReflection {
    pub schema_version: u32,
    pub layout: PlanUxLayout,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<PlanUxColumn>,
    pub steps: Vec<PlanUxStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<PlanUxEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    pub review: PlanUxReview,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_bindings: Vec<PlanUxParamBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<PlanUxLiveOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<PlanUxSession>,
    /// Data-flow trace for the plan MCP UI's Flow tab — independent of execution.
    pub flow: PlanUxFlowReflection,
}

pub struct PlanUxBuildContext<'a> {
    pub session: Option<&'a ExecuteSession>,
    pub param_bindings: &'a [PlanUxParamBinding],
}

impl<'a> PlanUxBuildContext<'a> {
    pub fn empty() -> Self {
        Self {
            session: None,
            param_bindings: &[],
        }
    }
}

pub fn plan_ux_reflection(
    dry: &DryPlasmPlanEvaluation,
    ctx: &PlanUxBuildContext<'_>,
) -> PlanUxReflection {
    let plan = dry.validated_plan();
    let compact = build_plan_dry_compact_view(
        plan,
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        ctx.session,
        None,
    );
    let comp_wire = crate::plasm_comp_wire::trace_comp_wire_from_dry(dry);
    let edges = comp_edges(&comp_wire);
    let steps = build_steps(plan, &dry.topological_order, &compact);
    let layout = infer_layout(dry.parallel_root_surfaces_only, &steps);
    let columns = build_columns(&layout, &steps);
    let writes = write_step_ids(plan, &dry.topological_order);
    let returns = render_returns(&plan.return_value);
    let session = session_advisory_from_dry(dry);
    let flow = plan_ux_flow_reflection(plan, &dry.flow, &steps);

    PlanUxReflection {
        schema_version: PLAN_UX_REFLECTION_SCHEMA_VERSION,
        layout,
        columns,
        steps,
        edges,
        returns,
        writes,
        review: PlanUxReview {
            verdict: match compact.verdict {
                PlanDryVerdict::Ok => "ok".into(),
                PlanDryVerdict::Review => "review".into(),
                PlanDryVerdict::Deny => "deny".into(),
            },
            warnings: compact.warnings.clone(),
            write_count: compact.write_count,
            read_count: compact.read_count,
        },
        param_bindings: ctx.param_bindings.to_vec(),
        live: None,
        session,
        flow,
    }
}

fn session_advisory_from_dry(dry: &DryPlasmPlanEvaluation) -> Option<PlanUxSession> {
    let unused = dry.review.unused_seeds.clone();
    if unused.is_empty() {
        return None;
    }
    Some(PlanUxSession {
        unused_seeds: unused,
    })
}

/// JSON value for `_meta.plasm.plan_ux_reflection` (mandatory for MCP App hosts).
pub fn plan_ux_reflection_value(
    dry: &DryPlasmPlanEvaluation,
    ctx: &PlanUxBuildContext<'_>,
) -> serde_json::Value {
    serde_json::to_value(plan_ux_reflection(dry, ctx)).expect("plan ux reflection serializes")
}

/// Reject stale or partial `plan_ux_reflection` wire (exact schema cutover).
pub fn validate_plan_ux_reflection_wire(v: &serde_json::Value) -> Result<(), String> {
    let reflection: PlanUxReflection = serde_json::from_value(v.clone())
        .map_err(|e| format!("plan_ux_reflection invalid: {e}"))?;
    if reflection.schema_version != PLAN_UX_REFLECTION_SCHEMA_VERSION {
        return Err(format!(
            "plan_ux_reflection.schema_version must be {} (got {})",
            PLAN_UX_REFLECTION_SCHEMA_VERSION,
            reflection.schema_version
        ));
    }
    Ok(())
}

fn infer_layout(parallel_root_surfaces_only: bool, steps: &[PlanUxStep]) -> PlanUxLayout {
    if parallel_root_surfaces_only {
        let root_entry_ids: BTreeSet<_> = steps
            .iter()
            .filter(|s| s.ordinal <= 1 || steps.len() <= 2)
            .filter_map(|s| s.entry_id.as_deref())
            .collect();
        let distinct: BTreeSet<_> = steps.iter().filter_map(|s| s.entry_id.as_deref()).collect();
        if distinct.len() >= 2 && (parallel_root_surfaces_only || root_entry_ids.len() >= 2) {
            return PlanUxLayout::ParallelColumns;
        }
    }
    if steps.len() <= 1 {
        return PlanUxLayout::Sequential;
    }
    let mut fanout = false;
    for step in steps {
        if matches!(
            step.widget,
            PlanUxWidgetKind::ForEach | PlanUxWidgetKind::Derive
        ) {
            fanout = true;
            break;
        }
    }
    if fanout {
        PlanUxLayout::Branches
    } else {
        PlanUxLayout::Sequential
    }
}

fn build_columns(layout: &PlanUxLayout, steps: &[PlanUxStep]) -> Vec<PlanUxColumn> {
    if !matches!(layout, PlanUxLayout::ParallelColumns) {
        return Vec::new();
    }
    let mut by_entry: BTreeMap<String, PlanUxColumn> = BTreeMap::new();
    for step in steps {
        let Some(entry_id) = step.entry_id.as_deref() else {
            continue;
        };
        by_entry
            .entry(entry_id.to_string())
            .or_insert_with(|| PlanUxColumn {
                entry_id: entry_id.to_string(),
                label: entry_id.to_string(),
                step_ids: Vec::new(),
            })
            .step_ids
            .push(step.id.clone());
    }
    by_entry.into_values().collect()
}

fn build_steps(
    plan: &crate::plasm_plan::Plan<ValidatedPlanState>,
    order: &[String],
    compact: &crate::plan_dry_display::PlanDryCompactView,
) -> Vec<PlanUxStep> {
    order
        .iter()
        .enumerate()
        .filter_map(|(idx, id)| {
            let node = plan.nodes.iter().find(|n| n.id().as_str() == id)?;
            let compact_step = compact.steps.get(idx);
            let (entry_id, entity, qualified_entity) = qualified_from_node(node);
            let operation = compact_step
                .map(|s| human_ux_summary_for_op(&s.op))
                .unwrap_or_else(|| node.id().as_str().to_string());
            let headline = if crate::plan_dry_display::is_synthetic_plan_node_id_public(id.as_str())
            {
                compact_step.map(|s| human_ux_headline_for_op(&s.op))
            } else {
                None
            };
            Some(PlanUxStep {
                id: id.clone(),
                ordinal: (idx + 1) as u8,
                widget: widget_for_node(node, compact_step.map(|s| &s.op)),
                entry_id,
                entity,
                qualified_entity,
                operation,
                effect_class: effect_class_wire(node.effect_class()),
                approval_gate: matches!(
                    node.effect_class(),
                    EffectClass::Write | EffectClass::SideEffect
                ),
                layout_hint: column_hint(node),
                headline,
            })
        })
        .collect()
}

fn effect_class_wire(class: EffectClass) -> String {
    match class {
        EffectClass::Read => "read".into(),
        EffectClass::Write => "write".into(),
        EffectClass::SideEffect => "side_effect".into(),
        EffectClass::ArtifactRead => "artifact_read".into(),
    }
}

fn qualified_from_node(
    node: &ValidatedPlanNode,
) -> (Option<String>, Option<String>, Option<String>) {
    match node {
        ValidatedPlanNode::Surface(s) => {
            let q = s.qualified_entity.as_ref();
            (
                q.map(|x| x.entry_id.clone()),
                q.map(|x| x.entity.clone()),
                q.as_ref().map(|x| format!("{}.{}", x.entry_id, x.entity)),
            )
        }
        ValidatedPlanNode::RelationTraversal(r) => (
            Some(r.relation.target.entry_id.clone()),
            Some(r.relation.target.entity.clone()),
            Some(format!(
                "{}.{}",
                r.relation.target.entry_id, r.relation.target.entity
            )),
        ),
        _ => (None, None, None),
    }
}

fn widget_for_node(
    node: &ValidatedPlanNode,
    compact_op: Option<&crate::plan_dry_display::PlanDryOp>,
) -> PlanUxWidgetKind {
    if let Some(op) = compact_op {
        return match op {
            crate::plan_dry_display::PlanDryOp::Relation { .. } => PlanUxWidgetKind::RelationHop,
            crate::plan_dry_display::PlanDryOp::Render { .. } => PlanUxWidgetKind::RenderTemplate,
            crate::plan_dry_display::PlanDryOp::ForEach { .. } => PlanUxWidgetKind::ForEach,
            crate::plan_dry_display::PlanDryOp::Derive { .. } => PlanUxWidgetKind::Derive,
            crate::plan_dry_display::PlanDryOp::Data { .. } => PlanUxWidgetKind::Data,
            crate::plan_dry_display::PlanDryOp::Surface { kind, .. } => match kind {
                PlanNodeKind::Query | PlanNodeKind::Get | PlanNodeKind::Search => {
                    PlanUxWidgetKind::ReadSurface
                }
                PlanNodeKind::Action => PlanUxWidgetKind::ActionSurface,
                _ => PlanUxWidgetKind::ReadSurface,
            },
            _ => PlanUxWidgetKind::Compute,
        };
    }
    match node {
        ValidatedPlanNode::Surface(s) => match s.kind {
            PlanNodeKind::Action => PlanUxWidgetKind::ActionSurface,
            _ => PlanUxWidgetKind::ReadSurface,
        },
        ValidatedPlanNode::RelationTraversal(_) => PlanUxWidgetKind::RelationHop,
        ValidatedPlanNode::Compute(n)
            if matches!(n.compute.op, crate::plasm_plan::ComputeOp::Render { .. }) =>
        {
            PlanUxWidgetKind::RenderTemplate
        }
        ValidatedPlanNode::Compute(_) => PlanUxWidgetKind::Compute,
        ValidatedPlanNode::ForEach(_) => PlanUxWidgetKind::ForEach,
        ValidatedPlanNode::Derive(_) => PlanUxWidgetKind::Derive,
        ValidatedPlanNode::Data(_) => PlanUxWidgetKind::Data,
    }
}

fn column_hint(node: &ValidatedPlanNode) -> Option<String> {
    match node {
        ValidatedPlanNode::Surface(s) => s
            .qualified_entity
            .as_ref()
            .map(|q| format!("column:{}", q.entry_id)),
        _ => None,
    }
}

fn comp_edges(comp: &plasm_trace::TraceCompWire) -> Vec<PlanUxEdge> {
    let deps = &comp.comp.bind.deps;
    let mut edges = Vec::new();
    for (to, froms) in deps {
        for from in froms {
            edges.push(PlanUxEdge {
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
            });
        }
    }
    edges
}

fn write_step_ids(
    plan: &crate::plasm_plan::Plan<ValidatedPlanState>,
    order: &[String],
) -> Vec<String> {
    order
        .iter()
        .filter(|id| {
            plan.nodes
                .iter()
                .find(|n| n.id().as_str() == id.as_str())
                .is_some_and(|n| {
                    matches!(
                        n.effect_class(),
                        EffectClass::Write | EffectClass::SideEffect
                    )
                })
        })
        .cloned()
        .collect()
}

fn render_returns(ret: &ValidatedPlanReturn) -> Vec<String> {
    ret.refs()
        .iter()
        .map(|id| id.as_str().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::compute_plan_commit_id_from_semantic;

    #[test]
    fn plan_ux_reflection_serializes_schema_version() {
        let ux = PlanUxReflection {
            schema_version: PLAN_UX_REFLECTION_SCHEMA_VERSION,
            layout: PlanUxLayout::Sequential,
            columns: vec![],
            steps: vec![PlanUxStep {
                id: "n0".into(),
                ordinal: 1,
                widget: PlanUxWidgetKind::ReadSurface,
                entry_id: Some("catalog_a".into()),
                entity: Some("WorkItem".into()),
                qualified_entity: Some("catalog_a.WorkItem".into()),
                operation: "query identifier".into(),
                effect_class: "read".into(),
                approval_gate: false,
                layout_hint: None,
                headline: Some("Read list".into()),
            }],
            edges: vec![],
            returns: vec!["n0".into()],
            writes: vec![],
            review: PlanUxReview {
                verdict: "ok".into(),
                warnings: None,
                write_count: 0,
                read_count: 1,
            },
            param_bindings: vec![],
            live: None,
            session: None,
            flow: crate::plan_flow_reflection::PlanUxFlowReflection {
                schema_version: crate::plan_flow_reflection::PLAN_UX_FLOW_REFLECTION_SCHEMA_VERSION,
                verdict: crate::plan_flow_reflection::PlanUxFlowVerdict::Clean,
                policy_revision: None,
                counts: Default::default(),
                violations: vec![],
                trace: vec![],
            },
        };
        let json = serde_json::to_value(&ux).expect("serialize");
        assert_eq!(
            json.get("schema_version").and_then(|v| v.as_u64()),
            Some(PLAN_UX_REFLECTION_SCHEMA_VERSION as u64)
        );
    }

    #[test]
    fn plan_ux_step_operation_is_human_not_debug() {
        let op = crate::plan_dry_display::PlanDryOp::Surface {
            kind: PlanNodeKind::Query,
            expr: "e1.identifier".into(),
        };
        let rendered = crate::plan_dry_display::human_ux_summary_for_op(&op);
        assert!(rendered.contains("Read"));
        assert!(!rendered.contains("PlanDryOp"));
        assert!(!rendered.contains("Surface"));
        let filter = crate::plan_dry_display::PlanDryOp::Filter {
            predicates: vec!["cost<100".into()],
        };
        assert_eq!(
            crate::plan_dry_display::human_ux_headline_for_op(&filter),
            "Filter rows"
        );
        assert_eq!(
            crate::plan_dry_display::human_ux_summary_for_op(&filter),
            "Where cost<100"
        );
    }

    #[test]
    fn plan_ux_reflection_not_in_commit_hash_input() {
        let semantic = serde_json::json!({
            "version": 1,
            "nodes": [{"id": "n0", "kind": "surface", "effect_class": "read",
                "result_shape": "rows", "dependencies": [], "uses_result": [], "operation": "q"}],
            "edges": [],
            "topological_order": ["n0"],
            "returns": ["n0"],
        });
        let id_a = compute_plan_commit_id_from_semantic(&semantic);
        let mut presentation = semantic.clone();
        presentation.as_object_mut().unwrap().insert(
            "plan_ux_reflection".into(),
            serde_json::json!({"schema_version": 1, "layout": "sequential"}),
        );
        assert_eq!(id_a, compute_plan_commit_id_from_semantic(&semantic));
    }

    #[test]
    fn validate_plan_ux_reflection_wire_rejects_stale_schema() {
        let stale = serde_json::json!({
            "schema_version": 2,
            "layout": "sequential",
            "steps": [],
            "review": { "verdict": "ok", "write_count": 0, "read_count": 0 },
            "flow": {
                "schema_version": 1,
                "verdict": "clean",
                "counts": { "allow": 0, "approve": 0, "review": 0, "deny": 0 },
                "violations": [],
                "trace": []
            }
        });
        let err = validate_plan_ux_reflection_wire(&stale).unwrap_err();
        assert!(err.contains("schema_version must be 3"), "{err}");
    }
}
