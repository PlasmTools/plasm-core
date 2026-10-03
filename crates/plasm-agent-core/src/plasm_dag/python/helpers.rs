//! Program methods elaborate through the same upstream flow and scoped DAG laws
//! as callbacks. Extra arguments are captures, not implicit rowset joins.
use super::*;

impl Lower<'_> {
    fn pure_typed_helper(&self, def: &ruff_python_ast::StmtFunctionDef) -> bool {
        use ruff_python_ast::visitor::{self, Visitor};
        if def.parameters.vararg.is_some()
            || def.parameters.kwarg.is_some()
            || def
                .parameters
                .posonlyargs
                .iter()
                .chain(&def.parameters.args)
                .chain(&def.parameters.kwonlyargs)
                .any(|arg| arg.parameter.annotation.is_none())
        {
            return false;
        }
        struct HostReference<'a> {
            symbols: &'a dyn plasm_core::symbol_tuning::SymbolResolve,
            found: bool,
        }
        impl<'a> Visitor<'a> for HostReference<'_> {
            fn visit_expr(&mut self, expr: &'a PyExpr) {
                match expr {
                    PyExpr::Name(n)
                        if n.id.as_str() == "self"
                            || self.symbols.resolve_session_entity(n.id.as_str()).is_ok() =>
                    {
                        self.found = true
                    }
                    PyExpr::Attribute(a)
                        if self.symbols.resolve_session_method(a.attr.as_str()).is_ok() =>
                    {
                        self.found = true
                    }
                    _ => {}
                }
                visitor::walk_expr(self, expr);
            }
        }
        let symbols = self.state.sym_map_for(self.es);
        let mut scan = HostReference {
            symbols: symbols.as_ref(),
            found: false,
        };
        scan.visit_body(&def.body);
        !scan.found
    }

    pub(super) fn helper_call(
        &mut self,
        site: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        name: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let def = self.helpers.get(name).ok_or("missing helper")?.clone();
        if self.pure_typed_helper(&def) {
            // A typed helper without Plasm references is a whole Python value
            // function. Keep mutable locals inside Monty; materialize only its
            // result through the same checked boundary as explicit @compute.
            let code = format!(
                "@compute\n{}",
                monty::statement_source(&Stmt::FunctionDef(def))
            );
            return self.text_compute_source(site, call, code, id);
        }
        let identity = format!("method:{name}");
        if self.active_callbacks.contains(&identity) {
            return Err(at(site, "recursive DAG methods have no bounded expansion"));
        }
        let mut args = call
            .arguments
            .args
            .iter()
            .map(|e| (None, e))
            .collect::<Vec<_>>();
        for keyword in &call.arguments.keywords {
            args.push((
                Some(
                    keyword
                        .arg
                        .as_ref()
                        .ok_or("expanded helper arguments have no DAG port")?
                        .to_string(),
                ),
                &keyword.value,
            ));
        }
        let signature = monty::statement_source(&Stmt::FunctionDef(def.clone()));
        let bindings = monty_analysis::bind_arguments(
            &signature,
            &args
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
        )?;
        let parameters = def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
            .collect::<Vec<_>>();
        let mut closure = BTreeMap::new();
        for (binding, (_, expression)) in bindings.iter().zip(&args) {
            if binding.variadic {
                return Err("variadic helper inputs have no DAG port".into());
            }
            closure.insert(binding.parameter.clone(), self.expr(expression, None)?);
        }
        for parameter in &parameters {
            if !closure.contains_key(parameter.parameter.name.as_str()) {
                let default = parameter.default.as_deref().ok_or("missing helper input")?;
                closure.insert(
                    parameter.parameter.name.to_string(),
                    self.expr(default, None)?,
                );
            }
        }
        // A method executes once. All authored arguments remain independent
        // captures, including plural rowsets; the unit port is only scheduling.
        let unit = *ruff_python_parser::parse_expression("None")
            .map_err(|e| e.to_string())?
            .into_syntax()
            .body;
        let source = self.expr(&unit, None)?;
        let row = self.fresh_parameter("method");
        let lambda = match *ruff_python_parser::parse_expression(&format!("lambda {row}: None"))
            .map_err(|e| e.to_string())?
            .into_syntax()
            .body
        {
            PyExpr::Lambda(lambda) => lambda,
            _ => unreachable!(),
        };
        for parameter in &parameters {
            if let Some(annotation) = &parameter.parameter.annotation {
                let binding = &closure[parameter.parameter.name.as_str()];
                let schema = text::inferred_schema(self.es, &self.state, binding, 0)?;
                let contract = super::super::binding_contract(&self.state, binding)
                    .ok_or("helper argument contract missing")?;
                let actual = if contract.value_kind == BindingValueKind::ScalarCell {
                    schema
                        .fields
                        .first()
                        .and_then(|f| f.value_type.clone())
                        .ok_or("helper scalar contract missing")?
                } else {
                    schema.row_contract()?
                };
                let actual = if !contract.row_cardinality.permits_scalar_field_extract() {
                    plasm_core::value_contract::ValueContract {
                        shape: plasm_core::value_contract::ValueShape::Array {
                            element: Box::new(actual),
                        },
                        domain: None,
                        nullable: false,
                    }
                } else {
                    actual
                };
                crate::python_compute::check_callback_return(
                    self.es,
                    annotation,
                    &schema.row_contract()?,
                    &actual,
                    &self.imports.source,
                )?;
            }
        }
        let span = monty_analysis::Span {
            start: def.start().to_u32(),
            end: def.end().to_u32(),
        };
        let callback = callbacks::Callback {
            locals: monty_analysis::function_locals(self.program_source, span)?
                .into_iter()
                .filter(|n| {
                    n != "self" && !parameters.iter().any(|p| p.parameter.name.as_str() == n)
                })
                .collect(),
            lambda,
            flow: Some(monty_analysis::function_flow(self.program_source, span)?),
            returns: def.returns.clone(),
            closure: Some(closure),
            identity: Some(identity),
            binding: None,
            lexical_callbacks: Some(std::sync::Arc::new(BTreeMap::new())),
        };
        let body = self.scoped_callback_body(
            site,
            &source,
            &callback,
            std::num::NonZeroU32::new(1).unwrap(),
            body::ScopeMode::Rows,
        )?;
        let schema = crate::map_body_schema::output_schema(self.es, &body)?;
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: false,
            page_size: None,
            source: super::super::types::DagNodeSource::MapBody {
                body: Box::new(body),
                schema,
            },
        })
    }
}
