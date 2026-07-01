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
//! - [`Expr::TeachingValue`]: teaching-table-only literals (e.g. top-level union constructor `v101{…}`); validated, not executed
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
#![allow(clippy::result_large_err)]

pub mod array_field_policy;
pub mod bind_wire_validate;
pub mod catalog_id;
pub mod catalog_il;
pub mod catalog_ownership;
pub mod cgs_context;
pub mod cgs_expression_validate;
pub mod cgs_federation;
pub mod connect_profile;
pub mod cross_entity;
pub mod discovery;
pub mod discovery_adversarial_intents;
pub mod discovery_presentation;
pub mod domain_lexicon;
pub mod entity_ref_value;
pub mod error;
pub mod error_render;
pub mod expr;
pub mod expr_correction;
pub mod expr_parser;
pub mod expr_sugar;
pub mod expr_surface_render;
pub mod identifiers;
pub mod identity;
pub mod loader;
pub mod normalizer;
pub mod paging_handle;
pub mod plasm_monad;
pub mod predicate;
pub mod preflight;
pub mod prompt_pipeline;
pub mod prompt_render;
pub mod query_resolve;
pub mod relation_segment;
pub mod resolved_identity;
pub mod result_gloss;
pub mod row_composition;
pub mod row_predicate;
pub mod schema;
pub mod schema_overlay;
pub mod scope_entity_ref_splat;
pub mod step_semantics;
pub mod string_unescape;
pub mod summary_render;
pub mod symbol_tuning;
pub mod teaching_term;
pub mod template_interpolate;
pub mod template_ref;
pub mod temporal;
pub mod tests;
pub mod text;
pub mod type_checker;
pub mod typed_invoke;
pub mod typed_literal;
pub mod typed_row;
pub mod value;

pub mod comp_canonical;
mod o200k_token_count;
mod operation_handle;
mod plan_commit;
mod spans;
mod utf8_trunc;
mod wire_coercion;

/// Local `o200k_base` BPE length (OpenAI `o200k_base` via riptoken).
pub use o200k_token_count::o200k_token_count;

pub use array_field_policy::{invoke_array_scalar_error, ArrayFieldCoercionPolicy};
pub use catalog_id::{
    cgs_session_catalog_id, cgs_symbol_map_entry_key, deserialize_catalog_stamp,
    serialize_catalog_stamp, CatalogEntryStamp, EmptyRegistryEntryId, SessionCatalogEntryId,
};
pub use catalog_il::{
    catalog_artifact_stem, catalog_il_body_name, cgs_to_catalog_il_bytes, is_catalog_manifest_path,
    load_catalog_artifact, load_catalog_il_bytes, load_catalog_il_verified, read_catalog_manifest,
    CatalogManifest, CATALOG_IL_BODY_SUFFIX, PLASM_CATALOG_FORMAT_VERSION,
};
pub use catalog_ownership::{
    catalog_entry_id_for_invoke, infer_qualified_entity_from_stamped_source,
    require_relation_source_qualified_entity, resolve_cgs_for_stamped_catalog,
    CatalogOwnershipContext, CatalogOwnershipError, InvokeCatalogResolutionContext,
    FEDERATED_RELATION_MISSING_OWNERSHIP,
};
pub use cgs_context::{CgsContext, Prefix};
pub use cgs_federation::{
    cgs_layer_stack, cgs_layer_stack_from_contexts, lookup_capability_in_layer_stack,
    CatalogResolver, CgsLayer, FederationDispatch, FederationResolveError, QualifiedEntityKey,
};
pub use comp_canonical::plasm_comp_commit_canonical;
pub use connect_profile::{
    catalog_connect_profile, CatalogAuthCapability, CatalogConnectProfile, CatalogOauthCapability,
};
pub use discovery::{
    Ambiguity, CapabilityQuery, CatalogEntryMeta, CgsCatalog, CgsDiscovery, ClosureStats,
    DiscoveryContextJson, DiscoveryError, DiscoveryResult, DiscoverySchemaNeighborhood,
    EntitySummary, InMemoryCgsRegistry, RankedCandidate, RegistryEntryPair,
};
pub use discovery_adversarial_intents::{
    adversarial_case_count, iter_all_cases, DiscoveryAdversarialCase,
    DiscoveryAdversarialFailureKind, CROSS_CUTTING, PER_CATALOG_SELECTION,
};
pub use discovery_presentation::{CatalogRoute, DiscoveryDecision};
pub use entity_ref_value::{
    normalize_entity_ref_value_for_target, try_narrow_entity_row_to_entity_ref_value,
    EntityRefAtom, EntityRefPayload, EntityRefValueError, ScopeEntityRefNormalizeError,
};
pub use error::{NormalizationError, SchemaError, TypeError};
pub use expr::{
    lift_invoke_payloads_in_expr, CancelExpr, ChainExpr, ChainStep, CreateExpr, DeleteExpr,
    EntityKey, Expr, GetExpr, InvokeExpr, PageExpr, QueryExpr, QueryPagination, Ref, WaitExpr,
    OPERATION_EXPR_PRIMARY_ENTITY, PAGE_EXPR_PRIMARY_ENTITY,
};
pub use expr_sugar::rewrite_id_field_brace_query_to_get;
pub use identity::{
    CapabilityName, CapabilityParamName, EntityFieldName, EntityId, EntityName, PathMethodSegment,
    RegistryEntryId, RelationName,
};
pub use loader::{
    finalize_cgs_load, load_schema, load_schema_dir, load_schema_dir_unvalidated,
    load_split_schema, plasm_cgs_fast_load_enabled, PathSchemaSource, SchemaSource,
};
pub use normalizer::{is_normalized, normalize};
pub use operation_handle::{OperationHandle, OperationHandleParseError};
pub use paging_handle::{
    is_valid_logical_session_ref_segment, PagingHandle, PagingHandleParseError,
};
pub use plan_commit::{PlanCommitId, PlanCommitRef};
pub use plasm_monad::{
    comp_equivalent, comp_semantic_eq, empty_comp, invoke_step_payload, map_step_payload,
    plasm_bind_step, plasm_map_step, plasm_parallel_return, plasm_pure_step, AggregateFunction,
    AggregateSpec, BindingName, CompEquivDiff, CompEquivResult, ComputeOp, ComputeTemplate,
    DeriveKind, DerivePayload, DeriveTemplate, EffectBarrier, EffectClass, EffectTemplate,
    FieldPath, FlatMapEffectPayload, FlatMapRelationPayload, InputCardinality, InvokePayload,
    MapPayload, OutputName, PlanDataInput, PlanExprIr, PlanExprTemplate, PlanInputBinding,
    PlanPredicate, PlanPredicateOp, PlanQualifiedEntityKey, PlanRelationTraversal, PlanResultUse,
    PlasmBindGraph, PlasmComp, PlasmCompArtifact, PlasmDataValue, PlasmHoleUse, PlasmReturn,
    PlasmStep, PlasmStepKind, PlasmStepPayload, PurePayload, RelationCardinality,
    RelationSourceCardinality, ResultShape, RewritePolicy, StepId, SurfaceKind,
    SyntheticFieldSchema, SyntheticResultSchema, SyntheticValueKind,
};
pub use predicate::Predicate;
pub use preflight::{
    validate_capability_preflight, PickSpec, PreflightFieldPath, PreflightPlan, PreflightStep,
    ScopeBind,
};
pub use prompt_pipeline::{PromptFocus, PromptPipelineConfig};
pub use prompt_render::grammar_frontmatter_stats_from_contract;
pub use prompt_render::grammar_frontmatter_stats_from_prompt;
pub use prompt_render::render_teaching_bundle;
pub use prompt_render::teaching_tsv_agent_body_from_wrapped_prompt;
pub use prompt_render::teaching_tsv_from_wrapped_prompt;
pub use prompt_render::teaching_tsv_table_from_wrapped_prompt;
pub use prompt_render::GrammarFrontmatterStats;
pub use prompt_render::PromptRenderMode;
pub use prompt_render::TeachingFenceSlice;
pub use prompt_render::TeachingPromptSettings;
pub use prompt_render::TeachingPromptSource;
pub use prompt_render::PLASM_TOOL_DESCRIPTION;
pub use prompt_render::ROW_COMPUTE_EXEMPLAR_THRESHOLD;
pub use prompt_render::TSV_TEACHING_TABLE_HEADER;
pub use query_resolve::{
    normalize_expr_query_capabilities, normalize_expr_query_capabilities_federated,
    required_scope_param_names, resolve_query_capability, QueryCapabilityResolveError,
};
pub use resolved_identity::ResolvedIdentity;
pub use row_composition::{
    parse_row_suffix_stream_tail, resolve_relation_target_id, row_identity_from_parts,
    row_identity_from_ref, IdEncoding, PreflightToken, ResolutionHint, RowIdentity, RowProvenance,
    RowState, RowSuffix,
};
pub use row_predicate::{
    entity_def_for_row_predicate, parse_row_predicate_list, row_predicate_from_expr,
    type_check_row_predicate, RowComparison, RowPredicate, RowPredicateTypeCtx,
};
pub use teaching_term::{
    method_ref_for_capability, method_ref_for_domain_segment, resolve_parameter_slot, EntityRef,
    MethodRef, ParameterSlot, Symbol, TeachingTerm,
};
pub use wire_coercion::{
    binding_value_as_plasm_value, coerce_json_value_for_field_type, coerce_value_for_field_type,
    coerce_value_for_field_type_with_policy, collect_relation_binding_proofs,
    field_type_assignable_for_relation_binding, identity_slot_to_json, json_value_to_plasm_value,
    parent_entity_field_type, plasm_value_to_json, relation_binding_assignable,
    RelationBindingProof,
};
pub mod relation_materialize;
pub use expr_surface_render::{
    render_expr_surface, render_expr_surface_federated, wire_surface_from_teaching_line,
    wire_surface_from_teaching_session_line,
};
pub use relation_materialize::{
    extract_from_parent_get_value, flatten_from_parent_get_source_rows,
    from_parent_get_embed_edges, partition_prefer_resolutions, prefer_hydrate_embed_path,
    relation_refs_fully_resolved, resolve_relation_row_resolution,
    validate_from_parent_get_embed_acyclic, RelationRowResolution, MAX_FROM_PARENT_GET_EMBED_DEPTH,
};
pub use relation_segment::{
    relation_segment_wrong_role_message, resolve_relation_segment, ProgramBindingLabel,
    RelationSegmentContext, RelationSegmentOutcome,
};
pub use schema::{
    capability_is_zero_arity_action, capability_is_zero_arity_invoke,
    capability_method_label_kebab, capability_template_all_var_names,
    template_domain_exemplar_requires_entity_anchor, template_invoke_requires_explicit_anchor_id,
    AgentPresentation, ArrayItemsSchema, AttachmentMediaKind, AuthScheme, CapabilityKind,
    CapabilityManifest, CapabilityMapping, CapabilitySchema, CapabilityTemplateJson, Cardinality,
    CgsCapabilityIndex, CrossFieldRule, CrossFieldRuleType, DiscoveryCapabilityHints,
    DiscoveryEntityHints, DiscoveryRelationHints, EmbedOnMissPolicy, EntityDef, FieldDeriveRule,
    FieldSchema, FieldValueKind, IdFormat, InputFieldSchema, InputFieldWire, InputSchema,
    InputType, InputValidation, InputVariantSchema, JsonPathSegment, NamedValueSchema,
    OauthDefaultScopeSet, OauthExtension, OauthRequirements, OauthScopeEntry, OutputSchema,
    OutputType, ParameterRole, RelationMaterialization, RelationSchema, RelationScopedFallback,
    ResourceSchema, ScopeAggregateKeyPolicy, ScopeRequirement, StringSemantics, ValidationOp,
    ValidationPredicate, ValueDomainKey, ValueDomainSlot, ViewDefinition, ViewNodeSpec,
    ViewOutputBinding, ViewParamBinding, ViewRelationBinding, ViewRelationOutputSpec,
    ViewScopeInject, ViewScopeParam, WireVariantDiscriminator, CGS, DEFAULT_HTTP_BACKEND,
};
pub use schema_overlay::{
    build_decode_scope_key, build_schema_overlay, overlay_bind_cache_suffix, overlay_collect_rows,
    overlay_entity_for_scope, overlay_merge_step_response, overlay_pipeline_cache_suffix,
    resolve_overlay_row_bind, walk_json_path, SchemaOverlay, SchemaOverlaySpec,
};
pub use scope_entity_ref_splat::apply_entity_ref_scope_splat;
pub use step_semantics::*;
pub use string_unescape::normalize_structured_string_inputs;
pub use summary_render::{
    expr_simulation_bindings, render_intent, render_intent_federated,
    render_intent_with_projection, render_intent_with_projection_federated, render_outcome,
};
pub use symbol_tuning::{
    catalog_cgs_hashes_from_session, catalog_pins_match, entity_slices_for_render, relation_endpoint_keys,
    resolve_prompt_surface_entities, strip_prompt_expression_annotations,
    symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog, symbol_map_for_prompt,
    wire_surface_for_parse, wire_surface_for_teaching_session, CatalogScope, ExposedEntitySymbolRow,
    ExposedRelationSymbolRow, ExposureEntityKey, FocusSpec, PersistedSymbolLedger,
    PersistedSymbolLedgerDecodeError, PersistedSymbolLedgerEncodeError,
    PersistedSymbolLedgerState, SymbolAllocate, SymbolMap, SymbolMapCacheKey,
    SymbolMapCrossRequestCache, SymbolRender, SymbolResolve, SymbolResolveError, SymbolSession,
    TeachingExposureSession, PERSISTED_SYMBOL_LEDGER_VERSION,
};
pub use template_interpolate::{
    dollar_interpolation_roots, interpolate_string, interpolate_string_map,
    interpolate_string_with_max, BindingScope, InterpolateError,
};
pub use template_ref::{
    contains_dollar_interpolation, find_dollar_interpolation_in_minijinja_body,
    for_each_interpolation_path, interpolation_paths, interpolation_roots,
    validate_interpolation_syntax, RefKind, TemplateRefContext,
};
pub use temporal::{normalize_temporal_value, temporal_wire_format_from_name, wire_temporal_value};
pub use type_checker::{
    reject_domain_placeholder_in_executable, type_check_chain, type_check_create,
    type_check_delete, type_check_expr, type_check_expr_federated, type_check_get,
    type_check_invoke, type_check_predicate, type_check_query,
};
pub use typed_invoke::{InvokeInputPayload, TypedInvokeInput};
pub use typed_literal::{TypedComparisonValue, TypedLiteral, TypedLiteralError};
pub use typed_row::TypedFieldValue;
pub use value::{
    CompOp, FieldType, PlasmInputRef, TemporalWireFormat, Value, ValueTableCellBudget,
    ValueWireFormat, PLASM_ATTACHMENT_KEY,
};
