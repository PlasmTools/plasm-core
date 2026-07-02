//! Symbol tuning for LLM prompts: opaque `e#` / `m#` / `p#` / `v#` tokens — each **distinct taught `p#` meaning**
//! is glossed **once** (the line before its first use in **teaching table**); **`v#`** rows teach each CGS `values:` /
//! `value_ref` domain **once**, and registry-backed `p#` gloss lines teach **`v# · wire`** (and optional
//! point-of-use prose when it varies); typing and enum ranges stay on the `v#` row.
//! **teaching table** gives entity/method examples (including `e#` per block),
//! `;;` descriptions (with a short **type** prefix like `date · …` / `bool · …` from CGS), comma-separated
//! `optional params: …` / `[scope …]` before the prose description (` — `), when present (required args appear in the expression).
//! Programs use **`p#` only** for keyed slots; `v#` is prompt-teaching for shared value domains.
//!
//! [`SymbolMap`] is built from the same entity slice as [`crate::prompt_render`] uses. Parse ingress
//! resolves opaque `e#` / `m#` / `p#` / `r#` in-grammar via [`SymbolMap`]; display uses
//! [`crate::expr_surface_render`] to render wire surface from parsed IR (`v#` is not expanded on ingress).
//!
//! **Caching (execute / MCP):** for a fixed loaded [`CGS`] (`catalog_cgs_hash_hex`), almost all teaching table
//! symbol structure is stable. [`TeachingExposureSession`] memoizes [`SymbolMap`] behind
//! [`TeachingExposureSession::symbol_map_arc`] and clears that cache whenever [`TeachingExposureSession::expose_entities`]
//! runs so wave indices stay consistent. Per-request variance is mostly the append-only entity list and
//! the derived `e#` / `m#` / `p#` / `v#` table.
//!
//! **Cross-session reuse (one process):** [`SymbolMapCrossRequestCache`] (bounded LRU; capacity from
//! `PLASM_SYMBOL_MAP_LRU_CAP`, default `64`, set `0` to disable) deduplicates identical [`SymbolMap`]
//! snapshots when the catalog fingerprint and exposure rows match a recent session.

mod capability_surface_params;
mod keys;
mod opaque_symbol_hash;
mod persisted_ident_metadata;
mod persisted_ledger;
mod session_bindings;
mod symbol_allocate;
mod symbol_resolve;
mod symbol_traits;
mod tables;

pub use keys::{
    CapParamKey, CatalogScope, EntityFieldKey, MethodKey, MethodSegmentKey, OpaqueESym, OpaqueMSym,
    OpaquePSym, OpaqueRSym, OpaqueVSym, QualifiedEntityKey, RelationKey,
};
pub use tables::{SymbolLedger, SymbolTables, SymbolValueLayer};

pub use session_bindings::{EntityBinding, MethodBinding, RelationBinding, SlotBinding, SlotKind};
pub use symbol_traits::{SymbolAllocate, SymbolRender, SymbolResolve, SymbolSession};

pub use capability_surface_params::{
    capability_exposure_param_pairs, capability_exposure_param_triples,
    capability_optional_legend_param_pairs, compact_mutator_param_marker,
    exposed_mutator_capability_keys, input_field_is_array, loaded_catalog_entry_ids,
    optional_legend_param_syms, resolve_ranked_wire_candidates, seeded_ranked_wire_candidates,
    CapabilityParamSurfaceFilter,
};
pub use persisted_ledger::{
    catalog_cgs_hashes_from_session, catalog_pins_match, PersistedSymbolLedger,
    PersistedSymbolLedgerDecodeError, PersistedSymbolLedgerEncodeError, PersistedSymbolLedgerState,
    PersistedSymbolTables, PERSISTED_SYMBOL_LEDGER_VERSION,
};
pub use symbol_resolve::{PSymResolution, SymbolResolveError};

use crate::identity::{
    CapabilityName, CapabilityParamName, EntityFieldName, EntityName, PathMethodSegment,
    RegistryEntryId, RelationName,
};
use crate::schema::{
    input_variant_body_type, resolve_capability_input_param_field,
    union_variant_constructor_symbol, ArrayItemsSchema, CapabilitySchema, InputFieldSchema,
    InputFieldWire, InputType, ParameterRole, StringSemantics, ValueDomainKey, CGS,
};
use crate::teaching_term::{
    method_ref_for_capability, resolve_parameter_slot, EntityRef, ParameterSlot, TeachingTerm,
};
use crate::CapabilityKind;
use crate::FieldType;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};

/// Which entities drive teaching table / symbol-map slicing (REPL `--focus`, eval, HTTP execute sessions).
#[derive(Clone, Copy, Debug, Default)]
pub enum FocusSpec<'a> {
    /// Full schema (no entity subset).
    #[default]
    All,
    /// One seed entity plus its 2-hop neighbourhood (existing behaviour).
    Single(&'a str),
    /// Union of neighbourhoods for several seeds (same CGS).
    Seeds(&'a [&'a str]),
    /// **Exact** entity list only (no 2-hop union). Used with [`TeachingExposureSession`] so teaching table and
    /// execution expand use the same monotonic `e#` / `m#` / `p#` as more of the graph is exposed.
    SeedsExact(&'a [&'a str]),
}

impl<'a> FocusSpec<'a> {
    #[inline]
    pub fn from_optional(focus: Option<&'a str>) -> Self {
        match focus {
            None => FocusSpec::All,
            Some(s) => FocusSpec::Single(s),
        }
    }
}

/// How an identifier is bound in the CGS: entity field, declared relation, or capability parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentRole {
    EntityField,
    Relation { target: EntityName },
    CapabilityParam { capability: CapabilityName },
}

/// Registry-backed slot role (entity field vs capability parameter). Relations use [`IdentMetadata::Relation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentRegistryRole {
    EntityField,
    CapabilityParam { capability: CapabilityName },
}

/// Typed metadata for one teaching table / symbol slot — **discriminated** so relations and CGS-backed
/// fields do not share optional `values:` keys (`RegistryBacked` always carries [`ValueDomainKey`]).
#[derive(Debug, Clone, PartialEq)]
pub enum IdentMetadata {
    /// Entity field or capability parameter: denormalized wire typing from [`CGS::values`].
    RegistryBacked {
        catalog_entry_id: String,
        entity: EntityName,
        role: IdentRegistryRole,
        value_registry_key: ValueDomainKey,
        field_type: FieldType,
        string_semantics: Option<StringSemantics>,
        array_items: Option<ArrayItemsSchema>,
        allowed_values: Option<Vec<String>>,
        wire_name: String,
        description: String,
    },
    /// Declared relation — not a `values:` row; terminal edge typing is entity-ref only.
    Relation {
        catalog_entry_id: String,
        entity: EntityName,
        wire_name: String,
        description: String,
        target: EntityName,
    },
    /// Heading-line / lookup miss placeholder (wire name only; no CGS row).
    SyntheticUnknown {
        catalog_entry_id: String,
        entity: EntityName,
        wire_name: String,
        description: String,
    },
    /// Inline capability input schema node (`operations`, `operations.replace_block.block`, …).
    /// [`SymbolMap`] maps dotted [`Self::param_path`] for [`SymbolMap::ident_sym_cap_param`]; teaching
    /// expansion uses the **leaf** segment so union ctor bodies type-check as `{ref=$,…}` after
    /// wire-surface render.
    CapabilityStructuralSlot {
        catalog_entry_id: String,
        entity: EntityName,
        capability: CapabilityName,
        param_path: String,
        description: String,
    },
}

/// Key for [`IdentMetadata`] maps: `(registry entry_id, CGS entity, wire name)`.
pub type IdentMetaKey = (String, EntityName, String);
use std::fmt::Write;

/// Catalog-qualified entity identity for incremental teaching exposure filtering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExposureEntityKey {
    pub entry_id: String,
    pub entity: EntityName,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExposureCapabilityKey {
    pub entry_id: String,
    pub domain: EntityName,
    pub capability: CapabilityName,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ExposureSlotKey {
    EntityField {
        entity: ExposureEntityKey,
        field: EntityFieldName,
    },
    Relation {
        source: ExposureEntityKey,
        relation: RelationName,
    },
    CapabilityParam {
        capability: ExposureCapabilityKey,
        param: CapabilityParamName,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExposureSurface {
    pub entities: BTreeSet<ExposureEntityKey>,
    pub capabilities: BTreeSet<ExposureCapabilityKey>,
    pub slots: BTreeSet<ExposureSlotKey>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExposureSurfaceDelta {
    pub required: ExposureSurface,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExposureAppendReport {
    pub entities_added: usize,
}

impl ExposureSurface {
    pub fn merge_from(&mut self, other: &ExposureSurface) {
        self.entities.extend(other.entities.iter().cloned());
        self.capabilities.extend(other.capabilities.iter().cloned());
        self.slots.extend(other.slots.iter().cloned());
    }

    pub fn fingerprint(&self) -> u64 {
        let mut h = DefaultHasher::new();
        for e in &self.entities {
            e.entry_id.hash(&mut h);
            e.entity.hash(&mut h);
        }
        for c in &self.capabilities {
            c.entry_id.hash(&mut h);
            c.domain.hash(&mut h);
            c.capability.hash(&mut h);
        }
        for s in &self.slots {
            match s {
                ExposureSlotKey::EntityField { entity, field } => {
                    0u8.hash(&mut h);
                    entity.entry_id.hash(&mut h);
                    entity.entity.hash(&mut h);
                    field.hash(&mut h);
                }
                ExposureSlotKey::Relation { source, relation } => {
                    1u8.hash(&mut h);
                    source.entry_id.hash(&mut h);
                    source.entity.hash(&mut h);
                    relation.hash(&mut h);
                }
                ExposureSlotKey::CapabilityParam { capability, param } => {
                    2u8.hash(&mut h);
                    capability.entry_id.hash(&mut h);
                    capability.domain.hash(&mut h);
                    capability.capability.hash(&mut h);
                    param.hash(&mut h);
                }
            }
        }
        h.finish()
    }
}

fn leaf_capability_param_expand_key(full_path: &str) -> String {
    full_path
        .rsplit_once('.')
        .map(|(_, leaf)| leaf.to_string())
        .unwrap_or_else(|| full_path.to_string())
}

/// Wire fragment shown after **`v# ·`** in compact registry-backed **`p#`** teaching gloss.
///
/// Nested capability params store full dotted paths (`operations.replace_range.fromRef`, …). Union /
/// variant prefixes are CGL input shape, not user-facing “types”; teach the **leaf** expand key only,
/// aligned with [`registry_backed_allocation_wire_key`] / [`slot_symbol_allocation_fingerprint`].
pub(crate) fn registry_backed_compact_wire_label(meta: &IdentMetadata) -> String {
    match meta {
        IdentMetadata::RegistryBacked {
            role: IdentRegistryRole::CapabilityParam { .. },
            wire_name,
            ..
        } if wire_name.contains('.') => leaf_capability_param_expand_key(wire_name.as_str()),
        _ => meta.wire_name().to_string(),
    }
}

fn insert_capability_param_paths(
    field: &InputFieldSchema,
    prefix: &str,
    out: &mut BTreeSet<String>,
) {
    let path = if prefix.is_empty() {
        field.name.clone()
    } else {
        format!("{prefix}.{}", field.name)
    };
    out.insert(path.clone());
    if let InputFieldWire::Inline(ty) = &field.wire {
        walk_inline_capability_param_paths(ty, &path, out);
    }
}

fn walk_inline_capability_param_paths(ty: &InputType, prefix: &str, out: &mut BTreeSet<String>) {
    match ty {
        InputType::Object { fields, .. } => {
            for f in fields {
                insert_capability_param_paths(f, prefix, out);
            }
        }
        InputType::Array { element_type, .. } => {
            walk_inline_capability_param_paths(element_type.as_ref(), prefix, out);
        }
        InputType::Union { variants } => {
            for v in variants {
                let vprefix = format!("{prefix}.{}", v.name);
                let body = input_variant_body_type(v);
                walk_inline_capability_param_paths(&body, &vprefix, out);
            }
        }
        _ => {}
    }
}

/// Full per-entity closure (legacy HTTP execute / REPL paths): every field, relation, capability, and param.
///
/// [`crate::prompt_render::surface_exposes_relation_nav_target`] admits CGS relation-nav rows toward those
/// types without requiring a separate teaching block for every hop (e.g. Pokeapi `Type`-only slices).
/// Entity-ref **fields** do not add their targets — incremental surfaces omit cross-entity navigation until
/// those entities are explicitly exposed.
///
/// `entry_id` is the caller’s registry row id (HTTP/MCP); exposure keys follow [`CGS::entry_id`] when set.
///
/// Catalog-local relation endpoint keys for intent-surface derivation (single-catalog open).
pub fn relation_endpoint_keys(entry_id: &str, names: &[String]) -> Vec<ExposureEntityKey> {
    names
        .iter()
        .map(|n| ExposureEntityKey {
            entry_id: entry_id.to_string(),
            entity: EntityName::from(n.as_str()),
        })
        .collect()
}

#[allow(unused_variables)]
pub fn legacy_exposure_surface_for_entities(
    cgs: &CGS,
    entry_id: &str,
    entities: &[&str],
    out: &mut ExposureSurface,
) {
    // Session/registry `entry_id` wins over optional YAML `CGS::entry_id` (fixtures often omit it).
    let cid = if entry_id.is_empty() {
        cgs.entry_id.clone().unwrap_or_default()
    } else {
        entry_id.to_string()
    };
    for ename in entities.iter().copied() {
        let Some(ent) = cgs.get_entity(ename) else {
            continue;
        };
        let ekey = ExposureEntityKey {
            entry_id: cid.clone(),
            entity: EntityName::from(ename),
        };
        out.entities.insert(ekey.clone());
        for (fname, _f) in &ent.fields {
            out.slots.insert(ExposureSlotKey::EntityField {
                entity: ekey.clone(),
                field: fname.clone(),
            });
        }
        for (rname, rel) in &ent.relations {
            out.slots.insert(ExposureSlotKey::Relation {
                source: ekey.clone(),
                relation: rname.clone(),
            });
            let tgt = rel.target_resource.as_str();
            if cgs.get_entity(tgt).is_some() {
                out.entities.insert(ExposureEntityKey {
                    entry_id: cid.clone(),
                    entity: EntityName::from(tgt),
                });
            }
        }
        let Some(names) = cgs.capability_names_by_domain().get(ename) else {
            continue;
        };
        for cap_name in names {
            let Some(cap) = cgs.capabilities.get(cap_name) else {
                continue;
            };
            let ckey = ExposureCapabilityKey {
                entry_id: cid.clone(),
                domain: EntityName::from(ename),
                capability: cap_name.clone(),
            };
            out.capabilities.insert(ckey.clone());
            if let Some(is) = &cap.input_schema {
                let mut paths = BTreeSet::new();
                match &is.input_type {
                    InputType::Object { fields, .. } => {
                        for f in fields {
                            insert_capability_param_paths(f, "", &mut paths);
                        }
                    }
                    InputType::Union { variants } => {
                        for v in variants {
                            let body = input_variant_body_type(v);
                            walk_inline_capability_param_paths(&body, "", &mut paths);
                        }
                    }
                    _ => {}
                }
                for path in paths {
                    out.slots.insert(ExposureSlotKey::CapabilityParam {
                        capability: ckey.clone(),
                        param: CapabilityParamName::new(path),
                    });
                }
            }
        }
    }
}

pub fn legacy_exposure_surface_delta_for_entities(
    cgs: &CGS,
    entry_id: &str,
    entities: &[&str],
) -> ExposureSurfaceDelta {
    let mut required = ExposureSurface::default();
    legacy_exposure_surface_for_entities(cgs, entry_id, entities, &mut required);
    ExposureSurfaceDelta { required }
}

pub(crate) fn collect_slot_metas_for_surface(
    catalog_cgs: &IndexMap<String, Arc<CGS>>,
    surface: &ExposureSurface,
) -> Vec<IdentMetadata> {
    let mut out = Vec::new();
    for slot in &surface.slots {
        match slot {
            ExposureSlotKey::EntityField { entity, field } => {
                let Some(cgs) = catalog_cgs.get(&entity.entry_id) else {
                    continue;
                };
                let Some(ent) = cgs.get_entity(entity.entity.as_str()) else {
                    continue;
                };
                let Some(f) = ent.fields.get(field) else {
                    continue;
                };
                let nv = f.named_value(cgs).expect("values row for entity field");
                let en = entity.entity.clone();
                let cid = entity.entry_id.clone();
                out.push(IdentMetadata::RegistryBacked {
                    catalog_entry_id: cid,
                    entity: en,
                    role: IdentRegistryRole::EntityField,
                    value_registry_key: f.kind.registry_key().clone(),
                    field_type: nv.field_type.clone(),
                    string_semantics: nv.string_semantics,
                    array_items: nv.array_items.clone(),
                    allowed_values: nv.allowed_values.clone(),
                    wire_name: field.as_str().to_string(),
                    description: f.description.clone(),
                });
            }
            ExposureSlotKey::Relation { source, relation } => {
                let Some(cgs) = catalog_cgs.get(&source.entry_id) else {
                    continue;
                };
                let Some(ent) = cgs.get_entity(source.entity.as_str()) else {
                    continue;
                };
                let Some(r) = ent.relations.get(relation) else {
                    continue;
                };
                out.push(IdentMetadata::Relation {
                    catalog_entry_id: source.entry_id.clone(),
                    entity: source.entity.clone(),
                    wire_name: relation.as_str().to_string(),
                    description: r.description.clone(),
                    target: r.target_resource.clone(),
                });
            }
            ExposureSlotKey::CapabilityParam { capability, param } => {
                let Some(cgs) = catalog_cgs.get(&capability.entry_id) else {
                    continue;
                };
                let Some(cap) = cgs.capabilities.get(&capability.capability) else {
                    continue;
                };
                let path = param.as_str();
                let Some(f) = resolve_capability_input_param_field(cap, path) else {
                    continue;
                };
                match &f.wire {
                    InputFieldWire::Registry(k) => {
                        let nv = match f.named_value(cgs) {
                            Ok(nv) => nv,
                            Err(_) => continue,
                        };
                        out.push(IdentMetadata::RegistryBacked {
                            catalog_entry_id: capability.entry_id.clone(),
                            entity: capability.domain.clone(),
                            role: IdentRegistryRole::CapabilityParam {
                                capability: cap.name.clone(),
                            },
                            value_registry_key: k.clone(),
                            field_type: nv.field_type.clone(),
                            string_semantics: nv.string_semantics,
                            array_items: nv.array_items.clone(),
                            allowed_values: nv.allowed_values.clone(),
                            wire_name: path.to_string(),
                            description: f.description.clone().unwrap_or_default(),
                        });
                    }
                    InputFieldWire::Inline(_) => {
                        out.push(IdentMetadata::CapabilityStructuralSlot {
                            catalog_entry_id: capability.entry_id.clone(),
                            entity: capability.domain.clone(),
                            capability: cap.name.clone(),
                            param_path: path.to_string(),
                            description: f.description.clone().unwrap_or_default(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// Build [`IdentMetadata`] for a nested or top-level capability input path using live [`CGS`] rows.
///
/// Used by teaching gloss emission when the opaque `p#` maps to a capability slot whose **leaf**
/// expand key collides with an entity relation wire name (e.g. param `…blocks` vs relation `blocks`).
pub(crate) fn ident_metadata_for_capability_input_path(
    cgs: &CGS,
    domain_entity: &str,
    cap_name: &str,
    param_path: &str,
) -> Option<IdentMetadata> {
    let cap = cgs.capabilities.get(&CapabilityName::from(cap_name))?;
    if cap.domain.as_str() != domain_entity {
        return None;
    }
    let f = resolve_capability_input_param_field(cap, param_path)?;
    let cid = cgs.entry_id.clone().unwrap_or_default();
    match &f.wire {
        InputFieldWire::Registry(k) => {
            let nv = f.named_value(cgs).ok()?;
            Some(IdentMetadata::RegistryBacked {
                catalog_entry_id: cid,
                entity: cap.domain.clone(),
                role: IdentRegistryRole::CapabilityParam {
                    capability: cap.name.clone(),
                },
                value_registry_key: k.clone(),
                field_type: nv.field_type.clone(),
                string_semantics: nv.string_semantics,
                array_items: nv.array_items.clone(),
                allowed_values: nv.allowed_values.clone(),
                wire_name: param_path.to_string(),
                description: f.description.clone().unwrap_or_default(),
            })
        }
        InputFieldWire::Inline(_) => Some(IdentMetadata::CapabilityStructuralSlot {
            catalog_entry_id: cid,
            entity: cap.domain.clone(),
            capability: cap.name.clone(),
            param_path: param_path.to_string(),
            description: f.description.clone().unwrap_or_default(),
        }),
    }
}

/// Same 2-hop focus neighbourhood as prompt rendering: `Some(set)` when focus is set.
#[inline]
pub(crate) fn field_is_filter_like_gloss(f: &InputFieldSchema) -> bool {
    !matches!(
        f.role,
        Some(ParameterRole::Search)
            | Some(ParameterRole::Sort)
            | Some(ParameterRole::SortDirection)
            | Some(ParameterRole::ResponseControl)
    )
}

/// Union of [`build_focus_set`] for each seed (same rules as single focus).
pub fn build_focus_set_union<'a>(cgs: &'a CGS, seeds: &[&'a str]) -> HashSet<&'a str> {
    let mut u = HashSet::new();
    for s in seeds {
        if let Some(set) = build_focus_set(cgs, Some(*s)) {
            u.extend(set);
        }
    }
    u
}

pub fn build_focus_set<'a>(cgs: &'a CGS, focus: Option<&'a str>) -> Option<HashSet<&'a str>> {
    let f = focus?;
    let mut s = HashSet::new();
    s.insert(f);
    if let Some(ent) = cgs.get_entity(f) {
        for field in ent.fields.values() {
            if let Ok(nv) = field.named_value(cgs) {
                if let FieldType::EntityRef { target } = &nv.field_type {
                    s.insert(target.as_str());
                }
            }
        }
        for rel in ent.relations.values() {
            s.insert(rel.target_resource.as_str());
        }
    }
    for (ename, ent) in &cgs.entities {
        for field in ent.fields.values() {
            if let Ok(nv) = field.named_value(cgs) {
                if let FieldType::EntityRef { target } = &nv.field_type {
                    if target.as_str() == f {
                        s.insert(ename.as_str());
                    }
                }
            }
        }
    }
    Some(s)
}

/// `(full_entities_in_prompt, dim_entity_names)` — mirrors [`crate::prompt_render`].
pub fn entity_slices_for_render<'a>(
    cgs: &'a CGS,
    focus: FocusSpec<'a>,
) -> (Vec<&'a str>, Vec<&'a str>) {
    if let FocusSpec::SeedsExact(seeds) = focus {
        let mut full = Vec::new();
        for s in seeds.iter().copied() {
            if cgs.get_entity(s).is_some() {
                full.push(s);
            }
        }
        // `SeedsExact` matches [`TeachingExposureSession::entities`] only (no 2-hop neighbourhood).
        // Exposure-bundle rendering ignores `_dim_entities` for this focus mode, so skip the full-schema
        // scan that built `dim` for legacy All/Single/Seeds slices.
        return (full, Vec::new());
    }

    let focus_set: Option<HashSet<&'a str>> = match focus {
        FocusSpec::All => None,
        FocusSpec::Single(s) => build_focus_set(cgs, Some(s)),
        FocusSpec::Seeds(seeds) => {
            if seeds.is_empty() {
                None
            } else {
                Some(build_focus_set_union(cgs, seeds))
            }
        }
        FocusSpec::SeedsExact(_) => unreachable!("handled above"),
    };
    let full_entities: Vec<&str> = cgs
        .entities
        .iter()
        .filter(|(n, ent)| {
            if ent.abstract_entity {
                return false;
            }
            focus_set
                .as_ref()
                .map(|s| s.contains(n.as_str()))
                .unwrap_or(true)
        })
        .map(|(n, _)| n.as_str())
        .collect();
    let dim_entities: Vec<&str> = cgs
        .entities
        .iter()
        .filter(|(n, ent)| {
            if ent.abstract_entity {
                return false;
            }
            focus_set
                .as_ref()
                .map(|s| !s.contains(n.as_str()))
                .unwrap_or(false)
        })
        .map(|(n, _)| n.as_str())
        .collect();
    (full_entities, dim_entities)
}

/// Full + dim entity name slices when [`TeachingExposureSession`] spans multiple loaded [`crate::schema::CGS`] graphs.
pub fn entity_slices_for_render_federated<'a>(
    cgs_layers: &[&'a CGS],
    exposure: &'a TeachingExposureSession,
) -> (Vec<&'a str>, Vec<&'a str>) {
    if cgs_layers.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let refs: Vec<&'a str> = exposure.entities.iter().map(|s| s.as_str()).collect();
    let mut full: Vec<&str> = Vec::new();
    let mut full_set: HashSet<&str> = HashSet::new();
    for &name in &refs {
        let ok = cgs_layers.iter().any(|c| c.get_entity(name).is_some());
        if ok {
            full.push(name);
            full_set.insert(name);
        }
    }
    let mut dim_set: HashSet<&str> = HashSet::new();
    for cgs in cgs_layers {
        for (n, ent) in &cgs.entities {
            if ent.abstract_entity || full_set.contains(n.as_str()) {
                continue;
            }
            dim_set.insert(n.as_str());
        }
    }
    let mut dim: Vec<&str> = dim_set.into_iter().collect();
    dim.sort();
    (full, dim)
}

/// Same `p#` name set as [`SymbolMap::build`] (entity fields + relations + capability inputs for `full_entities`).
pub(crate) fn collect_ident_names(cgs: &CGS, full_entities: &[&str]) -> BTreeSet<String> {
    let full_set: HashSet<&str> = full_entities.iter().copied().collect();
    let mut idents: BTreeSet<String> = BTreeSet::new();
    for e in full_entities {
        let Some(ent) = cgs.get_entity(e) else {
            continue;
        };
        for (k, _) in &ent.fields {
            idents.insert(k.as_str().to_string());
        }
        for (k, _) in &ent.relations {
            idents.insert(k.as_str().to_string());
        }
    }
    for dom in &full_set {
        let Some(names) = cgs.capability_names_by_domain().get(*dom) else {
            continue;
        };
        for cap_name in names {
            let Some(cap) = cgs.capabilities.get(cap_name) else {
                continue;
            };
            let Some(is) = &cap.input_schema else {
                continue;
            };
            let InputType::Object { fields, .. } = &is.input_type else {
                continue;
            };
            for f in fields {
                idents.insert(f.name.clone());
            }
        }
    }
    idents
}

/// Stable fingerprint for slot **full identity** (diagnostics / occurrence distinction): catalog,
/// owning entity, role, structural type, wire name, `value_ref`, and description.
///
/// **Opaque `p#` allocation** uses [`slot_symbol_allocation_fingerprint`] instead: registry-backed
/// slots that share the same `values:` row and wire name reuse one `p#`.
pub(crate) fn slot_allocation_fingerprint(meta: &IdentMetadata) -> String {
    let (role_tag, ft, sem, ai, av, vr, catalog_entry_id, entity, wire_name, desc) = match meta {
        IdentMetadata::RegistryBacked {
            catalog_entry_id,
            entity,
            role,
            value_registry_key,
            field_type,
            string_semantics,
            array_items,
            allowed_values,
            wire_name,
            description,
        } => {
            let role_tag = match role {
                IdentRegistryRole::EntityField => "ef".to_string(),
                IdentRegistryRole::CapabilityParam { capability } => {
                    format!("cap:{}|{}", entity.as_str(), capability.as_str())
                }
            };
            let ft = serde_json::to_string(field_type).unwrap_or_else(|_| "\"?\"".to_string());
            let sem =
                serde_json::to_string(string_semantics).unwrap_or_else(|_| "null".to_string());
            let ai = serde_json::to_string(array_items).unwrap_or_else(|_| "null".to_string());
            let av = serde_json::to_string(allowed_values).unwrap_or_else(|_| "null".to_string());
            let vr = value_registry_key.as_str();
            (
                role_tag,
                ft,
                sem,
                ai,
                av,
                vr,
                catalog_entry_id.as_str(),
                entity.as_str(),
                wire_name.as_str(),
                description.trim(),
            )
        }
        IdentMetadata::Relation {
            catalog_entry_id,
            entity,
            wire_name,
            description,
            target,
        } => {
            let role_tag = format!("rel:{}", target.as_str());
            let ft = serde_json::to_string(&FieldType::EntityRef {
                target: target.clone(),
            })
            .unwrap_or_else(|_| "\"?\"".to_string());
            (
                role_tag,
                ft,
                "null".to_string(),
                "null".to_string(),
                "null".to_string(),
                "",
                catalog_entry_id.as_str(),
                entity.as_str(),
                wire_name.as_str(),
                description.trim(),
            )
        }
        IdentMetadata::CapabilityStructuralSlot {
            catalog_entry_id,
            entity,
            capability,
            param_path,
            description,
        } => {
            let role_tag = format!("capstruct:{}|{}", entity.as_str(), capability.as_str());
            (
                role_tag,
                serde_json::to_string(&FieldType::Json).unwrap_or_else(|_| "\"?\"".to_string()),
                "null".to_string(),
                "null".to_string(),
                "null".to_string(),
                "",
                catalog_entry_id.as_str(),
                entity.as_str(),
                param_path.as_str(),
                description.trim(),
            )
        }
        IdentMetadata::SyntheticUnknown {
            catalog_entry_id,
            entity,
            wire_name,
            description,
        } => (
            "ef".to_string(),
            serde_json::to_string(&FieldType::String).unwrap_or_else(|_| "\"?\"".to_string()),
            "null".to_string(),
            "null".to_string(),
            "null".to_string(),
            "",
            catalog_entry_id.as_str(),
            entity.as_str(),
            wire_name.as_str(),
            description.trim(),
        ),
    };
    format!("{catalog_entry_id}|{entity}|{role_tag}|{wire_name}|{ft}|{sem}|{ai}|{av}|{vr}|{desc}",)
}

/// Fingerprint for **allocating** opaque `p#` symbols on registry-backed slots.
///
/// Slots that share the same CGS `values:` row ([`IdentMetadata::value_domain_allocation_fp`]) and
/// the same allocation wire key receive **one** `p#`. Occurrence lookups (`entity_field_to_sym`,
/// `cap_param_to_sym`) still bind every `(entity, slot)` / `(cap, param)` to that shared symbol.
///
/// **Capability parameters** whose wire path is dotted (nested input / union-variant bodies) key on
/// `(domain entity, capability, leaf)` instead of the full path so logically identical slots—same
/// `values:` row and leaf field name after variant pruning—share one opaque symbol (e.g. every
/// `…​.ref` block anchor under `document_edit_v2`). Top-level capability params keep the plain wire
/// name so they still merge with entity fields when those fields reuse the same registry row and
/// column name.
///
/// Relations and synthetic unknown slots keep fully scoped fingerprints via
/// [`slot_allocation_fingerprint`].
pub(crate) fn slot_symbol_allocation_fingerprint(meta: &IdentMetadata) -> String {
    if matches!(meta, IdentMetadata::CapabilityStructuralSlot { .. }) {
        return slot_allocation_fingerprint(meta);
    }
    if let IdentMetadata::RegistryBacked { .. } = meta {
        if let Some(vfp) = meta.value_domain_allocation_fp() {
            let wkey = registry_backed_allocation_wire_key(meta);
            return format!("{vfp}|w:{wkey}");
        }
    }
    slot_allocation_fingerprint(meta)
}

#[inline]
fn registry_backed_allocation_wire_key(meta: &IdentMetadata) -> String {
    match meta {
        IdentMetadata::RegistryBacked {
            role: IdentRegistryRole::CapabilityParam { capability },
            entity,
            wire_name,
            ..
        } if wire_name.contains('.') => format!(
            "{}|{}|{}",
            entity.as_str(),
            capability.as_str(),
            leaf_capability_param_expand_key(wire_name.as_str())
        ),
        IdentMetadata::RegistryBacked { wire_name, .. } => wire_name.clone(),
        _ => meta.wire_name().to_string(),
    }
}

/// Stable key for one concrete slot occurrence (entity field, relation, or capability param).
/// Unlike [`slot_allocation_fingerprint`], this keeps entity ownership so scoped symbol maps can
/// rebuild exact `(entity, slot)` bindings even when several occurrences intentionally share one
/// opaque `p#`.
fn slot_occurrence_key(meta: &IdentMetadata) -> String {
    match meta {
        IdentMetadata::RegistryBacked {
            catalog_entry_id,
            entity,
            role,
            wire_name,
            ..
        } => match role {
            IdentRegistryRole::EntityField => format!(
                "ef|{}|{}|{}",
                catalog_entry_id.as_str(),
                entity.as_str(),
                wire_name
            ),
            IdentRegistryRole::CapabilityParam { capability } => format!(
                "cap|{}|{}|{}|{}",
                catalog_entry_id.as_str(),
                entity.as_str(),
                capability.as_str(),
                wire_name
            ),
        },
        IdentMetadata::Relation {
            catalog_entry_id,
            entity,
            wire_name,
            target,
            ..
        } => format!(
            "rel|{}|{}|{}|{}",
            catalog_entry_id.as_str(),
            entity.as_str(),
            wire_name,
            target.as_str()
        ),
        IdentMetadata::CapabilityStructuralSlot {
            catalog_entry_id,
            entity,
            capability,
            param_path,
            ..
        } => format!(
            "capstruct|{}|{}|{}|{}",
            catalog_entry_id.as_str(),
            entity.as_str(),
            capability.as_str(),
            param_path
        ),
        IdentMetadata::SyntheticUnknown {
            catalog_entry_id,
            entity,
            wire_name,
            ..
        } => format!(
            "ef|{}|{}|{}",
            catalog_entry_id.as_str(),
            entity.as_str(),
            wire_name
        ),
    }
}

/// Build typed metadata for all (entity, ident) pairs in the full-entity slice.
/// Replaces the global first-wins `build_ident_gloss_map` + `build_ident_type_map` pipeline.
pub(crate) fn build_ident_metadata(
    cgs: &CGS,
    full_entities: &[&str],
) -> HashMap<IdentMetaKey, IdentMetadata> {
    let full_set: HashSet<&str> = full_entities.iter().copied().collect();
    let mut out: HashMap<IdentMetaKey, IdentMetadata> = HashMap::new();
    let cid = cgs.entry_id.clone().unwrap_or_default();

    for &ename in full_entities {
        let Some(ent) = cgs.get_entity(ename) else {
            continue;
        };
        let en = EntityName::from(ename.to_string());
        for (fname, f) in &ent.fields {
            let nv = f.named_value(cgs).expect("values row for entity field");
            out.entry((cid.clone(), en.clone(), fname.as_str().to_string()))
                .or_insert_with(|| IdentMetadata::RegistryBacked {
                    catalog_entry_id: cid.clone(),
                    entity: en.clone(),
                    role: IdentRegistryRole::EntityField,
                    value_registry_key: f.kind.registry_key().clone(),
                    field_type: nv.field_type.clone(),
                    string_semantics: nv.string_semantics,
                    array_items: nv.array_items.clone(),
                    allowed_values: nv.allowed_values.clone(),
                    wire_name: fname.as_str().to_string(),
                    description: f.description.clone(),
                });
        }
        for (rname, r) in &ent.relations {
            out.entry((cid.clone(), en.clone(), rname.as_str().to_string()))
                .or_insert_with(|| IdentMetadata::Relation {
                    catalog_entry_id: cid.clone(),
                    entity: en.clone(),
                    wire_name: rname.as_str().to_string(),
                    description: r.description.clone(),
                    target: r.target_resource.clone(),
                });
        }
    }
    for dom in &full_set {
        let Some(names) = cgs.capability_names_by_domain().get(*dom) else {
            continue;
        };
        for cap_name in names {
            let Some(cap) = cgs.capabilities.get(cap_name) else {
                continue;
            };
            let Some(is) = &cap.input_schema else {
                continue;
            };
            let InputType::Object { fields, .. } = &is.input_type else {
                continue;
            };
            let en = cap.domain.clone();
            for f in fields {
                let Ok(nv) = f.named_value(cgs) else {
                    continue;
                };
                let crate::InputFieldWire::Registry(ref k) = &f.wire else {
                    continue;
                };
                out.entry((cid.clone(), en.clone(), f.name.clone()))
                    .or_insert_with(|| IdentMetadata::RegistryBacked {
                        catalog_entry_id: cid.clone(),
                        entity: en.clone(),
                        role: IdentRegistryRole::CapabilityParam {
                            capability: cap.name.clone(),
                        },
                        value_registry_key: k.clone(),
                        field_type: nv.field_type.clone(),
                        string_semantics: nv.string_semantics,
                        array_items: nv.array_items.clone(),
                        allowed_values: nv.allowed_values.clone(),
                        wire_name: f.name.clone(),
                        description: f.description.clone().unwrap_or_default(),
                    });
            }
        }
    }
    out
}

impl IdentMetadata {
    /// Same three-way dispatch as legacy [`IdentRole`] for fingerprint maps and symbol tables.
    #[inline]
    pub fn allocation_ident_role(&self) -> IdentRole {
        match self {
            IdentMetadata::RegistryBacked { role, .. } => match role {
                IdentRegistryRole::EntityField => IdentRole::EntityField,
                IdentRegistryRole::CapabilityParam { capability } => IdentRole::CapabilityParam {
                    capability: capability.clone(),
                },
            },
            IdentMetadata::Relation { target, .. } => IdentRole::Relation {
                target: target.clone(),
            },
            IdentMetadata::SyntheticUnknown { .. } => IdentRole::EntityField,
            IdentMetadata::CapabilityStructuralSlot { capability, .. } => {
                IdentRole::CapabilityParam {
                    capability: capability.clone(),
                }
            }
        }
    }

    #[inline]
    pub fn catalog_entry_id(&self) -> &str {
        match self {
            IdentMetadata::RegistryBacked {
                catalog_entry_id, ..
            }
            | IdentMetadata::Relation {
                catalog_entry_id, ..
            }
            | IdentMetadata::SyntheticUnknown {
                catalog_entry_id, ..
            }
            | IdentMetadata::CapabilityStructuralSlot {
                catalog_entry_id, ..
            } => catalog_entry_id.as_str(),
        }
    }

    #[inline]
    pub fn entity(&self) -> &EntityName {
        match self {
            IdentMetadata::RegistryBacked { entity, .. }
            | IdentMetadata::Relation { entity, .. }
            | IdentMetadata::SyntheticUnknown { entity, .. }
            | IdentMetadata::CapabilityStructuralSlot { entity, .. } => entity,
        }
    }

    #[inline]
    pub fn wire_name(&self) -> &str {
        match self {
            IdentMetadata::RegistryBacked { wire_name, .. }
            | IdentMetadata::Relation { wire_name, .. }
            | IdentMetadata::SyntheticUnknown { wire_name, .. } => wire_name.as_str(),
            IdentMetadata::CapabilityStructuralSlot { param_path, .. } => param_path.as_str(),
        }
    }

    fn description_trimmed(&self) -> &str {
        match self {
            IdentMetadata::RegistryBacked { description, .. }
            | IdentMetadata::Relation { description, .. }
            | IdentMetadata::SyntheticUnknown { description, .. }
            | IdentMetadata::CapabilityStructuralSlot { description, .. } => description.trim(),
        }
    }

    #[inline]
    pub fn description(&self) -> &str {
        match self {
            IdentMetadata::RegistryBacked { description, .. }
            | IdentMetadata::Relation { description, .. }
            | IdentMetadata::SyntheticUnknown { description, .. }
            | IdentMetadata::CapabilityStructuralSlot { description, .. } => description.as_str(),
        }
    }

    #[inline]
    pub fn allowed_values(&self) -> Option<&Vec<String>> {
        match self {
            IdentMetadata::RegistryBacked { allowed_values, .. } => allowed_values.as_ref(),
            IdentMetadata::Relation { .. }
            | IdentMetadata::SyntheticUnknown { .. }
            | IdentMetadata::CapabilityStructuralSlot { .. } => None,
        }
    }

    /// Render the gloss line content (after `p#  ;;  `). The `map` is used to resolve
    /// entity-ref targets to their `e#` symbol when symbol tuning is active.
    pub fn render_gloss(&self, map: Option<&SymbolMap>) -> String {
        self.render_gloss_with_cgs(map, None)
    }

    /// Like [`Self::render_gloss`], but resolves [`IdentMetadata::CapabilityStructuralSlot`] typing
    /// from live [`CGS`] inline [`InputType`] (e.g. `array[union · v101 | …]`) instead of `json`.
    pub fn render_gloss_with_cgs(&self, map: Option<&SymbolMap>, cgs: Option<&CGS>) -> String {
        match self {
            IdentMetadata::Relation {
                target,
                wire_name: _,
                description,
                ..
            } => {
                let type_label = match (map, cgs.and_then(|c| c.entry_id.as_deref())) {
                    (Some(m), Some(eid)) => {
                        format!("=> {}", m.entity_sym_for(eid, target.as_str()))
                    }
                    (Some(m), None) => format!("=> {}", m.entity_sym_for("", target.as_str())),
                    (None, _) => format!("=> {}", target),
                };
                let desc = description.trim();
                if desc.is_empty() {
                    format!("{type_label} \u{00b7} {}", target)
                } else {
                    let truncated = truncate_desc(desc, 100);
                    format!("{type_label} \u{00b7} {truncated}")
                }
            }
            IdentMetadata::SyntheticUnknown { wire_name, .. } => {
                let type_label = array_or_scalar_gloss_label(&FieldType::String, &None, None, map);
                format!("{type_label} \u{00b7} {}", wire_name)
            }
            IdentMetadata::CapabilityStructuralSlot {
                entity,
                capability,
                param_path,
                ..
            } => {
                let leaf = leaf_capability_param_expand_key(param_path.as_str());
                let type_label = cgs
                    .and_then(|c| {
                        capability_structural_slot_type_prefix(
                            c,
                            entity.as_str(),
                            capability,
                            param_path.as_str(),
                            map,
                        )
                    })
                    .unwrap_or_else(|| {
                        array_or_scalar_gloss_label(&FieldType::Json, &None, None, map)
                    });
                format!("{type_label} \u{00b7} {}", leaf)
            }
            IdentMetadata::RegistryBacked {
                field_type,
                array_items,
                string_semantics,
                allowed_values,
                wire_name,
                role,
                ..
            } => {
                let type_label =
                    array_or_scalar_gloss_label(field_type, array_items, *string_semantics, map);
                if matches!(field_type, FieldType::Select | FieldType::MultiSelect) {
                    if let Some(ref av) = allowed_values {
                        if !av.is_empty() {
                            let joined = av.join(", ");
                            return format!("{type_label} · {joined}");
                        }
                    }
                }
                let desc = self.description_trimmed();
                let cap_param = matches!(role, IdentRegistryRole::CapabilityParam { .. });
                if cap_param {
                    if desc.is_empty() {
                        return type_label;
                    }
                    let truncated = truncate_desc(desc, 100);
                    return format!("{type_label} \u{00b7} {truncated}");
                }
                if desc.is_empty() {
                    format!("{type_label} \u{00b7} {}", wire_name)
                } else {
                    let truncated = truncate_desc(desc, 100);
                    format!("{type_label} \u{00b7} {truncated}")
                }
            }
        }
    }

    /// Stable key for one CGS [`values:`] row: `(catalog_entry_id, value_ref)`.
    #[inline]
    pub fn value_domain_allocation_fp(&self) -> Option<String> {
        match self {
            IdentMetadata::RegistryBacked {
                catalog_entry_id,
                value_registry_key,
                ..
            } => Some(format!(
                "{}|vr:{}",
                catalog_entry_id.as_str(),
                value_registry_key.as_str()
            )),
            IdentMetadata::Relation { .. }
            | IdentMetadata::SyntheticUnknown { .. }
            | IdentMetadata::CapabilityStructuralSlot { .. } => None,
        }
    }

    /// Gloss for a **`v#` teaching table row** — typing from the shared `values:` registry row (`value_row_description`),
    /// not per-slot field/capability prose.
    pub fn render_value_domain_row_gloss(
        &self,
        value_row_description: &str,
        map: Option<&SymbolMap>,
        cgs: Option<&CGS>,
    ) -> Option<String> {
        let IdentMetadata::RegistryBacked {
            field_type,
            array_items,
            string_semantics,
            allowed_values,
            value_registry_key: _,
            ..
        } = self
        else {
            return None;
        };
        if let FieldType::EntityRef { target } = field_type {
            return Some(entity_ref_value_domain_row_gloss(
                target,
                cgs,
                value_row_description,
            ));
        }
        let type_label =
            array_or_scalar_gloss_label(field_type, array_items, *string_semantics, map);
        if matches!(field_type, FieldType::Select | FieldType::MultiSelect) {
            if let Some(ref av) = allowed_values {
                if !av.is_empty() {
                    let joined = av.join(", ");
                    return Some(format!("{type_label} · {joined}"));
                }
            }
        }
        let desc = value_row_description.trim();
        if desc.is_empty() {
            // Internal `values:` keys (`nv_*`) are not user-facing teaching; type label alone is enough.
            Some(type_label)
        } else {
            let truncated = truncate_desc(desc, 100);
            Some(format!("{type_label} · {truncated}"))
        }
    }
}

/// Full **`v#` Meaning** for an `entity_ref` value domain: `ref:Zone · str · …` — canonical target
/// entity name (not `e#`), id primitive when resolvable, then optional `values:` row prose.
pub(crate) fn entity_ref_value_domain_row_gloss(
    target: &EntityName,
    cgs: Option<&CGS>,
    value_row_description: &str,
) -> String {
    let canonical = target.as_str();
    let prim = cgs.and_then(|c| {
        let ent = c.get_entity(target.as_str())?;
        let f = ent.fields.get(ent.id_field.as_str())?;
        let nv = f.named_value(c).ok()?;
        match &nv.field_type {
            FieldType::EntityRef { .. } => None,
            FieldType::String => Some(string_semantics_gloss_label(nv.string_semantics)),
            FieldType::Array | FieldType::Json => None,
            ft => Some(field_type_to_gloss_label(ft)),
        }
    });
    let desc = value_row_description.trim();
    let desc_opt = if desc.is_empty() {
        None
    } else {
        Some(truncate_desc(desc, 100))
    };
    match (prim.as_deref(), desc_opt) {
        (Some(p), Some(d)) => format!("ref:{canonical} · {p} · {d}"),
        (Some(p), None) => format!("ref:{canonical} · {p}"),
        (None, Some(d)) => format!("ref:{canonical} · {d}"),
        (None, None) => format!("ref:{canonical}"),
    }
}

/// Short type label for teaching table `p#` gloss (matches [`FieldType`] / capability inputs).
/// Type keyword for a scalar `string` in teaching gloss (`str` vs `markdown`, …).
pub(crate) fn string_semantics_gloss_label(sem: Option<StringSemantics>) -> String {
    let s = sem.unwrap_or(StringSemantics::Short);
    s.gloss_type_keyword().unwrap_or("str").to_string()
}

pub(crate) fn field_type_to_gloss_label(ft: &FieldType) -> String {
    match ft {
        FieldType::Boolean => "bool".to_string(),
        FieldType::Number => "float".to_string(),
        FieldType::Integer => "int".to_string(),
        FieldType::String => "str".to_string(),
        FieldType::Blob => "blob".to_string(),
        FieldType::Uuid => "uuid".to_string(),
        FieldType::Select => "select".to_string(),
        FieldType::MultiSelect => "multiselect".to_string(),
        FieldType::Date => "date".to_string(),
        FieldType::Array => "array".to_string(),
        FieldType::Json => "json".to_string(),
        FieldType::EntityRef { target } => format!("ref:{target}"),
    }
}

fn array_element_gloss_label(ai: &ArrayItemsSchema, map: Option<&SymbolMap>) -> String {
    match &ai.field_type {
        FieldType::EntityRef { target } => {
            let sym = map
                .map(|m| m.entity_sym_for("", target.as_str()))
                .unwrap_or_else(|| target.to_string());
            format!("ref:{sym}")
        }
        _ => field_type_to_gloss_label(&ai.field_type),
    }
}

/// Label for inline capability input shapes (`operations`, nested union bodies) — avoids labeling
/// typed `array[union]` batches as bare `json` in teaching gloss.
fn structural_inline_input_type_label(ty: &InputType, map: Option<&SymbolMap>) -> Option<String> {
    match ty {
        InputType::Array { element_type, .. } => {
            if let InputType::Union { variants } = element_type.as_ref() {
                if variants
                    .iter()
                    .all(|v| union_variant_constructor_symbol(v).is_some())
                {
                    let alts: Vec<&str> = variants
                        .iter()
                        .filter_map(union_variant_constructor_symbol)
                        .collect();
                    return Some(format!("union · {}", alts.join(" | ")));
                }
            }
            structural_inline_input_type_label(element_type.as_ref(), map)
                .map(|inner| format!("array[{inner}]"))
        }
        InputType::Union { variants } => {
            if variants
                .iter()
                .all(|v| union_variant_constructor_symbol(v).is_some())
            {
                let alts: Vec<&str> = variants
                    .iter()
                    .filter_map(union_variant_constructor_symbol)
                    .collect();
                return Some(format!("union · {}", alts.join(" | ")));
            }
            None
        }
        InputType::Object { .. } => Some("object".to_string()),
        InputType::None => Some("none".to_string()),
        InputType::Value {
            field_type,
            allowed_values: _,
        } => Some(array_or_scalar_gloss_label(field_type, &None, None, map)),
    }
}

#[inline]
fn capability_structural_slot_type_prefix(
    cgs: &CGS,
    entity: &str,
    capability: &CapabilityName,
    param_path: &str,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let cap = cgs.capabilities.get(capability)?;
    if cap.domain.as_str() != entity {
        return None;
    }
    let f = resolve_capability_input_param_field(cap, param_path)?;
    let InputFieldWire::Inline(ty) = &f.wire else {
        return None;
    };
    structural_inline_input_type_label(ty.as_ref(), map)
}

/// Type prefix for `p#  ;;  …` lines: `array[inner]` when element typing is known, else `array`.
fn array_or_scalar_gloss_label(
    ft: &FieldType,
    items: &Option<ArrayItemsSchema>,
    string_semantics: Option<StringSemantics>,
    map: Option<&SymbolMap>,
) -> String {
    match ft {
        FieldType::Array => match items {
            Some(ai) => format!("array[{}]", array_element_gloss_label(ai, map)),
            None => "array".to_string(),
        },
        FieldType::String => string_semantics_gloss_label(string_semantics),
        FieldType::Blob => "blob".to_string(),
        _ => field_type_to_gloss_label(ft),
    }
}

/// Resolve a schema type string for `ident`, scoped like [`SymbolMap::build`].
/// Prefers **capability** input fields (query filters) over entity fields, then relations.
///
/// Relation names resolve to `=> e#` (when `map` is set) or `=> TargetEntity` — same “points at entity”
/// shape as capability result hints, not `relation→…`.
#[allow(dead_code)]
fn resolve_ident_type_string(
    cgs: &CGS,
    full_entities: &[&str],
    name: &str,
    map: Option<&SymbolMap>,
) -> Option<String> {
    let full_set: HashSet<&str> = full_entities.iter().copied().collect();
    let mut caps: Vec<&CapabilitySchema> = cgs.capabilities.values().collect();
    caps.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
    for cap in caps {
        if !full_set.contains(cap.domain.as_str()) {
            continue;
        }
        let Some(is) = &cap.input_schema else {
            continue;
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            continue;
        };
        for f in fields {
            if f.name == name {
                let nv = f.named_value(cgs).ok()?;
                let sem = nv.string_semantics;
                return Some(match nv.field_type {
                    FieldType::String => string_semantics_gloss_label(sem),
                    FieldType::Blob => "blob".to_string(),
                    _ => field_type_to_gloss_label(&nv.field_type),
                });
            }
        }
    }
    for e in full_entities {
        if let Some(ent) = cgs.get_entity(e) {
            if let Some(f) = ent.fields.get(name) {
                let nv = f.named_value(cgs).ok()?;
                let sem = nv.string_semantics;
                return Some(match nv.field_type {
                    FieldType::String => string_semantics_gloss_label(sem),
                    FieldType::Blob => "blob".to_string(),
                    _ => field_type_to_gloss_label(&nv.field_type),
                });
            }
        }
    }
    for e in full_entities {
        if let Some(ent) = cgs.get_entity(e) {
            if let Some(r) = ent.relations.get(name) {
                let target = r.target_resource.as_str();
                let hint = match map {
                    Some(m) => format!(
                        "=> {}",
                        m.entity_sym_for(cgs.entry_id.as_deref().unwrap_or(""), target)
                    ),
                    None => format!("=> {}", target),
                };
                return Some(hint);
            }
        }
    }
    None
}

/// Per-ident short type labels for inline `p#  ;;  …` gloss (parallel to [`build_ident_gloss_map`] descriptions).
#[allow(dead_code)]
pub(crate) fn build_ident_type_map(
    cgs: &CGS,
    full_entities: &[&str],
    map: Option<&SymbolMap>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for name in collect_ident_names(cgs, full_entities) {
        if let Some(t) = resolve_ident_type_string(cgs, full_entities, name.as_str(), map) {
            out.insert(name, t);
        }
    }
    out
}

/// One `e#` row in the teaching table teaching table (entity seeds / federation).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ExposedEntitySymbolRow {
    pub symbol: String,
    pub entry_id: String,
    pub entity: String,
}

/// One `r#` row for relation navigation (MCP `_meta.plasm` / HTTP symbols).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ExposedRelationSymbolRow {
    pub symbol: String,
    pub wire: String,
    pub entry_id: String,
    pub entity: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub target_entity: String,
    /// Executable hop when both endpoints have `e#` symbols (`e1.r2`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub plasm_expr: String,
}

/// Bidirectional maps for one prompt/eval slice.
#[derive(Debug, Clone)]
pub struct SymbolMap {
    pub(crate) tables: SymbolTables,
    pub(crate) values: SymbolValueLayer,
    /// Memoized when every assigned `e#` shares one non-empty registry row.
    sole_registry_entry_id: Option<String>,
}

fn compute_sole_registry_entry_id(tables: &SymbolTables) -> Option<String> {
    let mut ids: Vec<&str> = tables
        .sym_to_entity_binding
        .values()
        .map(|b| b.entry_id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    match ids.as_slice() {
        [one] if !one.is_empty() => Some(one.to_string()),
        _ => None,
    }
}

fn is_unset_single_graph_session(tables: &SymbolTables) -> bool {
    let mut any = false;
    for b in tables.sym_to_entity_binding.values() {
        any = true;
        if !b.entry_id.as_str().is_empty() {
            return false;
        }
    }
    any
}

#[inline]
fn opaque_v_symbol_display_index(sym: OpaqueVSym) -> u32 {
    sym.index().0.saturating_add(1)
}

/// Highest numeric suffix among opaque `vN` tokens seen in [`SymbolMap`] value-domain layer.
fn max_opaque_v_symbol_index(map: &SymbolMap) -> u32 {
    map.values
        .value_sym_to_fp
        .keys()
        .map(|sym| opaque_v_symbol_display_index(*sym))
        .max()
        .unwrap_or(0)
}

/// Next unused `vN` after [`SymbolMap`] plus optional extra tokens (e.g. pending field gloss rows).
pub(crate) fn next_opaque_v_symbol_after_map_and_extra_syms<'a>(
    map: &SymbolMap,
    extra: impl Iterator<Item = &'a str>,
) -> String {
    let mut max_n = max_opaque_v_symbol_index(map);
    for s in extra {
        if let Some(vsym) = OpaqueVSym::parse(s) {
            max_n = max_n.max(opaque_v_symbol_display_index(vsym));
        }
    }
    let n = max_n.saturating_add(1);
    format!("v{n}")
}

impl SymbolMap {
    /// Stable `(entry_id, entity)` → `e#` assignments for HTTP `/symbols` and terminals.
    pub fn exposed_entity_symbol_rows(&self) -> Vec<ExposedEntitySymbolRow> {
        self.tables
            .qualified_entity_to_sym
            .iter()
            .map(|(key, sym)| ExposedEntitySymbolRow {
                symbol: sym.as_wire(),
                entry_id: key.entry_id.to_string(),
                entity: key.entity.to_string(),
            })
            .collect()
    }

    /// Stable `(entry_id, entity, relation wire)` → `r#` assignments for MCP `_meta.plasm`.
    pub fn exposed_relation_symbol_rows(&self) -> Vec<ExposedRelationSymbolRow> {
        self.exposed_relation_symbol_rows_with_catalogs(None)
    }

    /// Like [`Self::exposed_relation_symbol_rows`] with optional per-catalog relation targets filled in.
    pub fn exposed_relation_symbol_rows_with_catalogs(
        &self,
        catalogs: Option<&IndexMap<String, Arc<CGS>>>,
    ) -> Vec<ExposedRelationSymbolRow> {
        let mut rows: Vec<ExposedRelationSymbolRow> = self
            .tables
            .relation_to_sym
            .iter()
            .map(|(key, sym)| {
                let entry_id = key.entry_id.to_string();
                let entity = key.entity.to_string();
                let wire = key.relation.to_string();
                let mut target_entity = String::new();
                if let Some(catalog_map) = catalogs {
                    if let Some(cgs) = catalog_map.get(&entry_id) {
                        if let Some(ent) = cgs.entities.get(entity.as_str()) {
                            if let Some(rel) = ent.relations.get(wire.as_str()) {
                                target_entity = rel.target_resource.to_string();
                            }
                        }
                    }
                }
                let sym_wire = sym.as_wire();
                let plasm_expr = self
                    .tables
                    .qualified_entity_to_sym
                    .get(&QualifiedEntityKey::new(&entry_id, &entity))
                    .map(|es| format!("{es}.{sym_wire}"))
                    .unwrap_or_default();
                ExposedRelationSymbolRow {
                    symbol: sym_wire,
                    wire,
                    entry_id,
                    entity,
                    target_entity,
                    plasm_expr,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            (&a.entry_id, &a.entity, &a.wire).cmp(&(&b.entry_id, &b.entity, &b.wire))
        });
        rows
    }

    /// If `token` is a session `e#` symbol (e.g. `e1` from the teaching table table), return the canonical entity name.
    #[inline]
    pub fn resolve_session_entity_symbol(&self, token: &str) -> Option<String> {
        let sym = OpaqueESym::parse(token)?;
        self.tables
            .sym_to_entity_binding
            .get(&sym)
            .map(|b| b.entity.to_string())
    }

    /// Owning registry `entry_id` for an opaque `e#` token in this session.
    #[inline]
    pub fn entry_id_for_entity_symbol(&self, sym: &str) -> Option<String> {
        OpaqueESym::parse(sym)
            .and_then(|s| self.tables.entry_id_for_entity_sym(s))
            .map(|s| s.to_string())
    }

    /// When every assigned `e#` shares one registry row, return that `entry_id` for parse-layer alignment.
    #[inline]
    pub fn sole_registry_entry_id(&self) -> Option<&str> {
        self.sole_registry_entry_id.as_deref()
    }

    /// True when every exposed entity row uses the unset forward-map key (`""`).
    #[inline]
    pub fn is_unset_single_graph_session(&self) -> bool {
        is_unset_single_graph_session(&self.tables)
    }

    /// Build maps for all entities in `full_entities` (slice order defines `e1`, `e2`, …).
    ///
    /// This is a thin wrapper around [`TeachingExposureSession::new`] + the session’s shared [`SymbolMap`]:
    /// one code path for `m#` / `p#` assignment and dotted-call alias metadata (execute / REPL / canonical teaching table).
    /// Uniquely owns the memoized map when no other `Arc` handles remain (avoids a full map clone on the hot path).
    pub fn build(cgs: &CGS, full_entities: &[&str]) -> Self {
        let cid = crate::catalog_id::cgs_symbol_map_entry_key(cgs);
        let arc = TeachingExposureSession::new(cgs, cid, full_entities).to_symbol_map();
        Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
    }

    /// Structured teaching table token for one exposed `(registry entry_id, entity)` pair.
    #[inline]
    pub fn try_entity_teaching_term_for(
        &self,
        catalog_entry_id: &str,
        canonical: &str,
    ) -> Option<TeachingTerm> {
        let sym = self
            .tables
            .qualified_entity_to_sym
            .get(&QualifiedEntityKey::new(catalog_entry_id, canonical))?;
        Some(TeachingTerm::Entity(
            EntityRef {
                name: EntityName::new(canonical),
            },
            sym.index(),
        ))
    }

    /// Structured teaching table token when `canonical` is exposed under **exactly one** catalog row.
    ///
    /// Federated sessions with colliding wire names (e.g. `github/Issue` + `linear/Issue`) return
    /// `None` — use [`Self::try_entity_teaching_term_for`] with the owning `entry_id`.
    #[inline]
    pub fn try_entity_teaching_term(&self, canonical: &str) -> Option<TeachingTerm> {
        let mut matches: Vec<_> = self
            .tables
            .qualified_entity_to_sym
            .iter()
            .filter(|(key, _)| key.entity.as_str() == canonical)
            .collect();
        if matches.len() != 1 {
            return None;
        }
        let (key, sym) = matches.pop().expect("len 1");
        Some(TeachingTerm::Entity(
            EntityRef {
                name: EntityName::new(key.entity.as_str()),
            },
            sym.index(),
        ))
    }

    /// Method token + CGS [`MethodRef`]; requires `cgs` to attach capability identity.
    #[inline]
    pub fn try_method_teaching_term(
        &self,
        cgs: &CGS,
        entity: &str,
        capability: &str,
    ) -> Option<TeachingTerm> {
        let entry_key = cgs.entry_id.as_deref().unwrap_or("");
        let key = MethodKey::new(entry_key, entity, capability);
        let sym = self.tables.method_to_sym.get(&key).copied().or_else(|| {
            self.tables.method_to_sym.iter().find_map(|(k, s)| {
                (k.domain.as_str() == entity
                    && k.capability.as_str() == capability
                    && (k.entry_id.as_str().is_empty() || k.entry_id.as_str() == entry_key))
                    .then_some(*s)
            })
        })?;
        let mref = method_ref_for_capability(cgs, entity, capability)?;
        Some(TeachingTerm::Method(mref, sym.index()))
    }

    /// Parameter token + [`ParameterSlot`]; `full_entities` must match the slice used to build this map.
    #[inline]
    pub fn try_ident_teaching_term(
        &self,
        cgs: &CGS,
        full_entities: &[&str],
        name: &str,
    ) -> Option<TeachingTerm> {
        let slot = resolve_parameter_slot(cgs, full_entities, name)?;
        let entry_key = cgs.entry_id.as_deref().unwrap_or("");
        match &slot {
            ParameterSlot::EntityField { entity, field } => {
                let sym = self.tables.entity_field_to_sym.get(&EntityFieldKey::new(
                    entry_key,
                    entity.as_str(),
                    field.as_str(),
                ))?;
                Some(TeachingTerm::Parameter(slot, sym.index()))
            }
            ParameterSlot::Relation { entity, name: rel } => {
                let sym = self.tables.relation_to_sym.get(&RelationKey::new(
                    entry_key,
                    entity.as_str(),
                    rel.as_str(),
                ))?;
                Some(TeachingTerm::Parameter(slot, sym.index()))
            }
            ParameterSlot::CapabilityInput {
                domain,
                capability,
                param,
            } => {
                let sym = self.tables.cap_param_to_sym.get(&CapParamKey::new(
                    entry_key,
                    domain.as_str(),
                    capability.as_str(),
                    param.as_str(),
                ))?;
                Some(TeachingTerm::Parameter(slot, sym.index()))
            }
        }
    }

    pub fn entity_sym_for_scope(&self, catalog: CatalogScope<'_>, canonical: &str) -> String {
        match catalog.entry_id() {
            None => self
                .try_entity_teaching_term(canonical)
                .map(|t| t.to_string())
                .unwrap_or_else(|| canonical.to_string()),
            Some(entry_id) => self
                .try_entity_teaching_term_for(entry_id, canonical)
                .map(|t| t.to_string())
                .unwrap_or_else(|| canonical.to_string()),
        }
    }

    /// Opaque `e#` for one exposed `(registry entry_id, entity)` pair.
    #[inline]
    pub fn entity_sym_for(&self, catalog_entry_id: &str, canonical: &str) -> String {
        self.entity_sym_for_scope(
            CatalogScope::from_forward_map_key(catalog_entry_id),
            canonical,
        )
    }

    /// Federated homonym hints: `(entry_id, e#)` rows whose wire entity name equals `wire`.
    #[must_use]
    pub fn entity_stamps_for_wire(&self, wire: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .tables
            .qualified_entity_to_sym
            .iter()
            .filter(|(key, _)| key.entity.as_str() == wire)
            .map(|(key, sym)| (key.entry_id.to_string(), sym.as_wire()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        out
    }

    /// Opaque `p#` for an **entity field** on a qualified entity row.
    #[inline]
    pub fn ident_sym_entity_field_for(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        field: &str,
    ) -> String {
        self.tables
            .entity_field_to_sym
            .get(&EntityFieldKey::new(catalog_entry_id, entity, field))
            .map(|sym| sym.as_wire())
            .unwrap_or_else(|| field.to_string())
    }

    /// Assigned entity-field `p#`, if exposed on the ledger.
    #[inline]
    pub fn lookup_entity_field_sym(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        field: &str,
    ) -> Option<OpaquePSym> {
        self.tables
            .entity_field_to_sym
            .get(&EntityFieldKey::new(catalog_entry_id, entity, field))
            .copied()
    }

    /// Opaque `r#` for a **relation** on a qualified entity row.
    #[inline]
    pub fn ident_sym_relation_for(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        relation: &str,
    ) -> String {
        self.tables
            .relation_to_sym
            .get(&RelationKey::new(catalog_entry_id, entity, relation))
            .map(|sym| sym.as_wire())
            .unwrap_or_else(|| relation.to_string())
    }

    /// Opaque `p#` for a **capability input** on a qualified catalog row.
    #[inline]
    pub fn ident_sym_cap_param_for(
        &self,
        catalog_entry_id: &str,
        domain_entity: &str,
        capability: &str,
        param: &str,
    ) -> String {
        self.tables
            .cap_param_to_sym
            .get(&CapParamKey::new(
                catalog_entry_id,
                domain_entity,
                capability,
                param,
            ))
            .map(|sym| sym.as_wire())
            .unwrap_or_else(|| param.to_string())
    }

    /// Resolve wire label → opaque symbol only when unambiguous across entity fields, relations, and cap params.
    pub fn ident_sym_unambiguous(&self, name: &str) -> Option<String> {
        let mut resolved: Option<String> = None;
        let mut note = |wire: String| -> Option<()> {
            match &resolved {
                None => {
                    resolved = Some(wire);
                    Some(())
                }
                Some(prev) if prev == &wire => Some(()),
                Some(_) => None,
            }
        };
        for (key, sym) in &self.tables.entity_field_to_sym {
            if key.field.as_str() == name && note(sym.as_wire()).is_none() {
                return None;
            }
        }
        for (key, sym) in &self.tables.relation_to_sym {
            if key.relation.as_str() == name && note(sym.as_wire()).is_none() {
                return None;
            }
        }
        for (key, sym) in &self.tables.cap_param_to_sym {
            if key.param.as_str() == name && note(sym.as_wire()).is_none() {
                return None;
            }
        }
        resolved
    }

    /// True when `sym` is an opaque session `p#` token (digits only after `p`).
    #[inline]
    pub fn is_opaque_p_sym(sym: &str) -> bool {
        OpaquePSym::is_token(sym)
    }

    /// True when `sym` is an opaque session `m#` token (digits only after `m`).
    #[inline]
    pub fn is_opaque_m_sym(sym: &str) -> bool {
        OpaqueMSym::is_token(sym)
    }

    /// True when `sym` is an opaque session `e#` token (digits only after `e`).
    #[inline]
    pub fn is_opaque_e_sym(sym: &str) -> bool {
        OpaqueESym::is_token(sym)
    }

    /// True when `sym` is an opaque session `r#` token (digits only after `r`).
    #[inline]
    pub fn is_opaque_r_sym(sym: &str) -> bool {
        OpaqueRSym::is_token(sym)
    }

    /// Wire name for an opaque session `p#` when bound in reverse maps.
    pub fn wire_for_opaque_p_sym(&self, sym: &str) -> Option<String> {
        let binding = self.resolve_session_slot(sym).ok()?;
        match &binding.kind {
            SlotKind::EntityField { field_wire, .. } => Some(field_wire.to_string()),
            SlotKind::CapParam { param_wire, .. } => Some(param_wire.to_string()),
        }
    }

    /// Rewrite canonical names into opaque tokens for LLM-facing recovery (entity names only).
    pub fn collapse_tokens_for_feedback(&self, input: &str) -> String {
        let mut keys: Vec<String> = self
            .tables
            .qualified_entity_to_sym
            .keys()
            .map(|k| k.entity.to_string())
            .collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        keys.dedup();
        let mut s = scan_replace(input, &keys, |k| {
            self.try_entity_teaching_term(k)
                .map(|t| t.to_string())
                .unwrap_or_else(|| k.to_string())
        });
        let mut idents: Vec<String> = self
            .tables
            .entity_field_to_sym
            .keys()
            .map(|k| k.field.to_string())
            .chain(
                self.tables
                    .relation_to_sym
                    .keys()
                    .map(|k| k.relation.to_string()),
            )
            .chain(
                self.tables
                    .cap_param_to_sym
                    .keys()
                    .map(|k| k.param.to_string()),
            )
            .collect();
        idents.sort_by_key(|k| std::cmp::Reverse(k.len()));
        idents.dedup();
        s = scan_replace(&s, &idents, |id| {
            self.ident_sym_unambiguous(id)
                .unwrap_or_else(|| id.to_string())
        });
        s
    }

    /// Value-domain forward table (`v#` gloss layer).
    #[inline]
    pub fn value_domain_fp_to_sym(&self) -> &IndexMap<String, OpaqueVSym> {
        &self.values.value_domain_fp_to_sym
    }

    /// Opaque `p#` tokens taught for a capability's input parameters (deterministic error hints).
    pub fn cap_param_syms_for(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
    ) -> Vec<String> {
        self.cap_param_syms_from_forward_table(catalog_entry_id, domain, capability)
    }

    /// `(taught: p1, p2, …)` suffix for capability invoke arg errors; empty when none taught.
    pub fn cap_param_syms_hint(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
    ) -> String {
        let taught = self.cap_param_syms_for(catalog_entry_id, domain, capability);
        if taught.is_empty() {
            String::new()
        } else {
            format!(" (taught: {})", taught.join(", "))
        }
    }

    /// Resolve `r#` → declared relation wire. Returns `None` if `sym` is not a session relation token.
    pub fn resolve_relation_ident<'a>(&'a self, sym: &str) -> Option<&'a str> {
        let rsym = OpaqueRSym::parse(sym)?;
        self.tables
            .sym_to_relation_binding
            .get(&rsym)
            .map(|b| b.relation_wire.as_str())
    }

    /// True when `sym` is a session `r#` relation token.
    #[inline]
    pub fn is_relation_symbol(&self, sym: &str) -> bool {
        OpaqueRSym::parse(sym).is_some_and(|s| self.tables.sym_to_relation_binding.contains_key(&s))
    }

    /// teaching table term for one relation `r#` on a qualified entity row.
    pub fn try_relation_teaching_term_for(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        relation: &str,
    ) -> Option<TeachingTerm> {
        let sym = self.tables.relation_to_sym.get(&RelationKey::new(
            catalog_entry_id,
            entity,
            relation,
        ))?;
        Some(TeachingTerm::Parameter(
            ParameterSlot::Relation {
                entity: EntityName::from(entity.to_string()),
                name: RelationName::from(relation),
            },
            sym.index(),
        ))
    }

    /// Registry-backed `p#` → shared `v#` value-domain symbol, when one exists.
    #[inline]
    pub fn value_sym_for_p_sym(&self, p_sym: &str) -> Option<String> {
        let psym = OpaquePSym::parse(p_sym)?;
        self.values
            .p_sym_to_value_sym
            .get(&psym)
            .map(|vs| vs.as_wire())
    }

    /// Pre-rendered teaching gloss for a `v#` row (after `;;`), if known.
    #[inline]
    pub fn value_domain_gloss_for_v_sym(&self, v_sym: &str) -> Option<&str> {
        let vsym = OpaqueVSym::parse(v_sym)?;
        self.values.value_sym_gloss.get(&vsym).map(|s| s.as_str())
    }

    /// Reverse lookup: `v#` → `(catalog_entry_id|vr:value_ref)` fingerprint.
    #[inline]
    pub fn value_domain_fp_for_v_sym(&self, v_sym: &str) -> Option<&str> {
        let vsym = OpaqueVSym::parse(v_sym)?;
        self.values.value_sym_to_fp.get(&vsym).map(|s| s.as_str())
    }

    /// All capability input bindings for one taught `p#` (homographs may yield multiple).
    pub fn capability_param_quads_for_p_sym(
        &self,
        sym: &str,
    ) -> Vec<(String, EntityName, CapabilityName, String)> {
        let Some(psym) = OpaquePSym::parse(sym) else {
            return Vec::new();
        };
        self.tables
            .cap_param_to_sym
            .iter()
            .filter(|(_, s)| **s == psym)
            .map(|(key, _)| {
                (
                    key.entry_id.to_string(),
                    key.domain.clone(),
                    key.capability.clone(),
                    key.param.to_string(),
                )
            })
            .collect()
    }

    /// Scoped cap-param quad when gloss context knows catalog + domain entity.
    ///
    /// Required for homographed `p#` symbols shared across capabilities on the same entity.
    pub fn capability_param_quad_for_p_sym_on_entity(
        &self,
        sym: &str,
        catalog_entry_id: &str,
        domain_entity: &str,
    ) -> Option<(String, EntityName, CapabilityName, String)> {
        self.capability_param_quads_for_p_sym(sym)
            .into_iter()
            .find(|(entry_id, domain, _, _)| {
                entry_id.as_str() == catalog_entry_id && domain.as_str() == domain_entity
            })
    }

    /// If `sym` maps a capability input parameter, return
    /// `(catalog entry id, domain entity, capability name, full param path)`.
    ///
    /// **Homograph note:** value-domain fingerprinting may bind one `p#` to multiple capabilities.
    /// This returns the first match only — use [`Self::capability_param_quad_for_p_sym_on_entity`]
    /// or [`Self::capability_param_quads_for_p_sym`] when gloss context is entity- or cap-scoped.
    pub fn capability_param_quad_for_p_sym(
        &self,
        sym: &str,
    ) -> Option<(String, EntityName, CapabilityName, String)> {
        self.capability_param_quads_for_p_sym(sym).into_iter().next()
    }

    /// If `sym` maps a capability input parameter, return the `(capability domain entity, param path)`.
    ///
    /// `param path` is the full dotted path for nested union fields (e.g. `operations.insert_before.blocks`).
    pub fn capability_param_key_for_p_sym(&self, sym: &str) -> Option<(EntityName, String)> {
        self.capability_param_quad_for_p_sym(sym)
            .map(|(_, dom, _, path)| (dom, path))
    }

    /// Opaque `m#` for one capability row — wire name lookup, then path-segment fallback.
    #[inline]
    pub fn method_sym_for_cap(
        &self,
        catalog_entry_id: &str,
        cap: &crate::CapabilitySchema,
    ) -> String {
        let wire = cap.name.as_str();
        let sym = self.method_sym_for(catalog_entry_id, cap.domain.as_str(), wire);
        if sym != wire {
            return sym;
        }
        let kebab = crate::schema::capability_method_label_kebab(cap);
        if kebab != wire {
            let sym = self.method_sym_for(catalog_entry_id, cap.domain.as_str(), kebab.as_str());
            if sym != kebab {
                return sym;
            }
        }
        kebab
    }

    /// Opaque `m#` for one `(registry entry_id, domain entity, capability wire name)` triple.
    ///
    /// `capability` may be the full wire name (`issue_create`) or the path method segment (`create`)
    /// when that segment is unique for the domain in this session.
    #[inline]
    pub fn method_sym_for(&self, catalog_entry_id: &str, entity: &str, capability: &str) -> String {
        let key = MethodKey::new(catalog_entry_id, entity, capability);
        if let Some(sym) = self.tables.method_to_sym.get(&key) {
            return sym.as_wire();
        }
        let segment_key = MethodSegmentKey {
            entry_id: RegistryEntryId::from(catalog_entry_id),
            domain: EntityName::from(entity),
            segment: PathMethodSegment::from(capability),
        };
        if let Some(sym) = self.tables.method_segment_to_sym.get(&segment_key) {
            return sym.as_wire();
        }
        capability.to_string()
    }

    /// If `label` is an opaque method token `m#`, return the capability wire name for parse.
    #[inline]
    pub fn resolve_method_symbol_token(&self, label: &str) -> Option<&str> {
        self.resolve_method_symbol_triple(label)
            .map(|(_, _, cap)| cap)
    }

    /// `m#` → `(registry entry_id, domain entity name, capability wire name)`.
    #[inline]
    pub fn resolve_method_symbol_triple(&self, label: &str) -> Option<(&str, &str, &str)> {
        let msym = OpaqueMSym::parse(label)?;
        self.tables.sym_to_method.get(&msym).map(|b| {
            (
                b.entry_id.as_str(),
                b.domain.as_str(),
                b.capability.as_str(),
            )
        })
    }

    /// `m#` → `(domain entity name, capability wire name)`.
    #[inline]
    pub fn resolve_method_symbol_pair(&self, label: &str) -> Option<(&str, &str)> {
        self.resolve_method_symbol_triple(label)
            .map(|(_, d, cap)| (d, cap))
    }

    /// `[scope …]` fragment for teaching table `;;` legends only (no `optional params:` list).
    /// For [`CapabilityKind::Query`], returns empty (scope is not shown for query-style capabilities).
    pub(crate) fn capability_scope_legend_gloss(
        &self,
        cgs: &CGS,
        cap: &CapabilitySchema,
    ) -> String {
        const MAX_SIG: usize = 96;
        let Some(is) = &cap.input_schema else {
            return String::new();
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            return String::new();
        };
        if cap.kind == CapabilityKind::Query {
            return String::new();
        }
        let entry_id = cgs.entry_id.as_deref().unwrap_or("");
        let mut scope_parts: Vec<String> = Vec::new();
        let domain = cap.domain.as_str();
        let cap_name = cap.name.as_str();
        for f in fields {
            if !matches!(f.role, Some(ParameterRole::Scope)) {
                continue;
            }
            let Ok(nv) = f.named_value(cgs) else {
                continue;
            };
            if let FieldType::EntityRef { target } = &nv.field_type {
                let ps = self.ident_sym_cap_param_for(entry_id, domain, cap_name, f.name.as_str());
                let es = self.entity_sym_for(entry_id, target.as_str());
                scope_parts.push(format!("{ps}→{es}"));
            } else {
                scope_parts.push(self.ident_sym_cap_param_for(
                    entry_id,
                    domain,
                    cap_name,
                    f.name.as_str(),
                ));
            }
        }
        if scope_parts.is_empty() {
            return String::new();
        }
        let s = format!("[scope {}]", scope_parts.join(", "));
        crate::utf8_trunc::truncate_utf8_owned_with_ellipsis(s, MAX_SIG)
    }

    /// Optional / scope parameter symbols for teaching table `;;` legends. Required parameters are omitted — they
    /// are already shown in the example expression. For [`CapabilityKind::Query`], omits `[scope …]`.
    /// Required invoke slots are defined by preceding `p#` gloss rows; this gloss is **optionality only**.
    pub(crate) fn capability_input_signature_gloss(
        &self,
        cgs: &CGS,
        cap: &CapabilitySchema,
    ) -> String {
        const MAX_SIG: usize = 96;
        if cap.input_schema.is_none() {
            return String::new();
        };
        let mut scope_s = self.capability_scope_legend_gloss(cgs, cap);
        let entry_id = cgs.entry_id.as_deref().unwrap_or("");
        let domain = cap.domain.as_str();
        let has_optional =
            !capability_optional_legend_param_pairs(self, entry_id, domain, cap).is_empty();
        if has_optional {
            if !scope_s.is_empty() {
                scope_s.push(' ');
            }
            let _ = write!(&mut scope_s, "optional params: optional");
        }
        if scope_s.is_empty() {
            return scope_s;
        }
        crate::utf8_trunc::truncate_utf8_owned_with_ellipsis(scope_s, MAX_SIG)
    }

    /// Reserved for future SYMBOL MAP content; **FIELDS** moved inline into **teaching table** (see [`build_ident_gloss_map`]).
    pub fn format_legend(&self, _cgs: &CGS) -> String {
        String::new()
    }

    /// Human-readable gloss for a field token `p#` (same rules as the former **FIELDS** block).
    ///
    /// When `ident_types` is set, emits `type · description` (type from CGS; description from [`build_ident_gloss_map`]).
    /// Relations use `=> e# · …` (target entity symbol), not `relation→…`.
    #[allow(dead_code)]
    pub fn field_gloss_display(
        &self,
        sym: &str,
        ident_gloss: &HashMap<String, String>,
        ident_types: Option<&HashMap<String, String>>,
    ) -> String {
        const MAX_DESC: usize = 100;
        let name = OpaquePSym::parse(sym)
            .and_then(|ps| self.tables.sym_to_slot.get(&ps))
            .and_then(|b| b.entity_field().map(|(_, w)| w.to_string()))
            .unwrap_or_else(|| sym.to_string());
        let desc = ident_gloss
            .get(name.as_str())
            .map(|d| d.as_str().trim())
            .filter(|d| !d.is_empty())
            .map(|d| truncate_desc(d, MAX_DESC))
            .unwrap_or_else(|| name.clone())
            .replace('\t', " ");
        let ty = ident_types
            .and_then(|m| m.get(name.as_str()))
            .map(|s| s.as_str().trim())
            .filter(|t| !t.is_empty());
        match ty {
            Some(t) => format!("{t} · {desc}"),
            None => desc,
        }
    }
}

/// Merge entity field, relation, and capability-parameter descriptions (first wins per name).
#[allow(dead_code)]
pub fn build_ident_gloss_map(cgs: &CGS) -> HashMap<String, String> {
    let mut ident_gloss: HashMap<String, String> = HashMap::new();
    for e in cgs.entities.values() {
        for (fname, f) in &e.fields {
            if !f.description.is_empty() {
                ident_gloss
                    .entry(fname.as_str().to_string())
                    .or_insert_with(|| f.description.clone());
            }
        }
    }
    for e in cgs.entities.values() {
        for r in e.relations.values() {
            if !r.description.is_empty() {
                ident_gloss
                    .entry(r.name.as_str().to_string())
                    .or_insert_with(|| r.description.clone());
            }
        }
    }
    for cap in cgs.capabilities.values() {
        let Some(is) = &cap.input_schema else {
            continue;
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            continue;
        };
        for f in fields {
            if let Some(d) = &f.description {
                if !d.is_empty() {
                    ident_gloss
                        .entry(f.name.clone())
                        .or_insert_with(|| d.clone());
                }
            }
        }
    }
    ident_gloss
}

/// Left-to-right `p#` tokens in an expression fragment (after stripping prompt annotations).
pub fn field_syms_in_expr(expr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < expr.len() {
        let b = *expr.as_bytes().get(i).unwrap_or(&0);
        if (b == b'p' || b == b'r') && ident_boundary_left(expr, i) {
            let mut end = i + 1;
            while end < expr.len() {
                let c = expr[end..].chars().next().unwrap();
                if c.is_ascii_digit() {
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            if end > i + 1 {
                let next = expr[end..].chars().next();
                if next.is_none() || !ident_continue(next.unwrap()) {
                    out.push(expr[i..end].to_string());
                    i = end;
                    continue;
                }
            }
        }
        i += expr[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    out
}

/// `p#` tokens for inline gloss: expression first (left-to-right), then optional legend fragments
/// (`result_gloss`, then `cap_legend`) so optional-only params in capability legends still get gloss rows.
pub fn field_syms_for_teaching_row(
    expr: &str,
    result_gloss: Option<&str>,
    cap_legend: Option<&str>,
    extra_syms: &[String],
) -> Vec<String> {
    let expr_clean = strip_prompt_expression_annotations(expr);
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for sym in field_syms_in_expr(&expr_clean) {
        if seen.insert(sym.clone()) {
            ordered.push(sym);
        }
    }
    for frag in [result_gloss, cap_legend].into_iter().flatten() {
        let t = frag.trim();
        if t.is_empty() {
            continue;
        }
        for sym in field_syms_in_expr(t) {
            if seen.insert(sym.clone()) {
                ordered.push(sym);
            }
        }
    }
    for sym in extra_syms {
        if seen.insert(sym.clone()) {
            ordered.push(sym.clone());
        }
    }
    ordered
}

/// Byte scan: `t` ends with `)` — find the `(` that balances the **outermost** trailing `)`.
fn matching_open_paren_for_trailing_close(t: &str) -> Option<usize> {
    if !t.ends_with(')') {
        return None;
    }
    let bytes = t.as_bytes();
    let mut depth = 0i32;
    let mut i = t.len();
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Trailing `( … )` blocks that are example laundry-lists, not tight semantics (e.g. `(DDoS L7, …, etc.)`).
fn trailing_paren_inner_is_agent_noise(inner: &str) -> bool {
    let t = inner.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if lower.contains("etc.") || lower.contains("e.g.") {
        return true;
    }
    if t.matches(',').count() >= 2 {
        return true;
    }
    t.len() > 55
}

fn strip_trailing_noise_parentheticals(mut s: &str) -> &str {
    loop {
        let mut t = s.trim_end();
        // Allow authored `(...).` — peel `.` so the balancing scan sees final `)`.
        t = t.strip_suffix('.').unwrap_or(t).trim_end();
        let Some(open) = matching_open_paren_for_trailing_close(t) else {
            break;
        };
        let inner = t[open + 1..t.len() - 1].trim();
        if !trailing_paren_inner_is_agent_noise(inner) {
            break;
        }
        let before = t[..open].trim_end();
        if before.is_empty() {
            break;
        }
        s = before;
    }
    s.trim_end()
}

/// Normalize authored `description:` prose for compact agent gloss: trim edges, drop trailing
/// parenthetical example lists, then strip a terminal ASCII full stop.
pub(crate) fn trim_description_for_agent_gloss(s: &str) -> &str {
    let t = s.trim();
    let t = strip_trailing_noise_parentheticals(t);
    match t.strip_suffix('.') {
        Some(rest) => rest.trim_end(),
        None => t,
    }
}

fn truncate_desc(s: &str, max: usize) -> String {
    let t = trim_description_for_agent_gloss(s);
    crate::utf8_trunc::truncate_utf8_bytes_with_ellipsis(t, max)
}

/// Same truncation cap as [`IdentMetadata::render_gloss`] trailing prose (teaching table / TSV parity).
pub(crate) fn gloss_description_truncated(s: &str) -> String {
    truncate_desc(s, 100)
}

fn scan_replace(
    input: &str,
    syms_sorted_long_first: &[String],
    canon: impl Fn(&str) -> String,
) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    let mut in_string = false;
    let mut escape = false;

    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        let ch_len = ch.len_utf8();
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += ch_len;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += ch_len;
            continue;
        }
        let mut replaced = false;
        if ident_boundary_left(input, i) {
            for sym in syms_sorted_long_first {
                if input[i..].starts_with(sym) {
                    let after = i + sym.len();
                    let boundary_ok = after >= input.len()
                        || !ident_continue(input[after..].chars().next().unwrap());
                    if boundary_ok {
                        out.push_str(&canon(sym));
                        i = after;
                        replaced = true;
                        break;
                    }
                }
            }
        }
        if !replaced {
            out.push(ch);
            i += ch_len;
        }
    }
    out
}

/// Rewrite opaque `letter+digits` tokens (e.g. `p12`, `v3`) using [`scan_replace`] boundary rules
/// (respects quoted spans). Keys are matched **longest-first** so `p12` is not split by `p1`.
pub(crate) fn rewrite_opaque_ident_tokens(
    input: &str,
    replacements: &HashMap<String, String>,
) -> String {
    if replacements.is_empty() {
        return input.to_string();
    }
    let mut syms: Vec<String> = replacements.keys().cloned().collect();
    syms.sort_by_key(|k| std::cmp::Reverse(k.len()));
    scan_replace(input, &syms, |sym| {
        replacements
            .get(sym)
            .cloned()
            .unwrap_or_else(|| sym.to_string())
    })
}

fn ident_boundary_left(s: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = s[..i].chars().next_back().unwrap();
    !ident_continue(prev)
}

fn ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Build a [`TeachingExposureSession`] from REPL/eval [`FocusSpec`], using the **same** `e#`/`m#`/`p#`
/// rules as HTTP/MCP execute: **sorted** entity names, **no** 2-hop neighbourhood expansion.
///
/// This keeps REPL and execute symbol indices aligned when the same seed set is used (`Single(s)`
/// ≡ one seed, `Seeds` ≡ sorted list). Use multiple seeds or incremental exposure if you need more
/// entities in teaching table.
///
/// The session’s `catalog_entry_id` argument is taken from [`CGS::entry_id`] when set (packed plugins,
/// registry rows) so [`ExposureSurface`] keys and [`TeachingExposureSession::catalog_cgs`] agree — using
/// `""` when the graph id is unset (YAML fixtures).
pub fn teaching_exposure_session_from_focus(
    cgs: &CGS,
    focus: FocusSpec<'_>,
) -> TeachingExposureSession {
    // Registry row id for this graph: align with `CGS::entry_id` (packed plugins use the API dir name)
    // so `ExposureSurface` keys and `catalog_cgs` lookups stay consistent.
    let catalog_key = crate::catalog_id::cgs_symbol_map_entry_key(cgs);
    match focus {
        FocusSpec::All => {
            let mut names: Vec<&str> = cgs
                .entities
                .iter()
                .filter(|(_, ent)| !ent.abstract_entity)
                .map(|(n, _)| n.as_str())
                .collect();
            names.sort();
            TeachingExposureSession::new(cgs, catalog_key, &names)
        }
        FocusSpec::Single(s) => TeachingExposureSession::new(cgs, catalog_key, &[s]),
        FocusSpec::Seeds(seeds) => {
            if seeds.is_empty() {
                return teaching_exposure_session_from_focus(cgs, FocusSpec::All);
            }
            let mut v: Vec<&str> = seeds.to_vec();
            v.sort();
            v.dedup();
            TeachingExposureSession::new(cgs, catalog_key, &v)
        }
        FocusSpec::SeedsExact(seeds) => {
            if seeds.is_empty() {
                return teaching_exposure_session_from_focus(cgs, FocusSpec::All);
            }
            let mut v: Vec<&str> = seeds.to_vec();
            let mut seen = std::collections::HashSet::new();
            v.retain(|s| seen.insert(*s));
            TeachingExposureSession::new(cgs, catalog_key, &v)
        }
    }
}

/// When `symbol_tuning` is true (same as [`crate::prompt_render::RenderConfig::uses_symbols`]: **compact** or **tsv** [`crate::prompt_render::PromptRenderMode`]), build the map used for prompts and pre-parse expansion.
pub fn symbol_map_for_prompt(
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> Option<Arc<SymbolMap>> {
    if !symbol_tuning {
        return None;
    }
    Some(teaching_exposure_session_from_focus(cgs, focus).symbol_map_arc())
}

/// Owned entity names for prompt surface metrics and teaching table line counts, plus optional
/// [`TeachingExposureSession`] when `symbol_tuning` is true (execute-parity slice; mirrors symbolic render modes); otherwise names from
/// [`entity_slices_for_render`] (2-hop for `Single` / `Seeds` when not exact).
pub fn resolve_prompt_surface_entities(
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> (Vec<String>, Option<TeachingExposureSession>) {
    if symbol_tuning {
        let exp = teaching_exposure_session_from_focus(cgs, focus);
        let names = exp.entities.clone();
        (names, Some(exp))
    } else {
        let (full, _) = entity_slices_for_render(cgs, focus);
        let names = full.iter().map(|s| (*s).to_string()).collect();
        (names, None)
    }
}

/// Monotonic `e#` / `m#` / `p#` assignment as an execute/MCP session exposes more entity names from
/// the CGS graph. Indices only **append** — existing symbols never change when new domains appear.
#[derive(Debug)]
pub struct TeachingExposureSession {
    /// Cumulative allowed teaching surface for filtered (`intent`) sessions; full closure for legacy paths.
    pub surface: ExposureSurface,
    /// Entities included in symbol space (order = `e1`, `e2`, …).
    pub entities: Vec<String>,
    /// Catalog registry `entry_id` for each row in [`Self::entities`] (same length). Disambiguates
    /// which [`crate::CgsContext`] owns the CGS entity when multiple catalogs are federated.
    pub entity_catalog_entry_ids: Vec<String>,
    /// Owning [`CGS`] per catalog `entry_id` (same keys as [`Self::entity_catalog_entry_ids`] values).
    catalog_cgs: IndexMap<String, Arc<CGS>>,
    pub(crate) tables: SymbolTables,
    pub(crate) ledger: SymbolLedger,
    /// `(catalog_entry_id, entity)` → wire name → slot metadata (rebuilt after each slot assignment wave).
    ident_meta_by_entity: HashMap<(String, EntityName), HashMap<String, IdentMetadata>>,
}

impl Clone for TeachingExposureSession {
    fn clone(&self) -> Self {
        Self {
            surface: self.surface.clone(),
            entities: self.entities.clone(),
            entity_catalog_entry_ids: self.entity_catalog_entry_ids.clone(),
            catalog_cgs: self.catalog_cgs.clone(),
            tables: self.tables.clone(),
            ledger: self.ledger.clone(),
            ident_meta_by_entity: self.ident_meta_by_entity.clone(),
        }
    }
}

#[inline]
pub(crate) fn slot_meta_is_relation(meta: &IdentMetadata) -> bool {
    matches!(meta, IdentMetadata::Relation { .. })
}

impl TeachingExposureSession {
    /// First wave: assign symbols for `entity_names_in_order` (typically sorted seeds from the client).
    /// `catalog_entry_id` is the registry row for this graph (`""` when not using a multi-entry catalog).
    pub fn new(cgs: &CGS, catalog_entry_id: &str, entity_names_in_order: &[&str]) -> Self {
        let mut s = Self {
            surface: ExposureSurface::default(),
            entities: Vec::new(),
            entity_catalog_entry_ids: Vec::new(),
            catalog_cgs: IndexMap::new(),
            tables: SymbolTables::default(),
            ledger: SymbolLedger::default(),
            ident_meta_by_entity: HashMap::new(),
        };
        let arc = Arc::new(cgs.clone());
        s.expose_entities(&[cgs], arc, catalog_entry_id, entity_names_in_order);
        s
    }

    pub fn new_with_intent_delta(
        cgs: &CGS,
        catalog_entry_id: &str,
        entity_names_in_order: &[&str],
        delta: ExposureSurfaceDelta,
    ) -> Self {
        let mut s = Self {
            surface: ExposureSurface::default(),
            entities: Vec::new(),
            entity_catalog_entry_ids: Vec::new(),
            catalog_cgs: IndexMap::new(),
            tables: SymbolTables::default(),
            ledger: SymbolLedger::default(),
            ident_meta_by_entity: HashMap::new(),
        };
        let arc = Arc::new(cgs.clone());
        let _ = s.expose_surface(&[cgs], arc, catalog_entry_id, entity_names_in_order, delta);
        s
    }

    /// Expose more entity names (e.g. next hop in the graph). Skips unknown or duplicate names.
    /// `cgs_layers` must include every [`CGS`] that contributes to this session (federated: all catalogs).
    /// `catalog_entry_id` identifies which catalog row these `names` belong to.
    pub fn expose_entities(
        &mut self,
        cgs_layers: &[&CGS],
        owning_cgs: Arc<CGS>,
        catalog_entry_id: &str,
        names: &[&str],
    ) {
        if cgs_layers.is_empty() {
            return;
        }
        self.catalog_cgs
            .insert(catalog_entry_id.to_string(), owning_cgs.clone());
        self.ledger.clear_symbol_map_cache();
        for n in names {
            let qkey = QualifiedEntityKey::new(catalog_entry_id, *n);
            if self.tables.qualified_entity_to_sym.contains_key(&qkey) {
                continue;
            }
            if owning_cgs.get_entity(n).is_none() {
                continue;
            }
            // Explicit seed list (`entity_names_in_order`): assign `e#` even when `abstract: true`.
            // Auto-neighbourhood / full-catalog slices still omit abstract entities elsewhere.
            let sym = OpaqueESym::from_zero_based(self.entities.len() as u32);
            self.entities.push((*n).to_string());
            self.entity_catalog_entry_ids
                .push(catalog_entry_id.to_string());
            self.tables.qualified_entity_to_sym.insert(qkey, sym);
            self.record_entity_binding(sym, catalog_entry_id, n);
        }
        self.union_legacy_surface_for_entities(catalog_entry_id, names);
        self.assign_new_methods_and_idents(cgs_layers);
    }

    fn union_legacy_surface_for_entities(&mut self, entry_id: &str, entities: &[&str]) {
        let Some(cgs) = self.catalog_cgs.get(entry_id) else {
            return;
        };
        legacy_exposure_surface_for_entities(cgs.as_ref(), entry_id, entities, &mut self.surface);
    }

    pub fn expose_surface(
        &mut self,
        cgs_layers: &[&CGS],
        owning_cgs: Arc<CGS>,
        catalog_entry_id: &str,
        entity_names_in_order: &[&str],
        delta: ExposureSurfaceDelta,
    ) -> ExposureAppendReport {
        if cgs_layers.is_empty() {
            return ExposureAppendReport::default();
        }
        self.catalog_cgs
            .insert(catalog_entry_id.to_string(), owning_cgs.clone());
        self.ledger.clear_symbol_map_cache();
        self.surface.merge_from(&delta.required);
        let mut entities_added = 0usize;
        for n in entity_names_in_order {
            let ekey = ExposureEntityKey {
                entry_id: catalog_entry_id.to_string(),
                entity: EntityName::from(*n),
            };
            if !self.surface.entities.contains(&ekey) {
                continue;
            }
            let qkey = QualifiedEntityKey::new(catalog_entry_id, *n);
            if self.tables.qualified_entity_to_sym.contains_key(&qkey) {
                continue;
            }
            let Some(_ent) = owning_cgs.get_entity(n) else {
                continue;
            };
            // Delta wave: same explicit-seed policy as [`Self::expose_entities`].
            entities_added += 1;
            let sym = OpaqueESym::from_zero_based(self.entities.len() as u32);
            self.entities.push((*n).to_string());
            self.entity_catalog_entry_ids
                .push(catalog_entry_id.to_string());
            self.tables.qualified_entity_to_sym.insert(qkey, sym);
            self.record_entity_binding(sym, catalog_entry_id, n);
        }
        self.assign_new_methods_and_idents(cgs_layers);
        ExposureAppendReport { entities_added }
    }

    fn named_value_row_description(&self, meta: &IdentMetadata) -> String {
        let IdentMetadata::RegistryBacked {
            catalog_entry_id,
            value_registry_key,
            ..
        } = meta
        else {
            return String::new();
        };
        let Some(cgs) = self.catalog_cgs.get(catalog_entry_id) else {
            return String::new();
        };
        cgs.values
            .get(value_registry_key.as_str())
            .map(|nv| nv.description.trim().to_string())
            .unwrap_or_default()
    }

    fn build_symbol_map_snapshot(&self) -> SymbolMap {
        let tables = self.tables.clone();
        let values = SymbolValueLayer::build_from_ledger(
            &self.ledger,
            |meta| self.named_value_row_description(meta),
            |meta, nv_desc, cgs_opt| {
                let partial = SymbolMap {
                    tables: tables.clone(),
                    values: SymbolValueLayer::default(),
                    sole_registry_entry_id: compute_sole_registry_entry_id(&tables),
                };
                meta.render_value_domain_row_gloss(nv_desc, Some(&partial), cgs_opt)
            },
            &self.catalog_cgs,
        );
        let sole_registry_entry_id = compute_sole_registry_entry_id(&tables);
        SymbolMap {
            tables,
            values,
            sole_registry_entry_id,
        }
    }

    /// [`IdentMetadata`] for `full_entities`, aligned with this session’s slot table (avoids a second CGS walk).
    pub(crate) fn ident_metadata_for_exposure_entities(
        &self,
        full_entities: &[&str],
    ) -> HashMap<IdentMetaKey, IdentMetadata> {
        let set: HashSet<&str> = full_entities.iter().copied().collect();
        let mut out = HashMap::new();
        for ((entry_id, entity), by_wire) in &self.ident_meta_by_entity {
            if !set.contains(entity.as_str()) {
                continue;
            }
            for meta in by_wire.values() {
                let k = (
                    entry_id.clone(),
                    entity.clone(),
                    meta.wire_name().to_string(),
                );
                out.entry(k).or_insert_with(|| meta.clone());
            }
        }
        out
    }

    /// Shared [`SymbolMap`] for this exposure session (memoized until the next [`Self::expose_entities`]).
    pub fn symbol_map_arc(&self) -> Arc<SymbolMap> {
        self.symbol_map_arc_cross(None, None).0
    }

    /// Relation `r#` rows for MCP `_meta.plasm` with target entities from [`Self::catalog_cgs`].
    pub fn exposed_relation_symbol_rows(&self) -> Vec<ExposedRelationSymbolRow> {
        self.symbol_map_arc()
            .exposed_relation_symbol_rows_with_catalogs(Some(&self.catalog_cgs))
    }

    /// Like [`Self::symbol_map_arc`], plus an optional process-wide LRU keyed by [`SymbolMapCacheKey`]
    /// (same schema + exposure rows → reuse the snapshot across HTTP/MCP sessions).
    ///
    /// Second return is `Some(true)` / `Some(false)` when the cross-request LRU was consulted
    /// (cache hit vs miss); `None` when this call used session-local memo or built without cross cache.
    pub fn symbol_map_arc_cross(
        &self,
        cross: Option<&SymbolMapCrossRequestCache>,
        key: Option<SymbolMapCacheKey>,
    ) -> (Arc<SymbolMap>, Option<bool>) {
        let exposure_fp = hash_exposure_session_rows(self);
        {
            let r = self
                .ledger
                .symbol_map_cache
                .read()
                .expect("symbol_map_cache lock poisoned");
            if let Some((fp, arc)) = r.as_ref() {
                if *fp == exposure_fp {
                    return (Arc::clone(arc), None);
                }
            }
        }
        let (built, lru_hit) = if let (Some(cache), Some(k)) = (cross, key) {
            if cache.is_enabled() {
                let (arc, hit) =
                    cache.get_or_insert_tracked(k, || self.build_symbol_map_snapshot());
                (arc, Some(hit))
            } else {
                (Arc::new(self.build_symbol_map_snapshot()), None)
            }
        } else {
            (Arc::new(self.build_symbol_map_snapshot()), None)
        };
        let mut w = self
            .ledger
            .symbol_map_cache
            .write()
            .expect("symbol_map_cache lock poisoned");
        *w = Some((exposure_fp, Arc::clone(&built)));
        (built, lru_hit)
    }

    /// Snapshot for wire-surface display — matches teaching lines for this session (same `Arc` as [`Self::symbol_map_arc`]).
    pub fn to_symbol_map(&self) -> Arc<SymbolMap> {
        self.symbol_map_arc()
    }

    /// All exposed rows as catalog-qualified keys (order matches `e1`, `e2`, …).
    pub fn all_qualified_entities(&self) -> Vec<ExposureEntityKey> {
        self.entities
            .iter()
            .zip(self.entity_catalog_entry_ids.iter())
            .map(|(entity, entry_id)| ExposureEntityKey {
                entry_id: entry_id.clone(),
                entity: EntityName::from(entity.as_str()),
            })
            .collect()
    }

    /// Qualified keys appended since `start_index` (wave delta detection).
    pub fn qualified_entities_since(&self, start_index: usize) -> Vec<ExposureEntityKey> {
        self.entities
            .iter()
            .zip(self.entity_catalog_entry_ids.iter())
            .skip(start_index)
            .map(|(entity, entry_id)| ExposureEntityKey {
                entry_id: entry_id.clone(),
                entity: EntityName::from(entity.as_str()),
            })
            .collect()
    }

    /// Relation navigation slots newly present in [`Self::surface`] vs a snapshot before an expand/federate wave.
    pub fn relation_slots_added_since(
        &self,
        before: &BTreeSet<ExposureSlotKey>,
    ) -> Vec<ExposureSlotKey> {
        self.surface
            .slots
            .iter()
            .filter(|slot| matches!(slot, ExposureSlotKey::Relation { .. }))
            .filter(|slot| !before.contains(*slot))
            .cloned()
            .collect()
    }

    /// Relation hops between exposed `endpoints` that are missing from the cumulative surface or lack
    /// assigned `r#` symbols (e.g. target entity qualified after the relation slot first appeared).
    pub fn pending_relation_slots_among(
        &self,
        endpoints: &[ExposureEntityKey],
    ) -> Vec<ExposureSlotKey> {
        use crate::schema::IncomingNavSlotKind;

        let map = self.symbol_map_arc();
        let qualified: BTreeSet<(String, String)> = endpoints
            .iter()
            .filter(|k| self.contains_qualified_entity(k.entry_id.as_str(), k.entity.as_str()))
            .map(|k| (k.entry_id.clone(), k.entity.to_string()))
            .collect();
        let mut out = BTreeSet::new();
        for target in endpoints {
            let target_key = (target.entry_id.clone(), target.entity.to_string());
            if !qualified.contains(&target_key) {
                continue;
            }
            let Some(cgs) = self.catalog_cgs_for_entry(target.entry_id.as_str()) else {
                continue;
            };
            for edge in cgs.incoming_nav_edges_to(target.entity.as_str()) {
                if !matches!(edge.kind, IncomingNavSlotKind::Relation) {
                    continue;
                }
                let source_key = ExposureEntityKey {
                    entry_id: target.entry_id.clone(),
                    entity: edge.source_entity.clone(),
                };
                let source_pair = (source_key.entry_id.clone(), source_key.entity.to_string());
                if !qualified.contains(&source_pair) {
                    continue;
                }
                let Some(src_ent) = cgs.get_entity(edge.source_entity.as_str()) else {
                    continue;
                };
                if !src_ent.relations.contains_key(edge.slot_name.as_str()) {
                    continue;
                }
                let relation = RelationName::new(edge.slot_name.clone());
                let sym = map.ident_sym_relation_for(
                    source_key.entry_id.as_str(),
                    source_key.entity.as_str(),
                    relation.as_str(),
                );
                let needs_sym = sym.is_empty() || !sym.starts_with('r');
                let slot = ExposureSlotKey::Relation {
                    source: source_key,
                    relation,
                };
                let needs_slot = !self.surface.slots.contains(&slot);
                if needs_slot || needs_sym {
                    out.insert(slot);
                }
            }
        }
        out.into_iter().collect()
    }

    /// Union of [`Self::relation_edge_delta_slots`] and [`Self::pending_relation_slots_among`].
    pub fn relation_slots_for_expand_wave(
        &self,
        slots_before: &BTreeSet<ExposureSlotKey>,
        added_qualified: &[ExposureEntityKey],
        relation_keys: &[ExposureEntityKey],
    ) -> Vec<ExposureSlotKey> {
        let mut out = self.relation_edge_delta_slots(slots_before, added_qualified);
        let mut seen: BTreeSet<ExposureSlotKey> = out.iter().cloned().collect();
        for slot in self.pending_relation_slots_among(relation_keys) {
            if seen.insert(slot.clone()) {
                out.push(slot);
            }
        }
        out
    }

    /// Relation hops to teach in an expand/federate delta: newly added slots plus parent→target edges
    /// unlocked when a new entity receives an `e#` symbol.
    pub fn relation_edge_delta_slots(
        &self,
        slots_before: &BTreeSet<ExposureSlotKey>,
        added_qualified: &[ExposureEntityKey],
    ) -> Vec<ExposureSlotKey> {
        use crate::schema::IncomingNavSlotKind;

        let mut out: BTreeSet<ExposureSlotKey> = self
            .relation_slots_added_since(slots_before)
            .into_iter()
            .collect();

        for target in added_qualified {
            let Some(cgs) = self.catalog_cgs_for_entry(target.entry_id.as_str()) else {
                continue;
            };
            for edge in cgs.incoming_nav_edges_to(target.entity.as_str()) {
                if !matches!(edge.kind, IncomingNavSlotKind::Relation) {
                    continue;
                }
                let source = ExposureEntityKey {
                    entry_id: target.entry_id.clone(),
                    entity: edge.source_entity.clone(),
                };
                if self
                    .qualified_entity_symbol(target.entry_id.as_str(), edge.source_entity.as_str())
                    .is_none()
                {
                    continue;
                }
                let Some(src_ent) = cgs.get_entity(edge.source_entity.as_str()) else {
                    continue;
                };
                if !src_ent.relations.contains_key(edge.slot_name.as_str()) {
                    continue;
                }
                let slot = ExposureSlotKey::Relation {
                    source,
                    relation: RelationName::new(edge.slot_name.clone()),
                };
                out.insert(slot);
            }
        }
        out.into_iter().collect()
    }

    /// `_meta.plasm.relations_delta` rows for relation slots unlocked this wave.
    pub fn relations_delta_rows_for_slots(
        &self,
        slots: &[ExposureSlotKey],
    ) -> Vec<ExposedRelationSymbolRow> {
        let map = self.symbol_map_arc();
        let mut rows = Vec::new();
        let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
        for slot in slots {
            let ExposureSlotKey::Relation { source, relation } = slot else {
                continue;
            };
            let key = (
                source.entry_id.clone(),
                source.entity.to_string(),
                relation.to_string(),
            );
            if !seen.insert(key) {
                continue;
            }
            let Some(cgs) = self.catalog_cgs_for_entry(source.entry_id.as_str()) else {
                continue;
            };
            let Some(ent) = cgs.get_entity(source.entity.as_str()) else {
                continue;
            };
            let Some(rel_schema) = ent.relations.get(relation.as_str()) else {
                continue;
            };
            let sym = map.ident_sym_relation_for(
                source.entry_id.as_str(),
                source.entity.as_str(),
                relation.as_str(),
            );
            if sym.is_empty() || !sym.starts_with('r') {
                continue;
            }
            let plasm_expr = self
                .qualified_entity_symbol(source.entry_id.as_str(), source.entity.as_str())
                .map(|es| format!("{es}.{sym}"))
                .unwrap_or_default();
            rows.push(ExposedRelationSymbolRow {
                symbol: sym,
                wire: relation.to_string(),
                entry_id: source.entry_id.clone(),
                entity: source.entity.to_string(),
                target_entity: rel_schema.target_resource.to_string(),
                plasm_expr,
            });
        }
        rows.sort_by(|a, b| {
            (&a.entry_id, &a.entity, &a.wire).cmp(&(&b.entry_id, &b.entity, &b.wire))
        });
        rows
    }

    /// Merge relation-hop slots into the cumulative surface and refresh `r#` symbols before edge-delta render.
    pub fn admit_relation_edge_slots_for_render(
        &mut self,
        cgs_layers: &[&CGS],
        slots: &[ExposureSlotKey],
    ) {
        if slots.is_empty() {
            return;
        }
        for slot in slots {
            if let ExposureSlotKey::Relation { .. } = slot {
                self.surface.slots.insert(slot.clone());
            }
        }
        self.ledger.clear_symbol_map_cache();
        self.assign_new_methods_and_idents(cgs_layers);
    }

    /// Whether `(entry_id, entity)` is already in this session's symbol space.
    pub fn contains_qualified_entity(&self, entry_id: &str, entity: &str) -> bool {
        self.tables
            .qualified_entity_to_sym
            .contains_key(&QualifiedEntityKey::new(entry_id, entity))
    }

    /// Loaded catalog graph for one registry row in this session.
    pub fn catalog_cgs_for_entry(&self, entry_id: &str) -> Option<&CGS> {
        self.catalog_cgs.get(entry_id).map(|arc| arc.as_ref())
    }

    /// Opaque `e#` wire for a qualified `(entry_id, entity)` when exposed.
    pub fn qualified_entity_symbol(&self, entry_id: &str, entity: &str) -> Option<String> {
        self.tables
            .qualified_entity_to_sym
            .get(&QualifiedEntityKey::new(entry_id, entity))
            .map(|sym| sym.as_wire())
    }

    /// Union of already-exposed qualified keys plus a new wave's `(entry_id, entity)` seeds.
    pub fn relation_endpoint_keys_for_wave(
        &self,
        batch_entry_id: &str,
        batch_names: &[String],
    ) -> Vec<ExposureEntityKey> {
        let mut keys = self.all_qualified_entities();
        let mut seen: BTreeSet<(String, String)> = keys
            .iter()
            .map(|k| (k.entry_id.clone(), k.entity.to_string()))
            .collect();
        for name in batch_names {
            let pair = (batch_entry_id.to_string(), name.clone());
            if seen.insert(pair.clone()) {
                keys.push(ExposureEntityKey {
                    entry_id: pair.0,
                    entity: EntityName::from(pair.1.as_str()),
                });
            }
        }
        keys
    }

    /// Owning `(catalog entry id, CGS entity name)` for an exposed **entity name** (aligned with
    /// `e#` / teaching rows). Prefer [`Self::qualified_entity_for_exposed_entity_pair`] when the
    /// catalog is known — bare names are ambiguous under federation.
    pub fn qualified_entity_for_exposed_entity(
        &self,
        entity_name: &str,
    ) -> Option<crate::QualifiedEntityKey> {
        let mut matches: Vec<_> = self
            .entities
            .iter()
            .zip(self.entity_catalog_entry_ids.iter())
            .filter(|(e, _)| e.as_str() == entity_name)
            .collect();
        if matches.len() != 1 {
            return None;
        }
        let (_, id) = matches.pop().expect("len 1");
        Some(crate::QualifiedEntityKey::new(
            id.clone(),
            entity_name.to_string(),
        ))
    }

    /// Owning catalog for one exposed row when both `entry_id` and entity name are known.
    pub fn qualified_entity_for_exposed_entity_pair(
        &self,
        entry_id: &str,
        entity_name: &str,
    ) -> Option<crate::QualifiedEntityKey> {
        self.tables
            .qualified_entity_to_sym
            .contains_key(&QualifiedEntityKey::new(entry_id, entity_name))
            .then(|| crate::QualifiedEntityKey::new(entry_id.to_string(), entity_name.to_string()))
    }

    /// Registry `entry_id` for an exposed **entity name** (aligned with `e#` / teaching table table order).
    ///
    /// In federated sessions, each exposed row is tied to one loaded catalog; this is the
    /// authoritative owning id for that symbol row. Returns `None` if `entity` is not in
    /// [`Self::entities`].
    #[deprecated(
        note = "use qualified_entity_for_exposed_entity — catalog ownership is (entry_id, entity)"
    )]
    pub fn catalog_entry_id_for_entity(&self, entity: &str) -> Option<&str> {
        self.entities
            .iter()
            .zip(self.entity_catalog_entry_ids.iter())
            .find(|(e, _)| e.as_str() == entity)
            .map(|(_, id)| id.as_str())
    }
}

fn hash_exposure_session_rows(exposure: &TeachingExposureSession) -> u64 {
    opaque_symbol_hash::hash_exposure_session_rows(exposure)
}

/// Fingerprint for [`SymbolMapCrossRequestCache`]: pinned catalogs + exposed entity rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SymbolMapCacheKey {
    catalogs_fingerprint: u64,
    exposure_fingerprint: u64,
}

/// Cache key for a single-catalog session (`entry_id` + [`CGS::catalog_cgs_hash_hex`] + exposure rows).
pub fn symbol_map_cache_key_single_catalog(
    cgs: &CGS,
    exposure: &TeachingExposureSession,
) -> SymbolMapCacheKey {
    let mut ch = DefaultHasher::new();
    cgs.entry_id.as_deref().unwrap_or("").hash(&mut ch);
    cgs.catalog_cgs_hash_hex().hash(&mut ch);
    SymbolMapCacheKey {
        catalogs_fingerprint: ch.finish(),
        exposure_fingerprint: hash_exposure_session_rows(exposure),
    }
}

/// Cache key when expression parse spans multiple [`CGS`] layers (federation).
pub fn symbol_map_cache_key_federated(
    layers: &[&CGS],
    exposure: &TeachingExposureSession,
) -> SymbolMapCacheKey {
    let mut parts: Vec<String> = layers
        .iter()
        .map(|c| {
            format!(
                "{}:{}",
                c.entry_id.as_deref().unwrap_or(""),
                c.catalog_cgs_hash_hex()
            )
        })
        .collect();
    parts.sort();
    let mut ch = DefaultHasher::new();
    for p in &parts {
        p.hash(&mut ch);
    }
    SymbolMapCacheKey {
        catalogs_fingerprint: ch.finish(),
        exposure_fingerprint: hash_exposure_session_rows(exposure),
    }
}

/// Cross-request LRU of [`SymbolMap`] snapshots (bounded; disabled when capacity is `0`).
pub struct SymbolMapCrossRequestCache {
    cap: usize,
    inner: RwLock<IndexMap<SymbolMapCacheKey, Arc<SymbolMap>>>,
}

impl std::fmt::Debug for SymbolMapCrossRequestCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolMapCrossRequestCache")
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

impl SymbolMapCrossRequestCache {
    pub const ENV_CAP: &'static str = "PLASM_SYMBOL_MAP_LRU_CAP";

    pub fn new(capacity: usize) -> Self {
        Self {
            cap: capacity,
            inner: RwLock::new(IndexMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let cap = std::env::var(Self::ENV_CAP)
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(64);
        Self::new(cap)
    }

    pub fn is_enabled(&self) -> bool {
        self.cap > 0
    }

    /// Remove all cached [`SymbolMap`] snapshots (e.g. after replacing API catalog plugins on disk).
    pub fn clear(&self) {
        let mut map = self
            .inner
            .write()
            .expect("SymbolMapCrossRequestCache lock poisoned");
        map.clear();
    }

    pub fn get_or_insert(
        &self,
        key: SymbolMapCacheKey,
        build: impl FnOnce() -> SymbolMap,
    ) -> Arc<SymbolMap> {
        self.get_or_insert_tracked(key, build).0
    }

    /// Returns `(snapshot, cache_hit)` where `cache_hit` is true iff an existing LRU entry was reused.
    pub fn get_or_insert_tracked(
        &self,
        key: SymbolMapCacheKey,
        build: impl FnOnce() -> SymbolMap,
    ) -> (Arc<SymbolMap>, bool) {
        if !self.is_enabled() {
            return (Arc::new(build()), false);
        }
        let mut map = self
            .inner
            .write()
            .expect("SymbolMapCrossRequestCache lock poisoned");
        if let Some(arc) = map.shift_remove(&key) {
            map.insert(key, Arc::clone(&arc));
            return (arc, true);
        }
        let arc = Arc::new(build());
        while map.len() >= self.cap {
            let Some(k) = map.keys().next().cloned() else {
                break;
            };
            map.shift_remove(&k);
        }
        map.insert(key, Arc::clone(&arc));
        (arc, false)
    }
}

/// Wire surface for teaching-session display paths (parse opaque + AST render).
/// **Never** call before [`crate::expr_parser::parse`] or program ingress.
pub fn wire_surface_for_teaching_session(
    input: &str,
    session: &TeachingExposureSession,
    symbol_tuning: bool,
) -> String {
    let input = input.trim();
    if !symbol_tuning {
        return input.to_string();
    }
    crate::expr_surface_render::wire_surface_from_teaching_session_line(input, session)
        .unwrap_or_else(|| strip_prompt_expression_annotations(input))
}

/// Strip human-only suffixes from pasted prompt examples (`;;` comment may include `=>` result type,
/// legacy `=>` before `;;`, `->` relation target hint).
///
/// This is only for prompt-render/eval diagnostics that inspect historical teaching rows. The
/// expression/program parser path must consume the documented Plasm surface directly.
pub fn strip_prompt_expression_annotations(input: &str) -> String {
    let trimmed = input.trim();
    // Expression is always before the first `;;` (result type now lives inside the comment).
    let no_cap = trimmed.split("  ;;  ").next().unwrap_or(trimmed).trim();
    // Legacy lines: `expr  =>  [e#]  ;;  …`
    let no_gloss = no_cap
        .rsplit_once("  =>  ")
        .map(|(a, _)| a.trim())
        .unwrap_or(no_cap);
    let expr_only = no_gloss
        .split_once(" -> ")
        .map(|(a, _)| a.trim())
        .unwrap_or(no_gloss);
    expr_only.to_string()
}

/// Rebuild-or-skip wire surface for interactive display paths.
pub fn wire_surface_for_parse(
    input: &str,
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> String {
    let input = input.trim();
    if !symbol_tuning {
        return input.to_string();
    }
    let exposure = teaching_exposure_session_from_focus(cgs, focus);
    wire_surface_for_teaching_session(input, &exposure, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::Expr;
    use crate::loader::load_schema_dir;
    use crate::schema::{
        CapabilityMapping, CapabilitySchema, FieldSchema, FieldValueKind, NamedValueSchema,
        ResourceSchema, ValueDomainKey,
    };
    use crate::CapabilityKind;

    #[test]
    fn opaque_dotted_call_on_get_parses_without_string_expansion() {
        let dir = std::path::Path::new("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let session = TeachingExposureSession::new(&cgs, "proof", &["Document"]);
        let map = session.symbol_map_arc();
        let stack = [crate::CgsLayer::new("proof", &cgs)];
        let e_sym = session
            .tables
            .sym_to_entity_binding
            .iter()
            .find(|(_, b)| b.entity.as_str() == "Document")
            .map(|(k, _)| k.as_wire())
            .expect("Document e#");
        let m_sym = session
            .tables
            .sym_to_method
            .iter()
            .find(|(_, b)| b.capability.as_str() == "annotation_suggestion_insert")
            .map(|(k, _)| k.as_wire())
            .expect("annotation insert m#");
        let slug_sym = session
            .tables
            .entity_field_to_sym
            .iter()
            .find(|(key, _)| key.entity.as_str() == "Document" && key.field.as_str() == "slug")
            .map(|(_, v)| v.as_wire())
            .expect("slug p#");
        let agent_sym = session
            .tables
            .cap_param_to_sym
            .iter()
            .find(|(key, _)| {
                key.domain.as_str() == "Document"
                    && key.capability.as_str() == "annotation_suggestion_insert"
                    && key.param.as_str() == "agent_id"
            })
            .map(|(_, v)| v.as_wire())
            .expect("agent_id p#");
        let opaque = format!(
            "{e}({slug}=\"acme\").{m}({agent}=\"bot\")",
            e = e_sym,
            slug = slug_sym,
            m = m_sym,
            agent = agent_sym,
        );
        let cap = cgs
            .get_capability("annotation_suggestion_insert")
            .expect("annotation_suggestion_insert");
        let _label = crate::capability_method_label_kebab(cap);
        let opaque_parsed = crate::expr_parser::parse_with_cgs_layers(&opaque, &stack, map.clone())
            .expect("opaque surface parses in-grammar");
        let Expr::Invoke(opaque_inv) = &opaque_parsed.expr else {
            panic!("expected Invoke, got {:?}", opaque_parsed.expr);
        };
        assert_eq!(
            opaque_inv.capability.as_str(),
            "annotation_suggestion_insert"
        );
        assert_eq!(opaque_inv.catalog_entry_id.as_deref(), Some("proof"));
    }

    #[test]
    fn rewrite_opaque_ident_tokens_prefers_longest_symbol_match() {
        let mut m = HashMap::new();
        m.insert("p1".into(), "pz".into());
        m.insert("p12".into(), "px".into());
        assert_eq!(rewrite_opaque_ident_tokens("p12+p1+p123", &m), "px+pz+p123");
        let mut v = HashMap::new();
        v.insert("v10".into(), "va".into());
        v.insert("v1".into(), "vb".into());
        assert_eq!(rewrite_opaque_ident_tokens("v10.v1", &v), "va.vb");
    }

    #[test]
    fn slot_allocation_fingerprint_splits_same_wire_different_field_types() {
        let en = EntityName::from("N".to_string());
        let meta = |entity: EntityName, ft: FieldType, vr: &str| IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity,
            role: IdentRegistryRole::EntityField,
            value_registry_key: ValueDomainKey::new(vr).expect("key"),
            field_type: ft,
            string_semantics: None,
            array_items: None,
            allowed_values: None,
            wire_name: "id".into(),
            description: "same desc".into(),
        };
        assert_ne!(
            slot_allocation_fingerprint(&meta(en.clone(), FieldType::Integer, "fp_slot_int")),
            slot_allocation_fingerprint(&meta(en.clone(), FieldType::String, "fp_slot_str")),
        );
        let a = meta(
            EntityName::from("Alpha".to_string()),
            FieldType::Integer,
            "fp_slot_alpha",
        );
        let b = meta(
            EntityName::from("Beta".to_string()),
            FieldType::Integer,
            "fp_slot_beta",
        );
        assert_ne!(
            slot_allocation_fingerprint(&a),
            slot_allocation_fingerprint(&b),
            "full slot fingerprints stay entity-scoped for diagnostics"
        );
    }

    #[test]
    fn slot_symbol_allocation_fingerprint_merges_same_values_row_and_wire_across_entities() {
        let shared_vr = "nv_shared_zone_id_test";
        let meta_ef = |entity: &str| IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from(entity.to_string()),
            role: IdentRegistryRole::EntityField,
            value_registry_key: ValueDomainKey::new(shared_vr).expect("key"),
            field_type: FieldType::String,
            string_semantics: Some(StringSemantics::Short),
            array_items: None,
            allowed_values: None,
            wire_name: "zone_id".into(),
            description: String::new(),
        };
        let meta_cap = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Ruleset".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: "ruleset_query".into(),
            },
            value_registry_key: ValueDomainKey::new(shared_vr).expect("key"),
            field_type: FieldType::String,
            string_semantics: Some(StringSemantics::Short),
            array_items: None,
            allowed_values: None,
            wire_name: "zone_id".into(),
            description: String::new(),
        };
        assert_eq!(
            slot_symbol_allocation_fingerprint(&meta_ef("Zone")),
            slot_symbol_allocation_fingerprint(&meta_ef("Ruleset")),
        );
        assert_eq!(
            slot_symbol_allocation_fingerprint(&meta_ef("Zone")),
            slot_symbol_allocation_fingerprint(&meta_cap),
        );
        let other_vr = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Zone".to_string()),
            role: IdentRegistryRole::EntityField,
            value_registry_key: ValueDomainKey::new("nv_other_zone_id_test").expect("key"),
            field_type: FieldType::String,
            string_semantics: Some(StringSemantics::Short),
            array_items: None,
            allowed_values: None,
            wire_name: "zone_id".into(),
            description: String::new(),
        };
        assert_ne!(
            slot_symbol_allocation_fingerprint(&meta_ef("Zone")),
            slot_symbol_allocation_fingerprint(&other_vr),
            "distinct values: rows must not share a p# even with the same wire name"
        );
    }

    #[test]
    fn slot_symbol_allocation_fingerprint_merges_union_variant_params_with_same_leaf() {
        let vr = "nv_merge_leaf_cap_test";
        let mk = |path: &str| IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Document".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: "document_edit_v2".into(),
            },
            value_registry_key: ValueDomainKey::new(vr).expect("key"),
            field_type: FieldType::String,
            string_semantics: Some(StringSemantics::Short),
            array_items: None,
            allowed_values: None,
            wire_name: path.into(),
            description: String::new(),
        };
        assert_eq!(
            slot_symbol_allocation_fingerprint(&mk("operations.replace_block.ref")),
            slot_symbol_allocation_fingerprint(&mk("operations.insert_before.ref")),
            "union-variant full paths differ but leaf + capability match"
        );
        assert_ne!(
            slot_symbol_allocation_fingerprint(&mk("operations.replace_block.ref")),
            slot_symbol_allocation_fingerprint(&mk("operations.replace_block.markdown")),
            "distinct leaves under the same capability stay split"
        );
    }

    #[test]
    fn overshow_entity_scoped_slot_maps_split_incompatible_id_slots() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let capture_item_id = map.ident_sym_entity_field_for("", "CaptureItem", "id");
        let profile_id = map.ident_sym_entity_field_for("", "Profile", "id");
        let pipeline_snapshot_id = map.ident_sym_entity_field_for("", "PipelineSnapshot", "id");
        assert_ne!(
            capture_item_id, profile_id,
            "same-shaped `id` fields on different entities must not share one p#"
        );
        assert_ne!(
            capture_item_id, pipeline_snapshot_id,
            "entity-scoped lookup must not fall back to the wrong legacy bare-name `id` symbol"
        );
    }

    #[test]
    fn overshow_unambiguous_ident_lookup_rejects_ambiguous_id_name() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        assert_eq!(
            map.ident_sym_entity_field_for("", "PipelineSnapshot", "workers"),
            map.ident_sym_entity_field_for("", "PipelineSnapshot", "workers")
        );
        assert_eq!(
            map.wire_for_opaque_p_sym("id"),
            None,
            "bare `id` should not collapse to a single p# when both int and str id slots exist"
        );
    }

    #[test]
    fn teaching_exposure_session_keeps_entity_symbols_stable_across_waves() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let mut s = TeachingExposureSession::new(&cgs, "", &["Pet"]);
        let pet_sym = s.to_symbol_map().entity_sym_for("", "Pet");
        s.expose_entities(&[&cgs], Arc::new(cgs.clone()), "", &["Store"]);
        assert_eq!(pet_sym, s.to_symbol_map().entity_sym_for("", "Pet"));
        assert_ne!(pet_sym, s.to_symbol_map().entity_sym_for("", "Store"));
    }

    /// `m#` / `p#` append-only invariants: adding a second entity must not renumber existing method
    /// or field slot symbols for the first entity.
    #[test]
    fn teaching_exposure_session_keeps_method_and_field_symbols_stable_across_waves() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let mut s = TeachingExposureSession::new(&cgs, "", &["Profile"]);
        let map0 = s.to_symbol_map();
        let display_p = map0.ident_sym_entity_field_for("", "Profile", "display_name");
        let get_m = map0.method_sym_for("", "Profile", "profile_get");
        s.expose_entities(&[&cgs], Arc::new(cgs.clone()), "", &["RecordedContent"]);
        let map1 = s.to_symbol_map();
        assert_eq!(
            map1.ident_sym_entity_field_for("", "Profile", "display_name"),
            display_p
        );
        assert_eq!(map1.method_sym_for("", "Profile", "profile_get"), get_m);
    }

    #[test]
    fn ident_metadata_from_exposure_matches_build_ident_metadata() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let sesh = TeachingExposureSession::new(&cgs, "", &["Pet", "Store"]);
        let full_refs: Vec<&str> = sesh.entities.iter().map(|s| s.as_str()).collect();
        let from_exp = sesh.ident_metadata_for_exposure_entities(&full_refs);
        let mut from_build = HashMap::new();
        for &e in &full_refs {
            from_build.extend(build_ident_metadata(&cgs, &[e]));
        }
        assert_eq!(from_exp, from_build);
    }

    #[test]
    fn symbol_map_cross_request_cache_reuses_snapshot() {
        let cache = SymbolMapCrossRequestCache::new(8);
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let exp = TeachingExposureSession::new(&cgs, "", &["Pet"]);
        let key = symbol_map_cache_key_single_catalog(&cgs, &exp);
        let (a, h1) = exp.symbol_map_arc_cross(Some(&cache), Some(key));
        assert_eq!(h1, Some(false));
        let exp2 = TeachingExposureSession::new(&cgs, "", &["Pet"]);
        let (b, h2) = exp2.symbol_map_arc_cross(Some(&cache), Some(key));
        assert_eq!(h2, Some(true));
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn symbol_map_cross_request_cache_clear_drops_lru_entries() {
        let cache = SymbolMapCrossRequestCache::new(8);
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let exp = TeachingExposureSession::new(&cgs, "", &["Pet"]);
        let key = symbol_map_cache_key_single_catalog(&cgs, &exp);
        let (_, h1) = exp.symbol_map_arc_cross(Some(&cache), Some(key));
        assert_eq!(h1, Some(false));
        cache.clear();
        let exp2 = TeachingExposureSession::new(&cgs, "", &["Pet"]);
        let (_, h2) = exp2.symbol_map_arc_cross(Some(&cache), Some(key));
        assert_eq!(h2, Some(false));
    }

    #[test]
    fn symbol_map_session_local_cache_rejects_stale_snapshot_after_federated_extend() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(dir).expect("matrix"));
        let mut contexts = indexmap::IndexMap::new();
        contexts.insert(
            "linear".to_string(),
            Arc::new(crate::CgsContext::entry("linear", cgs.clone())),
        );
        contexts.insert(
            "github".to_string(),
            Arc::new(crate::CgsContext::entry("github", cgs.clone())),
        );
        let layers: Vec<&CGS> = contexts.values().map(|c| c.cgs.as_ref()).collect();
        let mut exp = TeachingExposureSession::new(
            cgs.as_ref(),
            "linear",
            &["LangItem", "LangLine", "LangTag"],
        );
        let cache = SymbolMapCrossRequestCache::new(8);
        let key_three = symbol_map_cache_key_federated(&layers, &exp);
        let (map_three, _) = exp.symbol_map_arc_cross(Some(&cache), Some(key_three));
        assert!(map_three.resolve_session_entity_symbol("e3").is_some());
        assert!(map_three.resolve_session_entity_symbol("e4").is_none());
        let fp_three = hash_exposure_session_rows(&exp);

        exp.expose_entities(&layers, cgs.clone(), "github", &["LangDetail"]);
        // Simulate a stale session-local memo (e.g. concurrent compile during extend).
        *exp.ledger
            .symbol_map_cache
            .write()
            .expect("symbol_map_cache lock poisoned") = Some((fp_three, Arc::clone(&map_three)));

        let key_four = symbol_map_cache_key_federated(&layers, &exp);
        let (map_four, _) = exp.symbol_map_arc_cross(Some(&cache), Some(key_four));
        assert!(
            map_four.resolve_session_entity_symbol("e4").is_some(),
            "federated extend must rebuild symbol map when exposure fingerprint advances"
        );
        assert_eq!(map_four.entity_sym_for("github", "LangDetail"), "e4");
    }

    /// Two exposures reaching the **same ordered entity rows + surface** via different wave
    /// structure (one open vs incremental `expose_entities`) may assign different opaque numbering.
    /// The cross-request [`SymbolMapCacheKey`] must encode that numbering: if it did not, the LRU
    /// would serve one session a `SymbolMap` whose `p#`→wire map disagrees with the teaching TSV it
    /// was shown (the `p21=title` rendered / `p21=height` resolved contamination).
    #[test]
    fn cache_key_distinguishes_wave_structure_numbering() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(dir).expect("matrix"));
        let mut contexts = indexmap::IndexMap::new();
        contexts.insert(
            "linear".to_string(),
            Arc::new(crate::CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = contexts.values().map(|c| c.cgs.as_ref()).collect();

        // Path A: one wave exposing both entities together.
        let exp_a = TeachingExposureSession::new(cgs.as_ref(), "linear", &["LangItem", "LangLine"]);
        // Path B: same entities, same final order, but built in two waves.
        let mut exp_b = TeachingExposureSession::new(cgs.as_ref(), "linear", &["LangItem"]);
        exp_b.expose_entities(&layers, cgs.clone(), "linear", &["LangLine"]);

        assert_eq!(
            exp_a.entities, exp_b.entities,
            "both exposures must reach the same ordered entity rows"
        );

        let key_a = symbol_map_cache_key_federated(&layers, &exp_a);
        let key_b = symbol_map_cache_key_federated(&layers, &exp_b);

        let map_a = exp_a.build_symbol_map_snapshot();
        let map_b = exp_b.build_symbol_map_snapshot();
        let numbering = |m: &SymbolMap| m.tables.clone();
        let same_numbering = numbering(&map_a) == numbering(&map_b);

        // The cache key must agree with the actual numbering — never coarser. A coarser key (the bug)
        // would leave `key_a == key_b` while `same_numbering == false`, letting the LRU cross-serve.
        assert_eq!(
            key_a == key_b,
            same_numbering,
            "SymbolMapCacheKey must encode opaque-symbol numbering so the cross-request LRU never \
             serves a differently-numbered SymbolMap under a colliding key"
        );
    }

    #[test]
    fn wire_surface_opaque_get_petstore() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let opaque = format!("{}(42)", map.entity_sym_for("", "Pet"));
        let wire = crate::expr_surface_render::wire_surface_from_teaching_line(
            &opaque,
            &cgs,
            std::sync::Arc::new(map),
        )
        .expect("wire");
        assert_eq!(wire, "Pet(42)");
    }

    #[test]
    fn wire_surface_opaque_method_petstore() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let pet = map.entity_sym_for("", "Pet");
        let cap_ref = crate::method_ref_for_domain_segment(&cgs, "Pet", "upload-image")
            .expect("upload-image capability on Pet");
        let m = map.method_sym_for("", "Pet", cap_ref.capability.as_str());
        if m == cap_ref.capability.as_str() {
            return;
        }
        let opaque = format!("{}(1).{}()", pet, m);
        let wire = crate::expr_surface_render::wire_surface_from_teaching_line(
            &opaque,
            &cgs,
            std::sync::Arc::new(map),
        )
        .expect("wire");
        assert!(wire.contains("upload-image"), "got {wire}");
        assert!(wire.starts_with("Pet(1)"), "got {wire}");
    }

    #[test]
    fn wire_surface_relation_owned_on_receiver() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let exp = TeachingExposureSession::new(&cgs, "pokeapi", &["Berry", "BerryFirmness"]);
        let map = exp.to_symbol_map();
        let berry = map.entity_sym_for("pokeapi", "Berry");
        let firmness = map.ident_sym_relation_for("pokeapi", "Berry", "firmness");
        if firmness == "firmness" {
            return;
        }
        let opaque = format!("{berry}(\"cheri\").{firmness}", firmness = firmness);
        let wire = crate::expr_surface_render::wire_surface_from_teaching_line(&opaque, &cgs, map)
            .expect("wire");
        assert!(wire.contains(".firmness"), "got {wire}");
    }

    #[test]
    fn field_syms_in_expr_order() {
        assert_eq!(
            field_syms_in_expr(r#"e1(42).m22(p37="x",p18=1)"#),
            vec!["p37".to_string(), "p18".to_string()]
        );
        assert_eq!(
            field_syms_in_expr("e4{p61=e1(42)}"),
            vec!["p61".to_string()]
        );
        assert_eq!(
            field_syms_in_expr("e1.r4[p5,p6]"),
            vec!["r4".to_string(), "p5".to_string(), "p6".to_string()]
        );
        assert!(field_syms_in_expr("e1(42)").is_empty());
    }

    #[test]
    fn field_syms_for_teaching_row_includes_optional_from_legend() {
        assert_eq!(
            field_syms_for_teaching_row(
                r#"e1(42).m22(p37=$,..)"#,
                None,
                Some(r#"optional params: optional — Create a goal"#),
                &["p18".to_string(), "p17".to_string()],
            ),
            vec!["p37".to_string(), "p18".to_string(), "p17".to_string(),]
        );
    }

    #[test]
    fn strip_prompt_annotations_result_inside_comment() {
        assert_eq!(
            strip_prompt_expression_annotations("e1  ;;  => [e1]  List all accessible workspaces"),
            "e1"
        );
        assert_eq!(
            strip_prompt_expression_annotations(
                "e6{p26=e5(42), p1=true}  ;;  => [e6]  optional params: p1 — List lists"
            ),
            "e6{p26=e5(42), p1=true}"
        );
    }

    #[test]
    fn strip_prompt_annotations_legacy_result_before_comment() {
        assert_eq!(
            strip_prompt_expression_annotations("e1  =>  [e1]  ;;  List all accessible workspaces"),
            "e1"
        );
    }

    #[test]
    fn strip_prompt_annotations_preserves_or_inside_tagged_heredoc() {
        let src = concat!(
            "x = m(p=<<T\n",
            "If GET state or equivalent state returns\n",
            "T)\n",
            "x",
        );
        let stripped = strip_prompt_expression_annotations(src);
        assert!(
            stripped.contains("state or equivalent"),
            "expected prose `or` to survive inside heredoc; got {:?}",
            stripped
        );
    }

    #[test]
    fn strip_prompt_annotations_no_longer_strips_or_alternatives() {
        assert_eq!(
            strip_prompt_expression_annotations("e1.m1() or e1.m2()"),
            "e1.m1() or e1.m2()"
        );
    }

    #[test]
    fn wire_surface_for_parse_does_not_strip_prompt_annotation_tails() {
        let cgs = CGS::new();
        assert_eq!(
            wire_surface_for_parse("e1  ;;  old hint", &cgs, FocusSpec::All, false),
            "e1  ;;  old hint"
        );
        assert_eq!(
            wire_surface_for_parse("e1  =>  [e1]", &cgs, FocusSpec::All, false),
            "e1  =>  [e1]"
        );
        assert_eq!(
            wire_surface_for_parse("e1.m1() or e1.m2()", &cgs, FocusSpec::All, false),
            "e1.m1() or e1.m2()"
        );
    }

    #[test]
    fn trim_description_for_agent_gloss_strips_terminal_period() {
        assert_eq!(
            trim_description_for_agent_gloss("Zone identifier."),
            "Zone identifier"
        );
        assert_eq!(trim_description_for_agent_gloss("  x.  "), "x");
        assert_eq!(trim_description_for_agent_gloss("no period"), "no period");
        assert_eq!(trim_description_for_agent_gloss(""), "");
    }

    #[test]
    fn trim_description_for_agent_gloss_strips_example_list_parentheticals() {
        assert_eq!(
            trim_description_for_agent_gloss(
                "Managed entrypoint ruleset for one execution phase on a zone (DDoS L7, managed WAF, rate limits, etc.)."
            ),
            "Managed entrypoint ruleset for one execution phase on a zone"
        );
        assert_eq!(
            trim_description_for_agent_gloss("Short capability (single token)."),
            "Short capability (single token)"
        );
    }

    #[test]
    fn registry_backed_compact_wire_label_nested_capability_param_is_leaf_only() {
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Doc".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: CapabilityName::from("document_edit_v2".to_string()),
            },
            value_registry_key: ValueDomainKey::new("fixture_payment_method_str").expect("key"),
            field_type: FieldType::String,
            string_semantics: None,
            array_items: None,
            allowed_values: None,
            wire_name: "operations.replace_range.fromRef".to_string(),
            description: String::new(),
        };
        assert_eq!(registry_backed_compact_wire_label(&m), "fromRef");
    }

    #[test]
    fn registry_backed_compact_wire_label_top_level_capability_param_unchanged() {
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Doc".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: CapabilityName::from("document_edit_v2".to_string()),
            },
            value_registry_key: ValueDomainKey::new("fixture_payment_method_str").expect("key"),
            field_type: FieldType::String,
            string_semantics: None,
            array_items: None,
            allowed_values: None,
            wire_name: "operations".to_string(),
            description: String::new(),
        };
        assert_eq!(registry_backed_compact_wire_label(&m), "operations");
    }

    #[test]
    fn render_gloss_capability_param_omits_wire_path_without_description() {
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Order".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: CapabilityName::from("test_cap".to_string()),
            },
            value_registry_key: ValueDomainKey::new("fixture_payment_method_str").expect("key"),
            field_type: FieldType::String,
            string_semantics: None,
            array_items: None,
            allowed_values: None,
            wire_name: "payment_method_id".to_string(),
            description: String::new(),
        };
        assert_eq!(m.render_gloss(None), "str");
    }

    #[test]
    fn render_gloss_capability_param_uses_description_when_set() {
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Order".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: CapabilityName::from("test_cap".to_string()),
            },
            value_registry_key: ValueDomainKey::new("fixture_payment_method_str").expect("key"),
            field_type: FieldType::String,
            string_semantics: None,
            array_items: None,
            allowed_values: None,
            wire_name: "payment_method_id".to_string(),
            description: "Payment method".to_string(),
        };
        assert_eq!(m.render_gloss(None), "str · Payment method");
    }

    #[test]
    fn render_gloss_string_semantics_markdown_replaces_str_label() {
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Issue".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: CapabilityName::from("test_cap".to_string()),
            },
            value_registry_key: ValueDomainKey::new("fixture_issue_body_md").expect("key"),
            field_type: FieldType::String,
            string_semantics: Some(StringSemantics::Markdown),
            array_items: None,
            allowed_values: None,
            wire_name: "body".to_string(),
            description: String::new(),
        };
        assert_eq!(m.render_gloss(None), "markdown");
    }

    #[test]
    fn render_gloss_array_param_shows_element_type() {
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Order".to_string()),
            role: IdentRegistryRole::CapabilityParam {
                capability: CapabilityName::from("exchange_delivered_order_items".to_string()),
            },
            value_registry_key: ValueDomainKey::new("fixture_order_item_ids").expect("key"),
            field_type: FieldType::Array,
            string_semantics: None,
            array_items: Some(ArrayItemsSchema {
                kind: FieldValueKind::Registry(
                    ValueDomainKey::new("fixture_variant_ref").expect("key"),
                ),
                field_type: FieldType::EntityRef {
                    target: EntityName::from("Variant".to_string()),
                },
                value_format: None,
                allowed_values: None,
            }),
            allowed_values: None,
            wire_name: "item_ids".to_string(),
            description: String::new(),
        };
        assert_eq!(m.render_gloss(None), "array[ref:Variant]");
    }

    #[test]
    fn render_gloss_select_shows_allowed_values_not_wire_name() {
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Issue".to_string()),
            role: IdentRegistryRole::EntityField,
            value_registry_key: ValueDomainKey::new("issue_state_reason").expect("key"),
            field_type: FieldType::Select,
            string_semantics: None,
            array_items: None,
            allowed_values: Some(vec![
                "completed".to_string(),
                "reopened".to_string(),
                "not_planned".to_string(),
                "duplicate".to_string(),
            ]),
            wire_name: "state_reason".to_string(),
            description: String::new(),
        };
        assert_eq!(
            m.render_gloss(None),
            "select · completed, reopened, not_planned, duplicate"
        );
    }

    /// Two `p#` slots may share one `values:` key; each still earns a full select gloss (no cross-`p#` peer line).
    #[test]
    fn value_domain_v_symbols_dedupe_shared_registry_rows() {
        let mut cgs = CGS::new();
        cgs.entry_id = Some("fixture_entry".into());
        cgs.values.insert(
            "fixture_str_vtest".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        cgs.values.insert(
            "shared_sel_vtest".into(),
            NamedValueSchema {
                description: "shared select semantics".into(),
                field_type: FieldType::Select,
                value_format: None,
                allowed_values: Some(vec!["alpha".into(), "beta".into()]),
                string_semantics: None,
                array_items: None,
            },
        );
        let vr = FieldValueKind::Registry(ValueDomainKey::new("shared_sel_vtest").expect("key"));
        let id_kind =
            FieldValueKind::Registry(ValueDomainKey::new("fixture_str_vtest").expect("key"));
        cgs.add_resource(ResourceSchema {
            name: "Widget".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: id_kind,
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "foo".into(),
                    kind: vr.clone(),
                    description: "foo slot".into(),
                    required: false,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "bar".into(),
                    kind: vr,
                    description: "bar slot".into(),
                    required: false,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "widget_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Widget".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({"method":"GET","path":[{"type":"literal","value":"w"},{"type":"var","name":"id"}]}).into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
        })
        .unwrap();
        cgs.validate().expect("fixture CGS");
        let map = TeachingExposureSession::new(&cgs, "fixture_entry", &["Widget"]).to_symbol_map();
        let p_foo = map.ident_sym_entity_field_for("fixture_entry", "Widget", "foo");
        let p_bar = map.ident_sym_entity_field_for("fixture_entry", "Widget", "bar");
        let v_foo = map
            .value_sym_for_p_sym(&p_foo)
            .expect("registry-backed foo maps to v#");
        let v_bar = map
            .value_sym_for_p_sym(&p_bar)
            .expect("registry-backed bar maps to v#");
        assert_eq!(v_foo, v_bar, "same value_ref → one v#");
        assert_eq!(
            map.value_domain_fp_for_v_sym(&v_foo).unwrap(),
            "fixture_entry|vr:shared_sel_vtest"
        );
        let gloss = map.value_domain_gloss_for_v_sym(&v_foo).expect("v gloss");
        assert!(
            gloss.contains("alpha") && gloss.contains("beta"),
            "expected full select teaching on v# row: {gloss}"
        );
    }

    #[test]
    fn render_gloss_select_full_for_each_slot_sharing_value_registry_key() {
        let av = Some(vec!["a".to_string(), "b".to_string()]);
        let gloss_a = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("E".to_string()),
            role: IdentRegistryRole::EntityField,
            value_registry_key: ValueDomainKey::new("shared_status").expect("key"),
            field_type: FieldType::Select,
            string_semantics: None,
            array_items: None,
            allowed_values: av.clone(),
            wire_name: "status_a".into(),
            description: String::new(),
        }
        .render_gloss(None);
        let gloss_b = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("E".to_string()),
            role: IdentRegistryRole::EntityField,
            value_registry_key: ValueDomainKey::new("shared_status").expect("key"),
            field_type: FieldType::Select,
            string_semantics: None,
            array_items: None,
            allowed_values: av,
            wire_name: "status_b".into(),
            description: String::new(),
        }
        .render_gloss(None);
        assert_eq!(gloss_a, "select · a, b");
        assert_eq!(gloss_b, "select · a, b");
        assert!(
            !gloss_a.contains("same values as"),
            "peer-gloss path must stay removed"
        );
    }

    #[test]
    fn render_gloss_select_long_allowed_values_not_truncated() {
        let tokens: Vec<String> = (0..40).map(|i| format!("http_request_phase_{i}")).collect();
        let last = tokens.last().expect("last").clone();
        let m = IdentMetadata::RegistryBacked {
            catalog_entry_id: String::new(),
            entity: EntityName::from("Ruleset".to_string()),
            role: IdentRegistryRole::EntityField,
            value_registry_key: ValueDomainKey::new("fixture_long_select").expect("key"),
            field_type: FieldType::Select,
            string_semantics: None,
            array_items: None,
            allowed_values: Some(tokens),
            wire_name: "phase".to_string(),
            description: String::new(),
        };
        let g = m.render_gloss(None);
        assert!(g.contains(&last), "expected full enum tail in gloss: {g}");
        assert!(
            !g.contains('…'),
            "select gloss must not use ellipsis truncation: {g}"
        );
    }

    #[test]
    fn build_ident_metadata_includes_scalar_kinds() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let meta = build_ident_metadata(&cgs, &full);
        assert!(
            meta.values().any(|m| {
                matches!(
                    m,
                    IdentMetadata::RegistryBacked {
                        field_type: crate::FieldType::Date,
                        ..
                    }
                )
            }),
            "expected Date field type in metadata"
        );
        assert!(
            meta.values().any(|m| {
                matches!(
                    m,
                    IdentMetadata::RegistryBacked {
                        field_type: crate::FieldType::Boolean,
                        ..
                    }
                )
            }),
            "expected Boolean field type in metadata"
        );
    }

    #[test]
    fn teaching_term_entity_roundtrips_display_with_symbol_map() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let dt = map.try_entity_teaching_term("Pet").expect("Pet in map");
        assert_eq!(dt.to_string(), map.entity_sym_for("", "Pet"));
        assert!(matches!(dt, crate::TeachingTerm::Entity(_, _)));
    }

    #[test]
    fn teaching_term_method_matches_symbol_map_when_cgs_resolves() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let kebab = "upload-image";
        let cap_ref = crate::method_ref_for_domain_segment(&cgs, "Pet", kebab)
            .expect("upload-image capability on Pet");
        let cap_name = cap_ref.capability.as_str();
        let m_str = map.method_sym_for("", "Pet", cap_name);
        if m_str == cap_name {
            return;
        }
        let dt = map
            .try_method_teaching_term(&cgs, "Pet", cap_name)
            .expect("method domain term");
        assert_eq!(dt.to_string(), m_str);
        assert!(matches!(dt, crate::TeachingTerm::Method(_, _)));
    }

    #[test]
    fn federation_duplicate_entity_name_allocates_distinct_e_symbols() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let mut cgs_a = load_schema_dir(dir).unwrap();
        cgs_a.entry_id = Some("alpha".into());
        let mut cgs_b = cgs_a.clone();
        cgs_b.entry_id = Some("beta".into());
        let arc_b = std::sync::Arc::new(cgs_b);
        let mut s = TeachingExposureSession::new(&cgs_a, "alpha", &["Pet"]);
        s.expose_entities(&[arc_b.as_ref()], arc_b.clone(), "beta", &["Pet"]);
        assert_eq!(s.entities.len(), 2);
        let map = s.to_symbol_map();
        let sa = map
            .tables
            .qualified_entity_to_sym
            .get(&QualifiedEntityKey::new("alpha", "Pet"))
            .expect("alpha Pet")
            .as_wire();
        let sb = map
            .tables
            .qualified_entity_to_sym
            .get(&QualifiedEntityKey::new("beta", "Pet"))
            .expect("beta Pet")
            .as_wire();
        assert_ne!(sa, sb);
    }

    #[test]
    fn intent_filtered_domain_session_has_narrower_capability_surface_than_legacy() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let legacy = TeachingExposureSession::new(&cgs, "overshow", &["Profile", "Meeting"]);
        let endpoints =
            relation_endpoint_keys("overshow", &["Profile".to_string(), "Meeting".to_string()]);
        let delta = crate::discovery::derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "organisation project profile metadata list",
            &endpoints,
            &["Profile".to_string()],
            None,
            crate::discovery::ExposureSurfaceOptions::default(),
        );
        let filtered =
            TeachingExposureSession::new_with_intent_delta(&cgs, "overshow", &["Profile"], delta);
        assert!(
            filtered.surface.capabilities.len() < legacy.surface.capabilities.len(),
            "expected fewer capabilities when only Profile is seeded vs legacy Profile+Meeting closure"
        );
    }

    #[test]
    fn proof_insert_before_blocks_slot_meta_is_structural_not_relation_collision() {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("../../apis/proof");
        if !p.is_dir() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(&p).unwrap();
        let entry_id = cgs.entry_id.as_deref().unwrap_or("");
        let map = TeachingExposureSession::new(&cgs, entry_id, &["Document"]).symbol_map_arc();
        let sym = map.ident_sym_cap_param_for(
            entry_id,
            "Document",
            "document_edit_v2",
            "operations.insert_before.blocks",
        );
        let quad = map
            .capability_param_quad_for_p_sym(sym.as_str())
            .unwrap_or_else(|| panic!("no quad for {sym}"));
        let meta = ident_metadata_for_capability_input_path(
            &cgs,
            "Document",
            quad.2.as_str(),
            quad.3.as_str(),
        )
        .unwrap_or_else(|| panic!("no meta for {quad:?}"));
        assert!(
            matches!(meta, IdentMetadata::RegistryBacked { .. }),
            "expected registry-backed blocks array (flat logical surface), got {meta:?}"
        );
    }

    #[test]
    fn federated_entity_name_collision_assigns_distinct_entity_symbols() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&root).expect("plasm_language_matrix");
        let layers = [&cgs, &cgs];
        let mut exp = TeachingExposureSession::new(&cgs, "github", &["LangItem"]);
        exp.expose_entities(
            &layers,
            std::sync::Arc::new(cgs.clone()),
            "linear",
            &["LangItem"],
        );
        assert_eq!(exp.entities, vec!["LangItem", "LangItem"]);
        assert_eq!(exp.entity_catalog_entry_ids, vec!["github", "linear"]);
        let map = exp.symbol_map_arc();
        assert_eq!(
            map.entry_id_for_entity_symbol("e1").as_deref(),
            Some("github")
        );
        assert_eq!(
            map.entry_id_for_entity_symbol("e2").as_deref(),
            Some("linear")
        );
        assert_eq!(map.entity_sym_for("github", "LangItem"), "e1");
        assert_eq!(map.entity_sym_for("linear", "LangItem"), "e2");
        assert_eq!(
            map.entity_stamps_for_wire("LangItem"),
            vec![
                ("github".to_string(), "e1".to_string()),
                ("linear".to_string(), "e2".to_string()),
            ]
        );
    }

    #[test]
    fn pending_relation_slots_repair_relation_target_qualified_in_later_wave() {
        // Relation-slot repair: a source entity's outgoing relation slot (`LangItem.summary` →
        // `LangSummary`) is created when the source is exposed, but its target only qualifies in a
        // later wave. `relation_slots_for_expand_wave` must then surface the hop as pending and the
        // symbol map must assign it an `r#`. Driven by the abstract `plasm_language_matrix` fixture
        // (strict rule: plasm-core language tests must not couple to `apis/<name>/` catalogs).
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&root).expect("plasm_language_matrix");
        let cgs_arc = std::sync::Arc::new(cgs.clone());
        let layers = [&cgs];
        let intent = "lang items and their summaries";
        let relation_keys = crate::relation_endpoint_keys("matrix", &["LangItem".to_string()]);
        let delta = crate::discovery::derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            intent,
            &relation_keys,
            &["LangItem".to_string()],
            None,
            crate::discovery::ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let mut exp =
            TeachingExposureSession::new_with_intent_delta(&cgs, "matrix", &["LangItem"], delta);
        let slots_before = exp.surface.slots.clone();
        let n0 = exp.entities.len();
        let summary_delta = crate::discovery::derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            intent,
            &exp.relation_endpoint_keys_for_wave("matrix", &["LangSummary".to_string()]),
            &["LangSummary".to_string()],
            None,
            crate::discovery::ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        exp.expose_surface(
            &layers,
            cgs_arc.clone(),
            "matrix",
            &["LangSummary"],
            summary_delta,
        );
        let added = exp.qualified_entities_since(n0);
        let relation_keys =
            exp.relation_endpoint_keys_for_wave("matrix", &["LangSummary".to_string()]);
        let edge_slots = exp.relation_slots_for_expand_wave(&slots_before, &added, &relation_keys);
        assert!(
            edge_slots.iter().any(|slot| matches!(
                slot,
                ExposureSlotKey::Relation { source, relation }
                    if source.entity.as_str() == "LangItem" && relation.as_str() == "summary"
            )),
            "LangItem→summary hop should be pending after LangSummary qualifies: {edge_slots:?}"
        );
        exp.admit_relation_edge_slots_for_render(&layers, &edge_slots);
        let map = exp.symbol_map_arc();
        let r_sym = map.ident_sym_relation_for("matrix", "LangItem", "summary");
        assert!(
            r_sym.starts_with('r'),
            "parser symbol map must assign r# for LangItem.summary after repair: {r_sym}"
        );
    }

    #[test]
    fn entity_p_sym_resolves_via_forward_table() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let exp =
            TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA", "HomographRowB"]);
        let map = exp.symbol_map_arc();
        let issue_title = map.ident_sym_entity_field_for("langmatrix", "HomographRowA", "headline");
        let label_name = map.ident_sym_entity_field_for("langmatrix", "HomographRowB", "caption");
        if !SymbolMap::is_opaque_p_sym(issue_title.as_str()) {
            return;
        }
        let ent_a = cgs.get_entity("HomographRowA").expect("HomographRowA");
        assert_eq!(
            map.resolve_entity_field(
                CatalogScope::qualified("langmatrix"),
                "HomographRowA",
                ent_a,
                issue_title.as_str()
            )
            .expect("HomographRowA p#"),
            "headline"
        );
        let ent_b = cgs.get_entity("HomographRowB").expect("HomographRowB");
        assert_eq!(
            map.resolve_entity_field(
                CatalogScope::qualified("langmatrix"),
                "HomographRowB",
                ent_b,
                label_name.as_str()
            )
            .expect("HomographRowB p#"),
            "caption"
        );
    }

    #[test]
    fn github_create_capabilities_optional_legend_uses_compact_marker_and_pairs() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("../../apis/github");
        if !dir.is_dir() {
            return;
        }
        let cgs = crate::loader::load_schema(&dir).expect("github");
        let exp =
            TeachingExposureSession::new(&cgs, "github", &["Repository", "Issue", "PullRequest"]);
        let map = exp.symbol_map_arc();
        for (cap_name, param) in [
            ("issue_create", "labels"),
            ("issue_update", "labels"),
            ("pr_create", "body"),
            ("repo_content_create", "branch"),
            ("repo_content_update", "branch"),
        ] {
            let cap = cgs.get_capability(cap_name).expect(cap_name);
            let sig = map.capability_input_signature_gloss(&cgs, cap);
            assert!(
                sig.contains("optional params: optional"),
                "expected compact optional marker in `{sig}` for {cap_name}"
            );
            let pairs = capability_optional_legend_param_pairs(
                map.as_ref(),
                "github",
                cap.domain.as_str(),
                cap,
            );
            assert!(
                pairs.iter().any(|(w, s)| w == param && s.starts_with('p')),
                "expected optional `{param}` → p# in pairs for {cap_name}: {pairs:?}"
            );
        }
    }
}
