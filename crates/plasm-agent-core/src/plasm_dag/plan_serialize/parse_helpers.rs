//! Surface parse helpers (aggregates, sort, plan-value literals).

use super::super::prelude::*;
use super::super::types::CompileState;
use super::template_uses::dedupe_inputs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanValueExpressionError {
    #[error("tagged heredoc literal syntax is invalid: {0}")]
    HeredocSyntax(#[source] plasm_core::expr_parser::SurfaceSyntaxError),
    #[error(transparent)]
    ProgramString(#[from] plasm_core::program_string_template::ProgramStringError),
    #[error(transparent)]
    DataExpression(Box<plasm_core::expr_parser::data::DataExpressionError>),
    #[error(transparent)]
    DataValue(#[from] plasm_core::PlasmDataValueError),
    #[error("plain template binding `{binding}` is not in scope")]
    TemplateBindingOutOfScope { binding: String },
    #[error("row reference `_` is outside a row scope")]
    RowReferenceOutOfScope,
    #[error("data binding `{binding}` is unknown")]
    UnknownBinding { binding: String },
}

impl From<plasm_core::expr_parser::data::DataExpressionError> for PlanValueExpressionError {
    fn from(error: plasm_core::expr_parser::data::DataExpressionError) -> Self {
        Self::DataExpression(Box::new(error))
    }
}

#[derive(Debug, Error)]
pub enum GroupByError {
    #[error("group_by argument delimiters are invalid: {0}")]
    Delimiter(#[source] plasm_core::expr_parser::SurfaceSyntaxError),
    #[error("group_by requires at least one key field")]
    KeysRequired,
}

#[derive(Debug, Error)]
pub enum AggregateSpecError {
    #[error(transparent)]
    Atom(#[from] plasm_core::plasm_monad::PlanAtomError),
    #[error("right-hand side `{expression}` in `{spec}` must be `count` or `func(field)` (e.g. `sum(amount)`)")]
    InvalidExpression { expression: String, spec: String },
    #[error("aggregate call `{expression}` must end with `)`")]
    ClosingDelimiterMissing { expression: String },
    #[error("unknown aggregate function `{function}`")]
    UnknownFunction { function: String },
    #[error("aggregate spec `{spec}` must use an explicit output name, e.g. `total=sum(amount)` or `n=count`")]
    OutputNameRequired { spec: String },
    #[error("aggregate spec `{spec}` must be `output=count` or `output=sum(field)`; bare `count` and `aggregate(count)` are accepted as shorthand for `count=count`")]
    InvalidSpec { spec: String },
    #[error("{0}")]
    Delimiter(#[source] plasm_core::expr_parser::SurfaceSyntaxError),
}

#[derive(Debug, Error)]
pub enum SortSpecError {
    #[error("sort requires a field")]
    FieldRequired,
    #[error("sort field must not be empty")]
    EmptyField,
    #[error("sort direction must not be empty when a comma is present")]
    EmptyDirection,
    #[error("unknown sort direction `{direction}`; use asc/ascending or desc/descending")]
    UnknownDirection { direction: String },
    #[error("sort accepts at most a field and direction")]
    TooManyArguments,
    #[error("sort argument delimiters are invalid: {0}")]
    Delimiter(#[source] plasm_core::expr_parser::SurfaceSyntaxError),
}

pub(in crate::plasm_dag) fn parse_field_list(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    fields: &str,
) -> Result<Vec<String>, crate::plasm_render_compile::RenderFieldListError> {
    parse_field_list_with_tokens(session, symbol_map_cross_cache, qe, fields)
        .map(|pairs| pairs.into_iter().map(|(_, wire)| wire).collect())
}

/// Parses one comma-separated aggregate specification after `.aggregate(...)` / `group_by` tail.
///
/// Canonical form: `output=count` or `output=sum(field)` (also `avg`/`min`/`max`).
///
/// **Shadow (repair-only, not taught):** bare `count` and `aggregate(count)` canonicalize to
/// `count=count` with synthetic output name `count`.
pub(in crate::plasm_dag) fn parse_one_aggregate_spec(
    raw: &str,
) -> Result<crate::plasm_plan::AggregateSpec, AggregateSpecError> {
    let raw = raw.trim();
    if let Some((name, rhs)) = raw.split_once('=') {
        let name = OutputName::new(name.trim().to_string())?;
        let rhs = rhs.trim();
        if rhs == "count" {
            return Ok(crate::plasm_plan::AggregateSpec {
                name,
                function: crate::plasm_plan::AggregateFunction::Count,
                field: None,
            });
        }
        let open = rhs
            .find('(')
            .ok_or_else(|| AggregateSpecError::InvalidExpression {
                expression: rhs.to_owned(),
                spec: raw.to_owned(),
            })?;
        let func = &rhs[..open];
        let field = rhs[open + 1..].strip_suffix(')').ok_or_else(|| {
            AggregateSpecError::ClosingDelimiterMissing {
                expression: rhs.to_owned(),
            }
        })?;
        let function = match func {
            "sum" => AggregateFunction::Sum,
            "avg" => AggregateFunction::Avg,
            "min" => AggregateFunction::Min,
            "max" => AggregateFunction::Max,
            "first" => AggregateFunction::First,
            "last" => AggregateFunction::Last,
            other => {
                return Err(AggregateSpecError::UnknownFunction {
                    function: other.to_owned(),
                })
            }
        };
        return Ok(crate::plasm_plan::AggregateSpec {
            name,
            function,
            field: Some(FieldPath::from_dotted(field.trim())?),
        });
    }

    // Shadow count-only forms → canonical `count=count`.
    if raw.eq_ignore_ascii_case("count") {
        return Ok(crate::plasm_plan::AggregateSpec {
            name: OutputName::new("count".to_string())?,
            function: crate::plasm_plan::AggregateFunction::Count,
            field: None,
        });
    }
    if let Some(inner) = raw
        .strip_prefix("aggregate(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let inner = inner.trim();
        if inner.eq_ignore_ascii_case("count") {
            return Ok(crate::plasm_plan::AggregateSpec {
                name: OutputName::new("count".to_string())?,
                function: crate::plasm_plan::AggregateFunction::Count,
                field: None,
            });
        }
        if inner.contains('(') {
            return Err(AggregateSpecError::OutputNameRequired {
                spec: raw.to_owned(),
            });
        }
    }

    if raw.contains('(') {
        return Err(AggregateSpecError::OutputNameRequired {
            spec: raw.to_owned(),
        });
    }

    Err(AggregateSpecError::InvalidSpec {
        spec: raw.to_owned(),
    })
}

pub(in crate::plasm_dag) fn parse_aggregates(
    args: &str,
) -> Result<Vec<crate::plasm_plan::AggregateSpec>, AggregateSpecError> {
    split_top_level(args, ',')
        .map_err(AggregateSpecError::Delimiter)?
        .into_iter()
        .map(parse_one_aggregate_spec)
        .collect()
}

pub(in crate::plasm_dag) fn parse_sort_direction_token(
    direction: &str,
) -> Result<bool, SortSpecError> {
    let d = direction.trim();
    if d.is_empty() {
        return Err(SortSpecError::EmptyDirection);
    }
    match d.to_ascii_lowercase().as_str() {
        "desc" | "descending" => Ok(true),
        "asc" | "ascending" => Ok(false),
        other => Err(SortSpecError::UnknownDirection {
            direction: other.to_string(),
        }),
    }
}

/// Parse `.sort(...)` args: `field`, `field, desc`, or whitespace sugar `field desc`.
pub(in crate::plasm_dag) fn parse_sort_field_and_direction(
    args: &str,
) -> Result<(String, bool), SortSpecError> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err(SortSpecError::FieldRequired);
    }
    let parts = split_top_level(trimmed, ',').map_err(SortSpecError::Delimiter)?;
    match parts.len() {
        0 => Err(SortSpecError::FieldRequired),
        1 => {
            let single = parts[0].trim();
            if single.is_empty() {
                return Err(SortSpecError::EmptyField);
            }
            if let Some((field, dir)) = single.rsplit_once(|c: char| c.is_ascii_whitespace()) {
                let field = field.trim();
                let dir = dir.trim();
                if !field.is_empty() && !dir.is_empty() {
                    if let Ok(descending) = parse_sort_direction_token(dir) {
                        return Ok((field.to_string(), descending));
                    }
                }
            }
            Ok((single.to_string(), false))
        }
        2 => {
            let key = parts[0].trim();
            if key.is_empty() {
                return Err(SortSpecError::EmptyField);
            }
            let descending = parse_sort_direction_token(parts[1].trim())?;
            Ok((key.to_string(), descending))
        }
        _ => Err(SortSpecError::TooManyArguments),
    }
}

pub(in crate::plasm_dag) fn parse_plan_value_expr(
    raw: &str,
    state: &CompileState<'_>,
    row_binding: Option<&str>,
) -> Result<(PlanValue, Vec<crate::plasm_plan::PlanDataInput>), PlanValueExpressionError> {
    let raw = raw.trim();
    if raw.starts_with("<<") {
        let body = plasm_core::expr_parser::parse_tagged_heredoc_literal(raw)
            .map_err(PlanValueExpressionError::HeredocSyntax)?;
        if plasm_core::contains_minijinja_markers(&body) {
            let compiled = plasm_core::CompiledProgramString::compile(body)?;
            let mut inputs = Vec::new();
            let mut input_bindings = Vec::new();
            for root in compiled.roots() {
                if plasm_core::is_minijinja_template_builtin(root) {
                    continue;
                }
                if !state.contains(root) {
                    return Err(PlanValueExpressionError::TemplateBindingOutOfScope {
                        binding: root.clone(),
                    });
                }
                inputs.push(crate::plasm_plan::PlanDataInput {
                    node: root.clone(),
                    alias: root.clone(),
                    cardinality: crate::plasm_plan::InputCardinality::Auto,
                });
                input_bindings.push(plasm_core::PlanInputBinding {
                    from: root.clone(),
                    to: root.clone(),
                });
            }
            return Ok((
                PlanValue::Template {
                    template: compiled,
                    input_bindings,
                },
                dedupe_inputs(inputs),
            ));
        }
        return Ok((
            PlanValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::string(body),
            },
            Vec::new(),
        ));
    }
    let expression = plasm_core::expr_parser::data::parse_data_expression(raw)?;
    lower_data_expression(expression, state, row_binding)
}

fn lower_data_expression(
    expression: plasm_core::expr_parser::data::DataExpr,
    state: &CompileState<'_>,
    row_binding: Option<&str>,
) -> Result<(PlanValue, Vec<crate::plasm_plan::PlanDataInput>), PlanValueExpressionError> {
    use plasm_core::expr_parser::data::DataExpr;
    match expression {
        DataExpr::Literal(value) => Ok((PlanValue::Literal { value }, Vec::new())),
        DataExpr::Object(object) => {
            let mut fields = BTreeMap::new();
            let mut inputs = Vec::new();
            for (key, expression) in object {
                let (value, uses) = lower_data_expression(expression, state, row_binding)?;
                fields.insert(key, value);
                inputs.extend(uses);
            }
            Ok((PlanValue::Object { fields }, dedupe_inputs(inputs)))
        }
        DataExpr::Array(array) => {
            let mut items = Vec::new();
            let mut inputs = Vec::new();
            for expression in array {
                let (value, uses) = lower_data_expression(expression, state, row_binding)?;
                items.push(value);
                inputs.extend(uses);
            }
            Ok((PlanValue::Array { items }, dedupe_inputs(inputs)))
        }
        DataExpr::Reference { root, path } if root == "_" => {
            let binding = row_binding.ok_or(PlanValueExpressionError::RowReferenceOutOfScope)?;
            Ok((
                PlanValue::BindingSymbol {
                    binding: binding.into(),
                    path,
                },
                Vec::new(),
            ))
        }
        DataExpr::Reference { root, path } => {
            let dep = state
                .get(&root)
                .ok_or_else(|| PlanValueExpressionError::UnknownBinding {
                    binding: root.clone(),
                })?;
            let cardinality = if !path.is_empty() && dep.singleton {
                crate::plasm_plan::InputCardinality::Auto
            } else {
                crate::plasm_plan::InputCardinality::Singleton
            };
            Ok((
                PlanValue::NodeSymbol {
                    node: root.clone(),
                    alias: root.clone(),
                    path,
                },
                vec![crate::plasm_plan::PlanDataInput {
                    node: root.clone(),
                    alias: root,
                    cardinality,
                }],
            ))
        }
    }
}

/// Split `group_by` args into key field names (no `=`) and trailing aggregate tail.
pub(in crate::plasm_dag) fn parse_group_by_key_and_aggregate_tail(
    args: &str,
) -> Result<(Vec<String>, String), GroupByError> {
    let parts = split_top_level(args, ',').map_err(GroupByError::Delimiter)?;
    let mut keys = Vec::new();
    let mut agg_start = parts.len();
    for (i, part) in parts.iter().enumerate() {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        if t.contains('=') {
            agg_start = i;
            break;
        }
        keys.push(t.to_string());
    }
    if keys.is_empty() {
        return Err(GroupByError::KeysRequired);
    }
    let agg_tail = if agg_start < parts.len() {
        parts[agg_start..].join(",")
    } else {
        String::new()
    };
    Ok((keys, agg_tail))
}

#[cfg(test)]
mod sort_error_tests {
    use super::*;

    #[test]
    fn sort_parse_errors_are_semantic_and_keep_bad_direction() {
        assert!(matches!(
            parse_sort_field_and_direction(""),
            Err(SortSpecError::FieldRequired)
        ));
        assert!(matches!(
            parse_sort_field_and_direction("name, sideways"),
            Err(SortSpecError::UnknownDirection { direction }) if direction == "sideways"
        ));
        assert_eq!(
            parse_sort_field_and_direction("name, descending").unwrap(),
            ("name".to_string(), true)
        );
    }
}
