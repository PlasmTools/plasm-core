//! Boundary obligations use the same literate evidence machinery as scoped laws.
use super::literate_contract::{parse, report as summarize, validate};

const DOCUMENT: &str = include_str!("../../../../doc-site/docs/reference/python-conformance.md");

pub(super) fn report() -> String {
    let laws = parse(DOCUMENT).expect("literate boundary laws");
    validate(&laws, "BC", 4).expect("boundary law evidence");
    summarize(&laws, "Semantic boundaries")
}

#[test]
fn semantic_contract_inventory_links_executable_evidence() {
    println!("{}", report());
}

#[test]
fn semantic_contract_rejects_wrong_evidence_roles_and_unknown_properties() {
    let wrong_role = DOCUMENT.replace(
        "\"kind\": \"property\", \"id\": \"get_identity_spelling\"",
        "\"kind\": \"matrix\", \"id\": \"get\"",
    );
    assert!(
        matches!(validate(&parse(&wrong_role).unwrap(), "BC", 4), Err(super::literate_contract::ContractError::WitnessRole { id, role: super::literate_contract::Role::Metamorphic }) if id == "get")
    );
    let unknown = DOCUMENT.replace("get_identity_spelling", "unimplemented_property");
    assert!(
        matches!(parse(&unknown), Err(super::literate_contract::ContractError::Json(source)) if source.classify() == serde_json::error::Category::Data)
    );
    let missing_role = DOCUMENT.replace(
        "\"role\": \"metamorphic\"",
        "\"role\": \"runtime_evidence\"",
    );
    assert!(matches!(
        validate(&parse(&missing_role).unwrap(), "BC", 4),
        Err(super::literate_contract::ContractError::EvidenceRoles { .. })
    ));
}
