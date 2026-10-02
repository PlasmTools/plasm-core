//! One navigation constructor, reused in root and correlated expression lowering.
use plasm_agent::plasm_compile::{compile_python_program, PythonRelationOperation};
use plasm_core::{
    plasm_monad::{
        payload::{PlanRelationTraversal, RelationCardinality},
        PlasmComp, PlasmStepPayload,
    },
    symbol_tuning::SymbolRender,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Witness {
    id: String,
    relation: String,
    target: String,
    cardinality: RelationCardinality,
    correlated: bool,
    scoped: bool,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rejection {
    body: String,
    error: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    operation: String,
    premise: String,
    transfer: String,
    law: String,
    witnesses: Vec<Witness>,
    invalid: Vec<Rejection>,
}
fn rules() -> Vec<Rule> {
    let doc = include_str!("../../../../doc-site/docs/reference/python-relation-constructors.md");
    serde_json::from_str(
        doc.split("```plasm-relation-constructors\n")
            .nth(1)
            .unwrap()
            .split("```")
            .next()
            .unwrap(),
    )
    .unwrap()
}
fn relations(comp: &PlasmComp, correlated: bool) -> Vec<(PlanRelationTraversal, bool)> {
    comp.steps
        .values()
        .flat_map(|step| match step {
            PlasmStepPayload::FlatMapRelation(payload) => {
                vec![(payload.relation.clone(), correlated)]
            }
            PlasmStepPayload::MapBody(body) => relations(&body.body, true),
            _ => Vec::new(),
        })
        .collect()
}
fn validate(
    rules: &[Rule],
    evidence: &BTreeMap<String, Vec<(PlanRelationTraversal, bool)>>,
) -> Result<(), String> {
    let expected: BTreeSet<_> = PythonRelationOperation::ALL
        .iter()
        .map(|op| op.name())
        .collect();
    if expected.len() != PythonRelationOperation::ALL.len() {
        return Err("duplicate production name".into());
    }
    let laws = super::literate_contract::parse(include_str!(
        "../../../../doc-site/docs/reference/python-conformance.md"
    ))?;
    let mut actual = BTreeSet::new();
    for rule in rules {
        if !actual.insert(rule.operation.as_str()) {
            return Err("duplicate rule".into());
        }
        if rule.premise.trim().is_empty()
            || rule.transfer.trim().is_empty()
            || !laws.contains_key(&rule.law)
            || rule.witnesses.is_empty()
            || rule.invalid.is_empty()
            || rule
                .invalid
                .iter()
                .any(|n| n.body.trim().is_empty() || n.error.trim().is_empty())
        {
            return Err("missing obligation".into());
        }
        for witness in &rule.witnesses {
            if !evidence.get(&witness.id).is_some_and(|nodes| {
                nodes.iter().any(|(node, correlated)| {
                    node.relation == witness.relation
                        && node.target.entity.as_str() == witness.target
                        && node.target.entry_id.as_str() == super::language_matrix::MATRIX_ENTRY_ID
                        && node.cardinality == witness.cardinality
                        && *correlated == witness.correlated
                        && (!witness.scoped || !node.binding_proofs.is_empty())
                })
            }) {
                return Err("witness does not prove relation contract".into());
            }
        }
    }
    if actual != expected {
        return Err("production/rule mismatch".into());
    }
    Ok(())
}
#[tokio::test]
async fn relation_dispatch_requires_recursive_typed_evidence_and_rejections() {
    use super::{language_matrix as matrix, python};
    let rules = rules();
    let mut evidence = BTreeMap::new();
    for rule in &rules {
        for witness in &rule.witnesses {
            let case = python::cases()
                .find(|case| case.id == witness.id)
                .expect("unknown live witness");
            assert!(case.expect_live_error.is_none());
            let (es, _) = python::parity_context(case, "http://127.0.0.1:1");
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let entity = symbols.entity_sym_for(matrix::MATRIX_ENTRY_ID, "LangItem");
            let source = python::program(
                &python::write_tokens(case.python, &symbols, matrix::MATRIX_ENTRY_ID),
                &entity,
            );
            let bundle = compile_python_program(&es, &source).await.unwrap();
            evidence.insert(
                witness.id.clone(),
                relations(&bundle.artifact().comp, false),
            );
        }
    }
    validate(&rules, &evidence).unwrap();
    assert!(validate(&[], &evidence).is_err());
    let mut changed = rules.clone();
    changed.push(rules[0].clone());
    assert!(validate(&changed, &evidence).is_err());
    for change in ["missing", "target", "cardinality", "scope", "correlated"] {
        let mut changed = rules.clone();
        let witness = &mut changed[0].witnesses[0];
        match change {
            "missing" => witness.id = "missing".into(),
            "target" => witness.target = "Wrong".into(),
            "cardinality" => {
                witness.cardinality = match witness.cardinality {
                    RelationCardinality::One => RelationCardinality::Many,
                    RelationCardinality::Many => RelationCardinality::One,
                }
            }
            "scope" => witness.scoped = true,
            "correlated" => witness.correlated = true,
            _ => unreachable!(),
        }
        assert!(
            validate(&changed, &evidence).is_err(),
            "accepted false {change} witness"
        );
    }
    let mut changed = rules.clone();
    changed[0].invalid.clear();
    assert!(validate(&changed, &evidence).is_err());
    let es = matrix::matrix_execute_session(matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(matrix::MATRIX_ENTRY_ID, "LangItem");
    for rule in rules {
        for invalid in rule.invalid {
            let error = compile_python_program(&es, &python::program(&invalid.body, &entity))
                .await
                .expect_err("invalid relation admitted");
            assert!(
                error.contains(&invalid.error),
                "expected {:?}: {error}",
                invalid.error
            );
        }
    }
}
