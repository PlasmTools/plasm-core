//! Deterministic model-boundary fixtures for real PostgreSQL discovery.
//! No benchmark data, embeddings service or selector model is contacted.
use plasm_agent_core::{
    discovery_store::{DiscoveryAuthorization, DiscoveryStore},
    intent_provenance::IntentProvenance,
};
use plasm_core::{
    catalog_discovery::{
        capability_documents, content_hash, CatalogDiscoveryArtifact, EmbeddedCapability,
        EmbeddingProfile, DISCOVERY_RENDERER_VERSION,
    },
    catalog_il::{CatalogManifest, CatalogSetManifest},
    prerequisites::{CapabilityRef, DeploymentBindings},
    CGS,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub fn pack(path: &Path, mut cgs: CGS) -> CGS {
    cgs.bind_registry_entry_id("matrix");
    let cgs = cgs.fresh_catalog_digest();
    let hash = cgs.catalog_cgs_hash_hex();
    let capabilities = capability_documents(&cgs)
        .unwrap()
        .into_iter()
        .map(|document| {
            let mut embedding = vec![0.0; 1536];
            embedding[0] = 1.0;
            EmbeddedCapability {
                document,
                embedding,
            }
        })
        .collect();
    let discovery = CatalogDiscoveryArtifact {
        entry_id: "matrix".into(),
        cgs_hash: hash.clone(),
        renderer_version: DISCOVERY_RENDERER_VERSION,
        profile: EmbeddingProfile::default(),
        capabilities,
        prerequisites: cgs.prerequisites.clone(),
    };
    let discovery = serde_json::to_vec(&discovery).unwrap();
    let recipes =
        serde_json::to_vec(&plasm_compile::compile_cgs_capability_templates(&cgs).unwrap())
            .unwrap();
    let manifest = CatalogManifest {
        format_version: 3,
        entry_id: "matrix".into(),
        version: cgs.version,
        cgs_hash: hash,
        label: "MCP matrix".into(),
        tags: vec![],
        cgs_json: "matrix.cgs.json".into(),
        recipes_json: "matrix.recipes.json".into(),
        recipes_hash: content_hash(&recipes),
        discovery_json: "matrix.discovery.json".into(),
        discovery_hash: content_hash(&discovery),
        embedding_profile: EmbeddingProfile::default(),
    };
    for (name, bytes) in [
        (
            "matrix.cgs.json",
            plasm_core::catalog_il::cgs_to_catalog_il_bytes(&cgs).unwrap(),
        ),
        ("matrix.recipes.json", recipes),
        ("matrix.discovery.json", discovery),
        (
            "matrix.manifest.json",
            serde_json::to_vec(&manifest).unwrap(),
        ),
        (
            "catalog-set.json",
            serde_json::to_vec(&CatalogSetManifest {
                format_version: 3,
                manifests: vec!["matrix.manifest.json".into()],
            })
            .unwrap(),
        ),
        (
            "deployment-bindings.json",
            serde_json::to_vec(&DeploymentBindings::default()).unwrap(),
        ),
    ] {
        std::fs::write(path.join(name), bytes).unwrap();
    }
    cgs
}

pub async fn seed_model_boundary(
    store: &DiscoveryStore,
    generation: &str,
    cgs: &CGS,
    turns: &[&str],
) {
    let url = std::env::var("PLASM_DISCOVERY_DATABASE_URL").expect("isolated test database");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let intent = *turns.last().unwrap();
    let profile = EmbeddingProfile::default();
    let key = content_hash(&serde_json::to_vec(&(intent, &profile)).unwrap());
    let mut vector = vec![0.0; 1536];
    vector[0] = 1.0;
    sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector) ON CONFLICT DO NOTHING")
        .bind(key).bind(serde_json::to_value(profile).unwrap()).bind(serde_json::to_string(&vector).unwrap()).execute(&pool).await.unwrap();
    pool.close().await;
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let retrieval = store.retrieve(generation, intent, &allowed).await.unwrap();
    let provenance = IntentProvenance::from_turns(turns.iter().map(|s| (*s).to_owned())).unwrap();
    let model = plasm_agent_core::discovery_matcher::JEV_MODEL;
    for batch in
        plasm_agent_core::discovery_matcher::issue_batches(model, &provenance, &retrieval).unwrap()
    {
        let body: Value = serde_json::from_str(batch.body()).unwrap();
        let answers: BTreeMap<_,_> = body["questions"].as_object().unwrap().keys().map(|key| (key.clone(),json!({"type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"unrelated":0.0,"uncertain":0.0}}))).collect();
        store
            .store_selector_envelope(
                batch.cache_key(),
                &json!({"model":model,"provider":"TypeSafe","answers":answers}).to_string(),
            )
            .await
            .unwrap();
    }
    let available: BTreeSet<_> = cgs
        .capabilities
        .keys()
        .map(|cap| CapabilityRef {
            catalog: "matrix".into(),
            capability: cap.to_string(),
        })
        .collect();
    let support = plasm_agent_core::discovery_support::issue(
        model,
        &provenance,
        &available,
        &BTreeMap::from([("matrix".into(), cgs.clone())]),
    )
    .unwrap();
    let body: Value = serde_json::from_str(&support.body).unwrap();
    let answers: BTreeMap<_,_> = body["questions"].as_object().unwrap().keys().map(|key| (key.clone(),json!({"type":"choice","choice":"established","confidence":1.0,"probabilities":{"established":1.0,"not_established":0.0,"undetermined":0.0}}))).collect();
    store
        .store_selector_envelope(
            &support.cache_key,
            &json!({"model":model,"provider":"TypeSafe","answers":answers}).to_string(),
        )
        .await
        .unwrap();
}
