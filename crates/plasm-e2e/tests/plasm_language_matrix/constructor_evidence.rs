//! Shared gate for inventories witnessed by admitted programs and typed IL.
use plasm_agent::plasm_compile::compile_python_program;
use plasm_core::plasm_monad::PlasmComp;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rejection {
    #[serde(default)]
    module: bool,
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
    witnesses: Vec<String>,
    invalid: Vec<Rejection>,
}
fn validate(
    rules: &[Rule],
    evidence: &BTreeMap<String, BTreeSet<String>>,
    inventory: &[&str],
) -> Result<(), super::constructor_registry::build::RegistryError> {
    use super::constructor_registry::build::RegistryError;
    let expected: BTreeSet<_> = inventory.iter().copied().collect();
    if expected.len() != inventory.len() {
        return Err(RegistryError::DuplicateProduction);
    }
    let laws = super::literate_contract::parse(include_str!(
        "../../../../doc-site/docs/reference/python-conformance.md"
    ))?;
    let mut actual = BTreeSet::new();
    for rule in rules {
        if !actual.insert(rule.operation.as_str()) {
            return Err(RegistryError::DuplicateRule {
                operation: rule.operation.clone(),
            });
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
            return Err(RegistryError::MissingObligation {
                operation: rule.operation.clone(),
            });
        }
        for witness in &rule.witnesses {
            if !evidence
                .get(witness)
                .is_some_and(|kinds| kinds.contains(&rule.operation))
            {
                return Err(RegistryError::WitnessOmitsConstructor {
                    id: witness.clone(),
                    operation: rule.operation.clone(),
                });
            }
        }
    }
    if actual != expected {
        return Err(RegistryError::InventoryMismatch);
    }
    Ok(())
}
pub(super) async fn assert_registry(
    document: &str,
    fence: &str,
    inventory: &[&str],
    observe: impl Fn(&PlasmComp, &str) -> BTreeSet<String>,
) {
    use super::{language_matrix as matrix, python};
    let marker = format!("```{fence}\n");
    let rules: Vec<Rule> = serde_json::from_str(
        document
            .split(&marker)
            .nth(1)
            .expect("rule fence")
            .split("```")
            .next()
            .unwrap(),
    )
    .unwrap();
    let mut evidence = BTreeMap::new();
    for rule in &rules {
        for id in &rule.witnesses {
            if evidence.contains_key(id) {
                continue;
            }
            let case = python::cases()
                .find(|case| case.id == id)
                .expect("unknown witness");
            assert!(case.expect_live_error.is_none(), "not positive evidence");
            let (es, _) = python::parity_context(case, "http://127.0.0.1:1");
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let entity = python::witness_entity(case, &symbols);
            let source = python::program(
                &python::write_tokens(case.python, &symbols, matrix::MATRIX_ENTRY_ID),
                &entity,
            );
            let compiled = compile_python_program(&es, &source).await.unwrap();
            let kinds = observe(&compiled.artifact().comp, &source);
            evidence.insert(id.clone(), kinds);
        }
    }
    assert_complete(&rules, &evidence, inventory);
    assert_invalid(rules).await;
}

fn assert_complete(
    rules: &[Rule],
    evidence: &BTreeMap<String, BTreeSet<String>>,
    inventory: &[&str],
) {
    validate(rules, evidence, inventory).unwrap();
    for index in 0..rules.len() {
        let mut changed = rules.to_vec();
        changed.remove(index);
        assert!(validate(&changed, evidence, inventory).is_err());
        let mut changed = rules.to_vec();
        changed[index].witnesses = vec!["missing".into()];
        assert!(validate(&changed, evidence, inventory).is_err());
        let mut changed = rules.to_vec();
        changed[index].invalid.clear();
        assert!(validate(&changed, evidence, inventory).is_err());
    }
    let mut changed = rules.to_vec();
    changed.push(rules[0].clone());
    assert!(validate(&changed, evidence, inventory).is_err());
    for rule in rules {
        for witness in &rule.witnesses {
            let mut false_evidence = evidence.clone();
            false_evidence
                .get_mut(witness)
                .unwrap()
                .remove(&rule.operation);
            assert!(validate(rules, &false_evidence, inventory).is_err());
        }
    }
}

async fn assert_invalid(rules: Vec<Rule>) {
    use super::{language_matrix as matrix, python};
    let mut failures = Vec::new();
    for rule in rules {
        let case = python::cases()
            .find(|case| case.id == rule.witnesses[0])
            .unwrap();
        let (es, _) = python::parity_context(case, "http://127.0.0.1:1");
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity = python::witness_entity(case, &symbols);
        for invalid in rule.invalid {
            let body = python::write_tokens(&invalid.body, &symbols, matrix::MATRIX_ENTRY_ID);
            let source = if invalid.module {
                body.replace("ENTITY", &entity)
                    .replace("E.", &format!("{entity}."))
            } else {
                python::program(&body, &entity)
            };
            match compile_python_program(&es, &source).await {
                Ok(_) => failures.push(format!(
                    "{} admitted invalid program: {source}",
                    rule.operation
                )),
                Err(error) if !error.to_string().contains(&invalid.error) => failures.push(
                    format!("{} expected {:?}: {error}", rule.operation, invalid.error),
                ),
                Err(_) => {}
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Expression families are semantic coverage obligations, not a closed production
/// grammar. The matrix owns their expected values; Monty owns their Python syntax.
pub(super) async fn assert_expression_contracts(
    document: &str,
    fence: &str,
    obligations: &[(&str, &[&str])],
) {
    let rules: Vec<Rule> = serde_json::from_str(
        document
            .split(&format!("```{fence}\n"))
            .nth(1)
            .expect("contract fence")
            .split("```")
            .next()
            .unwrap(),
    )
    .unwrap();
    let mut evidence: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (kind, witnesses) in obligations {
        for witness in *witnesses {
            evidence
                .entry((*witness).into())
                .or_default()
                .insert((*kind).into());
        }
    }
    let inventory = obligations
        .iter()
        .map(|(kind, _)| *kind)
        .collect::<Vec<_>>();
    assert_complete(&rules, &evidence, &inventory);
    super::python::run_python_cases(
        super::python::cases().filter(|case| evidence.contains_key(case.id)),
    )
    .await;
    assert_invalid(rules).await;
}
