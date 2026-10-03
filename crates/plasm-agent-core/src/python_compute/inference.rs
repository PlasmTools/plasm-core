//! Decode upstream Python inference into materialized Plasm contracts.
//! Generated nominal declarations are a reversible map, not carrier-type proofs.
use monty_analysis::{AnalysisLimits, AnalysisOutcome, AnalysisRequest, Graph, Node, TypeId};
use plasm_core::{
    value_contract::{ValueContract as Type, ValueShape},
    FieldType,
};
use ruff_python_ast::Expr;
use ruff_text_size::Ranged;
use std::collections::BTreeMap;

mod declarations;
mod helpers;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) fn infer(
    expression: &str,
    parameter: &str,
    argument: &Type,
    imports: &str,
) -> Result<Type, String> {
    infer_body(
        &format!("\n    return ({expression})"),
        &[(parameter, argument)],
        imports,
    )
}

/// Infer return expressions in their complete upstream-checked lexical context.
/// The final generated return annotation also checks fallthrough paths.
pub(super) fn infer_body(
    body: &str,
    parameters: &[(&str, &Type)],
    imports: &str,
) -> Result<Type, String> {
    let mut declarations = declarations::Declarations::default();
    let parameters = parameters
        .iter()
        .map(|(name, ty)| {
            declarations
                .render(ty, 0)
                .map(|annotation| format!("{name}: {annotation}"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let source = format!("{imports}\ndef __plasm_expression({parameters}):{body}\n");
    let parsed = ruff_python_parser::parse_module(&source).map_err(|e| e.to_string())?;
    let Some(ruff_python_ast::Stmt::FunctionDef(function)) = parsed.suite().last() else {
        return Err("missing generated compute function".into());
    };
    {
        use ruff_python_ast::visitor::{self, Visitor};
        struct Reserved(bool);
        impl<'a> Visitor<'a> for Reserved {
            fn visit_expr(&mut self, expr: &'a Expr) {
                if matches!(expr, Expr::Name(name) if name.id.starts_with("PlasmAnalysisType")) {
                    self.0 = true;
                }
                visitor::walk_expr(self, expr);
            }
        }
        let mut reserved = Reserved(false);
        reserved.visit_body(&function.body);
        if reserved.0 {
            return Err("analysis declaration names are not program capabilities".into());
        }
    }
    let mut source = source;
    for pass in 0..2 {
        let result = monty_analysis::analyze_function(
            &AnalysisRequest {
                source: source.clone(),
                stubs: Some(declarations.source.clone()),
                targets: Vec::new(),
                limits: AnalysisLimits::default(),
            },
            "__plasm_expression",
        )?;
        let AnalysisOutcome::Inferred(graph) = result.outcome else {
            let AnalysisOutcome::Rejected(errors) = result.outcome else {
                unreachable!()
            };
            return Err(body_diagnostics(errors, &source));
        };
        if pass == 0 && graph.nodes.contains(&Node::Unknown) {
            let specialized = helpers::close_local_calls(&source, &mut declarations)?;
            if specialized != source {
                source = specialized;
                continue;
            }
        }
        let decoder = Decoder {
            graph: &graph,
            declarations: &declarations,
        };
        return graph
            .roots
            .iter()
            .map(|id| decoder.decode(*id, 0))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .reduce(Type::join)
            .ok_or_else(|| "checker supplied no return contract".into());
    }
    unreachable!("local helper inference uses at most two whole-body passes")
}

/// Check the original annotation before materialization widens Python literals.
/// A materialized string contract must not erase a Literal return constraint.
pub(super) fn check_annotated_body(
    body: &str,
    inputs: &[(&str, &Type)],
    annotation: &str,
    aliases: &BTreeMap<String, Type>,
    imports: &str,
    cgs: &plasm_core::CGS,
    catalogs: &BTreeMap<String, std::sync::Arc<plasm_core::CGS>>,
) -> Result<(), String> {
    // Authored output contracts are validated refinements. Reuse the same
    // output/input declarations as final worker admission; inference's nominal
    // evidence declarations must not impose a second, incompatible return ABI.
    let mut stubs = super::upstream::stubs_in(&BTreeMap::new(), cgs, catalogs)?;
    for (name, contract) in aliases {
        super::upstream::validate_member(name)?;
        let rendered = super::upstream::output_annotation(contract, cgs, catalogs, &mut stubs)?;
        stubs.push_str(&format!("{name}: TypeAlias = {rendered}\n"));
    }
    // An existing materialized container retains its concrete element type;
    // Python mutable-container invariance must not reject that unchanged value
    // merely because constructors also accept dictionaries. Preserve the entire
    // authored annotation (including Literal constraints) in both alternatives.
    struct ObservedAliases<'a>(&'a BTreeMap<String, Type>);
    impl ruff_python_ast::visitor::transformer::Transformer for ObservedAliases<'_> {
        fn visit_expr(&self, expression: &mut Expr) {
            if let Expr::Name(name) = expression {
                if self.0.contains_key(name.id.as_str()) {
                    name.id = format!("PlasmObserved{}", name.id).into();
                    return;
                }
            }
            ruff_python_ast::visitor::transformer::walk_expr(self, expression);
        }
    }
    for (name, contract) in aliases {
        let observed = super::upstream::input_type(contract, cgs, catalogs, &mut stubs)?;
        stubs.push_str(&format!("PlasmObserved{name}: TypeAlias = {observed}\n"));
    }
    let mut observed = *ruff_python_parser::parse_expression(annotation)
        .map_err(|e| e.to_string())?
        .into_syntax()
        .body;
    ruff_python_ast::visitor::transformer::Transformer::visit_expr(
        &ObservedAliases(aliases),
        &mut observed,
    );
    let annotation = format!("({annotation}) | ({})", monty::expression_source(&observed));
    let parameters = inputs
        .iter()
        .map(|(name, ty)| {
            super::upstream::input_type(ty, cgs, catalogs, &mut stubs)
                .map(|ty| format!("{name}: {ty}"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let mut source =
        format!("{imports}\ndef __plasm_return({parameters}) -> {annotation}:{body}\n");
    if helpers::has_local_calls(&source)? {
        let mut evidence = declarations::Declarations::default();
        evidence.source =
            format!("from typing import NewType, Protocol, TypedDict, NotRequired\n{stubs}");
        source = helpers::close_local_calls(&source, &mut evidence)?;
        stubs = evidence.source;
        let inferred = monty_analysis::analyze_function(
            &AnalysisRequest {
                source: source.clone(),
                stubs: Some(stubs.clone()),
                targets: Vec::new(),
                limits: AnalysisLimits::default(),
            },
            "__plasm_return",
        )?;
        match inferred.outcome {
            AnalysisOutcome::Inferred(graph) if graph.nodes.contains(&Node::Unknown) => {
                return Err("local helper return is not a closed materialized contract".into());
            }
            AnalysisOutcome::Rejected(errors) => return Err(body_diagnostics(errors, &source)),
            AnalysisOutcome::Inferred(_) => {}
        }
    }
    let result = monty_analysis::analyze(&AnalysisRequest {
        source: source.clone(),
        stubs: Some(stubs),
        targets: vec![],
        limits: AnalysisLimits::default(),
    })?;
    match result.outcome {
        AnalysisOutcome::Inferred(_) => Ok(()),
        AnalysisOutcome::Rejected(errors) => Err(body_diagnostics(errors, &source)),
    }
}

fn body_diagnostics(errors: Vec<monty_analysis::AnalysisDiagnostic>, source: &str) -> String {
    errors
        .into_iter()
        .map(|error| {
            let location = error
                .span
                .and_then(|span| source.get(..span.start as usize))
                .map(|prefix| {
                    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
                    let column = prefix
                        .rsplit('\n')
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .count()
                        + 1;
                    format!("plasm_compute.py:{line}:{column}: ")
                })
                .unwrap_or_default();
            format!("{location}{}: {}", error.code, error.message)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ask the authoritative checker about Python assignment compatibility. Plasm
/// uses this only to select a value/collection boundary, never to infer operators.
pub(super) fn assignable(actual: &Type, expected: &Type) -> Result<bool, String> {
    let mut declarations = declarations::Declarations::default();
    let actual = declarations.render(actual, 0)?;
    let expected = declarations.render(expected, 0)?;
    let result = monty_analysis::analyze(&AnalysisRequest {
        source: format!("def expected(value: {expected}) -> None:\n    pass\ndef check(value: {actual}) -> None:\n    expected(value)\n"),
        stubs: Some(declarations.source), targets: vec![], limits: AnalysisLimits::default(),
    })?;
    Ok(matches!(result.outcome, AnalysisOutcome::Inferred(_)))
}

/// Resolve a Python annotation through the same checker used for expression
/// inference. Aliases carry only host-owned nominal/record contracts.
#[cfg(test)]
pub(super) fn annotation(source: &str, aliases: &BTreeMap<String, Type>) -> Result<Type, String> {
    annotation_with_imports(source, aliases, "", None)?
        .ok_or("annotation has unresolved value types".into())
}

pub(super) fn annotation_with_imports(
    source: &str,
    aliases: &BTreeMap<String, Type>,
    imports: &str,
    actual: Option<&Type>,
) -> Result<Option<Type>, String> {
    let mut declarations = declarations::Declarations::default();
    for (name, contract) in aliases {
        super::upstream::validate_member(name)?;
        let rendered = declarations.render(contract, 0)?;
        declarations
            .source
            .push_str(&format!("{name}: TypeAlias = {rendered}\n"));
    }
    let source = if let Some(actual) = actual {
        let actual = declarations.render(actual, 0)?;
        format!("{imports}\ndef __plasm_expected(value: {source}) -> None:\n    pass\ndef __plasm_annotation(value: {actual}):\n    __plasm_expected(value)\n    return value\n")
    } else {
        format!("{imports}\ndef __plasm_annotation(value: {source}):\n    return value\n")
    };
    let result = monty_analysis::analyze_function(
        &AnalysisRequest {
            source,
            stubs: Some(declarations.source.clone()),
            targets: vec![],
            limits: AnalysisLimits::default(),
        },
        "__plasm_annotation",
    )?;
    match result.outcome {
        AnalysisOutcome::Inferred(graph) => {
            if graph
                .nodes
                .iter()
                .any(|node| matches!(node, Node::Any | Node::Unknown))
            {
                return Ok(None);
            }
            let decoder = Decoder {
                graph: &graph,
                declarations: &declarations,
            };
            let [root] = graph.roots.as_slice() else {
                return Err("annotation must produce one contract".into());
            };
            decoder.decode(*root, 0).map(Some)
        }
        AnalysisOutcome::Rejected(errors) => Err(errors
            .into_iter()
            .map(|e| format!("{}: {}", e.code, e.message))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

fn never() -> Type {
    Type {
        shape: ValueShape::Never,
        domain: None,
        nullable: false,
    }
}
fn array(element: Type) -> Type {
    Type {
        shape: ValueShape::Array {
            element: Box::new(element),
        },
        domain: None,
        nullable: false,
    }
}

struct Decoder<'a> {
    graph: &'a Graph,
    declarations: &'a declarations::Declarations,
}
impl Decoder<'_> {
    fn decode(&self, id: TypeId, depth: usize) -> Result<Type, String> {
        if depth >= 64 {
            return Err("recursive inferred type is not a finite Plasm contract".into());
        }
        let recur = |id| self.decode(id, depth + 1);
        let node = self
            .graph
            .nodes
            .get(id.0 as usize)
            .ok_or("invalid inferred type reference")?;
        Ok(match node {
            Node::Never => never(),
            Node::None => Type {
                shape: ValueShape::Null,
                domain: None,
                nullable: true,
            },
            Node::BoolLiteral(_) => Type::scalar(FieldType::Boolean),
            Node::IntLiteral(_) => Type::scalar(FieldType::Integer),
            Node::StringLiteral(_) | Node::LiteralString => Type::scalar(FieldType::String),
            Node::Alias(id) => recur(*id)?,
            Node::Union(items) => items
                .iter()
                .map(|id| recur(*id))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .reduce(Type::join)
                .unwrap_or_else(never),
            // Monty tuples materialize as ordered JSON arrays. Preserve a
            // closed element contract by joining every positional and variadic
            // member; indexing remains conservative for heterogeneous tuples.
            Node::Tuple {
                prefix,
                variable,
                suffix,
            } => {
                let elements = prefix
                    .iter()
                    .chain(variable.iter())
                    .chain(suffix.iter())
                    .map(|id| recur(*id))
                    .collect::<Result<Vec<_>, _>>()?;
                array(
                    elements
                        .into_iter()
                        .reduce(Type::join)
                        .unwrap_or_else(never),
                )
            }
            Node::Protocol { identity, .. }
            | Node::NewType { identity, .. }
            | Node::Instance { identity, .. }
                if identity.source == "/analysis_stubs.pyi"
                    && (identity.path.len() == 1
                        || (identity.path.len() == 2 && identity.path[0] == "analysis_stubs")) =>
            {
                self.declarations
                    .contracts
                    .get(identity.path.last().ok_or("missing nominal name")?)
                    .cloned()
                    .ok_or("unsealed inferred nominal identity")?
            }
            Node::Instance {
                identity,
                arguments,
            } => {
                let name = identity.path.last().map(String::as_str).unwrap_or("");
                // Only the upstream builtins/typeshed have primitive meaning.
                if identity.source.ends_with("/builtins.pyi") {
                    match (name, arguments.as_slice()) {
                        ("bool", []) => Type::scalar(FieldType::Boolean),
                        ("int", []) => Type::scalar(FieldType::Integer),
                        ("float", []) => Type::scalar(FieldType::Number),
                        ("str", []) => Type::scalar(FieldType::String),
                        ("list", [element]) => array(recur(*element)?),
                        ("set", [element]) => Type {
                            shape: ValueShape::Set {
                                element: Box::new(recur(*element)?),
                            },
                            domain: None,
                            nullable: false,
                        },
                        ("dict", [key, value]) => {
                            let key = recur(*key)?;
                            if !matches!(key.shape, ValueShape::Never)
                                && (key.summary() != plasm_core::SyntheticValueKind::String
                                    || key.nullable)
                            {
                                return Err("materialized dictionaries require string keys".into());
                            }
                            Type {
                                shape: ValueShape::Dictionary {
                                    key: Box::new(key),
                                    value: Box::new(recur(*value)?),
                                },
                                domain: None,
                                nullable: false,
                            }
                        }
                        _ => {
                            return Err(format!(
                                "inferred Python {name} is not a materialized Plasm type"
                            ))
                        }
                    }
                } else if identity.source.ends_with("/datetime.pyi") {
                    plasm_core::temporal_value::TemporalKind::parse(name)
                        .ok_or("unsupported inferred temporal type")?
                        .contract()
                } else {
                    return Err(format!(
                        "unsealed inferred Python type {identity:?}::{name}"
                    ));
                }
            }
            Node::Intersection { positive, negative } => {
                // Truth refinements narrow an existing nominal/primitive contract;
                // dropping only truthiness retains a sound materialized supertype.
                let values = positive
                    .iter()
                    .filter(|id| {
                        !matches!(self.graph.nodes[id.0 as usize], Node::Truthy | Node::Falsy)
                    })
                    .map(|id| recur(*id))
                    .collect::<Result<Vec<_>, _>>()?;
                // All decoded constraints are established by the upstream graph
                // and sealed declarations. Their order supplies no authority.
                let mut values = values.into_iter();
                let mut value = values
                    .next()
                    .ok_or("inferred intersection has no materialized positive constraint")?;
                for evidence in values {
                    value = value.intersect_constraints(&evidence)?;
                }
                for id in negative {
                    if matches!(self.graph.nodes[id.0 as usize], Node::None) {
                        value.nullable = false;
                        // Materialized contracts abstract value-level exclusions.
                        // Keeping the positive type is a sound upper bound: for
                        // example str & ~Literal["x"] remains str, never Never.
                        // Union alternatives already eliminated by the checker
                        // remain eliminated in the exported graph.
                    }
                }
                value
            }
            _ => {
                return Err(format!(
                    "inferred Python type is not a closed materialized contract: {node:?}"
                ))
            }
        })
    }
}

/// Reconstruct both successor contracts from upstream evidence for a sealed
/// single-expression compute. The paths identify observations, not predicate syntax.
pub(crate) fn branch_contracts(
    source: &str,
    argument: &Type,
    paths: &[Vec<String>],
) -> Result<Option<Vec<(Type, Type)>>, String> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let (imports, suite) = crate::python_datetime::Imports::split(source, parsed.suite())?;
    let [ruff_python_ast::Stmt::FunctionDef(function)] = suite else {
        return Ok(None);
    };
    let [ruff_python_ast::Stmt::Return(ret)] = function.body.as_slice() else {
        return Ok(None);
    };
    let Some(predicate) = &ret.value else {
        return Ok(None);
    };
    let [parameter] = function.parameters.args.as_slice() else {
        return Ok(None);
    };
    let name = parameter.parameter.name.as_str();
    let mut declarations = declarations::Declarations::default();
    let annotation = declarations.render(argument, 0)?;
    let subjects = paths
        .iter()
        .map(|path| {
            for field in path {
                super::upstream::validate_member(field)?;
            }
            Ok(format!("{name}.{}", path.join(".")))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result = monty_analysis::analyze_branches(&monty_analysis::BranchRequest {
        bindings: vec![monty_analysis::Binding {
            name: name.into(),
            annotation,
        }],
        predicate: source[predicate.start().to_usize()..predicate.end().to_usize()].into(),
        subjects,
        imports: imports.source,
        stubs: Some(declarations.source.clone()),
        limits: AnalysisLimits::default(),
    })?;
    let graph = match result.outcome {
        AnalysisOutcome::Inferred(graph) => graph,
        AnalysisOutcome::Rejected(errors) => {
            return Err(errors
                .into_iter()
                .map(|e| format!("{}: {}", e.code, e.message))
                .collect::<Vec<_>>()
                .join("\n"))
        }
    };
    let decoder = Decoder {
        graph: &graph,
        declarations: &declarations,
    };
    graph.roots[..paths.len()]
        .iter()
        .zip(&graph.roots[paths.len()..])
        .map(|(yes, no)| Ok((decoder.decode(*yes, 0)?, decoder.decode(*no, 0)?)))
        .collect::<Result<Vec<_>, String>>()
        .map(Some)
}

pub(crate) fn observation_paths(value: &Type) -> Result<Vec<Vec<String>>, String> {
    fn walk(
        value: &Type,
        path: Vec<String>,
        out: &mut Vec<Vec<String>>,
        depth: usize,
    ) -> Result<(), String> {
        if depth >= 64 {
            return Err("branch observation depth exceeds 64".into());
        }
        if !path.is_empty() {
            out.push(path.clone());
        }
        if !value.nullable {
            if let ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } =
                &value.shape
            {
                for (name, value) in fields {
                    let mut path = path.clone();
                    path.push(name.clone());
                    walk(value, path, out, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    walk(value, Vec::new(), &mut paths, 0)?;
    Ok(paths)
}
