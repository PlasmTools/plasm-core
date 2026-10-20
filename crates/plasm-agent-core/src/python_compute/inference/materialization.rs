//! On rejection only, query the checker for expression evidence. This does not
//! infer Python semantics or admit a value; the original typed failure survives.
use super::*;
use monty_analysis::Span;
use ruff_python_ast::visitor::{self, Visitor};

pub(super) fn locate(
    error: InferenceError,
    source: &str,
    declarations: &declarations::Declarations,
) -> InferenceError {
    let InferenceError::Graph(fault @ InferenceGraphError::UnsupportedNode { .. }) = error else {
        return error;
    };
    let diagnostics = evidence(source, declarations, &fault);
    InferenceError::Materialization {
        source: fault,
        diagnostics,
    }
}

fn evidence(
    source: &str,
    declarations: &declarations::Declarations,
    fault: &InferenceGraphError,
) -> InferenceDiagnostics {
    struct Expressions {
        spans: Vec<Span>,
        collecting_return: bool,
    }
    impl<'a> Visitor<'a> for Expressions {
        fn visit_stmt(&mut self, statement: &'a ruff_python_ast::Stmt) {
            match statement {
                ruff_python_ast::Stmt::Return(ret) => {
                    if let Some(value) = &ret.value {
                        self.collecting_return = true;
                        self.visit_expr(value);
                        self.collecting_return = false;
                    }
                }
                // A helper's return is not the enclosing compute boundary.
                ruff_python_ast::Stmt::FunctionDef(_) | ruff_python_ast::Stmt::ClassDef(_) => {}
                _ => visitor::walk_stmt(self, statement),
            }
        }
        fn visit_expr(&mut self, expression: &'a Expr) {
            if !self.collecting_return
                || self.spans.len() >= AnalysisLimits::default().max_targets as usize
            {
                return;
            }
            self.spans.push(Span {
                start: expression.start().to_u32(),
                end: expression.end().to_u32(),
            });
            visitor::walk_expr(self, expression);
        }
    }
    let mut expressions = Expressions {
        spans: Vec::new(),
        collecting_return: false,
    };
    if let Ok(parsed) = ruff_python_parser::parse_module(source) {
        // Do not inspect generated parameter annotations as user expressions.
        if let Some(ruff_python_ast::Stmt::FunctionDef(function)) = parsed.suite().last() {
            expressions.visit_body(&function.body);
        }
    }
    let request = AnalysisRequest {
        source: source.into(),
        stubs: Some(declarations.source.clone()),
        targets: expressions.spans.clone(),
        limits: AnalysisLimits::default(),
    };
    let mut matches = Vec::new();
    let mut evidence_failure = None;
    match monty_analysis::analyze(&request) {
        Ok(result) => match result.outcome {
            AnalysisOutcome::Inferred(graph) => {
                let decoder = Decoder {
                    graph: &graph,
                    declarations,
                };
                for (span, root) in expressions.spans.into_iter().zip(graph.roots.iter()) {
                    if matches!(decoder.decode(*root), Err(InferenceError::Graph(ref actual)) if actual == fault)
                    {
                        matches.push(span);
                    }
                }
            }
            AnalysisOutcome::Rejected(_) => {
                evidence_failure =
                    Some("expression analysis returned checker diagnostics".to_owned());
            }
        },
        Err(error) => evidence_failure = Some(error.to_string()),
    }
    // Report the narrowest expressions with the same failing type, rather than
    // every enclosing container. These are checker facts, not guessed causes.
    let diagnostics = matches
        .iter()
        .filter(|span| {
            !matches
                .iter()
                .any(|inner| inner != *span && inner.start >= span.start && inner.end <= span.end)
        })
        .map(|span| monty_analysis::AnalysisDiagnostic {
            code: "unmaterializable-expression".into(),
            message: format!("{fault}; this return expression needs a closed materializable type"),
            source: Some("/analysis.py".into()),
            span: Some(*span),
        })
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return body_diagnostics(
            vec![monty_analysis::AnalysisDiagnostic {
                code: "unmaterializable-return".into(),
                message: format!(
                    "{fault}; expression location unavailable{}",
                    evidence_failure
                        .map(|reason| format!(": {reason}"))
                        .unwrap_or_default()
                ),
                source: None,
                span: None,
            }],
            source,
            Some(declarations),
        );
    }
    body_diagnostics(diagnostics, source, Some(declarations))
}
