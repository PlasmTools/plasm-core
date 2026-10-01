//! Closed catalog capability partition shared by production dispatch and conformance.
use plasm_core::CapabilityKind;

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
