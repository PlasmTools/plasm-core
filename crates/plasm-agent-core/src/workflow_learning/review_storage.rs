//! Durable typed review references and shared program-observation hydration.
use super::*;

pub(super) fn hydrate_row(
    scope: &LibraryScope,
    logical: LogicalSessionId,
    row: sqlx::postgres::PgRow,
) -> Result<ExecutionReceipt> {
    let revision: i64 = row.try_get("session_revision")?;
    let revision = u64::try_from(revision)
        .map_err(|_| RepositoryError::InvalidEvidence("invalid execution revision"))?;
    let wire: String = row.try_get("execution_id")?;
    let run = ExecutionId::from_wire(&wire).ok_or(RepositoryError::InvalidEvidence(
        "invalid execution identity",
    ))?;
    hydrate_fact(
        scope.clone(),
        logical,
        revision,
        run,
        row.try_get("receipt")?,
        &row.try_get::<String, _>("program_digest")?,
        row.try_get("observation")?,
    )
}

pub(super) fn hydrate_fact(
    scope: LibraryScope,
    logical: LogicalSessionId,
    revision: u64,
    run: ExecutionId,
    fact: serde_json::Value,
    program_digest: &str,
    observation: serde_json::Value,
) -> Result<ExecutionReceipt> {
    let program: ProgramEvidence = serde_json::from_value(observation)?;
    if program.digest()?.as_str() != program_digest {
        return Err(RepositoryError::ImmutableConflict);
    }
    Ok(ExecutionReceipt::hydrate(
        scope,
        logical,
        revision,
        run,
        serde_json::from_value(fact)?,
        program,
    ))
}

/// Resolve on the caller's connection/transaction, preserving its snapshot and
/// avoiding nested pool leases. All references and program digests are checked.
pub(crate) async fn hydrate_review<'e, E>(
    executor: E,
    value: serde_json::Value,
    expected_digest: &str,
) -> Result<SealedReview>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    if plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&value)?) != expected_digest
    {
        return Err(RepositoryError::ImmutableConflict);
    }
    let stored: StoredReviewSeal = serde_json::from_value(value)?;
    validate_review(&stored.request)?;
    if stored.request.subtasks.len() > 16
        || stored.executions.len() > 512
        || stored
            .request
            .subtasks
            .iter()
            .any(|subtask| subtask.execution_ids.len() > 32)
    {
        return Err(RepositoryError::InvalidEvidence(
            "review execution references exceed limits",
        ));
    }
    let requested: BTreeSet<_> = stored
        .request
        .subtasks
        .iter()
        .flat_map(|subtask| subtask.execution_ids.iter().copied())
        .collect();
    let references: BTreeSet<_> = stored
        .executions
        .iter()
        .map(|reference| reference.execution_id)
        .collect();
    if requested.is_empty()
        || requested != references
        || references.len() != stored.executions.len()
        || stored
            .executions
            .iter()
            .any(|reference| reference.session_revision > stored.request.session_revision)
    {
        return Err(RepositoryError::InvalidEvidence(
            "review does not bind exactly the requested execution facts",
        ));
    }
    let requested = serde_json::to_value(&stored.executions)?;
    let rows = sqlx::query("SELECT refs.ordinality AS position,r.receipt,r.program_digest,p.observation FROM jsonb_array_elements($1::jsonb) WITH ORDINALITY refs(value,ordinality) LEFT JOIN workflow_learning_receipts r ON r.tenant=$2 AND r.project=$3 AND r.logical_session=$4 AND r.session_revision=(refs.value->>'session_revision')::bigint AND r.execution_id=refs.value->>'execution_id' LEFT JOIN workflow_learning_program_observations p ON p.tenant=r.tenant AND p.project=r.project AND p.digest=r.program_digest ORDER BY refs.ordinality")
        .bind(requested).bind(&stored.request.scope.tenant).bind(&stored.request.scope.project)
        .bind(stored.request.logical_session.to_string()).fetch_all(executor).await?;
    if rows.len() != stored.executions.len() {
        return Err(RepositoryError::InvalidEvidence(
            "review execution references are incomplete",
        ));
    }
    let mut executions = Vec::with_capacity(rows.len());
    for (row, reference) in rows.into_iter().zip(&stored.executions) {
        let fact: Option<serde_json::Value> = row.try_get("receipt")?;
        let digest: Option<String> = row.try_get("program_digest")?;
        let observation: Option<serde_json::Value> = row.try_get("observation")?;
        if digest.as_deref() != Some(reference.program_digest.as_str()) {
            return Err(RepositoryError::ImmutableConflict);
        }
        let receipt = hydrate_fact(
            stored.request.scope.clone(),
            stored.request.logical_session,
            reference.session_revision,
            reference.execution_id,
            fact.ok_or(RepositoryError::InvalidEvidence(
                "review execution fact missing",
            ))?,
            &digest.ok_or(RepositoryError::InvalidEvidence(
                "review program digest missing",
            ))?,
            observation.ok_or(RepositoryError::InvalidEvidence(
                "review program observation missing",
            ))?,
        )?;
        if receipt.evidence_digest()? != reference.evidence_digest {
            return Err(RepositoryError::ImmutableConflict);
        }
        executions.push(receipt);
    }
    Ok(SealedReview {
        request: stored.request,
        executions,
        assurance: stored.assurance,
    })
}

pub(super) async fn store_program(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    receipt: &ExecutionReceipt,
) -> Result<String> {
    let program = receipt.program_evidence();
    let digest = program.digest()?.as_str().to_owned();
    let value = serde_json::to_value(program)?;
    let saved: Option<String> = sqlx::query_scalar("INSERT INTO workflow_learning_program_observations(tenant,project,digest,observation) VALUES($1,$2,$3,$4) ON CONFLICT(tenant,project,digest) DO UPDATE SET digest=workflow_learning_program_observations.digest WHERE workflow_learning_program_observations.observation=EXCLUDED.observation RETURNING digest")
        .bind(&receipt.scope.tenant).bind(&receipt.scope.project).bind(&digest).bind(value).fetch_optional(&mut tx.executor()).await?;
    if saved.as_deref() != Some(digest.as_str()) {
        return Err(RepositoryError::ImmutableConflict);
    }
    Ok(digest)
}
