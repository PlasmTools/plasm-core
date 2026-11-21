//! Read a catalogue relationship as one path. A choice binds to both the
//! starting lookup and its declared materializer. It cannot grant a mutation.
use super::selection::{Assessment, Disposition, Verdict, Warning};
use super::*;
use plasm_core::schema::{CapabilitySchema, Cardinality, RelationMaterialization};

#[derive(Default)]
pub(crate) struct ReadRoutes {
    pub(super) initial_count: Option<usize>,
    groups: Vec<Group>,
    sources: super::source_reads::SourceIndex,
}

#[derive(Default)]
struct Group {
    catalog: String,
    entity: String,
    description: String,
    options: BTreeMap<String, Route>,
}

struct Route {
    meaning: String,
    ends: Vec<CapabilityRef>,
}

pub(crate) fn expand(
    recall: &mut RetrievalReceipt,
    catalogs: &BTreeMap<String, CGS>,
    allowed: &DiscoveryAuthorization,
    index: &CatalogueSearch,
) -> ReadRoutes {
    let mut groups = Vec::<Group>::new();
    let mut positions = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut present: BTreeSet<_> = recall
        .candidates
        .iter()
        .map(|c| c.reference.clone())
        .collect();
    // A catalogue can contribute independent evidence even when its first
    // operation falls outside the detailed projection. Pin the retrieval
    // scope before adding any endpoints from CGS.
    let recalled_catalogs = recall
        .candidates
        .iter()
        .map(|c| c.reference.catalog.clone())
        .collect();
    let initial_count = Some(recall.candidates.len());
    for hit in recall.candidates.iter().take(PROJECTED_CANDIDATE_LIMIT) {
        let api = &hit.reference.catalog;
        let name = &hit.document.entity;
        if !seen.insert((api, name)) {
            continue;
        }
        let Some(cgs) = catalogs.get(api) else {
            continue;
        };
        let Some(entity) = cgs.entities.get(name.as_str()) else {
            continue;
        };
        let get = cgs.primary_get_capability(name);
        let mut add = |target: &str, label: String, meaning: String, caps: &[&CapabilitySchema]| {
            if caps.iter().any(|c| {
                !matches!(
                    c.kind,
                    plasm_core::CapabilityKind::Get
                        | plasm_core::CapabilityKind::Query
                        | plasm_core::CapabilityKind::Search
                )
            }) {
                return;
            }
            let ends: Vec<_> = caps
                .iter()
                .map(|c| CapabilityRef {
                    catalog: api.clone(),
                    capability: c.name.to_string(),
                })
                .collect();
            if !ends
                .iter()
                .all(|r| allowed.permits(r) && index.candidate(r).is_some())
            {
                return;
            }
            let pos = *positions
                .entry((api.clone(), target.to_owned()))
                .or_insert_with(|| {
                    groups.push(Group {
                        catalog: api.clone(),
                        entity: target.into(),
                        description: cgs
                            .entities
                            .get(target)
                            .map(|e| e.description.clone())
                            .unwrap_or_default(),
                        ..Default::default()
                    });
                    groups.len() - 1
                });
            groups[pos].options.insert(label, Route { meaning, ends });
        };
        {
            for cap in get
                .into_iter()
                .chain(cgs.representative_query_for_projection(name))
            {
                add(name, cap.name.to_string(), cap.description.clone(), &[cap]);
            }
        }
        let Some(get) = get else { continue };
        for rel in entity.relations.values() {
            let target = match &rel.materialize {
                Some(
                    RelationMaterialization::GetScopedBindings { capability, .. }
                    | RelationMaterialization::QueryScopedBindings { capability, .. }
                    | RelationMaterialization::QueryScoped { capability, .. },
                ) => cgs.capabilities.get(capability),
                None if rel.cardinality == Cardinality::One => {
                    cgs.primary_get_capability(rel.target_resource.as_str())
                }
                _ => None,
            };
            if let Some(cap) = target {
                add(
                    rel.target_resource.as_str(),
                    format!("{name}__{}", rel.name),
                    format!(
                        "Read the identified {name} and follow its {} relation to {}. {}",
                        rel.name, rel.target_resource, cap.description
                    ),
                    &[get, cap],
                );
            }
        }
    }
    for reference in groups
        .iter()
        .flat_map(|g| g.options.values())
        .flat_map(|r| &r.ends)
    {
        if present.insert(reference.clone()) {
            recall
                .candidates
                .push(index.candidate(reference).expect("validated route").clone());
        }
    }
    let sources =
        super::source_reads::SourceIndex::new(&recalled_catalogs, catalogs, allowed, index);
    for r in sources.references() {
        if present.insert(r.clone()) {
            recall
                .candidates
                .push(index.candidate(r).expect("validated read").clone());
        }
    }
    ReadRoutes {
        initial_count,
        groups,
        sources,
    }
}

#[derive(Clone, Default)]
pub(crate) struct IssuedReads {
    pub(super) questions: BTreeMap<String, BTreeMap<String, Vec<CapabilityRef>>>,
    pub(super) material: BTreeMap<CapabilityRef, String>,
    pub(super) required_inputs: BTreeMap<CapabilityRef, usize>,
    pub(super) same_entity_query: BTreeMap<CapabilityRef, CapabilityRef>,
    pub(super) labels: BTreeMap<String, Vec<String>>,
}
impl ReadRoutes {
    pub(super) fn issue(
        &self,
        model: &str,
        intent: &str,
        candidates: &mut Vec<RetrievedCapability>,
        recall: &RetrievalReceipt,
        base_budget: usize,
    ) -> Result<(Option<GraphDecision>, BTreeSet<CapabilityRef>), WholeIntentError> {
        let mut issued = IssuedReads::default();
        if self.groups.is_empty() {
            return Ok((None, BTreeSet::new()));
        }
        let max_bytes = (MAX_SELECTION_BYTES / 2).min(base_budget);
        let mut packet = json!({"model":model,"state":{"intent":intent},"questions":{}});
        let mut projected = BTreeSet::new();
        let mut included: BTreeSet<_> = candidates.iter().map(|c| c.reference.clone()).collect();
        let base = included.clone();
        let scoped: BTreeSet<_> = base.iter().map(|r| &r.catalog).collect();
        let mut menus: BTreeMap<String, (String, BTreeMap<String, (Value, Vec<CapabilityRef>)>)> =
            BTreeMap::new();
        for g in &self.groups {
            if !scoped.contains(&g.catalog) {
                continue;
            }
            for (label, r) in &g.options {
                projected.extend(r.ends.iter().cloned());
                let target = &r.ends[0];
                let (menu, wording) = if r.ends.len() > 1 {
                    (
                        format!("path_{}_{}", target.catalog, target.capability),
                        format!(
                            "Starting at {} {}, which related records, if any, does the user want to see? Select unrelated if no outgoing relationship is requested.",
                            target.catalog, target.capability
                        ),
                    )
                } else {
                    let is_get = recall.candidates.iter().any(|c| {
                        c.reference == *target
                            && c.document.operation.kind == plasm_core::CapabilityKind::Get
                    });
                    if is_get {
                        ("record".into(),"Which identified record must be opened to see the material the user asks about?".into())
                    } else {
                        ("collection".into(),"Which collection contains the evidence against which the user wants to check something, or the items the user wants listed?".into())
                    }
                };
                let entry = menus
                    .entry(menu)
                    .or_insert_with(|| (wording, BTreeMap::new()));
                let name = format!("{}_{}", g.catalog, label);
                let description = if r.ends.len() > 1 {
                    r.meaning.clone()
                } else {
                    format!(
                        "{} {}. {} {}",
                        g.catalog, g.entity, g.description, r.meaning
                    )
                };
                entry
                    .1
                    .insert(name.clone(), (json!(description), r.ends.clone()));
                if r.ends.len() > 1 {
                    menus.entry(format!("target_{}_{}",g.catalog,g.entity)).or_insert_with(||(format!("Is {} {} a result the user wants? If so, choose the relationship from the identified source. If the result is incidental, choose unrelated.",g.catalog,g.entity),BTreeMap::new())).1.insert(name,(json!(format!("{} {} leads to {}",g.catalog,label,g.entity)),r.ends.clone()));
                }
            }
        }
        let mut size = packet.to_string().len();
        let rank: BTreeMap<_, _> = recall
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (&c.reference, i))
            .collect();
        let mut add_candidate = |r: &CapabilityRef| -> Result<(), WholeIntentError> {
            if included.insert(r.clone()) {
                let i = rank.get(r).ok_or(WholeIntentError::InvalidInput)?;
                candidates.push(recall.candidates[*i].clone());
            }
            Ok(())
        };
        let mut menus: Vec<_> = menus.into_iter().collect();
        menus.sort_by_key(|(id, (_, opts))| {
            (
                id.starts_with("path_") || id.starts_with("target_"),
                opts.values()
                    .filter_map(|(_, ends)| rank.get(&ends[0]))
                    .min()
                    .copied(),
            )
        });
        for (id, (wording, items)) in menus {
            let mut choices = BTreeMap::new();
            let mut criteria = Map::new();
            criteria.insert("unrelated".into(), json!("No such read is needed."));
            let instructions = format!(
                "User request: {intent}\n{wording} Compare the alternatives and select the best match, or unrelated."
            );
            let overhead = id.len() + instructions.len() + 160;
            if size + overhead >= max_bytes {
                continue;
            }
            let budget = if id.starts_with("path_") { 2200 } else { 6500 };
            let mut remaining = (max_bytes - size - overhead).min(budget);
            let mut items: Vec<_> = items.into_iter().collect();
            items.sort_by_key(|(_, (_, ends))| {
                rank.get(ends.last().expect("issued endpoint")).copied()
            });
            for (name, (description, ends)) in items {
                let cost = name.len() + description.to_string().len() + 5;
                if cost > remaining {
                    continue;
                }
                remaining -= cost;
                criteria.insert(name.clone(), description);
                choices.insert(name, ends);
            }
            if choices.is_empty() {
                continue;
            }
            let q = json!({"type":"choice","instructions":instructions,"criteria":criteria});
            size += q.to_string().len() + id.len() + 4;
            packet["questions"][&id] = q;
            for r in choices.values().flatten() {
                add_candidate(r)?;
            }
            issued.questions.insert(id, choices);
        }
        let max_bytes = max_bytes + base_budget.min(MAX_SELECTION_BYTES / 2);
        self.sources
            .issue(intent, &mut packet, &mut issued, max_bytes);
        for r in issued
            .questions
            .values()
            .flat_map(|opts| opts.values())
            .flatten()
        {
            projected.insert(r.clone());
            add_candidate(r)?;
        }
        let body = packet.to_string();
        if body.len() > max_bytes {
            return Err(WholeIntentError::Budget);
        }
        if issued.questions.is_empty() {
            return Ok((None, projected));
        }
        let binding = serde_json::to_vec(&(&body, &issued.questions))?;
        Ok((
            Some(GraphDecision {
                key: content_hash(&binding),
                model: model.into(),
                intent: intent.into(),
                body,
                issued,
            }),
            projected,
        ))
    }
}
impl IssuedReads {
    /// A record independently nominated as the source and reached by a
    /// corroborated graph path already supplies the material side of a comparison.
    pub(super) fn material_receiver(
        &self,
        answers: &BTreeMap<String, crate::decision_codec::ValidatedChoice>,
    ) -> Option<&CapabilityRef> {
        let selected = |id: &str| self.questions.get(id)?.get(&answers.get(id)?.choice);
        let record = selected("record")?.first()?;
        if self.same_entity_query.get(record)? != selected("evidence")?.first()? {
            return None;
        }
        if selected(&format!("source_{}", record.catalog))?.first()? != record {
            return None;
        }
        self.questions
            .keys()
            .filter(|id| id.starts_with("path_"))
            .filter_map(|id| selected(id))
            .find(|ends| {
                ends.len() > 1
                    && ends.first() == Some(record)
                    && self
                        .questions
                        .keys()
                        .filter(|id| id.starts_with("target_"))
                        .any(|id| selected(id) == Some(*ends))
            })?;
        Some(record)
    }
    pub(super) fn extend_labels<'a>(&'a self, labels: &mut BTreeMap<String, Vec<&'a str>>) {
        if !self.material.is_empty() {
            labels.insert("scope".into(), vec!["compare", "other"]);
            labels.insert(
                "locator".into(),
                vec!["self_contained", "scoped", "unspecified"],
            );
        }
        for (id, options) in &self.questions {
            labels.insert(
                id.clone(),
                std::iter::once(if id.starts_with("source_") || id.starts_with("fact_") {
                    "absent"
                } else {
                    "unrelated"
                })
                .chain(options.keys().map(String::as_str))
                .collect(),
            );
        }
        for (key, opts) in &self.labels {
            labels.insert(key.clone(), opts.iter().map(String::as_str).collect());
        }
    }
    pub(crate) fn apply(
        &self,
        answers: &BTreeMap<String, crate::decision_codec::ValidatedChoice>,
        assessments: &mut BTreeMap<CapabilityRef, Assessment>,
    ) {
        let mut anchors: BTreeSet<_> = assessments
            .iter()
            .filter(|(_, a)| a.disposition.teaches())
            .map(|(r, _)| r.clone())
            .collect();
        let comparing = answers.get("scope").is_some_and(|a| a.choice == "compare");
        if comparing {
            anchors.extend(self.material_receiver(answers).cloned());
        }
        for key in if comparing {
            ["evidence", "read"]
        } else {
            ["record", "collection"]
        } {
            if let Some(ends) = self
                .questions
                .get(key)
                .and_then(|opts| opts.get(&answers[key].choice))
            {
                anchors.extend(ends.iter().cloned());
            }
        }
        let corroborated: BTreeSet<_> = self
            .questions
            .iter()
            .filter(|(id, _)| id.starts_with("target_"))
            .filter_map(|(id, opts)| opts.get(&answers[id].choice))
            .collect();
        for (id, options) in &self.questions {
            if (id.starts_with("source_") || id.starts_with("fact_"))
                || id.starts_with("target_")
                || (comparing && matches!(id.as_str(), "record" | "collection"))
                || (!comparing && id == "evidence")
            {
                continue;
            }
            let answer = &answers[id];
            let Some(ends) = options.get(&answer.choice) else {
                continue;
            };
            if ends.len() > 1
                && !anchors.contains(&ends[0])
                && (comparing
                    || !anchors.iter().any(|r| r.catalog == ends[0].catalog)
                    || !corroborated.contains(ends))
            {
                continue;
            }
            // A single unknown source may fit two issued read contracts. Preserve
            // the runner-up when Luna cannot distinguish them by even 2:1 odds.
            // Only material lookups participate; graph edges and effects do not.
            let alternative = (id == "read" && answer.confidence < 0.5)
                .then(|| {
                    let selected = answer.probabilities.get(&answer.choice)?;
                    answer
                        .probabilities
                        .iter()
                        .filter(|(key, probability)| {
                            *key != &answer.choice
                                && **probability > 0.0
                                && **probability * 2.0 >= *selected
                        })
                        .max_by(|a, b| a.1.total_cmp(b.1))
                        .and_then(|(key, _)| options.get(key))
                })
                .flatten();
            for r in ends.iter().chain(alternative.into_iter().flatten()) {
                if assessments.get(r).is_some_and(|a| a.disposition.teaches()) {
                    continue;
                }
                assessments.insert(
                    r.clone(),
                    Assessment {
                        verdict: if alternative.is_some() {
                            Verdict::UncertainInput
                        } else {
                            Verdict::Include
                        },
                        confidence: answer.confidence,
                        disposition: Disposition::Warn(Warning::ReadPath),
                    },
                );
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct GraphDecision {
    pub(super) key: String,
    pub(super) model: String,
    pub(super) intent: String,
    pub(crate) body: String,
    pub(crate) issued: IssuedReads,
}
impl DecisionPacket for GraphDecision {
    type Response = (
        BTreeMap<String, crate::decision_codec::ValidatedChoice>,
        Option<u64>,
    );
    fn cache_key(&self) -> &str {
        &self.key
    }
    fn validate_response(
        &self,
        raw: &str,
    ) -> Result<Self::Response, Box<dyn std::error::Error + Send + Sync>> {
        let mut questions = BTreeMap::new();
        self.issued.extend_labels(&mut questions);
        Ok(crate::decision_codec::decode_with_usage(
            &self.model,
            &questions,
            raw,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_codec::ValidatedChoice;

    #[test]
    fn chosen_edge_teaches_both_endpoints_and_leaves_other_routes_unassessed() {
        let reference = |name: &str| CapabilityRef {
            catalog: "matrix".into(),
            capability: name.into(),
        };
        let root = reference("zone_get");
        let rules = reference("ruleset_query");
        let other = reference("widget_get");
        let issued = IssuedReads {
            material: Default::default(),
            required_inputs: Default::default(),
            same_entity_query: Default::default(),
            labels: Default::default(),
            questions: BTreeMap::from([(
                "relation".into(),
                BTreeMap::from([
                    ("rules".into(), vec![root.clone(), rules.clone()]),
                    ("widget".into(), vec![other.clone()]),
                ]),
            )]),
        };
        let answer = |choice: &str| {
            BTreeMap::from([(
                "relation".into(),
                ValidatedChoice {
                    choice: choice.into(),
                    confidence: 0.8,
                    probabilities: BTreeMap::new(),
                },
            )])
        };
        let mut assessments = BTreeMap::from([(
            root.clone(),
            Assessment {
                verdict: Verdict::Include,
                confidence: 0.9,
                disposition: Disposition::Warn(Warning::ReadPath),
            },
        )]);
        let mut target_only = BTreeMap::from([(rules.clone(), assessments[&root].clone())]);
        issued.apply(&answer("rules"), &mut target_only);
        assert!(!target_only.contains_key(&root));
        issued.apply(&answer("unrelated"), &mut assessments);
        assert_eq!(assessments.len(), 1);
        issued.apply(&answer("rules"), &mut assessments);
        assert_eq!(assessments.len(), 2);
        for r in [root, rules] {
            assert_eq!(
                assessments[&r].disposition,
                Disposition::Warn(Warning::ReadPath)
            );
        }
        assert!(!assessments.contains_key(&other));
    }
}
