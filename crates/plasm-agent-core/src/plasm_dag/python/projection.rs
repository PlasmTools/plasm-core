//! Typed named projection; alias strings are field paths, never embedded source.
use super::*;
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    fn project_scope(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        source: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let parameter = self.fresh_parameter("projection");
        let parsed = ruff_python_parser::parse_expression(&format!("lambda {parameter}: {{}}"))
            .map_err(|e| e.to_string())?;
        let PyExpr::Lambda(mut lambda) = *parsed.into_syntax().body else {
            unreachable!()
        };
        let PyExpr::Dict(dict) = lambda.body.as_mut() else {
            unreachable!()
        };
        let mut fields = BTreeMap::new();
        for argument in &call.arguments.args {
            let field = string(argument)?;
            let expr = ruff_python_parser::parse_expression(&format!("{parameter}.field"))
                .map_err(|e| e.to_string())?;
            let mut expr = *expr.into_syntax().body;
            let PyExpr::Attribute(attr) = &mut expr else {
                unreachable!()
            };
            attr.attr = ruff_python_ast::Identifier::new(field.clone(), site.range());
            if fields.insert(field, expr).is_some() {
                return Err(at(site, "duplicate projection column"));
            }
        }
        for keyword in &call.arguments.keywords {
            let alias = keyword
                .arg
                .as_ref()
                .ok_or("projection unpacking is not admitted")?
                .to_string();
            let expression = if matches!(&keyword.value, PyExpr::Lambda(_))
                || name(&keyword.value).is_some_and(|n| self.callbacks.contains_key(n))
            {
                let callback = self.callback(&keyword.value)?;
                let callable = self.fresh_parameter("projection_callback");
                self.callbacks.insert(callable.clone(), callback);
                *ruff_python_parser::parse_expression(&format!("{callable}({parameter})"))
                    .map_err(|e| e.to_string())?
                    .into_syntax()
                    .body
            } else {
                let field = string(&keyword.value)?;
                let expr = ruff_python_parser::parse_expression(&format!("{parameter}.field"))
                    .map_err(|e| e.to_string())?;
                let mut expr = *expr.into_syntax().body;
                let PyExpr::Attribute(attr) = &mut expr else {
                    unreachable!()
                };
                attr.attr = ruff_python_ast::Identifier::new(field, site.range());
                expr
            };
            if fields.insert(alias, expression).is_some() {
                return Err(at(site, "duplicate projection column"));
            }
        }
        for (alias, value) in fields {
            let key = ruff_python_parser::parse_expression(
                &serde_json::to_string(&alias).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            dict.items.push(ruff_python_ast::DictItem {
                key: Some(*key.into_syntax().body),
                value,
            });
        }
        let body = self.scoped_body(
            site,
            source,
            &lambda,
            std::num::NonZeroU32::new(65_536).unwrap(),
            super::body::ScopeMode::Record,
        )?;
        if !matches!(
            body.effect_class(),
            EffectClass::Read | EffectClass::ArtifactRead
        ) {
            return Err(at(site, "projection expressions cannot introduce effects"));
        }
        let schema = crate::map_body_schema::output_schema(self.es, &body)?;
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: super::super::binding_contract(&self.state, source)
                .is_some_and(|c| c.row_cardinality.permits_scalar_field_extract()),
            page_size: None,
            source: super::super::types::DagNodeSource::MapBody {
                body: Box::new(body),
                schema,
            },
        })
    }

    pub(super) fn projection_field(
        &self,
        source: &str,
        field: &str,
    ) -> Result<PlasmDataValue, PythonLoweringError> {
        let schema = super::text::inferred_schema(self.es, &self.state, source, 0)?;
        if !schema.fields.iter().any(|f| f.name.as_str() == field) {
            return Err(format!("unknown projected field {field}").into());
        }
        Ok(PlasmDataValue::BindingSymbol {
            binding: "_".into(),
            path: vec![field.into()],
        })
    }

    pub(super) fn project_aliases(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        source: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        if call.arguments.keywords.iter().any(|k| {
            matches!(k.value, PyExpr::Lambda(_))
                || name(&k.value).is_some_and(|n| self.callbacks.contains_key(n))
        }) {
            return self.project_scope(site, call, source, id);
        }
        let mut fields = BTreeMap::new();
        for argument in &call.arguments.args {
            let name = string(argument)?;
            let value = self.projection_field(source, &name)?;
            if fields.insert(name, value).is_some() {
                return Err(at(site, "duplicate projection column"));
            }
        }
        for keyword in &call.arguments.keywords {
            let alias = keyword
                .arg
                .as_ref()
                .ok_or("projection unpacking is not admitted")?
                .to_string();
            let value = self.projection_field(source, &string(&keyword.value)?)?;
            if fields.insert(alias, value).is_some() {
                return Err(at(site, "duplicate projection column"));
            }
        }
        let previous = self.frame.row.replace(source.into());
        let result = self.emit_value(PlasmDataValue::Object { fields }, vec![], id);
        self.frame.row = previous;
        result
    }
}

pub(super) fn projection_parameter(
    lambda: &ruff_python_ast::ExprLambda,
) -> Result<&str, PythonLoweringError> {
    let p = lambda
        .parameters
        .as_ref()
        .ok_or("projection requires one row parameter")?;
    let binding = monty_analysis::bind_one_positional(&callable_signature(lambda)?)?;
    if binding.variadic {
        return Err("a variadic tuple cannot carry a direct DAG row receiver".into());
    }
    let row = p
        .posonlyargs
        .iter()
        .chain(&p.args)
        .find(|p| p.parameter.name.as_str() == binding.parameter)
        .ok_or("upstream row binding has no source parameter")?
        .parameter
        .name
        .as_str();
    if row == "self" || row.starts_with("__") {
        return Err("reserved projection parameter".into());
    }
    Ok(row)
}

pub(super) fn callable_signature(
    lambda: &ruff_python_ast::ExprLambda,
) -> Result<String, PythonLoweringError> {
    let parameters = lambda
        .parameters
        .as_ref()
        .ok_or("missing callback parameters")?;
    let parsed = ruff_python_parser::parse_module("def callback(row):\n    pass\n")
        .map_err(|e| e.to_string())?;
    let mut statement = parsed.into_syntax().body.remove(0);
    let Stmt::FunctionDef(def) = &mut statement else {
        unreachable!()
    };
    def.parameters = parameters.clone().into();
    Ok(monty::statement_source(&statement))
}
