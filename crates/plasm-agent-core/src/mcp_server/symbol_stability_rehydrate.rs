//! Cross-pod rehydrate symbol stability (durable descriptor + embedded ledger).

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::execute_session::SessionReuseKey;
    use crate::execute_session_rehydrate::rehydrate_execute_session;
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::http_execute::{apply_capability_seeds, CapabilitySeed, RankedCapabilitiesArg};
    use crate::mcp_transport_store::execute_session_registry::PersistedExecuteSessionDescriptor;
    use crate::server_state::PlasmHostState;
    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;
    use plasm_core::CgsCatalog;
    use uuid::Uuid;

    fn github_fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github")
    }

    fn github_host() -> Option<PlasmHostState> {
        let dir = github_fixture_dir();
        if !dir.is_dir() {
            return None;
        }
        let cgs = Arc::new(load_schema_dir(&dir).ok()?);
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "github".into(),
            "GitHub".into(),
            vec!["github".into()],
            cgs.clone(),
        )]);
        let engine =
            plasm_runtime::ExecutionEngine::new(plasm_runtime::ExecutionConfig::default()).ok()?;
        Some(build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: plasm_runtime::ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        }))
    }

    fn repo_issue_label_seeds() -> Vec<CapabilitySeed> {
        vec![
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Repository".into(),
            },
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Issue".into(),
            },
            CapabilitySeed {
                entry_id: "github".into(),
                entity: "Label".into(),
            },
        ]
    }

    /// Multi-replica path: durable descriptor + embedded ledger rehydrate preserves `m#`.
    #[tokio::test]
    async fn symbol_stability_incremental_expand_rehydrate_preserves_m_symbols() {
        let Some(st) = github_host() else {
            return;
        };
        let st = Arc::new(st);
        let logical_id = Uuid::new_v4();
        let intent = "create branch for label documentation workflow";

        let seeds_open = repo_issue_label_seeds();
        let out = apply_capability_seeds(
            st.as_ref(),
            None,
            None,
            seeds_open.clone(),
            None,
            None,
            Some(logical_id),
            intent,
            RankedCapabilitiesArg::Unspecified,
        )
        .await
        .expect("open");

        let mut seeds_extend = seeds_open;
        seeds_extend.push(CapabilitySeed {
            entry_id: "github".into(),
            entity: "Branch".into(),
        });
        let out_extend = apply_capability_seeds(
            st.as_ref(),
            None,
            Some((out.prompt_hash.as_str(), out.session_id.as_str())),
            seeds_extend,
            None,
            None,
            Some(logical_id),
            intent,
            RankedCapabilitiesArg::Unspecified,
        )
        .await
        .expect("extend");

        let es = st
            .get_execute_session(&out_extend.prompt_hash, &out_extend.session_id)
            .await
            .expect("live session");
        let live_map = es
            .teaching_exposure
            .as_ref()
            .expect("exposure")
            .symbol_map_arc();
        let m_branch = live_map.method_sym_for("github", "Repository", "repo_branch_create");

        let reuse_key = SessionReuseKey {
            tenant_scope: es.tenant_scope.clone(),
            entry_id: es.entry_id.clone(),
            catalog_cgs_hash: es.catalog_cgs_hash.clone(),
            entities: es.entities.clone(),
            context_intent: es.context_intent.clone(),
            ranked_capabilities: es.ranked_capabilities.clone(),
            principal: es.principal.clone(),
            logical_session_id: Some(logical_id.hyphenated().to_string()),
        };
        let exposure =
            crate::execute_session_materialize::build_durable_exposure_snapshot(st.as_ref(), &es)
                .await
                .expect("durable exposure snapshot");
        let mut desc = PersistedExecuteSessionDescriptor::from_session_and_durable_snapshot(
            &es,
            out_extend.session_id.as_str(),
            &reuse_key,
            es.snapshot_bind_credentials().await,
            &exposure,
        );
        desc.expires_at_unix = u64::MAX;
        let reg = st.catalog.snapshot();
        desc.registry_catalog_hashes_by_entry = HashMap::from([(
            "github".into(),
            reg.load_context("github")
                .expect("github")
                .cgs
                .catalog_cgs_hash_hex(),
        )]);
        assert!(
            !desc.symbol_ledger_bytes.is_empty(),
            "descriptor must embed symbol ledger bytes"
        );

        let rehydrated = rehydrate_execute_session(st.as_ref(), &desc)
            .await
            .expect("ledger rehydrate");
        let re_map = rehydrated
            .teaching_exposure
            .as_ref()
            .expect("rehydrated exposure")
            .symbol_map_arc();
        assert_eq!(
            re_map.method_sym_for("github", "Repository", "repo_branch_create"),
            m_branch,
            "rehydrate must restore exact branch-create m#"
        );
        assert_eq!(
            re_map
                .resolve_method_symbol_triple(m_branch.as_str())
                .map(|(_, _, cap)| cap.to_string()),
            Some("repo_branch_create".into())
        );
    }
}
