//! Close directly called local helpers through the same upstream checker used
//! for the enclosing compute. No Python expression is typed by this module.
use super::{body_diagnostics, declarations::Declarations, Decoder, Type};
use monty_analysis::{AnalysisLimits, AnalysisOutcome, AnalysisRequest, Graph, Node, Span, TypeId};
use ruff_python_ast::{
    visitor::{self, Visitor},
    Expr, Stmt, StmtFunctionDef,
};
use ruff_text_size::Ranged;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

struct Helper {
    definition: StmtFunctionDef,
    inputs: BTreeMap<String, Type>,
    output: Option<CheckedType>,
}

struct CheckedType {
    contract: Type,
    literal_annotation: Option<String>,
}

fn literal_annotation(graph: &Graph, id: TypeId, depth: usize) -> Option<String> {
    if depth >= 64 {
        return None;
    }
    match graph.nodes.get(id.0 as usize)? {
        Node::StringLiteral(value) => {
            Some(format!("Literal[{}]", serde_json::to_string(value).ok()?))
        }
        Node::IntLiteral(value) => Some(format!("Literal[{value}]")),
        Node::BoolLiteral(value) => Some(format!(
            "Literal[{}]",
            if *value { "True" } else { "False" }
        )),
        Node::None => Some("None".into()),
        Node::Alias(inner) => literal_annotation(graph, *inner, depth + 1),
        Node::Union(items) => items
            .iter()
            .map(|inner| literal_annotation(graph, *inner, depth + 1))
            .collect::<Option<Vec<_>>>()
            .map(|items| items.join(" | ")),
        _ => None,
    }
}

impl Helper {
    fn observe_input(&mut self, parameter: &str, actual: Type) -> bool {
        match self.inputs.entry(parameter.into()) {
            Entry::Vacant(slot) => {
                slot.insert(actual);
                true
            }
            Entry::Occupied(mut slot) => {
                let joined = Type::join(slot.get().clone(), actual);
                if *slot.get() == joined {
                    return false;
                }
                slot.insert(joined);
                self.output = None;
                true
            }
        }
    }
}

struct Calls<'a> {
    names: &'a BTreeSet<String>,
    found: Vec<ruff_python_ast::ExprCall>,
}

impl<'a> Visitor<'a> for Calls<'_> {
    fn visit_expr(&mut self, expression: &'a Expr) {
        if let Expr::Call(call) = expression {
            if let Expr::Name(name) = call.func.as_ref() {
                if self.names.contains(name.id.as_str()) {
                    self.found.push(call.clone());
                }
            }
        }
        visitor::walk_expr(self, expression);
    }
}

fn source_request(source: &str, stubs: &str, target: Option<Span>) -> AnalysisRequest {
    AnalysisRequest {
        source: source.into(),
        stubs: Some(stubs.into()),
        targets: target.into_iter().collect(),
        limits: AnalysisLimits::default(),
    }
}

fn checked_type(
    result: monty_analysis::AnalysisResult,
    source: &str,
    declarations: &Declarations,
) -> Result<Option<CheckedType>, String> {
    let graph = match result.outcome {
        AnalysisOutcome::Inferred(graph) => graph,
        AnalysisOutcome::Rejected(errors) => return Err(body_diagnostics(errors, source)),
    };
    if graph.nodes.contains(&Node::Unknown) {
        return Ok(None);
    }
    let decoder = Decoder {
        graph: &graph,
        declarations,
    };
    let exact = graph
        .roots
        .iter()
        .copied()
        .map(|id| literal_annotation(&graph, id, 0))
        .collect::<Option<Vec<_>>>()
        .map(|items| items.join(" | "));
    let contract = graph
        .roots
        .iter()
        .copied()
        .map(|id: TypeId| decoder.decode(id, 0))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .reduce(Type::join)
        .ok_or_else(|| "checker supplied no helper contract".to_owned())?;
    Ok(Some(CheckedType {
        contract,
        literal_annotation: exact,
    }))
}

fn helper_definitions(source: &str) -> Result<BTreeMap<String, StmtFunctionDef>, String> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let Some(Stmt::FunctionDef(outer)) = parsed.suite().last() else {
        return Err("missing generated compute function".into());
    };
    let mut definitions = BTreeMap::new();
    for stmt in &outer.body {
        if let Stmt::FunctionDef(function) = stmt {
            if definitions
                .insert(function.name.to_string(), function.clone())
                .is_some()
            {
                return Err("local helper names must be unique in one compute".into());
            }
        }
    }
    Ok(definitions)
}

pub(super) fn has_local_calls(source: &str) -> Result<bool, String> {
    let definitions = helper_definitions(source)?;
    if definitions.is_empty() {
        return Ok(false);
    }
    let names = definitions.keys().cloned().collect::<BTreeSet<_>>();
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let Some(Stmt::FunctionDef(outer)) = parsed.suite().last() else {
        unreachable!()
    };
    let mut calls = Calls {
        names: &names,
        found: Vec::new(),
    };
    calls.visit_body(&outer.body);
    Ok(!calls.found.is_empty())
}

fn insert_annotations(
    source: &str,
    helpers: &BTreeMap<String, Helper>,
    declarations: &mut Declarations,
) -> Result<String, String> {
    let mut insertions = Vec::new();
    for helper in helpers.values() {
        let def = &helper.definition;
        for param in def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
        {
            if param.parameter.annotation.is_none() {
                if let Some(ty) = helper.inputs.get(param.parameter.name.as_str()) {
                    insertions.push((
                        param.parameter.name.end().to_usize(),
                        format!(": {}", declarations.render(ty, 0)?),
                    ));
                }
            }
        }
        if def.returns.is_none() {
            if let Some(output) = &helper.output {
                let annotation = match &output.literal_annotation {
                    Some(exact) => exact.clone(),
                    None => declarations.render(&output.contract, 0)?,
                };
                insertions.push((def.parameters.end().to_usize(), format!(" -> {annotation}")));
            }
        }
    }
    insertions.sort_by(|a, b| b.0.cmp(&a.0));
    let mut annotated = source.to_owned();
    for (offset, text) in insertions {
        annotated.insert_str(offset, &text);
    }
    Ok(annotated)
}

/// Specialize only direct local helper calls. Each argument and helper body is
/// inferred by Monty; synthesized annotations make that evidence available to
/// Monty's ordinary call checker on the next pass. Unknown is never admitted.
pub(super) fn close_local_calls(
    source: &str,
    declarations: &mut Declarations,
) -> Result<String, String> {
    let definitions = helper_definitions(source)?;
    if definitions.is_empty() {
        return Ok(source.into());
    }
    if definitions.len() > 32 {
        return Err("compute local helper limit exceeded".into());
    }
    let mut helpers = definitions
        .into_iter()
        .map(|(name, definition)| {
            (
                name,
                Helper {
                    definition,
                    inputs: BTreeMap::new(),
                    output: None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let names = helpers.keys().cloned().collect::<BTreeSet<_>>();
    let mut resolved_arguments = BTreeSet::new();
    let mut resolved_defaults = BTreeSet::new();
    for _ in 0..=helpers.len() * 2 {
        let annotated = insert_annotations(source, &helpers, declarations)?;
        let parsed = ruff_python_parser::parse_module(&annotated).map_err(|e| e.to_string())?;
        let Some(Stmt::FunctionDef(outer)) = parsed.suite().last() else {
            unreachable!()
        };
        let mut calls = Calls {
            names: &names,
            found: Vec::new(),
        };
        calls.visit_body(&outer.body);
        if calls.found.len() > 128 {
            return Err("compute local helper call limit exceeded".into());
        }
        let current = helper_definitions(&annotated)?;
        let mut changed = false;
        for (call_index, call) in calls.found.into_iter().enumerate() {
            let Expr::Name(callee) = call.func.as_ref() else {
                unreachable!()
            };
            let Some(def) = current.get(callee.id.as_str()) else {
                continue;
            };
            let mut arguments = call
                .arguments
                .args
                .iter()
                .map(|expr| (None, expr))
                .collect::<Vec<_>>();
            for keyword in &call.arguments.keywords {
                arguments.push((
                    Some(
                        keyword
                            .arg
                            .as_ref()
                            .ok_or("expanded helper arguments cannot be inferred")?
                            .to_string(),
                    ),
                    &keyword.value,
                ));
            }
            let signature = monty::statement_source(&Stmt::FunctionDef(def.clone()));
            let bindings = monty_analysis::bind_arguments(
                &signature,
                &arguments
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>(),
            )?;
            let original = &helpers[callee.id.as_str()].definition;
            let annotated_names = original
                .parameters
                .posonlyargs
                .iter()
                .chain(&original.parameters.args)
                .chain(&original.parameters.kwonlyargs)
                .filter(|param| param.parameter.annotation.is_some())
                .map(|param| param.parameter.name.to_string())
                .collect::<BTreeSet<_>>();
            let supplied = bindings
                .iter()
                .map(|binding| binding.parameter.as_str())
                .collect::<BTreeSet<_>>();
            for param in def
                .parameters
                .posonlyargs
                .iter()
                .chain(&def.parameters.args)
                .chain(&def.parameters.kwonlyargs)
            {
                let name = param.parameter.name.as_str();
                let key = (callee.id.to_string(), name.to_owned());
                if supplied.contains(name)
                    || annotated_names.contains(name)
                    || resolved_defaults.contains(&key)
                {
                    continue;
                }
                let Some(default) = &param.default else {
                    continue;
                };
                let span = Span {
                    start: default.start().to_u32(),
                    end: default.end().to_u32(),
                };
                let result = monty_analysis::analyze(&source_request(
                    &annotated,
                    &declarations.source,
                    Some(span),
                ))?;
                let Some(actual) = checked_type(result, &annotated, declarations)? else {
                    continue;
                };
                changed |= helpers
                    .get_mut(callee.id.as_str())
                    .expect("known helper")
                    .observe_input(name, actual.contract);
                resolved_defaults.insert(key);
            }
            for (argument_index, (binding, (_, expression))) in
                bindings.iter().zip(arguments).enumerate()
            {
                let key = (call_index, argument_index);
                if resolved_arguments.contains(&key) {
                    continue;
                }
                if binding.variadic || matches!(expression, Expr::Starred(_)) {
                    return Err("variadic local helper calls cannot be inferred".into());
                }
                if annotated_names.contains(&binding.parameter) {
                    resolved_arguments.insert(key);
                    continue;
                }
                let span = Span {
                    start: expression.start().to_u32(),
                    end: expression.end().to_u32(),
                };
                let result = monty_analysis::analyze(&source_request(
                    &annotated,
                    &declarations.source,
                    Some(span),
                ))?;
                let Some(actual) = checked_type(result, &annotated, declarations)? else {
                    continue;
                };
                resolved_arguments.insert(key);
                let helper = helpers.get_mut(callee.id.as_str()).expect("known helper");
                changed |= helper.observe_input(&binding.parameter, actual.contract);
            }
        }
        let annotated = insert_annotations(source, &helpers, declarations)?;
        let current = helper_definitions(&annotated)?;
        for (name, def) in current {
            if helpers[&name].output.is_some() || def.returns.is_some() {
                continue;
            }
            let result = monty_analysis::analyze_function_at(
                &source_request(&annotated, &declarations.source, None),
                Span {
                    start: def.start().to_u32(),
                    end: def.end().to_u32(),
                },
            )?;
            if let Some(output) = checked_type(result, &annotated, declarations)? {
                helpers.get_mut(&name).expect("known helper").output = Some(output);
                changed = true;
            }
        }
        if !changed {
            return Ok(annotated);
        }
    }
    Err("compute local helper inference did not converge".into())
}
