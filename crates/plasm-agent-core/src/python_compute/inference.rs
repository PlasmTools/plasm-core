//! Decode upstream Python inference into materialized Plasm contracts.
//! Generated nominal declarations are a reversible map, not carrier-type proofs.
use monty_analysis::{AnalysisLimits, AnalysisOutcome, AnalysisRequest, Graph, Node, Span, TypeId};
use plasm_core::{
    value_contract::{ValueContract as Type, ValueShape},
    FieldType,
};
use ruff_python_ast::Expr;
use ruff_text_size::Ranged;
use std::collections::BTreeMap;

mod declarations;
#[cfg(test)]
mod tests;

pub(super) fn infer(
    expression: &str,
    parameter: &str,
    argument: &Type,
    imports: &str,
) -> Result<Type, String> {
    let mut declarations = declarations::Declarations::default();
    let annotation = declarations.render(argument, 0)?;
    let source = format!("{imports}\ndef __plasm_expression({parameter}: {annotation}):\n    return ({expression})\n");
    let parsed = ruff_python_parser::parse_module(&source).map_err(|e| e.to_string())?;
    let Some(ruff_python_ast::Stmt::FunctionDef(function)) = parsed.suite().last() else {
        return Err("missing generated expression function".into());
    };
    let [ruff_python_ast::Stmt::Return(ret)] = function.body.as_slice() else {
        return Err("expression must not introduce statements".into());
    };
    let expr = ret.value.as_deref().ok_or("missing expression result")?;
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
        reserved.visit_expr(expr);
        if reserved.0 {
            return Err("analysis declaration names are not program capabilities".into());
        }
    }
    let mut targets = Vec::new();
    collect(expr, &mut targets, 0)?;
    let result = monty_analysis::analyze(&AnalysisRequest {
        source,
        stubs: Some(declarations.source.clone()),
        targets: targets
            .iter()
            .map(|e| Span {
                start: e.start().to_u32(),
                end: e.end().to_u32(),
            })
            .collect(),
        limits: AnalysisLimits::default(),
    })?;
    let AnalysisOutcome::Inferred(graph) = result.outcome else {
        let AnalysisOutcome::Rejected(errors) = result.outcome else {
            unreachable!()
        };
        return Err(errors
            .into_iter()
            .map(|e| format!("{}: {}", e.code, e.message))
            .collect::<Vec<_>>()
            .join("\n"));
    };
    let decoder = Decoder {
        graph: &graph,
        declarations: &declarations,
    };
    elaborate(expr, &targets, &decoder, 0)
}

// Preserve structural record keys at the materialization boundary. Python's
// dict[str, T] alone does not prove a record's per-field contracts.
fn collect<'a>(expr: &'a Expr, targets: &mut Vec<&'a Expr>, depth: usize) -> Result<(), String> {
    if depth >= 64 {
        return Err("expression result nesting exceeds 64".into());
    }
    targets.push(expr);
    match expr {
        Expr::Dict(dict) => {
            for item in &dict.items {
                collect(&item.value, targets, depth + 1)?;
            }
        }
        Expr::List(list) => {
            for item in &list.elts {
                collect(item, targets, depth + 1)?;
            }
        }
        Expr::ListComp(list) => collect(&list.elt, targets, depth + 1)?,
        Expr::If(choice) => {
            collect(&choice.body, targets, depth + 1)?;
            collect(&choice.orelse, targets, depth + 1)?;
        }
        _ => {}
    }
    Ok(())
}

fn elaborate(
    expr: &Expr,
    targets: &[&Expr],
    decoder: &Decoder<'_>,
    depth: usize,
) -> Result<Type, String> {
    if depth >= 64 {
        return Err("expression result nesting exceeds 64".into());
    }
    let recur = |expr| elaborate(expr, targets, decoder, depth + 1);
    match expr {
        Expr::Dict(dict) => {
            let mut fields = BTreeMap::new();
            for item in &dict.items {
                let Some(Expr::StringLiteral(key)) = &item.key else {
                    return Err("materialized records require explicit string keys".into());
                };
                if fields
                    .insert(key.value.to_str().to_owned(), recur(&item.value)?)
                    .is_some()
                {
                    return Err("duplicate materialized record field".into());
                }
            }
            Ok(Type::record(fields, Default::default()))
        }
        Expr::List(list) => Ok(array(
            list.elts
                .iter()
                .map(recur)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .reduce(Type::join)
                .unwrap_or_else(never),
        )),
        Expr::ListComp(list) => Ok(array(recur(&list.elt)?)),
        Expr::If(choice)
            if matches!(
                &*choice.body,
                Expr::Dict(_) | Expr::List(_) | Expr::ListComp(_)
            ) || matches!(
                &*choice.orelse,
                Expr::Dict(_) | Expr::List(_) | Expr::ListComp(_)
            ) =>
        {
            Ok(Type::join(recur(&choice.body)?, recur(&choice.orelse)?))
        }
        _ => {
            let index = targets
                .iter()
                .position(|target| std::ptr::eq(*target, expr))
                .ok_or("missing analysis target")?;
            decoder.decode(
                *decoder
                    .graph
                    .roots
                    .get(index)
                    .ok_or("missing inferred expression type")?,
                0,
            )
        }
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
            Node::NewType { identity, .. } | Node::Instance { identity, .. }
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
