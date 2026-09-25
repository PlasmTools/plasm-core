//! Admission of the read-only scoped map contract into the ordinary host plan.
use crate::{execute_session::ExecuteSession, plasm_plan::*};

pub(crate) fn validate(
    es: &ExecuteSession,
    map: &ValidatedMapBodyNode,
    nodes: &[ValidatedPlanNode],
) -> Result<(), String> {
    let body = &map.body;
    body.execution_layers()?;
    if body.max_parents.get() > 256 {
        return Err("correlated slice supports at most 256 parents".into());
    }
    let source = nodes
        .iter()
        .find(|n| n.id().as_str() == body.parent.source.as_str())
        .ok_or("map body parent source missing")?;
    let expected = QualifiedEntityKey {
        entry_id: body.parent.entity.entry_id.clone(),
        entity: body.parent.entity.entity.clone(),
    };
    let actual = match source {
        ValidatedPlanNode::Surface(s) if s.effect_class == EffectClass::Read => {
            s.qualified_entity.as_ref()
        }
        ValidatedPlanNode::RelationTraversal(r) => Some(&r.relation.target),
        _ => None,
    };
    if actual != Some(&expected) {
        return Err("map body parent must be a read of the captured catalog/entity".into());
    }
    for node in map.plan.nodes() {
        let ValidatedPlanNode::RelationTraversal(relation) = node else {
            continue;
        };
        let source = map
            .plan
            .nodes()
            .iter()
            .find(|n| n.id() == &relation.relation.source)
            .ok_or("body relation source missing")?;
        let source_owner = match source {
            ValidatedPlanNode::Capture(c) => Some(&c.entity),
            ValidatedPlanNode::Surface(s) => s.qualified_entity.as_ref(),
            ValidatedPlanNode::RelationTraversal(r) => Some(&r.relation.target),
            _ => None,
        }
        .ok_or("body relation requires a typed entity source")?;
        let plasm_core::Expr::Chain(chain) = &relation.relation.ir.expr else {
            return Err("body relation requires a catalog chain".into());
        };
        let stamped = plasm_core::catalog_ownership::require_relation_source_qualified_entity(
            &chain.source,
            true,
            None,
        )
        .map_err(|e| e.to_string())?
        .ok_or("body relation requires catalog ownership")?;
        if stamped.entry_id() != source_owner.entry_id.as_str()
            || stamped.entity.as_str() != source_owner.entity.as_str()
        {
            return Err("body relation receiver does not match captured source ownership".into());
        }
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            es,
            source_owner.entry_id.as_str(),
            source_owner.entity.as_str(),
        )?;
        let declared = cgs
            .get_entity(source_owner.entity.as_str())
            .and_then(|e| e.relations.get(relation.relation.relation.as_str()))
            .ok_or("body relation missing from captured source catalog")?;
        if serde_json::to_value(&declared.materialize).map_err(|e| e.to_string())?
            != serde_json::to_value(Some(&relation.relation.materialize))
                .map_err(|e| e.to_string())?
        {
            return Err("body relation materialization does not match catalog".into());
        }
    }
    crate::map_body_schema::output_schema(es, body)?;
    crate::plan_session_provisions::validate(es, map.plan.nodes(), &body.body.bind)?;
    Ok(())
}

#[cfg(test)]
mod tests;
