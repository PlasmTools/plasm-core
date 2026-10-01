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
    assert!(validate(&missing, "SC", 12)
        .unwrap_err()
        .contains("inventory"));
    let mut dangling = parse(DOCUMENT).unwrap();
    dangling.get_mut("SC-01").unwrap().extends = vec!["invented_row".into()];
    assert!(validate(&dangling, "SC", 12)
        .unwrap_err()
        .contains("unknown"));
    let fabricated = DOCUMENT.replace("scoped_nested_records", "invented_case");
    assert!(validate(&parse(&fabricated).unwrap(), "SC", 12)
        .unwrap_err()
        .contains("unknown matrix witness"));
    let missing_gap = DOCUMENT.replace("\"gap\": \"Required law-specific witnesses are not yet registered in this literate ledger.\"", "\"gap\": null");
    assert!(validate(&parse(&missing_gap).unwrap(), "SC", 12)
        .unwrap_err()
        .contains("explicit gap"));
    let claimed = DOCUMENT.replacen(
        "\"id\": \"SC-01\",",
        "\"id\": \"SC-01\", \"status\": \"covered\",",
        1,
    );
    assert!(parse(&claimed).unwrap_err().contains("unknown field"));
}
