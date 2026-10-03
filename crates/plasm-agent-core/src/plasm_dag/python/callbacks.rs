//! Lexically sealed callback declarations. No callback code is run at admission.
use super::*;

#[derive(Clone)]
pub(super) struct Callback {
    pub locals: BTreeSet<String>,
    pub lambda: ruff_python_ast::ExprLambda,
    pub flow: Option<monty_analysis::FunctionFlow>,
    pub returns: Option<Box<PyExpr>>,
    pub closure: Option<BTreeMap<String, String>>,
    pub identity: Option<String>,
    pub binding: Option<String>,
    pub lexical_callbacks: Option<std::sync::Arc<BTreeMap<String, Callback>>>,
}
impl Lower<'_> {
    pub(super) fn declare_callback(
        &mut self,
        def: &ruff_python_ast::StmtFunctionDef,
    ) -> Result<(), PythonLoweringError> {
        let label = def.name.as_str();
        if label == "self"
            || label.starts_with("__")
            || matches!(label, "Program" | "compute" | "Value" | "Row" | "agg" | "_")
            || self.imports.bindings.contains_key(label)
            || self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(label)
                .is_ok()
        {
            return Err(at(def, "reserved callback binding name"));
        }
        if def
            .decorator_list
            .iter()
            .any(|d| name(&d.expression) == Some("compute"))
        {
            return Err(at(def, "@compute must be a Program class method with self and a typed input; call it as self.method(rows). A nested def is a scoped callback"));
        }
        if def.is_async || def.type_params.is_some() || !def.decorator_list.is_empty() {
            return Err(at(
                def,
                "DAG callbacks require a synchronous undecorated callable",
            ));
        }
        let parsed =
            ruff_python_parser::parse_expression("lambda row: None").map_err(|e| e.to_string())?;
        let PyExpr::Lambda(mut lambda) = *parsed.into_syntax().body else {
            unreachable!()
        };
        lambda.parameters = Some(def.parameters.clone());
        super::projection::projection_parameter(&lambda)?;
        let flow = monty_analysis::function_flow(
            self.program_source,
            monty_analysis::Span {
                start: def.start().to_u32(),
                end: def.end().to_u32(),
            },
        )?;
        let mut closure = self.frame.names.clone();
        for parameter in def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
        {
            if let Some(default) = &parameter.default {
                let binding = self.expr(default, None)?;
                closure.insert(parameter.parameter.name.to_string(), binding);
            }
        }
        self.callbacks.insert(
            label.to_string(),
            Callback {
                locals: monty_analysis::function_locals(
                    self.program_source,
                    monty_analysis::Span {
                        start: def.start().to_u32(),
                        end: def.end().to_u32(),
                    },
                )?
                .into_iter()
                .filter(|name| {
                    !def.parameters
                        .posonlyargs
                        .iter()
                        .chain(&def.parameters.args)
                        .chain(&def.parameters.kwonlyargs)
                        .any(|p| p.parameter.name.as_str() == name)
                })
                .collect(),
                lambda,
                flow: Some(flow),
                returns: def.returns.clone(),
                closure: Some(closure),
                identity: Some(format!("{}:{}", label, def.start().to_u32())),
                binding: Some(label.to_owned()),
                lexical_callbacks: Some(std::sync::Arc::new(self.callbacks.clone())),
            },
        );
        Ok(())
    }
    pub(super) fn callback_value_call(
        &mut self,
        site: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let callback = self.callback(&call.func)?;
        let mut arguments = call
            .arguments
            .args
            .iter()
            .map(|expr| (None, expr))
            .collect::<Vec<_>>();
        for keyword in &call.arguments.keywords {
            let name = keyword
                .arg
                .as_ref()
                .ok_or("expanded callback arguments require a materialized mapping")?;
            arguments.push((Some(name.to_string()), &keyword.value));
        }
        let shape = arguments
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let binding = monty_analysis::bind_arguments(
            &projection::callable_signature(&callback.lambda)?,
            &shape,
        )?;
        let row = projection::projection_parameter(&callback.lambda)?;
        if binding.len() != 1 || binding[0].variadic || binding[0].parameter != row {
            return Err(at(site, "callback value nodes have one row port; capture other dependencies in the callback closure"));
        }
        let source = self.expr(arguments[0].1, None)?;
        let contract = super::super::binding_contract(&self.state, &source)
            .ok_or("callback input contract missing")?;
        if !contract.row_cardinality.permits_scalar_field_extract() {
            return Err(at(
                site,
                "callback value invocation requires a singleton row",
            ));
        }
        let body = self.scoped_callback_body(
            site,
            &source,
            &callback,
            std::num::NonZeroU32::new(1).unwrap(),
            super::body::ScopeMode::Value,
        )?;
        if !matches!(
            body.effect_class(),
            EffectClass::Read | EffectClass::ArtifactRead
        ) {
            return Err(at(
                site,
                "callback value expressions cannot introduce effects",
            ));
        }
        let schema = crate::map_body_schema::output_schema(self.es, &body)?;
        let scope = self.fresh();
        self.insert(DagNode {
            id: scope.clone(),
            expr: String::new(),
            singleton: true,
            page_size: None,
            source: super::super::types::DagNodeSource::MapBody {
                body: Box::new(body),
                schema,
            },
        })?;
        self.emit_value(
            PlasmDataValue::NodeSymbol {
                node: scope.clone(),
                alias: scope.clone(),
                path: vec!["value".into()],
            },
            vec![crate::plasm_plan::PlanDataInput {
                node: scope.clone(),
                alias: scope,
                cardinality: crate::plasm_plan::InputCardinality::Singleton,
            }],
            id,
        )
    }
    pub(super) fn callback(
        &mut self,
        expression: &PyExpr,
    ) -> Result<Callback, PythonLoweringError> {
        match expression {
            PyExpr::Lambda(lambda) => {
                let mut closure = self.frame.names.clone();
                if let Some(parameters) = &lambda.parameters {
                    for parameter in parameters
                        .posonlyargs
                        .iter()
                        .chain(&parameters.args)
                        .chain(&parameters.kwonlyargs)
                    {
                        if let Some(default) = &parameter.default {
                            let binding = self.expr(default, None)?;
                            closure.insert(parameter.parameter.name.to_string(), binding);
                        }
                    }
                }
                Ok(Callback {
                    locals: BTreeSet::new(),
                    lambda: lambda.clone(),
                    flow: None,
                    returns: None,
                    closure: Some(closure),
                    identity: None,
                    binding: None,
                    lexical_callbacks: None,
                })
            }
            PyExpr::Name(name) => self
                .callbacks
                .get(name.id.as_str())
                .cloned()
                .ok_or_else(|| at(expression, "unknown scoped callback")),
            _ => Err(at(
                expression,
                "expected a lambda or a declared scoped callback",
            )),
        }
    }
}

impl Callback {
    pub fn flow(&self) -> monty_analysis::FunctionFlow {
        self.flow
            .clone()
            .unwrap_or_else(|| monty_analysis::FunctionFlow::expression(*self.lambda.body.clone()))
    }
    pub fn branch(
        parameter: &str,
        flow: monty_analysis::FunctionFlow,
        closure: BTreeMap<String, String>,
    ) -> Result<Self, PythonLoweringError> {
        let parsed = ruff_python_parser::parse_expression(&format!("lambda {parameter}: None"))
            .map_err(|e| e.to_string())?;
        let PyExpr::Lambda(lambda) = *parsed.into_syntax().body else {
            unreachable!()
        };
        Ok(Self {
            locals: BTreeSet::new(),
            lambda,
            flow: Some(flow),
            returns: None,
            closure: Some(closure),
            identity: None,
            binding: None,
            lexical_callbacks: None,
        })
    }
}
