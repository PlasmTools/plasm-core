//! Canonical teaching field-gloss collection — single emit path for registry / typed / opaque slots.

use std::collections::{HashMap, HashSet};

use crate::identity::EntityName;
use crate::schema::CGS;
use crate::symbol_tuning::{IdentMetaKey, IdentMetadata, SymbolMap};

use super::gloss_dedup::*;
use super::{is_union_ctor_teaching_surface_line, TeachingFieldGloss};

/// Tracks gloss lines emitted before teaching example rows (first-use only).
#[derive(Default)]
pub(crate) struct GlossEmitLedger {
    pub(crate) registry_p_sym_alias: HashMap<String, String>,
    pub(crate) registry_value_gloss_canonical_v: HashMap<String, String>,
    pub(crate) registry_v_sym_alias: HashMap<String, String>,
    pub(crate) defined_value_domains: HashSet<String>,
    pub(crate) structural_value_domains: HashSet<ValueDomainStructuralKey>,
    pub(crate) global_gloss_identities: HashSet<GlossEmitIdentity>,
    pub(crate) canonical_rows: HashMap<GlossEmitIdentity, TeachingFieldGloss>,
    /// `p#` / `r#` already shown on a prior teaching-row LHS (witness or projection bracket).
    pub(crate) demonstrated_lhs_syms: HashSet<String>,
}

impl GlossEmitLedger {
    pub(crate) fn alias_row(
        &self,
        source_sym: &str,
        alias_sym: &str,
        out: &[TeachingFieldGloss],
    ) -> Option<TeachingFieldGloss> {
        if source_sym == alias_sym {
            return None;
        }
        let source = out
            .iter()
            .rev()
            .find(|g| g.symbol == source_sym)
            .or_else(|| {
                self.canonical_rows
                    .values()
                    .find(|g| g.symbol == source_sym)
            })?;
        let mut alias = source.clone();
        alias.symbol = alias_sym.to_string();
        Some(alias)
    }
}

pub(crate) struct GlossScratch<'a> {
    pub(crate) field_gloss: &'a mut Vec<TeachingFieldGloss>,
    pub(crate) state: &'a mut GlossEmitLedger,
    pub(crate) map: &'a SymbolMap,
    pub(crate) meta: &'a HashMap<IdentMetaKey, IdentMetadata>,
    pub(crate) catalog_entry_id: &'a str,
    pub(crate) entity: &'a str,
    pub(crate) cgs: &'a CGS,
}

impl GlossScratch<'_> {
    pub(crate) fn emit_before_teaching_example(
        &mut self,
        expr: &str,
        cap_legend: Option<&str>,
        result_gloss: Option<&str>,
        optional_param_syms: &[String],
    ) {
        emit_field_def_lines_before_example(
            self.field_gloss,
            expr,
            cap_legend,
            result_gloss,
            optional_param_syms,
            self.map,
            self.entity,
            self.catalog_entry_id,
            self.meta,
            self.state,
            self.cgs,
        );
    }
}

pub(crate) struct GlossEmitCtx<'a> {
    pub out: &'a mut Vec<TeachingFieldGloss>,
    pub state: &'a mut GlossEmitLedger,
    pub map: &'a SymbolMap,
    pub ident_meta: &'a HashMap<IdentMetaKey, IdentMetadata>,
    pub cgs: &'a CGS,
    pub catalog_entry_id: &'a str,
    pub entity: &'a str,
    pub skip_p_gloss: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn commit_teaching_field_gloss_row(
    out: &mut Vec<TeachingFieldGloss>,
    symbol: String,
    meaning: FieldGlossMeaning,
    legend_display: Option<&str>,
    canonical_entity: &str,
    catalog_entry_id: &str,
    symbol_map: Option<&SymbolMap>,
    cgs: Option<&CGS>,
    is_inline_union_summary: bool,
    emit_state: &mut GlossEmitLedger,
) -> bool {
    if matches!(
        &meaning,
        FieldGlossMeaning::RegistryBackedSlot {
            point_of_use: PointOfUseProse::None,
            ..
        }
    ) {
        if let FieldGlossMeaning::RegistryBackedSlot { value, .. } = &meaning {
            if emit_state.structural_value_domains.contains(value) {
                return false;
            }
        }
    }
    let identity = gloss_emit_identity_from_parts(
        &meaning,
        symbol.as_str(),
        catalog_entry_id,
        canonical_entity,
    );
    if !emit_state.global_gloss_identities.insert(identity.clone()) {
        if let Some(row) = emit_state.canonical_rows.get(&identity) {
            let mut reuse = row.clone();
            reuse.symbol = symbol.clone();
            out.push(reuse);
            return true;
        }
        return false;
    }
    if let FieldGlossMeaning::ValueDomain(key) = &meaning {
        emit_state.structural_value_domains.insert(key.clone());
    }
    let mut row = TeachingFieldGloss {
        symbol: symbol.clone(),
        field_type: String::new(),
        allowed_values: String::new(),
        description: String::new(),
        is_inline_union_summary,
        meaning: meaning.clone(),
        catalog_entry_id: catalog_entry_id.to_string(),
        entity: canonical_entity.to_string(),
        emit_identity: Some(identity.clone()),
    };
    meaning.apply_to_teaching_field_gloss(&mut row, symbol_map, cgs);
    if let Some(text) = legend_display.filter(|s| !s.is_empty()) {
        row.description = text.to_string();
    }
    emit_state.canonical_rows.insert(identity, row.clone());
    out.push(row);
    true
}

fn resolve_slot_metadata<'a>(
    ctx: &GlossEmitCtx<'a>,
    sym: &str,
    field_name: &str,
) -> Option<IdentMetadata> {
    let en = EntityName::from(ctx.entity.to_string());
    let cid = ctx.catalog_entry_id.to_string();
    ctx.map
        .capability_param_quad_for_p_sym(sym)
        .and_then(|(eid, dom, cap, path)| {
            if !eid.is_empty() && eid.as_str() != ctx.catalog_entry_id {
                return None;
            }
            crate::symbol_tuning::ident_metadata_for_capability_input_path(
                ctx.cgs,
                dom.as_str(),
                cap.as_str(),
                path.as_str(),
            )
        })
        .or_else(|| {
            ctx.map
                .capability_param_key_for_p_sym(sym)
                .and_then(|(dom, w)| {
                    ctx.ident_meta
                        .get(&(cid.clone(), dom.clone(), w.clone()))
                        .cloned()
                })
        })
        .or_else(|| {
            ctx.ident_meta
                .get(&(cid, en.clone(), field_name.to_string()))
                .cloned()
        })
}

fn try_emit_value_domain_gloss(
    ctx: &mut GlossEmitCtx<'_>,
    vs: &str,
    vg: &str,
    meta: &IdentMetadata,
) -> bool {
    let Some(v_canon) = meaning_canonical_sym_for_emit(
        vg,
        vs,
        &mut ctx.state.registry_value_gloss_canonical_v,
        &mut ctx.state.registry_v_sym_alias,
    ) else {
        return false;
    };
    if !ctx.state.defined_value_domains.insert(v_canon.clone()) {
        return false;
    }
    if let Some(key) = ValueDomainStructuralKey::from_registry_meta(meta) {
        ctx.state.structural_value_domains.insert(key);
    }
    push_teaching_field_gloss_row(
        ctx.out,
        v_canon,
        vg,
        ctx.entity,
        ctx.catalog_entry_id,
        Some(ctx.map),
        Some(ctx.ident_meta),
        Some(ctx.cgs),
        false,
        ctx.state,
    );
    true
}

fn attach_wire_projection_gloss_alias(ctx: &mut GlossEmitCtx<'_>, wire_sym: &str, slot_sym: &str) {
    if SymbolMap::is_opaque_p_sym(wire_sym) {
        return;
    }
    if let Some(alias) = ctx.state.alias_row(slot_sym, wire_sym, ctx.out) {
        ctx.out.push(alias);
    }
}

enum RegistryPSlotPlan {
    Skip,
    EmitWireSlot {
        value: ValueDomainStructuralKey,
        wire: String,
        point_of_use: PointOfUseProse,
    },
}

fn plan_registry_p_slot_emit(
    ctx: &GlossEmitCtx<'_>,
    sym: &str,
    meta: &IdentMetadata,
    _vs: &str,
) -> RegistryPSlotPlan {
    let nv_desc = values_row_description_for_meta(meta, ctx.cgs);
    match classify_registry_wire_gloss_role(sym, meta, ctx.cgs, nv_desc.as_str()) {
        WireGlossRole::RedundantWithValueDomain => RegistryPSlotPlan::Skip,
        WireGlossRole::EmitRegistrySlot {
            value,
            wire,
            point_of_use,
        } => RegistryPSlotPlan::EmitWireSlot {
            value,
            wire,
            point_of_use,
        },
        WireGlossRole::EmitTyped(_) => RegistryPSlotPlan::Skip,
    }
}

fn try_emit_registry_p_slot(
    ctx: &mut GlossEmitCtx<'_>,
    sym: &str,
    meta: &IdentMetadata,
    vs: &str,
) -> bool {
    let RegistryPSlotPlan::EmitWireSlot {
        value,
        wire,
        point_of_use,
    } = plan_registry_p_slot_emit(ctx, sym, meta, vs)
    else {
        return false;
    };
    if ctx.skip_p_gloss {
        return false;
    }
    commit_teaching_field_gloss_row(
        ctx.out,
        sym.to_string(),
        FieldGlossMeaning::RegistryBackedSlot {
            value,
            wire,
            point_of_use,
        },
        None,
        ctx.entity,
        ctx.catalog_entry_id,
        Some(ctx.map),
        Some(ctx.cgs),
        false,
        ctx.state,
    )
}

fn resolve_standard_gloss_meaning(
    ctx: &GlossEmitCtx<'_>,
    sym: &str,
    meta: Option<&IdentMetadata>,
) -> Option<FieldGlossMeaning> {
    let m = meta?;
    if let IdentMetadata::RegistryBacked { .. } = m {
        if let Some(key) = ValueDomainStructuralKey::from_registry_meta(m) {
            let _ = key;
        }
        let nv_desc = values_row_description_for_meta(m, ctx.cgs);
        match classify_registry_wire_gloss_role(sym, m, ctx.cgs, nv_desc.as_str()) {
            WireGlossRole::RedundantWithValueDomain => None,
            WireGlossRole::EmitRegistrySlot {
                value,
                wire,
                point_of_use,
            } if ctx
                .map
                .value_sym_for_teaching_gloss_key(ctx.catalog_entry_id, ctx.entity, sym)
                .is_some() =>
            {
                Some(FieldGlossMeaning::RegistryBackedSlot {
                    value,
                    wire,
                    point_of_use,
                })
            }
            WireGlossRole::EmitTyped(meaning) => Some(meaning),
            WireGlossRole::EmitRegistrySlot { .. } => Some(build_typed_field_meaning(m)),
        }
    } else {
        Some(build_typed_field_meaning(m))
    }
}

fn try_emit_teaching_gloss(ctx: &mut GlossEmitCtx<'_>, sym: &str) -> bool {
    if SymbolMap::is_opaque_r_sym(sym) {
        if ctx.skip_p_gloss {
            return false;
        }
        let wire = ctx
            .map
            .resolve_relation_ident(sym)
            .unwrap_or(sym)
            .to_string();
        let meta = resolve_slot_metadata(ctx, sym, wire.as_str());
        let meaning = meta
            .as_ref()
            .map(|m| FieldGlossMeaning::Relation {
                wire: wire.clone(),
                description: GlossDescription::from_trimmed(m.description()),
            })
            .unwrap_or(FieldGlossMeaning::Relation {
                wire,
                description: GlossDescription::from_trimmed(""),
            });
        return commit_teaching_field_gloss_row(
            ctx.out,
            sym.to_string(),
            meaning,
            None,
            ctx.entity,
            ctx.catalog_entry_id,
            Some(ctx.map),
            Some(ctx.cgs),
            false,
            ctx.state,
        );
    }

    let field_name = if sym.starts_with('r') {
        ctx.map.resolve_relation_ident(sym).unwrap_or(sym)
    } else {
        sym
    };
    let meta = resolve_slot_metadata(ctx, sym, field_name);

    if let (Some(m @ IdentMetadata::RegistryBacked { .. }), Some(vs)) = (
        meta.as_ref(),
        ctx.map
            .value_sym_for_teaching_gloss_key(ctx.catalog_entry_id, ctx.entity, sym),
    ) {
        let vg = ctx
            .map
            .value_domain_gloss_for_v_sym(&vs)
            .map(str::to_string)
            .or_else(|| {
                let d = values_row_description_for_meta(m, ctx.cgs);
                m.render_value_domain_row_gloss(d.as_str(), Some(ctx.map), Some(ctx.cgs))
            });
        if let Some(vg) = vg.as_deref() {
            try_emit_value_domain_gloss(ctx, &vs, vg, m);
        } else if let Some(key) = ValueDomainStructuralKey::from_registry_meta(m) {
            ctx.state.structural_value_domains.insert(key);
        }
        let p_emitted = try_emit_registry_p_slot(ctx, sym, m, &vs);
        attach_wire_projection_gloss_alias(ctx, sym, sym);
        if p_emitted {
            return true;
        }
        // Wire-named registry slots fall through to typed wire gloss (projection brackets use wire tokens).
    }

    if ctx.skip_p_gloss {
        return false;
    }

    let meaning = match meta.as_ref() {
        Some(m) => resolve_standard_gloss_meaning(ctx, sym, Some(m)),
        None => Some(FieldGlossMeaning::OpaqueLegend {
            description: field_name.to_string(),
        }),
    };
    let Some(meaning) = meaning else {
        return false;
    };
    if matches!(
        &meaning,
        FieldGlossMeaning::RegistryBackedSlot {
            point_of_use: PointOfUseProse::None,
            ..
        }
    ) {
        return false;
    }
    commit_teaching_field_gloss_row(
        ctx.out,
        sym.to_string(),
        meaning,
        None,
        ctx.entity,
        ctx.catalog_entry_id,
        Some(ctx.map),
        Some(ctx.cgs),
        false,
        ctx.state,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_field_def_lines_before_example(
    out: &mut Vec<TeachingFieldGloss>,
    expr: &str,
    cap_legend: Option<&str>,
    result_gloss: Option<&str>,
    optional_param_syms: &[String],
    map: &SymbolMap,
    entity: &str,
    catalog_entry_id: &str,
    ident_meta: &HashMap<IdentMetaKey, IdentMetadata>,
    state: &mut GlossEmitLedger,
    cgs: &CGS,
) {
    let current_lhs_syms = lhs_demonstrated_syms_for_teaching_expr(
        expr,
        result_gloss,
        Some(map),
        catalog_entry_id,
        entity,
    );
    let projection_witness_row = result_gloss.is_some_and(|g| g.contains("· projection"));
    let union_ctor_row = is_union_ctor_teaching_surface_line(expr);
    for sym in crate::symbol_tuning::teaching_slot_keys_for_teaching_row(
        expr,
        result_gloss,
        cap_legend,
        optional_param_syms,
        Some(map),
        catalog_entry_id,
        entity,
    ) {
        let teach_on_same_row = SymbolMap::is_opaque_r_sym(sym.as_str())
            || projection_witness_row
            || union_ctor_row
            || map.capability_param_quad_for_p_sym(sym.as_str()).is_some()
            || map.is_capability_param_wire_on_entity(catalog_entry_id, entity, sym.as_str());
        let skip_p_gloss = state.demonstrated_lhs_syms.contains(&sym)
            || (SymbolMap::is_opaque_p_sym(sym.as_str())
                && !teach_on_same_row
                && current_lhs_syms.contains(&sym));
        let mut ctx = GlossEmitCtx {
            out,
            state,
            map,
            ident_meta,
            cgs,
            catalog_entry_id,
            entity,
            skip_p_gloss,
        };
        let _ = try_emit_teaching_gloss(&mut ctx, sym.as_str());
    }
    state.demonstrated_lhs_syms.extend(current_lhs_syms);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_teaching_field_gloss_row(
    out: &mut Vec<TeachingFieldGloss>,
    symbol: String,
    legend_rhs: &str,
    canonical_entity: &str,
    catalog_entry_id: &str,
    symbol_map: Option<&SymbolMap>,
    ident_meta: Option<&HashMap<IdentMetaKey, IdentMetadata>>,
    cgs: Option<&CGS>,
    is_inline_union_summary: bool,
    emit_state: &mut GlossEmitLedger,
) {
    let legend = legend_rhs.trim();
    let is_opaque_p = SymbolMap::is_opaque_p_sym(symbol.as_str());
    let is_opaque_r = SymbolMap::is_opaque_r_sym(symbol.as_str());
    let is_opaque_v = symbol.starts_with('v')
        && symbol.len() > 1
        && symbol[1..].chars().all(|c| c.is_ascii_digit());

    if let (Some(map), Some(im), Some(cgs_ref)) = (symbol_map, ident_meta, cgs) {
        let mut ctx = GlossEmitCtx {
            out,
            state: emit_state,
            map,
            ident_meta: im,
            cgs: cgs_ref,
            catalog_entry_id,
            entity: canonical_entity,
            skip_p_gloss: false,
        };
        if (is_opaque_p || is_opaque_r) && try_emit_teaching_gloss(&mut ctx, symbol.as_str()) {
            return;
        }
    }

    if is_opaque_v {
        let field_name = symbol.clone();
        let meta = ident_meta.and_then(|im| {
            im.get(&(
                catalog_entry_id.to_string(),
                EntityName::from(canonical_entity.to_string()),
                field_name,
            ))
        });
        let meaning = meta
            .as_ref()
            .and_then(|m| {
                ValueDomainStructuralKey::from_registry_meta(m).map(FieldGlossMeaning::ValueDomain)
            })
            .unwrap_or_else(|| FieldGlossMeaning::OpaqueLegend {
                description: legend.to_string(),
            });
        commit_teaching_field_gloss_row(
            out,
            symbol,
            meaning,
            Some(legend),
            canonical_entity,
            catalog_entry_id,
            symbol_map,
            cgs,
            is_inline_union_summary,
            emit_state,
        );
        return;
    }

    if let (Some(map), Some(im), Some(cgs_ref)) = (symbol_map, ident_meta, cgs) {
        let mut ctx = GlossEmitCtx {
            out,
            state: emit_state,
            map,
            ident_meta: im,
            cgs: cgs_ref,
            catalog_entry_id,
            entity: canonical_entity,
            skip_p_gloss: false,
        };
        let _ = try_emit_teaching_gloss(&mut ctx, symbol.as_str());
    }
}
