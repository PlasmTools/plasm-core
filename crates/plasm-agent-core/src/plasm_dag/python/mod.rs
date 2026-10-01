//! Static Python frontend. No user module, decorator or class is executed.
pub(crate) mod admission;
mod assembly;
mod body;
mod branching;
pub(crate) mod build_statements;
mod callback_flow;
mod callbacks;
pub(crate) mod catalog_operations;
mod expression_captures;
mod fanout;
mod inputs;
mod iteration;
pub(crate) mod literal_operands;
#[cfg(test)]
mod literal_tests;
mod membership;
pub(crate) mod projection;
pub(crate) mod quantifiers;
mod reads;
pub(crate) mod reductions;
mod refinements;
pub(crate) mod relation_operations;
pub(crate) mod row_operations;
mod statements;
pub(super) mod text;
mod value_expressions;
mod value_type;
mod writes;
use super::prelude::*;
use super::types::{CompileState, DagNode};
use crate::plasm_comp_bundle::PlasmCompBundle;
use plasm_core::plasm_monad::*;
use ruff_python_ast::{Expr as PyExpr, Stmt};
use ruff_text_size::Ranged;

fn lower_python_program(es: &ExecuteSession, source: &str) -> Result<PlasmCompBundle, String> {
    if source.len() > 32_768 {
        return Err("Python program exceeds 32 KiB source budget".into());
    }
    let ast = ruff_python_parser::parse_module(source).map_err(|e| format!("Python parse: {e}"))?;
    let root = admission::Root::parse(source, ast.suite(), es)?;
    let pipeline = PromptPipelineConfig::default();
    let mut lower = Lower {
        imports: &root.imports,
        es,
        program_source: source,
        methods: &root.methods,
        state: CompileState::new(&pipeline, None),
        callbacks: BTreeMap::new(),
        active_callbacks: Vec::new(),
        serial: 0,
        row_scope: None,
        quantifier_names: BTreeMap::new(),
        branch_types: Default::default(),
        value_depth: 0,
        scope_depth: 0,
        scope_row: None,
        scope_names: BTreeMap::new(),
        spans: BTreeMap::new(),
    };
    let mut roots = None;
    for stmt in &root.build.body {
        if roots.is_some() {
            return Err(at(stmt, "statements after return are not admitted"));
        }
        if let Some(s) = lower.statement(stmt)? {
            let value = s
                .value
                .as_deref()
                .ok_or_else(|| at(stmt, "return requires a rowset"))?;
            if let PyExpr::Tuple(t) = value {
                roots = Some(
                    t.elts
                        .iter()
                        .map(|e| lower.expr(e, None))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            } else {
                roots = Some(vec![lower.expr(value, None)?]);
            }
        }
    }
    let roots = roots.ok_or("build requires an explicit return")?;
    if roots.is_empty() {
        return Err("return must contain at least one rowset".into());
    }
    let nodes = lower
        .state
        .nodes
        .iter()
        .map(|n| super::plan_serialize::lower_plan_node(n))
        .collect::<Result<Vec<_>, _>>()?;
    let ret = if roots.len() > 1 {
        crate::plasm_plan::PlanReturn::Parallel {
            nodes: roots.clone(),
        }
    } else {
        crate::plasm_plan::PlanReturn::Node {
            node: roots[0].clone(),
        }
    };
    let mut plan =
        crate::plasm_plan::Plan::from_nodes(Some(root.name), nodes, ret, BTreeMap::new());
    super::plan_serialize::stamp_plan_uses_result_qualified_entities(&mut plan)?;
    let validated = crate::plasm_plan::validate_plan_artifact(&plan)?;
    let mut artifact = crate::plasm_comp_wire::plasm_comp_from_validated(&validated);
    crate::plan_session_provisions::seal(es, validated.nodes(), &mut artifact.comp.bind)?;
    artifact
        .comp
        .metadata
        .insert("source_language".into(), serde_json::json!("python"));
    artifact
        .comp
        .metadata
        .insert("python_source_spans".into(), serde_json::json!(lower.spans));
    // Every declared compute must have a typed DAG callsite. This prevents
    // uninstantiated Row helpers from escaping admission without a source schema.
    let mut used = std::collections::BTreeSet::new();
    let mut comps = vec![&artifact.comp];
    while let Some(comp) = comps.pop() {
        for step in comp.steps.values() {
            match step {
                plasm_core::PlasmStepPayload::Map(map) => {
                    if let ComputeOp::Python { source, .. } = &map.compute.op {
                        used.insert(source.as_str());
                    }
                }
                plasm_core::PlasmStepPayload::MapBody(body) => comps.push(&body.body),
                _ => {}
            }
        }
    }
    for (name, source) in &root.methods {
        if !used.contains(source.as_str()) {
            return Err(format!("compute {name} requires a typed DAG callsite"));
        }
    }
    let bundle = PlasmCompBundle::new(artifact)?;

    Ok(bundle)
}

pub(crate) fn compile_python_program(
    es: &ExecuteSession,
    source: &str,
) -> Result<PlasmCompBundle, String> {
    compile_python_program_checked(es, source).map_err(String::from)
}

pub(crate) fn compile_python_program_checked(
    es: &ExecuteSession,
    source: &str,
) -> Result<PlasmCompBundle, crate::program_diagnostic::ProgramStageError> {
    use crate::program_diagnostic::ProgramStageError;
    if source.len() > 32_768 {
        return Err(ProgramStageError::Parse {
            correction: "Python program exceeds 32 KiB source budget".into(),
            span_offset: None,
        });
    }
    ruff_python_parser::parse_module(source).map_err(|error| ProgramStageError::Parse {
        correction: format!("Python syntax: {error}. Submit one class derived from Program with an indented build(self) method and an explicit return."),
        span_offset: Some(u32::from(error.location.start()) as usize),
    })?;
    let bundle = lower_python_program(es, source)
        .map_err(|correction| ProgramStageError::Type { correction })?;
    crate::plasm_plan_run::evaluate_plasm_comp_dry(es, &bundle).map_err(|correction| {
        ProgramStageError::Plan {
            correction: format!("Python plan: {correction}"),
        }
    })?;
    Ok(bundle)
}

struct Lower<'a> {
    imports: &'a crate::python_datetime::Imports,
    es: &'a ExecuteSession,
    program_source: &'a str,
    methods: &'a BTreeMap<String, String>,
    state: CompileState<'a>,
    callbacks: BTreeMap<String, callbacks::Callback>,
    active_callbacks: Vec<String>,
    serial: usize,
    scope_depth: usize,
    scope_row: Option<String>,
    quantifier_names: BTreeMap<String, String>,
    branch_types: refinements::Facts,
    value_depth: usize,
    scope_names: BTreeMap<String, String>,
    row_scope: Option<fanout::RowScope>,
    spans: BTreeMap<String, serde_json::Value>,
}
impl Lower<'_> {
    fn fresh(&mut self) -> String {
        self.serial += 1;
        format!("__py{}", self.serial)
    }
    fn fresh_parameter(&mut self, purpose: &str) -> String {
        loop {
            let candidate = format!("{purpose}{}", self.fresh());
            if !self.state.contains(&candidate)
                && !self.scope_names.contains_key(&candidate)
                && !self.methods.contains_key(&candidate)
                && !self.quantifier_names.contains_key(&candidate)
            {
                return candidate;
            }
        }
    }
    fn insert(&mut self, node: DagNode) -> Result<String, String> {
        let id = node.id.clone();
        self.state.insert(node)?;
        Ok(id)
    }
    fn expr(&mut self, e: &PyExpr, label: Option<&str>) -> Result<String, String> {
        let id = self.lower_expr(e, label)?;
        self.spans.entry(id.clone()).or_insert_with(|| span(e));
        Ok(id)
    }
    fn lower_expr(&mut self, e: &PyExpr, label: Option<&str>) -> Result<String, String> {
        if let Some(n) = name(e) {
            let n = self.scoped_binding(n).to_owned();
            if !self.state.contains(&n) {
                return Err(at(e, "unknown local rowset"));
            }
            let index = self.state.labels[&n];
            let target = self.state.nodes[index].id.clone();
            if let Some(label) = label {
                std::sync::Arc::make_mut(&mut self.state.labels).insert(label.into(), index);
            }
            return Ok(target);
        }
        let id = label.map(str::to_owned).unwrap_or_else(|| self.fresh());
        if !self.deferred_expression(e) {
            return self.record_value(e, &id);
        }

        if let PyExpr::Attribute(field) = e {
            let source = self.expr(&field.value, None)?;
            let qe = super::schema_validate::resolve_qualified_entity_for_dag_source(
                &self.state,
                &[],
                source.clone(),
            );
            if let Some(qe) = &qe {
                if super::relation::resolve_relation_wire_on_entity(
                    self.es,
                    self.state.cross_cache,
                    qe,
                    field.attr.as_str(),
                    Some(plasm_core::ProgramBindingLabel(&source)),
                )
                .is_some()
                {
                    return relation_operations::RelationOperation::Navigate.lower(
                        self,
                        e,
                        &source,
                        field.attr.as_str(),
                        &id,
                    );
                }
            }
            return self.record_value(e, &id);
        }
        let PyExpr::Call(call) = e else {
            return Err(at(e, "expected a catalog read or rowset operation"));
        };
        if name(&call.func).is_some_and(|n| self.callbacks.contains_key(n)) {
            return self.callback_value_call(e, call, &id);
        }
        let PyExpr::Attribute(attr) = &*call.func else {
            return Err(at(e, "dynamic calls are not admitted"));
        };
        if let Some(token) = name(&attr.value) {
            if let Ok(owner) = self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(token)
            {
                use catalog_operations::{CatalogOperation, CatalogReadKind};
                let operation = if let Some(read) = CatalogReadKind::primary(attr.attr.as_str()) {
                    CatalogOperation::Read(read)
                } else {
                    let symbols = self.state.sym_map_for(self.es);
                    let method = symbols
                        .resolve_session_method(attr.attr.as_str())
                        .map_err(|error| at(e, &error.to_string()))?;
                    let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
                        self.es,
                        method.entry_id.as_str(),
                        method.domain.as_str(),
                    )?;
                    let cap = cgs
                        .get_capability(method.capability.as_str())
                        .ok_or("missing method capability")?;
                    CatalogOperation::from_kind(cap.kind)
                };
                return match operation {
                    CatalogOperation::Read(_) => self.read(e, call, attr.attr.as_str(), owner, &id),
                    CatalogOperation::Write(_) => {
                        self.write(e, call, attr.attr.as_str(), owner, None, &id)
                    }
                };
            }
        }
        if name(&attr.value) == Some("self") {
            return self.text_compute(e, call, attr.attr.as_str(), &id);
        }
        if self
            .state
            .sym_map_for(self.es)
            .resolve_session_method(attr.attr.as_str())
            .is_ok()
        {
            let source = self.expr(&attr.value, None)?;
            let contract =
                super::binding_contract(&self.state, &source).ok_or("missing receiver contract")?;
            if !contract.supports_method_invoke()
                || !contract.row_cardinality.permits_scalar_field_extract()
            {
                return Err(at(
                    e,
                    "write receiver requires a proven singleton with entity identity",
                ));
            }
            let owner = plasm_core::symbol_tuning::EntityBinding {
                entry_id: contract.row_entity.entry_id.as_str().into(),
                entity: contract.row_entity.entity.as_str().into(),
            };
            return self.write(e, call, attr.attr.as_str(), owner, Some(&source), &id);
        }
        let operation = row_operations::RowOperation::parse(attr.attr.as_str())
            .ok_or_else(|| at(e, "unsupported rowset operation"))?;
        self.row_operation(operation, e, call, &attr.value, &id)
    }
}
fn name(e: &PyExpr) -> Option<&str> {
    if let PyExpr::Name(n) = e {
        Some(n.id.as_str())
    } else {
        None
    }
}
fn string(e: &PyExpr) -> Result<String, String> {
    match e {
        PyExpr::StringLiteral(value) => Ok(value.value.to_str().to_owned()),
        PyExpr::BinOp(binary) if binary.op == ruff_python_ast::Operator::Add => {
            Ok(string(&binary.left)? + &string(&binary.right)?)
        }
        _ => Err(at(e, "expected a string literal or literal concatenation")),
    }
}

fn integer(e: &PyExpr) -> Result<i64, String> {
    if let PyExpr::NumberLiteral(n) = e {
        if let ruff_python_ast::Number::Int(i) = &n.value {
            return i
                .to_string()
                .parse()
                .map_err(|_| at(e, "integer out of range"));
        }
    }
    Err(at(e, "expected an integer literal"))
}
fn literal(e: &PyExpr) -> Result<plasm_core::Value, String> {
    literal_operands::LiteralOperand::classify(e)
        .ok_or_else(|| {
            at(
                e,
                "expected a string, finite number, boolean or null literal",
            )
        })?
        .scalar(e)
}

fn at(n: &impl Ranged, message: &str) -> String {
    format!(
        "Python bytes {}..{}: {message}",
        u32::from(n.start()),
        u32::from(n.end())
    )
}

fn span(n: &impl Ranged) -> serde_json::Value {
    serde_json::json!({"start":u32::from(n.start()), "end":u32::from(n.end())})
}
