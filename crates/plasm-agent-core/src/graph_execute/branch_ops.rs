//! Pure fork/commit operations on [`SessionMaterialization`] (epoch CAS + absorb).

use plasm_runtime::SessionMaterialization;

use crate::execute_session::GraphEpoch;

use super::GraphCommitError;

/// Snapshot graph + clone response/query stores for branch execute.
pub(crate) fn fork_materialization(
    hot: &SessionMaterialization,
) -> (SessionMaterialization, GraphEpoch) {
    let parent_epoch = GraphEpoch(hot.stats().version);
    let graph = hot.snapshot().into_graph();
    (
        SessionMaterialization {
            graph,
            responses: hot.responses.clone(),
            query_index: hot.query_index.clone(),
        },
        parent_epoch,
    )
}

/// Merge branch when session graph epoch matches `parent_epoch`.
pub(crate) fn commit_materialization(
    session: &mut SessionMaterialization,
    parent_epoch: GraphEpoch,
    branch: SessionMaterialization,
) -> Result<GraphEpoch, GraphCommitError> {
    let found = GraphEpoch(session.stats().version);
    if found != parent_epoch {
        return Err(GraphCommitError::StaleParentEpoch {
            expected: parent_epoch,
            found,
        });
    }
    session
        .absorb_branch(branch)
        .map_err(GraphCommitError::Merge)?;
    Ok(GraphEpoch(session.stats().version))
}
