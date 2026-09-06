use plasm_core::loader::load_schema_dir;
use std::path::Path;

#[test]
fn validate_list_cardinality_cutover_catalogs() {
    let catalogs = [
        "../../apis/appworld/spotify",
        "../../apis/appworld/venmo",
        "../../apis/appworld/gmail",
        "../../apis/appworld/phone",
        "../../apis/appworld/splitwise",
        "../../apis/hackernews",
        "../../apis/openbrewerydb",
        "../../apis/tau3_banking",
        "../../apis/tavily",
        "../../apis/jira",
        "../../apis/grafana",
        "../../apis/linkedin",
        "../../apis/slack",
        "../../apis/gitlab",
        "../../apis/outlook",
        "../../apis/discord",
        "../../apis/dnd5e",
        "../../apis/architect-exchange",
        "../../apis/github",
        "../../apis/clickup",
        "../../apis/twitter",
        "../../fixtures/schemas/plasm_language_matrix",
        "../../fixtures/schemas/plasm_language_matrix_views",
    ];
    let mut failures = Vec::new();
    for rel in catalogs {
        let p = Path::new(rel);
        if !p.exists() {
            continue;
        }
        if let Err(e) = load_schema_dir(p) {
            failures.push(format!("{rel}: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "catalog load failures ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
