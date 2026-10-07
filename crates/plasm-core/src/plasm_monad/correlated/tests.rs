use super::*;
use crate::plasm_monad::{
    empty_comp, invoke_step_payload, map_step_payload, BindingName, DeriveKind, DerivePayload,
    DeriveTemplate, InputCardinality, OutputName, PlanDataInput, PlasmDataValue, SurfaceKind,
};
use std::collections::BTreeMap;

fn id(s: &str) -> StepId {
    StepId::new(s).unwrap()
}

// Structural fixture only: Render represents a known pure collection reduction.
// Monty payload admission and live correlated execution are separate gates.
fn body() -> CorrelatedBody {
    let mut comp = empty_comp(Some("correlated fixture".into()));
    comp.steps.insert(
        "children".into(),
        invoke_step_payload(
            SurfaceKind::Query,
            EffectClass::Read,
            ResultShape::List,
            "scoped child read",
        ),
    );
    let PlasmStepPayload::Invoke(read) = comp.steps.get_mut("children").unwrap() else {
        panic!("fixture")
    };
    read.ir = Some(crate::plasm_monad::PlanExprIr {
        expr: crate::Expr::Query(
            crate::QueryExpr::filtered(
                "Tag",
                crate::Predicate::eq(
                    "item_id",
                    crate::Value::PlasmInputRef(crate::PlasmInputRef::node_output(
                        "parent",
                        vec!["id".into()],
                    )),
                ),
            )
            .with_capability("tag_query"),
        ),
        projection: None,
        display_expr: None,
    });
    comp.steps.insert(
        "reduced".into(),
        map_step_payload(
            "children",
            ComputeOp::Render {
                columns: vec![OutputName::new("label").unwrap()],
                template: "{{ rows | map(attribute='label') | join('|') }}".into(),
                column_aliases: Default::default(),
                render_bindings: vec![],
            },
            ResultShape::Single,
        ),
    );
    comp.steps.insert(
        "output".into(),
        PlasmStepPayload::Derive(DerivePayload {
            derive: DeriveTemplate {
                kind: DeriveKind::Map,
                source: Some("parent".into()),
                item_binding: Some(BindingName::new("item").unwrap()),
                inputs: vec![PlanDataInput {
                    node: "reduced".into(),
                    alias: "labels".into(),
                    cardinality: InputCardinality::Singleton,
                }],
                value: PlasmDataValue::Object {
                    fields: BTreeMap::from([
                        (
                            "title".into(),
                            PlasmDataValue::BindingSymbol {
                                binding: "item".into(),
                                path: vec!["title".into()],
                            },
                        ),
                        (
                            "labels".into(),
                            PlasmDataValue::NodeSymbol {
                                node: "reduced".into(),
                                alias: "labels".into(),
                                path: vec!["content".into()],
                            },
                        ),
                    ]),
                },
            },
            effect_class: EffectClass::ArtifactRead,
            result_shape: ResultShape::Single,
        }),
    );
    comp.bind.topo = vec![id("children"), id("reduced"), id("output")];
    comp.bind.deps = BTreeMap::from([
        (id("children"), BTreeSet::from([id("parent")])),
        (id("reduced"), BTreeSet::from([id("children")])),
        (id("output"), BTreeSet::from([id("parent"), id("reduced")])),
    ]);
    comp.return_ = PlasmReturn::Step { step: id("output") };
    CorrelatedBody {
        output: ScopedOutput::Record,
        parent: ParentCapture {
            source: id("items"),
            local: id("parent"),
            contract: crate::plasm_monad::CaptureContract::Rows {
                schema: None,
                entity_authority: true,
            },
            entity: PlanQualifiedEntityKey {
                entry_id: "fixture".into(),
                entity: "Item".into(),
            },
        },
        max_parents: NonZeroU32::new(256).unwrap(),
        captures: vec![],
        body: comp,
    }
}

#[test]
fn capture_value_contract_rejects_receiver_authority_on_wire() {
    let value = crate::value_contract::ValueContract::scalar(crate::FieldType::String);
    let wire = serde_json::json!({"kind":"value", "value":value, "entity_authority":true});
    assert!(serde_json::from_value::<super::super::CaptureContract>(wire).is_err());
}

#[test]
fn correlated_body_schedules_capture_read_reduce_construct() {
    let body = body();
    assert_eq!(
        body.execution_layers().unwrap(),
        vec![
            vec![id("children")],
            vec![id("reduced")],
            vec![id("output")]
        ]
    );
    assert!(body.body.bind.execution_layers(&BTreeSet::new()).is_err());
    assert!(!body.body.steps.contains_key("parent"));
    let wire = serde_json::to_string(&body).unwrap();
    let restored: CorrelatedBody = serde_json::from_str(&wire).unwrap();
    assert!(body.semantic_eq(&restored));
    assert_eq!(body.execution_layers(), restored.execution_layers());
}

#[test]
fn correlated_body_rejects_shadowing_scope_escape_and_missing_edges() {
    let mut shadow = body();
    shadow.parent.local = id("children");
    assert!(matches!(
        shadow.execution_layers(),
        Err(CorrelatedBodyError::BindGraph(
            super::super::bind_graph::BindGraphError::CapturedInputShadowed { .. }
        ))
    ));
    let mut escaped = body();
    escaped
        .body
        .bind
        .deps
        .get_mut(&id("children"))
        .unwrap()
        .insert(id("other_parent"));
    assert!(matches!(
        escaped.execution_layers(),
        Err(CorrelatedBodyError::BindGraph(
            super::super::bind_graph::BindGraphError::DependencyEscapesBodyScope { .. }
        ))
    ));
    let mut edge = body();
    edge.body.bind.deps.remove(&id("reduced"));
    assert!(matches!(
        edge.execution_layers(),
        Err(CorrelatedBodyError::UndeclaredDependency { .. })
    ));
    let mut omitted = body();
    omitted.body.bind.topo.pop();
    assert!(matches!(
        omitted.execution_layers(),
        Err(CorrelatedBodyError::StepsTopologyMismatch)
    ));
}

#[test]
fn correlated_body_rejects_cycles_duplicates_and_undeclared_holes() {
    let mut cycle = body();
    cycle
        .body
        .bind
        .deps
        .get_mut(&id("children"))
        .unwrap()
        .insert(id("output"));
    assert!(matches!(
        cycle.execution_layers(),
        Err(CorrelatedBodyError::BindGraph(
            super::super::bind_graph::BindGraphError::CyclicOrUnsatisfiableDependencies
        ))
    ));
    let mut duplicate = body();
    duplicate.body.bind.topo.push(id("output"));
    assert!(matches!(
        duplicate.execution_layers(),
        Err(CorrelatedBodyError::BindGraph(
            super::super::bind_graph::BindGraphError::DuplicateTopologicalStep
        ))
    ));
    let mut hole = body();
    hole.body.bind.holes.insert(
        id("children"),
        vec![crate::plasm_monad::PlasmHoleUse {
            step: id("reduced"),
            alias: "hidden".into(),
        }],
    );
    assert!(matches!(
        hole.execution_layers(),
        Err(CorrelatedBodyError::BindGraph(
            super::super::bind_graph::BindGraphError::InvalidHole { .. }
        ))
    ));
}

#[test]
fn correlated_body_rejects_mutation_even_when_mislabelled_read() {
    let mut plan = body();
    let PlasmStepPayload::Invoke(read) = plan.body.steps.get_mut("children").unwrap() else {
        panic!("fixture")
    };
    read.plan_kind = SurfaceKind::Delete;
    assert!(matches!(
        plan.execution_layers(),
        Err(CorrelatedBodyError::MutationEffectMismatch)
    ));
    read_only_effect_rejected(EffectClass::Write);
    read_only_effect_rejected(EffectClass::SideEffect);
}
fn read_only_effect_rejected(effect: EffectClass) {
    let mut plan = body();
    let PlasmStepPayload::Invoke(read) = plan.body.steps.get_mut("children").unwrap() else {
        panic!("fixture")
    };
    read.effect_class = effect;
    assert!(matches!(
        plan.execution_layers(),
        Err(CorrelatedBodyError::ReadEffectMismatch)
    ));
}

#[test]
fn correlated_body_has_map_cardinality_and_hard_parent_bound() {
    let plan = body();
    for n in [0, 1, 256] {
        plan.check_parent_count(n).unwrap();
    }
    assert!(matches!(
        plan.check_parent_count(257),
        Err(CorrelatedBodyError::ParentCountExceeded { count: 257, .. })
    ));
    plan.check_output_count(1).unwrap();
    for n in [0, 2] {
        assert!(matches!(
            plan.check_output_count(n),
            Err(CorrelatedBodyError::OutputCountMismatch { count }) if count == n
        ));
    }
    let mut plural = body();
    plural.body.return_ = PlasmReturn::Parallel {
        steps: vec![id("output"), id("reduced")],
    };
    assert!(plural.execution_layers().is_err());
    let mut external = body();
    external.body.return_ = PlasmReturn::Step { step: id("parent") };
    assert!(matches!(
        external.execution_layers(),
        Err(CorrelatedBodyError::ReturnOutsideScope)
    ));
}

#[test]
fn correlated_body_review_identity_includes_scope_bound_and_body() {
    let original = body();
    let mut other = original.clone();
    other.body.name = Some("new display name".into());
    other
        .body
        .metadata
        .insert("summary".into(), serde_json::json!("inert"));
    assert!(original.semantic_eq(&other));
    other.parent.entity.entry_id = "other_catalog".into();
    assert!(!original.semantic_eq(&other));
    other = original.clone();
    other.max_parents = NonZeroU32::new(255).unwrap();
    assert!(!original.semantic_eq(&other));
    other = original.clone();
    let PlasmStepPayload::Map(reduce) = other.body.steps.get_mut("reduced").unwrap() else {
        panic!("fixture")
    };
    let ComputeOp::Render { template, .. } = &mut reduce.compute.op else {
        panic!("fixture")
    };
    template.push('!');
    assert!(!original.semantic_eq(&other));
}

#[test]
fn correlated_body_deserialization_cannot_bypass_bounds_or_scope() {
    let mut wire = serde_json::to_value(body()).unwrap();
    wire["max_parents"] = serde_json::json!(0);
    assert!(serde_json::from_value::<CorrelatedBody>(wire).is_err());
    let mut wire = serde_json::to_value(body()).unwrap();
    wire["parent"]["local"] = serde_json::json!("");
    let restored: CorrelatedBody = serde_json::from_value(wire).unwrap();
    assert!(restored.execution_layers().is_err());
}

#[test]
fn correlated_body_checks_operands_not_only_declared_edges() {
    let mut plan = body();
    let PlasmStepPayload::Derive(output) = plan.body.steps.get_mut("output").unwrap() else {
        panic!("fixture")
    };
    output.derive.value = PlasmDataValue::NodeSymbol {
        node: "unrelated".into(),
        alias: "labels".into(),
        path: vec![],
    };
    assert!(matches!(
        plan.execution_layers(),
        Err(CorrelatedBodyError::Scope(
            super::scope::CorrelatedScopeError::UndeclaredDependency { .. }
        ))
    ));
    let mut plan = body();
    let PlasmStepPayload::Derive(output) = plan.body.steps.get_mut("output").unwrap() else {
        panic!("fixture")
    };
    output.derive.value = PlasmDataValue::BindingSymbol {
        binding: "foreign_parent".into(),
        path: vec!["title".into()],
    };
    assert!(matches!(
        plan.execution_layers(),
        Err(CorrelatedBodyError::Scope(
            super::scope::CorrelatedScopeError::BindingEscapesRowScope { .. }
        ))
    ));
}

#[test]
fn correlated_body_cannot_return_entity_preserving_projection_as_synthetic() {
    let mut plan = body();
    plan.body.return_ = PlasmReturn::Step {
        step: id("reduced"),
    };
    let PlasmStepPayload::Map(output) = plan.body.steps.get_mut("reduced").unwrap() else {
        panic!("fixture")
    };
    output.compute.op = ComputeOp::Project {
        fields: BTreeMap::new(),
    };
    assert!(matches!(
        plan.execution_layers(),
        Err(CorrelatedBodyError::OutputHasEntityAuthority)
    ));
}

#[test]
fn correlated_body_rejects_query_operand_from_a_different_scope() {
    let mut plan = body();
    let PlasmStepPayload::Invoke(read) = plan.body.steps.get_mut("children").unwrap() else {
        panic!("fixture")
    };
    let crate::Expr::Query(query) = &mut read.ir.as_mut().unwrap().expr else {
        panic!("fixture")
    };
    query.predicate = Some(crate::Predicate::eq(
        "item_id",
        crate::Value::PlasmInputRef(crate::PlasmInputRef::node_output(
            "different_parent",
            vec!["id".into()],
        )),
    ));
    assert!(matches!(
        plan.execution_layers(),
        Err(CorrelatedBodyError::Scope(
            super::scope::CorrelatedScopeError::AliasEscapesScope { .. }
        ))
    ));
}

#[test]
fn rowset_identity_scope_returns_only_admitted_ports() {
    let mut body = body();
    body.body.steps.clear();
    body.body.bind = Default::default();
    body.body.return_ = PlasmReturn::Step {
        step: body.parent.local.clone(),
    };
    let schema = crate::plasm_monad::SyntheticResultSchema {
        entity: Some("Item".into()),
        fields: vec![],
        optional_fields: Default::default(),
    };
    body.output = ScopedOutput::Rows {
        entity: body.parent.entity.clone(),
        schema: schema.clone(),
        entity_authority: true,
        acknowledgement: false,
    };
    assert!(body.execution_layers().unwrap().is_empty());
    body.captures.push(ScopedCapture {
        source: id("outer"),
        local: id("captured"),
        entity: body.parent.entity.clone(),
        contract: crate::plasm_monad::CaptureContract::Rows {
            schema: Some(schema),
            entity_authority: true,
        },
        singleton: false,
    });
    body.body.return_ = PlasmReturn::Step {
        step: id("captured"),
    };
    assert!(body.execution_layers().unwrap().is_empty());
    body.body.return_ = PlasmReturn::Step { step: id("outer") };
    assert!(
        body.execution_layers().is_err(),
        "outer names cannot escape through the scope return"
    );
    body.body.return_ = PlasmReturn::Step {
        step: body.parent.local.clone(),
    };
    body.output = ScopedOutput::Record;
    assert!(
        body.execution_layers().is_err(),
        "a rowset port is not a synthetic record constructor"
    );
}
