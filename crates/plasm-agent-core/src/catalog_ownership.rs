//! Federated catalog ownership: resolve `(entry_id, entity)` without blind `session.entry_id` fallback.

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::QualifiedEntityKey;
use plasm_core::{FederationDispatch, CGS};

/// Build the same [`CatalogResolver`] / [`FederationDispatch`] used by type-check and live execute.
pub(crate) fn federation_for_session(session: &ExecuteSession) -> FederationDispatch {
    if let Some(exp) = session.teaching_exposure.as_ref() {
        FederationDispatch::from_contexts_and_exposure(session.contexts_by_entry.clone(), exp)
    } else {
        FederationDispatch::from_contexts_only(session.contexts_by_entry.clone())
    }
}

/// Owning registry row + CGS entity name for dispatch and plan `qualified_entity`.
pub(crate) fn resolve_qualified_entity_key(
    session: &ExecuteSession,
    entity: &str,
    resolving_cgs: Option<&CGS>,
) -> Result<QualifiedEntityKey, String> {
    if session.contexts_by_entry.len() <= 1 {
        if session.cgs.entities.contains_key(entity) {
            return Ok(QualifiedEntityKey {
                entry_id: session.entry_id.clone(),
                entity: entity.to_string(),
            });
        }
        return Err(format!(
            "entity `{entity}` is not defined in any catalog loaded in this session"
        ));
    }
    let fed = federation_for_session(session);
    fed.resolve_qualified_entity_key(
        entity,
        resolving_cgs,
        session.cgs.as_ref(),
        session.entry_id.as_str(),
    )
    .map(QualifiedEntityKey::from)
    .map_err(|e| e.to_string())
}

/// Resolve CGS for schema/type-check with federation doctrine.
pub(crate) fn resolve_cgs_for_entity<'a>(
    session: &'a ExecuteSession,
    entity: &str,
    owning_cgs: Option<&CGS>,
) -> Result<&'a CGS, String> {
    if session.contexts_by_entry.len() <= 1 {
        if session.cgs.entities.contains_key(entity) {
            return Ok(session.cgs.as_ref());
        }
        return Err(format!(
            "entity `{entity}` is not defined in any catalog loaded in this session"
        ));
    }
    let qe = resolve_qualified_entity_key(session, entity, owning_cgs)?;
    session
        .contexts_by_entry
        .get(&qe.entry_id)
        .map(|ctx| ctx.cgs.as_ref())
        .ok_or_else(|| {
            format!(
                "entity `{entity}` resolved to catalog {:?} which is not loaded",
                qe.entry_id
            )
        })
}

/// Resolve CGS for a plan/materialized node that already stamps catalog ownership.
pub(crate) fn resolve_cgs_for_entry_entity<'a>(
    session: &'a ExecuteSession,
    entry_id: &str,
    entity: &str,
) -> Result<&'a CGS, String> {
    if let Some(ctx) = session.contexts_by_entry.get(entry_id) {
        if ctx.cgs.entities.contains_key(entity) {
            return Ok(ctx.cgs.as_ref());
        }
    }
    if session.contexts_by_entry.len() <= 1
        && session.entry_id == entry_id
        && session.cgs.entities.contains_key(entity)
    {
        return Ok(session.cgs.as_ref());
    }
    Err(format!(
        "entity `{entity}` is not present under catalog `{entry_id}` in this session"
    ))
}

/// Re-export for in-crate callers that already import `catalog_ownership`.
pub(crate) use crate::http_backend::plan_http_origin;

/// Trace/metadata helper: never panics; falls back to primary `entry_id` only when entity exists there.
pub(crate) fn entry_id_for_entity_trace(session: &ExecuteSession, entity: &str) -> String {
    resolve_qualified_entity_key(session, entity, None)
        .map(|qe| qe.entry_id)
        .unwrap_or_else(|_| session.entry_id.clone())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use indexmap::IndexMap;
    use plasm_core::{load_schema, CgsContext, TeachingExposureSession, CGS};

    use crate::http_backend::{CatalogHttpBackend, ReplHttpOverride, ResolvedHttpOrigin};

    use super::*;

    fn session_with_contexts(
        primary_id: &str,
        primary: Arc<CGS>,
        extra: Vec<(&str, Arc<CGS>)>,
        exposure: Option<TeachingExposureSession>,
    ) -> ExecuteSession {
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            primary_id.into(),
            Arc::new(CgsContext::entry(primary_id, primary.clone())),
        );
        for (id, cgs) in extra {
            ctxs.insert(id.into(), Arc::new(CgsContext::entry(id, cgs)));
        }
        let entities: Vec<String> = exposure
            .as_ref()
            .map(|e| e.entities.clone())
            .unwrap_or_default();
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            primary.clone(),
            ctxs,
            primary_id.into(),
            String::new(),
            String::new(),
            None,
            entities,
            exposure,
            None,
            primary.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    fn matrix_cgs() -> Arc<CGS> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Arc::new(
            load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix"))
                .expect("load plasm_language_matrix"),
        )
    }

    #[test]
    fn resolves_from_exposure_first() {
        let cgs = matrix_cgs();
        let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        let session = session_with_contexts("github", cgs, vec![], Some(exp));
        let qe = resolve_qualified_entity_key(&session, "LangItem", None).expect("qe");
        assert_eq!(qe.entry_id, "github");
        assert_eq!(qe.entity, "LangItem");
    }

    #[test]
    fn resolves_unexposed_entity_via_resolving_cgs() {
        let cgs_primary = matrix_cgs();
        let cgs_secondary = matrix_cgs();
        let exp = TeachingExposureSession::new(cgs_primary.as_ref(), "github", &["LangItem"]);
        let mut exp = exp;
        let layers: Vec<&CGS> = vec![cgs_primary.as_ref(), cgs_secondary.as_ref()];
        exp.expose_entities(&layers, cgs_secondary.clone(), "linear", &["LangLine"]);
        let session = session_with_contexts(
            "github",
            cgs_primary,
            vec![("linear", cgs_secondary)],
            Some(exp),
        );
        let qe = resolve_qualified_entity_key(
            &session,
            "LangDetail",
            Some(
                session
                    .contexts_by_entry
                    .get("linear")
                    .unwrap()
                    .cgs
                    .as_ref(),
            ),
        )
        .expect("qe");
        assert_eq!(qe.entry_id, "linear");
    }

    #[test]
    fn resolve_cgs_for_entity_single_catalog() {
        let cgs = matrix_cgs();
        let session = session_with_contexts("solo", cgs, vec![], None);
        let got = resolve_cgs_for_entity(&session, "LangItem", None).expect("ok");
        assert!(got.entities.contains_key("LangItem"));
    }

    #[test]
    fn resolve_cgs_for_entry_entity_federated_homonym() {
        let cgs = matrix_cgs();
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
        let session = session_with_contexts(
            "github",
            cgs.clone(),
            vec![("linear", cgs.clone())],
            Some(exp),
        );
        assert!(resolve_cgs_for_entity(&session, "LangItem", None).is_err());
        assert!(resolve_cgs_for_entry_entity(&session, "linear", "LangItem").is_ok());
        assert!(resolve_cgs_for_entry_entity(&session, "github", "LangItem").is_ok());
    }

    #[test]
    fn plan_http_origin_prefers_engine_over_schema_placeholder() {
        let engine =
            ReplHttpOverride::from_cli_normalized("http://127.0.0.1:8765").expect("engine");
        let placeholder = CatalogHttpBackend::from_cgs_field("http://127.0.0.1:9");
        assert_eq!(
            plan_http_origin(Some(&engine), Some(&placeholder))
                .as_ref()
                .map(ResolvedHttpOrigin::as_str),
            Some("http://127.0.0.1:8765")
        );
        let concrete = CatalogHttpBackend::from_cgs_field("https://api.example.com");
        assert_eq!(
            plan_http_origin(Some(&engine), Some(&concrete))
                .as_ref()
                .map(ResolvedHttpOrigin::as_str),
            Some("https://api.example.com")
        );
    }

    #[test]
    fn errors_when_entity_missing() {
        let cgs = matrix_cgs();
        let session = session_with_contexts("solo", cgs, vec![], None);
        let err = resolve_qualified_entity_key(&session, "Missing", None).expect_err("err");
        assert!(err.contains("not defined"));
    }

    #[test]
    fn errors_on_ambiguous_entity_name() {
        let cgs = matrix_cgs();
        let session = session_with_contexts("aaa", cgs.clone(), vec![("bbb", cgs)], None);
        let err = resolve_qualified_entity_key(&session, "LangItem", None).expect_err("ambiguous");
        assert!(err.contains("ambiguous"));
    }
}
