//! Unified Plasm program AST.
//!
//! Owns statement/root shape for multi-line programs: bindings, pipe / primary row surfaces,
//! collect-meta tails, and `=>` applicators. Catalogue path leaves remain opaque strings until
//! parsed with a CGS via [`super::parse`].

use super::program_surface::{
    collect_program_statement_lines, split_assignment_at_top_level, split_top_level,
    validate_program_label,
};
use super::{
    parse_pipe_expr, peel_collect_meta, split_apply_expr, Applicator, CollectMeta, PipeExpr,
};
use crate::row_composition::RowSuffix;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProgram {
    pub statements: Vec<Statement>,
    pub roots: Vec<ExprNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Bind { label: String, expr: ExprNode },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowExpr {
    Primary {
        head: String,
        collect_meta: Vec<CollectMeta>,
    },
    Pipe(PipeExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprNode {
    pub row: RowExpr,
    /// Stratum-3 applicator when the surface contains top-level `=>`.
    pub apply: Option<Applicator>,
}

impl ExprNode {
    pub fn row_suffixes(&self) -> Result<Vec<RowSuffix>, String> {
        match &self.row {
            RowExpr::Pipe(p) => p.row_suffixes(),
            RowExpr::Primary { collect_meta, .. } => {
                Ok(collect_meta.iter().map(RowSuffix::from).collect())
            }
        }
    }

    pub fn primary_head(&self) -> &str {
        match &self.row {
            RowExpr::Primary { head, .. } => head.as_str(),
            RowExpr::Pipe(p) => p.head.as_str(),
        }
    }
}

/// Parse line-oriented Plasm program shape (bindings, final roots, postfix transforms).
///
/// Joins tagged heredocs across physical lines ([`super::collect_program_statement_lines`]) before
/// splitting bindings vs roots. Path syntax inside primaries is validated only when parsed with a CGS.
pub fn parse_program_shape(source: &str) -> Result<ParsedProgram, String> {
    let mut statements = Vec::new();
    let mut roots = None::<Vec<ExprNode>>;

    for raw_stmt in collect_program_statement_lines(source)? {
        let line = raw_stmt.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((label, rhs)) = split_assignment_at_top_level(line) {
            validate_program_label(label)?;
            let expr = parse_expr_node(rhs)?;
            statements.push(Statement::Bind {
                label: label.to_string(),
                expr,
            });
        } else {
            if line.starts_with("return ") {
                return Err("return is not Plasm syntax; use bare final roots".into());
            }
            roots = Some(if parse_pipe_expr(line)?.is_some() {
                vec![parse_expr_node(line)?]
            } else {
                split_top_level(line, ',')?
                    .into_iter()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(parse_expr_node)
                    .collect::<Result<Vec<_>, _>>()?
            });
        }
    }

    let roots = roots.ok_or_else(|| "Plasm program needs a final root line".to_string())?;
    if roots.is_empty() {
        return Err("Plasm program final roots list is empty".into());
    }
    Ok(ParsedProgram { statements, roots })
}

pub fn parse_expr_node(raw: &str) -> Result<ExprNode, String> {
    let (row_surface, apply) = split_apply_expr(raw)?;
    if let Some(pipe) = parse_pipe_expr(&row_surface)? {
        return Ok(ExprNode {
            row: RowExpr::Pipe(pipe),
            apply,
        });
    }
    let (head, collect_meta) = peel_collect_meta(&row_surface)?;
    if head.trim().is_empty() {
        return Err("expression primary is empty".into());
    }
    Ok(ExprNode {
        row: RowExpr::Primary { head, collect_meta },
        apply,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binding_and_direct_pipe_root() {
        let p =
            parse_program_shape("repo = e2(owner=\"ryan\", repo=\"plasm\")\ne1{p4=repo} | take 20")
                .expect("program");
        assert_eq!(p.statements.len(), 1);
        assert_eq!(p.roots.len(), 1);
        assert_eq!(p.roots[0].primary_head(), "e1{p4=repo}");
        assert!(matches!(
            p.roots[0].row_suffixes().unwrap().as_slice(),
            [RowSuffix::Limit { count: 20 }]
        ));
    }

    #[test]
    fn parses_label_pipe_chain() {
        let p = parse_program_shape("commits = e1{}\ncommits | order by date desc | take 10")
            .expect("program");
        assert_eq!(p.roots[0].primary_head(), "commits");
        assert_eq!(p.roots[0].row_suffixes().unwrap().len(), 2);
    }

    #[test]
    fn preserves_commas_inside_direct_pipe_root() {
        let p = parse_program_shape("e1 | select id, title").expect("program");
        assert_eq!(p.roots.len(), 1);
        assert_eq!(p.roots[0].primary_head(), "e1");
        assert!(matches!(
            p.roots[0].row_suffixes().unwrap().as_slice(),
            [RowSuffix::Project { fields }] if fields == &vec!["id".to_string(), "title".to_string()]
        ));
    }

    #[test]
    fn retains_apply_stratum_on_pipe_root() {
        let node =
            parse_expr_node("e1 | where owner=\"alice\" | take 2 => { t: _.message, o: _.owner }")
                .expect("expr");
        assert!(matches!(
            node.apply,
            Some(Applicator::Derive { ref body }) if body.contains("_.message")
        ));
        assert!(matches!(
            node.row_suffixes().unwrap().first(),
            Some(RowSuffix::Filter { .. })
        ));
        assert!(matches!(
            node.row_suffixes().unwrap().last(),
            Some(RowSuffix::Limit { count: 2 })
        ));
    }

    #[test]
    fn rejects_domain_symbol_binding_labels() {
        let err = parse_program_shape("e1 = foo()\nbar").expect_err("domain symbol label");
        assert!(
            err.contains("Binding names must be labels") && err.contains("e1"),
            "{err}"
        );
    }

    #[test]
    fn joins_multiline_tagged_heredoc_before_roots() {
        let src = "body = <<H\nhello\nH\nbody";
        let p = parse_program_shape(src).expect("program");
        assert_eq!(p.statements.len(), 1);
        assert_eq!(p.roots.len(), 1);
        assert_eq!(p.roots[0].primary_head(), "body");
    }

    /// Regression: heredoc bodies must stay opaque — prose starting with `If` must not break
    /// statement joining or surface parsing (phrase/value confusion).
    #[test]
    fn multiline_heredoc_body_with_if_line_joins_and_parses_shape() {
        let src = concat!(
            "report_doc = e3.m19(p12=<<BUG10\n",
            "# Proof Bug Report\n",
            "Proof side:\n",
            "If GET state or equivalent state returns\n",
            "old baseToken while block ref has new mt1.\n",
            "BUG10)\n",
            "report_doc",
        );
        let stmts = collect_program_statement_lines(src).expect("collect statements");
        assert_eq!(stmts.len(), 2, "expected binding + roots; got {:?}", stmts);
        parse_program_shape(src).expect("program shape");
    }
}
