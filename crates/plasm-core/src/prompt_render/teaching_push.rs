//! Validated teaching-row push pipeline.

use std::collections::HashMap;

use crate::cross_entity::{choose_strategy, extract_cross_entity_predicates};
use crate::schema::{RelationMaterialization, RelationSchema};
use crate::symbol_tuning::SymbolMap;
use crate::{CapabilityName, Expr, CGS};

use super::gloss_collect::GlossScratch;
use super::input_legend::RowContractLegend;
use super::line_validate::{
    domain_line_validate_cached, DomainLineValidCacheKey, DomainLineValidEntry,
};
use super::teaching_legend::{
    teaching_expr_demonstrates_optional_params, teaching_expr_line_from_layers,
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
    if teaching_line.legend.optional_params_present()
        && !teaching_expr_demonstrates_optional_params(expr, &optional_syms)
    {
        teaching_line.legend.optional_params.clear();
    }
    if omit_capability_prose {
        teaching_line.legend.description.clear();
    }
    let dedupe_key = TeachingRowDedupeKey::new(expr, gloss.as_ref(), cap_leg.as_ref());

    let Some((parsed, work)) =
        domain_line_validate_cached(line_valid_cache, line_valid_cache_seed, cgs, expr, map_arc)
    else {
        return false;
    };

    let meta = if collect_meta {
        domain_line_execution_meta_from_validated(
            cgs,
            work,
            relation,
            source_capability,
            &parsed.expr,
        )
    } else {
        TeachingLineMeta {
            expression: work,
            kind: DomainLineKind::Other,
            source_capability: None,
            cross_entity: None,
            relation_materialization: None,
        }
    };
    // Method shape controls dispatch, while the derived capability effect controls whether the
    // result is a terminal write or a chainable read-action value.
    let source_effect = source_capability
        .and_then(|name| cgs.get_capability(name.as_str()))
        .map(|cap| cap.effective_effect());
    teaching_line.arrow = super::ReturnArrow::classify_with_effect(
        meta.kind,
        &teaching_line.result_type,
        source_effect,
    );
    teaching_rows.push(EntityTeachingExprRow {
        teaching_expr: teaching_line,
        meta,
        dedupe_key,
    });
    true
}
