//! Linear packaged-catalog validation plus fixture-backed Python search/view admission.

use std::path::PathBuf;
use std::sync::Arc;

use plasm_agent::{execute_session::ExecuteSession, plasm_compile::compile_python_program};
use plasm_core::loader::load_schema_dir;
use plasm_core::normalize_expr_query_capabilities;
use plasm_core::resolve_query_capability;
use plasm_core::Expr;
use plasm_core::Predicate;
use plasm_core::QueryExpr;
use plasm_core::{CgsContext, TeachingExposureSession};

const FIXTURE_ENTRY: &str = "smoke_matrix";

fn fixture_cgs(name: &str) -> Arc<plasm_core::CGS> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas")
        .join(name);
    Arc::new(load_schema_dir(&dir).unwrap_or_else(|error| panic!("{name}: {error}")))
}

fn fixture_session(cgs: Arc<plasm_core::CGS>, entities: &[&str]) -> ExecuteSession {
    let mut contexts = indexmap::IndexMap::new();
    contexts.insert(
        FIXTURE_ENTRY.into(),
        Arc::new(CgsContext::entry(FIXTURE_ENTRY, cgs.clone())),
    );
    let exposure = TeachingExposureSession::new(&cgs, FIXTURE_ENTRY, entities);
    ExecuteSession::new(
        "linear_smoke_fixture".into(),
        String::new(),
        cgs.clone(),
        contexts,
        FIXTURE_ENTRY.into(),
        String::new(),
        String::new(),
        None,
        entities.iter().map(|entity| (*entity).into()).collect(),
        Some(exposure),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

fn linear_cgs() -> plasm_core::CGS {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../apis/linear");
    load_schema_dir(&dir).expect("load apis/linear")
}

#[test]
fn linear_mappings_parse() {
    let cgs = linear_cgs();
    cgs.validate().expect("CGS validate");
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability templates");
}

#[test]
fn fixture_search_and_views_admit_python() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(fixture_search_and_views_admit_python_async());
        })
        .expect("spawn fixture admission")
        .join()
        .expect("join fixture admission");
}

async fn fixture_search_and_views_admit_python_async() {
    let search_session = fixture_session(fixture_cgs("plasm_language_matrix"), &["LangItem"]);
    let search_symbols = search_session
        .teaching_exposure
        .as_ref()
        .unwrap()
        .symbol_map_arc();
    let item = search_symbols.entity_sym_for(FIXTURE_ENTRY, "LangItem");
    let source = format!("class Search(Program):\n    def build(self):\n        return {item}.search(q=\"bug\", team_key=\"ENG\")\n");
    let search = compile_python_program(&search_session, &source)
        .await
        .unwrap_or_else(|error| panic!("fixture search admission: {error}"));
    let query = search
        .artifact()
        .comp
        .steps
        .values()
        .find_map(|step| {
            let plasm_core::plasm_monad::PlasmStepPayload::Invoke(invoke) = step else {
                return None;
            };
            let Expr::Query(query) = &invoke.ir.as_ref()?.expr else {
                return None;
            };
            Some(query)
        })
        .expect("search query IR");
    assert_eq!(query.capability_name.as_deref(), Some("langitem_search"));

    // Preserve keyed composed reads, pathless dashboards, their view_embed edge,
    // and independent keyed reads without coupling language admission to apis/linear.
    let views = fixture_session(
        fixture_cgs("plasm_language_matrix_views"),
        &["LangDigest", "LangWorkSnapshot", "LangKeyPick", "LangItem"],
    );
    let symbols = views.teaching_exposure.as_ref().unwrap().symbol_map_arc();
    let digest = symbols.entity_sym_for(FIXTURE_ENTRY, "LangDigest");
    let snapshot = symbols.entity_sym_for(FIXTURE_ENTRY, "LangWorkSnapshot");
    let keyed = symbols.entity_sym_for(FIXTURE_ENTRY, "LangKeyPick");
    let items = symbols.ident_sym_relation_for(FIXTURE_ENTRY, "LangWorkSnapshot", "items");
    let edge = &views.cgs.get_entity("LangWorkSnapshot").unwrap().relations["items"];
    assert!(matches!(
        edge.materialize,
        Some(plasm_core::RelationMaterialization::ViewEmbed { .. })
    ));
    for (label, expression) in [
        ("keyed context", format!("{digest}.get(\"ENG-42\")")),
        ("dashboard", format!("{snapshot}.query()")),
        ("dashboard relation", format!("{snapshot}.get().{items}")),
        ("keyed read", format!("{keyed}.get(\"ENG\")")),
    ] {
        let source =
            format!("class Views(Program):\n    def build(self):\n        return {expression}\n");
        let bundle = compile_python_program(&views, &source)
            .await
            .unwrap_or_else(|error| panic!("{label} admission: {error}"));
        plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry(&views, &bundle)
            .unwrap_or_else(|error| panic!("{label} dry plan: {error}"));
    }
}

#[test]
fn linear_view_embed_materialize_declared() {
    let cgs = linear_cgs();
    let snap = cgs.get_entity("MyWorkSnapshot").expect("MyWorkSnapshot");
    let issues = snap.relations.get("issues").expect("issues");
    assert!(matches!(
        issues.materialize,
        Some(plasm_core::RelationMaterialization::ViewEmbed { .. })
    ));
}

#[test]
fn fixture_filters_resolve_search() {
    // Filter-only search must not depend on a free-text predicate. This local
    // abstract contract requires team_key and has no free-text input or wire key.
    let mut cgs = (*fixture_cgs("plasm_language_matrix")).clone();
    let search = cgs.capabilities.get_mut("langitem_search").unwrap();
    search.inputs.selection.0.retain(|field| field.name != "q");
    search
        .inputs
        .selection
        .0
        .iter_mut()
        .find(|field| field.name == "team_key")
        .unwrap()
        .required = true;
    let fields = search.mapping.as_mut().unwrap().template.0["query"]["fields"]
        .as_array_mut()
        .expect("search query fields");
    fields.retain(|field| field[0].as_str() != Some("q"));
    cgs.validate().expect("filter-only fixture CGS");
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("filter-only fixture templates");

    let q = QueryExpr::filtered("LangItem", Predicate::eq("team_key", "ENG"));
    let cap = resolve_query_capability(&q, &cgs).expect("filter-only resolve");
    assert_eq!(cap.name.as_str(), "langitem_search");

    let mut expr = Expr::Query(q);
    normalize_expr_query_capabilities(&mut expr, &cgs).expect("filter-only normalize");
    let Expr::Query(q2) = expr else {
        panic!("expected query");
    };
    assert_eq!(q2.capability_name.as_deref(), Some("langitem_search"));
}

#[test]
fn fixture_explicit_q_filters_resolve_search() {
    let cgs = fixture_cgs("plasm_language_matrix");
    let q = QueryExpr::filtered(
        "LangItem",
        Predicate::and(vec![
            Predicate::eq("team_key", "ENG"),
            Predicate::eq("q", "bug"),
        ]),
    );
    let cap = resolve_query_capability(&q, &cgs).expect("resolve");
    assert_eq!(cap.name.as_str(), "langitem_search");

    let mut expr = Expr::Query(q);
    normalize_expr_query_capabilities(&mut expr, &cgs).expect("normalize");
    let Expr::Query(q2) = expr else {
        panic!("expected query");
    };
    assert_eq!(q2.capability_name.as_deref(), Some("langitem_search"));
}
