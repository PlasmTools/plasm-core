//! Explicit, revision-bound human decisions for cross-family teaching curation.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const PREFIX: &str = "workflow-recuration-v1:";

#[derive(Debug, thiserror::Error)]
pub enum CrossFamilyRecurationError {
    #[error("cross-family curation requires an authenticated human owner")]
    Denied,
    #[error("example revisions are unavailable or changed")]
    StaleRevision,
    #[error("examples do not have a validated equivalent parameterized presentation")]
    NotEquivalent,
    #[error("the complete alternate intent exceeds the bounded alias contract")]
    IntentTooLarge,
    #[error("supporting evidence is unavailable")]
    MissingEvidence,
    #[error("workflow learning database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("workflow learning serialization: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

type Result<T> = std::result::Result<T, CrossFamilyRecurationError>;

/// Imported decisions remain provenance claims, never local authorization.
pub(super) fn preserved_imported_reason(
    reason: Option<String>,
    candidate: &Candidate,
) -> Option<String> {
    let reason = reason?;
    let receipt: CrossFamilyRecurationReceipt =
        serde_json::from_str(reason.strip_prefix(PREFIX)?).ok()?;
    (receipt.plan.merged_intent == candidate.intent
        && receipt.proof == "typed-string-equality-parameterization-v1")
        .then_some(reason)
}

/// Exact immutable source revisions and the complete merged searchable intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossFamilyRecurationPlan {
    pub source_example: Uuid,
    pub survivor_example: Uuid,
    pub source_revision: String,
    pub survivor_revision: String,
    pub merged_intent: super::super::index::NormalizedIntent,
}

/// Human-facing review material; code and purpose are not model-tool payloads.
#[derive(Debug, Clone, Serialize)]
pub struct CrossFamilyExampleView {
    pub id: Uuid,
    pub intent: super::super::index::NormalizedIntent,
    pub source: String,
    pub catalogues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrossFamilyRecurationPreview {
    pub plan: CrossFamilyRecurationPlan,
    pub source: CrossFamilyExampleView,
    pub survivor: CrossFamilyExampleView,
}

/// Kept in the superseded revisions and therefore in private archives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossFamilyRecurationReceipt {
    pub decision_id: Uuid,
    pub source_scope: LibraryScope,
    pub plan: CrossFamilyRecurationPlan,
    pub published_example: Uuid,
    pub actor: String,
    pub credential: String,
    pub proof: String,
}

fn plan(source: &StoredExample, survivor: &StoredExample) -> Result<CrossFamilyRecurationPlan> {
    let a = &source.candidate;
    let b = &survivor.candidate;
    if source.id == survivor.id
        || a.family == b.family
        || a.variant != b.variant
        || a.coverage.iter().collect::<BTreeSet<_>>() != b.coverage.iter().collect::<BTreeSet<_>>()
        || !a.eligible(OutcomeAssurance::PostconditionVerified)
        || !b.eligible(OutcomeAssurance::PostconditionVerified)
    {
        return Err(CrossFamilyRecurationError::NotEquivalent);
    }
    let (Some(a_program), Some(b_program)) = (a.checked_program(), b.checked_program()) else {
        return Err(CrossFamilyRecurationError::NotEquivalent);
    };
    if !a
        .quality
        .equivalent_parameterized_observation(&b.quality, a_program, b_program)
    {
        return Err(CrossFamilyRecurationError::NotEquivalent);
    }
    let mut merged_intent = b.intent.clone();
    let mut known: BTreeSet<_> = std::iter::once(&merged_intent.purpose)
        .chain(&merged_intent.aliases)
        .map(|value| super::super::index::normalize(value))
        .collect();
    for text in std::iter::once(&a.intent.purpose).chain(&a.intent.aliases) {
        if text.trim().is_empty() || text.chars().count() > super::super::normalization::ALIAS_LIMIT
        {
            return Err(CrossFamilyRecurationError::IntentTooLarge);
        }
        if known.insert(super::super::index::normalize(text)) {
            merged_intent.aliases.push(text.clone());
        }
    }
    if merged_intent.aliases.len() > super::super::normalization::ALIAS_COUNT {
        return Err(CrossFamilyRecurationError::IntentTooLarge);
    }
    let mut facets: BTreeSet<_> = merged_intent
        .facets
        .iter()
        .map(|value| super::super::index::normalize(value))
        .collect();
    for text in &a.intent.facets {
        if text.trim().is_empty() || text.chars().count() > super::super::normalization::FACET_LIMIT
        {
            return Err(CrossFamilyRecurationError::IntentTooLarge);
        }
        if facets.insert(super::super::index::normalize(text)) {
            merged_intent.facets.push(text.clone());
        }
    }
    if merged_intent.facets.len() > super::super::normalization::FACET_COUNT {
        return Err(CrossFamilyRecurationError::IntentTooLarge);
    }
    Ok(CrossFamilyRecurationPlan {
        source_example: source.id,
        survivor_example: survivor.id,
        source_revision: super::frozen::proposal_hash(a)?,
        survivor_revision: super::frozen::proposal_hash(b)?,
        merged_intent,
    })
}

impl WorkflowRepository {
    /// Show both complete examples and their exact revision-bound decision.
    pub async fn describe_cross_family_recuration(
        &self,
        actor: &training::TrustedTrainingActor,
        source: Uuid,
        survivor: Uuid,
    ) -> Result<CrossFamilyRecurationPreview> {
        let plan = self
            .preview_cross_family_recuration(actor, source, survivor)
            .await?;
        let a = self.retrieve(actor.scope(), source).await?;
        let b = self.retrieve(actor.scope(), survivor).await?;
        if super::frozen::proposal_hash(&a.candidate)? != plan.source_revision
            || super::frozen::proposal_hash(&b.candidate)? != plan.survivor_revision
        {
            return Err(CrossFamilyRecurationError::StaleRevision);
        }
        let view = |e: StoredExample| -> Result<CrossFamilyExampleView> {
            let source = e
                .candidate
                .program()
                .ok_or(CrossFamilyRecurationError::NotEquivalent)?
                .source
                .clone();
            Ok(CrossFamilyExampleView {
                id: e.id,
                intent: e.candidate.intent,
                source,
                catalogues: e.candidate.catalog_fingerprints.into_keys().collect(),
            })
        };
        Ok(CrossFamilyRecurationPreview {
            plan,
            source: view(a)?,
            survivor: view(b)?,
        })
    }

    /// Prepare a reviewable proposal without mutating the library.
    pub async fn preview_cross_family_recuration(
        &self,
        actor: &training::TrustedTrainingActor,
        source: Uuid,
        survivor: Uuid,
    ) -> Result<CrossFamilyRecurationPlan> {
        if !actor.is_human_owner() {
            return Err(CrossFamilyRecurationError::Denied);
        }
        let a = self
            .retrieve(actor.scope(), source)
            .await
            .map_err(|error| match error {
                RepositoryError::Unavailable => CrossFamilyRecurationError::StaleRevision,
                other => CrossFamilyRecurationError::Repository(other),
            })?;
        let b = self
            .retrieve(actor.scope(), survivor)
            .await
            .map_err(|error| match error {
                RepositoryError::Unavailable => CrossFamilyRecurationError::StaleRevision,
                other => CrossFamilyRecurationError::Repository(other),
            })?;
        plan(&a, &b)
    }

    /// Commit the exact human-reviewed plan; serialized tenant publication and
    /// revision hashes prevent concurrent curation or promotion from racing it.
    pub async fn recurate_cross_family(
        &self,
        actor: &training::TrustedTrainingActor,
        expected: &CrossFamilyRecurationPlan,
        decision_id: Uuid,
    ) -> Result<CrossFamilyRecurationReceipt> {
        if !actor.is_human_owner() {
            return Err(CrossFamilyRecurationError::Denied);
        }
        if expected.source_example == expected.survivor_example {
            return Err(CrossFamilyRecurationError::NotEquivalent);
        }
        let scope = actor.scope();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO workflow_learning_tenants(tenant) VALUES($1) ON CONFLICT DO NOTHING",
        )
        .bind(&scope.tenant)
        .execute(&mut tx.executor())
        .await?;
        sqlx::query("SELECT tenant FROM workflow_learning_tenants WHERE tenant=$1 FOR UPDATE")
            .bind(&scope.tenant)
            .fetch_one(&mut tx.executor())
            .await?;
        let previous: Vec<String> = sqlx::query_scalar("SELECT reason FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND reason LIKE $3 AND position($4 in reason)>0")
            .bind(&scope.tenant).bind(&scope.project).bind(format!("{PREFIX}%")).bind(decision_id.to_string())
            .fetch_all(&mut tx.executor()).await?;
        for reason in previous {
            let Some(json) = reason.strip_prefix(PREFIX) else {
                continue;
            };
            let prior: CrossFamilyRecurationReceipt = serde_json::from_str(json)?;
            if prior.source_scope != *scope || prior.decision_id != decision_id {
                continue;
            }
            if prior.plan == *expected
                && prior.actor == actor.subject()
                && prior.credential == actor.credential()
            {
                tx.commit().await?;
                return Ok(prior);
            }
            return Err(CrossFamilyRecurationError::StaleRevision);
        }
        let rows = sqlx::query("SELECT id,state,reason,candidate FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND id=ANY($3) ORDER BY id FOR UPDATE")
            .bind(&scope.tenant).bind(&scope.project).bind(vec![expected.source_example, expected.survivor_example])
            .fetch_all(&mut tx.executor()).await?;
        if rows.len() != 2 {
            return Err(CrossFamilyRecurationError::StaleRevision);
        }
        let source_row = rows
            .iter()
            .find(|row| row.try_get::<Uuid, _>("id").ok() == Some(expected.source_example))
            .ok_or(CrossFamilyRecurationError::StaleRevision)?;
        let source_state: String = source_row.try_get("state")?;
        if source_state == "superseded" {
            let reason: Option<String> = source_row.try_get("reason")?;
            let prior = reason
                .as_deref()
                .and_then(|reason| reason.strip_prefix(PREFIX))
                .and_then(|json| serde_json::from_str::<CrossFamilyRecurationReceipt>(json).ok());
            if let Some(prior) = prior {
                if prior.source_scope == *scope
                    && prior.decision_id == decision_id
                    && prior.plan == *expected
                    && prior.actor == actor.subject()
                    && prior.credential == actor.credential()
                {
                    tx.commit().await?;
                    return Ok(prior);
                }
            }
            return Err(CrossFamilyRecurationError::StaleRevision);
        }
        if rows
            .iter()
            .any(|row| row.try_get::<String, _>("state").ok().as_deref() != Some("active"))
        {
            return Err(CrossFamilyRecurationError::StaleRevision);
        }
        let mut examples = Vec::new();
        for row in &rows {
            examples.push(StoredExample {
                id: row.try_get("id")?,
                candidate: serde_json::from_value(row.try_get("candidate")?)?,
            });
        }
        let a = examples
            .iter()
            .find(|e| e.id == expected.source_example)
            .ok_or(CrossFamilyRecurationError::StaleRevision)?;
        let b = examples
            .iter()
            .find(|e| e.id == expected.survivor_example)
            .ok_or(CrossFamilyRecurationError::StaleRevision)?;
        if plan(a, b)? != *expected {
            return Err(CrossFamilyRecurationError::StaleRevision);
        }
        for id in [a.id, b.id] {
            let supported: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_support WHERE example_id=$1) OR EXISTS(SELECT 1 FROM workflow_learning_archive_example_support WHERE example_id=$1 AND tenant=$2 AND project=$3)")
                .bind(id).bind(&scope.tenant).bind(&scope.project).fetch_one(&mut tx.executor()).await?;
            if !supported {
                return Err(CrossFamilyRecurationError::MissingEvidence);
            }
        }
        let mut candidate = b.candidate.clone();
        candidate.intent = expected.merged_intent.clone();
        let published_example = Uuid::new_v4();
        let receipt = CrossFamilyRecurationReceipt {
            decision_id,
            source_scope: scope.clone(),
            plan: expected.clone(),
            published_example,
            actor: actor.subject().into(),
            credential: actor.credential().into(),
            proof: "typed-string-equality-parameterization-v1".into(),
        };
        let reason = format!("{PREFIX}{}", serde_json::to_string(&receipt)?);
        let superseded = sqlx::query("UPDATE workflow_learning_examples SET state='superseded',reason=$1 WHERE tenant=$2 AND project=$3 AND id=ANY($4) AND state='active'")
            .bind(&reason).bind(&scope.tenant).bind(&scope.project).bind(vec![a.id,b.id]).execute(&mut tx.executor()).await?.rows_affected();
        if superseded != 2 {
            return Err(CrossFamilyRecurationError::StaleRevision);
        }
        sqlx::query("INSERT INTO workflow_learning_examples(id,tenant,project,family,variant,identity,candidate,state,reason) VALUES($1,$2,$3,$4,$5,$6,$7,'active',$8)")
            .bind(published_example).bind(&scope.tenant).bind(&scope.project).bind(&candidate.family).bind(&candidate.variant).bind(&candidate.identity)
            .bind(serde_json::to_value(&candidate)?).bind(&reason).execute(&mut tx.executor()).await?;
        sqlx::query("INSERT INTO workflow_learning_support(example_id,review_id) SELECT $1,review_id FROM workflow_learning_support WHERE example_id=$2 ON CONFLICT DO NOTHING")
            .bind(published_example).bind(b.id).execute(&mut tx.executor()).await?;
        sqlx::query("INSERT INTO workflow_learning_archive_example_support(example_id,tenant,project,archive_digest,item_digest) SELECT $1,tenant,project,archive_digest,item_digest FROM workflow_learning_archive_example_support WHERE example_id=$2 AND tenant=$3 AND project=$4 ON CONFLICT DO NOTHING")
            .bind(published_example).bind(b.id).bind(&scope.tenant).bind(&scope.project).execute(&mut tx.executor()).await?;
        for family in [&a.candidate.family, &b.candidate.family] {
            let updated = sqlx::query("UPDATE workflow_learning_families SET version=version+1 WHERE tenant=$1 AND project=$2 AND family=$3 AND variant=$4")
                .bind(&scope.tenant).bind(&scope.project).bind(family).bind(&candidate.variant).execute(&mut tx.executor()).await?.rows_affected();
            if updated != 1 {
                return Err(CrossFamilyRecurationError::StaleRevision);
            }
        }
        tx.commit().await?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only against an explicitly named disposable copy of a corpus.
    #[tokio::test]
    #[ignore = "requires disposable retained-corpus copy"]
    async fn retained_corpus_recuration_preserves_recall_and_evidence(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let url = std::env::var("PLASM_RECURATION_CLONE_URL")?;
        let parsed = url::Url::parse(&url)?;
        assert!(
            parsed.path().starts_with("/plasm_recuration_verify_"),
            "test must never mutate the original corpus"
        );
        let scope = LibraryScope {
            tenant: std::env::var("PLASM_RETAINED_CORPUS_TENANT")?,
            project: std::env::var("PLASM_RETAINED_CORPUS_PROJECT")?,
        };
        let repo = WorkflowRepository::connect(&url).await?;
        let actor = training::TrustedTrainingActor::human_owner(
            scope.clone(),
            "corpus-audit-owner".into(),
            "explicit-test-decision".into(),
        )?;
        let caller = training::TrustedTrainingActor::authenticated(
            scope.clone(),
            "untrusted-caller".into(),
            "caller".into(),
        )?;
        let examples = repo.list_eligible(&scope, 1000).await?;
        let mut pair = None;
        for a in &examples {
            for b in repo
                .comparable_coverage(
                    &scope,
                    &a.candidate.catalog_fingerprints,
                    &a.candidate.coverage,
                    &a.candidate.intent,
                    8,
                )
                .await?
            {
                if a.id != b.id && plan(a, &b).is_ok() {
                    pair = Some((a.clone(), b));
                    break;
                }
            }
            if pair.is_some() {
                break;
            }
        }
        let (source, survivor) =
            pair.expect("retained corpus must contain a validated cross-family pair");
        async fn support_count(repo: &WorkflowRepository, id: Uuid) -> Result<i64> {
            Ok(sqlx::query_scalar("SELECT (SELECT count(*) FROM workflow_learning_support WHERE example_id=$1) + (SELECT count(*) FROM workflow_learning_archive_example_support WHERE example_id=$1)")
                .bind(id).fetch_one(&repo.pool).await?)
        }
        let source_support = support_count(&repo, source.id).await?;
        let survivor_support = support_count(&repo, survivor.id).await?;
        assert!(source_support > 0 && survivor_support > 0);
        assert!(matches!(
            repo.preview_cross_family_recuration(&caller, source.id, survivor.id)
                .await,
            Err(CrossFamilyRecurationError::Denied)
        ));
        let wrong_scope = training::TrustedTrainingActor::human_owner(
            LibraryScope {
                tenant: "other-tenant".into(),
                project: scope.project.clone(),
            },
            "other".into(),
            "other".into(),
        )?;
        assert!(matches!(
            repo.preview_cross_family_recuration(&wrong_scope, source.id, survivor.id)
                .await,
            Err(CrossFamilyRecurationError::StaleRevision)
        ));
        let preview = repo
            .describe_cross_family_recuration(&actor, source.id, survivor.id)
            .await?;
        assert_eq!(
            preview.source.source,
            source.candidate.program().expect("source program").source
        );
        assert_eq!(
            preview.survivor.source,
            survivor
                .candidate
                .program()
                .expect("survivor program")
                .source
        );
        let proposed = preview.plan;
        let mut changed = proposed.clone();
        changed.source_revision.push('0');
        assert!(matches!(
            repo.recurate_cross_family(&actor, &changed, Uuid::new_v4())
                .await,
            Err(CrossFamilyRecurationError::StaleRevision)
        ));
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND state='active'").bind(&scope.tenant).bind(&scope.project).fetch_one(&repo.pool).await?;
        let before_guidance: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2").bind(&scope.tenant).bind(&scope.project).fetch_one(&repo.pool).await?;
        let decision = Uuid::new_v4();
        let (first, concurrent) = tokio::join!(
            repo.recurate_cross_family(&actor, &proposed, decision),
            repo.recurate_cross_family(&actor, &proposed, decision)
        );
        let receipt = first?;
        assert_eq!(concurrent?, receipt, "concurrent retries must commit once");
        assert_eq!(
            repo.recurate_cross_family(&actor, &proposed, decision)
                .await?,
            receipt
        );
        let mut reused_decision = proposed.clone();
        reused_decision.source_example = Uuid::new_v4();
        assert!(matches!(
            repo.recurate_cross_family(&actor, &reused_decision, decision)
                .await,
            Err(CrossFamilyRecurationError::StaleRevision)
        ));
        assert!(matches!(
            repo.recurate_cross_family(&actor, &proposed, Uuid::new_v4())
                .await,
            Err(CrossFamilyRecurationError::StaleRevision)
        ));
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND state='active'").bind(&scope.tenant).bind(&scope.project).fetch_one(&repo.pool).await?;
        assert_eq!(after, before - 1);
        let after_guidance: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2").bind(&scope.tenant).bind(&scope.project).fetch_one(&repo.pool).await?;
        assert_eq!(after_guidance, before_guidance);
        let active = repo.retrieve(&scope, receipt.published_example).await?;
        assert_eq!(active.candidate.program(), survivor.candidate.program());
        assert!(active
            .candidate
            .intent
            .aliases
            .contains(&source.candidate.intent.purpose));
        assert!(source.candidate.intent.facets.iter().all(|facet| active
            .candidate
            .intent
            .aliases
            .contains(facet)
            || active.candidate.intent.facets.contains(facet)));
        let state_count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND id=ANY($3) AND state='superseded'").bind(&scope.tenant).bind(&scope.project).bind(vec![source.id,survivor.id]).fetch_one(&repo.pool).await?;
        assert_eq!(state_count, 2);
        assert_eq!(support_count(&repo, source.id).await?, source_support);
        assert_eq!(support_count(&repo, survivor.id).await?, survivor_support);
        assert_eq!(
            support_count(&repo, receipt.published_example).await?,
            survivor_support
        );
        for old in [&source.candidate, &survivor.candidate] {
            let catalogs = old.catalog_fingerprints.keys().cloned().collect();
            let recalled = repo
                .search(
                    &scope,
                    super::ExampleSearch {
                        intent: &old.intent,
                        embedding: &old.embedding,
                        profile: &old.embedding_profile,
                        catalogs: &catalogs,
                        limit: 32,
                        active_only: true,
                    },
                )
                .await?;
            assert!(
                recalled
                    .iter()
                    .any(|item| item.id == receipt.published_example),
                "both prior intents must recall the new revision"
            );
        }
        let output_root = std::env::var_os("PLASM_RECURATION_OUTPUT_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = output_root.join(format!("plasm-recuration-serving-{}", Uuid::new_v4()));
        let archive = repo
            .export_filesystem_archive(
                &scope,
                &path,
                super::super::super::archive::ArchiveKind::ServingLibrary,
            )
            .await?;
        let reader = super::super::super::filesystem_archive::Reader::open(&path)?;
        let identity = format!("example:{}", receipt.published_example);
        let record = reader.read_record(&identity)?;
        let super::super::super::filesystem_archive::Record::Example {
            candidate,
            reason: Some(reason),
            ..
        } = record
        else {
            panic!("published example record and decision")
        };
        assert!(candidate
            .intent
            .aliases
            .contains(&source.candidate.intent.purpose));
        let serving_receipt: CrossFamilyRecurationReceipt =
            serde_json::from_str(reason.strip_prefix(PREFIX).expect("typed decision prefix"))?;
        assert_eq!(serving_receipt, receipt);
        assert!(preserved_imported_reason(Some(reason.clone()), &candidate).is_some());
        let mut altered = candidate.as_ref().clone();
        altered.intent.purpose.push_str(" changed");
        assert!(preserved_imported_reason(Some(reason), &altered).is_none());
        let private_path = output_root.join(format!("plasm-recuration-private-{}", Uuid::new_v4()));
        repo.export_filesystem_archive(
            &scope,
            &private_path,
            super::super::super::archive::ArchiveKind::PrivateCollection,
        )
        .await?;
        let private = super::super::super::filesystem_archive::Reader::open(&private_path)?;
        let record = private.read_record(&format!("example:{}", source.id))?;
        let super::super::super::filesystem_archive::Record::Example {
            reason: Some(reason),
            ..
        } = record
        else {
            panic!("original revision and decision")
        };
        let archived_receipt: CrossFamilyRecurationReceipt =
            serde_json::from_str(reason.strip_prefix(PREFIX).expect("typed decision prefix"))?;
        assert_eq!(archived_receipt, receipt);
        println!("recurated one pair: {before} -> {after} active, both intents recalled; serving archive {} at {}; private archive at {}", archive.digest, path.display(), private_path.display());
        Ok(())
    }
}
