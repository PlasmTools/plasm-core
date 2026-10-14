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

mod collections;
mod declarations;
mod helpers;
#[cfg(test)]
mod tests;

/// Original checker records remain inspectable; formatting happens in Display.
#[derive(Debug, Clone)]
pub struct InferenceDiagnostics {
    pub entries: Vec<LocatedAnalysisDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct LocatedAnalysisDiagnostic {
    pub diagnostic: monty_analysis::AnalysisDiagnostic,
    pub location: Option<(usize, usize)>,
}

impl From<Vec<monty_analysis::AnalysisDiagnostic>> for InferenceDiagnostics {
    fn from(errors: Vec<monty_analysis::AnalysisDiagnostic>) -> Self {
        Self {
            entries: errors
                .into_iter()
                .map(|diagnostic| LocatedAnalysisDiagnostic {
                    diagnostic,
                    location: None,
                })
                .collect(),
        }
    }
}

impl std::fmt::Display for InferenceDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            if let Some((line, column)) = entry.location {
                write!(f, "plasm_compute.py:{line}:{column}: ")?;
            }
            write!(f, "{}: {}", entry.diagnostic.code, entry.diagnostic.message)?;
        }
        Ok(())
    }
}
impl std::error::Error for InferenceDiagnostics {}

impl From<plasm_core::value_contract::ValueContractError> for InferenceError {
    fn from(error: plasm_core::value_contract::ValueContractError) -> Self {
        Self::Graph(InferenceGraphError::ValueContract(error))
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum InferenceError {
    #[error("Python checker diagnostics: {diagnostics}")]
    Diagnostics {
        #[source]
        diagnostics: InferenceDiagnostics,
    },
    #[error("return entity catalog `{entry_id}` is not loaded")]
    ReturnCatalogMissing { entry_id: String },
    #[error("Python inference backend failed: {0}")]
    Backend(#[from] monty_analysis::AnalysisError),
    #[error("Python source parse failed: {source}")]
    Parse {
        #[source]
        source: ruff_python_parser::ParseError,
    },
    #[error(transparent)]
    Program(#[from] crate::program_rejection::PythonProgramError),
    #[error(transparent)]
    Compute(#[from] crate::program_rejection::PythonComputeError),
    #[error(transparent)]
    ComputeRejection(#[from] crate::program_rejection::PythonComputeRejection),
    #[error("catalog value contract could not be constructed: {0}")]
    CatalogValueContract(#[source] Box<crate::program_rejection::PythonComputeError>),
    #[error(transparent)]
    SymbolResolve(#[from] plasm_core::symbol_tuning::SymbolResolveError),
    #[error("inferred type graph is invalid: {0}")]
    Graph(#[from] InferenceGraphError),
    #[error("Python type declaration failed: {0}")]
    Declaration(#[from] DeclarationError),
    #[error("Python return contract check failed: {0}")]
    Return(#[from] ReturnContractError),
    #[error("Python analysis failed its control-flow bound")]
    ControlFlowBoundExceeded,
    #[error("generated compute function is missing")]
    GeneratedFunctionMissing,
    #[error("local helper names are duplicated")]
    DuplicateLocalHelper,
    #[error("local helper parameter binding could not be resolved")]
    HelperParameterBindingMissing,
    #[error("local helper argument expansion is unsupported")]
    ExpandedHelperArgumentsUnsupported,
    #[error("local helper calls are variadic")]
    VariadicLocalHelperCall,
    #[error("local helper count exceeds the inference bound")]
    LocalHelperLimitExceeded,
    #[error("local helper call count exceeds the inference bound")]
    LocalHelperCallLimitExceeded,
    #[error("local helper inference did not converge")]
    LocalHelperInferenceDidNotConverge,
    #[error("checker supplied no inferred helper contract")]
    HelperContractMissing,
}

impl From<ruff_python_parser::ParseError> for InferenceError {
    fn from(error: ruff_python_parser::ParseError) -> Self {
        Self::Parse { source: error }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InferenceGraphError {
    #[error("cyclic inferred type cannot be materialized")]
    RecursiveType,
    #[error("inferred type graph contains an invalid reference")]
    InvalidReference,
    #[error("inferred nominal type has no declared name")]
    NominalNameMissing,
    #[error("inferred nominal type is not declared")]
    NominalUnsealed,
    #[error("materialized dictionaries require string keys")]
    DictionaryKeyNotString,
    #[error("inferred Python type is not materializable")]
    UnsupportedType,
    #[error("inferred temporal type is unsupported")]
    UnsupportedTemporal,
    #[error("inferred Python type `{name}` has no materialized Plasm contract")]
    UnsupportedNamedType { name: String },
    #[error("inferred Python nominal type `{identity}` is not sealed")]
    UnsealedNamedType { identity: String },
    #[error("inferred type graph contains an unsupported node")]
    UnsupportedNode,
    #[error("inferred intersection has no positive materialized constraint")]
    EmptyIntersection,
    #[error(transparent)]
    ValueContract(#[from] plasm_core::value_contract::ValueContractError),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum DeclarationError {
    #[error("analysis declaration exceeds the maximum nesting depth")]
    DepthExceeded,
    #[error("mapping input requires a record contract")]
    MappingInputNotRecord,
    #[error("analysis requires an explicit temporal or array shape")]
    ShapeNeedsExplicitContract,
    #[error("Python field name `{field}` is not a valid member")]
    InvalidMember { field: String },
    #[error("Python type declaration serialization failed: {0}")]
    Serialization(#[source] std::sync::Arc<serde_json::Error>),
    #[error(transparent)]
    Compute(#[from] crate::program_rejection::PythonComputeError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReturnContractError {
    #[error("annotation has unresolved value types")]
    UnresolvedAnnotation,
    #[error("generated compute function is missing")]
    ComputeFunctionMissing,
    #[error("analysis declaration names are reserved")]
    ReservedDeclarationName,
    #[error("checker supplied no return contract")]
    ReturnContractMissing,
    #[error("annotation must produce exactly one contract")]
    AnnotationContractCount,
    #[error("local helper return is not a closed materialized contract")]
    OpenHelperReturn,
}

#[cfg(test)]
pub(super) fn infer(
    expression: &str,
    parameter: &str,
    argument: &Type,
    imports: &str,
) -> Result<Type, InferenceError> {
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
) -> Result<Type, InferenceError> {
    Ok(infer_output(body, parameters, imports)?.contract)
}

/// A nominal return is evidence of a possible input occurrence, not authority.
/// Execution must still prove unchanged instance identity before continuation.
pub(super) struct InferredOutput {
    pub contract: Type,
    pub selected_parameters: std::collections::BTreeSet<String>,
}

pub(super) fn infer_output(
    body: &str,
    inputs: &[(&str, &Type)],
    imports: &str,
) -> Result<InferredOutput, InferenceError> {
    let mut declarations = declarations::Declarations::default();
    let parameters = inputs
        .iter()
        .map(|(name, ty)| {
            declarations
                .render(ty, 0)
                .map(|annotation| format!("{name}: {annotation}"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let source = format!("{imports}\ndef __plasm_expression({parameters}):{body}\n");
    let parsed = ruff_python_parser::parse_module(&source)?;
    let Some(ruff_python_ast::Stmt::FunctionDef(function)) = parsed.suite().last() else {
        return Err(ReturnContractError::ComputeFunctionMissing.into());
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
            return Err(ReturnContractError::ReservedDeclarationName.into());
        }
    }
    let mut source = source;
    for pass in 0..3 {
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
            return Err(InferenceError::Diagnostics {
                diagnostics: body_diagnostics(errors, &source),
            });
        };
        if pass < 2 && graph.nodes.contains(&Node::Unknown) {
            let specialized = collections::close_collection_initializers(
                &source,
                &mut declarations,
                graph
                    .nodes
                    .iter()
                    .filter(|node| **node == Node::Unknown)
                    .count(),
            )?;
            let specialized = helpers::close_local_calls(&specialized, &mut declarations)?;
            if specialized != source {
                source = specialized;
                continue;
            }
        }
        let decoder = Decoder {
            graph: &graph,
            declarations: &declarations,
        };
        let contract = decoder
            .decode_roots(&graph.roots)?
            .into_iter()
            .reduce(Type::join)
            .ok_or(ReturnContractError::ReturnContractMissing)?;
        let selected_parameters = selection_parameters(&graph, &declarations, inputs);
        return Ok(InferredOutput {
            contract,
            selected_parameters,
        });
    }
    unreachable!("local closure uses at most three whole-body passes")
}

fn selection_parameters(
    graph: &Graph,
    declarations: &declarations::Declarations,
    inputs: &[(&str, &Type)],
) -> std::collections::BTreeSet<String> {
    use std::collections::BTreeSet;
    // Traverse the exported type graph, not Python syntax. Only a record or a
    // flat list of the input's nominal records can carry occurrence evidence.
    let mut pending = graph
        .roots
        .iter()
        .map(|id| (*id, false))
        .collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    let mut selected = BTreeSet::new();
    while let Some((id, in_list)) = pending.pop() {
        if !visited.insert((id.0, in_list)) {
            continue;
        }
        match graph.nodes.get(id.0 as usize) {
            Some(Node::Alias(next)) => pending.push((*next, in_list)),
            Some(Node::Union(items)) => pending.extend(items.iter().map(|id| (*id, in_list))),
            Some(Node::Never) => {}
            Some(Node::Instance {
                identity,
                arguments,
            }) if !in_list
                && identity.path.last().is_some_and(|name| name == "list")
                && arguments.len() == 1 =>
            {
                pending.push((arguments[0], true));
            }
            Some(Node::Protocol { identity, .. }) | Some(Node::Instance { identity, .. }) => {
                let Some(contract) = identity
                    .path
                    .last()
                    .and_then(|name| declarations.contracts.get(name))
                else {
                    return BTreeSet::new();
                };
                if !matches!(
                    contract.shape,
                    ValueShape::Record { .. } | ValueShape::ObservedRecord { .. }
                ) {
                    return BTreeSet::new();
                }
                let candidates = inputs
                    .iter()
                    .filter_map(|(name, input)| {
                        let row = match &input.shape {
                            ValueShape::Array { element } => element.as_ref(),
                            _ => input,
                        };
                        (row == contract).then(|| (*name).to_owned())
                    })
                    .collect::<BTreeSet<_>>();
                if candidates.is_empty() {
                    return BTreeSet::new();
                }
                selected.extend(candidates);
            }
            _ => return BTreeSet::new(),
        }
    }
    selected
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
) -> Result<(), InferenceError> {
    // Authored output contracts are validated refinements. Reuse the same
    // output/input declarations as final worker admission; inference's nominal
    // evidence declarations must not impose a second, incompatible return ABI.
    let mut stubs =
        super::upstream::stubs_in(&BTreeMap::new(), cgs, catalogs).map_err(InferenceError::from)?;
    for (name, contract) in aliases {
        super::upstream::validate_member(name).map_err(|_| DeclarationError::InvalidMember {
            field: name.clone(),
        })?;
        let rendered = super::upstream::output_annotation(contract, cgs, catalogs, &mut stubs)
            .map_err(InferenceError::from)?;
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
        let observed = super::upstream::input_type(contract, cgs, catalogs, &mut stubs)
            .map_err(InferenceError::from)?;
        stubs.push_str(&format!("PlasmObserved{name}: TypeAlias = {observed}\n"));
    }
    let mut observed = *ruff_python_parser::parse_expression(annotation)?
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
                .map_err(InferenceError::from)
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let mut source =
        format!("{imports}\ndef __plasm_return({parameters}) -> {annotation}:{body}\n");
    if helpers::has_local_calls(&source)? {
        let mut evidence = declarations::Declarations::default();
        evidence.source.push_str(&stubs);
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
                return Err(ReturnContractError::OpenHelperReturn.into());
            }
            AnalysisOutcome::Rejected(errors) => {
                return Err(InferenceError::Diagnostics {
                    diagnostics: body_diagnostics(errors, &source),
                });
            }
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
        AnalysisOutcome::Rejected(errors) => Err(InferenceError::Diagnostics {
            diagnostics: body_diagnostics(errors, &source),
        }),
    }
}

fn body_diagnostics(
    errors: Vec<monty_analysis::AnalysisDiagnostic>,
    source: &str,
) -> InferenceDiagnostics {
    InferenceDiagnostics {
        entries: errors
            .into_iter()
            .map(|diagnostic| {
                let location = diagnostic
                    .span
                    .and_then(|span| source.get(..span.start as usize))
                    .map(|prefix| {
                        (
                            prefix.bytes().filter(|b| *b == b'\n').count() + 1,
                            prefix
                                .rsplit('\n')
                                .next()
                                .unwrap_or_default()
                                .chars()
                                .count()
                                + 1,
                        )
                    });
                LocatedAnalysisDiagnostic {
                    diagnostic,
                    location,
                }
            })
            .collect(),
    }
}
/// Ask the checker about assignment compatibility, not operator inference.
pub(super) fn assignable(actual: &Type, expected: &Type) -> Result<bool, InferenceError> {
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
pub(super) fn annotation(
    source: &str,
    aliases: &BTreeMap<String, Type>,
) -> Result<Type, InferenceError> {
    annotation_with_imports(source, aliases, "", None)?
        .ok_or(ReturnContractError::UnresolvedAnnotation.into())
}

pub(super) fn annotation_with_imports(
    source: &str,
    aliases: &BTreeMap<String, Type>,
    imports: &str,
    actual: Option<&Type>,
) -> Result<Option<Type>, InferenceError> {
    let mut declarations = declarations::Declarations::default();
    for (name, contract) in aliases {
        super::upstream::validate_member(name).map_err(|_| DeclarationError::InvalidMember {
            field: name.clone(),
        })?;
        let rendered = declarations.render(contract, 0)?;
        declarations
            .source
            .push_str(&format!("{name}: TypeAlias = {rendered}\n"));
    }
    let source = if let Some(actual) = actual {
        // An array whose element type is bottom is provably empty. Check its
        // empty literal contextually: invariant list[Never] is not a mutable
        // list[int], but materializing this value as an empty list is lawful.
        // Nonempty and nullable containers retain ordinary assignment checking.
        let argument = if !actual.nullable
            && matches!(&actual.shape, ValueShape::Array { element } if !element.nullable && matches!(element.shape, ValueShape::Never))
        {
            "[]"
        } else {
            "value"
        };
        let actual = declarations.render(actual, 0)?;
        format!("from typing import Never\n{imports}\ndef __plasm_expected(value: {source}) -> None:\n    pass\ndef __plasm_annotation(value: {actual}):\n    __plasm_expected({argument})\n    return value\n")
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
                return Err(ReturnContractError::AnnotationContractCount.into());
            };
            decoder.decode(*root).map(Some)
        }
        AnalysisOutcome::Rejected(errors) => Err(InferenceError::Diagnostics {
            diagnostics: errors.into(),
        }),
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
    fn decode(&self, root: TypeId) -> Result<Type, InferenceError> {
        self.decode_roots(&[root])?
            .into_iter()
            .next()
            .ok_or_else(|| InferenceGraphError::InvalidReference.into())
    }

    fn decode_roots(&self, roots: &[TypeId]) -> Result<Vec<Type>, InferenceError> {
        let mut decoded = vec![None; self.graph.nodes.len()];
        let mut active = vec![false; self.graph.nodes.len()];
        let mut pending = roots
            .iter()
            .rev()
            .map(|root| (*root, false))
            .collect::<Vec<_>>();
        while let Some((id, finish)) = pending.pop() {
            let index = id.0 as usize;
            let node = self
                .graph
                .nodes
                .get(index)
                .ok_or(InferenceGraphError::InvalidReference)?;
            if finish {
                decoded[index] = Some(self.decode_node(id, &decoded)?);
                active[index] = false;
                continue;
            }
            if decoded[index].is_some() {
                continue;
            }
            if active[index] {
                return Err(InferenceGraphError::RecursiveType.into());
            }
            active[index] = true;
            pending.push((id, true));
            let children: Vec<TypeId> = match node {
                Node::Alias(child) => vec![*child],
                Node::Union(items) => items.clone(),
                Node::Tuple {
                    prefix,
                    variable,
                    suffix,
                } => prefix
                    .iter()
                    .chain(variable)
                    .chain(suffix)
                    .copied()
                    .collect(),
                Node::Instance {
                    identity,
                    arguments,
                } if identity.source.ends_with("/builtins.pyi") => arguments.clone(),
                Node::Intersection { positive, negative } => {
                    // Validate exclusions too, although they do not require
                    // materialization of their value types.
                    for child in negative {
                        self.graph
                            .nodes
                            .get(child.0 as usize)
                            .ok_or(InferenceGraphError::InvalidReference)?;
                    }
                    let mut children = Vec::new();
                    for child in positive {
                        let node = self
                            .graph
                            .nodes
                            .get(child.0 as usize)
                            .ok_or(InferenceGraphError::InvalidReference)?;
                        if !matches!(node, Node::Truthy | Node::Falsy) {
                            children.push(*child);
                        }
                    }
                    children
                }
                _ => Vec::new(),
            };
            pending.extend(children.into_iter().rev().map(|id| (id, false)));
        }
        roots
            .iter()
            .map(|root| {
                decoded
                    .get(root.0 as usize)
                    .and_then(Clone::clone)
                    .ok_or_else(|| InferenceGraphError::InvalidReference.into())
            })
            .collect()
    }

    fn decode_node(&self, id: TypeId, decoded: &[Option<Type>]) -> Result<Type, InferenceError> {
        let recur = |id: TypeId| {
            decoded
                .get(id.0 as usize)
                .and_then(Clone::clone)
                .ok_or_else(|| InferenceError::from(InferenceGraphError::InvalidReference))
        };
        let node = self
            .graph
            .nodes
            .get(id.0 as usize)
            .ok_or(InferenceGraphError::InvalidReference)?;
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
            // Arrays are the wire encoding, not the Python container identity.
            // Preserve each position so captures rehydrate as tuples.
            Node::Tuple {
                prefix,
                variable,
                suffix,
            } => Type {
                shape: ValueShape::Tuple {
                    members: plasm_core::value_contract::TupleContract {
                        prefix: prefix
                            .iter()
                            .map(|id| recur(*id))
                            .collect::<Result<_, _>>()?,
                        variable: variable.map(|id| recur(id).map(Box::new)).transpose()?,
                        suffix: suffix
                            .iter()
                            .map(|id| recur(*id))
                            .collect::<Result<_, _>>()?,
                    },
                },
                domain: None,
                nullable: false,
            },
            Node::Protocol { identity, .. }
            | Node::NewType { identity, .. }
            | Node::Instance { identity, .. }
                if identity.source == "/analysis_stubs.pyi"
                    && (identity.path.len() == 1
                        || (identity.path.len() == 2 && identity.path[0] == "analysis_stubs")) =>
            {
                self.declarations
                    .contracts
                    .get(
                        identity
                            .path
                            .last()
                            .ok_or(InferenceGraphError::NominalNameMissing)?,
                    )
                    .cloned()
                    .ok_or(InferenceGraphError::NominalUnsealed)?
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
                                return Err(InferenceGraphError::DictionaryKeyNotString.into());
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
                            return Err(InferenceGraphError::UnsupportedNamedType {
                                name: name.to_owned(),
                            }
                            .into());
                        }
                    }
                } else if identity.source.ends_with("/datetime.pyi") {
                    plasm_core::temporal_value::TemporalKind::parse(name)
                        .ok_or(InferenceGraphError::UnsupportedTemporal)?
                        .contract()
                } else {
                    return Err(InferenceGraphError::UnsealedNamedType {
                        identity: format!("{}::{name}", identity.source),
                    }
                    .into());
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
                    .ok_or(InferenceGraphError::EmptyIntersection)?;
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
            _ => return Err(InferenceGraphError::UnsupportedNode.into()),
        })
    }
}

/// Reconstruct both successor contracts from upstream evidence for a sealed
/// single-expression compute. The paths identify observations, not predicate syntax.
pub(crate) fn branch_contracts(
    source: &str,
    argument: &Type,
    paths: &[Vec<String>],
) -> Result<Option<Vec<(Type, Type)>>, InferenceError> {
    let parsed = ruff_python_parser::parse_module(source)?;
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
                super::upstream::validate_member(field).map_err(|_| {
                    DeclarationError::InvalidMember {
                        field: field.clone(),
                    }
                })?;
            }
            Ok(format!("{name}.{}", path.join(".")))
        })
        .collect::<Result<Vec<_>, InferenceError>>()?;
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
            return Err(InferenceError::Diagnostics {
                diagnostics: errors.into(),
            });
        }
    };
    let decoder = Decoder {
        graph: &graph,
        declarations: &declarations,
    };
    let values = decoder.decode_roots(&graph.roots)?;
    if values.len() != paths.len().saturating_mul(2) {
        return Err(InferenceGraphError::InvalidReference.into());
    }
    let mut values = values.into_iter();
    let yes = values.by_ref().take(paths.len()).collect::<Vec<_>>();
    Ok(Some(yes.into_iter().zip(values).collect()))
}

pub(crate) fn observation_paths(value: &Type) -> Result<Vec<Vec<String>>, InferenceError> {
    let mut paths = Vec::new();
    let mut pending = vec![(value, Vec::new())];
    while let Some((value, path)) = pending.pop() {
        if !path.is_empty() {
            paths.push(path.clone());
        }
        if !value.nullable {
            if let ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } =
                &value.shape
            {
                for (name, value) in fields.iter().rev() {
                    let mut path = path.clone();
                    path.push(name.clone());
                    pending.push((value, path));
                }
            }
        }
    }
    Ok(paths)
}
