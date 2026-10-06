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
mod helpers;
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
mod static_iteration;
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

use crate::program_rejection::PythonLoweringError;
use crate::program_rejection::{PythonProgramError, PythonSourceError};

fn lower_python_program(
    es: &ExecuteSession,
    source: &str,
    suite: &[Stmt],
) -> Result<PlasmCompBundle, PythonLoweringError> {
    let root = admission::Root::parse(source, suite, es)?;
    let pipeline = PromptPipelineConfig::default();
    let mut lower = Lower {
        imports: &root.imports,
        es,
        program_source: source,
        methods: &root.methods,
        helpers: &root.helpers,
        used_methods: BTreeSet::new(),
        state: CompileState::new(&pipeline, None),
        callbacks: BTreeMap::new(),
        active_callbacks: Vec::new(),
        return_check: None,
        serial: 0,
        value_depth: 0,
        static_expansions: 0,
        frame: LexicalFrame::default(),
        static_sequences: BTreeMap::new(),
        spans: BTreeMap::new(),
    };
    // Upstream lexical scope includes declarations after return. Reserved host
    // bindings cannot be shadowed even by an unreachable local assignment.
    for local in monty_analysis::function_locals(
        source,
        monty_analysis::Span {
            start: root.build.start().to_u32(),
            end: root.build.end().to_u32(),
        },
    )? {
        if local != "self"
            && (root.imports.bindings.contains_key(&local)
                || lower
                    .state
                    .sym_map_for(es)
                    .resolve_session_entity(&local)
                    .is_ok())
        {
            return Err(PythonProgramError::BuildLocalShadowsReservedBinding.into());
        }
    }
    for parameter in root
        .build
        .parameters
        .posonlyargs
        .iter()
        .chain(&root.build.parameters.args)
        .chain(&root.build.parameters.kwonlyargs)
    {
        if parameter.parameter.name.as_str() != "self" {
            let value = parameter.default.as_deref().ok_or(
                crate::program_rejection::PythonLoweringInvariantError::MissingBoundBuildDefault,
            )?;
            let binding = lower.fresh();
            lower.expr(value, Some(&binding))?;
            lower.remember_static_sequence(&binding, value);
            if let Some(annotation) = parameter.parameter.annotation.as_deref() {
                let input = text::inferred_schema(es, &lower.state, &binding, 0)?
                    .row_contract()
                    .map_err(PythonLoweringError::from)?;
                crate::python_compute::check_callback_closed_return(
                    es,
                    annotation,
                    &input,
                    value,
                    &root.imports.source,
                )?;
            }
            lower
                .frame
                .names
                .insert(parameter.parameter.name.to_string(), binding);
        }
    }
    let flow = monty_analysis::function_flow(
        source,
        monty_analysis::Span {
            start: root.build.start().to_u32(),
            end: root.build.end().to_u32(),
        },
    )?;
    for stmt in &flow.statements {
        lower.statement(stmt)?;
    }
    let value = match &flow.exit {
        monty_analysis::FlowExit::Return(Some(value))
        | monty_analysis::FlowExit::Branch {
            source_expression: Some(value),
            ..
        } => value,
        _ => return Err(PythonProgramError::BranchingReturnUnsupported.into()),
    };
    let roots = if let PyExpr::Tuple(tuple) = value {
        tuple
            .elts
            .iter()
            .map(|expression| lower.expr(expression, None))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![lower.expr(value, None)?]
    };
    if roots.is_empty() {
        return Err(PythonProgramError::EmptyReturn.into());
    }
    let nodes = lower
        .state
        .nodes
        .iter()
        .map(|n| super::plan_serialize::lower_plan_node(n))
        .collect::<Vec<_>>();
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
    // A declared compute is valid only after a typed callsite was emitted.
    // The frontend records that witness where lowering succeeds; it never
    // reparses serialized runtime steps to infer whether a declaration was used.
    for name in root.methods.keys() {
        if !lower.used_methods.contains(name) {
            return Err(PythonProgramError::UnusedCompute { name: name.clone() }.into());
        }
    }
    let bundle = PlasmCompBundle::new(artifact)?;

    Ok(bundle)
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
            error: std::sync::Arc::new(
                crate::program_diagnostic::ProgramParseError::SourceBudgetExceeded,
            ),
        });
    }
    let ast =
        ruff_python_parser::parse_module(source).map_err(|error| ProgramStageError::Parse {
            correction: format!("Python syntax: {error}"),
            span_offset: Some(u32::from(error.location.start()) as usize),
            error: std::sync::Arc::new(crate::program_diagnostic::ProgramParseError::Python(error)),
        })?;
    let bundle = lower_python_program(es, source, ast.suite())
        .map_err(|error| ProgramStageError::PythonLowering { error })?;
    crate::plasm_plan_run::evaluate_plasm_comp_dry(es, &bundle)?;
    Ok(bundle)
}

#[derive(Default)]
struct LexicalFrame {
    depth: usize,
    row: Option<String>,
    ports: BTreeSet<String>,
    quantifiers: BTreeMap<String, String>,
    facts: refinements::Facts,
    names: BTreeMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExpressionPlacement {
    /// The expression itself is a host DAG operation.
    Host,
    /// A Python lazy branch contains a host dependency and needs scoped lowering.
    LazyHost,
    /// Monty owns evaluation; captured DAG values are explicit inputs.
    PythonValue,
}

impl LexicalFrame {
    fn nested(&self, row: String, names: BTreeMap<String, String>) -> Self {
        let mut ports = self.ports.clone();
        ports.insert(row.clone());
        Self {
            depth: self.depth + 1,
            row: Some(row),
            ports,
            quantifiers: BTreeMap::new(),
            facts: self.facts.clone(),
            names,
        }
    }
}

struct Lower<'a> {
    imports: &'a crate::python_datetime::Imports,
    es: &'a ExecuteSession,
    program_source: &'a str,
    methods: &'a BTreeMap<String, String>,
    helpers: &'a BTreeMap<String, ruff_python_ast::StmtFunctionDef>,
    used_methods: BTreeSet<String>,
    state: CompileState<'a>,
    callbacks: BTreeMap<String, callbacks::Callback>,
    active_callbacks: Vec<String>,
    return_check: Option<(Box<PyExpr>, plasm_core::value_contract::ValueContract)>,
    serial: usize,
    frame: LexicalFrame,
    static_sequences: BTreeMap<String, Vec<PyExpr>>,
    value_depth: usize,
    static_expansions: usize,
    spans: BTreeMap<String, serde_json::Value>,
}
impl Lower<'_> {
    fn expression_placement(&self, expression: &PyExpr) -> ExpressionPlacement {
        if self.immediate_host_dependency(expression) {
            ExpressionPlacement::Host
        } else if self.lazy_host_dependency(expression) {
            ExpressionPlacement::LazyHost
        } else {
            ExpressionPlacement::PythonValue
        }
    }

    fn fresh(&mut self) -> String {
        self.serial += 1;
        format!("__py{}", self.serial)
    }
    fn fresh_parameter(&mut self, purpose: &str) -> String {
        loop {
            let candidate = format!("{purpose}{}", self.fresh());
            if !self.state.contains(&candidate)
                && !self.frame.names.contains_key(&candidate)
                && !self.methods.contains_key(&candidate)
                && !self.frame.quantifiers.contains_key(&candidate)
            {
                return candidate;
            }
        }
    }
    fn insert(&mut self, node: DagNode) -> Result<String, PythonLoweringError> {
        let id = node.id.clone();
        self.state.insert(node)?;
        Ok(id)
    }
    fn expr(&mut self, e: &PyExpr, label: Option<&str>) -> Result<String, PythonLoweringError> {
        let id = self.lower_expr(e, label)?;
        self.spans.entry(id.clone()).or_insert_with(|| span(e));
        Ok(id)
    }
    fn lower_expr(
        &mut self,
        e: &PyExpr,
        label: Option<&str>,
    ) -> Result<String, PythonLoweringError> {
        if let Some(n) = name(e) {
            let n = self.scoped_binding(n).to_owned();
            if !self.state.contains(&n) {
                return Err(at(
                    e,
                    PythonSourceError::UnknownLocalRowset {
                        binding: n.to_owned(),
                    },
                ));
            }
            let index = self.state.labels[&n];
            let target = self.state.nodes[index].id.clone();
            if let Some(label) = label {
                std::sync::Arc::make_mut(&mut self.state.labels).insert(label.into(), index);
            }
            return Ok(target);
        }
        let id = label.map(str::to_owned).unwrap_or_else(|| self.fresh());
        if self.expression_placement(e) != ExpressionPlacement::Host {
            return self.record_value(e, &id);
        }

        if let PyExpr::Attribute(field) = e {
            let source = self.expr(&field.value, None)?;
            let qe = self.binding_relation_owner(&source);
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
            return Err(at(e, PythonSourceError::ExpectedRowsetExpression));
        };
        if name(&call.func).is_some_and(|n| self.callbacks.contains_key(n)) {
            return self.callback_value_call(e, call, &id);
        }
        let PyExpr::Attribute(attr) = &*call.func else {
            return Err(at(e, PythonSourceError::DynamicCall));
        };
        if let Some(token) = name(&attr.value) {
            if let Ok(owner) = self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(token)
            {
                use catalog_operations::{
                    resolve_taught_method, CatalogReadKind, ReadSelection, ResolvedCatalogMethod,
                };
                if let Some(kind) = CatalogReadKind::primary(attr.attr.as_str()) {
                    return self.read(e, call, ReadSelection::Primary(kind), owner, &id);
                }
                return match resolve_taught_method(
                    self.es,
                    &self.state,
                    e,
                    attr.attr.as_str(),
                    &owner,
                )? {
                    ResolvedCatalogMethod::Read(read) => {
                        self.read(e, call, ReadSelection::Taught(read), owner, &id)
                    }
                    ResolvedCatalogMethod::Write(write) => {
                        self.write(e, call, owner, write, None, &id)
                    }
                };
            }
        }
        if name(&attr.value) == Some("self") {
            if self.helpers.contains_key(attr.attr.as_str()) {
                return self.helper_call(e, call, attr.attr.as_str(), &id);
            }
            return self.text_compute(e, call, attr.attr.as_str(), &id);
        }
        if self
            .state
            .sym_map_for(self.es)
            .resolve_session_method(attr.attr.as_str())
            .is_ok()
        {
            let source = self.expr(&attr.value, None)?;
            let contract = super::binding_contract(&self.state, &source).ok_or(
                crate::program_rejection::PythonLoweringInvariantError::ReceiverContractMissing,
            )?;
            if !contract.supports_method_invoke()
                || !contract.row_cardinality.permits_scalar_field_extract()
            {
                return Err(at(
                    e,
                    PythonSourceError::WriteReceiverNeedsIdentitySingleton,
                ));
            }
            let owner = plasm_core::symbol_tuning::EntityBinding {
                entry_id: contract.row_entity.entry_id.as_str().into(),
                entity: contract.row_entity.entity.as_str().into(),
            };
            let resolved = catalog_operations::resolve_taught_method(
                self.es,
                &self.state,
                e,
                attr.attr.as_str(),
                &owner,
            )?;
            let catalog_operations::ResolvedCatalogMethod::Write(write) = resolved else {
                return Err(at(
                    e,
                    PythonSourceError::ExpectedWriteMethod {
                        method: attr.attr.to_string(),
                    },
                ));
            };
            return self.write(e, call, owner, write, Some(&source), &id);
        }
        let operation =
            row_operations::RowOperation::parse(attr.attr.as_str()).ok_or_else(|| {
                at(
                    e,
                    PythonSourceError::UnknownRowOperation {
                        operation: attr.attr.to_string(),
                    },
                )
            })?;
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
fn string(e: &PyExpr) -> Result<String, PythonLoweringError> {
    match e {
        PyExpr::StringLiteral(value) => Ok(value.value.to_str().to_owned()),
        PyExpr::BinOp(binary) if binary.op == ruff_python_ast::Operator::Add => {
            Ok(string(&binary.left)? + &string(&binary.right)?)
        }
        _ => Err(at(e, PythonSourceError::ExpectedStringLiteral)),
    }
}

fn integer(e: &PyExpr) -> Result<i64, PythonLoweringError> {
    if let PyExpr::NumberLiteral(n) = e {
        if let ruff_python_ast::Number::Int(i) = &n.value {
            return i.to_string().parse().map_err(|source| {
                at(
                    e,
                    PythonSourceError::IntegerOutOfRange {
                        literal: i.to_string(),
                        source,
                    },
                )
            });
        }
    }
    Err(at(e, PythonSourceError::ExpectedIntegerLiteral))
}
fn literal(e: &PyExpr) -> Result<plasm_core::Value, PythonLoweringError> {
    literal_operands::LiteralOperand::classify(e)
        .ok_or_else(|| at(e, PythonSourceError::ExpectedScalarLiteral))?
        .scalar(e)
}

fn at(n: &impl Ranged, error: impl Into<PythonLoweringError>) -> PythonLoweringError {
    error
        .into()
        .with_span((u32::from(n.start()), u32::from(n.end())))
}

fn input_at(
    n: &impl Ranged,
    error: crate::program_rejection::PythonInputError,
) -> PythonLoweringError {
    PythonLoweringError::input_at(error, (u32::from(n.start()), u32::from(n.end())))
}

fn span(n: &impl Ranged) -> serde_json::Value {
    serde_json::json!({"start":u32::from(n.start()), "end":u32::from(n.end())})
}
