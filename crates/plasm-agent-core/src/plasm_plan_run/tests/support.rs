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
        None,
    )
}

pub(super) fn duplicate_product_create_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs_base = load_schema(&root.join("tests/fixtures/scoped_create_tiny"))
        .expect("load scoped_create_tiny");
    let cgs_acme = Arc::new({
        let mut c = cgs_base.clone();
        c.entry_id = Some("acme".into());
        c
    });
    let cgs_other = Arc::new({
        let mut c = cgs_base;
        c.entry_id = Some("other".into());
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
        None,
    )
}

pub(super) fn federated_github_linear_issue_session() -> Option<ExecuteSession> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let github_dir = root.join("../../apis/github");
    let linear_dir = root.join("../../apis/linear");
    if !github_dir.is_dir() || !linear_dir.is_dir() {
        return None;
    }
    let cgs_github = Arc::new(load_schema(&github_dir).ok()?);
    let cgs_linear = Arc::new(load_schema(&linear_dir).ok()?);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "github".into(),
        Arc::new(CgsContext::entry("github", cgs_github.clone())),
    );
    ctxs.insert(
        "linear".into(),
        Arc::new(CgsContext::entry("linear", cgs_linear.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs_github.as_ref(), cgs_linear.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs_github.as_ref(), "github", &["Issue"]);
    exp.expose_entities(&layers, cgs_linear.clone(), "linear", &["Issue"]);
    Some(ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs_github.clone(),
        ctxs,
        "github".into(),
        String::new(),
        String::new(),
        None,
        vec!["Issue".into()],
        Some(exp),
        None,
        cgs_github.catalog_cgs_hash_hex(),
        None,
        None,
    ))
}

/// Federated pokeapi (read) + linear (Issue create, Team) session for the federated-write smoke.
pub(super) fn federated_pokeapi_linear_write_session() -> Option<ExecuteSession> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pokeapi_dir = root.join("../../apis/pokeapi");
    let linear_dir = root.join("../../apis/linear");
    if !pokeapi_dir.is_dir() || !linear_dir.is_dir() {
        return None;
    }
    let cgs_pokeapi = Arc::new(load_schema(&pokeapi_dir).ok()?);
    let cgs_linear = Arc::new(load_schema(&linear_dir).ok()?);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "pokeapi".into(),
        Arc::new(CgsContext::entry("pokeapi", cgs_pokeapi.clone())),
    );
    ctxs.insert(
        "linear".into(),
        Arc::new(CgsContext::entry("linear", cgs_linear.clone())),
    );
    let layers: Vec<&plasm_core::CGS> = vec![cgs_pokeapi.as_ref(), cgs_linear.as_ref()];
    let mut exp = TeachingExposureSession::new(cgs_pokeapi.as_ref(), "pokeapi", &["Pokemon"]);
    exp.expose_entities(&layers, cgs_linear.clone(), "linear", &["Issue", "Team"]);
    Some(ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs_pokeapi.clone(),
        ctxs,
        "pokeapi".into(),
        String::new(),
        String::new(),
        None,
        vec!["Pokemon".into()],
        Some(exp),
        None,
        cgs_pokeapi.catalog_cgs_hash_hex(),
        None,
        None,
    ))
}

pub(super) fn federated_github_linear_issue_team_session() -> Option<ExecuteSession> {
    let mut session = federated_github_linear_issue_session()?;
    let linear = session.contexts_by_entry.get("linear")?.cgs.clone();
    let layers: Vec<&plasm_core::CGS> = session
        .contexts_by_entry
        .values()
        .map(|c| c.cgs.as_ref())
        .collect();
    let exp = session.teaching_exposure.as_mut()?;
    exp.expose_entities(&layers, linear, "linear", &["Team"]);
    Some(session)
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
        None,
    )
}
