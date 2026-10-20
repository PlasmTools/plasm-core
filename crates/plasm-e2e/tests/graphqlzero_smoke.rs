//! GraphQLZero schema loads and CML templates parse (no network).

use std::path::PathBuf;

#[test]
fn graphqlzero_mappings_parse() {
    let root = PathBuf::from(plasm_e2e::test_manifest_dir());
    let dir = root.join("../../apis/graphqlzero");
    let cgs = plasm_core::load_schema(&dir).expect("load apis/graphqlzero");
    cgs.validate().expect("CGS validate");
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability templates");
}
