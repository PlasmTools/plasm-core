//! Typed seed-role stamps for closed witnesses.
//!
//! Catalog enums [`DiscoverySeedClass`] / [`DiscoverySeedNav`] are authoritative.
//! Decision machinery (prune / plans) must never re-parse presentation strings.

use std::collections::BTreeSet;
use std::fmt;

use crate::schema::{DiscoverySeedClass, DiscoverySeedNav};

/// Authored entity `discovery.seed_class`, or unset when absent from the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeedClassStamp {
    Authored(DiscoverySeedClass),
    Unset,
}

impl SeedClassStamp {
    pub fn from_catalog(class: Option<DiscoverySeedClass>) -> Self {
        class.map(Self::Authored).unwrap_or(Self::Unset)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authored(c) => c.as_str(),
            Self::Unset => "unset",
        }
    }

    pub fn is_primary(self) -> bool {
        matches!(self, Self::Authored(DiscoverySeedClass::Primary))
    }

    pub fn is_dependent(self) -> bool {
        matches!(self, Self::Authored(DiscoverySeedClass::Dependent))
    }

    pub fn is_ambient(self) -> bool {
        matches!(self, Self::Authored(DiscoverySeedClass::Ambient))
    }
}

impl fmt::Display for SeedClassStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Governing in-pool edge `discovery.seed_nav`, or unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeedNavStamp {
    Authored(DiscoverySeedNav),
    Unset,
}

impl SeedNavStamp {
    pub fn from_catalog(nav: Option<DiscoverySeedNav>) -> Self {
        nav.map(Self::Authored).unwrap_or(Self::Unset)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authored(n) => n.as_str(),
            Self::Unset => "unset",
        }
    }

    pub fn is_attach(self) -> bool {
        matches!(self, Self::Authored(DiscoverySeedNav::Attach))
    }

    pub fn is_own(self) -> bool {
        matches!(self, Self::Authored(DiscoverySeedNav::Own))
    }

    pub fn is_locate(self) -> bool {
        matches!(self, Self::Authored(DiscoverySeedNav::Locate))
    }
}

impl fmt::Display for SeedNavStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Role of an entity on its in-pool `own` edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OwnEnd {
    Source,
    Target,
    Both,
    Unset,
}

impl OwnEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Target => "target",
            Self::Both => "both",
            Self::Unset => "unset",
        }
    }
}

impl fmt::Display for OwnEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One directed in-pool `own` edge (`source` owns collection/history of `target`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnEdge {
    pub source: String,
    pub target: String,
}

impl OwnEdge {
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
        }
    }

    pub fn render(&self) -> String {
        format!("{}→{}", self.source, self.target)
    }
}

/// In-pool `own` edges incident on a witness entity. Empty ≡ unset for BAML.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwnPairs(BTreeSet<OwnEdge>);

impl OwnPairs {
    pub fn new(edges: impl IntoIterator<Item = OwnEdge>) -> Self {
        Self(edges.into_iter().collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &OwnEdge> {
        self.0.iter()
    }

    pub fn end_role(&self, entity: &str) -> OwnEnd {
        if self.0.is_empty() {
            return OwnEnd::Unset;
        }
        let mut is_source = false;
        let mut is_target = false;
        for edge in &self.0 {
            if edge.source == entity {
                is_source = true;
            }
            if edge.target == entity {
                is_target = true;
            }
        }
        match (is_source, is_target) {
            (true, true) => OwnEnd::Both,
            (true, false) => OwnEnd::Source,
            (false, true) => OwnEnd::Target,
            (false, false) => OwnEnd::Unset,
        }
    }

    /// BAML / LLM presentation (`Source→Target` `|`-joined, or `unset`).
    pub fn render(&self) -> String {
        if self.0.is_empty() {
            "unset".into()
        } else {
            self.0
                .iter()
                .map(OwnEdge::render)
                .collect::<Vec<_>>()
                .join("|")
        }
    }
}

/// Outgoing in-pool child edge (wire kept for presentation; prune uses `target`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PoolChild {
    pub wire: String,
    pub target: String,
}

impl PoolChild {
    pub fn new(wire: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            wire: wire.into(),
            target: target.into(),
        }
    }
}

/// Pool-local graph links used by prune (typed) and BAML (`render_graph_note`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PoolLinks {
    pub parents: BTreeSet<String>,
    pub children: BTreeSet<PoolChild>,
    pub siblings: BTreeSet<String>,
}

impl PoolLinks {
    pub fn parent_entities(&self) -> impl Iterator<Item = &str> {
        self.parents.iter().map(String::as_str)
    }

    pub fn child_targets(&self) -> impl Iterator<Item = &str> {
        self.children.iter().map(|c| c.target.as_str())
    }

    /// Compact graph cues for the LLM (parents/children/siblings in pool).
    pub fn render_graph_note(&self) -> String {
        let mut parts = Vec::new();
        if !self.parents.is_empty() {
            parts.push(format!(
                "relation_child_of={}",
                self.parents.iter().cloned().collect::<Vec<_>>().join("|")
            ));
        }
        if !self.children.is_empty() {
            parts.push(format!(
                "relation_anchor_to={}",
                self.children
                    .iter()
                    .map(|c| format!("{}:{}", c.wire, c.target))
                    .collect::<Vec<_>>()
                    .join("|")
            ));
        }
        if !self.siblings.is_empty() {
            parts.push(format!(
                "siblings_in_pool={}",
                self.siblings.iter().cloned().collect::<Vec<_>>().join("|")
            ));
        }
        parts.join("; ")
    }
}

/// Prefer attach over own over locate when multiple incoming edges compete.
pub(crate) fn prefer_seed_nav(a: DiscoverySeedNav, b: DiscoverySeedNav) -> DiscoverySeedNav {
    use DiscoverySeedNav::*;
    match (a, b) {
        (Attach, _) | (_, Attach) => Attach,
        (Own, _) | (_, Own) => Own,
        (Locate, Locate) => Locate,
    }
}
