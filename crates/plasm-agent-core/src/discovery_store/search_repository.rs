//! Durable discovery search records and their ownership keys.
//!
//! The existing table and key bytes are intentionally unchanged: completion and
//! decision records are immutable, admission is insert-once, and frontiers and
//! progress are replaceable only while the appropriate search lock is held.

use super::{DiscoveryStore, DiscoveryStoreError};
use crate::discovery_matcher::online::SearchProgress;
use crate::discovery_service::{
    DeferredBacklog, OperationFrontier, RoutingReceipt, SearchFrontier,
};
use serde::{de::DeserializeOwned, Serialize};
use std::future::Future;

#[derive(Clone, Copy)]
pub(crate) struct SearchKey<'a>(pub(crate) &'a str);
#[derive(Clone, Copy)]
pub(crate) struct FrontierKey<'a>(pub(crate) &'a str);
#[derive(Clone, Copy)]
pub(crate) struct BacklogKey<'a>(pub(crate) &'a str);

pub(crate) enum SearchAdmission {
    Fresh,
    PreviouslyStarted(Option<Box<SearchProgress>>),
}

#[cfg(test)]
impl SearchAdmission {
    pub(crate) fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }
}

pub(crate) struct SearchRepository<'a> {
    store: &'a DiscoveryStore,
}

impl DiscoveryStore {
    pub(crate) fn searches(&self) -> SearchRepository<'_> {
        SearchRepository { store: self }
    }
}

impl SearchRepository<'_> {
    async fn load<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, DiscoveryStoreError> {
        self.load_raw(key)
            .await?
            .map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .transpose()
    }

    async fn load_raw(&self, key: &str) -> Result<Option<String>, DiscoveryStoreError> {
        Ok(
            sqlx::query_scalar("SELECT envelope FROM discovery_selector_cache WHERE cache_key=$1")
                .bind(key)
                .fetch_optional(&mut *self.store.pool.acquire().await?)
                .await?,
        )
    }

    async fn insert_raw(&self, key: &str, raw: &str) -> Result<(), DiscoveryStoreError> {
        sqlx::query("INSERT INTO discovery_selector_cache (cache_key, envelope) VALUES ($1,$2) ON CONFLICT (cache_key) DO NOTHING")
            .bind(key).bind(raw).execute(&mut *self.store.pool.acquire().await?).await?;
        Ok(())
    }

    async fn replace<T: Serialize>(&self, key: &str, value: &T) -> Result<(), DiscoveryStoreError> {
        let raw = serde_json::to_string(value)?;
        sqlx::query("INSERT INTO discovery_selector_cache (cache_key,envelope) VALUES ($1,$2) ON CONFLICT (cache_key) DO UPDATE SET envelope=EXCLUDED.envelope")
            .bind(key).bind(raw).execute(&mut *self.store.pool.acquire().await?).await?;
        Ok(())
    }

    pub(crate) async fn with_search<F, Fut, T>(
        &self,
        key: SearchKey<'_>,
        work: F,
    ) -> Result<T, DiscoveryStoreError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.store.pool.with_search_lock(key.0, work).await
    }
    pub(crate) async fn with_frontier<F, Fut, T>(
        &self,
        key: FrontierKey<'_>,
        work: F,
    ) -> Result<T, DiscoveryStoreError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.store.pool.with_search_lock(key.0, work).await
    }
    pub(crate) async fn with_backlog<F, Fut, T>(
        &self,
        key: BacklogKey<'_>,
        work: F,
    ) -> Result<T, DiscoveryStoreError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.store.pool.with_search_lock(key.0, work).await
    }

    pub(crate) async fn completion(
        &self,
        key: SearchKey<'_>,
    ) -> Result<Option<RoutingReceipt>, DiscoveryStoreError> {
        self.load(key.0).await
    }
    pub(crate) async fn complete(
        &self,
        key: SearchKey<'_>,
        receipt: &RoutingReceipt,
    ) -> Result<(), DiscoveryStoreError> {
        self.insert_raw(key.0, &serde_json::to_string(receipt)?)
            .await
    }
    pub(crate) async fn reserve(
        &self,
        key: SearchKey<'_>,
    ) -> Result<SearchAdmission, DiscoveryStoreError> {
        let inserted = sqlx::query("INSERT INTO discovery_selector_cache (cache_key, envelope) VALUES ($1,'{\"started\":true}') ON CONFLICT (cache_key) DO NOTHING")
            .bind(format!("{}:started", key.0)).execute(&mut *self.store.pool.acquire().await?).await?.rows_affected() == 1;
        if inserted {
            Ok(SearchAdmission::Fresh)
        } else {
            Ok(SearchAdmission::PreviouslyStarted(
                self.load(&format!("{}:progress", key.0)).await?,
            ))
        }
    }
    pub(crate) async fn checkpoint(
        &self,
        key: SearchKey<'_>,
        progress: &SearchProgress,
    ) -> Result<(), DiscoveryStoreError> {
        self.replace(&format!("{}:progress", key.0), progress).await
    }
    pub(crate) async fn operations(
        &self,
        key: SearchKey<'_>,
    ) -> Result<Option<OperationFrontier>, DiscoveryStoreError> {
        self.load(&format!("{}:operations", key.0)).await
    }
    pub(crate) async fn checkpoint_operations(
        &self,
        key: SearchKey<'_>,
        state: &OperationFrontier,
    ) -> Result<(), DiscoveryStoreError> {
        self.replace(&format!("{}:operations", key.0), state).await
    }
    pub(crate) async fn frontier(
        &self,
        key: FrontierKey<'_>,
    ) -> Result<Option<SearchFrontier>, DiscoveryStoreError> {
        self.load(key.0).await
    }
    pub(crate) async fn checkpoint_frontier(
        &self,
        key: FrontierKey<'_>,
        state: &SearchFrontier,
    ) -> Result<(), DiscoveryStoreError> {
        self.replace(key.0, state).await
    }
    pub(crate) async fn backlog(
        &self,
        key: BacklogKey<'_>,
    ) -> Result<Option<DeferredBacklog>, DiscoveryStoreError> {
        self.load(key.0).await
    }
    pub(crate) async fn checkpoint_backlog(
        &self,
        key: BacklogKey<'_>,
        state: &DeferredBacklog,
    ) -> Result<(), DiscoveryStoreError> {
        self.replace(key.0, state).await
    }
    pub(crate) async fn decision(&self, key: &str) -> Result<Option<String>, DiscoveryStoreError> {
        self.load_raw(key).await
    }
    pub(crate) async fn record_decision(
        &self,
        key: &str,
        raw: &str,
    ) -> Result<(), DiscoveryStoreError> {
        self.insert_raw(key, raw).await
    }
}
