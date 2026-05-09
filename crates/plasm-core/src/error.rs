use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum TypeError {
    #[error("Field '{field}' not found in entity '{entity}'")]
    FieldNotFound { field: String, entity: String },

    #[error(
        "Operator '{op:?}' not compatible with field type '{field_type:?}' for field '{field}'"
    )]
    IncompatibleOperator {
        field: String,
        op: String,
        field_type: String,
    },

    #[error("Value type '{value_type}' not compatible with field type '{field_type}' for field '{field}'")]
    IncompatibleValue {
        field: String,
        value_type: String,
        field_type: String,
    },

    /// The model echoed the teaching placeholder `$` literally instead of substituting a real value.
    #[error("Literal `$` is prompt-only teaching syntax for field '{field}'; replace with a real value ({expected_type})")]
    DomainPlaceholderLiteral {
        field: String,
        expected_type: String,
        description: Option<String>,
    },

    #[error("Relation '{relation}' not found in entity '{entity}'")]
    RelationNotFound { relation: String, entity: String },

    #[error("Entity '{entity}' not found in schema")]
    EntityNotFound { entity: String },

    #[error("Get on entity '{entity}': {message}")]
    RefKeyMismatch { entity: String, message: String },

    #[error(
        "Chain auto-get requires a Get capability on '{target_entity}' (from {source_entity}.{selector})"
    )]
    ChainTargetMissingGet {
        source_entity: String,
        selector: String,
        target_entity: String,
    },

    #[error("Capability '{capability}' not found in schema")]
    CapabilityNotFound { capability: String },

    #[error("Input required for capability '{capability}' but not provided")]
    InputRequired { capability: String },

    #[error("Recursive type error in relation '{relation}': {source}")]
    RecursiveError {
        relation: String,
        #[source]
        source: Box<TypeError>,
    },
}

#[derive(Error, Debug, Clone)]
pub enum SchemaError {
    #[error("Duplicate entity name: '{name}'")]
    DuplicateEntity { name: String },

    #[error("Duplicate field name '{field}' in entity '{entity}'")]
    DuplicateField { entity: String, field: String },

    #[error("Duplicate relation name '{relation}' in entity '{entity}'")]
    DuplicateRelation { entity: String, relation: String },

    #[error(
        "Entity '{entity}' references unknown target entity '{target}' in relation '{relation}'"
    )]
    UnknownTargetEntity {
        entity: String,
        relation: String,
        target: String,
    },

    #[error("ID field '{id_field}' not found in entity '{entity}'")]
    MissingIdField { entity: String, id_field: String },

    #[error("Entity '{entity}' key_vars references unknown field '{field}'")]
    UnknownKeyVarField { entity: String, field: String },

    #[error("Entity '{entity}' primary_read '{capability}' is not a defined capability")]
    UnknownPrimaryReadCapability { entity: String, capability: String },

    #[error(
        "Entity '{entity}' primary_read '{capability}' must target this entity (got domain '{domain}')"
    )]
    PrimaryReadWrongDomain {
        entity: String,
        capability: String,
        domain: String,
    },

    #[error("Entity '{entity}' primary_read '{capability}' must be a Get capability (got {kind})")]
    PrimaryReadNotGet {
        entity: String,
        capability: String,
        kind: String,
    },

    #[error("EntityRef target '{target}' is not a defined entity ({context})")]
    EntityRefUnknownTarget { target: String, context: String },

    #[error(
        "Capability '{capability}' parameter '{parameter}' has EntityRef({param_target}) but entity field has EntityRef({field_target})"
    )]
    EntityRefNameMismatch {
        capability: String,
        parameter: String,
        param_target: String,
        field_target: String,
    },

    #[error(
        "Entity '{entity}' has multiple unscoped {kind} capabilities: {capabilities:?}. At most one unscoped (primary) capability per kind is allowed; add role: scope to the parent-FK parameter on sub-resource capabilities."
    )]
    DuplicateCapability {
        entity: String,
        kind: String,
        capabilities: Vec<String>,
    },

    /// Relation names, field names, and zero-arity pipeline method labels must be disjoint per entity
    /// so `.segment` without `()` resolves unambiguously.
    #[error("Entity '{entity}': pipeline segment '{segment}' — {message}")]
    PipelineSegmentConflict {
        entity: String,
        segment: String,
        message: String,
    },

    #[error("Expression alias '{alias}' is claimed by entity '{owner}' and entity '{other}'")]
    DuplicateExpressionAlias {
        alias: String,
        owner: String,
        other: String,
    },

    #[error("Entity '{entity}' expression alias '{alias}' is the same name as another entity")]
    ExpressionAliasShadowsEntity { entity: String, alias: String },

    #[error(
        "Entity '{entity}' field '{field}': field_type `Date` requires `value_format` (rfc3339, unix_ms, unix_sec, iso8601_date)"
    )]
    DateFieldMissingValueFormat { entity: String, field: String },

    #[error(
        "Entity '{entity}' field '{field}': `value_format` is only allowed for `Date` / `datetime` fields"
    )]
    ValueFormatOnNonDateField { entity: String, field: String },

    #[error(
        "Entity '{entity}' field '{field}': `string_semantics` is only allowed for `string` fields"
    )]
    StringSemanticsOnNonString { entity: String, field: String },

    #[error(
        "Capability '{capability}' parameter '{param}': `string_semantics` is only allowed for `string` parameters"
    )]
    StringSemanticsOnNonStringParam { capability: String, param: String },

    #[error(
        "Entity '{entity}' field '{field}': `agent_presentation` is only allowed for `string` or `blob` fields"
    )]
    AgentPresentationOnNonString { entity: String, field: String },

    #[error(
        "Entity '{entity}' field '{field}': `attachment_media` is only allowed when field_type is `blob`"
    )]
    AttachmentMediaOnNonBlob { entity: String, field: String },

    #[error(
        "Capability '{capability}' parameter '{param}': field_type `Date` requires `value_format`"
    )]
    DateParamMissingValueFormat { capability: String, param: String },

    #[error(
        "Capability '{capability}' parameter '{param}': `value_format` is only allowed for `Date` / `datetime` parameters"
    )]
    ValueFormatOnNonDateParam { capability: String, param: String },

    #[error(
        "Entity '{entity}' field '{field}': field_type `array` requires non-empty `items:` describing element types"
    )]
    ArrayFieldMissingItems { entity: String, field: String },

    #[error(
        "Capability '{capability}' parameter '{param}': type `array` requires non-empty `items:` describing element types"
    )]
    ArrayParamMissingItems { capability: String, param: String },

    #[error(
        "Entity '{entity}' field '{field}': field_type `multi_select` requires non-empty `allowed_values`"
    )]
    MultiSelectFieldMissingAllowedValues { entity: String, field: String },

    #[error(
        "Entity '{entity}' field '{field}': `select` / `multi_select` must use `value_ref` to a `values:` entry (inline closed sets are not allowed)"
    )]
    ClosedFieldRequiresValueDomain { entity: String, field: String },

    #[error(
        "Capability '{capability}' parameter '{param}': `select` / `multi_select` must use `value_ref` to a `values:` entry (inline closed sets are not allowed)"
    )]
    ClosedParamRequiresValueDomain { capability: String, param: String },

    #[error("Unknown `value_ref` / value domain key '{key}' ({context})")]
    UnknownValueDomain { key: String, context: String },

    #[error(
        "Input field `{name}` uses inline structural `input_type` — no single `values:` row applies here"
    )]
    InlineStructuralInputField { name: String },

    #[error("{context}: denormalized wire fields disagree with `values['{key}']` — {detail}")]
    RegistryDenormalizationMismatch {
        key: String,
        context: String,
        detail: String,
    },

    #[error(
        "Entity '{entity}' field '{field}': `value_ref` is only valid for `select` / `multi_select` fields"
    )]
    ValueDomainOnNonClosedField { entity: String, field: String },

    #[error(
        "Capability '{capability}' parameter '{param}': `value_ref` is only valid for `select` / `multi_select` parameters"
    )]
    ValueDomainOnNonClosedParam { capability: String, param: String },

    #[error(
        "Capability '{capability}' parameter '{param}': type `multi_select` requires non-empty `allowed_values`"
    )]
    MultiSelectParamMissingAllowedValues { capability: String, param: String },

    #[error(
        "Capability '{capability}' (entity '{entity}') is `kind: action` but has no modeled response: add non-empty `provides:` and/or `output:` with `type: side_effect` and a non-empty `description:` of what the operation changes, or model read-only HTTP as `get` + an entity"
    )]
    ActionUntypedResponse { capability: String, entity: String },

    #[error(
        "Capability '{capability}': `output.type: side_effect` requires non-empty `description:` (state what changes in the domain)"
    )]
    SideEffectMissingDescription { capability: String },

    /// Declared in `entities` but no capability lists this entity as `domain`.
    #[error(
        "Entity '{entity}' has no capabilities — every entity must be the `domain` of at least one capability"
    )]
    EntityWithoutCapability { entity: String },

    /// Required `role: scope` parameter uses a type the expression surface cannot encode into examples.
    #[error(
        "Capability '{capability}' parameter '{parameter}': required scope field type is not supported for typed queries (use entity_ref, string, integer, number, boolean, select/multiselect with allowed_values, or date with a temporal value_format)"
    )]
    ScopeParameterNotEncodable {
        capability: String,
        parameter: String,
    },

    #[error(
        "Entity '{entity}' relation '{relation}' has cardinality one — only `materialize: from_parent_get` is allowed when the child is not a top-level `{relation}.name` ref (query-scoped materialization is for many-relations)"
    )]
    RelationOneWithDisallowedMaterialize { entity: String, relation: String },

    #[error(
        "Entity '{entity}' relation '{relation}': materialize references unknown parent field '{field}'"
    )]
    RelationMaterializeUnknownParentField {
        entity: String,
        relation: String,
        field: String,
    },

    #[error("Entity '{entity}' relation '{relation}': query_scoped_bindings must be non-empty")]
    RelationMaterializeEmptyBindings { entity: String, relation: String },

    #[error("Entity '{entity}' relation '{relation}': from_parent_get `path` must be non-empty")]
    RelationFromParentGetEmptyPath { entity: String, relation: String },

    #[error(
        "Entity '{entity}' relation '{relation}': from_parent_get wildcard segment must be `wildcard: true`"
    )]
    RelationFromParentGetInvalidWildcard { entity: String, relation: String },

    #[error(
        "Entity '{entity}' relation '{relation}': no query/search capability on '{target}' declares parameters {params:?}"
    )]
    RelationMaterializeNoMatchingCapability {
        entity: String,
        relation: String,
        target: String,
        params: Vec<String>,
    },

    #[error(
        "Entity '{entity}' relation '{relation}': materialize capability '{capability}' for target '{target}' is invalid: {detail}"
    )]
    RelationMaterializeCapabilityInvalid {
        entity: String,
        relation: String,
        target: String,
        capability: String,
        detail: String,
    },

    /// After structural checks, no type-checked teaching example line could be synthesized.
    #[error("Entity '{entity}' is not expression-complete: {detail}")]
    EntityExpressionIncomplete { entity: String, detail: String },

    /// A capability exists in the CGS but has zero representation in the teaching bundle.
    #[error(
        "Capability '{capability}' (entity '{entity}') has no synthesized teaching line in the teaching bundle — the renderer could not produce a valid expression witness for it"
    )]
    CapabilityNotRepresentedInDomain { capability: String, entity: String },

    /// One or more capabilities were not represented in teaching-line synthesis.
    #[error(
        "Capability coverage incomplete: teaching-line synthesis omitted examples for {uncovered:?}"
    )]
    CapabilityCoverageIncomplete { uncovered: Vec<(String, String)> },

    #[error("oauth.provider must be non-empty")]
    OauthProviderEmpty,

    #[error("oauth scope requirement at '{context}' is empty (need any_of or all_of)")]
    OauthRequirementEmpty { context: String },

    #[error(
        "oauth scope requirement at '{context}' must not mix any_of and all_of at the same level"
    )]
    OauthRequirementMixed { context: String },

    #[error("oauth requirements reference unknown capability '{capability}'")]
    OauthUnknownCapability { capability: String },

    #[error(
        "oauth requirements reference unknown relation '{key}' (entity '{entity}', relation '{relation}')"
    )]
    OauthUnknownRelation {
        key: String,
        entity: String,
        relation: String,
    },

    #[error(
        "oauth requirement at '{context}' references scope '{scope}' not declared under oauth.scopes"
    )]
    OauthUnknownScope { context: String, scope: String },

    #[error("auth.{context}: specify exactly one of `env` or `hosted_kv` (non-empty)")]
    AuthCredentialSourceInvalid { context: String },

    #[error("auth.oauth2_client_credentials: `hosted_kv` keys must start with `plasm:outbound:`")]
    AuthHostedKvKeyPrefix { field: String },

    #[error("auth.oauth2_client_credentials: token_url must be non-empty")]
    AuthOauth2TokenUrlEmpty,

    #[error("auth.scheme `none` (no outbound credentials) cannot be combined with an `oauth:` extension")]
    AuthNoneIncompatibleWithOauthExtension,

    #[error("View '{view}': unknown capability '{capability}' in node '{node}'")]
    ViewUnknownNodeCapability {
        view: String,
        node: String,
        capability: String,
    },

    #[error("View '{view}': duplicate node id '{node}'")]
    ViewDuplicateNodeId { view: String, node: String },

    #[error("View '{view}': output field '{field}' references unknown node '{node}'")]
    ViewOutputUnknownNode {
        view: String,
        field: String,
        node: String,
    },

    #[error(
        "View '{view}': capability '{capability}' must target entity '{entity}' (declared view entity)"
    )]
    ViewCapabilityEntityMismatch {
        view: String,
        capability: String,
        entity: String,
    },

    #[error("View '{view}': unknown output entity '{entity}'")]
    ViewUnknownEntity { view: String, entity: String },

    #[error("View '{view}': output field '{field}' is not declared on entity '{entity}'")]
    ViewUnknownOutputField {
        view: String,
        field: String,
        entity: String,
    },

    #[error("View '{view}': declares capability '{capability}' but it is not defined")]
    ViewCapabilityMissing { view: String, capability: String },

    #[error("View '{view}': capability '{capability}' mapping invalid for views: {detail}")]
    ViewCapabilityMappingInvalid {
        view: String,
        capability: String,
        detail: String,
    },

    #[error(
        "View '{view}': node '{node}' capability '{capability}' must be Query or Get (got {kind})"
    )]
    ViewUnsupportedNodeCapabilityKind {
        view: String,
        node: String,
        capability: String,
        kind: String,
    },
}

#[derive(Error, Debug, Clone)]
pub enum NormalizationError {
    #[error("Predicate complexity exceeds maximum depth limit")]
    MaxDepthExceeded,

    #[error("Internal normalization error: {message}")]
    InternalError { message: String },
}
