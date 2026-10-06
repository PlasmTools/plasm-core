//! Membership closure is a DAG law; Python binding evidence comes from Monty.
use super::*;

pub(super) fn validate_closed_rhs(
    expr: &PyExpr,
    outer_parameter: &str,
) -> Result<(), PythonLoweringError> {
    let source = format!("({})", monty::expression_source(expr));
    if monty_analysis::external_names(&source)?
        .iter()
        .any(|(_, name)| name == outer_parameter)
    {
        return Err(at(expr, PythonSourceError::MembershipCapturesRow));
    }
    Ok(())
}
