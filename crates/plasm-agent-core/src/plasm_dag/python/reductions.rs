//! Literal aggregate descriptors are parsed, never called as Python functions.
use super::*;
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn reduction(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        method: &str,
        source: &str,
        id: &str,
    ) -> Result<String, String> {
        let keys = call
            .arguments
            .args
            .iter()
            .map(|arg| FieldPath::from_dotted(&string(arg)?))
            .collect::<Result<Vec<_>, String>>()?;
        let node = if method == "distinct" {
            if !call.arguments.keywords.is_empty() {
                return Err(at(site, "distinct accepts only literal field names"));
            }
            super::super::row_suffix::lower_distinct_compute(
                self.es,
                &self.state,
                &[],
                source,
                id,
                "",
                keys,
            )?
        } else {
            if method == "aggregate" && !keys.is_empty() {
                return Err(at(
                    site,
                    "aggregate accepts only named aggregate descriptors",
                ));
            }
            let mut aggregates = Vec::new();
            for keyword in &call.arguments.keywords {
                let alias = keyword
                    .arg
                    .as_ref()
                    .ok_or_else(|| at(site, "aggregate unpacking is not admitted"))?;
                let PyExpr::Call(descriptor) = &keyword.value else {
                    return Err(at(site, "expected agg.count() or agg.function(\"field\")"));
                };
                let PyExpr::Attribute(function) = &*descriptor.func else {
                    return Err(at(site, "expected an agg descriptor"));
                };
                if name(&function.value) != Some("agg") || !descriptor.arguments.keywords.is_empty()
                {
                    return Err(at(site, "expected a literal agg descriptor"));
                }
                let function = match function.attr.as_str() {
                    "count" => AggregateFunction::Count,
                    "sum" => AggregateFunction::Sum,
                    "avg" => AggregateFunction::Avg,
                    "min" => AggregateFunction::Min,
                    "max" => AggregateFunction::Max,
                    "first" => AggregateFunction::First,
                    "last" => AggregateFunction::Last,
                    _ => return Err(at(site, "unsupported aggregate function")),
                };
                let field = match (function, descriptor.arguments.args.as_ref()) {
                    (AggregateFunction::Count, []) => None,
                    (AggregateFunction::Count, _) => {
                        return Err(at(site, "agg.count takes no arguments"))
                    }
                    (_, [field]) => Some(FieldPath::from_dotted(&string(field)?)?),
                    _ => return Err(at(site, "aggregate requires one literal field name")),
                };
                aggregates.push(AggregateSpec {
                    name: OutputName::new(alias.to_string())?,
                    function,
                    field,
                });
            }
            super::super::row_suffix::lower_reduction_compute(
                self.es,
                &self.state,
                &[],
                source,
                id,
                "",
                (method == "group_by").then_some(keys),
                aggregates,
            )?
        };
        self.insert(node)
    }
}
