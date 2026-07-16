//! Peer-session overlap advisory for `session_mode: new`.

use super::super::*;
use super::seeds::capability_seeds_from_session;
use std::collections::BTreeSet;

/// Structured peer-session overlap advisory for `session_mode: new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionChurnAdvisory {
    pub markdown: String,
    pub peer_ref: String,
    pub overlap: Vec<String>,
}

fn intent_token_jaccard(a: &str, b: &str) -> f64 {
    let ta = plasm_core::catalog_search_index::CatalogSearchIndex::tokenize(a);
    let tb = plasm_core::catalog_search_index::CatalogSearchIndex::tokenize(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Advisory when `session_mode: new` overlaps seeds with a recent live logical session.
///
/// Emits only when overlap ≥ 2 entities **or** peer `context_intent` Jaccard vs `new_intent`
/// exceeds a fixed threshold.
pub(crate) async fn format_session_churn_advisory(
    st: &PlasmHostState,
    tenant_scope: &str,
    except: Option<crate::session_identity::LogicalSessionId>,
    requested_seeds: &[CapabilitySeed],
    new_intent: &str,
) -> Option<SessionChurnAdvisory> {
    use crate::mcp_logical_ref::format_logical_session_wire_ref;

    let requested: BTreeSet<(String, String)> = requested_seeds
        .iter()
        .map(|s| (s.entry_id.clone(), s.entity.clone()))
        .collect();
    if requested.is_empty() {
        return None;
    }
    let recent = st
        .logical_sessions
        .recent_sessions_for_tenant(tenant_scope, except)
        .await;
    for rec in recent.into_iter().rev() {
        let Some(pair) = st
            .logical_execute_bindings
            .get(&rec.logical_session_id.as_uuid())
            .await
        else {
            continue;
        };
        let Some(sess) = st.get_execute_session(&pair.0, &pair.1).await else {
            continue;
        };
        let exposed: BTreeSet<(String, String)> = capability_seeds_from_session(sess.as_ref())
            .into_iter()
            .map(|s| (s.entry_id, s.entity))
            .collect();
        let overlap: Vec<String> = requested
            .intersection(&exposed)
            .map(|(eid, ent)| format!("{eid}:{ent}"))
            .collect();
        if overlap.is_empty() {
            continue;
        }
        let peer_intent = sess.context_intent.as_deref().unwrap_or("");
        let intent_close = intent_token_jaccard(new_intent, peer_intent) >= 0.35;
        if overlap.len() < 2 && !intent_close {
            continue;
        }
        let wire_ref = format_logical_session_wire_ref(rec.logical_session_id);
        let markdown = format!(
            "**Note:** session `{wire_ref}` already exposes {}. Use `session_mode: \"extend\"` with that `logical_session_ref` unless this is a separate goal.\n\n",
            overlap.join(", ")
        );
        return Some(SessionChurnAdvisory {
            markdown,
            peer_ref: wire_ref,
            overlap,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::intent_token_jaccard;

    #[test]
    fn session_churn_intent_jaccard_thresholds() {
        assert!(
            (intent_token_jaccard(
                "create github issue labels",
                "create github issue labels"
            ) - 1.0)
                .abs()
                < 1e-9
        );
        assert!(intent_token_jaccard("create github issue", "list pokemon forms") < 0.35);
    }
}
