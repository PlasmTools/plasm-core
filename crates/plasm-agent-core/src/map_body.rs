//! Admission of recursive bounded scopes into the ordinary host plan.
use crate::{execute_session::ExecuteSession, plasm_plan::*};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MapBodyValidationError {
    #[error(transparent)]
    Correlated(#[from] plasm_core::plasm_monad::correlated::CorrelatedBodyError),
    #[error(transparent)]
    IterationEffect(#[from] plasm_core::plasm_monad::correlated::IterationStepEffectError),
    #[error(transparent)]
    PlanInvariant(#[from] crate::plasm_plan::PlanAtomError),
    #[error(transparent)]
    BindGraph(#[from] plasm_core::plasm_monad::BindGraphError),
    #[error(transparent)]
    PlanValidation(#[from] Box<crate::plasm_plan::PlanValidationError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Catalog(#[from] CatalogOwnershipError),
    #[error(transparent)]
    CatalogChain(#[from] plasm_core::catalog_ownership::CatalogOwnershipError),
    #[error(transparent)]
    PythonField(#[from] PythonFieldError),
    #[error(transparent)]
    Schema(#[from] MapSchemaError),
    #[error(transparent)]
    SessionProvision(#[from] Box<crate::plan_session_provisions::SessionProvisionError>),
    #[error("scope contract violation: {0}")]
    Contract(#[from] ScopeContractError),
}

impl From<crate::plasm_plan::PlanValidationError> for MapBodyValidationError {
    fn from(error: crate::plasm_plan::PlanValidationError) -> Self {
        Self::PlanValidation(Box::new(error))
    }
}

impl From<crate::plan_session_provisions::SessionProvisionError> for MapBodyValidationError {
    fn from(error: crate::plan_session_provisions::SessionProvisionError) -> Self {
        Self::SessionProvision(Box::new(error))
    }
}

impl From<MapBodyValidationError> for plasm_runtime::ExecutionFailure {
    fn from(error: MapBodyValidationError) -> Self {
        let code = match &error {
            MapBodyValidationError::Correlated(_) => "scope_body_invalid",
            MapBodyValidationError::IterationEffect(_) => "scope_iteration_effect_invalid",
            MapBodyValidationError::PlanInvariant(_) => "plan_identifier_invalid",
            MapBodyValidationError::BindGraph(_) => "plan_bind_graph_invalid",
            MapBodyValidationError::PlanValidation(_) => "scope_plan_invalid",
            MapBodyValidationError::Json(_) => "scope_plan_serialization_invalid",
            MapBodyValidationError::Catalog(_) => "scope_catalog_entity_unavailable",
            MapBodyValidationError::CatalogChain(_) => "scope_catalog_chain_invalid",
            MapBodyValidationError::PythonField(_) => "scope_python_field_unavailable",
            MapBodyValidationError::Schema(_) => "scope_value_schema_invalid",
            MapBodyValidationError::SessionProvision(_) => "scope_capture_provision_invalid",
            MapBodyValidationError::Contract(_) => "scope_contract_invalid",
        };
        Self::new(
            plasm_runtime::FailureCause::Program,
            code,
            error.to_string(),
        )
    }
}

#[derive(Debug, Error)]
pub enum ScopeContractError {
    #[error("iteration predicate is not a pure singleton filter over its seed")]
    IterationPredicate,
    #[error("iteration predicate capture lacks an enclosing dependency")]
    PredicateCaptureDependency,
    #[error("iteration predicate plan is missing")]
    PredicatePlanMissing,
    #[error("iteration step does not use its current seed")]
    IterationSeedMismatch,
    #[error("iteration step capture lacks an enclosing dependency")]
    StepCaptureDependency,
    #[error("iteration step plan is missing")]
    StepPlanMissing,
    #[error("map body parent does not preserve captured catalog/entity identity")]
    ParentIdentityChanged,
    #[error("parent entity is missing from its catalog")]
    ParentEntityMissing,
    #[error("scoped capture cannot promote a plural source to singleton")]
    PluralCapturePromoted,
    #[error("scalar capture changed its value contract or acquired receiver authority")]
    ScalarCaptureContractChanged,
    #[error("captured receiver ownership differs")]
    CaptureOwnerMismatch,
    #[error("scope return is missing")]
    ScopeReturnMissing,
    #[error("scope result is missing")]
    ScopeResultMissing,
    #[error("scope acknowledgement differs from returned operation")]
    AcknowledgementMismatch,
    #[error("scope result acquired entity authority")]
    ScopeAcquiredAuthority,
    #[error("captured Python input differs from outer source schema")]
    PythonInputSchemaMismatch,
    #[error("body relation source is missing")]
    RelationSourceMissing,
    #[error("body relation requires a typed entity source")]
    RelationSourceUntyped,
    #[error("body relation requires a catalog chain")]
    RelationNotCatalogChain,
    #[error("body relation requires catalog ownership")]
    RelationOwnershipMissing,
    #[error("body relation receiver differs from captured source ownership")]
    RelationReceiverMismatch,
    #[error("captured relation identity field is omitted by projection")]
    RelationIdentityOmitted { field: String },
    #[error("body relation is missing from source catalog")]
    RelationCatalogEntryMissing,
    #[error("body relation materialization differs from catalog")]
    RelationMaterializationMismatch,
    #[error("scope capture source depth exceeded")]
    CaptureDepthExceeded,
    #[error("map parent source is missing")]
    ParentSourceMissing,
    #[error("scope capture owner is missing")]
    CaptureOwnerMissing,
    #[error("scope return has no receiver authority")]
    SyntheticScopeNoAuthority,
    #[error("union inputs lack common entity receiver authority")]
    UnionAuthorityMismatch,
    #[error("capture requires identity-preserving catalog reads")]
    IdentityNotPreserved,
    #[error("scope port schema differs from enclosing source")]
    PortSchemaMismatch,
    #[error("scope output schema differs from catalog")]
    OutputSchemaMismatch,
}

#[derive(Debug, Error)]
pub enum CatalogOwnershipError {
    #[error("catalog entity `{entity}` is unavailable under `{catalog_entry}`")]
    EntityUnavailable {
        catalog_entry: String,
        entity: String,
    },
}

#[derive(Debug, Error)]
pub enum PythonFieldError {
    #[error("source field `{field}` is unavailable on plan node `{node}`")]
    Unavailable { node: String, field: String },
}

#[derive(Debug, Error)]
pub enum MapSchemaError {
    #[error("map-body schema could not be derived")]
    Invalid,
}

pub(crate) fn iteration_predicate(
    it: &ValidatedIterateUntilNode,
) -> Result<Option<ValidatedMapBodyNode>, MapBodyValidationError> {
    let Some(body) = &it.until_scope else {
        return Ok(None);
    };
    if !it.until_predicates.is_empty()
        || !matches!(body.output, plasm_core::plasm_monad::ScopedOutput::Filter)
        || body.parent.source.as_str() != it.source.as_str()
        || body.max_parents.get() != 1
    {
        return Err(ScopeContractError::IterationPredicate.into());
    }
    body.execution_layers()?;
    for capture in &body.captures {
        if !it
            .uses_result
            .iter()
            .any(|u| u.node == capture.source.as_str() && u.r#as == capture.local.as_str())
        {
            return Err(ScopeContractError::PredicateCaptureDependency.into());
        }
    }
    Ok(Some(ValidatedMapBodyNode {
        id: PlanNodeId::new("until")?,
        body: body.clone(),
        plan: it
            .until_plan
            .clone()
            .ok_or(ScopeContractError::PredicatePlanMissing)?,
        depends_on: it.depends_on.clone(),
        uses_result: it.uses_result.clone(),
    }))
}

pub(crate) fn iteration_step(
    it: &ValidatedIterateUntilNode,
) -> Result<Option<ValidatedMapBodyNode>, MapBodyValidationError> {
    let Some(body) = &it.step_scope else {
        return Ok(None);
    };
    plasm_core::plasm_monad::correlated::iteration_step_effect(body)?;
    if body.parent.source.as_str() != it.source.as_str() {
        return Err(ScopeContractError::IterationSeedMismatch.into());
    }
    body.execution_layers()?;
    for capture in &body.captures {
        if !it
            .uses_result
            .iter()
            .any(|u| u.node == capture.source.as_str() && u.r#as == capture.local.as_str())
        {
            return Err(ScopeContractError::StepCaptureDependency.into());
        }
    }
    Ok(Some(ValidatedMapBodyNode {
        id: PlanNodeId::new("step")?,
        body: body.clone(),
        plan: it
            .step_plan
            .clone()
            .ok_or(ScopeContractError::StepPlanMissing)?,
        depends_on: it.depends_on.clone(),
        uses_result: it.uses_result.clone(),
    }))
}

pub(crate) fn validate(
    es: &ExecuteSession,
    map: &ValidatedMapBodyNode,
    nodes: &[ValidatedPlanNode],
) -> Result<(), MapBodyValidationError> {
    let body = &map.body;
    body.execution_layers()?;
    let expected = QualifiedEntityKey {
        entry_id: body.parent.entity.entry_id.clone(),
        entity: body.parent.entity.entity.clone(),
    };
    if body.parent_entity_authority
        && capture_owner(nodes, body.parent.source.as_str(), 0)? != &expected
    {
        return Err(ScopeContractError::ParentIdentityChanged.into());
    }
    if let Some(schema) = &body.parent_schema {
        validate_port_schema(es, nodes, body.parent.source.as_str(), schema)?;
    } else {
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            es,
            &expected.entry_id,
            &expected.entity,
        )
        .map_err(|_| CatalogOwnershipError::EntityUnavailable {
            catalog_entry: expected.entry_id.clone(),
            entity: expected.entity.clone(),
        })?;
        for field in cgs
            .get_entity(&expected.entity)
            .ok_or(ScopeContractError::ParentEntityMissing)?
            .fields
            .keys()
        {
            crate::python_compute::source_field_kind(
                es,
                nodes,
                body.parent.source.as_str(),
                field.as_str(),
                0,
            )
            .map_err(|_| PythonFieldError::Unavailable {
                node: body.parent.source.to_string(),
                field: field.to_string(),
            })?;
        }
    }
    for capture in &body.captures {
        if capture.singleton
            && !crate::plasm_plan::scoped_capture_permits_singleton(nodes, capture.source.as_str())
        {
            return Err(ScopeContractError::PluralCapturePromoted.into());
        }
        if let Some(value) = &capture.value_contract {
            if capture.entity_authority
                || !capture.singleton
                || &crate::map_body_schema::row_contract(es, nodes, capture.source.as_str())
                    .map_err(|_| MapSchemaError::Invalid)?
                    != value
            {
                return Err(ScopeContractError::ScalarCaptureContractChanged.into());
            }
        } else {
            validate_port_schema(es, nodes, capture.source.as_str(), &capture.schema)?;
        }
        if capture.entity_authority {
            let owner = capture_owner(nodes, capture.source.as_str(), 0)?;
            if owner.entry_id != capture.entity.entry_id || owner.entity != capture.entity.entity {
                return Err(ScopeContractError::CaptureOwnerMismatch.into());
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
            return Err(ScopeContractError::ScopeReturnMissing.into());
        };
        let output = map
            .plan
            .nodes()
            .iter()
            .find(|n| n.id().as_str() == step.as_str())
            .ok_or(ScopeContractError::ScopeResultMissing)?;
        if *acknowledgement != (output.result_shape() == ResultShape::SideEffectAck) {
            return Err(ScopeContractError::AcknowledgementMismatch.into());
        }
        validate_port_schema(es, map.plan.nodes(), step.as_str(), schema)?;
        if *entity_authority {
            let owner = capture_owner(map.plan.nodes(), step.as_str(), 0)?;
            if owner.entry_id != entity.entry_id || owner.entity != entity.entity {
                return Err(ScopeContractError::ScopeAcquiredAuthority.into());
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
                        )
                        .map_err(|_| PythonFieldError::Unavailable {
                            node: body.parent.source.to_string(),
                            field: field.name.to_string(),
                        })?;
                        if field.value_type.as_ref() != Some(&actual) {
                            return Err(ScopeContractError::PythonInputSchemaMismatch.into());
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
            .ok_or(ScopeContractError::RelationSourceMissing)?;
        let source_owner = match source {
            ValidatedPlanNode::Capture(c) if c.entity_authority => Some(&c.entity),
            ValidatedPlanNode::Surface(s) => s.qualified_entity.as_ref(),
            ValidatedPlanNode::RelationTraversal(r) => Some(&r.relation.target),
            _ => None,
        }
        .ok_or(ScopeContractError::RelationSourceUntyped)?;
        let plasm_core::Expr::Chain(chain) = &relation.relation.ir.expr else {
            return Err(ScopeContractError::RelationNotCatalogChain.into());
        };
        let stamped = plasm_core::catalog_ownership::require_relation_source_qualified_entity(
            &chain.source,
            true,
            None,
        )?
        .ok_or(ScopeContractError::RelationOwnershipMissing)?;
        if stamped.entry_id() != source_owner.entry_id.as_str()
            || stamped.entity.as_str() != source_owner.entity.as_str()
        {
            return Err(ScopeContractError::RelationReceiverMismatch.into());
        }
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            es,
            source_owner.entry_id.as_str(),
            source_owner.entity.as_str(),
        )
        .map_err(|_| CatalogOwnershipError::EntityUnavailable {
            catalog_entry: source_owner.entry_id.clone(),
            entity: source_owner.entity.clone(),
        })?;
        if let ValidatedPlanNode::Capture(capture) = source {
            if let Some(schema) = &capture.schema {
                let entity = cgs
                    .get_entity(source_owner.entity.as_str())
                    .ok_or(ScopeContractError::ParentEntityMissing)?;
                let keys: Vec<_> = if entity.key_vars.is_empty() {
                    vec![entity.id_field.as_str()]
                } else {
                    entity.key_vars.iter().map(|v| v.as_str()).collect()
                };
                for key in keys {
                    if !schema.fields.iter().any(|f| f.name.as_str() == key) {
                        return Err(ScopeContractError::RelationIdentityOmitted {
                            field: key.to_owned(),
                        }
                        .into());
                    }
                }
            }
        }
        let declared = cgs
            .get_entity(source_owner.entity.as_str())
            .and_then(|e| e.relations.get(relation.relation.relation.as_str()))
            .ok_or(ScopeContractError::RelationCatalogEntryMissing)?;
        if declared.materialize != Some(relation.relation.materialize.clone()) {
            return Err(ScopeContractError::RelationMaterializationMismatch.into());
        }
    }
    crate::map_body_schema::output_schema(es, body).map_err(|_| MapSchemaError::Invalid)?;
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
) -> Result<&'a QualifiedEntityKey, MapBodyValidationError> {
    if depth > 256 {
        return Err(ScopeContractError::CaptureDepthExceeded.into());
    }
    let node = nodes
        .iter()
        .find(|n| n.id().as_str() == id)
        .ok_or(ScopeContractError::ParentSourceMissing)?;
    match node {
        ValidatedPlanNode::Capture(c) if c.entity_authority => Ok(&c.entity),
        ValidatedPlanNode::Surface(s)
            if s.result_shape != crate::plasm_plan::ResultShape::SideEffectAck =>
        {
            s.qualified_entity
                .as_ref()
                .ok_or(ScopeContractError::CaptureOwnerMissing.into())
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
                return Err(ScopeContractError::ScopeReturnMissing.into());
            };
            if !matches!(
                map.body.output,
                plasm_core::plasm_monad::ScopedOutput::Rows {
                    entity_authority: true,
                    ..
                }
            ) {
                return Err(ScopeContractError::SyntheticScopeNoAuthority.into());
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
                .ok_or(ScopeContractError::UnionAuthorityMismatch.into())
        }
        ValidatedPlanNode::Compute(c) if c.compute.op.preserves_row_identity() => {
            let owner = capture_owner(nodes, &c.compute.source, depth + 1)?;
            Ok(owner)
        }
        _ => Err(ScopeContractError::IdentityNotPreserved.into()),
    }
}

fn validate_port_schema(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    source: &str,
    schema: &SyntheticResultSchema,
) -> Result<(), MapBodyValidationError> {
    for field in &schema.fields {
        let actual =
            crate::python_compute::source_field_kind(es, nodes, source, field.name.as_str(), 0)
                .map_err(|_| PythonFieldError::Unavailable {
                    node: source.to_owned(),
                    field: field.name.to_string(),
                })?;
        if field.value_type.as_ref() != Some(&actual) {
            return Err(ScopeContractError::PortSchemaMismatch.into());
        }
    }
    Ok(())
}
