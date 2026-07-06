//! Capability seeds, exposure planning, plasm_context MCP surface.

use super::super::*;
use plasm_core::discovery::relation_target_deferred_mutator_wires;
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

/// Mutating capabilities on **non-seeded** relation targets that match intent but are absent from
/// the current teaching surface (e.g. before ranked replay expands exposure).
pub(crate) fn relation_target_deferred_mutator_hint(
    cgs: &CGS,
    entry_id: &str,
    intent: &str,
    _relation_keys: &[ExposureEntityKey],
    seeded_entities: &[String],
    exp: &TeachingExposureSession,
    ranked: Option<&[String]>,
) -> Option<String> {
    let on_surface: HashSet<(String, String, String)> = exp
        .surface
        .capabilities
        .iter()
        .map(|k| {
            (
                k.entry_id.clone(),
                k.domain.to_string(),
                k.capability.to_string(),
            )
        })
        .collect();
    let deferred = relation_target_deferred_mutator_wires(
        cgs,
        entry_id,
        intent,
        seeded_entities,
        &on_surface,
        ranked,
    );
    if deferred.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n**Deferred write capabilities** (relation-target mutators not yet on the teaching surface): `{}`. Restate intent toward mutation or pass `ranked_capabilities` with the needed mutator wire name(s).\n",
        deferred.join("`, `")
    ))
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

pub(crate) const SYMBOL_SPACE_RESET_NOTICE: &str = "**SYMBOL SPACE RESET — discard cached `e#` / `m#` / `p#` / `r#`.** The pinned catalog digest changed; re-read the teaching table from this response only.\n\n";

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

/// Trim/dedupe seeds, then resolve each `entry_id` against the live registry (aliases, label, tags).
pub fn resolve_capability_seeds(
    seeds: Vec<CapabilitySeed>,
    registry: &plasm_core::discovery::InMemoryCgsRegistry,
    allowed_entry_ids: Option<&[String]>,
) -> Result<Vec<CapabilitySeed>, String> {
    let mut out = normalize_capability_seeds(seeds);
    for s in &mut out {
        s.entry_id = registry
            .resolve_entry_id(s.entry_id.as_str(), allowed_entry_ids)
            .map_err(|e| {
                if e.to_string().starts_with("unknown catalog entry:") {
                    e.to_string()
                } else {
                    format!("unknown catalog entry: {e}")
                }
            })?;
    }
    Ok(out)
}

pub(super) fn relation_endpoint_keys_for_wave(
    exp: &plasm_core::TeachingExposureSession,
    batch_entry_id: &str,
    batch_names: &[String],
) -> Vec<plasm_core::ExposureEntityKey> {
    exp.relation_endpoint_keys_for_wave(batch_entry_id, batch_names)
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

/// Advisory when `session_mode: new` overlaps seeds with a recent live logical session.
pub(crate) async fn format_session_churn_advisory(
    st: &PlasmHostState,
    tenant_scope: &str,
    except: Option<crate::session_identity::LogicalSessionId>,
    requested_seeds: &[CapabilitySeed],
) -> String {
    use crate::mcp_logical_ref::format_logical_session_wire_ref;

    let requested: BTreeSet<(String, String)> = requested_seeds
        .iter()
        .map(|s| (s.entry_id.clone(), s.entity.clone()))
        .collect();
    if requested.is_empty() {
        return String::new();
    }
    let recent = st
        .logical_sessions
        .recent_sessions_for_tenant(tenant_scope, except)
        .await;
    for rec in recent.into_iter().rev() {
        let Some(pair) = st
            .logical_execute_bindings
            .get(&rec.logical_session_id.as_uuid())
            .await
        else {
            continue;
        };
        let Some(sess) = st.get_execute_session(&pair.0, &pair.1).await else {
            continue;
        };
        let exposed: BTreeSet<(String, String)> = capability_seeds_from_session(sess.as_ref())
            .into_iter()
            .map(|s| (s.entry_id, s.entity))
            .collect();
        let overlap: Vec<String> = requested
            .intersection(&exposed)
            .map(|(eid, ent)| format!("{eid}:{ent}"))
            .collect();
        if overlap.is_empty() {
            continue;
        }
        let wire_ref = format_logical_session_wire_ref(rec.logical_session_id);
        return format!(
            "**Note:** session `{wire_ref}` already exposes {}. Use `session_mode: \"extend\"` with that `logical_session_ref` unless this is a separate goal.\n\n",
            overlap.join(", ")
        );
    }
    String::new()
}

pub(crate) fn format_session_unchanged_reuse_markdown(
    exp: Option<&plasm_core::TeachingExposureSession>,
) -> String {
    if let Some(exp) = exp.filter(|e| !e.entities.is_empty()) {
        let map = plasm_core::prompt_render::render_compact_exposure_symbol_map(exp);
        let active = plasm_core::prompt_render::render_active_mutator_surface_recap(exp);
        let mut out = format!("Unchanged — {map}.");
        if !active.is_empty() {
            out.push_str("\n\nActive mutators (reuse):\n```tsv\nplasm_expr\tMeaning\n");
            out.push_str(&active);
            out.push_str("\n```\n");
        }
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
) -> Option<plasm_core::TeachingExposureSession> {
    st.get_execute_session(prompt_hash, session_id)
        .await
        .and_then(|s| s.teaching_exposure.clone())
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

/// True when explicit `ranked_capabilities` names mutators not yet on the exposure surface.
pub(super) fn ranked_capabilities_need_exposure_replay(
    exp: &plasm_core::TeachingExposureSession,
    ranked_arg: &RankedCapabilitiesArg,
) -> bool {
    let RankedCapabilitiesArg::Set(Some(list)) = ranked_arg else {
        return false;
    };
    let Some(normalized) = normalize_ranked_capabilities_for_gate(Some(list.clone())) else {
        return false;
    };
    normalized.iter().any(|name| {
        !exp.surface
            .capabilities
            .iter()
            .any(|k| k.capability.as_str() == name.as_str())
    })
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

/// MCP `plasm_context` `ranked_capabilities` argument: omitted vs explicit replace/clear.
#[derive(Clone, Debug)]
pub enum RankedCapabilitiesArg {
    /// Key absent — keep the session's ranked list on expand waves.
    Unspecified,
    /// Key present (`null`, `[]`, or string array) — replace the session list when intent-scoped.
    Set(Option<Vec<String>>),
}

pub(crate) fn normalize_ranked_capabilities_for_gate(
    raw: Option<Vec<String>>,
) -> Option<Vec<String>> {
    let mut v: Vec<String> = raw?
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort();
    v.dedup();
    Some(v)
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
        .get_execute_session(prompt_hash_p.as_str(), session_id_p.as_str())
        .await
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

pub(super) async fn apply_ranked_capabilities_session_update(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    ranked_arg: &RankedCapabilitiesArg,
) -> Result<(), super::session::SessionMutateError> {
    let RankedCapabilitiesArg::Set(opt) = ranked_arg else {
        return Ok(());
    };
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| super::session::SessionMutateError::from(e))?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| super::session::SessionMutateError::from(e))?;
    let Some(sess_arc) = st
        .get_execute_session(prompt_hash_p.as_str(), session_id_p.as_str())
        .await
    else {
        return Err("unknown or expired execute session".into());
    };
    let mut sess = (*sess_arc).clone();
    if sess.context_intent.is_none() {
        return Ok(());
    }
    sess.ranked_capabilities = normalize_ranked_capabilities_for_gate(opt.clone());
    st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
        .await?;
    Ok(())
}

pub(crate) const STALE_EXECUTE_BINDING_NOTICE: &str = "**Prior Plasm symbol table is void.** The execute session for this logical handle was missing, expired, or invalidated by a catalog reload. A new `(prompt_hash, session)` was opened — **discard** any cached `e#` / `m#` / `p#` or prior teaching-table text from earlier `plasm_context` output in this chat. Re-read the teaching table from this response only. Monotonic `e#` / `m#` / `p#` apply to the **new** session.\n\n";

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
    plasm
}

/// Wrap teaching table / incremental delta in a Markdown fenced block so MCP and other Markdown UIs
/// preserve newlines (CommonMark collapses single newlines in ordinary paragraphs).
pub(super) fn wrap_teaching_markdown_literal_block(
    body: &str,
    render_mode: PromptRenderMode,
) -> String {
    let t = body.trim_end();
    let fence = render_mode.markdown_fence_info_string();
    format!("```{fence}\n{t}\n```\n")
}

#[cfg(test)]
mod ranked_replay_tests {
    use super::*;
    use crate::http_execute::ApplyCapabilitySeedsOutcome;
    use plasm_core::TeachingExposureSession;

    use crate::http_execute::context::ranked_replay_fixtures::{
        github_cgs_arc, github_exp_with_intent, github_issue_repo_endpoints, load_github_cgs,
        load_matrix_cgs, matrix_cgs_arc, matrix_exp_with_intent, matrix_langitem_endpoints,
    };

    #[test]
    fn ranked_capabilities_need_exposure_replay_when_mutator_missing_from_surface() {
        let cgs = load_github_cgs();
        let exp = TeachingExposureSession::new(&cgs, "github", &["Repository"]);
        assert!(
            ranked_capabilities_need_exposure_replay(
                &exp,
                &RankedCapabilitiesArg::Set(Some(vec!["zzzz_mutator_not_on_surface".into()])),
            ),
            "unknown ranked mutator must trigger replay"
        );
        let on_surface = exp
            .surface
            .capabilities
            .first()
            .expect("repository surface has capabilities")
            .capability
            .clone();
        assert!(
            !ranked_capabilities_need_exposure_replay(
                &exp,
                &RankedCapabilitiesArg::Set(Some(vec![on_surface.to_string()])),
            ),
            "ranked cap already on surface must not trigger replay"
        );
        assert!(
            !ranked_capabilities_need_exposure_replay(&exp, &RankedCapabilitiesArg::Unspecified),
            "unspecified ranked list must not force replay"
        );
    }

    #[test]
    fn read_first_open_admits_seeded_mutator_at_weak_intent() {
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};

        let cgs = matrix_cgs_arc();
        let entities = ["LangItem"];
        let endpoints = matrix_langitem_endpoints();
        let weak_intent = "langitem browse inventory metadata";
        let mutator = "langitem_create";
        let delta = derive_intent_exposure_surface_batch(
            cgs.as_ref(),
            "matrix",
            weak_intent,
            &endpoints,
            &entities
                .iter()
                .map(|e| (*e).to_string())
                .collect::<Vec<_>>(),
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let exp = TeachingExposureSession::new_with_intent_delta(
            cgs.as_ref(),
            "matrix",
            &entities,
            delta,
        );
        assert!(
            exp.surface
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == mutator),
            "read-first should autosurface seeded mutators at weak intent"
        );
        assert!(
            !ranked_capabilities_need_exposure_replay(
                &exp,
                &RankedCapabilitiesArg::Set(Some(vec![mutator.into()])),
            ),
            "seeded mutator already on surface must not trigger ranked replay"
        );
    }

    #[test]
    fn relation_target_deferred_mutator_hint_surfaces_missing_relation_mutator() {
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};

        let cgs = load_github_cgs();
        let seeded = vec!["Issue".to_string()];
        let endpoints = vec![
            ExposureEntityKey {
                entry_id: "github".into(),
                entity: plasm_core::EntityName::from("Issue"),
            },
            ExposureEntityKey {
                entry_id: "github".into(),
                entity: plasm_core::EntityName::from("IssueComment"),
            },
        ];
        let intent = "add comment body text issue thread";
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "github",
            intent,
            &endpoints,
            &seeded,
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let mut exp =
            TeachingExposureSession::new_with_intent_delta(&cgs, "github", &["Issue"], delta);
        exp.surface
            .capabilities
            .retain(|c| c.capability.as_str() != "issue_comment_create");
        let hint = relation_target_deferred_mutator_hint(
            &cgs, "github", intent, &endpoints, &seeded, &exp, None,
        )
        .expect("expected deferred relation-target mutator hint");
        assert!(
            hint.contains("issue_comment_create"),
            "hint must name withheld relation-target mutator: {hint}"
        );
    }

    #[test]
    fn relation_target_deferred_mutator_hint_empty_when_surface_complete() {
        let cgs = load_github_cgs();
        let seeded = vec!["Repository".to_string(), "Issue".to_string()];
        let endpoints = github_issue_repo_endpoints();
        let intent = "create new issue title body repository";
        let exp = github_exp_with_intent(intent, None, true);
        assert!(
            relation_target_deferred_mutator_hint(
                &cgs, "github", intent, &endpoints, &seeded, &exp, None,
            )
            .is_none(),
            "complete surface must not emit deferred mutator hint"
        );
    }

    #[test]
    fn ranked_replay_surfaces_deferred_mutator_after_read_first_open() {
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};

        let cgs = github_cgs_arc();
        let seeded = vec!["Issue".to_string()];
        let endpoints = vec![
            ExposureEntityKey {
                entry_id: "github".into(),
                entity: plasm_core::EntityName::from("Issue"),
            },
            ExposureEntityKey {
                entry_id: "github".into(),
                entity: plasm_core::EntityName::from("IssueComment"),
            },
        ];
        let intent = "add comment body text issue thread";
        let mutator = "issue_comment_create";
        let delta = derive_intent_exposure_surface_batch(
            cgs.as_ref(),
            "github",
            intent,
            &endpoints,
            &seeded,
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let mut exp = TeachingExposureSession::new_with_intent_delta(
            cgs.as_ref(),
            "github",
            &["Issue"],
            delta,
        );
        exp.surface
            .capabilities
            .retain(|c| c.capability.as_str() != mutator);
        assert!(
            ranked_capabilities_need_exposure_replay(
                &exp,
                &RankedCapabilitiesArg::Set(Some(vec![mutator.into()])),
            ),
            "ranked replay gate must fire for deferred relation-target mutator"
        );

        let ranked = vec![mutator.to_string()];
        let replay_delta = derive_intent_exposure_surface_batch(
            cgs.as_ref(),
            "github",
            intent,
            &endpoints,
            &seeded,
            Some(&ranked),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        exp.expose_surface(
            &[cgs.as_ref()],
            cgs.clone(),
            "github",
            &["Issue"],
            replay_delta,
        );
        assert!(
            exp.surface
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == mutator),
            "ranked replay must add deferred mutator to exposure surface"
        );
        let cap = cgs.get_capability(mutator).expect(mutator);
        let method_sym =
            exp.symbol_map_arc()
                .method_sym_for("github", "IssueComment", cap.name.as_str());
        assert!(
            method_sym.starts_with('m'),
            "{mutator} method must appear on teaching surface after replay: {method_sym}"
        );
    }

    #[test]
    fn reuse_markdown_includes_active_mutator_recap() {
        use plasm_core::capability_method_label_kebab;

        let cgs = load_matrix_cgs();
        let exp = matrix_exp_with_intent(
            "langitem browse inventory metadata",
            Some(&["langitem_create".to_string()]),
            true,
        );
        let md = format_session_unchanged_reuse_markdown(Some(&exp));
        assert!(
            md.contains("Active mutators"),
            "reuse markdown must recap mutators: {md}"
        );
        let cap = cgs
            .get_capability("langitem_create")
            .expect("langitem_create");
        let method_sym =
            exp.symbol_map_arc()
                .method_sym_for("matrix", "LangItem", cap.name.as_str());
        assert!(
            md.contains(&method_sym),
            "reuse recap must include {method_sym}: {md}"
        );
    }

    #[test]
    fn ranked_replay_diagnostics_when_already_exposed() {
        use plasm_core::prompt_render::format_ranked_replay_diagnostics;

        let exp = matrix_exp_with_intent(
            "create new langitem title",
            Some(&["langitem_create".to_string()]),
            true,
        );
        let caps_before = exp.surface.capabilities.clone();
        let diag =
            format_ranked_replay_diagnostics(&exp, &["langitem_create".to_string()], &caps_before);
        assert!(
            diag.contains("already exposed"),
            "expected already-exposed diagnostic: {diag}"
        );
        assert!(
            diag.contains("matrix:LangItem.langitem_create"),
            "diagnostics must use qualified capability keys: {diag}"
        );
    }

    #[test]
    fn mcp_conformance_ranked_write_symbols_authorable_from_recap() {
        use plasm_core::capability_method_label_kebab;
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
        use plasm_core::ExposureEntityKey;

        let cgs = load_github_cgs();
        let entities = vec!["Repository".to_string(), "Issue".to_string()];
        let endpoints = github_issue_repo_endpoints();
        let weak_intent = "browse repository metadata inventory";
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "github",
            weak_intent,
            &endpoints,
            &entities,
            Some(&["issue_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        assert!(
            delta
                .required
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "issue_create"),
            "ranked issue_create must appear on seeded Issue"
        );
        let exp = TeachingExposureSession::new_with_intent_delta(
            &cgs,
            "github",
            &["Repository", "Issue"],
            delta,
        );
        let reuse = format_session_unchanged_reuse_markdown(Some(&exp));
        let cap = cgs.get_capability("issue_create").expect("issue_create");
        let map = exp.symbol_map_arc();
        let method_sym = map.method_sym_for("github", "Issue", cap.name.as_str());
        let labels_sym = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "labels");
        assert!(
            reuse.contains(&method_sym),
            "reuse recap must expose issue_create method sym: {reuse}"
        );
        assert!(
            reuse.contains(&format!("labels={labels_sym}")),
            "reuse recap must name labels param: {reuse}"
        );
    }

    #[test]
    fn ranked_replay_admits_pr_create_at_zero_score_on_seeded_pull_request() {
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
        use plasm_core::ExposureEntityKey;

        let cgs = load_github_cgs();
        let entities = vec![
            "Repository".to_string(),
            "PullRequest".to_string(),
            "Issue".to_string(),
        ];
        let endpoints = ["Repository", "PullRequest", "Issue"]
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "github".into(),
                entity: plasm_core::EntityName::from(*e),
            })
            .collect::<Vec<_>>();
        let zero_intent = "xyzzy qwerty plugh unrelated metadata browse";
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "github",
            zero_intent,
            &endpoints,
            &entities,
            Some(&["pr_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        assert!(
            delta
                .required
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "pr_create"),
            "ranked pr_create must appear on seeded PullRequest at score zero: {:?}",
            delta.required.capabilities
        );
    }

    #[test]
    fn plasm_context_meta_surfaces_stale_symbol_space_recovery() {
        let out = ApplyCapabilitySeedsOutcome {
            prompt_hash: "ph_new".into(),
            session_id: "sid_new".into(),
            primary_entry_id: "github".into(),
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
