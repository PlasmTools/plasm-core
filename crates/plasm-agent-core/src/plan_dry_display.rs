//! Typed compact dry-run plan display — built from [`ValidatedPlanNode`] / [`ComputeOp`], not parsed text.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    AggregateFunction, AggregateSpec, ComputeOp, ComputeTemplate, EffectClass, FieldPath, Plan,
    PlanNodeKind, PlanPredicate, PlanPredicateOp, PlanValue, ValidatedEffectTemplate,
    ValidatedPlan, ValidatedPlanExprIr, ValidatedPlanExprTemplate, ValidatedPlanNode,
    ValidatedPlanReturn, ValidatedPlanState, ValidatedSurfaceNode,
};

use plasm_core::SelectionEffect;
use serde::{Deserialize, Serialize};

mod projection;

/// Size-comparison oracle for tests only: the former one-line-per-IR-node view.
#[cfg(test)]
pub(crate) fn unfolded_review_steps(
    plan: &Plan<ValidatedPlanState>,
    order: &[String],
    es: Option<&ExecuteSession>,
) -> Vec<PlanDryStep> {
    let names = build_plan_node_display_map(&plan.nodes, order);
    order
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let node = plan.nodes.iter().find(|n| n.id().as_str() == id)?;
            Some(PlanDryStep {
                ordinal: (i + 1).min(u8::MAX as usize) as u8,
                id: map_display_id(id, &names),
                synthetic: is_synthetic_plan_node_id(id),
                op: compact_op_from_node(node, es, &names),
                uses: step_upstream_labels(node, &names),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDryVerdict {
    Ok,
    Review,
    Deny,
    /// Correctable program diagnostic (parse / type / preflight) — not a tool fault.
    NeedsFix,
}

impl PlanDryVerdict {
    /// Canonical agent/control-plane wire string (`ok` | `review` | `deny` | `needs_fix`).
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Review => "review",
            Self::Deny => "deny",
            Self::NeedsFix => "needs_fix",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDryReview {
    pub has_unprojected_multi_row_read: bool,
    pub has_unbounded_read_root: bool,
    pub has_full_collection_compute: bool,
    pub has_foreach_fanout_risk: bool,
    /// `RelationTraversal` with `source_cardinality: many` (per upstream row).
    pub has_relation_many_source_fanout: bool,
    /// `query` surface → `.limit` → `.filter` on materialized rows (fetch vs row filter nudge).
    pub has_query_limit_row_filter: bool,
    /// Paginated list/search surface without pushed read budget or explicit page_size.
    pub has_paginated_list_fetch_all_default: bool,
    /// Embed-style relation with downstream `.limit` but no pushed relation read budget.
    pub has_unbounded_relation_embed_hydrate: bool,
    pub unused_seeds: Vec<String>,
    /// Binding labels that execute but are neither consumed downstream nor returned.
    pub unused_bindings: Vec<String>,
}

impl PlanDryReview {
    /// True when live execute should auto-async (fanout / true fetch-all), not advisory review alone.
    /// Unnarrowed roots with a default host page stay sync but still `needs_review` for MCP plan return.
    pub fn execution_is_expensive(&self) -> bool {
        crate::plan_read_bounds::read_execution_is_expensive(
            self.has_unbounded_read_root,
            self.has_paginated_list_fetch_all_default,
            self.has_relation_many_source_fanout,
            self.has_foreach_fanout_risk,
        )
    }

    pub fn needs_review(&self, return_unbounded_root: bool) -> bool {
        self.has_unprojected_multi_row_read
            || self.has_unbounded_read_root
            || return_unbounded_root
            || self.has_full_collection_compute
            || self.has_foreach_fanout_risk
            || self.has_relation_many_source_fanout
    }

    pub fn warning_line(&self, return_unbounded_root: bool) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.has_unprojected_multi_row_read {
            parts.push("project list reads".to_string());
        }
        if (self.has_unbounded_read_root || return_unbounded_root)
            && !parts.iter().any(|p| p.contains("unbounded"))
        {
            parts.push("unbounded read".to_string());
        }
        if self.has_full_collection_compute && !parts.iter().any(|p| p.contains("project")) {
            parts.push("narrow before aggregate/limit".to_string());
        }
        if self.has_foreach_fanout_risk {
            parts.push("for_each fanout".to_string());
        }
        if self.has_relation_many_source_fanout {
            parts.push("relation per-row fanout".to_string());
        }
        if self.has_unbounded_relation_embed_hydrate {
            parts.push("relation embed hydrate unbounded".to_string());
        }
        if self.has_query_limit_row_filter && !parts.iter().any(|p| p.contains("fetch filter")) {
            parts.push("fetch filter: e1{…} at HTTP, binding.filter{…} on rows".to_string());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("; "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDryCompactView {
    pub verdict: PlanDryVerdict,
    pub node_count: usize,
    pub read_count: usize,
    pub write_count: usize,
    pub return_label: String,
    /// When true, compact dry-run text includes the bind-ordered execution footer.
    pub show_execution_order_footer: bool,
    pub deny_line: Option<String>,
    pub warnings: Option<String>,
    pub steps: Vec<PlanDryStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDryStep {
    pub ordinal: u8,
    pub id: String,
    pub synthetic: bool,
    pub op: PlanDryOp,
    pub uses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDryOp {
    MapBody {
        source: String,
        effects: Vec<PlanDryEffect>,
    },
    Python {
        entity: String,
        per_row: bool,
        input: Option<String>,
    },
    Surface {
        kind: PlanNodeKind,
        expr: String,
        selections: Vec<PlanDrySelection>,
        description: String,
    },
    Project {
        fields: Vec<String>,
    },
    Filter {
        predicates: Vec<String>,
    },
    GroupBy {
        keys: Vec<String>,
        aggregates: String,
    },
    Aggregate {
        aggregates: String,
    },
    Sort {
        key: String,
        descending: bool,
    },
    Limit {
        count: usize,
    },
    Dedupe {
        keys: Vec<String>,
    },
    With {
        columns: Vec<String>,
    },
    Render {
        columns: Vec<String>,
        template_chars: usize,
    },
    Union {
        other: String,
    },
    MergeBranches {
        other: String,
    },
    ForEach {
        source: String,
        binding: String,
        body: String,
    },
    IterateUntil {
        source: String,
        binding: String,
        body: String,
        take: u32,
    },
    Relation {
        relation: String,
        target: String,
        expr: String,
    },
    Data {
        summary: String,
    },
    Derive {
        source: String,
        binding: String,
        summary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDryEffect {
    pub binding: String,
    pub uses: Vec<String>,
    pub kind: PlanNodeKind,
    pub expression: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDrySelection {
    pub name: String,
    pub effect: SelectionEffect,
}

fn render_selection_effects(selections: &[PlanDrySelection]) -> String {
    if selections.is_empty() {
        return String::new();
    }
    format!(
        " · {}",
        selections
            .iter()
            .map(|selection| format!("{} {}", selection.name, selection.effect.semantic_gloss()))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

fn map_body_effects(plan: &ValidatedPlan, es: Option<&ExecuteSession>) -> Vec<PlanDryEffect> {
    let mut effects = Vec::new();
    enum Work<'a> {
        Plan(&'a ValidatedPlan),
        Node(
            &'a ValidatedPlanNode,
            std::sync::Arc<projection::Projection>,
        ),
    }
    let mut pending = vec![Work::Plan(plan)];
    while let Some(work) = pending.pop() {
        let (node, labels) = match work {
            Work::Node(node, labels) => (node, labels),
            Work::Plan(plan) => {
                let order = plan
                    .topological_order()
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect::<Vec<_>>();
                let labels = std::sync::Arc::new(projection::Projection::new(
                    plan.nodes(),
                    &order,
                    &plan.artifact().metadata,
                ));
                let by_id: std::collections::HashMap<_, _> =
                    plan.nodes().iter().map(|node| (node.id(), node)).collect();
                for id in plan.topological_order().iter().rev() {
                    if let Some(&node) = by_id.get(id) {
                        pending.push(Work::Node(node, labels.clone()));
                    }
                }
                continue;
            }
        };
        if labels.hidden.contains(node.id().as_str()) {
            continue;
        }
        let projection = labels;
        let labels = &projection.names;
        // Retain compute gates and their row scope rather than flattening a
        // conditional child action into an unconditional list of writes.
        if matches!(
            node,
            ValidatedPlanNode::MapBody(_)
                | ValidatedPlanNode::Compute(_)
                | ValidatedPlanNode::Derive(_)
                | ValidatedPlanNode::Data(_)
                | ValidatedPlanNode::Capture(_)
        ) {
            let op = projection.op(node, es);
            let expression = match op {
                PlanDryOp::Python { input, .. } => compact_python(input.as_deref()),
                PlanDryOp::Derive { summary, .. } | PlanDryOp::Data { summary } => summary,
                _ => render_plan_dry_op(&op),
            };
            effects.push(PlanDryEffect {
                binding: map_display_id(node.id().as_str(), &labels),
                uses: projection.uses(node),
                kind: node.kind(),
                expression,
                description: String::new(),
            });
            continue;
        }
        if let ValidatedPlanNode::Surface(surface) = node {
            // Keep receiver/input reads: their values feed the `$` holes in
            // subsequent calls. Omitting them would hide literal identities.
            effects.push(PlanDryEffect {
                binding: map_display_id(node.id().as_str(), &labels),
                uses: projection.uses(node),
                kind: surface.kind,
                expression: surface_compact_expr(surface, es, &labels),
                description: if matches!(
                    surface.effect_class,
                    EffectClass::Write | EffectClass::SideEffect
                ) {
                    effect_description(
                        crate::plan_flow_capability::surface_capability_key(surface),
                        es,
                    )
                } else {
                    String::new()
                },
            });
        }
        if let ValidatedPlanNode::ForEach(fanout) = node {
            if matches!(
                fanout.effect_class,
                EffectClass::Write | EffectClass::SideEffect
            ) {
                let target = &fanout.effect_template.qualified_entity;
                effects.push(PlanDryEffect {
                    binding: map_display_id(node.id().as_str(), &labels),
                    uses: projection.uses(node),
                    kind: fanout.effect_template.kind,
                    expression: format!(
                        "{} per child row",
                        crate::plan_dry_compact::compact_agent_surface_expr(&effect_template_body(
                            &fanout.effect_template,
                            es,
                            &labels
                        ))
                    ),
                    description: effect_description(
                        Some(crate::plan_flow::QualifiedCapabilityKey::from_parts(
                            target.entry_id.as_str(),
                            target.entity.as_str(),
                            &crate::plan_flow_capability::resolved_mutation_capability_name(
                                Some(&fanout.effect_template.ir_template.expr),
                                fanout.effect_template.kind,
                            ),
                        )),
                        es,
                    ),
                });
            }
        }
        pending.extend(node.nested_plans().rev().map(Work::Plan));
    }
    effects
}

fn render_map_body(source: &str, effects: &[PlanDryEffect]) -> String {
    let actions = effects
        .iter()
        .map(|effect| {
            let operation = with_effect_description(
                if matches!(effect.kind, PlanNodeKind::Compute | PlanNodeKind::MapBody) {
                    effect.expression.clone()
                } else {
                    format!("{} {}", render_kind(effect.kind), effect.expression)
                },
                &effect.description,
            );
            let mut text = format!("{}: {operation}", effect.binding);
            if !effect.uses.is_empty() {
                let _ = write!(text, " ← {}", effect.uses.join(", "));
            }
            format!("[{text}]")
        })
        .collect::<Vec<_>>();
    if actions.is_empty() {
        format!("for each row in {source}")
    } else {
        format!("for each row in {source}: {}", actions.join(" → "))
    }
}

fn with_effect_description(mut text: String, description: &str) -> String {
    if !description.is_empty() {
        let _ = write!(text, " — {description}");
    }
    text
}

fn effect_description(
    key: Option<crate::plan_flow::QualifiedCapabilityKey>,
    es: Option<&ExecuteSession>,
) -> String {
    let Some((key, es)) = key.zip(es) else {
        return String::new();
    };
    let Some(context) = es.contexts_by_entry.get(key.entry_id.as_str()) else {
        return String::new();
    };
    let Some(cap) = context.cgs.capabilities.get(&key.capability) else {
        return String::new();
    };
    let mut descriptions = vec![cap.description.as_str()];
    if let Some(output) = &cap.output_schema {
        if let plasm_core::OutputType::SideEffect { description } = &output.output_type {
            descriptions.push(description);
        }
    }
    let mut normalized = Vec::new();
    for description in descriptions {
        let text = description.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() && !normalized.contains(&text) {
            normalized.push(text);
        }
    }
    normalized.join("; ")
}

pub fn build_plan_dry_compact_view(
    plan: &Plan<ValidatedPlanState>,
    topological_order: &[String],
    review: &PlanDryReview,
    graph_summary: &serde_json::Value,
    es: Option<&ExecuteSession>,
    flow_verdict_override: Option<PlanDryVerdict>,
) -> PlanDryCompactView {
    let projection = projection::Projection::new(&plan.nodes, topological_order, &plan.metadata);
    let display_map = &projection.names;
    let return_unbounded = return_roots_include_unbounded_list_surface(plan);
    let flow_verdict = flow_verdict_override.or_else(|| {
        graph_summary
            .get("security_verdict")
            .and_then(|v| v.as_str())
            .and_then(|v| match v {
                "denied" | "deny" => Some(PlanDryVerdict::Deny),
                "needs_fix" => Some(PlanDryVerdict::NeedsFix),
                "needs_review" | "review" => Some(PlanDryVerdict::Review),
                "clean" | "ok" => Some(PlanDryVerdict::Ok),
                _ => None,
            })
    });
    let inferred_review = if review.needs_review(return_unbounded) {
        PlanDryVerdict::Review
    } else {
        PlanDryVerdict::Ok
    };
    let verdict = std::cmp::max(flow_verdict.unwrap_or(PlanDryVerdict::Ok), inferred_review);
    let deny_line = if verdict == PlanDryVerdict::Deny {
        let violations = graph_summary
            .get("flow_summary")
            .and_then(|v| v.get("violation_count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Some(format!("flow denied: {violations} policy violation(s)"))
    } else {
        None
    };
    let read_count = json_string_array(graph_summary.get("read_nodes")).len();
    let write_count = json_string_array(graph_summary.get("write_or_side_effect_nodes")).len();
    let show_execution_order_footer = matches!(
        &plan.return_value,
        ValidatedPlanReturn::Parallel { parallel } if parallel.len() > 1
    ) && write_count > 1;
    let steps = topological_order
        .iter()
        .enumerate()
        .filter_map(|(ordinal, id)| {
            let node = plan.nodes.iter().find(|n| n.id().as_str() == id)?;
            if projection.hidden.contains(id) {
                return None;
            }
            let display_id = display_map
                .get(id.as_str())
                .cloned()
                .unwrap_or_else(|| id.clone());
            let op = projection.op(node, es);
            let uses = projection.uses(node);
            Some(PlanDryStep {
                ordinal: (ordinal + 1).min(u8::MAX as usize) as u8,
                id: display_id,
                synthetic: is_synthetic_plan_node_id(id),
                op,
                uses,
            })
        })
        .collect();
    PlanDryCompactView {
        verdict,
        node_count: plan.nodes.len(),
        read_count,
        write_count,
        return_label: primary_return_label(plan, &display_map),
        show_execution_order_footer,
        deny_line,
        warnings: review.warning_line(return_unbounded),
        steps,
    }
}

pub fn render_plan_dry_compact_text(
    view: &PlanDryCompactView,
    plan_handle: Option<&str>,
) -> String {
    let mut out = String::new();
    let verdict = view.verdict.as_wire();
    let mut header = format!(
        "plan {verdict} · {} read{} · {} action{}",
        view.read_count,
        if view.read_count == 1 { "" } else { "s" },
        view.write_count,
        if view.write_count == 1 { "" } else { "s" },
    );
    let _ = write!(header, " → {}", view.return_label);
    if let Some(handle) = plan_handle {
        let _ = write!(header, " · {handle}");
    }
    let _ = writeln!(out, "{header}");
    if let Some(deny) = view.deny_line.as_ref() {
        let _ = writeln!(out, "deny: {deny}");
    }
    if let Some(warn) = view.warnings.as_ref() {
        let _ = writeln!(out, "warn: {warn}");
    }
    let _ = writeln!(out);
    for step in &view.steps {
        let op = match &step.op {
            PlanDryOp::Derive { summary, .. } => summary.clone(),
            PlanDryOp::Data { summary } => summary.clone(),
            PlanDryOp::Python { input, .. } => compact_python(input.as_deref()),
            other => render_plan_dry_op(other),
        };
        let uses = &step.uses;
        if uses.is_empty() {
            let _ = writeln!(out, "{}: {op}", step.id);
        } else {
            let _ = writeln!(out, "{}: {op} ← {}", step.id, uses.join(", "));
        }
    }
    if view.show_execution_order_footer {
        let _ = writeln!(out, "Actions execute in source order.");
    }
    out
}

/// Operator-facing step title for synthetic IR nodes (not tuned `read_1`/`compute_2` labels).
pub(crate) fn human_ux_headline_for_op(op: &PlanDryOp) -> String {
    match op {
        PlanDryOp::MapBody { .. } => "For each row".into(),
        PlanDryOp::Python {
            entity, per_row, ..
        } => format!(
            "Python {} of {entity}",
            if *per_row {
                "per-row rendering"
            } else {
                "reduction"
            }
        ),
        PlanDryOp::Surface { kind, .. } => match kind {
            PlanNodeKind::Query | PlanNodeKind::Search | PlanNodeKind::Get => "Read list".into(),
            PlanNodeKind::Create => "Create".into(),
            PlanNodeKind::Update => "Update".into(),
            PlanNodeKind::Delete => "Delete".into(),
            PlanNodeKind::Action => "Write".into(),
            _ => render_kind(*kind).to_string(),
        },
        PlanDryOp::Project { fields } => {
            if fields.len() <= 2 {
                format!("Keep {}", fields.join(", "))
            } else {
                format!("Keep {} fields", fields.len())
            }
        }
        PlanDryOp::Filter { .. } => "Filter rows".into(),
        PlanDryOp::GroupBy { keys, .. } => format!("Group by {}", keys.join(", ")),
        PlanDryOp::Aggregate { .. } => "Summarize".into(),
        PlanDryOp::Sort { key, descending } => {
            if *descending {
                format!("Sort by {key} (descending)")
            } else {
                format!("Sort by {key}")
            }
        }
        PlanDryOp::Limit { count } => format!("Take first {count}"),
        PlanDryOp::Dedupe { keys } if keys.is_empty() => "Distinct rows".into(),
        PlanDryOp::Dedupe { keys } => format!("Dedupe on {}", keys.join(", ")),
        PlanDryOp::With { columns } => format!("Add columns {}", columns.join(", ")),
        PlanDryOp::Render { .. } => "Render text".into(),
        PlanDryOp::MergeBranches { other } => format!("Merge exclusive branches {other}"),
        PlanDryOp::Union { other } => format!("Union {other}"),
        PlanDryOp::ForEach { .. } => "For each row".into(),
        PlanDryOp::IterateUntil { .. } => "Iterate until".into(),
        PlanDryOp::Relation { .. } => "Follow relation".into(),
        PlanDryOp::Data { .. } => "Static data".into(),
        PlanDryOp::Derive { .. } => "Derive rows".into(),
    }
}

/// Secondary line for plan UX — resolved wire names in predicate/field text.
pub(crate) fn human_ux_summary_for_op(op: &PlanDryOp) -> String {
    match op {
        PlanDryOp::MapBody { source, effects } => render_map_body(source, effects),
        PlanDryOp::Python {
            entity,
            per_row,
            input,
        } => format!(
            "Python {} of {entity}{}",
            if *per_row {
                "per-row rendering"
            } else {
                "reduction"
            },
            input
                .as_ref()
                .map(|v| format!(" · input {v}"))
                .unwrap_or_default()
        ),
        PlanDryOp::Filter { predicates } if !predicates.is_empty() => {
            format!("Where {}", predicates.join(", "))
        }
        PlanDryOp::Filter { .. } => "Filter rows".into(),
        PlanDryOp::Project { fields } => format!("Fields: {}", fields.join(", ")),
        PlanDryOp::Surface {
            kind,
            expr,
            selections,
            description,
        } => match kind {
            PlanNodeKind::Search => {
                format!("Search · {expr}{}", render_selection_effects(selections))
            }
            PlanNodeKind::Get => format!("Get · {expr}"),
            PlanNodeKind::Query => format!("Read · {expr}{}", render_selection_effects(selections)),
            PlanNodeKind::Create => {
                with_effect_description(format!("Create · {expr}"), description)
            }
            PlanNodeKind::Update => {
                with_effect_description(format!("Update · {expr}"), description)
            }
            PlanNodeKind::Delete => {
                with_effect_description(format!("Delete · {expr}"), description)
            }
            PlanNodeKind::Action => with_effect_description(format!("Write · {expr}"), description),
            _ => format!("{} · {expr}", render_kind(*kind)),
        },
        PlanDryOp::Sort { key, descending } => {
            if *descending {
                format!("Sort by {key} (descending)")
            } else {
                format!("Sort by {key}")
            }
        }
        PlanDryOp::Limit { count } => format!("Take first {count}"),
        PlanDryOp::GroupBy { keys, .. } => format!("Group by {}", keys.join(", ")),
        PlanDryOp::Aggregate { .. } => "Summarize".into(),
        PlanDryOp::Dedupe { keys } if keys.is_empty() => "Distinct rows".into(),
        PlanDryOp::Dedupe { keys } => format!("Dedupe on {}", keys.join(", ")),
        PlanDryOp::With { columns } => format!("Add {}", columns.join(", ")),
        PlanDryOp::Render { columns, .. } => format!("Render {}", columns.join(", ")),
        PlanDryOp::MergeBranches { other } => format!("Merge exclusive branches {other}"),
        PlanDryOp::Union { other } => format!("Union {other}"),
        PlanDryOp::Relation {
            relation, target, ..
        } => format!("Via {relation} → {target}"),
        PlanDryOp::ForEach {
            source, binding, ..
        } => format!("For each row in {source} as {binding}"),
        PlanDryOp::IterateUntil {
            source,
            binding,
            take,
            ..
        } => format!("Iterate {source} as {binding} until (take {take})"),
        PlanDryOp::Derive {
            source, binding, ..
        } => format!("Derive from {source} as {binding}"),
        PlanDryOp::Data { summary } => format!("Data · {summary}"),
    }
}

pub(crate) fn render_plan_dry_op(op: &PlanDryOp) -> String {
    match op {
        PlanDryOp::MapBody { source, effects } => render_map_body(source, effects),
        PlanDryOp::Python {
            entity,
            per_row,
            input,
        } => format!(
            "{} {entity} -> str{}",
            if *per_row {
                "python_map"
            } else {
                "python_reduce"
            },
            input
                .as_ref()
                .map(|v| format!(" · input {v}"))
                .unwrap_or_default()
        ),
        PlanDryOp::Surface {
            kind,
            expr,
            selections,
            description,
        } => with_effect_description(
            format!(
                "{} {expr}{}",
                render_kind(*kind),
                render_selection_effects(selections)
            ),
            description,
        ),
        PlanDryOp::Project { fields } => format!("project {}", fields.join(", ")),
        PlanDryOp::Filter { predicates } => format!("filter {}", predicates.join(", ")),
        PlanDryOp::GroupBy { keys, aggregates } => {
            format!("group_by {} → {{{aggregates}}}", keys.join(", "))
        }
        PlanDryOp::Aggregate { aggregates } => format!("aggregate → {{{aggregates}}}"),
        PlanDryOp::Sort { key, descending } => {
            format!("sort {key} {}", if *descending { "desc" } else { "asc" })
        }
        PlanDryOp::Limit { count } => format!("limit {count}"),
        PlanDryOp::Dedupe { keys } => {
            if keys.is_empty() {
                "distinct *".to_string()
            } else {
                format!("dedupe {}", keys.join(", "))
            }
        }
        PlanDryOp::With { columns } => format!("with {}", columns.join(", ")),
        PlanDryOp::Render {
            columns,
            template_chars,
        } => format!("render [{}] ({} chars)", columns.join(", "), template_chars),
        PlanDryOp::MergeBranches { other } => format!("Merge exclusive branches {other}"),
        PlanDryOp::Union { other } => format!("union {other}"),
        PlanDryOp::ForEach {
            source,
            binding,
            body,
        } => {
            format!("for_each {source} as {binding} => {body}")
        }
        PlanDryOp::IterateUntil {
            source,
            binding,
            body,
            take,
        } => {
            format!("iterate_until {source} as {binding} => {body} take {take}")
        }
        PlanDryOp::Relation {
            relation,
            target,
            expr,
        } => format!("relation {relation} → {target} {expr}"),
        PlanDryOp::Data { summary } => format!("data {summary}"),
        PlanDryOp::Derive {
            source,
            binding,
            summary,
        } => format!("derive map {source} as {binding} → {summary}"),
    }
}

fn compact_python(input: Option<&str>) -> String {
    match input {
        Some(input) => format!("compute value({input})"),
        None => "compute value".to_owned(),
    }
}

fn compact_op_from_node(
    node: &ValidatedPlanNode,
    es: Option<&ExecuteSession>,
    display_map: &HashMap<String, String>,
) -> PlanDryOp {
    match node {
        ValidatedPlanNode::MapBody(map) => PlanDryOp::MapBody {
            source: map_display_id(map.body.parent.source.as_str(), display_map),
            effects: map_body_effects(&map.plan, es),
        },
        ValidatedPlanNode::Capture(n) => PlanDryOp::Data {
            summary: format!("{}.{}", n.entity.entry_id, n.entity.entity),
        },
        ValidatedPlanNode::Surface(s) => PlanDryOp::Surface {
            kind: s.kind,
            expr: surface_compact_expr(s, es, display_map),
            selections: surface_selection_effects(s, es),
            description: if matches!(s.effect_class, EffectClass::Write | EffectClass::SideEffect) {
                effect_description(crate::plan_flow_capability::surface_capability_key(s), es)
            } else {
                String::new()
            },
        },
        ValidatedPlanNode::Data(n) => PlanDryOp::Data {
            summary: data_value_summary(&n.data),
        },
        ValidatedPlanNode::Derive(n) => PlanDryOp::Derive {
            source: map_display_id(n.source.as_str(), display_map),
            binding: n.item_binding.as_str().to_string(),
            summary: match &n.value {
                PlanValue::BindingSymbol { binding, path }
                    if binding == n.item_binding.as_str() =>
                {
                    std::iter::once(map_display_id(n.source.as_str(), display_map))
                        .chain(path.iter().cloned())
                        .collect::<Vec<_>>()
                        .join(".")
                }
                _ => plan_value_summary(&n.value),
            },
        },
        ValidatedPlanNode::Compute(n) => compact_op_from_compute(&n.compute, display_map),
        ValidatedPlanNode::RelationTraversal(n) => PlanDryOp::Relation {
            relation: format!(
                "{}.{}",
                map_display_id(n.relation.source.as_str(), display_map),
                n.relation.relation.as_str()
            ),
            target: format!(
                "{}.{}",
                n.relation.target.entry_id, n.relation.target.entity
            ),
            expr: render_plan_expr_ir_for_session(&n.relation.ir, es),
        },
        ValidatedPlanNode::ForEach(n) => PlanDryOp::ForEach {
            source: map_display_id(n.source.as_str(), display_map),
            binding: n.item_binding.as_str().to_string(),
            body: described_effect_template_body(&n.effect_template, es, display_map),
        },
        ValidatedPlanNode::IterateUntil(n) => PlanDryOp::IterateUntil {
            source: map_display_id(n.source.as_str(), display_map),
            binding: n.item_binding.as_str().to_string(),
            body: described_effect_template_body(&n.effect_template, es, display_map),
            take: n.take,
        },
    }
}

fn compact_op_from_compute(
    compute: &ComputeTemplate,
    display_map: &HashMap<String, String>,
) -> PlanDryOp {
    let _ = display_map;
    match &compute.op {
        ComputeOp::NarrowEntity { target, .. } => PlanDryOp::Filter {
            predicates: vec![format!("entity type {}:{}", target.entry_id, target.entity)],
        },
        ComputeOp::Python {
            entity,
            per_row,
            input_schema,
            ..
        } => PlanDryOp::Python {
            input: None,
            entity: if input_schema.is_some() {
                "Row".into()
            } else {
                entity.clone().unwrap_or_else(|| "Value".into())
            },
            per_row: *per_row,
        },
        ComputeOp::Project { fields } => PlanDryOp::Project {
            fields: fields.keys().map(|k| k.as_str().to_string()).collect(),
        },
        ComputeOp::Filter { predicates } => PlanDryOp::Filter {
            predicates: vec![predicates.render(&render_predicate_compact)],
        },
        ComputeOp::GroupBy { keys, aggregates } => PlanDryOp::GroupBy {
            keys: keys.iter().map(|k| k.dotted()).collect(),
            aggregates: render_aggregates_compact(aggregates),
        },
        ComputeOp::Aggregate { aggregates } => PlanDryOp::Aggregate {
            aggregates: render_aggregates_compact(aggregates),
        },
        ComputeOp::Sort { key, descending } => PlanDryOp::Sort {
            key: key.dotted(),
            descending: *descending,
        },
        ComputeOp::Limit { count } => PlanDryOp::Limit { count: *count },
        ComputeOp::DedupeBy { keys } => PlanDryOp::Dedupe {
            keys: keys.iter().map(|k| k.dotted()).collect(),
        },
        ComputeOp::With { columns } => PlanDryOp::With {
            columns: columns
                .iter()
                .map(|c| c.name.as_str().to_string())
                .collect(),
        },
        ComputeOp::Render {
            columns, template, ..
        } => PlanDryOp::Render {
            columns: columns.iter().map(|c| c.as_str().to_string()).collect(),
            template_chars: template.chars().count(),
        },
        ComputeOp::MergeBranches { other } => PlanDryOp::MergeBranches {
            other: other.to_string(),
        },
        ComputeOp::Union { other } => PlanDryOp::Union {
            other: other.as_str().to_string(),
        },
    }
}

fn surface_compact_expr(
    surface: &ValidatedSurfaceNode,
    es: Option<&ExecuteSession>,
    names: &HashMap<String, String>,
) -> String {
    if es.is_none() {
        if let Some(ir) = &surface.ir {
            let mut summary = crate::expr_display::expr_display(&ir.expr);
            if let Some(fields) = ir.projection.as_deref() {
                summary.push_str(&format!(" fields {}", fields.join(", ")));
            }
            return summary;
        }
    }
    let raw = surface
        .ir
        .as_ref()
        .map(|ir| render_executable_expr_named(&ir.expr, ir.projection.as_deref(), es, names))
        .or_else(|| {
            surface.ir_template.as_ref().map(|tmpl| {
                render_executable_expr_named(&tmpl.expr, tmpl.projection.as_deref(), es, names)
            })
        })
        .unwrap_or_else(|| "<typed Plasm IR>".to_string());
    crate::plan_dry_compact::compact_agent_surface_expr(&raw)
}

fn surface_selection_effects(
    surface: &ValidatedSurfaceNode,
    es: Option<&ExecuteSession>,
) -> Vec<PlanDrySelection> {
    if !matches!(surface.kind, PlanNodeKind::Query | PlanNodeKind::Search) {
        return Vec::new();
    }
    let Some(es) = es else { return Vec::new() };
    let expr = surface
        .ir
        .as_ref()
        .map(|ir| &ir.expr)
        .or_else(|| surface.ir_template.as_ref().map(|ir| &ir.expr));
    let Some(plasm_core::Expr::Query(query)) = expr else {
        return Vec::new();
    };
    let entry = query
        .catalog_entry_id
        .as_ref()
        .map(|entry| entry.as_str())
        .or_else(|| {
            surface
                .qualified_entity
                .as_ref()
                .map(|key| key.entry_id.as_str())
        })
        .unwrap_or(es.entry_id.as_str());
    let Some(cgs) = es
        .contexts_by_entry
        .get(entry)
        .map(|context| context.cgs.as_ref())
    else {
        tracing::warn!(
            entry,
            "plan selection catalog is absent from execute session"
        );
        return Vec::new();
    };
    let Ok(cap) = plasm_core::resolve_query_capability(query, cgs) else {
        tracing::warn!(entry, "plan selection capability could not be resolved");
        return Vec::new();
    };
    let used = query
        .predicate
        .as_ref()
        .map(|predicate| predicate.referenced_fields())
        .unwrap_or_default();
    cap.selection_params()
        .iter()
        .filter(|field| used.iter().any(|name| name == &field.name))
        .filter_map(|field| {
            field.selection_effect.map(|effect| PlanDrySelection {
                name: field.name.clone(),
                effect,
            })
        })
        .collect()
}

fn render_plan_expr_ir_for_session(
    ir: &ValidatedPlanExprIr,
    es: Option<&ExecuteSession>,
) -> String {
    render_executable_expr(&ir.expr, ir.projection.as_deref(), es)
}

/// Canonical wire-surface renderer for typed [`Expr`] in an execute session (dry plan, artifacts).
/// Compact IL summaries ([`crate::expr_display::expr_display_resolved`]) are a separate hint surface.
fn render_expr_wire_named(
    expr: &plasm_core::Expr,
    es: Option<&ExecuteSession>,
    names: &HashMap<String, String>,
) -> String {
    match es {
        None => serde_json::to_string(expr).expect("typed expression serializes to JSON"),
        Some(es) => {
            if es.contexts_by_entry.len() > 1 {
                if let Some(exposure) = es.teaching_exposure.as_ref() {
                    let fed = plasm_core::FederationDispatch::from_contexts_and_exposure(
                        es.contexts_by_entry.clone(),
                        exposure,
                    );
                    return plasm_core::render_expr_surface_named(
                        expr,
                        es.cgs.as_ref(),
                        Some(&fed),
                        names,
                    );
                }
            }
            plasm_core::render_expr_surface_named(expr, es.cgs.as_ref(), None, names)
        }
    }
}

/// Review receives only executable structure, never authored display text.
pub(crate) fn render_executable_expr(
    expr: &plasm_core::Expr,
    projection: Option<&[String]>,
    es: Option<&ExecuteSession>,
) -> String {
    render_executable_expr_named(expr, projection, es, &HashMap::new())
}

fn render_executable_expr_named(
    expr: &plasm_core::Expr,
    projection: Option<&[String]>,
    es: Option<&ExecuteSession>,
    names: &HashMap<String, String>,
) -> String {
    let mut rendered = render_expr_wire_named(expr, es, names);
    if let Some(fields) = projection {
        rendered.push('[');
        rendered.push_str(&fields.join(","));
        rendered.push(']');
    }
    rendered
}

fn render_plan_expr_template_for_session(
    template: &ValidatedPlanExprTemplate,
    es: Option<&ExecuteSession>,
) -> String {
    render_executable_expr(&template.expr, template.projection.as_deref(), es)
}

#[allow(dead_code)]
fn render_plan_expr_template(template: &ValidatedPlanExprTemplate) -> String {
    render_plan_expr_template_for_session(template, None)
}

fn effect_template_body(
    template: &ValidatedEffectTemplate,
    es: Option<&ExecuteSession>,
    names: &HashMap<String, String>,
) -> String {
    render_executable_expr_named(
        &template.ir_template.expr,
        template.ir_template.projection.as_deref(),
        es,
        names,
    )
}

fn described_effect_template_body(
    template: &ValidatedEffectTemplate,
    es: Option<&ExecuteSession>,
    names: &HashMap<String, String>,
) -> String {
    let target = &template.qualified_entity;
    let key = crate::plan_flow::QualifiedCapabilityKey::from_parts(
        target.entry_id.as_str(),
        target.entity.as_str(),
        &crate::plan_flow_capability::resolved_mutation_capability_name(
            Some(&template.ir_template.expr),
            template.kind,
        ),
    );
    with_effect_description(
        effect_template_body(template, es, names),
        &effect_description(Some(key), es),
    )
}

fn step_upstream_labels(
    node: &ValidatedPlanNode,
    display_map: &HashMap<String, String>,
) -> Vec<String> {
    let mut ids: Vec<String> = node
        .uses_result()
        .iter()
        .map(|u| map_display_id(&u.node, display_map))
        .collect();
    if ids.is_empty() {
        match node {
            ValidatedPlanNode::Compute(n) => {
                ids.push(map_display_id(&n.compute.source, display_map));
            }
            ValidatedPlanNode::Derive(n) => {
                ids.push(map_display_id(n.source.as_str(), display_map));
            }
            ValidatedPlanNode::ForEach(n) => {
                ids.push(map_display_id(n.source.as_str(), display_map));
            }
            ValidatedPlanNode::IterateUntil(n) => {
                ids.push(map_display_id(n.source.as_str(), display_map));
            }
            ValidatedPlanNode::RelationTraversal(n) => {
                ids.push(map_display_id(n.relation.source.as_str(), display_map));
            }
            _ => {}
        }
    }
    ids
}

fn map_display_id(id: &str, display_map: &HashMap<String, String>) -> String {
    display_map
        .get(id)
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

fn primary_return_label(
    plan: &Plan<ValidatedPlanState>,
    display_map: &HashMap<String, String>,
) -> String {
    match &plan.return_value {
        ValidatedPlanReturn::Node(id) => map_display_id(id.as_str(), display_map),
        ValidatedPlanReturn::Parallel { parallel } => {
            if parallel.len() == 1 {
                map_display_id(parallel[0].as_str(), display_map)
            } else {
                let names: Vec<String> = parallel
                    .iter()
                    .map(|id| map_display_id(id.as_str(), display_map))
                    .collect();
                format!("returns: {}", names.join(", "))
            }
        }
    }
}

fn render_predicate_compact(predicate: &PlanPredicate) -> String {
    format!(
        "{}{}{}",
        predicate.field_path.dotted(),
        render_predicate_op(predicate.op),
        render_plan_value_compact(&predicate.value)
    )
}

fn render_aggregates_compact(aggregates: &[AggregateSpec]) -> String {
    aggregates
        .iter()
        .map(|agg| {
            let field = agg
                .field
                .as_ref()
                .map(FieldPath::dotted)
                .unwrap_or_else(|| "*".to_string());
            format!(
                "{}={}({field})",
                agg.name.as_str(),
                render_aggregate_function(agg.function)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_predicate_op(op: PlanPredicateOp) -> &'static str {
    match op {
        PlanPredicateOp::Eq => "=",
        PlanPredicateOp::Ne => "!=",
        PlanPredicateOp::Lt => "<",
        PlanPredicateOp::Lte => "<=",
        PlanPredicateOp::Gt => ">",
        PlanPredicateOp::Gte => ">=",
        PlanPredicateOp::Contains => "~",
        PlanPredicateOp::In => " in ",
        PlanPredicateOp::NotIn => " not in ",
        PlanPredicateOp::Exists => " exists ",
    }
}

fn render_aggregate_function(function: AggregateFunction) -> &'static str {
    match function {
        AggregateFunction::Count => "count",
        AggregateFunction::Sum => "sum",
        AggregateFunction::Avg => "avg",
        AggregateFunction::Min => "min",
        AggregateFunction::Max => "max",
        AggregateFunction::First => "first",
        AggregateFunction::Last => "last",
    }
}

fn render_plan_value_compact(value: &PlanValue) -> String {
    match value {
        PlanValue::Quantified {
            all,
            collection,
            binding,
            predicate,
        } => format!(
            "{}({} for {} in {})",
            if *all { "all" } else { "any" },
            render_plan_value_compact(predicate),
            binding,
            render_plan_value_compact(collection)
        ),
        PlanValue::Expression { expression } => expression.render(|v| render_plan_value_compact(v)),

        PlanValue::Literal { value } => match value.to_wire() {
            Ok(wire) => render_json_value(&wire),
            Err(error) => format!("<invalid resolved value: {error}>"),
        },
        PlanValue::Object { fields } => format!("{{{}}}", fields.len()),
        PlanValue::Array { items } => format!("[{}]", items.len()),
        PlanValue::Template { .. } => "template".to_owned(),
        PlanValue::NodeSymbol { alias, path, .. } => {
            if path.is_empty() {
                alias.clone()
            } else {
                format!("{alias}.{}", path.join("."))
            }
        }
        PlanValue::BindingSymbol { binding, path } => {
            if path.is_empty() {
                binding.clone()
            } else {
                format!("{binding}.{}", path.join("."))
            }
        }
        PlanValue::Symbol { path } => path.clone(),
        PlanValue::EntityRefKey { key, .. } => render_plan_value_compact(key),
    }
}

fn data_value_summary(value: &PlanValue) -> String {
    match value {
        PlanValue::Object { fields } => format!("{{{}}}", fields.len()),
        _ => plan_value_summary(value),
    }
}

fn plan_value_summary(value: &PlanValue) -> String {
    match value {
        PlanValue::Object { fields } => format!("{{{}}}", fields.len()),
        PlanValue::Array { items } => format!("[{}]", items.len()),
        PlanValue::Literal { value } => match value.to_wire() {
            Ok(wire) => render_json_value(&wire),
            Err(error) => format!("<invalid resolved value: {error}>"),
        },
        PlanValue::Template { .. } => "template".to_string(),
        _ => render_plan_value_compact(value),
    }
}

fn render_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => format!("\"{s}\""),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(items) => format!("[{}]", items.len()),
        serde_json::Value::Object(map) => format!("{{{}}}", map.len()),
    }
}

fn render_kind(kind: PlanNodeKind) -> &'static str {
    match kind {
        PlanNodeKind::Query => "query",
        PlanNodeKind::Search => "search",
        PlanNodeKind::Get => "get",
        PlanNodeKind::Create => "create",
        PlanNodeKind::Update => "update",
        PlanNodeKind::Delete => "delete",
        PlanNodeKind::Action => "action",
        PlanNodeKind::Data => "data",
        PlanNodeKind::Derive => "derive",
        PlanNodeKind::Compute => "compute",
        PlanNodeKind::ForEach => "for_each",
        PlanNodeKind::IterateUntil => "iterate_until",
        PlanNodeKind::Relation => "relation",
        PlanNodeKind::MapBody => "map_body",
        PlanNodeKind::Capture => "capture",
    }
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn surface_read_list_root_unbounded(s: &ValidatedSurfaceNode) -> bool {
    // Default host page caps fetch cost but is not an agent-declared bound — still advisory.
    matches!(s.result_shape, crate::plasm_plan::ResultShape::List)
        && s.effect_class == EffectClass::Read
        && s.depends_on.is_empty()
        && s.page_size.is_none()
        && s.pushed_read_budget.is_none()
        && s.kind != PlanNodeKind::Search
        && s.predicates.is_empty()
}

pub(crate) fn return_roots_include_unbounded_list_surface(plan: &Plan<ValidatedPlanState>) -> bool {
    for id in plan.return_value.refs() {
        let Some(node) = plan.nodes.iter().find(|n| n.id() == id) else {
            continue;
        };
        if let ValidatedPlanNode::Surface(s) = node {
            if surface_read_list_root_unbounded(s) {
                return true;
            }
        }
    }
    false
}

fn is_synthetic_plan_node_id(id: &str) -> bool {
    id.starts_with("__plasm_")
        || id.starts_with("__py")
        || id.starts_with("__scope")
        || id
            .strip_prefix("return_")
            .and_then(|rest| rest.parse::<u32>().ok())
            .is_some()
}

pub(crate) fn is_synthetic_plan_node_id_public(id: &str) -> bool {
    is_synthetic_plan_node_id(id)
}

#[derive(Default)]
struct SyntheticPlanLabelCounters {
    r: usize,
    w: usize,
    c: usize,
    d: usize,
    f: usize,
    l: usize,
    x: usize,
}

fn next_synthetic_plan_label(
    node: &ValidatedPlanNode,
    counters: &mut SyntheticPlanLabelCounters,
) -> String {
    match node {
        ValidatedPlanNode::MapBody(_) => {
            counters.f += 1;
            format!("map_body_{}", counters.f)
        }
        ValidatedPlanNode::Capture(_) => {
            counters.x += 1;
            format!("capture_{}", counters.x)
        }
        ValidatedPlanNode::Surface(surface) => match surface.effect_class {
            EffectClass::Read => {
                counters.r += 1;
                format!("read_{}", counters.r)
            }
            EffectClass::Write | EffectClass::SideEffect => {
                counters.w += 1;
                format!("write_{}", counters.w)
            }
            EffectClass::ArtifactRead => {
                counters.x += 1;
                format!("artifact_{}", counters.x)
            }
        },
        ValidatedPlanNode::Compute(_) => {
            counters.c += 1;
            format!("compute_{}", counters.c)
        }
        ValidatedPlanNode::Derive(_) => {
            counters.d += 1;
            format!("derive_{}", counters.d)
        }
        ValidatedPlanNode::ForEach(_) | ValidatedPlanNode::IterateUntil(_) => {
            counters.f += 1;
            format!("apply_{}", counters.f)
        }
        ValidatedPlanNode::RelationTraversal(_) => {
            counters.l += 1;
            format!("relation_{}", counters.l)
        }
        ValidatedPlanNode::Data(_) => {
            counters.x += 1;
            format!("value_{}", counters.x)
        }
    }
}

/// Node id → compact display label (`read_1`, `compute_1`, …) for async operation progress lines.
pub(crate) fn plan_node_display_map(
    plan: &Plan<ValidatedPlanState>,
    topological_order: &[String],
) -> HashMap<String, String> {
    build_plan_node_display_map(&plan.nodes, topological_order)
}

fn build_plan_node_display_map(
    nodes: &[ValidatedPlanNode],
    topological_order: &[String],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut counters = SyntheticPlanLabelCounters::default();
    // Reserve every authored name up front, including names later in the plan.
    let mut occupied: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|node| !is_synthetic_plan_node_id(node.id().as_str()))
        .map(|node| node.id().as_str().to_owned())
        .collect();
    for id in topological_order {
        let Some(node) = nodes.iter().find(|n| n.id().as_str() == id) else {
            continue;
        };
        let label = if is_synthetic_plan_node_id(id.as_str()) {
            loop {
                let candidate = next_synthetic_plan_label(node, &mut counters);
                if occupied.insert(candidate.clone()) {
                    break candidate;
                }
            }
        } else {
            id.clone()
        };
        map.insert(id.clone(), label);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::{OutputName, SyntheticResultSchema};
    use plasm_core::{CgsContext, TeachingExposureSession};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[test]
    fn resource_lifecycle_teaching_retains_source_registration_and_unknown_readiness() {
        let dir = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/resource_registration");
        for definition in [
            "Routing definition",
            "Calibration profile",
            "Rendering preset",
        ] {
            let mut cgs = plasm_core::load_schema_dir(&dir).unwrap();
            cgs.values.get_mut("definition").unwrap().description = definition.into();
            let encoded = serde_json::to_vec(&cgs).unwrap();
            let cgs: plasm_core::CGS = serde_json::from_slice(&encoded).unwrap();
            let exposure = TeachingExposureSession::new(
                &cgs,
                "resource_registration",
                &["ResourceSource", "RegisteredResource"],
            );
            let text = plasm_core::prompt_render::python::prepare_python_teaching_wave(
                &exposure,
                &plasm_core::prompt_render::python::PythonTeachingState::default(),
            )
            .unwrap()
            .declarations;
            for fragment in [
                definition,
                "presence here does not establish destination registration",
                "registration alone does not establish current readiness",
                "Does not establish readiness or permission",
                "Result fields available immediately: handle",
            ] {
                assert!(text.contains(fragment), "missing {fragment}: {text}");
            }
            assert_eq!(
                cgs.capabilities["register_resource"].provides,
                vec!["handle"]
            );
            assert!(!cgs.entities["RegisteredResource"].fields["ready"].required);
        }
    }

    #[test]
    fn effect_review_is_lossless_compact_and_does_not_invent_annotations() {
        let description = "Registers a definition; leaves existing resources unchanged. Readiness remains unknown.";
        for kind in [
            PlanNodeKind::Create,
            PlanNodeKind::Update,
            PlanNodeKind::Delete,
            PlanNodeKind::Action,
        ] {
            for description in [description, ""] {
                let op = PlanDryOp::Surface {
                    kind,
                    expr: "Resource.change(value=sample)".into(),
                    selections: vec![],
                    description: description.into(),
                };
                for text in [render_plan_dry_op(&op), human_ux_summary_for_op(&op)] {
                    assert!(text.contains("Resource.change(value=sample)"));
                    assert_eq!(text.contains(" — "), !description.is_empty());
                    if !description.is_empty() {
                        assert!(text.ends_with(description), "{text}");
                    }
                    assert_eq!(text.lines().count(), 1);
                }
            }
        }
    }

    #[test]
    fn plan_uses_the_catalogs_typed_selection_meaning() {
        let op = PlanDryOp::Surface {
            kind: PlanNodeKind::Query,
            expr: "e1.query(query=..., status=...)".into(),
            description: String::new(),
            selections: vec![
                PlanDrySelection {
                    name: "query".into(),
                    effect: SelectionEffect::Rank,
                },
                PlanDrySelection {
                    name: "status".into(),
                    effect: SelectionEffect::Filter,
                },
            ],
        };
        let rendered = render_plan_dry_op(&op);
        assert!(rendered.contains("query ranks candidates; may retain nonmatches"));
        assert!(rendered.contains("status filters rows"));
        assert!(!rendered.contains("query filters rows"));
    }

    #[test]
    fn review_composition_keeps_symbolic_arguments_in_top_and_nested_templates() {
        let dir = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_language_matrix");
        let mut schema = plasm_core::load_schema_dir(&dir).unwrap();
        schema.bind_registry_entry_id("matrix");
        let cgs = Arc::new(schema);
        let contexts = indexmap::IndexMap::from([(
            "matrix".into(),
            Arc::new(CgsContext::entry("matrix", cgs.clone())),
        )]);
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            contexts,
            "matrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(TeachingExposureSession::new(&cgs, "matrix", &["LangItem"])),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
        );
        let names = HashMap::from([("__py1".into(), "read_2".into())]);
        for reference in [
            plasm_core::PlasmInputRef::node_output("__py1", vec!["title".into()]),
            plasm_core::PlasmInputRef::node_output("acquired", vec!["title".into()]),
            plasm_core::PlasmInputRef::node_output("verified", vec!["title".into()]),
            plasm_core::PlasmInputRef::row_binding("_", vec!["title".into()]),
        ] {
            let mut expr = plasm_core::expr_parser::parse("LangItem.create(title=seed)", &cgs)
                .unwrap()
                .expr;
            let plasm_core::Expr::Create(create) = &mut expr else {
                panic!("create fixture")
            };
            create.input = plasm_core::Value::Object(indexmap::IndexMap::from([(
                "title".into(),
                plasm_core::Value::PlasmInputRef(reference.clone()),
            )]))
            .into();
            let expected = match reference {
                plasm_core::PlasmInputRef::NodeInput { node, .. } => {
                    format!("title={}.title", names.get(&node).unwrap_or(&node))
                }
                plasm_core::PlasmInputRef::RowBinding { .. } => "title=_.title".into(),
            };
            let template = ValidatedPlanExprTemplate {
                expr,
                projection: None,
                input_bindings: vec![],
            };
            let top = render_executable_expr_named(&template.expr, None, Some(&es), &names);
            let nested = render_map_body(
                "verified_rows",
                &[PlanDryEffect {
                    binding: "created".into(),
                    uses: vec!["verification".into()],
                    kind: PlanNodeKind::Create,
                    expression: render_executable_expr_named(
                        &template.expr,
                        None,
                        Some(&es),
                        &names,
                    ),
                    description: "Creates a new item; other items remain unchanged.".into(),
                }],
            );
            assert!(top.contains(&expected), "{top}");
            assert!(nested.contains(&expected), "{nested}");
            assert!(nested.contains("verified_rows"));
            assert!(nested.contains("← verification"));
            assert!(nested.contains("other items remain unchanged"));
            assert!(!top.contains("title=$"));
        }
    }

    #[test]
    fn plan_resolves_used_selection_effects_from_catalog() {
        let dir = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let mut schema = plasm_core::load_schema_dir(&dir).unwrap();
        schema.bind_registry_entry_id("matrix");
        let cgs = Arc::new(schema);
        let contexts = indexmap::IndexMap::from([(
            "matrix".into(),
            Arc::new(CgsContext::entry("matrix", cgs.clone())),
        )]);
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            contexts,
            "matrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["Zone".into()],
            Some(TeachingExposureSession::new(&cgs, "matrix", &["Zone"])),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
        );
        let mut query =
            plasm_core::QueryExpr::filtered("Zone", plasm_core::Predicate::eq("name", "news"));
        query.capability_name = Some("zone_query".into());
        let surface = ValidatedSurfaceNode {
            id: crate::plasm_plan::PlanNodeId::new("read").unwrap(),
            kind: PlanNodeKind::Query,
            qualified_entity: None,
            ir: Some(ValidatedPlanExprIr {
                expr: plasm_core::Expr::Query(query),
                projection: None,
            }),
            ir_template: None,
            effect_class: EffectClass::Read,
            result_shape: crate::plasm_plan::ResultShape::List,
            projection: Vec::new(),
            predicates: Vec::new(),
            depends_on: Vec::new(),
            uses_result: Vec::new(),
            approval: None,
            page_size: None,
            pushed_read_budget: None,
        };
        assert_eq!(
            surface_selection_effects(&surface, Some(&es)),
            vec![PlanDrySelection {
                name: "name".into(),
                effect: SelectionEffect::Filter
            }]
        );
    }

    #[test]
    fn project_op_uses_field_names_only() {
        let mut fields = BTreeMap::new();
        fields.insert(
            OutputName::new("identifier").expect("name"),
            FieldPath::new(vec!["identifier".to_string()]).expect("path"),
        );
        fields.insert(
            OutputName::new("title").expect("name"),
            FieldPath::new(vec!["title".to_string()]).expect("path"),
        );
        let op = compact_op_from_compute(
            &ComputeTemplate {
                source: "open_auth".to_string(),
                op: ComputeOp::Project { fields },
                schema: SyntheticResultSchema {
                    optional_fields: Default::default(),
                    entity: None,
                    fields: Vec::new(),
                },
                page_size: None,
                collection_alias: None,
            },
            &HashMap::new(),
        );
        assert_eq!(
            op,
            PlanDryOp::Project {
                fields: vec!["identifier".to_string(), "title".to_string()],
            }
        );
        assert_eq!(render_plan_dry_op(&op), "project identifier, title");
    }

    #[test]
    fn plan_expr_wire_surface_is_not_il_summary() {
        let dir = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/petstore_minimal");
        if !dir.exists() {
            return;
        }
        let cgs = plasm_core::load_schema_dir(&dir).expect("petstore_minimal");
        let pe = plasm_core::expr_parser::parse("Pet(1)", &cgs).expect("parse");
        let wire = plasm_core::render_expr_surface(&pe.expr, &cgs);
        assert_eq!(wire, "Pet(1)");
        assert!(
            !wire.starts_with("Get("),
            "wire surface must not be compact IL: {wire}"
        );
        let ir = ValidatedPlanExprIr {
            expr: pe.expr,
            projection: pe.projection,
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&render_plan_expr_ir_for_session(&ir, None))
                .expect("structural review"),
            serde_json::to_value(&ir.expr).expect("expression")
        );
    }
}
