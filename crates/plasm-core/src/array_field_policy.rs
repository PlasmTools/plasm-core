//! Shared compile-time policy for array-typed catalog fields (query filters vs invoke args).

use crate::Value;

/// How array-typed values are coerced and type-checked before live HTTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayFieldCoercionPolicy {
    /// Query/search predicates: a scalar may wrap as a one-element array.
    QueryFilter,
    /// Invoke/create/update/action args: require `[...]` or a deferred column projection.
    InvokeArg,
}

impl ArrayFieldCoercionPolicy {
    pub const fn allows_scalar_wrap(self) -> bool {
        matches!(self, Self::QueryFilter)
    }

    /// Values resolved later from bindings (e.g. `labels.name` column projections).
    pub fn accepts_deferred_value(value: &Value) -> bool {
        matches!(value, Value::PlasmInputRef(_))
    }
}

/// User-facing error when an invoke arg passes a scalar to an array param.
pub fn invoke_array_scalar_error(got_type: &str) -> String {
    format!("expected array `[...]` or a list column projection, got {got_type}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    #[test]
    fn query_filter_allows_scalar_wrap_policy() {
        assert!(ArrayFieldCoercionPolicy::QueryFilter.allows_scalar_wrap());
        assert!(!ArrayFieldCoercionPolicy::InvokeArg.allows_scalar_wrap());
    }

    #[test]
    fn deferred_value_accepts_plasm_input_ref() {
        use crate::PlasmInputRef;
        assert!(ArrayFieldCoercionPolicy::accepts_deferred_value(
            &Value::PlasmInputRef(PlasmInputRef::node_output("rows", vec!["name".into()]))
        ));
        assert!(!ArrayFieldCoercionPolicy::accepts_deferred_value(
            &Value::String("x".into())
        ));
    }
}
