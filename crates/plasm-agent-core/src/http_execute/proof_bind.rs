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
) {
    if !proof_catalog_exec_session(sess, exec_cgs) {
        return;
    }
    let plasm_core::Expr::Get(get) = &parsed.expr else {
        return;
    };
    if get.reference.entity_type.as_str() != "EditorState" {
        return;
    }
    let Some(tok) = proof_base_token_from_execution_result(result) else {
        return;
    };
    let mut slot = sess.session_proof_base_token.write().await;
    *slot = Some(tok);
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
