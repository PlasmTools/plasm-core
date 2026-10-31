//! Build one bounded candidate set before judgment. Paths nominate, never grant permission.
use super::*;

type Route = Vec<Edge<CapabilityRef>>;
type Bundle = BTreeMap<CapabilityRef, Route>;
const MAX_VARIANTS: usize = 64;

#[cfg(test)]
mod recorded_cost_tests;
#[cfg(test)]
mod capacity_tests;
#[cfg(test)]
mod caller_loop;

struct LocalPacket {
    scope: witnesses::Scope,
    question: Question,
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
    pub issued: Option<IssuedBatch>,
    pub members: BTreeSet<CapabilityRef>,
    #[cfg(test)]
    pub diagnostic: Vec<serde_json::Value>,
}

fn variants(
    key: &CapabilityRef,
    route: &Route,
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    proposed: &Bundle,
    ancestors: &BTreeSet<CapabilityRef>,
) -> Result<Vec<Bundle>, DiscoveryMatcherError> {
    if proposed.contains_key(key) {
        return Ok(vec![Bundle::new()]);
    }
    if ancestors.contains(key) {
        return Ok(Vec::new());
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
        .edges()
        .filter(|edge| &edge.consumer == key && edge.relationship == Relationship::SuppliesInput)
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
    let mut next_ancestors = ancestors.clone();
    next_ancestors.insert(key.clone());
    let mut states = vec![Bundle::from([(key.clone(), route.clone())])];
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
    for (key, path) in selected {
        if context.exposed.contains(key) {
            continue;
        }
        let identity = (key.clone(), path.clone());
        if !cache.contains_key(&identity) {
            let (scope, question, members) = packet_scope(index, key, path, context)?;
            let prepared = request_packing::PreparedLocal::for_scope(
                context.model,
                context.intent,
                &index.labels[key],
                &scope,
            )?;
            cache.insert(
                identity,
                LocalPacket {
                    scope,
                    question,
                    members,
                    prepared,
                },
            );
        }
    }
    let local = selected
        .iter()
        .filter(|(key, _)| !context.exposed.contains(key))
        .map(|(key, path)| &cache[&(key.clone(), path.clone())].prepared)
        .collect::<Vec<_>>();
    request_packing::connected_size(&local)
}

#[cfg(test)]
pub(super) fn root_order(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
) -> Vec<CapabilityRef> {
    order_recalled(index, context, index.ranking.rank(context.intent.current()))
}

fn order_recalled(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    mut order: Vec<CapabilityRef>,
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
            } else if consumers.contains(key) {
                1
            } else {
                2
            },
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

pub(super) fn plan_recalled(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
    roots: Vec<CapabilityRef>,
) -> Result<Plan, DiscoveryMatcherError> {
    let order = order_recalled(index, context, roots);
    plan_ordered(index, context, order)
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
        let mut best: Option<((usize, usize, usize, usize, usize, CapabilityRef), Bundle)> = None;
        for (key, path) in &candidates {
            let mut fits = false;
            let alternatives = variants(key, path, index, context, &proposed, &BTreeSet::new())?;
            if alternatives.is_empty() {
                deferred.insert(key.clone(), DeferredReason::AcquisitionCycle);
                continue;
            }
            for bundle in alternatives {
                let mut trial = proposed.clone();
                trial.extend(bundle.clone());
                let size = body_size(&trial, index, context, &mut cache)?;
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
                let priority = (
                    context.visits.get(key).copied().unwrap_or(0),
                    *served.get(&slot).unwrap_or(&0),
                    path.len(),
                    size,
                    *ranks.get(key).unwrap_or(&usize::MAX),
                    key.clone(),
                );
                if best.as_ref().is_none_or(|(old, _)| priority < *old) {
                    best = Some((priority, bundle));
                }
            }
            if !fits {
                deferred.insert(key.clone(), DeferredReason::InputBytes);
            }
        }
        #[cfg(test)]
        let admission_kind = if best.is_some() {
            "optional_path"
        } else {
            "remaining_root"
        };
        let bundle = if let Some((_, bundle)) = best {
            Some(bundle)
        } else {
            let mut fitting = None;
            for key in &order {
                if routes.contains_key(key)
                    || context.exposed.contains(key)
                    || root_deferred.contains(key)
                {
                    continue;
                }
                let mut trial = proposed.clone();
                trial.insert(key.clone(), Vec::new());
                if body_size(&trial, index, context, &mut cache)? <= search_budget::MAX_INPUT_BYTES
                {
                    fitting = Some(Bundle::from([(key.clone(), Vec::new())]));
                    break;
                }
                deferred.insert(key.clone(), DeferredReason::InputBytes);
                root_deferred.insert(key.clone());
            }
            fitting
        };
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
    let issued = request_packing::connected(local)?;
    // An existing declaration may anchor an acquisition path that cannot fit.
    // Deferring that expansion does not defer the declaration already exposed.
    deferred.retain(|key, _| !context.exposed.contains(key));
    Ok(Plan {
        #[cfg(test)]
        diagnostic,
        wave: proposed
            .into_iter()
            .filter(|(key, _)| !context.exposed.contains(key))
            .collect(),
        deferred,
        issued,
        members,
    })
}
