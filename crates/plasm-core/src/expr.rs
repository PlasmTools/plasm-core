use crate::identity::{CapabilityName, EntityId, EntityName};
use crate::paging_handle::PagingHandle;
use crate::typed_invoke::InvokeInputPayload;
use crate::{Predicate, Value, CGS};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Sentinel returned by [`Expr::primary_entity`] for [`Expr::Page`] (not a CGS entity name).
pub const PAGE_EXPR_PRIMARY_ENTITY: &str = "__page__";

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

    /// Resume the next batch of a paginated query using an opaque host-minted handle (`page(pg1)` HTTP, `page(s0_pg1)` MCP).
    #[serde(rename = "page")]
    Page(PageExpr),
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
}

/// Get expression: fetch a specific resource by reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetExpr {
    #[serde(rename = "ref")]
    pub reference: Ref,
    /// Optional CML path bindings overriding [`Ref::key`] for the same names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_vars: Option<IndexMap<String, Value>>,
}

/// Create expression: create a new resource (no target ID).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateExpr {
    pub capability: CapabilityName,
    pub entity: EntityName,
    pub input: InvokeInputPayload,
}

/// Delete expression: remove a resource by ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteExpr {
    pub capability: CapabilityName,
    pub target: Ref,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_vars: Option<IndexMap<String, Value>>,
}

/// Invoke expression: call a capability on a resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvokeExpr {
    pub capability: CapabilityName,
    pub target: Ref,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<InvokeInputPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_vars: Option<IndexMap<String, Value>>,
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
            step: ChainStep::AutoGet,
        }
    }
}

/// Structured identity for an entity row: one scalar or several named path-key parts.
///
/// `Compound` uses lexicographic key order for equality and hashing so cache keys are stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntityKey {
    /// Single-key entity (`id_field` or sole `key_vars` entry).
    Simple(EntityId),
    /// Multi-part path identity; keys must match CGS `key_vars` names.
    Compound(BTreeMap<String, String>),
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
    /// Create a new get expression (single-key entity).
    pub fn new(entity_type: impl Into<EntityName>, id: impl Into<EntityId>) -> Self {
        Self {
            reference: Ref::new(entity_type, id),
            path_vars: None,
        }
    }

    /// Get by reference (compound or simple).
    pub fn from_ref(reference: Ref) -> Self {
        Self {
            reference,
            path_vars: None,
        }
    }

    /// [`from_ref`] plus optional CML path bindings (program `node_input` holes, entity-ref rows, …).
    pub fn from_ref_with_path_vars(
        reference: Ref,
        path_vars: Option<IndexMap<String, Value>>,
    ) -> Self {
        Self {
            reference,
            path_vars,
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
            path_vars: None,
        }
    }

    /// Delete targeting an existing [`Ref`] (e.g. compound key from the CLI).
    pub fn with_target(capability: impl Into<CapabilityName>, target: Ref) -> Self {
        Self {
            capability: capability.into(),
            target,
            path_vars: None,
        }
    }

    /// Like [`with_target`], preserving CML path overlays from a source [`GetExpr`].
    pub fn with_target_path_vars(
        capability: impl Into<CapabilityName>,
        target: Ref,
        path_vars: Option<IndexMap<String, Value>>,
    ) -> Self {
        Self {
            capability: capability.into(),
            target,
            path_vars,
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
            path_vars: None,
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
            path_vars: None,
        }
    }

    /// Like [`with_target`], preserving CML path overlays from a source [`GetExpr`].
    pub fn with_target_path_vars(
        capability: impl Into<CapabilityName>,
        target: Ref,
        input: Option<Value>,
        path_vars: Option<IndexMap<String, Value>>,
    ) -> Self {
        Self {
            capability: capability.into(),
            target,
            input: input.map(InvokeInputPayload::from),
            path_vars,
        }
    }
}

impl Ref {
    /// Single-key reference.
    pub fn new(entity_type: impl Into<EntityName>, id: impl Into<EntityId>) -> Self {
        Self {
            entity_type: entity_type.into(),
            key: EntityKey::Simple(id.into()),
        }
    }

    /// Multi-part reference (`key_vars` in schema order is enforced at validation time).
    pub fn compound(entity_type: impl Into<EntityName>, parts: BTreeMap<String, String>) -> Self {
        Self {
            entity_type: entity_type.into(),
            key: EntityKey::Compound(parts),
        }
    }

    pub fn simple_id(&self) -> Option<&EntityId> {
        match &self.key {
            EntityKey::Simple(id) => Some(id),
            EntityKey::Compound(_) => None,
        }
    }

    pub fn compound_parts(&self) -> Option<&BTreeMap<String, String>> {
        match &self.key {
            EntityKey::Simple(_) => None,
            EntityKey::Compound(m) => Some(m),
        }
    }

    /// Value bound to CML env key `id` and used where a single “primary id” string is required.
    pub fn primary_slot_str(&self) -> String {
        match &self.key {
            EntityKey::Simple(id) => id.to_string(),
            EntityKey::Compound(m) => m
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    /// True if any identity slot is still the DOMAIN teaching `$` token (must not reach transport).
    pub fn contains_domain_placeholder(&self) -> bool {
        match &self.key {
            EntityKey::Simple(id) => id.as_str() == "$",
            EntityKey::Compound(m) => m.values().any(|v| v == "$"),
        }
    }

    /// Convert this reference to a string representation (`Entity:simpleId` or `Entity:k=v,...`).
    pub fn as_string(&self) -> String {
        format!("{}:{}", self.entity_type, self.primary_slot_str())
    }

    /// Parse `Entity:simpleId` (single-key only).
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
        }
    }
}

/// Replace raw invoke/create payloads with [`InvokeInputPayload::Typed`] where CGS allows lifting.
pub fn lift_invoke_payloads_in_expr(expr: &mut Expr, cgs: &CGS) {
    match expr {
        Expr::Query(_) | Expr::Get(_) | Expr::Delete(_) | Expr::Page(_) => {}
        Expr::Create(create) => {
            if let Some(cap) = cgs.get_capability(&create.capability) {
                if let Some(schema) = &cap.input_schema {
                    let lifted =
                        InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs);
                    create.input = lifted;
                }
            }
        }
        Expr::Invoke(invoke) => {
            if let Some(cap) = cgs.get_capability(&invoke.capability) {
                if let Some(schema) = &cap.input_schema {
                    if let Some(inp) = &invoke.input {
                        let lifted =
                            InvokeInputPayload::lift(&inp.to_value(), &schema.input_type, cgs);
                        invoke.input = Some(lifted);
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
