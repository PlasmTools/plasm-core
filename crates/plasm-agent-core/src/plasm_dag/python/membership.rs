//! Admission for closed membership RHS expressions. They are planned once, not per row.
use super::*;
use ruff_python_ast::visitor::{self, Visitor};

pub(super) fn validate_closed_rhs(expr: &PyExpr, outer_parameter: &str) -> Result<(), String> {
    let mut check = ClosedRhs {
        outer_parameter,
        shadowed: false,
        error: None,
    };
    check.visit_expr(expr);
    check.error.map_or(Ok(()), Err)
}

struct ClosedRhs<'a> {
    outer_parameter: &'a str,
    shadowed: bool,
    error: Option<String>,
}

impl<'a> Visitor<'a> for ClosedRhs<'_> {
    fn visit_expr(&mut self, expr: &'a PyExpr) {
        if self.error.is_some() {
            return;
        }
        match expr {
            PyExpr::Name(name) if !self.shadowed && name.id.as_str() == self.outer_parameter => {
                self.error = Some(at(
                    expr,
                    "membership RHS must be closed; it cannot capture the enclosing row",
                ));
                return;
            }
            PyExpr::Lambda(lambda) => {
                // Defaults are evaluated outside the lambda's local scope. Ordinary
                // lowering subsequently rejects defaults and unsupported signatures.
                if let Some(parameters) = &lambda.parameters {
                    for parameter in &parameters.args {
                        if let Some(default) = &parameter.default {
                            self.visit_expr(default);
                        }
                    }
                }
                let old = self.shadowed;
                self.shadowed |= lambda.parameters.as_ref().is_some_and(|parameters| {
                    parameters
                        .args
                        .iter()
                        .any(|parameter| parameter.parameter.name.as_str() == self.outer_parameter)
                });
                self.visit_expr(&lambda.body);
                self.shadowed = old;
                return;
            }
            PyExpr::Call(call) => {
                let allowed = matches!(&*call.func, PyExpr::Attribute(attr) if matches!(attr.attr.as_str(), "get" | "query" | "select" | "where" | "take" | "order_by" | "union" | "distinct" | "flat_map"));
                if !allowed {
                    self.error = Some(at(
                        expr,
                        "membership RHS admits only closed read and rowset operations",
                    ));
                    return;
                }
            }
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }
}
