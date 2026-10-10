//! Admission of recursive bounded scopes into the ordinary host plan.
use crate::{execute_session::ExecuteSession, plasm_plan::*};
use thiserror::Error;

fn relation_materialization_label(
    materialization: &plasm_core::RelationMaterialization,
) -> &'static str {
    use plasm_core::RelationMaterialization as Materialization;

    match materialization {
        Materialization::Unavailable => "unavailable",
        Materialization::FromParentGet { .. } => "embedded parent GET",
        Materialization::PreferFromParentGet { .. } => "preferred embedded parent GET",
        Materialization::QueryScoped { .. } => "scoped target query",
        Materialization::QueryScopedBindings { .. } => "scoped target query bindings",
        Materialization::GetScopedBindings { .. } => "scoped target GET bindings",
        Materialization::ViewEmbed { .. } => "composed view relation output",
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::ScopeContractError;

    #[test]
    fn relation_materialization_error_uses_semantic_labels() {
        let error = ScopeContractError::RelationMaterializationMismatch {
            declared: Box::new(plasm_core::RelationMaterialization::Unavailable),
            lowered: Box::new(plasm_core::RelationMaterialization::ViewEmbed {
                view: "private-view-name".into(),
            }),
        };

        let diagnostic = error.to_string();
        assert_eq!(
            diagnostic,
            "body relation materialization differs from catalog: declared unavailable, lowered composed view relation output"
        );
        assert!(!diagnostic.contains("private-view-name"));
    }
}

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
    #[error(
        "body relation materialization differs from catalog: declared {declared_label}, lowered {lowered_label}",
        declared_label = relation_materialization_label(.declared),
        lowered_label = relation_materialization_label(.lowered)
    )]
    RelationMaterializationMismatch {
        declared: Box<plasm_core::RelationMaterialization>,
        lowered: Box<plasm_core::RelationMaterialization>,
    },
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
    Invalid(#[source] Box<crate::map_body_schema::MapBodySchemaError>),
}

impl From<crate::map_body_schema::MapBodySchemaError> for MapSchemaError {
    fn from(error: crate::map_body_schema::MapBodySchemaError) -> Self {
        Self::Invalid(Box::new(error))
    }
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
    validate_capture_port(es, nodes, &body.parent)?;
    for port in &body.captures {
        validate_capture_port(es, nodes, port)?;
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
            ValidatedPlanNode::Capture(c) if c.contract.entity_authority() => Some(&c.entity),
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
            if let Some(schema) = capture.contract.schema() {
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
        validate_relation_materialization(
            declared.materialize.as_ref(),
            &relation.relation.materialize,
        )?;
    }
    crate::map_body_schema::output_schema(es, body).map_err(MapSchemaError::from)?;
    crate::plan_session_provisions::validate(es, map.plan.nodes(), &body.body.bind)?;
    Ok(())
}

#[cfg(test)]
mod tests;

fn validate_relation_materialization(
    declared: Option<&plasm_core::RelationMaterialization>,
    lowered: &plasm_core::RelationMaterialization,
) -> Result<(), ScopeContractError> {
    let declared = declared.unwrap_or(&plasm_core::RelationMaterialization::Unavailable);
    if declared != lowered {
        return Err(ScopeContractError::RelationMaterializationMismatch {
            declared: Box::new(declared.clone()),
            lowered: Box::new(lowered.clone()),
        });
    }
    Ok(())
}

fn validate_capture_port(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    port: &impl plasm_core::plasm_monad::CapturePort,
) -> Result<(), MapBodyValidationError> {
    use plasm_core::plasm_monad::CaptureCardinality;
    let source = port.source().as_str();
    let cardinality = port.cardinality();
    if cardinality == CaptureCardinality::Singleton
        && !crate::plasm_plan::scoped_capture_permits_singleton(nodes, source)
    {
        return Err(ScopeContractError::PluralCapturePromoted.into());
    }
    if let Some(value) = port.contract().value_contract() {
        if cardinality == CaptureCardinality::Collection
            || &crate::map_body_schema::row_contract(es, nodes, source)
                .map_err(MapSchemaError::from)?
                != value
        {
            return Err(ScopeContractError::ScalarCaptureContractChanged.into());
        }
    } else if let Some(schema) = port.contract().schema() {
        validate_port_schema(es, nodes, source, schema)?;
    } else {
        let owner = port.entity();
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            es,
            &owner.entry_id,
            &owner.entity,
        )
        .map_err(|_| CatalogOwnershipError::EntityUnavailable {
            catalog_entry: owner.entry_id.clone(),
            entity: owner.entity.clone(),
        })?;
        for field in cgs
            .get_entity(&owner.entity)
            .ok_or(ScopeContractError::ParentEntityMissing)?
            .fields
            .keys()
        {
            crate::python_compute::source_field_kind(es, nodes, source, field.as_str(), 0)
                .map_err(|_| PythonFieldError::Unavailable {
                    node: source.to_owned(),
                    field: field.to_string(),
                })?;
        }
    }
    if port.contract().entity_authority() {
        let owner = capture_owner(nodes, source, 0)?;
        if owner.entry_id != port.entity().entry_id || owner.entity != port.entity().entity {
            return Err(if cardinality == CaptureCardinality::ParentOccurrence {
                ScopeContractError::ParentIdentityChanged
            } else {
                ScopeContractError::CaptureOwnerMismatch
            }
            .into());
        }
    }
    Ok(())
}

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
        ValidatedPlanNode::Capture(c) if c.contract.entity_authority() => Ok(&c.entity),
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
