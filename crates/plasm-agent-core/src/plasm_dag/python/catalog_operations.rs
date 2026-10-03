//! Closed catalog capability partition shared by production dispatch and conformance.
use plasm_core::symbol_tuning::EntityBinding;
use plasm_core::CapabilityKind;

use super::{at, CompileState, ExecuteSession, PyExpr, PythonLoweringError};

/// A taught method resolved against its owning catalog exactly once. Primary
/// `get`/`query`/`search` entry points remain read-specific constructors.
pub(super) struct ResolvedCatalogCall<'a, K> {
    pub cgs: &'a plasm_core::CGS,
    pub schema: &'a plasm_core::schema::CapabilitySchema,
    pub capability: plasm_core::CapabilityName,
    pub kind: K,
}

pub(super) enum ResolvedCatalogMethod<'a> {
    Read(ResolvedCatalogCall<'a, CatalogReadKind>),
    Write(ResolvedCatalogCall<'a, CatalogWriteKind>),
}

pub(super) enum ReadSelection<'a> {
    Primary(CatalogReadKind),
    Taught(ResolvedCatalogCall<'a, CatalogReadKind>),
}

impl ReadSelection<'_> {
    pub(super) fn kind(&self) -> CatalogReadKind {
        match self {
            Self::Primary(kind) => *kind,
            Self::Taught(call) => call.kind,
        }
    }
}

pub(super) fn resolve_taught_method<'a>(
    session: &'a ExecuteSession,
    state: &CompileState<'_>,
    site: &PyExpr,
    token: &str,
    owner: &EntityBinding,
) -> Result<ResolvedCatalogMethod<'a>, PythonLoweringError> {
    let method = state
        .sym_map_for(session)
        .resolve_session_method(token)
        .map_err(|error| at(site, &error.to_string()))?;
    let operation = CatalogOperation::from_kind(method.kind);
    if method.entry_id != owner.entry_id || method.domain != owner.entity {
        return Err(at(
            site,
            match operation {
                CatalogOperation::Read(_) => "read method and entity ownership differ",
                CatalogOperation::Write(_) => "method and receiver catalog/entity ownership differ",
            },
        ));
    }
    let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
        session,
        owner.entry_id.as_str(),
        owner.entity.as_str(),
    )?;
    let cap = cgs
        .get_capability(method.capability.as_str())
        .ok_or("missing method capability")?;
    if CatalogOperation::from_kind(cap.kind) != operation {
        return Err(at(site, "session method kind differs from pinned catalog"));
    }
    Ok(match operation {
        CatalogOperation::Read(kind) => ResolvedCatalogMethod::Read(ResolvedCatalogCall {
            cgs,
            schema: cap,
            capability: method.capability,
            kind,
        }),
        CatalogOperation::Write(kind) => ResolvedCatalogMethod::Write(ResolvedCatalogCall {
            cgs,
            schema: cap,
            capability: method.capability,
            kind,
        }),
    })
}

macro_rules! catalog_operations {
    (read { $($read:ident => $rn:literal),+ } write { $($write:ident => $wn:literal),+ }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CatalogReadKind { $($read),+ }
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CatalogWriteKind { $($write),+ }
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CatalogOperation { Read(CatalogReadKind), Write(CatalogWriteKind) }
        impl CatalogOperation {
            pub const ALL: &'static [Self] = &[
                $(Self::Read(CatalogReadKind::$read),)+
                $(Self::Write(CatalogWriteKind::$write),)+
            ];
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::Read(CatalogReadKind::$read) => $rn,)+
                    $(Self::Write(CatalogWriteKind::$write) => $wn,)+
                }
            }
            pub fn from_kind(kind: CapabilityKind) -> Self {
                match kind {
                    $(CapabilityKind::$read => Self::Read(CatalogReadKind::$read),)+
                    $(CapabilityKind::$write => Self::Write(CatalogWriteKind::$write),)+
                }
            }
        }
        impl CatalogReadKind {
            pub(super) fn primary(name: &str) -> Option<Self> {
                match name { $($rn => Some(Self::$read),)+ _ => None }
            }
        }
    }
}
catalog_operations! {
    read { Get => "get", Query => "query", Search => "search" }
    write { Create => "create", Update => "update", Delete => "delete", Action => "action" }
}
