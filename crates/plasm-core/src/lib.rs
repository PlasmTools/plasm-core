//! # plasm-core
//!
//! Core types and type system for the Plasm semantic projection layer.
//!
//! This crate defines the foundational data structures that all other Plasm crates
//! depend on. It is purely declarative — no I/O, no async, no HTTP.
//!
//! ## CGS (Capability Graph Schema)
//!
//! The [`CGS`] is the central schema container. It holds:
//!
//! - **Entities** ([`EntityDef`]): typed resources with fields and relations.
//!   Each entity has a primary ID field, typed fields ([`FieldSchema`] with [`FieldType`]),
//!   and outbound relations ([`RelationSchema`]) to other entities.
//!
//! - **Capabilities** ([`CapabilitySchema`]): operations on entities. Each capability
//!   has a [`CapabilityKind`] (Query, Get, Create, Update, Delete, Action), an HTTP
//!   mapping template (CML), and optional input/output schemas.
//!
//! Each [`CGS`] declares a required default HTTP origin ([`CGS::http_backend`]) for CML
//! execution against REST backends; the same graph still drives CLI generation and MCP surfaces.
//! Load via [`loader::load_schema`]
//! (split `domain.yaml` + `mappings.yaml`, combined authoring YAML, or `.cgs.yaml` interchange).
//!
//! ## Predicate IR
//!
//! The [`Predicate`] enum defines a typed query language for filtering entities:
//!
//! ```text
//! Predicate ::= True | False
//!             | Comparison { field, op, value }
//!             | And(Vec<Predicate>)
//!             | Or(Vec<Predicate>)
//!             | Not(Box<Predicate>)
//!             | ExistsRelation { relation, predicate? }
//! ```
//!
//! Predicates are type-checked against entity schemas via [`type_check_predicate`],
//! then normalized to canonical form via [`normalize`] (flatten nested And/Or,
//! apply DeMorgan's laws, eliminate trivials, deduplicate).
//!
//! ## Expression IR
//!
//! The [`Expr`] enum defines top-level operations:
//!
//! - [`QueryExpr`]: filter a collection (optional predicate + projection)
//! - [`GetExpr`]: fetch a single entity by reference
//! - [`CreateExpr`]: create a new entity (no target ID)
//! - [`DeleteExpr`]: remove an entity by reference
//! - [`InvokeExpr`]: call a capability on an entity (update, action, etc.)
//! - [`ChainExpr`]: Kleisli composition via EntityRef field navigation
//!
//! All expressions are type-checked before execution via [`type_check_expr`].
//!
//! ## Cross-Entity Composition
//!
//! The [`cross_entity`] module provides predicate analysis for dot-path predicates
//! that cross EntityRef boundaries (e.g. `pet.status = available` on an Order query).
//! It decomposes these into push-left (foreign query first) or pull-right (client-side
//! filter) strategies based on available capabilities.
//!
//! ## Value System
//!
//! [`Value`] is the universal value type (Null, Bool, Number, String, Array, Object).
//! [`FieldType`] defines the schema-level types (String, Number, Integer, Boolean,
//! Select, MultiSelect, Date, Array). [`CompOp`] defines comparison operators
//! (Eq, Neq, Gt, Lt, Gte, Lte, In, Contains, Exists) with per-type compatibility rules.
//!
//! ## Input Validation
//!
//! Capabilities can declare an [`InputSchema`] with typed fields ([`InputFieldSchema`]),
//! validation predicates, and cross-field rules. The type checker validates invoke
//! inputs against this schema, including enum value constraints and required field checks.
//!
//! ## Identity newtypes
//!
//! The [`identity`] module defines string newtypes ([`EntityName`], [`EntityId`], [`CapabilityName`], etc.)
//! so entity, capability, and parameter names do not cross-wire by accident. Re-exported at crate root.
//!
pub mod cgs_context;
pub mod cgs_expression_validate;
pub mod cgs_federation;
pub mod connect_profile;
pub mod cross_entity;
pub mod discovery;
pub mod domain_lexicon;
pub mod domain_term;
pub mod entity_ref_value;
pub mod error;
pub mod error_render;
pub mod expr;
pub mod expr_correction;
pub mod expr_parser;
pub mod identifiers;
pub mod identity;
pub mod loader;
pub mod normalizer;
pub mod paging_handle;
pub mod predicate;
pub mod prompt_pipeline;
pub mod prompt_render;
pub mod query_resolve;
pub mod result_gloss;
pub mod schema;
pub mod scope_entity_ref_splat;
pub mod step_semantics;
pub mod string_unescape;
pub mod summary_render;
pub mod symbol_tuning;
pub mod temporal;
pub mod tests;
pub mod type_checker;
pub mod typed_invoke;
pub mod typed_literal;
pub mod typed_row;
pub mod value;

mod o200k_token_count;
mod spans;
mod utf8_trunc;

/// Local `o200k_base` BPE length (OpenAI `o200k_base` via riptoken).
pub use o200k_token_count::o200k_token_count;

pub use cgs_context::{CgsContext, Prefix};
pub use cgs_federation::{FederationDispatch, QualifiedEntityKey};
pub use connect_profile::{
    catalog_connect_profile, CatalogAuthCapability, CatalogConnectProfile, CatalogOauthCapability,
};
pub use discovery::{
    Ambiguity, CapabilityQuery, CatalogEntryMeta, CgsCatalog, CgsDiscovery, ClosureStats,
    DiscoveryContextJson, DiscoveryError, DiscoveryResult, DiscoverySchemaNeighborhood,
    EntitySummary, InMemoryCgsRegistry, RankedCandidate, RegistryEntryPair,
};
pub use domain_term::{
    method_ref_for_domain_segment, resolve_parameter_slot, DomainTerm, EntityRef, MethodRef,
    ParameterSlot, Symbol,
};
pub use entity_ref_value::{
    normalize_entity_ref_value_for_target, try_narrow_entity_row_to_entity_ref_value,
    EntityRefAtom, EntityRefPayload, EntityRefValueError, ScopeEntityRefNormalizeError,
};
pub use error::{NormalizationError, SchemaError, TypeError};
pub use expr::{
    lift_invoke_payloads_in_expr, ChainExpr, ChainStep, CreateExpr, DeleteExpr, EntityKey, Expr,
    GetExpr, InvokeExpr, PageExpr, QueryExpr, QueryPagination, Ref, PAGE_EXPR_PRIMARY_ENTITY,
};
pub use identity::{
    CapabilityName, CapabilityParamName, EntityFieldName, EntityId, EntityName, PathMethodSegment,
    RegistryEntryId, RelationName,
};
pub use loader::{load_schema, load_schema_dir, load_split_schema, PathSchemaSource, SchemaSource};
pub use normalizer::{is_normalized, normalize};
pub use paging_handle::{
    is_valid_logical_session_ref_segment, PagingHandle, PagingHandleParseError,
};
pub use predicate::Predicate;
pub use prompt_pipeline::{PromptFocus, PromptPipelineConfig};
pub use prompt_render::domain_tsv_table_from_wrapped_prompt;
pub use prompt_render::render_domain_bundle;
pub use prompt_render::render_domain_prompt_bundle_for_exposure;
pub use prompt_render::render_domain_tsv;
pub use prompt_render::split_tsv_domain_contract_and_table;
pub use prompt_render::DomainPromptSettings;
pub use prompt_render::DomainPromptSource;
pub use prompt_render::PromptRenderMode;
pub use prompt_render::TSV_DOMAIN_TABLE_HEADER;
pub use query_resolve::{
    normalize_expr_query_capabilities, normalize_expr_query_capabilities_federated,
    required_scope_param_names, resolve_query_capability, QueryCapabilityResolveError,
};
pub use schema::{
    capability_is_zero_arity_action, capability_is_zero_arity_invoke,
    capability_method_label_kebab, capability_template_all_var_names,
    template_domain_exemplar_requires_entity_anchor, template_invoke_requires_explicit_anchor_id,
    AgentPresentation, ArrayItemsSchema, AttachmentMediaKind, AuthScheme, CapabilityKind,
    CapabilityManifest, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson, Cardinality,
    CgsCapabilityIndex, CrossFieldRule, CrossFieldRuleType, EntityDef, FieldDeriveRule,
    FieldSchema, FieldValueKind, IdFormat, InputFieldSchema, InputSchema, InputType,
    InputValidation, InvokePreflight, JsonPathSegment, NamedValueSchema, OauthDefaultScopeSet,
    OauthExtension, OauthRequirements, OauthScopeEntry, OutputSchema, OutputType, ParameterRole,
    RelationMaterialization, RelationSchema, ResourceSchema, ScopeAggregateKeyPolicy,
    ScopeRequirement, StringSemantics, ValidationOp, ValidationPredicate, ValueDomainKey,
    ValueDomainSlot, CGS, DEFAULT_HTTP_BACKEND,
};
pub use scope_entity_ref_splat::apply_entity_ref_scope_splat;
pub use step_semantics::*;
pub use string_unescape::normalize_structured_string_inputs;
pub use summary_render::{
    expr_simulation_bindings, render_intent, render_intent_federated,
    render_intent_with_projection, render_intent_with_projection_federated, render_outcome,
};
pub use symbol_tuning::{
    entity_slices_for_render, expand_expr_for_domain_session, expand_expr_for_parse,
    expand_path_symbols, resolve_prompt_surface_entities, strip_prompt_expression_annotations,
    symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog, symbol_map_for_prompt,
    DomainExposureSession, FocusSpec, SymbolMap, SymbolMapCacheKey, SymbolMapCrossRequestCache,
};
pub use type_checker::{
    type_check_chain, type_check_create, type_check_delete, type_check_expr,
    type_check_expr_federated, type_check_get, type_check_invoke, type_check_predicate,
    type_check_query,
};
pub use typed_invoke::{InvokeInputPayload, TypedInvokeInput};
pub use typed_literal::{TypedComparisonValue, TypedLiteral, TypedLiteralError};
pub use typed_row::TypedFieldValue;
pub use value::{
    CompOp, FieldType, PlasmInputRef, TemporalWireFormat, Value, ValueTableCellBudget,
    ValueWireFormat, PLASM_ATTACHMENT_KEY,
};
