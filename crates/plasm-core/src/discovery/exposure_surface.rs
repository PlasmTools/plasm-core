//! Intent-filtered exposure surface for MCP `plasm_context` / incremental expand waves.

use crate::identity::{CapabilityParamName, EntityFieldName, EntityName};
use crate::schema::{CapabilityKind, CapabilitySchema, EntityDef, InputType, CGS};
use crate::symbol_tuning::{
    ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey, ExposureSurface,
    ExposureSurfaceDelta,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

use super::{resolve_canonical_entity_name, score_relation_against_intent};

fn fields_for_admitted_read_cap(
    cgs: &CGS,
    cap: &CapabilitySchema,
    entity_name: &str,
) -> Vec<EntityFieldName> {
    if !cap.provides.is_empty() {
        let Some(ent) = cgs.get_entity(entity_name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for pname in &cap.provides {
            if let Some((fk, _)) = ent
                .fields
                .iter()
                .find(|(k, _)| k.as_str() == pname.as_str())
            {
                out.push(fk.clone());
            }
        }
        return out;
    }
    let Some(ent) = cgs.get_entity(entity_name) else {
        return Vec::new();
    };
    match cap.kind {
        CapabilityKind::Get => {
            let mut out = vec![ent.id_field.clone()];
            for kv in &ent.key_vars {
                if kv.as_str() != ent.id_field.as_str() {
                    out.push(kv.clone());
                }
            }
            out
        }
        CapabilityKind::Query | CapabilityKind::Search => {
            vec![ent.id_field.clone()]
        }
        _ => Vec::new(),
    }
}

#[cfg(feature = "ranked_capability_gate")]
fn ranked_gate_allows_mutation(ranked_capability_names: Option<&[String]>, cap_name: &str) -> bool {
    match ranked_capability_names {
        None | Some([]) => true,
        Some(names) => names.iter().any(|n| n.as_str() == cap_name),
    }
}

/// How seeded-entity mutators (`create`/`update`/`delete`/`action`) are admitted on an exposure wave.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutatorAdmit {
    /// Production default: seeded reads always taught; mutators need intent/ranked admission.
    #[default]
    IntentOnly,
    /// Test/benchmark overshow: seeded mutators always admitted.
    AlwaysOnSeeds,
}

/// Options for [`derive_intent_exposure_surface_batch`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExposureSurfaceOptions {
    /// Production ([`MutatorAdmit::IntentOnly`]): seeded entities always teach reads
    /// (`query`/`search`/`get` + `primary_read`); mutators require intent lexicon overlap or an
    /// explicit `ranked_capability_names` listing. [`MutatorAdmit::AlwaysOnSeeds`] always admits
    /// seeded mutators (tests / opt-in overshow). Non-seeded relation-target mutators always
    /// require intent overlap (and ranked-gate when enabled).
    pub mutator_admit: MutatorAdmit,
}

pub(crate) fn mutating_capability_admitted(
    score: u32,
    ranked_capability_names: Option<&[String]>,
    cap_name: &str,
) -> bool {
    if score == 0 {
        return false;
    }
    #[cfg(feature = "ranked_capability_gate")]
    {
        ranked_gate_allows_mutation(ranked_capability_names, cap_name)
    }
    #[cfg(not(feature = "ranked_capability_gate"))]
    {
        let _ = ranked_capability_names;
        true
    }
}

/// Capabilities on an explicitly seeded entity that are always admitted (no intent lexicon score).
fn seeded_entity_cap_always_includes(
    mutator_admit: MutatorAdmit,
    cap: &CapabilitySchema,
    entity_name: &str,
    ent: &EntityDef,
    seeded_entities: &HashSet<String>,
) -> bool {
    if cap.domain.as_str() != entity_name || !seeded_entities.contains(entity_name) {
        return false;
    }
    if matches!(
        cap.kind,
        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
    ) {
        return true;
    }
    if ent
        .primary_read
        .as_deref()
        .is_some_and(|pr| pr == cap.name.as_str())
    {
        return true;
    }
    matches!(mutator_admit, MutatorAdmit::AlwaysOnSeeds)
        && matches!(
            cap.kind,
            CapabilityKind::Create
                | CapabilityKind::Update
                | CapabilityKind::Delete
                | CapabilityKind::Action
        )
}

/// Max outgoing relation hints per entity in discover TSV (`wire→Target`).
pub const DISCOVERY_OUTGOING_RELATIONS_MAX: usize = 3;

/// Compact relation navigation chart for discover TSV (no session-local `r#`).
pub fn outgoing_relation_hints_for_entity(cgs: &CGS, entity: &str, max: usize) -> String {
    let Some(ent) = cgs.get_entity(entity) else {
        return String::new();
    };
    let mut rels: Vec<_> = ent.relations.iter().collect();
    rels.sort_by_key(|(wire, _)| wire.as_str());
    let mut parts = Vec::new();
    for (wire, rel) in rels {
        if parts.len() >= max {
            break;
        }
        if cgs.get_entity(rel.target_resource.as_str()).is_none() {
            continue;
        }
        parts.push(format!("{}→{}", wire, rel.target_resource));
    }
    parts.join("; ")
}

/// Minimal intent-filtered teaching surface for MCP `plasm_context` / incremental expand waves.
///
/// - **Seeded entities** (`entity_batch`): always admit `query` / `search` / `get` on that
///   entity’s domain, plus [`EntityDef::primary_read`] when set. With
///   [`MutatorAdmit::AlwaysOnSeeds`], seeded `create` / `update` / `delete` / `action` are also
///   always admitted (test/benchmark overshow). Production uses [`MutatorAdmit::IntentOnly`].
/// - **Non-seeded** read capabilities require a non-zero lexicon overlap score against `intent`.
/// - **Non-seeded** mutating capabilities require a non-zero score; with `ranked_capability_gate`,
///   when `ranked_capability_names` is non-empty they must also appear in that list.
/// - Relations on seeded entities are admitted only when the target appears in
///   `relation_endpoint_names` and relation intent scores > 0.
/// - Mutation closure (1-hop relation targets): create/update/delete/action on targets when intent
///   scores the capability (unchanged).
///
/// `entry_id` names the registry row for callers; exposure keys follow [`CGS::entry_id`] when set (see
/// [`crate::symbol_tuning::legacy_exposure_surface_for_entities`]).
pub fn derive_intent_exposure_surface_batch(
    cgs: &CGS,
    entry_id: &str,
    intent: &str,
    relation_endpoint_keys: &[ExposureEntityKey],
    entity_batch: &[String],
    ranked_capability_names: Option<&[String]>,
    options: ExposureSurfaceOptions,
) -> ExposureSurfaceDelta {
    let mut surface = ExposureSurface::default();
    let cid = if entry_id.is_empty() {
        cgs.entry_id.clone().unwrap_or_default()
    } else {
        entry_id.to_string()
    };
    let relation_set: BTreeSet<ExposureEntityKey> =
        relation_endpoint_keys.iter().cloned().collect();

    let mut query_tokens = HashSet::new();
    for tok in crate::catalog_search_index::CatalogSearchIndex::tokenize(intent) {
        query_tokens.insert(tok);
    }
    let bm25 =
        crate::catalog_search_index::CatalogSearchIndex::build_from_pairs([(cid.as_str(), cgs)]);

    let mut seeded_entities = HashSet::new();
    for raw_ent in entity_batch {
        if let Some(canonical) = resolve_canonical_entity_name(cgs, raw_ent) {
            seeded_entities.insert(canonical);
        }
    }

    for raw_ent in entity_batch {
        let Some(canonical) = resolve_canonical_entity_name(cgs, raw_ent) else {
            continue;
        };
        let ename = canonical.as_str();
        let ekey = ExposureEntityKey {
            entry_id: cid.clone(),
            entity: EntityName::from(ename),
        };
        surface.entities.insert(ekey.clone());

        let Some(ent) = cgs.get_entity(ename) else {
            continue;
        };

        surface.slots.insert(ExposureSlotKey::EntityField {
            entity: ekey.clone(),
            field: ent.id_field.clone(),
        });

        let Some(cap_names) = cgs.capability_names_by_domain().get(ename) else {
            continue;
        };
        for cap_name in cap_names {
            let Some(cap) = cgs.capabilities.get(cap_name) else {
                continue;
            };
            let score = bm25.capability_score(cid.as_str(), cap.name.as_str(), intent);
            let include = if seeded_entity_cap_always_includes(
                options.mutator_admit,
                cap,
                ename,
                ent,
                &seeded_entities,
            ) {
                true
            } else {
                match cap.kind {
                    CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => {
                        score > 0
                    }
                    _ => mutating_capability_admitted(
                        score,
                        ranked_capability_names,
                        cap.name.as_str(),
                    ),
                }
            };
            if !include {
                continue;
            }
            let ckey = ExposureCapabilityKey {
                entry_id: cid.clone(),
                domain: EntityName::from(ename),
                capability: cap.name.clone(),
            };
            surface.capabilities.insert(ckey.clone());

            if let Some(is) = &cap.input_schema {
                if let InputType::Object { fields, .. } = &is.input_type {
                    for f in fields {
                        surface.slots.insert(ExposureSlotKey::CapabilityParam {
                            capability: ckey.clone(),
                            param: CapabilityParamName::new(f.name.clone()),
                        });
                    }
                }
            }

            if matches!(
                cap.kind,
                CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
            ) {
                for fk in fields_for_admitted_read_cap(cgs, cap, ename) {
                    surface.slots.insert(ExposureSlotKey::EntityField {
                        entity: ekey.clone(),
                        field: fk,
                    });
                }
            }
        }

        for (rname, rel) in &ent.relations {
            let target_key = ExposureEntityKey {
                entry_id: cid.clone(),
                entity: rel.target_resource.clone(),
            };
            if relation_set.contains(&target_key)
                && score_relation_against_intent(&query_tokens, rel) > 0
            {
                surface.slots.insert(ExposureSlotKey::Relation {
                    source: ekey.clone(),
                    relation: rname.clone(),
                });
            }
        }
    }

    // Mutation closure: 1-hop relation targets may expose mutators when intent scores them.
    // Never insert a bare entity slot with zero teaching rows — when a mutator is admitted,
    // also admit that target's reads (`query`/`search`/`get` + `primary_read`).
    for raw_ent in entity_batch {
        let Some(ename) = resolve_canonical_entity_name(cgs, raw_ent) else {
            continue;
        };
        let Some(ent) = cgs.get_entity(ename.as_str()) else {
            continue;
        };
        for rel in ent.relations.values() {
            let target = rel.target_resource.as_str();
            let Some(target_ent) = cgs.get_entity(target) else {
                continue;
            };
            let tkey = ExposureEntityKey {
                entry_id: cid.clone(),
                entity: EntityName::from(target),
            };
            let Some(cap_names) = cgs.capability_names_by_domain().get(target) else {
                continue;
            };
            let mut admitted_mutators = Vec::new();
            for cap_name in cap_names {
                let Some(cap) = cgs.capabilities.get(cap_name) else {
                    continue;
                };
                if !matches!(
                    cap.kind,
                    CapabilityKind::Create
                        | CapabilityKind::Update
                        | CapabilityKind::Delete
                        | CapabilityKind::Action
                ) {
                    continue;
                }
                let score = bm25.capability_score(cid.as_str(), cap.name.as_str(), intent);
                if seeded_entities.contains(target)
                    && matches!(options.mutator_admit, MutatorAdmit::AlwaysOnSeeds)
                {
                    continue;
                }
                if !mutating_capability_admitted(score, ranked_capability_names, cap.name.as_str())
                {
                    continue;
                }
                admitted_mutators.push(ExposureCapabilityKey {
                    entry_id: cid.clone(),
                    domain: EntityName::from(target),
                    capability: cap.name.clone(),
                });
            }
            if admitted_mutators.is_empty() {
                continue;
            }
            surface.entities.insert(tkey.clone());
            for ckey in admitted_mutators {
                surface.capabilities.insert(ckey);
            }
            // Guarantee teachable reads on any newly exposed relation-target entity.
            for cap_name in cap_names {
                let Some(cap) = cgs.capabilities.get(cap_name) else {
                    continue;
                };
                let is_read = matches!(
                    cap.kind,
                    CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
                ) || target_ent
                    .primary_read
                    .as_deref()
                    .is_some_and(|pr| pr == cap.name.as_str());
                if !is_read {
                    continue;
                }
                let ckey = ExposureCapabilityKey {
                    entry_id: cid.clone(),
                    domain: EntityName::from(target),
                    capability: cap.name.clone(),
                };
                surface.capabilities.insert(ckey);
                if matches!(
                    cap.kind,
                    CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
                ) {
                    for fk in fields_for_admitted_read_cap(cgs, cap, target) {
                        surface.slots.insert(ExposureSlotKey::EntityField {
                            entity: tkey.clone(),
                            field: fk,
                        });
                    }
                }
            }
        }
    }

    ExposureSurfaceDelta { required: surface }
}

/// Mutating capability wire names on **non-seeded** relation targets that intent qualifies but
/// are absent from `on_surface` (entry_id, domain entity, capability wire triples).
pub fn relation_target_deferred_mutator_wires(
    cgs: &CGS,
    entry_id: &str,
    intent: &str,
    seeded_entities: &[String],
    on_surface: &HashSet<(String, String, String)>,
    ranked_capability_names: Option<&[String]>,
) -> Vec<String> {
    use std::collections::BTreeSet;

    let cid = if entry_id.is_empty() {
        cgs.entry_id.clone().unwrap_or_default()
    } else {
        entry_id.to_string()
    };
    let bm25 =
        crate::catalog_search_index::CatalogSearchIndex::build_from_pairs([(cid.as_str(), cgs)]);
    let seeded: HashSet<String> = seeded_entities.iter().cloned().collect();
    let mut deferred = BTreeSet::new();
    for raw_ent in seeded_entities {
        let Some(ent) = cgs.get_entity(raw_ent.as_str()) else {
            continue;
        };
        for rel in ent.relations.values() {
            let target = rel.target_resource.as_str();
            if seeded.contains(target) {
                continue;
            }
            let Some(cap_names) = cgs.capability_names_by_domain().get(target) else {
                continue;
            };
            for cap_name in cap_names {
                let Some(cap) = cgs.capabilities.get(cap_name) else {
                    continue;
                };
                if !matches!(
                    cap.kind,
                    CapabilityKind::Create
                        | CapabilityKind::Update
                        | CapabilityKind::Delete
                        | CapabilityKind::Action
                ) {
                    continue;
                }
                let score = bm25.capability_score(cid.as_str(), cap.name.as_str(), intent);
                if !mutating_capability_admitted(score, ranked_capability_names, cap.name.as_str())
                {
                    continue;
                }
                let trip = (cid.clone(), target.to_string(), cap.name.to_string());
                if on_surface.contains(&trip) {
                    continue;
                }
                deferred.insert(cap.name.to_string());
            }
        }
    }
    deferred.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::score_capability_bm25;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::{ExposureEntityKey, ExposureSlotKey, ExposureSurfaceDelta};
    use crate::EntityName;
    use std::path::Path;

    fn relation_keys(entry_id: &str, names: &[&str]) -> Vec<ExposureEntityKey> {
        names
            .iter()
            .map(|n| ExposureEntityKey {
                entry_id: entry_id.to_string(),
                entity: EntityName::from(*n),
            })
            .collect()
    }

    #[test]
    fn intent_surface_omits_relation_until_relation_target_in_scope() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["Profile"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "display profiles",
            &endpoints,
            &["Profile".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !delta.required.slots.iter().any(|s| matches!(
                s,
                ExposureSlotKey::Relation { relation, .. }
                    if relation.as_str() == "recorded_matches"
            )),
            "Profile.recorded_matches targets RecordedContent; omit until that entity is in scope"
        );
    }

    #[test]
    fn intent_surface_includes_profile_relation_when_recorded_content_in_scope() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["Profile", "RecordedContent"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "profile and captured content",
            &endpoints,
            &["Profile".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta.required.slots.iter().any(|s| matches!(
                s,
                ExposureSlotKey::Relation { relation, .. }
                    if relation.as_str() == "recorded_matches"
            )),
            "expected recorded_matches when RecordedContent is an allowed relation endpoint"
        );
    }

    #[test]
    fn mutating_capability_admitted_requires_nonzero_score() {
        assert!(!mutating_capability_admitted(0, None, "langitem_create"));
        assert!(mutating_capability_admitted(1, None, "langitem_create"));
    }

    #[test]
    fn intent_surface_ranked_admits_seeded_mutator_at_zero_score() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
        let cap = cgs
            .capabilities
            .get("langitem_create")
            .expect("langitem_create");
        let zero_intent = "xyzzy qwerty plugh unrelated";
        let zero_score = score_capability_bm25(&cgs, "matrix", cap, zero_intent);
        assert_eq!(
            zero_score, 0,
            "fixture intent must score zero for langitem_create"
        );
        let endpoints = relation_keys("matrix", &["LangItem"]);
        let delta_ranked = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            zero_intent,
            &endpoints,
            &["LangItem".to_string()],
            Some(&["langitem_create".to_string()]),
            ExposureSurfaceOptions {
                mutator_admit: MutatorAdmit::AlwaysOnSeeds,
            },
        );
        assert!(
            delta_ranked
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "ranked wire name must admit seeded mutator at score zero"
        );
    }

    #[test]
    fn intent_surface_always_on_seeds_admits_seeded_mutators_on_first_wave() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
        let cap = cgs
            .capabilities
            .get("langitem_create")
            .expect("langitem_create");
        let weak_intent = "langitem browse inventory metadata";
        let strong_intent = "create new langitem title";
        let weak_score = score_capability_bm25(&cgs, "matrix", cap, weak_intent);
        let strong_score = score_capability_bm25(&cgs, "matrix", cap, strong_intent);
        assert!(
            strong_score > 0,
            "strong create intent should BM25-score: {strong_score}"
        );
        assert!(
            strong_score >= weak_score,
            "strong intent should score at least as high as weak: {strong_score} vs {weak_score}"
        );
        let endpoints = relation_keys("matrix", &["LangItem"]);
        let delta_weak = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            weak_intent,
            &endpoints,
            &["LangItem".to_string()],
            None,
            ExposureSurfaceOptions {
                mutator_admit: MutatorAdmit::AlwaysOnSeeds,
            },
        );
        assert!(
            delta_weak
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "read-first should admit seeded mutators even at weak intent score"
        );
        let delta_strong = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            strong_intent,
            &endpoints,
            &["LangItem".to_string()],
            None,
            ExposureSurfaceOptions {
                mutator_admit: MutatorAdmit::AlwaysOnSeeds,
            },
        );
        assert!(
            delta_strong
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "read-first should admit strong-scored seeded mutator"
        );
        let delta_ranked = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            weak_intent,
            &endpoints,
            &["LangItem".to_string()],
            Some(&["langitem_create".to_string()]),
            ExposureSurfaceOptions {
                mutator_admit: MutatorAdmit::AlwaysOnSeeds,
            },
        );
        assert!(
            delta_ranked
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "read-first should admit ranked mutator wire name"
        );
    }

    #[test]
    fn intent_surface_seeded_prompt_run_create_requires_intent_overlap() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["PromptRun"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "list profiles read metadata only",
            &endpoints,
            &["PromptRun".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !delta
                .required
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "prompt_run_create"),
            "seeded PromptRun create must require intent overlap"
        );
        let delta_create = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["PromptRun".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta_create
                .required
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "prompt_run_create"),
            "seeded PromptRun create should appear when intent scores it"
        );
    }

    #[test]
    fn intent_surface_drops_unscored_reads_when_intent_targets_other_entity() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["Meeting", "Profile"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "organisation project profile metadata list",
            &endpoints,
            &["Profile".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta.required.capabilities.iter().any(|c| {
                c.domain.as_str() == "Profile"
                    && matches!(c.capability.as_str(), "profile_query" | "profile_get")
            }),
            "expected Profile query/get to remain when intent lexicon scores profile vocabulary"
        );
        assert!(
            !delta.required.capabilities.iter().any(|c| {
                c.domain.as_str() == "Meeting"
                    && matches!(c.capability.as_str(), "meeting_query" | "meeting_get")
            }),
            "Meeting reads should be omitted when intent does not score meeting vocabulary"
        );
    }

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_excludes_non_seeded_scored_mutation() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["PromptRun", "Profile"]);
        let ranked = vec!["prompt_run_create".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["Profile".to_string()],
            Some(&ranked),
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !surface_has_capability(&delta, "PromptRun", "prompt_run_create"),
            "PromptRun create must stay off surface when PromptRun is not seeded (ranked list alone does not add caps)"
        );
    }

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_keeps_mutation_on_list() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["PromptRun"]);
        let ranked = vec!["prompt_run_create".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["PromptRun".to_string()],
            Some(&ranked),
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta.required.capabilities.iter().any(|c| {
                c.capability.as_str() == "prompt_run_create"
            }),
            "ranked gate should admit mutations present in the ranked name list when intent scores them"
        );
    }

    const FEDERATED_FIELD_LAB_INTENT: &str =
        "Federated field lab v2 — pokeapi specimen linear missions proof dossier";

    fn surface_has_capability(
        delta: &ExposureSurfaceDelta,
        domain: &str,
        capability: &str,
    ) -> bool {
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.domain.as_str() == domain && c.capability.as_str() == capability)
    }

    #[test]
    fn intent_surface_seeded_sharelink_create_requires_intent_overlap() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir(&dir).expect("proof");
        cgs.entry_id = Some("proof".into());
        let endpoints = relation_keys("proof", &["ShareLink"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            FEDERATED_FIELD_LAB_INTENT,
            &endpoints,
            &["ShareLink".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !surface_has_capability(&delta, "ShareLink", "share_link_create"),
            "seeded create must require intent overlap when intent omits share/link/create tokens"
        );
        let delta_create = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            "create share link for proof dossier",
            &endpoints,
            &["ShareLink".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            surface_has_capability(&delta_create, "ShareLink", "share_link_create"),
            "seeded create should appear when intent scores the mutation"
        );
    }

    #[test]
    fn intent_surface_seeded_sharelink_create_with_intent_lexicon_match() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir(&dir).expect("proof");
        cgs.entry_id = Some("proof".into());
        let endpoints = relation_keys("proof", &["ShareLink"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            "create share link for proof dossier",
            &endpoints,
            &["ShareLink".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            surface_has_capability(&delta, "ShareLink", "share_link_create"),
            "seeded ShareLink must expose share_link_create when intent scores create"
        );
        let session = crate::symbol_tuning::TeachingExposureSession::new_with_intent_delta(
            &cgs,
            "proof",
            &["ShareLink"],
            delta,
        );
        let map = session.to_symbol_map();
        let m = map.method_sym_for("proof", "ShareLink", "share_link_create");
        assert!(
            m.starts_with('m') && m.len() > 1,
            "seeded share_link_create must receive an m# (got {m:?}) for federated lab plasm programs"
        );
    }

    #[test]
    fn intent_surface_seeded_pokemon_reads_without_intent_lexicon_match() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("pokeapi");
        let endpoints = relation_keys("pokeapi", &["Pokemon"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "pokeapi",
            FEDERATED_FIELD_LAB_INTENT,
            &endpoints,
            &["Pokemon".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            surface_has_capability(&delta, "Pokemon", "pokemon_query"),
            "seeded Pokemon must expose pokemon_query"
        );
        assert!(
            surface_has_capability(&delta, "Pokemon", "pokemon_get"),
            "seeded Pokemon must expose pokemon_get"
        );
    }

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_excludes_seeded_create_when_not_ranked() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("proof");
        let endpoints = relation_keys("proof", &["ShareLink"]);
        let ranked = vec!["__not_share_link_create__".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            "create share link for proof dossier",
            &endpoints,
            &["ShareLink".to_string()],
            Some(&ranked),
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !surface_has_capability(&delta, "ShareLink", "share_link_create"),
            "ranked gate excludes seeded-entity mutations not present in ranked_capabilities"
        );
    }
}
