//! Static row application. The lambda is never executed by Python.
use super::*;
use plasm_core::{Expr, PlasmInputRef};
use ruff_python_ast::ExprCall;

pub(super) struct RowScope {
    pub parameter: String,
    pub source: String,
}

impl Lower<'_> {
    pub(super) fn scoped_binding<'b>(&self, name: &'b str) -> &'b str {
        if self
            .row_scope
            .as_ref()
            .is_some_and(|scope| scope.parameter == name)
        {
            "_"
        } else {
            name
        }
    }

    pub(super) fn input_ref(&self, binding: &str, path: Vec<String>) -> PlasmInputRef {
        if self.row_scope.is_some() && binding == "_" {
            PlasmInputRef::row_binding("_", path)
        } else {
            PlasmInputRef::node_output(binding, path)
        }
    }

    pub(super) fn emit_catalog(&mut self, id: &str, expr: Expr) -> Result<String, String> {
        let parsed = plasm_core::expr_parser::ParsedExpr::from_expr(expr);
        let nodes = if let Some(scope) = &self.row_scope {
            // The scope overlay gives the row its source's field policy and singleton proof.
            super::super::pipeline::validate_catalog_operands(
                self.es,
                &self.state,
                id,
                &parsed.expr,
            )?;
            super::super::pipeline::lower_catalog_application(
                self.es,
                &self.state,
                id,
                "",
                &scope.source,
                "",
                parsed,
            )?
        } else {
            super::super::pipeline::compile_parsed_nodes(self.es, &self.state, id, "", parsed)?
        };
        for node in nodes {
            self.insert(node)?;
        }
        Ok(id.into())
    }

    pub(super) fn fanout(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        source: &str,
        id: &str,
    ) -> Result<String, String> {
        if self.row_scope.is_some()
            || !call.arguments.keywords.is_empty()
            || call.arguments.args.len() != 1
        {
            return Err(at(
                site,
                "flat_map requires one row lambda; nested fanout is not admitted",
            ));
        }
        let (source, body) = self.formatted_arguments(&call.arguments.args[0], source)?;
        let node = self.row_application(site, &body, &source, id)?;
        self.insert(node)
    }

    fn formatted_arguments(
        &mut self,
        body: &PyExpr,
        source: &str,
    ) -> Result<(String, PyExpr), String> {
        let mut body = body.clone();
        let PyExpr::Lambda(lambda) = &mut body else {
            return Ok((source.into(), body));
        };
        let Some(parameter) = lambda
            .parameters
            .as_ref()
            .filter(|p| p.args.len() == 1)
            .map(|p| p.args[0].parameter.name.to_string())
        else {
            return Ok((source.into(), body));
        };
        let PyExpr::Call(call) = &mut *lambda.body else {
            return Ok((source.into(), body));
        };
        let mut columns = Vec::new();
        let existing = super::super::schema_validate::compute_passthrough_or_fallback_schema(
            self.es,
            &self.state,
            &[],
            source,
            "PythonArgs",
        );
        for keyword in &mut call.arguments.keywords {
            if !matches!(&keyword.value, PyExpr::FString(_)) {
                continue;
            }
            let mut field = self.fresh();
            while existing.fields.iter().any(|f| f.name.as_str() == field) {
                field = self.fresh();
            }
            columns.push(plasm_core::WithColumn {
                name: OutputName::new(field.clone())?,
                expr: super::projection::string_interpolation(
                    &keyword.value,
                    &parameter,
                    &existing,
                )?,
            });
            let replacement = ruff_python_parser::parse_module(&format!("{parameter}.{field}"))
                .map_err(|e| e.to_string())?;
            let [Stmt::Expr(expression)] = replacement.suite().as_slice() else {
                return Err("invalid generated argument field".into());
            };
            keyword.value = *expression.value.clone();
        }
        if columns.is_empty() {
            return Ok((source.into(), body));
        }
        let derived = self.fresh();
        let node = super::super::row_suffix::lower_with_compute(
            self.es,
            &self.state,
            &[],
            source,
            &derived,
            "",
            columns,
        )?;
        self.insert(node)?;
        Ok((derived, body))
    }

    pub(super) fn row_application(
        &self,
        site: &PyExpr,
        body: &PyExpr,
        source: &str,
        id: &str,
    ) -> Result<DagNode, String> {
        let PyExpr::Lambda(lambda) = body else {
            return Err(at(site, "flat_map requires one row lambda"));
        };
        let p = lambda
            .parameters
            .as_ref()
            .ok_or_else(|| at(site, "lambda requires one row parameter"))?;
        if p.args.len() != 1
            || !p.posonlyargs.is_empty()
            || !p.kwonlyargs.is_empty()
            || p.vararg.is_some()
            || p.kwarg.is_some()
            || p.args[0].default.is_some()
        {
            return Err(at(site, "lambda requires exactly one row parameter"));
        }
        let parameter = p.args[0].parameter.name.as_str();
        if matches!(parameter, "self" | "Program" | "compute" | "Value" | "_")
            || parameter.starts_with("__")
            || self.state.contains(parameter)
            || self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(parameter)
                .is_ok()
        {
            return Err(at(
                site,
                "lambda parameter must not shadow a reserved or outer binding",
            ));
        }
        if let PyExpr::Attribute(hop) = &*lambda.body {
            if name(&hop.value) != Some(parameter) {
                return Err(at(
                    site,
                    "relation application must reference its current row",
                ));
            }
            return super::super::binding_continuation::lower_relation_application(
                self.es,
                &self.state,
                id,
                "",
                source,
                hop.attr.as_str(),
            );
        }
        let PyExpr::Call(body) = &*lambda.body else {
            return Err(at(site, "flat_map body must be one catalog call"));
        };
        let PyExpr::Attribute(method) = &*body.func else {
            return Err(at(site, "flat_map body must be one catalog call"));
        };
        let receiver = name(&method.value).ok_or_else(|| {
            at(
                site,
                "flat_map call requires the row or a catalog entity receiver",
            )
        })?;
        if receiver != parameter
            && self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(receiver)
                .is_err()
        {
            return Err(at(
                site,
                "flat_map call requires the row or a catalog entity receiver",
            ));
        }
        if matches!(
            method.attr.as_str(),
            "flat_map" | "map" | "take" | "select" | "where"
        ) {
            return Err(at(site, "flat_map body must be one catalog call"));
        }
        // A type-checking overlay only: no take node or sampled row enters the executable DAG.
        // Limit preserves the source's grain, identity and provenance and gives a row proof.
        let row = super::super::row_suffix_to_compute(
            self.es,
            &self.state,
            &[],
            &RowSuffix::Limit { count: 1 },
            source,
            "_",
            "",
        )?;
        let state = super::super::row_suffix::compile_state_with_nodes(&self.state, &[row]);
        let mut scoped = Lower {
            methods: self.methods,
            es: self.es,
            state,
            serial: self.serial,
            spans: BTreeMap::new(),
            row_scope: Some(RowScope {
                parameter: parameter.into(),
                source: source.into(),
            }),
        };
        let initial = scoped.state.nodes.len();
        scoped.expr(&lambda.body, Some(id))?;
        let nodes = scoped.state.nodes[initial..]
            .iter()
            .map(|n| (**n).clone())
            .collect::<Vec<_>>();
        if nodes.len() != 1
            || !matches!(
                nodes[0].source,
                super::super::types::DagNodeSource::ForEach { .. }
            )
        {
            return Err(at(
                site,
                "flat_map body must lower to exactly one catalog operation",
            ));
        }
        nodes
            .into_iter()
            .next()
            .ok_or_else(|| at(site, "missing row operation"))
    }
}
