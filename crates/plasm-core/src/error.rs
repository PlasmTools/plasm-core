use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ValueDomainViolation {
    #[error("value-domain type is missing")]
    MissingType,
    #[error("value-domain type `{name}` is retired; use {replacement}")]
    RetiredType {
        name: &'static str,
        replacement: &'static str,
    },
    #[error("unknown value-domain type `{name}`")]
    UnknownType { name: String },
    #[error("entity_ref value domain requires a target")]
    MissingEntityRefTarget,
    #[error("enum membership requires at least one token")]
    EmptyEnumMembership,
    #[error("enum gloss for token `{token}` contains reserved delimiter `{delimiter}`")]
    ForbiddenEnumGlossDelimiter { token: String, delimiter: char },
    #[error("pattern exceeds the {max_bytes}-byte limit (found {actual_bytes} bytes)")]
    PatternTooLong {
        actual_bytes: usize,
        max_bytes: usize,
    },
    #[error("value violates profile {0:?}")]
    InvalidProfile(crate::ProfileId),
    #[error("value is shorter than minimum length {0}")]
    BelowMinLength(usize),
    #[error("value exceeds maximum length {0}")]
    AboveMaxLength(usize),
    #[error("value does not match the declared pattern")]
    PatternMismatch,
    #[error("declared pattern could not be evaluated")]
    PatternConfiguration,
    #[error("enum membership is missing")]
    MissingEnumMembership,
    #[error("value is not an allowed enum member")]
    UnknownEnumMember,
    #[error("value is below the minimum constraint")]
    BelowMinimum,
    #[error("value is above the maximum constraint")]
    AboveMaximum,
    #[error("value does not satisfy the exclusive minimum constraint")]
    ExclusiveMinimum,
    #[error("value does not satisfy the exclusive maximum constraint")]
    ExclusiveMaximum,
    #[error("multiple_of must be non-zero")]
    ZeroMultiple,
    #[error("value is not a multiple of the declared constraint")]
    NotMultiple,
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum TypeError {
    #[error("field `{field}` value coercion failed: {source}")]
    CoercionFailure {
        field: String,
        #[source]
        source: crate::wire_coercion::CoercionError,
    },
    #[error("field `{field}` cannot be coerced to entity reference for `{target}`: {source}")]
    EntityRefCoercionFailure {
        field: String,
        target: String,
        #[source]
        source: crate::wire_coercion::CoercionError,
    },
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

    #[error("Value type '{value_type}' violates the declared value domain for field '{field}': {violation}")]
    ValueDomainViolation {
        field: String,
        value_type: String,
        #[source]
        violation: ValueDomainViolation,
    },

    /// The model echoed the teaching placeholder `$` literally instead of substituting a real value.
    #[error("Literal `$` is prompt-only teaching syntax for field '{field}'; replace with a real value ({expected_type})")]
    DomainPlaceholderLiteral {
        field: String,
        expected_type: String,
        description: Option<String>,
    },

    #[error("Cannot compare money in '{left}' with money in '{right}'")]
    CrossCurrencyCompare { left: String, right: String },

    #[error("Relation '{relation}' not found in entity '{entity}'")]
    RelationNotFound { relation: String, entity: String },

    #[error("Entity '{entity}' not found in schema")]
    EntityNotFound { entity: String },

    #[error("Get on entity '{entity}': {source}")]
    RefKeyMismatch {
        entity: String,
        #[source]
        source: ReferenceContractError,
    },

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

    #[error("Capability '{capability}' is not a Get on '{entity}'")]
    GetCapabilityMismatch { capability: String, entity: String },

    #[error("Input required for capability '{capability}' but not provided")]
    InputRequired { capability: String },

    /// Agent omitted a required CGS parameter that has no authored default (RA-15).
    #[error("required parameter `{parameter}` was omitted from `{expression}`")]
    RequiredParameterOmitted {
        parameter: String,
        expression: String,
    },

    #[error("Recursive type error in relation '{relation}': {source}")]
    RecursiveError {
        relation: String,
        #[source]
        source: Box<TypeError>,
    },

    /// Surface query failed lane-typed [`crate::ResolvedRowset`] normalization (RA-1).
    #[error("rowset normalize: {source}")]
    RowsetNormalize {
        #[source]
        source: Box<crate::rowset::RowsetNormalizeError>,
    },
}

impl TypeError {
    /// Python Program correction owned by the rejected type-error variant.
    /// Presentation layers must carry this text unchanged; they do not infer a
    /// remedy from a broad error category or from the rendered error string.
    pub fn python_correction(&self) -> String {
        match self {
            Self::FieldNotFound { field, entity } => format!(
                "Field {field:?} is not declared on {entity:?}. Use a field declared on that row's entity."
            ),
            Self::RelationNotFound { relation, entity } => format!(
                "Relation {relation:?} is not declared on {entity:?}. Use a relation declared on that entity."
            ),
            Self::EntityNotFound { entity } => format!(
                "Entity {entity:?} is not in the current catalog. Use an exposed entity binding."
            ),
            Self::CapabilityNotFound { capability } => format!(
                "Capability {capability:?} is not declared in the current catalog. Use a declared method on its entity."
            ),
            Self::GetCapabilityMismatch { capability, entity } => format!(
                "Use a declared Get method on {entity:?}; {capability:?} does not read that entity."
            ),
            Self::RequiredParameterOmitted { parameter, .. } => format!(
                "Supply the required keyword {parameter}=value on the declared Python method."
            ),
            Self::InputRequired { capability } => format!(
                "Capability {capability:?} requires input. Supply its declared required arguments."
            ),
            Self::RefKeyMismatch { source, .. } => format!(
                "Get identity mismatch: {source}. Follow the declared get identity signature."
            ),
            Self::DomainPlaceholderLiteral {
                field,
                expected_type,
                ..
            } => format!(
                "Replace the teaching placeholder for {field:?} with an actual {expected_type} value."
            ),
            Self::RecursiveError { relation, source } => {
                format!("Relation {relation:?}: {}", source.python_correction())
            }
            Self::ChainTargetMissingGet { target_entity, .. } => format!(
                "The relation target {target_entity:?} has no Get capability. Use a declared materialized relation or source."
            ),
            Self::CoercionFailure { .. } | Self::EntityRefCoercionFailure { .. } => {
                self.to_string()
            }
            Self::IncompatibleOperator { .. }
            | Self::IncompatibleValue { .. }
            | Self::ValueDomainViolation { .. }
            | Self::CrossCurrencyCompare { .. } => self.to_string(),
            Self::RowsetNormalize { source } => format!("Source input contract: {source}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReferenceContractError {
    #[error("compound key {keys:?} required; use named form Entity(key=value, ...)")]
    CompoundKeyRequired { keys: Vec<String> },
    #[error("simple id form expected for this entity")]
    SimpleKeyRequired,
    #[error("expected compound identity keys {expected:?}, got {actual:?}")]
    CompoundKeysMismatch {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    #[error("operation requires a `{entity}` receiver")]
    ReceiverRequired { entity: String },
    #[error("operation has no entity receiver; supply its declared inputs")]
    UnexpectedReceiver,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryAliasError {
    #[error("must be non-empty")]
    Empty,
    #[error("duplicate alias")]
    Duplicate,
    #[error("must not duplicate canonical entry_id")]
    DuplicatesCanonicalEntry,
}

#[derive(Debug, Clone, Error)]
pub enum ViewMappingError {
    #[error("expected `transport: view` in mappings for this capability")]
    TransportRequired,
    #[error("expected `view: {view}` to match views map key")]
    ViewKeyMismatch { view: String },
    #[error("traversal node `{node}` cannot also declare capability, bind, or when")]
    ConflictingTraversal { node: String },
    #[error("private binding `{binding}` shadows a scope or output field")]
    PrivateBindingShadow { binding: String },
    #[error("output field `{field}`: computed template must be non-empty")]
    ComputedTemplateEmpty { field: String },
    #[error("identity union needs at least one node and a many relation")]
    InvalidIdentityUnion,
    #[error("transport `view` requires string `view` key")]
    ViewKeyMissing,
    #[error("unknown views key `{view}`")]
    UnknownView { view: String },
    #[error("capability domain `{actual}` must match view entity `{expected}`")]
    DomainMismatch { actual: String, expected: String },
}

#[derive(Debug, Clone, Error)]
pub enum SchemaConstraintError {
    #[error("capability '{capability}': invalid input rule: {reason}")]
    InputRule { capability: String, reason: String },

    #[error("Get capability '{capability}' invocation input '{parameter}' collides with a reserved identity keyword")]
    GetInputIdentityCollision {
        capability: String,
        parameter: String,
    },
    #[error("capability '{capability}': query/search inputs may only use scope, selection, and controls")]
    QueryInputLane { capability: String },
    #[error("capability '{capability}': selection is only valid on query/search capabilities")]
    SelectionOnNonQuery { capability: String },
    #[error("capability '{capability}': {lane} contains an empty input name")]
    EmptyInputName {
        capability: String,
        lane: &'static str,
    },
    #[error("capability '{capability}': input '{input}' appears in both {previous} and {lane}; capability input lanes must be disjoint")]
    DuplicateInput {
        capability: String,
        input: String,
        previous: &'static str,
        lane: &'static str,
    },
    #[error("capability '{capability}': {lane} input '{input}' cannot declare selection_effect")]
    SelectionEffectForbidden {
        capability: String,
        input: String,
        lane: &'static str,
    },
    #[error("capability '{capability}': selection input '{input}' requires selection_effect")]
    SelectionEffectMissing { capability: String, input: String },
    #[error("entity '{entity}' field '{field}': `currency_field` is only valid on money fields")]
    CurrencyOnNonMoney { entity: String, field: String },
}

#[derive(Debug, Clone, Error)]
pub enum PipelineSegmentError {
    #[error("duplicate zero-arity pipeline method label on capabilities '{previous}' and '{capability}'")]
    DuplicateMethod {
        previous: String,
        capability: String,
    },
    #[error("zero-arity pipeline method '{capability}' collides with a relation")]
    MethodRelationCollision { capability: String },
    #[error("zero-arity pipeline method '{capability}' collides with field '{field}'")]
    MethodFieldCollision { capability: String, field: String },
    #[error("relation has the same name as EntityRef field '{field}'")]
    RelationReferenceCollision { field: String },
}

#[derive(Debug, Clone, Error)]
pub enum RegistryWireMismatch {
    #[error("array items field_type {actual:?} vs values {expected:?}")]
    FieldType {
        actual: crate::FieldType,
        expected: crate::FieldType,
    },
    #[error("array items value_format vs values mismatch")]
    ValueFormat,
    #[error("array items allowed_values vs values mismatch")]
    AllowedValues,
}

#[derive(Debug, Clone, Error)]
pub enum EntityExpressionError {
    #[error(transparent)]
    Teaching(std::sync::Arc<crate::prompt_render::python::PythonTeachingError>),
    #[error("No declared root capability or relation produces a receiver; add a get, query, search, create or declared entity output.")]
    ReceiverUnavailable,
}

#[derive(Debug, Clone, Error)]
pub enum RelationMaterializeError {
    #[error("no such capability name")]
    UnknownCapability,
    #[error("capability is declared on entity '{actual}' but relation targets '{expected}'")]
    DomainMismatch { actual: String, expected: String },
    #[error("capability kind must be {expected} (got {actual:?})")]
    KindMismatch {
        expected: crate::preflight::PreflightReadKind,
        actual: crate::CapabilityKind,
    },
    #[error("capability has no parent-scope parameters; scoped materialization requires them")]
    ParentScopeMissing,
    #[error("object input does not declare materialize parameter `{param}`")]
    ParamMissing { param: String },
    #[error("param `{param}` uses inline structural input, not a value-domain reference")]
    InlineParam { param: String },
    #[error("param `{param}` references unknown value domain `{value_ref}`")]
    ParamValueMissing { param: String, value_ref: String },
}

impl From<crate::money::CrossCurrencyError> for TypeError {
    fn from(e: crate::money::CrossCurrencyError) -> Self {
        TypeError::CrossCurrencyCompare {
            left: e.left().to_string(),
            right: e.right().to_string(),
        }
    }
}

#[derive(Error, Debug, Clone)]
pub enum SchemaError {
    #[error("invalid prerequisite declaration: {source}")]
    PrerequisiteInvalid {
        #[source]
        source: crate::prerequisites::PrerequisiteError,
    },
    #[error("Duplicate entity name: '{name}'")]
    DuplicateEntity { name: String },

    #[error("registry_aliases entry '{alias}': {source}")]
    RegistryAliasInvalid {
        alias: String,
        #[source]
        source: RegistryAliasError,
    },

    #[error("Capability '{capability}' preflight: {source}")]
    PreflightInvalid {
        capability: String,
        #[source]
        source: crate::preflight::PreflightValidationError,
    },

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

    /// Identity slots (`id_field` / `key_vars`) must be scalar types [`IdentityCodec`] can encode.
    /// `entity_ref` is for foreign keys — never a primary-key / compound-key slot.
    #[error(
        "{entity}.{field} has unsupported identity type {field_type} — use a scalar id type (string, integer, uuid, digit_id, …); entity_ref is only for foreign keys"
    )]
    UnsupportedIdentityType {
        entity: String,
        field: String,
        field_type: String,
    },

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

    #[error(
        "Entity '{entity}' has multiple Get capabilities {capabilities:?} — set primary_read to the canonical Get capability id"
    )]
    AmbiguousPrimaryRead {
        entity: String,
        capabilities: Vec<String>,
    },

    #[error("Entity '{entity}' primary_query '{capability}' is not a defined capability")]
    UnknownPrimaryQueryCapability { entity: String, capability: String },

    #[error(
        "Entity '{entity}' primary_query '{capability}' must target this entity (got domain '{domain}')"
    )]
    PrimaryQueryWrongDomain {
        entity: String,
        capability: String,
        domain: String,
    },

    #[error(
        "Entity '{entity}' primary_query '{capability}' must be a Query capability (got {kind})"
    )]
    PrimaryQueryNotQuery {
        entity: String,
        capability: String,
        kind: String,
    },

    #[error(
        "Entity '{entity}' has {count} Query capabilities {capabilities:?} — at most one kind:query per entity (compress with a selection discriminant + CML path branch, fold scoped lists into one query + relation materialize, or split entities)"
    )]
    TooManyQueryCapabilities {
        entity: String,
        count: usize,
        capabilities: Vec<String>,
    },

    #[error("Entity '{entity}' primary_search '{capability}' is not a defined capability")]
    UnknownPrimarySearchCapability { entity: String, capability: String },

    #[error(
        "Entity '{entity}' primary_search '{capability}' must target this entity (got domain '{domain}')"
    )]
    PrimarySearchWrongDomain {
        entity: String,
        capability: String,
        domain: String,
    },

    #[error(
        "Entity '{entity}' primary_search '{capability}' must be a Search capability (got {kind})"
    )]
    PrimarySearchNotSearch {
        entity: String,
        capability: String,
        kind: String,
    },

    #[error(
        "Entity '{entity}' has {count} Search capabilities {capabilities:?} — at most one kind:search per entity (compress with a selection discriminant + CML path branch, or split entities)"
    )]
    TooManySearchCapabilities {
        entity: String,
        count: usize,
        capabilities: Vec<String>,
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
        "Capability '{capability}': `body: {{ type: var, name: input }}` with a scalar parameter also named `input` — execute_create splats params over env['input']; use an explicit body object with named fields (see plasm-authoring reference — Request body)"
    )]
    BodyVarInputParamCollision { capability: String },

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
    #[error("Entity '{entity}': pipeline segment '{segment}' — {source}")]
    PipelineSegmentConflict {
        entity: String,
        segment: String,
        #[source]
        source: PipelineSegmentError,
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
        "Entity '{entity}' field '{field}': field_type `money` requires `value_format` with `money:` (decimal_string, json_number, or minor_units)"
    )]
    MoneyFieldMissingValueFormat { entity: String, field: String },

    #[error(
        "Entity '{entity}' field '{field}': `value_format` is only allowed for `Date` / `datetime` or `money` fields"
    )]
    ValueFormatOnIncompatibleField { entity: String, field: String },

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
        "Capability '{capability}' parameter '{param}': field_type `money` requires `value_format` with `money:`"
    )]
    MoneyParamMissingValueFormat { capability: String, param: String },

    #[error(
        "Capability '{capability}' parameter '{param}': `value_format` is only allowed for `Date` / `datetime` or `money` parameters"
    )]
    ValueFormatOnIncompatibleParam { capability: String, param: String },

    #[error(
        "Entity '{entity}' field '{field}': `currency_field` '{currency_field}' is not a field on this entity"
    )]
    CurrencyFieldUnknown {
        entity: String,
        field: String,
        currency_field: String,
    },

    #[error(
        "Entity '{entity}' field '{field}': `currency_field` '{currency_field}' must be a string or select field"
    )]
    CurrencyFieldNotString {
        entity: String,
        field: String,
        currency_field: String,
    },

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

    #[error("Unknown data class '{key}' ({context})")]
    UnknownDataClass { key: String, context: String },

    #[error(
        "Input field `{name}` uses inline structural `input_type` — no single `values:` row applies here"
    )]
    InlineStructuralInputField { name: String },

    #[error("{context}: denormalized wire fields disagree with `values['{key}']` — {source}")]
    RegistryDenormalizationMismatch {
        key: String,
        context: String,
        #[source]
        source: RegistryWireMismatch,
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
        "Entity '{entity}' relation '{relation}' has cardinality one — `materialize: from_parent_get`, `get_scoped_bindings`, or omit materialization when refs are populated by decode/views"
    )]
    RelationOneWithDisallowedMaterialize { entity: String, relation: String },

    #[error(
        "Entity '{entity}' relation '{relation}': `materialize.kind: get_scoped_bindings` is only valid for cardinality `one`"
    )]
    RelationGetScopedBindingsRequiresCardinalityOne { entity: String, relation: String },

    #[error(
        "Entity '{entity}' relation '{relation}': materialize references unknown parent field '{field}'"
    )]
    RelationMaterializeUnknownParentField {
        entity: String,
        relation: String,
        field: String,
    },

    #[error("entity `{entity}` relation `{relation}` has an invalid parent field: {source}")]
    RelationParentFieldInvalid {
        entity: String,
        relation: String,
        #[source]
        source: crate::wire_coercion::ParentFieldTypeError,
    },

    #[error("identity field `{entity}.{field}` cannot be resolved: {source}")]
    IdentityFieldTypeResolution {
        entity: String,
        field: String,
        #[source]
        source: crate::wire_coercion::ParentFieldTypeError,
    },

    #[error(
        "Entity '{entity}' relation '{relation}': binding `{cap_param}` ← parent `{parent_field}` is not assignable ({parent_type:?} → {param_type:?})"
    )]
    RelationMaterializeBindingTypeMismatch {
        entity: String,
        relation: String,
        cap_param: String,
        parent_field: String,
        parent_type: String,
        param_type: String,
    },

    #[error("Entity '{entity}' relation '{relation}': query_scoped_bindings must be non-empty")]
    RelationMaterializeEmptyBindings { entity: String, relation: String },

    #[error("from_parent_get embed graph has a cycle: {cycle}")]
    FromParentGetEmbedCycle {
        #[source]
        cycle: crate::relation_materialize::EntityCycle,
    },

    #[error("Entity '{entity}' relation '{relation}': from_parent_get `path` must be non-empty")]
    RelationFromParentGetEmptyPath { entity: String, relation: String },

    #[error(
        "Entity '{entity}' relation '{relation}': from_parent_get wildcard segment must be `wildcard: true`"
    )]
    RelationFromParentGetInvalidWildcard { entity: String, relation: String },

    #[error(
        "Entity '{entity}' relation '{relation}' → '{target}': cardinality `many` requires executable `materialize` (add view_embed, query_scoped, or from_parent_get)"
    )]
    RelationNotExecutable {
        entity: String,
        relation: String,
        target: String,
    },

    #[error("Entity '{entity}' relation '{relation}': view_embed view '{view}' is not defined")]
    RelationViewEmbedUnknownView {
        entity: String,
        relation: String,
        view: String,
    },

    #[error(
        "Entity '{entity}' relation '{relation}': view_embed view '{view}' is declared on entity '{view_entity}', not '{entity}'"
    )]
    RelationViewEmbedEntityMismatch {
        entity: String,
        relation: String,
        view: String,
        view_entity: String,
    },

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
        "Entity '{entity}' relation '{relation}': materialize capability '{capability}' for target '{target}' is invalid: {source}"
    )]
    RelationMaterializeCapabilityInvalid {
        entity: String,
        relation: String,
        target: String,
        capability: String,
        #[source]
        source: RelationMaterializeError,
    },

    /// After structural checks, no type-checked teaching example line could be synthesized.
    #[error("Entity '{entity}' is not expression-complete: {source}")]
    EntityExpressionIncomplete {
        entity: String,
        #[source]
        source: EntityExpressionError,
    },

    /// A capability exists in the CGS but has zero representation in the teaching bundle.
    #[error(
        "Capability '{capability}' (entity '{entity}') has no synthesized teaching line in the teaching bundle — the renderer could not produce a valid expression witness for it"
    )]
    CapabilityNotRepresentedInDomain { capability: String, entity: String },

    /// One or more capabilities were not represented in teaching-line synthesis.
    #[error(
        "Capability coverage incomplete: Python teaching omitted signatures for {uncovered:?}"
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

    #[error(
        "auth.{context}: specify at least one non-empty `env` or `hosted_kv` (both may be set; runtime prefers hosted KV when populated, else env)"
    )]
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

    #[error("View '{view}': capability '{capability}' mapping invalid for views: {source}")]
    ViewCapabilityMappingInvalid {
        view: String,
        capability: String,
        #[source]
        source: ViewMappingError,
    },

    #[error("view `{view}` capability `{capability}` has no mapping: {source}")]
    ViewMappingMissing {
        view: String,
        capability: String,
        #[source]
        source: crate::schema::MissingCapabilityMapping,
    },

    #[error("view `{view}` node cannot be resolved: {source}")]
    ViewNodeResolution {
        view: String,
        #[source]
        source: crate::schema::ViewNodeResolutionError,
    },

    #[error("Derived get '{capability}': {source}")]
    DerivedGetInvalid {
        capability: String,
        #[source]
        source: crate::derived_get::DerivedGetError,
    },

    #[error(
        "View '{view}': node '{node}' capability '{capability}' must be Query, Get, Search, or mutator (got {kind})"
    )]
    ViewUnsupportedNodeCapabilityKind {
        view: String,
        node: String,
        capability: String,
        kind: String,
    },

    #[error("View '{view}': node '{node}' when references unknown prior node '{ref_node}'")]
    ViewNodeWhenUnknownNode {
        view: String,
        node: String,
        ref_node: String,
    },

    #[error(
        "Capability '{capability}' on '{entity}' requires identity_key for non-idempotent mutators"
    )]
    IdentityKeyRequired { capability: String, entity: String },

    #[error("Capability '{capability}': identity_key references unknown param '{param}'")]
    IdentityKeyUnknownParam { capability: String, param: String },

    #[error("Capability '{capability}': idempotent output requires reconcile block")]
    ReconcileRequiredWhenIdempotent { capability: String },

    #[error("Capability '{capability}': reconcile.via '{via}' is not defined")]
    ReconcileUnknownCapability { capability: String, via: String },

    #[error(
        "View '{view}': relation_outputs references unknown relation '{relation}' on entity '{entity}'"
    )]
    ViewRelationOutputUnknownRelation {
        view: String,
        relation: String,
        entity: String,
    },

    #[error(
        "View '{view}': relation_outputs.{relation} target mismatch (CGS relation targets '{expected}', got '{got}')"
    )]
    ViewRelationOutputTargetMismatch {
        view: String,
        relation: String,
        expected: String,
        got: String,
    },

    #[error(
        "View '{view}': relation_outputs.{relation} cardinality mismatch (CGS relation is {expected}, got {got})"
    )]
    ViewRelationOutputCardinalityMismatch {
        view: String,
        relation: String,
        expected: String,
        got: String,
    },

    #[error("View '{view}': node '{node}' bind param '{param}' is not declared on capability '{capability}'")]
    ViewNodeBindUnknownParam {
        view: String,
        node: String,
        capability: String,
        param: String,
    },

    #[error("View '{view}': node '{node}' bind references unknown prior node '{ref_node}'")]
    ViewNodeBindUnknownNode {
        view: String,
        node: String,
        ref_node: String,
    },

    #[error(
        "View '{view}': node '{node}' bind references node '{ref_node}' which is not declared before '{node}'"
    )]
    ViewNodeBindForwardRef {
        view: String,
        node: String,
        ref_node: String,
    },

    #[error("View '{view}': node '{node}' computed bind template must be non-empty")]
    ViewNodeBindEmptyTemplate { view: String, node: String },

    #[error(transparent)]
    SchemaConstraint(#[from] SchemaConstraintError),

    #[error(
        "capability '{capability}' (kind: search): must declare a free-text selection param named query/q/search (or a sole selection slot) for Entity~\"…\""
    )]
    SearchMissingFreeText { capability: String },

    #[error(
        "capability '{capability}' (kind: search): free-text selection param '{param}' must be required: true — optional free-text selectors use kind: query (avoids barren e~\"<query>\"{{query=…}} teaching twins)"
    )]
    SearchOptionalFreeText { capability: String, param: String },

    #[error("schema_overlay: {source}")]
    SchemaOverlayInvalid {
        #[source]
        source: crate::schema_overlay::OverlayValidationError,
    },
}

#[derive(Error, Debug, Clone)]
pub enum NormalizationError {
    #[error("Predicate complexity exceeds maximum depth limit")]
    MaxDepthExceeded,
}
