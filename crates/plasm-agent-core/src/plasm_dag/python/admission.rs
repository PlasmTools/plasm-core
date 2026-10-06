use super::*;
use crate::program_rejection::PythonLoweringInvariantError;
use crate::program_rejection::PythonProgramError;
use ruff_python_ast::StmtFunctionDef;

macro_rules! declarations {
    ($($variant:ident($payload:ty) => $name:literal),+ $(,)?) => {
        enum Declaration<'a> { $($variant($payload)),+ }
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DeclarationKind { $($variant),+ }
        impl DeclarationKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
        }
        impl Declaration<'_> {
            fn kind(&self) -> DeclarationKind { match self { $(Self::$variant(_) => DeclarationKind::$variant),+ } }
        }
    }
}
declarations! {
    Program(&'a ruff_python_ast::StmtClassDef) => "program",
    Documentation(()) => "documentation",
    Build(&'a StmtFunctionDef) => "build",
    Compute(&'a StmtFunctionDef) => "compute",
    Helper(&'a StmtFunctionDef) => "helper",
}
impl<'a> Declaration<'a> {
    fn classify(stmt: &'a Stmt) -> Result<Self, PythonLoweringError> {
        match stmt {
            Stmt::ClassDef(class) => Ok(Self::Program(class)),
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::StringLiteral(_)) => {
                Ok(Self::Documentation(()))
            }
            Stmt::FunctionDef(def) if def.name.as_str() == "build" => Ok(Self::Build(def)),
            Stmt::FunctionDef(def) if def.decorator_list.is_empty() => Ok(Self::Helper(def)),
            Stmt::FunctionDef(def) => Ok(Self::Compute(def)),
            _ => Err(at(stmt, PythonSourceError::ClassExecutableState)),
        }
    }
}
impl DeclarationKind {
    /// Outer declaration candidates only; full admission remains session-aware.
    pub fn inventory(source: &str) -> Result<Vec<Self>, PythonLoweringError> {
        let ast = ruff_python_parser::parse_module(source).map_err(|error| {
            PythonLoweringError::Source {
                error: std::sync::Arc::new(crate::program_rejection::PythonSourceError::Parse {
                    source: error,
                }),
                span: None,
            }
        })?;
        let (_, suite) = crate::python_datetime::Imports::split(source, ast.suite())?;
        let [stmt] = suite else {
            return Err(PythonProgramError::ProgramDeclarationCount.into());
        };
        let root = Declaration::classify(stmt)?;
        let Declaration::Program(class) = root else {
            return Err(PythonProgramError::ProgramDeclarationMissing.into());
        };
        let mut kinds = vec![root.kind()];
        for stmt in &class.body {
            kinds.push(Declaration::classify(stmt)?.kind());
        }
        Ok(kinds)
    }
}

pub(super) struct Root<'a> {
    pub imports: crate::python_datetime::Imports,
    pub name: String,
    pub build: &'a StmtFunctionDef,
    pub methods: BTreeMap<String, String>,
    pub helpers: BTreeMap<String, StmtFunctionDef>,
}
impl<'a> Root<'a> {
    pub fn parse(
        source: &str,
        suite: &'a [Stmt],
        es: &ExecuteSession,
    ) -> Result<Self, PythonLoweringError> {
        let (imports, suite) = crate::python_datetime::Imports::split(source, suite)?;
        let [stmt] = suite else {
            return Err(PythonProgramError::ProgramDeclarationCount.into());
        };
        let Declaration::Program(class) = Declaration::classify(stmt)
            .map_err(|_| PythonProgramError::ProgramDeclarationMissing)?
        else {
            return Err(PythonProgramError::ProgramDeclarationMissing.into());
        };
        if class.name.as_str() == "Program" {
            return Err(at(
                class,
                PythonSourceError::ReservedRootName {
                    name: class.name.to_string(),
                },
            ));
        }
        let args = class
            .arguments
            .as_ref()
            .ok_or(PythonProgramError::ProgramBaseInvalid)?;
        if args.args.len() != 1
            || name(&args.args[0]) != Some("Program")
            || !args.keywords.is_empty()
            || !class.decorator_list.is_empty()
            || class.type_params.is_some()
        {
            return Err(at(class, PythonSourceError::ProgramBaseShape));
        }
        let mut build = None;
        let mut methods = BTreeMap::new();
        let mut helpers = BTreeMap::new();
        let symbols = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(es, None);
        for local in imports.bindings.keys() {
            if symbols.resolve_session_entity(local).is_ok() || local == class.name.as_str() {
                return Err(PythonProgramError::ImportShadowsReservedBinding.into());
            }
        }
        if symbols.resolve_session_entity(class.name.as_str()).is_ok() {
            return Err(at(
                class,
                PythonSourceError::RootShadowsEntity {
                    name: class.name.to_string(),
                },
            ));
        }
        for stmt in &class.body {
            let declaration = Declaration::classify(stmt)?;
            let def = match declaration {
                Declaration::Documentation(()) => continue,
                Declaration::Program(_) => {
                    return Err(at(stmt, PythonSourceError::ClassExecutableState))
                }
                Declaration::Build(def) => {
                    synchronous(def)?;
                    if build.replace(def).is_some() {
                        return Err(at(def, PythonSourceError::DuplicateBuildMethod));
                    }
                    if def.parameters.vararg.is_some() || def.parameters.kwarg.is_some() {
                        return Err(at(def, PythonSourceError::VariadicBuildInputs));
                    }
                    if !def.decorator_list.is_empty() || def.returns.is_some() {
                        return Err(at(def, PythonSourceError::BuildInterfaceShape));
                    }
                    let binding = monty_analysis::bind_one_positional(&monty::statement_source(
                        &Stmt::FunctionDef(def.clone()),
                    ))?;
                    if binding.parameter != "self" || binding.variadic {
                        return Err(at(def, PythonSourceError::BuildReceiverShape));
                    }
                    for parameter in def
                        .parameters
                        .posonlyargs
                        .iter()
                        .chain(&def.parameters.args)
                        .chain(&def.parameters.kwonlyargs)
                    {
                        if parameter.default.is_some() {
                            closed_default(parameter)?;
                        }
                    }
                    continue;
                }
                Declaration::Compute(def) | Declaration::Helper(def) => {
                    synchronous(def)?;
                    def
                }
            };
            if def.decorator_list.is_empty() {
                let mut helper = def.clone();
                let receiver = if !helper.parameters.posonlyargs.is_empty() {
                    helper.parameters.posonlyargs.remove(0)
                } else if !helper.parameters.args.is_empty() {
                    helper.parameters.args.remove(0)
                } else {
                    return Err(at(def, PythonSourceError::HelperReceiverMissing));
                };
                if receiver.parameter.name.as_str() != "self"
                    || receiver.default.is_some()
                    || def.name.as_str().starts_with("__")
                {
                    return Err(at(def, PythonSourceError::HelperDeclarationShape));
                }
                if helper.parameters.vararg.is_some() || helper.parameters.kwarg.is_some() {
                    return Err(at(def, PythonSourceError::VariadicHelperInputs));
                }
                for p in helper
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(&helper.parameters.args)
                    .chain(&helper.parameters.kwonlyargs)
                {
                    if p.default.is_some() {
                        closed_default(p)?;
                    }
                }
                if helpers.insert(def.name.to_string(), helper).is_some()
                    || methods.contains_key(def.name.as_str())
                {
                    return Err(at(
                        def,
                        PythonSourceError::DuplicateProgramMethod {
                            method: def.name.to_string(),
                        },
                    ));
                }
                continue;
            }
            if def.name.as_str().starts_with('_')
                || def.decorator_list.len() != 1
                || name(&def.decorator_list[0].expression) != Some("compute")
            {
                return Err(at(def, PythonSourceError::ComputeDecoratorShape));
            }
            let mut extracted = def.clone();
            let receiver = if !extracted.parameters.posonlyargs.is_empty() {
                extracted.parameters.posonlyargs.remove(0)
            } else if !extracted.parameters.args.is_empty() {
                extracted.parameters.args.remove(0)
            } else {
                return Err(at(def, PythonSourceError::ComputeReceiverMissing));
            };
            if receiver.default.is_some() {
                closed_default(&receiver)?;
            }
            if receiver.parameter.name.as_str() != "self" {
                return Err(at(
                    def,
                    PythonSourceError::ProgramReceiverName {
                        name: receiver.parameter.name.to_string(),
                    },
                ));
            }
            if extracted.parameters.vararg.is_some() || extracted.parameters.kwarg.is_some() {
                return Err(at(def, PythonSourceError::VariadicComputeInputs));
            }
            let inputs = extracted
                .parameters
                .posonlyargs
                .iter()
                .chain(&extracted.parameters.args)
                .chain(&extracted.parameters.kwonlyargs)
                .collect::<Vec<_>>();
            for input in &inputs {
                if let Some(annotation) = input.parameter.annotation.as_deref() {
                    if let Some(problem) =
                        ComputeAnnotationProblem::recognize(annotation, symbols.as_ref())
                    {
                        return Err(at(annotation, problem.error()));
                    }
                }
            }
            for input in &inputs {
                if input.default.is_some() {
                    closed_default(input)?;
                }
            }
            reject_compute_boundary_violation(def, symbols.as_ref())
                .map_err(|violation| violation.correction())?;
            let extracted = method_source(&imports.source, source, def, extracted)?;
            // A compute body is checked at its call site against the actual
            // projected input contract. The nominal entity alone cannot describe
            // observed relations or derived fields.
            if helpers.contains_key(def.name.as_str())
                || methods.insert(def.name.to_string(), extracted).is_some()
            {
                return Err(at(
                    def,
                    PythonSourceError::DuplicateComputeMethod {
                        method: def.name.to_string(),
                    },
                ));
            }
        }
        Ok(Self {
            imports,
            name: class.name.to_string(),
            build: build.ok_or(PythonLoweringInvariantError::ProgramBuildMissing)?,
            methods,
            helpers,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComputeAnnotationProblem {
    DagRowHandle(plasm_core::python_row_shape::PythonRowShape),
    EntityAsCollectionElement,
}

impl ComputeAnnotationProblem {
    fn recognize(
        annotation: &PyExpr,
        symbols: &dyn plasm_core::symbol_tuning::SymbolResolve,
    ) -> Option<Self> {
        let head = match annotation {
            PyExpr::Subscript(subscript) => name(&subscript.value),
            _ => name(annotation),
        };
        if let Some(shape) =
            head.and_then(plasm_core::python_row_shape::PythonRowShape::from_card_alias)
        {
            return Some(Self::DagRowHandle(shape));
        }
        match head {
            Some("list") => match annotation {
                PyExpr::Subscript(subscript)
                    if name(&subscript.slice)
                        .is_some_and(|token| symbols.resolve_session_entity(token).is_ok()) =>
                {
                    Some(Self::EntityAsCollectionElement)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn error(self) -> PythonSourceError {
        match self {
            Self::DagRowHandle(shape) => match shape {
                plasm_core::python_row_shape::PythonRowShape::Rows => {
                    PythonSourceError::ComputeRowsHandleAnnotation
                }
                plasm_core::python_row_shape::PythonRowShape::Singleton => {
                    PythonSourceError::ComputeSingletonHandleAnnotation
                }
            },
            Self::EntityAsCollectionElement => PythonSourceError::ComputeEntityElementAnnotation,
        }
    }
}

enum ComputeBoundaryViolation<'a> {
    Write(&'a ruff_python_ast::ExprCall),
    Relation(&'a ruff_python_ast::ExprAttribute),
}

impl ComputeBoundaryViolation<'_> {
    fn correction(&self) -> PythonLoweringError {
        match self {
            Self::Write(call) => at(call, PythonSourceError::ComputeWriteAuthority),
            Self::Relation(attribute) => at(attribute, PythonSourceError::ComputeRelationAuthority),
        }
    }
}

fn reject_compute_boundary_violation<'a>(
    def: &'a StmtFunctionDef,
    symbols: &dyn plasm_core::symbol_tuning::SymbolResolve,
) -> Result<(), ComputeBoundaryViolation<'a>> {
    use ruff_python_ast::visitor::{self, Visitor};

    struct Boundary<'a, 'b> {
        symbols: &'b dyn plasm_core::symbol_tuning::SymbolResolve,
        violation: Option<ComputeBoundaryViolation<'a>>,
    }
    impl<'a> Visitor<'a> for Boundary<'a, '_> {
        fn visit_expr(&mut self, expr: &'a PyExpr) {
            if self.violation.is_none() {
                if let PyExpr::Call(call) = expr {
                    if let PyExpr::Attribute(attribute) = call.func.as_ref() {
                        let taught_write = self
                            .symbols
                            .resolve_session_method(attribute.attr.as_str())
                            .is_ok_and(|method| {
                                matches!(
                                    super::catalog_operations::CatalogOperation::from_kind(
                                        method.kind
                                    ),
                                    super::catalog_operations::CatalogOperation::Write(_)
                                )
                            });
                        if taught_write {
                            self.violation = Some(ComputeBoundaryViolation::Write(call));
                            return;
                        }
                    }
                }
                if let PyExpr::Attribute(attribute) = expr {
                    if self
                        .symbols
                        .resolve_session_relation(attribute.attr.as_str())
                        .is_ok()
                    {
                        self.violation = Some(ComputeBoundaryViolation::Relation(attribute));
                        return;
                    }
                }
            }
            visitor::walk_expr(self, expr);
        }
    }

    let mut boundary = Boundary {
        symbols,
        violation: None,
    };
    boundary.visit_body(&def.body);
    if let Some(violation) = boundary.violation {
        return Err(violation);
    }
    Ok(())
}
fn synchronous(def: &StmtFunctionDef) -> Result<(), PythonLoweringError> {
    if def.is_async || def.type_params.is_some() {
        return Err(at(
            def,
            PythonSourceError::MethodDeclarationShape {
                method: def.name.to_string(),
                is_async: def.is_async,
                has_type_parameters: def.type_params.is_some(),
            },
        ));
    }
    Ok(())
}
fn closed_default(
    parameter: &ruff_python_ast::ParameterWithDefault,
) -> Result<(), PythonLoweringError> {
    let default = parameter
        .default
        .as_deref()
        .ok_or(PythonLoweringInvariantError::BoundMethodDefaultMissing)?;
    // Program classes are static declarations, not executed Python objects.
    // A scalar constant is representable as declaration metadata. A computed or
    // mutable default would require definition-time execution/state; deferring it
    // until argument omission changes Python semantics (including exceptions).
    literal(default).map_err(|error| {
        at(
            default,
            error.with_context(
                crate::program_rejection::PythonLoweringContext::MethodDefaultRequiresScalar,
            ),
        )
    })?;
    Ok(())
}

/// Normalize a callable signature without rewriting its diagnostic-bearing body.
pub(super) fn method_source(
    imports: &str,
    source: &str,
    original: &StmtFunctionDef,
    normalized: StmtFunctionDef,
) -> Result<String, PythonLoweringError> {
    let rendered = monty::statement_source(&Stmt::FunctionDef(normalized));
    let parsed = ruff_python_parser::parse_module(&rendered).map_err(|error| {
        PythonLoweringError::Source {
            error: std::sync::Arc::new(crate::program_rejection::PythonSourceError::Parse {
                source: error,
            }),
            span: None,
        }
    })?;
    let Some(Stmt::FunctionDef(def)) = parsed.suite().last() else {
        return Err(PythonProgramError::NormalizedMethodMissing.into());
    };
    let first = def
        .body
        .first()
        .ok_or(PythonLoweringInvariantError::NormalizedMethodBodyMissing)?;
    let header = rendered[..first.start().to_usize()].trim_end();
    let body_line = source[..original.name.start().to_usize()]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let padding = "\n"
        .repeat((body_line + 1).saturating_sub(imports.lines().count() + header.lines().count()));
    let body = crate::python_compute::definition_body(source, original)?;
    Ok(format!("{imports}{padding}{header}{body}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_inventory_uses_semantic_admission_errors() {
        assert!(matches!(
            DeclarationKind::inventory("class A(Program):\n    pass\nclass B(Program):\n    pass"),
            Err(PythonLoweringError::Program(
                PythonProgramError::ProgramDeclarationCount
            ))
        ));
        assert!(DeclarationKind::inventory(
            "class A(Program):\n    def build(self):\n        return e1"
        )
        .is_ok());
    }
}
