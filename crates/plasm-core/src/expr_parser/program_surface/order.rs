//! Binding / return statement order validation.

use std::collections::BTreeSet;

use super::errors::program_invalid_binding_label_error;
use super::errors::{
    program_binding_after_return_error, program_intermediate_return_error,
    program_intermediate_return_must_be_binding_error, program_multiple_return_lines_error,
};
use super::flatten::leading_identifier;
use super::labels::is_valid_program_label;
use super::split::{classify_top_level_assignment, TopLevelAssignment};

/// ML `let` block: bindings first, one return last. Rejects multiple roots-only lines and bindings after return.
pub fn validate_program_statement_order(statements: &[String]) -> Result<(), String> {
    let stmts: Vec<&str> = statements
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut bindings: BTreeSet<String> = BTreeSet::new();
    let mut saw_roots = false;
    let n = stmts.len();
    for (i, stmt) in stmts.iter().enumerate() {
        let is_last = i + 1 == n;
        if let Some(assignment) = classify_top_level_assignment(stmt) {
            let label = match assignment {
                TopLevelAssignment::Binding { label, .. } => label,
                TopLevelAssignment::InvalidLabel { label } => {
                    return Err(program_invalid_binding_label_error(label));
                }
            };
            if saw_roots {
                return Err(program_binding_after_return_error());
            }
            bindings.insert(label.to_string());
            continue;
        }
        if saw_roots && !is_last {
            if is_bare_binding_label(stmt, &bindings) {
                return Err(program_multiple_return_lines_error());
            }
            return Err(program_intermediate_return_error(stmt));
        }
        if !is_last && roots_line_is_postfix_on_binding(stmt, &bindings) {
            return Err(program_intermediate_return_must_be_binding_error(stmt));
        }
        saw_roots = true;
    }
    Ok(())
}

fn is_bare_binding_label(stmt: &str, bindings: &BTreeSet<String>) -> bool {
    let stmt = stmt.trim();
    let head = leading_identifier(stmt);
    !head.is_empty() && head.len() == stmt.len() && bindings.contains(head)
}

fn roots_line_is_postfix_on_binding(stmt: &str, bindings: &BTreeSet<String>) -> bool {
    let stmt = stmt.trim();
    let head = leading_identifier(stmt);
    if head.is_empty() || head.len() == stmt.len() {
        return false;
    }
    if !is_valid_program_label(head) {
        return false;
    }
    if !bindings.contains(head) {
        return false;
    }
    let rest = stmt[head.len()..].trim_start();
    if rest.is_empty() || rest.starts_with('[') {
        return false;
    }
    rest.starts_with('.')
}
