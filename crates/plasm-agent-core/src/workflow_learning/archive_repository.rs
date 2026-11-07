//! Scoped archive retention and publication through the ordinary bounded portfolio.
use super::*;
use crate::discovery_store::DiscoveryAuthorization;
use crate::workflow_learning::archive::*;
use crate::workflow_learning::catalogue_guidance::CatalogueAssociation;
use crate::workflow_learning::filesystem_archive::Record;
use crate::{execute_session::ExecuteSession, server_state::PlasmHostState};

pub(super) enum PublicationSupport {
    LocalReview(Uuid),
    Archive {
        scope: LibraryScope,
        archive: ArchiveDigest,
        item: ArchiveDigest,
    },
}
impl PublicationSupport {
    pub(super) async fn recuration_reason(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        candidate: &Candidate,
    ) -> Result<Option<String>> {
        let Self::Archive {
            scope,
            archive,
            item,
        } = self
        else {
            return Ok(None);
        };
        let record: serde_json::Value = sqlx::query_scalar("SELECT record FROM workflow_learning_archive_items WHERE tenant=$1 AND project=$2 AND archive_digest=$3 AND item_digest=$4")
            .bind(&scope.tenant).bind(&scope.project).bind(archive.as_str()).bind(item.as_str())
            .fetch_one(&mut tx.executor()).await?;
        let record: Record = serde_json::from_value(record)?;
        let Record::Example { reason, .. } = record else {
            return Ok(None);
        };
        Ok(super::recuration::preserved_imported_reason(
            reason, candidate,
        ))
    }
    pub(super) async fn has_example(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> Result<bool> {
        match self {
            Self::LocalReview(review)=>Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_support WHERE example_id=$1 AND review_id=$2)").bind(id).bind(review).fetch_one(&mut tx.executor()).await?),
            Self::Archive{scope,archive,item}=>Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_archive_example_support WHERE example_id=$1 AND tenant=$2 AND project=$3 AND archive_digest=$4 AND item_digest=$5)").bind(id).bind(&scope.tenant).bind(&scope.project).bind(archive.as_str()).bind(item.as_str()).fetch_one(&mut tx.executor()).await?),
        }
    }
    pub(super) async fn attach_example(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> Result<()> {
        match self {
            Self::LocalReview(review) => {
                sqlx::query("INSERT INTO workflow_learning_support(example_id,review_id) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(id).bind(review).execute(&mut tx.executor()).await?;
            }
            Self::Archive {
                scope,
                archive,
                item,
            } => {
                sqlx::query("INSERT INTO workflow_learning_archive_example_support(example_id,tenant,project,archive_digest,item_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(id).bind(&scope.tenant).bind(&scope.project).bind(archive.as_str()).bind(item.as_str()).execute(&mut tx.executor()).await?;
            }
        }
        Ok(())
    }
    pub(super) async fn attach_guidance(
        &self,
        tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> Result<()> {
        match self {
            Self::LocalReview(review) => {
                sqlx::query("INSERT INTO workflow_learning_guidance_support(association_id,review_id) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(id).bind(review).execute(&mut tx.executor()).await?;
            }
            Self::Archive {
                scope,
                archive,
                item,
            } => {
                sqlx::query("INSERT INTO workflow_learning_archive_guidance_support(association_id,tenant,project,archive_digest,item_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(id).bind(&scope.tenant).bind(&scope.project).bind(archive.as_str()).bind(item.as_str()).execute(&mut tx.executor()).await?;
            }
        }
        Ok(())
    }
}
impl WorkflowRepository {
    /// Serving archives contain selected active products and claim-free support
    /// projections, never sibling observations from their originating reviews.
    async fn export_selected_filesystem_archive(
        &self,
        scope: &LibraryScope,
        destination: &std::path::Path,
    ) -> std::result::Result<
        crate::workflow_learning::filesystem_archive::Manifest,
        crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
    > {
        use crate::workflow_learning::filesystem_archive::{
            FilesystemArchiveError as FsError, StoredExampleState, Writer,
        };
        let mut writer = Writer::new(destination, scope.clone(), ArchiveKind::ServingLibrary)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut tx.executor())
            .await?;
        let mut groups = BTreeMap::new();
        let mut covered_examples = BTreeSet::new();
        let mut covered_guidance = BTreeSet::new();
        let mut offset = 0_i64;
        loop {
            let rows = sqlx::query("SELECT r.id,r.digest,r.seal,r.seal_content_digest FROM workflow_learning_reviews r WHERE r.tenant=$1 AND r.project=$2 AND (EXISTS(SELECT 1 FROM workflow_learning_support s JOIN workflow_learning_examples e ON e.id=s.example_id WHERE s.review_id=r.id AND e.tenant=$1 AND e.project=$2 AND e.state='active') OR EXISTS(SELECT 1 FROM workflow_learning_guidance_support s JOIN workflow_learning_catalogue_guidance g ON g.id=s.association_id WHERE s.review_id=r.id AND g.tenant=$1 AND g.project=$2)) ORDER BY r.id LIMIT 128 OFFSET $3")
                .bind(&scope.tenant).bind(&scope.project).bind(offset).fetch_all(&mut tx.executor()).await?;
            let count = rows.len();
            for row in rows {
                let review_id: Uuid = row.try_get("id")?;
                let original_digest = ArchiveDigest::try_from(row.try_get::<String, _>("digest")?)
                    .map_err(|_| FsError::Invalid)?;
                let seal_value: serde_json::Value = row.try_get("seal")?;
                let content_digest: String = row.try_get("seal_content_digest")?;
                let seal =
                    super::hydrate_review(&mut tx.executor(), seal_value, &content_digest).await?;
                let example_rows = sqlx::query("SELECT e.id,e.candidate,e.reason FROM workflow_learning_examples e JOIN workflow_learning_support s ON s.example_id=e.id WHERE e.tenant=$1 AND e.project=$2 AND e.state='active' AND s.review_id=$3 ORDER BY e.id")
                    .bind(&scope.tenant).bind(&scope.project).bind(review_id).fetch_all(&mut tx.executor()).await?;
                let mut examples = Vec::new();
                let mut example_records = Vec::new();
                for example in example_rows {
                    let id: Uuid = example.try_get("id")?;
                    let candidate: Candidate =
                        serde_json::from_value(example.try_get("candidate")?)?;
                    examples.push(candidate.clone());
                    example_records.push((
                        id,
                        candidate,
                        example.try_get::<Option<String>, _>("reason")?,
                    ));
                }
                let guidance_rows = sqlx::query("SELECT g.id,g.association FROM workflow_learning_catalogue_guidance g JOIN workflow_learning_guidance_support s ON s.association_id=g.id WHERE g.tenant=$1 AND g.project=$2 AND s.review_id=$3 ORDER BY g.id")
                    .bind(&scope.tenant).bind(&scope.project).bind(review_id).fetch_all(&mut tx.executor()).await?;
                let mut associations = Vec::new();
                let mut guidance_records = Vec::new();
                for guidance in guidance_rows {
                    let id: Uuid = guidance.try_get("id")?;
                    let association: CatalogueAssociation =
                        serde_json::from_value(guidance.try_get("association")?)?;
                    associations.push(association.clone());
                    guidance_records.push((id, association));
                }
                let reference = EvidenceReference {
                    source_scope: scope.clone(),
                    review_id,
                    seal_digest: original_digest,
                };
                let group = insert_review_group(&mut groups, reference, seal)?;
                for (id, candidate, reason) in example_records {
                    group
                        .examples
                        .insert(id, (candidate, reason, StoredExampleState::Active));
                    covered_examples.insert(id);
                }
                for (id, association) in guidance_records {
                    group.guidance.insert(id, association);
                    covered_guidance.insert(id);
                }
            }
            if count < 128 {
                break;
            }
            offset += 128;
        }
        collect_imported_groups(
            &mut tx,
            scope,
            true,
            &mut groups,
            &mut covered_examples,
            &mut covered_guidance,
        )
        .await?;
        write_review_groups(&mut writer, groups, &BTreeSet::new())?;
        let active_examples: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 AND state='active'").bind(&scope.tenant).bind(&scope.project).fetch_one(&mut tx.executor()).await?;
        let active_guidance: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2").bind(&scope.tenant).bind(&scope.project).fetch_one(&mut tx.executor()).await?;
        if active_examples != covered_examples.len() as i64
            || active_guidance != covered_guidance.len() as i64
        {
            return Err(FsError::Invalid);
        }
        tx.commit().await?;
        writer.finish()
    }
    /// Record an explicit trusted attestation for one migrated source review.
    /// Archive adoption and the archive's own asserted hashes cannot create it.
    pub async fn authorize_imported_review(
        &self,
        actor: &crate::workflow_learning::training::TrustedTrainingActor,
        archive: &ArchiveDigest,
        evidence: &EvidenceReference,
        decision_id: Uuid,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        if expires_at <= chrono::Utc::now() {
            return Err(RepositoryError::ApprovalUnavailable);
        }
        let mut tx = self.pool.begin().await?;
        let permit = training::authorize_import_attestation(&mut tx, actor).await?;
        let changed = sqlx::query("INSERT INTO workflow_training_archive_review_decisions(tenant,project,archive_digest,source_tenant,source_project,review_id,seal_digest,actor,credential,expires_at,decision_id,authority) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT(tenant,project,archive_digest,source_tenant,source_project,review_id,seal_digest) DO UPDATE SET decision_id=workflow_training_archive_review_decisions.decision_id WHERE workflow_training_archive_review_decisions.actor=EXCLUDED.actor AND workflow_training_archive_review_decisions.credential=EXCLUDED.credential AND workflow_training_archive_review_decisions.expires_at=EXCLUDED.expires_at AND workflow_training_archive_review_decisions.decision_id=EXCLUDED.decision_id AND workflow_training_archive_review_decisions.authority=EXCLUDED.authority")
            .bind(&actor.scope().tenant).bind(&actor.scope().project).bind(archive.as_str())
            .bind(&evidence.source_scope.tenant).bind(&evidence.source_scope.project).bind(evidence.review_id).bind(evidence.seal_digest.as_str())
            .bind(actor.subject()).bind(actor.credential()).bind(expires_at).bind(decision_id).bind(serde_json::to_value(permit)?)
            .execute(&mut tx.executor()).await?.rows_affected();
        if changed != 1 {
            return Err(RepositoryError::ImmutableConflict);
        }
        tx.commit().await?;
        Ok(())
    }
    /// Validate the complete directory before adopting any item. Publication is
    /// limited to explicitly authorized, compiler-checked portable examples.
    #[allow(clippy::too_many_arguments)]
    pub async fn import_filesystem_archive(
        &self,
        target: &LibraryScope,
        source: &std::path::Path,
        policy: &crate::workflow_learning::filesystem_archive::ImportPolicy,
        host: &PlasmHostState,
        session: &ExecuteSession,
        allowed: &DiscoveryAuthorization,
    ) -> std::result::Result<
        ArchiveImportReport,
        crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
    > {
        use crate::workflow_learning::filesystem_archive::{
            FilesystemArchiveError as FsError, Reader, Record, StoredExampleState,
        };
        let reader = Reader::open(source)?;
        let manifest = reader.manifest();
        if manifest.digest != policy.expected_digest.as_str()
            || manifest.source_scope != policy.source_scope
        {
            return Err(FsError::Integrity);
        }
        let index = FilesystemArchiveIndex::build(&reader)?;
        let archive_digest =
            ArchiveDigest::try_from(manifest.digest.clone()).map_err(|_| FsError::Invalid)?;
        let isolated = crate::workflow_learning::teaching::validation_session(session);
        let session = &isolated;
        let mut tx = self.pool.begin().await?;
        let encoded_manifest = serde_json::to_value(manifest)?;
        let existing: serde_json::Value = sqlx::query_scalar("INSERT INTO workflow_learning_archives(tenant,project,digest,document) VALUES($1,$2,$3,$4) ON CONFLICT(tenant,project,digest) DO UPDATE SET document=workflow_learning_archives.document RETURNING document")
            .bind(&target.tenant).bind(&target.project).bind(&manifest.digest).bind(&encoded_manifest).fetch_one(&mut tx.executor()).await?;
        if existing != encoded_manifest {
            return Err(FsError::Integrity);
        }
        let mut report = Vec::new();
        for (identity, reference) in &manifest.records {
            let record = reader.read_record(identity)?;
            let product = match &record {
                Record::Example {
                    revision_id,
                    candidate,
                    state: StoredExampleState::Active,
                    ..
                } => {
                    let mut matched = None;
                    for evidence in index.example_support.get(revision_id).into_iter().flatten() {
                        let seal = index.read_seal(&reader, evidence)?;
                        if !imported_decision_valid(&mut tx, target, &archive_digest, evidence)
                            .await?
                        {
                            continue;
                        }
                        if candidate.verified_by(&seal) {
                            matched = Some(seal);
                            break;
                        }
                    }
                    match matched {
                        None => PreparedAdoption::Hold(ImportDisposition::AwaitingCompatibility {
                            reason: ArchiveCompatibilityIssue::SupportingEvidenceUnavailable,
                        }),
                        Some(seal) => {
                            let program = candidate.program().ok_or(FsError::Integrity)?;
                            if program
                                .catalog_fingerprints
                                .keys()
                                .any(|entry| !allowed.catalogs.contains(entry))
                                || program
                                    .required_capabilities
                                    .iter()
                                    .any(|cap| !allowed.permits(cap))
                            {
                                PreparedAdoption::Hold(ImportDisposition::AwaitingCompatibility {
                                    reason: ArchiveCompatibilityIssue::AuthorizationUnavailable,
                                })
                            } else {
                                match stage_archive_example(session, program, allowed) {
                                    Err(error) => PreparedAdoption::Hold(
                                        ImportDisposition::AwaitingCompatibility {
                                            reason: ArchiveCompatibilityIssue::SessionPreparation {
                                                diagnostic: error.to_string(),
                                            },
                                        },
                                    ),
                                    Ok(staged) => match program.rebind_teaching(&staged) {
                                        Err(error) => {
                                            PreparedAdoption::Hold(ImportDisposition::Quarantined {
                                                reason: ArchiveCompatibilityIssue::Teaching {
                                                    diagnostic: error.to_string(),
                                                },
                                            })
                                        }
                                        Ok(rebound) => match crate::plasm_compile::compile_program(
                                            host.engine.prompt_pipeline(),
                                            None,
                                            &staged,
                                            "archive_compatibility",
                                            &rebound.source,
                                        )
                                        .await
                                        {
                                            Ok(_) => PreparedAdoption::Example {
                                                candidate: candidate.clone(),
                                                seal: Box::new(seal),
                                            },
                                            Err(error) => PreparedAdoption::Hold(
                                                compilation_disposition(error),
                                            ),
                                        },
                                    },
                                }
                            }
                        }
                    }
                }
                Record::CatalogueGuidance {
                    association_id,
                    association,
                } => {
                    let mut matched = None;
                    for evidence in index
                        .guidance_support
                        .get(association_id)
                        .into_iter()
                        .flatten()
                    {
                        let seal = index.read_seal(&reader, evidence)?;
                        if !imported_decision_valid(&mut tx, target, &archive_digest, evidence)
                            .await?
                        {
                            continue;
                        }
                        if association.validate(&seal).is_ok() {
                            matched = Some(seal);
                            break;
                        }
                    }
                    match matched {
                        None => PreparedAdoption::Hold(ImportDisposition::AwaitingCompatibility {
                            reason: ArchiveCompatibilityIssue::SupportingEvidenceUnavailable,
                        }),
                        Some(seal) => {
                            let requirements = &association.requirements;
                            let eligible = requirements
                                .required_capabilities()
                                .iter()
                                .all(|cap| allowed.permits(cap))
                                && requirements.catalog_fingerprints().iter().all(
                                    |(entry, hash)| {
                                        allowed.catalogs.contains(entry)
                                            && session.contexts_by_entry.get(entry).is_some_and(
                                                |ctx| ctx.cgs.catalog_cgs_hash_hex() == *hash,
                                            )
                                    },
                                );
                            let mut compiles = false;
                            if eligible {
                                for receipt in seal.executions.iter().filter(|receipt| {
                                    receipt.catalogue_requirements.available() == Some(requirements)
                                }) {
                                    let Ok(checked) = crate::workflow_learning::teaching::CheckedTeachingExample::from_observation(&receipt.observation) else { continue };
                                    let Ok(staged) =
                                        stage_archive_example(session, checked.program(), allowed)
                                    else {
                                        continue;
                                    };
                                    let Ok(rebound) = checked.program().rebind_teaching(&staged)
                                    else {
                                        continue;
                                    };
                                    if crate::plasm_compile::compile_program(
                                        host.engine.prompt_pipeline(),
                                        None,
                                        &staged,
                                        "archive_guidance_compatibility",
                                        &rebound.source,
                                    )
                                    .await
                                    .is_ok()
                                    {
                                        compiles = true;
                                        break;
                                    }
                                }
                            }
                            if compiles {
                                PreparedAdoption::Guidance {
                                    association: Box::new(association.clone()),
                                    seal: Box::new(seal),
                                }
                            } else {
                                PreparedAdoption::Hold(ImportDisposition::AwaitingCompatibility {
                                    reason: ArchiveCompatibilityIssue::CatalogueContractsChanged,
                                })
                            }
                        }
                    }
                }
                _ => PreparedAdoption::Hold(ImportDisposition::RetainedObservation),
            };
            let item_digest =
                ArchiveDigest::try_from(reference.digest.clone()).map_err(|_| FsError::Invalid)?;
            let old: Option<serde_json::Value> = sqlx::query_scalar("SELECT disposition FROM workflow_learning_archive_items WHERE tenant=$1 AND project=$2 AND archive_digest=$3 AND item_digest=$4")
                .bind(&target.tenant).bind(&target.project).bind(&manifest.digest).bind(item_digest.as_str()).fetch_optional(&mut tx.executor()).await?;
            if old.as_ref().is_some_and(|value| {
                serde_json::from_value::<ImportDisposition>(value.clone()).is_ok_and(|v| {
                    matches!(
                        v,
                        ImportDisposition::EligibleCandidate
                            | ImportDisposition::RetainedObservation
                    )
                })
            }) {
                report.push(ImportedItem {
                    identity: identity.clone(),
                    digest: item_digest,
                    disposition: ImportDisposition::Duplicate,
                });
                continue;
            }
            let disposition = product.disposition();
            sqlx::query("INSERT INTO workflow_learning_archive_items(tenant,project,archive_digest,item_digest,identity,source_scope,record,disposition) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(tenant,project,archive_digest,item_digest) DO UPDATE SET disposition=EXCLUDED.disposition")
                .bind(&target.tenant).bind(&target.project).bind(&manifest.digest).bind(item_digest.as_str()).bind(identity)
                .bind(serde_json::to_value(&manifest.source_scope)?).bind(serde_json::to_value(&record)?).bind(serde_json::to_value(&disposition)?)
                .execute(&mut tx.executor()).await?;
            let support = PublicationSupport::Archive {
                scope: target.clone(),
                archive: archive_digest.clone(),
                item: item_digest.clone(),
            };
            match product {
                PreparedAdoption::Example { candidate, seal } => {
                    self.publish_candidates_in_transaction(
                        &mut tx,
                        target,
                        &seal,
                        &[*candidate],
                        PublicationPolicy::Reviewed,
                        true,
                        &support,
                    )
                    .await?
                }
                PreparedAdoption::Guidance { association, seal } => {
                    self.publish_associations_in_transaction(
                        &mut tx,
                        target,
                        &seal,
                        &[*association],
                        true,
                        &support,
                    )
                    .await?
                }
                PreparedAdoption::Hold(_) => {}
            }
            report.push(ImportedItem {
                identity: identity.clone(),
                digest: item_digest,
                disposition,
            });
        }
        tx.commit().await?;
        Ok(ArchiveImportReport {
            archive_digest,
            target_scope: target.clone(),
            items: report,
        })
    }
    /// Validate the filesystem content and confirm the exact manifest was
    /// adopted under the current target scope. This does not authorize serving.
    pub async fn validate_imported_filesystem_archive(
        &self,
        scope: &LibraryScope,
        source: &std::path::Path,
    ) -> std::result::Result<
        crate::workflow_learning::filesystem_archive::Manifest,
        crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
    > {
        use crate::workflow_learning::filesystem_archive::{
            FilesystemArchiveError as FsError, Reader,
        };
        let reader = Reader::open(source)?;
        let manifest = reader.manifest();
        let mut connection = self.pool.acquire().await?;
        let stored: Option<serde_json::Value> = sqlx::query_scalar("SELECT document FROM workflow_learning_archives WHERE tenant=$1 AND project=$2 AND digest=$3")
            .bind(&scope.tenant).bind(&scope.project).bind(&manifest.digest).fetch_optional(&mut connection).await?;
        if stored.as_ref() != Some(&serde_json::to_value(manifest)?) {
            return Err(FsError::Invalid);
        }
        Ok(reader.into_manifest())
    }
    /// Stream a private teaching collection from a single scoped database
    /// snapshot. Program observations, execution facts and seals are separate
    /// records, so repeated executions and reviews do not copy program bodies.
    pub async fn export_filesystem_archive(
        &self,
        scope: &LibraryScope,
        destination: &std::path::Path,
        kind: ArchiveKind,
    ) -> std::result::Result<
        crate::workflow_learning::filesystem_archive::Manifest,
        crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
    > {
        use crate::workflow_learning::filesystem_archive::{
            scope_token, FilesystemArchiveError as FsError, Record, StoredExampleState,
            StoredReviewState, Writer,
        };
        if kind == ArchiveKind::ServingLibrary {
            return self
                .export_selected_filesystem_archive(scope, destination)
                .await;
        }
        let mut writer = Writer::new(destination, scope.clone(), kind)?;
        let source_key = scope_token(scope)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut tx.executor())
            .await?;
        let mut offset = 0_i64;
        loop {
            let rows = sqlx::query("SELECT digest,observation FROM workflow_learning_program_observations WHERE tenant=$1 AND project=$2 ORDER BY digest LIMIT 128 OFFSET $3")
                .bind(&scope.tenant).bind(&scope.project).bind(offset).fetch_all(&mut tx.executor()).await?;
            let count = rows.len();
            for row in rows {
                let digest: String = row.try_get("digest")?;
                let observation: ProgramEvidence =
                    serde_json::from_value(row.try_get("observation")?)?;
                if observation.digest()?.as_str() != digest {
                    return Err(FsError::Integrity);
                }
                writer.write_record(
                    &format!("observation:{source_key}:{digest}"),
                    &Record::Observation {
                        source_scope: scope.clone(),
                        observation,
                    },
                )?;
            }
            if count < 128 {
                break;
            }
            offset += 128;
        }
        offset = 0;
        loop {
            let rows = sqlx::query("SELECT logical_session,session_revision,execution_id,program_digest,receipt,training_epoch FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 ORDER BY logical_session,session_revision,execution_id LIMIT 128 OFFSET $3")
                .bind(&scope.tenant).bind(&scope.project).bind(offset).fetch_all(&mut tx.executor()).await?;
            let count = rows.len();
            for row in rows {
                let logical_text: String = row.try_get("logical_session")?;
                let logical_session: LogicalSessionId =
                    serde_json::from_value(serde_json::Value::String(logical_text.clone()))?;
                let session_revision = u64::try_from(row.try_get::<i64, _>("session_revision")?)
                    .map_err(|_| FsError::Invalid)?;
                let run_text: String = row.try_get("execution_id")?;
                let execution_id = ExecutionId::from_wire(&run_text).ok_or(FsError::Invalid)?;
                let observation_digest =
                    ReceiptDigest::try_from(row.try_get::<String, _>("program_digest")?)
                        .map_err(|_| FsError::Invalid)?;
                let fact: StoredExecutionFact = serde_json::from_value(row.try_get("receipt")?)?;
                let training_epoch: Option<Uuid> = row.try_get("training_epoch")?;
                writer.write_record(
                    &format!("execution:{source_key}:{logical_text}:{session_revision}:{run_text}"),
                    &Record::Execution {
                        source_scope: scope.clone(),
                        logical_session,
                        session_revision,
                        execution_id,
                        observation_digest,
                        fact,
                        training_epoch,
                    },
                )?;
            }
            if count < 128 {
                break;
            }
            offset += 128;
        }
        offset = 0;
        loop {
            let rows = sqlx::query("SELECT id,digest,seal,seal_content_digest,state FROM workflow_learning_reviews r WHERE tenant=$1 AND project=$2 ORDER BY id LIMIT 128 OFFSET $3")
                .bind(&scope.tenant).bind(&scope.project).bind(offset).fetch_all(&mut tx.executor()).await?;
            let count = rows.len();
            for row in rows {
                let review_id: Uuid = row.try_get("id")?;
                let value: serde_json::Value = row.try_get("seal")?;
                let seal: StoredReviewSeal = serde_json::from_value(value.clone())?;
                if seal.request.scope != *scope {
                    return Err(FsError::Invalid);
                }
                let original_digest = ArchiveDigest::try_from(row.try_get::<String, _>("digest")?)
                    .map_err(|_| FsError::Invalid)?;
                let content_digest =
                    ReceiptDigest::try_from(row.try_get::<String, _>("seal_content_digest")?)
                        .map_err(|_| FsError::Invalid)?;
                if plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
                    &serde_json::to_value(&seal)?,
                )?) != content_digest.as_str()
                {
                    return Err(FsError::Integrity);
                }
                super::hydrate_review(&mut tx.executor(), value, content_digest.as_str()).await?;
                let disposition =
                    StoredReviewState::try_from(row.try_get::<String, _>("state")?.as_str())?;
                writer.write_record(
                    &format!("review:{source_key}:{review_id}"),
                    &Record::Review {
                        review_id,
                        original_digest,
                        content_digest,
                        disposition,
                        seal,
                    },
                )?;
            }
            if count < 128 {
                break;
            }
            offset += 128;
        }
        offset = 0;
        loop {
            let rows = sqlx::query("SELECT id,candidate,state,reason FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 ORDER BY id LIMIT 128 OFFSET $3")
                .bind(&scope.tenant).bind(&scope.project).bind(offset).fetch_all(&mut tx.executor()).await?;
            let count = rows.len();
            for row in rows {
                let revision_id: Uuid = row.try_get("id")?;
                let candidate: Candidate = serde_json::from_value(row.try_get("candidate")?)?;
                let state =
                    StoredExampleState::try_from(row.try_get::<String, _>("state")?.as_str())?;
                writer.write_record(
                    &format!("example:{revision_id}"),
                    &Record::Example {
                        revision_id,
                        candidate: Box::new(candidate),
                        state,
                        reason: row.try_get("reason")?,
                    },
                )?;
            }
            if count < 128 {
                break;
            }
            offset += 128;
        }
        offset = 0;
        loop {
            let rows = sqlx::query("SELECT id,association FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2 ORDER BY id LIMIT 128 OFFSET $3")
                .bind(&scope.tenant).bind(&scope.project).bind(offset).fetch_all(&mut tx.executor()).await?;
            let count = rows.len();
            for row in rows {
                let association_id: Uuid = row.try_get("id")?;
                let association: CatalogueAssociation =
                    serde_json::from_value(row.try_get("association")?)?;
                writer.write_record(
                    &format!("guidance:{association_id}"),
                    &Record::CatalogueGuidance {
                        association_id,
                        association,
                    },
                )?;
            }
            if count < 128 {
                break;
            }
            offset += 128;
        }
        for (kind, table, id_column, parent_table) in [
            (
                true,
                "workflow_learning_support",
                "example_id",
                "workflow_learning_examples",
            ),
            (
                false,
                "workflow_learning_guidance_support",
                "association_id",
                "workflow_learning_catalogue_guidance",
            ),
        ] {
            offset = 0;
            let query = format!("SELECT s.{id_column} AS item_id,r.id AS review_id,r.digest FROM {table} s JOIN workflow_learning_reviews r ON r.id=s.review_id JOIN {parent_table} p ON p.id=s.{id_column} WHERE r.tenant=$1 AND r.project=$2 AND p.tenant=$1 AND p.project=$2 ORDER BY s.{id_column},r.id LIMIT 128 OFFSET $3");
            loop {
                let rows = sqlx::query(&query)
                    .bind(&scope.tenant)
                    .bind(&scope.project)
                    .bind(offset)
                    .fetch_all(&mut tx.executor())
                    .await?;
                let count = rows.len();
                for row in rows {
                    let item_id: Uuid = row.try_get("item_id")?;
                    let review_id: Uuid = row.try_get("review_id")?;
                    let evidence = EvidenceReference {
                        source_scope: scope.clone(),
                        review_id,
                        seal_digest: ArchiveDigest::try_from(row.try_get::<String, _>("digest")?)
                            .map_err(|_| FsError::Invalid)?,
                    };
                    let record = if kind {
                        Record::ExampleSupport {
                            example_id: item_id,
                            evidence,
                        }
                    } else {
                        Record::GuidanceSupport {
                            association_id: item_id,
                            evidence,
                        }
                    };
                    writer.write_record(
                        &format!(
                            "{}:{item_id}:{source_key}:{review_id}",
                            if kind {
                                "example_support"
                            } else {
                                "guidance_support"
                            }
                        ),
                        &record,
                    )?;
                }
                if count < 128 {
                    break;
                }
                offset += 128;
            }
        }
        let mut groups = BTreeMap::new();
        collect_imported_groups(
            &mut tx,
            scope,
            false,
            &mut groups,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
        )
        .await?;
        let mut local_reviews = BTreeSet::new();
        for (key, group) in &groups {
            if group.reference.source_scope != *scope {
                continue;
            }
            let row = sqlx::query("SELECT digest,seal,seal_content_digest FROM workflow_learning_reviews WHERE tenant=$1 AND project=$2 AND id=$3")
                .bind(&scope.tenant).bind(&scope.project).bind(group.reference.review_id).fetch_optional(&mut tx.executor()).await?;
            if let Some(row) = row {
                let digest: String = row.try_get("digest")?;
                if digest != group.reference.seal_digest.as_str() {
                    return Err(FsError::Integrity);
                }
                let original = super::hydrate_review(
                    &mut tx.executor(),
                    row.try_get("seal")?,
                    &row.try_get::<String, _>("seal_content_digest")?,
                )
                .await?;
                for (candidate, _, _) in group.examples.values() {
                    if !candidate.observed_by(&original) {
                        return Err(FsError::Integrity);
                    }
                }
                for association in group.guidance.values() {
                    association
                        .validate(&original)
                        .map_err(|_| FsError::Integrity)?;
                }
                local_reviews.insert(key.clone());
            }
        }
        write_review_groups(&mut writer, groups, &local_reviews)?;
        tx.commit().await?;
        writer.finish()
    }
}
async fn imported_decision_valid(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    target: &LibraryScope,
    archive: &ArchiveDigest,
    evidence: &EvidenceReference,
) -> Result<bool> {
    let decision: Option<serde_json::Value> = sqlx::query_scalar("SELECT authority FROM workflow_training_archive_review_decisions WHERE tenant=$1 AND project=$2 AND archive_digest=$3 AND source_tenant=$4 AND source_project=$5 AND review_id=$6 AND seal_digest=$7 AND expires_at>clock_timestamp() FOR UPDATE")
        .bind(&target.tenant).bind(&target.project).bind(archive.as_str())
        .bind(&evidence.source_scope.tenant).bind(&evidence.source_scope.project).bind(evidence.review_id).bind(evidence.seal_digest.as_str())
        .fetch_optional(&mut tx.executor()).await?;
    let Some(value) = decision else {
        return Ok(false);
    };
    let permit: training::ImportAttestationPermit = serde_json::from_value(value)?;
    match training::recheck_import_attestation(tx, target, &permit).await {
        Ok(()) => Ok(true),
        Err(
            training::TrainingError::Denied
            | training::TrainingError::Unavailable
            | training::TrainingError::GrantUnavailable,
        ) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Compact indexes contain identities only. Program bodies remain in their own
/// bounded files and are loaded only while checking the relevant record.
#[derive(Default)]
struct FilesystemArchiveIndex {
    reviews: BTreeMap<(String, Uuid), String>,
    examples: BTreeMap<Uuid, String>,
    guidance: BTreeMap<Uuid, String>,
    example_support: BTreeMap<Uuid, Vec<EvidenceReference>>,
    guidance_support: BTreeMap<Uuid, Vec<EvidenceReference>>,
}
impl FilesystemArchiveIndex {
    fn build(
        reader: &crate::workflow_learning::filesystem_archive::Reader,
    ) -> std::result::Result<
        Self,
        crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
    > {
        use crate::workflow_learning::filesystem_archive::{
            scope_token, FilesystemArchiveError as FsError, Record,
        };
        let mut index = Self::default();
        for identity in reader.manifest().records.keys() {
            let record = reader.read_record(identity)?;
            match record {
                Record::Observation {
                    source_scope,
                    observation,
                } => {
                    if *identity
                        != format!(
                            "observation:{}:{}",
                            scope_token(&source_scope)?,
                            observation.digest()?.as_str()
                        )
                    {
                        return Err(FsError::Integrity);
                    }
                }
                Record::Execution {
                    source_scope,
                    logical_session,
                    session_revision,
                    execution_id,
                    observation_digest,
                    ..
                } => {
                    if *identity
                        != format!(
                            "execution:{}:{logical_session}:{session_revision}:{}",
                            scope_token(&source_scope)?,
                            execution_id.to_wire()
                        )
                        || !reader.manifest().records.contains_key(&format!(
                            "observation:{}:{}",
                            scope_token(&source_scope)?,
                            observation_digest.as_str()
                        ))
                    {
                        return Err(FsError::Invalid);
                    }
                }
                Record::Review {
                    review_id,
                    original_digest: _,
                    content_digest,
                    seal,
                    ..
                } => {
                    let source_key = scope_token(&seal.request.scope)?;
                    if *identity != format!("review:{source_key}:{review_id}")
                        || plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
                            &serde_json::to_value(&seal)?,
                        )?) != content_digest.as_str()
                        || index
                            .reviews
                            .insert((source_key, review_id), identity.clone())
                            .is_some()
                    {
                        return Err(FsError::Integrity);
                    }
                }
                Record::SelectedReview {
                    review_id,
                    evidence,
                    ..
                } => {
                    let source_key = scope_token(&evidence.source_scope)?;
                    if *identity != format!("review:{source_key}:{review_id}")
                        || evidence.executions.is_empty()
                        || index
                            .reviews
                            .insert((source_key, review_id), identity.clone())
                            .is_some()
                    {
                        return Err(FsError::Invalid);
                    }
                }
                Record::Example { revision_id, .. } => {
                    if *identity != format!("example:{revision_id}")
                        || index
                            .examples
                            .insert(revision_id, identity.clone())
                            .is_some()
                    {
                        return Err(FsError::Invalid);
                    }
                }
                Record::CatalogueGuidance { association_id, .. } => {
                    if *identity != format!("guidance:{association_id}")
                        || index
                            .guidance
                            .insert(association_id, identity.clone())
                            .is_some()
                    {
                        return Err(FsError::Invalid);
                    }
                }
                Record::ExampleSupport {
                    example_id,
                    evidence,
                } => {
                    if *identity
                        != format!(
                            "example_support:{example_id}:{}:{}",
                            scope_token(&evidence.source_scope)?,
                            evidence.review_id
                        )
                    {
                        return Err(FsError::Invalid);
                    }
                    index
                        .example_support
                        .entry(example_id)
                        .or_default()
                        .push(evidence);
                }
                Record::GuidanceSupport {
                    association_id,
                    evidence,
                } => {
                    if *identity
                        != format!(
                            "guidance_support:{association_id}:{}:{}",
                            scope_token(&evidence.source_scope)?,
                            evidence.review_id
                        )
                    {
                        return Err(FsError::Invalid);
                    }
                    index
                        .guidance_support
                        .entry(association_id)
                        .or_default()
                        .push(evidence);
                }
            }
        }
        if index
            .example_support
            .keys()
            .any(|id| !index.examples.contains_key(id))
            || index
                .guidance_support
                .keys()
                .any(|id| !index.guidance.contains_key(id))
        {
            return Err(FsError::Invalid);
        }
        for identity in index.reviews.values() {
            let (review_id, original_digest, source_scope) = match reader.read_record(identity)? {
                Record::Review {
                    review_id,
                    original_digest,
                    seal,
                    ..
                } => (review_id, original_digest, seal.request.scope),
                Record::SelectedReview {
                    review_id,
                    original_digest,
                    evidence,
                } => (review_id, original_digest, evidence.source_scope),
                _ => return Err(FsError::Invalid),
            };
            index.read_seal(
                reader,
                &EvidenceReference {
                    source_scope,
                    review_id,
                    seal_digest: original_digest,
                },
            )?;
        }
        for evidence in index
            .example_support
            .values()
            .chain(index.guidance_support.values())
            .flatten()
        {
            index.read_seal(reader, evidence)?;
        }
        Ok(index)
    }

    fn read_seal(
        &self,
        reader: &crate::workflow_learning::filesystem_archive::Reader,
        evidence: &EvidenceReference,
    ) -> std::result::Result<
        SealedReview,
        crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
    > {
        use crate::workflow_learning::filesystem_archive::{
            scope_token, FilesystemArchiveError as FsError, Record,
        };
        let identity = self
            .reviews
            .get(&(scope_token(&evidence.source_scope)?, evidence.review_id))
            .ok_or(FsError::Invalid)?;
        let (original_digest, seal) = match reader.read_record(identity)? {
            Record::Review {
                original_digest,
                seal,
                ..
            } => (original_digest, seal),
            Record::SelectedReview {
                original_digest,
                evidence,
                ..
            } => {
                // An ephemeral verification view is not the original seal and
                // is never serialized or presented as its free-text claims.
                let request = ReviewRequest {
                    scope: evidence.source_scope,
                    logical_session: evidence.logical_session,
                    session_revision: evidence.session_revision,
                    intent: String::new(),
                    subtasks: Vec::new(),
                };
                (
                    original_digest,
                    StoredReviewSeal {
                        request,
                        executions: evidence.executions,
                        assurance: evidence.assurance,
                    },
                )
            }
            _ => return Err(FsError::Invalid),
        };
        if original_digest != evidence.seal_digest || seal.request.scope != evidence.source_scope {
            return Err(FsError::Integrity);
        }
        let mut executions = Vec::new();
        for reference in &seal.executions {
            let key = format!(
                "execution:{}:{}:{}:{}",
                scope_token(&seal.request.scope)?,
                seal.request.logical_session,
                reference.session_revision,
                reference.execution_id.to_wire()
            );
            let Record::Execution {
                source_scope,
                logical_session,
                session_revision,
                execution_id,
                observation_digest,
                fact,
                ..
            } = reader.read_record(&key)?
            else {
                return Err(FsError::Invalid);
            };
            if source_scope != seal.request.scope
                || logical_session != seal.request.logical_session
                || session_revision != reference.session_revision
                || execution_id != reference.execution_id
                || observation_digest != reference.program_digest
            {
                return Err(FsError::Invalid);
            }
            let Record::Observation {
                source_scope,
                observation,
            } = reader.read_record(&format!(
                "observation:{}:{}",
                scope_token(&seal.request.scope)?,
                observation_digest.as_str()
            ))?
            else {
                return Err(FsError::Invalid);
            };
            if source_scope != seal.request.scope || observation.digest()? != observation_digest {
                return Err(FsError::Integrity);
            }
            let receipt = ExecutionReceipt::hydrate(
                seal.request.scope.clone(),
                logical_session,
                session_revision,
                execution_id,
                fact,
                observation,
            );
            if receipt.evidence_digest()? != reference.evidence_digest {
                return Err(FsError::Integrity);
            }
            executions.push(receipt);
        }
        Ok(SealedReview {
            request: seal.request,
            executions,
            assurance: seal.assurance,
        })
    }
}

struct ImportedReviewGroup {
    reference: EvidenceReference,
    seal: SealedReview,
    examples: BTreeMap<
        Uuid,
        (
            Candidate,
            Option<String>,
            crate::workflow_learning::filesystem_archive::StoredExampleState,
        ),
    >,
    guidance: BTreeMap<Uuid, CatalogueAssociation>,
}

async fn archived_record(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    scope: &LibraryScope,
    archive: &str,
    identity: &str,
) -> std::result::Result<
    crate::workflow_learning::filesystem_archive::Record,
    crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
> {
    use crate::workflow_learning::filesystem_archive::{
        record_digest, FilesystemArchiveError as FsError,
    };
    let row = sqlx::query("SELECT item_digest,record FROM workflow_learning_archive_items WHERE tenant=$1 AND project=$2 AND archive_digest=$3 AND identity=$4")
        .bind(&scope.tenant).bind(&scope.project).bind(archive).bind(identity).fetch_optional(&mut tx.executor()).await?.ok_or(FsError::Invalid)?;
    let record = serde_json::from_value(row.try_get("record")?)?;
    if record_digest(&record)? != row.try_get::<String, _>("item_digest")? {
        return Err(FsError::Integrity);
    }
    Ok(record)
}

async fn archived_review(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    scope: &LibraryScope,
    archive: &str,
    evidence: &EvidenceReference,
) -> std::result::Result<
    SealedReview,
    crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
> {
    use crate::workflow_learning::filesystem_archive::{
        scope_token, FilesystemArchiveError as FsError, Record,
    };
    let token = scope_token(&evidence.source_scope)?;
    let record = archived_record(
        tx,
        scope,
        archive,
        &format!("review:{token}:{}", evidence.review_id),
    )
    .await?;
    let (digest, seal) = match record {
        Record::Review {
            original_digest,
            content_digest,
            seal,
            ..
        } => {
            if plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
                &serde_json::to_value(&seal)?,
            )?) != content_digest.as_str()
            {
                return Err(FsError::Integrity);
            }
            (original_digest, seal)
        }
        Record::SelectedReview {
            original_digest,
            evidence,
            ..
        } => (
            original_digest,
            StoredReviewSeal {
                request: ReviewRequest {
                    scope: evidence.source_scope,
                    logical_session: evidence.logical_session,
                    session_revision: evidence.session_revision,
                    intent: String::new(),
                    subtasks: Vec::new(),
                },
                executions: evidence.executions,
                assurance: evidence.assurance,
            },
        ),
        _ => return Err(FsError::Invalid),
    };
    if digest != evidence.seal_digest || seal.request.scope != evidence.source_scope {
        return Err(FsError::Integrity);
    }
    let mut executions = Vec::new();
    for reference in &seal.executions {
        let key = format!(
            "execution:{token}:{}:{}:{}",
            seal.request.logical_session,
            reference.session_revision,
            reference.execution_id.to_wire()
        );
        let Record::Execution {
            source_scope,
            logical_session,
            session_revision,
            execution_id,
            observation_digest,
            fact,
            ..
        } = archived_record(tx, scope, archive, &key).await?
        else {
            return Err(FsError::Invalid);
        };
        if source_scope != evidence.source_scope
            || logical_session != seal.request.logical_session
            || session_revision != reference.session_revision
            || execution_id != reference.execution_id
            || observation_digest != reference.program_digest
        {
            return Err(FsError::Integrity);
        }
        let Record::Observation {
            source_scope,
            observation,
        } = archived_record(
            tx,
            scope,
            archive,
            &format!("observation:{token}:{}", observation_digest.as_str()),
        )
        .await?
        else {
            return Err(FsError::Invalid);
        };
        if source_scope != evidence.source_scope || observation.digest()? != observation_digest {
            return Err(FsError::Integrity);
        }
        let receipt = ExecutionReceipt::hydrate(
            source_scope,
            logical_session,
            session_revision,
            execution_id,
            fact,
            observation,
        );
        if receipt.evidence_digest()? != reference.evidence_digest {
            return Err(FsError::Integrity);
        }
        executions.push(receipt);
    }
    Ok(SealedReview {
        request: seal.request,
        executions,
        assurance: seal.assurance,
    })
}

fn insert_review_group(
    groups: &mut BTreeMap<(String, Uuid), ImportedReviewGroup>,
    reference: EvidenceReference,
    seal: SealedReview,
) -> std::result::Result<
    &mut ImportedReviewGroup,
    crate::workflow_learning::filesystem_archive::FilesystemArchiveError,
> {
    use crate::workflow_learning::filesystem_archive::{
        scope_token, FilesystemArchiveError as FsError,
    };
    let key = (scope_token(&reference.source_scope)?, reference.review_id);
    if let Some(group) = groups.get_mut(&key) {
        if group.reference.seal_digest != reference.seal_digest
            || group.seal.request.scope != seal.request.scope
            || group.seal.request.logical_session != seal.request.logical_session
            || group.seal.request.session_revision != seal.request.session_revision
            || serde_json::to_value(group.seal.assurance)? != serde_json::to_value(seal.assurance)?
        {
            return Err(FsError::Integrity);
        }
        for receipt in seal.executions {
            if let Some(existing) = group.seal.executions.iter().find(|r| {
                r.session_revision == receipt.session_revision
                    && r.execution_id == receipt.execution_id
            }) {
                if existing.evidence_digest()? != receipt.evidence_digest()?
                    || existing.program_evidence().digest()?
                        != receipt.program_evidence().digest()?
                {
                    return Err(FsError::Integrity);
                }
            } else {
                group.seal.executions.push(receipt);
            }
        }
    } else {
        groups.insert(
            key.clone(),
            ImportedReviewGroup {
                reference,
                seal,
                examples: BTreeMap::new(),
                guidance: BTreeMap::new(),
            },
        );
    }
    Ok(groups.get_mut(&key).expect("group inserted"))
}

async fn collect_imported_groups(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    scope: &LibraryScope,
    serving: bool,
    groups: &mut BTreeMap<(String, Uuid), ImportedReviewGroup>,
    covered_examples: &mut BTreeSet<Uuid>,
    covered_guidance: &mut BTreeSet<Uuid>,
) -> std::result::Result<(), crate::workflow_learning::filesystem_archive::FilesystemArchiveError> {
    use crate::workflow_learning::filesystem_archive::{
        FilesystemArchiveError as FsError, Record, StoredExampleState,
    };
    for guidance in [false, true] {
        let query = if guidance {
            "SELECT s.archive_digest,i.identity,p.id,p.association AS product,NULL::text AS state,NULL::text AS reason FROM workflow_learning_archive_guidance_support s JOIN workflow_learning_archive_items i USING(tenant,project,archive_digest,item_digest) JOIN workflow_learning_catalogue_guidance p ON p.id=s.association_id WHERE s.tenant=$1 AND s.project=$2 AND p.tenant=$1 AND p.project=$2 ORDER BY s.archive_digest,i.identity,p.id"
        } else {
            "SELECT s.archive_digest,i.identity,p.id,p.candidate AS product,p.state,p.reason FROM workflow_learning_archive_example_support s JOIN workflow_learning_archive_items i USING(tenant,project,archive_digest,item_digest) JOIN workflow_learning_examples p ON p.id=s.example_id WHERE s.tenant=$1 AND s.project=$2 AND p.tenant=$1 AND p.project=$2 AND (NOT $3 OR p.state='active') ORDER BY s.archive_digest,i.identity,p.id"
        };
        let mut q = sqlx::query(query).bind(&scope.tenant).bind(&scope.project);
        if !guidance {
            q = q.bind(serving);
        }
        let rows = q.fetch_all(&mut tx.executor()).await?;
        for row in rows {
            let archive: String = row.try_get("archive_digest")?;
            let identity: String = row.try_get("identity")?;
            let id: Uuid = row.try_get("id")?;
            let source = archived_record(tx, scope, &archive, &identity).await?;
            let (source_id, support_kind, source_reason) = match source {
                Record::Example {
                    revision_id,
                    reason,
                    ..
                } if !guidance => (revision_id, "example_support", reason),
                Record::CatalogueGuidance { association_id, .. } if guidance => {
                    (association_id, "guidance_support", None)
                }
                _ => return Err(FsError::Integrity),
            };
            let prefix = format!("{support_kind}:{source_id}:%");
            let identities: Vec<String> = sqlx::query_scalar("SELECT identity FROM workflow_learning_archive_items WHERE tenant=$1 AND project=$2 AND archive_digest=$3 AND identity LIKE $4 ORDER BY identity")
                .bind(&scope.tenant).bind(&scope.project).bind(&archive).bind(prefix).fetch_all(&mut tx.executor()).await?;
            if identities.is_empty() {
                return Err(FsError::Integrity);
            }
            for support_identity in identities {
                let reference =
                    match archived_record(tx, scope, &archive, &support_identity).await? {
                        Record::ExampleSupport {
                            example_id,
                            evidence,
                        } if !guidance && example_id == source_id => evidence,
                        Record::GuidanceSupport {
                            association_id,
                            evidence,
                        } if guidance && association_id == source_id => evidence,
                        _ => return Err(FsError::Integrity),
                    };
                let seal = archived_review(tx, scope, &archive, &reference).await?;
                let group = insert_review_group(groups, reference, seal)?;
                if guidance {
                    let association: CatalogueAssociation =
                        serde_json::from_value(row.try_get("product")?)?;
                    group.guidance.insert(id, association);
                    covered_guidance.insert(id);
                } else {
                    let candidate: Candidate = serde_json::from_value(row.try_get("product")?)?;
                    let state =
                        StoredExampleState::try_from(row.try_get::<String, _>("state")?.as_str())?;
                    let local_reason: Option<String> = row.try_get("reason")?;
                    let imported_reason = super::recuration::preserved_imported_reason(
                        source_reason.clone(),
                        &candidate,
                    );
                    group
                        .examples
                        .insert(id, (candidate, local_reason.or(imported_reason), state));
                    covered_examples.insert(id);
                }
            }
        }
    }
    Ok(())
}

fn write_review_groups(
    writer: &mut crate::workflow_learning::filesystem_archive::Writer,
    groups: BTreeMap<(String, Uuid), ImportedReviewGroup>,
    local_reviews: &BTreeSet<(String, Uuid)>,
) -> std::result::Result<(), crate::workflow_learning::filesystem_archive::FilesystemArchiveError> {
    use crate::workflow_learning::filesystem_archive::Record;
    crate::workflow_learning::serving_projection::validate_quality_anchors(
        groups.values().flat_map(|group| {
            group
                .examples
                .iter()
                .map(|(id, (candidate, _, _))| (id, candidate, &group.seal))
        }),
    )?;
    for ((token, review_id), group) in groups {
        let candidates: Vec<_> = group
            .examples
            .values()
            .map(|(candidate, _, _)| candidate.clone())
            .collect();
        let associations: Vec<_> = group.guidance.values().cloned().collect();
        let (evidence, receipts) =
            crate::workflow_learning::serving_projection::select_review_evidence(
                &group.seal,
                &candidates,
                &associations,
            )?;
        if !local_reviews.contains(&(token.clone(), review_id)) {
            writer.write_record(
                &format!("review:{token}:{review_id}"),
                &Record::SelectedReview {
                    review_id,
                    original_digest: group.reference.seal_digest.clone(),
                    evidence,
                },
            )?;
            for receipt in receipts {
                let observation = receipt.program_evidence();
                let observation_digest = observation.digest()?;
                writer.write_record(
                    &format!("observation:{token}:{}", observation_digest.as_str()),
                    &Record::Observation {
                        source_scope: group.reference.source_scope.clone(),
                        observation,
                    },
                )?;
                writer.write_record(
                    &format!(
                        "execution:{token}:{}:{}:{}",
                        receipt.logical_session,
                        receipt.session_revision,
                        receipt.execution_id.to_wire()
                    ),
                    &Record::Execution {
                        source_scope: group.reference.source_scope.clone(),
                        logical_session: receipt.logical_session,
                        session_revision: receipt.session_revision,
                        execution_id: receipt.execution_id,
                        observation_digest,
                        fact: receipt.stored_fact(),
                        training_epoch: None,
                    },
                )?;
            }
        }
        for (id, (candidate, reason, state)) in group.examples {
            writer.write_record(
                &format!("example:{id}"),
                &Record::Example {
                    revision_id: id,
                    candidate: Box::new(candidate),
                    state,
                    reason,
                },
            )?;
            writer.write_record(
                &format!("example_support:{id}:{token}:{review_id}"),
                &Record::ExampleSupport {
                    example_id: id,
                    evidence: group.reference.clone(),
                },
            )?;
        }
        for (id, association) in group.guidance {
            writer.write_record(
                &format!("guidance:{id}"),
                &Record::CatalogueGuidance {
                    association_id: id,
                    association,
                },
            )?;
            writer.write_record(
                &format!("guidance_support:{id}:{token}:{review_id}"),
                &Record::GuidanceSupport {
                    association_id: id,
                    evidence: group.reference.clone(),
                },
            )?;
        }
    }
    Ok(())
}

/// Only produced after immutable source evidence and destination compatibility pass.
enum PreparedAdoption {
    Example {
        candidate: Box<Candidate>,
        seal: Box<SealedReview>,
    },
    Guidance {
        association: Box<CatalogueAssociation>,
        seal: Box<SealedReview>,
    },
    Hold(ImportDisposition),
}
impl PreparedAdoption {
    fn disposition(&self) -> ImportDisposition {
        match self {
            Self::Hold(value) => value.clone(),
            _ => ImportDisposition::EligibleCandidate,
        }
    }
}

fn stage_archive_example(
    session: &ExecuteSession,
    program: &crate::workflow_learning::teaching::TeachingProgram,
    allowed: &DiscoveryAuthorization,
) -> std::result::Result<ExecuteSession, crate::workflow_learning::serving::ServingError> {
    use crate::workflow_learning::serving::{stage_example_symbols, ExampleDependencyProjection};
    let catalogs = session
        .contexts_by_entry
        .iter()
        .map(|(entry, context)| (entry.clone(), context.clone()))
        .collect();
    let projection = ExampleDependencyProjection::resolve(
        program,
        &catalogs,
        &session.prerequisite_deployments,
        allowed,
    )?;
    Ok(stage_example_symbols(
        session.clone(),
        &projection.required,
        &projection.groups,
        &projection.domains,
    )?
    .0)
}

fn compilation_disposition(error: crate::compilation_error::CompilationError) -> ImportDisposition {
    if matches!(
        error,
        crate::compilation_error::CompilationError::Program(_)
    ) {
        ImportDisposition::Quarantined {
            reason: ArchiveCompatibilityIssue::Compilation {
                diagnostic: error.to_string(),
            },
        }
    } else {
        ImportDisposition::AwaitingCompatibility {
            reason: ArchiveCompatibilityIssue::Infrastructure {
                diagnostic: error.to_string(),
            },
        }
    }
}
