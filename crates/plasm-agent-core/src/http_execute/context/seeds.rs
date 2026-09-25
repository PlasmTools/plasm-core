//! Capability seeds, exposure planning, plasm_context MCP surface.

use super::super::*;
use super::session_churn::SessionChurnAdvisory;
use plasm_core::{ExposureEntityKey, TeachingExposureSession};
use std::collections::{BTreeSet, HashSet};

pub(crate) fn capability_seeds_from_session(sess: &ExecuteSession) -> Vec<CapabilitySeed> {
    if let Some(exp) = sess.teaching_exposure.as_ref() {
        return exp
            .surface
            .entities
            .iter()
            .map(|k| CapabilitySeed {
                entry_id: k.entry_id.clone(),
                entity: k.entity.to_string(),
            })
            .collect();
    }
    sess.entities
        .iter()
        .map(|e| CapabilitySeed {
            entry_id: sess.entry_id.clone(),
            entity: e.clone(),
        })
        .collect()
}

/// Dedupe while preserving first-seen order (symbol numbering / exposure waves).
pub(crate) fn dedup_preserve_arrival_order(mut names: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

/// Sorted deduped entity set for [`crate::execute_session::SessionReuseKey`] set-equality only.
pub(super) fn sorted_entity_set_for_reuse_key(names: &[String]) -> Vec<String> {
    let mut v = names.to_vec();
    v.sort();
    v.dedup();
    v
}

/// Legacy name: arrival-order dedup (do **not** sort — sorting shifts `e#` on expand/reopen).
pub(super) fn normalize_execute_entity_names(names: Vec<String>) -> Vec<String> {
    dedup_preserve_arrival_order(names)
}

pub(crate) const SYMBOL_SPACE_RESET_NOTICE: &str = "**SYMBOL SPACE RESET — discard cached `e#` / `m#` / `r#`.** The pinned catalog digest changed; re-read the teaching table from this response only.\n\n";

pub fn normalize_capability_seeds(mut seeds: Vec<CapabilitySeed>) -> Vec<CapabilitySeed> {
    for s in &mut seeds {
        s.entry_id = s.entry_id.trim().to_string();
        s.entity = s.entity.trim().to_string();
    }
    seeds.retain(|s| !s.entry_id.is_empty() && !s.entity.is_empty());
    let mut seen = std::collections::HashSet::<(String, String)>::new();
    let mut out = Vec::new();
    for s in seeds {
        let key = (s.entry_id.clone(), s.entity.clone());
        if seen.insert(key) {
            out.push(s);
        }
    }
    out
}

pub(crate) fn format_exposure_entity_cheat_sheet(
    exp: &plasm_core::TeachingExposureSession,
) -> String {
    if exp.entities.is_empty() {
        return String::new();
    }
    let map = plasm_core::prompt_render::render_compact_exposure_symbol_map(exp);
    format!("Active symbols — {map}.")
}

pub(crate) fn format_session_unchanged_reuse_markdown(
    exp: Option<&plasm_core::TeachingExposureSession>,
) -> String {
    if let Some(exp) = exp.filter(|e| !e.entities.is_empty()) {
        let map = plasm_core::prompt_render::render_compact_exposure_symbol_map(exp);
        let mut out = format!("Unchanged — {map}. Reuse the supplied Python declarations.\n");
        out.push_str("Next: `plasm` / `plasm_run`.\n");
        out
    } else {
        "Unchanged — no exposed entities yet. Next: `plasm` / `plasm_run`.\n".to_string()
    }
}

pub(crate) async fn teaching_exposure_at(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
) -> Result<Option<plasm_core::TeachingExposureSession>, super::session::SessionMutateError> {
    Ok(st
        .try_get_execute_session(prompt_hash, session_id)
        .await
        .map_err(|error| error.to_string())?
        .and_then(|s| s.teaching_exposure.clone()))
}

pub(crate) fn unchanged_expand_wave(
    entry_id: String,
    exposure: Option<&plasm_core::TeachingExposureSession>,
) -> CapabilityWaveOutcome {
    CapabilityWaveOutcome {
        mode: "expand".to_string(),
        entry_id,
        entities: vec![],
        markdown_delta: format_session_unchanged_reuse_markdown(exposure),
        reused_session: true,
        teaching_prompt_chars_added: 0,
        relations_delta: Vec::new(),
    }
}

pub(super) fn seeds_fully_exposed(
    exp: &plasm_core::TeachingExposureSession,
    seeds: &[CapabilitySeed],
) -> bool {
    seeds
        .iter()
        .all(|s| exp.contains_qualified_entity(s.entry_id.as_str(), s.entity.as_str()))
}

fn relation_endpoint_keys_for_seeds(
    exp: &plasm_core::TeachingExposureSession,
    seeds: &[CapabilitySeed],
) -> Vec<plasm_core::ExposureEntityKey> {
    let mut keys = exp.all_qualified_entities();
    let mut seen: std::collections::BTreeSet<(String, String)> = keys
        .iter()
        .map(|k| (k.entry_id.clone(), k.entity.to_string()))
        .collect();
    for seed in seeds {
        let pair = (seed.entry_id.clone(), seed.entity.clone());
        if seen.insert(pair.clone()) {
            keys.push(plasm_core::ExposureEntityKey {
                entry_id: pair.0,
                entity: plasm_core::EntityName::from(pair.1.as_str()),
            });
        }
    }
    keys
}

/// Seeds are present **and** cross-entity relation hops among them are admitted with `r#` symbols.
pub(super) fn seeds_exposure_ready_for_reuse(
    exp: &plasm_core::TeachingExposureSession,
    seeds: &[CapabilitySeed],
) -> bool {
    if !seeds_fully_exposed(exp, seeds) {
        return false;
    }
    let relation_keys = relation_endpoint_keys_for_seeds(exp, seeds);
    exp.pending_relation_slots_among(&relation_keys).is_empty()
}

pub(crate) fn group_seed_entities_by_entry(
    seeds: &[CapabilitySeed],
) -> IndexMap<String, Vec<String>> {
    let mut groups: IndexMap<String, Vec<String>> = IndexMap::new();
    for seed in seeds {
        groups
            .entry(seed.entry_id.clone())
            .or_default()
            .push(seed.entity.clone());
    }
    for entities in groups.values_mut() {
        *entities = dedup_preserve_arrival_order(std::mem::take(entities));
    }
    groups
}

/// Canonical multi-catalog plan: primary catalog (lexicographically first among distinct `entry_id`s)
/// and a **deterministic** processing order (primary first, then every other catalog in sorted order).
/// This removes dependence on the order seeds appear in the `plasm_context` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityExposurePlan {
    pub primary_entry_id: String,
    pub seeds_by_entry: IndexMap<String, Vec<String>>,
    /// Catalog `entry_id`s in order: primary, then non-primary keys sorted lexicographically.
    pub process_order: Vec<String>,
}

pub(crate) fn build_capability_exposure_plan(
    seeds: &[CapabilitySeed],
) -> Option<CapabilityExposurePlan> {
    let seeds_by_entry = group_seed_entities_by_entry(seeds);
    if seeds_by_entry.is_empty() {
        return None;
    }
    let primary_entry_id = primary_entry_id_for_grouped(&seeds_by_entry);
    let process_order = process_order_for_capability_plan(&primary_entry_id, &seeds_by_entry);
    Some(CapabilityExposurePlan {
        primary_entry_id,
        seeds_by_entry,
        process_order,
    })
}

/// Primary first, then all other `entry_id`s in lexicographic order (independent of seed order).
pub(super) fn process_order_for_capability_plan(
    primary_entry_id: &str,
    grouped: &IndexMap<String, Vec<String>>,
) -> Vec<String> {
    let mut rest: Vec<&str> = grouped
        .keys()
        .map(|k| k.as_str())
        .filter(|k| *k != primary_entry_id)
        .collect();
    rest.sort();
    let mut out = vec![primary_entry_id.to_string()];
    out.extend(rest.iter().map(|s| (*s).to_string()));
    out
}

/// For expand-only waves: every catalog in the request is already loaded; use sorted `entry_id` order.
pub(super) fn process_order_for_expand_group(
    grouped: &IndexMap<String, Vec<String>>,
) -> Vec<String> {
    let mut keys: Vec<String> = grouped.keys().cloned().collect();
    keys.sort();
    keys
}

/// Lexicographically first catalog `entry_id` in the group map.
/// [`SessionReuseKey::entry_id`] and the first-open path must not depend on seed input order
/// (hosts may reorder an equivalent seed set between calls).
pub(crate) fn primary_entry_id_for_grouped(grouped: &IndexMap<String, Vec<String>>) -> String {
    let mut keys: Vec<&str> = grouped.keys().map(|k| k.as_str()).collect();
    keys.sort();
    keys.into_iter()
        .next()
        .expect("grouped non-empty when seeds non-empty")
        .to_string()
}

/// One-line summary for LLM-facing session waves (MCP + stored `prompt_text`); not a Plasm expression.
/// Normalize optional MCP intent for teaching table filtering and [`SessionReuseKey::context_intent`].
#[inline]
pub(crate) fn normalize_context_intent_for_domain_filter(raw: Option<&str>) -> Option<String> {
    raw.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

pub(super) async fn apply_context_intent_session_update(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    accumulated_intent: &str,
) -> Result<bool, super::session::SessionMutateError> {
    let normalized = normalize_context_intent_for_domain_filter(Some(accumulated_intent));
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| super::session::SessionMutateError::from(e))?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| super::session::SessionMutateError::from(e))?;
    let Some(sess_arc) = st
        .try_get_execute_session(prompt_hash_p.as_str(), session_id_p.as_str())
        .await
        .map_err(|error| error.to_string())?
    else {
        return Err("unknown or expired execute session".into());
    };
    let mut sess = (*sess_arc).clone();
    let changed = sess.context_intent != normalized;
    if changed {
        sess.context_intent = normalized;
        st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
            .await?;
    }
    Ok(changed)
}

pub(crate) const STALE_EXECUTE_BINDING_NOTICE: &str = "**Prior Plasm symbol table is void.** The execute session for this logical handle was missing, expired, or invalidated by a catalog reload. A new `(prompt_hash, session)` was opened — **discard** any cached `e#` / `m#` / `r#` or prior teaching-table text from earlier `plasm_context` output in this chat. Re-read the teaching table from this response only. Monotonic `e#` / `m#` / `r#` apply to the **new** session.\n\n";

/// Agent-facing Markdown for `plasm_context`: `logical_session_ref` plus wave bodies only (no telemetry).
pub(crate) fn build_plasm_context_agent_markdown(
    logical_session_ref: &str,
    waves: &[CapabilityWaveOutcome],
    symbol_space_reset: bool,
    churn_advisory: &str,
) -> String {
    let mut body = String::new();
    if !churn_advisory.is_empty() {
        body.push_str(churn_advisory.trim_end());
    }
    if symbol_space_reset {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(SYMBOL_SPACE_RESET_NOTICE);
    }
    for wave in waves {
        let delta = wave.markdown_delta.trim();
        if delta.is_empty() {
            continue;
        }
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(delta);
    }
    if body.is_empty() {
        format!("`{logical_session_ref}`\n")
    } else {
        format!("`{logical_session_ref}`\n\n{body}\n")
    }
}

/// Inputs for [`build_plasm_context_tool_meta`] beyond the apply outcome.
pub(crate) struct PlasmContextToolMetaParams<'a> {
    pub logical_session_ref: &'a str,
    pub session_mode: &'a str,
    pub intent_turns: usize,
    pub accumulated_intent_preview: &'a str,
    pub domain_revision: Option<u32>,
    pub symbol_map_fingerprint: Option<String>,
    pub relations: Option<serde_json::Value>,
    pub relations_delta: Option<serde_json::Value>,
    pub session_churn: Option<&'a SessionChurnAdvisory>,
}

/// Slim `_meta.plasm` for `plasm_context`: continuity + teaching revision only.
pub(crate) fn build_plasm_context_tool_meta(
    out: &ApplyCapabilitySeedsOutcome,
    params: PlasmContextToolMetaParams<'_>,
) -> serde_json::Map<String, serde_json::Value> {
    let PlasmContextToolMetaParams {
        logical_session_ref,
        session_mode,
        intent_turns,
        accumulated_intent_preview,
        domain_revision,
        symbol_map_fingerprint,
        relations,
        relations_delta,
        session_churn,
    } = params;
    let mut plasm = serde_json::Map::new();
    plasm.insert(
        "logical_session_ref".to_string(),
        serde_json::json!(logical_session_ref),
    );
    plasm.insert("session_mode".to_string(), serde_json::json!(session_mode));
    plasm.insert("intent_turns".to_string(), serde_json::json!(intent_turns));
    if !accumulated_intent_preview.is_empty() {
        plasm.insert(
            "accumulated_intent".to_string(),
            serde_json::json!(accumulated_intent_preview),
        );
    }
    let mut continuity = serde_json::Map::new();
    continuity.insert(
        "stale_binding_recovered".to_string(),
        serde_json::json!(out.stale_execute_binding_recovered),
    );
    if out.stale_execute_binding_recovered {
        if let Some((ref ph, ref sid)) = out.stale_binding_previous {
            continuity.insert(
                "previous_execute".to_string(),
                serde_json::json!({ "prompt_hash": ph, "session_id": sid }),
            );
        }
    }
    continuity.insert(
        "new_symbol_space".to_string(),
        serde_json::json!(out.new_symbol_space),
    );
    if out.new_symbol_space {
        continuity.insert(
            "discard_cached_plasm_symbols".to_string(),
            serde_json::json!(true),
        );
    }
    plasm.insert(
        "continuity".to_string(),
        serde_json::Value::Object(continuity),
    );
    if let Some(rev) = domain_revision {
        plasm.insert("domain_revision".to_string(), serde_json::json!(rev));
    }
    if let Some(fp) = symbol_map_fingerprint {
        plasm.insert("symbol_map_fingerprint".to_string(), serde_json::json!(fp));
    }
    if let Some(rel) = relations {
        plasm.insert("relations".to_string(), rel);
    }
    if let Some(delta) = relations_delta {
        plasm.insert("relations_delta".to_string(), delta);
    }
    if let Some(churn) = session_churn {
        plasm.insert(
            "session_churn".to_string(),
            serde_json::json!({
                "peer_ref": churn.peer_ref,
                "overlap": churn.overlap,
            }),
        );
    }
    plasm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_execute::context::exposure_fixtures::matrix_exp_explicit;
    use crate::http_execute::ApplyCapabilitySeedsOutcome;

    #[test]
    fn reuse_markdown_preserves_symbols_without_reteaching_syntax() {
        let exp = matrix_exp_explicit();
        let md = format_session_unchanged_reuse_markdown(Some(&exp));
        assert!(md.contains("Reuse the supplied Python declarations"));
        assert!(md.contains("e1"));
        assert!(!md.contains("plasm_expr"));
        assert!(!md.contains("```"));
    }

    #[test]
    fn plasm_context_meta_surfaces_stale_symbol_space_recovery() {
        let out = ApplyCapabilitySeedsOutcome {
            prompt_hash: "ph_new".into(),
            session_id: "sid_new".into(),
            primary_entry_id: "langmatrix".into(),
            principal: None,
            waves: vec![],
            binding_updated: true,
            new_symbol_space: true,
            stale_execute_binding_recovered: true,
            stale_binding_previous: Some(("ph_old".into(), "sid_old".into())),
            symbol_space_reset: false,
        };
        let meta = build_plasm_context_tool_meta(
            &out,
            PlasmContextToolMetaParams {
                logical_session_ref: "lsref",
                session_mode: "extend",
                intent_turns: 2,
                accumulated_intent_preview: "turn-one\nturn-two",
                domain_revision: Some(1),
                symbol_map_fingerprint: Some("abc".into()),
                relations: None,
                relations_delta: None,
                session_churn: None,
            },
        );
        let continuity = meta
            .get("continuity")
            .expect("continuity")
            .as_object()
            .unwrap();
        assert_eq!(
            continuity.get("stale_binding_recovered"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            continuity.get("new_symbol_space"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            continuity.get("discard_cached_plasm_symbols"),
            Some(&serde_json::json!(true))
        );
        let prev = continuity
            .get("previous_execute")
            .expect("previous_execute")
            .as_object()
            .unwrap();
        assert_eq!(prev.get("prompt_hash"), Some(&serde_json::json!("ph_old")));
        assert_eq!(prev.get("session_id"), Some(&serde_json::json!("sid_old")));
    }
}
