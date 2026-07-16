//! Intent-filtered exposure surface for MCP `plasm_context` / incremental expand waves.

use crate::identity::{CapabilityParamName, EntityFieldName, EntityName};
use crate::schema::{CapabilityKind, CapabilitySchema, InputType, CGS};
use crate::symbol_tuning::{
    ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey, ExposureSurface,
    ExposureSurfaceDelta,
};
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

use super::mutator_admit::seeded_entity_cap_always_includes;
pub(crate) use super::mutator_admit::{
    mutating_capability_admitted, seeded_mutating_capability_admitted,
};
pub use super::mutator_admit::{ExposureSurfaceOptions, MutatorAdmit};

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
///   always admitted (test/benchmark overshow). Production [`MutatorAdmit::IntentOnly`] admits
///   seeded mutators via BM25 score **or** ranked boost.
/// - **Non-seeded** read capabilities require a non-zero lexicon overlap score against `intent`.
/// - **Non-seeded** mutating capabilities require a non-zero score; with `ranked_capability_gate`,
///   when `ranked_capability_names` is non-empty they must also appear in that list.
/// - Relations on seeded entities are admitted only when the target appears in
///   `relation_endpoint_names` and relation intent scores > 0.
/// - Mutation closure (1-hop relation targets): create/update/delete/action on targets when the
///   seat-appropriate mutator gate passes.
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
                    _ => seeded_mutating_capability_admitted(
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
                let admit = if seeded_entities.contains(target) {
                    seeded_mutating_capability_admitted(
                        score,
                        ranked_capability_names,
                        cap.name.as_str(),
                    )
                } else {
                    mutating_capability_admitted(score, ranked_capability_names, cap.name.as_str())
                };
                if !admit {
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
#[path = "exposure_surface_tests.rs"]
mod tests;
