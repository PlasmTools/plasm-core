//! Admission of recursive bounded scopes into the ordinary host plan.
use crate::{execute_session::ExecuteSession, plasm_plan::*};

pub(crate) fn iteration_predicate(
    it: &ValidatedIterateUntilNode,
) -> Result<Option<ValidatedMapBodyNode>, String> {
    let Some(body) = &it.until_scope else {
        return Ok(None);
    };
    if !it.until_predicates.is_empty()
        || !matches!(body.output, plasm_core::plasm_monad::ScopedOutput::Filter)
        || body.parent.source.as_str() != it.source.as_str()
        || body.max_parents.get() != 1
    {
        return Err("iteration predicate requires a pure singleton filter over its seed".into());
    }
    body.execution_layers()?;
    for capture in &body.captures {
        if !it
            .uses_result
            .iter()
            .any(|u| u.node == capture.source.as_str() && u.r#as == capture.local.as_str())
        {
            return Err("iteration predicate capture lacks an enclosing dependency".into());
        }
    }
    Ok(Some(ValidatedMapBodyNode {
        id: PlanNodeId::new("until")?,
        body: body.clone(),
        plan: it
            .until_plan
            .clone()
            .ok_or("iteration predicate plan missing")?,
        depends_on: it.depends_on.clone(),
        uses_result: it.uses_result.clone(),
    }))
}

pub(crate) fn iteration_step(
    it: &ValidatedIterateUntilNode,
) -> Result<Option<ValidatedMapBodyNode>, String> {
    let Some(body) = &it.step_scope else {
        return Ok(None);
    };
    plasm_core::plasm_monad::correlated::iteration_step_effect(body)?;
    if body.parent.source.as_str() != it.source.as_str() {
        return Err("iteration step must use its current seed".into());
    }
    body.execution_layers()?;
    for capture in &body.captures {
        if !it
            .uses_result
            .iter()
            .any(|u| u.node == capture.source.as_str() && u.r#as == capture.local.as_str())
        {
            return Err("iteration step capture lacks an enclosing dependency".into());
        }
    }
    Ok(Some(ValidatedMapBodyNode {
        id: PlanNodeId::new("step")?,
        body: body.clone(),
        plan: it.step_plan.clone().ok_or("iteration step plan missing")?,
        depends_on: it.depends_on.clone(),
        uses_result: it.uses_result.clone(),
    }))
}

pub(crate) fn validate(
    es: &ExecuteSession,
    map: &ValidatedMapBodyNode,
    nodes: &[ValidatedPlanNode],
) -> Result<(), String> {
    let body = &map.body;
    body.execution_layers()?;
    let expected = QualifiedEntityKey {
        entry_id: body.parent.entity.entry_id.clone(),
        entity: body.parent.entity.entity.clone(),
    };
    if body.parent_entity_authority
        && capture_owner(nodes, body.parent.source.as_str(), 0)? != &expected
    {
        return Err("map body parent must preserve the captured catalog/entity identity".into());
    }
    if let Some(schema) = &body.parent_schema {
        validate_port_schema(es, nodes, body.parent.source.as_str(), schema)?;
    } else {
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            es,
            &expected.entry_id,
            &expected.entity,
        )?;
        for field in cgs
            .get_entity(&expected.entity)
            .ok_or("missing parent entity")?
            .fields
            .keys()
        {
            crate::python_compute::source_field_kind(
                es,
                nodes,
                body.parent.source.as_str(),
                field.as_str(),
                0,
            )?;
        }
    }
    for capture in &body.captures {
        if capture.singleton
            && !crate::plasm_plan::scoped_capture_permits_singleton(nodes, capture.source.as_str())
        {
            return Err("scoped capture cannot promote a plural source to singleton".into());
        }
        if let Some(value) = &capture.value_contract {
            if capture.entity_authority
                || !capture.singleton
                || &crate::map_body_schema::row_contract(es, nodes, capture.source.as_str())?
                    != value
            {
                return Err("scalar capture must preserve its source value contract without receiver authority".into());
            }
        } else {
            validate_port_schema(es, nodes, capture.source.as_str(), &capture.schema)?;
        }
        if capture.entity_authority {
            let owner = capture_owner(nodes, capture.source.as_str(), 0)?;
            if owner.entry_id != capture.entity.entry_id || owner.entity != capture.entity.entity {
                return Err("captured receiver ownership mismatch".into());
            }
        }
    }
    if let plasm_core::plasm_monad::ScopedOutput::Rows {
        entity,
        schema,
        entity_authority,
        acknowledgement,
    } = &body.output
    {
        let plasm_core::PlasmReturn::Step { step } = &body.body.return_ else {
            return Err("scope return missing".into());
        };
        let output = map
            .plan
            .nodes()
            .iter()
            .find(|n| n.id().as_str() == step.as_str())
            .ok_or("scope result missing")?;
        if *acknowledgement != (output.result_shape() == ResultShape::SideEffectAck) {
            return Err("scope acknowledgement contract differs from returned operation".into());
        }
        validate_port_schema(es, map.plan.nodes(), step.as_str(), schema)?;
        if *entity_authority {
            let owner = capture_owner(map.plan.nodes(), step.as_str(), 0)?;
            if owner.entry_id != entity.entry_id || owner.entity != entity.entity {
                return Err("scope result cannot acquire entity authority".into());
            }
        }
    }
    for node in map.plan.nodes() {
        if let ValidatedPlanNode::Compute(compute) = node {
            if compute.compute.source == body.parent.local.as_str() {
                if let ComputeOp::Python {
                    input_schema: Some(schema),
                    ..
                } = &compute.compute.op
                {
                    for field in &schema.fields {
                        let actual = crate::python_compute::source_field_kind(
                            es,
                            nodes,
                            body.parent.source.as_str(),
                            field.name.as_str(),
                            0,
                        )?;
                        if field.value_type.as_ref() != Some(&actual) {
                            return Err(
                                "captured Python input differs from the outer source schema".into(),
                            );
                        }
                    }
                }
            }
        }
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
            ValidatedPlanNode::Capture(c) if c.entity_authority => Some(&c.entity),
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
        if let ValidatedPlanNode::Capture(capture) = source {
            if let Some(schema) = &capture.schema {
                let entity = cgs
                    .get_entity(source_owner.entity.as_str())
                    .ok_or("missing capture entity")?;
                let keys: Vec<_> = if entity.key_vars.is_empty() {
                    vec![entity.id_field.as_str()]
                } else {
                    entity.key_vars.iter().map(|v| v.as_str()).collect()
                };
                for key in keys {
                    if !schema.fields.iter().any(|f| f.name.as_str() == key) {
                        return Err(format!(
                            "captured relation identity field {key} omitted by projection"
                        ));
                    }
                }
            }
        }
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

/// Catalog authority survives row-preserving operations, never object derivation.
fn capture_owner<'a>(
    nodes: &'a [ValidatedPlanNode],
    id: &str,
    depth: usize,
) -> Result<&'a QualifiedEntityKey, String> {
    if depth > 256 {
        return Err("map capture source depth exceeded".into());
    }
    let node = nodes
        .iter()
        .find(|n| n.id().as_str() == id)
        .ok_or("map parent source missing")?;
    match node {
        ValidatedPlanNode::Capture(c) if c.entity_authority => Ok(&c.entity),
        ValidatedPlanNode::Surface(s)
            if s.result_shape != crate::plasm_plan::ResultShape::SideEffectAck =>
        {
            s.qualified_entity
                .as_ref()
                .ok_or("missing map capture owner".into())
        }
        ValidatedPlanNode::RelationTraversal(r) => Ok(&r.relation.target),
        ValidatedPlanNode::MapBody(map) => {
            if matches!(
                map.body.output,
                plasm_core::plasm_monad::ScopedOutput::Filter
            ) {
                return capture_owner(nodes, map.body.parent.source.as_str(), depth + 1);
            }
            let plasm_core::PlasmReturn::Step { step } = &map.body.body.return_ else {
                return Err("missing scope return".into());
            };
            if !matches!(
                map.body.output,
                plasm_core::plasm_monad::ScopedOutput::Rows {
                    entity_authority: true,
                    ..
                }
            ) {
                return Err("synthetic scope has no receiver authority".into());
            }
            capture_owner(map.plan.nodes(), step.as_str(), depth + 1)
        }
        ValidatedPlanNode::Compute(c) if matches!(c.compute.op, ComputeOp::Union { .. }) => {
            let ComputeOp::Union { other } = &c.compute.op else {
                unreachable!()
            };
            let left = capture_owner(nodes, &c.compute.source, depth + 1).ok();
            let right = capture_owner(nodes, other.as_str(), depth + 1).ok();
            c.compute
                .op
                .preserved_identity(left, right)
                .ok_or_else(|| "union inputs lack common entity receiver authority".into())
        }
        ValidatedPlanNode::Compute(c) if c.compute.op.preserves_row_identity() => {
            let owner = capture_owner(nodes, &c.compute.source, depth + 1)?;
            Ok(owner)
        }
        _ => Err("map capture requires identity-preserving catalog reads".into()),
    }
}

fn validate_port_schema(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    source: &str,
    schema: &SyntheticResultSchema,
) -> Result<(), String> {
    for field in &schema.fields {
        let actual =
            crate::python_compute::source_field_kind(es, nodes, source, field.name.as_str(), 0)?;
        if field.value_type.as_ref() != Some(&actual) {
            return Err("captured port schema differs from enclosing source".into());
        }
    }
    Ok(())
}
