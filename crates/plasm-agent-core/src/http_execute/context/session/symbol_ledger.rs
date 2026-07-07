//! Logical-session symbol ledger restore/persist (append-only `e#` / `m#` / `p#` / `r#`).

use super::super::super::*;
use crate::execute_session_materialize::{
    verify_effective_catalog_pins_maps, CatalogPinVerifyOutcome, DurableExposureSnapshot,
};
use crate::mcp_transport_store::logical_symbol_ledger::{
    DurableSymbolLedgerLoad, SymbolLedgerUpsertError,
};
use uuid::Uuid;

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
        match verify_effective_catalog_pins_maps(
            st,
            &entry.catalog_cgs_hashes,
            outbound_ref,
            bindings_ref,
        )
        .await
        {
            Ok(CatalogPinVerifyOutcome::Mismatch) => {
                reset_ledger(st, uuid).await;
                (None, true)
            }
            Ok(CatalogPinVerifyOutcome::Ok(_)) => (Some((*entry.exposure).clone()), false),
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
                match verify_effective_catalog_pins_maps(
                    st,
                    &snap.catalog_cgs_hashes,
                    outbound_ref,
                    bindings_ref,
                )
                .await
                {
                    Ok(CatalogPinVerifyOutcome::Mismatch) => {
                        reset_ledger(st, uuid).await;
                        (None, true)
                    }
                    Ok(CatalogPinVerifyOutcome::Ok(catalog_cgs)) => {
                        match st
                            .logical_symbol_ledgers
                            .hydrate_and_cache(uuid, *snap, &catalog_cgs)
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

/// Write-through logical-session ledger using pre-encoded bytes from the durable exposure snapshot.
pub(crate) async fn upsert_logical_ledger_from_snapshot(
    st: &PlasmHostState,
    logical_id: Uuid,
    sess: &crate::execute_session::ExecuteSession,
    exposure: &DurableExposureSnapshot,
) -> Result<(), SymbolLedgerUpsertError> {
    let Some(exp) = sess.teaching_exposure.as_ref() else {
        return Ok(());
    };
    st.logical_symbol_ledgers
        .upsert_preencoded(
            logical_id,
            exposure
                .catalog_cgs_hashes_by_entry
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().to_string()))
                .collect(),
            exposure.symbol_ledger_bytes.clone(),
            exp.clone(),
        )
        .await
}
