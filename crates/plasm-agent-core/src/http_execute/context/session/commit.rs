//! Shared exposure-wave commit tail for federate and expand.
//!
//! Both flows, after applying their entities to the [`plasm_core::TeachingExposureSession`], run an
//! identical sequence: derive the new relation slots, admit them for render, compute the relation
//! delta, render the teaching delta (federated when the session spans multiple catalogs, else
//! single-catalog), append it to the session prompt, and persist. The callers differ only in how
//! they derive `relation_keys` and in the outcome shape they map [`CommittedWaveDelta`] onto.

use super::super::super::*;

use super::super::seeds::wrap_teaching_markdown_literal_block;

/// Outcome of committing one exposure-wave delta to an execute session.
pub(crate) struct CommittedWaveDelta {
    /// Rendered teaching delta wrapped for the prompt, or empty when nothing new was exposed.
    pub markdown: String,
    pub relations_delta: Vec<plasm_core::ExposedRelationSymbolRow>,
    /// `true` when the wave exposed no new entities/relations (session re-stored unchanged).
    pub reused: bool,
}

/// Admit new relation slots, render + append the teaching delta, and persist the session.
///
/// `exp` must already have the wave's entities exposed; `slots_before` is the slot set captured
/// before that exposure; `n0` is `exp.entities.len()` before the wave; `relation_keys` is the
/// caller-derived endpoint set for relation-slot discovery.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn commit_exposure_wave_delta(
    st: &PlasmHostState,
    prompt_hash_p: &PromptHashHex,
    session_id_p: &ExecuteSessionId,
    mut sess: crate::execute_session::ExecuteSession,
    mut exp: plasm_core::TeachingExposureSession,
    slots_before: &std::collections::BTreeSet<plasm_core::symbol_tuning::ExposureSlotKey>,
    n0: usize,
    relation_keys: &[plasm_core::ExposureEntityKey],
) -> CommittedWaveDelta {
    let added_qualified = exp.qualified_entities_since(n0);
    let new_relation_slots =
        exp.relation_slots_for_expand_wave(slots_before, &added_qualified, relation_keys);
    {
        let layers: Vec<&CGS> = sess
            .contexts_by_entry
            .values()
            .map(|c| c.cgs.as_ref())
            .collect();
        exp.admit_relation_edge_slots_for_render(&layers, &new_relation_slots);
    }
    let relations_delta = exp.relations_delta_rows_for_slots(&new_relation_slots);

    if added_qualified.is_empty() && new_relation_slots.is_empty() {
        sess.entities = exp.entities.clone();
        sess.teaching_exposure = Some(exp);
        st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
            .await;
        return CommittedWaveDelta {
            markdown: String::new(),
            relations_delta: Vec::new(),
            reused: true,
        };
    }

    let sym_cross = st.sessions.symbol_map_cross_cache();
    let delta = if sess.contexts_by_entry.len() > 1 {
        let by_entry: IndexMap<String, &CGS> = sess
            .contexts_by_entry
            .iter()
            .map(|(k, v)| (k.clone(), v.cgs.as_ref()))
            .collect();
        st.engine
            .prompt_pipeline()
            .render_teaching_exposure_delta_federated_with_edges(
                &by_entry,
                &exp,
                &added_qualified,
                &new_relation_slots,
                Some(sym_cross),
            )
    } else {
        let added: Vec<&str> = added_qualified.iter().map(|k| k.entity.as_str()).collect();
        st.engine
            .prompt_pipeline()
            .render_teaching_exposure_delta_with_edges(
                sess.cgs.as_ref(),
                &exp,
                &added,
                &new_relation_slots,
                Some(sym_cross),
            )
    };
    let wave =
        wrap_teaching_markdown_literal_block(&delta, st.engine.prompt_pipeline().render_mode);
    sess.prompt_text.push_str("\n\n");
    sess.prompt_text.push_str(&wave);
    sess.entities = exp.entities.clone();
    sess.teaching_exposure = Some(exp);
    sess.domain_revision = sess.domain_revision.saturating_add(1);
    st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
        .await;

    CommittedWaveDelta {
        markdown: wave,
        relations_delta,
        reused: false,
    }
}
