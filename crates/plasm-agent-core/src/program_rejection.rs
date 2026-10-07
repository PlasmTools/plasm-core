//! Source-owned Python compiler rejections carried intact to program diagnostics.

pub use monty_analysis::AnalysisError;

/// Semantic failures recognized while preparing the restricted Python compute contract.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PythonComputeError {
    #[error(transparent)]
    Inference(#[from] Box<crate::python_compute::inference::InferenceError>),
    #[error("compute source budget exceeded")]
    SourceBudgetExceeded,
    #[error("expected one compute function")]
    FunctionCount,
    #[error("expected a synchronous @compute function")]
    FunctionShape,
    #[error("compute dependency packet must be a singleton")]
    DependencyPacketNotSingleton,
    #[error("compute expects one required positional input")]
    InputParameterShape,
    #[error("compute annotation does not match source entity")]
    AnnotationSourceMismatch,
    #[error("synthetic type summary differs from recursive contract")]
    SyntheticContractMismatch,
    #[error("missing compute output contract")]
    OutputContractMissing,
    #[error("Python compute input mode differs from its source")]
    InputModeMismatch,
    #[error("Python compute output type differs from checked return contract")]
    OutputTypeMismatch,
    #[error("Python compute output schema differs from declared return contract")]
    OutputSchemaMismatch,
    #[error("per-row Python rendering must declare a rowset result")]
    PerRowRenderRequiresRowset,
    #[error("Python reduction must declare a singleton result")]
    ReductionRequiresSingleton,
    #[error("Python input schema differs from its source")]
    InputSchemaMismatch,
    #[error("Python compute projection must preserve consumed catalog field `{field}`")]
    ProjectionDropsConsumedField { field: String },
    #[error("Python compute requires typed entity rows")]
    TypedEntityRowsRequired,
    #[error("Python compute source catalog/entity ownership mismatch")]
    SourceOwnershipMismatch,
    #[error("Python input type nesting exceeds the maximum depth of {max_depth}")]
    InputTypeTooDeep { max_depth: usize },
    #[error("expected Python compute")]
    ExpectedPythonCompute,
    #[error("unsupported Python compute contract version")]
    UnsupportedContractVersion,
    #[error("Python compute catalog `{entry_id}` is not loaded")]
    CatalogNotLoaded { entry_id: String },
    #[error("Python compute catalog pin mismatch for `{entry_id}`")]
    CatalogPinMismatch { entry_id: String },
    #[error("Python compute requires session symbols")]
    SessionSymbolsMissing,
    #[error("Python compute annotated entity does not match declared owner")]
    DeclaredOwnerMismatch,
    #[error("Python compute input mode differs from its stored source")]
    StoredInputModeMismatch,
    #[error("Python output type differs from checked return contract")]
    StoredOutputTypeMismatch,
    #[error("Python input schema differs from its structural source")]
    StructuralInputSchemaMismatch,
    #[error("Python input field `{field}` differs from its derived type")]
    DerivedInputTypeMismatch { field: String },
    #[error("structural compute requires an explicit input schema")]
    ExplicitInputSchemaRequired,
    #[error("Python compute source `{source_id}` is absent")]
    SourceNodeMissing { source_id: String },
    #[error("Python compute source node is absent")]
    SourceNodeAbsent,
    #[error("Python compute input projection omits required value field `{field}`")]
    ProjectionOmitsRequiredValue { field: String },
    #[error("Python compute requires catalog ownership")]
    CatalogOwnershipRequired,
    #[error("Python compute requires rows preserving the annotated catalog type")]
    CatalogRowsRequired,
    #[error("Python compute fanout projection omits required value field `{field}`")]
    FanoutProjectionOmitsRequiredValue { field: String },
    #[error("return Row requires an inferred record or collection of records")]
    ReturnRowRequiresRecord,
    #[error("multi-input compute requires a typed dependency packet")]
    DependencyPacketMissing,
    #[error("multi-input compute dependency packet cannot carry entity authority")]
    DependencyPacketHasEntityAuthority,
    #[error("multi-input compute requires a record dependency packet")]
    DependencyPacketNotRecord,
    #[error("multi-input compute parameter count does not match dependency fields")]
    DependencyCountMismatch,
    #[error("every compute input requires a type annotation")]
    InputAnnotationMissing,
    #[error("named compute dependency is missing")]
    NamedDependencyMissing,
    #[error("list[Row] input requires a collection dependency")]
    ListRowRequiresCollection,
    #[error("Row input requires a record dependency")]
    RowInputRequiresRecord,
    #[error("compute row annotation differs from its dependency contract")]
    DependencyAnnotationMismatch,
    #[error("compute input annotation does not match its dependency contract")]
    InputContractMismatch,
    #[error("compute function has no body")]
    ComputeBodyMissing,
    #[error("callback context is not loaded")]
    CallbackContextMissing,
    #[error("compute input annotation is missing")]
    ComputeInputAnnotationMissing,
    #[error("expected an entity symbol in the compute annotation")]
    ExpectedEntitySymbol,
    #[error("expected `Value[eN]` or `Row[eN]` annotation")]
    ExpectedEntityRecordAnnotation,
    #[error("nominal compute input requires an entity")]
    NominalInputEntityMissing,
    #[error("callback return requires a record constructor contract")]
    CallbackRecordContractMissing,
    #[error("compute definition is missing")]
    ComputeDefinitionMissing,
    #[error("lowering omitted a bound input")]
    BoundInputMissing,
    #[error("compute parameter has no materialization port")]
    MaterializationPortMissing,
    #[error("compute row input is missing")]
    ComputeRowInputMissing,
    #[error("compute input contract is missing")]
    ComputeInputContractMissing,
    #[error("compute input contract is missing after inference")]
    InferredComputeInputContractMissing,
    #[error("compute value contract symbol could not be resolved: {0}")]
    ValueContractSymbol(#[source] plasm_core::symbol_tuning::SymbolResolveError),
    #[error("compute value contract catalog ownership mismatch")]
    ValueContractOwnerMismatch,
    #[error("compute value contract entity `{entity}` is absent from its catalog")]
    ValueContractEntityMissing { entity: String },
    #[error("compute value contract field `{field}` has no catalog value definition")]
    ValueContractFieldMissing { field: String },
    #[error("compute value contract definition is invalid: {0}")]
    ValueContractDefinition(#[from] plasm_core::value_contract::ValueContractError),
    #[error("compute input violates its materialized type contract: {0}")]
    ComputeInputValueContract(#[source] plasm_core::value_contract::ValueContractError),
    #[error("compute value contract declaration could not encode a literal")]
    ValueContractLiteralEncoding(#[source] std::sync::Arc<serde_json::Error>),
    #[error("compute value contract has no allocated value symbol for `{field}`")]
    ValueContractSymbolMissing { field: String },
    #[error("compute input field `{field}` has no recursive contract")]
    RecursiveInputContractMissing { field: String },
    #[error("Python boundary field `{field}` is not a valid attribute identifier")]
    InvalidBoundaryMember { field: String },
    #[error("Python value domain `{value_ref}` is absent from catalog `{entry_id}`")]
    BoundaryValueDomainMissing { entry_id: String, value_ref: String },
    #[error("Python declaration contract nesting exceeds the maximum depth")]
    BoundaryContractDepthExceeded,
    #[error("Python declaration requires an explicit temporal contract")]
    BoundaryTemporalShapeRequired,
    #[error("Python declaration requires an explicit array element contract")]
    BoundaryArrayShapeRequired,
    #[error("Python declaration catalog pin does not match for `{entry_id}`")]
    BoundaryCatalogPinMismatch { entry_id: String },
    #[error("Python declaration literal encoding failed")]
    BoundaryLiteralEncoding(#[source] std::sync::Arc<serde_json::Error>),
    #[error("Python declaration contract fingerprint encoding failed")]
    BoundaryFingerprintEncoding(#[source] std::sync::Arc<serde_json::Error>),
    #[error("Python output declaration key encoding failed")]
    BoundaryKeyEncoding(#[source] std::sync::Arc<serde_json::Error>),
    #[error("compute input catalog/entity ownership mismatch")]
    ComputeInputOwnerMismatch,
    #[error("compute input row budget exceeded")]
    ComputeInputRowBudgetExceeded,
    #[error(transparent)]
    ComputeInputMembership(#[from] plasm_core::collection_codec::CollectionFault),
    #[error(transparent)]
    ComputeInputValueBudget(#[from] plasm_core::ValueBudgetError),
}

/// Rejection carried intact to the program-diagnostic boundary.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PythonComputeRejection {
    #[error(transparent)]
    Typed(#[from] PythonComputeError),
    #[error(transparent)]
    SyntheticSchema(#[from] plasm_core::plasm_monad::SyntheticResultSchemaError),
    #[error(transparent)]
    RowContract(#[from] plasm_core::row_plan::contracts::RowContractError),
    #[error(transparent)]
    Program(#[from] PythonProgramError),
    #[error(transparent)]
    Inference(#[from] Box<crate::python_compute::inference::InferenceError>),
    #[error(transparent)]
    PythonSchema(std::sync::Arc<crate::python_compute::schema::PythonSchemaError>),
    #[error(transparent)]
    Argument(std::sync::Arc<crate::python_compute::arguments::PythonArgumentError>),
    #[error(transparent)]
    MapBodySchema(std::sync::Arc<crate::map_body_schema::MapBodySchemaError>),
    #[error(transparent)]
    PythonTeaching(std::sync::Arc<plasm_core::prompt_render::python::PythonTeachingError>),
    #[error(transparent)]
    Lowering(#[from] Box<PythonLoweringError>),
    #[error("Python parser rejected source: {0}")]
    Parse(#[source] ruff_python_parser::ParseError),
}

impl From<crate::python_compute::schema::PythonSchemaError> for PythonComputeRejection {
    fn from(error: crate::python_compute::schema::PythonSchemaError) -> Self {
        Self::PythonSchema(std::sync::Arc::new(error))
    }
}

impl From<crate::map_body_schema::MapBodySchemaError> for PythonComputeRejection {
    fn from(error: crate::map_body_schema::MapBodySchemaError) -> Self {
        Self::MapBodySchema(std::sync::Arc::new(error))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PythonInputError {
    #[error("multiple invocation lanes require record inputs")]
    MultipleLanesRequireRecords,
    #[error("invocation lane must normalize to a record")]
    LaneMustNormalizeToRecord,
    #[error("input lanes overlap at field `{field}`")]
    OverlappingLanes { field: String },
    #[error("input type nesting exceeds the maximum depth of {max_depth}")]
    NestingLimitExceeded { max_depth: usize },
    #[error(
        "union input requires exactly one declared literal discriminator and its variant fields"
    )]
    InvalidUnionDiscriminator,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum PythonSourceError {
    #[error("Python source is invalid: writes require named arguments; positional payloads are not admitted")]
    WritePositionalArguments { actual: usize },
    #[error("Python source is invalid: this method requires an entity receiver")]
    ReceiverRequired { capability: String },
    #[error("Python source is invalid: write argument unpacking is not admitted")]
    WriteArgumentUnpacking,
    #[error("Python source is invalid: duplicate write argument: `{argument}`")]
    DuplicateWriteArgument { argument: String },
    #[error("Python source is invalid: expected a field dependency")]
    ExpectedFieldDependency,
    #[error("Python source is invalid: field input requires a proven singleton; binding `{binding}` has no singleton proof. Use row transforms or map for plural rows; establish a singleton before scalar extraction.")]
    FieldInputNeedsSingleton { binding: String },
    #[error("Python source is invalid: session method kind differs from pinned catalog")]
    SessionMethodKindMismatch { method: String },
    #[error("Python source is invalid: page_size requires a positive literal bound attached directly to a catalog read call")]
    PageSizeCallShape { positional: usize, keywords: usize },
    #[error("Python source is invalid: page_size requires a positive u32: got {value}")]
    InvalidPageSize { value: i64 },
    #[error("Python source is invalid: page_size applies to catalog query/search reads")]
    PageSizeRequiresCatalogRead,
    #[error("Python source is invalid: row constructor entered transform dispatch")]
    RowConstructorInTransform,
    #[error("Python source is invalid: select requires fields")]
    SelectFieldsMissing,
    #[error(
        "Python source is invalid: order_by requires one field and optional descending=True/False"
    )]
    OrderByArgumentShape { positional: usize, keywords: usize },
    #[error("Python source is invalid: descending requires a literal Boolean")]
    DescendingRequiresBoolean,
    #[error("Python source is invalid: order_by only accepts the descending keyword")]
    OrderByKeyword { keyword: Option<String> },
    #[error("Python source is invalid: row operation requires one positional argument")]
    RowOperationArgumentShape {
        operation: String,
        positional: usize,
        keywords: usize,
    },
    #[error("Python source is invalid: take requires a nonnegative u32: got {value}")]
    InvalidTakeBound { value: i64 },
    #[error("Python source is invalid: signed value requires a numeric literal")]
    SignedLiteralRequiresNumber,
    #[error("Python source is invalid: integer out of range: `{literal}` ({source})")]
    IntegerOutOfRange {
        literal: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("Python source is invalid: expected a finite real number")]
    ExpectedFiniteReal,
    #[error("Python source is invalid: expected a string, finite number, boolean or null literal")]
    ExpectedScalarLiteral,
    #[error("Python source is invalid: unknown compute method: `{method}`")]
    UnknownComputeMethod { method: String },
    #[error("Python source is invalid: expanded keyword dependencies require a statically materialized argument mapping")]
    ExpandedComputeKeywords,
    #[error("Python source is invalid: Row compute input requires a singleton; use list[Row] for a collection")]
    RowComputeNeedsSingleton,
    #[error("Python source is invalid: reserved callback binding name")]
    ReservedCallbackName { name: String },
    #[error("Python source is invalid: @compute must be a Program class method with self and a typed input; call it as self.method(rows). A nested def is a scoped callback")]
    NestedComputeDeclaration,
    #[error("Python source is invalid: DAG callbacks require a synchronous undecorated callable")]
    CallbackDeclarationShape {
        is_async: bool,
        decorators: usize,
        has_type_parameters: bool,
    },
    #[error("Python source is invalid: callback value nodes have one row port; capture other dependencies in the callback closure")]
    CallbackValuePortShape { ports: usize },
    #[error("Python source is invalid: callback value invocation requires a singleton row")]
    CallbackValueNeedsSingleton,
    #[error("Python source is invalid: callback value expressions cannot introduce effects")]
    CallbackValueEffects,
    #[error("Python source is invalid: unknown scoped callback: `{name}`")]
    UnknownScopedCallback { name: String },
    #[error("Python source is invalid: expected a lambda or a declared scoped callback")]
    ExpectedScopedCallback,
    #[error("Python source is invalid: duplicate projection column: `{column}`")]
    DuplicateProjectionColumn { column: String },
    #[error("Python source is invalid: projection expressions cannot introduce effects")]
    ProjectionEffects,
    #[error("Python source is invalid: query/search require named selection arguments")]
    ReadPositionalArguments { actual: usize },
    #[error("Python source is invalid: keyword unpacking is not admitted")]
    SelectionUnpacking,
    #[error("Python source is invalid: duplicate selection argument: `{argument}`")]
    DuplicateSelectionArgument { argument: String },
    #[error("Python source is invalid: this Get takes no identity arguments")]
    NullaryGetArguments { positional: usize, keywords: usize },
    #[error("Python source is invalid: compound Get requires exactly its named identity keys")]
    CompoundGetArgumentShape {
        entity: String,
        expected: Vec<String>,
        positional: usize,
    },
    #[error("Python source is invalid: identity keyword unpacking is not admitted")]
    IdentityUnpacking,
    #[error("Python source is invalid: unknown compound identity key: `{entity}.{key}`")]
    UnknownCompoundIdentityKey { entity: String, key: String },
    #[error("Python source is invalid: duplicate compound identity key: `{entity}.{key}`")]
    DuplicateCompoundIdentityKey { entity: String, key: String },
    #[error("Python source is invalid: compound Get requires every identity key: {missing:?}")]
    MissingCompoundIdentityKeys {
        entity: String,
        missing: Vec<String>,
    },
    #[error("Python source is invalid: Get takes at most one positional identity")]
    GetPositionalArgumentCount { actual: usize },
    #[error("Python source is invalid: unexpected Get argument; expected identity: `{argument}`")]
    UnexpectedGetArgument { argument: String },
    #[error("Python source is invalid: Get received multiple values for identity")]
    DuplicateGetIdentity,
    #[error("Python source is invalid: Get requires identity (positional or keyword)")]
    MissingGetIdentity,
    #[error("Python source is invalid: unsupported identity expression")]
    UnsupportedIdentityExpression,
    #[error("Python source is invalid: identity negation requires an integer literal")]
    IdentityNegationRequiresInteger,
    #[error("Python source is invalid: IEEE float is not an identity literal")]
    FloatIdentity,
    #[error("Python source is invalid: integer identity out of range: `{literal}` ({source})")]
    IntegerIdentityOutOfRange {
        literal: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("Python source is invalid: identity requires a string, exact integer, boolean or typed binding")]
    ExpectedIdentityValue,
    #[error("Python source is invalid: distinct accepts only literal field names")]
    DistinctKeywords,
    #[error("Python source is invalid: aggregate accepts only named aggregate descriptors")]
    AggregatePositionalArguments,
    #[error("Python source is invalid: aggregate unpacking is not admitted")]
    AggregateUnpacking,
    #[error("Python source is invalid: expected agg.count() or agg.function(\"field\")")]
    ExpectedAggregateCall,
    #[error("Python source is invalid: expected an agg descriptor")]
    ExpectedAggregateAttribute,
    #[error("Python source is invalid: expected a literal agg descriptor")]
    AggregateDescriptorShape,
    #[error("Python source is invalid: unsupported aggregate function: `{function}`")]
    UnknownAggregateFunction { function: String },
    #[error("Python source is invalid: agg.count takes no arguments")]
    CountArguments { actual: usize },
    #[error("Python source is invalid: aggregate requires one literal field name")]
    AggregateFieldArgumentShape { function: String, actual: usize },
    #[error("Python source is invalid: class state and executable class bodies are not admitted")]
    ClassExecutableState,
    #[error("Python source is invalid: reserved root name: `{name}`")]
    ReservedRootName { name: String },
    #[error("Python source is invalid: root must derive directly and only from Program")]
    ProgramBaseShape,
    #[error("Python source is invalid: root cannot shadow a session entity: `{name}`")]
    RootShadowsEntity { name: String },
    #[error("Python source is invalid: duplicate build method")]
    DuplicateBuildMethod,
    #[error("Python source is invalid: variadic build inputs have no materialization port")]
    VariadicBuildInputs,
    #[error("Python source is invalid: build decorators and return annotations have no Program interface")]
    BuildInterfaceShape,
    #[error("Python source is invalid: build requires the Program receiver self")]
    BuildReceiverShape,
    #[error("Python source is invalid: helper requires self")]
    HelperReceiverMissing,
    #[error("Python source is invalid: invalid Program helper receiver or reserved method")]
    HelperDeclarationShape,
    #[error("Python source is invalid: variadic helper inputs have no DAG port")]
    VariadicHelperInputs,
    #[error("Python source is invalid: duplicate Program method: `{method}`")]
    DuplicateProgramMethod { method: String },
    #[error("Python source is invalid: method decorators require exactly @compute")]
    ComputeDecoratorShape,
    #[error("Python source is invalid: compute requires a bound Program receiver")]
    ComputeReceiverMissing,
    #[error("Python source is invalid: Program receiver must be named self: got `{name}`")]
    ProgramReceiverName { name: String },
    #[error(
        "Python source is invalid: variadic compute inputs have no typed materialization port"
    )]
    VariadicComputeInputs,
    #[error("Python source is invalid: duplicate compute method: `{method}`")]
    DuplicateComputeMethod { method: String },
    #[error("Python source is invalid: @compute is pure: materialized rows lose entity identity and cannot dispatch writes. Keep calculations in @compute; select original entity rows and invoke the declared write in build via flat_map.")]
    ComputeWriteAuthority,
    #[error("Python source is invalid: @compute receives materialized values without entity relation authority. Navigate the taught relation on its original entity row in build or a scoped callback, then pass the resulting rows as a typed compute input.")]
    ComputeRelationAuthority,
    #[error("Python source is invalid: expected a synchronous method without type parameters")]
    MethodDeclarationShape {
        method: String,
        is_async: bool,
        has_type_parameters: bool,
    },
    #[error("Python source is invalid: lazy value expressions cannot introduce effects")]
    LazyValueEffects,
    #[error("Python source is invalid: condition requires a boolean value")]
    ConditionRequiresBoolean,
    #[error("Python source is invalid: build iteration requires a literal list or tuple of string/integer IDs; use a rowset callback for data-dependent iteration")]
    StaticIterationRequiresLiteralIds,
    #[error("Python source is invalid: build iteration requires one local name")]
    StaticIterationTargetShape,
    #[error("Python source is invalid: reserved build iteration name: `{name}`")]
    ReservedStaticIterationName { name: String },
    #[error("Python source is invalid: build literal expansion exceeds 256 iterations")]
    StaticExpansionLimit { max: usize, actual: usize },
    #[error("Python source is invalid: build iteration does not admit async or for-else")]
    StaticForShape { is_async: bool, has_else: bool },
    #[error("Python source is invalid: return inside build iteration is not a DAG return; return after the loop")]
    StaticIterationReturn,
    #[error("Python source is invalid: build list comprehension requires one literal iterator")]
    StaticComprehensionIteratorCount { actual: usize },
    #[error("Python source is invalid: build list comprehension does not admit async or filters; filter rows with where")]
    StaticComprehensionShape { is_async: bool, filters: usize },
    #[error("Python source is invalid: unused expression statements must be writes")]
    UnusedNonWriteExpression,
    #[error("Python source is invalid: only immutable local assignments are admitted")]
    MutableLocalAssignment,
    #[error("Python source is invalid: reserved binding name: `{name}`")]
    ReservedBindingName { name: String },
    #[error("Python source is invalid: unknown local rowset: `{binding}`")]
    UnknownLocalRowset { binding: String },
    #[error("Python source is invalid: expected a catalog read or rowset operation")]
    ExpectedRowsetExpression,
    #[error("Python source is invalid: dynamic calls are not admitted")]
    DynamicCall,
    #[error(
        "Python source is invalid: write receiver requires a proven singleton with entity identity"
    )]
    WriteReceiverNeedsIdentitySingleton,
    #[error("Python source is invalid: method is not a mutation or action")]
    ExpectedWriteMethod { method: String },
    #[error("Python source is invalid: unsupported rowset operation: `{operation}`")]
    UnknownRowOperation { operation: String },
    #[error("Python source is invalid: expected a string literal or literal concatenation")]
    ExpectedStringLiteral,
    #[error("Python source is invalid: expected an integer literal")]
    ExpectedIntegerLiteral,
    #[error("Python source is invalid: membership RHS must be closed; it cannot capture the enclosing row")]
    MembershipCapturesRow,
    #[error("Python source is invalid: DAG handle annotation requires a visible eN entity: `{entity}` ({source})")]
    DagHandleEntityMissing {
        entity: String,
        #[source]
        source: plasm_core::symbol_tuning::SymbolResolveError,
    },
    #[error("Python source is invalid: DAG handle annotation does not match the argument's entity authority or cardinality")]
    DagHandleContractMismatch,
    #[error("Python source is invalid: recursive DAG methods have no bounded expansion")]
    RecursiveDagMethod { method: String },
    #[error("Python source is invalid: iterate requires one step callback, until=predicate and max_steps=positive integer")]
    IterationArgumentShape,
    #[error("Python source is invalid: iterate requires until")]
    MissingIterationUntil,
    #[error("Python source is invalid: iterate requires max_steps")]
    MissingIterationBound,
    #[error("Python source is invalid: max_steps must be a positive u32: got {value}")]
    InvalidIterationBound { value: i64 },
    #[error("Python source is invalid: quantification requires one iterable")]
    QuantifierIterableCount { actual: usize },
    #[error("Python source is invalid: quantification does not accept keywords")]
    QuantifierKeywords,
    #[error("Python source is invalid: quantification requires one synchronous generator")]
    QuantifierGeneratorShape { actual: usize },
    #[error("Python source is invalid: generator target must be a row name")]
    QuantifierTargetShape,
    #[error("Python source is invalid: async generators are not outer DAG expressions")]
    AsyncGenerator,
    #[error("Python source is invalid: scoped composition exceeds 16 map levels")]
    MapScopeDepth { max: usize, actual: usize },
    #[error("Python source is invalid: expected map")]
    ExpectedMapCall,
    #[error("Python source is invalid: expected map receiver")]
    ExpectedMapReceiver,
    #[error("Python source is invalid: map requires max_parents=<positive bound>")]
    MissingMapParentBound,
    #[error("Python source is invalid: scoped composition exceeds 16 levels")]
    CallbackScopeDepth { max: usize, actual: usize },
    #[error("Python source is invalid: recursive callbacks are not a bounded DAG")]
    RecursiveCallback { callback: Option<String> },
    #[error("Python source is invalid: callback parameter must not shadow a host binding")]
    CallbackParameterShadowsHost { name: String },
    #[error("Python source is invalid: map parameter must not shadow a reserved binding")]
    MapParameterShadowsReserved { name: String },
    #[error("Python source is invalid: flat_map requires rows or effects, not a scalar value")]
    FlatMapScalarResult,
    #[error("Python source is invalid: value expression depth exceeds 64")]
    ValueExpressionDepth { max: usize, actual: usize },
    #[error("Python source is invalid: duplicate output field: `{field}`")]
    DuplicateOutputField { field: String },
    #[error("Python source is invalid: a callback result cannot mix effects and values")]
    MixedCallbackResult,
    #[error(
        "Python source is invalid: relation dot requires a singleton; use flat_map for plural rows"
    )]
    RelationNeedsSingleton,
    #[error("Python source is invalid: read method and entity ownership differ")]
    ReadMethodOwnerMismatch {
        method: String,
        expected_entry: String,
        expected_entity: String,
        actual_entry: String,
        actual_entity: String,
    },
    #[error("Python source is invalid: method and receiver catalog/entity ownership differ")]
    WriteMethodOwnerMismatch {
        method: String,
        expected_entry: String,
        expected_entity: String,
        actual_entry: String,
        actual_entity: String,
    },
    #[error("Python source is invalid: flat_map requires exactly one callback")]
    FlatMapCallbackCount { actual: usize },
    #[error("Python source is invalid: map requires exactly one callback")]
    MapCallbackCount { actual: usize },
    #[error("Python source is invalid: flat_map accepts only max_parents as a keyword")]
    FlatMapKeywordShape { keywords: Vec<Option<String>> },
    #[error("Python source is invalid: map accepts only max_parents as a keyword")]
    MapKeywordShape { keywords: Vec<Option<String>> },
    #[error("Python source is invalid: `Rows` is a DAG handle; omit the annotation because @compute infers a Python collection from the call")]
    ComputeRowsHandleAnnotation,
    #[error("Python source is invalid: `Singleton` is a DAG handle; omit the annotation because @compute infers a Python row from the call")]
    ComputeSingletonHandleAnnotation,
    #[error("Python source is invalid: an eN symbol is a catalog binding, not a Python type; omit the annotation and let @compute infer its input from the call")]
    ComputeEntityElementAnnotation,
    #[error("Python source is invalid: imports are not build statements; declare permitted imports at module scope or inside @compute")]
    BuildImport,
    #[error("Python source is invalid: build cannot expand a while loop; use bounded iterate for re-observed effects or @compute for pure Python iteration")]
    BuildWhile,
    #[error("Python source is invalid: build does not execute Python branches; place a conditional in a scoped callback, or filter rows before applying effects")]
    BuildBranch,
    #[error("Python source is invalid: unknown iteration argument `{keyword}`; iterate accepts until and max_steps")]
    UnknownIterationKeyword { keyword: String },
    #[error("Python source is invalid: duplicate iteration argument `{keyword}`; pass until and max_steps once each")]
    DuplicateIterationKeyword { keyword: String },
    #[error("Python source is invalid: unpacked iteration arguments are not admitted; pass until and max_steps explicitly")]
    IterationKeywordUnpacking,
    #[error("Python source is invalid: invalid iteration step: {source}")]
    IterationStep {
        #[source]
        source: plasm_core::plasm_monad::correlated::IterationStepEffectError,
    },
    #[error("Python source is invalid: immutable build assignments require exactly one target")]
    BuildAssignmentTargets { actual: usize },
    #[error("Python source is invalid: build cannot delete bindings; bind a new immutable local")]
    BuildDelete,
    #[error("Python source is invalid: build cannot mutate bindings with augmented assignment; bind a new local")]
    BuildAugmentedAssignment,
    #[error("Python source is invalid: build admits plain immutable assignments; annotations belong on method inputs")]
    BuildAnnotatedAssignment,
    #[error("Python source is invalid: build does not execute context managers; use declared callbacks or @compute")]
    BuildWith,
    #[error("Python source is invalid: build does not execute raise statements; keep Python exceptions inside @compute")]
    BuildRaise,
    #[error("Python source is invalid: build does not execute exception handlers; keep them inside @compute")]
    BuildTry,
    #[error("Python source is invalid: build does not execute assertions; keep assertions inside @compute")]
    BuildAssert,
    #[error("Python source is invalid: build locals cannot declare global state")]
    BuildGlobal,
    #[error("Python source is invalid: build locals cannot declare nonlocal state")]
    BuildNonlocal,
    #[error("Python source is invalid: build requires a binding, callback, effect or explicit return rather than pass")]
    BuildPass,
    #[error("Python source is invalid: build cannot break literal expansion; use a bounded rowset operation")]
    BuildBreak,
    #[error("Python source is invalid: build cannot continue literal expansion; use a bounded rowset operation")]
    BuildContinue,
    #[error("Python source is invalid: build cannot declare type aliases; use served typed declarations")]
    BuildTypeAlias,
    #[error("Python source is invalid: build cannot declare nested classes; declare scoped callbacks instead")]
    BuildNestedClass,
    #[error("Python source is invalid: build admits standalone effect calls and documentation, not other expression statements")]
    BuildExpression,
    #[error("Python source is invalid: build does not execute IPython escape commands")]
    BuildIpythonCommand,
    #[error(transparent)]
    Analysis(#[from] monty_analysis::AnalysisError),
    #[error("Python parser rejected the source: {source}")]
    Parse {
        #[source]
        source: ruff_python_parser::ParseError,
    },
}

/// Semantic rejection raised while admitting or lowering a Python Program.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PythonProgramError {
    #[error("Program class must derive directly and only from Program")]
    ProgramBaseInvalid,
    #[error("expected exactly one Program subclass")]
    ProgramDeclarationCount,
    #[error("expected a Program subclass")]
    ProgramDeclarationMissing,
    #[error("an import cannot shadow a session entity or the Program class")]
    ImportShadowsReservedBinding,
    #[error("a build local cannot shadow a reserved host binding")]
    BuildLocalShadowsReservedBinding,
    #[error("build requires materialized return roots; branching roots have no DAG return representation")]
    BranchingReturnUnsupported,
    #[error("return must contain at least one rowset")]
    EmptyReturn,
    #[error("compute {name} requires a typed DAG callsite")]
    UnusedCompute { name: String },
    #[error("expected one program class")]
    ProgramClassCount,
    #[error("program class is missing its build method")]
    BuildMethodMissing,
    #[error("normalized Python method is missing")]
    NormalizedMethodMissing,
    #[error("internal branch template is not a lambda")]
    InvalidBranchTemplate,
    #[error("capture refinement exceeds 64 levels")]
    CaptureRefinementTooDeep,
    #[error("a variadic tuple cannot carry a direct DAG row receiver")]
    VariadicTupleReceiver,
    #[error("projection parameter uses a reserved name")]
    ReservedProjectionParameter,
    #[error("Python value expressions cannot acquire write authority")]
    ValueExpressionWriteAuthority,
    #[error("membership requires an explicit one-column rowset")]
    MembershipNeedsOneColumn,
    #[error("field input requires a proven singleton")]
    FieldInputNeedsSingleton,
    #[error("callback branches cannot mix rows and acknowledgements")]
    CallbackBranchKindMismatch,
    #[error("callback branches must return the same record fields")]
    CallbackBranchFieldMismatch,
    #[error("variadic helper inputs have no DAG port")]
    VariadicHelperInputs,
    #[error("unknown projected field `{field}`")]
    UnknownProjectedField { field: String },
    #[error("helper scalar-cell input has no `value` contract")]
    HelperScalarCellValueMissing,
    #[error("helper input needs a concrete temporal or array contract")]
    HelperInputRequiresTemporalOrArrayContract,
    #[error("helper input needs a concrete materialized value contract")]
    HelperInputRequiresMaterializedValueContract,
    #[error("helper input needs an explicit mapping annotation")]
    HelperInputRequiresMappingAnnotation,
}

impl PythonComputeRejection {
    pub fn correction(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Typed(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::SyntheticSchema(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Lowering(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::RowContract(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Program(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Inference(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::PythonSchema(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Argument(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::MapBodySchema(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::PythonTeaching(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Parse(error) => std::borrow::Cow::Owned(error.to_string()),
        }
    }
    pub fn into_correction(self) -> String {
        match self {
            Self::Typed(error) => error.to_string(),
            Self::SyntheticSchema(error) => error.to_string(),
            Self::Lowering(error) => error.to_string(),
            Self::RowContract(error) => error.to_string(),
            Self::Program(error) => error.to_string(),
            Self::Inference(error) => error.to_string(),
            Self::PythonSchema(error) => error.to_string(),
            Self::Argument(error) => error.to_string(),
            Self::MapBodySchema(error) => error.to_string(),
            Self::PythonTeaching(error) => error.to_string(),
            Self::Parse(error) => error.to_string(),
        }
    }
}

impl From<PythonComputeRejection> for plasm_runtime::ExecutionFailure {
    fn from(error: PythonComputeRejection) -> Self {
        Self::new(
            plasm_runtime::FailureCause::Program,
            "python_compute_rejected",
            error.into_correction(),
        )
    }
}

impl From<crate::python_compute::inference::InferenceError> for PythonComputeRejection {
    fn from(error: crate::python_compute::inference::InferenceError) -> Self {
        Self::Typed(PythonComputeError::Inference(Box::new(error)))
    }
}

impl From<crate::python_compute::arguments::PythonArgumentError> for PythonComputeRejection {
    fn from(error: crate::python_compute::arguments::PythonArgumentError) -> Self {
        Self::Argument(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::prompt_render::python::PythonTeachingError> for PythonComputeRejection {
    fn from(error: plasm_core::prompt_render::python::PythonTeachingError) -> Self {
        Self::PythonTeaching(std::sync::Arc::new(error))
    }
}

impl From<PythonLoweringError> for PythonComputeRejection {
    fn from(error: PythonLoweringError) -> Self {
        Self::Lowering(Box::new(error))
    }
}

#[cfg(test)]
mod python_compute_rejection_tests {
    use super::*;

    #[test]
    fn python_teaching_failure_remains_typed_until_correction_rendering() {
        let rejection = PythonComputeRejection::from(
            plasm_core::prompt_render::python::PythonTeachingError::LanguageChanged,
        );
        assert!(matches!(
            &rejection,
            PythonComputeRejection::PythonTeaching(error)
                if matches!(error.as_ref(), plasm_core::prompt_render::python::PythonTeachingError::LanguageChanged)
        ));
        assert_eq!(
            rejection.into_correction(),
            "Python teaching language changed within a pinned session"
        );
    }
}

/// Boundary-specific correction attached without replacing the semantic cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PythonLoweringContext {
    #[error("method defaults require scalar constant metadata; definition-time execution and mutable default state have no Program representation")]
    MethodDefaultRequiresScalar,
    #[error("materialized dictionaries require string keys")]
    DictionaryKeyRequiresString,
}

/// A Python rejection is minted at the recognizing compiler boundary and is
/// carried unchanged through the agent response envelope.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PythonLoweringError {
    Located {
        #[source]
        error: std::sync::Arc<PythonLoweringError>,
        context: Option<PythonLoweringContext>,
        span: Option<(u32, u32)>,
    },
    Program(#[source] PythonProgramError),
    Source {
        #[source]
        error: std::sync::Arc<PythonSourceError>,
        span: Option<(u32, u32)>,
    },
    Internal(#[source] PythonLoweringInvariantError),
    Analysis(#[source] std::sync::Arc<monty_analysis::AnalysisError>),
    SyntheticSchema(#[source] std::sync::Arc<plasm_core::plasm_monad::SyntheticResultSchemaError>),
    SymbolResolve(#[source] std::sync::Arc<plasm_core::symbol_tuning::SymbolResolveError>),
    Type(#[source] std::sync::Arc<plasm_core::TypeError>),
    Compute(#[source] PythonComputeRejection),
    ComputeTyped(#[source] PythonComputeError),
    Correlated(#[source] plasm_core::plasm_monad::CorrelatedBodyError),
    PlanValidation(#[source] std::sync::Arc<crate::plasm_plan::PlanValidationError>),
    StepId(#[source] std::sync::Arc<plasm_core::plasm_monad::StepIdError>),
    CompBundle(#[source] std::sync::Arc<crate::plasm_comp_bundle::PlasmCompBundleError>),
    StepPayloadLift(#[source] std::sync::Arc<crate::plasm_step_convert::StepPayloadLiftError>),
    Atom(#[source] plasm_core::plasm_monad::PlanAtomError),
    Iteration(#[source] plasm_core::expr_parser::IterateUntilError),
    Dag(#[source] std::sync::Arc<crate::plasm_dag::error::DagCompilationError>),
    RowSuffix(#[source] std::sync::Arc<crate::plasm_dag::RowSuffixLoweringError>),
    RowContract(#[source] std::sync::Arc<plasm_core::row_plan::contracts::RowContractError>),
    ValueContract(#[source] std::sync::Arc<plasm_core::value_contract::ValueContractError>),
    MapBodySchema(#[source] std::sync::Arc<crate::map_body_schema::MapBodySchemaError>),
    Input(#[source] std::sync::Arc<PythonInputError>),
    TypedInvoke(#[source] std::sync::Arc<plasm_core::TypedInvokeInputError>),
    CatalogOwnership(#[source] std::sync::Arc<crate::catalog_ownership::CatalogOwnershipError>),
    RowsetNormalize(#[source] std::sync::Arc<plasm_core::rowset::RowsetNormalizeError>),
    SessionProvision(
        #[source] std::sync::Arc<crate::plan_session_provisions::SessionProvisionError>,
    ),
    InputAt {
        #[source]
        error: std::sync::Arc<PythonInputError>,
        span: (u32, u32),
    },
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum PythonLoweringInvariantError {
    #[error("lowering rejected a duplicate DAG label `{label}`")]
    DuplicateDagLabel { label: String },
    #[error("program contains a duplicate binding label `{label}`")]
    DuplicateBindingLabel { label: String },
    #[error("lowering source has an invalid program binding label")]
    InvalidProgramBindingLabel,
    #[error("lowering expected a bound build default")]
    MissingBoundBuildDefault,
    #[error("lowering expected a constructed value")]
    MissingConstructedValue,
    #[error("unit row literal is not a resolved data value")]
    InvalidUnitLiteral,
    #[error("projection alias could not be serialized as a Python string literal")]
    ProjectionAliasSerialization(#[source] std::sync::Arc<serde_json::Error>),
    #[error("a Python literal could not be converted to resolved Plasm data")]
    InvalidResolvedLiteral,
    #[error("callback branch is missing a return value contract")]
    CallbackReturnValueContractMissing,
    #[error("lowering expected a receiver entity")]
    MissingReceiverEntity,
    #[error("lowering expected a method capability")]
    MissingMethodCapability,
    #[error("lowering expected an inferred source node")]
    MissingInferredSourceNode,
    #[error("lowering expected a statement node")]
    MissingStatementNode,
    #[error("lowering could not encode a bounded scope")]
    ScopeDepthExceeded,
    #[error("lowering requires typed catalog provenance")]
    CatalogProvenanceMissing,
    #[error("lowering requires an input contract")]
    InputContractMissing,
    #[error("lowering requires a value contract")]
    ValueContractMissing,
    #[error("lowering requires a scoped output contract")]
    ScopedOutputContractMissing,
    #[error("lowering requires a callback return type")]
    CallbackReturnTypeMissing,
    #[error("lowering requires a helper definition")]
    HelperDefinitionMissing,
    #[error("lowering requires a callback parameter list")]
    CallbackParametersMissing,
    #[error("lowering requires the Program build method")]
    ProgramBuildMissing,
    #[error("lowering requires a bound method default")]
    BoundMethodDefaultMissing,
    #[error("lowering requires a normalized method body")]
    NormalizedMethodBodyMissing,
    #[error("lowering requires a source parameter")]
    SourceParameterMissing,
    #[error("lowering requires a compute definition")]
    ComputeDefinitionMissing,
    #[error("lowering requires a pinned session context")]
    PinnedSessionContextMissing,
    #[error("lowering requires a loaded compute context")]
    ComputeContextMissing,
    #[error("lowering could not resolve a row source")]
    RowSourceMissing,
    #[error("lowering could not resolve an input entity owner")]
    InputEntityOwnerMissing,
    #[error("lowering could not resolve an input entity")]
    InputEntityMissing,
    #[error("lowering could not resolve a required input")]
    RequiredInputMissing,
    #[error("lowering could not resolve a materialization port")]
    MaterializationPortMissing,
    #[error("lowering could not resolve a row input")]
    RowInputMissing,
    #[error("lowering could not resolve a default dependency")]
    DefaultDependencyMissing,
    #[error("lowering could not resolve a scoped result")]
    ScopedResultMissing,
    #[error("lowering could not resolve a value result")]
    ValueResultMissing,
    #[error("lowering could not resolve an effect result")]
    EffectResultMissing,
    #[error("lowering could not resolve a capture contract")]
    CaptureContractMissing,
    #[error("lowering could not resolve a helper argument contract")]
    HelperArgumentContractMissing,
    #[error("lowering could not resolve a helper scalar contract")]
    HelperScalarContractMissing,
    #[error("lowering could not resolve an effect sequence item")]
    EffectSequenceItemMissing,
    #[error("lowering could not resolve a receiver contract")]
    ReceiverContractMissing,
    #[error("lowering could not resolve a scalar value field `{field}`")]
    ScalarValueFieldMissing { field: String },
    #[error("lowering exceeded the inferred row schema depth limit")]
    InferredSchemaDepthExceeded,
    #[error("lowering scope parent bound exceeds the execution budget")]
    ScopeParentBoundExceeded,
    #[error("map source has no inferred binding contract")]
    MapSourceContractMissing,
    #[error("map rows require typed catalog provenance")]
    MapCatalogProvenanceMissing,
    #[error("callback default dependency is absent")]
    CallbackDefaultDependencyMissing,
    #[error("callback default value contract is absent")]
    CallbackDefaultValueContractMissing,
    #[error("scoped callback result has no catalog provenance")]
    ScopedResultProvenanceMissing,
    #[error("scoped callback result node is absent")]
    ScopedResultNodeMissing,
    #[error("captured value has no catalog provenance")]
    CapturedValueProvenanceMissing,
    #[error("capture scalar contract is absent")]
    CaptureScalarContractMissing,
    #[error("lowering expected a value result")]
    ValueResultNodeMissing,
    #[error("lowering expected an effect result")]
    EffectResultNodeMissing,
    #[error("compute callback requires a materialized argument mapping")]
    CallbackArgumentMappingMissing,
    #[error("callback input contract is absent")]
    CallbackInputContractMissing,
    #[error("projection unpacking is not admitted")]
    ProjectionUnpackingNotAdmitted,
    #[error("projection requires exactly one row parameter")]
    ProjectionRowParameterMissing,
    #[error("upstream row binding has no source parameter")]
    UpstreamSourceParameterMissing,
    #[error("iteration seed node is absent")]
    IterationSeedMissing,
    #[error("quantifier source node is absent")]
    QuantifierSourceMissing,
    #[error("conditional branch type is absent")]
    ConditionalBranchTypeMissing,
    #[error("relation source contract is absent")]
    RelationSourceContractMissing,
    #[error("entity has no primary search capability")]
    PrimarySearchCapabilityMissing,
    #[error("catalog entity is absent")]
    CatalogEntityMissing,
    #[error("catalog Get entity is absent")]
    GetEntityMissing,
    #[error("method capability is absent")]
    MethodCapabilityMissing,
    #[error("read node is absent")]
    ReadNodeMissing,
    #[error("statement node is absent")]
    StatementNodeMissing,
    #[error("expression receiver contract is absent")]
    ExpressionReceiverContractMissing,
    #[error("expanded helper arguments have no DAG port")]
    HelperDagPortMissing,
    #[error("helper default input is absent")]
    HelperDefaultInputMissing,
    #[error("dictionary unpacking is not admitted")]
    DictionaryUnpackingNotAdmitted,
    #[error("boolean expression has no operands")]
    BooleanOperandsMissing,
    #[error("receiver entity is absent")]
    ReceiverEntityMissing,
    #[error("input binding is absent")]
    InputBindingMissing,
    #[error("iteration seed node is absent")]
    IterationSeedNodeMissing,
    #[error("lowering could not infer a scalar value contract")]
    ScalarValueContractMissing,
}

impl PythonLoweringError {
    pub fn parse_error(error: ruff_python_parser::ParseError) -> Self {
        Self::Source {
            error: std::sync::Arc::new(PythonSourceError::Parse { source: error }),
            span: None,
        }
    }

    /// Attach source coordinates without rendering or replacing the semantic cause.
    pub fn with_span(self, span: (u32, u32)) -> Self {
        match self {
            Self::Source { error, .. } => Self::Source {
                error,
                span: Some(span),
            },
            Self::Input(error) | Self::InputAt { error, .. } => Self::InputAt { error, span },
            Self::Located { error, context, .. } => Self::Located {
                error,
                context,
                span: Some(span),
            },
            error => Self::Located {
                error: std::sync::Arc::new(error),
                context: None,
                span: Some(span),
            },
        }
    }

    pub fn with_context(self, context: PythonLoweringContext) -> Self {
        Self::Located {
            error: std::sync::Arc::new(self),
            context: Some(context),
            span: None,
        }
    }

    pub fn input_at(error: PythonInputError, span: (u32, u32)) -> Self {
        Self::InputAt {
            error: std::sync::Arc::new(error),
            span,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Located { error, .. } => error.message(),
            Self::Program(_) => "Python Program admission failed",
            Self::Source { .. } => "invalid Python source",
            Self::Internal(_) => "Python lowering invariant failed",
            Self::Analysis(_) => "Python analysis failed",
            Self::SyntheticSchema(_) => "synthetic compute schema is invalid",
            Self::SymbolResolve(_) => "Python symbol resolution failed",
            Self::Type(_) => "Plasm type validation failed",
            Self::Compute(_) => "Python compute contract rejected",
            Self::ComputeTyped(_) => "Python compute contract rejected",
            Self::Correlated(_) => "correlated scope validation failed",
            Self::PlanValidation(_) => "Plan validation failed",
            Self::StepId(_) => "invalid step identifier",
            Self::CompBundle(_) => "invalid compiled Plasm bundle",
            Self::StepPayloadLift(_) => "plan payload lifting failed",
            Self::Atom(_) => "invalid plan atom",
            Self::Iteration(_) => "invalid state iteration",
            Self::Dag(_) => "DAG compilation failed",
            Self::RowSuffix(_) => "row suffix lowering failed",
            Self::RowContract(_) => "row contract validation failed",
            Self::MapBodySchema(_) => "map body schema validation failed",
            Self::ValueContract(_) => "value contract validation failed",
            Self::Input(_) => "invalid Python invocation input",
            Self::TypedInvoke(_) => "invalid Python invocation input",
            Self::CatalogOwnership(_) => "Python compute catalog ownership resolution failed",
            Self::RowsetNormalize(_) => "Python source rowset normalization failed",
            Self::SessionProvision(_) => "Python plan session provision validation failed",
            Self::InputAt { .. } => "invalid Python invocation input",
        }
    }

    pub fn into_message(self) -> String {
        match self {
            Self::Located { error, context, .. } => match context {
                Some(context) => format!("{context}: {error}"),
                None => error.to_string(),
            },
            Self::Program(error) => error.to_string(),
            Self::Source { error, .. } => error.to_string(),
            Self::Internal(error) => error.to_string(),
            Self::Analysis(error) => error.to_string(),
            Self::SyntheticSchema(error) => error.to_string(),
            Self::SymbolResolve(error) => error.to_string(),
            Self::Type(error) => error.to_string(),
            Self::Compute(error) => error.into_correction(),
            Self::ComputeTyped(error) => error.to_string(),
            Self::Correlated(error) => error.to_string(),
            Self::PlanValidation(error) => error.to_string(),
            Self::StepId(error) => error.to_string(),
            Self::CompBundle(error) => error.to_string(),
            Self::StepPayloadLift(error) => error.to_string(),
            Self::Atom(error) => error.to_string(),
            Self::Iteration(error) => error.to_string(),
            Self::Dag(error) => error.to_string(),
            Self::RowSuffix(error) => error.to_string(),
            Self::RowContract(error) => error.to_string(),
            Self::MapBodySchema(error) => error.to_string(),
            Self::ValueContract(error) => error.to_string(),
            Self::Input(error) => error.to_string(),
            Self::TypedInvoke(error) => error.to_string(),
            Self::CatalogOwnership(error) => error.to_string(),
            Self::RowsetNormalize(error) => error.to_string(),
            Self::SessionProvision(error) => error.to_string(),
            Self::InputAt { error, span } => {
                format!("Python bytes {}..{}: {error}", span.0, span.1)
            }
        }
    }

    fn display_message(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Located { error, context, .. } => match context {
                Some(context) => std::borrow::Cow::Owned(format!("{context}: {error}")),
                None => error.display_message(),
            },
            Self::Program(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Source { error, .. } => std::borrow::Cow::Owned(error.to_string()),
            Self::Internal(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Analysis(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::SyntheticSchema(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::SymbolResolve(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Type(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Compute(error) => std::borrow::Cow::Owned(error.correction().into_owned()),
            Self::ComputeTyped(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Correlated(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::PlanValidation(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::StepId(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::CompBundle(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::StepPayloadLift(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Atom(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Iteration(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Dag(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::RowSuffix(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::RowContract(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::MapBodySchema(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::ValueContract(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::Input(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::TypedInvoke(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::CatalogOwnership(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::RowsetNormalize(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::SessionProvision(error) => std::borrow::Cow::Owned(error.to_string()),
            Self::InputAt { error, span } => {
                std::borrow::Cow::Owned(format!("Python bytes {}..{}: {error}", span.0, span.1))
            }
        }
    }

    pub fn span_offset(&self) -> Option<usize> {
        match self {
            Self::Located { error, span, .. } => span
                .map(|(start, _)| start as usize)
                .or_else(|| error.span_offset()),
            Self::Source { span, .. } => span.map(|(start, _)| start as usize),
            Self::Program(_) | Self::Compute(_) | Self::ComputeTyped(_) => None,
            Self::Internal(_) => None,
            Self::Analysis(_) => None,
            Self::SyntheticSchema(_) => None,
            Self::SymbolResolve(_) | Self::Type(_) => None,
            Self::Correlated(_) => None,
            Self::PlanValidation(_)
            | Self::StepId(_)
            | Self::CompBundle(_)
            | Self::StepPayloadLift(_)
            | Self::Atom(_)
            | Self::Iteration(_)
            | Self::Dag(_)
            | Self::RowSuffix(_) => None,
            Self::RowContract(_) | Self::ValueContract(_) | Self::MapBodySchema(_) => None,
            Self::Input(_) => None,
            Self::TypedInvoke(_) => None,
            Self::CatalogOwnership(_) => None,
            Self::RowsetNormalize(_) => None,
            Self::SessionProvision(_) => None,
            Self::InputAt { span, .. } => Some(span.0 as usize),
        }
    }
}

impl From<PythonSourceError> for PythonLoweringError {
    fn from(error: PythonSourceError) -> Self {
        Self::Source {
            error: std::sync::Arc::new(error),
            span: None,
        }
    }
}

impl From<PythonProgramError> for PythonLoweringError {
    fn from(error: PythonProgramError) -> Self {
        Self::Program(error)
    }
}

impl From<PythonLoweringInvariantError> for PythonLoweringError {
    fn from(error: PythonLoweringInvariantError) -> Self {
        Self::Internal(error)
    }
}

impl From<monty_analysis::AnalysisError> for PythonLoweringError {
    fn from(error: monty_analysis::AnalysisError) -> Self {
        Self::Analysis(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::symbol_tuning::SymbolResolveError> for PythonLoweringError {
    fn from(error: plasm_core::symbol_tuning::SymbolResolveError) -> Self {
        Self::SymbolResolve(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::TypeError> for PythonLoweringError {
    fn from(error: plasm_core::TypeError) -> Self {
        Self::Type(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::plasm_monad::SyntheticResultSchemaError> for PythonLoweringError {
    fn from(error: plasm_core::plasm_monad::SyntheticResultSchemaError) -> Self {
        Self::SyntheticSchema(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::row_plan::contracts::RowContractError> for PythonLoweringError {
    fn from(error: plasm_core::row_plan::contracts::RowContractError) -> Self {
        Self::RowContract(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::value_contract::ValueContractError> for PythonLoweringError {
    fn from(error: plasm_core::value_contract::ValueContractError) -> Self {
        Self::ValueContract(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::value_expression::InferenceError> for PythonLoweringError {
    fn from(error: plasm_core::value_expression::InferenceError) -> Self {
        Self::from(plasm_core::value_contract::ValueContractError::from(error))
    }
}

impl From<crate::map_body_schema::MapBodySchemaError> for PythonLoweringError {
    fn from(error: crate::map_body_schema::MapBodySchemaError) -> Self {
        Self::MapBodySchema(std::sync::Arc::new(error))
    }
}

impl From<PythonComputeRejection> for PythonLoweringError {
    fn from(error: PythonComputeRejection) -> Self {
        Self::Compute(error)
    }
}

impl From<PythonComputeError> for PythonLoweringError {
    fn from(error: PythonComputeError) -> Self {
        Self::ComputeTyped(error)
    }
}

impl From<crate::python_compute::inference::InferenceError> for PythonLoweringError {
    fn from(error: crate::python_compute::inference::InferenceError) -> Self {
        Self::Compute(PythonComputeRejection::from(error))
    }
}

impl From<plasm_core::rowset::RowsetNormalizeError> for PythonLoweringError {
    fn from(error: plasm_core::rowset::RowsetNormalizeError) -> Self {
        Self::RowsetNormalize(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::plasm_monad::CorrelatedBodyError> for PythonLoweringError {
    fn from(error: plasm_core::plasm_monad::CorrelatedBodyError) -> Self {
        Self::Correlated(error)
    }
}

impl From<crate::plasm_plan::PlanValidationError> for PythonLoweringError {
    fn from(error: crate::plasm_plan::PlanValidationError) -> Self {
        Self::PlanValidation(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::plasm_monad::StepIdError> for PythonLoweringError {
    fn from(error: plasm_core::plasm_monad::StepIdError) -> Self {
        Self::StepId(std::sync::Arc::new(error))
    }
}

impl From<crate::plasm_comp_bundle::PlasmCompBundleError> for PythonLoweringError {
    fn from(error: crate::plasm_comp_bundle::PlasmCompBundleError) -> Self {
        Self::CompBundle(std::sync::Arc::new(error))
    }
}

impl From<crate::plasm_step_convert::StepPayloadLiftError> for PythonLoweringError {
    fn from(error: crate::plasm_step_convert::StepPayloadLiftError) -> Self {
        Self::StepPayloadLift(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::plasm_monad::PlanAtomError> for PythonLoweringError {
    fn from(error: plasm_core::plasm_monad::PlanAtomError) -> Self {
        Self::Atom(error)
    }
}

impl From<crate::plasm_dag::RowSuffixLoweringError> for PythonLoweringError {
    fn from(error: crate::plasm_dag::RowSuffixLoweringError) -> Self {
        Self::RowSuffix(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::expr_parser::IterateUntilError> for PythonLoweringError {
    fn from(error: plasm_core::expr_parser::IterateUntilError) -> Self {
        Self::Iteration(error)
    }
}

impl From<crate::plasm_dag::error::DagCompilationError> for PythonLoweringError {
    fn from(error: crate::plasm_dag::error::DagCompilationError) -> Self {
        Self::Dag(std::sync::Arc::new(error))
    }
}

impl From<PythonInputError> for PythonLoweringError {
    fn from(error: PythonInputError) -> Self {
        Self::Input(std::sync::Arc::new(error))
    }
}

impl From<plasm_core::TypedInvokeInputError> for PythonLoweringError {
    fn from(error: plasm_core::TypedInvokeInputError) -> Self {
        Self::TypedInvoke(std::sync::Arc::new(error))
    }
}

impl From<crate::catalog_ownership::CatalogOwnershipError> for PythonLoweringError {
    fn from(error: crate::catalog_ownership::CatalogOwnershipError) -> Self {
        Self::CatalogOwnership(std::sync::Arc::new(error))
    }
}

impl From<crate::plan_session_provisions::SessionProvisionError> for PythonLoweringError {
    fn from(error: crate::plan_session_provisions::SessionProvisionError) -> Self {
        Self::SessionProvision(std::sync::Arc::new(error))
    }
}

impl std::fmt::Display for PythonLoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display_message())
    }
}

#[cfg(test)]
mod python_lowering_error_tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn python_parser_cause_is_preserved() {
        let source = ruff_python_parser::parse_module("class Broken(:").unwrap_err();
        let expected = source.clone();
        let error = PythonLoweringError::parse_error(source);
        let PythonLoweringError::Source { error, .. } = error else {
            panic!("expected a typed source rejection");
        };
        let cause = error.source().expect("parser cause");
        assert_eq!(
            cause.downcast_ref::<ruff_python_parser::ParseError>(),
            Some(&expected)
        );
    }

    #[test]
    fn source_rejection_preserves_metadata_and_span() {
        let error = PythonLoweringError::from(PythonSourceError::UnknownComputeMethod {
            method: "missing_compute".to_owned(),
        })
        .with_span((12, 28));
        assert_eq!(error.span_offset(), Some(12));
        let cause = error.source().expect("source rejection");
        let cause = cause
            .downcast_ref::<std::sync::Arc<PythonSourceError>>()
            .expect("typed source rejection");
        assert!(matches!(
            cause.as_ref(),
            PythonSourceError::UnknownComputeMethod { method }
                if method == "missing_compute"
        ));
        assert!(error.to_string().contains("unknown compute method"));
    }

    #[test]
    fn attaching_span_preserves_foreign_semantic_error() {
        let error = PythonLoweringError::from(
            crate::plasm_dag::error::DagCompilationError::UnknownBinding {
                binding: "missing_rows".to_owned(),
            },
        )
        .with_span((20, 32));
        assert_eq!(error.span_offset(), Some(20));
        let located = error
            .source()
            .and_then(|source| source.downcast_ref::<std::sync::Arc<PythonLoweringError>>())
            .expect("located semantic rejection");
        let cause = located
            .source()
            .and_then(|source| {
                source
                    .downcast_ref::<std::sync::Arc<crate::plasm_dag::error::DagCompilationError>>()
            })
            .expect("retained DAG rejection source");
        assert!(matches!(
            cause.as_ref(),
            crate::plasm_dag::error::DagCompilationError::UnknownBinding { binding }
                if binding == "missing_rows"
        ));
    }

    #[test]
    fn lowering_retains_semantic_source() {
        let error =
            PythonLoweringError::from(PythonLoweringInvariantError::IterationSeedNodeMissing);
        assert!(matches!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<PythonLoweringInvariantError>()),
            Some(PythonLoweringInvariantError::IterationSeedNodeMissing)
        ));
    }

    #[test]
    fn json_encoding_errors_retain_shared_original_sources() {
        let source =
            std::sync::Arc::new(serde_json::from_str::<serde_json::Value>("{").unwrap_err());
        let compute = PythonComputeError::ValueContractLiteralEncoding(source.clone());
        let lowering = PythonLoweringInvariantError::ProjectionAliasSerialization(source.clone());
        assert!(
            matches!(compute.clone(), PythonComputeError::ValueContractLiteralEncoding(retained)
            if std::sync::Arc::ptr_eq(&retained, &source))
        );
        assert!(
            matches!(lowering.clone(), PythonLoweringInvariantError::ProjectionAliasSerialization(retained)
            if std::sync::Arc::ptr_eq(&retained, &source))
        );
        let compute_source = compute
            .source()
            .unwrap()
            .downcast_ref::<std::sync::Arc<serde_json::Error>>()
            .expect("compute retains the shared JSON source");
        assert!(std::sync::Arc::ptr_eq(compute_source, &source));
        let lowering_source = lowering
            .source()
            .unwrap()
            .downcast_ref::<std::sync::Arc<serde_json::Error>>()
            .expect("lowering retains the shared JSON source");
        assert!(std::sync::Arc::ptr_eq(lowering_source, &source));
    }
}
