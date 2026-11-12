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
            &DeploymentBindings::default(),
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
