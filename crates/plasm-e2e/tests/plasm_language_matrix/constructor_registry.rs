//! Row witnesses inspect typed IL; spelling alone is not execution evidence.
use plasm_agent::plasm_compile::{compile_python_program, PythonRowOperation};
use plasm_core::symbol_tuning::SymbolRender;
use serde::Deserialize;
use std::collections::BTreeSet;

#[tokio::test]
async fn production_row_dispatch_requires_typed_rules_and_negative_admission() {
    super::constructor_evidence::assert_registry(
        include_str!("../../../../doc-site/docs/reference/python-row-constructors.md"),
        "plasm-constructors",
        &PythonRowOperation::ALL
            .iter()
            .map(|op| op.name())
            .collect::<Vec<_>>(),
        |comp, _| row_operations(comp),
    )
    .await;
}

fn row_operations(comp: &plasm_core::plasm_monad::PlasmComp) -> BTreeSet<String> {
    use plasm_core::plasm_monad::{ComputeOp, PlasmStepPayload, ScopedOutput};
    let mut found = BTreeSet::new();
    for step in comp.steps.values() {
        let operation = match step {
            PlasmStepPayload::Map(p) => match &p.compute.op {
                ComputeOp::Project { .. } => Some("select"),
                ComputeOp::Filter { .. } => Some("where"),
                ComputeOp::GroupBy { .. } => Some("group_by"),
                ComputeOp::Aggregate { .. } => Some("aggregate"),
                ComputeOp::Sort { .. } => Some("order_by"),
                ComputeOp::Limit { .. } => Some("take"),
                ComputeOp::DedupeBy { .. } => Some("distinct"),
                ComputeOp::Union { .. } => Some("union"),
                _ => None,
            },
            PlasmStepPayload::Invoke(p) if p.page_size.is_some_and(|size| size > 0) => {
                Some("page_size")
            }
            PlasmStepPayload::UnfoldUntil(_) => Some("iterate"),
            PlasmStepPayload::MapBody(body) => {
                found.extend(row_operations(&body.body));
                match body.output {
                    ScopedOutput::Record => Some("map"),
                    ScopedOutput::Filter => Some("where"),
                    ScopedOutput::Quantify { .. } => None,
                    ScopedOutput::Rows { .. } => Some("flat_map"),
                }
            }
            _ => None,
        };
        if let Some(operation) = operation {
            found.insert(operation.into());
        }
    }
    found
}

mod build {
    use super::*;
    use plasm_agent::plasm_compile::PythonBuildStatement;
    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Rejection {
        body: String,
        error: String,
    }
    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct BuildRule {
        operation: String,
        premise: String,
        transfer: String,
        law: String,
        witnesses: Vec<String>,
        invalid: Vec<Rejection>,
    }
    fn rules() -> Vec<BuildRule> {
        let doc = include_str!("../../../../doc-site/docs/reference/python-build-constructors.md");
        serde_json::from_str(
            doc.split("```plasm-build-constructors\n")
                .nth(1)
                .unwrap()
                .split("```")
                .next()
                .unwrap(),
        )
        .unwrap()
    }
    fn program(body: &str) -> String {
        if body.starts_with("class ") {
            return body.into();
        }
        format!(
            "class BuildCase(Program):\n    def build(self):\n{}\n",
            body.lines()
                .map(|line| format!("        {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
    fn validate(rules: &[BuildRule]) -> Result<(), String> {
        let expected: BTreeSet<_> = PythonBuildStatement::ALL
            .iter()
            .map(|op| op.name())
            .collect();
        if expected.len() != PythonBuildStatement::ALL.len() {
            return Err("duplicate production name".into());
        }
        let laws = super::super::literate_contract::parse(include_str!(
            "../../../../doc-site/docs/reference/python-conformance.md"
        ))?;
        let mut actual = BTreeSet::new();
        for rule in rules {
            if !actual.insert(rule.operation.as_str()) {
                return Err("duplicate rule".into());
            }
            if rule.premise.trim().is_empty()
                || rule.transfer.trim().is_empty()
                || rule.witnesses.is_empty()
                || rule.invalid.is_empty()
                || rule
                    .invalid
                    .iter()
                    .any(|n| n.body.trim().is_empty() || n.error.trim().is_empty())
                || !laws.contains_key(&rule.law)
            {
                return Err("missing obligation".into());
            }
            for id in &rule.witnesses {
                let case = super::super::python::cases()
                    .find(|case| case.id == id)
                    .ok_or("dangling witness")?;
                if case.expect_live_error.is_some() {
                    return Err("not positive evidence".into());
                }
                if !PythonBuildStatement::inventory(&program(case.python))?
                    .iter()
                    .any(|kind| kind.name() == rule.operation)
                {
                    return Err("witness omits constructor".into());
                }
            }
        }
        if actual != expected {
            return Err("production/rule mismatch".into());
        }
        Ok(())
    }
    #[tokio::test]
    async fn production_build_dispatch_requires_rules_and_negative_admission() {
        let rules = rules();
        validate(&rules).unwrap();
        let es = super::super::language_matrix::matrix_execute_session(
            super::super::language_matrix::load_language_matrix_cgs(),
        );
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity =
            symbols.entity_sym_for(super::super::language_matrix::MATRIX_ENTRY_ID, "LangItem");
        for rule in rules {
            for rejection in rule.invalid {
                let source = program(&rejection.body.replace("E.", &format!("{entity}.")));
                let error = compile_python_program(&es, &source)
                    .await
                    .expect_err("invalid build admitted");
                assert!(
                    error.contains(&rejection.error),
                    "{} expected {:?}: {error}",
                    rule.operation,
                    rejection.error
                );
            }
        }
    }
    #[tokio::test]
    async fn documentation_erasure_preserves_the_semantic_plan() {
        let es = super::super::language_matrix::matrix_execute_session(
            super::super::language_matrix::load_language_matrix_cgs(),
        );
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity =
            symbols.entity_sym_for(super::super::language_matrix::MATRIX_ENTRY_ID, "LangItem");
        let body = format!("rows = {entity}.get('i1')\nreturn rows");
        let plain = compile_python_program(&es, &program(&body)).await.unwrap();
        let documented = compile_python_program(&es, &program(&format!("'documentation'\n{body}")))
            .await
            .unwrap();
        assert!(plasm_core::plasm_monad::comp_semantic_eq(
            &plain.artifact().comp,
            &documented.artifact().comp
        ));
    }

    #[test]
    fn build_inventory_rejects_missing_rules_and_false_witnesses() {
        let all = rules();
        validate(&all).unwrap();
        for index in 0..all.len() {
            let mut changed = all.clone();
            changed.remove(index);
            assert!(validate(&changed).is_err());
            let mut changed = all.clone();
            changed[index].witnesses = vec!["missing".into()];
            assert!(validate(&changed).is_err());
            let mut changed = all.clone();
            changed[index].invalid.clear();
            assert!(validate(&changed).is_err());
        }
        let mut changed = all.clone();
        changed.push(all[0].clone());
        assert!(validate(&changed).is_err());
        let mut changed = all.clone();
        changed[0].witnesses = vec!["get".into()];
        assert!(validate(&changed).is_err());
        let mut changed = all;
        changed[0].operation = "unknown".into();
        assert!(validate(&changed).is_err());
    }
}
