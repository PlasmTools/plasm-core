//! Mandatory UX projection from dry-run plans — sibling to comp wire, excluded from commit hash.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::execute_session::ExecuteSession;
use crate::plan_dry_display::{
    build_plan_dry_compact_view, plan_node_display_map, render_plan_dry_op, PlanDryVerdict,
};
use crate::plasm_plan::{
    EffectClass, PlanNodeKind, ValidatedPlanNode, ValidatedPlanReturn, ValidatedPlanState,
};
use crate::plasm_comp_wire::plasm_comp_json_from_dry;
use crate::plasm_plan_run::DryPlasmPlanEvaluation;

pub const PLAN_UX_REFLECTION_SCHEMA_VERSION: u32 = 1;

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
    pub display_id: Option<String>,
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
    );
    let comp = plasm_comp_json_from_dry(dry);
    let edges = comp_edges(&comp);
    let steps = build_steps(plan, &dry.topological_order, &compact);
    let layout = infer_layout(dry.parallel_root_surfaces_only, &steps);
    let columns = build_columns(&layout, &steps);
    let writes = write_step_ids(plan, &dry.topological_order);
    let returns = render_returns(&plan.return_value);

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
            },
            warnings: compact.warnings.clone(),
            write_count: compact.write_count,
            read_count: compact.read_count,
        },
        param_bindings: ctx.param_bindings.to_vec(),
        live: None,
    }
}

/// JSON value for `_meta.plasm.plan_ux_reflection` (mandatory for MCP App hosts).
pub fn plan_ux_reflection_value(
    dry: &DryPlasmPlanEvaluation,
    ctx: &PlanUxBuildContext<'_>,
) -> serde_json::Value {
    serde_json::to_value(plan_ux_reflection(dry, ctx)).expect("plan ux reflection serializes")
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
    let display_map = plan_node_display_map(plan, order);
    order
        .iter()
        .enumerate()
        .filter_map(|(idx, id)| {
            let node = plan.nodes.iter().find(|n| n.id().as_str() == id)?;
            let compact_step = compact.steps.get(idx);
            let (entry_id, entity, qualified_entity) = qualified_from_node(node);
            let operation = compact_step
                .map(|s| render_plan_dry_op(&s.op))
                .unwrap_or_else(|| node.id().as_str().to_string());
            let display_id = display_map
                .get(id.as_str())
                .cloned()
                .filter(|label| label != id);
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
                display_id,
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

fn comp_edges(comp: &serde_json::Value) -> Vec<PlanUxEdge> {
    let Some(deps) = comp
        .get("bind")
        .and_then(|b| b.get("deps"))
        .and_then(|d| d.as_object())
    else {
        return Vec::new();
    };
    let mut edges = Vec::new();
    for (to, froms) in deps {
        let Some(arr) = froms.as_array() else {
            continue;
        };
        for from in arr {
            if let Some(from_s) = from.as_str() {
                edges.push(PlanUxEdge {
                    from: from_s.to_string(),
                    to: to.clone(),
                });
            }
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
                display_id: Some("r1".into()),
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
        let rendered = render_plan_dry_op(&op);
        assert_eq!(rendered, "query e1.identifier");
        assert!(!rendered.contains("PlanDryOp"));
        assert!(!rendered.contains("Surface"));
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
}
