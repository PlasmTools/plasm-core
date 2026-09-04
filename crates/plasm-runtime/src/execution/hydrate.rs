//! Implicit GET after a summary query, and capability-param inheritance onto
//! synthesized identity GETs.
//!
//! A GET is a continuation of the parent fetch's capability-parameter scope, not a
//! new identity-only program. Domain parameters may arrive via [`CapabilityParamEnv`],
//! never via `ViewAmbientContext`; transport credentials remain in [`AuthResolver`].

use super::*;
use plasm_core::{CapabilityKind, CapabilitySchema};

/// Capability-parameter bindings inherited from a parent query (or stamped row)
/// onto a synthesized GET.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CapabilityParamEnv {
    bindings: IndexMap<String, Value>,
}

impl CapabilityParamEnv {
    pub(crate) fn bindings(&self) -> &IndexMap<String, Value> {
        &self.bindings
    }

    pub(crate) fn into_bindings(self) -> IndexMap<String, Value> {
        self.bindings
    }

    /// Keep parent env keys that the target capability declares as parameters.
    pub(crate) fn from_cml_env(env: &CmlEnv, cap: &CapabilitySchema) -> Self {
        Self::intersect(env, &capability_param_name_set(cap))
    }

    pub(crate) fn from_bindings(
        bindings: &IndexMap<String, Value>,
        cap: &CapabilitySchema,
    ) -> Self {
        let names = capability_param_name_set(cap);
        let mut out = IndexMap::new();
        for (k, v) in bindings {
            if names.contains(k) && !matches!(v, Value::PlasmInputRef(_)) {
                out.insert(k.clone(), v.clone());
            }
        }
        Self { bindings: out }
    }

    pub(crate) fn from_source_row(
        cgs: &CGS,
        target_entity: &str,
        mat: &SessionMaterialization,
        source: &CachedEntity,
    ) -> Self {
        let Some(cap) = cgs.find_capability(target_entity, CapabilityKind::Get) else {
            return Self::default();
        };
        Self::from_bindings(&mat.capability_params_for(&source.reference), cap)
    }

    /// Parent query env intersected with the entity's GET capability (if any).
    pub(crate) fn for_entity_get(cgs: &CGS, entity: &str, env: &CmlEnv) -> Self {
        match cgs.find_capability(entity, CapabilityKind::Get) {
            Some(get) => Self::from_cml_env(env, get),
            None => Self::default(),
        }
    }

    fn intersect(env: &CmlEnv, names: &HashSet<String>) -> Self {
        let mut bindings = IndexMap::new();
        for name in names {
            if let Some(v) = env.get(name) {
                if !matches!(v, Value::PlasmInputRef(_)) {
                    bindings.insert(name.clone(), v.clone());
                }
            }
        }
        Self { bindings }
    }

    pub(crate) fn as_path_vars(&self) -> Option<IndexMap<String, Value>> {
        if self.bindings.is_empty() {
            None
        } else {
            Some(self.bindings.clone())
        }
    }

    /// Required capability params that are neither identity slots nor inherited.
    pub(crate) fn missing_required(
        &self,
        cap: &CapabilitySchema,
        identity_keys: &HashSet<String>,
    ) -> Vec<String> {
        required_capability_param_names(cap)
            .into_iter()
            .filter(|n| !identity_keys.contains(n) && !self.bindings.contains_key(n))
            .collect()
    }
}

pub(crate) fn synthesized_get(reference: Ref, params: &CapabilityParamEnv) -> GetExpr {
    GetExpr::from_ref_with_path_vars(reference, params.as_path_vars())
}

/// Overlay session-stamped capability params onto a GET. Explicit `path_vars` win.
pub(crate) fn get_with_session_params(
    get: &GetExpr,
    cgs: &CGS,
    mat: &SessionMaterialization,
) -> GetExpr {
    let cap = get
        .capability_name
        .as_deref()
        .and_then(|n| cgs.get_capability(n))
        .or_else(|| cgs.find_capability(&get.reference.entity_type, CapabilityKind::Get));
    let Some(cap) = cap else {
        return get.clone();
    };
    let inherit =
        CapabilityParamEnv::from_bindings(&mat.capability_params_for(&get.reference), cap);
    let mut merged = inherit.into_bindings();
    if let Some(explicit) = &get.path_vars {
        for (k, v) in explicit {
            merged.insert(k.clone(), v.clone());
        }
    }
    let mut out = get.clone();
    out.path_vars = if merged.is_empty() {
        None
    } else {
        Some(merged)
    };
    out
}

pub(crate) fn identity_keys_for_entity(cgs: &CGS, entity: &str) -> HashSet<String> {
    let mut keys = HashSet::from(["id".to_string()]);
    if let Some(ent) = cgs.get_entity(entity) {
        keys.insert(ent.id_field.to_string());
        for k in &ent.key_vars {
            keys.insert(k.to_string());
        }
    }
    keys
}

pub(crate) fn stamp_entities_and_mat(
    entities: &[CachedEntity],
    mat: &mut SessionMaterialization,
    inherit: &CapabilityParamEnv,
) {
    let bindings = inherit.bindings().clone();
    if bindings.is_empty() {
        return;
    }
    for e in entities {
        mat.stamp_capability_params(&e.reference, bindings.clone());
    }
}

pub(crate) fn wrap_synthesized_get_error(
    cap_name: &str,
    entity_type: &str,
    err: RuntimeError,
) -> RuntimeError {
    RuntimeError::HydrationGet {
        cap_name: cap_name.to_string(),
        entity_type: entity_type.to_string(),
        source: Box::new(err),
    }
}

fn capability_param_name_set(capability: &CapabilitySchema) -> HashSet<String> {
    capability_param_names(capability)
}

fn required_capability_param_names(capability: &CapabilitySchema) -> Vec<String> {
    capability
        .input_fields()
        .filter(|f| f.required)
        .map(|f| f.name.clone())
        .collect()
}

impl ExecutionEngine {
    /// After a query, upgrade Summary rows via concurrent GET when configured and the
    /// GET's required capability params are inherited from the parent env.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn hydrate_query_summaries(
        &self,
        entity_type: &str,
        ordered_entities: &[CachedEntity],
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        hydrate_enabled: bool,
        parent_env: &CmlEnv,
    ) -> Result<(Vec<CachedEntity>, usize), RuntimeError> {
        if !hydrate_enabled {
            return Ok((ordered_entities.to_vec(), 0));
        }
        let Some(get_cap) = cgs.find_capability(entity_type, CapabilityKind::Get) else {
            return Ok((ordered_entities.to_vec(), 0));
        };

        // List + derived/view-backed get: hydrating each summary row would re-enter the
        // same list-backed Get (query → get → query → …). Skip.
        if get_cap.is_list_backed_get(cgs) {
            tracing::debug!(
                entity = %entity_type,
                capability = %get_cap.name,
                event = "hydrate_skipped_list_backed_get"
            );
            return Ok((ordered_entities.to_vec(), 0));
        }

        let inherit = CapabilityParamEnv::from_cml_env(parent_env, get_cap);
        let identity = identity_keys_for_entity(cgs, entity_type);
        let missing = inherit.missing_required(get_cap, &identity);
        if !missing.is_empty() {
            tracing::debug!(
                entity = %entity_type,
                capability = %get_cap.name,
                missing = ?missing,
                event = "hydrate_skipped_missing_params"
            );
            return Ok((ordered_entities.to_vec(), 0));
        }

        let ordered_refs: Vec<Ref> = ordered_entities
            .iter()
            .map(|e| e.reference.clone())
            .collect();

        let to_fetch: Vec<Ref> = ordered_refs
            .iter()
            .filter(|r| {
                !matches!(
                    mat.get(r).map(|e| e.completeness),
                    Some(EntityCompleteness::Complete)
                )
            })
            .cloned()
            .collect();

        let concurrency = self.config.hydrate_concurrency.max(1);
        let mut extra_network = 0usize;
        let cap_name = get_cap.name.clone();

        use futures_util::stream::{self, StreamExt};

        let mut stream = stream::iter(to_fetch.into_iter().map(|reference| {
            let get = synthesized_get(reference, &inherit);
            let cap_name = cap_name.clone();
            async move {
                self.fetch_get_decoded(
                    &get,
                    cgs,
                    mode,
                    None,
                    false,
                    None,
                    &ViewAmbientContext::default(),
                )
                .await
                .map_err(|e| wrap_synthesized_get_error(cap_name.as_str(), entity_type, e))
            }
        }))
        .buffer_unordered(concurrency);

        while let Some(res) = stream.next().await {
            cooperative_cancel_check()?;
            let (entity, source) = res?;
            if source == ExecutionSource::Live {
                extra_network += 1;
            }
            mat.insert(entity)?;
        }

        let mut out = Vec::with_capacity(ordered_refs.len());
        for r in &ordered_refs {
            let e = mat.get(r).ok_or_else(|| RuntimeError::CacheError {
                message: format!("entity missing after query/hydrate: {}", r),
            })?;
            out.push(e.clone());
        }
        Ok((out, extra_network))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &[(&str, &str)]) -> CmlEnv {
        let mut env = CmlEnv::new();
        for (k, v) in pairs {
            env.insert((*k).to_string(), Value::String((*v).to_string()));
        }
        env
    }

    #[test]
    fn intersect_keeps_named_params_and_skips_holes() {
        let mut env = env_with(&[("access_token", "tok"), ("query", "trip")]);
        env.insert(
            "hole".to_string(),
            Value::PlasmInputRef(plasm_core::value::PlasmInputRef::node_output(
                "sn",
                vec!["access_token".into()],
            )),
        );
        let names: HashSet<String> = ["access_token", "hole"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let inherited = CapabilityParamEnv::intersect(&env, &names);
        assert_eq!(
            inherited.bindings().get("access_token"),
            Some(&Value::String("tok".into()))
        );
        assert!(!inherited.bindings().contains_key("query"));
        assert!(!inherited.bindings().contains_key("hole"));
    }

    #[test]
    fn missing_required_ignores_identity_slots() {
        let inherit = CapabilityParamEnv {
            bindings: IndexMap::from([("access_token".into(), Value::String("tok".into()))]),
        };
        // Simulate required {access_token, note_id} with note_id as identity.
        let required = ["access_token", "note_id"];
        let identity: HashSet<String> = HashSet::from(["note_id".into(), "id".into()]);
        let missing: Vec<String> = required
            .into_iter()
            .map(str::to_string)
            .filter(|n| !identity.contains(n) && !inherit.bindings.contains_key(n))
            .collect();
        assert!(missing.is_empty());
        let empty = CapabilityParamEnv::default();
        let missing_tok: Vec<String> = required
            .into_iter()
            .map(str::to_string)
            .filter(|n| !identity.contains(n) && !empty.bindings.contains_key(n))
            .collect();
        assert_eq!(missing_tok, vec!["access_token".to_string()]);
    }

    #[test]
    fn synthesized_get_stamps_path_vars() {
        let inherit = CapabilityParamEnv {
            bindings: IndexMap::from([("access_token".into(), Value::String("tok".into()))]),
        };
        let get = synthesized_get(Ref::new("LangSecuredNote", "1"), &inherit);
        let pv = get.path_vars.expect("path_vars");
        assert_eq!(pv.get("access_token"), Some(&Value::String("tok".into())));
    }

    #[test]
    fn session_params_overlay_explicit_wins() {
        use plasm_core::loader::load_schema_dir;
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let reference = Ref::new("LangSecuredNote", "1");
        let mut mat = SessionMaterialization::new();
        mat.stamp_capability_params(
            &reference,
            IndexMap::from([("access_token".into(), Value::String("session".into()))]),
        );
        let inherited = get_with_session_params(&GetExpr::from_ref(reference.clone()), &cgs, &mat);
        assert_eq!(
            inherited
                .path_vars
                .as_ref()
                .and_then(|p| p.get("access_token")),
            Some(&Value::String("session".into()))
        );
        let mut explicit = IndexMap::new();
        explicit.insert("access_token".into(), Value::String("program".into()));
        let over = get_with_session_params(
            &GetExpr::from_ref_with_path_vars(reference, Some(explicit)),
            &cgs,
            &mat,
        );
        assert_eq!(
            over.path_vars.as_ref().and_then(|p| p.get("access_token")),
            Some(&Value::String("program".into()))
        );
    }

    #[test]
    fn preflight_derived_get_skips_cml_and_does_not_panic() {
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, GetExpr, Ref};
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix_views"),
        )
        .expect("matrix views CGS");
        let cap = cgs
            .get_capability("lang_key_pick_get")
            .expect("derived get");
        assert!(cap.derived.is_some());
        let mapping_err = cap.require_mapping().expect_err("derived Get has no CML mapping");
        assert!(
            mapping_err.contains("lang_key_pick_get") && mapping_err.contains("derived"),
            "unexpected require_mapping err: {mapping_err}"
        );
        let mut get = GetExpr::from_ref(Ref::new("LangKeyPick", "alpha"));
        get.capability_name = Some("lang_key_pick_get".into());
        let mat = SessionMaterialization::new();
        preflight_compile_expr(
            &Expr::Get(get),
            &cgs,
            &ViewAmbientContext::default(),
            &mat,
        )
        .expect("derived Get preflight must succeed without CML mapping");
    }

    /// Live plan path that previously unwound with `require_mapping` panic on derived Gets.
    #[tokio::test]
    async fn derived_get_live_execute_does_not_panic() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, GetExpr, Ref};
        use std::sync::Arc;

        #[derive(Clone)]
        struct ListItemsTransport;

        #[async_trait]
        impl HttpTransport for ListItemsTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                assert!(
                    request.path.contains("items"),
                    "derived Get must fetch source list, got path {}",
                    request.path
                );
                Ok((
                    serde_json::json!([
                        {"id": "alpha", "title": "Alpha Item"},
                        {"id": "beta", "title": "Beta Item"}
                    ]),
                    None,
                ))
            }

            async fn get_json_absolute(
                &self,
                _url: &str,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                Ok((serde_json::json!({}), None))
            }
        }

        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix_views"),
        )
        .expect("matrix views CGS");
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(ListItemsTransport),
            None,
        );
        let mut get = GetExpr::from_ref(Ref::new("LangKeyPick", "alpha"));
        get.capability_name = Some("lang_key_pick_get".into());
        let mut mat = SessionMaterialization::new();
        preflight_compile_expr(
            &Expr::Get(get.clone()),
            &cgs,
            &ViewAmbientContext::default(),
            &mat,
        )
        .expect("preflight");
        let result = engine
            .execute_get(
                &get,
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                &ViewAmbientContext::default(),
            )
            .await
            .expect("derived Get live execute must return Result, not unwind");
        assert_eq!(result.entities.len(), 1);
        let row = &result.entities[0];
        assert_eq!(row.reference.primary_slot_str(), "alpha");
        assert_eq!(
            row.fields.get("title").map(|f| f.to_value()),
            Some(Value::String("Alpha Item".into()))
        );
    }

    #[test]
    fn preflight_identity_get_compiles_from_session_stamps() {
        use plasm_core::loader::load_schema_dir;
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let get = GetExpr::from_ref(Ref::new("LangSecuredNote", "1"));
        let empty = SessionMaterialization::new();
        let err = preflight_compile_expr(
            &Expr::Get(get.clone()),
            &cgs,
            &ViewAmbientContext::default(),
            &empty,
        )
        .expect_err("unstamped identity GET must fail CML compile");
        assert!(
            err.to_string().contains("access_token"),
            "expected access_token miss, got {err}"
        );
        let mut mat = SessionMaterialization::new();
        mat.stamp_capability_params(
            &get.reference,
            IndexMap::from([("access_token".into(), Value::String("tok".into()))]),
        );
        preflight_compile_expr(&Expr::Get(get), &cgs, &ViewAmbientContext::default(), &mat)
            .expect("session-stamped identity GET compiles");
    }

    #[tokio::test]
    async fn summary_search_hydrates_get_with_inherited_capability_params() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, Predicate, QueryExpr};
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct RecordingTransport {
            paths: Arc<Mutex<Vec<String>>>,
            auths: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl HttpTransport for RecordingTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                self.paths.lock().unwrap().push(request.path.clone());
                let auth = match &request.headers {
                    Some(Value::Object(m)) => match m.get("Authorization") {
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                self.auths.lock().unwrap().push(auth);
                if request.path.contains("/secured_notes/") {
                    Ok((
                        serde_json::json!({
                            "note_id": 1,
                            "title": "trip itinerary",
                            "body": "trip body"
                        }),
                        None,
                    ))
                } else {
                    Ok((
                        serde_json::json!([{
                            "note_id": 1,
                            "title": "trip itinerary"
                        }]),
                        None,
                    ))
                }
            }

            async fn get_json_absolute(
                &self,
                _url: &str,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                Ok((serde_json::json!({}), None))
            }
        }

        let paths = Arc::new(Mutex::new(Vec::new()));
        let auths = Arc::new(Mutex::new(Vec::new()));
        let transport = RecordingTransport {
            paths: paths.clone(),
            auths: auths.clone(),
        };
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://matrix.test".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(transport),
            None,
        );
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let mut query = QueryExpr::filtered(
            "LangSecuredNote",
            Predicate::and(vec![
                Predicate::eq("query", "trip"),
                Predicate::eq("access_token", "tok"),
            ]),
        );
        query.capability_name = Some("langsecurednote_search".into());
        let mut mat = SessionMaterialization::new();
        let result = engine
            .execute(
                &Expr::Query(query),
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::default(),
            )
            .await
            .expect("search+hydrate");

        let recorded = paths.lock().unwrap().clone();
        assert!(
            recorded.iter().any(|p| p.ends_with("/secured_notes")),
            "search path missing: {recorded:?}"
        );
        assert!(
            recorded.iter().any(|p| p.contains("/secured_notes/")),
            "hydrate GET path missing: {recorded:?}"
        );
        assert!(
            auths.lock().unwrap().iter().any(|a| a == "Bearer tok"),
            "inherited Bearer missing: {:?}",
            auths.lock().unwrap()
        );
        let body = result
            .entities
            .iter()
            .find_map(|e| e.fields.get("body").map(|f| f.to_value()));
        assert_eq!(body, Some(Value::String("trip body".into())));
    }

    #[tokio::test]
    async fn explicit_identity_get_inherits_session_capability_params() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, GetExpr, Predicate, QueryExpr};
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct RecordingTransport {
            paths: Arc<Mutex<Vec<String>>>,
            auths: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl HttpTransport for RecordingTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                self.paths.lock().unwrap().push(request.path.clone());
                let auth = match &request.headers {
                    Some(Value::Object(m)) => match m.get("Authorization") {
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                self.auths.lock().unwrap().push(auth);
                if request.path.contains("/secured_notes/") {
                    Ok((
                        serde_json::json!({
                            "note_id": 1,
                            "title": "trip itinerary",
                            "body": "trip body"
                        }),
                        None,
                    ))
                } else {
                    Ok((
                        serde_json::json!([{
                            "note_id": 1,
                            "title": "trip itinerary"
                        }]),
                        None,
                    ))
                }
            }

            async fn get_json_absolute(
                &self,
                _url: &str,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                Ok((serde_json::json!({}), None))
            }
        }

        let paths = Arc::new(Mutex::new(Vec::new()));
        let auths = Arc::new(Mutex::new(Vec::new()));
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://matrix.test".into()),
                hydrate: false,
                ..ExecutionConfig::default()
            },
            Arc::new(RecordingTransport {
                paths: paths.clone(),
                auths: auths.clone(),
            }),
            None,
        );
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let mut query = QueryExpr::filtered(
            "LangSecuredNote",
            Predicate::and(vec![
                Predicate::eq("query", "trip"),
                Predicate::eq("access_token", "tok"),
            ]),
        );
        query.capability_name = Some("langsecurednote_search".into());
        let mut mat = SessionMaterialization::new();
        let listed = engine
            .execute(
                &Expr::Query(query),
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::default(),
            )
            .await
            .expect("search without hydrate");
        let reference = listed.entities[0].reference.clone();

        paths.lock().unwrap().clear();
        auths.lock().unwrap().clear();
        let get = GetExpr::from_ref(reference);
        assert!(
            get.path_vars.is_none(),
            "explicit identity GET has no path_vars"
        );
        let got = engine
            .execute_get(
                &get,
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                &ViewAmbientContext::default(),
            )
            .await
            .expect("identity GET inherits session Bearer");
        assert!(
            paths
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("/secured_notes/")),
            "explicit GET path missing: {:?}",
            paths.lock().unwrap()
        );
        assert!(
            auths.lock().unwrap().iter().any(|a| a == "Bearer tok"),
            "explicit GET must inherit Bearer: {:?}",
            auths.lock().unwrap()
        );
        let body = got
            .entities
            .iter()
            .find_map(|e| e.fields.get("body").map(|f| f.to_value()));
        assert_eq!(body, Some(Value::String("trip body".into())));
    }
}
