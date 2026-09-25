//! Static Python frontend. No user module, decorator or class is executed.
mod admission;
mod body;
mod fanout;
mod inputs;
mod iteration;
#[cfg(test)]
mod literal_tests;
mod membership;
mod projection;
mod reads;
mod reductions;
mod text;
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
        es,
        methods: &root.methods,
        state: CompileState::new(&pipeline, None),
        serial: 0,
        row_scope: None,
        spans: BTreeMap::new(),
    };
    let mut map = None;
    let mut roots = None;
    for stmt in &root.build.body {
        if roots.is_some() {
            return Err(at(stmt, "statements after return are not admitted"));
        }
        match stmt {
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::StringLiteral(_)) => {}
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::Call(_)) => {
                if map.is_some() {
                    return Err(at(
                        stmt,
                        "map must be the final computed binding in this slice",
                    ));
                }
                let id = lower.expr(&s.value, None)?;
                let node = lower.state.get(&id).ok_or("missing statement node")?;
                if !matches!(
                    &node.source,
                    super::types::DagNodeSource::Surface {
                        effect_class: EffectClass::Write | EffectClass::SideEffect,
                        ..
                    } | super::types::DagNodeSource::IterateUntil {
                        effect_class: EffectClass::Write | EffectClass::SideEffect,
                        ..
                    } | super::types::DagNodeSource::ForEach {
                        effect_class: EffectClass::Write | EffectClass::SideEffect,
                        ..
                    }
                ) {
                    return Err(at(stmt, "unused expression statements must be writes"));
                }
            }
            Stmt::Assign(s) if s.targets.len() == 1 => {
                let label = name(&s.targets[0])
                    .ok_or_else(|| at(stmt, "only immutable local assignments are admitted"))?;
                if matches!(
                    label,
                    "self" | "Program" | "compute" | "Value" | "agg" | "_"
                ) || label.starts_with("__")
                    || root.methods.contains_key(label)
                    || lower
                        .state
                        .sym_map_for(es)
                        .resolve_session_entity(label)
                        .is_ok()
                {
                    return Err(at(stmt, "reserved binding name"));
                }
                if map.is_some() {
                    return Err(at(
                        stmt,
                        "map must be the final computed binding in this slice",
                    ));
                }
                if lower.state.contains(label) {
                    return Err(at(stmt, "rebinding is not admitted"));
                }
                if body::is_map(&s.value) {
                    map = Some((label.to_owned(), lower.map(&s.value, &root.methods)?));
                    lower.spans.insert(label.to_owned(), span(&*s.value));
                } else {
                    lower.expr(&s.value, Some(label))?;
                }
            }
            Stmt::Return(s) => {
                let value = s
                    .value
                    .as_deref()
                    .ok_or_else(|| at(stmt, "return requires a rowset"))?;
                if body::is_map(value) {
                    if map.is_some() {
                        return Err(at(stmt, "multiple maps are not admitted"));
                    }
                    let label = lower.fresh();
                    map = Some((label.clone(), lower.map(value, &root.methods)?));
                    lower.spans.insert(label.clone(), span(value));
                    roots = Some(vec![label]);
                } else if let PyExpr::Tuple(t) = value {
                    if map.is_some() {
                        return Err(at(stmt, "parallel map returns are not admitted yet"));
                    }
                    roots = Some(
                        t.elts
                            .iter()
                            .map(|e| lower.expr(e, None))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                } else if let Some((label, _)) = &map {
                    if name(value) != Some(label.as_str()) {
                        return Err(at(stmt, "return must name the map result"));
                    }
                    roots = Some(vec![label.clone()]);
                } else {
                    roots = Some(vec![lower.expr(value, None)?]);
                }
            }
            _ => return Err(at(stmt, "unsupported build statement")),
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
    // The prefix uses the ordinary plan validator and provision seal.
    let prefix_root = map
        .as_ref()
        .map(|(_, b)| b.parent.source.to_string())
        .unwrap_or_else(|| roots[0].clone());
    let ret = if map.is_none() && roots.len() > 1 {
        crate::plasm_plan::PlanReturn::Parallel {
            nodes: roots.clone(),
        }
    } else {
        crate::plasm_plan::PlanReturn::Node { node: prefix_root }
    };
    let mut plan =
        crate::plasm_plan::Plan::from_nodes(Some(root.name), nodes, ret, BTreeMap::new());
    super::plan_serialize::stamp_plan_uses_result_qualified_entities(&mut plan)?;
    let validated = crate::plasm_plan::validate_plan_artifact(&plan)?;
    let mut artifact = crate::plasm_comp_wire::plasm_comp_from_validated(&validated);
    crate::plan_session_provisions::seal(es, validated.nodes(), &mut artifact.comp.bind)?;
    if let Some((label, body)) = map {
        let id = StepId::new(label.clone())?;
        artifact
            .comp
            .bind
            .deps
            .insert(id.clone(), BTreeSet::from([body.parent.source.clone()]));
        if let Some(previous) = validated
            .topological_order()
            .iter()
            .rev()
            .filter_map(|id| {
                validated
                    .nodes()
                    .iter()
                    .find(|n| n.id().as_str() == id.as_str())
            })
            .find(|n| {
                matches!(
                    n.effect_class(),
                    EffectClass::Write | EffectClass::SideEffect
                )
            })
        {
            let previous = StepId::new(previous.id().as_str())?;
            if artifact
                .comp
                .bind
                .deps
                .entry(id.clone())
                .or_default()
                .insert(previous.clone())
            {
                artifact
                    .comp
                    .metadata
                    .entry("program_order_effect_deps".into())
                    .or_insert_with(|| serde_json::json!([]))
                    .as_array_mut()
                    .ok_or("invalid effect dependency metadata")?
                    .push(serde_json::json!([previous.to_string(), id.to_string()]));
            }
        }
        artifact.comp.bind.topo.push(id.clone());
        artifact
            .comp
            .steps
            .insert(label, PlasmStepPayload::MapBody(Box::new(body)));
        artifact.comp.return_ = PlasmReturn::Step { step: id };
        artifact = crate::plasm_comp_wire::plasm_comp_artifact_from_comp(artifact.comp)?;
    }
    artifact
        .comp
        .metadata
        .insert("source_language".into(), serde_json::json!("python"));
    artifact
        .comp
        .metadata
        .insert("python_source_spans".into(), serde_json::json!(lower.spans));
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
    es: &'a ExecuteSession,
    methods: &'a BTreeMap<String, String>,
    state: CompileState<'a>,
    serial: usize,
    row_scope: Option<fanout::RowScope>,
    spans: BTreeMap<String, serde_json::Value>,
}
impl Lower<'_> {
    fn fresh(&mut self) -> String {
        self.serial += 1;
        format!("__py{}", self.serial)
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
            let n = self.scoped_binding(n);
            if !self.state.contains(n) {
                return Err(at(e, "unknown local rowset"));
            }
            if label.is_some() {
                return Err(at(
                    e,
                    "alias assignments are not admitted; use the existing binding",
                ));
            }
            return Ok(n.to_owned());
        }
        let id = label.map(str::to_owned).unwrap_or_else(|| self.fresh());
        if matches!(e, PyExpr::StringLiteral(_) | PyExpr::BinOp(_)) {
            return self.insert(DagNode {
                id,
                expr: String::new(),
                singleton: true,
                page_size: None,
                source: super::types::DagNodeSource::Data(PlanValue::Literal {
                    value: plasm_core::Value::String(string(e)?).try_into()?,
                }),
            });
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
                    let contract = super::binding_contract(&self.state, &source)
                        .ok_or("missing relation source contract")?;
                    if !contract.row_cardinality.permits_scalar_field_extract() {
                        return Err(at(
                            e,
                            "relation dot requires a singleton; use flat_map for plural rows",
                        ));
                    }
                    // Application lowering builds typed row-hole IR directly, preserving
                    // source cardinality, scope and relation proofs without source reparsing.
                    let node = super::binding_continuation::lower_relation_application(
                        self.es,
                        &self.state,
                        &id,
                        "",
                        &source,
                        field.attr.as_str(),
                    )?;
                    return self.insert(node);
                }
            }
            let row_schema =
                super::schema_validate::resolve_immediate_compute_schema(&self.state, &[], &source);
            let path = super::schema_validate::resolve_sort_field_path(
                self.es,
                None,
                qe.as_ref(),
                row_schema.as_ref(),
                &FieldPath::from_dotted(field.attr.as_str())?,
            )?;
            super::schema_validate::validate_compute_paths_for_dag_source(
                self.es,
                &self.state,
                &[],
                &source,
                std::slice::from_ref(&path),
                "field extract",
            )?;
            let contract =
                super::binding_contract(&self.state, &source).ok_or("missing row contract")?;
            let node = super::scalar_extract::lower_binding_scalar_field_dot(
                &id,
                "",
                &source,
                path.segments()[0].clone(),
                contract.row_cardinality,
            )?;
            return self.insert(node);
        }
        let PyExpr::Call(call) = e else {
            return Err(at(e, "expected a catalog read or rowset operation"));
        };
        let PyExpr::Attribute(attr) = &*call.func else {
            return Err(at(e, "dynamic calls are not admitted"));
        };
        if let Some(token) = name(&attr.value) {
            if let Ok(owner) = self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(token)
            {
                return if matches!(attr.attr.as_str(), "query" | "get" | "search") {
                    self.read(e, call, attr.attr.as_str(), owner, &id)
                } else {
                    let symbols = self.state.sym_map_for(self.es);
                    let read = symbols
                        .resolve_session_method(attr.attr.as_str())
                        .ok()
                        .and_then(|method| {
                            crate::catalog_ownership::resolve_cgs_for_entry_entity(
                                self.es,
                                owner.entry_id.as_str(),
                                owner.entity.as_str(),
                            )
                            .ok()
                            .and_then(|cgs| cgs.get_capability(method.capability.as_str()))
                            .map(|cap| {
                                matches!(
                                    cap.kind,
                                    plasm_core::CapabilityKind::Get
                                        | plasm_core::CapabilityKind::Query
                                        | plasm_core::CapabilityKind::Search
                                )
                            })
                        })
                        .unwrap_or(false);
                    if read {
                        self.read(e, call, attr.attr.as_str(), owner, &id)
                    } else {
                        self.write(e, call, attr.attr.as_str(), owner, None, &id)
                    }
                };
            }
        }
        if name(&attr.value) == Some("self") {
            return self.text_compute(e, call, attr.attr.as_str(), &id);
        }
        if attr.attr.as_str() == "page_size" {
            if call.arguments.args.len() != 1
                || !call.arguments.keywords.is_empty()
                || !matches!(&*attr.value, PyExpr::Call(_))
            {
                return Err(at(e, "page_size requires a positive literal bound attached directly to a catalog read call"));
            }
            let size = u32::try_from(integer(&call.arguments.args[0])?)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| at(e, "page_size requires a positive u32"))?;
            let read = self.expr(&attr.value, Some(&id))?;
            let index = *self.state.labels.get(&read).ok_or("missing read node")?;
            let node = Arc::make_mut(&mut self.state.nodes[index]);
            if !matches!(&node.source, super::types::DagNodeSource::Surface { parsed, effect_class: EffectClass::Read, .. } if matches!(parsed.expr, plasm_core::Expr::Query(_)))
            {
                return Err(at(e, "page_size applies to catalog query/search reads"));
            }
            node.page_size = Some(size as usize);
            return Ok(read);
        }
        let source = self.expr(&attr.value, None)?;
        if attr.attr.as_str() == "iterate" {
            return self.iteration(e, call, &source, &id);
        }
        if attr.attr.as_str() == "flat_map" {
            return self.fanout(e, call, &source, &id);
        }
        if self
            .state
            .sym_map_for(self.es)
            .resolve_session_method(attr.attr.as_str())
            .is_ok()
        {
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
        if matches!(attr.attr.as_str(), "aggregate" | "group_by" | "distinct") {
            return self.reduction(e, call, attr.attr.as_str(), &source, &id);
        }
        if attr.attr.as_str() == "select" && !call.arguments.keywords.is_empty() {
            return self.project_aliases(e, call, &source, &id);
        }
        if attr.attr.as_str() == "order_by" {
            if call.arguments.args.len() != 1 || call.arguments.keywords.len() > 1 {
                return Err(at(
                    e,
                    "order_by requires one field and optional descending=True/False",
                ));
            }
            let key = string(&call.arguments.args[0])?;
            let descending = match call.arguments.keywords.first() {
                None => false,
                Some(keyword)
                    if keyword
                        .arg
                        .as_ref()
                        .is_some_and(|arg| arg.as_str() == "descending") =>
                {
                    let PyExpr::BooleanLiteral(value) = &keyword.value else {
                        return Err(at(e, "descending requires a literal Boolean"));
                    };
                    value.value
                }
                _ => return Err(at(e, "order_by only accepts the descending keyword")),
            };
            let node = super::row_suffix::lower_sort_compute(
                self.es,
                &self.state,
                &[],
                &source,
                &id,
                "",
                (&key, descending),
            )?;
            return self.insert(node);
        }
        if !call.arguments.keywords.is_empty() {
            return Err(at(e, "row operation does not accept keyword arguments"));
        }
        let suffix = match attr.attr.as_str() {
            "union" if call.arguments.args.len() == 1 => RowSuffix::Union {
                rhs: self.expr(&call.arguments.args[0], None)?,
            },
            "take" if call.arguments.args.len() == 1 => {
                let count = u32::try_from(integer(&call.arguments.args[0])?)
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| at(e, "take requires a positive u32"))?;
                RowSuffix::Limit { count }
            }
            "select" if !call.arguments.args.is_empty() => RowSuffix::Project {
                fields: call
                    .arguments
                    .args
                    .iter()
                    .map(string)
                    .collect::<Result<_, _>>()?,
            },
            "where" if call.arguments.args.len() == 1 => {
                return self.filter(e, &source, &id, &call.arguments.args[0])
            }
            _ => return Err(at(e, "unsupported rowset operation")),
        };
        let node =
            super::row_suffix_to_compute(self.es, &self.state, &[], &suffix, &source, &id, "")?;
        self.insert(node)
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
    match e {
        PyExpr::UnaryOp(unary)
            if matches!(
                unary.op,
                ruff_python_ast::UnaryOp::UAdd | ruff_python_ast::UnaryOp::USub
            ) =>
        {
            let PyExpr::NumberLiteral(number) = &*unary.operand else {
                return Err(at(e, "signed value requires a numeric literal"));
            };
            let negative = unary.op == ruff_python_ast::UnaryOp::USub;
            match &number.value {
                ruff_python_ast::Number::Int(value) => {
                    // Parse the sign with the magnitude: i64::MIN has no positive i64.
                    let spelling = format!("{}{value}", if negative { "-" } else { "" });
                    Ok(plasm_core::Value::Integer(
                        spelling
                            .parse()
                            .map_err(|_| at(e, "integer out of range"))?,
                    ))
                }
                ruff_python_ast::Number::Float(value) if value.is_finite() => {
                    Ok(plasm_core::Value::Float(if negative {
                        -*value
                    } else {
                        *value
                    }))
                }
                _ => Err(at(e, "expected a finite real number")),
            }
        }
        PyExpr::StringLiteral(_) | PyExpr::BinOp(_) => Ok(plasm_core::Value::String(string(e)?)),
        PyExpr::NumberLiteral(n) => match &n.value {
            ruff_python_ast::Number::Float(value) if value.is_finite() => {
                Ok(plasm_core::Value::Float(*value))
            }
            ruff_python_ast::Number::Int(_) => Ok(plasm_core::Value::Integer(integer(e)?)),
            _ => Err(at(e, "expected a finite real number")),
        },
        PyExpr::NoneLiteral(_) => Ok(plasm_core::Value::Null),
        PyExpr::BooleanLiteral(b) => Ok(plasm_core::Value::Bool(b.value)),
        _ => Err(at(
            e,
            "expected a string, finite number, boolean or null literal",
        )),
    }
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
