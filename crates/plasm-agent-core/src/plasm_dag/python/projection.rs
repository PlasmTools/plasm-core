//! Typed named projection; alias strings are field paths, never embedded source.
use super::*;
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn project_aliases(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        source: &str,
        id: &str,
    ) -> Result<String, String> {
        let mut fields = call
            .arguments
            .args
            .iter()
            .map(string)
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = std::collections::BTreeSet::new();
        for field in &fields {
            if !seen.insert(field.clone()) {
                return Err(at(site, "duplicate projection column"));
            }
        }
        let mut columns = Vec::new();
        for keyword in &call.arguments.keywords {
            let alias = keyword
                .arg
                .as_ref()
                .ok_or_else(|| at(site, "projection unpacking is not admitted"))?;
            if !seen.insert(alias.to_string()) {
                return Err(at(site, "duplicate projection column"));
            }
            fields.push(alias.to_string());
            columns.push(plasm_core::WithColumn {
                name: OutputName::new(alias.to_string())?,
                expr: match &keyword.value {
                    PyExpr::Lambda(lambda) => {
                        let row = projection_parameter(lambda)?;
                        projection_expression(&lambda.body, row, 0)?
                    }
                    value => plasm_core::WithExpr::Field(FieldPath::from_dotted(&string(value)?)?),
                },
            });
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
        let node = super::super::row_suffix_to_compute(
            self.es,
            &self.state,
            &[],
            &RowSuffix::Project { fields },
            &derived,
            id,
            "",
        )?;
        self.insert(node)
    }
}

fn projection_parameter(lambda: &ruff_python_ast::ExprLambda) -> Result<&str, String> {
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

pub(super) fn projection_expression(
    e: &PyExpr,
    row: &str,
    depth: usize,
) -> Result<plasm_core::WithExpr, String> {
    use plasm_core::{ArithOp, WithExpr as W, WithLiteral as L};
    use ruff_python_ast::{CmpOp, Operator};
    if depth >= 64 {
        return Err("projection expression depth exceeds 64".into());
    }
    let sub = |e: &PyExpr| projection_expression(e, row, depth + 1).map(Box::new);
    Ok(match e {
        PyExpr::Attribute(a) if name(&a.value) == Some(row) => {
            W::Field(FieldPath::from_dotted(a.attr.as_str())?)
        }
        PyExpr::BinOp(b) => W::Arith {
            op: match b.op {
                Operator::Add => ArithOp::Add,
                Operator::Sub => ArithOp::Sub,
                Operator::Mult => ArithOp::Mul,
                Operator::Div => ArithOp::Div,
                _ => return Err(at(e, "unsupported projection arithmetic")),
            },
            lhs: sub(&b.left)?,
            rhs: sub(&b.right)?,
        },
        PyExpr::Call(c)
            if name(&c.func) == Some("len")
                && c.arguments.args.len() == 1
                && c.arguments.keywords.is_empty() =>
        {
            let W::Field(field) = projection_expression(&c.arguments.args[0], row, depth + 1)?
            else {
                return Err(at(e, "len requires a row field"));
            };
            W::Len { field }
        }
        PyExpr::If(c) => {
            let PyExpr::Compare(test) = &*c.test else {
                return Err(at(e, "conditional projection requires one comparison"));
            };
            if test.ops.len() != 1 || test.comparators().len() != 1 {
                return Err(at(e, "conditional projection requires one comparison"));
            }
            W::When {
                lhs: sub(&test.operands[0])?,
                op: match test.ops[0] {
                    CmpOp::Eq => PlanPredicateOp::Eq,
                    CmpOp::NotEq => PlanPredicateOp::Ne,
                    CmpOp::Lt => PlanPredicateOp::Lt,
                    CmpOp::LtE => PlanPredicateOp::Lte,
                    CmpOp::Gt => PlanPredicateOp::Gt,
                    CmpOp::GtE => PlanPredicateOp::Gte,
                    _ => return Err(at(e, "unsupported conditional comparison")),
                },
                rhs: sub(&test.comparators()[0])?,
                then: sub(&c.body)?,
                else_: sub(&c.orelse)?,
            }
        }
        _ => W::Literal(match literal(e)? {
            plasm_core::Value::Null => L::Null,
            plasm_core::Value::Bool(v) => L::Bool(v),
            plasm_core::Value::Integer(v) => L::Integer(v),
            plasm_core::Value::Float(v) => L::Number(v.to_string()),
            plasm_core::Value::String(v) => L::String(v),
            _ => return Err(at(e, "projection requires scalar values")),
        }),
    })
}

/// Format string fields as a typed row computation before the reviewed write.
pub(super) fn string_interpolation(
    e: &PyExpr,
    row: &str,
    schema: &SyntheticResultSchema,
) -> Result<plasm_core::WithExpr, String> {
    use plasm_core::{ArithOp, WithExpr as W, WithLiteral as L};
    let PyExpr::FString(text) = e else {
        return Err(at(e, "expected f-string"));
    };
    let mut out = W::Literal(L::String(String::new()));
    for element in text.value.elements() {
        let part = match element {
            ruff_python_ast::InterpolatedStringElement::Literal(value) => {
                W::Literal(L::String(value.value.to_string()))
            }
            ruff_python_ast::InterpolatedStringElement::Interpolation(value) => {
                if value.format_spec.is_some()
                    || value.conversion != ruff_python_ast::ConversionFlag::None
                    || value.debug_text.is_some()
                {
                    return Err(at(
                        e,
                        "formatted write arguments require explicit @compute formatting",
                    ));
                }
                let part = projection_expression(&value.expression, row, 0)?;
                match &part {
                    W::Literal(L::String(_)) => {}
                    W::Field(path) if schema.fields.iter().any(|f| f.name.as_str() == path.dotted()
                        && f.value_kind == SyntheticValueKind::String
                        && f.value_type.as_ref().is_some_and(|t| !t.nullable)) => {}
                    _ => return Err(at(e, "inline write interpolation requires non-null string fields; use @compute for other Python formatting")),
                }
                part
            }
        };
        out = W::Arith {
            op: ArithOp::Add,
            lhs: Box::new(out),
            rhs: Box::new(part),
        };
    }
    Ok(out)
}
