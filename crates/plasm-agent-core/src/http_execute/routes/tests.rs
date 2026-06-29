use super::super::*;
use crate::http;
use crate::http_execute::context::{
    build_capability_exposure_plan, build_plasm_context_agent_markdown,
    build_plasm_context_tool_meta, format_session_unchanged_reuse_markdown,
    group_seed_entities_by_entry, primary_entry_id_for_grouped,
};
use crate::http_execute::mcp_publish::build_mcp_run_tool_meta;
use crate::incoming_auth::IncomingPrincipal;
use crate::mcp_run_markdown::OmittedReferenceOnlyFields;
use axum::body::Body;
use axum::extract::Extension;
use axum::http::Request;
use axum::Router;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::loader::load_schema_dir;
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
use std::path::Path;
use tower::util::ServiceExt;

mod plasm_context;

#[test]
fn primary_entry_id_is_lexicographic_not_seed_insertion_order() {
    let seeds = vec![
        CapabilitySeed {
            entry_id: "zeta".into(),
            entity: "A".into(),
        },
        CapabilitySeed {
            entry_id: "alpha".into(),
            entity: "B".into(),
        },
    ];
    let grouped = group_seed_entities_by_entry(&seeds);
    assert_eq!(primary_entry_id_for_grouped(&grouped), "alpha");
}

#[test]
fn capability_exposure_plan_is_invariant_to_seed_order() {
    let seeds_a = vec![
        CapabilitySeed {
            entry_id: "zeta".into(),
            entity: "A".into(),
        },
        CapabilitySeed {
            entry_id: "alpha".into(),
            entity: "B".into(),
        },
    ];
    let seeds_b = vec![
        CapabilitySeed {
            entry_id: "alpha".into(),
            entity: "B".into(),
        },
        CapabilitySeed {
            entry_id: "zeta".into(),
            entity: "A".into(),
        },
    ];
    let a = build_capability_exposure_plan(&normalize_capability_seeds(seeds_a)).expect("plan a");
    let b = build_capability_exposure_plan(&normalize_capability_seeds(seeds_b)).expect("plan b");
    assert_eq!(a, b);
    assert_eq!(a.primary_entry_id, "alpha");
    assert_eq!(
        a.process_order,
        vec!["alpha".to_string(), "zeta".to_string()]
    );
}

#[test]
fn plasm_plan_publication_renders_named_output_owner() {
    let out = publish_plasm_result_steps(
        None,
        None,
        &[PublishedResultStep {
            name: Some("sorted".to_string()),
            node_id: Some("p1".to_string()),
            entry_id: Some("pokemon".to_string()),
            entity: Some("Pokemon".to_string()),
            cgs: None,
            display: "Pokemon[id,name]".to_string(),
            projection: Some(vec!["id".to_string(), "name".to_string()]),
            result: Arc::new(ExecutionResult {
                count: 0,
                entities: vec![],
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats {
                    duration_ms: 0,
                    network_requests: 0,
                    cache_hits: 0,
                    cache_misses: 0,
                    ..Default::default()
                },
                request_fingerprints: vec![],
            }),
            artifact: None,
        }],
    );
    assert!(out.markdown.contains("## sorted (0 rows)"));
    assert!(!out.markdown.contains("output:"));
    assert!(!out.markdown.contains("owner:"));
}

#[test]
fn live_run_tool_meta_finalizes_run_explorer_ui() {
    use crate::mcp_plasm_meta::{PlasmMetaIndex, RunUiStepFields};
    use crate::mcp_ui_payload::{finalize_mcp_tool_result, plasm_obj_from_tool_meta};
    use crate::output::LossySummaryFieldNames;
    use crate::run_artifacts::{
        artifact_http_path, plasm_run_resource_uri, plasm_short_resource_uri, RunArtifactId,
    };
    use rust_mcp_sdk::schema::{CallToolResult, TextContent};

    let run = RunArtifactId::from_bytes([2u8; 32]);
    let ph = "cd".repeat(32);
    let sid = "b".repeat(32);
    let handle = RunArtifactHandle {
        run_id: run,
        resource_index: 1,
        plasm_uri: plasm_short_resource_uri(1),
        canonical_plasm_uri: plasm_run_resource_uri(&ph, &sid, &run),
        http_path: artifact_http_path(&ph, &sid, &run),
        payload_len: 256,
        request_fingerprints: vec!["cafe".into()],
    };
    let mut idx = PlasmMetaIndex::new();
    let meta = build_mcp_run_tool_meta(
        Some(&mut idx),
        &[RunUiStepFields {
            run_step: 1,
            return_label: "items".into(),
            display: "WorkItem.query()".into(),
            row_count: 5,
            node_id: None,
            preview_entities: None,
            artifact: Some(handle),
            lossy_summary_fields: LossySummaryFieldNames::default(),
            column_schema: None,
        }],
        &OmittedReferenceOnlyFields::default(),
        None,
    )
    .expect("tool meta");
    assert!(
        meta.get("ui").is_none(),
        "UI attach happens at MCP finalize, not in build_mcp_run_tool_meta"
    );
    let plasm_obj = plasm_obj_from_tool_meta(meta).expect("plasm obj");
    let mut tool_meta = serde_json::Map::new();
    tool_meta.insert("plasm".into(), serde_json::Value::Object(plasm_obj));
    let res = finalize_mcp_tool_result(
        CallToolResult::text_content(vec![TextContent::new("ok".into(), None, None)]),
        tool_meta,
    );
    assert_eq!(
        res.meta
            .as_ref()
            .and_then(|m| m.get("ui"))
            .and_then(|u| u.get("resourceUri"))
            .and_then(|v| v.as_str()),
        Some(crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_URI)
    );
    assert!(res
        .meta
        .as_ref()
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("steps"))
        .and_then(|s| s.as_array())
        .is_some_and(|a| !a.is_empty()));
    assert!(res
        .meta
        .as_ref()
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("plan"))
        .is_none());
}

fn test_host_state_from_registry(reg: InMemoryCgsRegistry) -> PlasmHostState {
    let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
    http::build_plasm_host_state(http::PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(reg),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: std::sync::Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
}

fn test_state_with_registry() -> PlasmHostState {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
    test_host_state_from_registry(InMemoryCgsRegistry::from_pairs(vec![(
        "overshow".into(),
        "Overshow".into(),
        vec!["demo".into()],
        cgs,
    )]))
}

fn test_state_with_linear_registry() -> Option<PlasmHostState> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/linear");
    if !dir.exists() {
        return None;
    }
    let cgs = Arc::new(load_schema_dir(&dir).expect("linear"));
    Some(test_host_state_from_registry(
        InMemoryCgsRegistry::from_pairs(vec![(
            "linear".into(),
            "Linear".into(),
            vec!["linear".into()],
            cgs,
        )]),
    ))
}

fn test_state_with_matrix_federated_registry() -> Option<PlasmHostState> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    if !dir.exists() {
        return None;
    }
    let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
    Some(test_host_state_from_registry(
        InMemoryCgsRegistry::from_pairs(vec![
            (
                "github".into(),
                "Github".into(),
                vec!["demo".into()],
                cgs.clone(),
            ),
            ("linear".into(), "Linear".into(), vec!["demo".into()], cgs),
        ]),
    ))
}

fn test_app_execute(st: PlasmHostState) -> Router<()> {
    execute_routes()
        .layer(Extension(st.clone()))
        .layer(Extension(IncomingPrincipal(None)))
}

#[tokio::test]
async fn invalid_prompt_hash_path_segment_is_400() {
    let st = test_state_with_registry();
    let app = test_app_execute(st);
    // 63 hex digits — invalid length for SHA-256 hex (expect 64).
    let bad_hash = "a".repeat(63);
    let good_session = "0123456789abcdef0123456789abcdef";
    let uri = format!("/execute/{bad_hash}/{good_session}");
    let run = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("accept", "application/json")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(run).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let ct = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("application/problem+json"),
        "expected problem+json, got {ct:?}"
    );
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        doc.get("type").and_then(|t| t.as_str()),
        Some(problem_types::EXECUTE_INVALID_PATH_PARAM)
    );
    assert!(
        doc.get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .contains("prompt_hash"),
        "detail should name prompt_hash: {doc:?}"
    );
}

async fn get_execute_session_json(
    app: &Router<()>,
    location_path: &str,
) -> CreateExecuteSessionResponse {
    let get = Request::builder()
        .method("GET")
        .uri(location_path)
        .header("accept", "application/json")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(get).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).expect("session JSON")
}

#[tokio::test]
async fn create_session_then_bad_expression_is_400() {
    let st = test_state_with_registry();
    let app = test_app_execute(st.clone());

    let create = Request::builder()
        .method("POST")
        .uri("/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(create).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let loc = res
        .headers()
        .get(LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(loc.starts_with("/execute/"));

    let post_body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        post_body.is_empty(),
        "303 create must not include a body; session JSON is from GET {loc}"
    );

    let created = get_execute_session_json(&app, loc.as_str()).await;
    assert_eq!(created.entities, vec!["Profile"]);
    let expected_hash = PromptHashHex::from_prompt_sha256(&created.prompt);
    assert_eq!(created.prompt_hash, expected_hash.to_string());

    let run_uri = format!("/execute/{}/{}", created.prompt_hash, created.session);
    // Parse/type errors return problem+json without hitting the backend (Profile{} would need HTTP).
    let run = Request::builder()
        .method("POST")
        .uri(&run_uri)
        .header("accept", "application/json")
        .body(Body::from("@@@not-plasm"))
        .unwrap();
    let res2 = app.oneshot(run).await.unwrap();
    assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_execute_session_prompt_is_table_only() {
    let st = test_state_with_registry();
    let app = test_app_execute(st.clone());

    let create = Request::builder()
        .method("POST")
        .uri("/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(create).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let loc = res
        .headers()
        .get(LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    let full = get_execute_session_json(&app, loc.as_str()).await;
    assert!(
        !full
            .prompt
            .contains(plasm_core::prompt_render::TEACHING_VALID_EXPR_MARKER),
        "execute session prompt is table-only; grammar is taught via MCP tools/list"
    );
    assert!(full.prompt.contains("plasm_expr"));

    let get = Request::builder()
        .method("GET")
        .uri(loc.as_str())
        .header("accept", "application/json")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(get).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let cached: CreateExecuteSessionResponse = serde_json::from_slice(&body).expect("session JSON");
    assert_eq!(cached.prompt_hash, full.prompt_hash);
    assert!(
        !cached
            .prompt
            .contains(plasm_core::prompt_render::TEACHING_VALID_EXPR_MARKER),
        "GET session stays table-only"
    );
    assert!(cached.prompt.contains("plasm_expr"));
}

#[tokio::test]
async fn staged_table_response_joins_sections() {
    let res = respond_staged_lines_execute_result(
        ExecResponseKind::Table,
        vec![serde_json::json!([]), serde_json::json!([1, 2])],
        Some(vec!["first_table".to_string(), "second_table".to_string()]),
        None,
        None,
    );
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/plain"),
        "expected text/plain, got {ct:?}"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        text.contains("---"),
        "expected table sections separated by ---: {text:?}"
    );
}

#[tokio::test]
async fn staged_toon_response_is_outer_array() {
    let res = respond_staged_lines_execute_result(
        ExecResponseKind::Toon,
        vec![serde_json::json!(["a"]), serde_json::json!([])],
        None,
        None,
        None,
    );
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/toon"),
        "expected text/toon, got {ct:?}"
    );
}

#[tokio::test]
async fn program_parse_error_is_bad_request() {
    let st = test_state_with_registry();
    let app = test_app_execute(st.clone());
    let create = Request::builder()
        .method("POST")
        .uri("/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(create).await.unwrap();
    let loc = res
        .headers()
        .get(LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let created = get_execute_session_json(&app, loc.as_str()).await;
    let run_uri = format!("/execute/{}/{}", created.prompt_hash, created.session);
    let run = Request::builder()
        .method("POST")
        .uri(&run_uri)
        .header("accept", "application/json")
        .body(Body::from("@@@not-plasm"))
        .unwrap();
    let res2 = app.oneshot(run).await.unwrap();
    assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(res2.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let detail = doc.get("detail").and_then(|d| d.as_str()).unwrap_or("");
    assert!(
        detail.contains("Fix spelling"),
        "expected parse detail: {detail:?}"
    );
}

#[tokio::test]
async fn resolved_plan_endpoint_plan_mode() {
    use crate::catalog_pin::CatalogPin;
    use crate::plasm_compile::compile_plasm_surface_line_to_comp;
    use crate::resolved_plan_http::{
        ResolvedPlanProtocolVersion, ResolvedPlanRequest, ResolvedPlanRunMode,
        RESOLVED_PLAN_CONTENT_TYPE,
    };

    let st = test_state_with_registry();
    let app = test_app_execute(st.clone());
    let create = Request::builder()
        .method("POST")
        .uri("/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "entry_id": "overshow",
                "entities": ["Profile"],
                "context_intent": "profile query",
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(create).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let loc = res.headers().get(LOCATION).unwrap().to_str().unwrap();
    let created = get_execute_session_json(&app, loc).await;
    let sess = st
        .sessions
        .get_by_strs(&created.prompt_hash, &created.session)
        .await
        .expect("session");
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let bundle =
        compile_plasm_surface_line_to_comp(pipeline, Some(cross), &sess, "test", "Profile{}")
            .expect("compile");
    let comp = bundle.artifact().comp.clone();
    let digest = sess.cgs.catalog_cgs_hash_hex();
    let req = ResolvedPlanRequest {
        protocol_version: ResolvedPlanProtocolVersion::V1.as_u16(),
        client_session_id: "cs_test".into(),
        catalog_pins: vec![CatalogPin {
            api: "overshow".into(),
            digest: digest.clone(),
        }],
        mode: ResolvedPlanRunMode::Plan,
        source_program: "Profile{}".into(),
        comp,
    };
    let plan_uri = format!("/execute/{}/{}/plan", created.prompt_hash, created.session);
    let run = Request::builder()
        .method("POST")
        .uri(&plan_uri)
        .header("content-type", RESOLVED_PLAN_CONTENT_TYPE)
        .header("accept", "application/json")
        .body(Body::from(serde_json::to_string(&req).unwrap()))
        .unwrap();
    let res2 = app.oneshot(run).await.unwrap();
    assert_eq!(
        res2.status(),
        StatusCode::OK,
        "plan endpoint should succeed"
    );
    let bytes = axum::body::to_bytes(res2.into_body(), usize::MAX)
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(doc.get("plan").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(doc.get("dry_run").and_then(|v| v.as_bool()), Some(true));
    assert!(doc.get("comp").is_some());
}

#[tokio::test]
async fn resolved_plan_endpoint_rejects_digest_mismatch() {
    use crate::catalog_pin::CatalogPin;
    use crate::resolved_plan_http::{
        ResolvedPlanProtocolVersion, ResolvedPlanRequest, ResolvedPlanRunMode,
        RESOLVED_PLAN_CONTENT_TYPE,
    };

    let st = test_state_with_registry();
    let app = test_app_execute(st.clone());
    let create = Request::builder()
        .method("POST")
        .uri("/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(create).await.unwrap();
    let loc = res.headers().get(LOCATION).unwrap().to_str().unwrap();
    let created = get_execute_session_json(&app, loc).await;
    let req = ResolvedPlanRequest {
        protocol_version: ResolvedPlanProtocolVersion::V1.as_u16(),
        client_session_id: "cs_test".into(),
        catalog_pins: vec![CatalogPin {
            api: "overshow".into(),
            digest: "0".repeat(64),
        }],
        mode: ResolvedPlanRunMode::Plan,
        source_program: "Profile{}".into(),
        comp: plasm_core::PlasmComp {
            version: 1,
            name: Some("bad".into()),
            steps: std::collections::BTreeMap::new(),
            bind: plasm_core::PlasmBindGraph::default(),
            return_: plasm_core::PlasmReturn::Step {
                step: plasm_core::StepId::new("n1").expect("id"),
            },
            metadata: std::collections::BTreeMap::new(),
        },
    };
    let plan_uri = format!("/execute/{}/{}/plan", created.prompt_hash, created.session);
    let run = Request::builder()
        .method("POST")
        .uri(&plan_uri)
        .header("content-type", RESOLVED_PLAN_CONTENT_TYPE)
        .body(Body::from(serde_json::to_string(&req).unwrap()))
        .unwrap();
    let res2 = app.oneshot(run).await.unwrap();
    assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn same_inputs_same_prompt_hash() {
    let st = test_state_with_registry();
    let app = test_app_execute(st);

    async fn create_session(app: &Router<()>) -> CreateExecuteSessionResponse {
        let create = Request::builder()
            .method("POST")
            .uri("/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
            ))
            .unwrap();
        let res = app.clone().oneshot(create).await.unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let loc = res.headers().get(LOCATION).unwrap().to_str().unwrap();
        get_execute_session_json(app, loc).await
    }

    let a = create_session(&app).await;
    let b = create_session(&app).await;
    assert_eq!(a.prompt_hash, b.prompt_hash);
    assert_eq!(
        a.session, b.session,
        "server should reuse session id for same entry + entities"
    );
    assert!(!a.reused, "first GET should not set reused");
    assert!(
        !b.reused,
        "GET session JSON does not surface create-time reuse"
    );
}

#[tokio::test]
async fn execute_session_create_marks_reused_on_second_open() {
    let st = test_state_with_registry();
    let body = CreateExecuteSessionBody {
        entry_id: "overshow".into(),
        entities: vec!["Profile".into()],
        principal: None,
        logical_session_id: None,
        context_intent: None,
        ranked_capabilities: None,
        read_first_seeded_exposure: false,
    };
    let first = execute_session_create_response(&st, None, body.clone())
        .await
        .expect("first open");
    assert!(!first.reused);
    let second = execute_session_create_response(&st, None, body)
        .await
        .expect("second open");
    assert!(second.reused);
    assert_eq!(first.prompt_hash, second.prompt_hash);
    assert_eq!(first.session, second.session);
}

#[tokio::test]
async fn expand_domain_session_updates_session_entities() {
    let st = test_state_with_registry();
    let created = execute_session_create_response(
        &st,
        None,
        CreateExecuteSessionBody {
            entry_id: "overshow".into(),
            entities: vec!["Profile".into()],
            principal: None,
            logical_session_id: None,
            context_intent: None,
            ranked_capabilities: None,
            read_first_seeded_exposure: false,
        },
    )
    .await
    .expect("open");
    assert_eq!(created.entities, vec!["Profile"]);

    let first_wave = expand_execute_teaching_session(
        &st,
        None,
        &created.prompt_hash,
        &created.session,
        vec![CapabilitySeed {
            entry_id: "overshow".into(),
            entity: "RecordedContent".into(),
        }],
    )
    .await
    .expect("expand");
    assert!(
        first_wave.markdown.contains("```tsv"),
        "expected fenced teaching TSV (default TSV render): {}",
        first_wave.markdown
    );
    assert!(
        !first_wave.markdown.contains("Added capabilities"),
        "expand wave must not repeat seed accounting: {}",
        first_wave.markdown
    );
    assert!(
        !first_wave.markdown.contains("`e1`…`e"),
        "expand wave must not repeat symbol reminder prose: {}",
        first_wave.markdown
    );

    let sess = st
        .sessions
        .get_by_strs(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("session");
    assert_eq!(
        sess.entities,
        vec!["Profile".to_string(), "RecordedContent".to_string()],
        "GET /execute and logs use cumulative exposed entities after expand"
    );

    let dup = expand_execute_teaching_session(
        &st,
        None,
        &created.prompt_hash,
        &created.session,
        vec![CapabilitySeed {
            entry_id: "overshow".into(),
            entity: "RecordedContent".into(),
        }],
    )
    .await
    .expect("expand duplicate");
    assert!(
        dup.markdown.trim().is_empty(),
        "no-op expand returns empty markdown: {dup:?}"
    );
    let sess2 = st
        .sessions
        .get_by_strs(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("session");
    assert_eq!(
        sess2.entities,
        vec!["Profile".to_string(), "RecordedContent".to_string()]
    );
}

#[test]
fn parse_execute_program_body_rejects_lines_array() {
    let err = parse_execute_program_body(Some("application/json"), br#"{"lines":["a","b"]}"#)
        .expect_err("lines");
    assert!(err.contains("lines"), "{err}");
}

#[test]
fn format_session_unchanged_reuse_markdown_shape() {
    let s = format_session_unchanged_reuse_markdown(None);
    assert!(s.contains("Unchanged"));
    assert!(s.contains("Next: `plasm`"));
    assert!(s.contains("plasm_run"));
    assert!(!s.contains("rows:` fields only"));
    assert!(!s.contains("e#~$"));

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = load_schema_dir(&dir).expect("overshow_tools");
    let exp = plasm_core::TeachingExposureSession::new(&cgs, "overshow", &["Profile", "Meeting"]);
    let body = format_session_unchanged_reuse_markdown(Some(&exp));
    assert!(body.contains("e1=Profile"));
    assert!(body.contains("e2=Meeting"));
    assert!(
        body.len() <= 200,
        "reuse body too long for 3 entities: {} chars — {body}",
        body.len()
    );
}

#[tokio::test]
async fn unknown_entity_parse_error_includes_session_bounds() {
    let st = test_state_with_registry();
    let created = execute_session_create_response(
        &st,
        None,
        CreateExecuteSessionBody {
            entry_id: "overshow".into(),
            entities: vec!["Profile".into()],
            principal: None,
            logical_session_id: None,
            context_intent: None,
            ranked_capabilities: None,
            read_first_seeded_exposure: false,
        },
    )
    .await
    .expect("open");
    let sess = st
        .sessions
        .get(
            &created.prompt_hash.parse().unwrap(),
            &created.session.parse().unwrap(),
        )
        .await
        .expect("session");
    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let err =
        crate::plasm_compile::compile_plasm_expression(pipeline, Some(cross), &sess, "t", "e9()")
            .expect_err("out-of-range e#");
    assert!(
        err.contains("is not in this session"),
        "expected unknown entity in {err:?}"
    );
}

#[test]
fn negotiate_accept_variants() {
    assert_eq!(negotiate_accept(None).unwrap(), ExecResponseKind::Toon);
    assert_eq!(negotiate_accept(Some("")).unwrap(), ExecResponseKind::Toon);
    assert_eq!(
        negotiate_accept(Some("*/*")).unwrap(),
        ExecResponseKind::Toon
    );
    assert_eq!(
        negotiate_accept(Some("application/json")).unwrap(),
        ExecResponseKind::Json
    );
    assert_eq!(
        negotiate_accept(Some("text/plain")).unwrap(),
        ExecResponseKind::Table
    );
    assert_eq!(
        negotiate_accept(Some("text/toon")).unwrap(),
        ExecResponseKind::Toon
    );
    assert_eq!(
        negotiate_accept(Some("application/x-ndjson")).unwrap(),
        ExecResponseKind::Ndjson
    );
    assert!(negotiate_accept(Some("application/soap+xml")).is_err());
}
