//! Literal aggregate descriptors are parsed, never called as Python functions.
use super::*;
use ruff_python_ast::ExprCall;

macro_rules! aggregate_descriptors {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum AggregateDescriptor { $($variant),+ }
        impl AggregateDescriptor {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
            fn parse(name: &str) -> Option<Self> { match name { $($name => Some(Self::$variant),)+ _ => None } }
            fn function(self) -> AggregateFunction { match self { $(Self::$variant => AggregateFunction::$variant),+ } }
        }
    }
}
aggregate_descriptors! {
    Count => "count", Sum => "sum", Avg => "avg", Min => "min",
    Max => "max", First => "first", Last => "last",
}

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
                let descriptor_kind = AggregateDescriptor::parse(function.attr.as_str())
                    .ok_or_else(|| at(site, "unsupported aggregate function"))?;
                let field = match descriptor_kind {
                    AggregateDescriptor::Count => {
                        if !descriptor.arguments.args.is_empty() {
                            return Err(at(site, "agg.count takes no arguments"));
                        }
                        None
                    }
                    AggregateDescriptor::Sum
                    | AggregateDescriptor::Avg
                    | AggregateDescriptor::Min
                    | AggregateDescriptor::Max
                    | AggregateDescriptor::First
                    | AggregateDescriptor::Last => {
                        let [field] = descriptor.arguments.args.as_ref() else {
                            return Err(at(site, "aggregate requires one literal field name"));
                        };
                        Some(FieldPath::from_dotted(&string(field)?)?)
                    }
                };
                let function = descriptor_kind.function();
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
