//! Query execution streams (paginated, non-paginated, cross-entity).

use super::*;
use crate::view_plan::ViewAmbientContext;

/// A complete response needs both declared coverage and observed field presence.
/// Pagination changes collection coverage, never the field coverage of each row.
struct QueryResponseCoverage {
    fields: Vec<String>,
    declared_complete: bool,
}

impl QueryResponseCoverage {
    fn new(cgs: &CGS, capability: &CapabilitySchema) -> Self {
        let fields: Vec<String> = cgs
            .get_entity(capability.domain.as_str())
            .map(|entity| {
                entity
                    .fields
                    .keys()
                    .map(|field| field.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let provided: std::collections::HashSet<String> =
            cgs.effective_provides(capability).into_iter().collect();
        let declared_complete = cgs.get_entity(capability.domain.as_str()).is_some()
            && fields.iter().all(|field| provided.contains(field));
        Self {
            fields,
            declared_complete,
        }
    }

    fn classify(&self, row: &plasm_compile::DecodedEntity) -> EntityCompleteness {
        if self.declared_complete
            && self
                .fields
                .iter()
                .all(|field| row.fields.contains_key(field))
        {
            EntityCompleteness::Complete
        } else {
            EntityCompleteness::Summary
        }
    }
}

impl ExecutionEngine {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn query_to_stream<'a>(
        &'a self,
        query: &'a QueryExpr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        graph_page_spill: Option<crate::graph_page_spill::GraphPageSpillHandle>,
        ambient: &ViewAmbientContext,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        if let Some(pred) = &query.predicate {
            if let Some(source_entity) = cgs.get_entity(&query.entity) {
                let crosses = extract_cross_entity_predicates(pred, source_entity, cgs);
                if !crosses.is_empty() {
                    return self.cross_entity_query_stream(
                        query, &crosses, cgs, mat, mode, consume, ambient,
                    );
                }
            }
        }

        let filter = compile_query_dispatch(query, cgs)?;
        let capability = resolve_query_capability(query, cgs)?;
        let mut env = CmlEnv::new();
        if let Some(f) = &filter {
            let json_val = f.to_json();
            env.insert("filter".to_string(), json_to_plasm_value(&json_val));
        }
        if let Some(pred) = &query.predicate {
            extract_predicate_vars(pred, &mut env);
        }
        normalize_cml_env_inputs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;
        if let Some(proj) = &query.projection {
            env.insert(
                "projection".to_string(),
                Value::Array(proj.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }
        let capability_template = compiled_capability_template(capability)?;
        if let CapabilityTemplate::View(vt) = &capability_template {
            let view_name = vt.view.clone();
            let query = query.clone();
            let ambient = ambient.clone();
            let stream = Box::pin(async_stream::try_stream! {
                let res = crate::view_execution::execute_view_query(
                    self,
                    view_name.as_str(),
                    &query,
                    cgs,
                    mat,
                    mode,
                    &ambient,
                )
                .await?;
                yield ExecutionEvent::Complete(Box::new(res));
            });
            return Ok(stream);
        }
        if let Some(pconf) = template_pagination(&capability_template) {
            return self.paginated_query_stream(
                query.clone(),
                cgs,
                mat,
                mode,
                capability_template.clone(),
                pconf.clone(),
                env.clone(),
                capability,
                consume,
                None,
                graph_page_spill,
            );
        }

        self.non_paginated_query_stream(query, cgs, mat, mode, capability, capability_template, env)
    }
    #[allow(clippy::too_many_arguments)]
    fn cross_entity_query_stream<'a>(
        &'a self,
        query: &'a QueryExpr,
        crosses: &[plasm_core::cross_entity::CrossEntityPredicate],
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        ambient: &ViewAmbientContext,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        let query = query.clone();
        let crosses = crosses.to_vec();
        let ambient = ambient.clone();
        let stream = Box::pin(async_stream::try_stream! {
            let res = self
                .execute_query_cross_entity(&query, &crosses, cgs, mat, mode, consume, &ambient)
                .await?;
            yield ExecutionEvent::Complete(Box::new(res));
        });
        Ok(stream)
    }

    #[allow(clippy::too_many_arguments)]
    fn non_paginated_query_stream<'a>(
        &'a self,
        query: &'a QueryExpr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: ExecutionMode,
        capability: &'a CapabilitySchema,
        capability_template: CapabilityTemplate,
        env: CmlEnv,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        let compiled = compile_operation_dispatch(&capability_template, &env)?;
        let query = query.clone();
        let capability = capability.clone();
        let stream = Box::pin(async_stream::try_stream! {
            let cap_name = capability.name.as_str();
            let snapshot = mat.snapshot();
            let mut consult = CacheTelemetry::default();
            if let Some((membership, cached_entities)) = ExecutionCacheConsult::decide_query(
                &query,
                cap_name,
                &snapshot,
                &mat.query_index,
                cgs,
                &env,
                &mut consult,
            ) {
                let count = cached_entities.len();
                let mut stats = ExecutionStats::from_telemetry(consult, 0);
                stats.record_rows_materialized(count);
                yield ExecutionEvent::Complete(Box::new(ExecutionResult {
                    collection: ExecutionCollection::materialized(membership, cached_entities.into())?,
                    has_more: false, pagination_resume: None, paging_handle: None,
                    source: ExecutionSource::Cache, stats, request_fingerprints: vec![],
                    operations: OperationLedger::empty(),
                }));
                return;
            }
            ExecutionCacheConsult::record_query_network(&mut consult);

            let (response, source) = with_dispatch_entity(
                Some(query.entity.as_str()),
                self.execute_with_replay(&compiled, mode, Some(mat)),
            )
            .await?;
            let (normalized, decoder) = match &capability_template {
                CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => Ok((
                    prepare_http_query_response(response, cml, &env),
                    create_entity_decoder_for_capability(
                        &query.entity,
                        cgs,
                        Some(capability.name.as_str()),
                        Some(http_collection_source(cml)),
                        None,
                        Some(&cml_env_to_identity_strings(&env)),
                    ),
                )),
                CapabilityTemplate::View(_) | CapabilityTemplate::CredentialBind(_) => Err(RuntimeError::ConfigurationError {
                    message: "internal: view query must use composed-read stream".into(),
                }),
                CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => {
                    Err(RuntimeError::ConfigurationError {
                        message: "query/search capabilities must use HTTP CML templates".into(),
                    })
                }
            }?;
            let decoded_entities = decode_entities_with_cgs(&decoder, &normalized, Some(cgs))?;

            let response_coverage = QueryResponseCoverage::new(cgs, &capability);
            let hydrate_run = query.hydrate.unwrap_or(self.config.hydrate);
            let mut res = query_result_merge_cache(
                decoded_entities,
                |row| response_coverage.classify(row),
                source,
                mat,
                1,
            )?;
            res.stats.merge_telemetry(&consult);
            res.stats.record_rows_materialized(res.entities.len());
            let inherit = CapabilityParamEnv::from_cml_env(&env, &capability);
            stamp_entities_and_mat(&res.entities, mat, &inherit);
            let (entities, extra_net) = self
                .hydrate_query_summaries(
                    &query.entity,
                    &res.entities,
                    cgs,
                    mat,
                    mode,
                    hydrate_run,
                    &env,
                )
                .await?;
            res.entities = entities;
            res.stats.network_requests += extra_net;

            if let Some(pred) = &query.predicate {
                if let Some(entity_def) = cgs.get_entity(&query.entity) {
                    let cap_params = capability_param_names(&capability);
                    if let Some(entity_pred) =
                        entity_field_predicate(pred, entity_def, Some(&cap_params))
                    {
                        res.entities =
                            filter_entities_by_predicate(res.entities, &entity_pred)?;
                    }
                }
            }

            let membership = ExecutionCacheConsult::index_query_result(mat, &query, cap_name, &res.entities, cgs, &env)?;
            yield ExecutionEvent::Complete(Box::new(ExecutionResult {
                collection: ExecutionCollection::materialized(membership, res.entities.into())?,
                has_more: false, pagination_resume: None, paging_handle: None,
                source, stats: res.stats, request_fingerprints: vec![], operations: OperationLedger::empty(),
            }));
        });
        Ok(stream)
    }

    /// Paginated query: one HTTP round-trip per stream item (page).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paginated_query_stream<'a>(
        &'a self,
        query: QueryExpr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: ExecutionMode,
        capability_template: CapabilityTemplate,
        pconf: PaginationConfig,
        env: CmlEnv,
        capability: &'a CapabilitySchema,
        consume: StreamConsumeOpts,
        resume_state: Option<PaginationLoopState>,
        graph_page_spill: Option<crate::graph_page_spill::GraphPageSpillHandle>,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        const MAX_PAGES: usize = 10_000;

        let user = query.pagination.clone().unwrap_or_default();
        let mut driver = match resume_state {
            Some(s) => {
                let contract = pconf
                    .validate()
                    .map_err(|e| RuntimeError::ConfigurationError {
                        message: e.to_string(),
                    })?;
                super::pagination_driver::PaginationDriver::from_resume(contract, s)
            }
            None => {
                super::pagination_driver::PaginationDriver::try_from_config(pconf, &user, &consume)?
            }
        };
        let pconf = driver.config().clone();
        let single_http_roundtrip = !consume.fetch_all
            && !matches!(
                pconf.location,
                plasm_compile::PaginationLocation::BlockRange
            )
            && (consume.max_items.is_none() || consume.one_page);
        let (decoder, wrap_key) = match &capability_template {
            plasm_compile::CapabilityTemplate::Http(ref req)
            | plasm_compile::CapabilityTemplate::GraphQl(ref req) => (
                create_entity_decoder_for_capability(
                    &query.entity,
                    cgs,
                    Some(capability.name.as_str()),
                    Some(http_collection_source(req)),
                    None,
                    Some(&cml_env_to_identity_strings(&env)),
                ),
                response_bare_array_wrap_key(req),
            ),
            plasm_compile::CapabilityTemplate::View(_)
            | plasm_compile::CapabilityTemplate::CredentialBind(_) => {
                return Err(RuntimeError::ConfigurationError {
                    message: "composed views do not support CML pagination".into(),
                });
            }
            plasm_compile::CapabilityTemplate::EvmCall(_)
            | plasm_compile::CapabilityTemplate::EvmLogs(_) => (
                create_entity_decoder(
                    &query.entity,
                    cgs,
                    Some(PathExpr::new(vec![
                        PathSegment::Key {
                            name: "results".to_string(),
                        },
                        PathSegment::Wildcard,
                    ])),
                    None,
                    Some(&cml_env_to_identity_strings(&env)),
                ),
                "results".to_string(),
            ),
        };
        let base_compiled = compile_operation_dispatch(&capability_template, &env)?;
        let capability = capability.clone();
        let graph_backed = consume.graph_backed_result;
        let response_coverage = QueryResponseCoverage::new(cgs, &capability);

        let identity = plasm_core::collection_codec::CollectionIdentity::for_expression(
            cgs,
            &(
                &query,
                &env,
                driver.snapshot(),
                consume
                    .row_match_budget
                    .as_ref()
                    .map(|b| (&b.predicates, b.count)),
            ),
            mat.graph.stats().version,
        )?;
        let collector =
            crate::paginated_collect::PageCollector::new(&consume, cgs, query.entity.as_str())?;
        let stream = Box::pin(async_stream::try_stream! {
            use plasm_core::collection_codec::{CollectionCodec, Demand, PageTermination, RecordingCodec, SharedRows, Transform};
            let codec = RecordingCodec::new();
            let mut acquisition = codec.acquire(identity.clone());
            let mut pages = 0usize;
            let mut retained_total = 0usize;
            let mut total_network = 0usize;
            let mut any_live = false;
            let mut collector = collector;
            let top_k = consume.top_k.is_some();
            let prefix = if consume.bound_kind == ConsumeBoundKind::ExpressionTake && !top_k {
                consume.row_match_budget.as_ref().map(|budget| budget.count).or(consume.max_items)
            } else { None };
            let (has_more, pagination_resume) = loop {
                cooperative_cancel_check()?;
                if prefix == Some(0) { break (false, None); }
                if pages >= MAX_PAGES {
                    Err(RuntimeError::ConfigurationError { message: format!("Pagination stopped after {MAX_PAGES} pages (safety cap)") })?;
                }
                let (response, link_next, http_live) =
                    if let Some(url) = driver.take_next_absolute_url() {
                        if matches!(&base_compiled, CompiledOperation::Http(request) if request.credential.is_some()) {
                            Err(crate::credentials::credential_error("scoped credential pagination requires declared request parameters, not an absolute continuation URL"))?;
                        }
                        if mode != ExecutionMode::Live {
                            Err(RuntimeError::ConfigurationError {
                                message: "absolute-URL pagination beyond the first page requires Live execution mode (replay/hybrid do not store Link headers or body next URLs)".to_string(),
                            })?;
                        }
                        let (j, link) = with_dispatch_entity(
                            Some(query.entity.as_str()),
                            self.get_json_absolute(&url),
                        )
                        .await?;
                        (j, link, true)
                    } else {
                        let mut compiled = base_compiled.clone();
                        driver.apply_request_params(&mut compiled)?;
                        let (j, link, src) = with_dispatch_entity(
                            Some(query.entity.as_str()),
                            self.execute_with_replay_full(&compiled, mode, Some(mat)),
                        )
                        .await?;
                        (j, link, src == ExecutionSource::Live)
                    };


                let normalized = normalize_collection_response(response, wrap_key.as_str());
                let decoded = decode_entities_with_cgs(&decoder, &normalized, Some(cgs))?;
                let full_page_len = decoded.len();
                let last_id = decoded.last().map(|row| row.reference.primary_slot_str());
                let timestamp = current_timestamp();
                let page_cached: Vec<CachedEntity> = decoded.into_iter().map(|row| {
                    let completeness = response_coverage.classify(&row);
                    CachedEntity::from_decoded(row.reference, row.fields, row.relations, timestamp, completeness)
                }).collect();
                let page_ids: Vec<_> = page_cached.iter().map(|row| row.reference.primary_slot_str()).collect();
                crate::record_live_page_audit(driver.record_page(pages as u32, &page_ids, full_page_len as u32, page_cached.len() as u32, None, None)?);
                if !collector.skips_pre_page_merge() { mat.merge(page_cached.clone())?; }
                let inherit = CapabilityParamEnv::from_cml_env(&env, &capability);
                stamp_entities_and_mat(&page_cached, mat, &inherit);
                let cap_params = capability_param_names(&capability);
                let predicate = query.predicate.as_ref().and_then(|pred| cgs.get_entity(&query.entity)
                    .and_then(|entity| entity_field_predicate(pred, entity, Some(&cap_params))));
                // A plain expression prefix selects source occurrences before detail IO.
                // Filters and top-k need their candidates; host paging must retain the
                // backend page because its continuation starts at the next page.
                let hydration_input = if predicate.is_none()
                    && consume.row_match_budget.as_ref().is_none_or(|budget| budget.predicates.is_empty())
                {
                    match prefix {
                        Some(bound) => &page_cached[..page_cached.len().min(bound.saturating_sub(retained_total))],
                        None => &page_cached[..],
                    }
                } else { &page_cached[..] };
                let omitted_prefix_rows = page_cached.len() - hydration_input.len();
                let (hydrated, extra_net) = self.hydrate_query_summaries(
                    &query.entity, hydration_input, cgs, mat, mode, query.hydrate.unwrap_or(self.config.hydrate), &env,
                ).await?;
                let entities = match &predicate {
                    Some(pred) => filter_entities_by_predicate(hydrated, pred)?,
                    None => hydrated,
                };
                let top_k_input = if top_k { entities.iter().map(|row| row.reference.clone()).collect::<Vec<_>>() } else { vec![] };
                let mut ingest = collector.ingest_page(entities)?;
                let decoded_members = if top_k { top_k_input.len() } else { ingest.yield_entities.len() + omitted_prefix_rows };
                if !top_k && consume.bound_kind != ConsumeBoundKind::HostPage {
                    if let Some(cap) = prefix.or(consume.max_items) {
                        ingest.yield_entities.truncate(cap.saturating_sub(retained_total));
                    }
                }
                if !ingest.merge_into_mat.is_empty() { mat.merge(ingest.merge_into_mat)?; }
                report_rows_materialized(ingest.progress_rows);
                let references = if top_k { top_k_input } else { ingest.yield_entities.iter().map(|row| row.reference.clone()).collect() };
                retained_total += references.len();
                let more = driver.advance_after_page(&normalized, full_page_len, link_next.as_deref(), last_id.as_deref())?;
                let hit_bound = prefix.or(consume.max_items).is_some_and(|cap| retained_total >= cap);
                let empty_unproven = more && full_page_len == 0 && !matches!(pconf.location, plasm_compile::PaginationLocation::BlockRange);
                let done = !more || single_http_roundtrip || hit_bound || ingest.row_match_budget_satisfied || empty_unproven;
                // A continuation after a local filter does not prove another match exists.
                let termination = if !more { PageTermination::Exhausted }
                    else if done && (predicate.is_some() || consume.row_match_budget.is_some() || empty_unproven) { PageTermination::Unproven }
                    else { PageTermination::More };
                acquisition.push(&identity, pages, references.into(), decoded_members, termination)?;
                let page_net = usize::from(http_live) + extra_net;
                total_network += page_net;
                any_live |= http_live || extra_net > 0;
                let rows: SharedRows<CachedEntity> = ingest.yield_entities.into();
                if graph_backed {
                    if let Some(ref spill) = graph_page_spill {
                        // Spill precisely the retained output occurrences, never a raw pre-filter page.
                        graph_spill_page_and_trim_hot(spill, mat, pages, &rows).await?;
                    }
                }
                let mut stats = ExecutionStats::from_telemetry(CacheTelemetry::default(), page_net);
                stats.record_rows_materialized(rows.len());
                yield ExecutionEvent::Page { entities: if graph_backed { Vec::new().into() } else { rows }, stats };
                pages += 1;
                if done {
                    let expression_done = prefix.is_some_and(|bound| retained_total >= bound);
                    let resume = if more && !expression_done {
                        Some(QueryPaginationResumeData { query: query.clone(), capability_name: capability.name.to_string(), env: env.clone(),
                            template: capability_template.clone(), config: pconf.clone(), state: driver.snapshot() })
                    } else { None };
                    break (more && !expression_done, resume);
                }
            };
            let mut membership = match prefix { Some(bound) => acquisition.finish_prefix(bound)?, None => acquisition.finish() };
            if top_k {
                codec.materialize(&membership, Demand::Whole)?;
                let entities = collector.finish().unwrap_or_default();
                mat.merge(entities.clone())?;
                let references = entities.iter().map(|row| row.reference.clone()).collect();
                let spec = consume.top_k.as_ref().expect("top-k collector");
                membership = codec.derive(identity.derived(&("top_k", &spec.sort_key, spec.descending, spec.count, &spec.row_filter))?, &[&membership], Transform::Evaluate { rows: references })?;
                let entities: SharedRows<CachedEntity> = entities.into();
                if graph_backed {
                    if let Some(ref spill) = graph_page_spill { graph_spill_page_and_trim_hot(spill, mat, pages, &entities).await?; }
                }
                yield ExecutionEvent::Page { entities: if graph_backed { Vec::new().into() } else { entities }, stats: ExecutionStats::default() };
            }
            let collection = ExecutionCollection::graph(membership);
            let mut stats = ExecutionStats::from_telemetry(CacheTelemetry::default(), total_network);
            stats.record_rows_materialized(collection.count());
            yield ExecutionEvent::Complete(Box::new(ExecutionResult {
                collection, has_more, pagination_resume, paging_handle: None,
                source: if any_live { ExecutionSource::Live } else { ExecutionSource::Replay },
                stats, request_fingerprints: vec![], operations: OperationLedger::empty(),
            }));
        });
        Ok(stream)
    }
}

#[cfg(test)]
#[path = "query_stream_tests.rs"]
mod tests;
