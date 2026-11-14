//! Operation-bound selection over the authoritative connected catalogue graph.
use super::*;
use crate::discovery_matcher::operations::{CandidateProgress, DecisionBatch, OperationMatrix};

/// Planning limits must not erase a previously recorded uncertain judgment.
pub(super) fn preserve_uncertain_judgments(
    matrix: &OperationMatrix,
    deferred: &mut BTreeMap<CapabilityRef, DeferredReason>,
) {
    for (reference, reason) in deferred.iter_mut() {
        let uncertain = match matrix.progress(reference) {
            Ok(CandidateProgress::Uncertain { .. }) => true,
            Ok(CandidateProgress::Relevant { uncertain, .. }) => !uncertain.is_empty(),
            _ => false,
        };
        if uncertain {
            *reason = DeferredReason::OperationUncertain;
        }
    }
}

fn collect_wave(
    index: &CatalogueSearch,
    matrix: &OperationMatrix,
    wave: &[(CapabilityRef, Vec<Edge<CapabilityRef>>)],
    judged: &mut BTreeMap<CapabilityRef, CapabilityIntentMatch>,
    routes: &mut BTreeMap<CapabilityRef, Vec<Edge<CapabilityRef>>>,
    deferred: &mut BTreeMap<CapabilityRef, DeferredReason>,
    pending: &mut BTreeSet<CapabilityRef>,
) -> Result<(), DiscoveryMatcherError> {
    for (reference, path) in wave {
        if matches!(deferred.get(reference), Some(DeferredReason::InputBytes)) {
            continue;
        }
        let choice = match matrix.progress(reference)? {
            CandidateProgress::Relevant {
                pending, uncertain, ..
            } => {
                if !uncertain.is_empty() {
                    deferred.insert(reference.clone(), DeferredReason::OperationUncertain);
                } else if !pending.is_empty() {
                    deferred.insert(reference.clone(), DeferredReason::OperationPending);
                }
                Some(MatchChoice::Relevant)
            }
            CandidateProgress::Unrelated => Some(MatchChoice::Unrelated),
            CandidateProgress::Incomplete { .. } => {
                judged.remove(reference);
                routes.remove(reference);
                deferred.insert(reference.clone(), DeferredReason::OperationPending);
                None
            }
            CandidateProgress::Uncertain { .. } => {
                judged.remove(reference);
                routes.remove(reference);
                deferred.insert(reference.clone(), DeferredReason::OperationUncertain);
                None
            }
        };
        if let Some(choice) = choice {
            if choice == MatchChoice::Relevant {
                routes.insert(reference.clone(), path.clone());
            }
            if !deferred.contains_key(reference) {
                pending.remove(reference);
            }
            judged.insert(
                reference.clone(),
                CapabilityIntentMatch {
                    capability_id: index.candidates[reference].id.clone(),
                    choice,
                },
            );
        }
    }
    Ok(())
}

pub(crate) async fn select_operations_indexed<F, Fut, O, OFut>(
    context: SearchContext<'_>,
    index: &CatalogueSearch,
    recall: &RetrievalReceipt,
    matrix: &mut OperationMatrix,
    deferred_priorities: &BTreeMap<CapabilityRef, usize>,
    mut decide: F,
    mut observe: O,
) -> Result<(RetrievalReceipt, CapabilityMatchReceipt, SearchReceipt), DiscoveryMatcherError>
where
    F: FnMut(DecisionBatch) -> Fut,
    Fut: std::future::Future<Output = Result<String, DecisionProviderError>>,
    O: FnMut(SearchProgress) -> OFut,
    OFut: std::future::Future<Output = Result<(), DecisionProviderError>>,
{
    let mut budget = SearchBudget::default();
    let mut judged = BTreeMap::new();
    let mut routes = BTreeMap::new();
    let mut deferred = BTreeMap::new();
    let mut pending = BTreeSet::new();
    let mut visits = context.visits.clone();
    let mut completed_waves = 0;
    let mut packet_cache = connected::PacketCache::default();
    loop {
        let wave_context = SearchContext {
            visits: &visits,
            ..context
        };
        let (plan, batch) = prepare_operation_wave(
            wave_context,
            index,
            recall,
            matrix,
            deferred_priorities,
            budget.remaining_packet_bytes(),
            &mut packet_cache,
        )?;
        for (reference, _) in &plan.wave {
            deferred.remove(reference);
            if matches!(
                matrix.progress(reference),
                Ok(CandidateProgress::Uncertain { .. })
            ) {
                deferred.insert(reference.clone(), DeferredReason::OperationUncertain);
            }
            pending.insert(reference.clone());
        }
        deferred.extend(plan.deferred);
        preserve_uncertain_judgments(matrix, &mut deferred);
        let Some(batch) = batch else {
            collect_wave(
                index,
                matrix,
                &plan.wave,
                &mut judged,
                &mut routes,
                &mut deferred,
                &mut pending,
            )?;
            observe(progress(
                index,
                context.exposed,
                &budget,
                &judged,
                ProgressFrontier {
                    routes: &routes,
                    deferred: &deferred,
                },
                pending.clone(),
                ProgressState::Settled,
            ))
            .await
            .map_err(DiscoveryMatcherError::Checkpoint)?;
            break;
        };
        let reservation = if budget.requests() == 0 {
            budget.reserve(batch.members().iter().cloned(), &[batch.body()])
        } else {
            budget.reserve_followup(batch.members().iter().cloned(), batch.body())
        };
        if let Err(stop) = reservation {
            if completed_waves == 0 {
                return Err(DiscoveryMatcherError::IndivisibleItem {
                    bytes: batch.body().len(),
                });
            }
            for (reference, _) in &plan.wave {
                if !judged.contains_key(reference) {
                    deferred.insert(reference.clone(), stop.into());
                }
            }
            break;
        }
        observe(progress(
            index,
            context.exposed,
            &budget,
            &judged,
            ProgressFrontier {
                routes: &routes,
                deferred: &deferred,
            },
            pending.clone(),
            ProgressState::Admitted,
        ))
        .await
        .map_err(DiscoveryMatcherError::Checkpoint)?;
        // Planning and checkpointing are local work, not provider latency.
        // Give each admitted packet its own bounded provider retry window.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let raw = loop {
            let result = tokio::time::timeout_at(deadline, decide(batch.clone()))
                .await
                .unwrap_or_else(|_| {
                    Err(Box::new(
                        plasm_runtime::provider_transport::ProviderTransportError::DeadlineExpired,
                    ) as DecisionProviderError)
                });
            match result {
                Ok(raw) => break Some(raw),
                Err(error) => {
                    budget.record_usage(
                        error
                            .downcast_ref::<ReceivedResponseFailure>()
                            .and_then(|failure| failure.input_tokens),
                    );
                    let delay = error
                        .downcast_ref::<plasm_runtime::provider_transport::ProviderTransportError>()
                        .and_then(|error| error.retry_delay(budget.requests()))
                        .filter(|delay| {
                            budget.requests() < search_budget::MAX_ATTEMPTS
                                && *delay
                                    < deadline
                                        .saturating_duration_since(tokio::time::Instant::now())
                        });
                    observe(progress(
                        index,
                        context.exposed,
                        &budget,
                        &judged,
                        ProgressFrontier {
                            routes: &routes,
                            deferred: &deferred,
                        },
                        pending.clone(),
                        if delay.is_some() {
                            ProgressState::Admitted
                        } else {
                            failure_state(error.as_ref())
                        },
                    ))
                    .await
                    .map_err(DiscoveryMatcherError::Checkpoint)?;
                    let Some(delay) = delay else {
                        if completed_waves == 0 {
                            return Err(DiscoveryMatcherError::Provider(error));
                        }
                        tracing::warn!(error = %error, "follow-up discovery wave unavailable");
                        break None;
                    };
                    tokio::time::sleep(delay).await;
                    if budget.reserve_retry(batch.body()).is_err() {
                        if completed_waves == 0 {
                            return Err(DiscoveryMatcherError::Provider(error));
                        }
                        break None;
                    }
                    observe(progress(
                        index,
                        context.exposed,
                        &budget,
                        &judged,
                        ProgressFrontier {
                            routes: &routes,
                            deferred: &deferred,
                        },
                        pending.clone(),
                        ProgressState::Admitted,
                    ))
                    .await
                    .map_err(DiscoveryMatcherError::Checkpoint)?;
                }
            }
        };
        let Some(raw) = raw else {
            for (reference, _) in &plan.wave {
                deferred.insert(reference.clone(), DeferredReason::ProviderUnavailable);
            }
            break;
        };
        budget.record_usage(reported_input_tokens(&raw));
        let answers = match batch.decode(&raw) {
            Ok(answers) => answers,
            Err(error) if completed_waves > 0 => {
                tracing::warn!(%error, "follow-up discovery decision invalid");
                for (reference, _) in &plan.wave {
                    deferred.insert(reference.clone(), DeferredReason::ProviderUnavailable);
                }
                break;
            }
            Err(error) => return Err(error.into()),
        };
        let answered: BTreeSet<_> = answers
            .iter()
            .map(|answer| answer.capability.clone())
            .collect();
        matrix.record_packet(&batch.issued_pairs(), &answers)?;
        for reference in answered {
            *visits.entry(reference).or_default() += 1;
        }
        completed_waves += 1;
        collect_wave(
            index,
            matrix,
            &plan.wave,
            &mut judged,
            &mut routes,
            &mut deferred,
            &mut pending,
        )?;
        observe(progress(
            index,
            context.exposed,
            &budget,
            &judged,
            ProgressFrontier {
                routes: &routes,
                deferred: &deferred,
            },
            pending.clone(),
            ProgressState::Settled,
        ))
        .await
        .map_err(DiscoveryMatcherError::Checkpoint)?;
        // One affirmative contribution does not establish that the request's
        // information needs are complete. Keep assessing admitted pairs within
        // the same bounded budget, without granting unjudged candidates access.
        if budget.requests() >= search_budget::MAX_ATTEMPTS {
            break;
        }
    }
    preserve_uncertain_judgments(matrix, &mut deferred);
    deferred.retain(|reference, reason| {
        !judged.contains_key(reference)
            || matches!(
                reason,
                DeferredReason::OperationPending
                    | DeferredReason::OperationUncertain
                    | DeferredReason::ProviderUnavailable
            )
    });
    observe(progress(
        index,
        context.exposed,
        &budget,
        &judged,
        ProgressFrontier {
            routes: &routes,
            deferred: &deferred,
        },
        pending,
        ProgressState::Settled,
    ))
    .await
    .map_err(DiscoveryMatcherError::Checkpoint)?;
    let references: BTreeSet<_> = judged.keys().cloned().collect();
    let recalled: BTreeMap<_, _> = recall
        .candidates
        .iter()
        .map(|candidate| (&candidate.reference, candidate))
        .collect();
    let candidates: Vec<_> = references
        .iter()
        .map(|reference| {
            let mut candidate = index.candidates[reference].clone();
            candidate.admissions = recalled
                .get(reference)
                .map(|c| c.admissions.clone())
                .unwrap_or_else(|| BTreeSet::from(["typed_relationship".into()]));
            candidate
        })
        .collect();
    let retrieval = RetrievalReceipt {
        generation: context.generation.into(),
        lexical_count: candidates
            .iter()
            .filter(|c| c.admissions.contains("lexical"))
            .count(),
        vector_count: candidates
            .iter()
            .filter(|c| c.admissions.contains("vector"))
            .count(),
        candidates,
        lexical_truncated: recall.lexical_truncated,
        vector_truncated: recall.vector_truncated,
    };
    let matching = CapabilityMatchReceipt {
        matches: judged.into_values().collect(),
        budget_retained: Vec::new(),
    };
    // Saved judgments are considered by this selection even when their model
    // questions were answered on an earlier turn. Required acquisitions are
    // deterministic closure evidence and must be accounted for as well.
    let mut considered = budget.members().clone();
    considered.extend(references.iter().cloned());
    let retained: BTreeSet<_> = routes.keys().cloned().collect();
    let mut required_references = BTreeSet::new();
    for edge in required(
        &retained,
        context.catalogs,
        context.deployments,
        context.allowed,
    )? {
        required_references.extend([edge.source, edge.consumer]);
    }
    considered.extend(required_references.iter().cloned());
    let search = SearchReceipt {
        policy: SEARCH_POLICY.into(),
        exposed: context.exposed.iter().cloned().collect(),
        considered,
        unexplored: index
            .graph
            .authorized()
            .iter()
            .filter(|reference| {
                !context.exposed.contains(reference)
                    && !references.contains(*reference)
                    && !required_references.contains(*reference)
            })
            .cloned()
            .collect(),
        deferred: ordered_deferred(deferred, recall),
        requests: budget.requests(),
        input_bytes: budget.input_bytes(),
        reported_input_tokens: budget.reported_input_tokens(),
        missing_usage: budget.missing_usage(),
    };
    Ok((retrieval, matching, search))
}
