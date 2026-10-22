use super::*;

// Compare the actual wire substring, not Value equality (which ignores map order).
fn wire_state(batch: &IssuedBatch) -> &str {
    batch
        .body()
        .split_once(",\"state\":")
        .unwrap()
        .1
        .split_once(",\"questions\":")
        .unwrap()
        .0
}

#[test]
fn fixed_state_and_question_identity_survive_all_page_sizes() {
    let (intent, mut retrieval) = fixture();
    let template = retrieval.candidates[0].clone();
    retrieval.candidates = (0..128)
        .map(|i| {
            let mut c = template.clone();
            c.id = format!("candidate-{i:03}");
            c.reference.capability = format!("action-{i:03}");
            c.document.collection.meaning = "Abstract observations and transformations.".into();
            c.document.operation.contract =
                format!("Action {i}: requires prepared input; preserves other records.");
            c
        })
        .collect();
    let issued = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    assert_eq!(
        issued.len(),
        1,
        "ordinary 128-candidate evidence fits one page"
    );
    let parent = &issued[0];
    assert_eq!(parent.bindings.len(), 128);
    let original: serde_json::Value = serde_json::from_str(parent.body()).unwrap();
    for limit in [128, 64, 32, 1] {
        let mut pending = issued.clone();
        let mut leaves = Vec::new();
        while let Some(page) = pending.pop() {
            assert_eq!(wire_state(&page), wire_state(parent));
            assert_canonical_wire_order(&page);
            let body: serde_json::Value = serde_json::from_str(page.body()).unwrap();
            for key in page.bindings.keys() {
                assert_eq!(page.bindings[key], parent.bindings[key]);
                assert_eq!(body["questions"][key], original["questions"][key]);
            }
            if page.bindings.len() > limit {
                pending.extend(repartition(&page, &intent).unwrap());
            } else {
                leaves.push(page);
            }
        }
        let mut seen = BTreeSet::new();
        let mut answers = Vec::new();
        for page in &leaves {
            let choices: Vec<_> = page
                .bindings
                .values()
                .map(|id| {
                    assert!(seen.insert(id.clone()));
                    match id
                        .trim_start_matches("candidate-")
                        .parse::<usize>()
                        .unwrap()
                        % 3
                    {
                        0 => MatchChoice::Relevant,
                        1 => MatchChoice::Uncertain,
                        _ => MatchChoice::Unrelated,
                    }
                })
                .collect();
            answers.extend(decode_batch(page, &response(page, &choices)).unwrap());
        }
        assert_eq!(seen.len(), 128);
        let receipt = finish_selection(answers, &retrieval, &leaves).unwrap();
        let expected: Vec<_> = retrieval
            .candidates
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 3 != 2)
            .map(|(_, c)| c.reference.clone())
            .collect();
        assert_eq!(receipt.selected(&retrieval), expected);
    }
}

#[test]
fn byte_packing_preserves_identical_state_and_complete_contracts() {
    let (intent, retrieval) = scheduling::many_cards(4);
    let pages = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    assert_eq!(pages.len(), 4);
    for page in &pages {
        assert_eq!(wire_state(page), wire_state(&pages[0]));
        let body: serde_json::Value = serde_json::from_str(page.body()).unwrap();
        assert_eq!(body["state"]["rule"], RUBRIC);
        for (i, candidate) in retrieval.candidates.iter().enumerate() {
            assert_eq!(
                body["state"]["capabilities"][format!("c{i}")]["capability"]["operation"]
                    ["contract"],
                candidate.document.operation.contract
            );
        }
    }
}

fn assert_canonical_wire_order(batch: &IssuedBatch) {
    let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
    let mut expected: Vec<_> = batch
        .evidence
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let key = format!("c{i}");
            let mut card = body["state"]["capabilities"][&key].clone();
            card.sort_all_objects();
            (content_hash(&serde_json::to_vec(&card).unwrap()), key)
        })
        .collect();
    expected.sort();
    let positions: Vec<_> = expected
        .iter()
        .map(|(_, key)| batch.body().find(&format!("\"{key}\":")).unwrap())
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "wire map order was lost"
    );
}

#[test]
fn canonical_order_survives_codec_and_subdivision_with_retained_uncertainty() {
    let (intent, mut retrieval) = scheduling::many_cards(4);
    for (i, candidate) in retrieval.candidates.iter_mut().enumerate() {
        candidate.document.operation.contract =
            format!("Distinct abstract action {i}; preserves other records.");
    }
    let before = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    retrieval.candidates.reverse();
    let after = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    assert_eq!(before.len(), after.len());
    let mut answers = Vec::new();
    let mut retained = BTreeSet::new();
    for (a, b) in before.iter().zip(&after) {
        assert_eq!(a.body(), b.body());
        assert_eq!(a.cache_key(), b.cache_key());
        assert_canonical_wire_order(a);
        let choices: Vec<_> = a
            .bindings
            .values()
            .enumerate()
            .map(|(i, id)| {
                let choice = if i % 3 == 0 {
                    MatchChoice::Relevant
                } else if i % 3 == 1 {
                    MatchChoice::Uncertain
                } else {
                    MatchChoice::Unrelated
                };
                if choice != MatchChoice::Unrelated {
                    retained.insert(id.clone());
                }
                choice
            })
            .collect();
        answers.extend(decode_batch(a, &response(a, &choices)).unwrap());
        if a.questions.len() > 1 {
            for child in repartition(a, &intent).unwrap() {
                assert_canonical_wire_order(&child);
                let parent: serde_json::Value = serde_json::from_str(a.body()).unwrap();
                let split: serde_json::Value = serde_json::from_str(child.body()).unwrap();
                assert_eq!(parent["state"], split["state"]);
            }
        }
    }
    let receipt = finish_selection(answers, &retrieval, &before).unwrap();
    let expected: Vec<_> = retrieval
        .candidates
        .iter()
        .filter(|c| retained.contains(&c.id))
        .map(|c| c.reference.clone())
        .collect();
    assert_eq!(receipt.selected(&retrieval), expected);
}

#[test]
#[ignore = "requires explicitly captured isolated probe responses"]
fn replay_shared_evidence_probe_responses() {
    let root = std::path::PathBuf::from(std::env::var("PLASM_PROBE_RESPONSES").unwrap());
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("MANIFEST.json")).unwrap()).unwrap();
    let mut count = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("response-")
        {
            continue;
        }
        let response: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let case = manifest["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == response["case"])
            .unwrap();
        let keys: Vec<String> = case["labels"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let decoded = decisions::decode(
            RELEVANCE_MODEL,
            keys.iter(),
            response["raw"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), response["answers"]);
        count += 1;
    }
    assert_eq!(count, manifest["schedule"].as_array().unwrap().len());
}

/// Exports the production encoder for isolated, explicitly authorized probes.
#[test]
#[ignore = "requires explicit offline fixture input and export paths"]
fn export_shared_evidence_probe_requests() {
    #[derive(Deserialize)]
    struct Case {
        id: String,
        intent: String,
        contracts: Vec<String>,
    }
    let input = std::env::var("PLASM_PROBE_INPUT").unwrap();
    let output = std::env::var("PLASM_PROBE_OUTPUT").unwrap();
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    let (_, template) = fixture();
    let mut result = Vec::new();
    for case in cases {
        let intent = IntentProvenance::from_turns([case.intent]).unwrap();
        let mut retrieval = fixture().1;
        retrieval.candidates = case
            .contracts
            .iter()
            .enumerate()
            .map(|(i, contract)| {
                let mut c = template.candidates[0].clone();
                c.id = format!("candidate-{i:03}");
                c.reference.catalog = case.id.clone();
                c.reference.capability = format!("operation-{i:03}");
                c.document.entity = "AbstractRecord".into();
                c.document.operation.kind = plasm_core::schema::CapabilityKind::Action;
                c.document.operation.receiver = None;
                c.document.operation.contract = contract.clone();
                c.document.collection.meaning =
                    "Records and transformations described by the operation contracts.".into();
                c
            })
            .collect();
        for batch in issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap() {
            result.push(json!({"case":case.id,"body":batch.body(),"bindings":batch.bindings}));
        }
    }
    std::fs::write(output, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}

#[test]
fn full_shared_evidence_survives_large_pages_and_repartition_once_per_request() {
    let (intent, mut retrieval) = fixture();
    let template = retrieval.candidates[0].clone();
    for count in [1, 9, 32, 64, 128] {
        retrieval.candidates = (0..count).map(|i| {
            let mut c=template.clone();
            c.id=format!("candidate-{i}");
            c.reference.capability=format!("action-{i:03}");
            c.document.operation.contract=format!("Unique contract {i:03}; requires prepared input; changes all configured members.");
            c
        }).collect();
        let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
        let mut observed = BTreeSet::new();
        for batch in &batches {
            assert_canonical_wire_order(batch);
            let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
            assert_eq!(
                body["state"]["capabilities"].as_object().unwrap().len(),
                count
            );
            assert!(batch.bindings.len() <= MAX_PAGE_QUESTIONS);
            for (key, id) in &batch.bindings {
                let index = batch.evidence.iter().position(|q| &q.id == id).unwrap();
                assert!(body["questions"][key]["instructions"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("`capabilities.c{index}`")));
                assert_eq!(
                    body["state"]["capabilities"][format!("c{index}")]["capability"]["reference"],
                    json!(batch.evidence[index].reference)
                );
            }
            for c in &retrieval.candidates {
                assert_eq!(
                    batch.body().matches(&c.document.operation.contract).count(),
                    1
                );
            }
            for instruction in body["questions"].as_object().unwrap().values() {
                assert_eq!(instruction["type"], "choice");
                assert!(!instruction["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("Unique contract"));
            }
            observed.extend(batch.bindings.values().cloned());
            if batch.questions.len() > 1 {
                for child in repartition(batch, &intent).unwrap() {
                    let split: serde_json::Value = serde_json::from_str(child.body()).unwrap();
                    assert_eq!(split["state"], body["state"]);
                }
            }
        }
        assert_eq!(observed.len(), count);
    }
}

#[test]
fn changed_shared_contract_and_intent_invalidate_every_page_identity() {
    let (intent, mut retrieval) = scheduling::many_cards(4);
    let before = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    retrieval.candidates[3]
        .document
        .operation
        .contract
        .push_str(" Additional limit.");
    let changed = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    for (a, b) in before.iter().zip(changed.iter()) {
        assert_ne!(a.cache_key(), b.cache_key());
    }
    let revised = intent.derived("A revised outcome".into()).unwrap();
    let next = issue_batches(RELEVANCE_MODEL, &revised, &retrieval).unwrap();
    for (a, b) in changed.iter().zip(next.iter()) {
        assert_ne!(a.cache_key(), b.cache_key());
    }
}
