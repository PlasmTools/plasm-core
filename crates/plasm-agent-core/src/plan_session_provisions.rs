//! Plan-local witnesses for catalog-scoped action `provides`.
//!
//! Provider ordering is sealed in bind.deps. Dry witnesses are typed placeholders
//! in an isolated materialization; only real execution can authenticate a session.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use plasm_core::{CapabilityKind, Expr, InputType, PlasmBindGraph, StepId, Value};
use plasm_runtime::MutexGraphCacheSession;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{ValidatedPlanNode, ValidatedSurfaceNode};

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

fn catalog<'a>(es: &'a ExecuteSession, entry: &str) -> Result<&'a plasm_core::CGS, String> {
    es.contexts_by_entry
        .get(entry)
        .map(|ctx| ctx.cgs.as_ref())
        .ok_or_else(|| format!("session provisions: unknown catalog `{entry}`"))
}

fn provider(
    es: &ExecuteSession,
    node: &ValidatedPlanNode,
) -> Result<Vec<(ProvisionKey, Value)>, String> {
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
        .ok_or_else(|| {
            format!(
                "session provisions: unknown capability `{}`",
                invoke.capability
            )
        })?;
    if cap.provides.is_empty() {
        return Ok(vec![]);
    }
    let entity = cgs
        .get_entity(cap.domain.as_str())
        .ok_or_else(|| format!("session provisions: unknown entity `{}`", cap.domain))?;
    cap.provides
        .iter()
        .map(|name| {
            let field = entity
                .fields
                .get(name.as_str())
                .ok_or_else(|| format!("session provisions: unknown provided field `{name}`"))?;
            let named = field.named_value(cgs).map_err(|e| e.to_string())?;
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

fn get_inputs(es: &ExecuteSession, entry: &str, expr: &Expr) -> Result<Vec<ProvisionKey>, String> {
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
    .ok_or_else(|| {
        format!(
            "session provisions: missing Get for `{}`",
            get.reference.entity_type
        )
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
) -> Result<BTreeMap<StepId, BTreeSet<StepId>>, String> {
    let by_id: BTreeMap<_, _> = nodes.iter().map(|n| (n.id().as_str(), n)).collect();
    let mut available: BTreeMap<ProvisionKey, StepId> = BTreeMap::new();
    let mut deps: BTreeMap<StepId, BTreeSet<StepId>> = BTreeMap::new();
    let mut readers: BTreeMap<ProvisionKey, BTreeSet<StepId>> = BTreeMap::new();
    for id in &bind.topo {
        let node = by_id
            .get(id.as_str())
            .ok_or_else(|| format!("missing provision step `{id}`"))?;
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
) -> Result<(), String> {
    for (target, sources) in dependencies(es, nodes, bind)? {
        bind.deps.entry(target).or_default().extend(sources);
    }
    Ok(())
}

pub(crate) fn validate(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    bind: &PlasmBindGraph,
) -> Result<(), String> {
    for (target, sources) in dependencies(es, nodes, bind)? {
        if !bind
            .deps
            .get(&target)
            .is_some_and(|actual| sources.is_subset(actual))
        {
            return Err(format!(
                "step `{target}` is missing catalog session provider dependencies"
            ));
        }
    }
    Ok(())
}

/// Owns isolated preflight state. Its API cannot stamp placeholders into a live session.
pub(crate) struct DryProvisionSession {
    session: ExecuteSession,
}

impl DryProvisionSession {
    pub(crate) fn new(es: &ExecuteSession) -> Result<Self, String> {
        let mat = es
            .graph_cache
            .try_lock()
            .map_err(|_| "session graph locked during provision preflight".to_string())?
            .clone();
        let mut session = es.clone();
        session.graph_cache = Arc::new(MutexGraphCacheSession::new_materialization(mat));
        Ok(Self { session })
    }

    pub(crate) fn session(&self) -> &ExecuteSession {
        &self.session
    }

    pub(crate) fn stage(&self, node: &ValidatedPlanNode) -> Result<(), String> {
        let values = provider(&self.session, node)?;
        if values.is_empty() {
            return Ok(());
        }
        let mut mat = self
            .session
            .graph_cache
            .try_lock()
            .map_err(|_| "dry session graph locked during provision staging".to_string())?;
        for (key, value) in values {
            mat.stamp_provided_session_params(
                key.entry_id,
                indexmap::IndexMap::from([(key.field, value)]),
            );
        }
        Ok(())
    }
}
