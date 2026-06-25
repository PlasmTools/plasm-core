//! Plasm **program** compiler: multi-line bindings, postfix transforms (`.limit`, `.sort`, …),
//! and final roots, lowered to the serialized program [`Plan`](crate::plasm_plan::Plan) IR consumed by [`crate::plasm_plan_run`].
//!
//! Surface path expressions ([`plasm_core::expr_parser`]) remain the leaf language; this module
//! stitches labels, postfix transforms, and `=>` derives into a single coherent program surface.

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    AggregateFunction, ComputeOp, EffectClass, FieldPath, OutputName, PlanExprIr, PlanNodeKind,
    PlanRelationTraversal, PlanValue, QualifiedEntityKey, RelationCardinality,
    RelationSourceCardinality, SyntheticFieldSchema, SyntheticResultSchema, SyntheticValueKind,
};
use crate::plasm_plan_run::{
    format_session_symbolic_parse_error, parse_plasm_surface_line_program,
    symbol_map_for_plasm_surface_parse,
};
use crate::program_binding::{
    BoundedSingletonKind, ContinuationAnchor, ContinuationCapability, ProgramBindingContract,
    RowCardinalityProof, SegmentPolicy,
};
use plasm_core::expr_parser::{
    collect_program_statement_lines, expand_flattened_program_statements,
    missing_program_roots_error, peel_postfix_suffixes, split_assignment_at_top_level,
    split_token_top_level, split_top_level, strip_line_comment, try_parse_render_tail,
    validate_program_label, PlasmPostfixOp, RenderTailParse,
};
use plasm_core::query_resolve;
use plasm_core::row_composition::RowSuffix;
use plasm_core::schema::{CapabilitySchema, EntityDef, InputType};
use plasm_core::CapabilityKind;
use plasm_core::ChainExpr;
use plasm_core::ChainStep;
use plasm_core::EntityKey;
use plasm_core::Expr;
use plasm_core::GetExpr;
use plasm_core::PlasmInputRef;
use plasm_core::Predicate;
use plasm_core::PromptPipelineConfig;
use plasm_core::Ref;
use plasm_core::SymbolMapCrossRequestCache;
use plasm_core::Value;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;
use std::sync::Arc;

/// Program RHS surface text for DAG lowering (opaque session symbols preserved).
///
/// Symbol resolution happens in the parser and per-token field helpers — not via a textual
/// pre-expansion pass. Construct only via [`Self::new`] at [`compile_node_expr`].
#[derive(Debug, Clone)]
pub struct ExpandedProgramSurface(String);

impl ExpandedProgramSurface {
    pub fn new(session: &ExecuteSession, pipeline: &PromptPipelineConfig, fragment: &str) -> Self {
        Self(
            crate::plasm_plan_run::expand_program_surface_for_session_lower(
                session, pipeline, fragment,
            ),
        )
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for ExpandedProgramSurface {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone)]
struct DagNode {
    id: String,
    expr: String,
    source: DagNodeSource,
    singleton: bool,
    page_size: Option<usize>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum DagNodeSource {
    Surface {
        parsed: plasm_core::expr_parser::ParsedExpr,
        kind: PlanNodeKind,
        qualified_entity: QualifiedEntityKey,
        effect_class: EffectClass,
        result_shape: crate::plasm_plan::ResultShape,
        uses_result: Vec<serde_json::Value>,
    },
    /// CGS relation traversal compiled from `bound_label.relation…` (substitutes bound anchor Plasm).
    RelationTraversal {
        source_label: String,
        /// Expanded Plasm used as the continuation anchor for nested `label.…` bindings.
        expanded_plasm: String,
        parsed: plasm_core::expr_parser::ParsedExpr,
        plan_relation: PlanRelationTraversal,
        qualified_entity: QualifiedEntityKey,
        effect_class: EffectClass,
        result_shape: crate::plasm_plan::ResultShape,
    },
    Data(PlanValue),
    Compute {
        source: String,
        op: ComputeOp,
        schema: SyntheticResultSchema,
    },
    Derive {
        source: String,
        value: PlanValue,
        inputs: Vec<serde_json::Value>,
    },
    ForEach {
        source: String,
        parsed_template: serde_json::Value,
        display_expr: String,
        effect_kind: PlanNodeKind,
        qualified_entity: QualifiedEntityKey,
        uses_result: Vec<serde_json::Value>,
    },
}

#[derive(Debug)]
struct CompileState<'a> {
    nodes: Vec<DagNode>,
    labels: BTreeMap<String, usize>,
    pipeline: &'a PromptPipelineConfig,
    cross_cache: Option<&'a SymbolMapCrossRequestCache>,
}

impl<'a> CompileState<'a> {
    fn new(
        pipeline: &'a PromptPipelineConfig,
        cross_cache: Option<&'a SymbolMapCrossRequestCache>,
    ) -> Self {
        Self {
            nodes: Vec::new(),
            labels: BTreeMap::new(),
            pipeline,
            cross_cache,
        }
    }

    fn insert(&mut self, node: DagNode) -> Result<(), String> {
        if self.labels.contains_key(&node.id) {
            return Err(format!("duplicate Plasm program node label {:?}", node.id));
        }
        self.labels.insert(node.id.clone(), self.nodes.len());
        self.nodes.push(node);
        Ok(())
    }

    fn get(&self, id: &str) -> Option<&DagNode> {
        self.labels.get(id).and_then(|i| self.nodes.get(*i))
    }

    fn contains(&self, id: &str) -> bool {
        self.labels.contains_key(id)
    }

    fn program_node_id_set(&self) -> BTreeSet<String> {
        self.labels.keys().cloned().collect()
    }
}

fn collect_template_uses_from_expr(expr: &Expr) -> Vec<serde_json::Value> {
    let ctx = plasm_core::TemplateRefContext::for_row_scope("_");
    let mut acc = Vec::new();
    collect_expr_for_template_uses(&mut acc, expr, &ctx);
    dedupe_uses(acc)
}

/// `uses_result` for relation plan nodes: per-row `source` plus any `node_input` aliases (e.g. `repo`).
fn relation_plan_uses_result(
    source_label: &str,
    parsed: &plasm_core::expr_parser::ParsedExpr,
) -> Vec<serde_json::Value> {
    let mut uses = vec![serde_json::json!({
        "node": source_label,
        "as": "source",
    })];
    for u in collect_template_uses_from_expr(&parsed.expr) {
        let node = u.get("node").and_then(|v| v.as_str()).unwrap_or("");
        let alias = u.get("as").and_then(|v| v.as_str()).unwrap_or(node);
        if node == source_label || (node == "source" && alias == "source") {
            continue;
        }
        uses.push(if node == "source" {
            serde_json::json!({ "node": source_label, "as": alias })
        } else {
            u
        });
    }
    dedupe_uses(uses)
}

/// Records upstream plan nodes so `node_input` holes become `uses_result` → `ir_template` + instantiation
/// before compile.
///
/// Surfaces covered: query predicates; **get**/**delete**/**invoke** `path_vars`; invoke/create payloads (values
/// recurse into objects/arrays). [`Expr::Get`] compound identity literals live on `reference`; program bindings
/// in compound slots are lowered to `path_vars` and collected here. [`PlasmInputRef::RowBinding`] is skipped on
/// purpose (`for_each` row scope).
fn collect_expr_for_template_uses(
    acc: &mut Vec<serde_json::Value>,
    expr: &Expr,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    match expr {
        Expr::Query(q) => {
            if let Some(pred) = &q.predicate {
                collect_predicate_for_template_uses(acc, pred, ctx);
            }
        }
        Expr::Get(g) => {
            if let Some(pv) = &g.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v, ctx);
                }
            }
        }
        Expr::Create(c) => {
            let v = c.input.to_value();
            collect_value_for_template_uses(acc, &v, ctx);
        }
        Expr::Delete(d) => {
            if let Some(pv) = &d.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v, ctx);
                }
            }
        }
        Expr::Invoke(i) => {
            if let Some(input) = &i.input {
                let v = input.to_value();
                collect_value_for_template_uses(acc, &v, ctx);
            }
            if let Some(pv) = &i.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v, ctx);
                }
            }
        }
        Expr::Chain(ch) => {
            collect_expr_for_template_uses(acc, &ch.source, ctx);
            if let ChainStep::Explicit { expr } = &ch.step {
                collect_expr_for_template_uses(acc, expr.as_ref(), ctx);
            }
        }
        Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) => {}
        Expr::TeachingValue { value } => {
            collect_value_for_template_uses(acc, value, ctx);
        }
    }
}

fn collect_predicate_for_template_uses(
    acc: &mut Vec<serde_json::Value>,
    pred: &Predicate,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    match pred {
        Predicate::Comparison { value, .. } => {
            let v = value.to_value();
            collect_value_for_template_uses(acc, &v, ctx);
        }
        Predicate::And { args } | Predicate::Or { args } => {
            for a in args {
                collect_predicate_for_template_uses(acc, a, ctx);
            }
        }
        Predicate::Not { predicate } => {
            collect_predicate_for_template_uses(acc, predicate.as_ref(), ctx)
        }
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                collect_predicate_for_template_uses(acc, inner.as_ref(), ctx);
            }
        }
        Predicate::True | Predicate::False => {}
    }
}

fn collect_value_for_template_uses(
    acc: &mut Vec<serde_json::Value>,
    v: &Value,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    match v {
        Value::PlasmInputRef(PlasmInputRef::NodeInput { node, .. }) => {
            acc.push(json!({
                "node": node,
                "as": node,
            }));
        }
        Value::PlasmInputRef(PlasmInputRef::RowBinding { .. }) => {}
        Value::Object(m) => {
            for x in m.values() {
                collect_value_for_template_uses(acc, x, ctx);
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_value_for_template_uses(acc, x, ctx);
            }
        }
        Value::String(s) => {
            for (node, alias) in ctx.plan_node_roots_from_string(s) {
                acc.push(json!({
                    "node": node,
                    "as": alias,
                }));
            }
        }
        _ => {}
    }
}

#[allow(dead_code)]
pub(crate) fn is_plasm_dag_candidate(expressions: &[String]) -> bool {
    if expressions.len() != 1 {
        return false;
    }
    is_plasm_dag_source(expressions[0].trim())
}

pub(crate) fn is_plasm_dag_source(src: &str) -> bool {
    src.lines().any(|line| {
        let line = strip_line_comment(line).trim();
        !line.is_empty() && split_assignment_at_top_level(line).is_some()
    }) || src.contains("=>")
        || peel_postfix_suffixes(src)
            .map(|(_, ops)| !ops.is_empty())
            .unwrap_or(false)
}

/// Compile one program expression to plan JSON (DAG program vs single surface line).
#[allow(dead_code)]
pub(crate) fn compile_plasm_expression_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    if is_plasm_dag_source(source.trim()) {
        compile_plasm_dag_to_plan(pipeline, symbol_map_cross_cache, session, name, source)
    } else {
        compile_plasm_surface_line_to_plan(pipeline, symbol_map_cross_cache, session, name, source)
    }
}

pub(crate) fn compile_plasm_dag_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    compile_plasm_dag_to_plan_inner(pipeline, symbol_map_cross_cache, session, name, source)
}

// compile_plasm_program / compile_plasm_expression live in plasm_compile.rs

pub(crate) fn compile_plasm_dag_to_plan_inner(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    let mut state = CompileState::new(pipeline, symbol_map_cross_cache);
    let flattened = expand_flattened_program_statements(&collect_program_statement_lines(source)?);
    let statements = flattened.statements;
    if statements.is_empty() {
        return Err("Plasm program is empty".to_string());
    }
    let mut final_roots: Option<Vec<String>> = None;
    for stmt in statements {
        if let Some((id, rhs)) = split_assignment_at_top_level(&stmt) {
            validate_program_label(id)?;
            for node in compile_node_expr(session, &state, id, rhs.trim())? {
                state.insert(node)?;
            }
        } else {
            let stmt = stmt.trim();
            if stmt.starts_with("return ") {
                return Err(
                    "return is not Plasm syntax; write bare comma-separated final roots (e.g. `a, b`, not `return a, b`)"
                        .to_string(),
                );
            }
            final_roots = Some(split_return_list(stmt, &mut state, session)?);
        }
    }
    let roots = final_roots.ok_or_else(missing_program_roots_error)?;
    if roots.is_empty() {
        return Err("Plasm program final roots list is empty".to_string());
    }
    let nodes = state
        .nodes
        .iter()
        .map(node_to_json)
        .collect::<Result<Vec<_>, _>>()?;
    let return_value = if roots.len() == 1 {
        json!({ "kind": "node", "node": roots[0] })
    } else {
        json!({ "kind": "parallel", "nodes": roots })
    };
    let mut metadata = serde_json::Map::new();
    metadata.insert("language".to_string(), serde_json::json!("plasm-dag"));
    if let Some(label) = flattened.coerced_default_return {
        metadata.insert(
            "coerced_default_return".to_string(),
            serde_json::json!(label),
        );
    }
    Ok(json!({
        "version": 1,
        "kind": "program",
        "name": name,
        "nodes": nodes,
        "return": return_value,
        "metadata": serde_json::Value::Object(metadata),
    }))
}

/// One line of surface Plasm (or `a, b` at top level) as a one-line program plan — same shape as
/// [`compile_plasm_dag_to_plan`], so the MCP and HTTP runtimes can always execute through the plan runner.
pub(crate) fn compile_plasm_surface_line_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    line: &str,
) -> Result<serde_json::Value, String> {
    let trimmed = line.trim();
    if is_plasm_dag_source(trimmed) {
        return compile_plasm_dag_to_plan(pipeline, symbol_map_cross_cache, session, name, trimmed);
    }
    let mut state = CompileState::new(pipeline, symbol_map_cross_cache);
    if trimmed.starts_with("return ") {
        return Err(
            "return is not Plasm syntax; write bare comma-separated roots (e.g. `a, b`, not `return a, b`)"
                .to_string(),
        );
    }
    let roots = split_return_list(trimmed, &mut state, session)?;
    if roots.is_empty() {
        return Err("expression is empty".to_string());
    }
    let nodes = state
        .nodes
        .iter()
        .map(node_to_json)
        .collect::<Result<Vec<_>, _>>()?;
    let return_value = if roots.len() == 1 {
        json!({ "kind": "node", "node": &roots[0] })
    } else {
        json!({ "kind": "parallel", "nodes": roots })
    };
    Ok(json!({
        "version": 1,
        "kind": "program",
        "name": name,
        "nodes": nodes,
        "return": return_value,
        "metadata": { "language": "plasm-dag" }
    }))
}

fn cgs_for_qualified_entity(
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Option<Arc<plasm_core::schema::CGS>> {
    session
        .contexts_by_entry
        .get(qe.entry_id.as_str())
        .map(|c| c.cgs.clone())
        .or_else(|| (session.entry_id == qe.entry_id).then(|| session.cgs.clone()))
}

/// Logical row keys materialized by entity decode (`FieldDecoder` stores each field under its CGS name).
fn logical_row_field_paths_for_entity(ent: &EntityDef) -> BTreeSet<Vec<String>> {
    let mut set = BTreeSet::new();
    for name in ent.fields.keys() {
        set.insert(vec![name.as_str().to_string()]);
    }
    for rel_name in ent.relations.keys() {
        set.insert(vec![rel_name.as_str().to_string()]);
    }
    set
}

fn logical_row_field_paths_from_names(names: &[String]) -> BTreeSet<Vec<String>> {
    names.iter().map(|n| vec![n.clone()]).collect()
}

fn resolve_surface_dag_node<'a>(
    state: &'a CompileState<'_>,
    staged: &'a [DagNode],
    mut node_id: String,
) -> Option<&'a DagNode> {
    for _ in 0..512 {
        let node = lookup_dag_node(state, staged, node_id.as_str())?;
        match &node.source {
            DagNodeSource::Surface { .. } | DagNodeSource::RelationTraversal { .. } => {
                return Some(node);
            }
            DagNodeSource::Compute { source, .. } => node_id = source.clone(),
            DagNodeSource::Derive { .. }
            | DagNodeSource::Data(_)
            | DagNodeSource::ForEach { .. } => return None,
        }
    }
    None
}

fn capability_for_surface_expr<'a>(
    cgs: &'a plasm_core::schema::CGS,
    expr: &'a Expr,
) -> Result<Option<&'a CapabilitySchema>, String> {
    match expr {
        Expr::Query(q) => {
            let cap = if let Some(name) = q.capability_name.as_deref() {
                cgs.get_capability(name).ok_or_else(|| {
                    format!(
                        "unknown query capability `{name}` for entity `{}`",
                        q.entity
                    )
                })?
            } else {
                query_resolve::resolve_query_capability(q, cgs).map_err(|e| e.to_string())?
            };
            Ok(Some(cap))
        }
        Expr::Get(g) => Ok(cgs
            .find_capabilities(g.reference.entity_type.as_str(), CapabilityKind::Get)
            .into_iter()
            .next()),
        Expr::Create(c) => Ok(cgs.get_capability(c.capability.as_str())),
        Expr::Delete(d) => Ok(cgs.get_capability(d.capability.as_str())),
        Expr::Invoke(i) => Ok(cgs.get_capability(i.capability.as_str())),
        Expr::Chain(_)
        | Expr::TeachingValue { .. }
        | Expr::Page(_)
        | Expr::Wait(_)
        | Expr::Cancel(_) => Ok(None),
    }
}

/// Row keys projected by the upstream surface capability (`provides`), when narrower than the entity.
fn logical_row_field_paths_for_surface_node(
    session: &ExecuteSession,
    node: &DagNode,
) -> Result<Option<BTreeSet<Vec<String>>>, String> {
    let (parsed, qe) = match &node.source {
        DagNodeSource::Surface {
            parsed,
            qualified_entity,
            ..
        } => (parsed, qualified_entity),
        DagNodeSource::RelationTraversal {
            parsed,
            qualified_entity,
            ..
        } => (parsed, qualified_entity),
        _ => return Ok(None),
    };
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let Some(cap) = capability_for_surface_expr(cgs.as_ref(), &parsed.expr)? else {
        return Ok(None);
    };
    let provides = cgs.effective_provides(cap);
    if provides.is_empty() {
        return Ok(None);
    }
    Ok(Some(logical_row_field_paths_from_names(&provides)))
}

fn infer_entity_row_columns(
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Result<Vec<OutputName>, String> {
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let ent = cgs.get_entity(qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` in catalog `{}`",
            qe.entity, qe.entry_id
        )
    })?;
    let paths = logical_row_field_paths_for_entity(ent);
    paths
        .into_iter()
        .map(|segs| OutputName::new(segs.join(".")))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn lookup_dag_node<'a>(
    state: &'a CompileState<'_>,
    staged: &'a [DagNode],
    id: &str,
) -> Option<&'a DagNode> {
    state.get(id).or_else(|| staged.iter().find(|n| n.id == id))
}

/// Infer `[p#,…]` columns from `{{ r.field }}` / `{{ field }}` references in a row template body.
fn infer_columns_from_minijinja_template(template: &str) -> Option<Vec<OutputName>> {
    let mut cols = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let expr = after[..end].trim();
        let field = expr
            .strip_prefix("r.")
            .or_else(|| expr.strip_prefix("rows[0]."))
            .map(|f| f.split('|').next().unwrap_or(f).trim());
        let Some(field) = field else {
            rest = &after[end + 2..];
            continue;
        };
        if field == "rows" || field.is_empty() {
            rest = &after[end + 2..];
            continue;
        }
        if field
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            if let Ok(name) = OutputName::new(field.to_string()) {
                if !cols
                    .iter()
                    .any(|c: &OutputName| c.as_str() == name.as_str())
                {
                    cols.push(name);
                }
            }
        }
        rest = &after[end + 2..];
    }
    if cols.is_empty() {
        None
    } else {
        Some(cols)
    }
}

fn infer_render_columns_for_node(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    node: &DagNode,
) -> Result<Vec<OutputName>, String> {
    match &node.source {
        DagNodeSource::Compute {
            op,
            schema,
            source: parent_id,
            ..
        } => match op {
            ComputeOp::Project { fields } => Ok(fields.keys().cloned().collect()),
            ComputeOp::Aggregate { .. } => Ok(schema
                .fields
                .iter()
                .map(|f| f.name.clone())
                .collect()),
            ComputeOp::GroupBy { keys, aggregates } => {
                let mut cols = Vec::new();
                for key in keys {
                    cols.push(OutputName::new(key.dotted()).map_err(|e| e.to_string())?);
                }
                cols.extend(aggregates.iter().map(|a| a.name.clone()));
                Ok(cols)
            }
            ComputeOp::Sort { .. } | ComputeOp::Limit { .. } | ComputeOp::DedupeBy { .. } => {
                let parent = lookup_dag_node(state, staged, parent_id.as_str()).ok_or_else(|| {
                    format!("template column inference: missing upstream node `{parent_id}`")
                })?;
                infer_render_columns_for_node(session, state, staged, parent)
            }
            ComputeOp::Render { .. } => Err(
                "cannot infer columns from a row-to-text template result; bind a row-producing query/relation/projection, or write explicit `[field,...] <<TAG` columns before the template".into(),
            ),
            ComputeOp::Filter { .. } => {
                let parent = lookup_dag_node(state, staged, parent_id.as_str()).ok_or_else(|| {
                    format!("template column inference: missing upstream node `{parent_id}`")
                })?;
                infer_render_columns_for_node(session, state, staged, parent)
            }
        },
        DagNodeSource::Surface {
            qualified_entity, ..
        }
        | DagNodeSource::RelationTraversal {
            qualified_entity, ..
        } => infer_entity_row_columns(session, qualified_entity),
        DagNodeSource::Data(_) => Err(
            "data literals cannot provide inferred template columns; use explicit `[field,...] <<TAG` columns or bind a query".into(),
        ),
        DagNodeSource::Derive { .. } => {
            Err("derive bindings cannot provide inferred template columns".into())
        }
        DagNodeSource::ForEach { .. } => {
            Err("for_each bindings cannot provide inferred template columns".into())
        }
    }
}

fn single_segment_teaching_field_hint(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    path: &FieldPath,
) -> String {
    let segs = path.segments().to_vec();
    if segs.len() != 1 {
        return String::new();
    }
    let wire = segs[0].as_str();
    let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let sym = map.ident_sym_entity_field(qe.entity.as_str(), wire);
    if sym != wire {
        format!(" For `{wire}` the active teaching-table symbol is `{sym}`.")
    } else {
        String::new()
    }
}

fn resolve_immediate_compute_schema(
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
) -> Option<SyntheticResultSchema> {
    let node = staged
        .iter()
        .find(|n| n.id == source_id)
        .or_else(|| state.get(source_id))?;
    match &node.source {
        DagNodeSource::Compute { schema, .. } => Some(schema.clone()),
        _ => None,
    }
}

fn validate_compute_paths_for_schema(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    schema: &SyntheticResultSchema,
    paths: &[FieldPath],
    op_label: &str,
) -> Result<(), String> {
    let allowed: std::collections::BTreeSet<String> = schema
        .fields
        .iter()
        .map(|f| f.name.as_str().to_string())
        .collect();
    for path in paths {
        let wire = if path.segments().len() == 1 {
            crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                qe,
                path.segments()[0].as_str(),
            )
        } else {
            path.dotted()
        };
        if allowed.contains(&wire) {
            continue;
        }
        let cols: Vec<&str> = allowed.iter().map(String::as_str).collect();
        return Err(format!(
            "Plasm program {op_label}: field path `{wire}` is not a row field of the upstream compute output (row fields: {}). Use `p#` symbols from the teaching `rows:` column, or wire field names as sugar — for example after `group_by`, use aggregate output names such as `n`.",
            cols.join(", ")
        ));
    }
    Ok(())
}

fn is_opaque_passthrough_compute_schema(schema: &SyntheticResultSchema) -> bool {
    schema.fields.len() == 1
        && schema.fields[0].name.as_str() == "value"
        && matches!(schema.fields[0].value_kind, SyntheticValueKind::Unknown)
}

/// Passthrough row schema for postfix compute on catalog surfaces, relations, or typed compute chains.
fn compute_passthrough_or_fallback_schema(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source: &str,
    fallback_entity: &str,
) -> SyntheticResultSchema {
    resolve_immediate_compute_schema(state, staged, source)
        .filter(|s| !is_opaque_passthrough_compute_schema(s))
        .unwrap_or_else(|| {
            synthetic_schema_passthrough_rows(session, state, staged, source)
                .unwrap_or_else(|_| single_unknown_schema(fallback_entity))
        })
}

fn validate_compute_paths_for_dag_source(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
    paths: &[FieldPath],
    op_label: &str,
) -> Result<(), String> {
    if let Some(schema) = resolve_immediate_compute_schema(state, staged, source_id) {
        if !is_opaque_passthrough_compute_schema(&schema) {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string());
            return validate_compute_paths_for_schema(
                session,
                state.cross_cache,
                qe.as_ref(),
                &schema,
                paths,
                op_label,
            );
        }
    }
    if let Some(surface) = resolve_surface_dag_node(state, staged, source_id.to_string()) {
        if let Some(allowed) = logical_row_field_paths_for_surface_node(session, surface)? {
            return validate_compute_paths_for_allowed_set(
                session,
                state.cross_cache,
                surface,
                &allowed,
                paths,
                op_label,
            );
        }
    }
    if let Some(qe) = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string())
    {
        return validate_compute_paths_for_entity(session, state.cross_cache, &qe, paths, op_label);
    }
    Ok(())
}

fn capability_input_param_wires(cap: &CapabilitySchema) -> BTreeSet<String> {
    let Some(is) = &cap.input_schema else {
        return BTreeSet::new();
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return BTreeSet::new();
    };
    fields.iter().map(|f| f.name.clone()).collect()
}

#[allow(clippy::too_many_arguments)]
fn row_contract_field_error(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    cap: Option<&CapabilitySchema>,
    path: &FieldPath,
    wire: &str,
    allowed_cols: &[String],
    op_label: &str,
) -> String {
    let hint = single_segment_teaching_field_hint(session, symbol_map_cross_cache, qe, path);
    if let Some(cap) = cap {
        let inputs = capability_input_param_wires(cap);
        if inputs.contains(wire) {
            return format!(
                "Plasm program {op_label}: `{wire}` is an input on {}, not a row field. {} rows: {}. Use one of those row fields, or fetch each {} first for the full {} row.{hint}",
                cap.name.as_str(),
                cap.name.as_str(),
                allowed_cols.join(", "),
                qe.entity,
                qe.entity,
            );
        }
    }
    format!(
        "Plasm program {op_label}: field path `{}` is not a row field of the upstream capability output for entity `{}` (catalog entry `{}`); row fields: {}. Use `p#` symbols from the teaching `rows:` column, or wire field names as sugar.{hint}",
        path.dotted(),
        qe.entity,
        qe.entry_id,
        allowed_cols.join(", "),
    )
}

fn validate_surface_inline_projection(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node: &DagNode,
) -> Result<(), String> {
    let parsed = match &node.source {
        DagNodeSource::Surface { parsed, .. } | DagNodeSource::RelationTraversal { parsed, .. } => {
            parsed
        }
        _ => return Ok(()),
    };
    let Some(fields) = parsed.projection.as_ref() else {
        return Ok(());
    };
    if fields.is_empty() {
        return Ok(());
    }
    let paths: Vec<FieldPath> = fields
        .iter()
        .map(|f| FieldPath::from_dotted(f.as_str()))
        .collect::<Result<_, _>>()?;
    if let Some(allowed) = logical_row_field_paths_for_surface_node(session, node)? {
        validate_compute_paths_for_allowed_set(
            session,
            state.cross_cache,
            node,
            &allowed,
            &paths,
            "surface projection",
        )?;
    }
    Ok(())
}

fn validate_compute_paths_for_allowed_set(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    surface_node: &DagNode,
    allowed: &BTreeSet<Vec<String>>,
    paths: &[FieldPath],
    op_label: &str,
) -> Result<(), String> {
    let qe = match &surface_node.source {
        DagNodeSource::Surface {
            qualified_entity, ..
        }
        | DagNodeSource::RelationTraversal {
            qualified_entity, ..
        } => qualified_entity,
        _ => {
            return Err(
                "Plasm program internal: validate_compute_paths_for_allowed_set requires surface"
                    .into(),
            );
        }
    };
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let cap = match &surface_node.source {
        DagNodeSource::Surface { parsed, .. } | DagNodeSource::RelationTraversal { parsed, .. } => {
            capability_for_surface_expr(cgs.as_ref(), &parsed.expr)?
        }
        _ => None,
    };
    for path in paths {
        let mut segs: Vec<String> = path.segments().to_vec();
        if segs.len() == 1 {
            let wire = crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                Some(qe),
                segs[0].as_str(),
            );
            segs[0] = wire.clone();
            if allowed.contains(&segs) {
                continue;
            }
        } else if allowed.contains(&segs) {
            continue;
        }
        let cols: Vec<String> = allowed.iter().filter_map(|s| s.first().cloned()).collect();
        let wire = path.dotted();
        let wire_for_input = if segs.len() == 1 {
            segs[0].as_str()
        } else {
            wire.as_str()
        };
        return Err(row_contract_field_error(
            session,
            symbol_map_cross_cache,
            qe,
            cap,
            path,
            wire_for_input,
            &cols,
            op_label,
        ));
    }
    Ok(())
}

fn validate_compute_paths_for_entity(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    paths: &[FieldPath],
    op_label: &str,
) -> Result<(), String> {
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "Plasm program internal: catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let ent = cgs.get_entity(qe.entity.as_str()).ok_or_else(|| {
        format!(
            "Plasm program internal: unknown entity `{}` in catalog `{}`",
            qe.entity, qe.entry_id
        )
    })?;
    let allowed = logical_row_field_paths_for_entity(ent);
    for path in paths {
        let mut segs: Vec<String> = path.segments().to_vec();
        if segs.len() == 1 {
            let wire = crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                Some(qe),
                segs[0].as_str(),
            );
            segs[0] = wire;
        }
        if allowed.contains(&segs) {
            continue;
        }
        let hint = single_segment_teaching_field_hint(session, symbol_map_cross_cache, qe, path);
        return Err(format!(
            "Plasm program {op_label}: field path `{}` is not a row field of entity `{}` (catalog entry `{}`). Use wire field names or `p#` symbols from the active TSV teaching table for this entity — mixing another entity's symbols yields null columns.{hint}",
            path.dotted(),
            qe.entity,
            qe.entry_id
        ));
    }
    Ok(())
}

fn resolve_compute_field_path(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    path: &FieldPath,
) -> Result<FieldPath, String> {
    let segs = path.segments();
    if segs.len() != 1 {
        return Ok(path.clone());
    }
    let wire = crate::plasm_plan_run::resolve_wire_field_token(
        session,
        symbol_map_cross_cache,
        qe,
        segs[0].as_str(),
    );
    FieldPath::from_dotted(&wire)
}

/// Walk [`DagNodeSource::Compute`] chains to the nearest surface or relation node that carries a
/// [`QualifiedEntityKey`] (the row entity after decode).
fn resolve_qualified_entity_for_dag_source(
    state: &CompileState<'_>,
    staged: &[DagNode],
    mut node_id: String,
) -> Option<QualifiedEntityKey> {
    for _ in 0..512 {
        let node = staged
            .iter()
            .find(|n| n.id == node_id)
            .or_else(|| state.get(node_id.as_str()))?;
        match &node.source {
            DagNodeSource::Surface {
                qualified_entity, ..
            }
            | DagNodeSource::RelationTraversal {
                qualified_entity, ..
            } => {
                return Some(qualified_entity.clone());
            }
            DagNodeSource::Compute { source, .. } => node_id = source.clone(),
            DagNodeSource::Derive { .. }
            | DagNodeSource::Data(_)
            | DagNodeSource::ForEach { .. } => return None,
        }
    }
    None
}

/// Schema describing passthrough rows from `source_id` when it resolves to a catalog entity surface
/// or relation node (preserves [`SyntheticResultSchema::entity`] for downstream plan validation).
fn synthetic_schema_passthrough_rows(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
) -> Result<SyntheticResultSchema, String> {
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string())
        .ok_or_else(|| {
            format!(
                "bare-label `.singleton()` / `.page_size(...)` requires `{source_id}` to trace to a catalog entity row (surface query or relation); synthetic binds and literals cannot use this postfix here"
            )
        })?;
    let cols = infer_entity_row_columns(session, &qe)?;
    if cols.is_empty() {
        return Err(format!(
            "Plasm internal: cannot infer passthrough columns for entity `{}`",
            qe.entity
        ));
    }
    Ok(schema_from_output_fields(
        qe.entity.as_str(),
        cols.iter(),
        SyntheticValueKind::Unknown,
    ))
}

/// Identity [`ComputeOp::Project`] map plus schema for passthrough compute nodes (e.g. bare-label
/// `.page_size(n)` lowering).
fn passthrough_identity_projection_fields(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
) -> Result<(BTreeMap<OutputName, FieldPath>, SyntheticResultSchema), String> {
    let schema = synthetic_schema_passthrough_rows(session, state, staged, source_id)?;
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string())
        .expect("trace matches synthetic_schema_passthrough_rows");
    let mut map = BTreeMap::new();
    for field in &schema.fields {
        let path = FieldPath::from_dotted(field.name.as_str())?;
        map.insert(field.name.clone(), path);
    }
    let paths: Vec<FieldPath> = map.values().cloned().collect();
    validate_compute_paths_for_entity(
        session,
        state.cross_cache,
        &qe,
        &paths,
        "bare-label passthrough projection",
    )?;
    Ok((map, schema))
}

fn postfix_op_to_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    op: &PlasmPostfixOp,
    source: &str,
    id: &str,
    expr_display: &str,
) -> Result<DagNode, String> {
    let mk = |op: ComputeOp, schema: SyntheticResultSchema, singleton: bool| -> DagNode {
        DagNode {
            id: id.to_string(),
            expr: expr_display.to_string(),
            singleton,
            page_size: None,
            source: DagNodeSource::Compute {
                source: source.to_string(),
                op,
                schema,
            },
        }
    };
    match op {
        PlasmPostfixOp::Limit(n) => Ok(mk(
            ComputeOp::Limit { count: *n },
            compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanLimit"),
            *n <= 1,
        )),
        PlasmPostfixOp::Filter { body } => {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string())
                .ok_or_else(|| {
                    format!("filter(...) on `{source}` requires an upstream catalog entity row")
                })?;
            let cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
                format!(
                    "catalog `{}` is not loaded for entity `{}`",
                    qe.entry_id, qe.entity
                )
            })?;
            let layers = vec![cgs.as_ref()];
            let sym_map = symbol_map_for_plasm_surface_parse(session, state.cross_cache);
            let core_qe =
                plasm_core::QualifiedEntityKey::new(qe.entry_id.as_str(), qe.entity.as_str());
            let row_pred = plasm_core::parse_row_predicate_list(
                qe.entity.as_str(),
                body.as_str(),
                &layers,
                sym_map,
            )?;
            let tc_ctx = plasm_core::RowPredicateTypeCtx {
                qe: &core_qe,
                cgs: cgs.as_ref(),
                symbol_map: None,
            };
            plasm_core::type_check_row_predicate(&row_pred, &tc_ctx).map_err(|e| e.to_string())?;
            let mut paths = Vec::new();
            for clause in &row_pred.0 {
                paths.push(FieldPath::from_dotted(clause.field.as_str())?);
            }
            if !paths.is_empty() {
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "filter(...)",
                )?;
            }
            let predicates = crate::row_predicate_lower::lower_row_predicate_to_plan(
                &row_pred,
                session,
                &qe,
                state.cross_cache,
            )?;
            let schema = synthetic_schema_passthrough_rows(session, state, staged, source)?;
            Ok(mk(ComputeOp::Filter { predicates }, schema, false))
        }
        PlasmPostfixOp::Sort { args } => {
            let (key, descending) = parse_sort_field_and_direction(args)?;
            if key.is_empty() {
                return Err("sort(...) requires a non-empty field".into());
            }
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let key_fp = resolve_compute_field_path(
                session,
                state.cross_cache,
                qe.as_ref(),
                &FieldPath::from_dotted(&key)?,
            )?;
            validate_compute_paths_for_dag_source(
                session,
                state,
                staged,
                source,
                std::slice::from_ref(&key_fp),
                "sort(...)",
            )?;
            let schema =
                compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanSort");
            Ok(mk(
                ComputeOp::Sort {
                    key: key_fp,
                    descending,
                },
                schema,
                false,
            ))
        }
        PlasmPostfixOp::Aggregate { args } => {
            let mut aggregates = parse_aggregates(args)?;
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            if let Some(qe) = qe.as_ref() {
                for agg in &mut aggregates {
                    if let Some(field) = agg.field.as_ref() {
                        agg.field = Some(resolve_compute_field_path(
                            session,
                            state.cross_cache,
                            Some(qe),
                            field,
                        )?);
                    }
                }
                let paths: Vec<FieldPath> =
                    aggregates.iter().filter_map(|a| a.field.clone()).collect();
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "aggregate(...)",
                )?;
            }
            let schema = schema_from_aggregates("PlanAggregate", &aggregates);
            Ok(mk(ComputeOp::Aggregate { aggregates }, schema, true))
        }
        PlasmPostfixOp::GroupBy { args } => {
            let (key_names, agg_tail) = parse_group_by_key_and_aggregate_tail(args)?;
            let aggregates = if agg_tail.trim().is_empty() {
                if key_names.len() != 1 {
                    return Err(
                        "group_by(k1, k2, …) without aggregates requires .aggregate(...) — use group_by(k1, k2).aggregate(n=count) or group_by(k1, k2, n=count)".into(),
                    );
                }
                // Bare `group_by(key)` sugar alias (single key only).
                parse_aggregates("count=count")?
            } else {
                parse_aggregates(agg_tail.as_str())?
            };
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let mut key_fps = Vec::new();
            for key in &key_names {
                key_fps.push(resolve_compute_field_path(
                    session,
                    state.cross_cache,
                    qe.as_ref(),
                    &FieldPath::from_dotted(key)?,
                )?);
            }
            let mut aggregates = aggregates;
            if let Some(qe) = qe.as_ref() {
                for agg in &mut aggregates {
                    if let Some(field) = agg.field.as_ref() {
                        agg.field = Some(resolve_compute_field_path(
                            session,
                            state.cross_cache,
                            Some(qe),
                            field,
                        )?);
                    }
                }
                let mut paths = key_fps.clone();
                paths.extend(aggregates.iter().filter_map(|a| a.field.clone()));
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "group_by(...)",
                )?;
            }
            let schema = schema_from_group_by("PlanGroup", &key_fps, &aggregates);
            Ok(mk(
                ComputeOp::GroupBy {
                    keys: key_fps,
                    aggregates,
                },
                schema,
                false,
            ))
        }
        PlasmPostfixOp::Dedupe { keys } | PlasmPostfixOp::Distinct { keys: Some(keys) } => {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let key_fps = parse_dedupe_key_paths(session, state.cross_cache, qe.as_ref(), keys)?;
            validate_compute_paths_for_dag_source(
                session,
                state,
                staged,
                source,
                &key_fps,
                "dedupe(...)",
            )?;
            let schema = synthetic_schema_passthrough_rows(session, state, staged, source)?;
            Ok(mk(ComputeOp::DedupeBy { keys: key_fps }, schema, false))
        }
        PlasmPostfixOp::Distinct { keys: None } => {
            let schema = synthetic_schema_passthrough_rows(session, state, staged, source)?;
            Ok(mk(ComputeOp::DedupeBy { keys: vec![] }, schema, false))
        }
        PlasmPostfixOp::Projection { fields } => {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let mut map = BTreeMap::new();
            for field in parse_field_list(session, state.cross_cache, qe.as_ref(), fields)? {
                map.insert(
                    OutputName::new(field.clone())?,
                    FieldPath::from_dotted(&field)?,
                );
            }
            if let Some(qe) = qe {
                let paths: Vec<FieldPath> = map.values().cloned().collect();
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "postfix projection",
                )?;
                let entity = qe.entity.as_str();
                let schema =
                    schema_from_output_fields(entity, map.keys(), SyntheticValueKind::Unknown);
                return Ok(mk(ComputeOp::Project { fields: map }, schema, false));
            }
            let schema =
                schema_from_output_fields("PlanProject", map.keys(), SyntheticValueKind::Unknown);
            Ok(mk(ComputeOp::Project { fields: map }, schema, false))
        }
        PlasmPostfixOp::Singleton | PlasmPostfixOp::PageSize(_) => {
            Err("internal: singleton/page_size must be split as tail flags before lowering".into())
        }
    }
}

fn lower_row_expression(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    binding_id: &str,
    full_rhs: &str,
    final_id: Option<&str>,
) -> Result<Vec<DagNode>, String> {
    let (head, suffixes) = decompose_row_suffix_stream(session, state, full_rhs)?;
    if suffixes.is_empty() {
        return Ok(vec![compile_surface_node(
            session, state, binding_id, full_rhs,
        )?]);
    }
    lower_suffix_stream(
        session, state, binding_id, full_rhs, &head, suffixes, final_id,
    )
}

/// When `expr` carries postfix and/or relation suffixes, lower the full DAG spine.
/// Returns `None` for pure surface heads (no row suffix stream).
fn try_lower_row_suffix_expression(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
) -> Result<Option<Vec<DagNode>>, String> {
    let expr_trim = expr.trim();
    let expanded = ExpandedProgramSurface::new(session, state.pipeline, expr_trim);
    let (_, suffixes) = decompose_row_suffix_stream(session, state, expanded.as_str())?;
    if suffixes.is_empty() {
        return Ok(None);
    }
    Ok(Some(lower_row_expression(
        session,
        state,
        id,
        expr_trim,
        Some(id),
    )?))
}

/// Classify interleaved relation + transform suffixes after peeling postfix transforms and relation hops from the right.
fn decompose_row_suffix_stream(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    expr: &str,
) -> Result<(String, Vec<RowSuffix>), String> {
    let mut cur = expr.trim().to_string();
    let mut suffixes_rev: Vec<RowSuffix> = Vec::new();

    loop {
        let (core, ops) =
            peel_postfix_suffixes(&cur).map_err(|e| format!("row suffix stream: {e}"))?;
        for op in ops.iter().rev() {
            suffixes_rev.push(RowSuffix::from_postfix_op(op)?);
        }
        cur = core;

        if let Some((base, segment)) = try_split_single_hop_surface_chain(session, state, &cur) {
            suffixes_rev.push(RowSuffix::Relation { wire: segment });
            cur = base;
            continue;
        }
        break;
    }

    suffixes_rev.reverse();
    Ok((cur, suffixes_rev))
}

fn row_suffix_to_postfix(suffix: &RowSuffix) -> Option<PlasmPostfixOp> {
    match suffix {
        RowSuffix::Limit { count } => Some(PlasmPostfixOp::Limit(*count as usize)),
        RowSuffix::Project { fields } => Some(PlasmPostfixOp::Projection {
            fields: fields.join(","),
        }),
        RowSuffix::Sort { args } => Some(PlasmPostfixOp::Sort { args: args.clone() }),
        RowSuffix::Filter { body } => Some(PlasmPostfixOp::Filter { body: body.clone() }),
        RowSuffix::Aggregate { args } => Some(PlasmPostfixOp::Aggregate { args: args.clone() }),
        RowSuffix::GroupBy { args } => Some(PlasmPostfixOp::GroupBy { args: args.clone() }),
        RowSuffix::Dedupe { keys } => Some(PlasmPostfixOp::Dedupe { keys: keys.clone() }),
        RowSuffix::Distinct { keys } => Some(PlasmPostfixOp::Distinct { keys: keys.clone() }),
        RowSuffix::Singleton => Some(PlasmPostfixOp::Singleton),
        RowSuffix::PageSize { n } => Some(PlasmPostfixOp::PageSize(*n as usize)),
        RowSuffix::Relation { .. } => None,
    }
}

fn compile_state_with_nodes<'a>(
    state: &'a CompileState<'a>,
    nodes: &[DagNode],
) -> CompileState<'a> {
    let mut scratch = CompileState {
        nodes: state.nodes.clone(),
        labels: state.labels.clone(),
        pipeline: state.pipeline,
        cross_cache: state.cross_cache,
    };
    for node in nodes {
        let idx = scratch.nodes.len();
        scratch.labels.insert(node.id.clone(), idx);
        scratch.nodes.push(node.clone());
    }
    scratch
}

/// Fuse `.group_by(keys).aggregate(specs)` into one `group_by` args tail for plan lowering.
fn coalesce_group_by_aggregate_suffixes(steps: Vec<RowSuffix>) -> Vec<RowSuffix> {
    let mut out = Vec::with_capacity(steps.len());
    let mut i = 0;
    while i < steps.len() {
        if let RowSuffix::GroupBy { args: gb } = &steps[i] {
            if let Some(RowSuffix::Aggregate { args: agg }) = steps.get(i + 1) {
                out.push(RowSuffix::GroupBy {
                    args: format!("{gb},{agg}"),
                });
                i += 2;
                continue;
            }
        }
        out.push(steps[i].clone());
        i += 1;
    }
    out
}

/// Fold an ordered [`RowSuffix`] stream onto a surface or label head.
fn lower_suffix_stream(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    binding_id: &str,
    full_rhs: &str,
    head: &str,
    suffixes: Vec<RowSuffix>,
    final_id: Option<&str>,
) -> Result<Vec<DagNode>, String> {
    if suffixes.is_empty() {
        return Err("internal: lower_suffix_stream requires non-empty suffixes".into());
    }

    let tail_singleton = suffixes.iter().any(|s| matches!(s, RowSuffix::Singleton));
    let tail_page_size = suffixes.iter().find_map(|s| {
        if let RowSuffix::PageSize { n } = s {
            Some(*n as usize)
        } else {
            None
        }
    });

    let mut out: Vec<DagNode> = Vec::new();
    let head_trim = head.trim();

    let mut steps: Vec<RowSuffix> = suffixes
        .iter()
        .filter(|s| !matches!(s, RowSuffix::Singleton | RowSuffix::PageSize { .. }))
        .cloned()
        .collect();
    steps = coalesce_group_by_aggregate_suffixes(steps);

    if steps.is_empty() && (tail_singleton || tail_page_size.is_some()) {
        let out_id = final_id
            .map(str::to_string)
            .unwrap_or_else(|| binding_id.to_string());
        if state.contains(head_trim) {
            let staged: &[DagNode] = &out;
            let node = if tail_singleton {
                let schema = synthetic_schema_passthrough_rows(session, state, staged, head_trim)?;
                DagNode {
                    id: out_id,
                    expr: full_rhs.to_string(),
                    singleton: true,
                    page_size: tail_page_size,
                    source: DagNodeSource::Compute {
                        source: head_trim.to_string(),
                        op: ComputeOp::Limit { count: 1 },
                        schema,
                    },
                }
            } else {
                let (fields, schema) =
                    passthrough_identity_projection_fields(session, state, staged, head_trim)?;
                DagNode {
                    id: out_id,
                    expr: full_rhs.to_string(),
                    singleton: false,
                    page_size: tail_page_size,
                    source: DagNodeSource::Compute {
                        source: head_trim.to_string(),
                        op: ComputeOp::Project { fields },
                        schema,
                    },
                }
            };
            return Ok(vec![node]);
        }
        let mut node = compile_surface_node(session, state, &out_id, head_trim)?;
        node.singleton |= tail_singleton;
        node.page_size = tail_page_size.or(node.page_size);
        node.expr = full_rhs.to_string();
        return Ok(vec![node]);
    }

    let mut cur_id = if state.contains(head_trim) {
        head_trim.to_string()
    } else {
        let bid = format!("__plasm_{binding_id}_b0");
        let base = compile_surface_node(session, state, &bid, head_trim)?;
        out.push(base);
        bid
    };

    for (i, suffix) in steps.iter().enumerate() {
        let is_last = i + 1 == steps.len();
        let nid = if is_last {
            final_id
                .map(str::to_string)
                .unwrap_or_else(|| binding_id.to_string())
        } else if matches!(suffix, RowSuffix::Relation { .. }) {
            format!("__plasm_{binding_id}_r{i}")
        } else {
            format!("__plasm_{binding_id}_s{i}")
        };

        if let RowSuffix::Relation { wire } = suffix {
            let scratch = compile_state_with_nodes(state, &out);
            let rel = lower_relation_continuation(
                session,
                &scratch,
                &nid,
                &format!("{cur_id}.{wire}"),
                &cur_id,
                wire,
            )?;
            out.push(rel);
            cur_id = nid;
            continue;
        }

        if let Some(op) = row_suffix_to_postfix(suffix) {
            let node = postfix_op_to_compute(session, state, &out, &op, &cur_id, &nid, full_rhs)?;
            out.push(node);
            cur_id = nid;
        }
    }

    if let Some(ps) = tail_page_size {
        if let Some(first_surface) = out.iter_mut().find(|n| {
            matches!(
                n.source,
                DagNodeSource::Surface { .. } | DagNodeSource::RelationTraversal { .. }
            )
        }) {
            first_surface.page_size = Some(ps);
        }
    }
    if let Some(last) = out.last_mut() {
        last.singleton |= tail_singleton;
        last.expr = full_rhs.to_string();
        if let Some(ps) = tail_page_size {
            last.page_size = Some(ps);
        } else {
            last.page_size = tail_page_size.or(last.page_size);
        }
    }

    Ok(out)
}

fn plan_render_content_schema() -> Result<SyntheticResultSchema, String> {
    Ok(SyntheticResultSchema {
        entity: Some("PlanRender".to_string()),
        fields: vec![SyntheticFieldSchema {
            name: OutputName::new("content".to_string()).map_err(|e| e.to_string())?,
            value_kind: SyntheticValueKind::String,
            source: None,
        }],
    })
}

fn compile_render_from_tail(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs_display: &str,
    tail: RenderTailParse,
) -> Result<Vec<DagNode>, String> {
    match tail {
        RenderTailParse::Explicit {
            source,
            fields,
            template,
        } => {
            let scratch = compile_state_with_nodes(state, &[]);
            let qe =
                resolve_qualified_entity_for_dag_source(&scratch, &[], source.trim().to_string());
            let columns = parse_field_list(session, state.cross_cache, qe.as_ref(), fields.trim())?
                .into_iter()
                .map(OutputName::new)
                .collect::<Result<Vec<_>, _>>()?;
            compile_render_chain(
                session,
                state,
                id,
                rhs_display,
                source.trim(),
                Some(columns),
                template,
            )
        }
        RenderTailParse::Inferred { head, template } => {
            compile_render_chain(session, state, id, rhs_display, head.trim(), None, template)
        }
    }
}

fn compile_render_chain(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs_display: &str,
    head: &str,
    explicit_columns: Option<Vec<OutputName>>,
    template: String,
) -> Result<Vec<DagNode>, String> {
    let (head_core, suffixes) = decompose_row_suffix_stream(session, state, head)?;
    let tail_singleton = suffixes.iter().any(|s| matches!(s, RowSuffix::Singleton));
    let tail_page_size = suffixes.iter().find_map(|s| {
        if let RowSuffix::PageSize { n } = s {
            Some(*n as usize)
        } else {
            None
        }
    });

    let tmp = format!("__plasm_render_src_{id}");
    let prefix: Vec<DagNode> = if suffixes.is_empty() {
        if state.contains(head_core.trim()) {
            vec![]
        } else {
            vec![compile_surface_node(session, state, &tmp, head)?]
        }
    } else {
        lower_suffix_stream(session, state, &tmp, head, &head_core, suffixes, None)
            .map_err(|e| format!("Plasm program `{id}`: {e}"))?
    };

    let chain_tail_id: String = if prefix.is_empty() {
        head_core.trim().to_string()
    } else {
        prefix
            .last()
            .map(|n| n.id.clone())
            .ok_or_else(|| format!("Plasm program `{id}`: empty render chain"))?
    };

    let columns: Vec<OutputName> = if let Some(cols) = explicit_columns {
        cols
    } else if let Some(cols) = infer_columns_from_minijinja_template(&template) {
        cols
    } else {
        let tail_node =
            lookup_dag_node(state, &prefix, chain_tail_id.as_str()).ok_or_else(|| {
                format!(
                    "Plasm program `{id}`: template column inference failed for `{chain_tail_id}`"
                )
            })?;
        infer_render_columns_for_node(session, state, &prefix, tail_node)
            .map_err(|e| format!("Plasm program `{id}`: cannot infer template columns: {e}"))?
    };

    if columns.is_empty() {
        return Err(format!(
            "Plasm program `{id}`: row-to-text templates require at least one column; use `[field,...] <<TAG` after narrowing"
        ));
    }

    let mut render_node = DagNode {
        id: id.to_string(),
        expr: rhs_display.to_string(),
        singleton: true,
        // When postfix lowering built a non-empty prefix, trailing `.page_size(n)` / `.singleton()`
        // are applied to the final row-producing node there; avoid double-applying `page_size` on
        // the row-to-text template compute.
        page_size: if prefix.is_empty() {
            tail_page_size
        } else {
            None
        },
        source: DagNodeSource::Compute {
            source: chain_tail_id,
            op: ComputeOp::Render { columns, template },
            schema: plan_render_content_schema()?,
        },
    };
    render_node.singleton |= tail_singleton;

    let mut out = prefix;
    out.push(render_node);
    Ok(out)
}

fn compile_node_expr(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs: &str,
) -> Result<Vec<DagNode>, String> {
    let rhs_display = rhs.trim();
    let expanded = ExpandedProgramSurface::new(session, state.pipeline, rhs_display);
    let rhs = expanded.as_str();

    if let Some((left, right)) = split_token_top_level(rhs, "=>")? {
        let source = left.trim();
        require_node(state, source)?;
        if looks_like_plasm_effect_template(right) {
            let refs = state.program_node_id_set();
            let parsed = parse_plasm_surface_line_program(
                session,
                state.cross_cache,
                state.pipeline,
                right.trim(),
                Some(&refs),
                true,
            )
            .map_err(|e| {
                format!(
                    "Plasm program `{id}` template parse: {}",
                    format_session_symbolic_parse_error(
                        session,
                        state.cross_cache,
                        state.pipeline,
                        right.trim(),
                        &e,
                    )
                )
            })?;
            let uses = collect_template_uses_from_expr(&parsed.expr);
            let (kind, qualified, _effect, _shape) = infer_surface_contract(session, &parsed.expr)?;
            if !matches!(
                kind,
                PlanNodeKind::Create
                    | PlanNodeKind::Update
                    | PlanNodeKind::Delete
                    | PlanNodeKind::Action
            ) {
                return Err(format!(
                    "Plasm program `{id}` for_each right side must be a write/side-effect expression"
                ));
            }
            return Ok(vec![DagNode {
                id: id.to_string(),
                expr: rhs_display.to_string(),
                singleton: false,
                page_size: None,
                source: DagNodeSource::ForEach {
                    source: source.to_string(),
                    parsed_template: expr_template_json(&parsed, &uses, right.trim())?,
                    display_expr: right.trim().to_string(),
                    effect_kind: kind,
                    qualified_entity: qualified,
                    uses_result: uses,
                },
            }]);
        }
        let (value, inputs) = parse_plan_value_expr(right.trim(), state, Some("_"))?;
        reject_derive_map_surface_expr_literal(&value)?;
        return Ok(vec![DagNode {
            id: id.to_string(),
            expr: rhs_display.to_string(),
            singleton: false,
            page_size: None,
            source: DagNodeSource::Derive {
                source: source.to_string(),
                value,
                inputs,
            },
        }]);
    }
    if let Some(tail) = try_parse_render_tail(rhs)? {
        return compile_render_from_tail(session, state, id, rhs_display, tail);
    }

    if let Some(nodes) = try_lower_row_suffix_expression(session, state, id, rhs_display)? {
        return Ok(nodes);
    }

    if let Ok(value) = parse_plan_value_expr(rhs, state, None) {
        if looks_like_data_literal(rhs) {
            return Ok(vec![DagNode {
                id: id.to_string(),
                expr: rhs_display.to_string(),
                singleton: true,
                page_size: None,
                source: DagNodeSource::Data(value.0),
            }]);
        }
    }
    Ok(vec![compile_surface_node(session, state, id, rhs)?])
}

/// Longest bound label match so `repos.foo` wins over `repo.foo` when both exist.
fn longest_matching_bound_prefix(expr: &str, state: &CompileState<'_>) -> Option<(String, String)> {
    let expr = expr.trim();
    let mut best: Option<(usize, String, String)> = None;
    for label in state.labels.keys() {
        let prefix = format!("{label}.");
        if expr.starts_with(&prefix) {
            let tail = expr[prefix.len()..].to_string();
            if best.as_ref().is_none_or(|(len, _, _)| label.len() > *len) {
                best = Some((label.len(), label.clone(), tail));
            }
        }
    }
    best.map(|(_, l, t)| (l, t))
}

/// Unified binding contract for a program label (replaces parallel walkers).
fn binding_contract(state: &CompileState<'_>, label: &str) -> Option<ProgramBindingContract> {
    let node = state.get(label)?;
    Some(binding_contract_for_node(state, label, node))
}

fn binding_contract_for_node(
    state: &CompileState<'_>,
    label: &str,
    node: &DagNode,
) -> ProgramBindingContract {
    let mut contract = binding_contract_inner(state, label, node);
    if node.singleton {
        contract.row_cardinality = match contract.row_cardinality {
            RowCardinalityProof::StaticPlural | RowCardinalityProof::RuntimeChecked => {
                RowCardinalityProof::BoundedSingleton {
                    kind: BoundedSingletonKind::ExplicitSingletonPostfix,
                    from_plural_source: true,
                }
            }
            other => other,
        };
    }
    contract
}

fn binding_contract_inner(
    state: &CompileState<'_>,
    label: &str,
    node: &DagNode,
) -> ProgramBindingContract {
    match &node.source {
        DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            result_shape,
            ..
        } => {
            let row_cardinality =
                if matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_)) {
                    RowCardinalityProof::StaticSingleton
                } else if matches!(kind, PlanNodeKind::Query | PlanNodeKind::Search) {
                    RowCardinalityProof::StaticPlural
                } else {
                    RowCardinalityProof::RuntimeChecked
                };
            let continuation =
                if matches!(
                    kind,
                    PlanNodeKind::Get | PlanNodeKind::Query | PlanNodeKind::Search
                ) || matches!(parsed.expr, Expr::Get(_) | Expr::Query(_) | Expr::Chain(_))
                {
                    ContinuationCapability::RelationDot {
                        segments: SegmentPolicy::MultiSegment,
                    }
                } else {
                    ContinuationCapability::Terminal
                };
            let anchor = if matches!(&continuation, ContinuationCapability::Terminal) {
                ContinuationAnchor::None
            } else {
                ContinuationAnchor::RootSurface(node.expr.clone())
            };
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: qualified_entity.clone(),
                result_shape: *result_shape,
                row_cardinality,
                continuation,
                anchor,
            }
        }
        DagNodeSource::RelationTraversal {
            qualified_entity,
            result_shape,
            plan_relation,
            expanded_plasm,
            source_label,
            ..
        } => {
            let parent = binding_contract(state, source_label)
                .map(|c| c.row_cardinality)
                .unwrap_or(RowCardinalityProof::RuntimeChecked);
            let row_cardinality = match plan_relation.cardinality {
                RelationCardinality::One => parent.after_one_cardinality_relation(),
                RelationCardinality::Many => parent.after_many_cardinality_relation(),
            };
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: qualified_entity.clone(),
                result_shape: *result_shape,
                row_cardinality,
                continuation: ContinuationCapability::RelationDot {
                    segments: SegmentPolicy::SingleSegment,
                },
                anchor: ContinuationAnchor::RelationExpand(expanded_plasm.clone()),
            }
        }
        DagNodeSource::Compute {
            source,
            op: ComputeOp::Project { .. },
            schema,
        } => {
            let parent = binding_contract(state, source)
                .unwrap_or_else(|| synthetic_row_contract(source, schema));
            let anchor = match state.get(source).map(|n| &n.source) {
                Some(DagNodeSource::Surface { parsed, .. })
                    if matches!(parsed.expr, Expr::Get(_)) =>
                {
                    ContinuationAnchor::RootSurface(state.get(source).expect("source").expr.clone())
                }
                _ => ContinuationAnchor::BindingLabel,
            };
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: parent.row_entity.clone(),
                result_shape: parent.result_shape,
                row_cardinality: parent.row_cardinality,
                continuation: parent.continuation,
                anchor,
            }
        }
        DagNodeSource::Compute {
            source,
            op: ComputeOp::Limit { count },
            schema,
        } => {
            let parent = binding_contract(state, source)
                .unwrap_or_else(|| synthetic_row_contract(source, schema));
            let from_plural = matches!(
                parent.row_cardinality,
                RowCardinalityProof::StaticPlural | RowCardinalityProof::RuntimeChecked
            ) || *count > 1;
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: parent.row_entity.clone(),
                result_shape: parent.result_shape,
                row_cardinality: if *count <= 1 {
                    RowCardinalityProof::BoundedSingleton {
                        kind: BoundedSingletonKind::LimitOne,
                        from_plural_source: from_plural,
                    }
                } else {
                    RowCardinalityProof::StaticPlural
                },
                continuation: parent.continuation,
                anchor: ContinuationAnchor::BindingLabel,
            }
        }
        DagNodeSource::Compute {
            op: ComputeOp::Render { .. },
            ..
        } => ProgramBindingContract {
            label: label.to_string(),
            row_entity: QualifiedEntityKey {
                entry_id: String::new(),
                entity: String::new(),
            },
            result_shape: crate::plasm_plan::ResultShape::Single,
            row_cardinality: RowCardinalityProof::StaticSingleton,
            continuation: ContinuationCapability::RenderContentScalar,
            anchor: ContinuationAnchor::None,
        },
        DagNodeSource::Compute { schema, .. } => synthetic_terminal_contract(label, schema),
        DagNodeSource::Data(value) => {
            let singleton = matches!(
                value,
                PlanValue::Literal { value }
                    if value.as_array().is_none_or(|items| items.len() <= 1)
            );
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: QualifiedEntityKey {
                    entry_id: String::new(),
                    entity: String::new(),
                },
                result_shape: if singleton {
                    crate::plasm_plan::ResultShape::Single
                } else {
                    crate::plasm_plan::ResultShape::List
                },
                row_cardinality: if singleton {
                    RowCardinalityProof::StaticSingleton
                } else {
                    RowCardinalityProof::StaticPlural
                },
                continuation: ContinuationCapability::Terminal,
                anchor: ContinuationAnchor::None,
            }
        }
        DagNodeSource::Derive { .. } | DagNodeSource::ForEach { .. } => ProgramBindingContract {
            label: label.to_string(),
            row_entity: QualifiedEntityKey {
                entry_id: String::new(),
                entity: String::new(),
            },
            result_shape: crate::plasm_plan::ResultShape::Single,
            row_cardinality: RowCardinalityProof::RuntimeChecked,
            continuation: ContinuationCapability::Terminal,
            anchor: ContinuationAnchor::None,
        },
    }
}

fn synthetic_row_contract(label: &str, schema: &SyntheticResultSchema) -> ProgramBindingContract {
    ProgramBindingContract {
        label: label.to_string(),
        row_entity: QualifiedEntityKey {
            entry_id: String::new(),
            entity: schema.entity.clone().unwrap_or_default(),
        },
        result_shape: crate::plasm_plan::ResultShape::List,
        row_cardinality: RowCardinalityProof::RuntimeChecked,
        continuation: ContinuationCapability::PostfixOnly,
        anchor: ContinuationAnchor::None,
    }
}

fn synthetic_terminal_contract(
    label: &str,
    schema: &SyntheticResultSchema,
) -> ProgramBindingContract {
    ProgramBindingContract {
        label: label.to_string(),
        row_entity: QualifiedEntityKey {
            entry_id: String::new(),
            entity: schema.entity.clone().unwrap_or_default(),
        },
        result_shape: crate::plasm_plan::ResultShape::Single,
        row_cardinality: RowCardinalityProof::RuntimeChecked,
        continuation: ContinuationCapability::Terminal,
        anchor: ContinuationAnchor::None,
    }
}

fn resolve_cgs_for_qualified_entity<'a>(
    session: &'a ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Option<&'a plasm_core::CGS> {
    session
        .contexts_by_entry
        .get(&qe.entry_id)
        .map(|ctx| ctx.cgs.as_ref())
        .filter(|cgs| cgs.entities.contains_key(qe.entity.as_str()))
        .or_else(|| {
            if session.entry_id == qe.entry_id
                && session.cgs.entities.contains_key(qe.entity.as_str())
            {
                Some(session.cgs.as_ref())
            } else {
                None
            }
        })
}

fn relation_segment_context<'a>(
    map: &'a plasm_core::SymbolMap,
    qe: &'a QualifiedEntityKey,
    ent: &'a plasm_core::EntityDef,
    binding_label: Option<plasm_core::ProgramBindingLabel<'a>>,
    allow_lhs_coercion: bool,
) -> plasm_core::RelationSegmentContext<'a> {
    plasm_core::RelationSegmentContext {
        map,
        entity: qe.entity.as_str(),
        relations: &ent.relations,
        binding_label,
        allow_lhs_coercion,
    }
}

fn resolve_relation_wire_on_entity(
    session: &ExecuteSession,
    cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    segment: &str,
    binding_label: Option<plasm_core::ProgramBindingLabel<'_>>,
) -> Option<String> {
    let cgs = resolve_cgs_for_qualified_entity(session, qe)?;
    let ent = cgs.get_entity(qe.entity.as_str())?;
    let map = symbol_map_for_plasm_surface_parse(session, cross_cache);
    let ctx = relation_segment_context(map.as_ref(), qe, ent, binding_label, true);
    match plasm_core::resolve_relation_segment(&ctx, segment) {
        plasm_core::RelationSegmentOutcome::Wire(w) => Some(w),
        _ => None,
    }
}

fn resolve_relation_segment_for_continuation(
    session: &ExecuteSession,
    cross_cache: Option<&SymbolMapCrossRequestCache>,
    row_qe: &QualifiedEntityKey,
    segment: &str,
    binding_label: Option<plasm_core::ProgramBindingLabel<'_>>,
) -> Result<String, String> {
    let cgs = resolve_cgs_for_qualified_entity(session, row_qe).ok_or_else(|| {
        format!(
            "unknown catalog entity `{}` for relation continuation",
            row_qe.entity
        )
    })?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation continuation",
            row_qe.entity
        )
    })?;
    let map = symbol_map_for_plasm_surface_parse(session, cross_cache);
    let ctx = relation_segment_context(map.as_ref(), row_qe, ent, binding_label, true);
    match plasm_core::resolve_relation_segment(&ctx, segment) {
        plasm_core::RelationSegmentOutcome::Wire(w) => Ok(w),
        plasm_core::RelationSegmentOutcome::WrongRole { sym, wire } => Err(
            plasm_core::relation_segment_wrong_role_message(&sym, &wire, row_qe.entity.as_str()),
        ),
        plasm_core::RelationSegmentOutcome::NotFound => Err(format!(
            "entity `{}` has no relation `{segment}`",
            row_qe.entity
        )),
    }
}

fn relation_continuation_expr_from_source_row_hole(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    relation_wire: &str,
) -> Result<Expr, String> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation continuation",
            row_qe.entity
        )
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        format!(
            "entity `{}` has no relation `{relation_wire}` for row-hole continuation",
            row_qe.entity
        )
    })?;
    let _target_ent = cgs
        .get_entity(rel.target_resource.as_str())
        .ok_or_else(|| {
            format!(
                "relation `{relation_wire}` on `{}` targets unknown entity `{}`",
                row_qe.entity, rel.target_resource
            )
        })?;
    let _target_qe = if cgs.entities.contains_key(rel.target_resource.as_str()) {
        QualifiedEntityKey {
            entry_id: row_qe.entry_id.clone(),
            entity: rel.target_resource.to_string(),
        }
    } else {
        crate::catalog_ownership::resolve_qualified_entity_key(
            session,
            rel.target_resource.as_str(),
            Some(cgs),
        )?
    };
    let source_get = {
        let mut get = if ent.key_vars.is_empty() {
            let path_key = ent.id_field.as_str().to_string();
            let hole = Value::PlasmInputRef(PlasmInputRef::NodeInput {
                node: "source".into(),
                path: vec![path_key.clone()],
            });
            GetExpr::from_ref_with_path_vars(
                Ref::new(row_qe.entity.as_str(), ""),
                Some(indexmap::IndexMap::from([(path_key, hole)])),
            )
        } else {
            let mut path_vars = indexmap::IndexMap::new();
            for key in &ent.key_vars {
                path_vars.insert(
                    key.as_str().to_string(),
                    Value::PlasmInputRef(PlasmInputRef::NodeInput {
                        node: "source".into(),
                        path: vec![key.as_str().to_string()],
                    }),
                );
            }
            GetExpr::from_ref_with_path_vars(
                Ref {
                    entity_type: row_qe.entity.as_str().into(),
                    key: EntityKey::Compound(BTreeMap::new()),
                },
                Some(path_vars),
            )
        };
        get.catalog_entry_id = Some(row_qe.entry_id.clone());
        Expr::Get(get)
    };
    Ok(Expr::Chain(ChainExpr::auto_get(
        source_get,
        relation_wire.to_string(),
    )))
}

fn is_row_producing_relation_source(state: &CompileState<'_>, label: &str) -> bool {
    match state.get(label).map(|n| &n.source) {
        Some(DagNodeSource::RelationTraversal { .. }) => true,
        Some(DagNodeSource::Surface { kind, parsed, .. }) => {
            matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_))
        }
        Some(DagNodeSource::Compute {
            op: ComputeOp::Limit { .. } | ComputeOp::Project { .. },
            source,
            ..
        }) => is_row_producing_relation_source(state, source),
        _ => false,
    }
}

fn relation_sourced_continuation_eligible(state: &CompileState<'_>, label: &str) -> bool {
    match state.get(label).map(|n| &n.source) {
        Some(DagNodeSource::RelationTraversal { .. }) => true,
        Some(DagNodeSource::Compute {
            op: ComputeOp::Limit { .. } | ComputeOp::Project { .. },
            source,
            ..
        }) => is_row_producing_relation_source(state, source),
        _ => false,
    }
}

fn relation_uses_from_parent_get(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    segment: &str,
) -> bool {
    let Ok(cgs) = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    ) else {
        return false;
    };
    let Some(ent) = cgs.get_entity(row_qe.entity.as_str()) else {
        return false;
    };
    let Some(wire) = resolve_relation_wire_on_entity(session, None, row_qe, segment, None) else {
        return false;
    };
    ent.relations
        .get(wire.as_str())
        .and_then(|r| r.materialize.as_ref())
        .is_some_and(|m| {
            matches!(
                m,
                plasm_core::RelationMaterialization::FromParentGet { .. }
                    | plasm_core::RelationMaterialization::PreferFromParentGet { .. }
            )
        })
}

fn prefer_row_hole_relation_continuation(
    state: &CompileState<'_>,
    contract: &ProgramBindingContract,
    segment: &str,
    session: &ExecuteSession,
) -> bool {
    if matches!(contract.anchor, ContinuationAnchor::BindingLabel) {
        return true;
    }
    // Federated sessions: never re-parse RootSurface anchors — row-hole preserves
    // `row_entity.catalog_entry_id` for homonymous entity/relation targets.
    if session.contexts_by_entry.len() > 1 {
        return true;
    }
    // Relation-sourced rows (`species = pikachu.species`) must continue via materialized
    // parent inputs, not anchor re-parse (`pikachu.species.evolution_chain`) which loses
    // catalog context and may target the wrong entity in federated sessions.
    if relation_sourced_continuation_eligible(state, &contract.label) {
        return true;
    }
    if contract.anchor.allows_text_parse() {
        return false;
    }
    relation_uses_from_parent_get(session, &contract.row_entity, segment)
}

fn parse_relation_continuation_expr(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    contract: &ProgramBindingContract,
    segment: &str,
) -> Result<plasm_core::expr_parser::ParsedExpr, String> {
    let relation_wire = resolve_relation_segment_for_continuation(
        session,
        state.cross_cache,
        &contract.row_entity,
        segment,
        Some(plasm_core::ProgramBindingLabel(contract.label.as_str())),
    )?;
    if prefer_row_hole_relation_continuation(state, contract, segment, session) {
        return Ok(plasm_core::expr_parser::ParsedExpr {
            expr: relation_continuation_expr_from_source_row_hole(
                session,
                &contract.row_entity,
                &relation_wire,
            )?,
            projection: None,
        });
    }
    let refs = state.program_node_id_set();
    let try_expanded_chain = |expanded: &str| -> Option<plasm_core::expr_parser::ParsedExpr> {
        let parsed = parse_plasm_surface_line_program(
            session,
            state.cross_cache,
            state.pipeline,
            expanded,
            Some(&refs),
            false,
        )
        .ok()?;
        matches!(parsed.expr, Expr::Chain(_)).then_some(parsed)
    };
    let try_label_row_chain = |expanded: &str| -> Option<plasm_core::expr_parser::ParsedExpr> {
        let parsed = try_expanded_chain(expanded)?;
        if let Expr::Chain(ref chain) = parsed.expr {
            if chain.source.primary_entity() == contract.row_entity.entity.as_str() {
                return Some(parsed);
            }
        }
        None
    };
    if contract.anchor.allows_text_parse() {
        if let Some(expanded) = contract.continuation_text_expansion(&relation_wire) {
            if let Some(parsed) = try_expanded_chain(&expanded) {
                return Ok(parsed);
            }
        }
        if relation_wire != segment {
            if let Some(expanded) = contract.continuation_text_expansion(segment) {
                if let Some(parsed) = try_expanded_chain(&expanded) {
                    return Ok(parsed);
                }
            }
        }
    }
    if matches!(contract.anchor, ContinuationAnchor::BindingLabel) {
        if let Some(expanded) = contract.continuation_text_expansion(&relation_wire) {
            if let Some(parsed) = try_label_row_chain(&expanded) {
                if let Expr::Chain(ref chain) = parsed.expr {
                    if matches!(chain.source.as_ref(), Expr::Get(_)) {
                        return Ok(parsed);
                    }
                }
            }
        }
    }
    Ok(plasm_core::expr_parser::ParsedExpr {
        expr: relation_continuation_expr_from_source_row_hole(
            session,
            &contract.row_entity,
            &relation_wire,
        )?,
        projection: None,
    })
}

/// Output shape of a relation traversal under the cardinality lattice
/// (`docs/plasm-language-definition.md`): the result is a single row only when the
/// relation is one-cardinality **and** the source is a singleton; a one-cardinality hop
/// over a plural source is a 1:1 flat-map and therefore a list.
fn relation_result_shape(
    rel_cardinality: RelationCardinality,
    source_card: RelationSourceCardinality,
) -> crate::plasm_plan::ResultShape {
    match (rel_cardinality, source_card) {
        (RelationCardinality::Many, _) => crate::plasm_plan::ResultShape::List,
        (RelationCardinality::One, RelationSourceCardinality::Many) => {
            crate::plasm_plan::ResultShape::List
        }
        (RelationCardinality::One, _) => crate::plasm_plan::ResultShape::Single,
    }
}

fn lower_relation_continuation(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    source_label: &str,
    tail: &str,
) -> Result<DagNode, String> {
    let segment = tail.split('.').next().unwrap_or(tail).trim();
    if segment.is_empty() || tail.contains('.') {
        return Err(format!(
            "Plasm program `{id}`: `{source_label}.{tail}` — node-ref continuation supports a single CGS relation segment only"
        ));
    }
    let contract = binding_contract(state, source_label).ok_or_else(|| {
        format!("Plasm program `{id}`: unknown binding `{source_label}` for relation continuation")
    })?;
    if !contract.anchor.is_present() {
        return Err(format!(
            "Plasm program `{id}`: `{source_label}.{segment}` requires a continuation anchor on `{source_label}` — bind an intermediate row before continuing the relation chain"
        ));
    }
    let parsed = parse_relation_continuation_expr(session, state, &contract, segment)?;
    let Expr::Chain(ref chain) = parsed.expr else {
        return Err(format!(
            "Plasm program `{id}`: `{source_label}.{segment}` did not lower to a relation chain"
        ));
    };
    let Some(wire) = resolve_relation_wire_on_entity(
        session,
        state.cross_cache,
        &contract.row_entity,
        segment,
        Some(plasm_core::ProgramBindingLabel(id)),
    ) else {
        return Err(format!(
            "Plasm program `{id}`: entity `{}` has no relation `{segment}`",
            contract.row_entity.entity
        ));
    };
    if chain.selector.as_str() != wire.as_str() {
        return Err(format!(
            "Plasm program `{id}`: relation wire mismatch for `{source_label}.{segment}`"
        ));
    }
    let (target_qe, rel_cardinality) = lookup_relation_chain_meta(
        session,
        state.cross_cache,
        chain,
        Some(&contract.row_entity),
    )?;
    let expanded = contract
        .continuation_text_expansion(segment)
        .unwrap_or_else(|| format!("{source_label}.{segment}"));
    let source_card = contract.relation_source_cardinality();
    let result_shape = relation_result_shape(rel_cardinality, source_card);
    let ir = PlanExprIr {
        expr: serde_json::to_value(&parsed.expr).map_err(|e| e.to_string())?,
        projection: parsed.projection.clone(),
        display_expr: Some(expr.to_string()),
    };
    let binding_proofs =
        relation_binding_proofs_for_lower(session, &contract.row_entity, wire.as_str())
            .unwrap_or_default();
    let materialize = relation_materialize_for_lower(session, &contract.row_entity, wire.as_str())?;
    let plan_relation = PlanRelationTraversal {
        source: source_label.to_string(),
        relation: wire,
        target: target_qe.clone(),
        cardinality: rel_cardinality,
        source_cardinality: source_card,
        expr: expanded.clone(),
        ir: ir.clone(),
        binding_proofs,
        materialize: Some(materialize),
    };
    Ok(DagNode {
        id: id.to_string(),
        expr: expr.to_string(),
        singleton: false,
        page_size: None,
        source: DagNodeSource::RelationTraversal {
            source_label: source_label.to_string(),
            expanded_plasm: expanded,
            parsed,
            plan_relation,
            qualified_entity: target_qe,
            effect_class: EffectClass::Read,
            result_shape,
        },
    })
}

fn try_split_single_hop_surface_chain(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    expr: &str,
) -> Option<(String, String)> {
    let refs = state.program_node_id_set();
    let parsed = parse_plasm_surface_line_program(
        session,
        state.cross_cache,
        state.pipeline,
        expr,
        Some(&refs),
        false,
    )
    .ok()?;
    let Expr::Chain(chain) = parsed.expr else {
        return None;
    };
    if matches!(chain.source.as_ref(), Expr::Chain(_)) {
        return None;
    }
    let segment = chain.selector.clone();
    let trimmed = expr.trim();
    let suffix = format!(".{segment}");
    if trimmed.ends_with(&suffix) {
        let base_expr = trimmed[..trimmed.len() - suffix.len()].trim().to_string();
        if !base_expr.is_empty() {
            return Some((base_expr, segment));
        }
    }
    // Opaque relation symbols (e.g. `.r2`) may differ from wire names (e.g. `.pokemon`).
    let dot = trimmed.rfind('.')?;
    let base_expr = trimmed[..dot].trim().to_string();
    if base_expr.is_empty() {
        return None;
    }
    let base_parsed = parse_plasm_surface_line_program(
        session,
        state.cross_cache,
        state.pipeline,
        &base_expr,
        Some(&refs),
        false,
    )
    .ok()?;
    if base_parsed.expr == *chain.source {
        return Some((base_expr, segment));
    }
    None
}

fn plan_render_content_scalar_reference_err(id: &str, expr: &str, label: &str) -> String {
    format!(
        "Plasm program `{id}`: `{expr}` reads generated text from `{label}.content`. That path is a **scalar string** for `=>` derives and capability parameters only — not a final root and not a relation receiver. Return `{label}` if you want the generated text row, or use `{label}.content` only inside string/body/template/object payload positions."
    )
}

fn relation_materialize_for_lower(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    relation_wire: &str,
) -> Result<plasm_core::RelationMaterialization, String> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation materialize",
            row_qe.entity
        )
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        format!(
            "entity `{}` has no relation `{relation_wire}` for materialize",
            row_qe.entity
        )
    })?;
    Ok(rel
        .materialize
        .clone()
        .unwrap_or(plasm_core::RelationMaterialization::Unavailable))
}

fn relation_binding_proofs_for_lower(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    relation_wire: &str,
) -> Result<Vec<plasm_core::RelationBindingProof>, String> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation binding proofs",
            row_qe.entity
        )
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        format!(
            "entity `{}` has no relation `{relation_wire}` for binding proofs",
            row_qe.entity
        )
    })?;
    plasm_core::collect_relation_binding_proofs(cgs, ent, rel)
}

/// Resolve relation metadata for a parsed [`Expr::Chain`] (declared CGS relation on the source entity).
fn lookup_relation_chain_meta(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    chain: &plasm_core::ChainExpr,
    source_row_qe: Option<&QualifiedEntityKey>,
) -> Result<(QualifiedEntityKey, RelationCardinality), String> {
    let inferred_row_qe;
    let row_qe = if let Some(qe) = source_row_qe {
        Some(qe)
    } else if let Some(entry_id) = chain.source.session_catalog_entry_id() {
        inferred_row_qe = QualifiedEntityKey {
            entry_id: entry_id.to_string(),
            entity: chain.source.primary_entity().to_string(),
        };
        Some(&inferred_row_qe)
    } else {
        None
    };
    if session.contexts_by_entry.len() > 1 && row_qe.is_none() {
        return Err(
            "federated relation continuation requires catalog ownership from the source row (use session e# / binding continuation, not bare wire entity names)"
                .to_string(),
        );
    }
    let cgs = if let Some(row_qe) = row_qe {
        resolve_cgs_for_qualified_entity(session, row_qe).ok_or_else(|| {
            format!(
                "unknown catalog entity `{}` for entry `{}`",
                row_qe.entity, row_qe.entry_id
            )
        })?
    } else {
        let root_entity = chain.source.primary_entity();
        crate::catalog_ownership::resolve_cgs_for_entity(session, root_entity, None)?
    };
    let root_entity = chain.source.primary_entity();
    let source_entity = chain
        .source
        .relation_navigation_entity(cgs)
        .ok_or_else(|| {
            format!(
                "could not resolve relation navigation entity for chain continuing `{root_entity}`"
            )
        })?;
    let source_entity = source_entity.as_str();
    let ent = cgs.get_entity(source_entity).ok_or_else(|| {
        format!("unknown entity `{source_entity}` (Plasm program relation continuation)")
    })?;
    let rel = ent.relations.get(chain.selector.as_str()).ok_or_else(|| {
        let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
        let sym = row_qe.map(|qe| {
            map.ident_sym_relation_for(qe.entry_id.as_str(), source_entity, chain.selector.as_str())
        }).unwrap_or_else(|| {
            map.ident_sym_relation(source_entity, chain.selector.as_str())
        });
        let sym_note = if sym.as_str() != chain.selector.as_str() {
            format!(" Active teaching-table relation symbol for `{0}` on `{source_entity}` is `{sym}`.", chain.selector)
        } else {
            String::new()
        };
        format!(
            "entity `{source_entity}` has no relation `{}` — use a declared catalog relation wire name or the `.p#` navigation slot from the active TSV teaching rows for `{source_entity}`.{sym_note}",
            chain.selector
        )
    })?;
    let target_ent = rel.target_resource.as_str();
    if cgs.get_entity(target_ent).is_none() {
        return Err(format!(
            "relation `{}` on entity `{}` targets unknown entity `{}` in the resolved catalog — `target` must name an `entities:` key (see CGS load validation / domain.yaml); field projection after this chain cannot be typed",
            chain.selector, source_entity, target_ent
        ));
    }
    let qe = if let Some(row_qe) = row_qe {
        QualifiedEntityKey {
            entry_id: row_qe.entry_id.clone(),
            entity: target_ent.to_string(),
        }
    } else {
        crate::catalog_ownership::resolve_qualified_entity_key(session, target_ent, Some(cgs))?
    };
    let cardinality = match rel.cardinality {
        plasm_core::Cardinality::One => RelationCardinality::One,
        plasm_core::Cardinality::Many => RelationCardinality::Many,
    };
    Ok((qe, cardinality))
}

fn compile_surface_node(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
) -> Result<DagNode, String> {
    if let Some(mut nodes) = try_lower_row_suffix_expression(session, state, id, expr)? {
        return nodes.pop().ok_or_else(|| {
            format!("Plasm program `{id}`: postfix/relation chain `{expr}` produced no nodes")
        });
    }
    if let Some((label, tail)) = longest_matching_bound_prefix(expr, state) {
        let contract = binding_contract(state, &label).ok_or_else(|| {
            format!("Plasm program `{id}`: unknown binding `{label}` for continuation")
        })?;
        let tail_trim = tail.trim();
        if matches!(
            contract.continuation,
            ContinuationCapability::RenderContentScalar
        ) && (tail_trim == "content" || tail_trim.starts_with("content."))
        {
            return Err(plan_render_content_scalar_reference_err(id, expr, &label));
        }
        if matches!(contract.continuation, ContinuationCapability::Terminal) {
            return Err(format!(
                "Plasm program `{id}`: `{label}` is not a Plasm expression anchor — only surface/relation bindings and row-preserving projection bindings can be extended with `{label}.…`; aggregate/render/derive/data/for_each bindings must use postfix transforms or an explicit entity constructor"
            ));
        }
        if contract.supports_relation_dot() {
            let tail_trim = tail.trim();
            if !tail_trim.contains('.') {
                if looks_like_surface_postfix_tail(tail_trim) {
                    let synthetic = format!("{label}.{tail_trim}");
                    match peel_postfix_suffixes(&synthetic) {
                        Ok((primary, ops)) if primary.trim() == label && !ops.is_empty() => {
                            let mut nodes =
                                lower_row_expression(session, state, id, &synthetic, Some(id))?;
                            return nodes.pop().ok_or_else(|| {
                                format!("Plasm program `{id}`: postfix continuation `{synthetic}` produced no nodes")
                            });
                        }
                        Ok((_, ops)) if !ops.is_empty() => {
                            let mut nodes =
                                lower_row_expression(session, state, id, &synthetic, Some(id))?;
                            return nodes.pop().ok_or_else(|| {
                                format!("Plasm program `{id}`: postfix continuation `{synthetic}` produced no nodes")
                            });
                        }
                        _ => {
                            return Err(unknown_row_transform_error(id, tail_trim));
                        }
                    }
                }
                if relation_sourced_continuation_eligible(state, &label)
                    || matches!(contract.anchor, ContinuationAnchor::BindingLabel)
                    || contract.anchor.allows_text_parse()
                {
                    return lower_relation_continuation(
                        session, state, id, expr, &label, tail_trim,
                    );
                }
            } else if contract.anchor.allows_text_parse() {
                let expanded = contract
                    .continuation_text_expansion(tail_trim)
                    .ok_or_else(|| {
                        format!(
                            "Plasm program `{id}`: `{label}` has no continuation anchor for `{tail_trim}`"
                        )
                    })?;
                let refs = state.program_node_id_set();
                let parsed = parse_plasm_surface_line_program(
                    session,
                    state.cross_cache,
                    state.pipeline,
                    &expanded,
                    Some(&refs),
                    false,
                )
                .map_err(|e| {
                    format!(
                        "Plasm program `{id}` expression parse: {}\n(hint: `{label}.…` substitutes the Plasm bound to `{label}`; expanded form `{expanded}`)",
                        format_session_symbolic_parse_error(
                            session,
                            state.cross_cache,
                            state.pipeline,
                            &expanded,
                            &e,
                        ),
                        expanded = expanded
                    )
                })?;
                if let Expr::Chain(ref chain) = parsed.expr {
                    let (target_qe, rel_cardinality) = lookup_relation_chain_meta(
                        session,
                        state.cross_cache,
                        chain,
                        Some(&contract.row_entity),
                    )?;
                    let source_card = contract.relation_source_cardinality();
                    let result_shape = relation_result_shape(rel_cardinality, source_card);
                    let ir = PlanExprIr {
                        expr: serde_json::to_value(&parsed.expr).map_err(|e| e.to_string())?,
                        projection: parsed.projection.clone(),
                        display_expr: Some(expr.to_string()),
                    };
                    let binding_proofs = relation_binding_proofs_for_lower(
                        session,
                        &contract.row_entity,
                        chain.selector.as_str(),
                    )
                    .unwrap_or_default();
                    let materialize = relation_materialize_for_lower(
                        session,
                        &contract.row_entity,
                        chain.selector.as_str(),
                    )?;
                    let plan_relation = PlanRelationTraversal {
                        source: label.clone(),
                        relation: chain.selector.clone(),
                        target: target_qe.clone(),
                        cardinality: rel_cardinality,
                        source_cardinality: source_card,
                        expr: expanded.clone(),
                        ir: ir.clone(),
                        binding_proofs,
                        materialize: Some(materialize),
                    };
                    return Ok(DagNode {
                        id: id.to_string(),
                        expr: expr.to_string(),
                        singleton: false,
                        page_size: None,
                        source: DagNodeSource::RelationTraversal {
                            source_label: label,
                            expanded_plasm: expanded,
                            parsed,
                            plan_relation,
                            qualified_entity: target_qe,
                            effect_class: EffectClass::Read,
                            result_shape,
                        },
                    });
                }
                return Err(format!(
                    "Plasm program `{id}`: `{label}.…` expands to a non-relation Plasm expression; node-ref continuation currently supports CGS relation chains (`label.<relation>`) only"
                ));
            }
        }
        return Err(format!(
            "Plasm program `{id}`: `{label}` cannot be extended with `{tail}` — use postfix transforms on the binding expression or a CGS relation chain (`label.<relation>`)"
        ));
    }
    let refs = state.program_node_id_set();
    let parsed = parse_plasm_surface_line_program(
        session,
        state.cross_cache,
        state.pipeline,
        expr,
        Some(&refs),
        false,
    )
    .map_err(|e| {
        format!(
            "Plasm program `{id}` expression parse: {}",
            format_session_symbolic_parse_error(
                session,
                state.cross_cache,
                state.pipeline,
                expr,
                &e,
            )
        )
    })?;
    let uses = collect_template_uses_from_expr(&parsed.expr);
    let (kind, qualified_entity, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    let node = DagNode {
        id: id.to_string(),
        expr: expr.to_string(),
        singleton: matches!(parsed.expr, Expr::Get(_)),
        page_size: None,
        source: DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            effect_class,
            result_shape,
            uses_result: uses,
        },
    };
    validate_surface_inline_projection(session, state, &node)?;
    Ok(node)
}

fn node_to_json(node: &DagNode) -> Result<serde_json::Value, String> {
    match &node.source {
        DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            effect_class,
            result_shape,
            uses_result,
        } => {
            let ir = if uses_result.is_empty() {
                json!({
                    "expr": parsed.expr,
                    "projection": parsed.projection,
                    "display_expr": node.expr,
                })
            } else {
                expr_template_json(parsed, uses_result, node.expr.as_str())?
            };
            let mut obj = json!({
                "id": node.id,
                "kind": kind,
                "expr": node.expr,
                "effect_class": effect_class,
                "result_shape": result_shape,
                "projection": parsed.projection.clone().unwrap_or_default(),
                "predicates": [],
                "depends_on": uses_result.iter().filter_map(|u| u.get("node").and_then(|v| v.as_str()).map(str::to_string)).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
                "uses_result": uses_result,
            });
            if matches!(result_shape, crate::plasm_plan::ResultShape::Page) {
                obj["qualified_entity"] = serde_json::Value::Null;
            } else {
                obj["qualified_entity"] = json!(qualified_entity);
            }
            if uses_result.is_empty() {
                obj["ir"] = ir;
            } else {
                obj["ir_template"] = ir;
            }
            if let Some(n) = node.page_size {
                obj["page_size"] = json!(n);
            }
            Ok(obj)
        }
        DagNodeSource::RelationTraversal {
            source_label,
            parsed,
            plan_relation,
            qualified_entity,
            effect_class,
            result_shape,
            ..
        } => {
            let mut obj = json!({
                "id": node.id,
                "kind": PlanNodeKind::Relation,
                "qualified_entity": qualified_entity,
                "effect_class": effect_class,
                "result_shape": result_shape,
                "projection": parsed.projection.clone().unwrap_or_default(),
                "predicates": [],
                "relation": plan_relation,
                "depends_on": [source_label],
                "uses_result": relation_plan_uses_result(source_label, parsed),
            });
            if let Some(n) = node.page_size {
                obj["page_size"] = json!(n);
            }
            Ok(obj)
        }
        DagNodeSource::Data(value) => Ok(json!({
            "id": node.id,
            "kind": "data",
            "effect_class": "artifact_read",
            "result_shape": "artifact",
            "data": value,
            "depends_on": [],
            "uses_result": [],
        })),
        DagNodeSource::Compute { source, op, schema } => Ok(json!({
            "id": node.id,
            "kind": "compute",
            "effect_class": "artifact_read",
            "result_shape": if matches!(op, ComputeOp::Render { .. }) { "single" } else { "list" },
            "compute": {
                "source": source,
                "op": op,
                "schema": schema,
                "page_size": node.page_size,
            },
            "depends_on": [source],
            "uses_result": [{ "node": source, "as": "source" }],
        })),
        DagNodeSource::Derive {
            source,
            value,
            inputs,
        } => {
            let mut depends = vec![source.clone()];
            for input in inputs {
                if let Some(n) = input.get("node").and_then(|v| v.as_str()) {
                    if !depends.iter().any(|d| d == n) {
                        depends.push(n.to_string());
                    }
                }
            }
            Ok(json!({
                "id": node.id,
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "depends_on": depends,
                "uses_result": std::iter::once(json!({ "node": source, "as": "_" })).chain(inputs.iter().map(|input| {
                    json!({
                        "node": input.get("node").and_then(|v| v.as_str()).unwrap_or_default(),
                        "as": input.get("alias").and_then(|v| v.as_str()).unwrap_or_default(),
                    })
                })).collect::<Vec<_>>(),
                "derive_template": {
                    "kind": "map",
                    "source": source,
                    "item_binding": "_",
                    "inputs": inputs,
                    "value": value,
                }
            }))
        }
        DagNodeSource::ForEach {
            source,
            parsed_template,
            display_expr,
            effect_kind,
            qualified_entity,
            uses_result,
        } => {
            let mut depends = vec![source.clone()];
            for input in uses_result {
                if let Some(n) = input.get("node").and_then(|v| v.as_str()) {
                    if !depends.iter().any(|d| d == n) {
                        depends.push(n.to_string());
                    }
                }
            }
            Ok(json!({
                "id": node.id,
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": source,
                "item_binding": "_",
                "depends_on": depends,
                "uses_result": std::iter::once(json!({ "node": source, "as": "_" })).chain(uses_result.iter().cloned()).collect::<Vec<_>>(),
                "effect_template": {
                    "kind": effect_kind,
                    "qualified_entity": qualified_entity,
                    "expr_template": display_expr,
                    "ir_template": parsed_template,
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "projection": [],
                    "input_bindings": [],
                }
            }))
        }
    }
}

fn parse_dedupe_key_paths(
    session: &ExecuteSession,
    cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    keys: &str,
) -> Result<Vec<FieldPath>, String> {
    let trimmed = keys.trim();
    if trimmed.is_empty() {
        return Err("dedupe(...) requires at least one key field".into());
    }
    parse_field_list(session, cross_cache, qe, trimmed)?
        .into_iter()
        .map(|field| FieldPath::from_dotted(&field))
        .collect::<Result<Vec<_>, _>>()
}

fn looks_like_surface_postfix_tail(tail: &str) -> bool {
    let t = tail.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with('[') {
        return true;
    }
    if !(t.contains('(') || t.contains('{')) {
        return false;
    }
    let head = t
        .split('(')
        .next()
        .unwrap_or(t)
        .split('{')
        .next()
        .unwrap_or(t);
    let name = head.trim().trim_start_matches('.');
    is_known_postfix_method(name)
}

fn is_known_postfix_method(name: &str) -> bool {
    matches!(
        name,
        "limit"
            | "page_size"
            | "sort"
            | "filter"
            | "aggregate"
            | "group_by"
            | "dedupe"
            | "distinct"
            | "singleton"
    )
}

fn unknown_row_transform_error(id: &str, tail: &str) -> String {
    format!(
        "Plasm program `{id}`: unknown row transform `{tail}`; allowed postfix: .limit(N), .page_size(N), .sort(field[, dir]), .filter{{…}}, .filter(…), .aggregate(specs), .group_by(field, specs), .dedupe(field[, …]), .distinct(field[, …]), .distinct(), .singleton(), [fields]"
    )
}

fn split_return_list(
    line: &str,
    state: &mut CompileState<'_>,
    session: &ExecuteSession,
) -> Result<Vec<String>, String> {
    let mut roots = Vec::new();
    for part in split_top_level(line, ',')? {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let part =
            rewrite_binding_field_projection_root(part, state).unwrap_or_else(|| part.to_string());
        if state.contains(part.as_str()) {
            roots.push(part);
        } else {
            let id = format!("return_{}", roots.len() + 1);
            for node in compile_node_expr(session, state, &id, part.as_str())? {
                state.insert(node)?;
            }
            roots.push(id);
        }
    }
    Ok(roots)
}

/// When a final root looks like `binding(p10, p9)` and `binding` is a program label, rewrite to
/// `binding[p10, p9]` (canonical projection postfix).
fn rewrite_binding_field_projection_root(part: &str, state: &CompileState<'_>) -> Option<String> {
    let open = part.find('(')?;
    if open == 0 {
        return None;
    }
    let label = part[..open].trim();
    if !state.contains(label) {
        return None;
    }
    let close = part.rfind(')')?;
    if close <= open {
        return None;
    }
    let tail = part[open + 1..close].trim();
    if tail.is_empty() {
        return None;
    }
    if part[close + 1..].trim().is_empty() {
        Some(format!("{label}[{tail}]"))
    } else {
        None
    }
}

fn require_node(state: &CompileState<'_>, node: &str) -> Result<(), String> {
    if state.contains(node) {
        Ok(())
    } else {
        Err(format!("unknown Plasm program node `{node}`"))
    }
}

fn parse_field_list(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    fields: &str,
) -> Result<Vec<String>, String> {
    let out = split_top_level(fields, ',')?
        .into_iter()
        .map(|s| {
            let t = s.trim();
            plasm_core::expr_parser::normalize_nested_projection_field(t)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|s| {
            crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                qe,
                s.as_str(),
            )
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if out.is_empty() {
        return Err("field list must be non-empty".to_string());
    }
    Ok(out)
}

/// Parses one comma-separated aggregate specification after `.aggregate(...)` / `group_by` tail.
///
/// Canonical form: `output=count` or `output=sum(field)` (also `avg`/`min`/`max`).
///
/// **Shadow (repair-only, not taught):** bare `count` and `aggregate(count)` canonicalize to
/// `count=count` with synthetic output name `count`.
fn parse_one_aggregate_spec(raw: &str) -> Result<crate::plasm_plan::AggregateSpec, String> {
    let raw = raw.trim();
    if let Some((name, rhs)) = raw.split_once('=') {
        let name = OutputName::new(name.trim().to_string())?;
        let rhs = rhs.trim();
        if rhs == "count" {
            return Ok(crate::plasm_plan::AggregateSpec {
                name,
                function: crate::plasm_plan::AggregateFunction::Count,
                field: None,
            });
        }
        let open = rhs.find('(').ok_or_else(|| {
            format!(
                "right-hand side `{rhs}` in `{raw}` must be `count` or `func(field)` (e.g. `sum(amount)`)"
            )
        })?;
        let func = &rhs[..open];
        let field = rhs[open + 1..]
            .strip_suffix(')')
            .ok_or_else(|| format!("aggregate call `{rhs}` must end with `)`"))?;
        let function = match func {
            "sum" => AggregateFunction::Sum,
            "avg" => AggregateFunction::Avg,
            "min" => AggregateFunction::Min,
            "max" => AggregateFunction::Max,
            "first" => AggregateFunction::First,
            "last" => AggregateFunction::Last,
            other => return Err(format!("unknown aggregate function `{other}`")),
        };
        return Ok(crate::plasm_plan::AggregateSpec {
            name,
            function,
            field: Some(FieldPath::from_dotted(field.trim())?),
        });
    }

    // Shadow count-only forms → canonical `count=count`.
    if raw.eq_ignore_ascii_case("count") {
        return Ok(crate::plasm_plan::AggregateSpec {
            name: OutputName::new("count".to_string())?,
            function: crate::plasm_plan::AggregateFunction::Count,
            field: None,
        });
    }
    if let Some(inner) = raw
        .strip_prefix("aggregate(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner = inner.trim();
        if inner.eq_ignore_ascii_case("count") {
            return Ok(crate::plasm_plan::AggregateSpec {
                name: OutputName::new("count".to_string())?,
                function: crate::plasm_plan::AggregateFunction::Count,
                field: None,
            });
        }
        if inner.contains('(') {
            return Err(format!(
                "aggregate spec `{raw}` must name the output explicitly; use e.g. `total={inner}` (not `{raw}` without `output=`)"
            ));
        }
    }

    if raw.contains('(') {
        return Err(format!(
            "aggregate spec `{raw}` must use an explicit output name, e.g. `total=sum(amount)` or `n=count`"
        ));
    }

    Err(format!(
        "aggregate spec `{raw}` must be `output=count` or `output=sum(field)`…; bare `count` and `aggregate(count)` are accepted as shorthand for `count=count`"
    ))
}

fn parse_aggregates(args: &str) -> Result<Vec<crate::plasm_plan::AggregateSpec>, String> {
    split_top_level(args, ',')?
        .into_iter()
        .map(parse_one_aggregate_spec)
        .collect()
}

fn parse_sort_direction_token(direction: &str) -> Result<bool, String> {
    let d = direction.trim();
    if d.is_empty() {
        return Err("sort(...) direction must not be empty when a comma is present".to_string());
    }
    match d.to_ascii_lowercase().as_str() {
        "desc" | "descending" => Ok(true),
        "asc" | "ascending" => Ok(false),
        other => Err(format!(
            "sort(...) unknown direction `{other}`; use `desc` / `descending` for descending, omit the direction or use `asc` / `ascending` for ascending"
        )),
    }
}

/// Parse `.sort(...)` args: `field`, `field, desc`, or whitespace sugar `field desc`.
fn parse_sort_field_and_direction(args: &str) -> Result<(String, bool), String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err("sort(...) requires a field".to_string());
    }
    let parts = split_top_level(trimmed, ',')?;
    match parts.len() {
        0 => Err("sort(...) requires a field".to_string()),
        1 => {
            let single = parts[0].trim();
            if single.is_empty() {
                return Err("sort(...) requires a non-empty field".to_string());
            }
            if let Some((field, dir)) = single.rsplit_once(|c: char| c.is_ascii_whitespace()) {
                let field = field.trim();
                let dir = dir.trim();
                if !field.is_empty() && !dir.is_empty() {
                    if let Ok(descending) = parse_sort_direction_token(dir) {
                        return Ok((field.to_string(), descending));
                    }
                }
            }
            Ok((single.to_string(), false))
        }
        2 => {
            let key = parts[0].trim();
            if key.is_empty() {
                return Err("sort(...) requires a non-empty field".to_string());
            }
            let descending = parse_sort_direction_token(parts[1].trim())?;
            Ok((key.to_string(), descending))
        }
        _ => {
            Err("sort(...) expects at most `.sort(field)` or `.sort(field, direction)`".to_string())
        }
    }
}

fn parse_plan_value_expr(
    raw: &str,
    state: &CompileState<'_>,
    row_binding: Option<&str>,
) -> Result<(PlanValue, Vec<serde_json::Value>), String> {
    let raw = raw.trim();
    if raw.starts_with('{') && raw.ends_with('}') {
        let mut inputs = Vec::new();
        let mut fields = BTreeMap::new();
        for part in split_top_level(&raw[1..raw.len() - 1], ',')? {
            let (k, v) = part
                .split_once(':')
                .ok_or_else(|| format!("object field `{part}` must be key: value"))?;
            let (value, child_inputs) = parse_plan_value_expr(v, state, row_binding)?;
            inputs.extend(child_inputs);
            fields.insert(k.trim().to_string(), value);
        }
        return Ok((PlanValue::Object { fields }, dedupe_inputs(inputs)));
    }
    if raw.starts_with('[') && raw.ends_with(']') {
        let mut inputs = Vec::new();
        let mut items = Vec::new();
        for part in split_top_level(&raw[1..raw.len() - 1], ',')? {
            let (value, child_inputs) = parse_plan_value_expr(part, state, row_binding)?;
            inputs.extend(child_inputs);
            items.push(value);
        }
        return Ok((PlanValue::Array { items }, dedupe_inputs(inputs)));
    }
    if let Some(path) = raw.strip_prefix("_.") {
        return Ok((
            PlanValue::BindingSymbol {
                binding: row_binding.unwrap_or("_").to_string(),
                path: path.split('.').map(str::to_string).collect(),
            },
            Vec::new(),
        ));
    }
    if let Some((node, path)) = raw.split_once('.') {
        if let Some(dep) = state.get(node) {
            return Ok((
                PlanValue::NodeSymbol {
                    node: node.to_string(),
                    alias: node.to_string(),
                    path: path.split('.').map(str::to_string).collect(),
                },
                vec![json!({
                    "node": node,
                    "alias": node,
                    "cardinality": if dep.singleton { "auto" } else { "singleton" }
                })],
            ));
        }
    }
    if state.contains(raw) {
        let path = if row_binding.is_some() {
            vec!["content".to_string()]
        } else {
            Vec::new()
        };
        return Ok((
            PlanValue::NodeSymbol {
                node: raw.to_string(),
                alias: raw.to_string(),
                path,
            },
            vec![serde_json::json!({
                "node": raw,
                "alias": raw,
                "cardinality": "singleton"
            })],
        ));
    }
    if raw.starts_with("<<") {
        return Ok((
            PlanValue::Template {
                template: raw.to_string(),
                input_bindings: Vec::new(),
            },
            Vec::new(),
        ));
    }
    let value = parse_literal(raw)?;
    Ok((PlanValue::Literal { value }, Vec::new()))
}

/// `=>` derive maps accept `value_or_template` only — not surface `Entity(…)` calls.
fn reject_derive_map_surface_expr_literal(value: &PlanValue) -> Result<(), String> {
    let PlanValue::Literal { value } = value else {
        return Ok(());
    };
    let Some(s) = value.as_str() else {
        return Ok(());
    };
    let t = s.trim();
    if derive_rhs_literal_looks_like_surface_call(t) {
        return Err(format!(
            "`=>` derive map does not accept surface expressions ({t:?}); use `binding.relation` / `binding.p#` for relation hops, or `source => {{ … }}` for per-row maps"
        ));
    }
    Ok(())
}

fn derive_rhs_literal_looks_like_surface_call(s: &str) -> bool {
    if !s.contains('(') {
        return false;
    }
    let head = s.split('(').next().unwrap_or("").trim();
    if head.is_empty() {
        return false;
    }
    let mut chars = head.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first == 'e' && chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return true;
    }
    first.is_ascii_uppercase() && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_literal(raw: &str) -> Result<serde_json::Value, String> {
    if raw.starts_with('"') || raw == "null" || raw == "true" || raw == "false" {
        return serde_json::from_str(raw).map_err(|e| format!("literal `{raw}`: {e}"));
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Ok(json!(n));
    }
    if let Ok(n) = raw.parse::<f64>() {
        return Ok(json!(n));
    }
    Ok(json!(raw))
}

fn expr_template_json(
    parsed: &plasm_core::expr_parser::ParsedExpr,
    uses: &[serde_json::Value],
    display_expr: &str,
) -> Result<serde_json::Value, String> {
    let value = serde_json::to_value(&parsed.expr).map_err(|e| e.to_string())?;
    Ok(json!({
        "expr": value,
        "projection": parsed.projection,
        "display_expr": display_expr,
        "input_bindings": uses.iter().map(|u| {
            json!({
                "from": u.get("as").and_then(|v| v.as_str()).unwrap_or_default(),
                "to": u.get("as").and_then(|v| v.as_str()).unwrap_or_default(),
            })
        }).collect::<Vec<_>>()
    }))
}

fn dedupe_uses(uses: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut seen = BTreeSet::new();
    uses.into_iter()
        .filter(|u| {
            let key = format!(
                "{}:{}",
                u.get("node").and_then(|v| v.as_str()).unwrap_or_default(),
                u.get("as").and_then(|v| v.as_str()).unwrap_or_default()
            );
            seen.insert(key)
        })
        .collect()
}

fn dedupe_inputs(inputs: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut seen = BTreeSet::new();
    inputs
        .into_iter()
        .filter(|u| {
            let key = format!(
                "{}:{}",
                u.get("node").and_then(|v| v.as_str()).unwrap_or_default(),
                u.get("alias").and_then(|v| v.as_str()).unwrap_or_default()
            );
            seen.insert(key)
        })
        .collect()
}

fn infer_surface_contract(
    session: &ExecuteSession,
    expr: &Expr,
) -> Result<
    (
        PlanNodeKind,
        QualifiedEntityKey,
        EffectClass,
        crate::plasm_plan::ResultShape,
    ),
    String,
> {
    if let Expr::Chain(_) = expr {
        return Err(
            "internal: relation chains must be lowered before infer_surface_contract".to_string(),
        );
    }

    let (mut kind, entity, effect, shape) = infer_surface_contract_from_expr(expr)?;
    let qe = if let Some(qe) = expr.qualified_entity_key() {
        QualifiedEntityKey::from(qe)
    } else {
        let resolving_cgs =
            crate::catalog_ownership::resolve_cgs_for_entity(session, entity.as_str(), None)?;
        crate::catalog_ownership::resolve_qualified_entity_key(
            session,
            entity.as_str(),
            Some(resolving_cgs),
        )?
    };
    if let Expr::Query(q) = expr {
        if let Some(capability_name) = q.capability_name.as_ref() {
            let resolving_cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
                format!(
                    "catalog `{}` is not loaded for entity `{}`",
                    qe.entry_id, qe.entity
                )
            })?;
            if let Some(cap) = resolving_cgs.capabilities.get(capability_name.as_str()) {
                if cap.kind == plasm_core::CapabilityKind::Search {
                    kind = PlanNodeKind::Search;
                }
            }
        }
    }
    Ok((kind, qe, effect, shape))
}

fn infer_surface_contract_from_expr(
    expr: &Expr,
) -> Result<
    (
        PlanNodeKind,
        String,
        EffectClass,
        crate::plasm_plan::ResultShape,
    ),
    String,
> {
    match expr {
        Expr::TeachingValue { .. } => Err(
            "Expr::TeachingValue is teaching-table-only and cannot appear in execution plans"
                .to_string(),
        ),
        Expr::Query(q) => Ok((
            PlanNodeKind::Query,
            q.entity.as_str().to_string(),
            EffectClass::Read,
            crate::plasm_plan::ResultShape::List,
        )),
        Expr::Get(g) => Ok((
            PlanNodeKind::Get,
            g.reference.entity_type.as_str().to_string(),
            EffectClass::Read,
            crate::plasm_plan::ResultShape::Single,
        )),
        Expr::Create(c) => Ok((
            PlanNodeKind::Create,
            c.entity.as_str().to_string(),
            EffectClass::Write,
            crate::plasm_plan::ResultShape::MutationResult,
        )),
        Expr::Delete(d) => Ok((
            PlanNodeKind::Delete,
            d.target.entity_type.as_str().to_string(),
            EffectClass::Write,
            crate::plasm_plan::ResultShape::SideEffectAck,
        )),
        Expr::Invoke(i) => Ok((
            PlanNodeKind::Action,
            i.target.entity_type.as_str().to_string(),
            EffectClass::SideEffect,
            crate::plasm_plan::ResultShape::SideEffectAck,
        )),
        Expr::Chain(_) => unreachable!(
            "infer_surface_contract routes Expr::Chain before infer_surface_contract_from_expr"
        ),
        Expr::Page(_) => Ok((
            PlanNodeKind::Query,
            "__page__".to_string(),
            EffectClass::Read,
            crate::plasm_plan::ResultShape::Page,
        )),
        Expr::Wait(_) | Expr::Cancel(_) => Err(
            "`wait` / `cancel` are host operation continuations and cannot appear in compiled plan surfaces"
                .to_string(),
        ),
    }
}

fn schema_from_output_fields<'a>(
    entity: &str,
    fields: impl Iterator<Item = &'a OutputName>,
    kind: SyntheticValueKind,
) -> SyntheticResultSchema {
    SyntheticResultSchema {
        entity: Some(entity.to_string()),
        fields: fields
            .map(|name| SyntheticFieldSchema {
                name: name.clone(),
                value_kind: kind,
                source: None,
            })
            .collect(),
    }
}

fn schema_from_aggregates(
    entity: &str,
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> SyntheticResultSchema {
    SyntheticResultSchema {
        entity: Some(entity.to_string()),
        fields: aggregates
            .iter()
            .map(|agg| SyntheticFieldSchema {
                name: agg.name.clone(),
                value_kind: if agg.function == AggregateFunction::Count {
                    SyntheticValueKind::Integer
                } else {
                    SyntheticValueKind::Number
                },
                source: None,
            })
            .collect(),
    }
}

fn schema_from_group_by(
    entity: &str,
    keys: &[FieldPath],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> SyntheticResultSchema {
    let mut fields: Vec<SyntheticFieldSchema> = keys
        .iter()
        .filter_map(|k| {
            OutputName::new(k.dotted())
                .ok()
                .map(|name| SyntheticFieldSchema {
                    name,
                    value_kind: SyntheticValueKind::String,
                    source: None,
                })
        })
        .collect();
    fields.extend(aggregates.iter().map(|agg| SyntheticFieldSchema {
        name: agg.name.clone(),
        value_kind: if agg.function == AggregateFunction::Count {
            SyntheticValueKind::Integer
        } else {
            SyntheticValueKind::Number
        },
        source: None,
    }));
    SyntheticResultSchema {
        entity: Some(entity.to_string()),
        fields,
    }
}

/// Split `group_by` args into key field names (no `=`) and trailing aggregate tail.
fn parse_group_by_key_and_aggregate_tail(args: &str) -> Result<(Vec<String>, String), String> {
    let parts = split_top_level(args, ',')?;
    let mut keys = Vec::new();
    let mut agg_start = parts.len();
    for (i, part) in parts.iter().enumerate() {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        if t.contains('=') {
            agg_start = i;
            break;
        }
        keys.push(t.to_string());
    }
    if keys.is_empty() {
        return Err("group_by(...) requires at least one key field".into());
    }
    let agg_tail = if agg_start < parts.len() {
        parts[agg_start..].join(",")
    } else {
        String::new()
    };
    Ok((keys, agg_tail))
}

fn single_unknown_schema(entity: &str) -> SyntheticResultSchema {
    SyntheticResultSchema {
        entity: Some(entity.to_string()),
        fields: vec![SyntheticFieldSchema {
            name: OutputName::new("value".to_string()).expect("constant non-empty"),
            value_kind: SyntheticValueKind::Unknown,
            source: None,
        }],
    }
}

fn looks_like_data_literal(rhs: &str) -> bool {
    let t = rhs.trim_start();
    t.starts_with('{') || t.starts_with('[') || t.starts_with('"') || t.starts_with("<<")
}

fn looks_like_plasm_effect_template(rhs: &str) -> bool {
    // Distinguish for-each side effects from `source => { … }` derive. `.m#` (teaching-table methods) and
    // all readable verbs must register here—`.label(`, `.update(`, etc.—not just `.m`.
    rhs.contains(".m")
        || rhs.contains("=>")
        || rhs.contains(".update(")
        || rhs.contains(".create(")
        || rhs.contains(".delete(")
        || rhs.contains(".label(")
        || rhs.contains(".invoke(")
}

#[cfg(test)]
mod tests {
    //! DAG compile + dry-run **unit** tests (no HTTP).
    //!
    //! Prefer `cargo test -p plasm-e2e --test plasm_language_matrix` for **author-visible**
    //! “this program means X” semantics on the language-matrix fixture. Keep tests here for
    //! **compiler/plan invariants** (splitting, diagnostics, federation quirks, GitHub-shaped
    //! graphs). When a case overlaps the matrix, cite the matrix row id on the test (e.g.
    //! `lang_domain_symbol_page_size`).
    use super::*;
    use crate::plasm_plan_run::{
        evaluate_plasm_plan_dry, render_plasm_plan_dry_text, symbol_map_for_plasm_surface_parse,
    };
    use plasm_core::{load_schema, CgsContext, PromptPipelineConfig, TeachingExposureSession, CGS};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "langmatrix".into(),
            Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
        );
        let exp =
            TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem", "LangLine"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "langmatrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangLine".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn search_group_by_rejects_fields_outside_capability_provides() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-group-by-relation",
            r#"rows = LangItem~"probe"
bad = rows.group_by(summary)
bad"#,
        )
        .expect_err("search rows omit relation fields from provides");
        assert!(
            err.contains("not a row field of the upstream capability output")
                || err.contains("not a row field"),
            "{err}"
        );
        assert!(
            err.contains("row fields") || err.contains(" rows: "),
            "{err}"
        );
        assert!(
            !err.contains("projected columns"),
            "diagnostic must not steer agents toward wire column names: {err}"
        );
    }

    #[test]
    fn search_group_by_rejects_filter_input_param() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-group-by-filter-input",
            r#"rows = LangItem~"probe"{team_key="eng"}
bad = rows.group_by(q)
bad"#,
        )
        .expect_err("search text param q is not a row field");
        assert!(err.contains("is an input on langitem_search"), "{err}");
        assert!(err.contains("not a row field"), "{err}");
    }

    #[test]
    fn search_projection_rejects_filter_input_param() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-projection-filter-input",
            r#"rows = LangItem~"probe"{team_key="eng"}[q]
rows"#,
        )
        .expect_err("filter params are inputs not row fields for projection");
        assert!(err.contains("is an input on langitem_search"), "{err}");
    }

    #[test]
    fn search_group_by_accepts_filter_param_when_in_provides() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "search-group-by-filter-in-provides",
            r#"rows = LangItem~"probe"{team_key="eng"}
by_team = rows.group_by(team_key)
by_team"#,
        )
        .expect("team_key in provides should be a valid group_by key");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(!dry.node_results.is_empty());
    }

    #[test]
    fn derive_map_rejects_surface_entity_ctor() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-entity-ctor",
            r#"hits = LangItem
bad = hits => LangItem(id=_.id)
bad"#,
        )
        .expect_err("entity ctor on => must not compile as derive literal");
        assert!(
            err.contains("derive map does not accept surface expressions"),
            "{err}"
        );
    }

    #[test]
    fn derive_map_rejects_session_symbol_entity_ctor() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-reject-e1-ctor",
            r#"hits = LangItem
bad = hits => e1(p5=_.id)
bad"#,
        )
        .expect_err("e1(...) on => must not compile as derive literal");
        assert!(
            err.contains("derive map does not accept surface expressions"),
            "{err}"
        );
    }

    #[test]
    fn surface_relation_chain_with_postfix_compiles() {
        let session = test_session();
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "relation-chain-limit",
            r#"LangItem("i1").lines.limit(2)[id]"#,
        )
        .expect("direct relation chain with postfix should compile");
        let nodes = plan.get("nodes").and_then(|v| v.as_array()).expect("nodes");
        assert!(
            nodes.len() >= 2,
            "expected relation + compute nodes, got {nodes:?}"
        );
        assert!(
            !format!("{plan:?}").contains("internal: relation chains must be lowered"),
            "{plan:?}"
        );
    }

    /// Compile `program` and assert its post-`.limit` projection resolved against the relation
    /// **target** entity: compilation succeeds and the plan does not leak the receiver-only field
    /// `team_key`. Shared by the wire/opaque/`query_scoped` cases below.
    fn assert_relation_limit_projection_targets(
        session: &ExecuteSession,
        what: &str,
        program: &str,
    ) {
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            session,
            "relation-hop-limit-project",
            program,
        )
        .unwrap_or_else(|e| {
            panic!("{what}: projection after relation+limit must resolve against the relation target, got: {e}\nprogram:\n{program}")
        });
        assert!(
            !format!("{plan:?}").contains("team_key"),
            "{what}: plan leaked receiver-entity field `team_key`\nprogram:\n{program}"
        );
    }

    /// Regression: a projection after `relation-hop + .limit(...)` must resolve field tokens against
    /// the relation **target**, not the **receiver**. Covers `from_parent_get` and `query_scoped`
    /// materialize, wire names and opaque `e#`/`r#`/`p#`. Before the fix the limit-compute qe traced
    /// back to the receiver (`LangItem`) and could resolve a homograph to a receiver field
    /// (`team_key`). The projected `note`/`label` fields exist only on the relation targets
    /// (`LangLine`/`LangTag`), never on the `LangItem` receiver.
    #[test]
    fn relation_hop_limit_projection_resolves_against_target_entity() {
        let session = test_session();
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_item = map.entity_sym_for("langmatrix", "LangItem");
        let r_lines = map.ident_sym_relation_for("langmatrix", "LangItem", "lines");
        let p_note = map.ident_sym_entity_field_for("langmatrix", "LangLine", "note");
        assert_ne!(r_lines, "lines", "relation symbol not opaque: {r_lines}");
        assert_ne!(p_note, "note", "field symbol not opaque: {p_note}");
        let opaque =
            format!("item = {e_item}(\"i1\")\nlines = item.{r_lines}.limit(2)\nlines[{p_note}]");
        for (what, program) in [
            (
                "from_parent_get (wire)",
                "item = LangItem(\"i1\")\nlines = item.lines.limit(2)\nlines[note]",
            ),
            (
                "query_scoped (wire)",
                "item = LangItem(\"i1\")\ntags = item.tags.limit(2)\ntags[label]",
            ),
            ("from_parent_get (opaque e#/r#/p#)", opaque.as_str()),
        ] {
            assert_relation_limit_projection_targets(&session, what, program);
        }
    }

    /// Complement to the positive case: projecting a **receiver** field (`title`, a `LangItem` field)
    /// after the relation hop + limit must be **rejected** against the target row, with a diagnostic
    /// that names the missing row field rather than an unrelated receiver field.
    #[test]
    fn relation_hop_limit_then_separate_projection_resolves_target_entity() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "relation-hop-limit-split-project",
            r#"item = LangItem("i1")
lines = item.lines.limit(2)
bad = lines[title]
bad"#,
        )
        .expect_err("`title` is a LangItem field, not a LangLine row field — must be rejected against the target");
        assert!(
            err.contains("not a row field"),
            "expected target-entity rejection, got: {err}"
        );
        assert!(
            !err.contains("team_key"),
            "diagnostic must not surface unrelated receiver fields: {err}"
        );
    }

    /// Primary session `entry_id` is `github`, but `LangLine` was exposed from `linear` in teaching table
    /// — plan `qualified_entity` must use the owning catalog, not the lexicographic primary.
    #[test]
    fn federated_surface_qualified_entity_matches_exposure_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangLine"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangLine".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_linear = map.entity_sym_for("linear", "LangLine");
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            &format!(r#"{e_linear}("L1")"#),
        )
        .expect("compile");
        let qe = &plan["nodes"][0]["qualified_entity"];
        assert_eq!(qe["entry_id"], "linear", "{plan}");
        assert_eq!(qe["entity"], "LangLine");
    }

    /// Same wire entity name in two catalogs: session `e1` / `e2` must stamp `qualified_entity` per catalog.
    #[test]
    fn federated_duplicate_entity_name_e_symbol_stamps_catalog_in_plan() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        for (sym, entry_id) in [("e1", "github"), ("e2", "linear")] {
            let plan = compile_plasm_surface_line_to_plan(
                &PromptPipelineConfig::default(),
                None,
                &session,
                sym,
                sym,
            )
            .unwrap_or_else(|e| panic!("compile {sym}: {e}"));
            let qe = &plan["nodes"][0]["qualified_entity"];
            assert_eq!(qe["entry_id"], entry_id, "plan for {sym}");
            assert_eq!(qe["entity"], "LangItem");
        }
    }

    /// Federated primary is `linear` but relation target `LangDetail` resolves via owning CGS pointer, not primary `entry_id`.
    #[test]
    fn federated_relation_target_qe_from_owning_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs_primary = Arc::new(
            load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix")).expect("cgs"),
        );
        let cgs_secondary = Arc::new(
            load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix")).expect("cgs"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs_primary.clone())),
        );
        ctxs.insert(
            "pokeapi".into(),
            Arc::new(CgsContext::entry("pokeapi", cgs_secondary.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs_primary.as_ref(), cgs_secondary.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs_primary.as_ref(), "linear", &["LangLine"]);
        exp.expose_entities(&layers, cgs_secondary.clone(), "pokeapi", &["LangItem"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs_primary.clone(),
            ctxs,
            "linear".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangLine".into()],
            Some(exp),
            None,
            cgs_primary.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_poke = map.entity_sym_for("pokeapi", "LangItem");
        let source = format!(
            r#"item = {e_poke}("LI1")
summary = item.summary
summary"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-relation-target",
            &source,
        )
        .expect("compile");
        let summary = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "summary")
            .expect("summary node");
        assert_eq!(summary["kind"], "relation");
        assert_eq!(
            summary["relation"]["target"]["entry_id"], "pokeapi",
            "{summary}"
        );
        assert_eq!(summary["relation"]["target"]["entity"], "LangSummary");
        let ir = summary["relation"]["ir"]["expr"].to_string();
        assert!(
            !ir.contains(r#""$""#),
            "typed continuation IR must not use teaching placeholder: {ir}"
        );
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
        evaluate_plasm_plan_dry(&session, &plan).expect("federated relation dry-run");
    }

    /// Same wire entity in github+linear: relation hop from `e2` binding must target linear catalog.
    #[test]
    fn federated_duplicate_entity_relation_hop_preserves_source_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e2 = map.entity_sym_for("linear", "LangItem");
        let source = format!(
            r#"parent = {e2}("LI1")
kids = parent.children
kids"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-dup-relation",
            &source,
        )
        .expect("compile federated duplicate-entity relation hop");
        let kids = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "kids")
            .expect("kids node");
        assert_eq!(kids["kind"], "relation");
        assert_eq!(kids["relation"]["target"]["entry_id"], "linear", "{kids}");
        assert_eq!(kids["relation"]["target"]["entity"], "LangItem");
        evaluate_plasm_plan_dry(&session, &plan).expect("dry-run");
    }

    /// Real github+linear catalogs: linear `Issue.children` hop from `e2` binding (not github `sub_issues`).
    #[test]
    fn federated_github_linear_issue_children_relation_dry_run() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let github_dir = root.join("../../apis/github");
        let linear_dir = root.join("../../apis/linear");
        if !github_dir.is_dir() || !linear_dir.is_dir() {
            return;
        }
        let cgs_github =
            Arc::new(plasm_core::loader::load_schema_dir(&github_dir).expect("github"));
        let cgs_linear =
            Arc::new(plasm_core::loader::load_schema_dir(&linear_dir).expect("linear"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs_github.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs_linear.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs_github.as_ref(), cgs_linear.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs_github.as_ref(), "github", &["Issue"]);
        exp.expose_entities(&layers, cgs_linear.clone(), "linear", &["Issue"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs_github.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["Issue".into()],
            Some(exp),
            None,
            cgs_github.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e2 = map.entity_sym_for("linear", "Issue");
        let r_sym = map.ident_sym_relation_for("linear", "Issue", "children");
        let source = format!(
            r#"parent = {e2}("issue-id")
kids = parent.{r_sym}
kids"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "fed-linear-children-real",
            &source,
        )
        .expect("compile real github+linear children hop");
        let kids = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "kids")
            .expect("kids node");
        assert_eq!(kids["relation"]["target"]["entry_id"], "linear", "{kids}");
        evaluate_plasm_plan_dry(&session, &plan).expect("dry-run real catalogs");
    }

    /// Faithful repro of the live Linear failure: real `apis/linear`, multi-entity opaque session
    /// (Issue + Label + Comment), `issue.comments.limit(3)[id,body]` written with opaque `e#`/`r#`/`p#`.
    /// Before the fix this surfaced `team_key` (a receiver Issue field) when validating the Comment
    /// projection after the limit compute.
    #[test]
    fn linear_issue_comments_limit_projection_opaque_resolves_target() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let linear_dir = root.join("../../apis/linear");
        if !linear_dir.is_dir() {
            return;
        }
        let cgs = Arc::new(plasm_core::loader::load_schema_dir(&linear_dir).expect("linear"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let exp =
            TeachingExposureSession::new(cgs.as_ref(), "linear", &["Issue", "Label", "Comment"]);
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "linear".into(),
            String::new(),
            String::new(),
            None,
            vec!["Issue".into(), "Label".into(), "Comment".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let map = session
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let e_issue = map.entity_sym_for("linear", "Issue");
        let r_comments = map.ident_sym_relation_for("linear", "Issue", "comments");
        let p_id = map.ident_sym_entity_field_for("linear", "Comment", "id");
        let p_body = map.ident_sym_entity_field_for("linear", "Comment", "body");
        let source = format!(
            r#"issue = {e_issue}("PLASM-1")
comments = issue.{r_comments}.limit(3)
comments[{p_id},{p_body}]"#
        );
        assert_relation_limit_projection_targets(
            &session,
            "linear Issue.comments (real catalog)",
            &source,
        );
    }

    #[test]
    fn lookup_relation_chain_meta_requires_qe_federated() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let session = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let chain = plasm_core::ChainExpr::auto_get(
            plasm_core::Expr::Get(plasm_core::GetExpr::new("LangItem", "LI1")),
            "summary".to_string(),
        );
        let err = super::lookup_relation_chain_meta(&session, None, &chain, None).unwrap_err();
        assert!(
            err.contains("federated relation continuation requires catalog ownership"),
            "{err}"
        );
    }

    #[test]
    fn typed_relation_continuation_ir_has_no_domain_placeholder() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
one = commits.singleton()
author = one.author
author"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "no-dollar-ir",
            source,
        )
        .expect("compile");
        let author = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "author")
            .expect("author");
        let ir = author["relation"]["ir"]["expr"].to_string();
        assert!(
            !ir.contains(r#""$""#),
            "relation IR must not contain Get($): {ir}"
        );
        assert!(ir.contains("author") || ir.contains("Commit"), "{ir}");
    }

    /// Matrix analogue: parallel comma roots + search sugar (`lang_derive_map_parallel`, `lang_search`).
    #[test]
    fn group_by_aggregate_chain_lowers_to_single_group_by_node() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "gb-agg-chain",
            "LangItem.group_by(owner).aggregate(n=count)",
        )
        .expect("group_by().aggregate() chain");
        let computes: Vec<_> = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .filter(|n| n["kind"] == "compute")
            .collect();
        assert_eq!(computes.len(), 1, "expected fused GroupBy compute: {plan}");
        assert_eq!(computes[0]["compute"]["op"]["kind"], "group_by", "{plan}");
    }

    #[test]
    fn bare_comma_plasm_roots_compile_as_parallel_return() {
        let session = test_session();
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "parallel-roots",
            r#"LangItem, LangItem~"Alpha""#,
        )
        .expect("compile parallel roots");
        assert_eq!(plan["return"]["kind"], "parallel");
    }

    #[test]
    fn rejects_return_prefixed_surface_line() {
        let session = test_session();
        let err = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            "return LangItem, LangLine",
        )
        .expect_err("return prefix");
        assert!(
            err.contains("return is not Plasm syntax"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn rejects_return_prefixed_final_roots_in_dag() {
        let session = test_session();
        let source = "items = LangItem\nreturn items";
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "x",
            source,
        )
        .expect_err("return");
        assert!(
            err.contains("return is not Plasm syntax"),
            "unexpected: {err}"
        );
    }

    fn linear_test_session(cgs: Arc<CGS>) -> ExecuteSession {
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(
            cgs.as_ref(),
            "linear",
            &["Issue", "IssueContext", "MyWorkSnapshot", "Team", "Comment"],
        );
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "linear".into(),
            String::new(),
            String::new(),
            None,
            exp.entities.clone(),
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    /// Linear `Issue{…}` brace filters must plan as `search` (same as live `issue_search` resolution).
    #[test]
    fn linear_issue_brace_filter_plans_as_search() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = Arc::new(plasm_core::loader::load_schema_dir(&dir).expect("linear"));
        let session = linear_test_session(cgs);
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            "Issue{team_key=ENG, state_name=Todo}",
        )
        .expect("compile");
        assert_eq!(plan["nodes"][0]["kind"].as_str(), Some("search"), "{plan}");
    }

    /// Linear `issue_search` rows include `team_key` in provides so agents can `group_by` on filter dimensions.
    #[test]
    fn linear_issue_search_group_by_team_key_dry_run() {
        fn plan_group_by_keys(plan: &serde_json::Value) -> Vec<String> {
            let mut out = Vec::new();
            let Some(nodes) = plan.get("nodes").and_then(|n| n.as_array()) else {
                return out;
            };
            for node in nodes {
                if node.get("kind").and_then(|k| k.as_str()) != Some("compute") {
                    continue;
                }
                let Some(op) = node.get("compute").and_then(|c| c.get("op")) else {
                    continue;
                };
                if op.get("kind").and_then(|k| k.as_str()) != Some("group_by") {
                    continue;
                }
                let Some(keys) = op.get("keys").and_then(|k| k.as_array()) else {
                    continue;
                };
                for key in keys {
                    if let Some(path) = key.as_array() {
                        if let Some(field) = path.first().and_then(|x| x.as_str()) {
                            out.push(field.to_string());
                        }
                    } else if let Some(s) = key.as_str() {
                        out.push(s.to_string());
                    }
                }
            }
            out
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = Arc::new(plasm_core::loader::load_schema_dir(&dir).expect("linear"));
        let session = linear_test_session(cgs);
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "linear-search-group-by-team",
            r#"issues = Issue~$
by_team = issues.group_by(team_key)
by_team"#,
        )
        .expect("compile");
        let keys = plan_group_by_keys(&plan);
        assert!(
            keys.iter().any(|k| k == "team_key"),
            "expected group_by on team_key, got {keys:?}; plan={plan}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(!dry.node_results.is_empty());
    }

    /// Matrix: `lang_domain_symbol_page_size` (surface `e#.page_size` + plan node `page_size`).
    #[test]
    fn surface_line_plan_compiles_e1_with_page_size() {
        let session = test_session();
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            "e1.page_size(100)",
        )
        .expect("compile");
        assert_eq!(plan["nodes"].as_array().map(|a| a.len()), Some(1));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(!dry.node_results.is_empty());
    }

    #[test]
    fn flat_line_projection_on_binding_returns_projection() {
        let session = test_session();
        let source = r#"item = LangItem("i1") lines = item.lines lines[note]"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flat-line-projection",
            source,
        )
        .expect("flat line with trailing projection on in-scope binding");
        let return_node = plan["return"]["node"].as_str().expect("return node id");
        assert_ne!(
            return_node, "item",
            "must return lines projection, not first binding"
        );
        let return_entry = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == return_node)
            .expect("return node in plan");
        assert_eq!(
            return_entry["kind"], "compute",
            "return should be a projection node"
        );
        assert!(
            plan["metadata"].get("coerced_default_return").is_none(),
            "deliberate trailing projection must not record coercion"
        );
    }

    #[test]
    fn flattened_dag_bindings_compile_with_coerced_return() {
        let session = test_session();
        let source = r#"item = LangItem("i1") lines = item.lines lines"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened",
            source,
        )
        .expect("flattened space-separated bindings compile on DAG path");
        assert_eq!(plan["return"]["node"], "item");
        assert_eq!(plan["metadata"]["coerced_default_return"], "item");
        let lines = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "lines")
            .expect("lines relation node");
        assert_eq!(lines["relation"]["relation"], "lines");
    }

    #[test]
    fn flattened_dag_assignment_then_root_coerces_first_binding_return() {
        let session = test_session();
        let source = r#"item = LangItem("i1") LangItem.sort(score, desc).limit(2)"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened-root",
            source,
        )
        .expect("flattened assignment + postfix root compiles on DAG path");
        assert_eq!(plan["return"]["node"], "item");
        assert_eq!(plan["metadata"]["coerced_default_return"], "item");
    }

    #[test]
    fn flattened_dag_with_multiline_quoted_arg_errors_before_flatten() {
        let session = test_session();
        let source = "prof = LangItem(\"i1\") LangLine(message=\"long\nbody\")";
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened-quote",
            source,
        )
        .expect_err("physical newline in quoted arg should fail before flatten");
        assert!(
            err.contains("physical newline inside a quoted Plasm string parameter")
                && err.contains("tagged heredoc"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn multiline_quoted_arg_gets_heredoc_diagnostic() {
        let err = collect_program_statement_lines("body = LangLine(message=\"long\nbody\")")
            .expect_err("physical newline in quote");
        assert!(
            err.contains("physical newline inside a quoted Plasm string parameter")
                && err.contains("tagged heredoc"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn flattened_dag_diagnostic_does_not_mask_heredoc_newline_errors() {
        let session = test_session();
        let source = "body = <<B hello B";
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "heredoc-flat",
            source,
        )
        .expect_err("bad heredoc should fail");
        assert!(
            !err.contains("Do not separate bindings or final roots with spaces"),
            "unexpected: {err}"
        );
    }

    /// Matrix: heredoc / render delimiter hygiene (`lang_bindings_render`, `lang_heredoc_binding`).
    #[test]
    fn split_top_level_does_not_split_commas_inside_tagged_heredoc() {
        let parts = split_top_level("fn(<<T\na,b,c\nT\n), bar", ',').expect("split");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("a,b,c"), "{:?}", parts[0]);
        assert_eq!(parts[1].trim(), "bar");
    }

    #[test]
    fn collect_program_statement_lines_errors_on_squashed_heredoc_opener() {
        let err = collect_program_statement_lines("body = <<B # junk").expect_err("err");
        assert!(
            err.contains("opener") || err.contains("tag") || err.contains("newline"),
            "{err}"
        );
    }

    #[test]
    fn collect_program_statement_lines_glued_heredoc_close() {
        // `H)` closes the heredoc and ends with `)`; outer `m(` balances that delimiter.
        let stmts = collect_program_statement_lines("x = m(<<H\none\nH)").expect("parse");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("<<H"), "{:?}", stmts[0]);
        assert!(stmts[0].contains("one"));
    }

    /// Matrix: heredoc binding + parallel roots (`lang_heredoc_binding`).
    #[test]
    fn multiline_heredoc_binding_then_parallel_roots_compiles() {
        let session = test_session();
        let source = "body = <<T\nhello\nT\nLangItem, LangLine(\"L1\")";
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "heredoc-roots",
            source,
        )
        .expect("compile");
        assert_eq!(plan["return"]["kind"], "parallel");
    }

    fn github_repository_commit_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(load_schema(&root.join("../../apis/github")).expect("load github"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["Repository", "Commit"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["Repository".into(), "Commit".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn compiles_two_hop_one_cardinality_relation_binding_chain() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
one = commits.singleton()
author = one.author
author"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-two-hop-one-rel",
            source,
        )
        .expect("compile");
        let author = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "author")
            .expect("author relation node");
        assert_eq!(author["kind"], "relation");
        assert_eq!(
            author["relation"]["source_cardinality"].as_str(),
            Some("runtime_checked_singleton")
        );
        assert_eq!(author["relation"]["cardinality"], "one");
        let ir = author["relation"]["ir"]["expr"].to_string();
        assert!(!ir.contains(r#""$""#), "author IR: {ir}");
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(
            dry.node_results.len(),
            plan["nodes"].as_array().unwrap().len()
        );
    }

    /// `repo.<relation>` continues the bound repository Plasm and compiles to a `kind: relation` plan node.
    #[test]
    fn compiles_bound_node_ref_relation_chain_dag_to_valid_plan() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
commits"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-node-ref-rel",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 2, "{plan:#}");
        let rel = &nodes[1];
        assert_eq!(rel["kind"], "relation");
        assert_eq!(rel["relation"]["source"], "repo");
        assert_eq!(rel["relation"]["relation"], "commits");
        assert_eq!(rel["relation"]["target"]["entity"], "Commit");
        assert_eq!(rel["uses_result"][0]["node"], "repo");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(
            dry.node_results[1]["simulation"]["kind"],
            "relation_traversal"
        );
    }

    fn github_issue_label_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(load_schema(&root.join("../../apis/github")).expect("load github"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        let exp =
            TeachingExposureSession::new(cgs.as_ref(), "github", &["Repository", "Issue", "Label"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["Repository".into(), "Issue".into(), "Label".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn relation_uses_result_includes_scope_binding_aliases() {
        let session = github_issue_label_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
issues = Issue{repository=repo.full_name}
labels = issues.labels
labels"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-issue-label-scope",
            source,
        )
        .expect("compile");
        let labels = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "labels")
            .expect("labels relation node");
        let uses = labels["uses_result"].as_array().expect("uses_result");
        assert!(
            uses.iter()
                .any(|u| u["node"] == "repo" && u["as"] == "repo"),
            "expected repo in uses_result: {uses:?}"
        );
        assert!(
            uses.iter()
                .any(|u| u["node"] == "issues" && u["as"] == "source"),
            "expected issues source in uses_result: {uses:?}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        let facts = dry
            .graph_summary
            .get("boundedness_facts")
            .and_then(|v| v.as_array())
            .expect("boundedness_facts");
        let joined: Vec<String> = facts
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        assert!(
            joined
                .iter()
                .any(|f: &String| f.contains("Includes relation traversal")),
            "expected relation-traversal boundedness fact: {joined:?}"
        );
    }

    #[test]
    fn lhs_gated_relation_segment_ignores_wrong_token() {
        let session = github_issue_label_session();
        let qe = QualifiedEntityKey {
            entry_id: "github".into(),
            entity: "Issue".into(),
        };
        let wire = resolve_relation_segment_for_continuation(
            &session,
            None,
            &qe,
            "p99",
            Some(plasm_core::ProgramBindingLabel("labels")),
        )
        .expect("binding label selects relation wire");
        assert_eq!(wire, "labels");
    }

    /// Opaque `p#` homograph to `labels` is rejected in relation nav; DAG LHS binding forgives it.
    #[test]
    fn homograph_p_rejected_in_parse_forgiven_with_lhs_binding_label() {
        use plasm_core::expr_parser::{parse_with_cgs_layers_program, ParseErrorKind};
        use plasm_core::relation_segment::{
            resolve_relation_segment, RelationSegmentContext, RelationSegmentOutcome,
        };

        let session = github_issue_label_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sym = map.ident_sym_cap_param_for("github", "Issue", "issue_query", "labels");
        assert!(
            p_sym.starts_with('p'),
            "expected opaque p# for labels filter, got {p_sym}"
        );
        let issue_e = map.entity_sym_for("github", "Issue");
        let line = format!("{issue_e}.{p_sym}");
        let layers = crate::plasm_plan_run::session_cgs_layers(&session);
        let err = parse_with_cgs_layers_program(&line, &layers, map.clone(), None, false, None)
            .expect_err("homograph p# in relation nav");
        assert!(matches!(
            err.kind,
            ParseErrorKind::RelationSegmentWrongRole { .. }
        ));

        let issue_ent = session.cgs.get_entity("Issue").expect("Issue");
        let ctx = RelationSegmentContext {
            map: &map,
            entity: "Issue",
            relations: &issue_ent.relations,
            binding_label: None,
            allow_lhs_coercion: false,
        };
        assert!(matches!(
            resolve_relation_segment(&ctx, p_sym.as_str()),
            RelationSegmentOutcome::WrongRole { .. }
        ));

        let qe = QualifiedEntityKey {
            entry_id: "github".into(),
            entity: "Issue".into(),
        };
        let wire = resolve_relation_segment_for_continuation(
            &session,
            None,
            &qe,
            p_sym.as_str(),
            Some(plasm_core::ProgramBindingLabel("labels")),
        )
        .expect("LHS binding label selects relation wire");
        assert_eq!(wire, "labels");
    }

    #[test]
    fn multiline_explicit_return_position_not_first_binding() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "ml-return-position-limit-projection",
            r#"item = LangItem("i1")
lines = item.lines.limit(2)
lines"#,
        )
        .expect("compile multiline program with explicit trailing return");
        assert_eq!(
            plan["return"]["node"], "lines",
            "final line is return position; must not coerce to first binding `item`"
        );
        assert!(
            plan["metadata"].get("coerced_default_return").is_none(),
            "explicit roots line must not record coercion"
        );
    }

    #[test]
    fn flattened_single_liner_lhs_gated_relation_primary_return() {
        let session = github_issue_label_session();
        let source = r#"repo = Repository(owner="octocat", repo="Hello-World") issues = Issue{repository=repo.full_name} labels = issues.labels labels"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-flattened-labels",
            source,
        )
        .expect("flattened single-liner");
        assert_eq!(plan["return"]["node"], "repo");
        assert_eq!(plan["metadata"]["coerced_default_return"], "repo");
        let labels = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "labels")
            .expect("labels node");
        assert_eq!(labels["relation"]["relation"], "labels");
    }

    #[test]
    fn surface_line_compile_matches_dag_for_flattened_single_liner() {
        let session = test_session();
        let source = "items = LangItem tags = items.tags tags";
        let dag = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flatten-parity-dag",
            source,
        )
        .expect("dag compile");
        let surface = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flatten-parity-surface",
            source,
        )
        .expect("surface line compile");
        assert_eq!(surface["return"], dag["return"]);
        assert_eq!(
            surface["metadata"]["coerced_default_return"],
            dag["metadata"]["coerced_default_return"]
        );
        assert_eq!(
            surface["nodes"].as_array().map(|n| n.len()),
            dag["nodes"].as_array().map(|n| n.len())
        );
    }

    #[test]
    fn relation_plural_opaque_p2_continuation() {
        let session = github_issue_label_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let sym = map.ident_sym_relation("Issue", "labels");
        let source = format!(
            r#"repo = Repository(owner="octocat", repo="Hello-World")
issues = Issue{{repository=repo.full_name}}
labels = issues.{sym}
labels"#
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-issue-labels-opaque-p",
            &source,
        )
        .expect("compile opaque plural relation continuation");
        let labels = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "labels")
            .expect("labels relation node");
        assert_eq!(labels["relation"]["relation"], "labels");
        assert_eq!(
            labels["relation"]["source_cardinality"].as_str(),
            Some("many")
        );
        assert_eq!(labels["relation"]["source"], "issues");
        evaluate_plasm_plan_dry(&session, &plan).expect("dry");
    }

    fn language_matrix_tags_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "langmatrix".into(),
            Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
        );
        let exp =
            TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem", "LangTag"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "langmatrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into(), "LangTag".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn language_matrix_plural_opaque_relation_continuation() {
        let session = language_matrix_tags_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let sym = map.ident_sym_relation("LangItem", "tags");
        let source = format!("items = LangItem\ntags = items.{sym}\ntags");
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-plural-opaque-tags",
            &source,
        )
        .expect("compile matrix opaque plural relation continuation");
        let tags = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "tags")
            .expect("tags relation node");
        assert_eq!(tags["relation"]["relation"], "tags");
        assert_eq!(
            tags["relation"]["source_cardinality"].as_str(),
            Some("many")
        );
    }

    #[test]
    fn compiles_node_ref_relation_limit_and_project() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
limited = commits.limit(20)
projected = limited[sha,message]
projected"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-chain-limit-project",
            source,
        )
        .expect("compile");
        assert_eq!(plan["nodes"].as_array().map(Vec::len), Some(4));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), 4, "{dry:?}");
    }

    #[test]
    fn bare_label_singleton_lowers_to_limit_preserving_commit_entity() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(3)
one = commits.singleton()
one"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bare-label-singleton",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let one = nodes.iter().find(|n| n["id"] == "one").expect("one");
        assert_eq!(one["kind"], "compute");
        assert_eq!(one["compute"]["source"], "commits");
        assert_eq!(one["compute"]["op"]["kind"], "limit");
        assert_eq!(one["compute"]["op"]["count"], 1);
        assert_eq!(one["compute"]["schema"]["entity"], json!("Commit"));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), nodes.len());
    }

    /// Matrix: `lang_bind_limit1_continuation` — limit on a surface bind must keep LangItem entity for `.tags` continuation.
    #[test]
    fn limit_on_surface_bind_preserves_langitem_entity() {
        let session = test_session();
        let source = r#"root = LangItem{owner="alice"}
one = root.limit(1)
tags = one.tags
tags"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "limit-surface-bind",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let one = nodes.iter().find(|n| n["id"] == "one").expect("one");
        assert_eq!(one["kind"], "compute");
        assert_eq!(one["compute"]["op"]["kind"], "limit");
        assert_eq!(one["compute"]["schema"]["entity"], json!("LangItem"));
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), nodes.len());
    }

    #[test]
    fn bare_label_page_size_lowers_to_identity_project() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(5)
paged = commits.page_size(10)
paged"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bare-label-page-size",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let paged = nodes.iter().find(|n| n["id"] == "paged").expect("paged");
        assert_eq!(paged["kind"], "compute");
        assert_eq!(paged["compute"]["op"]["kind"], "project");
        assert_eq!(paged["compute"]["page_size"], json!(10));
        assert_eq!(paged["compute"]["schema"]["entity"], json!("Commit"));
    }

    #[test]
    fn bracket_render_accepts_bare_label_singleton_on_source() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(2)\nmail = commits.singleton()[{p_sha}] <<MD\nx\nMD\nmail"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bare-label-singleton-render",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let prefix = nodes
            .iter()
            .find(|n| n["id"].as_str() == Some("__plasm_render_src_mail"))
            .expect("render prefix");
        assert_eq!(prefix["compute"]["op"]["kind"], "limit");
        let render = nodes.iter().find(|n| n["id"] == "mail").expect("mail");
        assert_eq!(render["compute"]["op"]["kind"], "render");
        assert!(
            render["compute"]["page_size"].is_null(),
            "expected render compute.page_size omitted when prefix lowered tail flags"
        );
    }

    #[test]
    fn bracket_render_content_rejected_as_program_root_with_actionable_copy() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nmail = commits[{p_sha}] <<MD\nx\nMD\nmail.content"
        );
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "render-content-root",
            &source,
        )
        .expect_err("mail.content must not be a root");
        assert!(
            err.contains("scalar string") && err.contains("mail.content"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn derive_accepts_render_content_as_binding_rhs() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nmail = commits[{p_sha}] <<MD\nx\nMD\nout = mail => mail.content\nout"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "derive-render-content",
            &source,
        )
        .expect("derive with mail.content");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let derive = nodes.iter().find(|n| n["id"] == "out").expect("derive");
        assert_eq!(derive["kind"], "derive");
    }

    /// teaching table `p#` inside postfix projection must lower to wire field names in `Plan` IR (not
    /// survive as literal `p#` paths that dry-run would project as null).
    #[test]
    fn dag_postfix_projection_expands_domain_field_symbols_to_wire_paths() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let p_msg = map.ident_sym_entity_field("Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(2)\ncommits[{p_sha},{p_msg}]"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-projection",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let last = nodes.last().expect("compute projection node");
        assert_eq!(last["kind"], "compute");
        let op = &last["compute"]["op"];
        assert_eq!(op["kind"], "project");
        let fields = op["fields"].as_object().expect("project fields");
        assert!(
            fields.contains_key("sha") && fields.contains_key("message"),
            "expected wire keys sha/message, got {fields:?}"
        );
        if p_sha != "sha" {
            assert!(
                !fields.contains_key(&p_sha),
                "teaching-table symbol {p_sha} must not appear as projection column: {fields:?}"
            );
        }
    }

    #[test]
    fn dag_postfix_sort_expands_domain_field_symbol_in_sort_key() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_msg = map.ident_sym_entity_field("Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(3)\nordered = commits.sort({p_msg}, desc)\nordered"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-sort",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let sort_node = nodes
            .iter()
            .find(|n| n["id"] == "ordered")
            .expect("sort node");
        let op = &sort_node["compute"]["op"];
        assert_eq!(op["kind"], "sort");
        assert_eq!(op["key"], json!(["message"]));
        assert_eq!(op["descending"], true);
    }

    #[test]
    fn dag_postfix_sort_whitespace_direction_expands_domain_field_symbol() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_msg = map.ident_sym_entity_field("Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(3)\nordered = commits.sort({p_msg} desc)\nordered"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-sort-whitespace",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let sort_node = nodes
            .iter()
            .find(|n| n["id"] == "ordered")
            .expect("sort node");
        let op = &sort_node["compute"]["op"];
        assert_eq!(op["kind"], "sort");
        assert_eq!(op["key"], json!(["message"]));
        assert_eq!(op["descending"], true);
    }

    #[test]
    fn dag_postfix_sort_on_projected_binding_accepts_p_symbol() {
        let session = test_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_id = map.ident_sym_entity_field("LangItem", "id");
        let p_score = map.ident_sym_entity_field("LangItem", "score");
        let source = format!(
            "rows = LangItem.limit(5)\nnarrow = rows[{p_id},{p_score}]\nordered = narrow.sort({p_score} desc)\nordered"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-projected-sort",
            &source,
        )
        .expect("compile");
        let sort_node = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "ordered")
            .expect("sort node");
        let op = &sort_node["compute"]["op"];
        assert_eq!(op["kind"], "sort");
        assert_eq!(op["key"], json!(["score"]));
        assert_eq!(op["descending"], true);
    }

    #[test]
    fn dag_postfix_group_by_filter_dedupe_accept_p_symbols() {
        let session = test_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_owner = map.ident_sym_entity_field("LangItem", "owner");
        let p_score = map.ident_sym_entity_field("LangItem", "score");
        let p_id = map.ident_sym_entity_field("LangItem", "id");
        for (name, source) in [
            (
                "group_by",
                format!("rows = LangItem.limit(5)\nout = rows.group_by({p_owner})\nout"),
            ),
            (
                "filter",
                format!("rows = LangItem.limit(5)\nout = rows.filter{{{p_score}>0}}\nout"),
            ),
            (
                "dedupe",
                format!("rows = LangItem.limit(5)\nout = rows.dedupe({p_id})\nout"),
            ),
        ] {
            compile_plasm_dag_to_plan(
                &PromptPipelineConfig::default(),
                None,
                &session,
                name,
                &source,
            )
            .unwrap_or_else(|e| panic!("{name} should accept p# symbols: {e}"));
        }
    }

    #[test]
    fn sort_field_error_recommends_p_symbols_not_projected_columns() {
        let session = test_session();
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "sort-bad-field",
            "rows = LangItem.limit(2)\nrows.sort(not_a_field desc)\nrows",
        )
        .expect_err("unknown sort field");
        assert!(
            err.contains("p#") || err.contains("rows:"),
            "expected p# guidance, got: {err}"
        );
        assert!(
            !err.contains("projected columns"),
            "must not steer to wire column names: {err}"
        );
    }

    #[test]
    fn dag_postfix_aggregate_expands_domain_field_symbol_in_sum() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_add = map.ident_sym_entity_field("Commit", "stats_additions");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(5)\ntot = commits.aggregate(t=sum({p_add}))\ntot"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-aggregate",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let agg = nodes
            .iter()
            .find(|n| n["id"] == "tot")
            .expect("aggregate node");
        let op = &agg["compute"]["op"];
        assert_eq!(op["kind"], "aggregate");
        let aggs = op["aggregates"].as_array().expect("aggregates");
        assert_eq!(aggs[0]["name"], "t");
        assert_eq!(aggs[0]["function"], "sum");
        assert_eq!(aggs[0]["field"], json!(["stats_additions"]));
    }

    #[test]
    fn dag_render_field_list_expands_domain_field_symbols() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let p_msg = map.ident_sym_entity_field("Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nout = commits[{p_sha},{p_msg}] <<MD\n{{{{ rows | length }}}}\nMD\nout"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-domain-render",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "out")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        let col_names: Vec<_> = cols
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        assert_eq!(col_names, vec!["sha", "message"]);
    }

    #[test]
    fn dag_render_infers_columns_from_projected_binding() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let p_msg = map.ident_sym_entity_field("Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(2)[{p_sha},{p_msg}]\nreport = commits <<MD\n{{{{ rows | length }}}}\nMD\nreport"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-inferred-render-projection",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "report")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        let mut col_names: Vec<_> = cols
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        col_names.sort();
        assert_eq!(col_names, vec!["message", "sha"]);
    }

    #[test]
    fn dag_render_infers_entity_row_columns_after_limit_only() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(20)
report = commits <<MD
{{ rows | length }}
MD
report"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-inferred-render-limit",
            source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "report")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        assert!(
            cols.len() >= 2,
            "expected entity-backed columns (got {cols:?})"
        );
        let names: Vec<_> = cols.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"sha"), "{names:?}");
        assert!(names.contains(&"message"), "{names:?}");
    }

    #[test]
    fn dag_render_node_ref_postfix_explicit_columns_before_heredoc() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let p_msg = map.ident_sym_entity_field("Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits\nreport = commits.limit(20)[{p_sha},{p_msg}] <<MD\nx\nMD\nreport"
        );
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-render-chain-binding",
            &source,
        )
        .expect("compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let render = nodes
            .iter()
            .find(|n| n["id"] == "report")
            .expect("render node");
        let op = &render["compute"]["op"];
        assert_eq!(op["kind"], "render");
        let cols = op["columns"].as_array().expect("columns");
        let col_names: Vec<_> = cols
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        assert_eq!(col_names, vec!["sha", "message"]);
    }

    #[test]
    fn dag_render_rejects_inference_from_prior_render_output() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let p_msg = map.ident_sym_entity_field("Commit", "message");
        let source = format!(
            "repo = Repository(owner=\"ryan-s-roberts\", repo=\"plasm-core\")\ncommits = repo.commits.limit(1)\nfirst = commits[{p_sha},{p_msg}] <<MD\n{{{{ r.sha }}}}\nMD\nbad = first <<MD\ny\nMD\nbad"
        );
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "github-render-on-render",
            &source,
        )
        .expect_err("render from render");
        assert!(
            err.contains("cannot infer template columns")
                || err.contains("row-to-text template result"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn compiles_continuation_from_projection_anchor() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
trimmed = repo[id]
commits = trimmed.commits
commits"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "projection-anchor",
            source,
        )
        .expect("projection anchor should compile");
        let nodes = plan["nodes"].as_array().expect("nodes");
        let rel = nodes
            .iter()
            .find(|n| n["id"] == "commits")
            .expect("relation node");
        assert_eq!(rel["kind"], "relation");
        assert_eq!(rel["relation"]["source"], "trimmed");
        assert_eq!(rel["relation"]["relation"], "commits");
        assert_eq!(rel["relation"]["source_cardinality"], "single");
        assert_eq!(rel["uses_result"][0]["node"], "trimmed");
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        let text = render_plasm_plan_dry_text(&dry, None);
        assert!(text.contains("trimmed.commits"), "{text}");
    }

    #[test]
    fn rejects_continuation_from_aggregate_anchor() {
        let session = github_repository_commit_session();
        let source = r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits
totals = commits.aggregate(n=count)
bad = totals.commits"#;
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "aggregate-non-anchor",
            source,
        )
        .expect_err("aggregate is not a Plasm anchor");
        assert!(
            err.contains("row-preserving projection bindings"),
            "unexpected: {err}"
        );
    }

    /// Direct postfix `.limit` on a surface expression must compile with the same plan shape as
    /// bind-first `label = expr` then `label.limit(n)` (unified language contract).
    #[test]
    fn direct_surface_limit_equivalent_to_bind_first_two_node_plan() {
        let session = github_repository_commit_session();
        let bind_first = r#"commits = Repository(owner="ryan-s-roberts", repo="plasm-core").commits
x = commits.limit(2)
x"#;
        let direct = r#"Repository(owner="ryan-s-roberts", repo="plasm-core").commits.limit(2)"#;
        let p1 = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "bind-first-limit",
            bind_first,
        )
        .expect("bind-first");
        let p2 = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "direct-limit",
            direct,
        )
        .expect("direct");
        let n1 = p1["nodes"].as_array().expect("nodes");
        let n2 = p2["nodes"].as_array().expect("nodes");
        assert_eq!(n1.len(), 3, "{p1:#}");
        assert_eq!(n2.len(), 3, "{p2:#}");
        assert_eq!(n1[1]["kind"], "relation");
        assert_eq!(n2[1]["kind"], "relation");
        let last1 = &n1[2];
        let last2 = &n2[2];
        assert_eq!(last1["kind"], "compute");
        assert_eq!(last2["kind"], "compute");
        let op1 = &last1["compute"]["op"];
        let op2 = &last2["compute"]["op"];
        assert_eq!(op1["kind"], "limit", "{op1:#}");
        assert_eq!(op2["kind"], "limit", "{op2:#}");
        assert_eq!(op1["count"], 2);
        assert_eq!(op2["count"], 2);
    }

    #[test]
    fn parse_aggregates_canonical_n_count() {
        let specs = super::parse_aggregates("n=count").expect("canonical count");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name.as_str(), "n");
        assert_eq!(
            specs[0].function,
            crate::plasm_plan::AggregateFunction::Count
        );
        assert!(specs[0].field.is_none());
    }

    #[test]
    fn parse_aggregates_shadow_bare_count() {
        let specs = super::parse_aggregates("count").expect("shadow bare count");
        assert_eq!(specs[0].name.as_str(), "count");
        assert_eq!(
            specs[0].function,
            crate::plasm_plan::AggregateFunction::Count
        );
    }

    #[test]
    fn parse_aggregates_shadow_aggregate_count() {
        let specs = super::parse_aggregates("aggregate(count)").expect("shadow aggregate(count)");
        assert_eq!(specs[0].name.as_str(), "count");
    }

    #[test]
    fn parse_aggregates_rejects_aggregate_sum_without_alias() {
        let err = super::parse_aggregates("aggregate(sum(amount))").unwrap_err();
        assert!(
            err.contains("total=sum(amount)") || err.contains("explicit"),
            "{err}"
        );
    }

    #[test]
    fn compile_row_filter_brace_on_binding() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "lang_row_filter_brace",
            "items = LangItem\nfiltered = items.filter{owner=\"o1\"}\nfiltered",
        )
        .expect("compile filter program");
        let has_filter = plan["nodes"].as_array().expect("nodes").iter().any(|n| {
            n.get("compute")
                .and_then(|c| c.get("op"))
                .and_then(|o| o.get("kind"))
                == Some(&serde_json::json!("filter"))
        });
        assert!(has_filter, "expected filter compute node: {plan}");
    }

    #[test]
    fn group_by_postfix_accepts_canonical_key_and_aggs() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let state = super::CompileState::new(&pipeline, None);
        let node = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::GroupBy {
                args: "owner, n=count".into(),
            },
            "src",
            "id",
            "expr",
        )
        .expect("group_by");
        match node.source {
            super::DagNodeSource::Compute {
                op:
                    crate::plasm_plan::ComputeOp::GroupBy {
                        ref keys,
                        ref aggregates,
                    },
                ..
            } => {
                assert_eq!(keys.len(), 1);
                assert_eq!(keys[0].dotted(), "owner");
                assert_eq!(aggregates.len(), 1);
                assert_eq!(aggregates[0].name.as_str(), "n");
            }
            _ => panic!("expected group_by compute"),
        }
    }

    #[test]
    fn sort_unknown_direction_errors() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let state = super::CompileState::new(&pipeline, None);
        let err = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::Sort {
                args: "score, newest".into(),
            },
            "src",
            "id",
            "expr",
        )
        .unwrap_err();
        assert!(err.contains("newest"), "{err}");
    }

    #[test]
    fn sort_accepts_direction_aliases() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let state = super::CompileState::new(&pipeline, None);
        let asc = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::Sort {
                args: "score, ascending".into(),
            },
            "src",
            "id",
            "expr",
        )
        .expect("asc");
        match asc.source {
            super::DagNodeSource::Compute {
                op: crate::plasm_plan::ComputeOp::Sort { descending, .. },
                ..
            } => assert!(!descending),
            _ => panic!("expected compute sort"),
        }
        let desc = super::postfix_op_to_compute(
            &session,
            &state,
            &[],
            &plasm_core::expr_parser::PlasmPostfixOp::Sort {
                args: "score, descending".into(),
            },
            "src",
            "id",
            "expr",
        )
        .expect("desc");
        match desc.source {
            super::DagNodeSource::Compute {
                op: crate::plasm_plan::ComputeOp::Sort { descending, .. },
                ..
            } => assert!(descending),
            _ => panic!("expected compute sort"),
        }
    }

    /// teaching table `p#` indices are session-global; mixing another entity's symbols into a Commit projection
    /// must fail at compile time instead of producing all-null columns at runtime.
    #[test]
    fn postfix_projection_rejects_foreign_entity_domain_symbols() {
        let session = github_repository_commit_session();
        let map = symbol_map_for_plasm_surface_parse(&session, None);
        let p_repo = map.ident_sym_entity_field("Repository", "open_issues_count");
        let p_sha = map.ident_sym_entity_field("Commit", "sha");
        let source = format!(
            r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(20)
commits[{p_repo},{p_sha}]
commits"#,
            p_repo = p_repo,
            p_sha = p_sha,
        );
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "foreign-field-proj",
            &source,
        )
        .expect_err("cross-entity symbols must not compile");
        assert!(
            err.contains("open_issues_count")
                && (err.contains("not a row field") || err.contains("null columns")),
            "{err}"
        );
    }

    mod projection_props {
        use super::*;
        use proptest::prelude::*;
        use std::sync::OnceLock;

        fn github_session_cached() -> &'static ExecuteSession {
            static CELL: OnceLock<ExecuteSession> = OnceLock::new();
            CELL.get_or_init(github_repository_commit_session)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(48))]

            #[test]
            fn nonempty_commit_field_subset_always_compiles(
                picks in prop::collection::vec(0usize..4usize, 1usize..=8usize)
            ) {
                let fields = ["sha", "message", "stats_additions", "total_changes"];
                let mut set = BTreeSet::new();
                for i in picks {
                    set.insert(fields[i % fields.len()]);
                }
                let proj = set.into_iter().collect::<Vec<_>>().join(",");
                let session = github_session_cached();
                let source = format!(
                    r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(3)
commits[{proj}]
commits"#
                );
                compile_plasm_dag_to_plan(
                    &PromptPipelineConfig::default(),
                    None,
                    session,
                    "prop-commit-subset",
                    &source,
                )
                .expect("projection");
            }

            #[test]
            fn repository_field_name_literal_in_commit_projection_fails(
                bad in "(open_issues_count|forks_count|stargazers_count)"
            ) {
                let session = github_session_cached();
                let source = format!(
                    r#"repo = Repository(owner="ryan-s-roberts", repo="plasm-core")
commits = repo.commits.limit(3)
commits[{bad}]
commits"#
                );
                let err = compile_plasm_dag_to_plan(
                    &PromptPipelineConfig::default(),
                    None,
                    session,
                    "prop-bad-commit-path",
                    &source,
                )
                .expect_err("reject");
                prop_assert!(
                    err.contains("null columns") || err.contains("not a row field"),
                    "{err}"
                );
            }
        }
    }

    #[test]
    fn matrix_bind_relation_hop_summary_relation_ir() {
        let session = test_session();
        let source = r#"item = LangItem("i1")
summary = item.summary
summary"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-bind-hop",
            source,
        )
        .expect("compile");
        let summary = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "summary")
            .expect("summary node");
        assert_eq!(summary["kind"], "relation");
        let ir = summary["relation"]["ir"]["expr"].to_string();
        assert!(
            ir.contains("LangItem") && ir.contains("summary"),
            "expected surface chain IR, got {ir}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), 2, "{dry:?}");
    }

    #[test]
    fn matrix_bind_relation_hop_detail_relation_ir() {
        let session = test_session();
        let source = r#"item = LangItem("i1")
summary = item.summary
detail = summary.detail
detail"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "matrix-bind-hop-detail",
            source,
        )
        .expect("compile");
        let detail = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "detail")
            .expect("detail node");
        assert_eq!(detail["kind"], "relation");
        let ir = detail["relation"]["ir"]["expr"].to_string();
        assert!(
            ir.contains("__plasm_hole") && ir.contains("node_input") && ir.contains("LangSummary"),
            "expected row-hole relation IR from relation-sourced binding, got {ir}"
        );
        let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), 3, "{dry:?}");
    }

    #[test]
    fn for_each_heredoc_row_cursor_does_not_depend_on_underscore() {
        let session = test_session();
        let source = r#"items = LangItem.limit(2)
created = items => LangItem.create(title=<<T
row ${_.title}
T
)
created"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "for-each-row-heredoc",
            source,
        )
        .expect("compile");
        let created = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "created")
            .expect("for_each node");
        assert_eq!(created["kind"], "for_each");
        let depends = created["depends_on"].as_array().expect("depends_on");
        assert!(
            depends.iter().all(|d| d.as_str() != Some("_")),
            "depends_on must not include row cursor: {depends:?}"
        );
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
    }

    #[test]
    fn for_each_heredoc_cross_binding_collects_upstream_node() {
        let session = test_session();
        let source = r#"report = <<RPT
static body
RPT
items = LangItem.limit(2)
created = items => LangItem.create(title=<<T
${report.content}
T
)
created"#;
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "for-each-cross-binding",
            source,
        )
        .expect("compile");
        let created = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "created")
            .expect("for_each node");
        let uses = created["uses_result"].as_array().expect("uses_result");
        assert!(
            uses.iter().any(|u| u["node"] == "report"),
            "cross-binding heredoc must record upstream node: {uses:?}"
        );
        let plan_value = crate::plasm_plan::parse_plan_value(&plan).expect("parse plan");
        crate::plasm_plan::validate_plan_artifact(&plan_value).expect("validate plan");
    }

    #[test]
    fn lang_for_each_update_matrix_program_compiles_for_each_action() {
        let session = test_session();
        let source = "items = LangItem(\"i1\")[id,title,owner]\n\
            sync = items => LangItem(\"i1\").update(score=3, title=_.title, owner=_.owner)\n\
            sync";
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "lang_for_each_update",
            source,
        )
        .expect("compile");
        let sync = plan["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == "sync")
            .expect("for_each sync node");
        assert_eq!(sync["kind"], "for_each");
        assert_eq!(
            sync.pointer("/effect_template/kind")
                .and_then(|k| k.as_str()),
            Some("action")
        );
        let dry = crate::plasm_plan_run::evaluate_plasm_plan_dry(&session, &plan).expect("dry");
        assert!(
            dry.node_results.iter().any(|nr| {
                nr.get("kind").and_then(|k| k.as_str()) == Some("for_each")
                    && nr.pointer("/effect_template/kind").and_then(|k| k.as_str())
                        == Some("action")
            }),
            "dry node_results: {:?}",
            dry.node_results
        );
    }

    #[test]
    fn binding_field_projection_root_rewrites_paren_to_bracket() {
        let session = test_session();
        let plan = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "binding-projection-root",
            "rows = LangItem\npick = rows.limit(1)\npick(id, title)",
        )
        .expect("compile binding projection root");
        let ret = plan["return"].pointer("/node").and_then(|v| v.as_str());
        assert_eq!(
            ret,
            Some("return_1"),
            "paren root desugars to bracket projection return"
        );
    }

    #[test]
    fn rewrite_binding_field_projection_root_unit() {
        let session = test_session();
        let pipeline = PromptPipelineConfig::default();
        let mut state = CompileState::new(&pipeline, None);
        for node in compile_node_expr(&session, &state, "rows", "LangItem").expect("rows") {
            state.insert(node).expect("insert rows");
        }
        for node in compile_node_expr(&session, &state, "pick", "rows.limit(1)").expect("pick") {
            state.insert(node).expect("insert pick");
        }
        let rewritten = rewrite_binding_field_projection_root("pick(id, title)", &state);
        assert_eq!(rewritten.as_deref(), Some("pick[id, title]"));
    }
}
