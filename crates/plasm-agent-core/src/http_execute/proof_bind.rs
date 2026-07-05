//! Proof catalog session bind helpers (share token / base token).

use indexmap::IndexMap;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{Value, CGS};
use plasm_runtime::{ExecutionResult, ExecutionSource, ExecutionStats};
use url::Url;

use crate::execute_session::ExecuteSession;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) enum ProofBindError {
    Bind(String),
}

fn trim_json_string_field(map: &IndexMap<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| match v {
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        _ => None,
    })
}

fn parse_proof_share_link_url(raw: &str) -> Result<(String, Option<String>), ProofBindError> {
    let u = Url::parse(raw.trim()).map_err(|e| {
        ProofBindError::Bind(format!("document_share_bind: invalid share_url ({e})"))
    })?;
    let segments: Vec<&str> = u.path_segments().map(|s| s.collect()).unwrap_or_default();
    let slug = segments
        .iter()
        .position(|seg| *seg == "d")
        .and_then(|i| segments.get(i + 1).copied())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ProofBindError::Bind(
                "document_share_bind: share_url path must include `/d/{slug}`".into(),
            )
        })?;
    let token = u
        .query_pairs()
        .find(|(k, _)| k == "token")
        .map(|(_, v)| v.into_owned())
        .and_then(|t| {
            let tr = t.trim();
            if tr.is_empty() {
                None
            } else {
                Some(tr.to_string())
            }
        });
    Ok((slug.to_string(), token))
}

fn proof_catalog_exec_session(sess: &ExecuteSession, exec_cgs: &CGS) -> bool {
    sess.entry_id.as_str() == "proof"
        || exec_cgs.entry_id.as_deref().is_some_and(|id| id == "proof")
}

fn proof_base_token_from_execution_result(result: &ExecutionResult) -> Option<String> {
    let entity = result.entities.first()?;
    let tfv = entity.fields.get("base_token")?;
    match tfv.to_value() {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        _ => None,
    }
}

/// After a successful `editor_state_get`, persist `baseToken` for CML.
pub(crate) async fn maybe_proof_refresh_session_base_token(
    sess: &ExecuteSession,
    exec_cgs: &CGS,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
) -> bool {
    if !proof_catalog_exec_session(sess, exec_cgs) {
        return false;
    }
    let plasm_core::Expr::Get(get) = &parsed.expr else {
        return false;
    };
    if get.reference.entity_type.as_str() != "EditorState" {
        return false;
    }
    let Some(tok) = proof_base_token_from_execution_result(result) else {
        return false;
    };
    let mut slot = sess.session_proof_base_token.write().await;
    *slot = Some(tok);
    true
}

/// Proof-only session bind: stores share token on [`ExecuteSession`] (no HTTP).
pub(crate) async fn try_proof_document_share_bind(
    sess: &ExecuteSession,
    exec_cgs: &CGS,
    expr: &plasm_core::Expr,
) -> Result<Option<ExecutionResult>, ProofBindError> {
    let plasm_core::Expr::Invoke(invoke) = expr else {
        return Ok(None);
    };
    if invoke.capability.as_str() != "document_share_bind" {
        return Ok(None);
    }
    if exec_cgs.get_capability("document_share_bind").is_none() {
        return Ok(None);
    }
    if invoke.target.entity_type.as_str() != "Document" {
        return Err(ProofBindError::Bind(
            "document_share_bind applies only to Document".into(),
        ));
    }
    let slug = invoke.target.primary_slot_str();
    let Some(inp) = invoke.input.as_ref() else {
        return Err(ProofBindError::Bind(
            "document_share_bind: pass share_url and/or share_token".into(),
        ));
    };
    let map = match inp.to_value() {
        Value::Object(m) => m,
        _ => {
            return Err(ProofBindError::Bind(
                "document_share_bind: arguments must be an object".into(),
            ));
        }
    };
    let share_url = trim_json_string_field(&map, "share_url");
    let share_token_arg = trim_json_string_field(&map, "share_token");

    let token = match (share_url, share_token_arg) {
        (Some(url), explicit_tok) => {
            let (url_slug, url_tok) = parse_proof_share_link_url(&url)?;
            if url_slug != slug {
                return Err(ProofBindError::Bind(format!(
                    "document_share_bind: share_url slug `{url_slug}` does not match Document `{slug}`"
                )));
            }
            explicit_tok.or(url_tok).ok_or_else(|| {
                ProofBindError::Bind(
                    "document_share_bind: missing token — add `?token=` to share_url or pass share_token="
                        .into(),
                )
            })?
        }
        (None, Some(tok)) => tok,
        (None, None) => {
            return Err(ProofBindError::Bind(
                "document_share_bind: provide share_url or share_token".into(),
            ));
        }
    };

    {
        let mut slot = sess.session_share_token.write().await;
        *slot = Some(token);
    }
    {
        let mut bt = sess.session_proof_base_token.write().await;
        *bt = None;
    }

    Ok(Some(ExecutionResult {
        entities: vec![],
        count: 0,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Live,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::try_proof_document_share_bind;
    use crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistOutcome;
    use crate::test_support::proof_bind_fixtures::{
        credential_snapshot, merge_durable_credentials_into_hot, rehydrate_proof_session,
        ProofBindFixture,
    };

    #[tokio::test]
    async fn restore_bind_credentials_assigns_and_clears_both_slots() {
        let fx = ProofBindFixture::open("restore_clear");
        *fx.session.session_share_token.write().await = Some("stale-share".into());
        *fx.session.session_proof_base_token.write().await = Some("stale-base".into());
        fx.session
            .restore_bind_credentials(&credential_snapshot(None, None))
            .await;
        assert!(fx.session.session_share_token.read().await.is_none());
        assert!(fx.session.session_proof_base_token.read().await.is_none());

        fx.session
            .restore_bind_credentials(&credential_snapshot(Some("share"), Some("base")))
            .await;
        assert_eq!(
            fx.session.session_share_token.read().await.as_deref(),
            Some("share")
        );
        assert_eq!(
            fx.session.session_proof_base_token.read().await.as_deref(),
            Some("base")
        );
    }

    #[tokio::test]
    async fn token_only_document_share_bind_survives_rehydrate() {
        let fx = ProofBindFixture::open("bind_rehydrate");
        let session_id = "sid1";
        try_proof_document_share_bind(&fx.session, fx.cgs.as_ref(), &fx.token_only_bind_expr())
            .await
            .expect("bind")
            .expect("bind intercept");

        fx.registry
            .patch_bind_credentials(&fx.session, session_id, Some(&fx.reuse_key))
            .await
            .expect("patch bind credentials");

        let host = fx.host_with_registry();
        let rehydrated = rehydrate_proof_session(&host, &fx, session_id).await;
        assert_eq!(
            rehydrated.session_share_token.read().await.as_deref(),
            Some("secret-tok")
        );
    }

    #[tokio::test]
    async fn merge_into_live_session_clears_stale_hot_credentials_from_durable() {
        let fx = ProofBindFixture::open("merge_clear");
        let session_id = "sid_merge";
        try_proof_document_share_bind(&fx.session, fx.cgs.as_ref(), &fx.token_only_bind_expr())
            .await
            .expect("bind")
            .expect("bind intercept");
        fx.registry
            .patch_bind_credentials(&fx.session, session_id, Some(&fx.reuse_key))
            .await
            .expect("patch bind");

        *fx.session.session_share_token.write().await = None;
        *fx.session.session_proof_base_token.write().await = None;
        fx.registry
            .patch_bind_credentials(&fx.session, session_id, Some(&fx.reuse_key))
            .await
            .expect("patch cleared credentials");

        *fx.session.session_share_token.write().await = Some("stale-hot".into());
        *fx.session.session_proof_base_token.write().await = Some("stale-base".into());
        merge_durable_credentials_into_hot(
            &fx.registry,
            &fx.session,
            fx.session.prompt_hash.as_str(),
            session_id,
        )
        .await;
        assert!(fx.session.session_share_token.read().await.is_none());
        assert!(fx.session.session_proof_base_token.read().await.is_none());
    }

    #[tokio::test]
    async fn patch_bind_credentials_upserts_when_no_durable_row_yet() {
        let fx = ProofBindFixture::open("bind_upsert");
        *fx.session.session_share_token.write().await = Some("early-tok".into());
        let session_id = "sid_new";
        let outcome = fx
            .registry
            .patch_bind_credentials(&fx.session, session_id, Some(&fx.reuse_key))
            .await
            .expect("upsert bind credentials");
        assert_eq!(outcome, ExecuteSessionPersistOutcome::Durable);
        let desc = fx
            .registry
            .load(fx.session.prompt_hash.as_str(), session_id)
            .await
            .expect("descriptor after upsert");
        assert_eq!(desc.session_share_token.as_deref(), Some("early-tok"));
    }

    #[tokio::test]
    async fn plan_build_exec_opts_reads_fresh_share_token_after_mid_plan_bind() {
        use std::sync::{Arc, Mutex};

        use crate::plan_execute_shared::PlanLineExecuteShared;

        let fx = ProofBindFixture::open("plan_share_refresh");
        let st = fx.host_with_registry();
        let shared = PlanLineExecuteShared::prepare(&fx.session, &st, "sid_plan").await;
        let fp_sink = Arc::new(Mutex::new(Vec::<String>::new()));

        let before = shared
            .build_exec_opts(
                &fx.session,
                &st,
                fx.cgs.as_ref(),
                "Document",
                fp_sink.clone(),
                plasm_core::PreflightToken::VERIFIED,
                None,
            )
            .await;
        assert!(
            before
                .execute_session
                .as_ref()
                .and_then(|m| m.share_token.as_deref())
                .is_none(),
            "expected no share token before bind"
        );

        try_proof_document_share_bind(&fx.session, fx.cgs.as_ref(), &fx.token_only_bind_expr())
            .await
            .expect("bind")
            .expect("bind intercept");

        let after = shared
            .build_exec_opts(
                &fx.session,
                &st,
                fx.cgs.as_ref(),
                "Document",
                fp_sink,
                plasm_core::PreflightToken::VERIFIED,
                None,
            )
            .await;
        assert_eq!(
            after
                .execute_session
                .as_ref()
                .and_then(|m| m.share_token.as_deref()),
            Some("secret-tok"),
            "plan line after bind must see fresh session share token"
        );
    }
}
