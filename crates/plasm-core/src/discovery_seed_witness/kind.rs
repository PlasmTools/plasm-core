//! Capability-kind buckets for witness prune / seating — one parse path, no string soup.

use crate::schema::CapabilityKind;

/// Coarse bucket used by prune and cover seating (not a second taxonomy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapBucket {
    Read,
    QueryGet,
    Create,
    CreateOrUpdate,
    Action,
    Other,
}

impl CapBucket {
    /// Parse witness / evidence kind strings (`Query`, `query`, …) via [`CapabilityKind`].
    pub fn parse(kind: &str) -> Self {
        if kind.trim().eq_ignore_ascii_case("read_action")
            || kind.trim().eq_ignore_ascii_case("readaction")
        {
            return Self::Read;
        }
        match parse_capability_kind(kind) {
            Some(CapabilityKind::Query)
            | Some(CapabilityKind::Search)
            | Some(CapabilityKind::Get) => Self::Read,
            Some(CapabilityKind::Create) => Self::Create,
            Some(CapabilityKind::Update) => Self::CreateOrUpdate,
            Some(CapabilityKind::Action) => Self::Action,
            Some(CapabilityKind::Delete) | None => Self::Other,
        }
    }

    pub fn is_read(self) -> bool {
        matches!(self, Self::Read)
    }

    pub fn is_query_get(kind: &str) -> bool {
        matches!(
            parse_capability_kind(kind),
            Some(CapabilityKind::Query) | Some(CapabilityKind::Get)
        )
    }

    pub fn is_create(kind: &str) -> bool {
        matches!(parse_capability_kind(kind), Some(CapabilityKind::Create))
    }

    pub fn is_create_or_update(kind: &str) -> bool {
        matches!(
            parse_capability_kind(kind),
            Some(CapabilityKind::Create) | Some(CapabilityKind::Update)
        )
    }

    pub fn is_action(kind: &str) -> bool {
        matches!(parse_capability_kind(kind), Some(CapabilityKind::Action))
    }

    pub fn is_read_kind(kind: &str) -> bool {
        Self::parse(kind).is_read()
    }

    /// Lexical bias when picking a parent read Direct among Query/Search/Get.
    pub fn read_rank(kind: &str) -> u32 {
        if kind.trim().eq_ignore_ascii_case("read_action")
            || kind.trim().eq_ignore_ascii_case("readaction")
        {
            return 10;
        }
        match parse_capability_kind(kind) {
            Some(CapabilityKind::Query) => 30,
            Some(CapabilityKind::Search) => 20,
            Some(CapabilityKind::Get) => 10,
            _ => 0,
        }
    }
}

fn parse_capability_kind(kind: &str) -> Option<CapabilityKind> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "query" => Some(CapabilityKind::Query),
        "search" => Some(CapabilityKind::Search),
        "get" => Some(CapabilityKind::Get),
        "create" => Some(CapabilityKind::Create),
        "update" => Some(CapabilityKind::Update),
        "delete" => Some(CapabilityKind::Delete),
        "action" => Some(CapabilityKind::Action),
        _ => None,
    }
}
