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
    ) -> Result<String, String> {
        use ruff_python_ast::visitor::transformer::{self, Transformer};
        struct Rename<'a> {
            from: &'a str,
            to: &'a str,
            shadows: std::cell::Cell<bool>,
        }
        impl Transformer for Rename<'_> {
            fn visit_expr(&self, expr: &mut PyExpr) {
                if let PyExpr::Name(name) = expr {
                    if name.id.as_str() == self.from {
                        if name.ctx != ruff_python_ast::ExprContext::Load {
                            self.shadows.set(true);
                        }
                        name.id = self.to.into();
                    }
                }
                if let PyExpr::Lambda(lambda) = expr {
                    if projection_parameter(lambda).ok() == Some(self.from) {
                        self.shadows.set(true);
                    }
                }
                transformer::walk_expr(self, expr);
            }
        }
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
            let expression = if let PyExpr::Lambda(value) = &keyword.value {
                let row = projection_parameter(value)?;
                if self.state.contains(row) {
                    return Err(at(
                        site,
                        "projection parameter must not shadow an outer binding",
                    ));
                }
                let mut expression = *value.body.clone();
                let rename = Rename {
                    from: row,
                    to: &parameter,
                    shadows: std::cell::Cell::new(false),
                };
                rename.visit_expr(&mut expression);
                if rename.shadows.get() {
                    return Err(at(
                        site,
                        "projection parameter must not be shadowed in a nested scope",
                    ));
                }
                expression
            } else if name(&keyword.value).is_some_and(|n| self.callbacks.contains_key(n)) {
                let mut expr =
                    *ruff_python_parser::parse_expression(&format!("callback({parameter})"))
                        .map_err(|e| e.to_string())?
                        .into_syntax()
                        .body;
                let PyExpr::Call(call) = &mut expr else {
                    unreachable!()
                };
                *call.func = keyword.value.clone();
                expr
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
    ) -> Result<PlasmDataValue, String> {
        let schema = super::text::inferred_schema(self.es, &self.state, source, 0)?;
        if !schema.fields.iter().any(|f| f.name.as_str() == field) {
            return Err(format!("unknown projected field {field}"));
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
    ) -> Result<String, String> {
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
        let previous = self.scope_row.replace(source.into());
        let result = self.emit_value(PlasmDataValue::Object { fields }, vec![], id);
        self.scope_row = previous;
        result
    }
}

pub(super) fn projection_parameter(lambda: &ruff_python_ast::ExprLambda) -> Result<&str, String> {
    let p = lambda
        .parameters
        .as_ref()
        .ok_or("projection requires one row parameter")?;
    if p.args.len() != 1
        || !p.posonlyargs.is_empty()
        || !p.kwonlyargs.is_empty()
        || p.vararg.is_some()
        || p.kwarg.is_some()
        || p.args[0].default.is_some()
        || p.args[0].parameter.annotation.is_some()
    {
        return Err("projection requires one unannotated row parameter".into());
    }
    let row = p.args[0].parameter.name.as_str();
    if row == "self" || row.starts_with("__") {
        return Err("reserved projection parameter".into());
    }
    Ok(row)
}
