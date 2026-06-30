//! Capability seeds, exposure planning, plasm_context MCP surface.

use super::super::*;

pub(super) fn normalize_execute_entity_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

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
        *entities = normalize_execute_entity_names(std::mem::take(entities));
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

pub(super) async fn apply_ranked_capabilities_session_update(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    ranked_arg: &RankedCapabilitiesArg,
) -> Result<(), String> {
    let RankedCapabilitiesArg::Set(opt) = ranked_arg else {
        return Ok(());
    };
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
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
        .await;
    Ok(())
}

pub(crate) const STALE_EXECUTE_BINDING_NOTICE: &str = "**Prior Plasm symbol table is void.** The execute session for this logical handle was missing, expired, or invalidated by a catalog reload. A new `(prompt_hash, session)` was opened — **discard** any cached `e#` / `m#` / `p#` or prior teaching-table text from earlier `plasm_context` output in this chat. Re-read the teaching table from this response only. Monotonic `e#` / `m#` / `p#` apply to the **new** session.\n\n";

/// Agent-facing Markdown for `plasm_context`: `logical_session_ref` plus wave bodies only (no telemetry).
pub(crate) fn build_plasm_context_agent_markdown(
    logical_session_ref: &str,
    waves: &[CapabilityWaveOutcome],
) -> String {
    let mut body = String::new();
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

/// Slim `_meta.plasm` for `plasm_context`: continuity + teaching revision only.
pub(crate) fn build_plasm_context_tool_meta(
    logical_session_ref: &str,
    out: &ApplyCapabilitySeedsOutcome,
    domain_revision: Option<u32>,
    relations: Option<serde_json::Value>,
    relations_delta: Option<serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut plasm = serde_json::Map::new();
    plasm.insert(
        "logical_session_ref".to_string(),
        serde_json::json!(logical_session_ref),
    );
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
    use plasm_core::{load_schema, TeachingExposureSession};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn ranked_capabilities_need_exposure_replay_when_mutator_missing_from_surface() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema(&root.join("../../apis/github")).expect("github");
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
    fn ranked_replay_surfaces_deferred_mutator_after_read_first_open() {
        use plasm_core::capability_method_label_kebab;
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
        use plasm_core::loader::load_schema_dir;
        use plasm_core::ExposureEntityKey;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            load_schema_dir(&root.join("../../fixtures/schemas/plasm_language_matrix"))
                .expect("plasm_language_matrix"),
        );
        let entities = ["LangItem"];
        let endpoints = entities
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "matrix".into(),
                entity: plasm_core::EntityName::from(*e),
            })
            .collect::<Vec<_>>();
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
        let mut exp = TeachingExposureSession::new_with_intent_delta(
            cgs.as_ref(),
            "matrix",
            &entities,
            delta,
        );
        assert!(
            !exp.surface
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == mutator),
            "read-first weak intent must defer seeded mutator"
        );
        assert!(
            ranked_capabilities_need_exposure_replay(
                &exp,
                &RankedCapabilitiesArg::Set(Some(vec![mutator.into()])),
            ),
            "ranked replay gate must fire for deferred mutator"
        );

        let ranked = vec![mutator.to_string()];
        let replay_delta = derive_intent_exposure_surface_batch(
            cgs.as_ref(),
            "matrix",
            weak_intent,
            &endpoints,
            &entities
                .iter()
                .map(|e| (*e).to_string())
                .collect::<Vec<_>>(),
            Some(&ranked),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        exp.expose_surface(
            &[cgs.as_ref()],
            cgs.clone(),
            "matrix",
            &entities,
            replay_delta,
        );
        assert!(
            exp.surface
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == mutator),
            "ranked replay must add deferred mutator to exposure surface"
        );
        let map = exp.symbol_map_arc();
        let cap = cgs.get_capability(mutator).expect(mutator);
        let method = capability_method_label_kebab(cap);
        let method_sym = map.method_sym("LangItem", &method);
        assert!(
            method_sym.starts_with('m'),
            "{mutator} method must appear on teaching surface after replay: {method_sym}"
        );
    }

    #[test]
    fn reuse_markdown_includes_active_mutator_recap() {
        use plasm_core::capability_method_label_kebab;
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
        use plasm_core::loader::load_schema_dir;
        use plasm_core::ExposureEntityKey;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema_dir(&root.join("../../fixtures/schemas/plasm_language_matrix"))
            .expect("plasm_language_matrix");
        let entities = ["LangItem"];
        let endpoints = entities
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "matrix".into(),
                entity: plasm_core::EntityName::from(*e),
            })
            .collect::<Vec<_>>();
        let intent = "langitem browse inventory metadata";
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            intent,
            &endpoints,
            &entities
                .iter()
                .map(|e| (*e).to_string())
                .collect::<Vec<_>>(),
            Some(&["langitem_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let exp = TeachingExposureSession::new_with_intent_delta(
            &cgs,
            "matrix",
            &entities,
            delta,
        );
        let md = format_session_unchanged_reuse_markdown(Some(&exp));
        assert!(
            md.contains("Active mutators"),
            "reuse markdown must recap mutators: {md}"
        );
        let cap = cgs.get_capability("langitem_create").expect("langitem_create");
        let method = capability_method_label_kebab(cap);
        let method_sym = exp.symbol_map_arc().method_sym("LangItem", &method);
        assert!(
            md.contains(&method_sym),
            "reuse recap must include {method_sym}: {md}"
        );
    }

    #[test]
    fn ranked_replay_diagnostics_when_already_exposed() {
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
        use plasm_core::loader::load_schema_dir;
        use plasm_core::ExposureEntityKey;
        use plasm_core::prompt_render::format_ranked_replay_diagnostics;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema_dir(&root.join("../../fixtures/schemas/plasm_language_matrix"))
            .expect("plasm_language_matrix");
        let entities = ["LangItem"];
        let endpoints = entities
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "matrix".into(),
                entity: plasm_core::EntityName::from(*e),
            })
            .collect::<Vec<_>>();
        let intent = "create new langitem title";
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            intent,
            &endpoints,
            &entities
                .iter()
                .map(|e| (*e).to_string())
                .collect::<Vec<_>>(),
            Some(&["langitem_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let exp = TeachingExposureSession::new_with_intent_delta(
            &cgs,
            "matrix",
            &entities,
            delta,
        );
        let caps_before = exp.surface.capabilities.clone();
        let diag = format_ranked_replay_diagnostics(
            &exp,
            &["langitem_create".to_string()],
            &caps_before,
        );
        assert!(
            diag.contains("already exposed"),
            "expected already-exposed diagnostic: {diag}"
        );
    }

    #[test]
    fn mcp_conformance_ranked_write_symbols_authorable_from_recap() {
        use plasm_core::capability_method_label_kebab;
        use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
        use plasm_core::loader::load_schema;
        use plasm_core::ExposureEntityKey;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema(&root.join("../../apis/github")).expect("github");
        let entities = vec!["Repository".to_string(), "Issue".to_string()];
        let endpoints = entities
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "github".into(),
                entity: plasm_core::EntityName::from(e.as_str()),
            })
            .collect::<Vec<_>>();
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
        let method = capability_method_label_kebab(cap);
        let map = exp.symbol_map_arc();
        let method_sym = map.method_sym("Issue", &method);
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
}
