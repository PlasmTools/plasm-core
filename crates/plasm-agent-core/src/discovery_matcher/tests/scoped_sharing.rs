use super::*;

fn locals() -> (RetrievalReceipt, CatalogEvidence, Vec<IssuedBatch>) {
    let (catalogs, retrieval) = super::reference_index::matrix();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    let intent =
        IntentProvenance::from_turns(["Prepare material while preserving originals.".into()])
            .unwrap();
    let local = prepare_local_selection(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    assert!(local.budget_retained.is_empty());
    (retrieval, evidence, local.batches)
}

#[test]
fn connected_packet_reconstructs_complete_question_local_contracts() {
    let (_, _, local) = locals();
    let packed = request_packing::connected(local.clone()).unwrap().unwrap();
    let wire: serde_json::Value = serde_json::from_str(packed.body()).unwrap();
    let shared = &wire["state"];
    assert_eq!(packed.bindings.len(), local.len());
    for original in &local {
        let original: serde_json::Value = serde_json::from_str(original.body()).unwrap();
        for key in original["questions"].as_object().unwrap().keys() {
            let scoped = &shared["question_evidence"][key];
            let mut restored = json!({"values":{},"records":{}});
            for id in scoped["definition_refs"].as_array().unwrap() {
                let definition = &shared["definitions"][id.as_str().unwrap()];
                restored[definition["kind"].as_str().unwrap()]
                    [definition["name"].as_str().unwrap()] = definition["definition"].clone();
            }
            assert_eq!(restored, original["state"]["types"]);
            assert_eq!(scoped["capabilities"], original["state"]["capabilities"]);
            assert_eq!(scoped["type_refs"], original["state"]["type_refs"]);
            let restored: Vec<_> = scoped["binding_refs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| shared["binding_witnesses"][id.as_str().unwrap()].clone())
                .collect();
            assert_eq!(
                restored,
                original["state"]["bindings"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            );
            assert_eq!(
                wire["questions"][key]["criteria"],
                original["questions"][key]["criteria"]
            );
            assert!(!wire["questions"][key]["instructions"]
                .as_str()
                .unwrap()
                .contains("{\""));
        }
    }
    assert!(packed.body().len() < local.iter().map(|p| p.body().len()).sum::<usize>());
    assert_eq!(
        packed.body(),
        request_packing::connected(local).unwrap().unwrap().body()
    );
}

#[test]
fn connected_context_changes_fail_without_silently_mixing_intents() {
    let (_, _, local) = locals();
    for field in ["outcome", "discovery_need", "rule"] {
        let mut batches = local.clone();
        let mut wire: serde_json::Value = serde_json::from_str(batches[1].body()).unwrap();
        wire["state"][field] = json!("Explicit revision; do not prepare material.");
        batches[1].body = wire.to_string();
        assert!(matches!(
            request_packing::connected(batches),
            Err(DiscoveryMatcherError::InvalidConnectedContext)
        ));
    }
}

fn reconstruct(
    packet: &serde_json::Value,
    key: &str,
    original: &serde_json::Value,
) -> serde_json::Value {
    if packet["questions"].as_object().unwrap().len() == 1 {
        return packet["state"].clone();
    }
    let prefix = original["questions"][key]["instructions"].as_str().unwrap();
    let text = packet["questions"][key]["instructions"].as_str().unwrap();
    let local: serde_json::Value =
        serde_json::from_str(text.strip_prefix(&format!("{prefix}\n")).unwrap()).unwrap();
    let mut state = packet["state"].clone();
    for (name, value) in local.as_object().unwrap() {
        if name == "types" {
            for kind in ["values", "records"] {
                for (key, definition) in value[kind].as_object().unwrap() {
                    assert!(state["types"][kind].get(key).is_none());
                    state["types"][kind][key] = definition.clone();
                }
            }
        } else {
            assert!(state.get(name).is_none());
            state[name] = value.clone();
        }
    }
    state
}

#[test]
fn packed_questions_reconstruct_exact_composition_and_opaque_scopes() {
    let (_, _, local) = locals();
    let packed = request_packing::pack(local.clone()).unwrap();
    assert!(packed.len() < local.len());
    assert!(
        packed.iter().map(|p| p.body.len()).sum::<usize>()
            < local.iter().map(|p| p.body.len()).sum::<usize>()
    );
    let mut seen = BTreeSet::new();
    for batch in &packed {
        assert!(batch.body.len() <= witnesses::MAX_BYTES);
        assert!(batch.bindings.len() <= 4);
        let packet: serde_json::Value = serde_json::from_str(&batch.body).unwrap();
        for key in batch.bindings.keys() {
            assert!(seen.insert(key.clone()));
            let original = local.iter().find(|b| b.bindings.contains_key(key)).unwrap();
            let wire: serde_json::Value = serde_json::from_str(&original.body).unwrap();
            assert_eq!(reconstruct(&packet, key, &wire), wire["state"]);
            assert_eq!(
                packet["questions"][key]["criteria"],
                wire["questions"][key]["criteria"]
            );
            // Canonical card order also survives the question-local JSON string.
            if batch.bindings.len() > 1 {
                let start = original.body.find("\"capabilities\":").unwrap();
                let end = original.body.find(",\"discovery_need\":").unwrap();
                assert!(packet["questions"][key]["instructions"]
                    .as_str()
                    .unwrap()
                    .contains(&original.body[start..end]));
            }
        }
    }
    assert_eq!(seen.len(), local.len());
}

#[test]
fn singleton_context_changes_and_provider_model_keep_exact_bytes() {
    let (_, _, local) = locals();
    let first = local[0].clone();
    assert_eq!(
        request_packing::pack(vec![first.clone()]).unwrap()[0].body,
        first.body
    );
    for field in ["outcome", "discovery_need", "rule"] {
        let mut second = local[1].clone();
        let mut body: serde_json::Value = serde_json::from_str(&second.body).unwrap();
        body["state"][field] = json!("Explicitly revised context");
        second.body = body.to_string();
        let packed = request_packing::pack(vec![first.clone(), second.clone()]).unwrap();
        assert_eq!(packed.len(), 2);
        assert_eq!(packed[0].body, first.body);
        assert_eq!(packed[1].body, second.body);
    }
}

#[test]
fn packed_results_remain_exactly_covered_and_cached_receipts_agree() {
    let (retrieval, _, local) = locals();
    let packed = request_packing::pack(local).unwrap();
    let mut pages = SelectionPages::new(packed.clone());
    let mut answers = Vec::new();
    for batch in &packed {
        let choices = vec![MatchChoice::Uncertain; batch.bindings.len()];
        let raw = response(batch, &choices);
        answers.extend(decode_batch(batch, &raw).unwrap());
        pages.accept(&raw).unwrap();
    }
    let sequential = pages.finish(&retrieval).unwrap();
    answers.reverse();
    assert_eq!(
        finish_selection(answers, &retrieval, &packed).unwrap(),
        sequential
    );
    assert_eq!(
        sequential.selected(&retrieval).len(),
        retrieval.candidates.len()
    );
    let mut malformed: serde_json::Value = serde_json::from_str(&response(
        &packed[0],
        &vec![MatchChoice::Uncertain; packed[0].bindings.len()],
    ))
    .unwrap();
    let missing = malformed["answers"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    malformed["answers"]
        .as_object_mut()
        .unwrap()
        .remove(&missing);
    assert!(decode_batch(&packed[0], &malformed.to_string()).is_err());
}

#[test]
fn changed_wire_shapes_fail_before_any_evidence_can_be_lost() {
    let (_, _, local) = locals();
    for (section, field, value) in [
        ("state", "new_evidence", json!({"meaning":"must survive"})),
        ("types", "values", json!(null)),
        ("types", "records", json!([])),
        ("types", "new_definitions", json!({})),
    ] {
        let mut batches = local.clone();
        let mut wire: serde_json::Value = serde_json::from_str(&batches[0].body).unwrap();
        if section == "state" {
            wire["state"][field] = value;
        } else {
            wire["state"]["types"][field] = value;
        }
        batches[0].body = wire.to_string();
        assert!(matches!(
            request_packing::pack(batches),
            Err(DiscoveryMatcherError::Packing(_))
        ));
    }
}

#[test]
fn production_packing_preserves_permutation_and_exact_cache_identity() {
    let (mut retrieval, evidence, _) = locals();
    let intent =
        IntentProvenance::from_turns(["Prepare material while preserving originals.".into()])
            .unwrap();
    let first = prepare_selection(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    retrieval.candidates.reverse();
    let reversed = prepare_selection(RELEVANCE_MODEL, &intent, &retrieval, &evidence).unwrap();
    assert_eq!(
        first
            .batches
            .iter()
            .map(|b| (&b.body, &b.cache_key))
            .collect::<Vec<_>>(),
        reversed
            .batches
            .iter()
            .map(|b| (&b.body, &b.cache_key))
            .collect::<Vec<_>>()
    );
    let revised = prepare_selection(
        RELEVANCE_MODEL,
        &intent
            .derived("Inspect only; do not prepare material.".into())
            .unwrap(),
        &retrieval,
        &evidence,
    )
    .unwrap();
    assert_ne!(first.batches[0].cache_key, revised.batches[0].cache_key);
}

#[test]
fn conflicting_definitions_nulls_and_absence_remain_question_local() {
    let (_, _, local) = locals();
    let mut batches = local[..2].to_vec();
    for (index, batch) in batches.iter_mut().enumerate() {
        let mut wire: serde_json::Value = serde_json::from_str(&batch.body).unwrap();
        wire["state"]["types"]["values"]["matrix/shared"] = json!({
            "description": "A signed observation retains its measurement, unit, source identity, observation time and verification status. Consumers can use the verified measurement to derive an aggregate without changing the source observation or treating an absent measurement as null.",
            "type": "string"
        });
        wire["state"]["types"]["values"]["matrix/opaque"] = if index == 0 {
            json!({"description":"Unknown", "observed":null})
        } else {
            json!({"description":"A different local meaning"})
        };
        batch.body = wire.to_string();
    }
    let packed = request_packing::pack(batches.clone()).unwrap();
    assert_eq!(packed.len(), 1);
    let packet: serde_json::Value = serde_json::from_str(&packed[0].body).unwrap();
    assert!(packet["state"]["types"]["values"]
        .get("matrix/opaque")
        .is_none());
    for original in batches {
        let wire: serde_json::Value = serde_json::from_str(&original.body).unwrap();
        let key = original.bindings.keys().next().unwrap();
        assert_eq!(reconstruct(&packet, key, &wire), wire["state"]);
    }
}

#[test]
fn byte_limit_and_no_savings_keep_singleton_bytes_and_complete_answers() {
    let (mut retrieval, _, local) = locals();
    for description in ["x".repeat(9000), "\\".repeat(500)] {
        let mut batches = local[..2].to_vec();
        for batch in &mut batches {
            let mut wire: serde_json::Value = serde_json::from_str(&batch.body).unwrap();
            let key = wire["state"]["capabilities"]
                .as_object()
                .unwrap()
                .keys()
                .next()
                .unwrap()
                .clone();
            wire["state"]["capabilities"][key]["declaration"]["description"] = json!(description);
            batch.body = wire.to_string();
            assert!(batch.body.len() < witnesses::MAX_BYTES);
        }
        if description.starts_with('\\') {
            assert!(batches.iter().map(|b| b.body.len()).sum::<usize>() < witnesses::MAX_BYTES / 2);
        }
        let packed = request_packing::pack(batches.clone()).unwrap();
        assert_eq!(packed.len(), 2);
        for (before, after) in batches.iter().zip(&packed) {
            assert_eq!(before.body, after.body);
        }
        let ids: BTreeSet<_> = packed
            .iter()
            .flat_map(|b| b.bindings.values().cloned())
            .collect();
        retrieval.candidates.retain(|c| ids.contains(&c.id));
        let answers = packed
            .iter()
            .flat_map(|b| decode_batch(b, &response(b, &[MatchChoice::Uncertain])).unwrap())
            .collect();
        assert_eq!(
            finish_selection(answers, &retrieval, &packed)
                .unwrap()
                .matches
                .len(),
            2
        );
    }
}
