//! Exhaustive typed traversal. Adding a field or variant requires an identity decision.
use super::walk::{record, variants, Structural, Walk};
use crate::expr::*;
use crate::plasm_monad::{payload::*, *};
use crate::value_contract::*;
use crate::Predicate;

record!(PlasmComp {
    version,
    name,
    steps,
    bind,
    return_,
    metadata
});
record!(PlasmBindGraph {
    topo,
    deps,
    primary,
    holes
});
record!(PlasmHoleUse { step, alias });
variants!(PlasmReturn { Step { step }, Parallel { steps } });
impl Structural for StepId {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.fields("StepId", &[&self.0]);
    }
}
variants!(PlasmStepPayload { Invoke(v), Pure(v), Map(v), Derive(v), FlatMapRelation(v), FlatMapApply(v), UnfoldUntil(v), MapBody(v) });
record!(InvokePayload {
    plan_kind,
    qualified_entity,
    ir,
    ir_template,
    projection,
    predicates,
    page_size,
    approval,
    display_expr,
    effect_class,
    result_shape
});
record!(PurePayload {
    data,
    recorded_output,
    effect_class,
    result_shape
});
record!(RecordedOutputOrigin {
    run_id,
    entry_id,
    entity,
    labels,
    provenance
});
record!(MapPayload {
    compute,
    effect_class,
    result_shape
});
record!(DerivePayload {
    derive,
    effect_class,
    result_shape
});
record!(FlatMapRelationPayload {
    relation,
    effect_class,
    result_shape
});
record!(FlatMapApplyPayload {
    source,
    item_binding,
    effect_template,
    projection,
    predicates,
    approval,
    effect_class,
    result_shape
});
record!(UnfoldUntilPayload {
    source,
    item_binding,
    effect_template,
    until_predicates,
    until_scope,
    step_scope,
    take,
    seed_ir,
    approval,
    effect_class,
    result_shape
});
variants!(EffectClass {
    Read,
    Write,
    SideEffect,
    ArtifactRead
});
variants!(ResultShape {
    List,
    Single,
    MutationResult,
    SideEffectAck,
    Page,
    Artifact
});
variants!(SurfaceKind {
    Query,
    Search,
    Get,
    Create,
    Update,
    Delete,
    Action
});
record!(EffectTemplate {
    kind,
    qualified_entity,
    expr_template,
    ir_template,
    effect_class,
    result_shape,
    projection,
    input_bindings
});
record!(DeriveTemplate {
    kind,
    source,
    item_binding,
    inputs,
    value
});
variants!(DeriveKind { Cell, Map, Data });
record!(PlanQualifiedEntityKey { entry_id, entity });
record!(PlanExprIr {
    expr,
    projection,
    display_expr
});
record!(PlanExprTemplate {
    expr,
    projection,
    display_expr,
    input_bindings
});
record!(PlanRelationTraversal {
    source,
    relation,
    target,
    cardinality,
    source_cardinality,
    expr,
    ir,
    binding_proofs,
    materialize,
    view_embed_proof
});
variants!(RelationCardinality { One, Many });
variants!(RelationSourceCardinality {
    Single,
    Many,
    RuntimeCheckedSingleton
});
record!(ComputeTemplate {
    source,
    op,
    schema,
    page_size,
    collection_alias
});
variants!(ComputeOp {
    NarrowEntity { source_at_most_one, target },
    Python { source, entry_id, entity, catalog_hash, contract_version, language_profile, input_schema, output_type, per_row, selection },
    Project { fields }, Filter { predicates }, GroupBy { keys, aggregates },
    Aggregate { aggregates }, Sort { key, descending }, Limit { count },
    DedupeBy { keys }, With { columns }, Union { other }, MergeBranches { other },
    Render { columns, template, column_aliases, render_bindings }
});
record!(PythonRowSelection { owner, ports });
record!(PythonSelectionPort {
    parameter,
    source,
    packet_field
});
record!(AggregateSpec {
    name,
    function,
    field
});
variants!(AggregateFunction {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    First,
    Last
});
record!(SyntheticResultSchema {
    optional_fields,
    entity,
    fields
});
record!(SyntheticFieldSchema {
    value_type,
    name,
    value_kind,
    source
});
variants!(SyntheticValueKind {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
    Money,
    Temporal,
    EntityRef,
    Duration,
    Unknown
});
record!(ParentCapture {
    source,
    local,
    entity,
    contract
});
record!(CorrelatedBody {
    parent,
    output,
    max_parents,
    captures,
    body
});
record!(ScopedCapture {
    source,
    local,
    entity,
    contract,
    singleton
});
variants!(ScopedOutput { Record, Filter, Quantify { all }, Rows { entity, schema, entity_authority, acknowledgement } });
variants!(CaptureContract { Value { value }, Rows { schema, entity_authority } });
variants!(PlasmDataValue {
    Quantified { all, collection, binding, predicate }, Expression { expression },
    Literal { value }, BindingSymbol { binding, path }, NodeSymbol { node, alias, path },
    Symbol { path }, Template { template, input_bindings }, EntityRefKey { api, entity, key },
    Array { items }, Object { fields }
});
record!(PlanPredicate {
    field_path,
    op,
    value
});
variants!(PlanPredicateOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    In,
    NotIn,
    Exists
});
record!(PlanDataInput {
    node,
    alias,
    cardinality
});
record!(PlanInputBinding { from, to });
variants!(InputCardinality {
    Acknowledgement,
    Collection,
    Auto,
    Singleton
});
record!(ValueContract {
    shape,
    domain,
    nullable
});
record!(DomainRef {
    entry_id,
    catalog_hash,
    value_ref
});
record!(TupleContract {
    prefix,
    variable,
    suffix
});
variants!(ValueShape {
    MappingRecord { record }, Set { element }, Temporal { kind, wire }, Scalar { field_type },
    Array { element }, Tuple { members }, Dictionary { key, value }, Record { fields },
    ObservedRecord { fields, optional_fields }, Null, Never, Union { variants }
});
record!(WithColumn { name, expr });
variants!(WithExpr { Field(v), Literal(v), Arith { op, lhs, rhs }, Now, Len { field }, When { lhs, op, rhs, then, else_ } });
variants!(ArithOp { Add, Sub, Mul, Div });
variants!(WithLiteral { Null, Bool(v), Integer(v), Number(v), String(v) });
variants!(Expr { Query(v), Get(v), Create(v), Delete(v), Invoke(v), Chain(v), Page(v), Wait(v), Cancel(v), TeachingValue { value } });
record!(QueryExpr {
    entity,
    predicate,
    projection,
    pagination,
    hydrate,
    capability_name,
    catalog_entry_id
});
record!(QueryPagination {
    offset,
    page,
    cursor,
    from_block,
    to_block
});
record!(GetExpr {
    reference,
    catalog_entry_id,
    capability_name,
    input
});
record!(CreateExpr {
    capability,
    entity,
    input,
    catalog_entry_id,
    dotted_receiver
});
record!(DeleteExpr {
    capability,
    target,
    input,
    catalog_entry_id
});
record!(InvokeExpr {
    capability,
    target,
    input,
    catalog_entry_id
});
record!(ChainExpr {
    source,
    selector,
    catalog_entry_id,
    step
});
variants!(ChainStep { AutoGet, Explicit { expr } });
record!(PageExpr { handle, limit });
record!(WaitExpr { handle });
record!(CancelExpr { handle });
record!(Ref { entity_type, key });
variants!(EntityKey { Simple(v), Compound(v) });
variants!(IdentitySlot { Lit(v), Binding(v) });
variants!(Predicate { True, False, Comparison { field, op, value }, And { args }, Or { args }, Not { predicate }, ExistsRelation { relation, predicate } });

impl<T: Structural> Structural for crate::boolean_expr::BooleanExpr<T> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        match self {
            Self::Atom(v) => w.fields("Boolean::Atom", &[v]),
            Self::Not(v) => w.fields("Boolean::Not", &[v]),
            Self::And(v) => w.fields("Boolean::And", &[v]),
            Self::Or(v) => w.fields("Boolean::Or", &[v]),
        }
    }
}
impl<T: Structural> Structural for crate::value_expression::ValueOperation<T> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        use crate::value_expression::ValueOperation::*;
        match self {
            Field { value, name } => w.fields("Operation::Field", &[value, name]),
            Arithmetic {
                operator,
                left,
                right,
            } => w.fields("Operation::Arithmetic", &[operator, left, right]),
            Length { value } => w.fields("Operation::Length", &[value]),
            Refine { value, contract } => w.fields("Operation::Refine", &[value, contract]),
            Compare {
                operator,
                left,
                right,
            } => w.fields("Operation::Compare", &[operator, left, right]),
            Choose {
                condition,
                then,
                otherwise,
            } => w.fields("Operation::Choose", &[condition, then, otherwise]),
            And { left, right } => w.fields("Operation::And", &[left, right]),
            Or { left, right } => w.fields("Operation::Or", &[left, right]),
            Not { value } => w.fields("Operation::Not", &[value]),
        }
    }
}
