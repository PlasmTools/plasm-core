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
use crate::plasm_plan_run::{parse_plasm_surface_line_program, symbol_map_for_plasm_surface_parse};
use plasm_core::expr_parser::{
    collect_program_statement_lines, is_valid_program_label, peel_postfix_suffixes,
    split_assignment_at_top_level, split_token_top_level, split_top_level, strip_line_comment,
    try_parse_render_tail, validate_program_label, PlasmPostfixOp, RenderTailParse,
};
use plasm_core::schema::EntityDef;
use plasm_core::ChainStep;
use plasm_core::Expr;
use plasm_core::PlasmInputRef;
use plasm_core::Predicate;
use plasm_core::PromptPipelineConfig;
use plasm_core::SymbolMapCrossRequestCache;
use plasm_core::Value;
use plasm_core::CGS;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;
use std::sync::Arc;

/// Program RHS after session-scoped DOMAIN expansion (`e#` / `p#` / `m#` → wire/catalog text).
///
/// Construct only via [`Self::new`] at the [`compile_node_expr`] entry so postfix peel and field
/// lists never see raw gloss tokens when lowering to [`ComputeOp`](crate::plasm_plan::ComputeOp).
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
    let mut acc = Vec::new();
    collect_expr_for_template_uses(&mut acc, expr);
    dedupe_uses(acc)
}

/// Records upstream plan nodes so `node_input` holes become `uses_result` → `ir_template` + instantiation
/// before compile.
///
/// Surfaces covered: query predicates; **get**/**delete**/**invoke** `path_vars`; invoke/create payloads (values
/// recurse into objects/arrays). [`Expr::Get`] compound identity literals live on `reference`; program bindings
/// in compound slots are lowered to `path_vars` and collected here. [`PlasmInputRef::RowBinding`] is skipped on
/// purpose (`for_each` row scope).
fn collect_expr_for_template_uses(acc: &mut Vec<serde_json::Value>, expr: &Expr) {
    match expr {
        Expr::Query(q) => {
            if let Some(pred) = &q.predicate {
                collect_predicate_for_template_uses(acc, pred);
            }
        }
        Expr::Get(g) => {
            if let Some(pv) = &g.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v);
                }
            }
        }
        Expr::Create(c) => {
            let v = c.input.to_value();
            collect_value_for_template_uses(acc, &v);
        }
        Expr::Delete(d) => {
            if let Some(pv) = &d.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v);
                }
            }
        }
        Expr::Invoke(i) => {
            if let Some(input) = &i.input {
                let v = input.to_value();
                collect_value_for_template_uses(acc, &v);
            }
            if let Some(pv) = &i.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v);
                }
            }
        }
        Expr::Chain(ch) => {
            collect_expr_for_template_uses(acc, &ch.source);
            if let ChainStep::Explicit { expr } = &ch.step {
                collect_expr_for_template_uses(acc, expr.as_ref());
            }
        }
        Expr::Page(_) => {}
        Expr::TeachingValue { value } => {
            collect_value_for_template_uses(acc, value);
        }
    }
}

fn collect_predicate_for_template_uses(acc: &mut Vec<serde_json::Value>, pred: &Predicate) {
    match pred {
        Predicate::Comparison { value, .. } => {
            let v = value.to_value();
            collect_value_for_template_uses(acc, &v);
        }
        Predicate::And { args } | Predicate::Or { args } => {
            for a in args {
                collect_predicate_for_template_uses(acc, a);
            }
        }
        Predicate::Not { predicate } => {
            collect_predicate_for_template_uses(acc, predicate.as_ref())
        }
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                collect_predicate_for_template_uses(acc, inner.as_ref());
            }
        }
        Predicate::True | Predicate::False => {}
    }
}

fn collect_value_for_template_uses(acc: &mut Vec<serde_json::Value>, v: &Value) {
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
                collect_value_for_template_uses(acc, x);
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_value_for_template_uses(acc, x);
            }
        }
        _ => {}
    }
}

pub fn is_plasm_dag_candidate(expressions: &[String]) -> bool {
    if expressions.len() != 1 {
        return false;
    }
    let src = expressions[0].trim();
    src.lines().any(|line| {
        let line = strip_line_comment(line).trim();
        !line.is_empty() && split_assignment_at_top_level(line).is_some()
    }) || src.contains("=>")
        || peel_postfix_suffixes(src)
            .map(|(_, ops)| !ops.is_empty())
            .unwrap_or(false)
}

pub fn split_bare_plasm_roots(src: &str) -> Option<Vec<String>> {
    let src = src.trim();
    if src.is_empty() || src.contains("=>") {
        return None;
    }
    let stmts = collect_program_statement_lines(src).ok()?;
    if stmts.len() != 1 {
        return None;
    }
    let sole = stmts[0].trim();
    if split_assignment_at_top_level(sole).is_some() {
        return None;
    }
    let parts = split_top_level(sole, ',').ok()?;
    if parts.len() <= 1 {
        return None;
    }
    let parts: Vec<String> = parts
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (parts.len() > 1).then_some(parts)
}

pub fn compile_plasm_dag_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    compile_plasm_dag_to_plan_inner(pipeline, symbol_map_cross_cache, session, name, source)
        .map_err(|err| {
            flattened_program_newline_diagnostic(source)
                .map(|hint| format!("{hint}\n\nOriginal parse error: {err}"))
                .unwrap_or(err)
        })
}

fn compile_plasm_dag_to_plan_inner(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    let mut state = CompileState::new(pipeline, symbol_map_cross_cache);
    let statements = collect_program_statement_lines(source)?;
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
    let roots = final_roots.ok_or_else(|| {
        "Plasm program needs a final line of bare roots (comma-separated expressions or node labels)"
            .to_string()
    })?;
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
    Ok(json!({
        "version": 1,
        "kind": "program",
        "name": name,
        "nodes": nodes,
        "return": return_value,
        "metadata": { "language": "plasm-dag" }
    }))
}

/// One line of surface Plasm (or `a, b` at top level) as a one-line program plan — same shape as
/// [`compile_plasm_dag_to_plan`], so the MCP and HTTP runtimes can always execute through the plan runner.
pub fn compile_plasm_surface_line_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    line: &str,
) -> Result<serde_json::Value, String> {
    let mut state = CompileState::new(pipeline, symbol_map_cross_cache);
    let trimmed = line.trim();
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

/// Trailing `.singleton()` / `.page_size(n)` in postfix peel order become flags on the final node.
fn split_tail_postfix_flags(
    mut ops: Vec<PlasmPostfixOp>,
) -> (Vec<PlasmPostfixOp>, bool, Option<usize>) {
    let mut singleton = false;
    let mut page_size = None;
    while let Some(last) = ops.last() {
        match last {
            PlasmPostfixOp::Singleton => {
                singleton = true;
                ops.pop();
            }
            PlasmPostfixOp::PageSize(n) => {
                page_size = Some(*n);
                ops.pop();
            }
            _ => break,
        }
    }
    (ops, singleton, page_size)
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
            ComputeOp::GroupBy { key, aggregates } => {
                let mut cols = vec![OutputName::new(key.dotted()).map_err(|e| e.to_string())?];
                cols.extend(aggregates.iter().map(|a| a.name.clone()));
                Ok(cols)
            }
            ComputeOp::Sort { .. } | ComputeOp::Limit { .. } => {
                let parent = lookup_dag_node(state, staged, parent_id.as_str()).ok_or_else(|| {
                    format!("template column inference: missing upstream node `{parent_id}`")
                })?;
                infer_render_columns_for_node(session, state, staged, parent)
            }
            ComputeOp::Render { .. } => Err(
                "cannot infer columns from a row-to-text template result; bind a row-producing query/relation/projection, or write explicit `[field,...] <<TAG` columns before the template".into(),
            ),
            ComputeOp::Filter { .. } => {
                Err("filtered compute rows cannot provide inferred template columns".into())
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
        let segs: Vec<String> = path.segments().to_vec();
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
            single_unknown_schema("PlanLimit"),
            *n <= 1,
        )),
        PlasmPostfixOp::Sort { args } => {
            let parts = split_top_level(args, ',')?;
            let key = parts
                .first()
                .ok_or_else(|| "sort(...) requires a field".to_string())?
                .trim();
            if key.is_empty() {
                return Err("sort(...) requires a non-empty field".into());
            }
            let descending = parse_sort_direction(&parts)?;
            let key_fp = FieldPath::from_dotted(key)?;
            if let Some(qe) =
                resolve_qualified_entity_for_dag_source(state, staged, source.to_string())
            {
                validate_compute_paths_for_entity(
                    session,
                    state.cross_cache,
                    &qe,
                    std::slice::from_ref(&key_fp),
                    "sort(...)",
                )?;
            }
            Ok(mk(
                ComputeOp::Sort {
                    key: key_fp,
                    descending,
                },
                single_unknown_schema("PlanSort"),
                false,
            ))
        }
        PlasmPostfixOp::Aggregate { args } => {
            let aggregates = parse_aggregates(args)?;
            if let Some(qe) =
                resolve_qualified_entity_for_dag_source(state, staged, source.to_string())
            {
                let paths: Vec<FieldPath> =
                    aggregates.iter().filter_map(|a| a.field.clone()).collect();
                validate_compute_paths_for_entity(
                    session,
                    state.cross_cache,
                    &qe,
                    &paths,
                    "aggregate(...)",
                )?;
            }
            let schema = schema_from_aggregates("PlanAggregate", &aggregates);
            Ok(mk(ComputeOp::Aggregate { aggregates }, schema, true))
        }
        PlasmPostfixOp::GroupBy { args } => {
            let parts = split_top_level(args, ',')?;
            let key = parts
                .first()
                .ok_or_else(|| "group_by(...) requires a key field".to_string())?
                .trim();
            let rest = if parts.len() <= 1 {
                return Err("group_by(...) requires aggregate specs".into());
            } else {
                parts[1..].join(",")
            };
            let aggregates = parse_aggregates(rest.as_str())?;
            let key_fp = FieldPath::from_dotted(key)?;
            if let Some(qe) =
                resolve_qualified_entity_for_dag_source(state, staged, source.to_string())
            {
                let mut paths = vec![key_fp.clone()];
                paths.extend(aggregates.iter().filter_map(|a| a.field.clone()));
                validate_compute_paths_for_entity(
                    session,
                    state.cross_cache,
                    &qe,
                    &paths,
                    "group_by(...)",
                )?;
            }
            let schema = schema_from_aggregates("PlanGroup", &aggregates);
            Ok(mk(
                ComputeOp::GroupBy {
                    key: key_fp,
                    aggregates,
                },
                schema,
                false,
            ))
        }
        PlasmPostfixOp::Projection { fields } => {
            let mut map = BTreeMap::new();
            for field in parse_field_list(fields)? {
                map.insert(
                    OutputName::new(field.clone())?,
                    FieldPath::from_dotted(&field)?,
                );
            }
            if let Some(qe) =
                resolve_qualified_entity_for_dag_source(state, staged, source.to_string())
            {
                let paths: Vec<FieldPath> = map.values().cloned().collect();
                validate_compute_paths_for_entity(
                    session,
                    state.cross_cache,
                    &qe,
                    &paths,
                    "postfix projection",
                )?;
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

/// Lower `core` plus postfix ops to one or more [`DagNode`]s (surface base + optional compute chain).
fn compile_postfix_plan(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    binding_id: &str,
    full_rhs: &str,
    core: &str,
    ops: Vec<PlasmPostfixOp>,
) -> Result<Vec<DagNode>, String> {
    let (compute_ops, tail_singleton, tail_page_size) = split_tail_postfix_flags(ops);
    let primary = core.trim();

    if compute_ops.is_empty() {
        if state.contains(primary) && (tail_singleton || tail_page_size.is_some()) {
            // Equivalent to applying tail flags on `primary` as a surface expression: lower through a
            // compute node so the bound label stays a row producer with correct entity metadata.
            let staged: &[DagNode] = &[];
            let mut node = if tail_singleton {
                let schema = synthetic_schema_passthrough_rows(session, state, staged, primary)?;
                DagNode {
                    id: binding_id.to_string(),
                    expr: full_rhs.to_string(),
                    singleton: tail_singleton,
                    page_size: tail_page_size,
                    source: DagNodeSource::Compute {
                        source: primary.to_string(),
                        op: ComputeOp::Limit { count: 1 },
                        schema,
                    },
                }
            } else {
                let (fields, schema) =
                    passthrough_identity_projection_fields(session, state, staged, primary)?;
                DagNode {
                    id: binding_id.to_string(),
                    expr: full_rhs.to_string(),
                    singleton: false,
                    page_size: tail_page_size,
                    source: DagNodeSource::Compute {
                        source: primary.to_string(),
                        op: ComputeOp::Project { fields },
                        schema,
                    },
                }
            };
            node.singleton |= tail_singleton;
            node.page_size = tail_page_size.or(node.page_size);
            return Ok(vec![node]);
        }
        let mut node = compile_surface_node(session, state, binding_id, primary)?;
        node.singleton |= tail_singleton;
        node.page_size = tail_page_size.or(node.page_size);
        node.expr = full_rhs.to_string();
        return Ok(vec![node]);
    }

    let mut out: Vec<DagNode> = Vec::new();
    let base_source: String = if state.contains(primary) {
        primary.to_string()
    } else {
        let bid = format!("__plasm_{binding_id}_b0");
        let base = compile_surface_node(session, state, &bid, primary)?;
        out.push(base);
        bid
    };

    let mut cur_source = base_source;
    for (i, op) in compute_ops.iter().enumerate() {
        let nid = if i + 1 == compute_ops.len() {
            binding_id.to_string()
        } else {
            format!("__plasm_{binding_id}_s{i}")
        };
        let mut node =
            postfix_op_to_compute(session, state, &out, op, &cur_source, &nid, full_rhs)?;
        if i + 1 < compute_ops.len() {
            node.expr = format!("{full_rhs}::__step{i}");
        }
        cur_source = nid.clone();
        out.push(node);
    }

    if let Some(last) = out.last_mut() {
        last.singleton |= tail_singleton;
        last.page_size = tail_page_size.or(last.page_size);
        last.expr = full_rhs.to_string();
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
            let columns = parse_field_list(fields.trim())?
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
    let (core, ops) =
        peel_postfix_suffixes(head).map_err(|e| format!("Plasm program `{id}`: {e}"))?;
    let ops_full = ops.clone();
    let (compute_ops, tail_singleton, tail_page_size) = split_tail_postfix_flags(ops);

    let mut prefix: Vec<DagNode> = Vec::new();
    let chain_tail_id: String = if compute_ops.is_empty()
        && state.contains(core.trim())
        && !tail_singleton
        && tail_page_size.is_none()
    {
        core.trim().to_string()
    } else {
        let tmp = format!("__plasm_render_src_{id}");
        prefix = compile_postfix_plan(session, state, &tmp, head, core.trim(), ops_full)
            .map_err(|e| format!("Plasm program `{id}`: {e}"))?;
        prefix
            .last()
            .map(|n| n.id.clone())
            .ok_or_else(|| format!("Plasm program `{id}`: empty render chain"))?
    };

    let columns: Vec<OutputName> = if let Some(cols) = explicit_columns {
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
            .map_err(|e| format!("Plasm program `{id}` template parse: {e}"))?;
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

    let (core, ops) =
        peel_postfix_suffixes(rhs).map_err(|e| format!("Plasm program `{id}`: {e}"))?;
    if ops.is_empty() {
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
        return Ok(vec![compile_surface_node(
            session,
            state,
            id,
            core.as_str(),
        )?]);
    }
    compile_postfix_plan(session, state, id, rhs_display, core.as_str(), ops)
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

fn anchor_expanded_plasm(state: &CompileState<'_>, label: &str) -> Option<String> {
    let node = state.get(label)?;
    match &node.source {
        DagNodeSource::Surface { .. } => Some(node.expr.clone()),
        DagNodeSource::RelationTraversal { expanded_plasm, .. } => Some(expanded_plasm.clone()),
        DagNodeSource::Compute {
            source,
            op: ComputeOp::Project { .. },
            ..
        } => anchor_expanded_plasm(state, source),
        DagNodeSource::Data(_)
        | DagNodeSource::Compute { .. }
        | DagNodeSource::Derive { .. }
        | DagNodeSource::ForEach { .. } => None,
    }
}

fn is_bracket_render_binding(state: &CompileState<'_>, label: &str) -> bool {
    matches!(
        state.get(label).map(|n| &n.source),
        Some(DagNodeSource::Compute {
            op: ComputeOp::Render { .. },
            ..
        })
    )
}

fn plan_render_content_scalar_reference_err(id: &str, expr: &str, label: &str) -> String {
    format!(
        "Plasm program `{id}`: `{expr}` reads generated text from `{label}.content`. That path is a **scalar string** for `=>` derives and capability parameters only — not a final root and not a relation receiver. Return `{label}` if you want the generated text row, or use `{label}.content` only inside string/body/template/object payload positions."
    )
}

fn relation_source_cardinality_from_bound_node(
    state: &CompileState<'_>,
    source_label: &str,
) -> RelationSourceCardinality {
    let Some(node) = state.get(source_label) else {
        return RelationSourceCardinality::RuntimeCheckedSingleton;
    };
    match &node.source {
        DagNodeSource::Surface { parsed, kind, .. } => {
            if matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_)) {
                RelationSourceCardinality::Single
            } else if matches!(kind, PlanNodeKind::Query | PlanNodeKind::Search) {
                RelationSourceCardinality::Many
            } else {
                RelationSourceCardinality::RuntimeCheckedSingleton
            }
        }
        DagNodeSource::RelationTraversal { .. } => RelationSourceCardinality::Many,
        DagNodeSource::Compute {
            source,
            op: ComputeOp::Project { .. },
            ..
        } => relation_source_cardinality_from_bound_node(state, source),
        DagNodeSource::Data(_)
        | DagNodeSource::Compute { .. }
        | DagNodeSource::Derive { .. }
        | DagNodeSource::ForEach { .. } => RelationSourceCardinality::RuntimeCheckedSingleton,
    }
}

/// Resolve relation metadata for a parsed [`Expr::Chain`] (declared CGS relation on the source entity).
fn lookup_relation_chain_meta(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    chain: &plasm_core::ChainExpr,
) -> Result<(QualifiedEntityKey, RelationCardinality), String> {
    let source_entity = chain.source.primary_entity();
    let fed_holder = session.federation_dispatch();
    let cgs: &CGS = match fed_holder.as_ref() {
        Some(fed) => fed.resolve_cgs(source_entity, session.cgs.as_ref()),
        None => session.cgs.as_ref(),
    };
    let ent = cgs.get_entity(source_entity).ok_or_else(|| {
        format!("unknown entity `{source_entity}` (Plasm program relation continuation)")
    })?;
    let rel = ent.relations.get(chain.selector.as_str()).ok_or_else(|| {
        let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
        let sym = map.ident_sym_relation(source_entity, chain.selector.as_str());
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
    let qe = session
        .domain_exposure
        .as_ref()
        .and_then(|e| e.qualified_entity_for_exposed_entity(target_ent))
        .map(QualifiedEntityKey::from)
        .unwrap_or_else(|| QualifiedEntityKey {
            entry_id: session.entry_id.clone(),
            entity: target_ent.to_string(),
        });
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
    if let Some((label, tail)) = longest_matching_bound_prefix(expr, state) {
        if let Some(anchor_plasm) = anchor_expanded_plasm(state, &label) {
            let expanded = format!("{anchor_plasm}.{tail}");
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
                    "Plasm program `{id}` expression parse: {e}\n(hint: `{label}.…` substitutes the Plasm bound to `{label}`; expanded form `{expanded}`)"
                )
            })?;
            if let Expr::Chain(ref chain) = parsed.expr {
                let (target_qe, rel_cardinality) =
                    lookup_relation_chain_meta(session, state.cross_cache, chain)?;
                let source_card = relation_source_cardinality_from_bound_node(state, &label);
                let result_shape = match rel_cardinality {
                    RelationCardinality::Many => crate::plasm_plan::ResultShape::List,
                    RelationCardinality::One => crate::plasm_plan::ResultShape::Single,
                };
                let ir = PlanExprIr {
                    expr: serde_json::to_value(&parsed.expr).map_err(|e| e.to_string())?,
                    projection: parsed.projection.clone(),
                    display_expr: Some(expr.to_string()),
                };
                let plan_relation = PlanRelationTraversal {
                    source: label.clone(),
                    relation: chain.selector.clone(),
                    target: target_qe.clone(),
                    cardinality: rel_cardinality,
                    source_cardinality: source_card,
                    expr: expanded.clone(),
                    ir: ir.clone(),
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
        let tail_trim = tail.trim();
        if is_bracket_render_binding(state, &label)
            && (tail_trim == "content" || tail_trim.starts_with("content."))
        {
            return Err(plan_render_content_scalar_reference_err(id, expr, &label));
        }
        return Err(format!(
            "Plasm program `{id}`: `{label}` is not a Plasm expression anchor — only surface/relation bindings and row-preserving projection bindings can be extended with `{label}.…`; aggregate/render/derive/data/for_each bindings must use postfix transforms or an explicit entity constructor"
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
    .map_err(|e| format!("Plasm program `{id}` expression parse: {e}"))?;
    let uses = collect_template_uses_from_expr(&parsed.expr);
    let (kind, qualified_entity, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    Ok(DagNode {
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
    })
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
                "uses_result": [{ "node": source_label, "as": "source" }],
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

fn flattened_program_newline_diagnostic(src: &str) -> Option<String> {
    for raw in src.lines() {
        let line = strip_line_comment(raw).trim();
        if line.is_empty() || line.contains("<<") {
            continue;
        }
        let Some((_id, rhs)) = split_assignment_at_top_level(line) else {
            continue;
        };
        if has_flattened_assignment_boundary(rhs) || has_flattened_final_root_boundary(rhs) {
            return Some(
                "Plasm program statements must be separated by real newline characters (U+000A) in the `program` string; use one binding per line, then final roots on their own line. Do not separate bindings or final roots with spaces. Send one physical line per binding, e.g. `repo = e2(...)\\ncommits = e1{p4=repo}.limit(20)\\ncommits`. For multiline string parameters, use tagged heredocs such as `p58=<<MAIL_7f3a` followed by body lines and a closing `MAIL_7f3a)`."
                    .to_string(),
            );
        }
    }
    None
}

fn has_flattened_assignment_boundary(s: &str) -> bool {
    let mut depth = 0i32;
    let mut quote = None::<char>;
    for (i, c) in s.char_indices() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            '=' if quote.is_none() && depth == 0 => {
                let before_eq = &s[..i];
                let before_trimmed = before_eq.trim_end();
                let token_start = before_trimmed
                    .char_indices()
                    .rev()
                    .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx + ch.len_utf8()))
                    .unwrap_or(0);
                let label = &before_trimmed[token_start..];
                if token_start > 0 && is_valid_program_label(label) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn has_flattened_final_root_boundary(s: &str) -> bool {
    let mut depth = 0i32;
    let mut quote = None::<char>;
    for (i, c) in s.char_indices() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' | '[' | '{' if quote.is_none() => depth += 1,
            ')' | ']' | '}' if quote.is_none() => depth -= 1,
            c if quote.is_none() && depth == 0 && c.is_whitespace() => {
                let left = s[..i].trim();
                let right = s[i..].trim();
                if !left.is_empty() && starts_like_statement_or_root(right) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn starts_like_statement_or_root(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if matches!(first, 'e' | 'p' | 'm') {
        let mut chars = s.chars();
        chars.next();
        if matches!(chars.next(), Some(c) if c.is_ascii_digit()) {
            return true;
        }
    }
    let token = s
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | '[' | '{' | '.' | '='))
        .next()
        .unwrap_or_default();
    is_valid_program_label(token)
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
        if state.contains(part) {
            roots.push(part.to_string());
        } else {
            let id = format!("return_{}", roots.len() + 1);
            for node in compile_node_expr(session, state, &id, part)? {
                state.insert(node)?;
            }
            roots.push(id);
        }
    }
    Ok(roots)
}

fn require_node(state: &CompileState<'_>, node: &str) -> Result<(), String> {
    if state.contains(node) {
        Ok(())
    } else {
        Err(format!("unknown Plasm program node `{node}`"))
    }
}

fn parse_field_list(fields: &str) -> Result<Vec<String>, String> {
    let out = split_top_level(fields, ',')?
        .into_iter()
        .map(|s| {
            let t = s.trim();
            plasm_core::expr_parser::normalize_nested_projection_field(t)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
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

fn parse_sort_direction(parts: &[&str]) -> Result<bool, String> {
    match parts.len() {
        0 => Err("sort(...) requires a field".to_string()),
        1 => Ok(false),
        2 => {
            let d = parts[1].trim();
            if d.is_empty() {
                return Err(
                    "sort(...) direction must not be empty when a comma is present".to_string(),
                );
            }
            match d.to_ascii_lowercase().as_str() {
                "desc" | "descending" => Ok(true),
                "asc" | "ascending" => Ok(false),
                other => Err(format!(
                    "sort(...) unknown direction `{other}`; use `desc` / `descending` for descending, omit the direction or use `asc` / `ascending` for ascending"
                )),
            }
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
        return Ok((
            PlanValue::EntityRefKey {
                api: String::new(),
                entity: String::new(),
                key: Box::new(PlanValue::Symbol {
                    path: raw.to_string(),
                }),
            },
            Vec::new(),
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
    if let Expr::Chain(chain) = expr {
        let (qe, cardinality) = lookup_relation_chain_meta(session, None, chain)?;
        let shape = match cardinality {
            RelationCardinality::Many => crate::plasm_plan::ResultShape::List,
            RelationCardinality::One => crate::plasm_plan::ResultShape::Single,
        };
        return Ok((PlanNodeKind::Query, qe, EffectClass::Read, shape));
    }

    let (mut kind, entity, effect, shape) = infer_surface_contract_from_expr(expr)?;
    if let Expr::Query(q) = expr {
        if let Some(capability_name) = q.capability_name.as_ref() {
            let fed_holder = session.federation_dispatch();
            let q_cgs: &CGS = match fed_holder.as_ref() {
                Some(fed) => fed.resolve_cgs(q.entity.as_str(), session.cgs.as_ref()),
                None => session.cgs.as_ref(),
            };
            if let Some(cap) = q_cgs.capabilities.get(capability_name.as_str()) {
                if cap.kind == plasm_core::CapabilityKind::Search {
                    kind = PlanNodeKind::Search;
                }
            }
        }
    }
    let qe = session
        .domain_exposure
        .as_ref()
        .and_then(|e| e.qualified_entity_for_exposed_entity(entity.as_str()))
        .map(QualifiedEntityKey::from)
        .unwrap_or_else(|| QualifiedEntityKey {
            entry_id: session.entry_id.clone(),
            entity,
        });
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
            "Expr::TeachingValue is DOMAIN-only and cannot appear in execution plans".to_string(),
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
    use plasm_core::{load_schema, CgsContext, DomainExposureSession, PromptPipelineConfig, CGS};
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
        let exp = DomainExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem", "LangLine"]);
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
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    /// Primary session `entry_id` is `github`, but `LangLine` was exposed from `linear` in DOMAIN
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
        let mut exp = DomainExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
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
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        );
        let plan = compile_plasm_surface_line_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "t",
            "LangLine",
        )
        .expect("compile");
        let qe = &plan["nodes"][0]["qualified_entity"];
        assert_eq!(qe["entry_id"], "linear", "{plan}");
        assert_eq!(qe["entity"], "LangLine");
    }

    /// Matrix analogue: parallel comma roots + search sugar (`lang_derive_map_parallel`, `lang_search`).
    #[test]
    fn splits_bare_comma_plasm_roots() {
        let roots = split_bare_plasm_roots(r#"LangItem, LangItem~"Alpha""#).expect("split");
        assert_eq!(
            roots,
            vec!["LangItem".to_string(), r#"LangItem~"Alpha""#.to_string()]
        );
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
    fn flattened_dag_bindings_get_newline_diagnostic() {
        let session = test_session();
        let source = r#"item = LangItem("i1") lines = item.lines lines"#;
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened",
            source,
        )
        .expect_err("flattened input should fail with diagnostic");
        assert!(
            err.contains("real newline characters") && err.contains("Original parse error"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn flattened_dag_assignment_then_root_gets_newline_diagnostic() {
        let session = test_session();
        let source = r#"item = LangItem("i1") LangItem.sort(score, desc).limit(2)"#;
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened-root",
            source,
        )
        .expect_err("flattened input should fail with diagnostic");
        assert!(
            err.contains("Do not separate bindings or final roots with spaces"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn flattened_dag_with_multiline_quoted_arg_gets_newline_diagnostic_first() {
        let session = test_session();
        let source = "prof = LangItem(\"i1\") LangLine(message=\"long\nbody\")";
        let err = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "flattened-quote",
            source,
        )
        .expect_err("flattened input should fail with diagnostic");
        assert!(
            err.contains("one binding per line") && err.contains("tagged heredocs"),
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

    #[test]
    fn split_bare_plasm_roots_multiline_heredoc_second_root() {
        let src = "LangItem, <<TXT\n,\nTXT";
        let roots = split_bare_plasm_roots(src).expect("roots");
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].trim(), "LangItem");
        assert!(
            roots[1].contains("<<TXT") && roots[1].contains(','),
            "{:?}",
            roots[1]
        );
    }

    /// Matrix: heredoc binding + parallel roots (`lang_heredoc_binding`).
    #[test]
    fn multiline_heredoc_binding_then_parallel_roots_compiles() {
        let session = test_session();
        let source = "body = <<T\nhello\nT\nLangItem, LangLine";
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
        let exp = DomainExposureSession::new(cgs.as_ref(), "github", &["Repository", "Commit"]);
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
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
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

    /// DOMAIN `p#` inside postfix projection must lower to wire field names in `Plan` IR (not
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
        assert_eq!(n1.len(), 2, "{p1:#}");
        assert_eq!(n2.len(), 2, "{p2:#}");
        let last1 = &n1[1];
        let last2 = &n2[1];
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
                        ref key,
                        ref aggregates,
                    },
                ..
            } => {
                assert_eq!(key.dotted(), "owner");
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

    /// DOMAIN `p#` indices are session-global; mixing another entity's symbols into a Commit projection
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
            err.contains("open_issues_count") && err.contains("Commit"),
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
}
