//! Role-scoped opaque `p#` resolution sites (site + required catalog context in one type).

use crate::CapabilitySchema;
use crate::CGS;
use crate::EntityDef;
use crate::EntityFieldName;

/// Parse-time role for opaque `p#` → wire resolution.
///
/// Each variant carries the catalog objects required for that role — invalid combinations
/// are unrepresentable (no optional context bag).
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
