use super::super::*;
use plasm_core::load_schema;
use plasm_core::CgsContext;
use plasm_core::TeachingExposureSession;
use std::path::PathBuf;

pub(super) fn test_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        load_schema(&root.join("tests/fixtures/execute_tiny")).expect("load execute_tiny"),
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "acme".into(),
        Arc::new(CgsContext::entry("acme", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(cgs.as_ref(), "acme", &["Product", "Category"]);
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "acme".into(),
        String::new(),
        String::new(),
        None,
        vec!["Product".into(), "Category".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

pub(super) fn duplicate_product_create_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs_base = load_schema(&root.join("tests/fixtures/scoped_create_tiny"))
        .expect("load scoped_create_tiny");
    let cgs_acme = Arc::new({
        let mut c = cgs_base.clone();
        c.bind_registry_entry_id("acme");
        c
    });
    let cgs_other = Arc::new({
        let mut c = cgs_base;
        c.bind_registry_entry_id("other");
        c
    });
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "acme".into(),
        Arc::new(CgsContext::entry("acme", cgs_acme.clone())),
    );
    ctxs.insert(
        "other".into(),
        Arc::new(CgsContext::entry("other", cgs_other.clone())),
    );
    let mut exp = TeachingExposureSession::new(cgs_acme.as_ref(), "acme", &["Product"]);
    exp.expose_entities(
        &[cgs_acme.as_ref(), cgs_other.as_ref()],
        cgs_other.clone(),
        "other",
        &["Product"],
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs_acme.clone(),
        ctxs,
        "acme".into(),
        String::new(),
        String::new(),
        None,
        vec!["Product".into()],
        Some(exp),
        None,
        cgs_acme.catalog_cgs_hash_hex(),
        None,
    )
}

pub(super) fn federated_langmatrix_item_session() -> Option<ExecuteSession> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let matrix_dir = root.join("../../fixtures/schemas/plasm_language_matrix");
    let mut cgs_a = load_schema(&matrix_dir).ok()?;
    cgs_a.bind_registry_entry_id("langmatrix_a");
    let mut cgs_b = load_schema(&matrix_dir).ok()?;
    cgs_b.bind_registry_entry_id("langmatrix_b");
    let cgs_a = Arc::new(cgs_a);
    let cgs_b = Arc::new(cgs_b);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix_a".into(),
        Arc::new(CgsContext::entry("langmatrix_a", cgs_a.clone())),
    );
    ctxs.insert(
        "langmatrix_b".into(),
        Arc::new(CgsContext::entry("langmatrix_b", cgs_b.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs_a.as_ref(), cgs_b.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs_a.as_ref(), "langmatrix_a", &["LangItem"]);
    exp.expose_entities(&layers, cgs_b.clone(), "langmatrix_b", &["LangItem"]);
    Some(ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs_a.clone(),
        ctxs,
        "langmatrix_a".into(),
        String::new(),
        String::new(),
        None,
        vec!["LangItem".into()],
        Some(exp),
        None,
        cgs_a.catalog_cgs_hash_hex(),
        None,
    ))
}

pub(super) fn federated_langmatrix_item_team_session() -> Option<ExecuteSession> {
    federated_langmatrix_item_session()
}

pub(super) fn language_matrix_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = Arc::new(load_schema(&dir).expect("load plasm_language_matrix"));
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    );
    let wave: &[&str] = &["LangItem", "LangLine", "LangTag"];
    let exp = TeachingExposureSession::new(cgs.as_ref(), "langmatrix", wave);
    ExecuteSession::new(
        "matrix_ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "langmatrix".into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| (*s).to_string()).collect(),
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

pub(super) fn matrix_views_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix_views"))
            .expect("load plasm_language_matrix_views"),
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix_views".into(),
        Arc::new(CgsContext::entry("langmatrix_views", cgs.clone())),
    );
    let wave: &[&str] = &[
        "LangItem",
        "LangTag",
        "LangTriageContext",
        "LangDigest",
        "LangItemLink",
        "LangOwnerFilterDemo",
    ];
    let exp = TeachingExposureSession::new(cgs.as_ref(), "langmatrix_views", wave);
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "langmatrix_views".into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| (*s).to_string()).collect(),
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}
