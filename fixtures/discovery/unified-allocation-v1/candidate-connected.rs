//! Build one bounded candidate set before judgment. Paths nominate, never grant permission.
use super::*;

type Route = Vec<Edge<CapabilityRef>>;
type Bundle = BTreeMap<CapabilityRef, Route>;
const MAX_VARIANTS: usize = 64;

struct LocalPacket {
    scope: witnesses::Scope,
    question: Question,
    members: BTreeSet<CapabilityRef>,
    prepared: request_packing::PreparedLocal,
}

pub(super) struct Plan {
    pub wave: Vec<(CapabilityRef, Route)>,
    pub deferred: BTreeMap<CapabilityRef, DeferredReason>,
    pub issued: Option<IssuedBatch>,
    pub members: BTreeSet<CapabilityRef>,
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

pub(super) fn root_order(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
) -> Vec<CapabilityRef> {
    let mut order = index.ranking.rank(context.intent.current());
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
    // Never reset exploration progress. Within a round, finish a directly
    // connected frontier before unrelated roots; lexical ties remain stable.
    order.sort_by_key(|key| {
        (
            context.visits.get(key).copied().unwrap_or(0),
            !consumers.contains(key),
        )
    });
    order
}

pub(super) fn plan(
    index: &CatalogueSearch,
    context: SearchContext<'_>,
) -> Result<Plan, DiscoveryMatcherError> {
    let order = root_order(index, context);
    let ranks: BTreeMap<_, _> = order
        .iter()
        .enumerate()
        .map(|(n, key)| (key.clone(), n))
        .collect();
    let mut proposed = Bundle::new();
    let mut deferred = BTreeMap::new();
    let mut cache = BTreeMap::new();
    let mut completed = BTreeSet::new();
    for key in order
        .iter()
        .take(2)
        .filter(|key| context.exposed.contains(key))
    {
        proposed.insert(key.clone(), Vec::new());
    }
    loop {
        let mut routes = proposed.clone();
        let mut pending = required(
            &proposed.keys().cloned().collect(),
            context.catalogs,
            context.deployments,
            context.allowed,
        )?;
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
        // A nomination inherits the initiating root's rank, including through
        // required ancestry. Membership remains optional evidence.
        let mut nominations = BTreeMap::<usize, BTreeSet<(CapabilityRef, Route)>>::new();
        for edge in index.graph.nominate(&routes.keys().cloned().collect()) {
            if edge.relationship == Relationship::SuppliesInput {
                continue;
            }
            let mut path = routes[&edge.consumer].clone();
            if path
                .iter()
                .any(|e| e.source == edge.source || e.consumer == edge.source)
            {
                continue;
            }
            let origin = path.first().map_or(&edge.consumer, |e| &e.consumer);
            let rank = *ranks.get(origin).expect("route starts at a ranked root");
            path.push(edge.clone());
            nominations
                .entry(rank)
                .or_default()
                .insert((edge.source, path));
        }
        let current_size = body_size(&proposed, index, context, &mut cache)?;
        let mut winner = None;
        // Origin rank is the first priority component. Stop at the first
        // origin with a fitting proposal; later origins cannot win.
        for (rank, root) in order.iter().enumerate() {
            let mut offers = BTreeSet::<(u8, Bundle)>::new();
            if !context.exposed.contains(root) {
                if !completed.contains(root) {
                    let mut available = proposed.clone();
                    available.remove(root);
                    for bundle in variants(
                        root,
                        &Vec::new(),
                        index,
                        context,
                        &available,
                        &BTreeSet::new(),
                    )? {
                        offers.insert((0, bundle));
                    }
                }
                offers.insert((1, Bundle::from([(root.clone(), Vec::new())])));
            }
            if let Some(nominated) = nominations.get(&rank) {
                for (key, path) in nominated {
                    for bundle in variants(key, path, index, context, &proposed, &BTreeSet::new())?
                    {
                        offers.insert((2, bundle));
                    }
                }
            }
            let mut best: Option<((u8, usize, usize, Bundle), Bundle)> = None;
            for (class, bundle) in offers {
                let added = bundle
                    .keys()
                    .filter(|key| !proposed.contains_key(*key) && !context.exposed.contains(*key))
                    .count();
                if added == 0 {
                    continue;
                }
                let mut trial = proposed.clone();
                for (key, path) in &bundle {
                    trial.entry(key.clone()).or_insert_with(|| path.clone());
                }
                let size = body_size(&trial, index, context, &mut cache)?;
                if size > search_budget::MAX_INPUT_BYTES {
                    for key in bundle.keys().filter(|key| !proposed.contains_key(*key)) {
                        deferred.insert(key.clone(), DeferredReason::InputBytes);
                    }
                    continue;
                }
                let priority = (class, size.saturating_sub(current_size), added, bundle);
                if best.as_ref().is_none_or(|(old, _)| priority < *old) {
                    best = Some((priority, trial));
                }
            }
            if let Some(((class, _, _, _), trial)) = best {
                winner = Some((root.clone(), class, trial));
                break;
            }
        }
        let Some((root, class, trial)) = winner else {
            break;
        };
        if class == 0 {
            completed.insert(root);
        }
        proposed = trial;
        deferred.retain(|key, _| !proposed.contains_key(key));
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
        wave: proposed
            .into_iter()
            .filter(|(key, _)| !context.exposed.contains(key))
            .collect(),
        deferred,
        issued,
        members,
    })
}
