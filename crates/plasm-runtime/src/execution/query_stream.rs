//! Query execution streams (paginated, non-paginated, cross-entity).

use super::*;
use crate::view_plan::ViewAmbientContext;

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
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
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
                yield PageResult::from_execution_result(res);
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
            yield PageResult::from_execution_result(res);
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
            if let Some(cached_entities) = ExecutionCacheConsult::decide_query(
                &query,
                cap_name,
                &snapshot,
                &mat.query_index,
                cgs,
                &mut consult,
            ) {
                let count = cached_entities.len();
                let mut stats = ExecutionStats::from_telemetry(consult, 0);
                stats.record_rows_materialized(count);
                yield PageResult {
                    entities: cached_entities,
                    page_index: 0,
                    has_more: false,
                    coverage: ResultCoverage::Unknown,
                    pagination_resume: None,
                    stats,
                    operations: OperationLedger::empty(),
                };
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

            let response_completeness = {
                let all_entity_fields: std::collections::HashSet<String> = cgs
                    .get_entity(&query.entity)
                    .map(|e| {
                        e.fields
                            .keys()
                            .map(|k| k.as_str().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let provided: std::collections::HashSet<String> =
                    cgs.effective_provides(&capability).into_iter().collect();
                if provided.is_superset(&all_entity_fields) {
                    EntityCompleteness::Complete
                } else {
                    EntityCompleteness::Summary
                }
            };
            let hydrate_run = query.hydrate.unwrap_or(self.config.hydrate);
            let mut res = query_result_merge_cache(
                decoded_entities,
                response_completeness,
                source,
                mat,
                1,
            )?;
            res.stats.merge_telemetry(&consult);
            res.stats.record_rows_materialized(res.count);
            ExecutionCacheConsult::index_query_result(mat, &query, cap_name, &res.entities);
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
            let hydrate_partial = res
                .entities
                .iter()
                .any(CachedEntity::has_unavailable_detail_fields);

            if let Some(pred) = &query.predicate {
                if let Some(entity_def) = cgs.get_entity(&query.entity) {
                    let cap_params = capability_param_names(&capability);
                    if let Some(entity_pred) =
                        entity_field_predicate(pred, entity_def, Some(&cap_params))
                    {
                        res.entities =
                            filter_entities_by_predicate(res.entities, &entity_pred)?;
                        res.count = res.entities.len();
                    }
                }
            }

            yield PageResult {
                entities: res.entities,
                page_index: 0,
                has_more: false,
                coverage: if hydrate_partial {
                    ResultCoverage::Partial
                } else {
                    ResultCoverage::Complete
                },
                pagination_resume: None,
                stats: res.stats,
                operations: OperationLedger::empty(),
            };
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

        let stream = Box::pin(async_stream::try_stream! {
            let mut pages = 0usize;
            let mut accumulated_total = 0usize;
            let mut collector = crate::paginated_collect::PageCollector::new(&consume);
            #[allow(unused_assignments)]
            let mut last_coverage = ResultCoverage::Unknown;

            loop {
                cooperative_cancel_check()?;
                if pages >= MAX_PAGES {
                    Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "Pagination stopped after {} pages (safety cap). Refine filters or increase cap in engine.",
                            MAX_PAGES
                        ),
                    })?;
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
                let mut decoded_entities = decode_entities_with_cgs(&decoder, &normalized, Some(cgs))?;
                let full_page_len = decoded_entities.len();
                let last_id = decoded_entities
                    .last()
                    .map(|d| d.reference.primary_slot_str());

                let mut truncated = false;
                if let Some(cap) = consume.max_items {
                    let remain = cap.saturating_sub(accumulated_total);
                    if decoded_entities.len() > remain {
                        decoded_entities.truncate(remain);
                        truncated = true;
                    }
                }

                let timestamp = current_timestamp();
                let page_cached: Vec<CachedEntity> = decoded_entities
                    .into_iter()
                    .map(|decoded| {
                        CachedEntity::from_decoded(
                            decoded.reference,
                            decoded.fields,
                            decoded.relations,
                            timestamp,
                            EntityCompleteness::Summary,
                        )
                    })
                    .collect();
                let page_ids: Vec<String> = page_cached
                    .iter()
                    .map(|e| e.reference.primary_slot_str())
                    .collect();
                let audit = driver.record_page(
                    pages as u32,
                    &page_ids,
                    full_page_len as u32,
                    page_cached.len() as u32,
                    None,
                    None,
                )?;
                crate::record_live_page_audit(audit);
                accumulated_total += page_cached.len();

                if !collector.skips_pre_page_merge() {
                    mat.merge(page_cached.clone())?;
                }

                let hydrate_run = query.hydrate.unwrap_or(self.config.hydrate);
                let inherit = CapabilityParamEnv::from_cml_env(&env, &capability);
                stamp_entities_and_mat(&page_cached, mat, &inherit);
                let (hydrated, extra_net) = self
                    .hydrate_query_summaries(
                        &query.entity,
                        &page_cached,
                        cgs,
                        mat,
                        mode,
                        hydrate_run,
                        &env,
                    )
                    .await?;

                let cap_params = capability_param_names(&capability);
                let entities = match query.predicate.as_ref().and_then(|pred| {
                    cgs.get_entity(&query.entity)
                        .and_then(|e| entity_field_predicate(pred, e, Some(&cap_params)))
                }) {
                    Some(entity_pred) => {
                        filter_entities_by_predicate(hydrated, &entity_pred)?
                    }
                    None => hydrated,
                };
                let hydrate_field_partial = entities
                    .iter()
                    .any(CachedEntity::has_unavailable_detail_fields);

                let ingest = collector.ingest_page(entities);
                if !ingest.merge_into_mat.is_empty() {
                    mat.merge(ingest.merge_into_mat.clone())?;
                }
                report_rows_materialized(ingest.progress_rows);

                let page_http = if http_live { 1 } else { 0 };
                let page_net = page_http + extra_net;
                let page_cache_misses = ingest.progress_rows;
                if graph_backed {
                    if let Some(ref spill) = graph_page_spill {
                        let spill_rows = if ingest.yield_entities.is_empty() && !collector.skips_pre_page_merge() {
                            page_cached.clone()
                        } else {
                            ingest.yield_entities.clone()
                        };
                        graph_spill_page_and_trim_hot(spill, mat, pages, &spill_rows).await?;
                    }
                }
                let yield_entities = if graph_backed {
                    Vec::new()
                } else {
                    ingest.yield_entities
                };

                let page_stats = ExecutionStats {
                    duration_ms: 0,
                    network_requests: page_net,
                    cache_hits: 0,
                    cache_misses: page_cache_misses,
                    ..Default::default()
                };

                if single_http_roundtrip {
                    let continue_pages = driver.advance_after_page(
                        &normalized,
                        full_page_len,
                        link_next.as_deref(),
                        last_id.as_deref(),
                    )?;
                    let pagination_resume = if continue_pages {
                        Some(QueryPaginationResumeData {
                            query: query.clone(),
                            capability_name: capability.name.to_string(),
                            env: env.clone(),
                            template: capability_template.clone(),
                            config: pconf.clone(),
                            state: driver.snapshot(),
                        })
                    } else {
                        None
                    };
                    let hit_max = consume.max_items.is_some_and(|m| accumulated_total >= m);
                    let stop = if truncated || hit_max {
                        ConsumeStop::ItemCapHit {
                            truncated_page: truncated,
                            driver_has_more: Some(continue_pages),
                        }
                    } else if continue_pages {
                        ConsumeStop::BackendHasMore
                    } else {
                        ConsumeStop::BackendExhausted
                    };
                    let mut page = page_result(
                        yield_entities,
                        pages,
                        &consume,
                        stop,
                        pagination_resume,
                        page_stats,
                    );
                    if hydrate_field_partial { page.coverage = page.coverage.combine(ResultCoverage::Partial); }
                    last_coverage = page.coverage;
                    yield page;
                    break;
                }
                if truncated {
                    let mut page = page_result(
                        yield_entities,
                        pages,
                        &consume,
                        ConsumeStop::ItemCapHit {
                            truncated_page: true,
                            driver_has_more: None,
                        },
                        None,
                        page_stats,
                    );
                    if hydrate_field_partial { page.coverage = page.coverage.combine(ResultCoverage::Partial); }
                    last_coverage = page.coverage;
                    yield page;
                    break;
                }
                if consume
                    .max_items
                    .is_some_and(|m| accumulated_total >= m)
                {
                    let mut page = page_result(
                        yield_entities,
                        pages,
                        &consume,
                        ConsumeStop::ItemCapHit {
                            truncated_page: false,
                            driver_has_more: None,
                        },
                        None,
                        page_stats,
                    );
                    if hydrate_field_partial { page.coverage = page.coverage.combine(ResultCoverage::Partial); }
                    last_coverage = page.coverage;
                    yield page;
                    break;
                }
                if ingest.row_match_budget_satisfied {
                    let mut page = page_result(
                        yield_entities,
                        pages,
                        &consume,
                        ConsumeStop::RowMatchBudgetSatisfied,
                        None,
                        page_stats,
                    );
                    if hydrate_field_partial { page.coverage = page.coverage.combine(ResultCoverage::Partial); }
                    last_coverage = page.coverage;
                    yield page;
                    break;
                }
                if full_page_len == 0 && !matches!(pconf.location, plasm_compile::PaginationLocation::BlockRange) {
                    let mut page = page_result(
                        yield_entities,
                        pages,
                        &consume,
                        ConsumeStop::EmptyPageUnproven,
                        None,
                        page_stats,
                    );
                    if hydrate_field_partial { page.coverage = page.coverage.combine(ResultCoverage::Partial); }
                    last_coverage = page.coverage;
                    yield page;
                    break;
                }

                let continue_pages = driver.advance_after_page(
                    &normalized,
                    full_page_len,
                    link_next.as_deref(),
                    last_id.as_deref(),
                )?;

                let stop = if continue_pages {
                    ConsumeStop::BackendHasMore
                } else {
                    ConsumeStop::BackendExhausted
                };
                let mut page = page_result(
                    yield_entities,
                    pages,
                    &consume,
                    stop,
                    None,
                    page_stats,
                );
                if hydrate_field_partial { page.coverage = page.coverage.combine(ResultCoverage::Partial); }
                    last_coverage = page.coverage;
                yield page;

                pages += 1;

                if !continue_pages {
                    break;
                }
            }

            if let Some(final_entities) = collector.finish() {
                mat.merge(final_entities.clone())?;
                let yield_entities = if graph_backed {
                    Vec::new()
                } else {
                    final_entities
                };
                yield PageResult {
                    entities: yield_entities,
                    page_index: pages,
                    has_more: false,
                    coverage: last_coverage,
                    pagination_resume: None,
                    stats: ExecutionStats::default(),
                    operations: OperationLedger::empty(),
                };
            }
        });

        Ok(stream)
    }
}
