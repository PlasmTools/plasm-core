//! Symbol tuning for LLM prompts: opaque `e#` / `m#` / `p#` / `v#` tokens — each **distinct taught `p#` meaning**
//! is glossed **once** (the line before its first use in **DOMAIN**); **`v#`** rows teach each CGS `values:` /
//! `value_ref` domain **once**, and registry-backed `p#` gloss lines teach **`v# · wire`** (and optional
//! point-of-use prose when it varies); typing and enum ranges stay on the `v#` row.
//! **DOMAIN** gives entity/method examples (including `e#` per block),
//! `;;` descriptions (with a short **type** prefix like `date · …` / `bool · …` from CGS), comma-separated
//! `optional params: …` / `[scope …]` before the prose description (` — `), when present (required args appear in the expression).
//! Programs use **`p#` only** for keyed slots; `v#` is prompt-teaching for shared value domains.
//!
//! [`SymbolMap`] is built from the same entity slice as [`crate::prompt_render`] uses. Call
//! [`expand_path_symbols`] on model output **before** [`crate::expr_parser::parse`] (`v#` is not expanded).
//!
//! **Caching (execute / MCP):** for a fixed loaded [`CGS`] (`catalog_cgs_hash_hex`), almost all DOMAIN
//! symbol structure is stable. [`DomainExposureSession`] memoizes [`SymbolMap`] behind
//! [`DomainExposureSession::symbol_map_arc`] and clears that cache whenever [`DomainExposureSession::expose_entities`]
//! runs so wave indices stay consistent. Per-request variance is mostly the append-only entity list and
//! the derived `e#` / `m#` / `p#` / `v#` table.
//!
//! **Cross-session reuse (one process):** [`SymbolMapCrossRequestCache`] (bounded LRU; capacity from
//! `PLASM_SYMBOL_MAP_LRU_CAP`, default `64`, set `0` to disable) deduplicates identical [`SymbolMap`]
//! snapshots when the catalog fingerprint and exposure rows match a recent session.

use crate::domain_term::{
    method_ref_for_domain_segment, resolve_parameter_slot, DomainTerm, EntityRef, ParameterSlot,
    Symbol,
};
use crate::identity::{
    CapabilityName, CapabilityParamName, EntityFieldName, EntityName, RelationName,
};
use crate::schema::{
    capability_method_label_kebab, ArrayItemsSchema, CapabilitySchema, InputFieldSchema, InputType,
    ParameterRole, StringSemantics, ValueDomainKey, CGS,
};
use crate::CapabilityKind;
use crate::FieldType;
use indexmap::IndexMap;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};

/// Which entities drive DOMAIN / symbol-map slicing (REPL `--focus`, eval, HTTP execute sessions).
#[derive(Clone, Copy, Debug, Default)]
pub enum FocusSpec<'a> {
    /// Full schema (no entity subset).
    #[default]
    All,
    /// One seed entity plus its 2-hop neighbourhood (existing behaviour).
    Single(&'a str),
    /// Union of neighbourhoods for several seeds (same CGS).
    Seeds(&'a [&'a str]),
    /// **Exact** entity list only (no 2-hop union). Used with [`DomainExposureSession`] so DOMAIN and
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

/// Typed metadata for one DOMAIN / symbol slot — **discriminated** so relations and CGS-backed
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
}

/// Key for [`IdentMetadata`] maps: `(registry entry_id, CGS entity, wire name)`.
pub type IdentMetaKey = (String, EntityName, String);
use std::fmt::Write;

/// Catalog-qualified entity identity for incremental DOMAIN exposure filtering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExposureEntityKey {
    pub entry_id: String,
    pub entity: EntityName,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExposureCapabilityKey {
    pub entry_id: String,
    pub domain: EntityName,
    pub capability: CapabilityName,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// Full per-entity closure (legacy HTTP execute / REPL paths): every field, relation, capability, and param.
///
/// `entry_id` is the caller’s registry row id (HTTP/MCP); exposure keys follow [`CGS::entry_id`] when set.
#[allow(unused_variables)]
pub fn legacy_exposure_surface_for_entities(
    cgs: &CGS,
    entry_id: &str,
    entities: &[&str],
    out: &mut ExposureSurface,
) {
    // Match [`crate::prompt_render`] / gloss scratch: registry-backed rows key on `CGS::entry_id`,
    // defaulting to empty when unset (YAML fixtures often omit `entry_id:`).
    let cid = cgs.entry_id.clone().unwrap_or_default();
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
        for (rname, _r) in &ent.relations {
            out.slots.insert(ExposureSlotKey::Relation {
                source: ekey.clone(),
                relation: rname.clone(),
            });
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
                let InputType::Object { fields, .. } = &is.input_type else {
                    continue;
                };
                for f in fields {
                    out.slots.insert(ExposureSlotKey::CapabilityParam {
                        capability: ckey.clone(),
                        param: CapabilityParamName::new(f.name.clone()),
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
                let nv = cgs
                    .named_value_for_slot(f)
                    .expect("values row for entity field");
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
                let Some(is) = &cap.input_schema else {
                    continue;
                };
                let InputType::Object { fields, .. } = &is.input_type else {
                    continue;
                };
                let Some(f) = fields.iter().find(|x| x.name == param.as_str()) else {
                    continue;
                };
                let nv = cgs
                    .named_value_for_slot(f)
                    .expect("values row for capability param");
                out.push(IdentMetadata::RegistryBacked {
                    catalog_entry_id: capability.entry_id.clone(),
                    entity: capability.domain.clone(),
                    role: IdentRegistryRole::CapabilityParam {
                        capability: cap.name.clone(),
                    },
                    value_registry_key: f.kind.registry_key().clone(),
                    field_type: nv.field_type.clone(),
                    string_semantics: nv.string_semantics,
                    array_items: nv.array_items.clone(),
                    allowed_values: nv.allowed_values.clone(),
                    wire_name: param.as_str().to_string(),
                    description: f.description.clone().unwrap_or_default(),
                });
            }
        }
    }
    out
}

/// Same 2-hop focus neighbourhood as prompt rendering: `Some(set)` when focus is set.
#[inline]
fn field_is_filter_like_gloss(f: &InputFieldSchema) -> bool {
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
            if let Ok(nv) = cgs.named_value_for_slot(field) {
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
            if let Ok(nv) = cgs.named_value_for_slot(field) {
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
            if let Some(ent) = cgs.get_entity(s) {
                if !ent.abstract_entity {
                    full.push(s);
                }
            }
        }
        // `SeedsExact` matches [`DomainExposureSession::entities`] only (no 2-hop neighbourhood).
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

/// Full + dim entity name slices when [`DomainExposureSession`] spans multiple loaded [`crate::schema::CGS`] graphs.
pub fn entity_slices_for_render_federated<'a>(
    cgs_layers: &[&'a CGS],
    exposure: &'a DomainExposureSession,
) -> (Vec<&'a str>, Vec<&'a str>) {
    if cgs_layers.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let refs: Vec<&'a str> = exposure.entities.iter().map(|s| s.as_str()).collect();
    let mut full: Vec<&str> = Vec::new();
    let mut full_set: HashSet<&str> = HashSet::new();
    for &name in &refs {
        let ok = cgs_layers.iter().any(|c| {
            c.get_entity(name)
                .map(|e| !e.abstract_entity)
                .unwrap_or(false)
        });
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
/// the same wire name receive **one** `p#`—matching compact DOMAIN teaching where `v# · wire` is
/// identical. Occurrence lookups (`entity_field_to_sym`, `cap_param_to_sym`) still bind every
/// `(entity, slot)` / `(cap, param)` to that shared symbol.
///
/// Relations and synthetic unknown slots keep fully scoped fingerprints via
/// [`slot_allocation_fingerprint`].
pub(crate) fn slot_symbol_allocation_fingerprint(meta: &IdentMetadata) -> String {
    if let IdentMetadata::RegistryBacked { .. } = meta {
        if let Some(vfp) = meta.value_domain_allocation_fp() {
            return format!("{vfp}|w:{}", meta.wire_name());
        }
    }
    slot_allocation_fingerprint(meta)
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
            let nv = cgs
                .named_value_for_slot(f)
                .expect("values row for entity field");
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
                let nv = cgs
                    .named_value_for_slot(f)
                    .expect("values row for capability param");
                out.entry((cid.clone(), en.clone(), f.name.clone()))
                    .or_insert_with(|| IdentMetadata::RegistryBacked {
                        catalog_entry_id: cid.clone(),
                        entity: en.clone(),
                        role: IdentRegistryRole::CapabilityParam {
                            capability: cap.name.clone(),
                        },
                        value_registry_key: f.kind.registry_key().clone(),
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
            } => catalog_entry_id.as_str(),
        }
    }

    #[inline]
    pub fn entity(&self) -> &EntityName {
        match self {
            IdentMetadata::RegistryBacked { entity, .. }
            | IdentMetadata::Relation { entity, .. }
            | IdentMetadata::SyntheticUnknown { entity, .. } => entity,
        }
    }

    #[inline]
    pub fn wire_name(&self) -> &str {
        match self {
            IdentMetadata::RegistryBacked { wire_name, .. }
            | IdentMetadata::Relation { wire_name, .. }
            | IdentMetadata::SyntheticUnknown { wire_name, .. } => wire_name.as_str(),
        }
    }

    fn description_trimmed(&self) -> &str {
        match self {
            IdentMetadata::RegistryBacked { description, .. }
            | IdentMetadata::Relation { description, .. }
            | IdentMetadata::SyntheticUnknown { description, .. } => description.trim(),
        }
    }

    #[inline]
    pub fn description(&self) -> &str {
        match self {
            IdentMetadata::RegistryBacked { description, .. }
            | IdentMetadata::Relation { description, .. }
            | IdentMetadata::SyntheticUnknown { description, .. } => description.as_str(),
        }
    }

    #[inline]
    pub fn allowed_values(&self) -> Option<&Vec<String>> {
        match self {
            IdentMetadata::RegistryBacked { allowed_values, .. } => allowed_values.as_ref(),
            IdentMetadata::Relation { .. } | IdentMetadata::SyntheticUnknown { .. } => None,
        }
    }

    /// Render the gloss line content (after `p#  ;;  `). The `map` is used to resolve
    /// entity-ref targets to their `e#` symbol when symbol tuning is active.
    pub fn render_gloss(&self, map: Option<&SymbolMap>) -> String {
        match self {
            IdentMetadata::Relation {
                target,
                wire_name: _,
                description,
                ..
            } => {
                let type_label = match map {
                    Some(m) => format!("=> {}", m.entity_sym(target.as_str())),
                    None => format!("=> {}", target),
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
            IdentMetadata::RegistryBacked {
                field_type,
                array_items,
                string_semantics,
                allowed_values,
                wire_name,
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
            IdentMetadata::Relation { .. } | IdentMetadata::SyntheticUnknown { .. } => None,
        }
    }

    /// Gloss for a **`v#` DOMAIN row** — typing from the shared `values:` registry row (`value_row_description`),
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
        let nv = c.named_value_for_slot(f).ok()?;
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

/// Short type label for DOMAIN `p#` gloss (matches [`FieldType`] / capability inputs).
/// Type keyword for a scalar `string` in DOMAIN gloss (`str` vs `markdown`, …).
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
                .map(|m| m.entity_sym(target.as_str()))
                .unwrap_or_else(|| target.to_string());
            format!("ref:{sym}")
        }
        _ => field_type_to_gloss_label(&ai.field_type),
    }
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
                let nv = cgs.named_value_for_slot(f).ok()?;
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
                let nv = cgs.named_value_for_slot(f).ok()?;
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
                    Some(m) => format!("=> {}", m.entity_sym(target)),
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

/// Bidirectional maps for one prompt/eval slice.
#[derive(Debug, Clone)]
pub struct SymbolMap {
    sym_to_entity: IndexMap<String, String>,
    /// `(registry entry_id, CGS entity name)` → opaque `e#`. Duplicate entity names across catalogs are distinct rows.
    qualified_entity_to_sym: IndexMap<(String, String), String>,
    /// method symbol -> (catalog entry_id, domain entity, kebab label)
    sym_to_method: IndexMap<String, (String, String, String)>,
    /// (registry entry_id or `""`, domain entity, kebab method label)
    method_to_sym: IndexMap<(String, String, String), String>,
    /// Dotted-call alias map: `(path anchor entity, m#)` → kebab when method is registered on another domain
    /// (`Team(42).m22` → kebab when `m22` maps to a child-domain method on a path anchor).
    anchor_scoped_method_sym: HashMap<(String, String), String>,
    sym_to_ident: IndexMap<String, String>,
    /// Wire name → first-assigned `p#` when multiple slots share a label (feedback collapse / legacy).
    ident_to_sym: IndexMap<String, String>,
    /// `(entry_id, entity, field)` → `p#` for entity fields.
    entity_field_to_sym: HashMap<(String, String, String), String>,
    /// `(entry_id, entity, relation_or_ref_name)` → `p#` for relations and entity-ref nav.
    relation_to_sym: HashMap<(String, String, String), String>,
    /// `(entry_id, domain_entity, capability_name, param)` → `p#` for capability inputs.
    cap_param_to_sym: HashMap<(String, String, String, String), String>,
    /// `(catalog_entry_id|vr:value_ref)` → `v#` — one symbol per CGS `values:` row in this session.
    pub(crate) value_domain_fp_to_sym: IndexMap<String, String>,
    /// `v#` → value-domain fingerprint (reverse of [`Self::value_domain_fp_to_sym`]).
    pub(crate) value_sym_to_fp: IndexMap<String, String>,
    /// `p#` → `v#` for registry-backed slots (relations / synthetic slots omit entries).
    pub(crate) p_sym_to_value_sym: HashMap<String, String>,
    /// Pre-rendered `v#  ;;  …` gloss bodies (DOMAIN teaching only; not used by [`expand_path_symbols`]).
    value_sym_gloss: IndexMap<String, String>,
}

impl SymbolMap {
    /// If `token` is a session `e#` symbol (e.g. `e1` from the DOMAIN table), return the canonical entity name.
    #[inline]
    pub fn resolve_session_entity_symbol(&self, token: &str) -> Option<String> {
        self.sym_to_entity.get(token).cloned()
    }

    /// Build maps for all entities in `full_entities` (slice order defines `e1`, `e2`, …).
    ///
    /// This is a thin wrapper around [`DomainExposureSession::new`] + [`DomainExposureSession::to_symbol_map`]:
    /// one code path for `m#` / `p#` assignment and dotted-call alias metadata (execute / REPL / canonical DOMAIN).
    pub fn build(cgs: &CGS, full_entities: &[&str]) -> Self {
        DomainExposureSession::new(cgs, "", full_entities).to_symbol_map()
    }

    /// Structured DOMAIN token when `canonical` is in this map.
    #[inline]
    pub fn try_entity_domain_term(&self, canonical: &str) -> Option<DomainTerm> {
        let mut matches: Vec<_> = self
            .qualified_entity_to_sym
            .iter()
            .filter(|((_, ent), _)| ent.as_str() == canonical)
            .collect();
        matches.sort_by_key(|((eid, _), sym)| (eid.clone(), (*sym).clone()));
        let sym_str = matches.first()?.1;
        let idx = Symbol::parse_index(sym_str, 'e')?;
        Some(DomainTerm::Entity(
            EntityRef {
                name: EntityName::new(canonical),
            },
            idx,
        ))
    }

    /// Method token + CGS [`MethodRef`]; requires `cgs` to attach capability identity.
    #[inline]
    pub fn try_method_domain_term(
        &self,
        cgs: &CGS,
        entity: &str,
        kebab: &str,
    ) -> Option<DomainTerm> {
        let entry_key = cgs.entry_id.as_deref().unwrap_or("");
        let sym_str = self
            .method_to_sym
            .get(&(entry_key.to_string(), entity.to_string(), kebab.to_string()))
            .or_else(|| {
                self.method_to_sym.iter().find_map(|((eid, e, k), s)| {
                    (e == entity && k == kebab && (eid.is_empty() || eid.as_str() == entry_key))
                        .then_some(s)
                })
            })?;
        let idx = Symbol::parse_index(sym_str, 'm')?;
        let mref = method_ref_for_domain_segment(cgs, entity, kebab)?;
        Some(DomainTerm::Method(mref, idx))
    }

    /// Parameter token + [`ParameterSlot`]; `full_entities` must match the slice used to build this map.
    #[inline]
    pub fn try_ident_domain_term(
        &self,
        cgs: &CGS,
        full_entities: &[&str],
        name: &str,
    ) -> Option<DomainTerm> {
        let slot = resolve_parameter_slot(cgs, full_entities, name)?;
        let entry_key = cgs.entry_id.as_deref().unwrap_or("");
        let sym_str = match &slot {
            ParameterSlot::EntityField { entity, field } => self.entity_field_to_sym.get(&(
                entry_key.to_string(),
                entity.as_str().to_string(),
                field.clone(),
            )),
            ParameterSlot::Relation { entity, name: rel } => self.relation_to_sym.get(&(
                entry_key.to_string(),
                entity.as_str().to_string(),
                rel.clone(),
            )),
            ParameterSlot::CapabilityInput {
                domain,
                capability,
                param,
            } => self.cap_param_to_sym.get(&(
                entry_key.to_string(),
                domain.as_str().to_string(),
                capability.as_str().to_string(),
                param.clone(),
            )),
        }?;
        let idx = Symbol::parse_index(sym_str, 'p')?;
        Some(DomainTerm::Parameter(slot, idx))
    }

    /// Opaque `e#` string — prefer [`Self::try_entity_domain_term`] then [`std::fmt::Display`] on [`DomainTerm`](crate::domain_term::DomainTerm) when threading DOMAIN state.
    #[inline]
    pub fn entity_sym(&self, canonical: &str) -> String {
        self.try_entity_domain_term(canonical)
            .map(|t| t.to_string())
            .unwrap_or_else(|| canonical.to_string())
    }

    /// Opaque `p#` for an **entity field** (scoped; preferred over [`Self::ident_sym`] when the entity is known).
    #[inline]
    pub fn ident_sym_entity_field(&self, entity: &str, field: &str) -> String {
        let mut v: Vec<_> = self
            .entity_field_to_sym
            .iter()
            .filter(|((_, e, f), _)| e.as_str() == entity && f.as_str() == field)
            .collect();
        v.sort_by_key(|((a, b, c), s)| (a.clone(), b.clone(), c.clone(), (*s).clone()));
        v.first()
            .map(|(_, s)| (*s).clone())
            .unwrap_or_else(|| field.to_string())
    }

    /// Opaque `p#` for a **relation** (or entity-ref nav segment) on `entity`.
    #[inline]
    pub fn ident_sym_relation(&self, entity: &str, relation: &str) -> String {
        let mut v: Vec<_> = self
            .relation_to_sym
            .iter()
            .filter(|((_, e, r), _)| e.as_str() == entity && r.as_str() == relation)
            .collect();
        v.sort_by_key(|((a, b, c), s)| (a.clone(), b.clone(), c.clone(), (*s).clone()));
        v.first()
            .map(|(_, s)| (*s).clone())
            .unwrap_or_else(|| relation.to_string())
    }

    /// Opaque `p#` for a **capability input** parameter (domain entity + capability + param name).
    #[inline]
    pub fn ident_sym_cap_param(
        &self,
        domain_entity: &str,
        capability: &str,
        param: &str,
    ) -> String {
        let mut v: Vec<_> = self
            .cap_param_to_sym
            .iter()
            .filter(|((_, dom, cap, p), _)| {
                dom.as_str() == domain_entity && cap.as_str() == capability && p.as_str() == param
            })
            .collect();
        v.sort_by_key(|((a, b, c, d), s)| {
            (a.clone(), b.clone(), c.clone(), d.clone(), (*s).clone())
        });
        v.first()
            .map(|(_, s)| (*s).clone())
            .unwrap_or_else(|| param.to_string())
    }

    /// `p#` by wire name alone only when all concrete slots with that wire name resolve to the same
    /// symbol. Returns `None` for ambiguous names like `id` when different entities map them to
    /// different `p#` tokens.
    pub fn ident_sym_unambiguous(&self, name: &str) -> Option<String> {
        let mut resolved: Option<&String> = None;
        for ((_, _, field), sym) in &self.entity_field_to_sym {
            if field != name {
                continue;
            }
            match resolved {
                None => resolved = Some(sym),
                Some(prev) if prev == sym => {}
                Some(_) => return None,
            }
        }
        for ((_, _, relation), sym) in &self.relation_to_sym {
            if relation != name {
                continue;
            }
            match resolved {
                None => resolved = Some(sym),
                Some(prev) if prev == sym => {}
                Some(_) => return None,
            }
        }
        for ((_, _, _, param), sym) in &self.cap_param_to_sym {
            if param != name {
                continue;
            }
            match resolved {
                None => resolved = Some(sym),
                Some(prev) if prev == sym => {}
                Some(_) => return None,
            }
        }
        resolved
            .cloned()
            .or_else(|| self.ident_to_sym.get(name).cloned())
    }

    /// Opaque `p#` by wire name alone — **ambiguous** when the same label names distinct slots; prefer
    /// [`Self::ident_sym_entity_field`], [`Self::ident_sym_cap_param`], or [`Self::ident_sym_relation`].
    #[inline]
    pub fn ident_sym(&self, name: &str) -> String {
        self.ident_to_sym
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Resolve `p#` → canonical field name. Returns `None` if `sym` is not a known `p#` token.
    pub fn resolve_ident<'a>(&'a self, sym: &str) -> Option<&'a str> {
        self.sym_to_ident.get(sym).map(|s| s.as_str())
    }

    /// Registry-backed `p#` → shared `v#` value-domain symbol, when one exists.
    #[inline]
    pub fn value_sym_for_p_sym(&self, p_sym: &str) -> Option<&str> {
        self.p_sym_to_value_sym.get(p_sym).map(|s| s.as_str())
    }

    /// Pre-rendered DOMAIN gloss for a `v#` row (after `;;`), if known.
    #[inline]
    pub fn value_domain_gloss_for_v_sym(&self, v_sym: &str) -> Option<&str> {
        self.value_sym_gloss.get(v_sym).map(|s| s.as_str())
    }

    /// Reverse lookup: `v#` → `(catalog_entry_id|vr:value_ref)` fingerprint.
    #[inline]
    pub fn value_domain_fp_for_v_sym(&self, v_sym: &str) -> Option<&str> {
        self.value_sym_to_fp.get(v_sym).map(|s| s.as_str())
    }

    /// If `sym` maps a capability input parameter, return the `(capability domain entity, param wire)`.
    pub fn capability_param_key_for_p_sym(&self, sym: &str) -> Option<(EntityName, String)> {
        for ((_eid, dom, _cap, pname), s) in &self.cap_param_to_sym {
            if s == sym {
                return Some((EntityName::from(dom.as_str()), pname.clone()));
            }
        }
        None
    }

    /// Rewrite canonical entity and field names in a short snippet into opaque `e#` / `p#` tokens for
    /// LLM-facing recovery text (inverse of [`expand_path_symbols`] for identifier-shaped spans).
    pub fn collapse_tokens_for_feedback(&self, input: &str) -> String {
        let mut keys: Vec<String> = self
            .qualified_entity_to_sym
            .keys()
            .map(|(_, ent)| ent.clone())
            .collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        keys.dedup();
        let mut s = scan_replace(input, &keys, |k| {
            self.try_entity_domain_term(k)
                .map(|t| t.to_string())
                .unwrap_or_else(|| k.to_string())
        });
        let mut idents: Vec<String> = self.ident_to_sym.keys().cloned().collect();
        idents.sort_by_key(|k| std::cmp::Reverse(k.len()));
        s = scan_replace(&s, &idents, |id| {
            self.ident_sym_unambiguous(id)
                .unwrap_or_else(|| id.to_string())
        });
        s
    }

    /// Opaque `m#` string — prefer [`Self::try_method_domain_term`] when `cgs` is available.
    #[inline]
    pub fn method_sym(&self, entity: &str, kebab: &str) -> String {
        self.method_to_sym
            .iter()
            .find(|((_, e, k), _)| e == entity && k == kebab)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| kebab.to_string())
    }

    /// If `label` is an opaque method token `m#` (digits), return the kebab method label for parse.
    #[inline]
    pub fn resolve_method_symbol_token(&self, label: &str) -> Option<&str> {
        self.resolve_method_symbol_pair(label)
            .map(|(_, kebab)| kebab)
    }

    /// `m#` → `(domain entity name, kebab label)` — disambiguates duplicate kebabs (e.g. `space_query` vs `task_query` both `query`).
    #[inline]
    pub fn resolve_method_symbol_pair(&self, label: &str) -> Option<(&str, &str)> {
        let rest = label.strip_prefix('m')?;
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        self.sym_to_method
            .get(label)
            .map(|(_, d, k)| (d.as_str(), k.as_str()))
    }

    /// `[scope …]` fragment for DOMAIN `;;` legends only (no `optional params:` list).
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
        let mut scope_parts: Vec<String> = Vec::new();
        let domain = cap.domain.as_str();
        let cap_name = cap.name.as_str();
        for f in fields {
            if !matches!(f.role, Some(ParameterRole::Scope)) {
                continue;
            }
            let Ok(nv) = cgs.named_value_for_slot(f) else {
                continue;
            };
            if let FieldType::EntityRef { target } = &nv.field_type {
                let ps = self.ident_sym_cap_param(domain, cap_name, f.name.as_str());
                let es = self.entity_sym(target.as_str());
                scope_parts.push(format!("{ps}→{es}"));
            } else {
                scope_parts.push(self.ident_sym_cap_param(domain, cap_name, f.name.as_str()));
            }
        }
        if scope_parts.is_empty() {
            return String::new();
        }
        let s = format!("[scope {}]", scope_parts.join(", "));
        crate::utf8_trunc::truncate_utf8_owned_with_ellipsis(s, MAX_SIG)
    }

    /// Optional / scope parameter symbols for DOMAIN `;;` legends. Required parameters are omitted — they
    /// are already shown in the example expression. For [`CapabilityKind::Query`], omits `[scope …]`.
    /// Required invoke slots are defined by preceding `p#` gloss rows; this gloss is **optionality only**.
    pub(crate) fn capability_input_signature_gloss(
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
        let mut scope_s = self.capability_scope_legend_gloss(cgs, cap);
        let mut optional_parts: Vec<String> = Vec::new();
        let domain = cap.domain.as_str();
        let cap_name = cap.name.as_str();
        for f in fields {
            if matches!(f.role, Some(ParameterRole::Scope)) {
                continue;
            }
            if !field_is_filter_like_gloss(f) {
                continue;
            }
            let sym = self.ident_sym_cap_param(domain, cap_name, f.name.as_str());
            if f.required {
                continue;
            }
            optional_parts.push(sym);
        }
        if !optional_parts.is_empty() {
            if !scope_s.is_empty() {
                scope_s.push(' ');
            }
            let _ = write!(
                &mut scope_s,
                "optional params: {}",
                optional_parts.join(", ")
            );
        }
        if scope_s.is_empty() {
            return scope_s;
        }
        crate::utf8_trunc::truncate_utf8_owned_with_ellipsis(scope_s, MAX_SIG)
    }

    /// Reserved for future SYMBOL MAP content; **FIELDS** moved inline into **DOMAIN** (see [`build_ident_gloss_map`]).
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
        let name = self
            .sym_to_ident
            .get(sym)
            .map(|s| s.as_str())
            .unwrap_or(sym);
        let desc = ident_gloss
            .get(name)
            .map(|d| d.as_str().trim())
            .filter(|d| !d.is_empty())
            .map(|d| truncate_desc(d, MAX_DESC))
            .unwrap_or_else(|| name.to_string())
            .replace('\t', " ");
        let ty = ident_types
            .and_then(|m| m.get(name))
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
        if expr.as_bytes().get(i) == Some(&b'p') && ident_boundary_left(expr, i) {
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

/// Same truncation cap as [`IdentMetadata::render_gloss`] trailing prose (DOMAIN / TSV parity).
pub(crate) fn gloss_description_truncated(s: &str) -> String {
    truncate_desc(s, 100)
}

/// Expand symbolic path text to canonical identifiers for the parser.
pub fn expand_path_symbols(input: &str, map: &SymbolMap) -> String {
    let mut s = replace_sym_tokens(input, map, SymPhase::Entity);
    s = replace_sym_tokens(&s, map, SymPhase::Ident);
    s = expand_method_tokens(&s, map);
    s
}

enum SymPhase {
    Entity,
    Ident,
}

fn replace_sym_tokens(input: &str, map: &SymbolMap, phase: SymPhase) -> String {
    use std::collections::HashMap;
    let lookup: HashMap<String, String> = match phase {
        SymPhase::Entity => map
            .sym_to_entity
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect(),
        SymPhase::Ident => map
            .sym_to_ident
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect(),
    };
    let mut syms: Vec<String> = lookup.keys().cloned().collect();
    syms.sort_by_key(|k| std::cmp::Reverse(k.len()));
    scan_replace(input, &syms, |sym| {
        lookup.get(sym).cloned().unwrap_or_else(|| sym.to_string())
    })
}

fn expand_method_tokens(input: &str, map: &SymbolMap) -> String {
    let mut syms: Vec<String> = map.sym_to_method.keys().cloned().collect();
    syms.sort_by_key(|k| std::cmp::Reverse(k.len()));
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
        let mut advanced = false;
        if ch == '.' && i + 1 < input.len() {
            let rest = &input[i + 1..];
            if let Some(sym) = syms.iter().find(|s| rest.starts_with(*s)) {
                let sym_len = sym.len();
                let after = i + 1 + sym_len;
                let boundary_ok =
                    after >= input.len() || !ident_continue(input[after..].chars().next().unwrap());
                if boundary_ok {
                    if let Some((_, ent, kebab)) = map.sym_to_method.get(sym.as_str()) {
                        if let Some(left_ent) = find_entity_before_dot(input, i) {
                            if left_ent == *ent {
                                out.push('.');
                                out.push_str(kebab);
                                i += 1 + sym_len;
                                advanced = true;
                            } else if let Some(sk) =
                                map.anchor_scoped_method_sym.get(&(left_ent, sym.clone()))
                            {
                                out.push('.');
                                out.push_str(sk);
                                i += 1 + sym_len;
                                advanced = true;
                            }
                        }
                    }
                }
            }
        }
        if advanced {
            continue;
        }
        out.push(ch);
        i += ch_len;
    }
    out
}

fn find_entity_before_dot(s: &str, dot_idx: usize) -> Option<String> {
    if dot_idx == 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = dot_idx - 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        if i == 0 {
            return None;
        }
        i -= 1;
    }
    if bytes[i] == b')' {
        return entity_before_dot_after_close_paren(s, i);
    }
    // `Entity.method()` / `Entity.rel` — identifier immediately before `.`
    let end = i + 1;
    let mut start = end;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
            start -= 1;
        } else {
            break;
        }
    }
    (start < end).then(|| s[start..end].to_string())
}

/// `Something(…).method` — resolve `Something` from the `)` at `close_paren_idx`.
fn entity_before_dot_after_close_paren(s: &str, close_paren_idx: usize) -> Option<String> {
    let bytes = s.as_bytes();
    let mut depth = 1usize;
    let mut j = close_paren_idx;
    while j > 0 {
        j -= 1;
        match bytes[j] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    let (start, end) = entity_ident_before_open_paren(s, j)?;
                    return Some(s[start..end].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn entity_ident_before_open_paren(s: &str, open_paren: usize) -> Option<(usize, usize)> {
    if open_paren == 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut end = open_paren;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == end {
        None
    } else {
        Some((start, end))
    }
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

/// Build a [`DomainExposureSession`] from REPL/eval [`FocusSpec`], using the **same** `e#`/`m#`/`p#`
/// rules as HTTP/MCP execute: **sorted** entity names, **no** 2-hop neighbourhood expansion.
///
/// This keeps REPL and execute symbol indices aligned when the same seed set is used (`Single(s)`
/// ≡ one seed, `Seeds` ≡ sorted list). Use multiple seeds or incremental exposure if you need more
/// entities in DOMAIN.
pub fn domain_exposure_session_from_focus(
    cgs: &CGS,
    focus: FocusSpec<'_>,
) -> DomainExposureSession {
    match focus {
        FocusSpec::All => {
            let mut names: Vec<&str> = cgs
                .entities
                .iter()
                .filter(|(_, ent)| !ent.abstract_entity)
                .map(|(n, _)| n.as_str())
                .collect();
            names.sort();
            DomainExposureSession::new(cgs, "", &names)
        }
        FocusSpec::Single(s) => DomainExposureSession::new(cgs, "", &[s]),
        FocusSpec::Seeds(seeds) => {
            if seeds.is_empty() {
                return domain_exposure_session_from_focus(cgs, FocusSpec::All);
            }
            let mut v: Vec<&str> = seeds.to_vec();
            v.sort();
            v.dedup();
            DomainExposureSession::new(cgs, "", &v)
        }
        FocusSpec::SeedsExact(seeds) => {
            if seeds.is_empty() {
                return domain_exposure_session_from_focus(cgs, FocusSpec::All);
            }
            let mut v: Vec<&str> = seeds.to_vec();
            v.sort();
            v.dedup();
            DomainExposureSession::new(cgs, "", &v)
        }
    }
}

/// When `symbol_tuning` is true (same as [`crate::prompt_render::RenderConfig::uses_symbols`]: **compact** or **tsv** [`crate::prompt_render::PromptRenderMode`]), build the map used for prompts and pre-parse expansion.
pub fn symbol_map_for_prompt(
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> Option<SymbolMap> {
    if !symbol_tuning {
        return None;
    }
    Some(domain_exposure_session_from_focus(cgs, focus).to_symbol_map())
}

/// Owned entity names for prompt surface metrics and DOMAIN line counts, plus optional
/// [`DomainExposureSession`] when `symbol_tuning` is true (execute-parity slice; mirrors symbolic render modes); otherwise names from
/// [`entity_slices_for_render`] (2-hop for `Single` / `Seeds` when not exact).
pub fn resolve_prompt_surface_entities(
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> (Vec<String>, Option<DomainExposureSession>) {
    if symbol_tuning {
        let exp = domain_exposure_session_from_focus(cgs, focus);
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
pub struct DomainExposureSession {
    /// Cumulative allowed DOMAIN surface for filtered (`intent`) sessions; full closure for legacy paths.
    pub surface: ExposureSurface,
    /// Entities included in symbol space (order = `e1`, `e2`, …).
    pub entities: Vec<String>,
    /// Catalog registry `entry_id` for each row in [`Self::entities`] (same length). Disambiguates
    /// which [`crate::CgsContext`] owns the CGS entity when multiple catalogs are federated.
    pub entity_catalog_entry_ids: Vec<String>,
    /// Owning [`CGS`] per catalog `entry_id` (same keys as [`Self::entity_catalog_entry_ids`] values).
    catalog_cgs: IndexMap<String, Arc<CGS>>,
    sym_to_entity: IndexMap<String, String>,
    /// `(registry entry_id, CGS entity name)` → opaque `e#`.
    qualified_entity_to_sym: IndexMap<(String, String), String>,
    method_to_sym: IndexMap<(String, String, String), String>,
    sym_to_method: IndexMap<String, (String, String, String)>,
    ident_to_sym: IndexMap<String, String>,
    sym_to_ident: IndexMap<String, String>,
    anchor_scoped_method_sym: HashMap<(String, String), String>,
    /// Share fingerprint → opaque `p#` (append-only; never renumbered). Registry-backed slots key on
    /// `(values row, wire)`; see [`slot_symbol_allocation_fingerprint`].
    slot_fingerprint_to_sym: IndexMap<String, String>,
    /// Share fingerprint → representative slot metadata (append-only; first occurrence wins).
    fingerprint_meta: IndexMap<String, IdentMetadata>,
    /// Concrete slot occurrence → metadata. Preserves every `(entity, slot)` binding even when
    /// multiple occurrences intentionally share one fingerprint / `p#`.
    slot_occurrence_meta: IndexMap<String, IdentMetadata>,
    entity_field_to_sym: HashMap<(String, String, String), String>,
    relation_to_sym: HashMap<(String, String, String), String>,
    cap_param_to_sym: HashMap<(String, String, String, String), String>,
    /// `(catalog_entry_id|vr:value_ref)` → opaque `v#` (append-only).
    value_domain_fp_to_sym: IndexMap<String, String>,
    /// Fingerprint → representative registry-backed metadata for `v#` gloss text.
    value_domain_fp_to_repr_meta: IndexMap<String, IdentMetadata>,
    /// Memoized [`SymbolMap`] for this session; cleared in [`Self::expose_entities`].
    symbol_map_cache: RwLock<Option<Arc<SymbolMap>>>,
}

impl Clone for DomainExposureSession {
    fn clone(&self) -> Self {
        Self {
            surface: self.surface.clone(),
            entities: self.entities.clone(),
            entity_catalog_entry_ids: self.entity_catalog_entry_ids.clone(),
            catalog_cgs: self.catalog_cgs.clone(),
            sym_to_entity: self.sym_to_entity.clone(),
            qualified_entity_to_sym: self.qualified_entity_to_sym.clone(),
            method_to_sym: self.method_to_sym.clone(),
            sym_to_method: self.sym_to_method.clone(),
            ident_to_sym: self.ident_to_sym.clone(),
            sym_to_ident: self.sym_to_ident.clone(),
            anchor_scoped_method_sym: self.anchor_scoped_method_sym.clone(),
            slot_fingerprint_to_sym: self.slot_fingerprint_to_sym.clone(),
            fingerprint_meta: self.fingerprint_meta.clone(),
            slot_occurrence_meta: self.slot_occurrence_meta.clone(),
            entity_field_to_sym: self.entity_field_to_sym.clone(),
            relation_to_sym: self.relation_to_sym.clone(),
            cap_param_to_sym: self.cap_param_to_sym.clone(),
            value_domain_fp_to_sym: self.value_domain_fp_to_sym.clone(),
            value_domain_fp_to_repr_meta: self.value_domain_fp_to_repr_meta.clone(),
            symbol_map_cache: RwLock::new(None),
        }
    }
}

impl DomainExposureSession {
    /// First wave: assign symbols for `entity_names_in_order` (typically sorted seeds from the client).
    /// `catalog_entry_id` is the registry row for this graph (`""` when not using a multi-entry catalog).
    pub fn new(cgs: &CGS, catalog_entry_id: &str, entity_names_in_order: &[&str]) -> Self {
        let mut s = Self {
            surface: ExposureSurface::default(),
            entities: Vec::new(),
            entity_catalog_entry_ids: Vec::new(),
            catalog_cgs: IndexMap::new(),
            sym_to_entity: IndexMap::new(),
            qualified_entity_to_sym: IndexMap::new(),
            method_to_sym: IndexMap::new(),
            sym_to_method: IndexMap::new(),
            ident_to_sym: IndexMap::new(),
            sym_to_ident: IndexMap::new(),
            anchor_scoped_method_sym: HashMap::new(),
            slot_fingerprint_to_sym: IndexMap::new(),
            fingerprint_meta: IndexMap::new(),
            slot_occurrence_meta: IndexMap::new(),
            entity_field_to_sym: HashMap::new(),
            relation_to_sym: HashMap::new(),
            cap_param_to_sym: HashMap::new(),
            value_domain_fp_to_sym: IndexMap::new(),
            value_domain_fp_to_repr_meta: IndexMap::new(),
            symbol_map_cache: RwLock::new(None),
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
            sym_to_entity: IndexMap::new(),
            qualified_entity_to_sym: IndexMap::new(),
            method_to_sym: IndexMap::new(),
            sym_to_method: IndexMap::new(),
            ident_to_sym: IndexMap::new(),
            sym_to_ident: IndexMap::new(),
            anchor_scoped_method_sym: HashMap::new(),
            slot_fingerprint_to_sym: IndexMap::new(),
            fingerprint_meta: IndexMap::new(),
            slot_occurrence_meta: IndexMap::new(),
            entity_field_to_sym: HashMap::new(),
            relation_to_sym: HashMap::new(),
            cap_param_to_sym: HashMap::new(),
            value_domain_fp_to_sym: IndexMap::new(),
            value_domain_fp_to_repr_meta: IndexMap::new(),
            symbol_map_cache: RwLock::new(None),
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
        *self
            .symbol_map_cache
            .write()
            .expect("symbol_map_cache lock poisoned") = None;
        for n in names {
            let qkey = (catalog_entry_id.to_string(), (*n).to_string());
            if self.qualified_entity_to_sym.contains_key(&qkey) {
                continue;
            }
            let Some(ent) = owning_cgs.get_entity(n) else {
                continue;
            };
            if ent.abstract_entity {
                continue;
            }
            let i = self.entities.len() + 1;
            let sym = format!("e{i}");
            self.entities.push((*n).to_string());
            self.entity_catalog_entry_ids
                .push(catalog_entry_id.to_string());
            self.qualified_entity_to_sym.insert(qkey, sym.clone());
            self.sym_to_entity.insert(sym, (*n).to_string());
        }
        self.union_legacy_surface_for_entities(catalog_entry_id, names);
        self.assign_new_methods_and_idents(cgs_layers);
        self.rebuild_anchor_scoped_method_labels(cgs_layers);
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
        *self
            .symbol_map_cache
            .write()
            .expect("symbol_map_cache lock poisoned") = None;
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
            let qkey = (catalog_entry_id.to_string(), (*n).to_string());
            if self.qualified_entity_to_sym.contains_key(&qkey) {
                continue;
            }
            let Some(ent) = owning_cgs.get_entity(n) else {
                continue;
            };
            if ent.abstract_entity {
                continue;
            }
            entities_added += 1;
            let i = self.entities.len() + 1;
            let sym = format!("e{i}");
            self.entities.push((*n).to_string());
            self.entity_catalog_entry_ids
                .push(catalog_entry_id.to_string());
            self.qualified_entity_to_sym.insert(qkey, sym.clone());
            self.sym_to_entity.insert(sym, (*n).to_string());
        }
        self.assign_new_methods_and_idents(cgs_layers);
        self.rebuild_anchor_scoped_method_labels(cgs_layers);
        ExposureAppendReport { entities_added }
    }

    fn assign_new_methods_and_idents(&mut self, cgs_layers: &[&CGS]) {
        let _ = cgs_layers;
        let mut new_triples: Vec<(String, String, String)> = Vec::new();
        for cap_key in self.surface.capabilities.iter() {
            let Some(cgs) = self.catalog_cgs.get(&cap_key.entry_id) else {
                continue;
            };
            let Some(cap) = cgs.capabilities.get(&cap_key.capability) else {
                continue;
            };
            let kebab = capability_method_label_kebab(cap);
            let triple = (
                cap_key.entry_id.clone(),
                cap.domain.to_string(),
                kebab.clone(),
            );
            if !self.method_to_sym.contains_key(&triple) {
                new_triples.push(triple);
            }
        }
        new_triples.sort();
        let mut next_m = self.sym_to_method.len() + 1;
        for triple in new_triples {
            let sym = format!("m{next_m}");
            next_m += 1;
            self.method_to_sym.insert(triple.clone(), sym.clone());
            self.sym_to_method.insert(sym, triple);
        }

        self.assign_new_slot_symbols();
    }

    fn assign_new_slot_symbols(&mut self) {
        let mut collected: Vec<IdentMetadata> =
            collect_slot_metas_for_surface(&self.catalog_cgs, &self.surface);
        collected.sort_by(|a, b| {
            slot_symbol_allocation_fingerprint(a).cmp(&slot_symbol_allocation_fingerprint(b))
        });
        let mut by_fp: IndexMap<String, IdentMetadata> = IndexMap::new();
        for m in collected {
            self.slot_occurrence_meta
                .entry(slot_occurrence_key(&m))
                .or_insert_with(|| m.clone());
            let fp = slot_symbol_allocation_fingerprint(&m);
            by_fp.entry(fp).or_insert(m);
        }
        for (fp, meta) in &by_fp {
            self.fingerprint_meta
                .entry(fp.clone())
                .or_insert_with(|| meta.clone());
        }

        // `v#`: one symbol per `(catalog_entry_id, value_ref)` seen in this wave's slot table.
        let mut value_fps_in_wave: IndexMap<String, IdentMetadata> = IndexMap::new();
        for meta in by_fp.values() {
            if let Some(vfp) = meta.value_domain_allocation_fp() {
                value_fps_in_wave
                    .entry(vfp)
                    .or_insert_with(|| (*meta).clone());
            }
        }
        let mut new_v_fps: Vec<String> = value_fps_in_wave
            .keys()
            .filter(|fp| !self.value_domain_fp_to_sym.contains_key(*fp))
            .cloned()
            .collect();
        new_v_fps.sort();
        let base_v = self.value_domain_fp_to_sym.len();
        for (i, fp) in new_v_fps.iter().enumerate() {
            let sym = format!("v{}", base_v + i + 1);
            self.value_domain_fp_to_sym.insert(fp.clone(), sym);
            self.value_domain_fp_to_repr_meta
                .entry(fp.clone())
                .or_insert_with(|| value_fps_in_wave.get(fp).expect("vfp").clone());
        }

        let mut new_fps: Vec<String> = by_fp
            .keys()
            .filter(|fp| !self.slot_fingerprint_to_sym.contains_key(*fp))
            .cloned()
            .collect();
        new_fps.sort();
        let mut next_p = self.slot_fingerprint_to_sym.len() + 1;
        for fp in new_fps {
            let sym = format!("p{next_p}");
            next_p += 1;
            self.slot_fingerprint_to_sym.insert(fp, sym);
        }
        self.rebuild_parameter_symbol_maps();
    }

    fn rebuild_parameter_symbol_maps(&mut self) {
        self.entity_field_to_sym.clear();
        self.relation_to_sym.clear();
        self.cap_param_to_sym.clear();
        self.sym_to_ident.clear();
        self.ident_to_sym.clear();
        let mut fp_order: Vec<String> = self.slot_fingerprint_to_sym.keys().cloned().collect();
        fp_order.sort();
        for fp in fp_order {
            let Some(meta) = self.fingerprint_meta.get(&fp) else {
                continue;
            };
            let Some(sym) = self.slot_fingerprint_to_sym.get(&fp) else {
                continue;
            };
            self.sym_to_ident
                .insert(sym.clone(), meta.wire_name().to_string());
            self.ident_to_sym
                .entry(meta.wire_name().to_string())
                .or_insert_with(|| sym.clone());
            match meta.allocation_ident_role() {
                IdentRole::EntityField => {
                    self.entity_field_to_sym.insert(
                        (
                            meta.catalog_entry_id().to_string(),
                            meta.entity().as_str().to_string(),
                            meta.wire_name().to_string(),
                        ),
                        sym.clone(),
                    );
                }
                IdentRole::Relation { .. } => {
                    self.relation_to_sym.insert(
                        (
                            meta.catalog_entry_id().to_string(),
                            meta.entity().as_str().to_string(),
                            meta.wire_name().to_string(),
                        ),
                        sym.clone(),
                    );
                }
                IdentRole::CapabilityParam { capability } => {
                    self.cap_param_to_sym.insert(
                        (
                            meta.catalog_entry_id().to_string(),
                            meta.entity().as_str().to_string(),
                            capability.as_str().to_string(),
                            meta.wire_name().to_string(),
                        ),
                        sym.clone(),
                    );
                }
            }
        }
        for meta in self.slot_occurrence_meta.values() {
            let fp = slot_symbol_allocation_fingerprint(meta);
            let Some(sym) = self.slot_fingerprint_to_sym.get(&fp) else {
                continue;
            };
            match meta.allocation_ident_role() {
                IdentRole::EntityField => {
                    self.entity_field_to_sym.insert(
                        (
                            meta.catalog_entry_id().to_string(),
                            meta.entity().as_str().to_string(),
                            meta.wire_name().to_string(),
                        ),
                        sym.clone(),
                    );
                }
                IdentRole::Relation { .. } => {
                    self.relation_to_sym.insert(
                        (
                            meta.catalog_entry_id().to_string(),
                            meta.entity().as_str().to_string(),
                            meta.wire_name().to_string(),
                        ),
                        sym.clone(),
                    );
                }
                IdentRole::CapabilityParam { capability } => {
                    self.cap_param_to_sym.insert(
                        (
                            meta.catalog_entry_id().to_string(),
                            meta.entity().as_str().to_string(),
                            capability.as_str().to_string(),
                            meta.wire_name().to_string(),
                        ),
                        sym.clone(),
                    );
                }
            }
        }
    }

    fn rebuild_anchor_scoped_method_labels(&mut self, cgs_layers: &[&CGS]) {
        let _ = cgs_layers;
        self.anchor_scoped_method_sym.clear();
        let full_refs: Vec<&str> = self.entities.iter().map(|s| s.as_str()).collect();
        let full_set: HashSet<&str> = full_refs.iter().copied().collect();
        for i in 0..self.entities.len() {
            let dom = self.entities[i].as_str();
            if !full_set.contains(dom) {
                continue;
            }
            let entry_id = self.entity_catalog_entry_ids[i].as_str();
            let Some(cgs) = self.catalog_cgs.get(entry_id) else {
                continue;
            };
            let Some(names) = cgs.capability_names_by_domain().get(dom) else {
                continue;
            };
            for cap_name in names {
                let Some(cap) = cgs.capabilities.get(cap_name) else {
                    continue;
                };
                let cap_key = ExposureCapabilityKey {
                    entry_id: entry_id.to_string(),
                    domain: EntityName::from(dom),
                    capability: cap_name.clone(),
                };
                if !self.surface.capabilities.contains(&cap_key) {
                    continue;
                }
                if cap.kind != CapabilityKind::Create {
                    continue;
                }
                let pvars =
                    crate::schema::path_var_names_from_mapping_json(&cap.mapping.template.0);
                if pvars.len() != 1 {
                    continue;
                }
                let pv = pvars[0].as_str();
                let Some(anchor_lower) = pv.strip_suffix("_id") else {
                    continue;
                };
                let Some(anchor_entity) = cgs
                    .entities
                    .keys()
                    .find(|e| e.as_str().to_lowercase() == anchor_lower)
                else {
                    continue;
                };
                let anchor = anchor_entity.as_str();
                if !full_set.contains(anchor) {
                    continue;
                }
                if cap.domain.as_str() == anchor {
                    continue;
                }
                let Some(is) = cap.input_schema.as_ref() else {
                    continue;
                };
                let InputType::Object { fields, .. } = &is.input_type else {
                    continue;
                };
                let req: Vec<_> = fields.iter().filter(|f| f.required).collect();
                if req.len() != 1 {
                    continue;
                }
                let Ok(nv) = cgs.named_value_for_slot(req[0]) else {
                    continue;
                };
                if nv.field_type != FieldType::String {
                    continue;
                }
                let kebab = capability_method_label_kebab(cap);
                let Some(sym) = self.method_to_sym.get(&(
                    entry_id.to_string(),
                    cap.domain.to_string(),
                    kebab.clone(),
                )) else {
                    continue;
                };
                self.anchor_scoped_method_sym
                    .insert((anchor.to_string(), sym.clone()), kebab);
            }
        }
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
        let mut p_sym_to_value_sym = HashMap::new();
        for (fp, p_sym) in &self.slot_fingerprint_to_sym {
            let Some(meta) = self.fingerprint_meta.get(fp) else {
                continue;
            };
            let Some(vfp) = meta.value_domain_allocation_fp() else {
                continue;
            };
            if let Some(v_sym) = self.value_domain_fp_to_sym.get(&vfp) {
                p_sym_to_value_sym.insert(p_sym.clone(), v_sym.clone());
            }
        }

        let value_domain_fp_to_sym = self.value_domain_fp_to_sym.clone();
        let mut value_sym_to_fp: IndexMap<String, String> = IndexMap::new();
        for (fp, vs) in &value_domain_fp_to_sym {
            value_sym_to_fp.insert(vs.clone(), fp.clone());
        }

        let mut sm = SymbolMap {
            sym_to_entity: self.sym_to_entity.clone(),
            qualified_entity_to_sym: self.qualified_entity_to_sym.clone(),
            sym_to_method: self.sym_to_method.clone(),
            method_to_sym: self.method_to_sym.clone(),
            anchor_scoped_method_sym: self.anchor_scoped_method_sym.clone(),
            sym_to_ident: self.sym_to_ident.clone(),
            ident_to_sym: self.ident_to_sym.clone(),
            entity_field_to_sym: self.entity_field_to_sym.clone(),
            relation_to_sym: self.relation_to_sym.clone(),
            cap_param_to_sym: self.cap_param_to_sym.clone(),
            value_domain_fp_to_sym,
            value_sym_to_fp,
            p_sym_to_value_sym,
            value_sym_gloss: IndexMap::new(),
        };

        for (fp, vsym) in &sm.value_domain_fp_to_sym {
            let Some(meta) = self.value_domain_fp_to_repr_meta.get(fp) else {
                continue;
            };
            let nv_desc = self.named_value_row_description(meta);
            let cgs_opt = self
                .catalog_cgs
                .get(meta.catalog_entry_id())
                .map(|arc| arc.as_ref());
            if let Some(g) = meta.render_value_domain_row_gloss(&nv_desc, Some(&sm), cgs_opt) {
                sm.value_sym_gloss.insert(vsym.clone(), g);
            }
        }
        sm
    }

    /// [`IdentMetadata`] for `full_entities`, aligned with this session’s slot table (avoids a second CGS walk).
    pub(crate) fn ident_metadata_for_exposure_entities(
        &self,
        full_entities: &[&str],
    ) -> HashMap<IdentMetaKey, IdentMetadata> {
        let set: HashSet<&str> = full_entities.iter().copied().collect();
        let mut out = HashMap::new();
        for meta in self.slot_occurrence_meta.values() {
            if !set.contains(meta.entity().as_str()) {
                continue;
            }
            let k = (
                meta.catalog_entry_id().to_string(),
                meta.entity().clone(),
                meta.wire_name().to_string(),
            );
            out.entry(k).or_insert_with(|| meta.clone());
        }
        out
    }

    /// Shared [`SymbolMap`] for this exposure session (memoized until the next [`Self::expose_entities`]).
    pub fn symbol_map_arc(&self) -> Arc<SymbolMap> {
        self.symbol_map_arc_cross(None, None).0
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
        {
            let r = self
                .symbol_map_cache
                .read()
                .expect("symbol_map_cache lock poisoned");
            if let Some(arc) = r.as_ref() {
                return (Arc::clone(arc), None);
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
            .symbol_map_cache
            .write()
            .expect("symbol_map_cache lock poisoned");
        *w = Some(Arc::clone(&built));
        (built, lru_hit)
    }

    /// Snapshot for [`expand_path_symbols`] — matches DOMAIN lines for this session.
    pub fn to_symbol_map(&self) -> SymbolMap {
        (*self.symbol_map_arc()).clone()
    }

    /// Owning `(catalog entry id, CGS entity name)` for an exposed **entity name** (aligned with
    /// `e#` / DOMAIN rows). Prefer this over [`Self::catalog_entry_id_for_entity`] — catalog
    /// ownership is always a pair, never “catalog derivable from entity string alone.”
    pub fn qualified_entity_for_exposed_entity(
        &self,
        entity_name: &str,
    ) -> Option<crate::QualifiedEntityKey> {
        self.entities
            .iter()
            .zip(self.entity_catalog_entry_ids.iter())
            .find(|(e, _)| e.as_str() == entity_name)
            .map(|(_, id)| crate::QualifiedEntityKey::new(id.clone(), entity_name.to_string()))
    }

    /// Registry `entry_id` for an exposed **entity name** (aligned with `e#` / DOMAIN table order).
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

fn hash_exposure_session_rows(exposure: &DomainExposureSession) -> u64 {
    let mut h = DefaultHasher::new();
    for (e, row) in exposure
        .entities
        .iter()
        .zip(&exposure.entity_catalog_entry_ids)
    {
        e.hash(&mut h);
        row.hash(&mut h);
    }
    exposure.surface.fingerprint().hash(&mut h);
    h.finish()
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
    exposure: &DomainExposureSession,
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
    exposure: &DomainExposureSession,
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

/// Expand using a [`DomainExposureSession`] snapshot (HTTP execute / MCP); ignores [`FocusSpec`]. When `symbol_tuning` is false, only annotation stripping runs (canonical / tests).
pub fn expand_expr_for_domain_session(
    input: &str,
    session: &DomainExposureSession,
    symbol_tuning: bool,
) -> String {
    let stripped = strip_prompt_expression_annotations(input);
    if !symbol_tuning {
        return stripped;
    }
    let map = session.symbol_map_arc();
    expand_path_symbols(&stripped, map.as_ref())
}

/// Strip human-only suffixes from pasted prompt examples (`;;` comment may include `=>` result type,
/// legacy `=>` before `;;`, `->` relation target hint).
pub fn strip_prompt_expression_annotations(input: &str) -> String {
    let trimmed = input.trim();
    // Expression is always before the first `;;` (result type now lives inside the comment).
    let no_cap = trimmed.split("  ;;  ").next().unwrap_or(trimmed).trim();
    // Legacy lines: `expr  =>  [e#]  ;;  …`
    let no_gloss = no_cap
        .rsplit_once("  =>  ")
        .map(|(a, _)| a.trim())
        .unwrap_or(no_cap);
    let no_or = no_gloss.split(" or ").next().unwrap_or(no_gloss).trim();
    let expr_only = no_or
        .split_once(" -> ")
        .map(|(a, _)| a.trim())
        .unwrap_or(no_or);
    expr_only.to_string()
}

/// Rebuild-or-skip expansion for interactive / HTTP paths (reconstructs the map each call). `symbol_tuning` matches [`crate::prompt_render::RenderConfig::uses_symbols`].
pub fn expand_expr_for_parse(
    input: &str,
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> String {
    let stripped = strip_prompt_expression_annotations(input);
    if !symbol_tuning {
        return stripped;
    }
    let exposure = domain_exposure_session_from_focus(cgs, focus);
    expand_expr_for_domain_session(&stripped, &exposure, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::schema::{
        CapabilityMapping, CapabilitySchema, FieldSchema, FieldValueKind, NamedValueSchema,
        ResourceSchema, ValueDomainKey,
    };
    use crate::CapabilityKind;

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
    fn overshow_entity_scoped_slot_maps_split_incompatible_id_slots() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let capture_item_id = map.ident_sym_entity_field("CaptureItem", "id");
        let profile_id = map.ident_sym_entity_field("Profile", "id");
        let pipeline_snapshot_id = map.ident_sym_entity_field("PipelineSnapshot", "id");
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
            map.ident_sym_unambiguous("workers"),
            Some(map.ident_sym_entity_field("PipelineSnapshot", "workers"))
        );
        assert_eq!(
            map.ident_sym_unambiguous("id"),
            None,
            "bare `id` should not collapse to a single p# when both int and str id slots exist"
        );
    }

    #[test]
    fn domain_exposure_session_keeps_entity_symbols_stable_across_waves() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let mut s = DomainExposureSession::new(&cgs, "", &["Pet"]);
        let pet_sym = s.to_symbol_map().entity_sym("Pet");
        s.expose_entities(&[&cgs], Arc::new(cgs.clone()), "", &["Store"]);
        assert_eq!(pet_sym, s.to_symbol_map().entity_sym("Pet"));
        assert_ne!(pet_sym, s.to_symbol_map().entity_sym("Store"));
    }

    /// `m#` / `p#` append-only invariants: adding a second entity must not renumber existing method
    /// or field slot symbols for the first entity.
    #[test]
    fn domain_exposure_session_keeps_method_and_field_symbols_stable_across_waves() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let mut s = DomainExposureSession::new(&cgs, "", &["Profile"]);
        let map0 = s.to_symbol_map();
        let display_p = map0.ident_sym_entity_field("Profile", "display_name");
        let get_m = map0.method_sym("Profile", "get");
        s.expose_entities(&[&cgs], Arc::new(cgs.clone()), "", &["RecordedContent"]);
        let map1 = s.to_symbol_map();
        assert_eq!(
            map1.ident_sym_entity_field("Profile", "display_name"),
            display_p
        );
        assert_eq!(map1.method_sym("Profile", "get"), get_m);
    }

    #[test]
    fn ident_metadata_from_exposure_matches_build_ident_metadata() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let sesh = DomainExposureSession::new(&cgs, "", &["Pet", "Store"]);
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
        let exp = DomainExposureSession::new(&cgs, "", &["Pet"]);
        let key = symbol_map_cache_key_single_catalog(&cgs, &exp);
        let (a, h1) = exp.symbol_map_arc_cross(Some(&cache), Some(key));
        assert_eq!(h1, Some(false));
        let exp2 = DomainExposureSession::new(&cgs, "", &["Pet"]);
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
        let exp = DomainExposureSession::new(&cgs, "", &["Pet"]);
        let key = symbol_map_cache_key_single_catalog(&cgs, &exp);
        let (_, h1) = exp.symbol_map_arc_cross(Some(&cache), Some(key));
        assert_eq!(h1, Some(false));
        cache.clear();
        let exp2 = DomainExposureSession::new(&cgs, "", &["Pet"]);
        let (_, h2) = exp2.symbol_map_arc_cross(Some(&cache), Some(key));
        assert_eq!(h2, Some(false));
    }

    #[test]
    fn petstore_roundtrip_expand() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let expr = format!("{}(42)", map.entity_sym("Pet"));
        let back = expand_path_symbols(&expr, &map);
        assert_eq!(back, "Pet(42)");
    }

    #[test]
    fn method_expand_requires_entity_context() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let pet = map.entity_sym("Pet");
        let m = map.method_sym("Pet", "upload-image");
        if m == "upload-image" {
            return;
        }
        let expr = format!("{}(1).{}()", pet, m);
        let back = expand_path_symbols(&expr, &map);
        assert!(back.contains("upload-image"), "got {back}");
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
        assert!(field_syms_in_expr("e1(42)").is_empty());
    }

    #[test]
    fn field_syms_for_teaching_row_includes_optional_from_legend() {
        assert_eq!(
            field_syms_for_teaching_row(
                r#"e1(42).m22(p37=$,..)"#,
                None,
                Some(r#"optional params: p18, p17 — Create a goal"#),
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
    fn render_gloss_capability_param_uses_wire_name_without_description() {
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
        assert_eq!(m.render_gloss(None), "str · payment_method_id");
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
        assert_eq!(m.render_gloss(None), "markdown · body");
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
        assert_eq!(m.render_gloss(None), "array[ref:Variant] · item_ids");
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
            invoke_preflight: None,
            discovery: None,
        })
        .unwrap();
        cgs.validate().expect("fixture CGS");
        let map = DomainExposureSession::new(&cgs, "fixture_entry", &["Widget"]).to_symbol_map();
        let p_foo = map.ident_sym_entity_field("Widget", "foo");
        let p_bar = map.ident_sym_entity_field("Widget", "bar");
        let v_foo = map
            .value_sym_for_p_sym(&p_foo)
            .expect("registry-backed foo maps to v#");
        let v_bar = map
            .value_sym_for_p_sym(&p_bar)
            .expect("registry-backed bar maps to v#");
        assert_eq!(v_foo, v_bar, "same value_ref → one v#");
        assert_eq!(
            map.value_domain_fp_for_v_sym(v_foo).unwrap(),
            "fixture_entry|vr:shared_sel_vtest"
        );
        let gloss = map.value_domain_gloss_for_v_sym(v_foo).expect("v gloss");
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
    fn domain_term_entity_roundtrips_display_with_symbol_map() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let dt = map.try_entity_domain_term("Pet").expect("Pet in map");
        assert_eq!(dt.to_string(), map.entity_sym("Pet"));
        assert!(matches!(dt, crate::DomainTerm::Entity(_, _)));
    }

    #[test]
    fn domain_term_method_matches_symbol_map_when_cgs_resolves() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let kebab = "upload-image";
        let m_str = map.method_sym("Pet", kebab);
        if m_str == kebab {
            return;
        }
        let dt = map
            .try_method_domain_term(&cgs, "Pet", kebab)
            .expect("method domain term");
        assert_eq!(dt.to_string(), m_str);
        assert!(matches!(dt, crate::DomainTerm::Method(_, _)));
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
        let mut s = DomainExposureSession::new(&cgs_a, "alpha", &["Pet"]);
        s.expose_entities(&[arc_b.as_ref()], arc_b.clone(), "beta", &["Pet"]);
        assert_eq!(s.entities.len(), 2);
        let map = s.to_symbol_map();
        let sa = map
            .qualified_entity_to_sym
            .get(&("alpha".into(), "Pet".into()))
            .expect("alpha Pet");
        let sb = map
            .qualified_entity_to_sym
            .get(&("beta".into(), "Pet".into()))
            .expect("beta Pet");
        assert_ne!(sa, sb);
    }

    #[test]
    fn intent_filtered_domain_session_has_narrower_capability_surface_than_legacy() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let refs: &[&str] = &["PromptRun"];
        let legacy = DomainExposureSession::new(&cgs, "overshow", refs);
        let delta = crate::discovery::derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "list profiles read metadata only",
            &["PromptRun".into()],
            &["PromptRun".into()],
            None,
        );
        let filtered = DomainExposureSession::new_with_intent_delta(&cgs, "overshow", refs, delta);
        assert!(
            filtered.surface.capabilities.len() < legacy.surface.capabilities.len(),
            "expected fewer admitted capabilities than legacy full closure (prompt_run_create omitted)"
        );
    }
}
