use crate::catalog_id::CatalogEntryStamp;
use crate::cgs_federation::QualifiedEntityKey;
use crate::identity::{CapabilityName, EntityId, EntityName};
use crate::operation_handle::OperationHandle;
use crate::paging_handle::PagingHandle;
use crate::typed_invoke::InvokeInputPayload;
use crate::{Predicate, Value, CGS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Sentinel returned by [`Expr::primary_entity`] for [`Expr::Page`] (not a CGS entity name).
pub const PAGE_EXPR_PRIMARY_ENTITY: &str = "__page__";

/// Sentinel for [`Expr::Wait`] / [`Expr::Cancel`] (host operation continuations).
pub const OPERATION_EXPR_PRIMARY_ENTITY: &str = "__operation__";

/// Top-level expression types in the Plasm IR.
///
/// Serde uses `tag = "op"`; typed payloads (`Predicate` RHS, invoke/create inputs) serialize through
/// their [`Value`] projection so snapshots stay wire-compatible when the typed IR evolves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Expr {
    #[serde(rename = "query")]
    Query(QueryExpr),

    #[serde(rename = "get")]
    Get(GetExpr),

    #[serde(rename = "create")]
    Create(CreateExpr),

    #[serde(rename = "delete")]
    Delete(DeleteExpr),

    #[serde(rename = "invoke")]
    Invoke(InvokeExpr),

    #[serde(rename = "chain")]
    Chain(ChainExpr),

    /// Resume the next batch of a paginated query using an opaque host-minted handle (`page(pg1)` HTTP, `page(l_<token>_pg1)` MCP).
    #[serde(rename = "page")]
    Page(PageExpr),

    /// Poll/resume an in-flight async plan run (`wait(l_<token>_o1)` MCP or `wait(o1)` HTTP).
    #[serde(rename = "wait")]
    Wait(WaitExpr),

    /// Cancel an in-flight async plan run (`cancel(l_<token>_o1)` MCP or `cancel(o1)` HTTP).
    #[serde(rename = "cancel")]
    Cancel(CancelExpr),

    /// teaching-table-only teaching literal (e.g. top-level union constructor `v101{p#=$,…}`).
    ///
    /// Parsed and type-checked for prompt validation; not executable — the runtime rejects execution.
    #[serde(rename = "teaching_value")]
    TeachingValue {
        /// Typically [`Value::UnionCtor`] with `$` teaching placeholders.
        value: Value,
    },
}

/// Opaque pagination continuation issued by the execute host (not a CGS entity operation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageExpr {
    /// Opaque host handle (`pg1`, …).
    pub handle: PagingHandle,
    /// Optional per-batch entity cap (clamps the upstream page size for this request only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Poll/resume in-flight async plan execution (host-only continuation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitExpr {
    pub handle: OperationHandle,
}

/// Cancel in-flight async plan execution (host-only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelExpr {
    pub handle: OperationHandle,
}

/// Starting position for paginated query execution (CML `pagination` block).
/// How many pages to fetch is **out-of-band** — see [`plasm_runtime::StreamConsumeOpts`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct QueryPagination {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_block: Option<u64>,
}

/// Query expression: filter resources by predicate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryExpr {
    pub entity: EntityName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate: Option<Predicate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>, // Fields to include in response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<QueryPagination>,
    /// When `Some(false)`, skip automatic per-row GET hydration after query. `None` uses engine default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydrate: Option<bool>,
    /// Name of the specific capability that should execute this query.
    ///
    /// When `None`, [`resolve_query_capability`](crate::resolve_query_capability) picks the
    /// capability (primary unscoped query, or predicate-key disambiguation for scoped
    /// queries). Interactive paths may call [`normalize_expr_query_capabilities`](crate::normalize_expr_query_capabilities)
    /// after parse to set this
    /// field for display (`cap=…` in REPL hints). When an entity has multiple query/search
    /// capabilities, explicit or inferred `capability_name` keeps the type-checker,
    /// predicate compiler, and CML execution aligned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_name: Option<CapabilityName>,
    /// Owning registry `entry_id` when the surface head was a session `e#` symbol (federated disambiguation).
    #[serde(
        default,
        skip_serializing_if = "CatalogEntryStamp::is_none",
        with = "crate::catalog_id::catalog_entry_stamp"
    )]
    pub catalog_entry_id: CatalogEntryStamp,
}

/// Get expression: fetch a specific resource by reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetExpr {
    #[serde(rename = "ref")]
    pub reference: Ref,
    #[serde(
        default,
        skip_serializing_if = "CatalogEntryStamp::is_none",
        with = "crate::catalog_id::catalog_entry_stamp"
    )]
    pub catalog_entry_id: CatalogEntryStamp,
    /// When set, dispatches to this GET capability instead of the entity default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_name: Option<CapabilityName>,
}

/// Create expression: create a new resource (no target ID).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateExpr {
    pub capability: CapabilityName,
    pub entity: EntityName,
    pub input: InvokeInputPayload,
    #[serde(
        default,
        skip_serializing_if = "CatalogEntryStamp::is_none",
        with = "crate::catalog_id::catalog_entry_stamp"
    )]
    pub catalog_entry_id: CatalogEntryStamp,
    /// Surface receiver for dotted-call create (`Team(1).team-create-space(…)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dotted_receiver: Option<Box<Expr>>,
}

impl CreateExpr {
    /// Materialize catalog stamp from dotted receiver when missing (parse boundary).
    pub fn materialize_catalog_from_receiver(&mut self) {
        if self.catalog_entry_id.is_none() {
            if let Some(id) = self
                .dotted_receiver
                .as_ref()
                .and_then(|r| r.session_catalog_entry_id())
            {
                self.catalog_entry_id = CatalogEntryStamp::some(id.clone());
            }
        }
    }
}

/// Delete expression: remove a resource by ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteExpr {
    pub capability: CapabilityName,
    pub target: Ref,
    #[serde(
        default,
        skip_serializing_if = "CatalogEntryStamp::is_none",
        with = "crate::catalog_id::catalog_entry_stamp"
    )]
    pub catalog_entry_id: CatalogEntryStamp,
}

/// Invoke expression: call a capability on a resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvokeExpr {
    pub capability: CapabilityName,
    pub target: Ref,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<InvokeInputPayload>,
    #[serde(
        default,
        skip_serializing_if = "CatalogEntryStamp::is_none",
        with = "crate::catalog_id::catalog_entry_stamp"
    )]
    pub catalog_entry_id: CatalogEntryStamp,
}

/// Chain expression: Kleisli composition via EntityRef field navigation.
///
/// Executes `source`, extracts the EntityRef field named by `selector` from the
/// result entity, then dispatches to the target entity's Get capability (or an
/// explicit continuation expression).
///
/// When source yields a collection (`[A]`), the chain maps over each element
/// (List monad `traverse`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainExpr {
    /// Expression that yields the source entity (typically a GetExpr).
    pub source: Box<Expr>,
    /// Name of the EntityRef field on the source entity to follow.
    pub selector: String,
    /// Owning registry row when stamped at parse (avoids re-walking `source`).
    #[serde(
        default,
        skip_serializing_if = "CatalogEntryStamp::is_none",
        with = "crate::catalog_id::catalog_entry_stamp"
    )]
    pub catalog_entry_id: CatalogEntryStamp,
    /// What to do with the resolved Ref — auto-GET or explicit continuation.
    #[serde(default)]
    pub step: ChainStep,
}

/// Continuation after extracting an EntityRef value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
pub enum ChainStep {
    /// Automatically find the target entity's Get capability and fetch it.
    #[default]
    #[serde(rename = "auto_get")]
    AutoGet,
    /// Supply an explicit expression; the extracted Ref is substituted into it.
    #[serde(rename = "explicit")]
    Explicit { expr: Box<Expr> },
}

impl ChainExpr {
    /// Convenience: chain a Get source through an EntityRef field with auto-resolve.
    pub fn auto_get(source: Expr, selector: impl Into<String>) -> Self {
        Self {
            source: Box::new(source),
            selector: selector.into(),
            catalog_entry_id: CatalogEntryStamp::none(),
            step: ChainStep::AutoGet,
        }
    }
}

/// One identity slot on a Get/invoke/delete target — never a CML path-template inventedish key.
///
/// Lit and Binding share the same [`EntityKey`] shape so `Entity("x")` and `Entity(binding)` stay
/// referentially transparent (PLP-1 / IdentitySlot cutover).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IdentitySlot {
    /// Concrete identity string (literal or already-resolved).
    Lit(EntityId),
    /// Deferred program binding (`node_input` / row binding) — resolved at execute.
    Binding(crate::PlasmInputRef),
}

impl IdentitySlot {
    #[must_use]
    pub fn lit(id: impl Into<EntityId>) -> Self {
        Self::Lit(id.into())
    }

    #[must_use]
    pub fn binding(r: crate::PlasmInputRef) -> Self {
        Self::Binding(r)
    }

    #[must_use]
    pub fn as_lit_str(&self) -> Option<&str> {
        match self {
            Self::Lit(id) => Some(id.as_str()),
            Self::Binding(_) => None,
        }
    }

    #[must_use]
    pub fn is_empty_lit(&self) -> bool {
        matches!(self, Self::Lit(id) if id.as_str().is_empty())
    }

    #[must_use]
    pub fn is_domain_placeholder(&self) -> bool {
        match self {
            Self::Lit(id) => id.as_str() == "$",
            Self::Binding(_) => false,
        }
    }

    /// Display / primary-slot string: Lit as-is; Binding as a stable hole marker (not wire id).
    #[must_use]
    pub fn display_str(&self) -> String {
        match self {
            Self::Lit(id) => id.to_string(),
            Self::Binding(crate::PlasmInputRef::NodeInput { node, path }) => {
                if path.is_empty() {
                    format!("@{node}")
                } else {
                    format!("@{node}.{}", path.join("."))
                }
            }
            Self::Binding(crate::PlasmInputRef::RowBinding { binding, path }) => {
                if path.is_empty() {
                    format!("@{binding}")
                } else {
                    format!("@{binding}.{}", path.join("."))
                }
            }
        }
    }
}

impl From<&str> for IdentitySlot {
    fn from(s: &str) -> Self {
        Self::lit(s)
    }
}

impl From<String> for IdentitySlot {
    fn from(s: String) -> Self {
        Self::lit(s)
    }
}

/// Structured identity for an entity row: one scalar or several named identity slots.
///
/// `Compound` uses lexicographic key order for equality and hashing so cache keys are stable.
/// Keys must match CGS `key_vars` / `id_field` — never invented `{entity}_id` transport names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntityKey {
    /// Single-key entity (`id_field` or sole `key_vars` entry).
    Simple(IdentitySlot),
    /// Multi-part identity; keys must match CGS `key_vars` names.
    Compound(BTreeMap<String, IdentitySlot>),
}

/// A stable reference to a resource instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ref {
    pub entity_type: EntityName,
    pub key: EntityKey,
}

impl QueryExpr {
    /// Create a new query for all resources of the given type.
    pub fn all(entity: impl Into<EntityName>) -> Self {
        Self {
            entity: entity.into(),
            predicate: None,
            projection: None,
            pagination: None,
            hydrate: None,
            capability_name: None,
            catalog_entry_id: CatalogEntryStamp::none(),
        }
    }

    /// Create a new query with a predicate filter.
    pub fn filtered(entity: impl Into<EntityName>, predicate: Predicate) -> Self {
        Self {
            entity: entity.into(),
            predicate: Some(predicate),
            projection: None,
            pagination: None,
            hydrate: None,
            capability_name: None,
            catalog_entry_id: CatalogEntryStamp::none(),
        }
    }

    /// Create a new query with field projection.
    pub fn projected(
        entity: impl Into<EntityName>,
        predicate: Option<Predicate>,
        fields: Vec<String>,
    ) -> Self {
        Self {
            entity: entity.into(),
            predicate,
            projection: Some(fields),
            pagination: None,
            hydrate: None,
            capability_name: None,
            catalog_entry_id: CatalogEntryStamp::none(),
        }
    }

    /// Attach the name of the specific capability to use for execution.
    pub fn with_capability(mut self, name: impl Into<CapabilityName>) -> Self {
        self.capability_name = Some(name.into());
        self
    }

    /// Add or modify the predicate for this query.
    pub fn with_predicate(mut self, predicate: Predicate) -> Self {
        self.predicate = Some(predicate);
        self
    }

    /// Add or modify the projection for this query.
    pub fn with_projection(mut self, fields: Vec<String>) -> Self {
        self.projection = Some(fields);
        self
    }

    /// Attach pagination options (used when the CML mapping declares `pagination`).
    pub fn with_pagination(mut self, pagination: QueryPagination) -> Self {
        self.pagination = Some(pagination);
        self
    }
}

impl GetExpr {
    /// Create a new get expression (single-key entity, literal identity).
    pub fn new(entity_type: impl Into<EntityName>, id: impl Into<EntityId>) -> Self {
        Self {
            reference: Ref::new(entity_type, id),
            catalog_entry_id: CatalogEntryStamp::none(),
            capability_name: None,
        }
    }

    /// Pathless zero-arity singleton Get (`e#` / `e#.m#()`): empty identity — no synthetic `"0"`.
    ///
    /// CML env population skips empty identity slots so optional id params are omitted, not wired as `0`.
    pub fn pathless_nullary(entity_type: impl Into<EntityName>) -> Self {
        Self::new(entity_type, "")
    }

    /// Attach the GET capability wire name (overrides entity default get).
    pub fn with_capability(mut self, name: impl Into<CapabilityName>) -> Self {
        self.capability_name = Some(name.into());
        self
    }

    /// Get by reference (compound or simple; Lit or Binding slots).
    pub fn from_ref(reference: Ref) -> Self {
        Self {
            reference,
            catalog_entry_id: CatalogEntryStamp::none(),
            capability_name: None,
        }
    }
}

impl CreateExpr {
    pub fn new(
        capability: impl Into<CapabilityName>,
        entity: impl Into<EntityName>,
        input: impl Into<InvokeInputPayload>,
    ) -> Self {
        Self {
            capability: capability.into(),
            entity: entity.into(),
            input: input.into(),
            catalog_entry_id: CatalogEntryStamp::none(),
            dotted_receiver: None,
        }
    }
}

impl DeleteExpr {
    pub fn new(
        capability: impl Into<CapabilityName>,
        entity_type: impl Into<EntityName>,
        id: impl Into<EntityId>,
    ) -> Self {
        Self {
            capability: capability.into(),
            target: Ref::new(entity_type, id),
            catalog_entry_id: CatalogEntryStamp::none(),
        }
    }

    /// Delete targeting an existing [`Ref`] (e.g. compound key from the CLI).
    pub fn with_target(capability: impl Into<CapabilityName>, target: Ref) -> Self {
        Self {
            capability: capability.into(),
            target,
            catalog_entry_id: CatalogEntryStamp::none(),
        }
    }
}

impl InvokeExpr {
    /// Create a new invoke expression.
    pub fn new(
        capability: impl Into<CapabilityName>,
        entity_type: impl Into<EntityName>,
        id: impl Into<EntityId>,
        input: Option<Value>,
    ) -> Self {
        Self {
            capability: capability.into(),
            target: Ref::new(entity_type, id),
            input: input.map(InvokeInputPayload::from),
            catalog_entry_id: CatalogEntryStamp::none(),
        }
    }

    /// Invoke on an existing [`Ref`] (e.g. compound key from a parsed `Get`).
    pub fn with_target(
        capability: impl Into<CapabilityName>,
        target: Ref,
        input: Option<Value>,
    ) -> Self {
        Self {
            capability: capability.into(),
            target,
            input: input.map(InvokeInputPayload::from),
            catalog_entry_id: CatalogEntryStamp::none(),
        }
    }
}

impl Ref {
    /// Single-key reference with a literal identity.
    pub fn new(entity_type: impl Into<EntityName>, id: impl Into<EntityId>) -> Self {
        Self {
            entity_type: entity_type.into(),
            key: EntityKey::Simple(IdentitySlot::lit(id)),
        }
    }

    /// Single-key reference with a deferred binding identity.
    pub fn simple_binding(entity_type: impl Into<EntityName>, binding: crate::PlasmInputRef) -> Self {
        Self {
            entity_type: entity_type.into(),
            key: EntityKey::Simple(IdentitySlot::binding(binding)),
        }
    }

    /// Multi-part reference with literal string parts (`key_vars` enforced at validation).
    pub fn compound(entity_type: impl Into<EntityName>, parts: BTreeMap<String, String>) -> Self {
        Self {
            entity_type: entity_type.into(),
            key: EntityKey::Compound(
                parts
                    .into_iter()
                    .map(|(k, v)| (k, IdentitySlot::lit(v)))
                    .collect(),
            ),
        }
    }

    /// Multi-part reference with Lit|Binding slots.
    pub fn compound_slots(
        entity_type: impl Into<EntityName>,
        parts: BTreeMap<String, IdentitySlot>,
    ) -> Self {
        Self {
            entity_type: entity_type.into(),
            key: EntityKey::Compound(parts),
        }
    }

    /// Literal simple id, if this is a Simple Lit slot.
    pub fn simple_id(&self) -> Option<&EntityId> {
        match &self.key {
            EntityKey::Simple(IdentitySlot::Lit(id)) => Some(id),
            _ => None,
        }
    }

    /// Compound Lit-only parts as strings (Bindings omitted). Prefer [`compound_slots_ref`].
    pub fn compound_parts(&self) -> Option<BTreeMap<String, String>> {
        match &self.key {
            EntityKey::Simple(_) => None,
            EntityKey::Compound(m) => {
                let mut out = BTreeMap::new();
                for (k, slot) in m {
                    match slot {
                        IdentitySlot::Lit(id) => {
                            out.insert(k.clone(), id.to_string());
                        }
                        IdentitySlot::Binding(_) => return None,
                    }
                }
                Some(out)
            }
        }
    }

    pub fn compound_slots_ref(&self) -> Option<&BTreeMap<String, IdentitySlot>> {
        match &self.key {
            EntityKey::Simple(_) => None,
            EntityKey::Compound(m) => Some(m),
        }
    }

    /// Value bound to CML env key `id` and used where a single “primary id” string is required.
    /// Binding slots yield their display marker (not a wire id).
    pub fn primary_slot_str(&self) -> String {
        match &self.key {
            EntityKey::Simple(slot) => slot.display_str(),
            EntityKey::Compound(m) => m
                .iter()
                .map(|(k, v)| format!("{k}={}", v.display_str()))
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    /// True when the primary Simple Lit identity is empty (pathless nullary Get).
    pub fn is_pathless_nullary(&self) -> bool {
        matches!(&self.key, EntityKey::Simple(s) if s.is_empty_lit())
    }

    /// True if any identity slot is still the teaching table teaching `$` token (must not reach transport).
    pub fn contains_domain_placeholder(&self) -> bool {
        match &self.key {
            EntityKey::Simple(slot) => slot.is_domain_placeholder(),
            EntityKey::Compound(m) => m.values().any(|v| v.is_domain_placeholder()),
        }
    }

    /// Convert this reference to a string representation (`Entity:simpleId` or `Entity:k=v,...`).
    pub fn as_string(&self) -> String {
        format!("{}:{}", self.entity_type, self.primary_slot_str())
    }

    /// Parse `Entity:simpleId` (single-key Lit only).
    pub fn from_string(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() == 2 {
            Some(Self::new(parts[0], parts[1]))
        } else {
            None
        }
    }
}

impl std::fmt::Display for Ref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.entity_type, self.primary_slot_str())
    }
}

/// Structural wire form for `_ref` round-trip (preserves compound keys; Lit only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefWire {
    Simple {
        entity: String,
        id: String,
    },
    Compound {
        entity: String,
        parts: BTreeMap<String, String>,
    },
}

impl RefWire {
    #[must_use]
    pub fn from_ref(reference: &Ref) -> Self {
        match &reference.key {
            EntityKey::Simple(IdentitySlot::Lit(id)) => Self::Simple {
                entity: reference.entity_type.to_string(),
                id: id.to_string(),
            },
            EntityKey::Simple(IdentitySlot::Binding(_)) => Self::Simple {
                entity: reference.entity_type.to_string(),
                id: reference.primary_slot_str(),
            },
            EntityKey::Compound(parts) => Self::Compound {
                entity: reference.entity_type.to_string(),
                parts: parts
                    .iter()
                    .map(|(k, v)| (k.clone(), v.display_str()))
                    .collect(),
            },
        }
    }

    #[must_use]
    pub fn into_ref(self) -> Ref {
        match self {
            Self::Simple { entity, id } => Ref::new(entity, id),
            Self::Compound { entity, parts } => Ref::compound(entity, parts),
        }
    }

    /// Parse `_ref` JSON — structural form first, legacy `Entity:simpleId` string fallback.
    pub fn parse_json(value: &serde_json::Value) -> Option<Ref> {
        if let Ok(wire) = serde_json::from_value::<Self>(value.clone()) {
            return Some(wire.into_ref());
        }
        value
            .as_str()
            .and_then(Ref::from_string)
            .filter(|r| matches!(r.key, EntityKey::Simple(IdentitySlot::Lit(_))))
    }
}

impl Expr {
    pub fn query(query: QueryExpr) -> Self {
        Expr::Query(query)
    }
    pub fn get(get: GetExpr) -> Self {
        Expr::Get(get)
    }
    pub fn create(create: CreateExpr) -> Self {
        Expr::Create(create)
    }
    pub fn delete(delete: DeleteExpr) -> Self {
        Expr::Delete(delete)
    }
    pub fn invoke(invoke: InvokeExpr) -> Self {
        Expr::Invoke(invoke)
    }
    pub fn chain(chain: ChainExpr) -> Self {
        Expr::Chain(chain)
    }

    pub fn page(page: PageExpr) -> Self {
        Expr::Page(page)
    }

    pub fn wait(wait: WaitExpr) -> Self {
        Expr::Wait(wait)
    }

    pub fn cancel(cancel: CancelExpr) -> Self {
        Expr::Cancel(cancel)
    }

    /// Registry `entry_id` when the surface entity token was a session `e#` symbol.
    pub fn session_catalog_entry_id(&self) -> Option<&crate::identity::RegistryEntryId> {
        match self {
            Expr::Query(q) => q.catalog_entry_id.as_ref(),
            Expr::Get(g) => g.catalog_entry_id.as_ref(),
            Expr::Create(c) => c.catalog_entry_id.as_ref(),
            Expr::Delete(d) => d.catalog_entry_id.as_ref(),
            Expr::Invoke(i) => i.catalog_entry_id.as_ref(),
            Expr::Chain(c) => c
                .catalog_entry_id
                .as_ref()
                .or_else(|| c.source.session_catalog_entry_id()),
            Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) | Expr::TeachingValue { .. } => None,
        }
    }

    /// `(catalog_entry_id, entity)` for federated type-check and plan dispatch when present.
    pub fn qualified_entity_key(&self) -> Option<QualifiedEntityKey> {
        let entry_id = self.session_catalog_entry_id()?;
        Some(QualifiedEntityKey::new(
            entry_id.clone(),
            self.primary_entity(),
        ))
    }

    /// Attach session catalog ownership from a parsed opaque `e#` head.
    pub fn with_session_catalog_entry_id(
        mut self,
        catalog_entry_id: Option<impl AsRef<str>>,
    ) -> Self {
        let stamp = CatalogEntryStamp::from_opt_str(catalog_entry_id);
        match &mut self {
            Expr::Query(q) => q.catalog_entry_id = stamp,
            Expr::Get(g) => g.catalog_entry_id = stamp,
            Expr::Create(c) => c.catalog_entry_id = stamp,
            Expr::Delete(d) => d.catalog_entry_id = stamp,
            Expr::Invoke(i) => i.catalog_entry_id = stamp,
            Expr::Chain(c) => {
                if c.catalog_entry_id.is_none() {
                    c.catalog_entry_id = stamp;
                }
            }
            Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) | Expr::TeachingValue { .. } => {}
        }
        self
    }

    /// Get the primary entity type this expression operates on.
    pub fn primary_entity(&self) -> &str {
        match self {
            Expr::Query(q) => q.entity.as_str(),
            Expr::Get(g) => g.reference.entity_type.as_str(),
            Expr::Create(c) => c.entity.as_str(),
            Expr::Delete(d) => d.target.entity_type.as_str(),
            Expr::Invoke(i) => i.target.entity_type.as_str(),
            Expr::Chain(c) => c.source.primary_entity(),
            Expr::Page(_) => PAGE_EXPR_PRIMARY_ENTITY,
            Expr::Wait(_) | Expr::Cancel(_) => OPERATION_EXPR_PRIMARY_ENTITY,
            Expr::TeachingValue { .. } => "__teaching_value__",
        }
    }

    /// Entity that owns the next `.relation` segment after `self` in a surface chain.
    /// Unlike [`primary_entity`](Self::primary_entity), nested chains resolve from the
    /// outermost hop's declared relation target (e.g. `LangItem.summary.detail` → `LangSummary`).
    pub fn relation_navigation_entity(&self, cgs: &CGS) -> Option<String> {
        match self {
            Expr::Chain(chain) => {
                let parent = chain.source.relation_navigation_entity(cgs)?;
                let ent = cgs.get_entity(&parent)?;
                let rel = ent.relations.get(chain.selector.as_str())?;
                Some(rel.target_resource.to_string())
            }
            other => Some(other.primary_entity().to_string()),
        }
    }
}

/// Replace raw invoke/create payloads with [`InvokeInputPayload::Typed`] where CGS allows lifting.
pub fn lift_invoke_payloads_in_expr(expr: &mut Expr, cgs: &CGS) {
    match expr {
        Expr::Query(_)
        | Expr::Get(_)
        | Expr::Delete(_)
        | Expr::Page(_)
        | Expr::Wait(_)
        | Expr::Cancel(_)
        | Expr::TeachingValue { .. } => {}
        Expr::Create(create) => {
            if let Some(cap) = cgs.get_capability(&create.capability) {
                // Dual object lanes stay Raw — Typed lift is single-schema; typecheck partitions.
                if cap.invocation_object_schemas().count() <= 1 {
                    if let Some(schema) = cap.primary_invocation_schema() {
                        create.input = InvokeInputPayload::lift(
                            &create.input.to_value(),
                            &schema.input_type,
                            cgs,
                        );
                    }
                }
            }
        }
        Expr::Invoke(invoke) => {
            if let Some(cap) = cgs.get_capability(&invoke.capability) {
                // Dual object lanes stay Raw — Typed lift is single-schema; typecheck partitions.
                if cap.invocation_object_schemas().count() <= 1 {
                    if let (Some(schema), Some(inp)) =
                        (cap.primary_invocation_schema(), invoke.input.as_ref())
                    {
                        invoke.input = Some(InvokeInputPayload::lift(
                            &inp.to_value(),
                            &schema.input_type,
                            cgs,
                        ));
                    }
                }
            }
        }
        Expr::Chain(chain) => {
            lift_invoke_payloads_in_expr(chain.source.as_mut(), cgs);
            if let ChainStep::Explicit { expr: inner } = &mut chain.step {
                lift_invoke_payloads_in_expr(inner.as_mut(), cgs);
            }
        }
    }
}
