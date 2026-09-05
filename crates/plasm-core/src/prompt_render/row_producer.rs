//! Row-producer teaching line shaping (projection brackets on query/search rows).

/// Append `[p#,…]` to a teaching expression base when a non-empty bracket is present.
#[inline]
pub(super) fn with_projection_bracket(base: impl AsRef<str>, bracket: Option<&str>) -> String {
    match bracket.filter(|b| !b.trim().is_empty()) {
        Some(br) => format!("{}{br}", base.as_ref()),
        None => base.as_ref().to_string(),
    }
}
