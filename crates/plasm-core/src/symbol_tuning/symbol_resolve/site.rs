//! Role-scoped opaque `p#` resolution sites (site + required catalog context in one type).

use crate::CapabilitySchema;
use crate::EntityDef;
use crate::EntityFieldName;
use crate::CGS;

/// Parse-time role for opaque `p#` → wire resolution.
///
/// Each variant carries the catalog objects required for that role — invalid combinations
/// are unrepresentable (no optional context bag).
///
/// Resolution order (intentionally asymmetric):
/// - `EntityRowField`: `sym_to_slot` representative first (frozen EntityField at allocation),
///   then qualified forward map.
/// - `QueryFilter`: forward map (entity fields + query/search cap params), then slot fallback
///   — must not prefer `sym_to_slot` alone when cap-param occurrences share the fingerprint.
/// - `InvokeParam`: cap-param forward map, then `sym_to_slot`.
/// - `CompoundKey`: qualified key index, homograph scan, then slot fallback.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PSymResolution<'a> {
    EntityRowField {
        entity: &'a str,
        ent: &'a EntityDef,
    },
    QueryFilter {
        entity: &'a str,
        ent: &'a EntityDef,
        cgs: &'a CGS,
    },
    InvokeParam {
        domain: &'a str,
        capability: &'a str,
        cap: &'a CapabilitySchema,
    },
    CompoundKey {
        entity: &'a str,
        key_vars: &'a [EntityFieldName],
    },
}
