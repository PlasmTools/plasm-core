//! Pure fork/commit operations on [`SessionMaterialization`] (per-store optimistic validation + absorb).

use plasm_runtime::{
    detect_materialization_conflicts, BranchMaterializationBase, SessionMaterialization,
};

use super::GraphCommitError;

/// Snapshot graph + clone response/query stores for branch execute.
pub(crate) fn fork_materialization(
    hot: &SessionMaterialization,
) -> (SessionMaterialization, BranchMaterializationBase) {
    BranchMaterializationBase::fork_from(hot)
}

/// Merge branch when no write-set entry changed in the session since fork.
pub(crate) fn commit_materialization(
    session: &mut SessionMaterialization,
    base: BranchMaterializationBase,
    branch: SessionMaterialization,
) -> Result<(), GraphCommitError> {
    let conflicts = detect_materialization_conflicts(session, &base, &branch);
    if conflicts.has_any() {
        return Err(GraphCommitError::WriteConflict(conflicts));
    }
    session
        .absorb_branch(branch)
        .map_err(GraphCommitError::Merge)?;
    Ok(())
}
