use super::*;

#[test]
fn response_schema_is_identical_across_page_sizes_and_intents() {
    let (intent, mut retrieval) = fixture();
    let original = retrieval.candidates[0].clone();
    let mut schema = None;
    for count in [1, 2, 7, 17, 128] {
        retrieval.candidates = (0..count)
            .map(|i| {
                let mut c = original.clone();
                c.id = format!("candidate-{i}");
                c.reference.capability = format!("inspect-{i}");
                c
            })
            .collect();
        for provenance in [
            &intent,
            &intent
                .derived("Inspect a different selection".into())
                .unwrap(),
        ] {
            for batch in issue_batches(RELEVANCE_MODEL, provenance, &retrieval).unwrap() {
                let body: serde_json::Value = serde_json::from_str(batch.body()).unwrap();
                let current = body["response_format"].clone();
                assert_eq!(
                    current["json_schema"]["schema"]["properties"]["answers"]["type"],
                    "array"
                );
                if let Some(expected) = &schema {
                    assert_eq!(expected, &current);
                }
                schema = Some(current);
            }
        }
    }
}

#[test]
fn wire_prefix_orders_rules_history_current_intent_then_complete_evidence() {
    let (_, retrieval) = fixture();
    let intent =
        IntentProvenance::from_turns(["Initial constraint".into(), "Revised need".into()]).unwrap();
    let next = intent.derived("Follow-up need".into()).unwrap();
    let a = issue_batches(RELEVANCE_MODEL, &intent, &retrieval)
        .unwrap()
        .remove(0);
    let b = issue_batches(RELEVANCE_MODEL, &next, &retrieval)
        .unwrap()
        .remove(0);
    let body: serde_json::Value = serde_json::from_str(a.body()).unwrap();
    let later: serde_json::Value = serde_json::from_str(b.body()).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[..2], later["messages"].as_array().unwrap()[..2]);
    let rules: serde_json::Value =
        serde_json::from_str(messages[1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        rules,
        json!({"criteria": CRITERIA.into_iter().collect::<BTreeMap<_,_>>(), "intent_interpretation": QUESTION_FORMAT, "rule": RUBRIC})
    );
    let history = messages[2]["content"].as_str().unwrap();
    assert_eq!(
        history,
        r#"{"intent_turns":["Initial constraint","Revised need"],"current_intent":"Revised need"}"#
    );
    let evidence: serde_json::Value =
        serde_json::from_str(messages[3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        evidence["questions"]["q0"]["instructions"]["capability"]["operation"],
        json!(retrieval.candidates[0].document.operation)
    );
    assert_eq!(messages[3], later["messages"][3]);
    assert_ne!(a.cache_key(), b.cache_key());
}

#[test]
fn stable_array_admission_checks_identity_not_position() {
    let keys = ["q0".to_owned(), "q1".to_owned()];
    let raw = completion(
        r#"{"answers":[{"key":"q1","choice":"uncertain"},{"key":"q0","choice":"relevant"}]}"#
            .into(),
    );
    let decoded = chat::decode(RELEVANCE_MODEL, keys.iter(), &raw).unwrap();
    assert_eq!(decoded["q0"], MatchChoice::Relevant);
    assert_eq!(decoded["q1"], MatchChoice::Uncertain);
    for content in [
        r#"{"answers":[]}"#,
        r#"{"answers":[{"key":"q0","choice":"relevant"}]}"#,
        r#"{"answers":[{"key":"q0","choice":"relevant"},{"key":"q0","choice":"uncertain"}]}"#,
        r#"{"answers":[{"key":"q0","choice":"relevant"},{"key":"foreign","choice":"uncertain"}]}"#,
        r#"{"answers":[{"key":"q0","choice":"relevant"},{"key":"q1","choice":"uncertain"},{"key":"extra","choice":"unrelated"}]}"#,
        r#"{"answers":[{"key":"q0","key":"q1","choice":"relevant"}]}"#,
        r#"{"answers":[{"key":"q0","choice":"relevant","choice":"uncertain"}]}"#,
        r#"{"answers":[{"key":"q0","choice":"required_source"}]}"#,
        r#"{"answers":[{"key":"q0","choice":"relevant","confidence":1}]}"#,
        r#"{"answers":[{"key":0,"choice":"relevant"}]}"#,
        r#"{"answers":[{"choice":"relevant"}]}"#,
        r#"{"answers":[],"extra":true}"#,
        r#"{"answers":[],"answers":[]}"#,
        r#"{"answers":{"q0":{"choice":"relevant"}}}"#,
    ] {
        assert!(
            chat::decode(RELEVANCE_MODEL, keys.iter(), &completion(content.into())).is_err(),
            "{content}"
        );
    }
}
