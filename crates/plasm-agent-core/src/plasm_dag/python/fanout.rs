//! Static row application. The lambda is never executed by Python.
use super::*;
use plasm_core::{Expr, PlasmInputRef};
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn scoped_binding<'b>(&'b self, name: &'b str) -> &'b str {
        self.frame
            .names
            .get(name)
            .map(String::as_str)
            .unwrap_or(name)
    }

    pub(super) fn input_ref(&self, binding: &str, path: Vec<String>) -> PlasmInputRef {
        PlasmInputRef::node_output(binding, path)
    }

    pub(super) fn emit_catalog(
        &mut self,
        id: &str,
        expr: Expr,
    ) -> Result<String, PythonLoweringError> {
        let parsed = plasm_core::expr_parser::ParsedExpr::from_expr(expr);
        let nodes =
            super::super::pipeline::compile_parsed_nodes(self.es, &self.state, id, "", parsed)?;
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
    ) -> Result<String, PythonLoweringError> {
        let body = self.scope(site, super::body::ScopeMode::Rows, Some(_source))?;
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
