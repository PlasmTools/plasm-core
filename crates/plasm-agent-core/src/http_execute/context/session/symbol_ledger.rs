//! Logical-session symbol ledger restore/persist (append-only `e#` / `m#` / `p#` / `r#`).

use super::super::super::*;
use crate::mcp_transport_store::logical_symbol_ledger::DurableSymbolLedgerLoad;
use indexmap::IndexMap;
use plasm_core::catalog_cgs_hashes_from_session;
use std::sync::Arc;
use uuid::Uuid;

async fn materialize_catalog_cgs_for_hashes(
    st: &PlasmHostState,
    hashes: &IndexMap<String, String>,
    outbound_ref: Option<&HashMap<String, String>>,
    bindings_ref: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
) -> Result<(IndexMap<String, Arc<plasm_core::CGS>>, bool), String> {
    let mut catalog_cgs = IndexMap::new();
    for (entry_id, pinned) in hashes {
        let hosted_kv = outbound_ref
            .and_then(|m| m.get(entry_id))
            .map(String::as_str);
        let entry_bindings = bindings_ref.and_then(|m| m.get(entry_id));
        let materialized = crate::execute_session_materialize::materialize_entry_context(
            st,
            entry_id,
            hosted_kv,
            entry_bindings,
        )
        .await?;
        let live_hash = materialized
            .effective_cgs
            .effective_catalog_cgs_hash_hex();
        if &live_hash != pinned {
            return Ok((catalog_cgs, true));
        }
        catalog_cgs.insert(entry_id.clone(), materialized.effective_cgs);
    }
    Ok((catalog_cgs, false))
}

async fn reset_ledger(st: &PlasmHostState, uuid: Uuid) {
    st.logical_symbol_ledgers.remove(&uuid).await;
}

/// Restore append-only symbol state when opening a fresh transport row under an existing logical session.
pub(crate) async fn resolve_restore_for_open(
    st: &PlasmHostState,
    logical_session_id: Option<Uuid>,
    outbound_ref: Option<&HashMap<String, String>>,
    bindings_ref: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
) -> (Option<plasm_core::TeachingExposureSession>, bool) {
    let Some(uuid) = logical_session_id else {
        return (None, false);
    };

    if let Some(entry) = st.logical_symbol_ledgers.get_local(&uuid).await {
        match materialize_catalog_cgs_for_hashes(
            st,
            &entry.catalog_cgs_hashes,
            outbound_ref,
            bindings_ref,
        )
        .await
        {
            Ok((_, true)) => {
                reset_ledger(st, uuid).await;
                (None, true)
            }
            Ok((_, false)) => (Some((*entry.exposure).clone()), false),
            Err(_) => {
                reset_ledger(st, uuid).await;
                (None, true)
            }
        }
    } else {
        match st.logical_symbol_ledgers.load_durable(&uuid).await {
            DurableSymbolLedgerLoad::NotFound => (None, false),
            DurableSymbolLedgerLoad::UnsupportedVersion(_) => {
                reset_ledger(st, uuid).await;
                (None, true)
            }
            DurableSymbolLedgerLoad::Decode(_) => (None, false),
            DurableSymbolLedgerLoad::Found(snap) => {
                match materialize_catalog_cgs_for_hashes(
                    st,
                    &snap.catalog_cgs_hashes,
                    outbound_ref,
                    bindings_ref,
                )
                .await
                {
                    Ok((catalog_cgs, true)) => {
                        reset_ledger(st, uuid).await;
                        (None, true)
                    }
                    Ok((catalog_cgs, false)) => {
                        match st
                            .logical_symbol_ledgers
                            .hydrate_and_cache(uuid, snap, &catalog_cgs)
                            .await
                        {
                            Ok(entry) => (Some((*entry.exposure).clone()), false),
                            Err(_) => {
                                reset_ledger(st, uuid).await;
                                (None, true)
                            }
                        }
                    }
                    Err(_) => {
                        reset_ledger(st, uuid).await;
                        (None, true)
                    }
                }
            }
        }
    }
}

/// Persist teaching exposure from a live execute row into the logical-session ledger.
pub(crate) async fn persist_from_execute_row(
    st: &PlasmHostState,
    logical_session_id: Option<Uuid>,
    prompt_hash: &str,
    session_id: &str,
) {
    let Some(uuid) = logical_session_id else {
        return;
    };
    let Some(sess) = st.get_execute_session(prompt_hash, session_id).await else {
        return;
    };
    let Some(exp) = sess.teaching_exposure.clone() else {
        return;
    };
    let hashes = catalog_cgs_hashes_from_session(&exp);
    if let Err(err) = st
        .logical_symbol_ledgers
        .upsert(uuid, hashes, exp)
        .await
    {
        tracing::warn!(?err, %uuid, "symbol ledger persist failed");
    }
}

/// Persist after an exposure wave when the execute row is bound to a logical session.
pub(crate) async fn persist_after_wave_commit(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
) {
    let Some(uuid) = st
        .logical_execute_bindings
        .find_by_execute(prompt_hash, session_id)
        .await
    else {
        return;
    };
    persist_from_execute_row(st, Some(uuid), prompt_hash, session_id).await;
}
