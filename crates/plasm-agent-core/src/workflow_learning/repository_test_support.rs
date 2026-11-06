//! Explicit test-host authorization for existing evidence/portfolio fixtures.
use super::*;
use chrono::{Duration, Utc};

fn host(scope: &LibraryScope) -> training::TrustedTrainingActor {
    training::TrustedTrainingActor::trusted_host(
        scope.clone(),
        "fixture-host".into(),
        "fixture-host-key".into(),
    )
    .unwrap()
}

impl WorkflowRepository {
    pub(crate) async fn bind_training_fixture(
        &self,
        scope: &LibraryScope,
        logical: LogicalSessionId,
    ) -> Result<()> {
        self.bind_session(scope, logical).await?;
        let actor = host(scope);
        self.bind_training_controller(&actor, logical).await?;
        let status = self.training_status(scope, logical).await?;
        if matches!(status.training_state, training::TrainingState::Ordinary) {
            self.install_training_grant(
                &actor,
                Uuid::new_v4(),
                training::TrainingGrantSpec {
                    grantee: actor.subject().into(),
                    credential: actor.credential().into(),
                    actions: vec![
                        training::TrainingAction::Capture,
                        training::TrainingAction::Review,
                        training::TrainingAction::PublishAutomatically,
                        training::TrainingAction::Release,
                    ],
                    expires_at: Utc::now() + Duration::hours(1),
                    publication_policy: training::PublicationPolicy::DelegatedAutomatic,
                    target: training::TrainingGrantTarget::Session {
                        logical_session: logical,
                    },
                },
            )
            .await?;
            self.training_command(
                &actor,
                logical,
                &training::TrainingCommandEnvelope {
                    idempotency_key: Uuid::new_v4(),
                    expected_revision: status.revision,
                    command: training::TrainingCommand::Request,
                },
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn record_training_fixture(&self, receipt: &ExecutionReceipt) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let permit =
            training::lock_training_epoch(&mut tx, &receipt.scope, receipt.logical_session).await?;
        tx.commit().await?;
        self.record_execution(receipt, &permit).await
    }

    pub(crate) async fn review_training_fixture(
        &self,
        request: &ReviewRequest,
    ) -> Result<ReviewHandle> {
        let actor = host(&request.scope);
        let status = self
            .training_status(&request.scope, request.logical_session)
            .await?;
        self.review(
            request,
            &ReviewAuthorization {
                actor,
                expected_revision: status.revision,
                idempotency_key: Uuid::new_v4(),
            },
        )
        .await
    }

    pub(crate) async fn prepare_training_fixture(
        &self,
        request: &ReviewRequest,
    ) -> Result<PendingReview<'_>> {
        let actor = host(&request.scope);
        let status = self
            .training_status(&request.scope, request.logical_session)
            .await?;
        self.prepare_review(
            request,
            &ReviewAuthorization {
                actor,
                expected_revision: status.revision,
                idempotency_key: Uuid::new_v4(),
            },
        )
        .await
    }

    pub(crate) async fn publish_training_fixture(
        &self,
        scope: &LibraryScope,
        review: Uuid,
        hashes: &[String],
        subject: &str,
    ) -> Result<FrozenPublicationReceipt> {
        let mut connection = self.pool.acquire().await?;
        let guidance: serde_json::Value = sqlx::query_scalar(
            "SELECT associations FROM workflow_learning_guidance_batches WHERE review_id=$1",
        )
        .bind(review)
        .fetch_optional(&mut connection)
        .await?
        .unwrap_or_else(|| serde_json::json!([]));
        drop(connection);
        let digest = plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&guidance)?);
        let actor = training::TrustedTrainingActor::human_owner(
            scope.clone(),
            subject.into(),
            "fixture-owner-key".into(),
        )?;
        self.publish_frozen(scope, review, hashes, &digest, &actor)
            .await
    }

    pub(crate) async fn approve_training_fixture(
        &self,
        scope: &LibraryScope,
        review: Uuid,
        subject: &str,
    ) -> Result<()> {
        let hashes = self
            .export_proposals(scope)
            .await?
            .into_iter()
            .filter(|p| p.review_ref == review)
            .map(|p| p.candidate_hash)
            .collect::<Vec<_>>();
        self.publish_training_fixture(scope, review, &hashes, subject)
            .await?;
        Ok(())
    }
}
