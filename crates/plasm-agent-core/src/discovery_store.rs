//! PostgreSQL publication and retrieval. No acquisition capabilities execute here.

use anyhow::{bail, Context, Result};
use plasm_core::catalog_discovery::{
    content_hash, validate_embedding, CapabilityDocument, CatalogDiscoveryArtifact,
    EmbeddingProfile,
};
use plasm_core::catalog_il::{
    load_catalog_artifact, load_discovery_artifact, read_catalog_manifest, CatalogManifest,
};
use plasm_core::prerequisites::{prerequisite_closure, CapabilityRef, DeploymentBindings};
use plasm_core::CGS;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

const CHANNEL_LIMIT: i64 = 64;
const SELECTOR_LIMIT: usize = 128;

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
            .context("manifest has no directory")?;
        let manifest = read_catalog_manifest(manifest_path).map_err(anyhow::Error::msg)?;
        let cgs = load_catalog_artifact(directory, &manifest).map_err(anyhow::Error::msg)?;
        let compiled = plasm_compile::load_compiled_catalog_artifact(directory, &manifest, &cgs)
            .map_err(anyhow::Error::msg)?;
        let discovery =
            load_discovery_artifact(directory, &manifest, &cgs).map_err(anyhow::Error::msg)?;
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

#[derive(Clone)]
pub struct DiscoveryStore {
    pool: PgPool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedCapability {
    pub id: String,
    pub reference: CapabilityRef,
    pub document: CapabilityDocument,
    pub admissions: BTreeSet<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RetrievalReceipt {
    pub generation: String,
    pub candidates: Vec<RetrievedCapability>,
    pub lexical_count: usize,
    pub vector_count: usize,
    pub lexical_truncated: bool,
    pub vector_truncated: bool,
    pub fusion_truncated: usize,
    pub relation_truncated: usize,
}

impl DiscoveryStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    /// Install the discovery schema explicitly; startup checks extension availability separately.
    pub async fn migrate(&self) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
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
        self.verify_extension().await
    }

    pub async fn verify_extension(&self) -> Result<()> {
        sqlx::query("SELECT '[1,0]'::vector <=> '[1,0]'::vector")
            .execute(&self.pool)
            .await
            .context("discovery requires the pgvector extension in PostgreSQL")?;
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
            bail!("deployment identifier must not be empty");
        }
        if catalogs.is_empty() {
            bail!("a discovery generation requires catalogs");
        }
        catalogs.sort_by(|a, b| a.manifest.entry_id.cmp(&b.manifest.entry_id));
        let mut refs = BTreeMap::new();
        for catalog in &catalogs {
            if refs
                .insert(catalog.manifest.entry_id.clone(), &catalog.cgs)
                .is_some()
            {
                bail!("duplicate catalog in generation");
            }
            catalog
                .discovery
                .validate(&catalog.cgs)
                .map_err(anyhow::Error::msg)?;
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
        prerequisite_closure(&refs, bindings, &capabilities, &allowed)
            .map_err(anyhow::Error::msg)?;
        for binding in &bindings.bindings {
            let cgs = refs
                .get(&binding.consumer.catalog)
                .context("binding consumer catalog missing")?;
            if !cgs
                .prerequisites
                .requirements
                .get(&binding.consumer.capability)
                .is_some_and(|rs| rs.iter().any(|r| r.id == binding.requirement))
            {
                bail!("deployment binding addresses an undeclared requirement");
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
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(734901183)")
            .execute(&mut *tx)
            .await?;
        for catalog in &catalogs {
            let recipe_bytes = serde_json::to_vec(&catalog.compiled)?;
            if content_hash(&recipe_bytes) != catalog.manifest.recipes_hash {
                bail!(
                    "compiled request recipe digest does not match manifest for catalog `{}`",
                    catalog.manifest.entry_id
                );
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
                    bail!("catalog revision exists without compiled request recipes");
                };
                if stored_recipes != recipe_bytes {
                    bail!("catalog revision exists with different compiled request recipes");
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
        let mut tx = self.pool.begin().await?;
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
        let bindings: serde_json::Value =
            sqlx::query_scalar("SELECT bindings FROM discovery_generations WHERE generation_id=$1")
                .bind(generation)
                .fetch_optional(&self.pool)
                .await?
                .context("unknown discovery generation")?;
        let rows = sqlx::query("SELECT g.entry_id,r.cgs,r.manifest,p.recipes FROM discovery_generation_catalogs g JOIN discovery_revisions r USING(revision_id) JOIN discovery_compiled_recipes p USING(revision_id) WHERE g.generation_id=$1 ORDER BY g.entry_id")
            .bind(generation).fetch_all(&self.pool).await?;
        let mut catalogs = BTreeMap::new();
        let mut compiled_catalogs = BTreeMap::new();
        for row in rows {
            let bytes: Vec<u8> = row.try_get("cgs")?;
            let manifest: CatalogManifest = serde_json::from_value(row.try_get("manifest")?)?;
            let cgs = plasm_core::catalog_il::load_catalog_il_verified(&bytes, &manifest.cgs_hash)
                .map_err(anyhow::Error::msg)?;
            let entry_id: String = row.try_get("entry_id")?;
            let recipe_bytes: Vec<u8> = row.try_get("recipes")?;
            if content_hash(&recipe_bytes) != manifest.recipes_hash {
                bail!("compiled request recipe digest mismatch for catalog `{entry_id}`");
            }
            let compiled = plasm_compile::CompiledCatalog::decode_artifact(&recipe_bytes, &cgs)
                .map_err(anyhow::Error::msg)?;
            catalogs.insert(entry_id.clone(), cgs);
            compiled_catalogs.insert(entry_id, Arc::new(compiled));
        }
        Ok((
            catalogs,
            compiled_catalogs,
            serde_json::from_value(bindings)?,
        ))
    }

    pub async fn pin_generation(
        &self,
        session: &str,
        generation: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let actual: Option<String> = sqlx::query_scalar("INSERT INTO discovery_session_pins (session_id,generation_id,expires_at) VALUES ($1,$2,$3) ON CONFLICT (session_id) DO UPDATE SET expires_at=GREATEST(discovery_session_pins.expires_at,EXCLUDED.expires_at) WHERE discovery_session_pins.generation_id=EXCLUDED.generation_id RETURNING generation_id")
            .bind(session).bind(generation).bind(expires_at).fetch_optional(&self.pool).await?;
        if actual.is_none() {
            bail!("session is pinned to a different registry generation");
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
            .bind(session).bind(expires_at).bind(deployment).fetch_optional(&self.pool).await?;
        generation.context("no active discovery generation")
    }

    pub async fn pinned_generation(&self, session: &str) -> Result<String> {
        sqlx::query_scalar("SELECT generation_id FROM discovery_session_pins WHERE session_id=$1 AND expires_at > now()")
            .bind(session).fetch_optional(&self.pool).await?.context("discovery session pin missing or expired")
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
            .execute(&self.pool)
            .await
            .context("refresh discovery session lease")?;
        if refreshed.rows_affected() != 1 {
            bail!("discovery session pin missing, expired or generation mismatch");
        }
        Ok(())
    }

    pub async fn retrieve(
        &self,
        generation: &str,
        intent: &str,
        allowed: &DiscoveryAuthorization,
    ) -> Result<RetrievalReceipt> {
        if intent.trim().is_empty() {
            bail!("discovery intent must not be empty");
        }
        let profile = EmbeddingProfile::default();
        let cache_key = content_hash(&serde_json::to_vec(&(intent, &profile))?);
        let cached: Option<String> = sqlx::query_scalar("SELECT embedding::text FROM discovery_intent_embeddings WHERE cache_key=$1 AND profile=$2")
            .bind(&cache_key).bind(serde_json::to_value(&profile)?).fetch_optional(&self.pool).await?;
        let vector = if let Some(cached) = cached {
            cached
        } else {
            let vectors = crate::discovery_embeddings::EmbeddingClient::from_env()?
                .embed(&[intent.to_owned()])
                .await?;
            let vector = vector_literal(vectors.first().context("missing intent embedding")?)?;
            sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector) ON CONFLICT DO NOTHING")
                .bind(&cache_key).bind(serde_json::to_value(&profile)?).bind(&vector).execute(&self.pool).await?;
            vector
        };
        self.retrieve_vector(generation, intent, &vector, allowed)
            .await
    }

    pub async fn cached_selector_envelope(&self, cache_key: &str) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT envelope FROM discovery_selector_cache WHERE cache_key=$1")
            .bind(cache_key)
            .fetch_optional(&self.pool)
            .await
            .context("read selector cache")
    }

    pub async fn store_selector_envelope(&self, cache_key: &str, envelope: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO discovery_selector_cache (cache_key, envelope) VALUES ($1,$2) ON CONFLICT (cache_key) DO NOTHING",
        )
        .bind(cache_key)
        .bind(envelope)
        .execute(&self.pool)
        .await
        .context("write selector cache")?;
        Ok(())
    }

    /// Existing teaching is selectable evidence, resolved against the same authorized generation.
    pub async fn include_exposed(
        &self,
        receipt: &mut RetrievalReceipt,
        exposed: &[CapabilityRef],
        allowed: &DiscoveryAuthorization,
    ) -> Result<()> {
        let mut existing = BTreeMap::new();
        for reference in exposed {
            if !allowed.permits(reference) {
                bail!("exposed capability is no longer authorized");
            }
            let row = sqlx::query("SELECT c.*,g.entry_id FROM discovery_capabilities c JOIN discovery_generation_catalogs g USING (revision_id) WHERE g.generation_id=$1 AND g.entry_id=$2 AND c.capability=$3")
                .bind(&receipt.generation).bind(&reference.catalog).bind(&reference.capability).fetch_optional(&self.pool).await?.context("exposed capability does not belong to the pinned generation")?;
            let mut candidate = candidate_from_row(&row)?;
            candidate.admissions.insert("already_exposed".into());
            existing.insert(candidate.id.clone(), candidate);
        }
        if existing.len() > SELECTOR_LIMIT {
            bail!("routing incomplete: exposed capabilities exceed the selector budget");
        }
        let mut remaining = Vec::new();
        for candidate in std::mem::take(&mut receipt.candidates) {
            if let Some(prior) = existing.get_mut(&candidate.id) {
                prior.admissions.extend(candidate.admissions);
            } else {
                remaining.push(candidate);
            }
        }
        let available = SELECTOR_LIMIT - existing.len();
        receipt.relation_truncated += remaining.len().saturating_sub(available);
        receipt.candidates = existing
            .into_values()
            .chain(remaining.into_iter().take(available))
            .collect();
        Ok(())
    }

    async fn retrieve_vector(
        &self,
        generation: &str,
        intent: &str,
        vector: &str,
        allowed: &DiscoveryAuthorization,
    ) -> Result<RetrievalReceipt> {
        let restrictions = serde_json::to_value(&allowed.capabilities)?;
        let allowed: Vec<_> = allowed.catalogs.iter().cloned().collect();
        let mut lexical = sqlx::query("WITH terms AS (SELECT unnest(tsvector_to_array(to_tsvector('english',$3))) AS term), q AS (SELECT to_tsquery('english',string_agg(quote_literal(term),' | ')) AS query FROM terms) SELECT c.*,g.entry_id FROM discovery_capabilities c JOIN discovery_generation_catalogs g USING (revision_id) CROSS JOIN q WHERE g.generation_id=$1 AND g.entry_id=ANY($2) AND (NOT ($5::jsonb ? g.entry_id) OR ($5::jsonb -> g.entry_id) ? c.capability) AND c.search @@ q.query ORDER BY ts_rank_cd(c.search,q.query) DESC,c.revision_id,c.capability LIMIT $4")
            .bind(generation).bind(&allowed).bind(intent).bind(CHANNEL_LIMIT + 1).bind(&restrictions).fetch_all(&self.pool).await?;
        let mut vector = sqlx::query("SELECT c.*,g.entry_id FROM discovery_capabilities c JOIN discovery_generation_catalogs g USING (revision_id) WHERE g.generation_id=$1 AND g.entry_id=ANY($2) AND (NOT ($5::jsonb ? g.entry_id) OR ($5::jsonb -> g.entry_id) ? c.capability) ORDER BY c.embedding <=> $3::text::vector,c.revision_id,c.capability LIMIT $4")
            .bind(generation).bind(&allowed).bind(vector).bind(CHANNEL_LIMIT + 1).bind(&restrictions).fetch_all(&self.pool).await?;
        let lexical_truncated = lexical.len() > CHANNEL_LIMIT as usize;
        let vector_truncated = vector.len() > CHANNEL_LIMIT as usize;
        lexical.truncate(CHANNEL_LIMIT as usize);
        vector.truncate(CHANNEL_LIMIT as usize);
        let lexical_count = lexical.len();
        let vector_count = vector.len();
        let mut documents = BTreeMap::new();
        let mut ranks = BTreeMap::<String, f64>::new();
        for (channel, rows) in [("lexical", lexical), ("vector", vector)] {
            for (rank, row) in rows.into_iter().enumerate() {
                let mut candidate = candidate_from_row(&row)?;
                *ranks.entry(candidate.id.clone()).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
                candidate.admissions.insert(channel.into());
                documents
                    .entry(candidate.id.clone())
                    .and_modify(|existing: &mut RetrievedCapability| {
                        existing.admissions.insert(channel.into());
                    })
                    .or_insert(candidate);
            }
        }
        let mut ranked: Vec<_> = ranks.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let fusion_truncated = ranked.len().saturating_sub(CHANNEL_LIMIT as usize);
        let mut candidates: Vec<_> = ranked
            .into_iter()
            .take(CHANNEL_LIMIT as usize)
            .filter_map(|(id, _)| documents.remove(&id))
            .collect();
        let mut admitted: BTreeSet<_> = candidates.iter().map(|c| c.id.clone()).collect();
        let mut relation_targets = BTreeSet::new();
        for candidate in &candidates {
            for entity in &candidate.document.related_entities {
                relation_targets.insert((candidate.reference.catalog.clone(), entity.clone()));
            }
        }
        let mut expanded = BTreeMap::new();
        for (catalog, entity) in relation_targets {
            let rows = sqlx::query("SELECT c.*,g.entry_id FROM discovery_capabilities c JOIN discovery_generation_catalogs g USING (revision_id) WHERE g.generation_id=$1 AND g.entry_id=ANY($2) AND g.entry_id=$3 AND c.entity=$4 AND (NOT ($5::jsonb ? g.entry_id) OR ($5::jsonb -> g.entry_id) ? c.capability) ORDER BY c.revision_id,c.capability")
                .bind(generation).bind(&allowed).bind(catalog).bind(entity).bind(&restrictions).fetch_all(&self.pool).await?;
            for row in rows {
                let mut candidate = candidate_from_row(&row)?;
                if admitted.insert(candidate.id.clone()) {
                    candidate.admissions.insert("relation".into());
                    expanded.insert(candidate.id.clone(), candidate);
                }
            }
        }
        let remaining = SELECTOR_LIMIT.saturating_sub(candidates.len());
        let relation_truncated = expanded.len().saturating_sub(remaining);
        candidates.extend(expanded.into_values().take(remaining));
        Ok(RetrievalReceipt {
            generation: generation.into(),
            candidates,
            lexical_count,
            vector_count,
            lexical_truncated,
            vector_truncated,
            fusion_truncated,
            relation_truncated,
        })
    }
}

fn vector_literal(vector: &[f32]) -> Result<String> {
    validate_embedding(vector, 1536).map_err(anyhow::Error::msg)?;
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
mod tests {
    use super::*;
    use plasm_core::catalog_discovery::{
        capability_documents, EmbeddedCapability, DISCOVERY_RENDERER_VERSION,
    };
    use plasm_core::prerequisites::DeploymentBinding;

    fn prepared(entry: &str, version: u64) -> PreparedCatalog {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        cgs.bind_registry_entry_id(entry);
        cgs.version = version;
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
            )
            .await
            .unwrap();
        assert_eq!(receipt.lexical_count, 0);
        assert_eq!(receipt.candidates.len(), 2);
        assert_eq!(receipt.candidates[0].reference.capability, "read");
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
                .fetch_one(&store.pool)
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
                .fetch_one(&store.pool)
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
            .bind(&lease_pin.pin_id).execute(&store.pool).await.unwrap();
        assert!(
            store.refresh_session_pin(&lease_pin).await.is_err(),
            "expired sessions must not be resurrected"
        );
        assert!(store.pinned_generation(&lease_pin.pin_id).await.is_err());
        sqlx::query("DELETE FROM discovery_session_pins WHERE session_id=$1")
            .bind(&lease_pin.pin_id)
            .execute(&store.pool)
            .await
            .unwrap();
        let mut empty_candidates = empty;
        let exposed = [CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        }];
        store
            .include_exposed(&mut empty_candidates, &exposed, &allowed)
            .await
            .unwrap();
        assert_eq!(empty_candidates.candidates.len(), 1);
        assert!(empty_candidates.candidates[0]
            .admissions
            .contains("already_exposed"));
        assert!(store
            .include_exposed(
                &mut empty_candidates,
                &exposed,
                &DiscoveryAuthorization::catalogs(BTreeSet::new())
            )
            .await
            .is_err());

        sqlx::query(
            "UPDATE discovery_compiled_recipes SET recipes=$1 WHERE revision_id=(SELECT revision_id FROM discovery_generation_catalogs WHERE generation_id=$2 AND entry_id='matrix')",
        )
        .bind(b"{}".as_slice())
        .bind(&generation)
        .execute(&store.pool)
        .await
        .unwrap();
        let error = store
            .load_generation(&generation)
            .await
            .expect_err("manifest digest must guard persisted recipe bytes");
        assert!(
            error
                .to_string()
                .contains("compiled request recipe digest mismatch"),
            "{error:#}"
        );
    }
}
