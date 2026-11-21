//! Compare independently consulted sources. Nomination establishes possible
//! catalogue reads, never the identity of a record or permission to mutate it.
use super::graph_reads::{GraphDecision, IssuedReads};
use super::*;
use crate::decision_codec::ValidatedChoice;

#[derive(Clone, Default)]
pub(super) struct SourceIndex {
    records: BTreeMap<String, BTreeMap<String, Source>>,
    collections: BTreeMap<String, Source>,
}
#[derive(Clone)]
struct Source {
    reference: CapabilityRef,
    description: String,
    binding: String,
    required_inputs: usize,
}
impl SourceIndex {
    pub(super) fn new(
        recalled_catalogs: &BTreeSet<String>,
        catalogs: &BTreeMap<String, CGS>,
        allowed: &DiscoveryAuthorization,
        index: &CatalogueSearch,
    ) -> Self {
        let mut sources = Self::default();
        for api in recalled_catalogs {
            let Some(cgs) = catalogs.get(api) else {
                continue;
            };
            for (name, e) in &cgs.entities {
                let subject = name.as_str();
                for (record, cap) in [
                    (true, cgs.primary_get_capability(subject)),
                    (false, cgs.representative_query_for_projection(subject)),
                ] {
                    let Some(cap) = cap else { continue };
                    let reference = CapabilityRef {
                        catalog: api.clone(),
                        capability: cap.name.to_string(),
                    };
                    if !allowed.permits(&reference) || index.candidate(&reference).is_none() {
                        continue;
                    }
                    if record && cap.kind == plasm_core::CapabilityKind::Get {
                        let mut required = if e.key_vars.is_empty() {
                            vec![e.id_field.as_str()]
                        } else {
                            e.key_vars.iter().map(|k| k.as_str()).collect()
                        };
                        for field in cap.input_fields().filter(|f| f.required) {
                            if !required.contains(&field.name.as_str()) {
                                required.push(field.name.as_str());
                            }
                        }
                        let source = Source {
                            reference,
                            description: e.description.clone(),
                            required_inputs: required.len(),
                            binding: format!(
                                "{api}::{subject} — {} Required lookup inputs: {}. {}",
                                e.description,
                                required.join(", "),
                                cap.description
                            ),
                        };
                        sources
                            .records
                            .entry(api.clone())
                            .or_default()
                            .insert(subject.into(), source);
                    } else if !record && cap.kind == plasm_core::CapabilityKind::Query {
                        sources.collections.insert(
                            format!("{api}_{subject}"),
                            Source {
                                reference,
                                description: cap.description.clone(),
                                binding: String::new(),
                                required_inputs: 0,
                            },
                        );
                    }
                }
            }
        }
        sources
    }
    pub(super) fn references(&self) -> impl Iterator<Item = &CapabilityRef> {
        self.records
            .values()
            .flat_map(|m| m.values())
            .chain(self.collections.values())
            .map(|s| &s.reference)
    }
    pub(super) fn issue(
        &self,
        intent: &str,
        packet: &mut Value,
        issued: &mut IssuedReads,
        limit: usize,
    ) {
        if self.records.is_empty() || self.collections.is_empty() {
            return;
        }
        let mut questions = Map::new();
        let mut bindings = BTreeMap::new();
        let mut details = BTreeMap::new();
        questions.insert("scope".into(),json!({"type":"choice","instructions":format!("What does the user need to know? {intent}"),"criteria":{"compare":"Inspect existing material AND independently check it against facts from another source. The decision about whether to act is separate.","other":"Inspect a record, traverse from it, or perform an action without independently corroborating its content."}}));
        questions.insert("locator".into(),json!({"type":"choice","instructions":format!("User: {intent}\nHow is the original material to inspect identified? Consider only that record and its own containing scope, separately from the independent evidence."),"criteria":{"self_contained":"An identifier or name identifies the item by itself.","scoped":"The request identifies both the item and an external container required to resolve it.","unspecified":"No containing scope or clear identity is established."}}));
        let mut input_counts = BTreeMap::new();
        for (api, items) in &self.records {
            let id = format!("source_{api}");
            let mut options = Map::new();
            let mut routes = BTreeMap::new();
            for (name, source) in items {
                options.insert(name.clone(), json!(source.description));
                routes.insert(name.clone(), vec![source.reference.clone()]);
                details.insert(source.reference.clone(), source.binding.clone());
                input_counts.insert(source.reference.clone(), source.required_inputs);
            }
            options.insert(
                "absent".into(),
                json!("None of these types provides that evidence."),
            );
            questions.insert(id.clone(),json!({"type":"choice","instructions":format!("User: {intent}\nIdentify the existing item whose contents the user refers to by an identifier or name. Which {api} type best fits that role, even if the record key still needs confirmation?"),"criteria":options}));
            bindings.insert(id, routes);
        }
        let mut by_catalog = BTreeMap::<_, Vec<_>>::new();
        for (name, source) in &self.collections {
            by_catalog
                .entry(&source.reference.catalog)
                .or_default()
                .push((name, source));
        }
        for (api, sources) in by_catalog {
            let mut options = Map::new();
            let mut routes = BTreeMap::new();
            for (name, source) in sources {
                options.insert(name.clone(), json!(source.description));
                routes.insert(name.clone(), vec![source.reference.clone()]);
                details.insert(source.reference.clone(), source.description.clone());
            }
            options.insert(
                "absent".into(),
                json!("No independent evidence from this catalogue is required."),
            );
            let id = format!("fact_{api}");
            questions.insert(id.clone(),json!({"type":"choice","instructions":format!("User: {intent}\nAssume you have already opened the source material for inspection. Which remaining collection supplies the independent evidence needed to check it? Choose the row source containing that evidence."),"criteria":options}));
            bindings.insert(id, routes);
        }
        let mut options = Map::new();
        let mut routes = BTreeMap::new();
        for (name, source) in &self.collections {
            options.insert(name.clone(), json!(source.description));
            routes.insert(name.clone(), vec![source.reference.clone()]);
        }
        options.insert(
            "unrelated".into(),
            json!("No comparison evidence of this kind is requested."),
        );
        questions.insert("evidence".into(),json!({"type":"choice","instructions":format!("User: {intent}\nIdentify the type of event, observation or other evidence whose latest or matching instances are needed to evaluate the original item. Which collection returns the actual evidence? Choose the closest existing source. Record binding and action authorization are determined separately."),"criteria":options}));
        bindings.insert("evidence".into(), routes);
        if questions.len() + packet["questions"].as_object().map_or(0, Map::len)
            > super::super::decisions::MAX_QUESTIONS
            || packet.to_string().len() + Value::Object(questions.clone()).to_string().len() > limit
        {
            return;
        }
        packet["questions"]
            .as_object_mut()
            .expect("issued question object")
            .extend(questions);
        issued.questions.extend(bindings);
        for (catalog, records) in &self.records {
            for (entity, record) in records {
                if let Some(collection) = self.collections.get(&format!("{catalog}_{entity}")) {
                    issued
                        .same_entity_query
                        .insert(record.reference.clone(), collection.reference.clone());
                }
            }
        }
        issued.material = details;
        issued.required_inputs = input_counts;
    }
}

/// Nominate one concrete type when their combined probability exceeds absence.
/// Only the final ranking can expose that read, with an uncertain binding.
fn nominee<'a>(
    answer: &ValidatedChoice,
    options: &'a BTreeMap<String, Vec<CapabilityRef>>,
) -> Option<&'a str> {
    if let Some((key, _)) = options.get_key_value(&answer.choice) {
        return Some(key);
    }
    let absent = *answer.probabilities.get("absent")?;
    let mass: f64 = options
        .keys()
        .filter_map(|k| answer.probabilities.get(k))
        .sum();
    if mass <= absent {
        return None;
    }
    let key = options
        .keys()
        .max_by(|a, b| answer.probabilities[*a].total_cmp(&answer.probabilities[*b]))?;
    (answer.probabilities[key] > mass / 2.0).then(|| key.as_str())
}
impl GraphDecision {
    pub(crate) fn material_packet(
        &self,
        answers: &BTreeMap<String, ValidatedChoice>,
    ) -> Option<Self> {
        if !answers.get("scope").is_some_and(|a| a.choice == "compare") {
            return None;
        }
        let mut questions = Map::new();
        let mut issued = IssuedReads::default();
        for (role, prefix, prompt) in [
            (
                "read",
                "source_",
                "Which candidate opens the actual source material being discussed? If an item is only identified by a local reference, select the most plausible read while leaving the binding unverified.",
            ),
            (
                "facts",
                "fact_",
                "Which candidate supplies the independent factual evidence against which the original item is being evaluated? Select the collection that contains the evidence itself.",
            ),
        ] {
            if role == "read" && self.issued.material_receiver(answers).is_some() {
                continue;
            }
            if role == "facts"
                && answers
                    .get("evidence")
                    .is_some_and(|v| v.choice != "unrelated")
            {
                continue;
            }
            let mut choices = BTreeMap::new();
            let mut criteria = Map::new();
            for (id, opts) in &self.issued.questions {
                if !id.starts_with(prefix) {
                    continue;
                }
                let Some(nominee) = nominee(&answers[id], opts) else {
                    continue;
                };
                let refs = &opts[nominee];
                let r = &refs[0];
                let label = if role == "read" {
                    format!("{}_{}", r.catalog, nominee)
                } else {
                    nominee.to_owned()
                };
                criteria.insert(label.clone(), json!(self.issued.material[r]));
                choices.insert(label, refs.clone());
            }
            // Scoped lookups require evidence of the original record's own scope.
            // Unresolved alternatives stay in the ordinary discovery receipt.
            if role == "read" && answers.get("locator").is_some_and(|a| a.choice != "scoped") {
                choices.retain(|_, refs| {
                    self.issued
                        .required_inputs
                        .get(&refs[0])
                        .is_none_or(|n| *n <= 1)
                });
                criteria.retain(|k, _| choices.contains_key(k));
            }
            if choices.is_empty() {
                continue;
            }
            if choices.len() == 1 {
                criteria.insert(
                    "unrelated".into(),
                    json!("This source cannot supply the requested information."),
                );
            }
            issued
                .labels
                .insert(role.into(), criteria.keys().cloned().collect());
            issued.questions.insert(role.into(), choices);
            let prompt = if role == "read" {
                answers.get("evidence").filter(|answer|answer.choice!="unrelated").map(|answer|format!("The independent evidence will be obtained from {}. Which candidate opens the OTHER item, containing the original claim to check? Match its required lookup inputs to the reference the user supplied. If the service is implicit, select a plausible read and keep the binding unverified.",answer.choice)).unwrap_or_else(||prompt.into())
            } else {
                prompt.into()
            };
            questions.insert(role.into(),json!({"type":"choice","instructions":format!("User: {}\n{prompt}",self.intent),"criteria":criteria}));
        }
        if questions.is_empty() {
            return None;
        }
        let body = json!({"model":self.model,"state":{"intent":self.intent},"questions":questions})
            .to_string();
        if body.len() > MAX_SELECTION_BYTES / 2 {
            return None;
        }
        let key = content_hash(&serde_json::to_vec(&(&body, &issued.questions)).ok()?);
        Some(Self {
            body,
            model: self.model.clone(),
            key,
            issued,
            intent: self.intent.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::selection::{Disposition, Verdict, Warning};
    use super::*;
    fn answer(choice: &str) -> ValidatedChoice {
        ValidatedChoice {
            choice: choice.into(),
            confidence: 0.8,
            probabilities: BTreeMap::new(),
        }
    }
    fn reference(catalog: &str, capability: &str) -> CapabilityRef {
        CapabilityRef {
            catalog: catalog.into(),
            capability: capability.into(),
        }
    }
    #[test]
    fn competing_record_types_preserve_uncertain_catalogue_support() {
        let opts = BTreeMap::from([("A".into(), vec![]), ("B".into(), vec![])]);
        let mut v = answer("absent");
        v.probabilities = BTreeMap::from([
            ("absent".into(), 0.48),
            ("A".into(), 0.46),
            ("B".into(), 0.06),
        ]);
        assert_eq!(nominee(&v, &opts), Some("A"));
        v.probabilities = BTreeMap::from([
            ("absent".into(), 0.70),
            ("A".into(), 0.25),
            ("B".into(), 0.05),
        ]);
        assert!(nominee(&v, &opts).is_none());
    }
    #[test]
    fn independent_source_ranking_is_conditional_and_binds_issued_reads() {
        let item = reference("library", "note_get");
        let observations = reference("monitor", "event_query");
        let mut issued = IssuedReads::default();
        for (id, option, r) in [
            ("source_library", "Note", &item),
            ("fact_monitor", "monitor_Event", &observations),
        ] {
            issued.questions.insert(
                id.into(),
                BTreeMap::from([(option.into(), vec![r.clone()])]),
            );
            issued
                .material
                .insert(r.clone(), format!("Evidence for {option}"));
        }
        let graph = GraphDecision {
            body: String::new(),
            key: String::new(),
            model: "decision".into(),
            intent: "Check the note against independent observations".into(),
            issued,
        };
        let mut answers = BTreeMap::from([
            ("scope".into(), answer("other")),
            ("source_library".into(), answer("Note")),
            ("fact_monitor".into(), answer("monitor_Event")),
            ("evidence".into(), answer("unrelated")),
        ]);
        assert!(graph.material_packet(&answers).is_none());
        answers.insert("scope".into(), answer("compare"));
        let packet = graph.material_packet(&answers).unwrap();
        let ranked = BTreeMap::from([
            ("read".into(), answer("library_Note")),
            ("facts".into(), answer("monitor_Event")),
        ]);
        let mut assessments = BTreeMap::new();
        packet.issued.apply(&ranked, &mut assessments);
        assert_eq!(assessments.len(), 2);
        for r in [&item, &observations] {
            assert_eq!(
                assessments[r].disposition,
                Disposition::Warn(Warning::ReadPath)
            );
        }
        let mut labels = BTreeMap::new();
        packet.issued.extend_labels(&mut labels);
        assert_eq!(labels["read"], ["library_Note", "unrelated"]);
        assert!(!labels["facts"].contains(&"library_Note"));
        let other = reference("archive", "entry_get");
        let mut ambiguous = packet.issued.clone();
        ambiguous
            .questions
            .get_mut("read")
            .unwrap()
            .insert("archive_Entry".into(), vec![other.clone()]);
        let mut split = ranked;
        let vote = split.get_mut("read").unwrap();
        vote.confidence = 0.10;
        vote.probabilities = BTreeMap::from([
            ("library_Note".into(), 0.55),
            ("archive_Entry".into(), 0.45),
        ]);
        let mut taught = BTreeMap::new();
        ambiguous.apply(&split, &mut taught);
        assert_eq!(taught[&other].verdict, Verdict::UncertainInput);
        split.get_mut("read").unwrap().confidence = 0.85;
        taught.clear();
        ambiguous.apply(&split, &mut taught);
        assert!(!taught.contains_key(&other));
        // The comparison's other source cannot supply a missing record scope.
        let mut graph = graph;
        graph.issued.required_inputs.insert(item.clone(), 2);
        answers.insert("locator".into(), answer("self_contained"));
        let packet = graph.material_packet(&answers).unwrap();
        assert!(!packet.issued.questions.contains_key("read"));
        assert!(packet.issued.questions.contains_key("facts"));
        answers.insert("locator".into(), answer("scoped"));
        assert!(graph
            .material_packet(&answers)
            .unwrap()
            .issued
            .questions
            .contains_key("read"));
        answers.insert("source_library".into(), answer("absent"));
        answers.insert("fact_monitor".into(), answer("absent"));
        assert!(graph.material_packet(&answers).is_none());
    }
}
