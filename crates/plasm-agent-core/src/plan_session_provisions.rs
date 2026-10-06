//! Plan-local witnesses for catalog-scoped action `provides`.
//!
//! Provider ordering is sealed in bind.deps. Dry witnesses are typed placeholders
//! in an isolated materialization; only real execution can authenticate a session.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use thiserror::Error;

use plasm_core::{CapabilityKind, Expr, InputType, PlasmBindGraph, StepId, Value};
use plasm_runtime::MutexGraphCacheSession;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{ValidatedPlanNode, ValidatedSurfaceNode};

#[derive(Debug, Clone, Error)]
pub enum SessionProvisionError {
    #[error("session provisions reference unloaded catalog `{entry}`")]
    CatalogNotLoaded { entry: String },
    #[error("session provisions reference unknown capability `{capability}`")]
    CapabilityNotFound { capability: String },
    #[error("session provisions reference unknown entity `{entity}`")]
    EntityNotFound { entity: String },
    #[error("session provisions reference unknown provided field `{field}`")]
    ProvidedFieldNotFound { field: String },
    #[error("provided field value schema is invalid: {0}")]
    FieldSchema(#[from] Box<plasm_core::SchemaError>),
    #[error("Get capability is missing for entity `{entity}`")]
    GetCapabilityNotFound { entity: String },
    #[error("plan bind graph references missing provision step `{step}`")]
    StepNotFound { step: String },
    #[error("step `{step}` is missing catalog session provider dependencies")]
    MissingProviderDependencies { step: String },
    #[error("session graph is locked during provision preflight")]
    GraphLockedDuringPreflight,
    #[error("dry session graph is locked during provision staging")]
    GraphLockedDuringStaging,
}

impl From<plasm_core::SchemaError> for SessionProvisionError {
    fn from(error: plasm_core::SchemaError) -> Self {
        Self::FieldSchema(Box::new(error))
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProvisionKey {
    entry_id: String,
    field: String,
}

fn surface_expr(surface: &ValidatedSurfaceNode) -> Option<&Expr> {
    surface
        .ir
        .as_ref()
        .map(|ir| &ir.expr)
        .or_else(|| surface.ir_template.as_ref().map(|ir| &ir.expr))
}

fn catalog<'a>(
    es: &'a ExecuteSession,
    entry: &str,
) -> Result<&'a plasm_core::CGS, SessionProvisionError> {
    es.contexts_by_entry
        .get(entry)
        .map(|ctx| ctx.cgs.as_ref())
        .ok_or_else(|| SessionProvisionError::CatalogNotLoaded {
            entry: entry.to_string(),
        })
}

fn provider(
    es: &ExecuteSession,
    node: &ValidatedPlanNode,
) -> Result<Vec<(ProvisionKey, Value)>, SessionProvisionError> {
    let Some(surface) = node.as_surface() else {
        return Ok(vec![]);
    };
    let Some(Expr::Invoke(invoke)) = surface_expr(surface) else {
        return Ok(vec![]);
    };
    let entry = surface
        .qualified_entity
        .as_ref()
        .map(|q| q.entry_id.as_str())
        .unwrap_or(&es.entry_id);
    let cgs = catalog(es, entry)?;
    let cap = cgs
        .get_capability(invoke.capability.as_str())
        .ok_or_else(|| SessionProvisionError::CapabilityNotFound {
            capability: invoke.capability.to_string(),
        })?;
    if cap.provides.is_empty() {
        return Ok(vec![]);
    }
    let entity = cgs.get_entity(cap.domain.as_str()).ok_or_else(|| {
        SessionProvisionError::EntityNotFound {
            entity: cap.domain.to_string(),
        }
    })?;
    cap.provides
        .iter()
        .map(|name| {
            let field = entity.fields.get(name.as_str()).ok_or_else(|| {
                SessionProvisionError::ProvidedFieldNotFound {
                    field: name.to_string(),
                }
            })?;
            let named = field.named_value(cgs)?;
            Ok((
                ProvisionKey {
                    entry_id: entry.to_string(),
                    field: name.to_string(),
                },
                plasm_core::dry_stub_value_for_named_value(named, 0),
            ))
        })
        .collect()
}

fn get_inputs(
    es: &ExecuteSession,
    entry: &str,
    expr: &Expr,
) -> Result<Vec<ProvisionKey>, SessionProvisionError> {
    let get = match expr {
        Expr::Get(get) => get,
        Expr::Chain(chain) => {
            // The chain source is a reference to an already-produced row, not
            // necessarily a Get request (query-only view producers are lawful).
            if let Expr::Get(source) = chain.source.as_ref() {
                let source_entry = source.catalog_entry_id.as_deref().unwrap_or(entry);
                let cgs = catalog(es, source_entry)?;
                if source.capability_name.is_none()
                    && cgs
                        .find_capability(&source.reference.entity_type, CapabilityKind::Get)
                        .is_none()
                {
                    return Ok(vec![]);
                }
            }
            return get_inputs(es, entry, &chain.source);
        }
        _ => return Ok(vec![]),
    };
    let entry = get.catalog_entry_id.as_deref().unwrap_or(entry);
    let cgs = catalog(es, entry)?;
    let cap = match get.capability_name.as_deref() {
        Some(name) => cgs.get_capability(name),
        None => cgs.find_capability(&get.reference.entity_type, CapabilityKind::Get),
    }
    .ok_or_else(|| SessionProvisionError::GetCapabilityNotFound {
        entity: get.reference.entity_type.to_string(),
    })?;
    let Some(args) = &cap.inputs.arguments else {
        return Ok(vec![]);
    };
    let InputType::Object { fields, .. } = &args.input_type else {
        return Ok(vec![]);
    };
    Ok(fields
        .iter()
        .map(|f| ProvisionKey {
            entry_id: entry.to_string(),
            field: f.name.to_string(),
        })
        .collect())
}

/// Derive implicit Get dependencies from earlier catalog providers, independently
/// of any serialized dependency claims. Latest provider wins, matching runtime.
pub(crate) fn dependencies(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    bind: &PlasmBindGraph,
) -> Result<BTreeMap<StepId, BTreeSet<StepId>>, SessionProvisionError> {
    let by_id: BTreeMap<_, _> = nodes.iter().map(|n| (n.id().as_str(), n)).collect();
    let mut available: BTreeMap<ProvisionKey, StepId> = BTreeMap::new();
    let mut deps: BTreeMap<StepId, BTreeSet<StepId>> = BTreeMap::new();
    let mut readers: BTreeMap<ProvisionKey, BTreeSet<StepId>> = BTreeMap::new();
    for id in &bind.topo {
        let node = by_id
            .get(id.as_str())
            .ok_or_else(|| SessionProvisionError::StepNotFound {
                step: id.to_string(),
            })?;
        let consumer = match node {
            ValidatedPlanNode::Surface(surface) => surface_expr(surface).map(|expr| {
                (
                    surface
                        .qualified_entity
                        .as_ref()
                        .map(|q| q.entry_id.as_str())
                        .unwrap_or(&es.entry_id),
                    expr,
                )
            }),
            ValidatedPlanNode::ForEach(node) => Some((
                node.effect_template.qualified_entity.entry_id.as_str(),
                &node.effect_template.ir_template.expr,
            )),
            ValidatedPlanNode::IterateUntil(node) => Some((
                node.effect_template.qualified_entity.entry_id.as_str(),
                &node.effect_template.ir_template.expr,
            )),
            ValidatedPlanNode::RelationTraversal(node) => Some((
                node.relation.target.entry_id.as_str(),
                &node.relation.ir.expr,
            )),
            _ => None,
        };
        if let Some((entry, expr)) = consumer {
            for key in get_inputs(es, entry, expr)? {
                readers.entry(key.clone()).or_default().insert(id.clone());
                if let Some(source) = available.get(&key) {
                    deps.entry(id.clone()).or_default().insert(source.clone());
                }
            }
        }
        for (key, _) in provider(es, node)? {
            // A later provider must not overwrite the session value before earlier
            // consumers observe it, even when those consumers have other dependencies.
            if let Some(prior_readers) = readers.remove(&key) {
                deps.entry(id.clone()).or_default().extend(prior_readers);
            }
            // Repeated login must not be moved ahead of an earlier provider.
            if let Some(source) = available.insert(key, id.clone()) {
                if source != *id {
                    deps.entry(id.clone()).or_default().insert(source);
                }
            }
        }
    }
    Ok(deps)
}

pub(crate) fn seal(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    bind: &mut PlasmBindGraph,
) -> Result<(), SessionProvisionError> {
    for (target, sources) in dependencies(es, nodes, bind)? {
        bind.deps.entry(target).or_default().extend(sources);
    }
    Ok(())
}

pub(crate) fn validate(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    bind: &PlasmBindGraph,
) -> Result<(), SessionProvisionError> {
    for (target, sources) in dependencies(es, nodes, bind)? {
        if !bind
            .deps
            .get(&target)
            .is_some_and(|actual| sources.is_subset(actual))
        {
            return Err(SessionProvisionError::MissingProviderDependencies {
                step: target.to_string(),
            });
        }
    }
    Ok(())
}

/// Owns isolated preflight state. Its API cannot stamp placeholders into a live session.
pub(crate) struct DryProvisionSession {
    session: ExecuteSession,
}

impl DryProvisionSession {
    pub(crate) fn new(es: &ExecuteSession) -> Result<Self, SessionProvisionError> {
        let mat = es
            .graph_cache
            .try_lock()
            .map_err(|_| SessionProvisionError::GraphLockedDuringPreflight)?
            .clone();
        let mut session = es.clone();
        session.graph_cache = Arc::new(MutexGraphCacheSession::new_materialization(mat));
        Ok(Self { session })
    }

    pub(crate) fn session(&self) -> &ExecuteSession {
        &self.session
    }

    pub(crate) fn stage(&self, node: &ValidatedPlanNode) -> Result<(), SessionProvisionError> {
        let values = provider(&self.session, node)?;
        if values.is_empty() {
            return Ok(());
        }
        let mut mat = self
            .session
            .graph_cache
            .try_lock()
            .map_err(|_| SessionProvisionError::GraphLockedDuringStaging)?;
        for (key, value) in values {
            mat.stamp_provided_session_params(
                key.entry_id,
                indexmap::IndexMap::from([(key.field, value)]),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod error_tests {
    use super::SessionProvisionError;

    #[test]
    fn field_schema_error_retains_typed_cause() {
        assert!(std::mem::size_of::<SessionProvisionError>() < 128);
        let error = SessionProvisionError::from(plasm_core::SchemaError::DuplicateEntity {
            name: "FixtureEntity".into(),
        });
        assert!(matches!(
            error.clone(),
            SessionProvisionError::FieldSchema(source)
                if matches!(source.as_ref(), plasm_core::SchemaError::DuplicateEntity { name }
                    if name == "FixtureEntity")
        ));
        assert!(matches!(
            std::error::Error::source(&error)
                .unwrap()
                .downcast_ref::<Box<plasm_core::SchemaError>>()
                .map(Box::as_ref),
            Some(plasm_core::SchemaError::DuplicateEntity { name }) if name == "FixtureEntity"
        ));
    }
}
