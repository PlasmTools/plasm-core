//! Export deterministic abstract catalogs for native-addon/client conformance.
//! Synthetic vectors are test data; this is not a discovery-quality benchmark.
use plasm_core::catalog_discovery::{
    capability_documents, content_hash, CatalogDiscoveryArtifact, EmbeddedCapability,
};
use plasm_core::catalog_il::{
    cgs_to_catalog_il_bytes, CatalogManifest, CatalogSetManifest, PLASM_CATALOG_FORMAT_VERSION,
};

#[test]
fn export_packed_client_fixtures() {
    let Some(output) = std::env::var_os("PLASM_CLIENT_FIXTURE_DIR") else {
        return;
    };
    let output = std::path::PathBuf::from(output);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, relative) in [
        (
            "plasm_language_matrix",
            "fixtures/schemas/plasm_language_matrix",
        ),
        (
            "capability_with_input",
            "fixtures/schemas/capability_with_input",
        ),
        (
            "python_union_matrix",
            "fixtures/schemas/python_union_matrix",
        ),
        (
            "execute_tiny",
            "crates/plasm-agent-core/tests/fixtures/execute_tiny",
        ),
    ] {
        let mut cgs = plasm_core::load_schema_dir(&root.join(relative)).unwrap();
        cgs.bind_registry_entry_id(name);
        cgs.validate().unwrap();
        let recipes =
            serde_json::to_vec(&plasm_compile::compile_cgs_capability_templates(&cgs).unwrap())
                .unwrap();
        let artifact = CatalogDiscoveryArtifact {
            entry_id: name.into(),
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            renderer_version: plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION,
            profile: Default::default(),
            prerequisites: cgs.prerequisites.clone(),
            capabilities: capability_documents(&cgs)
                .unwrap()
                .into_iter()
                .map(|document| EmbeddedCapability {
                    document,
                    embedding: vec![0.375; 1536],
                })
                .collect(),
        };
        let discovery = serde_json::to_vec(&artifact).unwrap();
        let manifest = CatalogManifest {
            format_version: PLASM_CATALOG_FORMAT_VERSION,
            entry_id: name.into(),
            version: cgs.version,
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            label: name.into(),
            tags: vec![],
            cgs_json: format!("{name}.cgs.json"),
            recipes_json: format!("{name}.recipes.json"),
            recipes_hash: content_hash(&recipes),
            discovery_json: format!("{name}.discovery.json"),
            discovery_hash: content_hash(&discovery),
            embedding_profile: Default::default(),
        };
        let dir = output.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(&manifest.cgs_json),
            cgs_to_catalog_il_bytes(&cgs).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join(&manifest.recipes_json), recipes).unwrap();
        std::fs::write(dir.join(&manifest.discovery_json), discovery).unwrap();
        std::fs::write(
            dir.join(format!("{name}.manifest.json")),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("catalog-set.json"),
            serde_json::to_vec(&CatalogSetManifest {
                format_version: PLASM_CATALOG_FORMAT_VERSION,
                manifests: vec![format!("{name}.manifest.json")],
            })
            .unwrap(),
        )
        .unwrap();
        plasm_core::catalog_il::load_catalog_il_verified(
            &std::fs::read(dir.join(&manifest.cgs_json)).unwrap(),
            &manifest.cgs_hash,
        )
        .unwrap();
    }
}
