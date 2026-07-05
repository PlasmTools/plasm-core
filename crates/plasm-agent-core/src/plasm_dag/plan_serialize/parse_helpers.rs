//! Surface parse helpers (aggregates, sort, plan-value literals).

use super::super::prelude::*;
use super::super::types::CompileState;
use super::template_uses::dedupe_inputs;

pub(in crate::plasm_dag) fn parse_field_list(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    fields: &str,
) -> Result<Vec<String>, String> {
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
) -> Result<crate::plasm_plan::AggregateSpec, String> {
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
        let open = rhs.find('(').ok_or_else(|| {
            format!(
                "right-hand side `{rhs}` in `{raw}` must be `count` or `func(field)` (e.g. `sum(amount)`)"
            )
        })?;
        let func = &rhs[..open];
        let field = rhs[open + 1..]
            .strip_suffix(')')
            .ok_or_else(|| format!("aggregate call `{rhs}` must end with `)`"))?;
        let function = match func {
            "sum" => AggregateFunction::Sum,
            "avg" => AggregateFunction::Avg,
            "min" => AggregateFunction::Min,
            "max" => AggregateFunction::Max,
            "first" => AggregateFunction::First,
            "last" => AggregateFunction::Last,
            other => return Err(format!("unknown aggregate function `{other}`")),
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
            return Err(format!(
                "aggregate spec `{raw}` must name the output explicitly; use e.g. `total={inner}` (not `{raw}` without `output=`)"
            ));
        }
    }

    if raw.contains('(') {
        return Err(format!(
            "aggregate spec `{raw}` must use an explicit output name, e.g. `total=sum(amount)` or `n=count`"
        ));
    }

    Err(format!(
        "aggregate spec `{raw}` must be `output=count` or `output=sum(field)`…; bare `count` and `aggregate(count)` are accepted as shorthand for `count=count`"
    ))
}

pub(in crate::plasm_dag) fn parse_aggregates(
    args: &str,
) -> Result<Vec<crate::plasm_plan::AggregateSpec>, String> {
    split_top_level(args, ',')?
        .into_iter()
        .map(parse_one_aggregate_spec)
        .collect()
}

pub(in crate::plasm_dag) fn parse_sort_direction_token(direction: &str) -> Result<bool, String> {
    let d = direction.trim();
    if d.is_empty() {
        return Err("sort(...) direction must not be empty when a comma is present".to_string());
    }
    match d.to_ascii_lowercase().as_str() {
        "desc" | "descending" => Ok(true),
        "asc" | "ascending" => Ok(false),
        other => Err(format!(
            "sort(...) unknown direction `{other}`; use `desc` / `descending` for descending, omit the direction or use `asc` / `ascending` for ascending"
        )),
    }
}

/// Parse `.sort(...)` args: `field`, `field, desc`, or whitespace sugar `field desc`.
pub(in crate::plasm_dag) fn parse_sort_field_and_direction(
    args: &str,
) -> Result<(String, bool), String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err("sort(...) requires a field".to_string());
    }
    let parts = split_top_level(trimmed, ',')?;
    match parts.len() {
        0 => Err("sort(...) requires a field".to_string()),
        1 => {
            let single = parts[0].trim();
            if single.is_empty() {
                return Err("sort(...) requires a non-empty field".to_string());
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
                return Err("sort(...) requires a non-empty field".to_string());
            }
            let descending = parse_sort_direction_token(parts[1].trim())?;
            Ok((key.to_string(), descending))
        }
        _ => {
            Err("sort(...) expects at most `.sort(field)` or `.sort(field, direction)`".to_string())
        }
    }
}

pub(in crate::plasm_dag) fn parse_plan_value_expr(
    raw: &str,
    state: &CompileState<'_>,
    row_binding: Option<&str>,
) -> Result<(PlanValue, Vec<serde_json::Value>), String> {
    let raw = raw.trim();
    if raw.starts_with('{') && raw.ends_with('}') {
        let mut inputs = Vec::new();
        let mut fields = BTreeMap::new();
        for part in split_top_level(&raw[1..raw.len() - 1], ',')? {
            let (k, v) = part
                .split_once(':')
                .ok_or_else(|| format!("object field `{part}` must be key: value"))?;
            let (value, child_inputs) = parse_plan_value_expr(v, state, row_binding)?;
            inputs.extend(child_inputs);
            fields.insert(k.trim().to_string(), value);
        }
        return Ok((PlanValue::Object { fields }, dedupe_inputs(inputs)));
    }
    if raw.starts_with('[') && raw.ends_with(']') {
        let mut inputs = Vec::new();
        let mut items = Vec::new();
        for part in split_top_level(&raw[1..raw.len() - 1], ',')? {
            let (value, child_inputs) = parse_plan_value_expr(part, state, row_binding)?;
            inputs.extend(child_inputs);
            items.push(value);
        }
        return Ok((PlanValue::Array { items }, dedupe_inputs(inputs)));
    }
    if let Some(path) = raw.strip_prefix("_.") {
        return Ok((
            PlanValue::BindingSymbol {
                binding: row_binding.unwrap_or("_").to_string(),
                path: path.split('.').map(str::to_string).collect(),
            },
            Vec::new(),
        ));
    }
    if let Some((node, path)) = raw.split_once('.') {
        if let Some(dep) = state.get(node) {
            return Ok((
                PlanValue::NodeSymbol {
                    node: node.to_string(),
                    alias: node.to_string(),
                    path: path.split('.').map(str::to_string).collect(),
                },
                vec![json!({
                    "node": node,
                    "alias": node,
                    "cardinality": if dep.singleton { "auto" } else { "singleton" }
                })],
            ));
        }
    }
    if state.contains(raw) {
        let path = if row_binding.is_some() {
            vec!["content".to_string()]
        } else {
            Vec::new()
        };
        return Ok((
            PlanValue::NodeSymbol {
                node: raw.to_string(),
                alias: raw.to_string(),
                path,
            },
            vec![serde_json::json!({
                "node": raw,
                "alias": raw,
                "cardinality": "singleton"
            })],
        ));
    }
    if raw.starts_with("<<") {
        let body = plasm_core::expr_parser::parse_tagged_heredoc_literal(raw)
            .map_err(|e| format!("heredoc literal: {e}"))?;
        return Ok((PlanValue::Literal { value: json!(body) }, Vec::new()));
    }
    let value = parse_literal(raw)?;
    Ok((PlanValue::Literal { value }, Vec::new()))
}

pub(in crate::plasm_dag) fn parse_literal(raw: &str) -> Result<serde_json::Value, String> {
    if raw.starts_with('"') || raw == "null" || raw == "true" || raw == "false" {
        return serde_json::from_str(raw).map_err(|e| format!("literal `{raw}`: {e}"));
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Ok(json!(n));
    }
    if let Ok(n) = raw.parse::<f64>() {
        return Ok(json!(n));
    }
    Ok(json!(raw))
}

/// Split `group_by` args into key field names (no `=`) and trailing aggregate tail.
pub(in crate::plasm_dag) fn parse_group_by_key_and_aggregate_tail(
    args: &str,
) -> Result<(Vec<String>, String), String> {
    let parts = split_top_level(args, ',')?;
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
        return Err("group_by(...) requires at least one key field".into());
    }
    let agg_tail = if agg_start < parts.len() {
        parts[agg_start..].join(",")
    } else {
        String::new()
    };
    Ok((keys, agg_tail))
}

pub(in crate::plasm_dag) fn parse_dedupe_key_paths(
    session: &ExecuteSession,
    cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    keys: &str,
) -> Result<Vec<FieldPath>, String> {
    let trimmed = keys.trim();
    if trimmed.is_empty() {
        return Err("dedupe(...) requires at least one key field".into());
    }
    parse_field_list(session, cross_cache, qe, trimmed)?
        .into_iter()
        .map(|field| FieldPath::from_dotted(&field))
        .collect::<Result<Vec<_>, _>>()
}
