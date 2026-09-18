use super::*;
use serde_json::json;
fn workflow() -> WorkflowIntent {
    WorkflowIntent::open(
        IntentScope::Workflow,
        "request-1".into(),
        "Update eligible records only".into(),
    )
    .unwrap()
}
fn draft() -> InterpretationDraft {
    InterpretationDraft {
        conditionals: vec![],
        dispositions: BTreeMap::new(),
        requirements: vec![RequirementDraft {
            uncertainty: None,
            kind: RequirementKind::InformationNeed,
            statement: "Determine eligible records".into(),
            source_turn_ids: vec!["u0".into()],
        }],
        no_requirements_reason: None,
    }
}
fn envelope(v: serde_json::Value) -> String {
    json!({"choices":[{"finish_reason":"stop","message":{"content":v.to_string()}}]}).to_string()
}
#[test]
fn revisions_are_idempotent_and_stale_interpretations_cannot_be_used() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    let old = w.clone();
    assert_eq!(
        w.append(0, "request-1".into(), "Update eligible records only".into())
            .unwrap(),
        1
    );
    assert_eq!(w, old);
    assert!(w
        .append(1, "request-1".into(), "Broaden scope".into())
        .is_err());
    assert!(w
        .append(0, "request-2".into(), "Include archived".into())
        .is_err());
    w.append(1, "request-2".into(), "Include archived".into())
        .unwrap();
    assert!(w.current().is_err());
    assert!(contract::retrieval_queries(&w).is_err());
    let restored: WorkflowIntent =
        serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
    assert_eq!(restored, w);
    assert!(restored.current().is_err());
}
#[test]
fn retained_ids_are_host_owned_and_supersession_is_explicit() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    let retained = InterpretationDraft {
        conditionals: vec![],
        dispositions: BTreeMap::from([("r0".into(), RequirementDisposition::Retain {})]),
        requirements: vec![],
        no_requirements_reason: None,
    };
    w.interpret(1, 1, retained).unwrap();
    assert_eq!(w.current().unwrap().requirements[0].id, "r0");
    assert!(w.interpret(1, 1, draft()).is_err());
    let mut invalid = draft();
    invalid
        .dispositions
        .insert("r999".into(), RequirementDisposition::Retain {});
    let before = w.clone();
    assert!(w.interpret(1, 2, invalid).is_err());
    assert_eq!(w, before);
    let mut replacement = draft();
    replacement.dispositions.insert(
        "r0".into(),
        RequirementDisposition::Retire {
            reason: "Reinterpret current request".into(),
            source_turn_ids: vec!["u0".into()],
        },
    );
    w.interpret(1, 2, replacement).unwrap();
    assert_eq!(w.current().unwrap().requirements[0].id, "r1");
    assert_eq!(w.current().unwrap().retired[0].requirement.id, "r0");
}
#[test]
fn assessment_cannot_drop_requirements_or_invent_capability_ids() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    w.set_links(1, 1, vec![]).unwrap();
    let r=serde_json::from_value(json!({"generation":"fixture","candidates":[],"lexical_count":0,"vector_count":0,"lexical_truncated":false,"vector_truncated":false,"fusion_truncated":0,"relation_truncated":0})).unwrap();
    assert!(contract::decode_assessment(
        &w,
        &r,
        &contract::assessment_request("fixture-model", &w, &r, "").unwrap(),
        &envelope(json!({"requirement_coverage":[]}))
    )
    .is_err());
    let missing = json!({"requirement":"r0","assessment":{"status":"unresolved","capability_ids":[],"explanation":"No eligibility source"}});
    assert!(contract::decode_assessment(
        &w,
        &r,
        &contract::assessment_request("fixture-model", &w, &r, "").unwrap(),
        &envelope(json!({"requirement_coverage":[missing.clone()]}))
    )
    .is_ok());
    assert!(contract::decode_assessment(
        &w,
        &r,
        &contract::assessment_request("fixture-model", &w, &r, "").unwrap(),
        &envelope(json!({"requirement_coverage":[missing.clone(),missing]}))
    )
    .is_err());
    assert!(contract::decode_assessment(&w,&r,&contract::assessment_request("fixture-model", &w, &r, "").unwrap(),&envelope(json!({"requirement_coverage":[{"requirement":"r0","assessment":{"status":"supported","capability_ids":["c0"],"explanation":"Provides the records"}}]}))).is_err());
}
#[test]
fn interpretation_sources_and_empty_claims_are_checked() {
    let mut w = workflow();
    let mut d = draft();
    d.requirements[0].source_turn_ids = vec!["u1".into()];
    assert!(w.interpret(1, 0, d).is_err());
    assert!(staged::decode_commit(
        &w,
        &staged::Questions::default(),
        &staged::commit_request("fixture-model", &w, &staged::Questions::default()).unwrap(),
        &envelope(json!({"retained_ids":[],"requirements":[],"no_requirements_reason":null}))
    )
    .is_err());
    assert!(
        staged::commit_request("fixture-model", &w, &staged::Questions::default())
            .unwrap()
            .body
            .contains("Update eligible records only")
    );
}
proptest::proptest! {
 #[test]
 fn retry_and_restart_preserve_authoritative_turns(text in "[a-zA-Z ]{1,100}"){
  proptest::prop_assume!(!text.trim().is_empty());
  let mut w=WorkflowIntent::open(IntentScope::Workflow,"first".into(),text.clone()).unwrap();
  w=serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();w.append(0,"first".into(),text.clone()).unwrap();
  let outer:serde_json::Value=serde_json::from_str(&staged::commit_request("fixture-model",&w,&staged::Questions::default()).unwrap().body).unwrap();
  let input:serde_json::Value=serde_json::from_str(outer["messages"][1]["content"].as_str().unwrap()).unwrap();
  proptest::prop_assert_eq!(input["workflow"]["turns"][0]["text"].as_str(),Some(text.as_str()));proptest::prop_assert_eq!(w.revision(),1);
 }
}

#[test]
fn outstanding_responses_cannot_cross_revision_or_interpretation_changes() {
    let mut w = workflow();
    let issued =
        staged::commit_request("fixture-model", &w, &staged::Questions::default()).unwrap();
    let raw = envelope(
        json!({"dispositions":{}, "settled_requirements":[{"kind":"information_need","statement":"Determine eligible records","source_turn_ids":["u0"]}],"existing_questions":{},"new_questions":[],"no_requirements_reason":null}),
    );
    let (interpreted, _) =
        staged::decode_commit(&w, &staged::Questions::default(), &issued, &raw).unwrap();
    assert!(
        staged::decode_commit(&interpreted, &staged::Questions::default(), &issued, &raw).is_err()
    );
    w.append(1, "second".into(), "Inspect only".into()).unwrap();
    assert!(staged::decode_commit(&w, &staged::Questions::default(), &issued, &raw).is_err());
}
#[test]
fn restored_state_must_obey_interpretation_invariants() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    for (field, value) in [("no_requirements_reason", json!(""))] {
        let mut serialized = serde_json::to_value(&w).unwrap();
        serialized["interpretation"][field] = value;
        let invalid: WorkflowIntent = serde_json::from_value(serialized).unwrap();
        assert!(invalid.current().is_err());
    }
}

#[test]
fn omission_is_not_retirement_and_rejected_revision_is_atomic() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    w.append(1, "second".into(), "Inspect only".into()).unwrap();
    let before = w.clone();
    assert!(w.interpret(2, 1, draft()).is_err());
    assert_eq!(w, before);
    let mut replacement = draft();
    replacement.requirements[0].source_turn_ids = vec!["u1".into()];
    replacement.dispositions.insert(
        "r0".into(),
        RequirementDisposition::Retire {
            reason: "User replaced the request".into(),
            source_turn_ids: vec!["u1".into()],
        },
    );
    w.interpret(2, 1, replacement).unwrap();
    let restored: WorkflowIntent =
        serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
    assert_eq!(
        restored.current().unwrap().retired[0].requirement.statement,
        "Determine eligible records"
    );
    assert!(contract::retrieval_queries(&restored).is_err());
}

#[test]
fn queued_turns_are_interpreted_in_order_without_future_source_leakage() {
    let mut w = workflow();
    w.append(1, "second".into(), "Do not update anything".into())
        .unwrap();
    let issued = staged::commit_request("fixture", &w, &staged::Questions::default()).unwrap();
    let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
    let input: serde_json::Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["workflow"]["turns"].as_array().unwrap().len(), 1);
    assert!(input["discovery_focus"].is_null());
    let mut future = draft();
    future.requirements[0].source_turn_ids = vec!["u1".into()];
    assert!(w.interpret(2, 0, future).is_err());
    w.interpret(2, 0, draft()).unwrap();
    assert_eq!(w.interpretation().unwrap().intent_revision, 1);
    assert!(w.current().is_err());
    assert!(contract::retrieval_queries(&w).is_err());
}

fn linked_fixture() -> WorkflowIntent {
    let mut w = workflow();
    let mut d = draft();
    d.requirements[0].statement = "Compare stock".into();
    d.requirements.push(RequirementDraft {
        uncertainty: None,
        kind: RequirementKind::Effect,
        statement: "Reorder stock".into(),
        source_turn_ids: vec!["u0".into()],
    });
    d.requirements.push(RequirementDraft {
        uncertainty: None,
        kind: RequirementKind::SelectionConstraint,
        statement: "Exclude restricted items".into(),
        source_turn_ids: vec!["u0".into()],
    });
    w.interpret(1, 0, d).unwrap();
    w
}
#[test]
fn links_gate_retrieval_and_preserve_operation_specific_scope() {
    use links::RequirementLink::*;
    let mut w = linked_fixture();
    assert!(contract::retrieval_queries(&w).is_err());
    assert!(w.set_links(1, 1, vec![]).is_err());
    w.set_links(
        1,
        1,
        vec![
            Constrains {
                source: "r2".into(),
                target: "r1".into(),
            },
            Before {
                source: "r0".into(),
                target: "r1".into(),
            },
        ],
    )
    .unwrap();
    let q = contract::retrieval_queries(&w).unwrap();
    assert!(!q[0].1.contains("Exclude restricted"));
    assert!(q[1]
        .1
        .contains("Selection constraint: Exclude restricted items"));
    assert!(q[1].1.contains("After: Compare stock"));
    let response = contract::linkage_request("fixture", &linked_fixture()).unwrap();
    assert!(contract::decode_linkage(&w, &response, &envelope(json!({"links":[]}))).is_err());
}
#[test]
fn link_validation_rejects_cycles_bad_types_and_unknown_ids() {
    use links::RequirementLink::*;
    let w = linked_fixture();
    let i = w.current().unwrap();
    let base = Constrains {
        source: "r2".into(),
        target: "r1".into(),
    };
    assert!(links::validate(
        i,
        &[
            base.clone(),
            Before {
                source: "r0".into(),
                target: "r1".into()
            },
            Before {
                source: "r1".into(),
                target: "r0".into()
            }
        ]
    )
    .is_err());
    assert!(links::validate(
        i,
        &[Constrains {
            source: "r0".into(),
            target: "r1".into()
        }]
    )
    .is_err());
    assert!(links::validate(
        i,
        &[
            base,
            Before {
                source: "r99".into(),
                target: "r0".into()
            }
        ]
    )
    .is_err());
}
proptest::proptest! {
 #[test]
 fn generated_order_cycles_never_publish(n in 2usize..32) {
    use links::RequirementLink;
    let mut w=workflow();let mut d=draft();
    d.requirements=(0..n).map(|i|RequirementDraft{uncertainty:None,kind:RequirementKind::Effect,statement:format!("Operation {i}"),source_turn_ids:vec!["u0".into()]}).collect();
    w.interpret(1,0,d).unwrap();
    let chain:Vec<_>=(0..n-1).map(|i|RequirementLink::Before{source:format!("r{i}"),target:format!("r{}",i+1)}).collect();
    proptest::prop_assert!(links::validate(w.current().unwrap(),&chain).is_ok());
    let mut cycle=chain;cycle.push(RequirementLink::Before{source:format!("r{}",n-1),target:"r0".into()});
    let before=w.clone();proptest::prop_assert!(w.set_links(1,1,cycle).is_err());proptest::prop_assert_eq!(w,before);
 }
}

#[test]
fn opposing_branches_cannot_condition_the_same_operation() {
    use links::RequirementLink::*;
    let mut w = workflow();
    let mut d = draft();
    d.requirements[0].kind = RequirementKind::Condition;
    d.requirements.push(RequirementDraft {
        uncertainty: None,
        kind: RequirementKind::Effect,
        statement: "Perform an action".into(),
        source_turn_ids: vec!["u0".into()],
    });
    d.conditionals = vec![conditional::Draft {
        predicate: 0,
        when_true: vec![1],
        when_false: vec![],
    }];
    w.interpret(1, 0, d).unwrap();
    let before = w.clone();
    assert!(w
        .set_links(
            1,
            1,
            vec![
                WhenTrue {
                    source: "r0".into(),
                    target: "r1".into()
                },
                WhenFalse {
                    source: "r0".into(),
                    target: "r1".into()
                }
            ]
        )
        .is_err());
    assert_eq!(w, before);
}

#[test]
fn restored_history_cannot_lose_retired_ids_or_reverse_version_order() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    w.append(1, "second".into(), "Replace the requirement".into())
        .unwrap();
    let mut d = draft();
    d.dispositions.insert(
        "r0".into(),
        RequirementDisposition::Retire {
            reason: "User revision".into(),
            source_turn_ids: vec!["u1".into()],
        },
    );
    w.interpret(2, 1, d).unwrap();
    for (field, value) in [("retired", json!([])), ("version", json!(1))] {
        let mut state = serde_json::to_value(&w).unwrap();
        state["interpretation"][field] = value;
        let restored: WorkflowIntent = serde_json::from_value(state).unwrap();
        assert!(restored.validate().is_err());
    }
}

#[test]
fn singleton_linkage_offers_only_empty_selection() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    let issued = contract::linkage_request("fixture", &w).unwrap();
    let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
    let input: serde_json::Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["link_choices"], json!([]));
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["properties"]["link_ids"]["maxItems"],
        json!(0)
    );
    let linked = contract::decode_linkage(&w, &issued, &envelope(json!({"link_ids":[]}))).unwrap();
    assert_eq!(contract::retrieval_queries(&linked).unwrap().len(), 1);
    assert!(contract::decode_linkage(&w, &issued, &envelope(json!({"link_ids":["l0"]}))).is_err());
    assert!(contract::decode_linkage(
        &w,
        &issued,
        &envelope(json!({"links":[{"kind":"constrains","source":"r0","target":"r0"}]}))
    )
    .is_err());
}
#[test]
fn independent_prohibition_is_never_offered_as_an_operation() {
    let mut w = linked_fixture();
    let mut revised = draft();
    revised.dispositions = ["r0", "r1", "r2"]
        .into_iter()
        .map(|id| (id.into(), RequirementDisposition::Retain {}))
        .collect();
    revised.requirements = vec![RequirementDraft {
        uncertainty: None,
        kind: RequirementKind::Prohibition,
        statement: "Do not alter protected records".into(),
        source_turn_ids: vec!["u0".into()],
    }];
    w.interpret(1, 1, revised).unwrap();
    let choices = links::choices(w.current().unwrap());
    assert!(choices.iter().all(|c| {
        let (s, t) = c.link.endpoints();
        s != "r3" && t != "r3"
    }));
    let choice=choices.iter().find(|c|matches!(&c.link,links::RequirementLink::Constrains{source,target} if source=="r2" && target=="r1")).unwrap();
    let issued = contract::linkage_request("fixture", &w).unwrap();
    let linked =
        contract::decode_linkage(&w, &issued, &envelope(json!({"link_ids":[choice.id]}))).unwrap();
    let q = contract::retrieval_queries(&linked).unwrap();
    assert_eq!(
        q.iter().find(|(id, _)| id == "r3").unwrap().1,
        "Do not alter protected records"
    );
    assert!(contract::decode_linkage(
        &w,
        &issued,
        &envelope(json!({"link_ids":[choice.id,choice.id]}))
    )
    .is_err());
}
proptest::proptest! {
 #[test]
 fn offered_edges_are_typed_nonself_and_aliases_ignore_input_order(kinds in proptest::collection::vec(0u8..5,1..20)) {
    let mut w=workflow();let mut d=draft();
    d.requirements=kinds.iter().enumerate().map(|(i,k)|RequirementDraft{uncertainty:None,kind:match k {0=>RequirementKind::Effect,1=>RequirementKind::InformationNeed,2=>RequirementKind::SelectionConstraint,3=>RequirementKind::Condition,_=>RequirementKind::Prohibition},statement:format!("Requirement {i}"),source_turn_ids:vec!["u0".into()]}).collect();
    if kinds.contains(&3) { proptest::prop_assert!(w.interpret(1,0,d).is_err()); return Ok(()); }
    w.interpret(1,0,d).unwrap();
    let original=w.current().unwrap();let mut reordered=original.clone();reordered.requirements.reverse();
    let choices=links::choices(original);
    proptest::prop_assert_eq!(serde_json::to_value(&choices).unwrap(),serde_json::to_value(links::choices(&reordered)).unwrap());
    for (n,c) in choices.iter().enumerate() {
        let(s,t)=c.link.endpoints();proptest::prop_assert_ne!(s,t);proptest::prop_assert_eq!(&c.id,&format!("l{n}"));
        let mut pair=original.clone();pair.requirements.retain(|r|r.id==s||r.id==t);
        proptest::prop_assert!(links::validate(&pair,std::slice::from_ref(&c.link)).is_ok());
    }
 }
}

#[test]
fn disposition_schema_requires_each_prior_id_and_decoder_rejects_omission() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    let issued = staged::commit_request("fixture", &w, &staged::Questions::default()).unwrap();
    let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
    let schema = &body["response_format"]["json_schema"]["schema"]["properties"]["dispositions"];
    assert_eq!(schema["required"], json!(["r0"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    let value = json!({"dispositions":{"r0":{"action":"retain"}},"settled_requirements":[],"existing_questions":{},"new_questions":[],"no_requirements_reason":null});
    assert!(staged::decode_commit(
        &w,
        &staged::Questions::default(),
        &issued,
        &envelope(value.clone())
    )
    .is_ok());
    let mut missing = value;
    missing["dispositions"] = json!({});
    assert!(staged::decode_commit(
        &w,
        &staged::Questions::default(),
        &issued,
        &envelope(missing)
    )
    .is_err());
}
#[test]
fn disposition_decoder_rejects_duplicate_keys_and_mixed_payloads() {
    let duplicate = r#"{"dispositions":{"r0":{"action":"retain"},"r0":{"action":"retire","reason":"changed","source_turn_ids":["u0"]}},"requirements":[],"no_requirements_reason":null}"#;
    assert!(serde_json::from_str::<InterpretationDraft>(duplicate).is_err());
    let mixed = json!({"dispositions":{"r0":{"action":"retain","reason":"changed","source_turn_ids":["u0"]}},"requirements":[],"no_requirements_reason":null});
    assert!(serde_json::from_value::<InterpretationDraft>(mixed).is_err());
}

#[test]
fn linking_request_rejects_unattachable_conditions_before_dispatch() {
    let mut w = workflow();
    let mut d = draft();
    d.requirements[0].kind = RequirementKind::Condition;
    let before = w.clone();
    let error = w.interpret(1, 0, d).unwrap_err();
    assert!(error.to_string().contains("ownership manifest"));
    assert_eq!(w, before);
}
proptest::proptest! {
 #[test]
 fn feasible_link_requests_have_a_valid_attachment_witness(kinds in proptest::collection::vec(0u8..5,0..30)) {
    let mut w=workflow();let mut d=draft();
    d.requirements=kinds.iter().enumerate().map(|(i,k)|RequirementDraft{uncertainty:None,kind:match k {0=>RequirementKind::Effect,1=>RequirementKind::InformationNeed,2=>RequirementKind::SelectionConstraint,3=>RequirementKind::Condition,_=>RequirementKind::Prohibition},statement:format!("Requirement {i}"),source_turn_ids:vec!["u0".into()]}).collect();
    d.no_requirements_reason=if kinds.is_empty(){Some("No requested work".into())}else{None};
    if kinds.contains(&3) { proptest::prop_assert!(w.interpret(1,0,d).is_err()); return Ok(()); }
    w.interpret(1,0,d).unwrap();
    let current=w.current().unwrap();let choices=links::choices(current);
    let feasible=links::ensure_feasible(current,&choices).is_ok();
    let expected= !kinds.iter().any(|k|*k==2||*k==3) || kinds.iter().any(|k|*k==0||*k==1);
    proptest::prop_assert_eq!(feasible,expected);
    if feasible {
        let witness:Vec<_>=current.requirements.iter().filter(|r|matches!(r.kind,RequirementKind::Condition|RequirementKind::SelectionConstraint))
            .map(|r|choices.iter().find(|c|c.link.endpoints().0==r.id).unwrap().link.clone()).collect();
        proptest::prop_assert!(links::validate(current,&witness).is_ok());
        proptest::prop_assert!(contract::linkage_request("fixture",&w).is_ok());
    } else { proptest::prop_assert!(contract::linkage_request("fixture",&w).is_err()); }
 }
}

#[test]
fn audit_requires_disposition_and_preserves_bound_state() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    let issued = contract::audit_request("fixture", &w).unwrap();
    let before = w.clone();
    let finding = json!({"disposition":"caution","kind":"omitted","requirement_ids":["r0"],"explanation":"A possible scope concern"});
    let decoded = contract::decode_audit(
        &w,
        &issued,
        &envelope(json!({"findings":[finding.clone()]})),
    )
    .unwrap();
    assert!(matches!(
        decoded.findings[0].disposition,
        contract::AuditDisposition::Caution
    ));
    let mut missing = finding.clone();
    missing.as_object_mut().unwrap().remove("disposition");
    assert!(contract::decode_audit(&w, &issued, &envelope(json!({"findings":[missing]}))).is_err());
    let mut unknown = finding.clone();
    unknown["requirement_ids"] = json!(["r99"]);
    assert!(contract::decode_audit(&w, &issued, &envelope(json!({"findings":[unknown]}))).is_err());
    let mut actionable = finding;
    actionable["disposition"] = json!("actionable");
    actionable["requirement_ids"] = json!([]);
    assert!(
        contract::decode_audit(&w, &issued, &envelope(json!({"findings":[actionable]}))).is_ok()
    );
    assert_eq!(w, before);
    w.append(1, "next".into(), "Inspect only".into()).unwrap();
    assert!(contract::decode_audit(&w, &issued, &envelope(json!({"findings":[]}))).is_err());
}

fn uncertainty() -> decisions::DecisionDraft {
    decisions::DecisionDraft {
        question: "Which scope does the qualification apply to?".into(),
        alternatives: vec!["Whole requested set".into(), "Newly added subset".into()],
    }
}
#[test]
fn unresolved_decisions_survive_retention_and_retirement_without_becoming_settled() {
    let mut w = workflow();
    let mut d = draft();
    d.requirements[0].uncertainty = Some(uncertainty());
    w.interpret(1, 0, d).unwrap();
    w.set_links(1, 1, vec![]).unwrap();
    assert!(w.settled().is_err());
    assert_eq!(
        w.current().unwrap().requirements[0]
            .decision
            .as_ref()
            .unwrap()
            .id,
        "d0"
    );
    let mut retained = draft();
    retained.requirements.clear();
    retained
        .dispositions
        .insert("r0".into(), RequirementDisposition::Retain {});
    w.interpret(1, 1, retained).unwrap();
    w.set_links(1, 2, vec![]).unwrap();
    assert!(w.settled().is_err());
    let restored: WorkflowIntent =
        serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
    assert_eq!(w, restored);
    w.append(
        1,
        "clarify".into(),
        "The qualification covers the whole set".into(),
    )
    .unwrap();
    let mut resolved = draft();
    resolved.requirements[0].source_turn_ids = vec!["u0".into(), "u1".into()];
    resolved.dispositions.insert(
        "r0".into(),
        RequirementDisposition::Retire {
            reason: "Replaced after user clarification".into(),
            source_turn_ids: vec!["u1".into()],
        },
    );
    w.interpret(2, 2, resolved).unwrap();
    w.set_links(2, 3, vec![]).unwrap();
    assert!(w.settled().is_ok());
    assert_eq!(
        w.current().unwrap().retired[0]
            .requirement
            .decision
            .as_ref()
            .unwrap()
            .id,
        "d0"
    );
}
#[test]
fn uncertainty_wire_is_required_and_invalid_decisions_are_atomic() {
    let mut w = workflow();
    let mut raw = serde_json::to_value(draft()).unwrap();
    raw["requirements"][0]
        .as_object_mut()
        .unwrap()
        .remove("uncertainty");
    assert!(serde_json::from_value::<InterpretationDraft>(raw).is_err());
    for alternatives in [
        vec!["single".into()],
        vec!["same".into(), " same ".into()],
        vec!["".into(), "other".into()],
    ] {
        let mut d = draft();
        d.requirements[0].uncertainty = Some(decisions::DecisionDraft {
            question: "Which?".into(),
            alternatives,
        });
        let before = w.clone();
        assert!(w.interpret(1, 0, d).is_err());
        assert_eq!(w, before);
    }
    let mut d = draft();
    d.requirements[0].uncertainty = Some(uncertainty());
    w.interpret(1, 0, d).unwrap();
    let mut raw = serde_json::to_value(&w).unwrap();
    raw["interpretation"]["requirements"][0]["decision"]["id"] = json!("d999");
    let corrupt: WorkflowIntent = serde_json::from_value(raw).unwrap();
    assert!(corrupt.validate().is_err());
}
#[test]
fn capability_assessment_cannot_erase_decision_blockers() {
    let mut w = workflow();
    let mut d = draft();
    d.requirements[0].uncertainty = Some(uncertainty());
    w.interpret(1, 0, d).unwrap();
    w.set_links(1, 1, vec![]).unwrap();
    let receipt=serde_json::from_value(json!({"generation":"fixture","candidates":[],"lexical_count":0,"vector_count":0,"lexical_truncated":false,"vector_truncated":false,"fusion_truncated":0,"relation_truncated":0})).unwrap();
    let issued = contract::assessment_request("fixture", &w, &receipt, "").unwrap();
    let mut answer = json!({"requirement_coverage":[{"requirement":"r0","assessment":{"status":"local","capability_ids":[],"explanation":"Local reasoning"}}]});
    let result =
        contract::decode_assessment(&w, &receipt, &issued, &envelope(answer.clone())).unwrap();
    assert_eq!(result.unresolved_decisions.len(), 1);
    assert!(w.settled().is_err());
    answer["unresolved_decisions"] = json!([]);
    assert!(contract::decode_assessment(&w, &receipt, &issued, &envelope(answer)).is_err());
    let queries = contract::retrieval_queries(&w).unwrap();
    assert!(queries[0].1.contains("PROVISIONAL"));
    assert!(queries[0].1.contains("Whole requested set"));
    assert!(queries[0].1.contains("Newly added subset"));
}
proptest::proptest! {
 #[test]
 fn decision_impact_follows_dependencies_not_array_order(n in 2usize..18, owner in 0usize..18) {
    let owner=owner%n;
    let mut w=workflow();let mut d=draft();
    d.requirements=(0..n).map(|i| RequirementDraft { uncertainty: if i==owner {Some(uncertainty())}else{None},kind:RequirementKind::Effect,statement:format!("Operation {i}"),source_turn_ids:vec!["u0".into()]}).collect();
    w.interpret(1,0,d).unwrap();
    w.set_links(1,1,(0..n-1).map(|i| links::RequirementLink::Before{source:format!("r{i}"),target:format!("r{}",i+1)}).collect()).unwrap();
    let i=w.linked().unwrap();let impact=decisions::impacts(i);
    let expected:BTreeSet<_>=(owner..n).map(|j|format!("r{j}")).collect();
    proptest::prop_assert_eq!(impact[0].requirement_ids.iter().cloned().collect::<BTreeSet<_>>(),expected);
    let mut reversed=i.clone();reversed.requirements.reverse();reversed.links.as_mut().unwrap().reverse();
    proptest::prop_assert_eq!(impact,decisions::impacts(&reversed));
    proptest::prop_assert!(w.settled().is_err());
 }
}

#[test]
fn constraint_decision_impacts_dependents_but_not_unrelated_operations() {
    let mut w = workflow();
    let mut d = draft();
    d.requirements = vec![
        RequirementDraft {
            uncertainty: Some(uncertainty()),
            kind: RequirementKind::SelectionConstraint,
            statement: "Scope remains undecided".into(),
            source_turn_ids: vec!["u0".into()],
        },
        RequirementDraft {
            uncertainty: None,
            kind: RequirementKind::Effect,
            statement: "Process records".into(),
            source_turn_ids: vec!["u0".into()],
        },
        RequirementDraft {
            uncertainty: None,
            kind: RequirementKind::Effect,
            statement: "Report processing results".into(),
            source_turn_ids: vec!["u0".into()],
        },
        RequirementDraft {
            uncertainty: None,
            kind: RequirementKind::Effect,
            statement: "Inspect unrelated service".into(),
            source_turn_ids: vec!["u0".into()],
        },
    ];
    w.interpret(1, 0, d).unwrap();
    w.set_links(
        1,
        1,
        vec![
            links::RequirementLink::Constrains {
                source: "r0".into(),
                target: "r1".into(),
            },
            links::RequirementLink::Before {
                source: "r1".into(),
                target: "r2".into(),
            },
        ],
    )
    .unwrap();
    assert_eq!(
        decisions::impacts(w.linked().unwrap())[0].requirement_ids,
        vec!["r0", "r1", "r2"]
    );
    let queries = contract::retrieval_queries(&w).unwrap();
    assert!(queries
        .iter()
        .find(|(id, _)| id == "r2")
        .unwrap()
        .1
        .contains("PROVISIONAL"));
    assert!(!queries
        .iter()
        .find(|(id, _)| id == "r3")
        .unwrap()
        .1
        .contains("PROVISIONAL"));
}

#[test]
fn assessment_status_is_explicit_and_controls_the_contract() {
    let mut w = workflow();
    w.interpret(1, 0, draft()).unwrap();
    w.set_links(1, 1, vec![]).unwrap();
    let r=serde_json::from_value(json!({"generation":"fixture","candidates":[],"lexical_count":0,"vector_count":0,"lexical_truncated":false,"vector_truncated":false,"fusion_truncated":0,"relation_truncated":0})).unwrap();
    let issued = contract::assessment_request("fixture", &w, &r, "").unwrap();
    let decode = |assessment| {
        contract::decode_assessment(
            &w,
            &r,
            &issued,
            &envelope(
                json!({"requirement_coverage":[{"requirement":"r0","assessment":assessment}]}),
            ),
        )
    };
    assert!(
        decode(json!({"status":"supported","capability_ids":[],"explanation":"Supported"}))
            .is_err()
    );
    assert!(
        decode(json!({"status":"local","capability_ids":["c0"],"explanation":"Compute"})).is_err()
    );
    assert!(decode(json!({"status":"local","capability_ids":[],"explanation":" "})).is_err());
    assert!(decode(json!({"useful_capabilities":[],"missing":"None identified."})).is_err());
    assert!(
        decode(json!({"status":"unknown","capability_ids":[],"explanation":"Compute"})).is_err()
    );
    let local = decode(
        json!({"status":"local","capability_ids":[],"explanation":"Compute from supplied numbers"}),
    )
    .unwrap();
    assert!(matches!(
        local.requirement_coverage[0].assessment,
        contract::Assessment::NoCapabilityNeeded { .. }
    ));
    let req: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
    let schema = &req["response_format"]["json_schema"]["schema"]["properties"]
        ["requirement_coverage"]["items"]["properties"]["assessment"];
    assert!(schema.get("anyOf").is_none());
    assert_eq!(
        schema["properties"]["status"]["enum"],
        json!(["supported", "unresolved", "local"])
    );
}
