use super::*;
use crate::plasm_monad::*;
use crate::{Expr, Value};
use std::collections::BTreeMap;

#[test]
fn structural_identity_v1_is_frozen() {
    assert_eq!(
        CompiledProgramId::from_comp(&empty_comp(None)).as_str(),
        "2e881659bae523416c0fe84edcfea61f3e6d3c9ced7b41f7325846da236c85cd"
    );
}

fn program(reverse: bool) -> PlasmComp {
    let mut fields = indexmap::IndexMap::new();
    let mut entries = vec![
        ("recipient", Value::String("example".into())),
        ("amount", Value::Integer(25)),
    ];
    if reverse {
        entries.reverse();
    }
    fields.extend(entries.into_iter().map(|(k, v)| (k.to_owned(), v)));
    let expr = Expr::Create(crate::CreateExpr {
        capability: "create".into(),
        entity: "Record".into(),
        input: crate::InvokeInputPayload::Raw(Value::Object(fields)),
        catalog_entry_id: Default::default(),
        dotted_receiver: None,
    });
    let mut comp = empty_comp(Some("structural fixture".into()));
    let step = StepId::new("write").unwrap();
    comp.steps.insert(
        step.to_string(),
        PlasmStepPayload::Invoke(InvokePayload {
            plan_kind: SurfaceKind::Create,
            qualified_entity: Some(PlanQualifiedEntityKey {
                entry_id: "fixture".into(),
                entity: "Record".into(),
            }),
            ir: Some(PlanExprIr {
                expr,
                projection: None,
                display_expr: None,
            }),
            ir_template: None,
            projection: vec!["id".into()],
            predicates: vec![],
            page_size: None,
            approval: None,
            display_expr: None,
            effect_class: EffectClass::Write,
            result_shape: ResultShape::MutationResult,
        }),
    );
    comp.bind.topo.push(step.clone());
    comp.return_ = PlasmReturn::Step { step };
    comp
}

fn invoke(comp: &mut PlasmComp) -> &mut InvokePayload {
    let PlasmStepPayload::Invoke(value) = comp.steps.get_mut("write").unwrap() else {
        panic!()
    };
    value
}

#[test]
fn object_order_and_persistence_do_not_change_program_identity() {
    let a = program(false);
    let b = program(true);
    assert_ne!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
    assert_eq!(
        CompiledProgramId::from_comp(&a),
        CompiledProgramId::from_comp(&b)
    );
    for value in [a, b] {
        let wire = serde_json::to_value(&value).unwrap();
        let restored = serde_json::from_value(wire).unwrap();
        assert_eq!(
            CompiledProgramId::from_comp(&value),
            CompiledProgramId::from_comp(&restored)
        );
    }
}

#[test]
fn lowered_typed_inputs_and_restored_operands_have_the_same_identity() {
    let mut comp = program(false);
    let Expr::Create(expr) = &mut invoke(&mut comp).ir.as_mut().unwrap().expr else {
        panic!()
    };
    expr.input = crate::InvokeInputPayload::Typed(crate::typed_invoke::TypedInvokeInput::Object {
        fields: indexmap::IndexMap::from([
            (
                "recipient".into(),
                crate::typed_invoke::TypedInvokeInput::Leaf(
                    crate::typed_literal::TypedLiteral::String("example".into()),
                ),
            ),
            (
                "amount".into(),
                crate::typed_invoke::TypedInvokeInput::Leaf(
                    crate::typed_literal::TypedLiteral::Integer(25),
                ),
            ),
        ]),
        extra: None,
    });
    let restored = serde_json::from_value(serde_json::to_value(&comp).unwrap()).unwrap();
    assert_eq!(
        CompiledProgramId::from_comp(&comp),
        CompiledProgramId::from_comp(&restored)
    );
    assert_eq!(
        CompiledProgramId::from_comp(&comp),
        CompiledProgramId::from_comp(&program(false))
    );
}

#[test]
fn semantics_and_order_are_not_erased() {
    let base = program(false);
    let id = CompiledProgramId::from_comp(&base);
    let mut mutations = Vec::new();
    let mut c = base.clone();
    invoke(&mut c).effect_class = EffectClass::Read;
    mutations.push(c);
    let mut c = base.clone();
    invoke(&mut c).result_shape = ResultShape::Single;
    mutations.push(c);
    let mut c = base.clone();
    invoke(&mut c).projection.push("status".into());
    mutations.push(c);
    let mut c = base.clone();
    invoke(&mut c).qualified_entity.as_mut().unwrap().entry_id = "other".into();
    mutations.push(c);
    let mut c = base.clone();
    invoke(&mut c).predicates.push(PlanPredicate {
        field_path: FieldPath::new(vec!["amount".into()]).unwrap(),
        op: PlanPredicateOp::Lt,
        value: PlasmDataValue::Literal {
            value: crate::operand_binding::ResolvedValue::new(Value::Integer(100)).unwrap(),
        },
    });
    mutations.push(c);
    let mut c = base.clone();
    c.bind.deps.insert(
        StepId::new("write").unwrap(),
        [StepId::new("read").unwrap()].into(),
    );
    mutations.push(c);
    let mut c = base.clone();
    c.return_ = PlasmReturn::Step {
        step: StepId::new("other").unwrap(),
    };
    mutations.push(c);
    for c in mutations {
        assert_ne!(id, CompiledProgramId::from_comp(&c));
    }
    let mut a = base.clone();
    a.bind.topo.push(StepId::new("second").unwrap());
    let mut b = a.clone();
    b.bind.topo.reverse();
    assert_ne!(
        CompiledProgramId::from_comp(&a),
        CompiledProgramId::from_comp(&b)
    );
}

#[test]
fn contracts_and_array_order_participate() {
    use crate::value_contract::{ValueContract, ValueShape};
    let a = ValueContract {
        shape: ValueShape::Record {
            fields: BTreeMap::new(),
        },
        domain: None,
        nullable: false,
    };
    let mut b = a.clone();
    b.nullable = true;
    fn digest(v: &dyn super::walk::Structural) -> Vec<u8> {
        let mut digest = Sha256::new();
        super::walk::hash(v, &mut digest);
        digest.finalize().to_vec()
    }
    assert_ne!(digest(&a), digest(&b));
    let amount = rust_decimal::Decimal::new(2500, 2);
    let usd = Value::Money(crate::money::MoneyValue::new(amount, Some("USD".into())));
    let gbp = Value::Money(crate::money::MoneyValue::new(amount, Some("GBP".into())));
    assert_ne!(digest(&usd), digest(&gbp));
    assert_ne!(
        digest(&Value::PlasmInputRef(crate::PlasmInputRef::node_output(
            "first",
            vec!["id".into()]
        ))),
        digest(&Value::PlasmInputRef(crate::PlasmInputRef::node_output(
            "second",
            vec!["id".into()]
        )))
    );
    assert_ne!(digest(&vec![1_u64, 2]), digest(&vec![2_u64, 1]));
    assert_ne!(
        digest(&vec!["ab".to_owned(), "c".into()]),
        digest(&vec!["a".to_owned(), "bc".into()])
    );
}

#[test]
fn deeply_nested_operands_use_an_explicit_walk_stack() {
    let mut value = Value::Null;
    for _ in 0..10_000 {
        value = Value::Array(vec![value]);
    }
    let mut digest = Sha256::new();
    super::walk::hash(&value, &mut digest);
    // Value's destructor is recursive; dismantle this artificial stress fixture.
    while let Value::Array(mut items) = value {
        value = items.pop().unwrap();
    }
}
