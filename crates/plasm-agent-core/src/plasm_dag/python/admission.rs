use super::*;
use ruff_python_ast::{Parameters, StmtFunctionDef};

pub(super) struct Root<'a> {
    pub name: String,
    pub build: &'a StmtFunctionDef,
    pub methods: BTreeMap<String, String>,
}
impl<'a> Root<'a> {
    pub fn parse(source: &str, suite: &'a [Stmt], es: &ExecuteSession) -> Result<Self, String> {
        let [Stmt::ClassDef(class)] = suite else {
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
        if symbols.resolve_session_entity(class.name.as_str()).is_ok() {
            return Err(at(class, "root cannot shadow a session entity"));
        }
        for stmt in &class.body {
            if let Stmt::Expr(s) = stmt {
                if matches!(&*s.value, PyExpr::StringLiteral(_)) {
                    continue;
                }
            }
            let Stmt::FunctionDef(def) = stmt else {
                return Err(at(
                    stmt,
                    "class state and executable class bodies are not admitted",
                ));
            };
            if def.is_async || def.type_params.is_some() {
                return Err(at(
                    def,
                    "expected a synchronous method without type parameters",
                ));
            }
            if def.name.as_str() == "build" {
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
                .ok_or("compute requires a collection annotation")?;
            let value = if let PyExpr::Subscript(list) = ann {
                if name(&list.value) == Some("list") {
                    &*list.slice
                } else {
                    ann
                }
            } else {
                ann
            };
            let inferred = name(value) == Some("Row");
            let owner = if inferred {
                None
            } else {
                let PyExpr::Subscript(value) = value else {
                    return Err(at(ann, "expected Row, Value[eN], or a list of either"));
                };
                if name(&value.value) != Some("Value") {
                    return Err(at(ann, "expected Value[eN]"));
                }
                Some(
                    symbols
                        .resolve_session_entity(name(&value.slice).ok_or("expected entity symbol")?)
                        .map_err(|e| e.to_string())?,
                )
            };
            let returns = def
                .returns
                .as_deref()
                .ok_or("compute requires a return annotation")?;
            let [Stmt::Return(ret)] = def.body.as_slice() else {
                return Err(at(def, "compute requires one pure return expression"));
            };
            // Preserve every byte inside string literals. Only prepend the function's
            // first indentation; continuation lines already carry valid Python layout.
            let body = format!("    {}", &source[ret.range()]);
            let extracted = format!(
                "@compute\ndef {}({}: {}) -> {}:\n{}\n",
                def.name,
                param.name,
                &source[ann.range()],
                &source[returns.range()],
                body
            );
            if let Some(owner) = owner {
                let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
                    es,
                    owner.entry_id.as_str(),
                    owner.entity.as_str(),
                )?;
                let per_row =
                    !matches!(ann, PyExpr::Subscript(s) if name(&s.value) == Some("list"));
                crate::python_compute::CheckedCompute::compile_input(
                    &extracted,
                    cgs,
                    owner.entry_id.as_str(),
                    symbols.as_ref(),
                    None,
                    per_row,
                )?;
            } else if name(returns) != Some("str") {
                return Err(at(returns, "compute requires -> str"));
            }
            if methods.insert(def.name.to_string(), extracted).is_some() {
                return Err(at(def, "duplicate compute method"));
            }
        }
        Ok(Self {
            name: class.name.to_string(),
            build: build.ok_or("Program requires build(self)")?,
            methods,
        })
    }
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
