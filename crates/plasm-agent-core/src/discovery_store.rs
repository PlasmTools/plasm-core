//! PostgreSQL publication and retrieval. No acquisition capabilities execute here.

use plasm_core::catalog_discovery::{
    content_hash, validate_embedding, CapabilityDocument, CatalogDiscoveryArtifact,
    EmbeddingProfile,
};
use plasm_core::catalog_il::{
    load_catalog_artifact, load_discovery_artifact, read_catalog_manifest, CatalogManifest,
};
use plasm_core::prerequisites::{
    prerequisite_closure, CapabilityRef, DeploymentBindings, PrerequisiteError,
};
use plasm_core::CGS;
use serde::{Deserialize, Serialize};
use sqlx::{Connection, Row};

mod database;
mod search_repository;
pub(crate) use database::DecisionLease;
use database::DiscoveryDatabase;
pub(crate) use search_repository::{BacklogKey, FrontierKey, SearchAdmission, SearchKey};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

const CHANNEL_LIMIT: i64 = 64;
pub const CANDIDATE_LIMIT: usize = 2 * CHANNEL_LIMIT as usize;

/// Completed judgments and retained declarations affect recall before either
/// bounded channel is truncated. They never change authorization or relevance.
#[derive(Default, Serialize)]
pub struct RecallFrontier(BTreeMap<String, BTreeMap<String, RecallVisit>>);

#[derive(Default, Serialize)]
struct RecallVisit {
    exposed: bool,
    visits: usize,
}

impl RecallFrontier {
    pub fn from_state(exposed: &[CapabilityRef], visits: &BTreeMap<CapabilityRef, usize>) -> Self {
        let mut frontier = Self::default();
        for (reference, count) in visits {
            frontier
                .0
                .entry(reference.catalog.clone())
                .or_default()
                .entry(reference.capability.clone())
                .or_default()
                .visits = *count;
        }
        for reference in exposed {
            frontier
                .0
                .entry(reference.catalog.clone())
                .or_default()
                .entry(reference.capability.clone())
                .or_default()
                .exposed = true;
        }
        frontier
    }
}

type Result<T> = std::result::Result<T, DiscoveryStoreError>;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryStoreError {
    #[error("discovery database admission is closed")]
    Admission(#[source] tokio::sync::AcquireError),
    #[error(transparent)]
    Template(#[from] plasm_compile::CatalogTemplateError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    CatalogIl(#[from] Box<plasm_core::catalog_il::CatalogIlError>),
    #[error(transparent)]
    CompiledCatalog(#[from] plasm_compile::CmlError),
    #[error(transparent)]
    Prerequisite(#[from] PrerequisiteError),
    #[error(transparent)]
    RetrievalValidation(#[from] RetrievalValidationError),
    #[error(transparent)]
    Embedding(#[from] plasm_catalog::embeddings::EmbeddingAcquisitionError),
    #[error("manifest path has no containing directory")]
    ManifestDirectoryMissing,
    #[error("intent provenance rewrites or omits pinned ancestry")]
    ProvenanceRewritten,
    #[error("session has no intent provenance to extend")]
    MissingPriorProvenance,
    #[error("the configured database does not provide pgvector")]
    PgvectorUnavailable(#[source] sqlx::Error),
    #[error("deployment identifier must not be empty")]
    EmptyDeploymentId,
    #[error("a discovery generation requires at least one catalog")]
    EmptyGeneration,
    #[error("discovery generation contains a duplicate catalog")]
    DuplicateCatalog,
    #[error("deployment binding refers to a missing consumer catalog")]
    BindingConsumerCatalogMissing,
    #[error("deployment binding refers to an undeclared requirement")]
    UndeclaredRequirement,
    #[error("compiled request recipe digest differs from the catalog manifest")]
    RecipeDigestMismatch,
    #[error("catalog revision exists without compiled request recipes")]
    RevisionRecipesMissing,
    #[error("catalog revision exists with different compiled request recipes")]
    RevisionRecipesConflict,
    #[error("compiled request recipe digest is invalid for a stored catalog")]
    StoredRecipeDigestMismatch,
    #[error("discovery generation is unknown")]
    UnknownGeneration,
    #[error("the verified discovery catalogue cache is poisoned")]
    CatalogueCachePoisoned,
    #[error("session is pinned to a different registry generation")]
    PinGenerationConflict,
    #[error("deployment has no active discovery generation")]
    NoActiveGeneration,
    #[error("discovery session pin is missing or expired")]
    PinMissingOrExpired,
    #[error("discovery session pin is missing, expired, or belongs to another generation")]
    PinRefreshRejected,
    #[error("discovery intent must not be empty")]
    EmptyIntent,
    #[error("embedding provider returned no vector for the requested intent")]
    MissingIntentEmbedding,
    #[error("embedding vector does not satisfy the discovery profile")]
    InvalidEmbedding(#[source] Box<plasm_core::catalog_discovery::CatalogDiscoveryError>),
    #[error("catalog artifact map does not contain the requested entry")]
    MissingCatalogEntry,
}

impl From<plasm_core::catalog_il::CatalogIlError> for DiscoveryStoreError {
    fn from(error: plasm_core::catalog_il::CatalogIlError) -> Self {
        Self::CatalogIl(Box::new(error))
    }
}

impl From<plasm_core::catalog_discovery::CatalogDiscoveryError> for DiscoveryStoreError {
    fn from(error: plasm_core::catalog_discovery::CatalogDiscoveryError) -> Self {
        Self::InvalidEmbedding(Box::new(error))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RetrievalValidationError {
    #[error("retrieval receipts refer to different catalogue generations")]
    GenerationMismatch,
    #[error("retrieval channel exceeds its admission bound")]
    ChannelBoundExceeded,
    #[error("retrieval channel contains a duplicate candidate identity")]
    DuplicateChannelIdentity,
    #[error("retrieval channels disagree on candidate identity or document")]
    ChannelDocumentMismatch,
    #[error("retrieval candidate bound exceeded")]
    CandidateBoundExceeded,
    #[error("retrieval candidate is not authorized")]
    UnauthorizedCandidate,
    #[error("retrieval contains a duplicate candidate id or capability reference")]
    DuplicateCandidate,
    #[error("retrieval document capability differs from its reference")]
    DocumentIdentityMismatch,
    #[error("retrieval document text is empty")]
    EmptyDocument,
    #[error("retrieval document digest does not match its text")]
    DocumentDigestMismatch,
}

/// Server-derived policy applied inside both retrieval channels and prerequisite closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryAuthorization {
    pub catalogs: BTreeSet<String>,
    /// Missing entry means all capabilities in an allowed catalog; an empty set allows none.
    pub capabilities: BTreeMap<String, BTreeSet<String>>,
}

impl DiscoveryAuthorization {
    pub fn catalogs(catalogs: BTreeSet<String>) -> Self {
        Self {
            catalogs,
            capabilities: BTreeMap::new(),
        }
    }
    /// Narrow a session policy with current caller restrictions. Never widen a pinned policy.
    pub fn intersection(&self, other: &Self) -> Self {
        let catalogs = self
            .catalogs
            .intersection(&other.catalogs)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut capabilities = BTreeMap::new();
        for catalog in &catalogs {
            let allowed = match (
                self.capabilities.get(catalog),
                other.capabilities.get(catalog),
            ) {
                (Some(left), Some(right)) => Some(left.intersection(right).cloned().collect()),
                (Some(allowed), None) | (None, Some(allowed)) => Some(allowed.clone()),
                (None, None) => None,
            };
            if let Some(allowed) = allowed {
                capabilities.insert(catalog.clone(), allowed);
            }
        }
        Self {
            catalogs,
            capabilities,
        }
    }
    pub fn permits(&self, reference: &CapabilityRef) -> bool {
        self.catalogs.contains(&reference.catalog)
            && self
                .capabilities
                .get(&reference.catalog)
                .is_none_or(|allowed| allowed.contains(&reference.capability))
    }
}

pub struct PreparedCatalog {
    manifest: CatalogManifest,
    cgs: CGS,
    compiled: plasm_compile::CompiledCatalog,
    discovery: CatalogDiscoveryArtifact,
    revision: String,
}

impl PreparedCatalog {
    pub fn load(manifest_path: &Path) -> Result<Self> {
        let directory = manifest_path
            .parent()
            .ok_or(DiscoveryStoreError::ManifestDirectoryMissing)?;
        let manifest = read_catalog_manifest(manifest_path)?;
        let cgs = load_catalog_artifact(directory, &manifest)?;
        let compiled = plasm_compile::load_compiled_catalog_artifact(directory, &manifest, &cgs)?;
        let discovery = load_discovery_artifact(directory, &manifest, &cgs)?;
        let revision = content_hash(&serde_json::to_vec(&manifest)?);
        Ok(Self {
            manifest,
            cgs,
            compiled,
            discovery,
            revision,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySessionPin {
    pub authorization: DiscoveryAuthorization,
    pub generation: String,
    pub pin_id: String,
}

/// Store clones share one search index pinned to its catalogue generation.
type SharedCatalogueSearchCache = Arc<
    std::sync::Mutex<
        Option<(
            String,
            Arc<crate::discovery_matcher::catalogue_search::CatalogueSearch>,
        )>,
    >,
>;

struct VerifiedCatalogues {
    generation: String,
    fingerprints: Vec<(String, String, String)>,
    catalogues: Arc<BTreeMap<String, CGS>>,
}

#[derive(Clone)]
pub struct DiscoveryStore {
    pool: DiscoveryDatabase,
    // Store clones (including short-lived Node route services) share cache fills.
    pub(crate) pending_decisions: std::sync::Arc<crate::discovery_scheduling::DecisionLocks>,
    pub(crate) provider_budgets: std::sync::Arc<crate::discovery_scheduling::SessionBudgets>,
    pub(crate) search_index: SharedCatalogueSearchCache,
    verified_catalogues: Arc<std::sync::Mutex<Option<VerifiedCatalogues>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedCapability {
    pub id: String,
    pub reference: CapabilityRef,
    pub document: CapabilityDocument,
    pub admissions: BTreeSet<String>,
}

enum RecallChannel {
    Lexical,
    Vector,
}

/// Schedule distinct authored evidence before repeated copies at the same frontier
/// depth. This is not semantic equivalence: every identity remains eligible and
/// receives its own downstream validation and judgment. Conditions and typed roles
/// remain in the exact document key; neither entity nor vocabulary is a priority.
fn recall_query(channel: RecallChannel) -> String {
    let (prefix, join, predicate, score) = match channel {
        RecallChannel::Lexical => (
            "WITH terms AS (SELECT unnest(tsvector_to_array(to_tsvector('english',$3))) AS term), q AS (SELECT to_tsquery('english',string_agg(quote_literal(term),' | ')) AS query FROM terms),",
            "CROSS JOIN q",
            "AND c.search @@ q.query",
            "ts_rank_cd(c.search,q.query)",
        ),
        RecallChannel::Vector => ("WITH", "", "", "-(c.embedding <=> $3::text::vector)"),
    };
    format!(
        "{prefix} scored AS (
            SELECT c.*,g.entry_id,{score} AS score,
                COALESCE(($6::jsonb -> g.entry_id -> c.capability ->> 'exposed')::boolean,false) AS exposed,
                COALESCE(($6::jsonb -> g.entry_id -> c.capability ->> 'visits')::numeric,0) AS visits
            FROM discovery_capabilities c
            JOIN discovery_generation_catalogs g USING (revision_id) {join}
            WHERE g.generation_id=$1 AND g.entry_id=ANY($2)
              AND (NOT ($5::jsonb ? g.entry_id) OR ($5::jsonb -> g.entry_id) ? c.capability)
              {predicate}
        ), scheduled AS (
            SELECT *,row_number() OVER (
                PARTITION BY entry_id,document - 'capability',exposed,visits
                ORDER BY score DESC,revision_id,capability
            ) AS evidence_rank FROM scored
        )
        SELECT * FROM scheduled
        ORDER BY exposed,visits,evidence_rank,score DESC,revision_id,capability LIMIT $4"
    )
}

/// Channel cutoffs are the only admission cutoffs. Interleave ranked channels
/// before bounded allocation; overlap merges provenance without another quota.
fn union_candidates(
    lexical: Vec<RetrievedCapability>,
    vector: Vec<RetrievedCapability>,
) -> std::result::Result<Vec<RetrievedCapability>, RetrievalValidationError> {
    if lexical.len() > CHANNEL_LIMIT as usize || vector.len() > CHANNEL_LIMIT as usize {
        return Err(RetrievalValidationError::ChannelBoundExceeded);
    }
    let order: Vec<_> = (0..lexical.len().max(vector.len()))
        .flat_map(|rank| {
            [&lexical, &vector]
                .into_iter()
                .filter_map(move |channel| channel.get(rank))
        })
        .map(|candidate| candidate.id.clone())
        .collect();
    let mut union: BTreeMap<String, RetrievedCapability> = BTreeMap::new();
    for (channel, candidates) in [("lexical", lexical), ("vector", vector)] {
        let mut seen = BTreeSet::new();
        for mut candidate in candidates {
            if !seen.insert(candidate.id.clone()) {
                return Err(RetrievalValidationError::DuplicateChannelIdentity);
            }
            candidate.admissions = BTreeSet::from([channel.into()]);
            if let Some(existing) = union.get_mut(&candidate.id) {
                if existing.reference != candidate.reference
                    || existing.document != candidate.document
                {
                    return Err(RetrievalValidationError::ChannelDocumentMismatch);
                }
                existing.admissions.insert(channel.into());
            } else {
                union.insert(candidate.id.clone(), candidate);
            }
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|id| union.remove(&id))
        .collect())
}

impl RetrievalReceipt {
    /// Interleave the ranked request and operation recalls without allowing a
    /// broad request to consume the entire candidate budget. Identity and
    /// document agreement are checked before any result can enter the union.
    pub(crate) fn interleave(
        generation: &str,
        receipts: Vec<Self>,
        allowed: &DiscoveryAuthorization,
    ) -> std::result::Result<Self, RetrievalValidationError> {
        let mut sources = Vec::with_capacity(receipts.len());
        let mut lexical_truncated = false;
        let mut vector_truncated = false;
        for receipt in receipts {
            if receipt.generation != generation {
                return Err(RetrievalValidationError::GenerationMismatch);
            }
            receipt.validate(allowed)?;
            lexical_truncated |= receipt.lexical_truncated;
            vector_truncated |= receipt.vector_truncated;
            sources.push(receipt.candidates.into_iter());
        }
        let mut candidates: Vec<RetrievedCapability> = Vec::new();
        let mut seen = BTreeMap::new();
        let mut first_round = true;
        loop {
            let mut advanced = false;
            for (source_index, source) in sources.iter_mut().enumerate() {
                let quota = if first_round && source_index == 0 {
                    2
                } else {
                    1
                };
                for _ in 0..quota {
                    let Some(candidate) = source.next() else {
                        break;
                    };
                    advanced = true;
                    if let Some(&position) = seen.get(&candidate.id) {
                        let existing: &mut RetrievedCapability = &mut candidates[position];
                        if existing.reference != candidate.reference
                            || existing.document != candidate.document
                        {
                            return Err(RetrievalValidationError::ChannelDocumentMismatch);
                        }
                        existing.admissions.extend(candidate.admissions);
                    } else if candidates.len() < CANDIDATE_LIMIT {
                        seen.insert(candidate.id.clone(), candidates.len());
                        candidates.push(candidate);
                    } else {
                        lexical_truncated = true;
                        vector_truncated = true;
                    }
                }
            }
            if !advanced {
                break;
            }
            first_round = false;
        }
        let result = Self {
            generation: generation.into(),
            lexical_count: candidates
                .iter()
                .filter(|c| c.admissions.contains("lexical"))
                .count(),
            vector_count: candidates
                .iter()
                .filter(|c| c.admissions.contains("vector"))
                .count(),
            candidates,
            lexical_truncated,
            vector_truncated,
        };
        result.validate(allowed)?;
        Ok(result)
    }

    /// Validate the admission boundary independently of downstream selection.
    pub fn validate(
        &self,
        allowed: &DiscoveryAuthorization,
    ) -> std::result::Result<(), RetrievalValidationError> {
        if self.candidates.len() > CANDIDATE_LIMIT {
            return Err(RetrievalValidationError::CandidateBoundExceeded);
        }
        let mut ids = BTreeSet::new();
        let mut references = BTreeSet::new();
        for candidate in &self.candidates {
            if !allowed.permits(&candidate.reference) {
                return Err(RetrievalValidationError::UnauthorizedCandidate);
            }
            if !ids.insert(&candidate.id) || !references.insert(&candidate.reference) {
                return Err(RetrievalValidationError::DuplicateCandidate);
            }
            if candidate.reference.capability != candidate.document.capability {
                return Err(RetrievalValidationError::DocumentIdentityMismatch);
            }
            if candidate.document.text.trim().is_empty() {
                return Err(RetrievalValidationError::EmptyDocument);
            }
            if content_hash(candidate.document.text.as_bytes()) != candidate.document.text_hash {
                return Err(RetrievalValidationError::DocumentDigestMismatch);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RetrievalReceipt {
    pub generation: String,
    /// Store retrieval returns the recall union; routing returns its judged
    /// projection plus typed graph nominations. Counts describe this candidate
    /// set, while truncation flags retain the upstream channel-cutoff evidence.
    pub candidates: Vec<RetrievedCapability>,
    pub lexical_count: usize,
    pub vector_count: usize,
    pub lexical_truncated: bool,
    pub vector_truncated: bool,
}

impl DiscoveryStore {
    pub async fn intent_provenance(
        &self,
        session: &str,
    ) -> Result<crate::intent_provenance::IntentProvenance> {
        let value: serde_json::Value = sqlx::query_scalar("SELECT p.provenance FROM discovery_intent_provenance p JOIN discovery_session_pins s USING (session_id) WHERE session_id=$1 AND s.expires_at > now()")
            .bind(session).fetch_one(&mut *self.pool.acquire().await?).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Serialize ancestry commits on the existing session pin. A stale or rewritten
    /// chain cannot become the context for an exposed capability surface.
    pub async fn commit_discovery_progress(
        &self,
        session: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        extending: bool,
    ) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        let mut tx = connection.begin().await?;
        sqlx::query("SELECT session_id FROM discovery_session_pins WHERE session_id=$1 AND expires_at > now() FOR UPDATE")
            .bind(session).fetch_one(&mut *tx).await?;
        let previous: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT provenance FROM discovery_intent_provenance WHERE session_id=$1",
        )
        .bind(session)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(previous) = previous {
            let previous = serde_json::from_value(previous)?;
            if !provenance.is_continuation_of(&previous) {
                return Err(DiscoveryStoreError::ProvenanceRewritten);
            }
        } else {
            if extending {
                return Err(DiscoveryStoreError::MissingPriorProvenance);
            }
        }
        sqlx::query("INSERT INTO discovery_intent_provenance (session_id,provenance) VALUES ($1,$2) ON CONFLICT (session_id) DO UPDATE SET provenance=EXCLUDED.provenance")
            .bind(session).bind(serde_json::to_value(provenance)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn connect(url: &str) -> Result<Self> {
        let pool = DiscoveryDatabase::connect(url).await?;
        Ok(Self {
            pool,
            pending_decisions: Default::default(),
            provider_budgets: Default::default(),
            search_index: Default::default(),
            verified_catalogues: Default::default(),
        })
    }

    /// Install the discovery schema explicitly; startup checks extension availability separately.
    pub async fn migrate(&self) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        let mut transaction = connection.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(734901182)")
            .execute(&mut *transaction)
            .await?;
        sqlx::Executor::execute(&mut *transaction, include_str!("discovery_store.sql")).await?;
        sqlx::Executor::execute(
            &mut *transaction,
            include_str!("discovery_requirement_outcomes.sql"),
        )
        .await?;
        sqlx::Executor::execute(&mut *transaction, include_str!("discovery_sufficiency.sql"))
            .await?;
        transaction.commit().await?;
        drop(connection);
        self.verify_extension().await
    }

    pub async fn verify_extension(&self) -> Result<()> {
        sqlx::query("SELECT '[1,0]'::vector <=> '[1,0]'::vector")
            .execute(&mut *self.pool.acquire().await?)
            .await
            .map_err(DiscoveryStoreError::PgvectorUnavailable)?;
        Ok(())
    }

    /// Validate a complete generation before writing; serialize publication and activate atomically.
    pub async fn import(
        &self,
        deployment: &str,
        mut catalogs: Vec<PreparedCatalog>,
        bindings: &DeploymentBindings,
    ) -> Result<String> {
        if deployment.trim().is_empty() {
            return Err(DiscoveryStoreError::EmptyDeploymentId);
        }
        if catalogs.is_empty() {
            return Err(DiscoveryStoreError::EmptyGeneration);
        }
        catalogs.sort_by(|a, b| a.manifest.entry_id.cmp(&b.manifest.entry_id));
        let mut refs = BTreeMap::new();
        for catalog in &catalogs {
            if refs
                .insert(catalog.manifest.entry_id.clone(), &catalog.cgs)
                .is_some()
            {
                return Err(DiscoveryStoreError::DuplicateCatalog);
            }
            catalog
                .discovery
                .validate(&catalog.cgs)
                .map_err(DiscoveryStoreError::from)?;
        }
        let allowed = refs.keys().cloned().collect();
        let capabilities: Vec<_> = refs
            .iter()
            .flat_map(|(catalog, cgs)| {
                cgs.capabilities.keys().map(|capability| CapabilityRef {
                    catalog: catalog.clone(),
                    capability: capability.to_string(),
                })
            })
            .collect();
        prerequisite_closure(&refs, bindings, &capabilities, &allowed)?;
        for binding in &bindings.bindings {
            let cgs = refs
                .get(&binding.consumer.catalog)
                .ok_or(DiscoveryStoreError::BindingConsumerCatalogMissing)?;
            if !cgs
                .prerequisites
                .requirements
                .get(&binding.consumer.capability)
                .is_some_and(|rs| rs.iter().any(|r| r.id == binding.requirement))
            {
                return Err(DiscoveryStoreError::UndeclaredRequirement);
            }
        }
        let mut canonical_bindings = bindings.clone();
        canonical_bindings
            .bindings
            .sort_by(|a, b| (&a.consumer, &a.requirement).cmp(&(&b.consumer, &b.requirement)));
        let generation = content_hash(&serde_json::to_vec(&(
            catalogs.iter().map(|c| &c.revision).collect::<Vec<_>>(),
            &canonical_bindings,
        ))?);
        let mut connection = self.pool.acquire().await?;
        let mut tx = connection.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(734901183)")
            .execute(&mut *tx)
            .await?;
        for catalog in &catalogs {
            let recipe_bytes = serde_json::to_vec(&catalog.compiled)?;
            if content_hash(&recipe_bytes) != catalog.manifest.recipes_hash {
                return Err(DiscoveryStoreError::RecipeDigestMismatch);
            }
            let inserted = sqlx::query("INSERT INTO discovery_revisions (revision_id,entry_id,manifest,cgs,profile) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
                .bind(&catalog.revision).bind(&catalog.manifest.entry_id).bind(serde_json::to_value(&catalog.manifest)?).bind(serde_json::to_vec(&catalog.cgs)?).bind(serde_json::to_value(&catalog.discovery.profile)?)
                .execute(&mut *tx).await?.rows_affected();
            if inserted == 0 {
                let stored_recipes: Option<Vec<u8>> = sqlx::query_scalar(
                    "SELECT recipes FROM discovery_compiled_recipes WHERE revision_id=$1",
                )
                .bind(&catalog.revision)
                .fetch_optional(&mut *tx)
                .await?;
                let Some(stored_recipes) = stored_recipes else {
                    return Err(DiscoveryStoreError::RevisionRecipesMissing);
                };
                if stored_recipes != recipe_bytes {
                    return Err(DiscoveryStoreError::RevisionRecipesConflict);
                }
                continue;
            }
            sqlx::query(
                "INSERT INTO discovery_compiled_recipes (revision_id,recipes) VALUES ($1,$2)",
            )
            .bind(&catalog.revision)
            .bind(recipe_bytes)
            .execute(&mut *tx)
            .await?;
            for capability in &catalog.discovery.capabilities {
                sqlx::query("INSERT INTO discovery_capabilities (revision_id,capability,entity,document,search,embedding) VALUES ($1,$2,$3,$4,to_tsvector('english',$5),$6::text::vector)")
                    .bind(&catalog.revision).bind(&capability.document.capability).bind(&capability.document.entity).bind(serde_json::to_value(&capability.document)?).bind(&capability.document.text).bind(vector_literal(&capability.embedding)?)
                    .execute(&mut *tx).await?;
            }
        }
        sqlx::query("INSERT INTO discovery_generations (generation_id,bindings) VALUES ($1,$2) ON CONFLICT DO NOTHING").bind(&generation).bind(serde_json::to_value(&canonical_bindings)?).execute(&mut *tx).await?;
        for catalog in &catalogs {
            sqlx::query("INSERT INTO discovery_generation_catalogs VALUES ($1,$2,$3) ON CONFLICT DO NOTHING").bind(&generation).bind(&catalog.manifest.entry_id).bind(&catalog.revision).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO discovery_active (deployment_id,generation_id) VALUES ($1,$2) ON CONFLICT (deployment_id) DO UPDATE SET generation_id=EXCLUDED.generation_id").bind(deployment).bind(&generation).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(generation)
    }

    /// Collect only expired, inactive generations; live pins are protected by foreign keys.
    pub async fn collect_retired(&self, before: chrono::DateTime<chrono::Utc>) -> Result<u64> {
        let mut connection = self.pool.acquire().await?;
        let mut tx = connection.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(734901183)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM discovery_session_pins WHERE expires_at <= now()")
            .execute(&mut *tx)
            .await?;
        let removed = sqlx::query("DELETE FROM discovery_generations g WHERE g.created_at < $1 AND NOT EXISTS (SELECT 1 FROM discovery_active a WHERE a.generation_id=g.generation_id) AND NOT EXISTS (SELECT 1 FROM discovery_session_pins p WHERE p.generation_id=g.generation_id)")
            .bind(before).execute(&mut *tx).await?.rows_affected();
        sqlx::query("DELETE FROM discovery_capabilities c WHERE NOT EXISTS (SELECT 1 FROM discovery_generation_catalogs g WHERE g.revision_id=c.revision_id)").execute(&mut *tx).await?;
        sqlx::query("DELETE FROM discovery_revisions r WHERE NOT EXISTS (SELECT 1 FROM discovery_generation_catalogs g WHERE g.revision_id=r.revision_id)").execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(removed)
    }

    pub async fn load_generation(
        &self,
        generation: &str,
    ) -> Result<(
        BTreeMap<String, CGS>,
        BTreeMap<String, Arc<plasm_compile::CompiledCatalog>>,
        DeploymentBindings,
    )> {
        let bindings = self.generation_bindings(generation).await?;
        let rows = sqlx::query("SELECT g.entry_id,r.cgs,r.manifest,p.recipes FROM discovery_generation_catalogs g JOIN discovery_revisions r USING(revision_id) JOIN discovery_compiled_recipes p USING(revision_id) WHERE g.generation_id=$1 ORDER BY g.entry_id")
            .bind(generation).fetch_all(&mut *self.pool.acquire().await?).await?;
        let mut catalogs = BTreeMap::new();
        let mut compiled_catalogs = BTreeMap::new();
        for row in rows {
            let bytes: Vec<u8> = row.try_get("cgs")?;
            let manifest: CatalogManifest = serde_json::from_value(row.try_get("manifest")?)?;
            let cgs = plasm_core::catalog_il::load_catalog_il_verified(&bytes, &manifest.cgs_hash)?;
            let entry_id: String = row.try_get("entry_id")?;
            let recipe_bytes: Vec<u8> = row.try_get("recipes")?;
            if content_hash(&recipe_bytes) != manifest.recipes_hash {
                return Err(DiscoveryStoreError::StoredRecipeDigestMismatch);
            }
            let compiled = plasm_compile::CompiledCatalog::decode_artifact(&recipe_bytes, &cgs)?;
            catalogs.insert(entry_id.clone(), cgs);
            compiled_catalogs.insert(entry_id, Arc::new(compiled));
        }
        Ok((catalogs, compiled_catalogs, bindings))
    }

    async fn generation_bindings(&self, generation: &str) -> Result<DeploymentBindings> {
        let bindings: serde_json::Value =
            sqlx::query_scalar("SELECT bindings FROM discovery_generations WHERE generation_id=$1")
                .bind(generation)
                .fetch_optional(&mut *self.pool.acquire().await?)
                .await?
                .ok_or(DiscoveryStoreError::UnknownGeneration)?;
        Ok(serde_json::from_value(bindings)?)
    }

    /// Discovery consumes semantic CGS only. Compiled IO recipes are loaded and
    /// verified by the execution path, not decoded on every relevance request.
    pub async fn load_discovery_generation(
        &self,
        generation: &str,
        allowed: &DiscoveryAuthorization,
    ) -> Result<(Arc<BTreeMap<String, CGS>>, DeploymentBindings)> {
        let bindings = self.generation_bindings(generation).await?;
        let allowed: Vec<_> = allowed.catalogs.iter().collect();
        let rows = sqlx::query("SELECT g.entry_id,r.cgs,r.manifest FROM discovery_generation_catalogs g JOIN discovery_revisions r USING(revision_id) WHERE g.generation_id=$1 AND g.entry_id=ANY($2) ORDER BY g.entry_id")
            .bind(generation).bind(allowed).fetch_all(&mut *self.pool.acquire().await?).await?;
        let catalog_rows = rows
            .into_iter()
            .map(|row| {
                let bytes: Vec<u8> = row.try_get("cgs")?;
                let manifest: CatalogManifest = serde_json::from_value(row.try_get("manifest")?)?;
                let name: String = row.try_get("entry_id")?;
                Ok((name, bytes, manifest.cgs_hash))
            })
            .collect::<Result<Vec<_>>>()?;
        // Re-read the database and fingerprint its exact bytes on every request:
        // a changed catalogue or manifest must still be re-verified, never hidden
        // by the cache. Only decoded, verified immutable graphs are shared.
        let fingerprints: Vec<_> = catalog_rows
            .iter()
            .map(|(name, bytes, hash)| (name.clone(), content_hash(bytes), hash.clone()))
            .collect();
        let cached = self
            .verified_catalogues
            .lock()
            .map_err(|_| DiscoveryStoreError::CatalogueCachePoisoned)?
            .as_ref()
            .filter(|entry| entry.generation == generation && entry.fingerprints == fingerprints)
            .map(|entry| Arc::clone(&entry.catalogues));
        let catalogs = if let Some(cached) = cached {
            cached
        } else {
            let catalogs = Arc::new(
                catalog_rows
                    .into_iter()
                    .map(|(name, bytes, hash)| {
                        let cgs = plasm_core::catalog_il::load_catalog_il_verified(&bytes, &hash)?;
                        Ok((name, cgs))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?,
            );
            *self
                .verified_catalogues
                .lock()
                .map_err(|_| DiscoveryStoreError::CatalogueCachePoisoned)? =
                Some(VerifiedCatalogues {
                    generation: generation.to_owned(),
                    fingerprints,
                    catalogues: Arc::clone(&catalogs),
                });
            catalogs
        };
        Ok((catalogs, bindings))
    }

    pub async fn pin_generation(
        &self,
        session: &str,
        generation: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let actual: Option<String> = sqlx::query_scalar("INSERT INTO discovery_session_pins (session_id,generation_id,expires_at) VALUES ($1,$2,$3) ON CONFLICT (session_id) DO UPDATE SET expires_at=GREATEST(discovery_session_pins.expires_at,EXCLUDED.expires_at) WHERE discovery_session_pins.generation_id=EXCLUDED.generation_id RETURNING generation_id")
            .bind(session).bind(generation).bind(expires_at).fetch_optional(&mut *self.pool.acquire().await?).await?;
        if actual.is_none() {
            return Err(DiscoveryStoreError::PinGenerationConflict);
        }
        Ok(())
    }

    pub async fn pin(
        &self,
        deployment: &str,
        session: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<String> {
        let generation: Option<String> = sqlx::query_scalar("INSERT INTO discovery_session_pins (session_id,generation_id,expires_at) SELECT $1,generation_id,$2 FROM discovery_active WHERE deployment_id=$3 ON CONFLICT (session_id) DO UPDATE SET expires_at=GREATEST(discovery_session_pins.expires_at,EXCLUDED.expires_at) RETURNING generation_id")
            .bind(session).bind(expires_at).bind(deployment).fetch_optional(&mut *self.pool.acquire().await?).await?;
        generation.ok_or(DiscoveryStoreError::NoActiveGeneration)
    }

    pub async fn pinned_generation(&self, session: &str) -> Result<String> {
        sqlx::query_scalar("SELECT generation_id FROM discovery_session_pins WHERE session_id=$1 AND expires_at > now()")
            .bind(session).fetch_optional(&mut *self.pool.acquire().await?).await?.ok_or(DiscoveryStoreError::PinMissingOrExpired)
    }

    pub async fn refresh_session_pin(&self, pin: &DiscoverySessionPin) -> Result<()> {
        self.renew_session_pin(pin, chrono::Utc::now() + chrono::Duration::hours(24))
            .await
    }

    pub async fn renew_session_pin(
        &self,
        pin: &DiscoverySessionPin,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let refreshed = sqlx::query("UPDATE discovery_session_pins SET expires_at=GREATEST(expires_at,$3) WHERE session_id=$1 AND generation_id=$2 AND expires_at > now()")
            .bind(&pin.pin_id)
            .bind(&pin.generation)
            .bind(expires_at)
            .execute(&mut *self.pool.acquire().await?)
            .await?;
        if refreshed.rows_affected() != 1 {
            return Err(DiscoveryStoreError::PinRefreshRejected);
        }
        Ok(())
    }

    pub async fn retrieve(
        &self,
        generation: &str,
        intent: &str,
        allowed: &DiscoveryAuthorization,
        frontier: &RecallFrontier,
    ) -> Result<RetrievalReceipt> {
        let mut receipts = self
            .retrieve_many(generation, &[intent], allowed, frontier)
            .await?;
        Ok(receipts.remove(0))
    }

    /// Embed uncached queries together and run their authorized searches in parallel.
    pub(crate) async fn retrieve_many(
        &self,
        generation: &str,
        intents: &[&str],
        allowed: &DiscoveryAuthorization,
        frontier: &RecallFrontier,
    ) -> Result<Vec<RetrievalReceipt>> {
        if intents.iter().any(|intent| intent.trim().is_empty()) {
            return Err(DiscoveryStoreError::EmptyIntent);
        }
        let profile = EmbeddingProfile::default();
        let profile_json = serde_json::to_value(&profile)?;
        let mut vectors = Vec::with_capacity(intents.len());
        let mut missing = Vec::new();
        for intent in intents {
            let key = content_hash(&serde_json::to_vec(&(intent, &profile))?);
            let cached: Option<String> = sqlx::query_scalar("SELECT embedding::text FROM discovery_intent_embeddings WHERE cache_key=$1 AND profile=$2")
                .bind(&key).bind(&profile_json).fetch_optional(&mut *self.pool.acquire().await?).await?;
            if cached.is_none() {
                missing.push((vectors.len(), key, (*intent).to_owned()));
            }
            vectors.push(cached);
        }
        if !missing.is_empty() {
            let directory = std::env::var_os("PLASM_DISCOVERY_REJECTION_DIR")
                .filter(|s| !s.is_empty())
                .map(Into::into);
            let mut receipts = plasm_runtime::provider_transport::ProviderReceiptSink::new(
                "query_embedding",
                directory,
            );
            let inputs: Vec<_> = missing
                .iter()
                .map(|(_, _, intent)| intent.clone())
                .collect();
            let embeddings = plasm_catalog::embeddings::EmbeddingClient::from_env()?
                .embed(&inputs, |event| receipts.record(event))
                .await?;
            if embeddings.len() != missing.len() {
                return Err(DiscoveryStoreError::MissingIntentEmbedding);
            }
            for ((position, key, _), embedding) in missing.into_iter().zip(embeddings) {
                let vector = vector_literal(&embedding)?;
                sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector) ON CONFLICT DO NOTHING")
                    .bind(&key).bind(&profile_json).bind(&vector).execute(&mut *self.pool.acquire().await?).await?;
                vectors[position] = Some(vector);
            }
        }
        let vectors = vectors
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(DiscoveryStoreError::MissingIntentEmbedding)?;
        futures::future::try_join_all(intents.iter().zip(vectors.iter()).map(|(intent, vector)| {
            self.retrieve_vector(generation, intent, vector, allowed, frontier)
        }))
        .await
    }

    /// Rank within the authorized generation. Typed graph expansion belongs to the routing layer.
    async fn retrieve_vector(
        &self,
        generation: &str,
        intent: &str,
        vector: &str,
        allowed: &DiscoveryAuthorization,
        frontier: &RecallFrontier,
    ) -> Result<RetrievalReceipt> {
        let restrictions = serde_json::to_value(&allowed.capabilities)?;
        let frontier = serde_json::to_value(frontier)?;
        let allowed: Vec<_> = allowed.catalogs.iter().cloned().collect();
        let mut lexical = sqlx::query(&recall_query(RecallChannel::Lexical))
            .bind(generation)
            .bind(&allowed)
            .bind(intent)
            .bind(CHANNEL_LIMIT + 1)
            .bind(&restrictions)
            .bind(&frontier)
            .fetch_all(&mut *self.pool.acquire().await?)
            .await?;
        let mut vector = sqlx::query(&recall_query(RecallChannel::Vector))
            .bind(generation)
            .bind(&allowed)
            .bind(vector)
            .bind(CHANNEL_LIMIT + 1)
            .bind(&restrictions)
            .bind(&frontier)
            .fetch_all(&mut *self.pool.acquire().await?)
            .await?;
        let lexical_truncated = lexical.len() > CHANNEL_LIMIT as usize;
        let vector_truncated = vector.len() > CHANNEL_LIMIT as usize;
        lexical.truncate(CHANNEL_LIMIT as usize);
        vector.truncate(CHANNEL_LIMIT as usize);
        let lexical_count = lexical.len();
        let vector_count = vector.len();
        let candidates = union_candidates(
            lexical
                .iter()
                .map(candidate_from_row)
                .collect::<Result<Vec<_>>>()?,
            vector
                .iter()
                .map(candidate_from_row)
                .collect::<Result<Vec<_>>>()?,
        )?;
        Ok(RetrievalReceipt {
            generation: generation.into(),
            candidates,
            lexical_count,
            vector_count,
            lexical_truncated,
            vector_truncated,
        })
    }
}

fn vector_literal(vector: &[f32]) -> Result<String> {
    validate_embedding(vector, 1536).map_err(DiscoveryStoreError::from)?;
    Ok(serde_json::to_string(vector)?)
}

fn candidate_from_row(row: &sqlx::postgres::PgRow) -> Result<RetrievedCapability> {
    let revision: String = row.try_get("revision_id")?;
    let capability: String = row.try_get("capability")?;
    Ok(RetrievedCapability {
        id: format!("{revision}/{capability}"),
        reference: CapabilityRef {
            catalog: row.try_get("entry_id")?,
            capability,
        },
        document: serde_json::from_value(row.try_get("document")?)?,
        admissions: BTreeSet::new(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    fn union_fixture(id: usize) -> RetrievedCapability {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let mut c: RetrievedCapability =
            serde_json::from_value(packet["routing"]["retrieval"]["candidates"][0].clone())
                .unwrap();
        c.id = format!("candidate{id:03}");
        c.reference.capability = format!("read{id}");
        c.document.capability = c.reference.capability.clone();
        c.document.text_hash = content_hash(c.document.text.as_bytes());
        c
    }

    #[test]
    fn operation_recall_interleaves_bounded_authorized_sources() {
        let broad: Vec<_> = (0..CANDIDATE_LIMIT).map(union_fixture).collect();
        let specific = union_fixture(CANDIDATE_LIMIT + 1);
        let allowed =
            DiscoveryAuthorization::catalogs(BTreeSet::from([specific.reference.catalog.clone()]));
        let receipt = |candidates| RetrievalReceipt {
            generation: "generation".into(),
            candidates,
            lexical_count: 0,
            vector_count: 0,
            lexical_truncated: false,
            vector_truncated: false,
        };
        let result = RetrievalReceipt::interleave(
            "generation",
            vec![receipt(broad), receipt(vec![specific.clone()])],
            &allowed,
        )
        .unwrap();
        assert_eq!(result.candidates.len(), CANDIDATE_LIMIT);
        assert_eq!(result.candidates[2].reference, specific.reference);
        assert!(result.lexical_truncated && result.vector_truncated);
        assert!(matches!(
            RetrievalReceipt::interleave("other", vec![receipt(vec![specific.clone()])], &allowed),
            Err(RetrievalValidationError::GenerationMismatch)
        ));
        let denied = DiscoveryAuthorization::catalogs(BTreeSet::new());
        assert!(matches!(
            RetrievalReceipt::interleave("generation", vec![receipt(vec![specific])], &denied),
            Err(RetrievalValidationError::UnauthorizedCandidate)
        ));
    }

    #[test]
    fn current_request_keeps_both_leading_recall_channels_ahead_of_fragments() {
        let current_lexical = union_fixture(0);
        let current_vector = union_fixture(1);
        let fragments: Vec<_> = (2..10).map(union_fixture).collect();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from([current_lexical
            .reference
            .catalog
            .clone()]));
        let receipt = |candidates| RetrievalReceipt {
            generation: "generation".into(),
            candidates,
            lexical_count: 0,
            vector_count: 0,
            lexical_truncated: false,
            vector_truncated: false,
        };
        let mut inputs = vec![receipt(vec![current_lexical, current_vector.clone()])];
        inputs.extend(
            fragments
                .into_iter()
                .map(|candidate| receipt(vec![candidate])),
        );
        let result = RetrievalReceipt::interleave("generation", inputs, &allowed).unwrap();
        assert_eq!(result.candidates[1].reference, current_vector.reference);
    }

    #[test]
    fn recall_frontier_preserves_catalog_identity_and_separates_exposure_from_visits() {
        let a = CapabilityRef {
            catalog: "a".into(),
            capability: "inspect".into(),
        };
        let b = CapabilityRef {
            catalog: "b".into(),
            capability: "inspect".into(),
        };
        let state = RecallFrontier::from_state(
            std::slice::from_ref(&a),
            &BTreeMap::from([(a.clone(), 2), (b, 1)]),
        );
        let json = serde_json::to_value(state).unwrap();
        assert_eq!(
            json["a"]["inspect"],
            serde_json::json!({"exposed":true,"visits":2})
        );
        assert_eq!(
            json["b"]["inspect"],
            serde_json::json!({"exposed":false,"visits":1})
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
    async fn postgres_recall_distinct_evidence_precedes_duplicate_alternatives() {
        let store = DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
            .await
            .unwrap();
        store.migrate().await.unwrap();
        let mut cgs = prepared("evidence", 1).cgs;
        let mut duplicate = cgs.capabilities["create"].clone();
        duplicate.description = "Quartz quartz quartz quartz".into();
        for i in 0..65 {
            let name = format!("alternative_{i:03}");
            let mut capability = duplicate.clone();
            capability.name = name.clone().into();
            cgs.capabilities.insert(name.into(), capability);
        }
        cgs.capabilities.get_mut("read").unwrap().description = "Quartz".into();
        duplicate.name = "conditional".into();
        duplicate.description = "Quartz under an explicit condition".into();
        cgs.capabilities.insert("conditional".into(), duplicate);
        let catalog = prepared_cgs("evidence", cgs);
        let expected_documents: BTreeMap<_, _> = catalog
            .discovery
            .capabilities
            .iter()
            .map(|c| (c.document.capability.clone(), c.document.clone()))
            .collect();
        let generation = store
            .import(
                "evidence-proof",
                vec![catalog],
                &DeploymentBindings {
                    bindings: vec![DeploymentBinding {
                        consumer: CapabilityRef {
                            catalog: "evidence".into(),
                            capability: "read".into(),
                        },
                        requirement: "scoped_access".into(),
                        provider_catalog: "evidence".into(),
                        provider: "value_source".into(),
                    }],
                },
            )
            .await
            .unwrap();
        let permitted: BTreeSet<String> = (1..65)
            .map(|i| format!("alternative_{i:03}"))
            .chain(["read".into(), "conditional".into()])
            .collect();
        let allowed = DiscoveryAuthorization {
            catalogs: BTreeSet::from(["evidence".into()]),
            capabilities: BTreeMap::from([("evidence".into(), permitted.clone())]),
        };
        let mut vector = vec![0.0; 1536];
        vector[0] = 1.0;
        let vector = vector_literal(&vector).unwrap();
        let first = store
            .retrieve_vector(
                &generation,
                "quartz",
                &vector,
                &allowed,
                &RecallFrontier::default(),
            )
            .await
            .unwrap();
        assert_eq!((first.lexical_count, first.vector_count), (64, 64));
        for target in ["read", "conditional"] {
            let position = first
                .candidates
                .iter()
                .position(|c| c.reference.capability == target);
            assert!(position.is_some_and(|p| p < 3),
                "distinct authored evidence {target} must precede repeated evidence, got {position:?}");
            assert_eq!(
                first.candidates[position.unwrap()].admissions,
                BTreeSet::from(["lexical".into(), "vector".into()])
            );
        }
        assert!(!first
            .candidates
            .iter()
            .any(|c| c.reference.capability == "alternative_000"));
        let visits = first
            .candidates
            .iter()
            .map(|c| (c.reference.clone(), 1))
            .collect();
        let next = store
            .retrieve_vector(
                &generation,
                "quartz",
                &vector,
                &allowed,
                &RecallFrontier::from_state(&[], &visits),
            )
            .await
            .unwrap();
        let seen: BTreeSet<_> = first
            .candidates
            .iter()
            .chain(&next.candidates)
            .map(|c| c.reference.capability.clone())
            .collect();
        assert_eq!(
            seen, permitted,
            "alternatives remain eligible; this is scheduling, not deduplication"
        );
        for c in first.candidates.iter().chain(&next.candidates) {
            assert_eq!(c.document, expected_documents[&c.reference.capability]);
            assert!(allowed.permits(&c.reference));
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
    async fn postgres_recall_frontier_advances_both_channels_without_skipping_deferred_candidates()
    {
        let store = DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
            .await
            .unwrap();
        store.migrate().await.unwrap();
        let names = (0..65)
            .map(|i| format!("frontier-{i:03}"))
            .chain(["frontier-target".into(), "frontier-forbidden".into()])
            .collect::<Vec<_>>();
        let catalogs = names.iter().map(|n| prepared(n, 1)).collect();
        let bindings = DeploymentBindings {
            bindings: names
                .iter()
                .map(|name| DeploymentBinding {
                    consumer: CapabilityRef {
                        catalog: name.clone(),
                        capability: "read".into(),
                    },
                    requirement: "scoped_access".into(),
                    provider_catalog: name.clone(),
                    provider: "value_source".into(),
                })
                .collect(),
        };
        let generation = store
            .import("recall-frontier-proof", catalogs, &bindings)
            .await
            .unwrap();
        let allowed = DiscoveryAuthorization {
            catalogs: names
                .iter()
                .filter(|n| n.as_str() != "frontier-forbidden")
                .cloned()
                .collect(),
            capabilities: names
                .iter()
                .map(|n| {
                    (
                        n.clone(),
                        BTreeSet::from([if n == "frontier-target" {
                            "read"
                        } else {
                            "create"
                        }
                        .to_owned()]),
                    )
                })
                .collect(),
        };
        // Synthetic indexed evidence isolates both channel cutoffs, without a model or task labels.
        sqlx::query("UPDATE discovery_capabilities c SET search=to_tsvector('english', CASE WHEN capability='read' THEN 'quartz' ELSE 'quartz quartz quartz quartz' END) FROM discovery_generation_catalogs g WHERE c.revision_id=g.revision_id AND g.generation_id=$1")
            .bind(&generation).execute(&mut *store.pool.acquire().await.unwrap()).await.unwrap();
        let mut vector = vec![0.0; 1536];
        vector[0] = 1.0;
        let vector = vector_literal(&vector).unwrap();
        let target = CapabilityRef {
            catalog: "frontier-target".into(),
            capability: "read".into(),
        };
        let first = store
            .retrieve_vector(
                &generation,
                "quartz",
                &vector,
                &allowed,
                &RecallFrontier::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            (
                first.lexical_count,
                first.vector_count,
                first.candidates.len()
            ),
            (64, 64, 64)
        );
        assert!(first.lexical_truncated && first.vector_truncated);
        assert!(!first.candidates.iter().any(|c| c.reference == target));
        let first_visits = BTreeMap::from([(first.candidates[0].reference.clone(), 1)]);
        let partial = store
            .retrieve_vector(
                &generation,
                "quartz",
                &vector,
                &allowed,
                &RecallFrontier::from_state(&[], &first_visits),
            )
            .await
            .unwrap();
        assert!(
            !partial.candidates.iter().any(|c| c.reference == target),
            "63 deferred higher-ranked candidates must remain unvisited, not be skipped as a page"
        );
        let visits = first
            .candidates
            .iter()
            .map(|c| (c.reference.clone(), 1))
            .collect();
        let next = store
            .retrieve_vector(
                &generation,
                "quartz",
                &vector,
                &allowed,
                &RecallFrontier::from_state(&[], &visits),
            )
            .await
            .unwrap();
        let found = next
            .candidates
            .iter()
            .find(|c| c.reference == target)
            .unwrap();
        assert_eq!(
            found.admissions,
            BTreeSet::from(["lexical".into(), "vector".into()])
        );
        assert!(
            next.candidates
                .iter()
                .position(|c| c.reference == target)
                .unwrap()
                < 2
        );
        let exposed = first
            .candidates
            .iter()
            .map(|c| c.reference.clone())
            .collect::<Vec<_>>();
        let retained = store
            .retrieve_vector(
                &generation,
                "quartz",
                &vector,
                &allowed,
                &RecallFrontier::from_state(&exposed, &BTreeMap::new()),
            )
            .await
            .unwrap();
        assert!(
            retained
                .candidates
                .iter()
                .position(|c| c.reference == target)
                .unwrap()
                < 2
        );
        for receipt in [&first, &partial, &next, &retained] {
            assert!(receipt.candidates.len() <= CANDIDATE_LIMIT);
            assert!(receipt
                .candidates
                .iter()
                .all(|c| allowed.permits(&c.reference)));
        }
        let replay = store
            .retrieve_vector(
                &generation,
                "quartz",
                &vector,
                &allowed,
                &RecallFrontier::from_state(&[], &visits),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&next).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
    }

    #[test]
    fn hybrid_union_preserves_channel_priority_before_bounded_allocation() {
        let lexical = vec![union_fixture(9), union_fixture(1)];
        let vector = vec![union_fixture(8), union_fixture(1), union_fixture(7)];
        let union = union_candidates(lexical.clone(), vector.clone()).unwrap();
        assert_eq!(
            union.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec![
                lexical[0].id.as_str(),
                vector[0].id.as_str(),
                lexical[1].id.as_str(),
                vector[2].id.as_str()
            ]
        );
        assert_eq!(
            union[2].admissions,
            BTreeSet::from(["lexical".into(), "vector".into()])
        );
    }

    #[test]
    fn retrieval_boundary_rejects_corruption_after_codec() {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let mut receipt: RetrievalReceipt =
            serde_json::from_value(packet["routing"]["retrieval"].clone()).unwrap();
        for candidate in &mut receipt.candidates {
            candidate.document.text_hash = content_hash(candidate.document.text.as_bytes());
        }
        let allowed = DiscoveryAuthorization::catalogs(
            receipt
                .candidates
                .iter()
                .map(|c| c.reference.catalog.clone())
                .collect(),
        );
        receipt.validate(&allowed).unwrap();
        let decoded: RetrievalReceipt =
            serde_json::from_slice(&serde_json::to_vec(&receipt).unwrap()).unwrap();
        decoded.validate(&allowed).unwrap();
        assert_eq!(
            decoded.validate(&DiscoveryAuthorization::catalogs(BTreeSet::new())),
            Err(RetrievalValidationError::UnauthorizedCandidate)
        );
        for defect in 0..5 {
            let mut corrupt: RetrievalReceipt =
                serde_json::from_slice(&serde_json::to_vec(&decoded).unwrap()).unwrap();
            match defect {
                0 => corrupt.candidates[0].document.text.push_str("altered"),
                1 => corrupt.candidates[0].document.capability.push_str("other"),
                2 => corrupt.candidates[0].document.text.clear(),
                3 => corrupt.candidates.push(corrupt.candidates[0].clone()),
                _ => {
                    let mut duplicate = corrupt.candidates[0].clone();
                    duplicate.id.push_str("different");
                    corrupt.candidates.push(duplicate);
                }
            }
            let wire: RetrievalReceipt =
                serde_json::from_slice(&serde_json::to_vec(&corrupt).unwrap()).unwrap();
            let expected = match defect {
                0 => RetrievalValidationError::DocumentDigestMismatch,
                1 => RetrievalValidationError::DocumentIdentityMismatch,
                2 => RetrievalValidationError::EmptyDocument,
                3 | 4 => RetrievalValidationError::DuplicateCandidate,
                _ => unreachable!(),
            };
            assert_eq!(wire.validate(&allowed), Err(expected), "defect {defect}");
        }
    }

    proptest! {
        #[test]
        fn bounded_union_preserves_every_channel_identity_and_codec(
            lexical in prop::collection::btree_set(0usize..128, 0..65),
            vector in prop::collection::btree_set(0usize..128, 0..65),
        ) {
            let candidates = union_candidates(lexical.iter().copied().map(union_fixture).collect(), vector.iter().copied().map(union_fixture).collect()).unwrap();
            let decoded: Vec<RetrievedCapability> = serde_json::from_slice(&serde_json::to_vec(&candidates).unwrap()).unwrap();
            prop_assert_eq!(decoded.len(), lexical.union(&vector).count());
            prop_assert!(decoded.len() <= CANDIDATE_LIMIT);
            for c in decoded {
                let id=c.reference.capability.trim_start_matches("read").parse::<usize>().unwrap();
                prop_assert_eq!(c.admissions.contains("lexical"),lexical.contains(&id));
                prop_assert_eq!(c.admissions.contains("vector"),vector.contains(&id));
                prop_assert_eq!(c.document.capability,c.reference.capability);
            }
        }
    }

    #[test]
    fn disjoint_channels_retain_all_128_and_reject_corruption() {
        let result = union_candidates(
            (0..64).map(union_fixture).collect(),
            (64..128).map(union_fixture).collect(),
        )
        .unwrap();
        assert_eq!(result.len(), 128);
        assert!(union_candidates((0..65).map(union_fixture).collect(), vec![]).is_err());
        assert!(union_candidates(vec![union_fixture(0), union_fixture(0)], vec![]).is_err());
        let mut corrupt = union_fixture(0);
        corrupt.document.text.push_str("different");
        assert!(union_candidates(vec![union_fixture(0)], vec![corrupt]).is_err());
    }
    use plasm_core::catalog_discovery::{
        capability_documents, EmbeddedCapability, DISCOVERY_RENDERER_VERSION,
    };
    use plasm_core::prerequisites::DeploymentBinding;

    pub(crate) fn prepared(entry: &str, version: u64) -> PreparedCatalog {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        cgs.bind_registry_entry_id(entry);
        cgs.version = version;
        prepared_cgs(entry, cgs)
    }

    fn prepared_cgs(entry: &str, cgs: CGS) -> PreparedCatalog {
        let version = cgs.version;
        let cgs = cgs.fresh_catalog_digest();
        let hash = cgs.catalog_cgs_hash_hex();
        let capabilities = capability_documents(&cgs)
            .unwrap()
            .into_iter()
            .map(|document| {
                let mut embedding = vec![0.0; 1536];
                embedding[usize::from(document.capability == "read")] = 1.0;
                EmbeddedCapability {
                    document,
                    embedding,
                }
            })
            .collect();
        let discovery = CatalogDiscoveryArtifact {
            entry_id: entry.into(),
            cgs_hash: hash.clone(),
            renderer_version: DISCOVERY_RENDERER_VERSION,
            profile: EmbeddingProfile::default(),
            capabilities,
            prerequisites: cgs.prerequisites.clone(),
        };
        let discovery_hash = content_hash(&serde_json::to_vec(&discovery).unwrap());
        let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
        let recipes_hash = content_hash(&serde_json::to_vec(&compiled).unwrap());
        let manifest = CatalogManifest {
            format_version: 3,
            entry_id: entry.into(),
            version,
            cgs_hash: hash,
            label: entry.into(),
            tags: vec![],
            cgs_json: "body.cgs.json".into(),
            recipes_json: "body.recipes.json".into(),
            recipes_hash,
            discovery_json: "body.discovery.json".into(),
            discovery_hash,
            embedding_profile: EmbeddingProfile::default(),
        };
        let revision = content_hash(&serde_json::to_vec(&manifest).unwrap());
        PreparedCatalog {
            manifest,
            cgs,
            compiled,
            discovery,
            revision,
        }
    }

    fn bindings() -> DeploymentBindings {
        DeploymentBindings {
            bindings: ["matrix", "excluded"]
                .into_iter()
                .map(|catalog| DeploymentBinding {
                    consumer: CapabilityRef {
                        catalog: catalog.into(),
                        capability: "read".into(),
                    },
                    requirement: "scoped_access".into(),
                    provider_catalog: catalog.into(),
                    provider: "value_source".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn pinned_authorization_intersection_never_broadens_capabilities() {
        let pinned = DiscoveryAuthorization {
            catalogs: BTreeSet::from(["retained".into(), "shared".into(), "denied".into()]),
            capabilities: BTreeMap::from([
                (
                    "shared".into(),
                    BTreeSet::from(["read".into(), "write".into()]),
                ),
                ("denied".into(), BTreeSet::new()),
            ]),
        };
        let caller = DiscoveryAuthorization {
            catalogs: BTreeSet::from([
                "retained".into(),
                "shared".into(),
                "denied".into(),
                "new".into(),
            ]),
            capabilities: BTreeMap::from([
                ("retained".into(), BTreeSet::from(["read".into()])),
                (
                    "shared".into(),
                    BTreeSet::from(["read".into(), "delete".into()]),
                ),
            ]),
        };
        let narrowed = pinned.intersection(&caller);
        assert_eq!(narrowed, caller.intersection(&pinned));
        for catalog in ["retained", "shared", "denied", "new"] {
            for capability in ["read", "write", "delete"] {
                let reference = CapabilityRef {
                    catalog: catalog.into(),
                    capability: capability.into(),
                };
                assert_eq!(
                    narrowed.permits(&reference),
                    pinned.permits(&reference) && caller.permits(&reference)
                );
            }
        }
        assert_eq!(pinned.intersection(&pinned), pinned);
        assert!(narrowed.capabilities["denied"].is_empty());
    }

    #[tokio::test]
    #[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
    async fn postgres_operation_queries_have_independent_authorized_rankings() {
        let store = DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
            .await
            .unwrap();
        store.migrate().await.unwrap();
        let mut deployment = bindings();
        deployment
            .bindings
            .retain(|binding| binding.consumer.catalog == "matrix");
        let generation = store
            .import(
                "operation-query-contract",
                vec![prepared("matrix", 1)],
                &deployment,
            )
            .await
            .unwrap();
        let intents = [
            "Obscure individual read",
            "Obscure sensor calibration operation",
        ];
        for (index, intent) in intents.iter().enumerate() {
            let profile = EmbeddingProfile::default();
            let key = content_hash(&serde_json::to_vec(&(intent, &profile)).unwrap());
            let mut vector = vec![0.0; 1536];
            vector[1 - index] = 1.0;
            sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector) ON CONFLICT DO NOTHING")
                .bind(key).bind(serde_json::to_value(profile).unwrap()).bind(vector_literal(&vector).unwrap())
                .execute(&mut *store.pool.acquire().await.unwrap()).await.unwrap();
        }
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let receipts = store
            .retrieve_many(&generation, &intents, &allowed, &RecallFrontier::default())
            .await
            .unwrap();
        assert_eq!(receipts.len(), 2);
        for receipt in &receipts {
            receipt.validate(&allowed).unwrap();
        }
        assert_eq!(receipts[0].candidates[0].reference.capability, "read");
        assert_ne!(receipts[1].candidates[0].reference.capability, "read");
    }

    #[tokio::test]
    #[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
    async fn postgres_union_routes_every_candidate_through_paged_judgment() {
        let store = DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
            .await
            .unwrap();
        store.migrate().await.unwrap();
        // Enough candidates for multiple relevance pages.
        // disjoint_channels_retain_all_128_and_reject_corruption covers the union ceiling.
        let catalogs: Vec<_> = (0..32)
            .map(|i| prepared(&format!("union-{i:02}"), 1))
            .collect();
        let names: BTreeSet<_> = catalogs
            .iter()
            .map(|c| c.manifest.entry_id.clone())
            .collect();
        let bindings = DeploymentBindings {
            bindings: names
                .iter()
                .map(|name| DeploymentBinding {
                    consumer: CapabilityRef {
                        catalog: name.clone(),
                        capability: "read".into(),
                    },
                    requirement: "scoped_access".into(),
                    provider_catalog: name.clone(),
                    provider: "value_source".into(),
                })
                .collect(),
        };
        let generation = store
            .import("union-contract", catalogs, &bindings)
            .await
            .unwrap();
        let intent = "Read selected records";
        let profile = EmbeddingProfile::default();
        let key = content_hash(&serde_json::to_vec(&(intent, &profile)).unwrap());
        let mut vector = vec![0.0; 1536];
        vector[1] = 1.0;
        sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector) ON CONFLICT DO NOTHING")
            .bind(key).bind(serde_json::to_value(profile).unwrap()).bind(vector_literal(&vector).unwrap())
            .execute(&mut *store.pool.acquire().await.unwrap()).await.unwrap();
        let allowed = DiscoveryAuthorization::catalogs(names);
        let retrieval = store
            .retrieve(&generation, intent, &allowed, &RecallFrontier::default())
            .await
            .unwrap();
        retrieval.validate(&allowed).unwrap();
        assert!(retrieval.candidates.len() > 32);
        assert!(retrieval.candidates.len() <= 128);
        let provenance =
            crate::intent_provenance::IntentProvenance::from_turns([intent.into()]).unwrap();
        let (catalogs, _) = store
            .load_discovery_generation(&generation, &allowed)
            .await
            .unwrap();
        let evidence = crate::discovery_matcher::evidence::CatalogEvidence::from_catalogs(
            &retrieval,
            &catalogs,
            &bindings,
            &|r| allowed.permits(r),
        )
        .unwrap();
        let batches = crate::discovery_matcher::issue_batches(
            crate::discovery_matcher::RELEVANCE_MODEL,
            &provenance,
            &retrieval,
            &evidence,
        )
        .unwrap();
        assert!(!batches.is_empty());
        for batch in batches {
            let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
            let answers: BTreeMap<_, _> = body["questions"]
                .as_object()
                .unwrap()
                .keys()
                .map(|key| {
                    (
                        key.clone(),
                        serde_json::json!({
                            "type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"uncertain":0.0,"unrelated":0.0}
                        }),
                    )
                })
                .collect();
            let raw=serde_json::json!({"model":crate::discovery_matcher::RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string();
            store
                .searches()
                .record_decision(batch.cache_key(), &raw)
                .await
                .unwrap();
        }
        let expected = retrieval.candidates.len();
        let service = crate::discovery_service::DiscoveryService::cached_test_service(store);
        let receipt = service
            .route(&generation, &provenance, &allowed, &[])
            .await
            .unwrap();
        assert_eq!(receipt.matching.matches.len(), expected);
        assert_eq!(receipt.closure.as_ref().unwrap().business.len(), expected);

        let wire = serde_json::to_value(&receipt).unwrap();
        assert!(wire.get("reranking").is_none());
        let decoded: crate::discovery_service::RoutingReceipt =
            serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.retrieval.candidates.len(), expected);
    }

    #[tokio::test]
    #[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
    async fn postgres_generation_and_retrieval_contract() {
        let store = DiscoveryStore::connect(
            &std::env::var("PLASM_TEST_POSTGRES_URL").expect("isolated test database required"),
        )
        .await
        .unwrap();
        store.migrate().await.unwrap();
        let generation = store
            .import(
                "matrix-deployment",
                vec![prepared("matrix", 1), prepared("excluded", 1)],
                &bindings(),
            )
            .await
            .unwrap();
        assert_eq!(
            generation,
            store
                .import(
                    "matrix-deployment",
                    vec![prepared("excluded", 1), prepared("matrix", 1)],
                    &bindings()
                )
                .await
                .unwrap()
        );
        assert!(store
            .import(
                "matrix-deployment",
                vec![prepared("matrix", 2)],
                &DeploymentBindings::default()
            )
            .await
            .is_err());
        let session = uuid::Uuid::new_v4().to_string();
        let expiry = chrono::Utc::now() + chrono::Duration::hours(1);
        assert_eq!(
            store
                .pin("matrix-deployment", &session, expiry)
                .await
                .unwrap(),
            generation
        );
        let root =
            crate::intent_provenance::IntentProvenance::from_turns(
                ["Read selected records".into()],
            )
            .unwrap();
        store
            .commit_discovery_progress(&session, &root, false)
            .await
            .unwrap();
        let child = root
            .derived("Resolve the required relation".into())
            .unwrap();
        store
            .commit_discovery_progress(&session, &child, true)
            .await
            .unwrap();
        store
            .commit_discovery_progress(&session, &child, true)
            .await
            .unwrap();
        assert_eq!(store.intent_provenance(&session).await.unwrap(), child);
        assert!(store
            .commit_discovery_progress(&session, &root, true)
            .await
            .is_err());
        let rewritten =
            crate::intent_provenance::IntentProvenance::from_turns(["Read all records".into()])
                .unwrap();
        assert!(store
            .commit_discovery_progress(&session, &rewritten, true)
            .await
            .is_err());
        let left = child.derived("Left next need".into()).unwrap();
        let right = child.derived("Right next need".into()).unwrap();
        let (left_result, right_result) = tokio::join!(
            store.commit_discovery_progress(&session, &left, true),
            store.commit_discovery_progress(&session, &right, true),
        );
        assert_ne!(
            left_result.is_ok(),
            right_result.is_ok(),
            "exactly one concurrent child commits"
        );
        let winner = if left_result.is_ok() { left } else { right };
        assert_eq!(store.intent_provenance(&session).await.unwrap(), winner);
        let next = store
            .import(
                "matrix-deployment",
                vec![prepared("matrix", 2), prepared("excluded", 1)],
                &bindings(),
            )
            .await
            .unwrap();
        assert_ne!(generation, next);
        assert_eq!(
            store
                .pin("matrix-deployment", &session, expiry)
                .await
                .unwrap(),
            generation
        );
        assert_eq!(store.pinned_generation(&session).await.unwrap(), generation);
        store
            .collect_retired(chrono::Utc::now() + chrono::Duration::hours(1))
            .await
            .unwrap();
        let (loaded, compiled, _) = store.load_generation(&generation).await.unwrap();
        let (semantic, _) = store
            .load_discovery_generation(
                &generation,
                &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            )
            .await
            .unwrap();
        assert_eq!(semantic.keys().collect::<Vec<_>>(), vec!["matrix"]);
        let matrix_only = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let warm = store
            .load_discovery_generation(&generation, &matrix_only)
            .await
            .unwrap()
            .0;
        assert!(
            Arc::ptr_eq(&semantic, &warm),
            "unchanged verified catalogue is shared"
        );
        let none = store
            .load_discovery_generation(
                &generation,
                &DiscoveryAuthorization::catalogs(BTreeSet::new()),
            )
            .await
            .unwrap()
            .0;
        assert!(
            none.is_empty(),
            "catalogue authorization is applied on a cache hit or miss"
        );
        let warmed = store
            .load_discovery_generation(&generation, &matrix_only)
            .await
            .unwrap()
            .0;
        let original: Vec<u8> = sqlx::query_scalar("SELECT r.cgs FROM discovery_revisions r JOIN discovery_generation_catalogs g USING (revision_id) WHERE g.generation_id=$1 AND g.entry_id='matrix'")
            .bind(&generation).fetch_one(&mut *store.pool.acquire().await.unwrap()).await.unwrap();
        let mut damaged = original.clone();
        damaged.push(b'!');
        sqlx::query("UPDATE discovery_revisions SET cgs=$1 WHERE revision_id=(SELECT revision_id FROM discovery_generation_catalogs WHERE generation_id=$2 AND entry_id='matrix')")
            .bind(&damaged).bind(&generation).execute(&mut *store.pool.acquire().await.unwrap()).await.unwrap();
        assert!(
            store
                .load_discovery_generation(&generation, &matrix_only)
                .await
                .is_err(),
            "changed bytes cannot reuse a verified graph"
        );
        sqlx::query("UPDATE discovery_revisions SET cgs=$1 WHERE revision_id=(SELECT revision_id FROM discovery_generation_catalogs WHERE generation_id=$2 AND entry_id='matrix')")
            .bind(&original).bind(&generation).execute(&mut *store.pool.acquire().await.unwrap()).await.unwrap();
        let restored = store
            .load_discovery_generation(&generation, &matrix_only)
            .await
            .unwrap()
            .0;
        assert!(
            Arc::ptr_eq(&warmed, &restored),
            "restored exact bytes match the last verified catalogue"
        );
        assert_eq!(
            semantic["matrix"].catalog_cgs_hash_hex(),
            loaded["matrix"].catalog_cgs_hash_hex()
        );
        assert_eq!(
            loaded["matrix"].catalog_cgs_hash_hex(),
            prepared("matrix", 1).manifest.cgs_hash
        );
        assert_eq!(
            compiled["matrix"].cgs_hash(),
            prepared("matrix", 1).manifest.cgs_hash
        );
        let mut vector = vec![0.0; 1536];
        vector[1] = 1.0;
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let receipt = store
            .retrieve_vector(
                &generation,
                "zzzzunmatched",
                &vector_literal(&vector).unwrap(),
                &allowed,
                &RecallFrontier::default(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.lexical_count, 0);
        assert_eq!(
            receipt.candidates.len(),
            prepared("matrix", 1).discovery.capabilities.len()
        );
        assert!(receipt
            .candidates
            .iter()
            .any(|candidate| candidate.reference.capability == "read"));
        assert_eq!(receipt.candidates[0].reference.capability, "read",
            "empty-frontier vector recall preserves similarity rank rather than opaque identity order");
        assert!(receipt
            .candidates
            .iter()
            .all(|c| c.reference.catalog == "matrix"));
        let lexical = store
            .retrieve_vector(
                &generation,
                "acquire unrelated words",
                &vector_literal(&vector).unwrap(),
                &allowed,
                &RecallFrontier::default(),
            )
            .await
            .unwrap();
        assert!(
            lexical.lexical_count > 0,
            "disjunctive terms must admit an individual matching term"
        );
        let empty = store
            .retrieve_vector(
                &generation,
                "read",
                &vector_literal(&vector).unwrap(),
                &DiscoveryAuthorization::catalogs(BTreeSet::new()),
                &RecallFrontier::default(),
            )
            .await
            .unwrap();
        assert!(empty.candidates.is_empty());
        let mut restricted = allowed.clone();
        restricted
            .capabilities
            .insert("matrix".into(), BTreeSet::from(["read".into()]));
        let only_read = store
            .retrieve_vector(
                &generation,
                "acquire read",
                &vector_literal(&vector).unwrap(),
                &restricted,
                &RecallFrontier::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            only_read.candidates.len(),
            1,
            "capability policy applies before lexical/vector ranking and relation expansion"
        );
        assert_eq!(only_read.candidates[0].reference.capability, "read");
        let other_generation = store
            .import(
                "independent-deployment",
                vec![prepared("matrix", 3), prepared("excluded", 1)],
                &bindings(),
            )
            .await
            .unwrap();
        assert_ne!(other_generation, next);
        let other_session = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            store
                .pin("matrix-deployment", &other_session, expiry)
                .await
                .unwrap(),
            next
        );
        let independent_session = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            store
                .pin("independent-deployment", &independent_session, expiry)
                .await
                .unwrap(),
            other_generation
        );

        assert!(!receipt.lexical_truncated && !receipt.vector_truncated);
        assert!(store
            .pin_generation(&session, &next, expiry + chrono::Duration::hours(1))
            .await
            .is_err());
        let saved_expiry: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT expires_at FROM discovery_session_pins WHERE session_id=$1")
                .bind(&session)
                .fetch_one(&mut *store.pool.acquire().await.unwrap())
                .await
                .unwrap();
        assert_eq!(
            saved_expiry.timestamp_micros(),
            expiry.timestamp_micros(),
            "failed repin must not extend retention"
        );
        let lease_pin = DiscoverySessionPin {
            authorization: allowed.clone(),
            generation: generation.clone(),
            pin_id: uuid::Uuid::new_v4().to_string(),
        };
        store
            .pin_generation(
                &lease_pin.pin_id,
                &generation,
                chrono::Utc::now() + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        store.refresh_session_pin(&lease_pin).await.unwrap();
        let renewed: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT expires_at FROM discovery_session_pins WHERE session_id=$1")
                .bind(&lease_pin.pin_id)
                .fetch_one(&mut *store.pool.acquire().await.unwrap())
                .await
                .unwrap();
        assert!(renewed > chrono::Utc::now() + chrono::Duration::hours(23));
        assert!(store
            .refresh_session_pin(&DiscoverySessionPin {
                generation: next.clone(),
                ..lease_pin.clone()
            })
            .await
            .is_err());
        sqlx::query("UPDATE discovery_session_pins SET expires_at=now() - interval '1 second' WHERE session_id=$1")
            .bind(&lease_pin.pin_id).execute(&mut *store.pool.acquire().await.unwrap()).await.unwrap();
        assert!(
            store.refresh_session_pin(&lease_pin).await.is_err(),
            "expired sessions must not be resurrected"
        );
        assert!(store.pinned_generation(&lease_pin.pin_id).await.is_err());
        sqlx::query("DELETE FROM discovery_session_pins WHERE session_id=$1")
            .bind(&lease_pin.pin_id)
            .execute(&mut *store.pool.acquire().await.unwrap())
            .await
            .unwrap();
        let original_recipes: Vec<u8> = sqlx::query_scalar(
            "SELECT recipes FROM discovery_compiled_recipes WHERE revision_id=(SELECT revision_id FROM discovery_generation_catalogs WHERE generation_id=$1 AND entry_id='matrix')",
        )
        .bind(&generation)
        .fetch_one(&mut *store.pool.acquire().await.unwrap())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE discovery_compiled_recipes SET recipes=$1 WHERE revision_id=(SELECT revision_id FROM discovery_generation_catalogs WHERE generation_id=$2 AND entry_id='matrix')",
        )
        .bind(b"{}".as_slice())
        .bind(&generation)
        .execute(&mut *store.pool.acquire().await.unwrap())
        .await
        .unwrap();
        let corrupted_result = store.load_generation(&generation).await;
        // Restore the test's deliberate corruption before asserting, including
        // when the implementation unexpectedly accepts the invalid bytes.
        sqlx::query(
            "UPDATE discovery_compiled_recipes SET recipes=$1 WHERE revision_id=(SELECT revision_id FROM discovery_generation_catalogs WHERE generation_id=$2 AND entry_id='matrix')",
        )
        .bind(original_recipes)
        .bind(&generation)
        .execute(&mut *store.pool.acquire().await.unwrap())
        .await
        .unwrap();
        let error =
            corrupted_result.expect_err("manifest digest must guard persisted recipe bytes");
        assert!(
            matches!(error, DiscoveryStoreError::StoredRecipeDigestMismatch),
            "{error:#}"
        );
    }
}
