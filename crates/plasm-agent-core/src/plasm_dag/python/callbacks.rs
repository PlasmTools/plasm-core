//! Lexically sealed callback declarations. No callback code is run at admission.
use super::*;

#[derive(Clone)]
pub(super) struct Callback {
    pub lambda: ruff_python_ast::ExprLambda,
    pub prelude: Vec<Stmt>,
    pub closure: Option<BTreeMap<String, String>>,
    pub identity: Option<String>,
}
impl Lower<'_> {
    pub(super) fn declare_callback(
        &mut self,
        def: &ruff_python_ast::StmtFunctionDef,
    ) -> Result<(), String> {
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
        if self.callbacks.contains_key(label) || self.state.contains(self.scoped_binding(label)) {
            return Err(at(def, "rebinding is not admitted"));
        }
        if def
            .decorator_list
            .iter()
            .any(|d| name(&d.expression) == Some("compute"))
        {
            return Err(at(def, "@compute must be a Program class method with self and a typed input; call it as self.method(rows). A nested def is a scoped callback"));
        }
        if def.is_async
            || def.type_params.is_some()
            || !def.decorator_list.is_empty()
            || def.returns.is_some()
        {
            return Err(at(def, "scoped callbacks require an undecorated synchronous function with inferred return type"));
        }
        let parsed =
            ruff_python_parser::parse_expression("lambda row: None").map_err(|e| e.to_string())?;
        let PyExpr::Lambda(mut lambda) = *parsed.into_syntax().body else {
            unreachable!()
        };
        lambda.parameters = Some(def.parameters.clone());
        super::projection::projection_parameter(&lambda)?;
        let (prefix, result) = match def.body.split_last() {
            Some((Stmt::Return(ret), prefix)) => (prefix.to_vec(), ret.value.clone()),
            _ => (def.body.to_vec(), None),
        };
        if let Some(result) = result {
            lambda.body = result;
        }
        self.callbacks.insert(
            label.to_string(),
            Callback {
                lambda,
                prelude: prefix,
                closure: Some(self.scope_names.clone()),
                identity: Some(format!("{}:{}", label, def.start().to_u32())),
            },
        );
        Ok(())
    }
    pub(super) fn callback_value_call(
        &mut self,
        site: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        id: &str,
    ) -> Result<String, String> {
        if call.arguments.args.len() != 1 || !call.arguments.keywords.is_empty() {
            return Err(at(site, "callback invocation requires one row"));
        }
        let callback = self.callback(&call.func)?;
        let source = self.expr(&call.arguments.args[0], None)?;
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
        let mut value = *ruff_python_parser::parse_expression("result.value")
            .map_err(|e| e.to_string())?
            .into_syntax()
            .body;
        let PyExpr::Attribute(attr) = &mut value else {
            unreachable!()
        };
        let PyExpr::Name(name) = attr.value.as_mut() else {
            unreachable!()
        };
        name.id = scope.into();
        self.record_value(&value, id)
    }
    pub(super) fn callback(&self, expression: &PyExpr) -> Result<Callback, String> {
        match expression {
            PyExpr::Lambda(lambda) => Ok(Callback {
                lambda: lambda.clone(),
                prelude: vec![],
                closure: None,
                identity: None,
            }),
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
    pub fn statements(&self) -> Vec<Stmt> {
        let mut statements = self.prelude.clone();
        statements.push(Stmt::Return(ruff_python_ast::StmtReturn {
            node_index: Default::default(),
            range: self.lambda.body.range(),
            value: Some(self.lambda.body.clone()),
        }));
        statements
    }
    pub fn branch(
        parameter: &str,
        statements: Vec<Stmt>,
        closure: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let parsed = ruff_python_parser::parse_expression(&format!("lambda {parameter}: None"))
            .map_err(|e| e.to_string())?;
        let PyExpr::Lambda(lambda) = *parsed.into_syntax().body else {
            unreachable!()
        };
        Ok(Self {
            lambda,
            prelude: statements,
            closure: Some(closure),
            identity: None,
        })
    }
    pub fn returns_only_none(&self) -> bool {
        fn paths(statements: &[Stmt]) -> bool {
            for stmt in statements {
                match stmt {
                    Stmt::Return(ret) => {
                        return ret
                            .value
                            .as_deref()
                            .is_none_or(|v| matches!(v, PyExpr::NoneLiteral(_)))
                    }
                    Stmt::If(branch) => {
                        if !paths(&branch.body)
                            || branch.elif_else_clauses.iter().any(|c| !paths(&c.body))
                        {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
            true
        }
        paths(&self.statements())
    }
}
