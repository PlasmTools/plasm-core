//! Build one bounded candidate set before judgment. Paths nominate, never grant permission.
use super::*;

type Route = Vec<Edge<CapabilityRef>>;
type Bundle = BTreeMap<CapabilityRef, Route>;
// Combinations share immutable paths until a complete trial needs an owned bundle.
type VariantBundle = BTreeMap<CapabilityRef, std::sync::Arc<Route>>;
type InputGroups = BTreeMap<NominationSlot, Vec<Edge<CapabilityRef>>>;
type GroupCache = BTreeMap<CapabilityRef, std::sync::Arc<InputGroups>>;

/// Lexicographic scheduling order: fairness precedes cost and recall rank.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct BundlePriority {
    visits: usize,
    served_in_slot: usize,
    path_length: usize,
    input_bytes: usize,
    recall_rank: usize,
    reference: CapabilityRef,
}
const MAX_VARIANTS: usize = 64;
/// Once overdue, current-request recall breaks ties between deferred roots.
const DEFERRED_PRIORITY_LIMIT: usize = 3;

#[cfg(test)]
mod allocation_progress_tests;
#[cfg(test)]
mod caller_loop;
#[cfg(test)]
mod capacity_tests;
#[cfg(test)]
mod recorded_cost_tests;

struct LocalPacket {
    #[cfg(test)]
    scope: witnesses::Scope,
    #[cfg(test)]
    question: Question,
    #[cfg(test)]
    members: BTreeSet<CapabilityRef>,
    prepared: request_packing::PreparedLocal,
}

/// Required acquisitions are ancestry, not optional allocation slots. A path
/// may start with several of them before reaching its first nominated supplier.
fn allocation_slot(
    index: &CatalogueSearch,
    route: &Route,
) -> Result<Option<NominationSlot>, DiscoveryMatcherError> {
    route
        .iter()
        .find(|edge| edge.relationship != Relationship::RequiredAcquisition)
        .map(|edge| nomination_slot(index, edge))
        .transpose()
}

pub(super) struct Plan {
    pub wave: Vec<(CapabilityRef, Route)>,
    pub deferred: BTreeMap<CapabilityRef, DeferredReason>,
    #[cfg(test)]
    pub issued: Option<IssuedBatch>,
    #[cfg(test)]
    pub members: BTreeSet<CapabilityRef>,
    #[cfg(test)]
    pub diagnostic: Vec<serde_json::Value>,
}

fn input_groups(
    key: &CapabilityRef,
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    cache: &mut GroupCache,
) -> Result<std::sync::Arc<InputGroups>, DiscoveryMatcherError> {
    if let Some(groups) = cache.get(key) {
        return Ok(std::sync::Arc::clone(groups));
    }
    let dependencies = required_closure(
        &BTreeSet::from([key.clone()]),
        context.catalogs,
        context.deployments,
        context.allowed,
    )?;
    let mut supplied = BTreeSet::new();
    for edge in dependencies
        .edges
        .iter()
        .filter(|edge| &edge.consumer == key)
    {
        supplied.extend(edge.requirement.bindings.iter().map(|binding| {
            plasm_core::prerequisites::InputSourceTarget::Argument {
                input: binding.input.clone(),
            }
        }));
    }
    let mut groups = BTreeMap::new();
    for edge in index
        .graph
        .incoming(key)
        .filter(|edge| edge.relationship == Relationship::SuppliesInput)
    {
        let slot = nomination_slot(index, edge)?;
        if matches!(&slot, NominationSlot::Input(_, input) if supplied.contains(input)) {
            continue;
        }
        groups
            .entry(slot)
            .or_insert_with(Vec::new)
            .push(edge.clone());
    }
    let groups = std::sync::Arc::new(groups);
    cache.insert(key.clone(), std::sync::Arc::clone(&groups));
    Ok(groups)
}

/// Count the same bounded combinations as `variants` without building bundles.
/// This preserves cycle detection and error traversal for an oversized root.
fn variant_count(
    key: &CapabilityRef,
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    proposed: &Bundle,
    ancestors: &BTreeSet<CapabilityRef>,
    cache: &mut GroupCache,
) -> Result<usize, DiscoveryMatcherError> {
    if proposed.contains_key(key) {
        return Ok(1);
    }
    if ancestors.contains(key) {
        return Ok(0);
    }
    let groups = input_groups(key, index, context, cache)?;
    let mut next_ancestors = ancestors.clone();
    next_ancestors.insert(key.clone());
    let mut states = 1usize;
    for options in groups.values() {
        let mut next = 0usize;
        for edge in options {
            let children = variant_count(
                &edge.source,
                index,
                context,
                proposed,
                &next_ancestors,
                cache,
            )?;
            next = next
                .saturating_add(children.saturating_mul(states))
                .min(MAX_VARIANTS);
            if next == MAX_VARIANTS {
                break;
            }
        }
        states = next;
        if states == 0 {
            break;
        }
    }
    Ok(states)
}

fn variants(
    key: &CapabilityRef,
    route: &Route,
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    proposed: &Bundle,
    ancestors: &BTreeSet<CapabilityRef>,
    cache: &mut GroupCache,
) -> Result<Vec<VariantBundle>, DiscoveryMatcherError> {
    if proposed.contains_key(key) {
        return Ok(vec![VariantBundle::new()]);
    }
    if ancestors.contains(key) {
        return Ok(Vec::new());
    }
    let groups = input_groups(key, index, context, cache)?;
    let mut next_ancestors = ancestors.clone();
    next_ancestors.insert(key.clone());
    // Every returned bundle retains this root and route; recursive suppliers
    // that are already proposed return an empty bundle and cannot replace them.
    let mut states = vec![VariantBundle::from([(
        key.clone(),
        std::sync::Arc::new(route.clone()),
    )])];
    for options in groups.values() {
        let mut next = Vec::new();
        for edge in options {
            let mut path = route.clone();
            path.push(edge.clone());
            for children in variants(
                &edge.source,
                &path,
                index,
                context,
                proposed,
                &next_ancestors,
                cache,
            )? {
                for state in &states {
                    let mut combined = state.clone();
                    for (child, path) in &children {
                        combined
                            .entry(child.clone())
                            .or_insert_with(|| path.clone());
                    }
                    next.push(combined);
                    if next.len() == MAX_VARIANTS {
                        break;
                    }
                }
                if next.len() == MAX_VARIANTS {
                    break;
                }
            }
            if next.len() == MAX_VARIANTS {
                break;
            }
        }
        states = next;
        if states.is_empty() {
            break;
        }
    }
    Ok(states)
}

/// Cache only immutable local evidence within this search. Admission always
/// measures the complete packed request, including repeated context and refs.
fn body_size(
    selected: &Bundle,
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    cache: &mut BTreeMap<(CapabilityRef, Route), LocalPacket>,
) -> Result<usize, DiscoveryMatcherError> {
    body_size_up_to(selected, index, context, cache, None)
}

fn body_size_up_to(
    selected: &Bundle,
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    cache: &mut BTreeMap<(CapabilityRef, Route), LocalPacket>,
    limit: Option<usize>,
) -> Result<usize, DiscoveryMatcherError> {
    let mut local = Vec::new();
    for (key, path) in selected {
        if context.exposed.contains(key) {
            continue;
        }
        let identity = (key.clone(), path.clone());
        if let std::collections::btree_map::Entry::Vacant(entry) = cache.entry(identity) {
            let (scope, _question, _members) = packet_scope(index, key, path, context)?;
            let prepared = request_packing::PreparedLocal::for_scope(
                context.model,
                context.intent,
                &index.labels[key],
                &scope,
            )?;
            entry.insert(LocalPacket {
                #[cfg(test)]
                scope,
                #[cfg(test)]
                question: _question,
                #[cfg(test)]
                members: _members,
                prepared,
            });
        }
        local.push((key.clone(), path.clone()));
        if let Some(limit) = limit {
            // Items are visited in packet order. Appending a question can only
            // add evidence, definitions and references; existing IDs stay fixed.
            // A prefix already over budget therefore cannot become affordable.
            let prepared: Vec<_> = local.iter().map(|key| &cache[key].prepared).collect();
            let size = request_packing::connected_size(&prepared)?;
            if size > limit {
                return Ok(size);
            }
        }
    }
    let prepared: Vec<_> = local.iter().map(|key| &cache[key].prepared).collect();
    request_packing::connected_size(&prepared)
}

fn body_size_lower_bound(
    selected: &Bundle,
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    cache: &mut BTreeMap<(CapabilityRef, Route), LocalPacket>,
) -> Result<usize, DiscoveryMatcherError> {
    // Prepare every item in the subset, then allow for the maximum possible
    // reduction in shared-reference ID widths after other items are inserted.
    let exact = body_size(selected, index, context, cache)?;
    let prepared: Vec<_> = selected
        .iter()
        .filter(|(key, _)| !context.exposed.contains(key))
        .map(|(key, path)| &cache[&(key.clone(), path.clone())].prepared)
        .collect();
    Ok(request_packing::connected_lower_bound(&prepared, exact))
}

#[cfg(test)]
pub(super) fn root_order(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
) -> Vec<CapabilityRef> {
    order_recalled(
        index,
        context,
        index.ranking.rank(context.intent.current()),
        &BTreeMap::new(),
    )
}

pub(super) fn order_recalled(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    mut order: Vec<CapabilityRef>,
    deferred: &BTreeMap<CapabilityRef, usize>,
) -> Vec<CapabilityRef> {
    // Exposure is not execution or relevance, but a declared producer can
    // nominate its direct consumer for judgment. Supplier-only traversal loses
    // the other end of a path after its source has already been retained.
    let consumers: BTreeSet<_> = index
        .graph
        .edges()
        .filter(|edge| {
            edge.relationship != Relationship::RequiredAcquisition
                && context.exposed.contains(&edge.source)
                && !context.exposed.contains(&edge.consumer)
        })
        .map(|edge| edge.consumer.clone())
        .collect();
    let ranked: BTreeSet<_> = order.iter().cloned().collect();
    order.extend(consumers.difference(&ranked).cloned());
    // Reserve priority for current-request recall before promoting the connected
    // frontier. Otherwise a retained hub can consume both initial root slots
    // indefinitely. Keep connected progress ahead of the remaining roots, and
    // preserve visit rounds so rejection still advances exploration. Choose the
    // recall representative from the least-visited round too: a rejected first
    // root must not monopolize this opportunity while consumers pass the next.
    let requested = order
        .iter()
        .filter(|key| !context.exposed.contains(key))
        .min_by_key(|key| context.visits.get(*key).copied().unwrap_or(0))
        .cloned();
    order.sort_by_key(|key| {
        (
            context.visits.get(key).copied().unwrap_or(0),
            if Some(key) == requested.as_ref() {
                0
            } else if ranked.contains(key) && deferred.contains_key(key) {
                1
            } else if consumers.contains(key) {
                2
            } else {
                3
            },
            std::cmp::Reverse(
                deferred
                    .get(key)
                    .copied()
                    .unwrap_or(0)
                    .min(DEFERRED_PRIORITY_LIMIT),
            ),
        )
    });
    order
}

#[cfg(test)]
pub(super) fn plan(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
) -> Result<Plan, DiscoveryMatcherError> {
    plan_recalled(index, context, index.ranking.rank(context.intent.current()))
}

#[cfg(test)]
pub(super) fn plan_recalled(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    roots: Vec<CapabilityRef>,
) -> Result<Plan, DiscoveryMatcherError> {
    let order = order_recalled(index, context, roots, &BTreeMap::new());
    plan_ordered(index, context, order)
}

pub(super) fn plan_recalled_with_deferred(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    roots: Vec<CapabilityRef>,
    deferred: &BTreeMap<CapabilityRef, usize>,
) -> Result<Plan, DiscoveryMatcherError> {
    let recalled: BTreeSet<_> = roots.iter().cloned().collect();
    let order = order_recalled(index, context, roots, deferred);
    let plan = plan_ordered(index, context, order.clone())?;
    // A leading request candidate can occupy nearly the whole packet. A
    // currently recalled candidate that repeatedly could not share its packet
    // must eventually get a turn, rather than accumulating deferrals forever.
    let blocked = order
        .iter()
        .find(|key| {
            recalled.contains(*key)
                && deferred.get(*key).copied().unwrap_or(0) >= DEFERRED_PRIORITY_LIMIT
                && matches!(plan.deferred.get(*key), Some(DeferredReason::InputBytes))
                && !plan.wave.iter().any(|(member, _)| member == *key)
        })
        .cloned();
    let Some(blocked) = blocked else {
        return Ok(plan);
    };
    let mut retry = vec![blocked.clone()];
    retry.extend(order.into_iter().filter(|key| key != &blocked));
    let alternate = plan_ordered(index, context, retry)?;
    if alternate.wave.iter().any(|(member, _)| member == &blocked) {
        Ok(alternate)
    } else {
        Ok(plan)
    }
}

// Separate ordering from exact packet construction for controlled scheduling
// experiments. Production callers retain order_recalled unchanged.
pub(super) fn plan_ordered(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    order: Vec<CapabilityRef>,
) -> Result<Plan, DiscoveryMatcherError> {
    let ranks: BTreeMap<_, _> = order
        .iter()
        .enumerate()
        .map(|(n, key)| (key.clone(), n))
        .collect();
    let mut proposed = Bundle::new();
    let mut deferred = BTreeMap::new();
    // A failed optional route does not establish that the direct root is too
    // large. Only direct admission failures can suppress another root trial.
    let mut root_deferred = BTreeSet::new();
    let mut cache = BTreeMap::new();
    let mut group_cache = GroupCache::new();
    let mut served = BTreeMap::<NominationSlot, usize>::new();
    #[cfg(test)]
    let mut diagnostic = Vec::new();
    // Ranked existing declarations can anchor missing suppliers, but consume
    // neither a new-root slot nor a contribution question.
    for key in order
        .iter()
        .take(2)
        .filter(|key| context.exposed.contains(key))
    {
        proposed.insert(key.clone(), Vec::new());
    }
    for key in order
        .iter()
        .filter(|key| !context.exposed.contains(key))
        .take(2)
    {
        let mut trial = proposed.clone();
        trial.insert(key.clone(), Vec::new());
        if body_size(&trial, index, context, &mut cache)? <= search_budget::MAX_INPUT_BYTES {
            #[cfg(test)]
            diagnostic.push(serde_json::json!({"kind":"initial_root","root":key,
                "before_bytes":body_size(&proposed,index,context,&mut cache)?,
                "after_bytes":body_size(&trial,index,context,&mut cache)?}));
            proposed = trial;
        } else {
            deferred.insert(key.clone(), DeferredReason::InputBytes);
            root_deferred.insert(key.clone());
        }
    }
    loop {
        let mut routes = proposed.clone();
        let dependencies = required(
            &proposed.keys().cloned().collect(),
            context.catalogs,
            context.deployments,
            context.allowed,
        )?;
        let mut pending = dependencies;
        while !pending.is_empty() {
            let edge = pending
                .iter()
                .find(|e| routes.contains_key(&e.consumer))
                .cloned()
                .ok_or_else(|| DiscoveryMatcherError::InvalidWitness {
                    reference: pending.first().unwrap().consumer.clone(),
                    part: "required route ancestry",
                })?;
            pending.remove(&edge);
            let mut path = routes[&edge.consumer].clone();
            path.push(edge.clone());
            routes.entry(edge.source).or_insert(path);
        }
        let mut candidates = Bundle::new();
        for edge in index.graph.nominate(&routes.keys().cloned().collect()) {
            let mut path = routes[&edge.consumer].clone();
            if path
                .iter()
                .any(|e| e.source == edge.source || e.consumer == edge.source)
            {
                continue;
            }
            path.push(edge.clone());
            candidates
                .entry(edge.source)
                .and_modify(|previous| {
                    if path < *previous {
                        *previous = path.clone();
                    }
                })
                .or_insert(path);
        }
        let mut best: Option<(BundlePriority, Bundle)> = None;
        for (key, path) in &candidates {
            let mut fits = false;
            let count = variant_count(
                key,
                index,
                context,
                &proposed,
                &BTreeSet::new(),
                &mut group_cache,
            )?;
            if count == 0 {
                deferred.insert(key.clone(), DeferredReason::AcquisitionCycle);
                continue;
            }
            // Every variant contains this candidate and all proposed items.
            // If even their conservative size is too large, avoid building the
            // combinations while preserving the original cycle classification.
            let mut minimum = proposed.clone();
            minimum.insert(key.clone(), path.clone());
            if body_size_lower_bound(&minimum, index, context, &mut cache)?
                > search_budget::MAX_INPUT_BYTES
            {
                deferred.insert(key.clone(), DeferredReason::InputBytes);
                continue;
            }
            let alternatives = variants(
                key,
                path,
                index,
                context,
                &proposed,
                &BTreeSet::new(),
                &mut group_cache,
            )?;
            debug_assert_eq!(count, alternatives.len());
            for bundle in alternatives {
                let bundle: Bundle = bundle
                    .into_iter()
                    .map(|(key, path)| (key, (*path).clone()))
                    .collect();
                let mut trial = proposed.clone();
                trial.extend(bundle.clone());
                let size = body_size_up_to(
                    &trial,
                    index,
                    context,
                    &mut cache,
                    Some(search_budget::MAX_INPUT_BYTES),
                )?;
                if size > search_budget::MAX_INPUT_BYTES {
                    continue;
                }
                fits = true;
                let slot = allocation_slot(index, path)?.ok_or_else(|| {
                    DiscoveryMatcherError::InvalidWitness {
                        reference: key.clone(),
                        part: "optional path has no nomination slot",
                    }
                })?;
                let priority = BundlePriority {
                    visits: context.visits.get(key).copied().unwrap_or(0),
                    served_in_slot: *served.get(&slot).unwrap_or(&0),
                    path_length: path.len(),
                    input_bytes: size,
                    recall_rank: *ranks.get(key).unwrap_or(&usize::MAX),
                    reference: key.clone(),
                };
                if best.as_ref().is_none_or(|(old, _)| priority < *old) {
                    best = Some((priority, bundle));
                }
            }
            if !fits {
                deferred.insert(key.clone(), DeferredReason::InputBytes);
            }
        }
        // Visit rounds apply across both nomination lanes. Otherwise a
        // repeatedly rejected optional path can fill every packet before an
        // unjudged recalled root receives its first question. Within a round,
        // retain connected-path priority; neither lane grants relevance.
        let mut fitting_root = None;
        for key in &order {
            if routes.contains_key(key)
                || context.exposed.contains(key)
                || root_deferred.contains(key)
                || best.as_ref().is_some_and(|(priority, _)| {
                    context.visits.get(key).copied().unwrap_or(0) >= priority.visits
                })
            {
                continue;
            }
            let mut trial = proposed.clone();
            trial.insert(key.clone(), Vec::new());
            if body_size(&trial, index, context, &mut cache)? <= search_budget::MAX_INPUT_BYTES {
                fitting_root = Some(Bundle::from([(key.clone(), Vec::new())]));
                break;
            }
            deferred.insert(key.clone(), DeferredReason::InputBytes);
            root_deferred.insert(key.clone());
        }
        #[cfg(test)]
        let admission_kind = if fitting_root.is_some() {
            "remaining_root"
        } else {
            "optional_path"
        };
        let bundle = fitting_root.or_else(|| best.map(|(_, bundle)| bundle));
        let Some(bundle) = bundle else {
            break;
        };
        #[cfg(test)]
        {
            let mut trial = proposed.clone();
            trial.extend(bundle.clone());
            diagnostic.push(serde_json::json!({"kind":admission_kind,
                "members":bundle.iter().map(|(key,path)|serde_json::json!({"reference":key,"path":path})).collect::<Vec<_>>(),
                "before_bytes":body_size(&proposed,index,context,&mut cache)?,
                "after_bytes":body_size(&trial,index,context,&mut cache)?}));
        }
        for (key, path) in bundle {
            if let Some(slot) = allocation_slot(index, &path)? {
                *served.entry(slot).or_default() += 1;
            }
            deferred.remove(&key);
            proposed.insert(key, path);
        }
    }
    let mut members: BTreeSet<_> = proposed.keys().cloned().collect();
    for route in proposed.values() {
        for edge in route {
            members.extend([edge.source.clone(), edge.consumer.clone()]);
        }
    }
    for edge in required(
        &members,
        context.catalogs,
        context.deployments,
        context.allowed,
    )? {
        members.extend([edge.source, edge.consumer]);
    }
    #[cfg(test)]
    let local = proposed
        .iter()
        .filter(|(key, _)| !context.exposed.contains(key))
        .map(|(key, path)| {
            let local = cache
                .remove(&(key.clone(), path.clone()))
                .expect("selected packet was prepared");
            members.extend(local.members.iter().cloned());
            issue_scope(
                context.model,
                context.intent,
                &local.question,
                &std::sync::Arc::new(vec![local.question.clone()]),
                &index.labels,
                local.scope,
                false,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    #[cfg(test)]
    let issued = request_packing::connected_prepared(local)?;
    // An existing declaration may anchor an acquisition path that cannot fit.
    // Deferring that expansion does not defer the declaration already exposed.
    deferred.retain(|key, _| !context.exposed.contains(key));
    let mut wave: Vec<_> = proposed
        .into_iter()
        .filter(|(key, _)| !context.exposed.contains(key))
        .collect();
    // A bundle is assembled in a map for graph closure, but the operation
    // scheduler must see recalled roots in the planner's priority order.
    // Otherwise catalogue identifiers silently decide ties between questions.
    wave.sort_by_key(|(key, _)| ranks.get(key).copied().unwrap_or(usize::MAX));
    Ok(Plan {
        #[cfg(test)]
        diagnostic,
        wave,
        deferred,
        #[cfg(test)]
        issued,
        #[cfg(test)]
        members,
    })
}
