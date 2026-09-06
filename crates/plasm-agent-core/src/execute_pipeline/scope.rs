//! Centralized federation scoping policy for plan nodes.

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::ValidatedPlanNode;
use crate::plasm_plan_run::entry_scoped_execute_session;
use plasm_core::cgs_federation::FederationDispatch;
use std::sync::Arc;

/// Which session view applies when type-checking or executing a plan node.
#[allow(clippy::large_enum_variant)]
pub enum SessionScope<'a> {
    Federated {
        session: &'a ExecuteSession,
        dispatch: Arc<FederationDispatch>,
    },
    EntryScoped {
        session: ExecuteSession,
    },
}

/// Resolve the execute session view for a validated plan node (dry and live both call this).
pub fn session_scope_for_node<'a>(
    es: &'a ExecuteSession,
    node: &ValidatedPlanNode,
    federation: Option<Arc<FederationDispatch>>,
) -> Result<SessionScope<'a>, String> {
    match node {
        ValidatedPlanNode::Surface(surface) => {
            if surface.qualified_entity.is_some() {
                let scoped = entry_scoped_execute_session(es, surface.qualified_entity.as_ref())?;
                Ok(SessionScope::EntryScoped { session: scoped })
            } else if let Some(fed) = federation {
                Ok(SessionScope::Federated {
                    session: es,
                    dispatch: fed,
                })
            } else {
                Ok(SessionScope::EntryScoped {
                    session: es.clone(),
                })
            }
        }
        ValidatedPlanNode::RelationTraversal(rel) => {
            let _ = rel;
            let fed = federation.ok_or_else(|| {
                "relation traversal requires federated session dispatch".to_string()
            })?;
            Ok(SessionScope::Federated {
                session: es,
                dispatch: fed,
            })
        }
        ValidatedPlanNode::ForEach(fe) => {
            let scoped =
                entry_scoped_execute_session(es, Some(&fe.effect_template.qualified_entity))?;
            Ok(SessionScope::EntryScoped { session: scoped })
        }
        ValidatedPlanNode::IterateUntil(it) => {
            let scoped =
                entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))?;
            Ok(SessionScope::EntryScoped { session: scoped })
        }
        ValidatedPlanNode::Compute(_)
        | ValidatedPlanNode::Derive(_)
        | ValidatedPlanNode::Data(_) => {
            if let Some(fed) = federation {
                Ok(SessionScope::Federated {
                    session: es,
                    dispatch: fed,
                })
            } else {
                Ok(SessionScope::EntryScoped {
                    session: es.clone(),
                })
            }
        }
    }
}
