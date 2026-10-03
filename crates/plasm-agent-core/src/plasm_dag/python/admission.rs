use super::*;
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
    fn classify(stmt: &'a Stmt) -> Result<Self, String> {
        match stmt {
            Stmt::ClassDef(class) => Ok(Self::Program(class)),
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::StringLiteral(_)) => {
                Ok(Self::Documentation(()))
            }
            Stmt::FunctionDef(def) if def.name.as_str() == "build" => Ok(Self::Build(def)),
            Stmt::FunctionDef(def) if def.decorator_list.is_empty() => Ok(Self::Helper(def)),
            Stmt::FunctionDef(def) => Ok(Self::Compute(def)),
            _ => Err(at(
                stmt,
                "class state and executable class bodies are not admitted",
            )),
        }
    }
}
impl DeclarationKind {
    /// Outer declaration candidates only; full admission remains session-aware.
    pub fn inventory(source: &str) -> Result<Vec<Self>, String> {
        let ast = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let (_, suite) = crate::python_datetime::Imports::split(source, ast.suite())?;
        let [stmt] = suite else {
            return Err("expected exactly one Program subclass".into());
        };
        let root = Declaration::classify(stmt)?;
        let Declaration::Program(class) = root else {
            return Err("expected exactly one Program subclass".into());
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
    pub fn parse(source: &str, suite: &'a [Stmt], es: &ExecuteSession) -> Result<Self, String> {
        let (imports, suite) = crate::python_datetime::Imports::split(source, suite)?;
        let [stmt] = suite else {
            return Err("expected exactly one Program subclass".into());
        };
        let Declaration::Program(class) = Declaration::classify(stmt)
            .map_err(|_| "expected exactly one Program subclass".to_owned())?
        else {
            return Err("expected exactly one Program subclass".into());
        };
        if class.name.as_str() == "Program" {
            return Err(at(class, "reserved root name"));
        }
        let args = class
            .arguments
            .as_ref()
            .ok_or("root must derive directly from Program")?;
        if args.args.len() != 1
            || name(&args.args[0]) != Some("Program")
            || !args.keywords.is_empty()
            || !class.decorator_list.is_empty()
            || class.type_params.is_some()
        {
            return Err(at(class, "root must derive directly and only from Program"));
        }
        let mut build = None;
        let mut methods = BTreeMap::new();
        let mut helpers = BTreeMap::new();
        let symbols = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(es, None);
        for local in imports.bindings.keys() {
            if symbols.resolve_session_entity(local).is_ok() || local == class.name.as_str() {
                return Err("import cannot shadow a session entity or Program class".into());
            }
        }
        if symbols.resolve_session_entity(class.name.as_str()).is_ok() {
            return Err(at(class, "root cannot shadow a session entity"));
        }
        for stmt in &class.body {
            let declaration = Declaration::classify(stmt)?;
            let def = match declaration {
                Declaration::Documentation(()) => continue,
                Declaration::Program(_) => {
                    return Err(at(
                        stmt,
                        "class state and executable class bodies are not admitted",
                    ))
                }
                Declaration::Build(def) => {
                    synchronous(def)?;
                    if build.replace(def).is_some() {
                        return Err(at(def, "duplicate build method"));
                    }
                    if def.parameters.vararg.is_some() || def.parameters.kwarg.is_some() {
                        return Err(at(
                            def,
                            "variadic build inputs have no materialization port",
                        ));
                    }
                    if !def.decorator_list.is_empty() || def.returns.is_some() {
                        return Err(at(
                            def,
                            "build decorators and return annotations have no Program interface",
                        ));
                    }
                    let binding = monty_analysis::bind_one_positional(&monty::statement_source(
                        &Stmt::FunctionDef(def.clone()),
                    ))?;
                    if binding.parameter != "self" || binding.variadic {
                        return Err(at(def, "build requires the Program receiver self"));
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
                    return Err(at(def, "helper requires self"));
                };
                if receiver.parameter.name.as_str() != "self"
                    || receiver.default.is_some()
                    || def.name.as_str().starts_with("__")
                {
                    return Err(at(
                        def,
                        "invalid Program helper receiver or reserved method",
                    ));
                }
                if helper.parameters.vararg.is_some() || helper.parameters.kwarg.is_some() {
                    return Err(at(def, "variadic helper inputs have no DAG port"));
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
                    return Err(at(def, "duplicate Program method"));
                }
                continue;
            }
            if def.name.as_str().starts_with('_')
                || def.decorator_list.len() != 1
                || name(&def.decorator_list[0].expression) != Some("compute")
            {
                return Err(at(def, "method decorators require exactly @compute"));
            }
            let mut extracted = def.clone();
            let receiver = if !extracted.parameters.posonlyargs.is_empty() {
                extracted.parameters.posonlyargs.remove(0)
            } else if !extracted.parameters.args.is_empty() {
                extracted.parameters.args.remove(0)
            } else {
                return Err(at(def, "compute requires a bound Program receiver"));
            };
            if receiver.default.is_some() {
                closed_default(&receiver)?;
            }
            if receiver.parameter.name.as_str() != "self" {
                return Err(at(def, "Program receiver must be named self"));
            }
            if extracted.parameters.vararg.is_some() || extracted.parameters.kwarg.is_some() {
                return Err(at(
                    def,
                    "variadic compute inputs have no typed materialization port",
                ));
            }
            let inputs = extracted
                .parameters
                .posonlyargs
                .iter()
                .chain(&extracted.parameters.args)
                .chain(&extracted.parameters.kwonlyargs)
                .collect::<Vec<_>>();
            if inputs
                .iter()
                .any(|input| input.parameter.annotation.is_none())
            {
                return Err(at(def, "every compute input requires an annotation"));
            }
            for input in &inputs {
                if input.default.is_some() {
                    closed_default(input)?;
                }
            }
            let extracted = method_source(&imports.source, source, def, extracted)?;
            // A compute body is checked at its call site against the actual
            // projected input contract. The nominal entity alone cannot describe
            // observed relations or derived fields.
            if helpers.contains_key(def.name.as_str())
                || methods.insert(def.name.to_string(), extracted).is_some()
            {
                return Err(at(def, "duplicate compute method"));
            }
        }
        Ok(Self {
            imports,
            name: class.name.to_string(),
            build: build.ok_or("Program requires build(self)")?,
            methods,
            helpers,
        })
    }
}
fn synchronous(def: &StmtFunctionDef) -> Result<(), String> {
    if def.is_async || def.type_params.is_some() {
        return Err(at(
            def,
            "expected a synchronous method without type parameters",
        ));
    }
    Ok(())
}
fn closed_default(parameter: &ruff_python_ast::ParameterWithDefault) -> Result<(), String> {
    let default = parameter
        .default
        .as_deref()
        .ok_or("missing bound default")?;
    // Program classes are static declarations, not executed Python objects.
    // A scalar constant is representable as declaration metadata. A computed or
    // mutable default would require definition-time execution/state; deferring it
    // until argument omission changes Python semantics (including exceptions).
    literal(default).map_err(|_| at(default, "method defaults require scalar constant metadata; definition-time execution and mutable default state have no Program representation"))?;
    Ok(())
}

/// Normalize a callable signature without rewriting its diagnostic-bearing body.
pub(super) fn method_source(
    imports: &str,
    source: &str,
    original: &StmtFunctionDef,
    normalized: StmtFunctionDef,
) -> Result<String, String> {
    let rendered = monty::statement_source(&Stmt::FunctionDef(normalized));
    let parsed = ruff_python_parser::parse_module(&rendered).map_err(|e| e.to_string())?;
    let Some(Stmt::FunctionDef(def)) = parsed.suite().last() else {
        return Err("missing normalized method".into());
    };
    let first = def.body.first().ok_or("missing normalized body")?;
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
