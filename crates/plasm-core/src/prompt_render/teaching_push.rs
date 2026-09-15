//! Validated teaching-row push pipeline.

use std::collections::HashMap;

use crate::cross_entity::{choose_strategy, extract_cross_entity_predicates};
use crate::schema::{CapabilityKind, RelationMaterialization, RelationSchema};
use crate::symbol_tuning::SymbolMap;
use crate::{CapabilityName, Expr, CGS};

use super::gloss_collect::GlossScratch;
use super::input_legend::RowContractLegend;
use super::line_validate::{
    domain_line_validate_cached, DomainLineValidCacheKey, DomainLineValidEntry,
    ValidatedTeachingLine,
};
use super::teaching_legend::{
    teaching_expr_demonstrates_optional_params, teaching_expr_is_nullary_method_call,
    teaching_expr_line_from_layers, teaching_result_is_singleton_entity_gloss,
};
use super::{
    CrossEntityPlanMeta, CrossEntityStrategyKind, DomainLineKind, EntityTeachingExprRow,
    RelationMaterializationSummary, TeachingLineMeta, TeachingRowDedupeKey,
};

/// teaching table line metadata from an already type-checked [`Expr`] (avoids a second parse in the render hot path).
pub(crate) fn domain_line_execution_meta_from_validated(
    cgs: &CGS,
    work: String,
    relation: Option<&RelationSchema>,
    source_capability: Option<&CapabilityName>,
    expr: &Expr,
) -> TeachingLineMeta {
    let relation_materialization = relation.map(|r| {
        RelationMaterializationSummary::from(
            r.materialize
                .as_ref()
                .unwrap_or(&RelationMaterialization::Unavailable),
        )
    });

    let (kind, cross_entity) = if relation.is_some() {
        (DomainLineKind::RelationNav, None)
    } else if work.contains('~') {
        (DomainLineKind::Search, None)
    } else {
        let kind = match expr {
            Expr::Get(_) => DomainLineKind::Get,
            Expr::Query(_) => DomainLineKind::Query,
            Expr::Create(_) | Expr::Delete(_) | Expr::Invoke(_) => DomainLineKind::Method,
            Expr::Chain(_)
            | Expr::Page(_)
            | Expr::Wait(_)
            | Expr::Cancel(_)
            | Expr::TeachingValue { .. } => DomainLineKind::Other,
        };
        let cross_entity = if let Expr::Query(q) = expr {
            if let (Some(pred), Some(ent_def)) = (&q.predicate, cgs.get_entity(q.entity.as_str())) {
                let crosses = extract_cross_entity_predicates(pred, ent_def, cgs);
                if crosses.is_empty() {
                    None
                } else {
                    Some(
                        crosses
                            .iter()
                            .map(|c| {
                                let strat = choose_strategy(c, q.entity.as_str(), cgs);
                                CrossEntityPlanMeta {
                                    ref_field: c.ref_field.clone(),
                                    foreign_entity: c.foreign_entity.clone(),
                                    strategy: match strat {
                                        crate::cross_entity::CrossEntityStrategy::PushLeft {
                                            ..
                                        } => CrossEntityStrategyKind::PushLeft,
                                        crate::cross_entity::CrossEntityStrategy::PullRight {
                                            ..
                                        } => CrossEntityStrategyKind::PullRight,
                                    },
                                }
                            })
                            .collect(),
                    )
                }
            } else {
                None
            }
        } else {
            None
        };
        (kind, cross_entity)
    };

    TeachingLineMeta {
        expression: work,
        kind,
        source_capability: source_capability.map(|n| n.to_string()),
        cross_entity,
        relation_materialization,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn try_push_teaching_example(
    gloss_emit: &mut Option<GlossScratch<'_>>,
    teaching_rows: &mut Vec<EntityTeachingExprRow>,
    collect_meta: bool,
    cgs: &CGS,
    expr: &str,
    gloss: Option<String>,
    cap_leg: Option<String>,
    relation: Option<&RelationSchema>,
    source_capability: Option<&CapabilityName>,
    // When true: strip [`TeachingExprLine::description`] from capability legend (Query/Get/Search);
    // scope / optional params / compact args remain.
    omit_capability_prose: bool,
    line_valid_cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    line_valid_cache_seed: u64,
    map_arc: Option<&std::sync::Arc<SymbolMap>>,
    row_contract: Option<RowContractLegend>,
) -> bool {
    let optional_syms: Vec<String> = match (map_arc, source_capability) {
        (Some(map), Some(cap_name)) => {
            cgs.get_capability(cap_name.as_str())
                .map_or_else(Vec::new, |cap| {
                    crate::symbol_tuning::optional_legend_param_syms(
                        map.as_ref(),
                        cgs.entry_id.as_deref().unwrap_or(""),
                        cap.domain.as_str(),
                        cap,
                    )
                })
        }
        _ => Vec::new(),
    };
    if let Some(gs) = gloss_emit.as_mut() {
        gs.emit_before_teaching_example(expr, cap_leg.as_deref(), gloss.as_deref(), &optional_syms);
    }
    let mut teaching_line = teaching_expr_line_from_layers(
        expr,
        gloss.as_deref(),
        cap_leg.as_deref(),
        row_contract.unwrap_or_default(),
    );
    if let (Some(map), Some(cap_name)) = (map_arc.as_ref(), source_capability) {
        if let Some(cap) = cgs.get_capability(cap_name.as_str()) {
            let entry_id = cgs.entry_id.as_deref().unwrap_or("");
            let mut wires: Vec<String> =
                crate::symbol_tuning::capability_optional_legend_param_pairs(
                    map.as_ref(),
                    entry_id,
                    cap.domain.as_str(),
                    cap,
                )
                .into_iter()
                .map(|(wire, _)| wire)
                .collect();
            wires.sort();
            wires.dedup();
            if !wires.is_empty() {
                teaching_line.legend.optional_params = wires;
            }
        }
    }
    if teaching_line.legend.optional_params_present()
        && !teaching_expr_demonstrates_optional_params(expr, &optional_syms)
    {
        teaching_line.legend.optional_params.clear();
    }
    if omit_capability_prose {
        teaching_line.legend.description.clear();
    }
    let dedupe_key = TeachingRowDedupeKey::new(expr, gloss.as_ref(), cap_leg.as_ref());

    let Some(validated) =
        domain_line_validate_cached(line_valid_cache, line_valid_cache_seed, cgs, expr, map_arc)
    else {
        return false;
    };

    let meta = match (&validated, collect_meta) {
        (ValidatedTeachingLine::Expr { parsed, wire }, true) => {
            domain_line_execution_meta_from_validated(
                cgs,
                wire.clone(),
                relation,
                source_capability,
                &parsed.expr,
            )
        }
        (ValidatedTeachingLine::QueryBind { wire }, true) => TeachingLineMeta {
            expression: wire.clone(),
            kind: DomainLineKind::Query,
            source_capability: None,
            cross_entity: None,
            relation_materialization: None,
        },
        (ValidatedTeachingLine::RelationFanout { wire }, true) => TeachingLineMeta {
            expression: wire.clone(),
            kind: DomainLineKind::RelationNav,
            source_capability: None,
            cross_entity: None,
            relation_materialization: relation.map(|r| {
                RelationMaterializationSummary::from(
                    r.materialize
                        .as_ref()
                        .unwrap_or(&RelationMaterialization::Unavailable),
                )
            }),
        },
        (_, false) => TeachingLineMeta {
            expression: validated.wire().to_string(),
            kind: DomainLineKind::Other,
            source_capability: None,
            cross_entity: None,
            relation_materialization: None,
        },
    };
    // Sparse exception: nullary method calls that yield a singleton entity row use `→ e`
    // (not terminal write / chain hint) — override Method→Terminal.
    teaching_line.arrow = super::ReturnArrow::classify(meta.kind, &teaching_line.result_type);
    let cap_is_mutating = meta
        .source_capability
        .as_ref()
        .and_then(|n| cgs.capabilities.get(n.as_str()))
        .is_some_and(|cap| {
            matches!(
                cap.kind,
                CapabilityKind::Create | CapabilityKind::Update | CapabilityKind::Delete
            )
        });
    if !cap_is_mutating
        && teaching_expr_is_nullary_method_call(&teaching_line.expression)
        && teaching_result_is_singleton_entity_gloss(&teaching_line.result_type)
    {
        teaching_line.is_singleton_row_fetch = true;
        teaching_line.arrow = super::ReturnArrow::Single;
    }
    teaching_rows.push(EntityTeachingExprRow {
        teaching_expr: teaching_line,
        meta,
        dedupe_key,
    });
    true
}
