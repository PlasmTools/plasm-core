//! Centralized federation scoping policy for plan nodes.

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::ValidatedPlanNode;
use crate::plasm_plan_run::entry_scoped_execute_session;
use crate::program_diagnostic::ProgramStageError;
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
) -> Result<SessionScope<'a>, ProgramStageError> {
    match node {
        ValidatedPlanNode::Surface(surface) => {
            if surface.qualified_entity.is_some() {
                let scoped = entry_scoped_execute_session(es, surface.qualified_entity.as_ref())
                    .map_err(|error| ProgramStageError::SessionCatalog { error })?;
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
                ProgramStageError::plan(
                    crate::program_diagnostic::PlanStageError::FederationRequired,
                )
            })?;
            Ok(SessionScope::Federated {
                session: es,
                dispatch: fed,
            })
        }
        ValidatedPlanNode::ForEach(fe) => {
            let scoped =
                entry_scoped_execute_session(es, Some(&fe.effect_template.qualified_entity))
                    .map_err(|error| ProgramStageError::SessionCatalog { error })?;
            Ok(SessionScope::EntryScoped { session: scoped })
        }
        ValidatedPlanNode::IterateUntil(it) => {
            let scoped =
                entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))
                    .map_err(|error| ProgramStageError::SessionCatalog { error })?;
            Ok(SessionScope::EntryScoped { session: scoped })
        }
        ValidatedPlanNode::MapBody(_)
        | ValidatedPlanNode::Capture(_)
        | ValidatedPlanNode::Compute(_)
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
