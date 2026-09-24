//! OpenRouter embedding acquisition shared by publication and query retrieval.

use anyhow::{bail, Context, Result};
use plasm_core::catalog_discovery::{
    embedding_cache_key, validate_embedding, CapabilityDocument, EmbeddedCapability,
    EmbeddingProfile,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const BATCH_SIZE: usize = 32;
const MAX_ATTEMPTS: u32 = 4;

pub struct EmbeddingClient {
    client: reqwest::Client,
    api_key: String,
    profile: EmbeddingProfile,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    model: String,
    data: Vec<EmbeddingRow>,
}

#[derive(Deserialize)]
struct EmbeddingRow {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedEmbedding {
    key: String,
    profile: EmbeddingProfile,
    vector: Vec<f32>,
}

impl EmbeddingClient {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .context("OPENROUTER_API_KEY is required for embedding acquisition")?;
        if api_key.trim().is_empty() {
            bail!("OPENROUTER_API_KEY must not be empty");
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
            api_key,
            profile: EmbeddingProfile::default(),
        })
    }

    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() || texts.len() > BATCH_SIZE {
            bail!("embedding requests require 1..={BATCH_SIZE} inputs");
        }
        if texts.iter().any(|text| text.trim().is_empty()) {
            bail!("embedding inputs must not be empty");
        }
        for attempt in 0..MAX_ATTEMPTS {
            let response = self
                .client
                .post("https://openrouter.ai/api/v1/embeddings")
                .bearer_auth(&self.api_key)
                .json(&serde_json::json!({
                    "model": self.profile.model,
                    "dimensions": self.profile.dimensions,
                    "encoding_format": self.profile.encoding_format,
                    "input": texts,
                }))
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    let body: EmbeddingResponse = response
                        .json()
                        .await
                        .context("invalid OpenRouter embedding response")?;
                    return validate_response(body, texts.len(), &self.profile);
                }
                Ok(response) => {
                    let status = response.status();
                    if !(status.as_u16() == 429 || status.is_server_error())
                        || attempt + 1 == MAX_ATTEMPTS
                    {
                        bail!("OpenRouter embedding request failed: HTTP {status}");
                    }
                }
                Err(error) if attempt + 1 == MAX_ATTEMPTS => return Err(error.into()),
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await;
        }
        bail!("embedding attempts exhausted")
    }
}

fn validate_response(
    response: EmbeddingResponse,
    count: usize,
    profile: &EmbeddingProfile,
) -> Result<Vec<Vec<f32>>> {
    // OpenRouter accepts its qualified catalog id but forwards OpenAI's native
    // response id. These two exact spellings identify the same configured model.
    let matches_native_id = profile.model == "openai/text-embedding-3-small"
        && response.model == "text-embedding-3-small";
    if response.model != profile.model && !matches_native_id {
        bail!("embedding response model does not match the requested profile");
    }
    if response.data.len() != count {
        bail!("embedding response count mismatch");
    }
    let mut ordered = vec![None; count];
    for row in response.data {
        validate_embedding(&row.embedding, profile.dimensions).map_err(anyhow::Error::msg)?;
        let slot = ordered
            .get_mut(row.index)
            .context("embedding response index out of range")?;
        if slot.is_some() {
            bail!("duplicate embedding response index");
        }
        *slot = Some(row.embedding);
    }
    ordered
        .into_iter()
        .map(|row| row.context("missing embedding response index"))
        .collect()
}

#[derive(Debug)]
pub struct EmbeddedDocuments {
    pub capabilities: Vec<EmbeddedCapability>,
    pub reused_documents: usize,
    pub embedded_documents: usize,
    pub requests: usize,
}

/// Fully cached publications require no credentials or network access.
pub async fn embed_documents(
    documents: Vec<CapabilityDocument>,
    cache_dir: &Path,
) -> Result<EmbeddedDocuments> {
    embed_documents_with(documents, cache_dir, |texts| async move {
        EmbeddingClient::from_env()?.embed(&texts).await
    })
    .await
}

/// Hold a process-shared cache lease across lookup and acquisition so parallel packers
/// cannot both pay to fill the same cache miss. File locks are released on process exit.
async fn lock_cache(cache_dir: &Path) -> Result<std::fs::File> {
    let directory = cache_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&directory)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("acquisition.lock"))?;
        file.lock()?;
        Ok(file)
    })
    .await?
}

fn read_cached_vector(
    path: &Path,
    key: &str,
    profile: &EmbeddingProfile,
) -> Result<Option<Vec<f32>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let cached: CachedEmbedding = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid embedding cache entry {}", path.display()))?;
    if cached.key != key || &cached.profile != profile {
        bail!("embedding cache identity mismatch at {}", path.display());
    }
    validate_embedding(&cached.vector, profile.dimensions).map_err(anyhow::Error::msg)?;
    Ok(Some(cached.vector))
}

/// Recover the vector cache from a verified portable artifact without embedding again.
pub async fn cache_artifact(
    artifact: &plasm_core::catalog_discovery::CatalogDiscoveryArtifact,
    cache_dir: &Path,
) -> Result<()> {
    let _lease = lock_cache(cache_dir).await?;
    artifact.profile.validate().map_err(anyhow::Error::msg)?;
    if artifact.renderer_version != plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION {
        bail!("cannot reuse embeddings from another document renderer");
    }
    std::fs::create_dir_all(cache_dir)?;
    for capability in &artifact.capabilities {
        validate_embedding(&capability.embedding, artifact.profile.dimensions)
            .map_err(anyhow::Error::msg)?;
        if plasm_core::catalog_discovery::content_hash(capability.document.text.as_bytes())
            != capability.document.text_hash
        {
            bail!("artifact document hash does not match canonical text");
        }
        let key = embedding_cache_key(&capability.document, &artifact.profile);
        let path = cache_dir.join(format!("{key}.json"));
        if let Some(existing) = read_cached_vector(&path, &key, &artifact.profile)? {
            if existing != capability.embedding {
                bail!("artifact and cache disagree for embedding key {key}");
            }
        } else {
            atomic_write(
                &path,
                &serde_json::to_vec(&CachedEmbedding {
                    key,
                    profile: artifact.profile.clone(),
                    vector: capability.embedding.clone(),
                })?,
            )?;
        }
    }
    Ok(())
}

async fn embed_documents_with<F, Fut>(
    documents: Vec<CapabilityDocument>,
    cache_dir: &Path,
    mut acquire: F,
) -> Result<EmbeddedDocuments>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Vec<f32>>>>,
{
    use std::collections::BTreeMap;
    let _lease = lock_cache(cache_dir).await?;
    let profile = EmbeddingProfile::default();
    let mut vectors: Vec<Option<Vec<f32>>> = vec![None; documents.len()];
    let mut missing: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut reused_documents = 0;
    for (index, document) in documents.iter().enumerate() {
        if plasm_core::catalog_discovery::content_hash(document.text.as_bytes())
            != document.text_hash
        {
            bail!("document hash does not match canonical text");
        }
        let key = embedding_cache_key(document, &profile);
        let path = cache_dir.join(format!("{key}.json"));
        match read_cached_vector(&path, &key, &profile)? {
            Some(vector) => {
                vectors[index] = Some(vector);
                reused_documents += 1;
            }
            None => missing.entry(key).or_default().push(index),
        }
    }
    let missing = missing.into_iter().collect::<Vec<_>>();
    let mut requests = 0;
    for batch in missing.chunks(BATCH_SIZE) {
        let texts = batch
            .iter()
            .map(|(_, indices)| documents[indices[0]].text.clone())
            .collect();
        let acquired = acquire(texts).await?;
        requests += 1;
        if acquired.len() != batch.len() {
            bail!("embedding acquisition count mismatch");
        }
        // Validate the entire response before storing any member of this batch.
        for vector in &acquired {
            validate_embedding(vector, profile.dimensions).map_err(anyhow::Error::msg)?;
        }
        for ((key, indices), vector) in batch.iter().zip(acquired) {
            atomic_write(
                &cache_dir.join(format!("{key}.json")),
                &serde_json::to_vec(&CachedEmbedding {
                    key: key.clone(),
                    profile: profile.clone(),
                    vector: vector.clone(),
                })?,
            )?;
            for &index in indices {
                vectors[index] = Some(vector.clone());
            }
        }
    }
    let capabilities = documents
        .into_iter()
        .zip(vectors)
        .map(|(document, vector)| {
            Ok(EmbeddedCapability {
                document,
                embedding: vector.context("embedding acquisition incomplete")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(EmbeddedDocuments {
        capabilities,
        reused_documents,
        embedded_documents: missing.len(),
        requests,
    })
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    match std::fs::read(path) {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_model_identity_accepts_only_the_configured_openai_model() {
        let profile = EmbeddingProfile::default();
        for (model, accepted) in [
            ("openai/text-embedding-3-small", true),
            ("text-embedding-3-small", true),
            ("text-embedding-3-large", false),
            ("another/text-embedding-3-small", false),
        ] {
            let response = EmbeddingResponse {
                model: model.into(),
                data: vec![EmbeddingRow {
                    index: 0,
                    embedding: vec![1.0; profile.dimensions],
                }],
            };
            assert_eq!(validate_response(response, 1, &profile).is_ok(), accepted);
        }
    }

    #[test]
    fn response_order_is_recovered_from_indices() {
        let profile = EmbeddingProfile::default();
        let row = |index, v| EmbeddingRow {
            index,
            embedding: vec![v; profile.dimensions],
        };
        let response = EmbeddingResponse {
            model: profile.model.clone(),
            data: vec![row(1, 2.0), row(0, 1.0)],
        };
        let vectors = validate_response(response, 2, &profile).unwrap();
        assert_eq!(vectors[0][0], 1.0);
        assert_eq!(vectors[1][0], 2.0);
    }

    #[test]
    fn duplicate_indices_cannot_publish_partial_batch() {
        let profile = EmbeddingProfile::default();
        let row = || EmbeddingRow {
            index: 0,
            embedding: vec![1.0; profile.dimensions],
        };
        let response = EmbeddingResponse {
            model: profile.model.clone(),
            data: vec![row(), row()],
        };
        assert!(validate_response(response, 2, &profile)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    fn document(text: &str) -> CapabilityDocument {
        CapabilityDocument {
            capability: text.into(),
            entity: "Fixture".into(),
            text: text.into(),
            text_hash: plasm_core::catalog_discovery::content_hash(text.as_bytes()),
            operation: plasm_core::catalog_discovery::OperationEvidence {
                kind: plasm_core::schema::CapabilityKind::Query,
                receiver: None,
                contract: text.into(),
            },
            collection: plasm_core::catalog_discovery::CollectionEvidence {
                meaning: text.into(),
            },
        }
    }

    #[tokio::test]
    async fn unchanged_documents_make_no_acquisition_and_keep_exact_vectors() {
        let cache = tempfile::tempdir().unwrap();
        let docs = vec![document("read items"), document("update items")];
        let first = embed_documents_with(docs.clone(), cache.path(), |texts| {
            std::future::ready(Ok(texts
                .iter()
                .enumerate()
                .map(|(index, _)| vec![index as f32 + 0.125; 1536])
                .collect()))
        })
        .await
        .unwrap();
        let second = embed_documents_with(docs, cache.path(), |_| async {
            panic!("unchanged packing must not acquire embeddings or require a key")
        })
        .await
        .unwrap();
        assert_eq!(first.capabilities, second.capabilities);
        assert_eq!(
            (
                second.reused_documents,
                second.embedded_documents,
                second.requests
            ),
            (2, 0, 0)
        );
    }

    #[tokio::test]
    async fn changed_catalog_acquires_only_changed_document_and_deduplicates_hashes() {
        let cache = tempfile::tempdir().unwrap();
        let original = document("read items");
        embed_documents_with(vec![original.clone()], cache.path(), |_| async {
            Ok(vec![vec![0.125; 1536]])
        })
        .await
        .unwrap();
        let changed = document("update item title");
        let next = embed_documents_with(
            vec![original, changed.clone(), changed],
            cache.path(),
            |texts| async move {
                assert_eq!(texts, ["update item title"]);
                Ok(vec![vec![0.25; 1536]])
            },
        )
        .await
        .unwrap();
        assert_eq!(
            (
                next.reused_documents,
                next.embedded_documents,
                next.requests
            ),
            (1, 1, 1)
        );
        assert_eq!(next.capabilities[0].embedding[0], 0.125);
        assert_eq!(
            next.capabilities[1].embedding,
            next.capabilities[2].embedding
        );
    }

    #[tokio::test]
    async fn interrupted_pack_resumes_without_rebuying_completed_batches() {
        let cache = tempfile::tempdir().unwrap();
        let docs = (0..35)
            .map(|i| document(&format!("capability {i}")))
            .collect::<Vec<_>>();
        let mut calls = 0;
        let failed = embed_documents_with(docs.clone(), cache.path(), |texts| {
            calls += 1;
            std::future::ready(if calls == 2 {
                Err(anyhow::anyhow!("transient acquisition exhausted"))
            } else {
                Ok(texts.iter().map(|_| vec![0.5; 1536]).collect())
            })
        })
        .await;
        assert!(failed.is_err());
        assert_eq!(
            std::fs::read_dir(cache.path())
                .unwrap()
                .filter(|entry| entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json"))
                .count(),
            BATCH_SIZE
        );
        let resumed = embed_documents_with(docs, cache.path(), |texts| async move {
            assert_eq!(texts.len(), 3);
            Ok(texts.iter().map(|_| vec![0.75; 1536]).collect())
        })
        .await
        .unwrap();
        assert_eq!(
            (
                resumed.reused_documents,
                resumed.embedded_documents,
                resumed.requests
            ),
            (32, 3, 1)
        );
    }

    #[tokio::test]
    async fn malformed_batch_and_hash_mismatch_do_not_populate_cache() {
        let cache = tempfile::tempdir().unwrap();
        let bad = embed_documents_with(
            vec![document("one"), document("two")],
            cache.path(),
            |_| async { Ok(vec![vec![0.5; 1536], vec![f32::NAN; 1536]]) },
        )
        .await;
        assert!(bad.is_err());
        assert_eq!(
            std::fs::read_dir(cache.path())
                .unwrap()
                .filter(|entry| entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json"))
                .count(),
            0
        );
        let mut corrupt = document("three");
        corrupt.text.push_str(" changed without updating hash");
        assert!(
            embed_documents_with(vec![corrupt], cache.path(), |_| async {
                panic!("invalid document hash must fail before acquiring embeddings")
            })
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn concurrent_packers_acquire_each_hash_once() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let cache = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let run = || {
            let calls = calls.clone();
            embed_documents_with(vec![document("shared document")], cache.path(), move |_| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(vec![vec![0.125; 1536]])
                }
            })
        };
        let (first, second) = tokio::join!(run(), run());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first.unwrap().capabilities, second.unwrap().capabilities);
    }
    #[tokio::test]
    async fn corrupt_or_mixed_profile_cache_fails_without_paid_repair() {
        let cache = tempfile::tempdir().unwrap();
        let doc = document("read items");
        let profile = EmbeddingProfile::default();
        let key = embedding_cache_key(&doc, &profile);
        let path = cache.path().join(format!("{key}.json"));
        let correct = || CachedEmbedding {
            key: key.clone(),
            profile: profile.clone(),
            vector: vec![0.125; 1536],
        };
        let mut wrong_model = correct();
        wrong_model.profile.model = "another/model".into();
        let mut wrong_key = correct();
        wrong_key.key = "another-document".into();
        let mut wrong_dimensions = correct();
        wrong_dimensions.vector.pop();
        for damaged in [
            b"invalid json".to_vec(),
            serde_json::to_vec(&wrong_model).unwrap(),
            serde_json::to_vec(&wrong_key).unwrap(),
            serde_json::to_vec(&wrong_dimensions).unwrap(),
        ] {
            std::fs::write(&path, &damaged).unwrap();
            assert!(
                embed_documents_with(vec![doc.clone()], cache.path(), |_| async {
                    panic!("corrupt cache must fail without purchasing a replacement")
                })
                .await
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), damaged);
        }
    }
}
