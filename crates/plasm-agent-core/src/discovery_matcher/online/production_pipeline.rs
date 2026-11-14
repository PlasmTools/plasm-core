//! Frozen development cases through publication, hybrid recall and real judgment.
//! Labels are consumed only after route_turn returns.
use super::*;
use crate::discovery_service::{DiscoveryService, RouteTurn};
use crate::discovery_store::{DiscoveryStore, PreparedCatalog};
use plasm_core::catalog_discovery::{
    capability_documents, content_hash, CatalogDiscoveryArtifact, EmbeddingProfile,
    DISCOVERY_RENDERER_VERSION,
};
use plasm_core::catalog_il::CatalogManifest;

fn write(path: &std::path::Path, value: &impl Serialize) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn pipeline_deployments() -> DeploymentBindings {
    match std::env::var_os("PLASM_TEST_DEPLOYMENT_BINDINGS") {
        Some(path) => serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
        None => DeploymentBindings::default(),
    }
}

fn remove_orphan_fixture_entities(cgs: &mut CGS) {
    let before = capability_documents(cgs).unwrap();
    let domains: BTreeSet<_> = cgs
        .capabilities
        .values()
        .map(|c| c.domain.clone())
        .collect();
    cgs.entities.retain(|name, _| domains.contains(name));
    assert_eq!(before, capability_documents(cgs).unwrap());
}

fn pipeline_catalogs(count: usize) -> BTreeMap<String, CGS> {
    let repaired =
        plasm_core::loader::load_schema_dir(&root().join("../../schemas/discovery_pipeline_v1"))
            .unwrap();
    let mut catalogs = catalogs(count, false);
    for (entry, cgs) in &mut catalogs {
        let before = capability_documents(cgs).unwrap();
        for (name, capability) in &mut cgs.capabilities {
            let original = if entry == "journal" {
                "record"
            } else {
                name.as_str()
            };
            // Only mappings change: original CGS annotations and ranking documents stay exact.
            capability.mapping = repaired.capabilities[original].mapping.clone();
        }
        assert_eq!(before, capability_documents(cgs).unwrap());
        remove_orphan_fixture_entities(cgs);
    }
    catalogs
}

async fn publish(
    out: &std::path::Path,
    catalogs: &BTreeMap<String, CGS>,
    store: &DiscoveryStore,
) -> String {
    let mut prepared = Vec::new();
    for (entry, source) in catalogs {
        let mut cgs = source.clone();
        cgs.bind_registry_entry_id(entry);
        let cgs = cgs.fresh_catalog_digest();
        let dir = out.join(entry);
        std::fs::create_dir_all(&dir).unwrap();
        let embedded = plasm_catalog::embeddings::embed_documents(
            capability_documents(&cgs).unwrap(),
            &PathBuf::from(std::env::var("PLASM_PIPELINE_EMBEDDING_CACHE").unwrap()),
        )
        .await
        .unwrap();
        write(
            &dir.join("embedding-summary.json"),
            &json!({"cached":embedded.reused_documents,"acquired":embedded.embedded_documents,"logical_batches":embedded.requests}),
        );
        let discovery = CatalogDiscoveryArtifact {
            entry_id: entry.clone(),
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            renderer_version: DISCOVERY_RENDERER_VERSION,
            profile: EmbeddingProfile::default(),
            capabilities: embedded.capabilities,
            prerequisites: cgs.prerequisites.clone(),
        };
        let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
        let manifest = CatalogManifest {
            format_version: 3,
            entry_id: entry.clone(),
            version: cgs.version,
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            label: entry.clone(),
            tags: vec![],
            cgs_json: "body.cgs.json".into(),
            recipes_json: "body.recipes.json".into(),
            recipes_hash: content_hash(&serde_json::to_vec(&compiled).unwrap()),
            discovery_json: "body.discovery.json".into(),
            discovery_hash: content_hash(&serde_json::to_vec(&discovery).unwrap()),
            embedding_profile: EmbeddingProfile::default(),
        };
        write(&dir.join("body.cgs.json"), &cgs);
        write(&dir.join("body.recipes.json"), &compiled);
        write(&dir.join("body.discovery.json"), &discovery);
        write(&dir.join("manifest.json"), &manifest);
        prepared.push(PreparedCatalog::load(&dir.join("manifest.json")).unwrap());
    }
    store
        .import(
            "fixed-production-pipeline",
            prepared,
            &pipeline_deployments(),
        )
        .await
        .unwrap()
}

struct PublishedPipeline<'a> {
    catalogs: &'a BTreeMap<String, CGS>,
    generation: &'a str,
    store: &'a DiscoveryStore,
    service: &'a DiscoveryService,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "paid real-provider MCP discovery evaluation"]
async fn whole_intent_multivendor_mcp_live() {
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::server_state::CatalogBootstrap;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
    use std::sync::Arc;

    let out = PathBuf::from(std::env::var("PLASM_WHOLE_INTENT_REPORT").unwrap());
    assert!(!out.exists(), "report must be fresh");
    std::fs::create_dir_all(&out).unwrap();
    let root = PathBuf::from(crate::test_manifest_dir()).join("../..");
    let cases_path = std::env::var_os("PLASM_WHOLE_INTENT_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("fixtures/discovery/whole-intent-mcp-v1/cases.json"));
    let cases: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(cases_path).unwrap()).unwrap();
    let names_config = std::env::var("PLASM_WHOLE_INTENT_CATALOGS")
        .unwrap_or_else(|_| "hackernews,omdb,openmeteo,graphqlzero,dispatch_fixture".into());
    let names: Vec<_> = names_config.split(',').collect();
    let mut catalogs = BTreeMap::new();
    for &name in &names {
        let path = if name == "dispatch_fixture" {
            root.join("fixtures/schemas/whole_intent_uncertainty")
        } else {
            root.join("apis").join(name)
        };
        let mut cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
        cgs.bind_registry_entry_id(name);
        catalogs.insert(name.to_owned(), cgs);
    }
    let store = DiscoveryStore::connect(&std::env::var("PLASM_DISCOVERY_DATABASE_URL").unwrap())
        .await
        .unwrap();
    store.migrate().await.unwrap();
    let catalog_dir = out.join("catalogs");
    publish(&catalog_dir, &catalogs, &store).await;
    for &name in &names {
        let source = catalog_dir.join(name);
        let mut manifest: CatalogManifest =
            serde_json::from_slice(&std::fs::read(source.join("manifest.json")).unwrap()).unwrap();
        for (old, new) in [
            (&mut manifest.cgs_json, format!("{name}.cgs.json")),
            (&mut manifest.recipes_json, format!("{name}.recipes.json")),
            (
                &mut manifest.discovery_json,
                format!("{name}.discovery.json"),
            ),
        ] {
            std::fs::copy(source.join(&*old), catalog_dir.join(&new)).unwrap();
            *old = new;
        }
        write(
            &catalog_dir.join(format!("{name}.manifest.json")),
            &manifest,
        );
    }
    write(
        &catalog_dir.join("catalog-set.json"),
        &json!({
            "format_version": 3,
            "manifests": names.iter().map(|n| format!("{n}.manifest.json")).collect::<Vec<_>>()
        }),
    );
    write(
        &catalog_dir.join("deployment-bindings.json"),
        &pipeline_deployments(),
    );
    let registry = Arc::new(plasm_core::discovery::CgsRegistry::from_pairs(
        catalogs
            .iter()
            .map(|(id, cgs)| (id.clone(), id.clone(), vec![], Arc::new(cgs.clone())))
            .collect(),
    ));
    let engine = ExecutionEngine::new(ExecutionConfig {
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    let host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry,
        catalog_bootstrap: CatalogBootstrap::CatalogDir { path: catalog_dir },
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .unwrap();
    let generation = host.catalog.activate_discovery().await.unwrap();
    let router = crate::mcp_server::build_mcp_router_for_transport(
        Arc::new(host),
        crate::mcp_server::McpHttpTransport::StreamableHttp,
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .unwrap();
    let init = http.post(&url).header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"whole-intent-eval","version":"1"}}}))
        .send().await.unwrap().error_for_status().unwrap();
    let session = init.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    http.post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let pool = sqlx::PgPool::connect(&std::env::var("PLASM_DISCOVERY_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let started = std::time::Instant::now();
        let response = http.post(&url).header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25").header("Mcp-Session-Id", &session)
            .json(&json!({"jsonrpc":"2.0","id":i+1,"method":"tools/call","params":{"name":"plasm_context","arguments":{"session_mode":"new","intent":case["intent"]}}}))
            .send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
        let message: serde_json::Value = if response.trim_start().starts_with('{') {
            serde_json::from_str(&response).unwrap()
        } else {
            response
                .replace("\r\n", "\n")
                .split("\n\n")
                .filter_map(|event| event.lines().find_map(|line| line.strip_prefix("data:")))
                .filter_map(|data| serde_json::from_str::<serde_json::Value>(data.trim()).ok())
                .find(|message| message["id"] == json!(i + 1))
                .unwrap()
        };
        let intent = case["intent"].as_str().unwrap();
        let envelopes: Vec<String> = sqlx::query_scalar(
            "SELECT envelope FROM discovery_selector_cache WHERE position($1 in envelope) > 0",
        )
        .bind(intent)
        .fetch_all(&pool)
        .await
        .unwrap();
        let receipts: Vec<serde_json::Value> = envelopes
            .iter()
            .filter_map(|v| serde_json::from_str::<serde_json::Value>(v).ok())
            .filter(|v| {
                v["intent"].as_str() == Some(intent)
                    && v["search"]["policy"].as_str()
                        == Some(crate::discovery_matcher::whole_intent::POLICY)
                    && v["retrieval"]["generation"].as_str() == Some(generation.as_str())
            })
            .collect();
        let selected: BTreeSet<_> = receipts
            .iter()
            .filter_map(|v| {
                serde_json::from_value::<crate::discovery_service::RoutingReceipt>(v.clone()).ok()
            })
            .flat_map(|receipt| {
                receipt
                    .matching
                    .selected(&receipt.retrieval)
                    .into_iter()
                    .map(|r| format!("{}/{}", r.catalog, r.capability))
                    .collect::<Vec<_>>()
            })
            .collect();
        let expected: BTreeSet<_> = case["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        let mut errors = Vec::new();
        if !message["error"].is_null() || message["result"]["isError"] == true {
            errors.push("MCP returned an error");
        }
        if message
            .pointer("/result/_meta/plasm/logical_session_ref")
            .is_none()
        {
            errors.push("missing logical session");
        }
        if receipts.is_empty() {
            errors.push("missing persisted routing receipt");
        }
        if selected != expected {
            errors.push("selection differs from expected");
        }
        if let Some(unresolved) = case.get("unresolved").and_then(|v| v.as_str()) {
            if !receipts
                .iter()
                .filter_map(|r| r["intent_analysis"].as_str())
                .flat_map(str::lines)
                .any(|line| {
                    line.to_lowercase().contains(unresolved)
                        && line.to_lowercase().contains("unresolved")
                })
            {
                errors.push("uncertainty missing from receipt");
            }
            let mcp_text = serde_json::to_string(&message["result"])
                .unwrap()
                .to_lowercase();
            if !mcp_text.contains(unresolved) || !mcp_text.contains("unresolved") {
                errors.push("uncertainty missing from MCP response");
            }
        } else if receipts
            .iter()
            .filter_map(|r| r["intent_analysis"].as_str())
            .flat_map(str::lines)
            .any(|line| line.contains(": unresolved"))
        {
            errors.push("supported request falsely marked unresolved");
        }
        if !errors.is_empty() {
            failures.push(case["id"].as_str().unwrap().to_owned());
        }
        results.push(json!({"case":case,"seconds":started.elapsed().as_secs_f64(),"mcp":message,"receipts":receipts,"selected":selected,"errors":errors}));
        write(&out.join("RESULTS.json"), &results);
    }
    server.abort();
    assert!(
        failures.is_empty(),
        "MCP discovery failed cases: {failures:?}"
    );
}

#[tokio::test]
#[ignore = "paid whole-intent discovery service evaluation"]
async fn whole_intent_multivendor_service_live() {
    let out = PathBuf::from(std::env::var("PLASM_WHOLE_INTENT_REPORT").unwrap());
    assert!(!out.exists(), "report must be fresh");
    std::fs::create_dir_all(&out).unwrap();
    let store = DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    store.migrate().await.unwrap();
    let root = PathBuf::from(crate::test_manifest_dir()).join("../..");
    let cases_path = std::env::var_os("PLASM_WHOLE_INTENT_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("fixtures/discovery/whole-intent-live-v1/cases.json"));
    let cases: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(cases_path).unwrap()).unwrap();
    let entries: BTreeSet<_> = cases
        .iter()
        .flat_map(|case| {
            case["catalogs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
        })
        .collect();
    let mut catalogs = BTreeMap::new();
    for name in entries {
        let path = if name == "dispatch_fixture" {
            root.join("fixtures/schemas/whole_intent_uncertainty")
        } else {
            root.join("apis").join(&name)
        };
        let mut cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
        cgs.bind_registry_entry_id(&name);
        catalogs.insert(name, cgs);
    }
    let generation = publish(&out.join("catalogs"), &catalogs, &store).await;
    let service = DiscoveryService::from_env(
        DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
            .await
            .unwrap(),
    )
    .unwrap();
    let mut results = Vec::new();
    for case in cases {
        let allowed = DiscoveryAuthorization::catalogs(
            case["catalogs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect(),
        );
        let intent =
            IntentProvenance::from_turns([case["intent"].as_str().unwrap().to_owned()]).unwrap();
        let started = std::time::Instant::now();
        let receipt = service
            .route_turn(RouteTurn {
                new_generation: &generation,
                intent_provenance: &intent,
                logical_session: None,
                allowed: &allowed,
                exposed: &[],
                expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(3600),
            })
            .await
            .unwrap();
        let selected: BTreeSet<_> = receipt
            .matching
            .selected(&receipt.retrieval)
            .into_iter()
            .map(|r| format!("{}/{}", r.catalog, r.capability))
            .collect();
        let seconds = started.elapsed().as_secs_f64();
        let replay = if results.is_empty() {
            let replay = service
                .route_turn(RouteTurn {
                    new_generation: &generation,
                    intent_provenance: &intent,
                    logical_session: None,
                    allowed: &allowed,
                    exposed: &[],
                    expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(3600),
                })
                .await
                .unwrap();
            let replay_selected: BTreeSet<_> = replay
                .matching
                .selected(&replay.retrieval)
                .into_iter()
                .map(|r| format!("{}/{}", r.catalog, r.capability))
                .collect();
            Some(
                json!({"selected":replay_selected,"requests":replay.search.requests,"input_bytes":replay.search.input_bytes,"reported_input_tokens":replay.search.reported_input_tokens}),
            )
        } else {
            None
        };
        let expected: Vec<_> = case["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let forbidden: Vec<_> = case["forbidden"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let missing: Vec<_> = expected
            .into_iter()
            .filter(|name| !selected.contains(*name))
            .collect();
        let unexpected: Vec<_> = selected
            .iter()
            .filter(|name| {
                !case["expected"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v.as_str() == Some(name.as_str()))
            })
            .cloned()
            .collect();
        let forbidden_exposed: Vec<_> = forbidden
            .into_iter()
            .filter(|name| selected.contains(*name))
            .collect();
        let expected_gap = case.get("unresolved").and_then(|v| v.as_str());
        let gaps: Vec<_> = receipt
            .intent_analysis
            .lines()
            .filter(|line| line.contains(": unresolved — "))
            .collect();
        let unresolved_ok = match expected_gap {
            Some(needle) => {
                gaps.len() == 1 && gaps[0].to_lowercase().contains(&needle.to_lowercase())
            }
            None => gaps.is_empty(),
        };
        let constraints_ok = !case["intent"].as_str().unwrap().contains("Do not ")
            || receipt.intent_analysis.contains("- Constraint:");
        let unauthorized: Vec<_> = receipt
            .retrieval
            .candidates
            .iter()
            .filter(|candidate| !allowed.permits(&candidate.reference))
            .map(|candidate| {
                format!(
                    "{}/{}",
                    candidate.reference.catalog, candidate.reference.capability
                )
            })
            .collect();
        let cache_ok = replay.as_ref().is_none_or(|value| {
            value["requests"] == 0
                && value["input_bytes"] == 0
                && value["reported_input_tokens"] == 0
                && value["selected"] == json!(selected)
        });
        results.push(json!({"case":case,"seconds":seconds,"missing":missing,"unexpected":unexpected,"forbidden_exposed":forbidden_exposed,"unresolved_ok":unresolved_ok,"constraints_ok":constraints_ok,"cache_ok":cache_ok,"replay":replay,"unauthorized":unauthorized,"selected":selected,"receipt":receipt}));
        write(&out.join("RESULTS.json"), &results);
    }
    assert!(
        results
            .iter()
            .all(|r| r["missing"].as_array().unwrap().is_empty()
                && r["unexpected"].as_array().unwrap().is_empty()
                && r["forbidden_exposed"].as_array().unwrap().is_empty()
                && r["unauthorized"].as_array().unwrap().is_empty()
                && r["unresolved_ok"] == true
                && r["constraints_ok"] == true
                && r["cache_ok"] == true),
        "whole-intent service evaluation failed; inspect RESULTS.json"
    );
}

async fn measure_live(
    out: &std::path::Path,
    fixture: &Fixture,
    case: &Case,
    count: usize,
    rotate: bool,
    pipeline: PublishedPipeline<'_>,
) -> serde_json::Value {
    let PublishedPipeline {
        catalogs,
        generation,
        store,
        service,
    } = pipeline;
    let allowed = DiscoveryAuthorization::catalogs(catalogs.keys().cloned().collect());
    let mut exposed = BTreeSet::new();
    let mut history = Vec::new();
    let mut visits = BTreeMap::new();
    let mut pin = None;
    let mut session = None;
    let mut turns = Vec::new();
    std::fs::create_dir_all(out).unwrap();
    for turn in 0..fixture.horizon {
        let request = case.requests[if rotate {
            turn % case.requests.len()
        } else {
            0
        }]
        .clone();
        history.push(request.clone());
        let intent = IntentProvenance::from_turns(history.clone()).unwrap();
        let existing: Vec<_> = exposed.iter().cloned().collect();
        let start = std::time::Instant::now();
        let receipt = service
            .route_turn(RouteTurn {
                new_generation: generation,
                intent_provenance: &intent,
                logical_session: pin.as_deref(),
                allowed: &allowed,
                exposed: &existing,
                expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(86400),
            })
            .await
            .unwrap();
        let elapsed_ms = start.elapsed().as_millis();
        pin = Some(receipt.pin_id.clone());
        write(&out.join(format!("receipt-{turn:02}.json")), &receipt);
        assert!(receipt.search.requests <= 3 && receipt.search.input_bytes <= 48000);
        // Diagnostic reread after measurement; the query vector is already cached.
        let recall = store
            .retrieve(
                generation,
                &request,
                &allowed,
                &crate::discovery_store::RecallFrontier::from_state(&existing, &visits),
            )
            .await
            .unwrap();
        write(&out.join(format!("recall-{turn:02}.json")), &recall);
        for candidate in &receipt.retrieval.candidates {
            if receipt
                .matching
                .matches
                .iter()
                .any(|m| m.capability_id == candidate.id)
            {
                *visits.entry(candidate.reference.clone()).or_insert(0usize) += 1;
            }
        }
        let before = exposed.clone();
        for candidate in &receipt.retrieval.candidates {
            if receipt
                .matching
                .matches
                .iter()
                .any(|m| m.capability_id == candidate.id && m.choice != MatchChoice::Unrelated)
            {
                exposed.insert(candidate.reference.clone());
            }
        }
        for edge in required(&exposed, catalogs, &DeploymentBindings::default(), &allowed).unwrap()
        {
            exposed.insert(edge.source);
        }
        for (entry, cgs) in catalogs {
            let names = exposed
                .difference(&before)
                .filter(|r| &r.catalog == entry)
                .map(|r| r.capability.clone())
                .collect::<Vec<_>>();
            teach_catalog(&mut session, cgs, entry, &names);
        }
        let relation_visible = session.as_ref().is_some_and(|s| {
            s.exposed_relation_symbol_rows()
                .iter()
                .any(|r| r.entity == "Cabinet" && r.wire == "specimens")
        });
        let outcomes = case
            .outcomes
            .iter()
            .map(|path| {
                path.iter().all(|n| exposed.contains(&key(n)))
                    && (!case.relation || relation_visible)
            })
            .collect::<Vec<_>>();
        let status = case.labels.keys().map(|name| {
            let reference = key(name);
            let candidate = receipt.retrieval.candidates.iter().find(|c| c.reference == reference);
            let decision = candidate.and_then(|c| receipt.matching.matches.iter().find(|m| m.capability_id == c.id));
            let stage = if exposed.contains(&reference) {"retained"}
                else if decision.is_some_and(|m| m.choice == MatchChoice::Unrelated) {"judgment_rejected"}
                else if receipt.search.deferred.iter().any(|d| d.reference == reference) {"budget_deferred"}
                else if !recall.candidates.iter().any(|c| c.reference == reference) {"recall_absent"}
                else {"allocation_unvisited"};
            json!({"target":reference,"stage":stage,"recall_rank":recall.candidates.iter().position(|c| c.reference == reference).map(|n|n+1)})
        }).collect::<Vec<_>>();
        turns.push(json!({"search":turn+1,"outcomes":outcomes,"status":status,"newly_exposed":exposed.difference(&before).collect::<Vec<_>>(),"exposed":exposed,"relation_visible":relation_visible,"whole_ms":elapsed_ms,"search_receipt":receipt.search,"lexical_count":recall.lexical_count,"vector_count":recall.vector_count}));
        write(&out.join("turns.json"), &turns);
    }
    json!({"id":case.id,"distractors":count,"rotating":rotate,"gate":fixture.gate,"turns":turns})
}

fn role_catalogs(profile: &serde_json::Value, count: usize) -> BTreeMap<String, CGS> {
    let mut cgs = pipeline_catalogs(0).remove("workshop").unwrap();
    let template = cgs.capabilities["routine"].clone();
    cgs.capabilities.clear();
    let descriptions = [
        (
            "routine".to_owned(),
            profile["left_effect"].as_str().unwrap(),
        ),
        (
            "record".to_owned(),
            profile["right_effect"].as_str().unwrap(),
        ),
    ]
    .into_iter()
    .chain((0..count).map(|i| {
        (
            format!("context_{i:04}"),
            profile["distractor"].as_str().unwrap(),
        )
    }));
    for (name, description) in descriptions {
        let mut cap = template.clone();
        cap.name = name.clone().into();
        cap.description = description.into();
        cap.output_schema.as_mut().unwrap().output_type =
            plasm_core::schema::OutputType::SideEffect {
                description: description.into(),
            };
        cgs.capabilities.insert(name.into(), cap);
    }
    remove_orphan_fixture_entities(&mut cgs);
    BTreeMap::from([("workshop".into(), cgs)])
}

#[tokio::test]
#[ignore = "explicit live frozen production pipeline run; real embeddings and decisions"]
async fn frozen_production_discovery_pipeline() {
    let out = PathBuf::from(std::env::var("PLASM_PIPELINE_REPORT").unwrap());
    let store = DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    store.migrate().await.unwrap();
    let service = DiscoveryService::from_env(
        DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
            .await
            .unwrap(),
    )
    .unwrap();
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    let mut results = Vec::new();
    let continuation = std::env::var_os("PLASM_PIPELINE_SKIP_ZERO_TRAJECTORY").is_some();
    for count in &fixture.growth {
        if continuation && *count == 0 {
            continue;
        }
        let catalogs = pipeline_catalogs(*count);
        let generation = publish(
            &out.join(format!("catalogs/trajectory-{count}")),
            &catalogs,
            &store,
        )
        .await;
        for case in &fixture.cases {
            for rotate in [false, true] {
                let id = format!("trajectory-{}-{count}-{rotate}", case.id);
                results.push(
                    measure_live(
                        &out.join(&id),
                        &fixture,
                        case,
                        *count,
                        rotate,
                        PublishedPipeline {
                            catalogs: &catalogs,
                            generation: &generation,
                            store: &store,
                            service: &service,
                        },
                    )
                    .await,
                );
                write(&out.join("RESULTS.json"), &results);
            }
        }
    }
    let directory = root().join("../action-role-pairs-v1");
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(directory.join("cases.json")).unwrap()).unwrap();
    let profiles: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(directory.join("profiles.json")).unwrap()).unwrap();
    for profile in profiles {
        let profile_id = profile["id"].as_str().unwrap();
        for count in &fixture.growth {
            let catalogs = role_catalogs(&profile, *count);
            let generation = publish(
                &out.join(format!("catalogs/roles-{profile_id}-{count}")),
                &catalogs,
                &store,
            )
            .await;
            for side in ["left", "right"] {
                let case = fixture
                    .cases
                    .iter()
                    .find(|c| c.id == format!("{profile_id}/{side}"))
                    .unwrap();
                results.push(
                    measure_live(
                        &out.join(format!("roles-{profile_id}-{side}-{count}")),
                        &fixture,
                        case,
                        *count,
                        false,
                        PublishedPipeline {
                            catalogs: &catalogs,
                            generation: &generation,
                            store: &store,
                            service: &service,
                        },
                    )
                    .await,
                );
                write(&out.join("RESULTS.json"), &results);
            }
        }
    }
    assert_eq!(results.len(), if continuation { 28 } else { 34 });
}

#[test]
fn production_pipeline_catalog_preflight() {
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(root().join("cases.json")).unwrap()).unwrap();
    let mut sets = fixture
        .growth
        .iter()
        .map(|n| pipeline_catalogs(*n))
        .collect::<Vec<_>>();
    let directory = root().join("../action-role-pairs-v1");
    let roles: Fixture =
        serde_json::from_slice(&std::fs::read(directory.join("cases.json")).unwrap()).unwrap();
    let profiles: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(directory.join("profiles.json")).unwrap()).unwrap();
    for profile in profiles {
        for count in &roles.growth {
            sets.push(role_catalogs(&profile, *count));
        }
    }
    for catalogs in sets {
        for (entry, mut cgs) in catalogs {
            cgs.bind_registry_entry_id(&entry);
            let cgs = cgs.fresh_catalog_digest();
            plasm_core::catalog_il::load_catalog_il_verified(
                &serde_json::to_vec(&cgs).unwrap(),
                &cgs.catalog_cgs_hash_hex(),
            )
            .unwrap();
            capability_documents(&cgs).unwrap();
            cgs.prerequisites.validate(&cgs).unwrap();
            let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
            plasm_compile::CompiledCatalog::decode_artifact(
                &serde_json::to_vec(&compiled).unwrap(),
                &cgs,
            )
            .unwrap();
        }
    }
}
