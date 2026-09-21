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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await;

    assert!(result.is_ok(), "{:?}", result.err());
    let r = result.unwrap();
    // The fixture has exactly 40 rows; the spec and CML use 20-row pages.
    assert_eq!(r.count, 40, "must consume both complete pages");
    assert_eq!(
        r.stats.network_requests, 2,
        "must advance to the second page and stop"
    );
    let ids: std::collections::BTreeSet<_> = r
        .entities
        .iter()
        .map(|e| e.reference.primary_slot_str())
        .collect();
    assert_eq!(
        ids.len(),
        r.count,
        "paginated berry query must not overlap identities"
    );
    assert!(r
        .entities
        .iter()
        .all(|e| e.reference.entity_type == "Berry"));
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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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
            ExecuteOptions::for_catalog(&cgs).unwrap(),
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

/// CEP-14 brand-new live path: concurrent identical cold reads converge without materialization conflict.
#[tokio::test]
async fn concurrent_cold_identical_reads_no_materialization_conflict() {
    use plasm_runtime::{detect_materialization_conflicts, BranchMaterializationBase};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let cgs = Arc::new(load_pokeapi_mini_cgs());
    let url = pokeapi_hermit_base_url().await.clone();
    let session = Arc::new(Mutex::new(SessionMaterialization::new()));
    let query = QueryExpr::all("Berry");
    let expr = Arc::new(Expr::Query(query));

    let n = 6usize;
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let session_bg = Arc::clone(&session);
        let url_bg = url.clone();
        let cgs_bg = Arc::clone(&cgs);
        let expr_bg = Arc::clone(&expr);
        handles.push(tokio::spawn(async move {
            let engine = make_engine(&url_bg);
            let (mut branch, base) = {
                let guard = session_bg.lock().await;
                BranchMaterializationBase::fork_from(&guard)
            };
            let result = engine
                .execute(
                    expr_bg.as_ref(),
                    cgs_bg.as_ref(),
                    &mut branch,
                    Some(ExecutionMode::Live),
                    StreamConsumeOpts {
                        fetch_all: true,
                        ..Default::default()
                    },
                    ExecuteOptions::for_catalog(&cgs_bg).unwrap(),
                )
                .await
                .expect("live berry query on branch");
            assert!(result.count >= 1, "expected berries from Hermit");
            let mut guard = session_bg.lock().await;
            let conflicts = detect_materialization_conflicts(&guard, &base, &branch);
            assert!(
                !conflicts.has_any(),
                "identical concurrent cold reads must not conflict: {conflicts:?}"
            );
            guard.absorb_branch(branch).expect("absorb branch");
        }));
    }
    for handle in handles {
        handle.await.expect("concurrent read task");
    }
    let guard = session.lock().await;
    assert!(
        guard.stats().total_entities >= 1,
        "session graph populated after concurrent reads"
    );
}

#[test]
fn hermit_shared_server_survives_caller_runtime_shutdown() {
    let first = tokio::runtime::Runtime::new().unwrap();
    let url = first.block_on(async { pokeapi_hermit_base_url().await.clone() });
    drop(first);
    let second = tokio::runtime::Runtime::new().unwrap();
    second.block_on(async {
        let response = reqwest::Client::new()
            .get(format!("{url}/api/v2/berry/?limit=20&offset=0"))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .expect("shared Hermit server must outlive the caller runtime");
        assert!(response.status().is_success());
    });
}

/// Production-catalog integration: ranked search and listing remain distinct after serialization.
#[test]
fn spotify_artist_search_and_listing_have_distinct_wire_contracts() {
    use plasm_compile::{compile_operation, parse_capability_template, CmlEnv, CompiledOperation};
    use plasm_core::{CapabilityKind, Value, CGS};
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/spotify");
    let loaded = plasm_core::load_schema(&directory).expect("Spotify production catalog");
    let cgs: CGS = serde_json::from_slice(&serde_json::to_vec(&loaded).unwrap()).unwrap();
    assert_eq!(
        cgs.get_capability("artist_search").unwrap().kind,
        CapabilityKind::Search
    );
    assert_eq!(
        cgs.get_capability("following_artist_query").unwrap().kind,
        CapabilityKind::Query
    );
    let compile = |capability: &str, env: &CmlEnv| {
        let template = parse_capability_template(
            &cgs.get_capability(capability)
                .unwrap()
                .mapping
                .as_ref()
                .unwrap()
                .template,
        )
        .unwrap();
        let CompiledOperation::Http(request) = compile_operation(&template, env).unwrap() else {
            panic!("HTTP mapping")
        };
        request
    };
    let mut env = CmlEnv::new();
    env.insert("q".into(), Value::String("A named artist".into()));
    let search = compile("artist_search", &env);
    assert_eq!(search.path, "/spotify/artists");
    assert_eq!(
        search.query.unwrap().as_object().unwrap().get("query"),
        Some(&Value::String("A named artist".into()))
    );
    env.insert("access_token".into(), Value::String("test-token".into()));
    for (shelf, path) in [
        ("following", "/spotify/following_artists"),
        ("catalog", "/spotify/artists"),
    ] {
        env.insert("shelf".into(), Value::String(shelf.into()));
        let listing = compile("following_artist_query", &env);
        assert_eq!(listing.path, path);
        assert!(listing.query.as_ref().is_none_or(|query| query
            .as_object()
            .is_some_and(|fields| !fields.contains_key("query"))));
    }
}

/// Production-catalog/spec integration: owner selection and peer mutation are distinct.
#[test]
fn venmo_friend_owner_role_preserves_openapi_wire_contract() {
    use plasm_compile::{compile_operation, parse_capability_template, CmlEnv, CompiledOperation};
    use plasm_core::Value;
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/venmo");
    let cgs = plasm_core::load_schema(&directory).expect("Venmo production catalog");
    let spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("openapi.json")).unwrap()).unwrap();
    let list_parameters = spec["paths"]["/venmo/friends"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let owner_parameter = list_parameters
        .iter()
        .find(|parameter| parameter["name"] == "user_email")
        .unwrap();
    assert!(owner_parameter["description"]
        .as_str()
        .unwrap()
        .contains("whose friends"));
    let query = cgs.get_capability("friend_query").unwrap();
    assert!(query
        .inputs
        .scope
        .0
        .iter()
        .any(|field| field.name == "owner_email"));
    assert!(!query
        .inputs
        .scope
        .0
        .iter()
        .any(|field| field.name == "user_email"));
    let owner_slot = query
        .inputs
        .scope
        .0
        .iter()
        .find(|field| field.name == "owner_email")
        .unwrap()
        .named_value(&cgs)
        .unwrap();
    assert!(owner_slot.description.contains("whose Venmo friend list"));
    let compile = |capability: &str, env: &CmlEnv| {
        let template = parse_capability_template(
            &cgs.get_capability(capability)
                .unwrap()
                .mapping
                .as_ref()
                .unwrap()
                .template,
        )
        .unwrap();
        let CompiledOperation::Http(request) = compile_operation(&template, env).unwrap() else {
            panic!("HTTP mapping")
        };
        request
    };
    let mut env = CmlEnv::new();
    env.insert(
        "access_token".into(),
        Value::String("role-test-token".into()),
    );
    let own = compile("friend_query", &env);
    assert!(own.query.as_ref().is_none_or(|query| query
        .as_object()
        .is_some_and(|object| !object.contains_key("user_email"))));
    env.insert(
        "owner_email".into(),
        Value::String("owner@example.com".into()),
    );
    let owned = compile("friend_query", &env);
    assert_eq!(owned.path, "/venmo/friends");
    assert_eq!(
        owned.query.unwrap().as_object().unwrap().get("user_email"),
        Some(&Value::String("owner@example.com".into()))
    );
    env.insert(
        "user_email".into(),
        Value::String("peer@example.com".into()),
    );
    assert_eq!(
        compile("friend_add", &env).path,
        "/venmo/friends/peer@example.com"
    );
    env.insert("id".into(), Value::String("peer@example.com".into()));
    assert_eq!(
        compile("friend_remove", &env).path,
        "/venmo/friends/peer@example.com"
    );
    assert!(spec["paths"]["/venmo/friends/{user_email}"]["post"].is_object());
    assert!(spec["paths"]["/venmo/friends/{user_email}"]["delete"].is_object());
}

/// Production catalog integration: externally specified authors/credits survive decoding.
#[test]
fn appworld_record_authorship_matches_openapi() {
    use plasm_compile::{
        decode_entities, entity_decoder_for_from_parent_get_target, path_expr_from_json_segments,
        PathExpr,
    };
    use plasm_core::RelationMaterialization;
    for (app, entity, relation, endpoint, wire, body, expected) in [
        (
            "spotify",
            "Song",
            "artists",
            "/spotify/songs/{song_id}",
            "artists",
            serde_json::json!({"artists":[{"id":41,"name":"Credit A"},{"id":42,"name":"Credit B"}]}),
            2,
        ),
        (
            "spotify",
            "Album",
            "artists",
            "/spotify/albums/{album_id}",
            "artists",
            serde_json::json!({"artists":[{"id":41,"name":"Credit A"}]}),
            1,
        ),
        (
            "todoist",
            "TaskComment",
            "author",
            "/todoist/task_comments/{task_comment_id}",
            "user",
            serde_json::json!({"task_comment_id":7,"user":{"name":"Comment Author","email":"author@example.com"}}),
            1,
        ),
    ] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apis/appworld")
            .join(app);
        let cgs = plasm_core::load_schema(&dir).unwrap();
        let spec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("openapi.json")).unwrap()).unwrap();
        let schema = &spec["paths"][endpoint]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"];
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == wire));
        let parent = cgs.get_entity(entity).unwrap();
        let rel = parent.relations.get(relation).unwrap();
        let Some(RelationMaterialization::FromParentGet { path }) = &rel.materialize else {
            panic!("embedded identity required")
        };
        let target = cgs.get_entity(rel.target_resource.as_str()).unwrap();
        let decoder = entity_decoder_for_from_parent_get_target(
            target,
            parent,
            path_expr_from_json_segments(path).unwrap(),
        );
        let rows = decode_entities(&decoder, &body).unwrap();
        assert_eq!(rows.len(), expected);
        if app == "spotify" {
            assert_eq!(
                rows[0].fields["name"],
                plasm_core::Value::String("Credit A".into())
            );
        }
        if app == "todoist" {
            let decoder =
                entity_decoder_for_from_parent_get_target(parent, parent, PathExpr::empty());
            let rows = decode_entities(&decoder, &body).unwrap();
            assert_eq!(
                rows[0].fields["author_email"],
                plasm_core::Value::String("author@example.com".into())
            );
            assert_eq!(
                rows[0].fields["author_name"],
                plasm_core::Value::String("Comment Author".into())
            );
        }
    }
}
