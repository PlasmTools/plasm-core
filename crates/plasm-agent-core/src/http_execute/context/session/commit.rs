//! Shared exposure-wave commit tail for federate and expand.

use super::super::super::*;

use super::super::seeds::{
    format_exposure_entity_cheat_sheet, wrap_teaching_markdown_literal_block,
};
/// Snapshot captured before an exposure wave mutates [`plasm_core::TeachingExposureSession`].
pub(crate) struct ExposureWaveSnapshot {
    pub slots_before: std::collections::BTreeSet<plasm_core::symbol_tuning::ExposureSlotKey>,
    pub caps_before: std::collections::BTreeSet<plasm_core::symbol_tuning::ExposureCapabilityKey>,
    pub entity_count_before: usize,
    pub relation_keys: Vec<plasm_core::ExposureEntityKey>,
    pub ranked_capability_names: Option<Vec<String>>,
}

/// What changed during one exposure wave (after surface merge + relation admission).
#[derive(Debug, Clone, Default)]
pub(crate) struct ExposureWaveChanges {
    pub added_entities: Vec<plasm_core::ExposureEntityKey>,
    pub new_relation_slots: Vec<plasm_core::symbol_tuning::ExposureSlotKey>,
    pub new_capabilities:
        std::collections::BTreeSet<plasm_core::symbol_tuning::ExposureCapabilityKey>,
}

impl ExposureWaveChanges {
    pub(crate) fn surface_unchanged(&self) -> bool {
        self.added_entities.is_empty()
            && self.new_relation_slots.is_empty()
            && self.new_capabilities.is_empty()
    }
}

/// Outcome of committing one exposure-wave delta to an execute session.
pub(crate) struct CommittedWaveDelta {
    /// Rendered teaching delta wrapped for the prompt (may include ranked diagnostics only).
    pub markdown: String,
    pub relations_delta: Vec<plasm_core::ExposedRelationSymbolRow>,
    /// Exposure surface did not gain entities, relation slots, or capabilities.
    pub surface_unchanged: bool,
}

fn append_ranked_replay_diagnostics(
    markdown: &mut String,
    exp: &plasm_core::TeachingExposureSession,
    ranked_names: Option<&[String]>,
    caps_before: &std::collections::BTreeSet<plasm_core::symbol_tuning::ExposureCapabilityKey>,
) {
    let Some(names) = ranked_names.filter(|n| !n.is_empty()) else {
        return;
    };
    let diag = plasm_core::prompt_render::format_ranked_replay_diagnostics(exp, names, caps_before);
    if markdown.trim().is_empty() {
        *markdown = format!("{diag}\n");
    } else {
        markdown.push_str("\n\n");
        markdown.push_str(&diag);
        markdown.push('\n');
    }
}

fn compute_exposure_wave_changes(
    exp: &plasm_core::TeachingExposureSession,
    snapshot: &ExposureWaveSnapshot,
) -> ExposureWaveChanges {
    let added_entities = exp.qualified_entities_since(snapshot.entity_count_before);
    let new_relation_slots = exp.relation_slots_for_expand_wave(
        &snapshot.slots_before,
        &added_entities,
        &snapshot.relation_keys,
    );
    let new_capabilities: std::collections::BTreeSet<_> = exp
        .surface
        .capabilities
        .difference(&snapshot.caps_before)
        .cloned()
        .collect();
    ExposureWaveChanges {
        added_entities,
        new_relation_slots,
        new_capabilities,
    }
}

fn render_exposure_wave_markdown(
    st: &PlasmHostState,
    sess: &crate::execute_session::ExecuteSession,
    exp: &plasm_core::TeachingExposureSession,
    changes: &ExposureWaveChanges,
) -> String {
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let pipeline = st.engine.prompt_pipeline();

    let delta = if !changes.added_entities.is_empty() || !changes.new_relation_slots.is_empty() {
        if sess.contexts_by_entry.len() > 1 {
            let by_entry: IndexMap<String, &CGS> = sess
                .contexts_by_entry
                .iter()
                .map(|(k, v)| (k.clone(), v.cgs.as_ref()))
                .collect();
            pipeline.render_teaching_exposure_delta_federated_with_edges(
                &by_entry,
                exp,
                &changes.added_entities,
                &changes.new_relation_slots,
                Some(sym_cross),
            )
        } else {
            let added: Vec<&str> = changes
                .added_entities
                .iter()
                .map(|k| k.entity.as_str())
                .collect();
            pipeline.render_teaching_exposure_delta_with_edges(
                sess.cgs.as_ref(),
                exp,
                &added,
                &changes.new_relation_slots,
                Some(sym_cross),
            )
        }
    } else if !changes.new_capabilities.is_empty() {
        if sess.contexts_by_entry.len() > 1 {
            let by_entry: IndexMap<String, &CGS> = sess
                .contexts_by_entry
                .iter()
                .map(|(k, v)| (k.clone(), v.cgs.as_ref()))
                .collect();
            pipeline.render_teaching_new_capabilities_delta_federated(
                &by_entry,
                exp,
                &changes.new_capabilities,
                Some(sym_cross),
            )
        } else {
            pipeline.render_teaching_new_capabilities_delta(
                sess.cgs.as_ref(),
                exp,
                &changes.new_capabilities,
                Some(sym_cross),
            )
        }
    } else {
        String::new()
    };

    wrap_teaching_markdown_literal_block(&delta, pipeline.render_mode)
}

/// Admit new relation slots, render + append the teaching delta, and persist the session.
pub(crate) async fn commit_exposure_wave_delta(
    st: &PlasmHostState,
    prompt_hash_p: &PromptHashHex,
    session_id_p: &ExecuteSessionId,
    mut sess: crate::execute_session::ExecuteSession,
    mut exp: plasm_core::TeachingExposureSession,
    snapshot: ExposureWaveSnapshot,
) -> Result<CommittedWaveDelta, super::SessionMutateError> {
    {
        let layers: Vec<&CGS> = sess
            .contexts_by_entry
            .values()
            .map(|c| c.cgs.as_ref())
            .collect();
        let added_qualified = exp.qualified_entities_since(snapshot.entity_count_before);
        let new_relation_slots = exp.relation_slots_for_expand_wave(
            &snapshot.slots_before,
            &added_qualified,
            &snapshot.relation_keys,
        );
        exp.admit_relation_edge_slots_for_render(&layers, &new_relation_slots);
    }

    let changes = compute_exposure_wave_changes(&exp, &snapshot);
    let relations_delta = exp.relations_delta_rows_for_slots(&changes.new_relation_slots);
    let ranked_slice = snapshot.ranked_capability_names.as_deref();

    if changes.surface_unchanged() {
        let mut markdown = String::new();
        append_ranked_replay_diagnostics(&mut markdown, &exp, ranked_slice, &snapshot.caps_before);
        sess.entities = exp.entities.clone();
        sess.teaching_exposure = Some(exp);
        st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
            .await?;
        return Ok(CommittedWaveDelta {
            markdown,
            relations_delta: Vec::new(),
            surface_unchanged: true,
        });
    }

    let mut wave = render_exposure_wave_markdown(st, &sess, &exp, &changes);
    if ranked_slice.is_some_and(|n| !n.is_empty()) && changes.new_capabilities.is_empty() {
        append_ranked_replay_diagnostics(&mut wave, &exp, ranked_slice, &snapshot.caps_before);
    }
    let cheat = format_exposure_entity_cheat_sheet(&exp);
    if !cheat.is_empty() {
        if !wave.trim().is_empty() {
            wave.push_str("\n\n");
        }
        wave.push_str(&cheat);
    }

    if !wave.trim().is_empty() {
        sess.prompt_text.push_str("\n\n");
        sess.prompt_text.push_str(&wave);
        sess.domain_revision = sess.domain_revision.saturating_add(1);
    }
    sess.entities = exp.entities.clone();
    sess.teaching_exposure = Some(exp);
    st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
        .await?;

    Ok(CommittedWaveDelta {
        markdown: wave,
        relations_delta,
        surface_unchanged: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::mcp_transport_store::ExecuteSessionRegistry;
    use crate::run_artifacts::RunArtifactStore;
    use crate::server_state::CatalogBootstrap;
    use crate::test_support::session_fixtures::ExecuteSessionFixture;
    use plasm_core::discovery::{
        derive_intent_exposure_surface_batch, ExposureSurfaceOptions, InMemoryCgsRegistry,
        MutatorAdmit,
    };
    use plasm_core::TeachingExposureSession;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
    use std::sync::Arc;

    use crate::http_execute::context::ranked_replay_fixtures::{
        matrix_cgs_arc, matrix_langitem_endpoints,
    };

    fn matrix_host(cgs: Arc<plasm_core::CGS>) -> PlasmHostState {
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        let (reg, _store) = ExecuteSessionRegistry::with_test_json_store();
        let mut st = build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
                "matrix".into(),
                "Matrix".into(),
                vec!["LangItem".into()],
                cgs.clone(),
            )])),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        });
        st.oss.execute_session_registry = reg;
        st
    }

    #[test]
    fn exposure_wave_changes_surface_unchanged_when_only_ranked_replay_no_op() {
        let exp = crate::http_execute::context::ranked_replay_fixtures::matrix_exp_with_intent(
            "create new langitem title",
            Some(&["langitem_create".to_string()]),
            MutatorAdmit::AlwaysOnSeeds,
        );
        let snapshot = ExposureWaveSnapshot {
            slots_before: exp.surface.slots.clone(),
            caps_before: exp.surface.capabilities.clone(),
            entity_count_before: exp.entities.len(),
            relation_keys: vec![],
            ranked_capability_names: Some(vec!["langitem_create".into()]),
        };
        let changes = compute_exposure_wave_changes(&exp, &snapshot);
        assert!(changes.surface_unchanged());
        assert!(changes.new_capabilities.is_empty());
    }

    #[tokio::test]
    async fn commit_capability_only_delta_emits_compact_mutator_tsv() {
        let cgs = matrix_cgs_arc();
        let entities = vec!["LangItem".to_string()];
        let endpoints = matrix_langitem_endpoints();
        let weak_intent = "langitem browse inventory metadata";
        let mutator = "langitem_create";
        let initial_delta = derive_intent_exposure_surface_batch(
            cgs.as_ref(),
            "matrix",
            weak_intent,
            &endpoints,
            &entities,
            None,
            ExposureSurfaceOptions {
                mutator_admit: MutatorAdmit::AlwaysOnSeeds,
            },
        );
        let mut exp = TeachingExposureSession::new_with_intent_delta(
            cgs.as_ref(),
            "matrix",
            &["LangItem"],
            initial_delta,
        );
        // Read-first autosurfaces seeded mutators at weak intent; simulate deferred
        // ranked replay by stripping the mutator before the commit snapshot.
        exp.surface
            .capabilities
            .retain(|c| c.capability.as_str() != mutator);

        let snapshot = ExposureWaveSnapshot {
            slots_before: exp.surface.slots.clone(),
            caps_before: exp.surface.capabilities.clone(),
            entity_count_before: exp.entities.len(),
            relation_keys: exp.all_qualified_entities(),
            ranked_capability_names: Some(vec![mutator.into()]),
        };

        let replay_delta = derive_intent_exposure_surface_batch(
            cgs.as_ref(),
            "matrix",
            weak_intent,
            &endpoints,
            &entities,
            Some(&[mutator.to_string()]),
            ExposureSurfaceOptions {
                mutator_admit: MutatorAdmit::AlwaysOnSeeds,
            },
        );
        exp.expose_surface(
            &[cgs.as_ref()],
            cgs.clone(),
            "matrix",
            &["LangItem"],
            replay_delta,
        );

        let prompt_hash = PromptHashHex::from_prompt_sha256("ranked-replay-commit-test");
        let session_id = ExecuteSessionId::new_random();
        let ph = prompt_hash.as_str().to_string();
        let sid = session_id.as_str().to_string();

        let mut fixture = ExecuteSessionFixture::new()
            .prompt_hash(ph.clone())
            .entry_id("matrix")
            .entities(entities.clone());
        fixture.context_intent = Some(weak_intent.to_string());
        fixture.ranked_capabilities = Some(vec![mutator.into()]);
        let mut sess = fixture.build(cgs.clone());
        sess.teaching_exposure = Some(exp.clone());

        let host = matrix_host(cgs.clone());
        host.store_execute_session(
            crate::execute_session::SessionReuseKey {
                tenant_scope: String::new(),
                entry_id: "matrix".into(),
                catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
                entities: entities.clone(),
                context_intent: Some(weak_intent.to_string()),
                ranked_capabilities: Some(vec![mutator.into()]),
                principal: None,
                logical_session_id: None,
            },
            ph.clone(),
            sid.clone(),
            sess,
        )
        .await
        .expect("store session");

        let committed = commit_exposure_wave_delta(
            &host,
            &prompt_hash,
            &session_id,
            host.get_execute_session(ph.as_str(), sid.as_str())
                .await
                .expect("session")
                .as_ref()
                .clone(),
            exp,
            snapshot,
        )
        .await
        .expect("commit wave");

        assert!(
            !committed.surface_unchanged,
            "ranked replay must change capability surface"
        );
        let md = committed.markdown;
        assert!(
            md.contains(mutator) || md.contains(".m"),
            "compact delta must include invoke witness: {md}"
        );
        assert!(
            !md.contains("Active mutators"),
            "capability-only delta must not duplicate reuse recap block: {md}"
        );
    }
}
