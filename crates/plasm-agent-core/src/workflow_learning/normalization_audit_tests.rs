use super::super::super::normalization::{NormalizationChat, NormalizationDiagnostic};
use super::*;
use std::collections::BTreeSet;
fn request() -> NormalizationRequest {
    NormalizationChat::configured(None)
        .unwrap()
        .request(serde_json::json!({"subtask":"inspect"}), BTreeSet::new())
        .unwrap()
}
#[test]
fn bounded_response_retains_hash_and_explicit_usage_state() {
    let raw = format!(
        "{{\"padding\":\"{}\",\"usage\":{{\"prompt_tokens\":19}}}}",
        "🦀".repeat(20_000)
    );
    let mut capture = NormalizationCapture::default();
    capture.response(200, &raw);
    let evidence = capture.response.unwrap();
    assert!(evidence.raw.truncated);
    assert!(evidence.raw.prefix.len() <= RAW_LIMIT);
    assert_eq!(evidence.raw.bytes, raw.len());
    assert_eq!(
        evidence.raw.sha256,
        plasm_core::catalog_discovery::content_hash(raw.as_bytes())
    );
    assert!(matches!(evidence.usage, UsageEvidence::Reported { .. }));
    let mut capture = NormalizationCapture::default();
    capture.response(200, "{}");
    assert!(matches!(
        capture.response.unwrap().usage,
        UsageEvidence::NotReported
    ));
    let mut capture = NormalizationCapture::default();
    capture.response(502, "not json");
    assert!(matches!(
        capture.response.unwrap().usage,
        UsageEvidence::Unavailable {
            reason: UsageUnavailable::MalformedEnvelope
        }
    ));
}
#[tokio::test]
async fn postgres_normalization_audit_preserves_pending_rejections_usage_and_scope() {
    let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
        return;
    };
    let repository = WorkflowRepository::connect(&url).await.unwrap();
    repository.migrate().await.unwrap();
    let scope = LibraryScope {
        tenant: format!("normalization-audit-{}", Uuid::new_v4()),
        project: "fixture".into(),
    };
    let request = request();
    let abandoned_id = {
        let abandoned = repository
            .begin_normalization(NormalizationContext::recall(scope.clone()), &request)
            .await
            .unwrap();
        abandoned.id()
    };
    let rejected = repository
        .begin_normalization(NormalizationContext::recall(scope.clone()), &request)
        .await
        .unwrap();
    let raw = r#"{"choices":[{"message":{"content":"invalid proposal"}}],"usage":{"prompt_tokens":9,"completion_tokens":2}}"#;
    let error = request.decode(raw).unwrap_err();
    let mut capture = NormalizationCapture::default();
    capture.response(200, raw);
    repository
        .finish_normalization(
            &rejected,
            capture.clone(),
            NormalizationOutcome::Rejected {
                diagnostic: error.diagnostic(),
            },
        )
        .await
        .unwrap();
    repository
        .finish_normalization(
            &rejected,
            capture.clone(),
            NormalizationOutcome::Rejected {
                diagnostic: error.diagnostic(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        repository
            .finish_normalization(
                &rejected,
                capture,
                NormalizationOutcome::Rejected {
                    diagnostic: NormalizationDiagnostic::MissingChoice
                }
            )
            .await,
        Err(RepositoryError::ImmutableConflict)
    ));
    let accepted = repository
        .begin_normalization(NormalizationContext::recall(scope.clone()), &request)
        .await
        .unwrap();
    let content = serde_json::json!({"intent":{"purpose":"inspect","facets":[],"aliases":[]},"family":{"kind":"new"}});
    let raw =
        serde_json::json!({"choices":[{"message":{"content":content.to_string()}}]}).to_string();
    let proposal = request.decode(&raw).unwrap();
    let mut capture = NormalizationCapture::default();
    capture.response(200, &raw);
    repository
        .finish_normalization(
            &accepted,
            capture,
            NormalizationOutcome::Accepted { proposal },
        )
        .await
        .unwrap();
    let rows = repository.normalization_audits(&scope, 100).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .find(|row| row["id"] == abandoned_id.to_string())
            .unwrap()["state"],
        "pending"
    );
    let rejection = rows
        .iter()
        .find(|row| row["id"] == rejected.id().to_string())
        .unwrap();
    assert_eq!(
        rejection["result"]["capture"]["response"]["usage"]["status"],
        "reported"
    );
    assert_eq!(
        rejection["result"]["result"]["diagnostic"]["predicate"],
        "proposal"
    );
    assert_eq!(rejection["model"], "openai/gpt-6-luna");
    assert_eq!(rejection["request_hash"], request.request_hash());
    let other = LibraryScope {
        tenant: scope.tenant.clone(),
        project: "other".into(),
    };
    assert!(repository
        .normalization_audits(&other, 100)
        .await
        .unwrap()
        .is_empty());
    let mut connection = repository.pool.acquire().await.unwrap();
    sqlx::query("DELETE FROM workflow_learning_normalization_calls WHERE tenant=$1")
        .bind(&scope.tenant)
        .execute(&mut connection)
        .await
        .unwrap();
}
