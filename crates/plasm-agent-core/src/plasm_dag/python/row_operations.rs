//! Closed dispatch vocabulary for deferred row operations, not inner Python.
use super::*;

macro_rules! row_operations {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum RowOperation { $($variant),+ }
        impl RowOperation {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
            pub fn parse(name: &str) -> Option<Self> { match name { $($name => Some(Self::$variant),)+ _ => None } }
        }
    }
}
row_operations! {
    Map => "map", FlatMap => "flat_map", Iterate => "iterate", PageSize => "page_size",
    Aggregate => "aggregate", GroupBy => "group_by", Distinct => "distinct",
    Select => "select", OrderBy => "order_by", Union => "union", Take => "take", Where => "where",
}

impl Lower<'_> {
    pub(super) fn row_operation(
        &mut self,
        operation: RowOperation,
        e: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        receiver: &PyExpr,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        use RowOperation::*;
        // Both construction and execution coverage enumerate this same closed
        // vocabulary. No wildcard arm may silently admit a new operation.
        match operation {
            Map => {
                let body = self.map(e)?;
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
            PageSize => {
                if call.arguments.args.len() != 1
                    || !call.arguments.keywords.is_empty()
                    || !matches!(receiver, PyExpr::Call(_))
                {
                    return Err(at(e, "page_size requires a positive literal bound attached directly to a catalog read call"));
                }
                let size = u32::try_from(integer(&call.arguments.args[0])?)
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| at(e, "page_size requires a positive u32"))?;
                let read = self.expr(receiver, Some(id))?;
                let index = *self.state.labels.get(&read).ok_or("missing read node")?;
                let node = Arc::make_mut(&mut self.state.nodes[index]);
                if !matches!(&node.source, super::super::types::DagNodeSource::Surface { parsed, effect_class: EffectClass::Read, .. } if matches!(parsed.expr, plasm_core::Expr::Query(_)))
                {
                    return Err(at(e, "page_size applies to catalog query/search reads"));
                }
                node.page_size = Some(size as usize);
                Ok(read)
            }
            Iterate | FlatMap | Aggregate | GroupBy | Distinct | Select | OrderBy | Union
            | Take | Where => {
                let source = self.expr(receiver, None)?;
                self.row_transform(operation, e, call, &source, id)
            }
        }
    }
    fn row_transform(
        &mut self,
        operation: RowOperation,
        e: &PyExpr,
        call: &ruff_python_ast::ExprCall,
        source: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        use RowOperation::*;
        let suffix = match operation {
            Map | PageSize => return Err(at(e, "row constructor entered transform dispatch")),
            Iterate => return self.iteration(e, call, source, id),
            FlatMap => return self.fanout(e, call, source, id),
            Aggregate | GroupBy | Distinct => {
                return self.reduction(e, call, operation.name(), source, id)
            }
            Select => {
                if !call.arguments.keywords.is_empty() {
                    return self.project_aliases(e, call, source, id);
                }
                if call.arguments.args.is_empty() {
                    return Err(at(e, "select requires fields"));
                }
                RowSuffix::Project {
                    fields: call
                        .arguments
                        .args
                        .iter()
                        .map(string)
                        .collect::<Result<_, _>>()?,
                }
            }
            OrderBy => {
                if call.arguments.args.len() != 1 || call.arguments.keywords.len() > 1 {
                    return Err(at(
                        e,
                        "order_by requires one field and optional descending=True/False",
                    ));
                }
                let key = string(&call.arguments.args[0])?;
                let descending = match call.arguments.keywords.first() {
                    None => false,
                    Some(keyword)
                        if keyword
                            .arg
                            .as_ref()
                            .is_some_and(|arg| arg.as_str() == "descending") =>
                    {
                        let PyExpr::BooleanLiteral(value) = &keyword.value else {
                            return Err(at(e, "descending requires a literal Boolean"));
                        };
                        value.value
                    }
                    _ => return Err(at(e, "order_by only accepts the descending keyword")),
                };
                let node = super::super::row_suffix::lower_sort_compute(
                    self.es,
                    &self.state,
                    &[],
                    source,
                    id,
                    "",
                    (&key, descending),
                )?;
                return self.insert(node);
            }
            Union | Take | Where => {
                if call.arguments.args.len() != 1 || !call.arguments.keywords.is_empty() {
                    return Err(at(e, "row operation requires one positional argument"));
                }
                match operation {
                    Union => RowSuffix::Union {
                        rhs: self.expr(&call.arguments.args[0], None)?,
                    },
                    Take => {
                        let count = u32::try_from(integer(&call.arguments.args[0])?)
                            .ok()
                            .ok_or_else(|| at(e, "take requires a nonnegative u32"))?;
                        RowSuffix::Limit { count }
                    }
                    Where => return self.filter(e, source, id, &call.arguments.args[0]),
                    Map | PageSize | Iterate | FlatMap | Aggregate | GroupBy | Distinct
                    | Select | OrderBy => unreachable!("matched positional operation"),
                }
            }
        };
        let node = super::super::row_suffix_to_compute(
            self.es,
            &self.state,
            &[],
            &suffix,
            source,
            id,
            "",
        )?;
        self.insert(node)
    }
}
