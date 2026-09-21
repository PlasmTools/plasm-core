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
//! [`Value`] is the universal value type (Null, Bool, Number, String, Array, Object, Money).
//! [`FieldType`] defines the schema-level types (String, Number, Integer, Boolean,
//! Select, MultiSelect, Date, Array, Money). [`CompOp`] defines comparison operators
//! (Eq, Neq, Gt, Lt, Gte, Lte, In, Contains, Exists) with per-type compatibility rules.
//!
//! ## Input Validation
//!
//! Capabilities can declare an [`InputSchema`] with typed fields ([`InputFieldSchema`]),
//! `values:` scalar constraints (via `value_ref`), and cross-field rules. The type checker validates invoke
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
pub mod capability_exposure;
pub mod catalog_discovery;
pub mod catalog_id;
pub mod catalog_il;
pub mod catalog_ownership;
pub mod cgs_context;
pub mod cgs_expression_validate;
pub mod cgs_federation;
pub mod cgs_normalize;
pub mod connect_profile;
pub mod cross_entity;
pub mod derived_get;
pub mod discovery;
pub mod domain_lexicon;
pub mod entity_ref_value;
pub mod enum_teaching_meaning;
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
pub mod money;
pub mod normalizer;
pub mod paging_handle;
pub mod path_env;
pub mod phrase_ident;
pub mod plasm_monad;
pub mod plp;
pub mod predicate;
pub mod preflight;
pub mod prerequisites;
pub mod program_string_template;
pub mod prompt_pipeline;
pub mod prompt_render;
pub mod query_defaults;
pub mod query_resolve;
pub mod relation_nav;
pub mod relation_segment;
pub mod relation_validation_expr;
pub mod resolved_identity;
pub mod result_gloss;
pub mod row_composition;
pub mod row_contract;
pub mod row_membership;
pub mod row_plan;
pub mod row_predicate;
pub mod row_union;
pub mod rowset;
pub mod schema;
pub mod schema_overlay;
pub mod scope_entity_ref_infer;
pub mod scope_entity_ref_splat;
pub mod step_semantics;
pub mod string_unescape;
pub mod summary_render;
pub mod symbol_tuning;
pub mod taught_seat;
pub mod teaching_term;
pub mod template_ref;
pub mod temporal;
pub mod tests;
pub mod text;
pub mod type_checker;
pub mod typed_invoke;
pub mod typed_literal;
pub mod typed_row;
pub mod value;
pub mod value_domain;
pub mod workflow_identity;

mod capability_input;
pub mod comp_canonical;
mod o200k_token_count;
mod operation_handle;
mod plan_commit;
#[cfg(test)]
mod span_graph_tests;
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
pub use derived_get::DerivedGetPlan;
pub use discovery::{CatalogEntryMeta, CgsCatalog, CgsRegistry, DiscoveryError, RegistryEntryPair};
pub use entity_ref_value::{
    normalize_entity_ref_value_for_target, try_narrow_entity_row_to_entity_ref_value,
    EntityRefAtom, EntityRefPayload, EntityRefValueError, ScopeEntityRefNormalizeError,
};
pub use error::{NormalizationError, SchemaError, TypeError};
pub use expr::{
    lift_invoke_payloads_in_expr, CancelExpr, ChainExpr, ChainStep, CreateExpr, DeleteExpr,
    EntityKey, Expr, GetExpr, IdentitySlot, InvokeExpr, PageExpr, QueryExpr, QueryPagination, Ref,
    RefWire, WaitExpr, OPERATION_EXPR_PRIMARY_ENTITY, PAGE_EXPR_PRIMARY_ENTITY,
};
pub use expr_sugar::{
    lower_id_field_brace_to_get, lower_id_field_brace_to_get_federated, predicate_is_sole_field_eq,
    IdentityLoweringError,
};
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
pub use path_env::{
    capability_declared_input_names, classify_path_var, create_binds_from_anchor_identity,
    graphql_operation_variable_names, identity_env_var_names, identity_wire_names,
    is_identity_projectable, path_var_names_from_mapping_json, project_capability_identity_env,
    project_identity_onto_vars, prove_path_env_coverage, prove_path_env_coverage_in_cgs,
    CapabilityIdentityProjection, CmlIdentityEnv, IdentityEnvVars, IdentityNameMatch,
    IdentityWireNames, PathEnvProjectionError, PathEnvProofError, SoleAliasPolicy,
};
pub use phrase_ident::{
    is_identifier_phrase, lower_program_phrase_idents_in_expr,
    lower_program_phrase_idents_in_expr_federated, quoted_literal_hint,
    unquote_single_string_literal, PhraseIdentFieldContext,
};
pub use plan_commit::{PlanCommitId, PlanCommitRef};
pub use plasm_monad::{
    comp_equivalent, comp_semantic_eq, empty_comp, invoke_step_payload, map_step_payload,
    plasm_bind_step, plasm_map_step, plasm_parallel_return, plasm_pure_step, AggregateFunction,
    AggregateSpec, ArithOp, BindingName, CompEquivDiff, CompEquivResult, ComputeOp,
    ComputeTemplate, DeriveKind, DerivePayload, DeriveTemplate, EffectBarrier, EffectClass,
    EffectTemplate, FieldPath, FlatMapApplyPayload, FlatMapRelationPayload, InputCardinality,
    InvokePayload, MapPayload, OutputName, PlanDataInput, PlanExprIr, PlanExprTemplate,
    PlanInputBinding, PlanPredicate, PlanPredicateOp, PlanQualifiedEntityKey,
    PlanRelationTraversal, PlanResultUse, PlasmBindGraph, PlasmComp, PlasmCompArtifact,
    PlasmDataValue, PlasmHoleUse, PlasmReturn, PlasmStep, PlasmStepKind, PlasmStepPayload,
    PurePayload, RelationCardinality, RelationSourceCardinality, ResultShape, RewritePolicy,
    StepId, SurfaceKind, SyntheticFieldSchema, SyntheticResultSchema, SyntheticValueKind,
    UnfoldUntilPayload, WithColumn, WithExpr, WithExprError, WithLiteral, PLASM_COMP_WIRE_VERSION,
};
pub use predicate::Predicate;
pub use preflight::{
    validate_capability_preflight, PickSpec, PreflightFieldPath, PreflightPlan, PreflightStep,
    ScopeBind,
};
pub use prompt_pipeline::{PromptFocus, PromptPipelineConfig};
pub use prompt_render::catalog_teaching_fence_info;
pub use prompt_render::grammar_frontmatter_stats_from_contract;
pub use prompt_render::grammar_frontmatter_stats_from_prompt;
pub use prompt_render::prompt_symbol_inflation_stats_from_prompt;
pub use prompt_render::render_teaching_bundle;
pub use prompt_render::teaching_tsv_agent_body_from_wrapped_prompt;
pub use prompt_render::teaching_tsv_from_wrapped_prompt;
pub use prompt_render::teaching_tsv_table_from_wrapped_prompt;
pub use prompt_render::teaching_tsv_table_from_wrapped_prompt_any;
pub use prompt_render::GrammarFrontmatterStats;
pub use prompt_render::PromptRenderMode;
pub use prompt_render::PromptSymbolInflationStats;
pub use prompt_render::TeachingFenceSlice;
pub use prompt_render::TeachingPromptSettings;
pub use prompt_render::TeachingPromptSource;
pub use prompt_render::PLASM_TOOL_DESCRIPTION;
pub use prompt_render::ROW_COMPUTE_EXEMPLAR_THRESHOLD;
pub use prompt_render::TSV_TEACHING_TABLE_HEADER;
pub use query_defaults::{
    apply_required_selection_defaults, apply_required_selection_defaults_in_expr,
};
pub use query_resolve::{
    normalize_expr_query_capabilities, normalize_expr_query_capabilities_federated,
    required_scope_param_names, resolve_query_capability, sole_nullary_singleton_get,
    sole_nullary_singleton_get_for_bare_query, QueryCapabilityResolveError,
};
pub use resolved_identity::{IdentityProjectionCtx, ResolvedIdentity};
pub use row_composition::{
    resolve_relation_target_id, row_identity_from_parts, row_identity_from_ref, IdEncoding,
    PreflightToken, ResolutionHint, RowIdentity, RowProvenance, RowState, RowSuffix,
};
pub use row_membership::{
    parse_closed_rowset_ref, parse_membership_clause, split_where_and_clauses, MembershipRhs,
    RowMembership,
};
pub use row_plan::{
    fold_compute_ops, parse_with_body, CatalogFilter, CollectCardinality, CollectReason,
    CollectRows, CollectedFrame, ColumnName, CompileRowPlan, EnginePlanId, FrameId, FrameShape,
    IngestBatch, IngestRows, LogicalColumn, LogicalColumnType, PlanNode, PlasmFrameSchema,
    ProjectSpec, RowComputeEngine, RowComputeError, RowFilter, RowPlan, ScanError, ScanSource,
    TypedAggregate,
};
pub use row_predicate::{
    entity_def_for_row_predicate, parse_row_predicate_list, row_predicate_from_expr,
    type_check_row_predicate, RowComparison, RowPredicate, RowPredicateTypeCtx,
};
pub use row_union::union_rowsets;
pub use rowset::{
    normalize_query_expr_to_rowset, BackendSelection, BackendSelectionBinding, InvocationControls,
    ParentScope, ResolvedRowset, RowSource, RowTerminal, RowTransform,
};
pub use teaching_term::{
    method_ref_for_capability, method_ref_for_domain_segment, resolve_parameter_slot, EntityRef,
    MethodRef, ParameterSlot, Symbol, TeachingTerm,
};
pub use wire_coercion::{
    apply_identity_slots_to_row, binding_value_as_plasm_value, coerce_json_value_for_field_type,
    coerce_value_for_field_type, coerce_value_for_field_type_with_policy,
    collect_relation_binding_proofs, compare_unify_json_ordered_numbers,
    decode_coerce_and_validate_field, decode_coerce_money_fields, dry_stub_entity_row_json,
    dry_stub_json_for_named_value, dry_stub_value_for_named_value,
    field_type_assignable_for_relation_binding, identity_slot_to_json, json_value_to_plasm_value,
    parent_entity_field_type, plasm_value_to_json, relation_binding_assignable,
    restore_id_field_from_compound_ref, try_plasm_value_to_json, value_compatible_with_field_type,
    DecodeFieldDiagnostic, RelationBindingProof,
};
pub mod relation_materialize;
pub mod view_embed_proof;
pub use capability_input::validate_capability_invocation_input;
pub use expr_surface_render::{
    render_expr_surface, render_expr_surface_federated, wire_surface_from_teaching_line,
    wire_surface_from_teaching_session_line,
};
pub use money::{
    json_amount_to_value, CrossCurrencyError, MoneyDecodeSpec, MoneyError, MoneyValue,
    MoneyWireFormat,
};
pub use program_string_template::{
    contains_dollar_interpolation, contains_minijinja_markers,
    find_dollar_interpolation_in_minijinja_body, flatten_row_fields_into_ctx,
    for_each_interpolation_path, interpolation_paths, interpolation_roots,
    is_minijinja_template_builtin, is_shared_minijinja_filter, register_shared_minijinja_filters,
    reject_dollar_interpolation, render_minijinja, render_program_string,
    shared_minijinja_environment, template_binding_mj_value, unified_template_context,
    validate_interpolation_syntax, CompiledProgramString, ProgramStringError,
    DEFAULT_MAX_INTERPOLATED_LEN, MINIJINJA_TEMPLATE_BUILTINS, SHARED_MINIJINJA_FILTERS,
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
pub use relation_validation_expr::relation_validation_expr;
pub use schema::{
    capability_is_zero_arity_action, capability_is_zero_arity_invoke,
    capability_mapping_is_view_transport, capability_method_label_kebab,
    capability_template_all_var_names, flow_control_param_names, is_flow_control_param_name,
    AgentPresentation, ArrayItemsSchema, AttachmentMediaKind, AuthScheme, BackendSelectionSchema,
    CapabilityInputs, CapabilityKind, CapabilityManifest, CapabilityMapping, CapabilityReceiver,
    CapabilitySchema, CapabilityTemplateJson, Cardinality, CgsCapabilityIndex, CrossFieldRule,
    CrossFieldRuleType, DataClassDimension, DataClassName, DataClassSchema, DataClassSeverity,
    DiscoveryCapabilityHints, DiscoveryEntityHints, DiscoveryRelationHints, EmbedOnMissPolicy,
    EntityDef, FieldDeriveRule, FieldSchema, FieldValueKind, IdFormat, InputFieldSchema,
    InputFieldWire, InputSchema, InputType, InputValidation, InputVariantSchema,
    InvocationControlsSchema, JsonPathSegment, NamedValueSchema, OauthDefaultScopeSet,
    OauthExtension, OauthRequirements, OauthScopeEntry, OutputSchema, OutputType,
    ParentScopeSchema, RelationMaterialization, RelationSchema, RelationScopedFallback,
    ResourceSchema, ScopeAggregateKeyPolicy, ScopeRequirement, SinkClassName, ValueDomainKey,
    ValueDomainSlot, ViewDefinition, ViewNodeSpec, ViewOutputBinding, ViewParamBinding,
    ViewRelationBinding, ViewRelationOutputSpec, ViewScopeInject, ViewScopeParam, ViewTraversal,
    WireVariantDiscriminator, CGS, DEFAULT_HTTP_BACKEND,
};
pub use schema_overlay::{
    build_decode_scope_key, build_schema_overlay, overlay_bind_cache_suffix, overlay_collect_rows,
    overlay_entity_for_scope, overlay_merge_step_response, overlay_pipeline_cache_suffix,
    resolve_overlay_row_bind, walk_json_path, SchemaOverlay, SchemaOverlaySpec,
};
pub use scope_entity_ref_infer::{
    effective_capability_input, prepare_create_capability_input, prepare_targeted_capability_input,
    should_omit_invoke_teaching_arg,
};
pub use scope_entity_ref_splat::apply_entity_ref_scope_splat;
pub use step_semantics::*;
pub use string_unescape::normalize_structured_string_inputs;
pub use summary_render::{
    expr_simulation_bindings, render_intent, render_intent_federated,
    render_intent_with_projection, render_intent_with_projection_federated, render_outcome,
};
pub use symbol_tuning::{
    catalog_cgs_hashes_from_session, catalog_pins_match, entity_slices_for_render,
    first_opaque_m_sym_in_expr, method_syms_in_expr, relation_endpoint_keys,
    resolve_prompt_surface_entities, strip_prompt_expression_annotations,
    symbol_map_cache_key_federated, symbol_map_cache_key_single_catalog,
    symbol_map_fingerprint_hex, symbol_map_for_prompt, wire_surface_for_parse,
    wire_surface_for_teaching_session, CatalogScope, ExposedEntitySymbolRow,
    ExposedRelationSymbolRow, ExposureEntityKey, FocusSpec, PersistedSymbolLedger,
    PersistedSymbolLedgerDecodeError, PersistedSymbolLedgerEncodeError, PersistedSymbolLedgerState,
    SymbolAllocate, SymbolMap, SymbolMapCacheKey, SymbolMapCrossRequestCache, SymbolRender,
    SymbolResolve, SymbolResolveError, SymbolSession, TeachingExposureSession,
    PERSISTED_SYMBOL_LEDGER_VERSION,
};
pub use template_ref::{RefKind, TemplateRefContext};
pub use temporal::{
    normalize_temporal_value, parse_temporal_now_env, rewrite_temporal_aliases,
    rewrite_temporal_aliases_in_predicate_body, temporal_predicate_alias_hint,
    temporal_reference_now, temporal_wire_format_from_name, wire_temporal_value,
};
pub use type_checker::{
    reject_domain_placeholder_in_executable, type_check_chain, type_check_create,
    type_check_delete, type_check_expr, type_check_expr_federated, type_check_get,
    type_check_invoke, type_check_predicate, type_check_query,
};
pub use typed_invoke::{InvokeInputPayload, TypedInvokeInput};
pub use typed_literal::{TypedComparisonValue, TypedLiteral, TypedLiteralError};
pub use typed_row::TypedFieldValue;
pub use value::{
    CompOp, FieldType, GetScalarExtract, PlasmInputRef, TemporalWireFormat, Value,
    ValueTableCellBudget, ValueWireFormat, PLASM_ATTACHMENT_KEY,
};
pub use value_domain::{
    compile_pattern, parse_type_name, validate_constraints_on_number,
    validate_constraints_on_string, validate_number_constraints, validate_string_constraints,
    validate_string_profile, Constraints, EnumMembership, KernelKind, ProfileId, ValueDomain,
    ENUM_GLOSS_FORBIDDEN_CHARS,
};
pub use view_embed_proof::ValidatedViewEmbedProof;
pub use workflow_identity::{
    conflict_rules_from_mapping_template, match_conflict_rule, ConflictRule, ConflictRuleExtract,
    ConflictRuleWhen, ReconcileBindSource, ReconcileSpec, ViewNodeCondition, ViewNodeWhen,
    WorkflowConflict, WorkflowConflictKind, WriteOutcome,
};

pub mod operand_binding;

pub mod boolean_expr;
pub use boolean_expr::BooleanExpr;

pub mod boolean_surface;
pub use boolean_surface::parse_boolean_filter;
