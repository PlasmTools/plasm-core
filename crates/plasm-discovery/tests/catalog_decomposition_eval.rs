//! Lexicon discovery eval via shared YAML cases.

use plasm_discovery_eval::{
    default_cases_path, default_catalogs_path, load_cases, load_catalog_entry_ids, load_registry,
    resolve_apis_root, score_lexicon_baseline,
};

#[test]
fn lexicon_cases_score_without_panic() {
    let apis = resolve_apis_root(None);
    if !default_cases_path().is_file() || !apis.join("github/domain.yaml").is_file() {
        return;
    }
    let cases = load_cases(&default_cases_path()).unwrap();
    let ids = load_catalog_entry_ids(&default_catalogs_path()).unwrap();
    let reg = load_registry(&apis, &ids).unwrap();
    for case in &cases {
        score_lexicon_baseline(&reg, case, 24);
    }
}
