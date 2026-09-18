//! Implicit GET after a summary query, and capability-param inheritance onto
//! synthesized identity GETs and scoped relation queries (RA-6).
//!
//! **RA-16 addressability:** current list-row `Ref`s stay resolvable *during*
//! hydrate (workspace). That is not permanent graph insertion of every scanned
//! row, not Completeness-as-freshness, and not a type-index update.
//!
//! TopK / RowMatch skip pre-page merge; a soft-failed or identity-divergent
//! detail GET must not emit `entity missing after query/hydrate`.
//!
//! A GET or `ℓ.r#` / `ℓ => _.r#` hop is a continuation of the parent fetch's
//! capability-parameter scope, not a new identity-only program. Unary Get is the
//! same RA-6 parent as Query: session / parent-row `request_auth` reaches `.r#`
//! when the child seat is the same provider or has no distinct foreign seat.
//! A name-shared parent token is omitted when RA-17 cannot prove catalog identity.
//! Domain parameters arrive via [`CapabilityParamEnv`]. Transport credentials
//! remain in [`AuthResolver`].

use super::*;
use plasm_core::prerequisites::{inherit_may_fill_param, CapabilityRef};
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
        Self::from_source_row_for_cap(cgs, mat, source, cap)
    }

    /// Parent-row stamps plus catalog session overlay, intersected with the child capability.
    ///
    /// RA-6: a unary Get is the same parent fetch as a Query — session / sibling-bound
    /// `request_auth` keys (e.g. `access_token`) must reach `ℓ.r#` when the child
    /// seat is the same provider or has no distinct foreign-provider seat.
    /// RA-17: a name-shared parent token is omitted when the child seat is
    /// deployed elsewhere, or inherit cannot prove catalog identity.
    pub(crate) fn from_source_row_for_cap(
        cgs: &CGS,
        mat: &SessionMaterialization,
        source: &CachedEntity,
        cap: &CapabilitySchema,
    ) -> Self {
        let catalog_key = SessionMaterialization::provide_catalog_key(cgs, None);
        let mut env = Self::from_bindings(
            &mat.capability_params_for_get(&source.reference, &catalog_key),
            cap,
        );
        env.omit_unproven_foreign_seats(cgs, cap, Some(catalog_key.as_str()), mat);
        env
    }

    fn omit_unproven_foreign_seats(
        &mut self,
        cgs: &CGS,
        cap: &CapabilitySchema,
        parent_catalog: Option<&str>,
        mat: &SessionMaterialization,
    ) {
        let child_catalog = cgs.entry_id.as_deref().unwrap_or("");
        let consumer = CapabilityRef {
            catalog: child_catalog.to_string(),
            capability: cap.name.as_str().to_string(),
        };
        let reqs = cgs
            .prerequisites
            .requirements
            .get(cap.name.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        self.bindings.retain(|name, _| {
            inherit_may_fill_param(
                &mat.prerequisite_deployments,
                &consumer,
                reqs,
                name,
                parent_catalog,
            )
        });
    }

    /// AND inherited selection onto a scoped child query. Relation-binding keys
    /// already present on the predicate are left untouched (RA-6).
    pub(crate) fn apply_to_scoped_query(&self, query: &mut QueryExpr) {
        if self.bindings.is_empty() {
            return;
        }
        let already: HashSet<String> = query
            .predicate
            .as_ref()
            .map(|p| p.referenced_fields().into_iter().collect())
            .unwrap_or_default();
        let extra: Vec<Predicate> = self
            .bindings
            .iter()
            .filter(|(k, _)| !already.contains(*k))
            .map(|(k, v)| Predicate::eq(k.clone(), v.clone()))
            .collect();
        if extra.is_empty() {
            return;
        }
        query.predicate = Some(match query.predicate.take() {
            None if extra.len() == 1 => extra.into_iter().next().expect("one extra pred"),
            None => Predicate::and(extra),
            Some(existing) => {
                let mut args = vec![existing];
                args.extend(extra);
                Predicate::and(args)
            }
        });
    }

    /// Parent fetch env intersected with the capability that produced the rows.
    ///
    /// Get params win when the entity has a Get. Query-only (then Search-only)
    /// parents keep that fetch's declared params so a later `ℓ.r#` hop does not
    /// drop parent-scoped credentials (RA-6). Unary Get parents stamp the same
    /// overlay so `.r#` inherits session / parent-row `request_auth`.
    pub(crate) fn for_entity_get(cgs: &CGS, entity: &str, env: &CmlEnv) -> Self {
        if let Some(get) = cgs.find_capability(entity, CapabilityKind::Get) {
            return Self::from_cml_env(env, get);
        }
        if let Some(query) = cgs.find_capability(entity, CapabilityKind::Query) {
            return Self::from_cml_env(env, query);
        }
        if let Some(search) = cgs.find_capability(entity, CapabilityKind::Search) {
            return Self::from_cml_env(env, search);
        }
        Self::default()
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

pub(crate) fn synthesized_get(reference: Ref, _params: &CapabilityParamEnv) -> GetExpr {
    GetExpr::from_ref(reference)
}

/// Session capability params are applied at CML env populate time from materialization —
/// identity lives on [`Ref`] only (IdentitySlot cutover).
pub(crate) fn get_with_session_params(
    get: &GetExpr,
    _cgs: &CGS,
    _mat: &SessionMaterialization,
) -> GetExpr {
    get.clone()
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

fn get_capability_for_stamp<'a>(get: &GetExpr, cgs: &'a CGS) -> Option<&'a CapabilitySchema> {
    match get.capability_name.as_deref() {
        Some(name) => cgs
            .get_capability(name)
            .filter(|c| c.kind == CapabilityKind::Get),
        None => cgs.find_capability(&get.reference.entity_type, CapabilityKind::Get),
    }
}

/// Stamp the overlay a unary Get actually used (session + per-ref + ambient)
/// onto the resulting row so a later `ℓ.r#` hop inherits RA-6 request_auth.
pub(crate) fn stamp_get_capability_params(
    mat: &mut SessionMaterialization,
    cgs: &CGS,
    get: &GetExpr,
    ambient: &ViewAmbientContext,
    row: &CachedEntity,
) {
    let Some(cap) = get_capability_for_stamp(get, cgs) else {
        return;
    };
    let catalog_key =
        SessionMaterialization::provide_catalog_key(cgs, get.catalog_entry_id.as_deref());
    let mut overlay = mat.capability_params_for_get(&get.reference, &catalog_key);
    for (k, v) in &ambient.capability_params {
        overlay.entry(k.clone()).or_insert_with(|| v.clone());
    }
    let inherit = CapabilityParamEnv::from_bindings(&overlay, cap);
    stamp_entities_and_mat(std::slice::from_ref(row), mat, &inherit);
}

/// Parent-row + session inherit for a scoped relation query, or a diagnostic
/// that names each missing required env key.
pub(crate) fn relation_inherit_for_scoped_query(
    cgs: &CGS,
    mat: &SessionMaterialization,
    parent: &CachedEntity,
    cap: &CapabilitySchema,
    query: &QueryExpr,
) -> Result<CapabilityParamEnv, RuntimeError> {
    let inherit = CapabilityParamEnv::from_source_row_for_cap(cgs, mat, parent, cap);
    let mut skip = identity_keys_for_entity(cgs, cap.domain.as_str());
    if let Some(pred) = query.predicate.as_ref() {
        skip.extend(pred.referenced_fields());
    }
    let missing = inherit.missing_required(cap, &skip);
    if missing.is_empty() {
        return Ok(inherit);
    }
    Err(RuntimeError::ConfigurationError {
        message: format!(
            "relation hop missing {}: inherit from parent Get / session Bearer (catalog request_auth)",
            missing.join(", ")
        ),
    })
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

/// Fields a detail GET would supply that are still missing/null on the summary row.
fn detail_fields_missing_after_soft_fail(
    entity: &CachedEntity,
    get_cap: &CapabilitySchema,
    cgs: &CGS,
) -> Vec<String> {
    let identity = identity_keys_for_entity(cgs, entity.reference.entity_type.as_str());
    cgs.effective_provides(get_cap)
        .into_iter()
        .filter(|f| !identity.contains(f))
        .filter(|f| {
            entity
                .fields
                .get(f.as_str())
                .map(|v| v.is_null())
                .unwrap_or(true)
        })
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

        // Addressability (RA-16): current list rows are resolvable for this call
        // without permanently inserting every scanned Ref. Heap / Standard merge
        // retain winners; graph memory must not grow with input size.
        let mut workspace: std::collections::HashMap<Ref, CachedEntity> = ordered_entities
            .iter()
            .map(|e| (e.reference.clone(), e.clone()))
            .collect();

        // Freshness: Completeness is field coverage of an observation, not
        // post-write validity. After recorded-read invalidation, the list
        // observation replaces an *already-cached* row. New scanned refs stay
        // workspace-only until the collector/finish merge.
        if !mat.allows_recorded_read_reuse() {
            for entity in ordered_entities {
                if mat.get(&entity.reference).is_some() {
                    mat.publish_fresh_row(entity.clone())?;
                }
            }
        }

        let ordered_refs: Vec<Ref> = ordered_entities
            .iter()
            .map(|e| e.reference.clone())
            .collect();

        // Skip GET from the *list* observation's field coverage — not graph
        // Completeness, which would treat a stale Complete row as fresh.
        let to_fetch: Vec<Ref> = ordered_entities
            .iter()
            .filter(|e| e.completeness != EntityCompleteness::Complete)
            .map(|e| e.reference.clone())
            .collect();

        let concurrency = self
            .config
            .effective_hydrate_concurrency(cgs.entry_id.as_deref());
        let mut extra_network = 0usize;
        let cap_name = get_cap.name.clone();

        use futures_util::stream::{self, StreamExt};

        let mut stream = stream::iter(to_fetch.into_iter().map(|reference| {
            let requested = reference.clone();
            let get = synthesized_get(reference, &inherit);
            let cap_name = cap_name.clone();
            let ambient =
                ViewAmbientContext::default().with_capability_params(inherit.bindings().clone());
            async move {
                let attempt = hydration_trace::next_id();
                let fetch = self.fetch_get_decoded(&get, cgs, mode, None, false, None, &ambient);
                let result = if let Some(id) = attempt {
                    hydration_trace::emit_for(id, "start", serde_json::json!({"entity":entity_type,
                        "capability":cap_name.as_str(), "identity_digest":blake3::hash(requested.to_string().as_bytes()).to_hex().to_string()}));
                    hydration_trace::scope(id, fetch).await
                } else { fetch.await };
                (attempt, requested.clone(), result.map(|(entity, source)| (requested, entity, source)))
            }
        }))
        .buffer_unordered(concurrency);

        while let Some((attempt, requested_ref, res)) = stream.next().await {
            cooperative_cancel_check()?;
            if let Some(id) = attempt {
                let facts = match &res {
                    Ok((requested, entity, source)) => serde_json::json!({
                        "disposition":if entity.reference == *requested { "merge_pending" } else { "identity_mismatch_summary_retained" },
                        "source":source, "fields":entity.fields.keys().map(|k| k.as_str()).collect::<Vec<_>>() }),
                    Err(e) => {
                        serde_json::json!({"disposition":"failure_summary_retained", "error":hydration_trace::failure(e)})
                    }
                };
                hydration_trace::emit_for(id, "merge_decision", facts);
            }
            match res {
                Ok((requested, mut entity, source)) => {
                    if source == ExecutionSource::Live {
                        extra_network += 1;
                    }
                    if entity.reference != requested {
                        tracing::warn!(
                            entity = %entity_type,
                            capability = %cap_name,
                            requested = %requested,
                            returned = %entity.reference,
                            event = "hydrate_get_identity_divergent"
                        );
                        if let Some(row) = workspace.get_mut(&requested_ref) {
                            let missing = detail_fields_missing_after_soft_fail(row, get_cap, cgs);
                            row.mark_detail_fields_unavailable(missing);
                        }
                        if let Some(id) = attempt {
                            if let Some(row) = workspace.get(&requested_ref) {
                                hydration_trace::emit_for(
                                    id,
                                    "final_row",
                                    hydration_trace::shape(&row.payload_to_json()),
                                );
                            }
                        }
                        continue;
                    }
                    entity.clear_unavailable_fields();
                    workspace.insert(requested.clone(), entity.clone());
                    // Upgrade an already-cached row; do not insert scanned-only refs.
                    if mat.get(&requested).is_some() {
                        mat.insert(entity)?;
                    }
                }
                Err(e) => {
                    // List rows remain usable as summaries when a detail GET fails
                    // (AppWorld and other mocks can 409/404 individual ids that still
                    // appeared in the list page). Do not fail the whole query.
                    tracing::warn!(
                        entity = %entity_type,
                        capability = %cap_name,
                        error = %e,
                        event = "hydrate_get_soft_fail"
                    );
                    if let Some(row) = workspace.get_mut(&requested_ref) {
                        let missing = detail_fields_missing_after_soft_fail(row, get_cap, cgs);
                        row.mark_detail_fields_unavailable(missing);
                    }
                }
            }
            if let Some(id) = attempt {
                if let Some(row) = workspace.get(&requested_ref) {
                    hydration_trace::emit_for(
                        id,
                        "final_row",
                        hydration_trace::shape(&row.payload_to_json()),
                    );
                }
            }
        }

        let mut out = Vec::with_capacity(ordered_refs.len());
        for r in &ordered_refs {
            let e = workspace.get(r).ok_or_else(|| RuntimeError::CacheError {
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
    fn synthesized_get_is_identity_only() {
        let inherit = CapabilityParamEnv {
            bindings: IndexMap::from([("access_token".into(), Value::String("tok".into()))]),
        };
        let get = synthesized_get(Ref::new("LangSecuredNote", "1"), &inherit);
        assert_eq!(get.reference, Ref::new("LangSecuredNote", "1"));
        // Session params ride materialization stamps, not Get AST.
        assert!(!inherit.bindings().is_empty());
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
        assert_eq!(inherited.reference, reference);
        assert_eq!(
            mat.capability_params_for(&reference).get("access_token"),
            Some(&Value::String("session".into()))
        );
        mat.stamp_capability_params(
            &reference,
            IndexMap::from([("access_token".into(), Value::String("program".into()))]),
        );
        assert_eq!(
            mat.capability_params_for(&reference).get("access_token"),
            Some(&Value::String("program".into())),
            "later stamp overlays session capability params"
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
        let mapping_err = cap
            .require_mapping()
            .expect_err("derived Get has no CML mapping");
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
            &plasm_compile::compile_cgs_capability_templates(&cgs).unwrap(),
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
            &plasm_compile::compile_cgs_capability_templates(&cgs).unwrap(),
            &ViewAmbientContext::default(),
            &mat,
        )
        .expect("preflight");
        let result = engine
            .execute(
                &Expr::Get(get),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
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
            &plasm_compile::compile_cgs_capability_templates(&cgs).unwrap(),
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
        preflight_compile_expr(
            &Expr::Get(get),
            &cgs,
            &plasm_compile::compile_cgs_capability_templates(&cgs).unwrap(),
            &ViewAmbientContext::default(),
            &mat,
        )
        .expect("session-stamped identity GET compiles");
    }

    /// Search `access_token` is a written selection hole, not Get's per-ref / catalog-bind inject.
    /// Tokenless `e#~"<query>"` must fail closed even when Get stamps or MCP bind would fill Get.
    #[test]
    fn auth_bearer_search_tokenless_search_does_not_inherit_get_session_inject() {
        use plasm_core::expr_parser::parse;
        use plasm_core::loader::load_schema_dir;
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/auth_bearer_search");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("auth_bearer_search");
        let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
        let ambient = ViewAmbientContext::default();
        let empty = SessionMaterialization::new();

        let tokenless_search =
            parse(r#"SecuredNote~"trip""#, &cgs).expect("parse tokenless search");
        let written_search =
            parse(r#"SecuredNote~"trip"{access_token="tok"}"#, &cgs).expect("parse written search");
        let tokenless_get = parse("SecuredNote(1)", &cgs).expect("parse tokenless get");

        let search_unstamped =
            preflight_compile_expr(&tokenless_search.expr, &cgs, &compiled, &ambient, &empty)
                .expect_err("tokenless Search unstamped must fail CML");
        assert!(
            search_unstamped.to_string().contains("access_token"),
            "expected access_token miss, got {search_unstamped}"
        );

        let mut stamped = SessionMaterialization::new();
        stamped.stamp_capability_params(
            &plasm_core::Ref::new("SecuredNote", "1"),
            IndexMap::from([("access_token".into(), Value::String("tok".into()))]),
        );
        let search_stamped =
            preflight_compile_expr(&tokenless_search.expr, &cgs, &compiled, &ambient, &stamped)
                .expect_err("Get-style session stamp must not fill Search selection");
        assert!(
            search_stamped.to_string().contains("access_token"),
            "expected access_token miss after Get stamp, got {search_stamped}"
        );

        preflight_compile_expr(&written_search.expr, &cgs, &compiled, &ambient, &empty)
            .expect("Search with written access_token compiles unstamped");

        let get_unstamped =
            preflight_compile_expr(&tokenless_get.expr, &cgs, &compiled, &ambient, &empty)
                .expect_err("tokenless Get unstamped must fail CML");
        assert!(
            get_unstamped.to_string().contains("access_token"),
            "expected access_token miss, got {get_unstamped}"
        );
        preflight_compile_expr(&tokenless_get.expr, &cgs, &compiled, &ambient, &stamped)
            .expect("session-stamped tokenless Get compiles");
    }

    #[tokio::test]
    async fn auth_bearer_search_catalog_bind_fills_get_not_search() {
        use crate::execution::session::ExecuteSessionMaterial;
        use crate::execution::ExecutionEngine;
        use plasm_core::expr_parser::parse;
        use plasm_core::loader::load_schema_dir;
        use std::sync::Arc;
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/auth_bearer_search");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("auth_bearer_search");
        let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
        let mut bind = indexmap::IndexMap::new();
        bind.insert("access_token".into(), "tok".into());
        let material = Arc::new(ExecuteSessionMaterial {
            catalog_revision: "auth_bearer_search".into(),
            compiled_catalog: compiled.clone(),
            credential_store: None,
            prompt_hash: "a".repeat(64),
            session_id: "b".repeat(32),
            transport_origin: None,
            ui_origin: None,
            catalog_bind: Some(bind),
            login_access_token_tail: ExecuteSessionMaterial::empty_login_access_token_tail(),
        });
        let tokenless_search = parse(r#"SecuredNote~"trip""#, &cgs).expect("parse search");
        let tokenless_get = parse("SecuredNote(1)", &cgs).expect("parse get");
        let empty = SessionMaterialization::new();
        let compiled_ref = compiled.clone();
        ExecutionEngine::run_in_execute_task_scopes(
            "http://localhost:1080".into(),
            None,
            None,
            None,
            Some(material),
            compiled,
            None,
            None,
            async move {
                let ambient = ViewAmbientContext::default();
                preflight_compile_expr(
                    &tokenless_get.expr,
                    &cgs,
                    compiled_ref.as_ref(),
                    &ambient,
                    &empty,
                )
                .expect("catalog_bind injects Bearer for tokenless Get");
                let err = preflight_compile_expr(
                    &tokenless_search.expr,
                    &cgs,
                    compiled_ref.as_ref(),
                    &ambient,
                    &empty,
                )
                .expect_err("catalog_bind must not fill Search selection");
                assert!(
                    err.to_string().contains("access_token"),
                    "expected access_token miss on Search under catalog_bind, got {err}"
                );
            },
        )
        .await;
    }

    /// After fixture login (`AuthSession` provides `access_token`), unary Get compiles
    /// without writing the token. Search remains a written selection hole.
    #[tokio::test]
    async fn auth_bearer_search_login_provides_fills_unary_get_not_search() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::expr_parser::parse;
        use plasm_core::loader::load_schema_dir;
        use std::sync::Arc;

        #[derive(Clone)]
        struct LoginTransport;

        #[async_trait]
        impl HttpTransport for LoginTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                assert!(
                    request.path.contains("/auth/login"),
                    "login must POST /auth/login, got {}",
                    request.path
                );
                Ok((serde_json::json!({ "access_token": "tok" }), None))
            }

            async fn get_json_absolute(
                &self,
                _url: &str,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                Ok((serde_json::json!({}), None))
            }
        }

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/auth_bearer_search");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("auth_bearer_search");
        let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
        let ambient = ViewAmbientContext::default();
        let login =
            parse(r#"AuthSession.login(username="u", password="p")"#, &cgs).expect("parse login");
        let get_int = parse("SecuredNote(1)", &cgs).expect("parse unary Get e2(<id>)");
        let get_str = parse(r#"SecuredNote("note-id")"#, &cgs);
        let tokenless_search = parse(r#"SecuredNote~"q""#, &cgs).expect("parse tokenless Search");
        let written_search =
            parse(r#"SecuredNote~"q"{access_token="tok"}"#, &cgs).expect("parse written Search");
        let sess_search = parse(r#"SecuredNote~"q"{access_token=sess.access_token}"#, &cgs);

        let get_before = preflight_compile_expr(
            &get_int.expr,
            &cgs,
            &compiled,
            &ambient,
            &SessionMaterialization::new(),
        )
        .expect_err("unary Get before login must fail closed");
        assert!(
            get_before.to_string().contains("access_token"),
            "expected access_token miss before login, got {get_before}"
        );

        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(LoginTransport),
            None,
        );
        let mut mat = SessionMaterialization::new();
        let login_res = engine
            .execute(
                &login.expr,
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("fixture login");
        assert!(
            login_res.count >= 1,
            "login must materialize AuthSession, got {login_res:?}"
        );
        assert!(
            !mat.provided_session_params_for(&SessionMaterialization::provide_catalog_key(
                &cgs, None
            ))
            .is_empty(),
            "login provides must stamp catalog overlay"
        );

        preflight_compile_expr(&get_int.expr, &cgs, &compiled, &ambient, &mat)
            .expect("unary Get e2(<id>) compiles after login without writing access_token");
        match get_str {
            Ok(parsed) => {
                preflight_compile_expr(&parsed.expr, &cgs, &compiled, &ambient, &mat).expect(
                    "unary Get e2(\"note-id\") compiles after login without writing access_token",
                );
            }
            Err(err) => {
                panic!("SecuredNote(\"note-id\") must parse for fixture compile matrix: {err}");
            }
        }

        let search_after =
            preflight_compile_expr(&tokenless_search.expr, &cgs, &compiled, &ambient, &mat)
                .expect_err("tokenless Search must still fail after login");
        assert!(
            search_after.to_string().contains("access_token"),
            "expected access_token miss on tokenless Search after login, got {search_after}"
        );
        preflight_compile_expr(&written_search.expr, &cgs, &compiled, &ambient, &mat)
            .expect("Search with written access_token compiles after login");
        match sess_search {
            Ok(parsed) => {
                preflight_compile_expr(&parsed.expr, &cgs, &compiled, &ambient, &mat)
                    .expect("Search with written access_token=sess.access_token compiles");
            }
            Err(err) => {
                panic!("SecuredNote~\"q\"{{access_token=sess.access_token}} must parse: {err}");
            }
        }
    }

    /// Written Search `{access_token=…}` after login must put that same Bearer on
    /// `GET /secured/notes?query=` — overlay from a later login must not swap it.
    #[tokio::test]
    async fn auth_bearer_search_written_token_after_login_is_on_the_wire() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::expr_parser::parse;
        use plasm_core::loader::load_schema_dir;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        };

        #[derive(Clone, Debug)]
        struct Wire {
            method: String,
            path: String,
            query: Option<String>,
            authorization: Option<String>,
        }

        #[derive(Clone)]
        struct RecordingTransport {
            logins: Arc<AtomicUsize>,
            wires: Arc<Mutex<Vec<Wire>>>,
        }

        #[async_trait]
        impl HttpTransport for RecordingTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                let authorization = match &request.headers {
                    Some(Value::Object(m)) => m.get("Authorization").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    }),
                    _ => None,
                };
                let query = match &request.query {
                    Some(Value::Object(m)) => m.get("query").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    }),
                    _ => None,
                };
                self.wires.lock().unwrap().push(Wire {
                    method: format!("{:?}", request.method),
                    path: request.path.clone(),
                    query,
                    authorization,
                });
                if request.path.contains("/auth/login") {
                    let n = self.logins.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok((
                        serde_json::json!({ "access_token": format!("tok-login-{n}") }),
                        None,
                    ))
                } else {
                    Ok((serde_json::json!([]), None))
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

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/auth_bearer_search");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("auth_bearer_search");
        let wires = Arc::new(Mutex::new(Vec::new()));
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                hydrate: false,
                ..ExecutionConfig::default()
            },
            Arc::new(RecordingTransport {
                logins: Arc::new(AtomicUsize::new(0)),
                wires: wires.clone(),
            }),
            None,
        );
        let login =
            parse(r#"AuthSession.login(username="u", password="p")"#, &cgs).expect("parse login");
        let search_t1 =
            parse(r#"SecuredNote~"q"{access_token="tok-login-1"}"#, &cgs).expect("parse search t1");
        let search_t2 =
            parse(r#"SecuredNote~"q"{access_token="tok-login-2"}"#, &cgs).expect("parse search t2");
        let search_t1_after_overlay = parse(
            r#"SecuredNote~"q-stale-write"{access_token="tok-login-1"}"#,
            &cgs,
        )
        .expect("parse search t1 after overlay t2");

        let mut mat = SessionMaterialization::new();
        engine
            .execute(
                &login.expr,
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("login 1");
        engine
            .execute(
                &search_t1.expr,
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("search after login 1");
        engine
            .execute(
                &login.expr,
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("login 2");
        engine
            .execute(
                &search_t2.expr,
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("search written t2 after login 2");
        engine
            .execute(
                &search_t1_after_overlay.expr,
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("search written t1 must survive overlay t2");

        let wires = wires.lock().unwrap().clone();
        let searches: Vec<_> = wires
            .iter()
            .filter(|w| w.path.contains("/secured/notes"))
            .collect();
        assert_eq!(
            searches.len(),
            3,
            "expected three Search GETs, wires={wires:?}"
        );
        for w in &searches {
            assert!(
                w.method.contains("Get") || w.method.contains("GET"),
                "Search must be GET, got {w:?}"
            );
            assert!(
                w.authorization.is_some(),
                "Search Authorization must be present: {w:?}"
            );
        }
        assert_eq!(searches[0].query.as_deref(), Some("q"));
        assert_eq!(searches[1].query.as_deref(), Some("q"));
        assert_eq!(searches[2].query.as_deref(), Some("q-stale-write"));
        assert_eq!(
            searches[0].authorization.as_deref(),
            Some("Bearer tok-login-1"),
            "Search after login 1 must send that login token"
        );
        assert_eq!(
            searches[1].authorization.as_deref(),
            Some("Bearer tok-login-2"),
            "Search written as login 2 must send login 2, not overlay-swap"
        );
        assert_eq!(
            searches[2].authorization.as_deref(),
            Some("Bearer tok-login-1"),
            "written tok-login-1 must not be swapped to overlay tok-login-2: {searches:?}"
        );
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
                ExecuteOptions::for_catalog(&cgs).unwrap(),
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
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("search without hydrate");
        let reference = listed.entities[0].reference.clone();

        paths.lock().unwrap().clear();
        auths.lock().unwrap().clear();
        let get = GetExpr::from_ref(reference.clone());
        assert_eq!(
            get.reference.simple_id().map(|s| s.as_str()),
            reference.simple_id().map(|s| s.as_str()),
            "explicit identity GET carries identity on Ref only"
        );
        let got = engine
            .execute(
                &Expr::Get(get),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
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

    #[test]
    fn ra6_query_only_parent_for_entity_get_keeps_scoped_param() {
        use plasm_core::loader::load_schema_dir;
        let matrix = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        assert!(
            matrix
                .find_capability("QueryOnlyRequest", CapabilityKind::Get)
                .is_none(),
            "QueryOnlyRequest must stay Query-only"
        );
        let env = env_with(&[("access_token", "tok"), ("status", "pending")]);
        let inherit = CapabilityParamEnv::for_entity_get(&matrix, "QueryOnlyRequest", &env);
        assert_eq!(
            inherit.bindings().get("access_token"),
            Some(&Value::String("tok".into())),
            "for_entity_get must retain Query params when the parent has no Get"
        );

        let scoped = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/relation_scoped_token"),
        )
        .expect("relation_scoped_token CGS");
        assert!(
            scoped
                .find_capability("TokenFolder", CapabilityKind::Get)
                .is_none(),
            "TokenFolder must stay Query-only so this witness hits hydrate.rs:95"
        );
        let folder_env = env_with(&[("access_token", "tok")]);
        let folder_inherit =
            CapabilityParamEnv::for_entity_get(&scoped, "TokenFolder", &folder_env);
        assert_eq!(
            folder_inherit.bindings().get("access_token"),
            Some(&Value::String("tok".into())),
            "Query-only TokenFolder stamp must keep access_token"
        );
    }

    #[test]
    fn ra6_query_only_parent_stamp_reaches_child_cap() {
        use plasm_core::loader::load_schema_dir;
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/relation_scoped_token"),
        )
        .expect("relation_scoped_token CGS");
        let inherit = CapabilityParamEnv::for_entity_get(
            &cgs,
            "TokenFolder",
            &env_with(&[("access_token", "tok")]),
        );
        let parent = CachedEntity::from_decoded(
            Ref::new("TokenFolder", "f1"),
            [
                ("folder_id".into(), Value::String("f1".into())),
                ("name".into(), Value::String("inbox".into())),
            ]
            .into_iter()
            .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Summary,
        );
        let mut mat = SessionMaterialization::new();
        mat.insert(parent.clone()).expect("parent row");
        stamp_entities_and_mat(std::slice::from_ref(&parent), &mut mat, &inherit);
        let child = cgs.get_capability("token_file_query").expect("child query");
        let hopped = CapabilityParamEnv::from_source_row_for_cap(&cgs, &mat, &parent, child);
        assert_eq!(
            hopped.bindings().get("access_token"),
            Some(&Value::String("tok".into())),
            "child traversal must see the parent Query stamp"
        );
        let mut q = QueryExpr::filtered("TokenFile", Predicate::eq("folder_id", "f1"));
        hopped.apply_to_scoped_query(&mut q);
        let blob = serde_json::to_string(q.predicate.as_ref().expect("pred")).expect("json");
        assert!(
            blob.contains("tok"),
            "scoped child pred must carry token: {blob}"
        );
    }

    #[test]
    fn scoped_query_inherit_adds_token_and_skips_bound_keys() {
        let inherit = CapabilityParamEnv {
            bindings: IndexMap::from([
                ("access_token".into(), Value::String("tok".into())),
                (
                    "folder_id".into(),
                    Value::String("should-not-overwrite".into()),
                ),
            ]),
        };
        let mut q = QueryExpr::filtered("TokenFile", Predicate::eq("folder_id", "f1"));
        inherit.apply_to_scoped_query(&mut q);
        let fields = q.predicate.as_ref().unwrap().referenced_fields();
        assert!(fields.contains(&"access_token".to_string()));
        assert!(fields.contains(&"folder_id".to_string()));
        let pred = q.predicate.unwrap();
        let blob = serde_json::to_string(&pred).expect("pred json");
        assert!(blob.contains("tok"), "{blob}");
        assert!(
            !blob.contains("should-not-overwrite"),
            "relation binding must win: {blob}"
        );
    }

    /// Live hop: Query-only TokenFolder (no Get) → child files. Fails if stamp drops access_token.
    #[tokio::test]
    async fn query_scoped_relation_inherits_parent_access_token() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{ChainExpr, Expr, Predicate, QueryExpr};
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
                if request.path.contains("/files") {
                    Ok((
                        serde_json::json!([{
                            "file_id": "n1",
                            "folder_id": "f1",
                            "name": "notes"
                        }]),
                        None,
                    ))
                } else {
                    Ok((
                        serde_json::json!([{
                            "folder_id": "f1",
                            "name": "inbox"
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
                base_url: Some("http://scoped-token.test".into()),
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
                .join("../../fixtures/schemas/relation_scoped_token"),
        )
        .expect("relation_scoped_token CGS");
        let mut folders = QueryExpr::filtered("TokenFolder", Predicate::eq("access_token", "tok"));
        folders.capability_name = Some("token_folder_query".into());
        let chain = ChainExpr::auto_get(Expr::Query(folders), "files");
        let mut mat = SessionMaterialization::new();
        let result = engine
            .execute(
                &Expr::Chain(chain),
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("folder query then files relation");
        assert_eq!(result.count, 1);
        let recorded_paths = paths.lock().unwrap().clone();
        assert!(
            recorded_paths.iter().any(|p| p.ends_with("/folders")),
            "folder query missing: {recorded_paths:?}"
        );
        assert!(
            recorded_paths.iter().any(|p| p.contains("/files")),
            "scoped file query missing: {recorded_paths:?}"
        );
        let recorded_auths = auths.lock().unwrap().clone();
        assert!(
            recorded_auths.iter().filter(|a| *a == "Bearer tok").count() >= 2,
            "parent query and scoped child must both send Bearer: {recorded_auths:?}"
        );
    }

    #[test]
    fn ra6_unary_get_session_overlay_reaches_child_without_per_ref_stamp() {
        use plasm_core::loader::load_schema_dir;
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/relation_get_scoped_token"),
        )
        .expect("relation_get_scoped_token CGS");
        let parent = CachedEntity::from_decoded(
            Ref::new("TokenNote", "n1"),
            [
                ("note_id".into(), Value::String("n1".into())),
                ("title".into(), Value::String("inbox".into())),
            ]
            .into_iter()
            .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Complete,
        );
        let mut mat = SessionMaterialization::new();
        mat.insert(parent.clone()).expect("parent row");
        mat.stamp_provided_session_params(
            SessionMaterialization::provide_catalog_key(&cgs, None),
            IndexMap::from([("access_token".into(), Value::String("tok".into()))]),
        );
        let child = cgs
            .get_capability("token_note_comment_query")
            .expect("child query");
        let hopped = CapabilityParamEnv::from_source_row_for_cap(&cgs, &mat, &parent, child);
        assert_eq!(
            hopped.bindings().get("access_token"),
            Some(&Value::String("tok".into())),
            "session Bearer must reach the child hop without a per-ref stamp"
        );
    }

    #[test]
    fn ra6_unary_get_empty_token_names_missing_env() {
        use plasm_core::loader::load_schema_dir;
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/relation_get_scoped_token"),
        )
        .expect("relation_get_scoped_token CGS");
        let parent = CachedEntity::from_decoded(
            Ref::new("TokenNote", "n1"),
            [("note_id".into(), Value::String("n1".into()))]
                .into_iter()
                .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Complete,
        );
        let mat = SessionMaterialization::new();
        let child = cgs
            .get_capability("token_note_comment_query")
            .expect("child query");
        let mut q = QueryExpr::filtered("TokenNoteComment", Predicate::eq("note_id", "n1"));
        q.capability_name = Some("token_note_comment_query".into());
        let err = relation_inherit_for_scoped_query(&cgs, &mat, &parent, child, &q)
            .expect_err("empty token must name the missing env key");
        let msg = err.to_string();
        assert!(
            msg.contains("access_token") && msg.contains("parent Get"),
            "diagnostic must name access_token and the inherit law: {msg}"
        );
    }

    /// Live hop: unary TokenNote Get → comments. Session Bearer must reach both requests (RA-6).
    ///
    /// Fixture program: `g = TokenNote("n1"); g.comments` after session `access_token=tok`.
    #[tokio::test]
    async fn get_scoped_relation_inherits_session_access_token() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{ChainExpr, Expr, GetExpr};
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
                if request.path.contains("/comments") {
                    Ok((
                        serde_json::json!([{
                            "comment_id": "c1",
                            "note_id": "n1",
                            "body": "ok"
                        }]),
                        None,
                    ))
                } else {
                    Ok((
                        serde_json::json!({
                            "note_id": "n1",
                            "title": "inbox"
                        }),
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
                base_url: Some("http://get-scoped-token.test".into()),
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
                .join("../../fixtures/schemas/relation_get_scoped_token"),
        )
        .expect("relation_get_scoped_token CGS");
        let mut mat = SessionMaterialization::new();
        mat.stamp_provided_session_params(
            SessionMaterialization::provide_catalog_key(&cgs, None),
            IndexMap::from([("access_token".into(), Value::String("tok".into()))]),
        );
        let get = GetExpr::from_ref(Ref::new("TokenNote", "n1"));
        let chain = ChainExpr::auto_get(Expr::Get(get), "comments");
        let result = engine
            .execute(
                &Expr::Chain(chain),
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("unary Get then comments relation");
        assert_eq!(result.count, 1);
        let recorded_paths = paths.lock().unwrap().clone();
        assert!(
            recorded_paths
                .iter()
                .any(|p| p.contains("/notes/n1") && !p.contains("/comments")),
            "unary Get missing: {recorded_paths:?}"
        );
        assert!(
            recorded_paths.iter().any(|p| p.contains("/comments")),
            "scoped comment query missing: {recorded_paths:?}"
        );
        let recorded_auths = auths.lock().unwrap().clone();
        assert!(
            recorded_auths.iter().filter(|a| *a == "Bearer tok").count() >= 2,
            "unary Get and scoped child must both send Bearer: {recorded_auths:?}"
        );
    }

    /// Cache-hit unary Get with no session stamp: relation hop must name `access_token`.
    #[tokio::test]
    async fn get_scoped_relation_empty_token_names_missing_env() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{ChainExpr, Expr, GetExpr};
        use std::sync::Arc;

        #[derive(Clone)]
        struct RejectTransport;

        #[async_trait]
        impl HttpTransport for RejectTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                _request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                Err(RuntimeError::request_failure(
                    "empty-token hop must not HTTP",
                    1,
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

        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://get-scoped-token.test".into()),
                hydrate: false,
                ..ExecutionConfig::default()
            },
            Arc::new(RejectTransport),
            None,
        );
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/relation_get_scoped_token"),
        )
        .expect("relation_get_scoped_token CGS");
        let parent = CachedEntity::from_decoded(
            Ref::new("TokenNote", "n1"),
            [
                ("note_id".into(), Value::String("n1".into())),
                ("title".into(), Value::String("inbox".into())),
            ]
            .into_iter()
            .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Complete,
        );
        let mut mat = SessionMaterialization::new();
        mat.insert(parent).expect("complete parent");
        let get = GetExpr::from_ref(Ref::new("TokenNote", "n1"));
        let chain = ChainExpr::auto_get(Expr::Get(get), "comments");
        let err = engine
            .execute(
                &Expr::Chain(chain),
                &cgs,
                &mut mat,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect_err("empty token relation hop");
        let msg = err.to_string();
        assert!(
            msg.contains("access_token") && msg.contains("parent Get"),
            "live empty hop must name access_token: {msg}"
        );
    }

    fn dual_session_consumer() -> CGS {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_dual_session/consumer"),
        )
        .expect("prerequisite_dual_session consumer");
        cgs.bind_registry_entry_id("consumer");
        cgs
    }

    fn file_query_source_deployments() -> plasm_core::prerequisites::DeploymentBindings {
        use plasm_core::prerequisites::{CapabilityRef, DeploymentBinding, DeploymentBindings};
        DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: CapabilityRef {
                    catalog: "consumer".into(),
                    capability: "file_query".into(),
                },
                requirement: "source_session".into(),
                provider_catalog: "source".into(),
                provider: "session".into(),
            }],
        }
    }

    /// Get on consumer Record, then `.files` whose `access_token` seat deploys to source.
    /// Name intersection must not wire the consumer token onto that seat (RA-17 inherit).
    #[test]
    fn ra17_inherit_omits_consumer_token_on_source_file_seat() {
        let cgs = dual_session_consumer();
        let parent = CachedEntity::from_decoded(
            Ref::new("Record", "rec-1"),
            [("id".into(), Value::String("rec-1".into()))]
                .into_iter()
                .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Complete,
        );
        let mut mat = SessionMaterialization::new();
        mat.set_prerequisite_deployments(file_query_source_deployments());
        mat.stamp_provided_session_params(
            SessionMaterialization::provide_catalog_key(&cgs, None),
            IndexMap::from([("access_token".into(), Value::String("consumer-tok".into()))]),
        );
        let child = cgs.get_capability("file_query").expect("file_query");
        let hopped = CapabilityParamEnv::from_source_row_for_cap(&cgs, &mat, &parent, child);
        assert!(
            hopped.bindings().get("access_token").is_none(),
            "consumer token must not inherit onto source-deployed file_query access_token: {:?}",
            hopped.bindings()
        );
        let mut q = QueryExpr::filtered("File", Predicate::eq("id", "rec-1"));
        q.capability_name = Some("file_query".into());
        let err = relation_inherit_for_scoped_query(&cgs, &mat, &parent, child, &q)
            .expect_err("omitted foreign inherit must not silently fill the source seat");
        let msg = err.to_string();
        assert!(
            msg.contains("access_token") && msg.contains("parent Get"),
            "omit must name the missing source seat: {msg}"
        );
        assert!(
            !msg.contains("consumer-tok"),
            "diagnostic must not carry the consumer token: {msg}"
        );
    }

    /// Same fixture, no distinct deployment: RA-6 name intersection still fills.
    #[test]
    fn ra6_inherit_keeps_access_token_without_foreign_seat() {
        let cgs = dual_session_consumer();
        let parent = CachedEntity::from_decoded(
            Ref::new("Record", "rec-1"),
            [("id".into(), Value::String("rec-1".into()))]
                .into_iter()
                .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Complete,
        );
        let mut mat = SessionMaterialization::new();
        mat.stamp_provided_session_params(
            SessionMaterialization::provide_catalog_key(&cgs, None),
            IndexMap::from([("access_token".into(), Value::String("consumer-tok".into()))]),
        );
        let child = cgs.get_capability("file_query").expect("file_query");
        let hopped = CapabilityParamEnv::from_source_row_for_cap(&cgs, &mat, &parent, child);
        assert_eq!(
            hopped.bindings().get("access_token"),
            Some(&Value::String("consumer-tok".into())),
            "undeployed child seat still inherits by name (RA-6)"
        );
    }

    /// After poison, fork_from must not revive consult_complete_get (RA-11 / branch_commit.rs:49).
    #[tokio::test]
    async fn ra11_fork_after_poison_unary_get_reobserves_live() {
        use crate::auth::ResolvedAuth;
        use crate::branch_commit::BranchMaterializationBase;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, GetExpr, Ref};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        #[derive(Clone)]
        struct CountingGet {
            gets: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl HttpTransport for CountingGet {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                self.gets.fetch_add(1, Ordering::SeqCst);
                assert!(
                    request.path.contains("items"),
                    "expected LangItem get path, got {}",
                    request.path
                );
                Ok((
                    serde_json::json!({
                        "id": "i1",
                        "title": "live-reobserve",
                        "score": 1,
                        "owner": "alice"
                    }),
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
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let gets = Arc::new(AtomicUsize::new(0));
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(CountingGet { gets: gets.clone() }),
            None,
        );
        let get = GetExpr::from_ref(Ref::new("LangItem", "i1"));
        let mut session = SessionMaterialization::new();
        let first = engine
            .execute(
                &Expr::Get(get.clone()),
                &cgs,
                &mut session,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("seed get");
        assert_eq!(first.source, ExecutionSource::Live);
        assert_eq!(gets.load(Ordering::SeqCst), 1);
        session.poison_read_caches_after_mutation();

        let (mut branch, _base) = BranchMaterializationBase::fork_from(&session);
        let again = engine
            .execute(
                &Expr::Get(get),
                &cgs,
                &mut branch,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("reobserve get");
        assert_eq!(
            again.source,
            ExecutionSource::Live,
            "fork after mutator poison must re-observe, not serve the pre-write snapshot"
        );
        assert_eq!(gets.load(Ordering::SeqCst), 2);
        let title = again.entities[0].fields.get("title").map(|f| f.to_value());
        assert_eq!(title, Some(Value::String("live-reobserve".into())));
    }

    #[tokio::test]
    async fn direction_payer_versus_debtor_filters_are_opposite_roles() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, Predicate, QueryExpr};
        use std::sync::Arc;

        #[derive(Clone)]
        struct LedgerTransport;

        #[async_trait]
        impl HttpTransport for LedgerTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                assert!(
                    request.path.contains("ledger"),
                    "expected ledger path, got {}",
                    request.path
                );
                Ok((
                    serde_json::json!([
                        {"id": "t1", "payer": "alice", "debtor": "bob", "amount": 10},
                        {"id": "t2", "payer": "bob", "debtor": "alice", "amount": 7}
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
                .join("../../fixtures/schemas/plasm_direction_matrix"),
        )
        .expect("direction matrix CGS");
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                hydrate: false,
                ..ExecutionConfig::default()
            },
            Arc::new(LedgerTransport),
            None,
        );
        // RA-2: payer/debtor are row fields, not Query selection braces.
        let mut all = QueryExpr::all("LedgerEntry");
        all.hydrate = Some(false);
        let mut mat = SessionMaterialization::new();
        let listed = engine
            .execute(
                &Expr::Query(all),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("list ledger");
        assert_eq!(listed.count, 2, "fixture must decode both directed rows");

        let paid = super::super::predicates::filter_entities_by_predicate(
            listed.entities.clone(),
            &Predicate::eq("payer", "alice"),
        )
        .expect("payer filter");
        let paid_ids: Vec<String> = paid
            .iter()
            .map(|e| e.reference.primary_slot_str().to_string())
            .collect();
        assert_eq!(
            paid_ids,
            vec!["t1".to_string()],
            "payer=alice is sent direction, not received: {paid_ids:?}"
        );

        let owed = super::super::predicates::filter_entities_by_predicate(
            listed.entities,
            &Predicate::eq("debtor", "alice"),
        )
        .expect("debtor filter");
        let owed_ids: Vec<String> = owed
            .iter()
            .map(|e| e.reference.primary_slot_str().to_string())
            .collect();
        assert_eq!(
            owed_ids,
            vec!["t2".to_string()],
            "debtor=alice is received direction, not sent: {owed_ids:?}"
        );
    }

    /// RA-16: Complete list rows + empty mat (TopK skip-merge) must not CacheError.
    #[tokio::test]
    async fn ra16_hydrate_keeps_list_ref_when_mat_empty() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::Ref;
        use std::sync::Arc;

        #[derive(Clone)]
        struct NoGetTransport;

        #[async_trait]
        impl HttpTransport for NoGetTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                _request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                panic!("RA-16 Complete list must not issue detail GET");
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
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(NoGetTransport),
            None,
        );
        let entity = CachedEntity::from_decoded(
            Ref::new("LangSecuredNote", "1"),
            [
                ("note_id".into(), Value::String("1".into())),
                ("title".into(), Value::String("trip".into())),
            ]
            .into_iter()
            .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Complete,
        );
        let mut mat = SessionMaterialization::new();
        let env = env_with(&[("access_token", "tok")]);
        let (out, extra) = engine
            .hydrate_query_summaries(
                "LangSecuredNote",
                std::slice::from_ref(&entity),
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                true,
                &env,
            )
            .await
            .expect("RA-16 must keep the list Ref when mat was empty");
        assert_eq!(extra, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reference, entity.reference);
        // Addressability is the hydrate return, not permanent graph insertion of the scan.
        assert!(
            mat.get(&entity.reference).is_none(),
            "RA-16 must not permanently insert a Complete list seed into an empty graph"
        );
    }

    /// RA-16: detail GET soft-fail must leave the list summary, not CacheError.
    #[tokio::test]
    async fn ra16_hydrate_keeps_summary_when_detail_get_soft_fails() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::Ref;
        use std::sync::Arc;

        #[derive(Clone)]
        struct NotFoundGet;

        #[async_trait]
        impl HttpTransport for NotFoundGet {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                assert!(
                    request.path.contains("/secured_notes/"),
                    "expected hydrate GET, got {}",
                    request.path
                );
                Err(RuntimeError::CacheError {
                    message: "404 detail".into(),
                })
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
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(NotFoundGet),
            None,
        );
        let entity = CachedEntity::from_decoded(
            Ref::new("LangSecuredNote", "1"),
            [("note_id".into(), Value::String("1".into()))]
                .into_iter()
                .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Summary,
        );
        let mut mat = SessionMaterialization::new();
        let env = env_with(&[("access_token", "tok")]);
        let (out, _) = engine
            .hydrate_query_summaries(
                "LangSecuredNote",
                std::slice::from_ref(&entity),
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                true,
                &env,
            )
            .await
            .expect("RA-16 must keep the summary after GET soft-fail");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reference, entity.reference);
        assert_eq!(
            out[0].completeness,
            crate::cache::EntityCompleteness::Summary
        );
        assert!(
            out[0].has_unavailable_detail_fields(),
            "soft-fail must mark Get-provided fields unavailable, not present-empty"
        );
        assert!(
            out[0].unavailable_fields.contains("body"),
            "expected body unavailable, got {:?}",
            out[0].unavailable_fields
        );
        let agent_row = crate::entity_to_agent_row_json(&out[0], Some(&cgs));
        let unavail = agent_row
            .get("_unavailable_fields")
            .and_then(|v| v.as_array())
            .expect("_unavailable_fields in agent row JSON");
        assert!(
            unavail.iter().any(|v| v.as_str() == Some("body")),
            "agent row must expose body as unavailable: {unavail:?}"
        );
        assert!(
            agent_row.get("body").is_none(),
            "unavailable must not invent a present body key; got {:?}",
            agent_row.get("body")
        );
    }

    /// RA-16: `| order by | take` (TopK) paginated query must not lose list Refs.
    #[tokio::test]
    async fn ra16_topk_paginated_query_keeps_list_ref() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use crate::top_k::TopKSpec;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, QueryExpr};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        #[derive(Clone)]
        struct IndexedList {
            list_pages: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl HttpTransport for IndexedList {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                if request.path.contains("/indexed/") {
                    return Err(RuntimeError::CacheError {
                        message: "404 detail".into(),
                    });
                }
                let page = self.list_pages.fetch_add(1, Ordering::SeqCst);
                if page > 0 {
                    return Ok((serde_json::json!({ "results": [] }), None));
                }
                Ok((
                    serde_json::json!({
                        "results": [
                            {"id": "id-0", "n": 0},
                            {"id": "id-1", "n": 1}
                        ]
                    }),
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
                .join("../../fixtures/schemas/plasm_pagination_matrix"),
        )
        .expect("pagination matrix CGS");
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(IndexedList {
                list_pages: Arc::new(AtomicUsize::new(0)),
            }),
            None,
        );
        let query = QueryExpr::all("Item");
        let consume = StreamConsumeOpts {
            fetch_all: true,
            top_k: Some(TopKSpec {
                count: 2,
                sort_key: vec!["n".into()],
                descending: true,
                row_filter: Vec::new(),
            }),
            ..StreamConsumeOpts::default()
        };
        let mut mat = SessionMaterialization::new();
        let result = engine
            .execute(
                &Expr::Query(query),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                consume,
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("RA-16 TopK paginated query must keep list Refs");
        assert!(
            result.count >= 1,
            "TopK must return listed rows, got {result:?}"
        );
        let ids: Vec<String> = result
            .entities
            .iter()
            .map(|e| e.reference.primary_slot_str().to_string())
            .collect();
        assert!(
            ids.contains(&"id-0".to_string()) || ids.contains(&"id-1".to_string()),
            "expected list identities, got {ids:?}"
        );
    }

    /// Freshness: Complete is field coverage. After poison, a fresh list observation
    /// must win; do not treat the stale Complete graph row as reusable.
    #[tokio::test]
    async fn proximal_stale_complete_does_not_override_fresh_list() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::Ref;
        use std::sync::Arc;

        #[derive(Clone)]
        struct NoGetTransport;

        #[async_trait]
        impl HttpTransport for NoGetTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                _request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                panic!("RA-16 Complete list must not issue detail GET");
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
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(NoGetTransport),
            None,
        );
        let entity = CachedEntity::from_decoded(
            Ref::new("LangSecuredNote", "1"),
            [
                ("note_id".into(), Value::String("1".into())),
                ("title".into(), Value::String("trip".into())),
            ]
            .into_iter()
            .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Complete,
        );
        let mut mat = SessionMaterialization::new();
        let mut old = entity.clone();
        old.fields.insert(
            "title".into(),
            plasm_core::TypedFieldValue::String("stale".into()),
        );
        mat.insert(old).unwrap();
        mat.poison_read_caches_after_mutation();
        let env = env_with(&[("access_token", "tok")]);
        let (out, extra) = engine
            .hydrate_query_summaries(
                "LangSecuredNote",
                std::slice::from_ref(&entity),
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                true,
                &env,
            )
            .await
            .expect("fresh list must remain addressable after poison");
        assert_eq!(
            out[0].fields.get("title"),
            entity.fields.get("title"),
            "fresh list must win after mutation"
        );
        assert_eq!(extra, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reference, entity.reference);
        assert!(mat.get(&entity.reference).is_some());
    }

    /// Identity: a detail GET that decodes a different Ref must not pollute the graph.
    #[tokio::test]
    async fn proximal_identity_divergent_detail_is_not_inserted() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::Ref;
        use std::sync::Arc;

        #[derive(Clone)]
        struct NotFoundGet;

        #[async_trait]
        impl HttpTransport for NotFoundGet {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                assert!(
                    request.path.contains("/secured_notes/"),
                    "expected hydrate GET, got {}",
                    request.path
                );
                Ok((
                    serde_json::json!({"note_id":"2", "title":"wrong row"}),
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
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
        let material = Arc::new(crate::ExecuteSessionMaterial {
            catalog_revision: "langmatrix".into(),
            compiled_catalog: compiled.clone(),
            credential_store: None,
            prompt_hash: "a".repeat(64),
            session_id: "b".repeat(32),
            transport_origin: None,
            ui_origin: None,
            catalog_bind: None,
            login_access_token_tail: crate::ExecuteSessionMaterial::empty_login_access_token_tail(),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(NotFoundGet),
            None,
        );
        let entity = CachedEntity::from_decoded(
            Ref::new("LangSecuredNote", "1"),
            [("note_id".into(), Value::String("1".into()))]
                .into_iter()
                .collect(),
            indexmap::IndexMap::new(),
            1,
            crate::cache::EntityCompleteness::Summary,
        );
        let env = env_with(&[("access_token", "tok")]);
        let base: Arc<str> = Arc::from("http://127.0.0.1:9");
        ExecutionEngine::run_in_execute_task_scopes(
            base,
            None,
            None,
            None,
            Some(material),
            compiled,
            None,
            None,
            async move {
                let mut mat = SessionMaterialization::new();
                let (out, _) = engine
                    .hydrate_query_summaries(
                        "LangSecuredNote",
                        std::slice::from_ref(&entity),
                        &cgs,
                        &mut mat,
                        ExecutionMode::Live,
                        true,
                        &env,
                    )
                    .await
                    .expect("list summary must survive a divergent detail GET");
                assert!(
                    mat.get(&Ref::new("LangSecuredNote", "2")).is_none(),
                    "detail Get for row 1 polluted row 2"
                );
                assert_eq!(out.len(), 1);
                assert_eq!(out[0].reference, entity.reference);
                assert_eq!(
                    out[0].completeness,
                    crate::cache::EntityCompleteness::Summary
                );
            },
        )
        .await;
    }

    /// Boundary log row captured inside the mock HTTP transport for hydrate GETs.
    #[derive(Clone, Debug)]
    struct HydrateBoundaryRow {
        requested_path_id: String,
        path: String,
        raw_returned_id: String,
        raw_code: String,
        raw_member: String,
    }

    /// Fixture membership (`title` ≈ invitation_code, `body` ≈ member identity).
    fn secured_note_fixture_membership(id: &str) -> (&'static str, &'static str) {
        match id {
            "1" => ("code-cairo", "member-ka"),
            "2" => ("code-venice", "member-jer"),
            "3" => ("code-seoul", "member-morgan"),
            _ => panic!("unknown fixture id {id}"),
        }
    }

    fn parse_secured_note_path_id(path: &str) -> String {
        path.rsplit('/')
            .next()
            .expect("secured_notes path has id segment")
            .to_string()
    }

    async fn run_secured_note_hydrate_boundary(
        concurrency: usize,
        wrong_identity_for: Option<&'static str>,
    ) -> (
        Vec<CachedEntity>,
        SessionMaterialization,
        Vec<HydrateBoundaryRow>,
    ) {
        use crate::auth::ResolvedAuth;
        use crate::execution::observation_honesty::{run_release_wave, BarrierSchedule};
        use crate::http_transport::HttpTransport;
        use crate::ExecuteSessionMaterial;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::Ref;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct BoundaryTransport {
            log: Arc<Mutex<Vec<HydrateBoundaryRow>>>,
            wrong_identity_for: Option<&'static str>,
            schedule: Option<Arc<BarrierSchedule>>,
        }

        #[async_trait]
        impl HttpTransport for BoundaryTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                assert!(
                    request.path.contains("/secured_notes/"),
                    "expected hydrate GET, got {}",
                    request.path
                );
                let requested = parse_secured_note_path_id(&request.path);
                if let Some(sched) = &self.schedule {
                    sched.park(requested.clone()).await;
                }
                let (returned_id, code, member) =
                    if self.wrong_identity_for == Some(requested.as_str()) {
                        (
                            "99".to_string(),
                            "code-injected".to_string(),
                            "member-injected".to_string(),
                        )
                    } else {
                        let (c, m) = secured_note_fixture_membership(&requested);
                        (requested.clone(), c.to_string(), m.to_string())
                    };
                self.log.lock().unwrap().push(HydrateBoundaryRow {
                    requested_path_id: requested.clone(),
                    path: request.path.clone(),
                    raw_returned_id: returned_id.clone(),
                    raw_code: code.clone(),
                    raw_member: member.clone(),
                });
                if let Some(schedule) = &self.schedule {
                    schedule.complete(&requested);
                }
                Ok((
                    serde_json::json!({
                        "note_id": returned_id,
                        "title": code,
                        "body": member,
                    }),
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

        let log = Arc::new(Mutex::new(Vec::new()));
        let schedule = if concurrency > 1 {
            Some(BarrierSchedule::new())
        } else {
            None
        };
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");
        let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
        let material = Arc::new(ExecuteSessionMaterial {
            catalog_revision: "langmatrix".into(),
            compiled_catalog: compiled.clone(),
            credential_store: None,
            prompt_hash: "a".repeat(64),
            session_id: "b".repeat(32),
            transport_origin: None,
            ui_origin: None,
            catalog_bind: None,
            login_access_token_tail: ExecuteSessionMaterial::empty_login_access_token_tail(),
        });
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                hydrate_concurrency: concurrency.max(1),
                ..ExecutionConfig::default()
            },
            Arc::new(BoundaryTransport {
                log: log.clone(),
                wrong_identity_for,
                schedule: schedule.clone(),
            }),
            None,
        );
        let summaries: Vec<CachedEntity> = ["1", "2", "3"]
            .into_iter()
            .map(|id| {
                let (code, _) = secured_note_fixture_membership(id);
                CachedEntity::from_decoded(
                    Ref::new("LangSecuredNote", id),
                    [
                        ("note_id".into(), Value::String(id.into())),
                        ("title".into(), Value::String(format!("list-{code}"))),
                    ]
                    .into_iter()
                    .collect(),
                    indexmap::IndexMap::new(),
                    1,
                    crate::cache::EntityCompleteness::Summary,
                )
            })
            .collect();
        let env = env_with(&[("access_token", "tok")]);
        let base: Arc<str> = Arc::from("http://127.0.0.1:9");
        // Concurrent witness: reverse arrival-friendly order (3,1,2) via explicit release.
        let release_order = vec!["3".into(), "1".into(), "2".into()];
        ExecutionEngine::run_in_execute_task_scopes(
            base,
            None,
            None,
            None,
            Some(material),
            compiled,
            None,
            None,
            async move {
                let mut mat = SessionMaterialization::new();
                let hydrate = engine.hydrate_query_summaries(
                    "LangSecuredNote",
                    &summaries,
                    &cgs,
                    &mut mat,
                    ExecutionMode::Live,
                    true,
                    &env,
                );
                let (out, _) = if let Some(sched) = schedule {
                    let release = run_release_wave(&sched, &release_order);
                    let (hydrate_res, _) = tokio::join!(hydrate, release);
                    let (out, _) = hydrate_res.expect("hydrate boundary run");
                    assert!(
                        sched.max_in_flight() >= 3,
                        "witness must observe ≥3 GETs in flight before release"
                    );
                    (out, 0usize)
                } else {
                    hydrate.await.expect("hydrate boundary run")
                };
                let boundaries = log.lock().unwrap().clone();
                assert_eq!(
                    boundaries.len(),
                    3,
                    "expected one GET per summary; boundaries={boundaries:?}"
                );
                (out, mat, boundaries)
            },
        )
        .await
    }

    fn assert_published_matches_fixture(out: &[CachedEntity], expect_complete: &[&str]) {
        use plasm_core::Ref;
        for id in expect_complete {
            let row = out
                .iter()
                .find(|e| e.reference == Ref::new("LangSecuredNote", *id))
                .unwrap_or_else(|| panic!("missing published row {id}"));
            let (code, member) = secured_note_fixture_membership(id);
            assert_eq!(
                row.fields.get("title").map(|f| f.to_value()),
                Some(Value::String(code.into())),
                "published title/code for {id}"
            );
            assert_eq!(
                row.fields.get("body").map(|f| f.to_value()),
                Some(Value::String(member.into())),
                "published member/body for {id}"
            );
            assert_eq!(
                row.completeness,
                crate::cache::EntityCompleteness::Complete,
                "row {id} should be Complete after matching hydrate"
            );
        }
    }

    /// Serial hydrate: request ↔ raw ↔ decode ↔ merge ↔ publish stay fixture-faithful.
    #[tokio::test]
    async fn proximal_serial_hydrate_preserves_fixture_membership() {
        let (out, mat, boundaries) = run_secured_note_hydrate_boundary(1, None).await;
        for b in &boundaries {
            assert_eq!(
                b.requested_path_id, b.raw_returned_id,
                "raw HTTP identity must match requested path id (path={}): {b:?}",
                b.path
            );
            let (code, member) = secured_note_fixture_membership(&b.requested_path_id);
            assert_eq!(b.raw_code, code);
            assert_eq!(b.raw_member, member);
        }
        assert_published_matches_fixture(&out, &["1", "2", "3"]);
        assert!(mat
            .get(&plasm_core::Ref::new("LangSecuredNote", "99"))
            .is_none());
    }

    /// Concurrent hydrate with forced response reordering must not cross-wire fields.
    #[tokio::test]
    async fn proximal_concurrent_hydrate_preserves_fixture_membership() {
        let (out, mat, boundaries) = run_secured_note_hydrate_boundary(16, None).await;
        for b in &boundaries {
            assert_eq!(
                b.requested_path_id, b.raw_returned_id,
                "well-behaved mock raw identity matches request: {b:?}"
            );
        }
        let _completion_order: Vec<&str> = boundaries
            .iter()
            .map(|b| b.requested_path_id.as_str())
            .collect();
        assert_published_matches_fixture(&out, &["1", "2", "3"]);
        assert!(mat
            .get(&plasm_core::Ref::new("LangSecuredNote", "99"))
            .is_none());
    }

    /// Wrong-identity inject: Plasm must reject the divergent body without contaminating
    /// retained neighbors (mirrors live `hydrate_get_identity_divergent`).
    #[tokio::test]
    async fn proximal_wrong_identity_inject_rejects_without_contaminating_neighbors() {
        let (out, mat, boundaries) = run_secured_note_hydrate_boundary(16, Some("2")).await;
        let injected = boundaries
            .iter()
            .find(|b| b.requested_path_id == "2")
            .expect("GET for id 2");
        assert_eq!(injected.raw_returned_id, "99");
        assert_eq!(injected.raw_code, "code-injected");

        assert_published_matches_fixture(&out, &["1", "3"]);

        let row2 = out
            .iter()
            .find(|e| e.reference == plasm_core::Ref::new("LangSecuredNote", "2"))
            .expect("summary for requested id 2 retained");
        assert_eq!(
            row2.completeness,
            crate::cache::EntityCompleteness::Summary,
            "divergent detail must not upgrade id 2 to Complete"
        );
        assert_ne!(
            row2.fields.get("body").map(|f| f.to_value()),
            Some(Value::String("member-injected".into())),
            "injected member must not land on retained id 2"
        );
        assert_ne!(
            row2.fields.get("title").map(|f| f.to_value()),
            Some(Value::String("code-injected".into())),
            "injected code must not land on retained id 2"
        );
        assert!(
            mat.get(&plasm_core::Ref::new("LangSecuredNote", "99"))
                .is_none(),
            "divergent identity must not be inserted"
        );
    }

    /// Bounded storage: TopK heap retains winners; discarded scanned rows must not stay cached.
    #[tokio::test]
    async fn proximal_topk_does_not_retain_discarded_rows() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use crate::top_k::TopKSpec;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::loader::load_schema_dir;
        use plasm_core::{Expr, QueryExpr};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        #[derive(Clone)]
        struct IndexedList {
            list_pages: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl HttpTransport for IndexedList {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                if request.path.contains("/indexed/") {
                    return Err(RuntimeError::CacheError {
                        message: "404 detail".into(),
                    });
                }
                let page = self.list_pages.fetch_add(1, Ordering::SeqCst);
                if page > 0 {
                    return Ok((serde_json::json!({ "results": [] }), None));
                }
                Ok((
                    serde_json::json!({
                        "results": [
                            {"id": "id-0", "n": 0},
                            {"id": "id-1", "n": 1}
                        ]
                    }),
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
                .join("../../fixtures/schemas/plasm_pagination_matrix"),
        )
        .expect("pagination matrix CGS");
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..ExecutionConfig::default()
            },
            Arc::new(IndexedList {
                list_pages: Arc::new(AtomicUsize::new(0)),
            }),
            None,
        );
        let query = QueryExpr::all("Item");
        let consume = StreamConsumeOpts {
            fetch_all: true,
            top_k: Some(TopKSpec {
                count: 1,
                sort_key: vec!["n".into()],
                descending: true,
                row_filter: Vec::new(),
            }),
            ..StreamConsumeOpts::default()
        };
        let mut mat = SessionMaterialization::new();
        let result = engine
            .execute(
                &Expr::Query(query),
                &cgs,
                &mut mat,
                Some(ExecutionMode::Live),
                consume,
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .expect("RA-16 TopK paginated query must keep list Refs");
        assert!(
            result.count >= 1,
            "TopK must return listed rows, got {result:?}"
        );
        assert!(
            mat.get(&plasm_core::Ref::new("Item", "id-0")).is_none(),
            "TopK loser remains in graph"
        );
        let ids: Vec<String> = result
            .entities
            .iter()
            .map(|e| e.reference.primary_slot_str().to_string())
            .collect();
        assert!(
            ids.contains(&"id-0".to_string()) || ids.contains(&"id-1".to_string()),
            "expected list identities, got {ids:?}"
        );
    }
}
