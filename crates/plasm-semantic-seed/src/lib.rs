//! Semantic seed selection: search + graph pool → witnesses → plans → pairwise.
//!
//! # Policy (FO hard, clarify soft)
//!
//! - **false_open** (ready with wrong seeds) is a hard failure.
//! - **clarify** is acceptable friction when a later turn can converge.
//! - Deterministic layer: catalog phrase retrieve + CGS graph expand into an
//!   over-complete candidate pool (no English synonym tables).
//! - **LLM pass 1:** `SelectRequirementWitnesses` maps intent → closed `w#`.
//! - **Rust:** construct minimal covering seed plans from selected witnesses.
//! - **LLM pass 2:** order-swapped pairwise `CompareSeedPlans` among competing
//!   complete plans; disagreement → clarify.
//! - No semantic post-selection rewriter on the ready path.

#[allow(
    clippy::derivable_impls,
    clippy::empty_line_after_doc_comments,
    clippy::map_clone,
    clippy::new_without_default,
    clippy::upper_case_acronyms,
    clippy::unwrap_or_default
)]
#[path = "../baml_client/mod.rs"]
mod baml_client;

use std::collections::HashMap;

use anyhow::Context;
use baml_client::sync_client::B;
use baml_client::types::{
    PlanComparison, RequirementWitnessRow, SeedPlanRow, WitnessSelectionAssessment,
};
use baml_client::ClientRegistry;
use plasm_core::discovery::{CgsCatalog, CgsDiscovery};
use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_candidate_graph::TypedCandidateGraph;
use plasm_core::discovery_coverage::{coverage_route_selection, CoveragePipelineResult};
use plasm_core::discovery_intent_class::DiscoveryIntentClass;
use plasm_core::discovery_seed_baml::empty_seed_selection_raw;
use plasm_core::discovery_seed_catalog::CatalogWorkflowContext;
use plasm_core::discovery_seed_select::SeedSelectionDecision;
use plasm_core::discovery_seed_select::SeedSelectionRaw;
use plasm_core::discovery_seed_witness::{
    build_witness_corpus, construct_minimal_plans, missing_named_catalog_coverage,
    prune_witness_selection, selection_clarify_from_plans, selection_from_plan,
    selection_hard_miss, shortlist_plans, synthesize_clarify_alternatives, verify_plan,
    DeterministicSeedPlan, PlanConstructError, RequirementWitness, WitnessCorpus, WitnessKind,
};

/// Runtime settings for LLM narrow steps.
pub struct SelectorConfig<'a> {
    pub client_name: &'a str,
    pub model: &'a str,
    pub api_key: &'a str,
    pub temperature: f64,
    pub seed: u64,
}

/// Catalog host for coverage pool tracing (allowed entry filter).
pub struct SelectorCatalogHost<'a, C> {
    pub catalog: &'a C,
    pub allowed_entry_ids: &'a [String],
}

/// Input bundle for one semantic seed selection invocation.
pub struct SelectorRequest<'a> {
    pub intent: &'a str,
    pub intent_class: &'a DiscoveryIntentClass,
    pub bundles: &'a [EntityCandidateBundle],
    pub catalog_context: &'a CatalogWorkflowContext,
    pub brand_lock_catalogs: &'a [String],
    pub candidate_graph: TypedCandidateGraph,
}

/// Result including coverage pipeline state for eval traces.
pub struct CoverageSeedSelection {
    pub pipeline: CoveragePipelineResult,
    pub selection: SeedSelectionRaw,
}

/// Search/graph pool → witnesses → plans → optional pairwise → ready|clarify|hard_miss.
pub fn select_discovery_seeds<C>(
    request: SelectorRequest<'_>,
    config: SelectorConfig<'_>,
    host: Option<SelectorCatalogHost<'_, C>>,
) -> anyhow::Result<SeedSelectionRaw>
where
    C: CgsDiscovery + CgsCatalog,
{
    Ok(select_discovery_seeds_detailed(request, config, host)?.selection)
}

/// Like [`select_discovery_seeds`] but returns the coverage pipeline for eval traces.
pub fn select_discovery_seeds_detailed<C>(
    request: SelectorRequest<'_>,
    config: SelectorConfig<'_>,
    host: Option<SelectorCatalogHost<'_, C>>,
) -> anyhow::Result<CoverageSeedSelection>
where
    C: CgsDiscovery + CgsCatalog,
{
    let host = host.ok_or_else(|| {
        anyhow::anyhow!("SelectorCatalogHost required for semantic seed selection")
    })?;

    // Deterministic coverage pipeline kept for eval shadow / plan metrics only.
    let (pipeline, _) =
        coverage_route_selection(host.catalog, request.intent, host.allowed_entry_ids)?;

    let Some(corpus) = build_witness_corpus(
        request.bundles,
        request.brand_lock_catalogs,
        &request.candidate_graph,
        Some(request.catalog_context),
    ) else {
        let mut selection = empty_seed_selection_raw();
        selection.reasoning = format!("{} | multipass=empty_pool", selection.reasoning);
        return Ok(CoverageSeedSelection {
            pipeline,
            selection,
        });
    };

    if corpus.witnesses.is_empty() {
        let mut selection = empty_seed_selection_raw();
        selection.reasoning = format!("{} | multipass=empty_witnesses", selection.reasoning);
        return Ok(CoverageSeedSelection {
            pipeline,
            selection,
        });
    }

    let assessment = select_requirement_witnesses_llm(request.intent, &corpus, &config)
        .context("SelectRequirementWitnesses")?;
    let named_catalogs = request.catalog_context.branded_entry_ids();
    let selection = match route_witness_assessment(
        request.intent,
        &corpus,
        &assessment,
        &config,
        &named_catalogs,
    ) {
        Ok(raw) => ensure_clarify_alternatives(&corpus, raw),
        Err(error) => {
            tracing::warn!(
                target: "plasm::discovery_auto_seed",
                %error,
                "multipass witness/plan route failed; fail-closed clarify"
            );
            selection_clarify_from_plans(&corpus, &[], format!("multipass=resolve_err {error}"))
        }
    };

    Ok(CoverageSeedSelection {
        pipeline,
        selection,
    })
}

fn route_witness_assessment(
    intent: &str,
    corpus: &WitnessCorpus,
    assessment: &WitnessSelectionAssessment,
    config: &SelectorConfig<'_>,
    named_catalogs: &[String],
) -> anyhow::Result<SeedSelectionRaw> {
    let decision = parse_witness_decision(&assessment.decision);
    match decision {
        WitnessDecision::Clarify => Ok(selection_clarify_from_plans(
            corpus,
            &[],
            format!(
                "{} | multipass=SelectRequirementWitnesses:clarify",
                assessment.reasoning
            ),
        )),
        WitnessDecision::HardMiss => {
            let uncovered = if assessment.uncovered_requirements.is_empty() {
                vec!["selector hard_miss".into()]
            } else {
                assessment.uncovered_requirements.clone()
            };
            Ok(selection_hard_miss(
                uncovered,
                format!(
                    "{} | multipass=SelectRequirementWitnesses:hard_miss",
                    assessment.reasoning
                ),
            ))
        }
        WitnessDecision::Continue => {
            let indices = corpus
                .resolve_symbols(&assessment.selected_witness_symbols)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if indices.is_empty() {
                return Ok(selection_clarify_from_plans(
                    corpus,
                    &[],
                    format!(
                        "{} | multipass=empty_witness_selection",
                        assessment.reasoning
                    ),
                ));
            }
            let indices = prune_witness_selection(corpus, &indices);
            if indices.is_empty() {
                return Ok(selection_clarify_from_plans(
                    corpus,
                    &[],
                    format!(
                        "{} | multipass=empty_after_role_prune",
                        assessment.reasoning
                    ),
                ));
            }
            let missing = missing_named_catalog_coverage(named_catalogs, corpus, &indices);
            if !missing.is_empty() {
                return Ok(selection_clarify_from_plans(
                    corpus,
                    &[],
                    format!(
                        "{} | multipass=missing_named_catalogs={}",
                        assessment.reasoning,
                        missing.join(",")
                    ),
                ));
            }

            let plans = match construct_minimal_plans(corpus, &indices) {
                Ok(plans) => plans,
                Err(PlanConstructError::EmptyWitnesses) => {
                    return Ok(selection_clarify_from_plans(
                        corpus,
                        &[],
                        format!("{} | multipass=empty_witnesses", assessment.reasoning),
                    ));
                }
                Err(PlanConstructError::Uncoverable { missing }) => {
                    return Ok(selection_hard_miss(
                        missing,
                        format!("{} | multipass=uncoverable_witnesses", assessment.reasoning),
                    ));
                }
            };

            let verified: Vec<DeterministicSeedPlan> = plans
                .into_iter()
                .filter(|plan| verify_plan(corpus, plan, &indices))
                .collect();
            if verified.is_empty() {
                return Ok(selection_hard_miss(
                    vec!["no verified covering plan".into()],
                    format!("{} | multipass=no_verified_plan", assessment.reasoning),
                ));
            }
            if verified.len() == 1 {
                let mut raw = selection_from_plan(corpus, &verified[0]);
                raw.reasoning = format!(
                    "{} | {} | multipass=single_plan",
                    assessment.reasoning, raw.reasoning
                );
                return Ok(raw);
            }

            let shortlist = shortlist_plans(&verified);
            match pairwise_agree_winner(intent, &shortlist, config)? {
                Some(winner) => {
                    let mut raw = selection_from_plan(corpus, winner);
                    raw.reasoning = format!(
                        "{} | {} | multipass=pairwise_agree",
                        assessment.reasoning, raw.reasoning
                    );
                    Ok(raw)
                }
                None => {
                    let mut raw = selection_clarify_from_plans(
                        corpus,
                        &shortlist,
                        format!(
                            "{} | multipass=pairwise_disagree plan_count={}",
                            assessment.reasoning,
                            shortlist.len()
                        ),
                    );
                    raw.requirements = assessment.selected_witness_symbols.clone();
                    Ok(raw)
                }
            }
        }
    }
}

fn ensure_clarify_alternatives(
    corpus: &WitnessCorpus,
    mut raw: SeedSelectionRaw,
) -> SeedSelectionRaw {
    if raw.decision == SeedSelectionDecision::Clarify && raw.alternative_sets.len() < 2 {
        raw.alternative_sets = synthesize_clarify_alternatives(corpus);
    }
    raw
}

enum WitnessDecision {
    Continue,
    Clarify,
    HardMiss,
}

fn parse_witness_decision(raw: &str) -> WitnessDecision {
    match raw.trim().to_ascii_lowercase().as_str() {
        "continue" | "ready" => WitnessDecision::Continue,
        "hard_miss" | "hardmiss" => WitnessDecision::HardMiss,
        _ => WitnessDecision::Clarify,
    }
}

/// Order-swapped pairwise: a plan must win every match in both presentation orders.
fn pairwise_agree_winner<'a>(
    intent: &str,
    plans: &'a [DeterministicSeedPlan],
    config: &SelectorConfig<'_>,
) -> anyhow::Result<Option<&'a DeterministicSeedPlan>> {
    if plans.is_empty() {
        return Ok(None);
    }
    if plans.len() == 1 {
        return Ok(Some(&plans[0]));
    }

    let mut wins: HashMap<&str, usize> = HashMap::new();
    for plan in plans {
        wins.insert(plan.symbol.as_str(), 0);
    }

    for i in 0..plans.len() {
        for j in (i + 1)..plans.len() {
            let a = &plans[i];
            let b = &plans[j];
            let forward = compare_plans_llm(intent, a, b, config)?;
            let reverse = compare_plans_llm(intent, b, a, config)?;
            let agreed = match (
                parse_compare_choice(&forward.choice),
                parse_compare_choice(&reverse.choice),
            ) {
                (CompareChoice::A, CompareChoice::B) => Some(a.symbol.as_str()),
                (CompareChoice::B, CompareChoice::A) => Some(b.symbol.as_str()),
                _ => None,
            };
            let Some(winner_sym) = agreed else {
                return Ok(None);
            };
            *wins.entry(winner_sym).or_default() += 1;
        }
    }

    let needed = plans.len() - 1;
    let mut champion = None;
    for plan in plans {
        if wins.get(plan.symbol.as_str()).copied().unwrap_or(0) == needed {
            if champion.is_some() {
                return Ok(None);
            }
            champion = Some(plan);
        }
    }
    Ok(champion)
}

enum CompareChoice {
    A,
    B,
    Indeterminate,
}

fn parse_compare_choice(raw: &str) -> CompareChoice {
    match raw.trim().to_ascii_uppercase().as_str() {
        "A" => CompareChoice::A,
        "B" => CompareChoice::B,
        _ => CompareChoice::Indeterminate,
    }
}

fn select_requirement_witnesses_llm(
    intent: &str,
    corpus: &WitnessCorpus,
    config: &SelectorConfig<'_>,
) -> anyhow::Result<WitnessSelectionAssessment> {
    baml_client::init();
    let registry = openrouter_registry(config);
    let rows: Vec<RequirementWitnessRow> = corpus.witnesses.iter().map(witness_to_row).collect();

    B.SelectRequirementWitnesses
        .with_client_registry(&registry)
        .call(intent, &corpus.brand_lock_catalogs, &rows)
        .context("BAML SelectRequirementWitnesses")
}

fn compare_plans_llm(
    intent: &str,
    plan_a: &DeterministicSeedPlan,
    plan_b: &DeterministicSeedPlan,
    config: &SelectorConfig<'_>,
) -> anyhow::Result<PlanComparison> {
    baml_client::init();
    let registry = openrouter_registry(config);
    let a = plan_to_row(plan_a);
    let b = plan_to_row(plan_b);
    B.CompareSeedPlans
        .with_client_registry(&registry)
        .call(intent, &a, &b)
        .context("BAML CompareSeedPlans")
}

fn openrouter_registry(config: &SelectorConfig<'_>) -> ClientRegistry {
    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        config.client_name,
        "openai-generic",
        plasm_eval_common::openrouter_eval_llm_options(
            config.model,
            config.api_key,
            config.temperature,
            config.seed,
        ),
    );
    registry.set_primary_client(config.client_name);
    registry
}

fn witness_to_row(w: &RequirementWitness) -> RequirementWitnessRow {
    let (kind, catalog, entity, detail) = match &w.kind {
        WitnessKind::DirectCapability {
            entry_id,
            entity,
            capability_name,
            kind,
            description,
            ..
        } => (
            "direct_capability".to_string(),
            entry_id.clone(),
            entity.clone(),
            format!("{kind} `{capability_name}` — {description}"),
        ),
        WitnessKind::RelationHop {
            entry_id,
            from_entity,
            wire,
            target_entity,
        } => (
            "relation_hop".to_string(),
            entry_id.clone(),
            from_entity.clone(),
            format!("relation `{wire}` → {target_entity}"),
        ),
    };
    RequirementWitnessRow {
        symbol: w.symbol.clone(),
        kind,
        catalog,
        entity,
        entity_description: w.entity_description.clone(),
        detail,
        aliases: w.aliases.clone(),
        graph_note: {
            let note = w.pool.render_graph_note();
            if note.is_empty() {
                "(none)".into()
            } else {
                note
            }
        },
        seed_class: w.seed_class.as_str().to_string(),
        seed_nav: w.seed_nav.as_str().to_string(),
        own_pair: w.own_pairs.render(),
        own_end: {
            let entity = match &w.kind {
                WitnessKind::DirectCapability { entity, .. } => entity.as_str(),
                WitnessKind::RelationHop { from_entity, .. } => from_entity.as_str(),
            };
            w.own_pairs.end_role(entity).as_str().to_string()
        },
    }
}

fn plan_to_row(plan: &DeterministicSeedPlan) -> SeedPlanRow {
    SeedPlanRow {
        symbol: plan.symbol.clone(),
        seeds: plan.summary.clone(),
        covered_witnesses: plan.covered_witness_symbols.join(","),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_pool_raw_is_hard_miss() {
        let raw = plasm_core::discovery_seed_baml::empty_seed_selection_raw();
        assert_eq!(
            raw.decision,
            plasm_core::discovery_seed_select::SeedSelectionDecision::HardMiss
        );
    }
}
