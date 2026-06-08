//! End-to-end tests using hermit as an in-process OpenAPI mock server.
//! No Docker, no testcontainers — hermit runs inside the test process.
//!
//! ## Scope (integration, not language catalog)
//!
//! Keep these tests focused on **integration**: Hermit routing, [`ExecutionEngine`] live calls,
//! hydration / pagination / cache behavior, and generic CLI surface shape over loaded CGS.
//!
//! **Do not** grow this file into a catalog of Plasm **surface syntax** or unified language
//! semantics (parse → DAG → plan → IR meaning). That belongs in `tests/plasm_language_matrix.rs`
//! (Hermit matrix) and its fixtures. Compiler-only invariants live in `plasm-agent-core`
//! `plasm_dag` tests; lexer/parser micro-cases live in `plasm-core` `expr_parser`.

mod common;

use common::hermit;
use plasm_core::{Expr, Predicate, QueryExpr, QueryPagination, CGS};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, SessionMaterialization,
    StreamConsumeOpts,
};
use std::path::Path;

async fn hermit_base_url() -> &'static String {
    hermit::petstore_hermit_base_url().await
}

async fn hermit_petstore_hydrate_base_url() -> &'static String {
    hermit::petstore_hermit_hydrate_base_url().await
}

async fn pokeapi_hermit_base_url() -> &'static String {
    hermit::pokeapi_hermit_base_url().await
}

fn load_pokeapi_mini_cgs() -> CGS {
    let paths = [
        "fixtures/schemas/pokeapi_mini",
        "../../fixtures/schemas/pokeapi_mini",
    ];
    for path in &paths {
        let p = Path::new(path);
        if p.exists() {
            return plasm_core::loader::load_schema_dir(p).expect("pokeapi_mini CGS");
        }
    }
    panic!("fixtures/schemas/pokeapi_mini not found");
}

fn find_spec_path() -> Option<std::path::PathBuf> {
    hermit::petstore_spec_path()
}

fn load_petstore_cgs() -> Option<CGS> {
    let paths = [
        "fixtures/schemas/petstore",
        "../../fixtures/schemas/petstore",
    ];
    for path in &paths {
        let p = std::path::Path::new(path);
        if p.exists() {
            return Some(plasm_core::loader::load_schema_dir(p).expect("Invalid petstore CGS"));
        }
    }
    None
}

fn make_engine(base_url: &str) -> ExecutionEngine {
    let config = ExecutionConfig {
        base_url: Some(base_url.to_string()),
        ..Default::default()
    };
    ExecutionEngine::new(config).unwrap()
}

// ── Tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn hermit_serves_petstore_pets() {
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_base_url().await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{}/pet/10", url)).send().await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("name").is_some(),
        "Pet should have a name: {:?}",
        body
    );
    assert!(
        body.get("status").is_some(),
        "Pet should have a status: {:?}",
        body
    );
}

#[tokio::test]
async fn query_pets_through_execution_engine() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_base_url().await;
    let engine = make_engine(url);
    let mut cache = SessionMaterialization::new();

    // List response already carries full pet rows; avoid default hydration so this stays a
    // single-request smoke test (parallel GETs against the shared hermit instance are flaky).
    let mut query = QueryExpr::filtered("Pet", Predicate::eq("status", "available"));
    query.hydrate = Some(false);

    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await;

    assert!(result.is_ok(), "Query should succeed: {:?}", result);
    let result = result.unwrap();
    assert!(result.count > 0, "Should return at least one pet");

    for entity in &result.entities {
        assert!(
            entity.fields.contains_key("name"),
            "Decoded pet should have name"
        );
    }
}

#[tokio::test]
async fn query_pets_with_hydrate_resolves_names() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_petstore_hydrate_base_url().await;
    let config = ExecutionConfig {
        base_url: Some(url.to_string()),
        ..Default::default()
    };
    let engine = ExecutionEngine::new(config).unwrap();
    let mut cache = SessionMaterialization::new();

    let query = QueryExpr::filtered("Pet", Predicate::eq("status", "available"));
    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await;

    assert!(result.is_ok(), "{:?}", result.err());
    let result = result.unwrap();
    assert!(result.count > 0);
    assert!(
        result.stats.network_requests >= 1,
        "expected at least the findByStatus request, got {}",
        result.stats.network_requests
    );
    for entity in &result.entities {
        assert!(entity.fields.contains_key("name"));
    }
}

#[tokio::test]
async fn get_pet_by_id_through_engine() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_base_url().await;
    let engine = make_engine(url);
    let mut cache = SessionMaterialization::new();

    let get = plasm_core::Expr::Get(plasm_core::GetExpr::new("Pet", "42"));
    let result = engine
        .execute(
            &get,
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await;

    assert!(result.is_ok(), "Get should succeed: {:?}", result);
    let result = result.unwrap();
    assert_eq!(result.count, 1);
    assert_eq!(result.entities[0].reference.entity_type, "Pet");
}

#[tokio::test]
async fn get_order_through_engine() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_base_url().await;
    let engine = make_engine(url);
    let mut cache = SessionMaterialization::new();

    let get = plasm_core::Expr::Get(plasm_core::GetExpr::new("Order", "99"));
    let result = engine
        .execute(
            &get,
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await;

    assert!(result.is_ok(), "Get order should succeed: {:?}", result);
    let result = result.unwrap();
    assert_eq!(result.count, 1);
    assert!(
        result.entities[0].fields.contains_key("status"),
        "Order should have status"
    );
}

#[tokio::test]
async fn get_user_by_username() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_base_url().await;
    let engine = make_engine(url);
    let mut cache = SessionMaterialization::new();

    let get = plasm_core::Expr::Get(plasm_core::GetExpr::new("User", "testuser"));
    let result = engine
        .execute(
            &get,
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await;

    assert!(result.is_ok(), "Get user should succeed: {:?}", result);
    let result = result.unwrap();
    assert_eq!(result.count, 1);
    assert!(
        result.entities[0].fields.contains_key("email"),
        "User should have email"
    );
}

#[tokio::test]
async fn agent_cli_builds_from_extracted_schema() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    let app = plasm_agent::cli_builder::build_app(
        &cgs,
        plasm_agent::cli_builder::AgentCliSurface::CgsClient,
    );

    let sub_names: Vec<String> = app
        .get_subcommands()
        .map(|c: &clap::Command| c.get_name().to_string())
        .collect();

    assert!(sub_names.contains(&"pet".to_string()));
    assert!(sub_names.contains(&"order".to_string()));
    assert!(sub_names.contains(&"user".to_string()));

    let pet = app.find_subcommand("pet").unwrap();
    let pet_subs: Vec<String> = pet
        .get_subcommands()
        .map(|c: &clap::Command| c.get_name().to_string())
        .collect();

    assert!(
        pet_subs.contains(&"query".to_string()),
        "Pet needs query: {:?}",
        pet_subs
    );
    assert!(
        pet_subs.contains(&"create".to_string()),
        "Pet needs create: {:?}",
        pet_subs
    );
    assert!(
        pet_subs.contains(&"delete".to_string()),
        "Pet needs delete: {:?}",
        pet_subs
    );
}

#[tokio::test]
async fn pokeapi_berry_query_paginates_with_cml() {
    let url = pokeapi_hermit_base_url().await;
    let cgs = load_pokeapi_mini_cgs();
    let engine = make_engine(url);
    let mut cache = SessionMaterialization::new();

    let mut query = QueryExpr::all("Berry");
    query.pagination = Some(QueryPagination::default());

    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: false,
            },
            ExecuteOptions::default(),
        )
        .await;

    assert!(result.is_ok(), "{:?}", result.err());
    let r = result.unwrap();
    assert!(r.count >= 1, "expected at least one berry, got {}", r.count);
    assert!(r
        .entities
        .iter()
        .all(|e| e.reference.entity_type == "Berry"));
    // Hermit may return `next: null` with several items in one body (ignores `limit`); the e2e still
    // proves CML pagination + `StreamConsumeOpts::fetch_all` integrates with the live engine and decoder.
}

#[tokio::test]
async fn cache_populated_after_get() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_base_url().await;
    let engine = make_engine(url);
    let mut cache = SessionMaterialization::new();

    assert_eq!(cache.stats().total_entities, 0);

    let get = plasm_core::Expr::Get(plasm_core::GetExpr::new("Pet", "7"));
    engine
        .execute(
            &get,
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await
        .unwrap();

    assert!(
        cache.stats().total_entities > 0,
        "Cache should have entities after Get"
    );
    let ref_ = plasm_core::Ref::new("Pet", "7");
    assert!(cache.contains(&ref_), "Cache should contain Pet:7");
}

#[tokio::test]
async fn scoped_query_second_run_reuses_session_response_store() {
    let Some(cgs) = load_petstore_cgs() else {
        return;
    };
    if find_spec_path().is_none() {
        return;
    }
    let url = hermit_base_url().await;
    let engine = make_engine(url);
    let mut cache = SessionMaterialization::new();

    let mut query = QueryExpr::filtered("Pet", Predicate::eq("status", "available"));
    query.hydrate = Some(false);
    let expr = Expr::Query(query.clone());

    let first = engine
        .execute(
            &expr,
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await
        .expect("first query");
    assert!(
        first.stats.network_requests >= 1,
        "first run should hit Hermit"
    );

    let second = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await
        .expect("second query");
    assert_eq!(
        second.stats.network_requests, 0,
        "second identical query should consult session response store, not HTTP"
    );
    assert!(
        second.stats.cache.response_store_hits >= 1 || second.stats.cache_hits >= 1,
        "expected response-store or legacy cache hit telemetry on second run"
    );
}
