//! Canonical pipe row algebra (`catalog_source | stage` / `binding | stage`).

use crate::row_composition::RowSuffix;
use crate::row_membership::{parse_closed_rowset_ref, MembershipRhs};

use super::{
    is_valid_program_label, peel_collect_meta, split_top_level, validate_pipe_head_syntax,
    CollectMeta,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeStage {
    Where {
        predicates: String,
    },
    Select {
        fields: Vec<String>,
        assignments: Vec<(String, String)>,
        include_all: bool,
    },
    Summarize {
        keys: Vec<String>,
        aggregates: String,
    },
    OrderBy {
        terms: Vec<(String, bool)>,
    },
    Take(usize),
    Distinct {
        keys: Option<String>,
    },
    Union {
        rhs: MembershipRhs,
    },
}

impl PipeStage {
    /// Lower one stage into ordered [`RowSuffix`] segments (typed row-algebra IR).
    pub fn to_row_suffixes(&self) -> Vec<RowSuffix> {
        match self {
            PipeStage::Where { predicates } => vec![RowSuffix::Filter {
                body: predicates.clone(),
            }],
            PipeStage::Select {
                fields,
                assignments,
                include_all,
            } => {
                let mut out = Vec::new();
                if !assignments.is_empty() {
                    out.push(RowSuffix::With {
                        body: assignments
                            .iter()
                            .map(|(name, expr)| format!("{name}: {expr}"))
                            .collect::<Vec<_>>()
                            .join(","),
                    });
                }
                if !*include_all {
                    let mut projected = fields.clone();
                    projected.extend(assignments.iter().map(|(name, _)| name.clone()));
                    out.push(RowSuffix::Project { fields: projected });
                }
                out
            }
            PipeStage::Summarize { keys, aggregates } if keys.is_empty() => {
                vec![RowSuffix::Aggregate {
                    args: aggregates.clone(),
                }]
            }
            PipeStage::Summarize { keys, aggregates } => vec![RowSuffix::GroupBy {
                args: format!("{},{}", keys.join(","), aggregates),
            }],
            // Stable sorts compose from least-significant to most-significant key.
            PipeStage::OrderBy { terms } => terms
                .iter()
                .rev()
                .map(|(field, descending)| RowSuffix::Sort {
                    args: format!("{field}, {}", if *descending { "desc" } else { "asc" }),
                })
                .collect(),
            PipeStage::Take(n) => vec![RowSuffix::Limit { count: *n as u32 }],
            PipeStage::Distinct { keys } => {
                vec![RowSuffix::Distinct { keys: keys.clone() }]
            }
            PipeStage::Union { rhs } => {
                let rhs = match rhs {
                    MembershipRhs::Binding(name) => name.clone(),
                    MembershipRhs::Pipe(inner) => format!("({inner})"),
                };
                vec![RowSuffix::Union { rhs }]
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeExpr {
    pub head: String,
    pub stages: Vec<PipeStage>,
    /// Collect-meta only (`.page_size` / `.singleton`) — not row algebra.
    pub collect_meta: Vec<CollectMeta>,
}

impl PipeExpr {
    /// Typed row-algebra stream for DAG lowering.
    pub fn row_suffixes(&self) -> Result<Vec<RowSuffix>, String> {
        let mut out = Vec::new();
        for stage in &self.stages {
            out.extend(stage.to_row_suffixes());
        }
        for meta in &self.collect_meta {
            out.push(RowSuffix::from(meta));
        }
        Ok(out)
    }
}

/// Parse a canonical pipe expression. `Ok(None)` means that no top-level pipe is present.
///
/// Top-level `=> applicator` is **not** part of the pipe (application stratum). It is stripped
/// here so callers that only need row algebra never see `take N => …` as a stage body.
pub fn parse_pipe_expr(raw: &str) -> Result<Option<PipeExpr>, String> {
    let (without_apply, _applicator) = match crate::expr_parser::split_token_top_level(raw, "=>")? {
        Some((left, right)) => (left, Some(right)),
        None => (raw, None),
    };
    let (without_meta, collect_meta) = peel_collect_meta(without_apply)?;
    let parts = split_top_level(&without_meta, '|')?;
    if parts.len() == 1 {
        return Ok(None);
    }
    if parts.iter().any(|part| part.trim().is_empty()) {
        return Err("pipe stages must not be empty; use `head | stage`".into());
    }

    let raw_head = parts[0].trim();
    if raw_head.starts_with("from ") {
        return Err(
            "`from` is not Plasm syntax; write a catalog head (`e#`, `e#{…}`, `e#(id)`, `e#~\"q\"`, or wire entity) before `|` stages"
                .into(),
        );
    }

    validate_pipe_head_syntax(raw_head)?;

    let stages = parts[1..]
        .iter()
        .map(|part| parse_stage(part.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(PipeExpr {
        head: raw_head.to_string(),
        stages,
        collect_meta,
    }))
}

fn parse_stage(raw: &str) -> Result<PipeStage, String> {
    if let Some(body) = keyword_tail(raw, "where") {
        require_nonempty(body, "where predicates")?;
        return Ok(PipeStage::Where {
            predicates: body.to_string(),
        });
    }
    if let Some(body) = keyword_tail(raw, "select") {
        return parse_select(body);
    }
    if let Some(body) = keyword_tail(raw, "summarize") {
        return parse_summarize(body);
    }
    if let Some(body) = keyword_tail(raw, "order by") {
        return parse_order_by(body);
    }
    if let Some(body) = keyword_tail(raw, "take") {
        let n = positive_integer(body, "take")?;
        return Ok(PipeStage::Take(n));
    }
    if raw == "distinct" {
        return Ok(PipeStage::Distinct { keys: None });
    }
    if let Some(body) = keyword_tail(raw, "distinct by") {
        require_nonempty(body, "distinct keys")?;
        return Ok(PipeStage::Distinct {
            keys: Some(body.to_string()),
        });
    }
    if let Some(body) = keyword_tail(raw, "union") {
        require_nonempty(body, "union rhs")?;
        return Ok(PipeStage::Union {
            rhs: parse_closed_rowset_ref(body, "union")?,
        });
    }
    Err(unknown_pipe_stage_diagnostic(raw))
}

fn pipe_stage_head_token(raw: &str) -> &str {
    let raw = raw.trim();
    let end = raw
        .find(|c: char| c == '(' || c.is_ascii_whitespace())
        .unwrap_or(raw.len());
    &raw[..end]
}

fn unknown_pipe_stage_diagnostic(raw: &str) -> String {
    let ident = pipe_stage_head_token(raw);
    if crate::is_shared_minijinja_filter(ident) {
        let mut msg = String::from("unknown pipe stage `");
        msg.push_str(raw);
        msg.push_str(
            "` is a Minijinja filter, not row algebra; write the filter inside `{{ }}` or per-row `=> <<TAG`",
        );
        return msg;
    }
    format!(
        "unknown pipe stage `{raw}`; use `where`, `select`, `summarize`, `order by`, `take`, `distinct`, or `union`"
    )
}

fn parse_select(body: &str) -> Result<PipeStage, String> {
    require_nonempty(body, "select items")?;
    let mut fields = Vec::new();
    let mut assignments = Vec::new();
    let mut include_all = false;
    for item in split_top_level(body, ',')? {
        let item = item.trim();
        if item == "*" {
            include_all = true;
        } else if let Some((name, expr)) = item.split_once('=') {
            let name = name.trim();
            let expr = expr.trim();
            if !is_valid_program_label(name) || expr.is_empty() {
                return Err(format!(
                    "select assignment `{item}` must be `name = expression`"
                ));
            }
            assignments.push((name.to_string(), expr.to_string()));
        } else if item.is_empty() {
            return Err("select items must not be empty".into());
        } else {
            fields.push(item.to_string());
        }
    }
    if include_all && fields.is_empty() && assignments.is_empty() {
        return Err("`select *` alone is redundant; remove the stage".into());
    }
    Ok(PipeStage::Select {
        fields,
        assignments,
        include_all,
    })
}

fn parse_summarize(body: &str) -> Result<PipeStage, String> {
    require_nonempty(body, "summarize aggregates")?;
    let (keys, aggregates) = if let Some(rest) = body.strip_prefix("by ") {
        let aggregate_start = find_named_aggregate_start(rest).ok_or_else(|| {
            "`summarize by` requires named aggregates, e.g. `summarize by owner n=count()`"
                .to_string()
        })?;
        let keys = split_top_level(rest[..aggregate_start].trim_end_matches([',', ' ']), ',')?
            .iter()
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return Err("`summarize by` requires at least one key".into());
        }
        (keys, rest[aggregate_start..].to_string())
    } else {
        (Vec::new(), body.to_string())
    };
    let aggregates = normalize_count_calls(&aggregates);
    if !aggregates.contains('=') {
        return Err("`summarize` requires named aggregates, e.g. `summarize n=count()`".into());
    }
    Ok(PipeStage::Summarize { keys, aggregates })
}

fn find_named_aggregate_start(raw: &str) -> Option<usize> {
    let bytes = raw.as_bytes();
    let mut depth = 0i32;
    let mut quote = None::<u8>;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' | b'\'' if quote == Some(b) => quote = None,
            b'"' | b'\'' if quote.is_none() => quote = Some(b),
            b'(' | b'[' | b'{' if quote.is_none() => depth += 1,
            b')' | b']' | b'}' if quote.is_none() => depth -= 1,
            _ => {}
        }
        if quote.is_none() && depth == 0 && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let start = j;
            if j < bytes.len() && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
                j += 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'=' {
                    return Some(start);
                }
            }
        }
        i += 1;
    }
    None
}

fn parse_order_by(body: &str) -> Result<PipeStage, String> {
    require_nonempty(body, "order keys")?;
    let mut terms = Vec::new();
    for raw_term in split_top_level(body, ',')? {
        let mut words = raw_term.split_whitespace();
        let field = words
            .next()
            .ok_or_else(|| "`order by` requires a field".to_string())?;
        let descending = match words.next() {
            None | Some("asc") => false,
            Some("desc") => true,
            Some(other) => {
                return Err(format!(
                    "unknown order direction `{other}`; use `asc` or `desc`"
                ))
            }
        };
        if words.next().is_some() {
            return Err(format!("invalid order term `{raw_term}`"));
        }
        terms.push((field.to_string(), descending));
    }
    Ok(PipeStage::OrderBy { terms })
}

fn keyword_tail<'a>(raw: &'a str, keyword: &str) -> Option<&'a str> {
    raw.strip_prefix(keyword)
        .filter(|tail| tail.chars().next().is_some_and(char::is_whitespace))
        .map(str::trim)
}

fn require_nonempty(raw: &str, what: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        Err(format!("pipe stage requires {what}"))
    } else {
        Ok(())
    }
}

fn positive_integer(raw: &str, stage: &str) -> Result<usize, String> {
    let n = raw
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("`{stage}` requires a positive integer"))?;
    if n == 0 {
        return Err(format!("`{stage}` requires a positive integer"));
    }
    Ok(n)
}

fn normalize_count_calls(raw: &str) -> String {
    raw.replace("count()", "count")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr_parser::pipe_head_has_catalog_surface_syntax;

    #[test]
    fn parses_catalog_pipeline_and_lowers_stages() {
        let parsed = parse_pipe_expr(
            "e1{state=\"open\"} | where score >= 10 | select *, age = now - updated_at | summarize by owner n=count(), total=sum(score) | order by total desc | take 10",
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed.head, "e1{state=\"open\"}");
        assert!(pipe_head_has_catalog_surface_syntax(&parsed.head));
        assert!(matches!(parsed.stages[0], PipeStage::Where { .. }));
        assert!(matches!(parsed.stages[1], PipeStage::Select { .. }));
        assert!(matches!(parsed.stages[2], PipeStage::Summarize { .. }));
        assert!(matches!(parsed.stages[3], PipeStage::OrderBy { .. }));
        assert!(matches!(parsed.stages[4], PipeStage::Take(10)));
        assert!(matches!(
            parsed.row_suffixes().unwrap().last(),
            Some(RowSuffix::Limit { count: 10 })
        ));
    }

    #[test]
    fn strips_apply_arrow_before_stage_parse() {
        let parsed =
            parse_pipe_expr("e1{state=\"open\"} | where score >= 10 | take 3 => { t: _.title }")
                .unwrap()
                .unwrap();
        assert!(matches!(parsed.stages.last(), Some(PipeStage::Take(3))));
    }

    #[test]
    fn parses_binding_pipeline_without_from() {
        let parsed = parse_pipe_expr("items | distinct by owner, id")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.head, "items");
        assert!(!pipe_head_has_catalog_surface_syntax(&parsed.head));
        assert!(matches!(
            parsed.stages.as_slice(),
            [PipeStage::Distinct { keys: Some(_) }]
        ));
    }

    #[test]
    fn parses_binding_where_pipeline_without_from() {
        let parsed = parse_pipe_expr("items | where score >= 10")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.head, "items");
        assert!(!pipe_head_has_catalog_surface_syntax(&parsed.head));
        assert!(matches!(
            parsed.stages.as_slice(),
            [PipeStage::Where { predicates }] if predicates == "score >= 10"
        ));
    }

    #[test]
    fn parses_catalog_e_symbol_pipe_without_from() {
        let parsed = parse_pipe_expr("e1 | take 1").unwrap().unwrap();
        assert_eq!(parsed.head, "e1");
        assert!(pipe_head_has_catalog_surface_syntax(&parsed.head));
    }

    #[test]
    fn parses_catalog_brace_pipe_without_from() {
        let parsed = parse_pipe_expr("e3{access_token=tok} | where created_at >= \"7d ago\"")
            .unwrap()
            .unwrap();
        assert!(pipe_head_has_catalog_surface_syntax(&parsed.head));
    }

    #[test]
    fn rejects_removed_from_keyword() {
        assert!(parse_pipe_expr("from e1 | take 1")
            .unwrap_err()
            .contains("`from` is not Plasm syntax"));
        assert!(parse_pipe_expr("from items | take 1")
            .unwrap_err()
            .contains("`from` is not Plasm syntax"));
    }

    #[test]
    fn rejects_non_row_algebra_pipe_stages() {
        for src in [
            "items | update(title=\"x\")",
            "items | m3(title=\"x\")",
            "items | relation",
            "items | <<MD\ntext\nMD",
        ] {
            let err = parse_pipe_expr(src).expect_err(src);
            assert!(err.contains("unknown pipe stage"), "src={src} err={err}");
            assert!(
                !err.contains("Minijinja"),
                "non-filter token must not claim the filter lane: src={src} err={err}"
            );
        }
    }

    #[test]
    fn taught_minijinja_filter_stage_names_render_lane() {
        for src in [
            r#"items | split_part(id, "-", 0)"#,
            "items | urlencode",
            r#"items | split("/", 1)"#,
            "items | strip_trailing_slash",
        ] {
            let err = parse_pipe_expr(src).expect_err(src);
            assert!(
                err.contains("unknown pipe stage")
                    && err.contains("Minijinja")
                    && err.contains("{{")
                    && err.contains("=> <<TAG"),
                "taught filter stage must name the render lane: src={src} err={err}"
            );
            assert!(
                parse_pipe_expr(src).is_err(),
                "must not legalize `| filter` as row algebra: {src}"
            );
        }
    }

    #[test]
    fn parses_binding_take_select_pipeline() {
        let parsed = parse_pipe_expr("items | take 3 | select id, title")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.stages.len(), 2);
    }

    #[test]
    fn parses_union_binding_and_parenthesized_pipe() {
        let bind = parse_pipe_expr("alice | union bob").unwrap().unwrap();
        assert!(matches!(
            bind.stages[0],
            PipeStage::Union {
                rhs: MembershipRhs::Binding(ref name)
            } if name == "bob"
        ));
        let pipe =
            parse_pipe_expr(r#"alice | union (LangItem | where owner = "bob" | select owner)"#)
                .unwrap()
                .unwrap();
        assert!(matches!(
            pipe.stages[0],
            PipeStage::Union {
                rhs: MembershipRhs::Pipe(ref inner)
            } if inner.contains("select owner")
        ));
        assert!(matches!(
            pipe.row_suffixes().unwrap().last(),
            Some(RowSuffix::Union { rhs }) if rhs.starts_with('(')
        ));
    }

    #[test]
    fn rejects_union_literal_list() {
        let err = parse_pipe_expr(r#"alice | union ("a", "b")"#).expect_err("list");
        assert!(err.contains("not a literal list"), "{err}");
    }
}
