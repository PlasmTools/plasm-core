use super::*;
use ruff_python_ast::{Parameters, StmtFunctionDef};

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
}
impl<'a> Declaration<'a> {
    fn classify(stmt: &'a Stmt) -> Result<Self, String> {
        match stmt {
            Stmt::ClassDef(class) => Ok(Self::Program(class)),
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::StringLiteral(_)) => {
                Ok(Self::Documentation(()))
            }
            Stmt::FunctionDef(def) if def.name.as_str() == "build" => Ok(Self::Build(def)),
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
                    parameters(&def.parameters, 1)?;
                    if !def.decorator_list.is_empty() || def.returns.is_some() {
                        return Err(at(
                            def,
                            "build decorators and return annotations are not admitted yet",
                        ));
                    }
                    continue;
                }
                Declaration::Compute(def) => {
                    synchronous(def)?;
                    def
                }
            };
            if def.name.as_str().starts_with('_')
                || def.decorator_list.len() != 1
                || name(&def.decorator_list[0].expression) != Some("compute")
            {
                return Err(at(def, "only build and @compute methods are admitted"));
            }
            parameters(&def.parameters, 2)?;
            let param = &def.parameters.args[1].parameter;
            let ann = param
                .annotation
                .as_deref()
                .ok_or("compute requires an input annotation")?;
            let value = if let PyExpr::Subscript(list) = ann {
                if name(&list.value) == Some("list") {
                    &*list.slice
                } else {
                    ann
                }
            } else {
                ann
            };
            let owner = if let PyExpr::Subscript(value) = value {
                if name(&value.value).is_some_and(crate::python_compute::is_entity_record_type) {
                    Some(
                        symbols
                            .resolve_session_entity(
                                name(&value.slice).ok_or("expected entity symbol")?,
                            )
                            .map_err(|e| e.to_string())?,
                    )
                } else {
                    None
                }
            } else {
                None
            };
            let returns = def
                .returns
                .as_deref()
                .map(|annotation| format!(" -> {}", &source[annotation.range()]))
                .unwrap_or_default();
            let body = crate::python_compute::definition_body(source, def)?;
            let padding = "\n".repeat(
                source[..def.name.start().to_usize()]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    .saturating_sub(imports.source.lines().count() + 1),
            );
            let extracted = format!(
                "{}{padding}@compute\ndef {}({}: {}){}:{}\n",
                imports.source,
                def.name,
                param.name,
                &source[ann.range()],
                returns,
                body
            );
            if let Some(owner) = owner {
                let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
                    es,
                    owner.entry_id.as_str(),
                    owner.entity.as_str(),
                )?;
                crate::python_compute::PreparedCompute::prepare_typed(
                    &extracted,
                    cgs,
                    owner.entry_id.as_str(),
                    symbols.as_ref(),
                    None,
                    &crate::python_compute::return_domains(es)?,
                )?;
            }
            if methods.insert(def.name.to_string(), extracted).is_some() {
                return Err(at(def, "duplicate compute method"));
            }
        }
        Ok(Self {
            imports,
            name: class.name.to_string(),
            build: build.ok_or("Program requires build(self)")?,
            methods,
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
pub(super) fn parameters(p: &Parameters, count: usize) -> Result<(), String> {
    if !p.posonlyargs.is_empty()
        || !p.kwonlyargs.is_empty()
        || p.vararg.is_some()
        || p.kwarg.is_some()
        || p.args.len() != count
        || p.args.iter().any(|a| a.default.is_some())
        || p.args[0].parameter.name.as_str() != "self"
        || p.args[0].parameter.annotation.is_some()
    {
        return Err("method requires self and explicit required positional parameters".into());
    }
    Ok(())
}
