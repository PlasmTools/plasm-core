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
    assert!(validate(&parse(&wrong_role).unwrap(), "BC", 4)
        .unwrap_err()
        .contains("cannot witness"));
    let unknown = DOCUMENT.replace("get_identity_spelling", "unimplemented_property");
    assert!(parse(&unknown).unwrap_err().contains("unknown variant"));
    let missing_role = DOCUMENT.replace(
        "\"role\": \"metamorphic\"",
        "\"role\": \"runtime_evidence\"",
    );
    assert!(validate(&parse(&missing_role).unwrap(), "BC", 4)
        .unwrap_err()
        .contains("four evidence roles"));
}
