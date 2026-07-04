//! Typed failures for opaque teaching-symbol resolution.

/// Typed failure when an opaque teaching symbol cannot be resolved in context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolResolveError {
    UnknownEntityPSym {
        catalog_entry_id: String,
        entity: String,
        token: String,
    },
    NotARowField {
        entity: String,
        token: String,
    },
    UnknownQueryFilterPSym {
        entity: String,
        token: String,
    },
    AmbiguousQueryFilterPSym {
        entity: String,
        token: String,
        candidates: Vec<String>,
    },
    UnknownCapParam {
        catalog_entry_id: String,
        domain: String,
        capability: String,
        capability_kind: crate::CapabilityKind,
        token: String,
    },
    UnknownCompoundKey {
        entity: String,
        token: String,
        expected: Vec<String>,
    },
    UnknownMethodSym {
        token: String,
    },
    UnknownEntitySym {
        token: String,
    },
    UnknownSessionPSym {
        token: String,
    },
    WrongSlotKind {
        token: String,
        expected: &'static str,
        got: String,
    },
    MethodAnchorMismatch {
        token: String,
        bound_domain: String,
        anchor_entity: String,
    },
}

impl SymbolResolveError {
    const SESSION_RECOVERY_SUFFIX: &'static str =
        "Use session_mode: \"extend\" with your logical_session_ref — do not call session_mode: \"new\" to recover.";

    /// Optional agent-facing hint appended after the primary error line.
    pub fn agent_program_hint(&self) -> Option<&'static str> {
        match self {
            Self::UnknownEntityPSym { .. } | Self::NotARowField { .. } => Some(
                "Use `p#` symbols from the teaching `rows:` column for this binding.",
            ),
            Self::UnknownQueryFilterPSym { .. } | Self::AmbiguousQueryFilterPSym { .. } => Some(
                "Use `p#` from the teaching `rows:` column or the query/search input signature for this entity.",
            ),
            Self::UnknownCapParam { .. } => Some(
                "Use `p#` symbols from the teaching table input signature for this capability.",
            ),
            Self::UnknownCompoundKey { .. } => Some(
                "Supply every compound identity key using wire names or the taught `p#` symbols.",
            ),
            Self::UnknownMethodSym { .. } | Self::MethodAnchorMismatch { .. } => Some(
                "Use `m#` symbols from the teaching table for this session. If the capability exists but was not taught, pass its wire name in `ranked_capabilities` and call plasm_context with session_mode: \"extend\".",
            ),
            Self::UnknownEntitySym { .. } => Some(
                "Use `e#` symbols from the teaching table for this session.",
            ),
            Self::UnknownSessionPSym { .. } | Self::WrongSlotKind { .. } => Some(
                "Use `p#` symbols from the teaching table for this session.",
            ),
        }
    }

    /// Primary error line plus optional `help:` suffix for agent program surfaces.
    pub fn to_agent_program_error(&self) -> String {
        match self.agent_program_hint() {
            Some(hint) => format!("{self}\nhelp: {hint} {}", Self::SESSION_RECOVERY_SUFFIX),
            None => format!("{self}\nhelp: {}", Self::SESSION_RECOVERY_SUFFIX),
        }
    }

    pub fn is_unknown_cap_param(&self) -> bool {
        matches!(self, Self::UnknownCapParam { .. })
    }

    pub fn is_unknown_query_filter(&self) -> bool {
        matches!(
            self,
            Self::UnknownQueryFilterPSym { .. } | Self::AmbiguousQueryFilterPSym { .. }
        )
    }
}

impl std::fmt::Display for SymbolResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEntityPSym {
                entity,
                token,
                ..
            } => write!(
                f,
                "`{token}` is not a row symbol for `{entity}` in this session"
            ),
            Self::NotARowField { entity, token } => write!(
                f,
                "`{token}` is not a row field on `{entity}` for this binding"
            ),
            Self::UnknownQueryFilterPSym { entity, token } => write!(
                f,
                "`{token}` is not a query filter symbol for `{entity}` in this session (not a row field or query/search scope param)"
            ),
            Self::AmbiguousQueryFilterPSym {
                entity,
                token,
                candidates,
            } => write!(
                f,
                "`{token}` is ambiguous for `{entity}` query filters — matches params {}",
                candidates.join(", ")
            ),
            Self::UnknownCapParam {
                domain,
                capability,
                capability_kind,
                token,
                ..
            } => write!(
                f,
                "`{token}` is not an input parameter on {capability_kind} `{domain}.{capability}` in this session"
            ),
            Self::UnknownCompoundKey {
                entity,
                token,
                expected,
            } => write!(
                f,
                "compound constructor key `{token}` is not valid for `{entity}` — expected one of {}",
                expected.join(", ")
            ),
            Self::UnknownMethodSym { token } => {
                write!(f, "`{token}` is not a method symbol in this session")
            }
            Self::UnknownEntitySym { token } => {
                write!(f, "`{token}` is not an entity symbol in this session")
            }
            Self::UnknownSessionPSym { token } => {
                write!(f, "`{token}` is not a slot symbol in this session")
            }
            Self::WrongSlotKind {
                token,
                expected,
                got,
            } => write!(
                f,
                "`{token}` is `{got}` in this session, expected {expected}"
            ),
            Self::MethodAnchorMismatch {
                token,
                bound_domain,
                anchor_entity,
            } => write!(
                f,
                "`{token}` is bound to `{bound_domain}`, not `{anchor_entity}` in this session"
            ),
        }
    }
}
