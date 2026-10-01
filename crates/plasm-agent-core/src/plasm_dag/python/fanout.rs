//! Static row application. The lambda is never executed by Python.
use super::*;
use plasm_core::{Expr, PlasmInputRef};
use ruff_python_ast::ExprCall;

pub(super) struct RowScope {
    pub parameter: String,
    pub source: String,
}

impl Lower<'_> {
    pub(super) fn scoped_binding<'b>(&'b self, name: &'b str) -> &'b str {
        if self
            .row_scope
            .as_ref()
            .is_some_and(|scope| scope.parameter == name)
        {
            "_"
        } else {
            self.scope_names
                .get(name)
                .map(String::as_str)
                .unwrap_or(name)
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
        _call: &ExprCall,
        _source: &str,
        id: &str,
    ) -> Result<String, String> {
        let body = self.scope(site, true, Some(_source))?;
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
