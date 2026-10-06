//! Program methods elaborate through the same upstream flow and scoped DAG laws
//! as callbacks. Extra arguments are captures, not implicit rowset joins.
use super::*;
use plasm_core::python_row_shape::PythonRowShape;

fn dag_handle_annotation(annotation: &PyExpr) -> Option<(PythonRowShape, &str)> {
    let PyExpr::Subscript(subscript) = annotation else {
        return None;
    };
    let (PyExpr::Name(alias), PyExpr::Name(entity)) =
        (subscript.value.as_ref(), subscript.slice.as_ref())
    else {
        return None;
    };
    Some((
        PythonRowShape::from_card_alias(alias.id.as_str())?,
        entity.id.as_str(),
    ))
}

fn flow_source_with_host_annotations(
    source: &str,
    helpers: &BTreeMap<String, ruff_python_ast::StmtFunctionDef>,
) -> String {
    let mut ranges = Vec::new();
    for def in helpers.values() {
        ranges.extend(
            def.parameters
                .posonlyargs
                .iter()
                .chain(&def.parameters.args)
                .chain(&def.parameters.kwonlyargs)
                .filter_map(|parameter| parameter.parameter.annotation.as_deref())
                .filter(|annotation| dag_handle_annotation(annotation).is_some())
                .map(|annotation| annotation.range()),
        );
        if let Some(annotation) = def.returns.as_deref() {
            if dag_handle_annotation(annotation).is_some() {
                ranges.push(annotation.range());
            }
        }
    }
    ranges.sort_by_key(|range| std::cmp::Reverse(range.start()));
    let mut sanitized = source.to_owned();
    for range in ranges {
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        sanitized.replace_range(
            start..end,
            &format!("object{}", " ".repeat(end - start - 6)),
        );
    }
    sanitized
}

impl Lower<'_> {
    fn check_dag_handle_annotation(
        &self,
        annotation: &PyExpr,
        label: &str,
    ) -> Result<bool, PythonLoweringError> {
        let Some((shape, entity)) = dag_handle_annotation(annotation) else {
            return Ok(false);
        };
        let expected = self
            .state
            .sym_map_for(self.es)
            .resolve_session_entity(entity)
            .map_err(|source| {
                at(
                    annotation,
                    PythonSourceError::DagHandleEntityMissing {
                        entity: entity.to_owned(),
                        source,
                    },
                )
            })?;
        let actual = super::super::binding_contract(&self.state, label)
            .ok_or(crate::program_rejection::PythonLoweringInvariantError::InputContractMissing)?;
        if actual.value_kind != BindingValueKind::EntityRow
            || actual.row_entity.entry_id.as_str() != expected.entry_id_str()
            || actual.row_entity.entity.as_str() != expected.entity_str()
            || !actual.anchor.is_present()
            || (shape == PythonRowShape::Singleton
                && !actual.row_cardinality.permits_scalar_field_extract())
        {
            return Err(at(annotation, PythonSourceError::DagHandleContractMismatch));
        }
        Ok(true)
    }

    fn pure_helper(&self, def: &ruff_python_ast::StmtFunctionDef) -> bool {
        use ruff_python_ast::visitor::{self, Visitor};
        if def.parameters.vararg.is_some() || def.parameters.kwarg.is_some() {
            return false;
        }
        if def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
            .filter_map(|parameter| parameter.parameter.annotation.as_deref())
            .any(|annotation| dag_handle_annotation(annotation).is_some())
            || def
                .returns
                .as_deref()
                .is_some_and(|annotation| dag_handle_annotation(annotation).is_some())
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
                    PyExpr::Call(call)
                        if matches!(
                            call.func.as_ref(),
                            PyExpr::Attribute(attribute)
                                if row_operations::RowOperation::parse(attribute.attr.as_str())
                                    .is_some()
                        ) =>
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

    /// A single unannotated DAG expression keeps its value shape when expanded.
    /// Python bodies and comprehensions cross the materialization boundary as
    /// one checked computation, so their local state never becomes DAG state.
    fn materialized_helper(&self, def: &ruff_python_ast::StmtFunctionDef) -> bool {
        if def.returns.is_some()
            || def
                .parameters
                .posonlyargs
                .iter()
                .chain(&def.parameters.args)
                .chain(&def.parameters.kwonlyargs)
                .any(|parameter| parameter.parameter.annotation.is_some())
        {
            return true;
        }
        let [Stmt::Return(ret)] = def.body.as_slice() else {
            return true;
        };
        let Some(value) = ret.value.as_deref() else {
            return true;
        };
        use ruff_python_ast::visitor::{self, Visitor};
        struct MaterializedExpression(bool);
        impl<'a> Visitor<'a> for MaterializedExpression {
            fn visit_expr(&mut self, expr: &'a PyExpr) {
                if matches!(
                    expr,
                    PyExpr::ListComp(_)
                        | PyExpr::SetComp(_)
                        | PyExpr::DictComp(_)
                        | PyExpr::Generator(_)
                ) {
                    self.0 = true;
                }
                visitor::walk_expr(self, expr);
            }
        }
        let mut scan = MaterializedExpression(false);
        scan.visit_expr(value);
        scan.0
    }

    pub(super) fn helper_call(
        &mut self,
        site: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        name: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let def = self
            .helpers
            .get(name)
            .ok_or(crate::program_rejection::PythonLoweringInvariantError::HelperDefinitionMissing)?
            .clone();
        if self.pure_helper(&def) && self.materialized_helper(&def) {
            // A helper without Plasm references is a whole Python value
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
            return Err(at(
                site,
                PythonSourceError::RecursiveDagMethod {
                    method: name.to_owned(),
                },
            ));
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
                        .ok_or(crate::program_rejection::PythonLoweringInvariantError::HelperDagPortMissing)?
                        .to_string(),
                ),
                &keyword.value,
            ));
        }
        let flow_source = flow_source_with_host_annotations(self.program_source, self.helpers);
        let mut bind_def = def.clone();
        let object = *ruff_python_parser::parse_expression("object")
            .map_err(PythonLoweringError::parse_error)?
            .into_syntax()
            .body;
        for parameter in bind_def
            .parameters
            .posonlyargs
            .iter_mut()
            .chain(&mut bind_def.parameters.args)
            .chain(&mut bind_def.parameters.kwonlyargs)
        {
            if parameter
                .parameter
                .annotation
                .as_deref()
                .is_some_and(|annotation| dag_handle_annotation(annotation).is_some())
            {
                parameter.parameter.annotation = Some(Box::new(object.clone()));
            }
        }
        if bind_def
            .returns
            .as_deref()
            .is_some_and(|annotation| dag_handle_annotation(annotation).is_some())
        {
            bind_def.returns = Some(Box::new(object));
        }
        let signature = monty::statement_source(&Stmt::FunctionDef(bind_def));
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
                return Err(
                    crate::program_rejection::PythonProgramError::VariadicHelperInputs.into(),
                );
            }
            closure.insert(binding.parameter.clone(), self.expr(expression, None)?);
        }
        for parameter in &parameters {
            if !closure.contains_key(parameter.parameter.name.as_str()) {
                let default = parameter.default.as_deref()
                    .ok_or(crate::program_rejection::PythonLoweringInvariantError::HelperDefaultInputMissing)?;
                closure.insert(
                    parameter.parameter.name.to_string(),
                    self.expr(default, None)?,
                );
            }
        }
        // A method executes once. All authored arguments remain independent
        // captures, including plural rowsets; the unit port is only scheduling.
        let unit = *ruff_python_parser::parse_expression("None")
            .map_err(PythonLoweringError::parse_error)?
            .into_syntax()
            .body;
        let source = self.expr(&unit, None)?;
        let row = self.fresh_parameter("method");
        let lambda = match *ruff_python_parser::parse_expression(&format!("lambda {row}: None"))
            .map_err(PythonLoweringError::parse_error)?
            .into_syntax()
            .body
        {
            PyExpr::Lambda(lambda) => lambda,
            _ => unreachable!(),
        };
        for parameter in &parameters {
            if let Some(annotation) = &parameter.parameter.annotation {
                let binding = &closure[parameter.parameter.name.as_str()];
                if self.check_dag_handle_annotation(annotation, binding)? {
                    continue;
                }
                let schema = text::inferred_schema(self.es, &self.state, binding, 0)?;
                let contract = super::super::binding_contract(&self.state, binding)
                    .ok_or(crate::program_rejection::PythonLoweringInvariantError::HelperArgumentContractMissing)?;
                let actual = if contract.value_kind == BindingValueKind::ScalarCell {
                    schema
                        .fields
                        .first()
                        .and_then(|f| f.value_type.clone())
                        .ok_or(crate::program_rejection::PythonLoweringInvariantError::HelperScalarContractMissing)?
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
            locals: monty_analysis::function_locals(&flow_source, span)?
                .into_iter()
                .filter(|n| {
                    n != "self" && !parameters.iter().any(|p| p.parameter.name.as_str() == n)
                })
                .collect(),
            lambda,
            flow: Some(monty_analysis::function_flow(&flow_source, span)?),
            returns: def
                .returns
                .clone()
                .filter(|annotation| dag_handle_annotation(annotation).is_none()),
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
        let lowered = self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: false,
            page_size: None,
            source: super::super::types::DagNodeSource::MapBody {
                body: Box::new(body),
                schema,
            },
        })?;
        if let Some(annotation) = def.returns.as_deref() {
            self.check_dag_handle_annotation(annotation, &lowered)?;
        }
        Ok(lowered)
    }
}
