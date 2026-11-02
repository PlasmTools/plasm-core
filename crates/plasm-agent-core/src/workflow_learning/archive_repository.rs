//! Scoped archive retention and publication through the ordinary bounded portfolio.
use super::*;
use crate::discovery_store::DiscoveryAuthorization;
use crate::workflow_learning::archive::*;
use crate::workflow_learning::catalogue_guidance::CatalogueAssociation;
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
    pub async fn record_archive_assessment(
        &self,
        scope: &LibraryScope,
        assessment: &TaskAssessment,
    ) -> std::result::Result<(), ArchiveError> {
        assessment.validate()?;
        let value = serde_json::to_value(assessment)?;
        let mut connection = self.pool.acquire().await?;
        let previous:serde_json::Value=sqlx::query_scalar("INSERT INTO workflow_learning_archive_assessments(tenant,project,attempt_id,assessment) VALUES($1,$2,$3,$4) ON CONFLICT(tenant,project,attempt_id) DO UPDATE SET assessment=workflow_learning_archive_assessments.assessment RETURNING assessment")
            .bind(&scope.tenant).bind(&scope.project).bind(&assessment.attempt_id).bind(&value).fetch_one(&mut connection).await?;
        if previous != value {
            return Err(RepositoryError::ImmutableConflict.into());
        }
        Ok(())
    }
    pub async fn export_archive(
        &self,
        scope: &LibraryScope,
        request: &ArchiveExportRequest,
    ) -> std::result::Result<ValidatedArchive, ArchiveError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut tx.executor())
            .await?;
        let rows=sqlx::query("SELECT assessment FROM workflow_learning_archive_assessments WHERE tenant=$1 AND project=$2 ORDER BY attempt_id LIMIT 10001").bind(&scope.tenant).bind(&scope.project).fetch_all(&mut tx.executor()).await?;
        if rows.len() > MAX_ITEMS {
            return Err(ArchiveError::Limit);
        }
        let assessments: Vec<TaskAssessment> = rows
            .into_iter()
            .map(|row| Ok(serde_json::from_value(row.try_get("assessment")?)?))
            .collect::<std::result::Result<_, ArchiveError>>()?;
        let members: BTreeSet<_> = request
            .collection
            .tasks
            .iter()
            .map(|task| (&task.task_id, &task.group_id))
            .collect();
        let mut owners = BTreeMap::new();
        for assessment in &assessments {
            assessment.validate()?;
            if assessment.protocol_digest != request.collection.protocol_digest
                || !members.contains(&(&assessment.task_id, &assessment.group_id))
            {
                return Err(ArchiveError::InvalidMetadata);
            }
            for session in &assessment.logical_sessions {
                if owners
                    .insert(*session, &assessment.attempt_id)
                    .is_some_and(|owner| owner != &assessment.attempt_id)
                {
                    return Err(ArchiveError::InvalidMetadata);
                }
            }
        }
        let mut required_sessions = BTreeSet::new();
        let mut review_sessions = BTreeMap::new();
        let reviews=sqlx::query("SELECT id,digest,seal,state FROM workflow_learning_reviews WHERE tenant=$1 AND project=$2 ORDER BY id LIMIT 10001").bind(&scope.tenant).bind(&scope.project).fetch_all(&mut tx.executor()).await?;
        let mut evidence = BTreeMap::new();
        let mut items = Vec::new();
        for row in reviews {
            let id: Uuid = row.try_get("id")?;
            let value: serde_json::Value = row.try_get("seal")?;
            let logical: crate::session_identity::LogicalSessionId = serde_json::from_value(
                value
                    .pointer("/request/logical_session")
                    .ok_or(ArchiveError::Reference)?
                    .clone(),
            )?;
            review_sessions.insert(id, logical);
            if request.kind == ArchiveKind::PrivateCollection {
                required_sessions.insert(logical);
            }

            let hash = ArchiveDigest::try_from(row.try_get::<String, _>("digest")?)?;
            evidence.insert(
                id,
                EvidenceReference {
                    source_scope: scope.clone(),
                    review_id: id,
                    seal_digest: hash.clone(),
                },
            );
            if request.kind == ArchiveKind::PrivateCollection {
                let value: serde_json::Value = row.try_get("seal")?;
                let disposition = row.try_get("state")?;
                let record = match serde_json::from_value::<SealedReview>(value.clone()) {
                    Ok(seal) if serde_json::to_value(&seal)? == value => ArchiveRecord::Review {
                        review_id: id,
                        seal_digest: hash,
                        disposition,
                        seal,
                    },
                    _ => ArchiveRecord::HistoricalReview {
                        review_id: id,
                        seal_digest: hash,
                        disposition,
                        seal: value,
                    },
                };
                items.push(ArchiveItem::new(format!("review:{id}"), record)?);
            }
        }
        if evidence.len() > MAX_ITEMS {
            return Err(ArchiveError::Limit);
        }
        if request.kind == ArchiveKind::PrivateCollection {
            let rows=sqlx::query("SELECT receipt,logical_session,session_revision,run_id FROM workflow_learning_receipts WHERE tenant=$1 AND project=$2 ORDER BY logical_session,session_revision,run_id LIMIT 10001").bind(&scope.tenant).bind(&scope.project).fetch_all(&mut tx.executor()).await?;
            for row in rows {
                let logical: crate::session_identity::LogicalSessionId = serde_json::from_value(
                    serde_json::Value::String(row.try_get("logical_session")?),
                )?;
                required_sessions.insert(logical);
                let value: serde_json::Value = row.try_get("receipt")?;
                let record = match serde_json::from_value::<ExecutionReceipt>(value.clone()) {
                    Ok(receipt) if serde_json::to_value(&receipt)? == value => {
                        ArchiveRecord::Observation { receipt }
                    }
                    _ => ArchiveRecord::HistoricalObservation { receipt: value },
                };
                items.push(ArchiveItem::new(
                    format!(
                        "observation:{}:{}:{}",
                        row.try_get::<String, _>("logical_session")?,
                        row.try_get::<i64, _>("session_revision")?,
                        row.try_get::<String, _>("run_id")?
                    ),
                    record,
                )?);
            }
            for assessment in &assessments {
                items.push(ArchiveItem::new(
                    format!("assessment:{}", assessment.attempt_id),
                    ArchiveRecord::Assessment {
                        assessment: assessment.clone(),
                    },
                )?);
            }
        }
        let mut external = BTreeMap::new();
        let rows=sqlx::query("SELECT id,candidate,state FROM workflow_learning_examples WHERE tenant=$1 AND project=$2 ORDER BY id LIMIT 10001").bind(&scope.tenant).bind(&scope.project).fetch_all(&mut tx.executor()).await?;
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let state: String = row.try_get("state")?;
            let value: serde_json::Value = row.try_get("candidate")?;
            if state == "active" {
                let candidate: Candidate = serde_json::from_value(value)?;
                if !candidate
                    .checked_program()
                    .is_some_and(|p| p.integrity_valid())
                {
                    return Err(ArchiveError::Integrity);
                }
                let ids:Vec<Uuid>=sqlx::query_scalar("SELECT review_id FROM workflow_learning_support WHERE example_id=$1 ORDER BY review_id").bind(id).fetch_all(&mut tx.executor()).await?;
                let mut support = ids
                    .into_iter()
                    .map(|id| evidence.get(&id).cloned().ok_or(ArchiveError::Reference))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                support.extend(imported_support(&mut tx, scope, id, false).await?);
                support
                    .sort_by_key(|reference| (reference.review_id, reference.seal_digest.clone()));
                support.dedup();
                for r in &support {
                    external.insert(r.key(), r.clone());
                }
                items.push(ArchiveItem::new(
                    format!("example:{id}"),
                    ArchiveRecord::Example {
                        revision_id: id,
                        candidate,
                        support,
                    },
                )?);
            } else if request.kind == ArchiveKind::PrivateCollection {
                items.push(ArchiveItem::new(
                    format!("revision:{id}"),
                    ArchiveRecord::RetainedRevision {
                        revision_id: id,
                        disposition: state,
                        candidate: value,
                    },
                )?);
            }
        }
        let rows=sqlx::query("SELECT id,association FROM workflow_learning_catalogue_guidance WHERE tenant=$1 AND project=$2 ORDER BY id LIMIT 10001").bind(&scope.tenant).bind(&scope.project).fetch_all(&mut tx.executor()).await?;
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let association: CatalogueAssociation =
                serde_json::from_value(row.try_get("association")?)?;
            let ids:Vec<Uuid>=sqlx::query_scalar("SELECT review_id FROM workflow_learning_guidance_support WHERE association_id=$1 ORDER BY review_id").bind(id).fetch_all(&mut tx.executor()).await?;
            let mut support = ids
                .into_iter()
                .map(|id| evidence.get(&id).cloned().ok_or(ArchiveError::Reference))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            support.extend(imported_support(&mut tx, scope, id, true).await?);
            support.sort_by_key(|reference| (reference.review_id, reference.seal_digest.clone()));
            support.dedup();
            for r in &support {
                external.insert(r.key(), r.clone());
            }
            items.push(ArchiveItem::new(
                format!("guidance:{id}"),
                ArchiveRecord::Guidance {
                    association_id: id,
                    association,
                    support,
                },
            )?);
        }
        let mut unmapped_external = Vec::new();
        for reference in external.values() {
            if reference.source_scope == *scope {
                required_sessions.insert(
                    *review_sessions
                        .get(&reference.review_id)
                        .ok_or(ArchiveError::Reference)?,
                );
            } else if !inherited_attribution(&mut tx, scope, reference, &request.collection).await?
            {
                unmapped_external.push(reference.review_id);
            }
        }
        let unmapped: Vec<_> = required_sessions
            .into_iter()
            .filter(|session| !owners.contains_key(session))
            .collect();
        let task_provenance = if unmapped.is_empty() && unmapped_external.is_empty() {
            TaskProvenance::Complete
        } else {
            TaskProvenance::Unmapped {
                logical_sessions: unmapped,
                external_reviews: unmapped_external,
            }
        };
        let external_evidence = external
            .into_values()
            .filter(|reference| {
                request.kind == ArchiveKind::ServingLibrary
                    || reference.source_scope != *scope
                    || !evidence.contains_key(&reference.review_id)
            })
            .collect();
        let archive = ValidatedArchive::create(
            ArchiveManifest {
                version: ARCHIVE_VERSION,
                source_scope: scope.clone(),
                kind: request.kind,
                collection: request.collection.clone(),
                assessment_coverage: AssessmentCoverage::NotIncluded,
                task_provenance,
                external_evidence,
            },
            items,
        )?;
        tx.commit().await?;
        Ok(archive)
    }
    /// Re-export exact imported evidence; never reconstruct source seals in target scope.
    pub async fn export_retained_archive(
        &self,
        scope: &LibraryScope,
        digest: &ArchiveDigest,
    ) -> std::result::Result<ValidatedArchive, ArchiveError> {
        let mut connection = self.pool.acquire().await?;
        let value:serde_json::Value=sqlx::query_scalar("SELECT document FROM workflow_learning_archives WHERE tenant=$1 AND project=$2 AND digest=$3").bind(&scope.tenant).bind(&scope.project).bind(digest.as_str()).fetch_optional(&mut connection).await?.ok_or(RepositoryError::Unavailable)?;
        ValidatedArchive::decode(&serde_json::to_vec(&value)?)
    }
    /// Host-authorized archive adoption. Compilation here validates only; it dispatches no effects.
    #[allow(clippy::too_many_arguments)]
    pub async fn import_archive(
        &self,
        target: &LibraryScope,
        bytes: &[u8],
        policy: &ArchiveImportPolicy,
        host: &PlasmHostState,
        session: &ExecuteSession,
        allowed: &DiscoveryAuthorization,
    ) -> std::result::Result<ArchiveImportReport, ArchiveError> {
        let archive = ValidatedArchive::decode(bytes)?;
        let document = archive.document();
        let isolated = crate::workflow_learning::teaching::validation_session(session);
        let session = &isolated;
        if document.digest != policy.expected_digest {
            return Err(ArchiveError::Integrity);
        }
        if document.manifest.source_scope != policy.source_scope {
            return Err(ArchiveError::Scope);
        }
        if document.manifest.collection.tasks.iter().any(|task| {
            policy.excluded_tasks.contains(&task.task_id)
                || policy.excluded_groups.contains(&task.group_id)
        }) {
            return Err(ArchiveError::InvalidMetadata);
        }
        let mut seals = BTreeMap::new();
        for item in &document.items {
            if let ArchiveRecord::Review {
                review_id,
                seal_digest,
                seal,
                ..
            } = &item.record
            {
                seals.insert(
                    (
                        seal.request.scope.tenant.clone(),
                        seal.request.scope.project.clone(),
                        *review_id,
                        seal_digest.clone(),
                    ),
                    seal.clone(),
                );
            }
        }
        for reference in &document.manifest.external_evidence {
            let mut connection = self.pool.acquire().await?;
            let records:Vec<serde_json::Value>=sqlx::query_scalar("SELECT record FROM workflow_learning_archive_items WHERE tenant=$1 AND project=$2 AND record->>'kind'='review' AND record->>'review_id'=$3 AND record->>'seal_digest'=$4 AND source_scope=$5 LIMIT 2")
                .bind(&target.tenant).bind(&target.project).bind(reference.review_id.to_string()).bind(reference.seal_digest.as_str()).bind(serde_json::to_value(&reference.source_scope)?).fetch_all(&mut connection).await?;
            for value in records {
                if let ArchiveRecord::Review { seal, .. } = serde_json::from_value(value)? {
                    if seal.request.scope != reference.source_scope {
                        return Err(ArchiveError::Scope);
                    }
                    seals.insert(reference.key(), seal);
                }
            }
        }
        let attributed = matches!(document.manifest.task_provenance, TaskProvenance::Complete);
        let mut prepared = Vec::new();
        for item in &document.items {
            let incompatible = match &item.record {
                ArchiveRecord::Example { candidate, .. } => candidate
                    .program()
                    .filter(|program| !program.witness_version_current()),
                _ => None,
            };
            let product = if let Some(program) = incompatible {
                PreparedAdoption::Hold(ImportDisposition::Quarantined {
                    reason: ArchiveCompatibilityIssue::IncompatibleTeachingWitness {
                        schema_version: program.schema_version,
                        compiler_version: program.compiler_version.clone(),
                        comp_wire_version: program.comp_wire_version,
                    },
                })
            } else if !attributed
                && matches!(
                    item.record,
                    ArchiveRecord::Example { .. } | ArchiveRecord::Guidance { .. }
                )
            {
                PreparedAdoption::Hold(ImportDisposition::AwaitingCompatibility {
                    reason: ArchiveCompatibilityIssue::TaskProvenanceUnavailable,
                })
            } else {
                match &item.record {
                    ArchiveRecord::Example {
                        candidate, support, ..
                    } => {
                        let seal = support
                            .iter()
                            .filter_map(|reference| seals.get(&reference.key()))
                            .find(|seal| candidate.verified_by(seal));
                        match seal {
                            None => {
                                PreparedAdoption::Hold(ImportDisposition::AwaitingCompatibility {
                                    reason:
                                        ArchiveCompatibilityIssue::SupportingEvidenceUnavailable,
                                })
                            }
                            Some(seal) => {
                                let program = candidate.program().ok_or(ArchiveError::Integrity)?;
                                if program
                                    .catalog_fingerprints
                                    .keys()
                                    .any(|entry| !allowed.catalogs.contains(entry))
                                    || program
                                        .required_capabilities
                                        .iter()
                                        .any(|cap| !allowed.permits(cap))
                                {
                                    PreparedAdoption::Hold(
                                        ImportDisposition::AwaitingCompatibility {
                                            reason:
                                                ArchiveCompatibilityIssue::AuthorizationUnavailable,
                                        },
                                    )
                                } else {
                                    match stage_archive_example(session, program, allowed) {
                                        Err(error) => PreparedAdoption::Hold(
                                            ImportDisposition::AwaitingCompatibility {
                                                reason:
                                                    ArchiveCompatibilityIssue::SessionPreparation {
                                                        diagnostic: error.to_string(),
                                                    },
                                            },
                                        ),
                                        Ok(staged) => {
                                            let session = &staged;
                                            match program.rebind_teaching(session) {
                                                Err(error) => PreparedAdoption::Hold(
                                                    ImportDisposition::Quarantined {
                                                        reason:
                                                            ArchiveCompatibilityIssue::Teaching {
                                                                diagnostic: error.to_string(),
                                                            },
                                                    },
                                                ),
                                                Ok(rebound) => {
                                                    match crate::plasm_compile::compile_program(
                                                        host.engine.prompt_pipeline(),
                                                        None,
                                                        session,
                                                        "archive_compatibility",
                                                        &rebound.source,
                                                    )
                                                    .await
                                                    {
                                                        Ok(_) => PreparedAdoption::Example {
                                                            candidate: Box::new(candidate.clone()),
                                                            seal: Box::new(seal.clone()),
                                                        },
                                                        Err(error) => PreparedAdoption::Hold(
                                                            compilation_disposition(error),
                                                        ),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ArchiveRecord::Guidance {
                        association,
                        support,
                        ..
                    } => {
                        let seal = support
                            .iter()
                            .filter_map(|reference| seals.get(&reference.key()))
                            .find(|seal| association.validate(seal).is_ok());
                        match seal {
                            None => {
                                PreparedAdoption::Hold(ImportDisposition::AwaitingCompatibility {
                                    reason:
                                        ArchiveCompatibilityIssue::SupportingEvidenceUnavailable,
                                })
                            }
                            Some(seal) => {
                                let requirements = &association.requirements;
                                let authorized = requirements
                                    .required_capabilities()
                                    .iter()
                                    .all(|cap| allowed.permits(cap));
                                let contracts = requirements.catalog_fingerprints().iter().all(
                                    |(entry, hash)| {
                                        allowed.catalogs.contains(entry)
                                            && session.contexts_by_entry.get(entry).is_some_and(
                                                |ctx| ctx.cgs.catalog_cgs_hash_hex() == *hash,
                                            )
                                    },
                                );
                                let deployments = seal
                                    .executions
                                    .iter()
                                    .filter(|receipt| {
                                        receipt.catalogue_requirements.available()
                                            == Some(requirements)
                                    })
                                    .all(|receipt| {
                                        receipt
                                            .symbol_context
                                            .captured()
                                            .and_then(|captured| {
                                                captured.restore_symbol_authority(session).ok()
                                            })
                                            .is_some_and(|original| {
                                                original.prerequisite_deployments
                                                    == session.prerequisite_deployments
                                            })
                                    });
                                if authorized && contracts && deployments {
                                    PreparedAdoption::Guidance {
                                        association: Box::new(association.clone()),
                                        seal: Box::new(seal.clone()),
                                    }
                                } else {
                                    PreparedAdoption::Hold(
                                        ImportDisposition::AwaitingCompatibility {
                                            reason:
                                                ArchiveCompatibilityIssue::CatalogueContractsChanged,
                                        },
                                    )
                                }
                            }
                        }
                    }
                    _ => PreparedAdoption::Hold(ImportDisposition::RetainedObservation),
                }
            };
            prepared.push((item, product));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO workflow_learning_tenants(tenant) VALUES($1) ON CONFLICT DO NOTHING",
        )
        .bind(&target.tenant)
        .execute(&mut tx.executor())
        .await?;
        sqlx::query("SELECT tenant FROM workflow_learning_tenants WHERE tenant=$1 FOR UPDATE")
            .bind(&target.tenant)
            .fetch_one(&mut tx.executor())
            .await?;
        let value = serde_json::to_value(document)?;
        let previous:serde_json::Value=sqlx::query_scalar("INSERT INTO workflow_learning_archives(tenant,project,digest,document) VALUES($1,$2,$3,$4) ON CONFLICT(tenant,project,digest) DO UPDATE SET document=workflow_learning_archives.document RETURNING document").bind(&target.tenant).bind(&target.project).bind(document.digest.as_str()).bind(&value).fetch_one(&mut tx.executor()).await?;
        if previous != value {
            return Err(RepositoryError::ImmutableConflict.into());
        }
        let mut report = Vec::new();
        for (item, product) in prepared {
            let conflict:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_learning_archive_items WHERE tenant=$1 AND project=$2 AND source_scope=$3 AND identity=$4 AND item_digest<>$5)").bind(&target.tenant).bind(&target.project).bind(serde_json::to_value(&document.manifest.source_scope)?).bind(&item.identity).bind(item.digest.as_str()).fetch_one(&mut tx.executor()).await?;
            if conflict {
                return Err(RepositoryError::ImmutableConflict.into());
            }
            let old:Option<serde_json::Value>=sqlx::query_scalar("SELECT disposition FROM workflow_learning_archive_items WHERE tenant=$1 AND project=$2 AND archive_digest=$3 AND item_digest=$4").bind(&target.tenant).bind(&target.project).bind(document.digest.as_str()).bind(item.digest.as_str()).fetch_optional(&mut tx.executor()).await?;
            let disposition = product.disposition();
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
                    identity: item.identity.clone(),
                    digest: item.digest.clone(),
                    disposition: ImportDisposition::Duplicate,
                });
                continue;
            }
            sqlx::query("INSERT INTO workflow_learning_archive_items(tenant,project,archive_digest,item_digest,identity,source_scope,record,disposition) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(tenant,project,archive_digest,item_digest) DO UPDATE SET disposition=EXCLUDED.disposition")
                .bind(&target.tenant).bind(&target.project).bind(document.digest.as_str()).bind(item.digest.as_str()).bind(&item.identity).bind(serde_json::to_value(&document.manifest.source_scope)?).bind(serde_json::to_value(&item.record)?).bind(serde_json::to_value(&disposition)?).execute(&mut tx.executor()).await?;
            let support = PublicationSupport::Archive {
                scope: target.clone(),
                archive: document.digest.clone(),
                item: item.digest.clone(),
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
                identity: item.identity.clone(),
                digest: item.digest.clone(),
                disposition,
            });
        }
        tx.commit().await?;
        Ok(ArchiveImportReport {
            archive_digest: document.digest.clone(),
            target_scope: target.clone(),
            items: report,
        })
    }
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

async fn imported_support(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    scope: &LibraryScope,
    id: Uuid,
    guidance: bool,
) -> std::result::Result<Vec<EvidenceReference>, ArchiveError> {
    let query = if guidance {
        "SELECT i.record FROM workflow_learning_archive_guidance_support s JOIN workflow_learning_archive_items i USING(tenant,project,archive_digest,item_digest) WHERE s.association_id=$1 AND s.tenant=$2 AND s.project=$3"
    } else {
        "SELECT i.record FROM workflow_learning_archive_example_support s JOIN workflow_learning_archive_items i USING(tenant,project,archive_digest,item_digest) WHERE s.example_id=$1 AND s.tenant=$2 AND s.project=$3"
    };
    let values: Vec<serde_json::Value> = sqlx::query_scalar(query)
        .bind(id)
        .bind(&scope.tenant)
        .bind(&scope.project)
        .fetch_all(&mut tx.executor())
        .await?;
    let mut references = Vec::new();
    for value in values {
        match serde_json::from_value(value)? {
            ArchiveRecord::Example { support, .. } | ArchiveRecord::Guidance { support, .. } => {
                references.extend(support)
            }
            _ => return Err(ArchiveError::Reference),
        }
    }
    Ok(references)
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

async fn inherited_attribution(
    tx: &mut sqlx_tracing::Transaction<'_, sqlx::Postgres>,
    target: &LibraryScope,
    reference: &EvidenceReference,
    collection: &CollectionIdentity,
) -> std::result::Result<bool, ArchiveError> {
    // Select a qualifying proof, not an arbitrary prefix of wrappers retaining
    // this review: incomplete earlier imports must not hide a complete one.
    let found = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workflow_learning_archive_items i \
         JOIN workflow_learning_archives a ON a.tenant=i.tenant AND a.project=i.project AND a.digest=i.archive_digest \
         WHERE i.tenant=$1 AND i.project=$2 AND i.source_scope=$3 \
         AND i.record->>'kind'='review' AND i.record->>'review_id'=$4 AND i.record->>'seal_digest'=$5 \
         AND a.document#>>'{manifest,task_provenance,status}'='complete' \
         AND a.document#>>'{manifest,collection,protocol_digest}'=$6 \
         AND $7::jsonb @> (a.document#>'{manifest,collection,tasks}'))",
    )
    .bind(&target.tenant)
    .bind(&target.project)
    .bind(serde_json::to_value(&reference.source_scope)?)
    .bind(reference.review_id.to_string())
    .bind(reference.seal_digest.as_str())
    .bind(collection.protocol_digest.as_str())
    .bind(serde_json::to_value(&collection.tasks)?)
    .fetch_one(&mut tx.executor())
    .await?;
    Ok(found)
}
