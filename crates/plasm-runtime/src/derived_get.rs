//! List-backed keyed Gets (`derive:` on `kind: get`).

use indexmap::IndexMap;
use plasm_core::{
    CatalogEntryStamp, DerivedGetPlan, GetExpr, Predicate, QueryExpr, Ref, TypedFieldValue, Value,
    CGS,
};

use crate::cache::{CachedEntity, EntityCompleteness};
use crate::execution::{
    current_timestamp, ExecutionEngine, ExecutionMode, ExecutionSource, StreamConsumeOpts,
};
use crate::materialization::SessionMaterialization;
use crate::value_match::{entities_matching_field_value, value_to_match_string};
use crate::view_plan::ViewAmbientContext;
use crate::RuntimeError;

/// Execute a derived Get: run source Query to completion, unique-match, project.
pub(crate) async fn execute_derived_get(
    engine: &ExecutionEngine,
    plan: &DerivedGetPlan,
    get: &GetExpr,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    ambient: &ViewAmbientContext,
) -> Result<(CachedEntity, ExecutionSource), RuntimeError> {
    let identity = get.reference.primary_slot_str();
    let needle = Value::String(identity.clone());

    let source_capability = cgs
        .get_capability(plan.source_query.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: plan.source_query.to_string(),
            entity: get.reference.entity_type.to_string(),
        })?;

    let mut query = QueryExpr::all(source_capability.domain.clone());
    query.capability_name = Some(plan.source_query.clone());
    if let Some(eid) = get.catalog_entry_id.as_ref() {
        query.catalog_entry_id = CatalogEntryStamp::some(eid.clone());
    }

    let consume = StreamConsumeOpts {
        fetch_all: true,
        ..Default::default()
    };
    // A derived Get must use the same per-reference bindings as a wire Get.
    // These are scoped to this invocation; never bleed them into ambient state.
    let catalog_key =
        SessionMaterialization::provide_catalog_key(cgs, get.catalog_entry_id.as_deref());
    let mut bindings = ambient.capability_params.clone();
    bindings.extend(cache.capability_params_for_get(&get.reference, &catalog_key));
    let mut predicates: Vec<_> = source_capability
        .scope_params()
        .iter()
        .chain(source_capability.selection_params())
        .filter_map(|field| {
            bindings
                .get(&field.name)
                .map(|value| Predicate::eq(field.name.clone(), value.clone()))
        })
        .collect();
    query.predicate = match predicates.len() {
        0 => None,
        1 => predicates.pop(),
        _ => Some(Predicate::and(predicates)),
    };
    let result = engine
        .execute_query(&query, cgs, cache, mode, consume, ambient)
        .await?;

    if result.has_more {
        return Err(RuntimeError::DerivedGetIncompleteSource {
            capability: plan.get_capability.to_string(),
        });
    }

    let matches = entities_matching_field_value(&result.entities, &plan.match_field, &needle);
    let matched = match matches.len() {
        0 => {
            return Err(RuntimeError::DerivedGetNotFound {
                capability: plan.get_capability.to_string(),
                match_field: plan.match_field.clone(),
                identity: value_to_match_string(&needle),
            });
        }
        1 => matches[0],
        n => {
            return Err(RuntimeError::DerivedGetNonUnique {
                capability: plan.get_capability.to_string(),
                match_field: plan.match_field.clone(),
                identity: value_to_match_string(&needle),
                matches: n,
            });
        }
    };

    let mut projected: IndexMap<String, Value> = IndexMap::new();
    for (target_field, source_field) in &plan.projection {
        let v = matched
            .fields
            .get(source_field.as_str())
            .map(TypedFieldValue::to_value)
            .ok_or_else(|| RuntimeError::DerivedGetSourceFieldMissing {
                capability: plan.get_capability.to_string(),
                field: source_field.clone(),
            })?;
        projected.insert(target_field.clone(), v);
    }

    // Ensure identity field is present even if projection omitted it somehow.
    if !projected.contains_key(plan.identity_field.as_str()) {
        projected.insert(plan.identity_field.clone(), needle);
    }

    let reference = Ref::new(get.reference.entity_type.clone(), identity);
    let timestamp = current_timestamp();
    let cached = CachedEntity::from_decoded(
        reference,
        projected,
        IndexMap::new(),
        timestamp,
        EntityCompleteness::Complete,
    );
    Ok((cached, result.source))
}
