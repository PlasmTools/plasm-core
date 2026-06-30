//! Filtered teaching-table synthesis for newly exposed mutators (ranked replay compact delta).

use std::collections::{BTreeSet, HashSet};

use indexmap::IndexMap;

use crate::symbol_tuning::{
    capability_exposure_param_pairs, field_syms_for_teaching_row, gloss_description_truncated,
    registry_backed_compact_wire_label, CapabilityParamSurfaceFilter, ExposureCapabilityKey,
    ExposureEntityKey, SymbolMap, TeachingExposureSession,
};
use crate::{CapabilityKind, CGS};

use super::{
    render_prompt_tsv_from_bundle, render_teaching_prompt_bundle_for_exposure,
    render_teaching_prompt_bundle_for_exposure_federated, EntityTeachingBlock,
    EntityTeachingExprRow, RenderConfig, TeachingFieldGloss, TeachingPromptBundle,
    TSV_TEACHING_TABLE_HEADER,
};

fn method_syms_for_new_capabilities(
    exp: &TeachingExposureSession,
    map: &SymbolMap,
    new_caps: &BTreeSet<ExposureCapabilityKey>,
) -> HashSet<String> {
    let mut out = HashSet::new();
    for cap_key in new_caps {
        if exp.catalog_cgs_for_entry(cap_key.entry_id.as_str()).is_none() {
            continue;
        };
        out.insert(map.method_sym_for(
            cap_key.entry_id.as_str(),
            cap_key.domain.as_str(),
            cap_key.capability.as_str(),
        ));
    }
    out
}

fn qualified_entity_for_block(
    exp: &TeachingExposureSession,
    entity_name: &str,
    new_caps: &BTreeSet<ExposureCapabilityKey>,
) -> Option<(String, String)> {
    let mut matches: Vec<(String, String)> = exp
        .entities
        .iter()
        .zip(exp.entity_catalog_entry_ids.iter())
        .filter(|(e, _)| e.as_str() == entity_name)
        .map(|(e, id)| (id.clone(), e.clone()))
        .collect();
    if matches.len() == 1 {
        return matches.pop();
    }
    let from_caps: BTreeSet<(String, String)> = new_caps
        .iter()
        .filter(|k| k.domain.as_str() == entity_name)
        .map(|k| (k.entry_id.clone(), k.domain.to_string()))
        .collect();
    if from_caps.len() == 1 {
        return from_caps.into_iter().next();
    }
    None
}

fn row_teaches_new_capability(
    row: &EntityTeachingExprRow,
    entry_id: &str,
    entity: &str,
    new_caps: &BTreeSet<ExposureCapabilityKey>,
    method_syms: &HashSet<String>,
) -> bool {
    if let Some(cap_wire) = row.meta.source_capability.as_deref() {
        let key = ExposureCapabilityKey {
            entry_id: entry_id.to_string(),
            domain: crate::EntityName::from(entity),
            capability: crate::CapabilityName::from(cap_wire),
        };
        if new_caps.contains(&key) {
            return true;
        }
    }
    let expr = row.teaching_expr.expression.as_str();
    method_syms.iter().any(|m| expr.contains(m.as_str()))
}

fn synthesize_param_gloss_row(
    exp: &TeachingExposureSession,
    cap_key: &ExposureCapabilityKey,
    wire: &str,
    sym: &str,
) -> TeachingFieldGloss {
    let ident = exp.ident_metadata_for_exposure_entities(&[cap_key.domain.as_str()]);
    let meta = ident.get(&(
        cap_key.entry_id.clone(),
        cap_key.domain.clone(),
        wire.to_string(),
    ));
    let field_type = meta
        .map(registry_backed_compact_wire_label)
        .unwrap_or_else(|| wire.to_string());
    let description = meta
        .map(|m| gloss_description_truncated(m.description().trim()))
        .filter(|d| !d.is_empty())
        .unwrap_or_default();
    TeachingFieldGloss {
        symbol: sym.to_string(),
        field_type,
        allowed_values: String::new(),
        description,
        is_inline_union_summary: false,
    }
}

fn gloss_rows_for_filtered_block(
    block: &EntityTeachingBlock,
    kept_rows: &[EntityTeachingExprRow],
    exp: &TeachingExposureSession,
    map: &SymbolMap,
    new_caps: &BTreeSet<ExposureCapabilityKey>,
) -> Vec<TeachingFieldGloss> {
    let mut needed: HashSet<String> = HashSet::new();
    for row in kept_rows {
        let legend = {
            let mut parts = Vec::new();
            if !row.teaching_expr.scope.is_empty() {
                parts.push(row.teaching_expr.scope.as_str());
            }
            if !row.teaching_expr.optional_params.is_empty() {
                parts.push(row.teaching_expr.optional_params.as_str());
            }
            let tail = row.teaching_expr.description.as_str();
            if parts.is_empty() {
                if tail.is_empty() {
                    None
                } else {
                    Some(tail.to_string())
                }
            } else {
                Some(format!("{} — {tail}", parts.join(" ")))
            }
        };
        for sym in field_syms_for_teaching_row(
            row.teaching_expr.expression.as_str(),
            None,
            legend.as_deref(),
        ) {
            needed.insert(sym);
        }
        if !row.teaching_expr.optional_params.is_empty() {
            for sym in
                field_syms_for_teaching_row(row.teaching_expr.optional_params.as_str(), None, None)
            {
                needed.insert(sym);
            }
        }
        if let Some(cap_wire) = row.meta.source_capability.as_deref() {
            for cap_key in new_caps {
                if cap_key.capability.as_str() != cap_wire {
                    continue;
                }
                let Some(cgs) = exp.catalog_cgs_for_entry(cap_key.entry_id.as_str()) else {
                    continue;
                };
                let Some(cap) = cgs.capabilities.get(cap_wire) else {
                    continue;
                };
                for (_, sym) in capability_exposure_param_pairs(
                    exp,
                    map,
                    cap_key,
                    cap,
                    CapabilityParamSurfaceFilter::AllOnSurface,
                ) {
                    needed.insert(sym);
                }
            }
        }
    }
    let mut out: Vec<TeachingFieldGloss> = block
        .field_gloss_rows
        .iter()
        .filter(|g| needed.contains(&g.symbol))
        .cloned()
        .collect();
    let mut covered: HashSet<String> = out.iter().map(|g| g.symbol.clone()).collect();
    for cap_key in new_caps {
        let Some(cgs) = exp.catalog_cgs_for_entry(cap_key.entry_id.as_str()) else {
            continue;
        };
        let Some(cap) = cgs.capabilities.get(cap_key.capability.as_str()) else {
            continue;
        };
        for (wire, sym) in capability_exposure_param_pairs(
            exp,
            map,
            cap_key,
            cap,
            CapabilityParamSurfaceFilter::AllOnSurface,
        ) {
            if !needed.contains(&sym) || covered.contains(&sym) {
                continue;
            }
            out.push(synthesize_param_gloss_row(
                exp,
                cap_key,
                wire.as_str(),
                sym.as_str(),
            ));
            covered.insert(sym);
        }
    }
    out
}

pub(crate) fn filter_teaching_bundle_to_new_capabilities(
    bundle: TeachingPromptBundle,
    exp: &TeachingExposureSession,
    new_caps: &BTreeSet<ExposureCapabilityKey>,
) -> TeachingPromptBundle {
    if new_caps.is_empty() {
        return TeachingPromptBundle {
            teaching_blocks: Vec::new(),
            model: Default::default(),
        };
    }
    let map = exp.symbol_map_arc();
    let method_syms = method_syms_for_new_capabilities(exp, map.as_ref(), new_caps);
    let mut teaching_blocks = Vec::new();
    let mut model_entities = Vec::new();
    for (block, entity_prompt) in bundle
        .teaching_blocks
        .into_iter()
        .zip(bundle.model.entities)
    {
        let Some((entry_id, entity)) =
            qualified_entity_for_block(exp, entity_prompt.entity.as_str(), new_caps)
        else {
            continue;
        };
        let kept_rows: Vec<EntityTeachingExprRow> = block
            .teaching_rows
            .iter()
            .filter(|row| {
                row_teaches_new_capability(
                    row,
                    entry_id.as_str(),
                    entity.as_str(),
                    new_caps,
                    &method_syms,
                )
            })
            .cloned()
            .collect();
        if kept_rows.is_empty() {
            continue;
        }
        let field_gloss_rows =
            gloss_rows_for_filtered_block(&block, &kept_rows, exp, map.as_ref(), new_caps);
        model_entities.push(super::EntityTeachingPrompt {
            entity: entity_prompt.entity,
            lines: kept_rows.iter().map(|r| r.meta.clone()).collect(),
        });
        teaching_blocks.push(EntityTeachingBlock {
            heading: block.heading,
            field_gloss_rows,
            teaching_rows: kept_rows,
        });
    }
    TeachingPromptBundle {
        teaching_blocks,
        model: super::TeachingPromptModel {
            entities: model_entities,
        },
    }
}

fn affected_entity_keys(new_caps: &BTreeSet<ExposureCapabilityKey>) -> Vec<ExposureEntityKey> {
    let mut keys: Vec<ExposureEntityKey> = new_caps
        .iter()
        .map(|k| ExposureEntityKey {
            entry_id: k.entry_id.clone(),
            entity: k.domain.clone(),
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Compact teaching TSV for newly exposed mutators: gloss rows + invoke witnesses only.
pub fn render_teaching_new_capabilities_delta_tsv(
    cgs: &CGS,
    config: RenderConfig<'_>,
    exp: &TeachingExposureSession,
    new_caps: &BTreeSet<ExposureCapabilityKey>,
) -> String {
    if new_caps.is_empty() {
        return String::new();
    }
    let entity_keys = affected_entity_keys(new_caps);
    let entity_names: Vec<&str> = entity_keys.iter().map(|k| k.entity.as_str()).collect();
    let bundle =
        render_teaching_prompt_bundle_for_exposure(cgs, config, exp, Some(entity_names.as_slice()));
    let filtered = filter_teaching_bundle_to_new_capabilities(bundle, exp, new_caps);
    render_prompt_tsv_from_bundle(&filtered)
}

/// Federated variant of [`render_teaching_new_capabilities_delta_tsv`].
pub fn render_teaching_new_capabilities_delta_tsv_federated<'b>(
    by_entry: &'b IndexMap<String, &'b CGS>,
    config: RenderConfig<'_>,
    exp: &TeachingExposureSession,
    new_caps: &BTreeSet<ExposureCapabilityKey>,
) -> String {
    if new_caps.is_empty() {
        return String::new();
    }
    let keys = affected_entity_keys(new_caps);
    let bundle = render_teaching_prompt_bundle_for_exposure_federated(
        by_entry,
        config,
        exp,
        Some(keys.as_slice()),
    );
    let filtered = filter_teaching_bundle_to_new_capabilities(bundle, exp, new_caps);
    let body = render_prompt_tsv_from_bundle(&filtered);
    if body.is_empty() || body == TSV_TEACHING_TABLE_HEADER {
        String::new()
    } else {
        body
    }
}

/// One-line mutator recap rows for reuse / diagnostics (subset of [`super::mcp_prompt_fragments`]).
pub(crate) fn render_mutator_recap_lines_for_caps(
    exp: &TeachingExposureSession,
    cap_keys: &[ExposureCapabilityKey],
) -> String {
    let map = exp.symbol_map_arc();
    let mut lines: Vec<String> = Vec::new();
    for cap_key in cap_keys {
        let Some(cgs) = exp.catalog_cgs_for_entry(cap_key.entry_id.as_str()) else {
            continue;
        };
        let Some(cap) = cgs.capabilities.get(cap_key.capability.as_str()) else {
            continue;
        };
        if matches!(
            cap.kind,
            CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
        ) {
            continue;
        }
        let entity = cap_key.domain.as_str();
        let Some(e_sym) = exp.qualified_entity_symbol(cap_key.entry_id.as_str(), entity) else {
            continue;
        };
        let m_sym = map.method_sym_for(
            cap_key.entry_id.as_str(),
            entity,
            cap_key.capability.as_str(),
        );
        let pairs = capability_exposure_param_pairs(
            exp,
            map.as_ref(),
            cap_key,
            cap,
            CapabilityParamSurfaceFilter::AllOnSurface,
        );
        let param_s = pairs
            .into_iter()
            .map(|(wire, sym)| format!("{wire}={sym}"))
            .collect::<Vec<_>>()
            .join(", ");
        let cap_wire = cap_key.capability.as_str();
        if param_s.is_empty() {
            lines.push(format!("{e_sym}.{m_sym}\t{cap_wire}"));
        } else {
            lines.push(format!("{e_sym}.{m_sym}\t{cap_wire} · {param_s}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
    use crate::loader::load_schema_dir;
    use std::path::PathBuf;

    #[test]
    fn filtered_delta_includes_optional_legend_gloss_rows() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema_dir(&root.join("../../apis/github")).expect("github");
        let entities = vec!["Repository".to_string(), "Issue".to_string()];
        let endpoints = entities
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "github".into(),
                entity: crate::EntityName::from(e.as_str()),
            })
            .collect::<Vec<_>>();
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "github",
            "create issue with labels in repository",
            &endpoints,
            &entities,
            Some(&["issue_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let exp = TeachingExposureSession::new_with_intent_delta(
            &cgs,
            "github",
            &["Repository", "Issue"],
            delta,
        );
        let cap_key = ExposureCapabilityKey {
            entry_id: "github".into(),
            domain: crate::EntityName::from("Issue"),
            capability: crate::CapabilityName::from("issue_create"),
        };
        let new_caps = BTreeSet::from([cap_key.clone()]);
        let cfg = RenderConfig::for_eval(None);
        let map = exp.symbol_map_arc();
        let labels_pair = capability_exposure_param_pairs(
            &exp,
            map.as_ref(),
            &cap_key,
            cgs.get_capability("issue_create").expect("issue_create"),
            CapabilityParamSurfaceFilter::AllOnSurface,
        )
        .into_iter()
        .find(|(wire, _)| wire == "labels");
        let tsv = render_teaching_new_capabilities_delta_tsv(&cgs, cfg, &exp, &new_caps);
        if let Some((_, labels_sym)) = labels_pair {
            assert!(
                tsv.contains(&format!("{labels_sym}\t")),
                "delta must include labels p# gloss row when on surface: {tsv}"
            );
        }
        assert!(
            tsv.contains("issue_create") || tsv.contains(".m"),
            "delta must include invoke witness for issue_create: {tsv}"
        );
        assert!(
            tsv.contains("labels=") || tsv.contains("label"),
            "invoke row must name labels param: {tsv}"
        );
    }

    #[test]
    fn optional_legend_pairs_name_wire_for_github_issue_create() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema_dir(&root.join("../../apis/github")).expect("github");
        let exp = TeachingExposureSession::new(&cgs, "github", &["Repository", "Issue"]);
        let map = exp.symbol_map_arc();
        let cap = cgs.get_capability("issue_create").expect("issue_create");
        let pairs = crate::symbol_tuning::capability_optional_legend_param_pairs(
            map.as_ref(),
            "github",
            "Issue",
            cap,
        );
        assert!(
            pairs
                .iter()
                .any(|(w, s)| w == "labels" && s.starts_with('p')),
            "labels optional legend must map to p#: {pairs:?}"
        );
    }
}
