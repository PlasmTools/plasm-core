//! Row-producer teaching line shaping (query list-all vs default projection bracket).

/// Whether a row-producer teaching line should attach the capability `provides` bracket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum RowProducerProjection {
    /// Attach `[p#,…]` from capability/entity field order when non-empty.
    #[default]
    CapabilityProvides,
    /// Query list-all (`e#` only): CGS teaches bare entity symbol without projection suffix.
    BareQueryListAll,
}
