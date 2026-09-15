//! Query execution entrypoints on ExecutionEngine.

use super::*;

impl ExecutionEngine {
    pub(crate) async fn execute_query(
        &self,
        query: &QueryExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        ambient: &ViewAmbientContext,
    ) -> Result<ExecutionResult, RuntimeError> {
        self.execute_query_inner(query, cgs, mat, mode, consume, ambient)
            .instrument(crate::spans::execute_query())
            .await
    }

    pub(crate) async fn execute_query_inner(
        &self,
        query: &QueryExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        ambient: &ViewAmbientContext,
    ) -> Result<ExecutionResult, RuntimeError> {
        let mut stream =
            self.query_to_stream(query, cgs, mat, mode, consume.clone(), None, ambient)?;
        collect_query_stream(&mut stream, &consume).await
    }

    /// Execute a query with cross-entity predicate decomposition.
    ///
    /// For each cross-entity predicate (e.g. `pet.status = available`):
    /// - **Push-left**: query the foreign entity first, collect matching IDs,
    ///   inject an FK equality predicate on the source query.
    /// - **Pull-right**: query source without the cross-entity predicate,
    ///   then client-side filter each row by fetching the foreign entity.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_query_cross_entity<'a>(
        &'a self,
        query: &'a QueryExpr,
        crosses: &'a [plasm_core::cross_entity::CrossEntityPredicate],
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        ambient: &'a ViewAmbientContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        let ambient = ambient.clone();
        Box::pin(async move {
            let source_entity =
                cgs.get_entity(&query.entity)
                    .ok_or_else(|| RuntimeError::ConfigurationError {
                        message: format!("Entity '{}' not found", query.entity),
                    })?;

            let mut push_left_preds: Vec<Predicate> = Vec::new();
            let mut pull_right_crosses: Vec<plasm_core::cross_entity::CrossEntityPredicate> =
                Vec::new();
            let mut total_network = 0usize;
            let mut any_live = false;

            for cross in crosses {
                match choose_strategy(cross, &query.entity, cgs) {
                    CrossEntityStrategy::PushLeft {
                        cross: c,
                        source_fk_param,
                    } => {
                        // Query foreign entity to get matching IDs.
                        let foreign_query =
                            QueryExpr::filtered(&c.foreign_entity, c.foreign_predicate.clone());
                        let foreign_result = self
                            .execute_query(
                                &foreign_query,
                                cgs,
                                mat,
                                mode,
                                StreamConsumeOpts {
                                    fetch_all: true,
                                    max_items: None,
                                    one_page: false,
                                    graph_backed_result: false,
                                    ..Default::default()
                                },
                                &ambient,
                            )
                            .await?;

                        total_network += foreign_result.stats.network_requests;
                        if foreign_result.source == ExecutionSource::Live {
                            any_live = true;
                        }

                        let foreign_ids: Vec<Value> = foreign_result
                            .entities
                            .iter()
                            .map(|e| Value::String(e.reference.primary_slot_str()))
                            .collect();

                        if foreign_ids.is_empty() {
                            return Ok(ExecutionResult {
                                entities: vec![],
                                count: 0,
                                has_more: false,
                                coverage: foreign_result.coverage,
                                pagination_resume: None,
                                paging_handle: None,
                                source: ExecutionSource::Live,
                                stats: ExecutionStats {
                                    duration_ms: 0,
                                    network_requests: total_network,
                                    cache_hits: 0,
                                    cache_misses: 0,
                                    ..Default::default()
                                },
                                request_fingerprints: Vec::new(),
                                operations: OperationLedger::empty(),
                            });
                        }

                        if foreign_ids.len() == 1 {
                            push_left_preds.push(Predicate::eq(
                                &source_fk_param,
                                foreign_ids.into_iter().next().unwrap(),
                            ));
                        } else {
                            push_left_preds
                                .push(Predicate::in_(&source_fk_param, Value::Array(foreign_ids)));
                        }
                    }
                    CrossEntityStrategy::PullRight { cross: c } => {
                        pull_right_crosses.push(c);
                    }
                }
            }

            // Build the rewritten query: local predicates + push-left FK predicates.
            let local_pred = strip_cross_entity_comparisons(
                query.predicate.as_ref().unwrap(),
                source_entity,
                cgs,
            );

            let mut all_preds: Vec<Predicate> = push_left_preds;
            if let Some(lp) = local_pred {
                all_preds.push(lp);
            }

            let rewritten_pred = match all_preds.len() {
                0 => None,
                1 => Some(all_preds.into_iter().next().unwrap()),
                _ => Some(Predicate::and(all_preds)),
            };

            let mut rewritten_query = query.clone();
            rewritten_query.predicate = rewritten_pred;

            let mut result = self
                .execute_query(&rewritten_query, cgs, mat, mode, consume, &ambient)
                .await?;
            result.stats.network_requests += total_network;
            if any_live {
                result.source = ExecutionSource::Live;
            }

            // Pull-right client-side filter for any crosses that couldn't push left.
            if !pull_right_crosses.is_empty() {
                let mut filtered = Vec::new();
                for entity in &result.entities {
                    let mut passes = true;
                    for cross in &pull_right_crosses {
                        let ref_id = extract_ref_id(entity, &cross.ref_field, cgs);
                        let Some(id) = ref_id else {
                            passes = false;
                            break;
                        };

                        let get = GetExpr::new(&cross.foreign_entity, &id);
                        let get_result = self.execute_get(&get, cgs, mat, mode, &ambient).await?;
                        result.stats.network_requests += get_result.stats.network_requests;

                        let Some(foreign) = get_result.entities.first() else {
                            passes = false;
                            break;
                        };

                        if !client_side_predicate_matches(foreign, &cross.foreign_predicate)? {
                            passes = false;
                            break;
                        }
                    }
                    if passes {
                        filtered.push(entity.clone());
                    }
                }
                result.entities = filtered;
                result.count = result.entities.len();
            }

            Ok(result)
        })
    }
}
