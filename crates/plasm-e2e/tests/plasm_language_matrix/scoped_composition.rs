//! Normative Markdown is the law ledger; there is no parallel JSON/status file.
use super::literate_contract::{parse, report as summarize, validate};

const DOCUMENT: &str =
    include_str!("../../../../doc-site/docs/reference/python-scoped-composition.md");

pub(super) fn report() -> String {
    let laws = parse(DOCUMENT).expect("literate scoped laws");
    validate(&laws, "SC", 12).expect("scoped law evidence");
    summarize(&laws, "Scoped composition")
}

#[test]
fn scoped_composition_inventory_links_original_obligations() {
    println!("{}", report());
}

#[test]
fn scoped_composition_inventory_rejects_omissions_and_unproved_claims() {
    let mut missing = parse(DOCUMENT).unwrap();
    missing.remove("SC-01");
    assert!(
        matches!(validate(&missing, "SC", 12), Err(super::literate_contract::ContractError::Inventory { prefix, count: 12 }) if prefix == "SC")
    );
    let mut dangling = parse(DOCUMENT).unwrap();
    dangling.get_mut("SC-01").unwrap().extends = vec!["invented_row".into()];
    assert!(
        matches!(validate(&dangling, "SC", 12), Err(super::literate_contract::ContractError::OriginalLinks { law }) if law == "SC-01")
    );
    let fabricated = DOCUMENT.replace("scoped_nested_records", "invented_case");
    assert!(
        matches!(validate(&parse(&fabricated).unwrap(), "SC", 12), Err(super::literate_contract::ContractError::UnknownWitness { id }) if id == "invented_case")
    );
    let missing_gap = DOCUMENT.replace("\"gap\": \"Required law-specific witnesses are not yet registered in this literate ledger.\"", "\"gap\": null");
    assert!(matches!(
        validate(&parse(&missing_gap).unwrap(), "SC", 12),
        Err(super::literate_contract::ContractError::Obligation { .. })
    ));
    let claimed = DOCUMENT.replacen(
        "\"id\": \"SC-01\",",
        "\"id\": \"SC-01\", \"status\": \"covered\",",
        1,
    );
    assert!(
        matches!(parse(&claimed), Err(super::literate_contract::ContractError::Json(source)) if source.classify() == serde_json::error::Category::Data)
    );
}
