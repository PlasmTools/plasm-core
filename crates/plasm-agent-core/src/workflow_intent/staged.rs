//! Preliminary questions inform interpretation without closing its decision set.
//! Commit can declare a missed question; the host allocates and attaches its ID.
use super::{contract, decisions::DecisionDraft, RequirementDisposition, WorkflowIntent};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub question: String,
    pub alternatives: Vec<String>,
    pub source_turn_ids: Vec<String>,
    pub resolution: Option<Resolution>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub reason: String,
    pub source_turn_ids: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Questions {
    entries: Vec<Question>,
    /// Requirement ID -> question ID. Text equality never serves as identity.
    owners: BTreeMap<String, String>,
}
pub struct QuestionRequest {
    pub body: String,
    binding: String,
}
pub struct CommitRequest {
    pub body: String,
    binding: String,
}
impl Questions {
    fn validate(&self, w: &WorkflowIntent) -> Result<()> {
        w.validate()?;
        let visible = visible_ids(w);
        for (index, q) in self.entries.iter().enumerate() {
            ensure!(q.id == format!("q{index}"), "invalid question identity");
            DecisionDraft {
                question: q.question.clone(),
                alternatives: q.alternatives.clone(),
            }
            .validate()?;
            provenance(&q.source_turn_ids, &visible, false)?;
            if let Some(r) = &q.resolution {
                ensure!(!r.reason.trim().is_empty(), "blank question resolution");
                provenance(&r.source_turn_ids, &visible, false)?;
            }
        }
        let rows = w
            .interpretation()
            .map(|i| i.requirements.as_slice())
            .unwrap_or_default();
        for (row_id, q_id) in &self.owners {
            let row = rows
                .iter()
                .find(|r| &r.id == row_id)
                .context("unknown question owner")?;
            let q = self
                .entries
                .iter()
                .find(|q| &q.id == q_id)
                .context("unknown owned question")?;
            let d = row
                .decision
                .as_ref()
                .context("settled requirement cannot own question")?;
            ensure!(
                d.question.question == q.question && d.question.alternatives == q.alternatives,
                "question owner disagrees with requirement decision"
            );
            // A resolved question may still own the previous revision until commit replaces it.
            if let Some(resolution) = &q.resolution {
                let committed = w
                    .interpretation()
                    .context("missing interpretation")?
                    .intent_revision as usize;
                ensure!(
                    resolution
                        .source_turn_ids
                        .iter()
                        .any(|id| w.turns()[committed..visible.len()]
                            .iter()
                            .any(|t| &t.id == id)),
                    "resolved question retained in committed interpretation"
                );
            }
        }
        ensure!(
            rows.iter()
                .all(|r| r.decision.is_none() || self.owners.contains_key(&r.id)),
            "requirement decision missing question owner"
        );
        Ok(())
    }
}
fn binding(w: &WorkflowIntent, q: &Questions) -> Result<String> {
    q.validate(w)?;
    Ok(plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&(w, q))?,
    ))
}
fn visible_ids(w: &WorkflowIntent) -> Vec<String> {
    w.turns()[..w.next_interpretation_revision() as usize]
        .iter()
        .map(|t| t.id.clone())
        .collect()
}
fn provenance(ids: &[String], visible: &[String], current: bool) -> Result<()> {
    ensure!(
        !ids.is_empty()
            && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
            && ids.iter().all(|id| visible.contains(id))
            && (!current || visible.last().is_some_and(|id| ids.contains(id))),
        "invalid question provenance"
    );
    Ok(())
}
pub fn question_request(model: &str, w: &WorkflowIntent, q: &Questions) -> Result<QuestionRequest> {
    w.validate()?;
    let visible = visible_ids(w);
    let mut sources = contract::ids(visible.clone());
    sources["minItems"] = json!(1);
    let text = json!({"type":"string","minLength":1});
    let disposition = json!({"anyOf":[contract::object(json!({"action":{"type":"string","enum":["retain"]}})),
        contract::object(json!({"action":{"type":"string","enum":["resolve"]},"reason":text,"source_turn_ids":sources}))]});
    let dispositions: serde_json::Map<String, Value> = q
        .entries
        .iter()
        .filter(|q| q.resolution.is_none())
        .map(|q| (q.id.clone(), disposition.clone()))
        .collect();
    let schema = contract::object(
        json!({"dispositions":contract::object(Value::Object(dispositions)),"new_questions":{"type":"array","maxItems":16,"items":contract::object(json!({"question":text,"alternatives":{"type":"array","minItems":2,"maxItems":8,"items":text},"source_turn_ids":sources}))}}),
    );
    Ok(QuestionRequest {
        body: contract::request(
            model,
            include_str!("questions.txt"),
            json!({"turns":&w.turns()[..visible.len()],"decisions":q.entries}),
            schema,
            "intent_questions",
        )?,
        binding: binding(w, q)?,
    })
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Disposition {
    Retain,
    Resolve {
        reason: String,
        source_turn_ids: Vec<String>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewQuestion {
    question: String,
    alternatives: Vec<String>,
    source_turn_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionDraft {
    dispositions: BTreeMap<String, Disposition>,
    new_questions: Vec<NewQuestion>,
}
pub fn decode_questions(
    w: &WorkflowIntent,
    q: &Questions,
    issued: &QuestionRequest,
    raw: &str,
) -> Result<Questions> {
    ensure!(issued.binding == binding(w, q)?, "stale question response");
    let draft: QuestionDraft = serde_json::from_str(&contract::content(raw)?)?;
    let expected: BTreeSet<_> = q
        .entries
        .iter()
        .filter(|q| q.resolution.is_none())
        .map(|q| q.id.clone())
        .collect();
    ensure!(
        draft.dispositions.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "question disposition totality"
    );
    ensure!(draft.new_questions.len() <= 16, "too many questions");
    let visible = visible_ids(w);
    let mut next = q.clone();
    for entry in &mut next.entries {
        if let Some(Disposition::Resolve {
            reason,
            source_turn_ids,
        }) = draft.dispositions.get(&entry.id)
        {
            ensure!(!reason.trim().is_empty(), "blank question resolution");
            provenance(source_turn_ids, &visible, true)?;
            entry.resolution = Some(Resolution {
                reason: reason.clone(),
                source_turn_ids: source_turn_ids.clone(),
            });
        }
    }
    for entry in draft.new_questions {
        DecisionDraft {
            question: entry.question.clone(),
            alternatives: entry.alternatives.clone(),
        }
        .validate()?;
        provenance(&entry.source_turn_ids, &visible, false)?;
        next.entries.push(Question {
            id: format!("q{}", next.entries.len()),
            question: entry.question,
            alternatives: entry.alternatives,
            source_turn_ids: entry.source_turn_ids,
            resolution: None,
        });
    }
    Ok(next)
}
pub fn commit_request(model: &str, w: &WorkflowIntent, q: &Questions) -> Result<CommitRequest> {
    // Focus never rewrites authoritative meaning; it is a separate retrieval lane.
    let issued = contract::commit_base_request(model, w)?;
    let mut body: Value = serde_json::from_str(&issued.body)?;
    let mut input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().context("input")?)?;
    input["decision_table"] = serde_json::to_value(&q.entries)?;
    input["decision_owners"] = serde_json::to_value(&q.owners)?;
    body["messages"][1]["content"] = json!(input.to_string());
    let schema = &mut body["response_format"]["json_schema"]["schema"];
    let mut requirement = schema["properties"]["requirements"]["items"].clone();
    requirement["properties"]
        .as_object_mut()
        .context("properties")?
        .remove("uncertainty");
    requirement["required"]
        .as_array_mut()
        .context("required")?
        .retain(|v| v != "uncertainty");
    // Finite shared definitions mirror the host's depth and branch-kind limits.
    let mut defs = serde_json::Map::new();
    for kind in [
        "effect",
        "information_need",
        "selection_constraint",
        "prohibition",
    ] {
        let mut leaf = requirement.clone();
        leaf["properties"]["kind"]["enum"] = json!([kind]);
        defs.insert(kind.into(), leaf);
    }
    let mut root_properties = Value::Null;
    for depth in 0..=super::conditional::MAX_DEPTH {
        let nodes =
            json!({"type":"array","maxItems":64,"items":{"$ref":format!("#/$defs/node_{depth}")}});
        let existing: serde_json::Map<String, Value> = q
            .entries
            .iter()
            .filter(|q| q.resolution.is_none())
            .map(|q| (q.id.clone(), nodes.clone()))
            .collect();
        let question = contract::object(json!({
            "question":{"type":"string","minLength":1},
            "alternatives":{"type":"array","minItems":2,"maxItems":8,"items":{"type":"string","minLength":1}},
            "requirements":nodes
        }));
        let properties = json!({
            "settled_requirements":nodes,
            "existing_questions":contract::object(Value::Object(existing)),
            "new_questions":{"type":"array","maxItems":16,"items":question}
        });
        if depth == 0 {
            root_properties = properties.clone();
        }
        defs.insert(format!("body_{depth}"), contract::object(properties));
        let mut variants = vec![
            json!({"$ref":"#/$defs/effect"}),
            json!({"$ref":"#/$defs/information_need"}),
            json!({"$ref":"#/$defs/selection_constraint"}),
        ];
        if depth == 0 {
            variants.push(json!({"$ref":"#/$defs/prohibition"}));
        }
        if depth < super::conditional::MAX_DEPTH {
            variants.push(contract::object(json!({
                "kind":{"type":"string","enum":["conditional"]},
                "predicate":contract::object(json!({
                    "statement":requirement["properties"]["statement"],
                    "source_turn_ids":requirement["properties"]["source_turn_ids"]
                })),
                "when_true":{"$ref":format!("#/$defs/body_{}",depth+1)},
                "when_false":{"$ref":format!("#/$defs/body_{}",depth+1)}
            })));
        }
        defs.insert(format!("node_{depth}"), json!({"anyOf":variants}));
    }
    root_properties["dispositions"] = schema["properties"]["dispositions"].clone();
    root_properties["no_requirements_reason"] = json!({"type":["string","null"]});
    *schema = contract::object(root_properties);
    schema["$defs"] = Value::Object(defs);
    input["revision_units"] = serde_json::to_value(
        w.interpretation()
            .map(super::conditional::units)
            .unwrap_or_default(),
    )?;
    body["messages"][1]["content"] = json!(input.to_string());
    Ok(CommitRequest {
        body: body.to_string(),
        binding: binding(w, q)?,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitDraft {
    #[serde(deserialize_with = "super::deserialize_dispositions")]
    dispositions: BTreeMap<String, RequirementDisposition>,
    settled_requirements: Vec<CommitNode>,
    #[serde(deserialize_with = "deserialize_question_groups")]
    existing_questions: BTreeMap<String, Vec<CommitNode>>,
    new_questions: Vec<CommitQuestion>,
    no_requirements_reason: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitStatement {
    statement: String,
    source_turn_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CommitNode {
    Effect(CommitStatement),
    InformationNeed(CommitStatement),
    SelectionConstraint(CommitStatement),
    Prohibition(CommitStatement),
    Conditional(Box<CommitConditional>),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitConditional {
    predicate: CommitStatement,
    when_true: CommitBody,
    when_false: CommitBody,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitBody {
    settled_requirements: Vec<CommitNode>,
    #[serde(deserialize_with = "deserialize_question_groups")]
    existing_questions: BTreeMap<String, Vec<CommitNode>>,
    new_questions: Vec<CommitQuestion>,
}
#[path = "commit_lowering.rs"]
mod lowering;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitQuestion {
    question: String,
    alternatives: Vec<String>,
    requirements: Vec<CommitNode>,
}
fn deserialize_question_groups<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, Vec<CommitNode>>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, Vec<CommitNode>>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("one owner group per question ID")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut groups = BTreeMap::new();
            while let Some((id, owners)) = map.next_entry::<String, Vec<CommitNode>>()? {
                if groups.insert(id, owners).is_some() {
                    return Err(serde::de::Error::custom("duplicate question group"));
                }
            }
            Ok(groups)
        }
    }
    deserializer.deserialize_map(Unique)
}

pub fn decode_commit(
    w: &WorkflowIntent,
    q: &Questions,
    issued: &CommitRequest,
    raw: &str,
) -> Result<(WorkflowIntent, Questions)> {
    ensure!(
        issued.binding == binding(w, q)?,
        "stale interpretation response"
    );
    let proposed: CommitDraft = serde_json::from_str(&contract::content(raw)?)?;
    let (draft, mut next, references) = lowering::lower(w, q, proposed)?;
    let retained: BTreeSet<_> = draft
        .dispositions
        .iter()
        .filter(|(_, d)| matches!(d, RequirementDisposition::Retain {}))
        .map(|(id, _)| id.clone())
        .collect();
    let mut updated = w.clone();
    let version = w.interpretation().map_or(0, |i| i.version);
    updated.interpret(w.revision(), version, draft)?;
    next.owners.retain(|id, _| retained.contains(id));
    let fresh: Vec<_> = updated
        .interpretation()
        .context("interpreted")?
        .requirements
        .iter()
        .filter(|r| !retained.contains(&r.id))
        .collect();
    ensure!(
        fresh.len() == references.len(),
        "requirement allocation mismatch"
    );
    for (r, reference) in fresh.into_iter().zip(references) {
        if let Some(id) = reference {
            next.owners.insert(r.id.clone(), id);
        }
    }
    for entry in &next.entries {
        let attached = next.owners.values().any(|id| id == &entry.id);
        ensure!(
            attached == entry.resolution.is_none(),
            "unresolved question dropped or resolved question retained: {}",
            entry.id
        );
    }
    next.validate(&updated)?;
    Ok((updated, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope(value: Value) -> String {
        json!({"choices":[{"finish_reason":"stop","message":{"content":value.to_string()}}]})
            .to_string()
    }
    fn open() -> WorkflowIntent {
        WorkflowIntent::open(
            super::super::IntentScope::Workflow,
            "t0".into(),
            "Prepare records; preserve the two possible scopes".into(),
        )
        .unwrap()
    }
    fn owner() -> Value {
        json!({"kind":"effect","statement":"Prepare records for the unresolved scope","source_turn_ids":["u0"]})
    }
    fn response() -> Value {
        json!({"dispositions":{},"settled_requirements":[],"existing_questions":{},"new_questions":[],"no_requirements_reason":null})
    }
    fn question() -> Value {
        json!({"question":"Which scope?","alternatives":["Owned records","Shared records"],"requirements":[owner()]})
    }
    fn preliminary(w: &WorkflowIntent) -> Questions {
        let q = Questions::default();
        let issued = question_request("fixture", w, &q).unwrap();
        decode_questions(w,&q,&issued,&envelope(json!({"dispositions":{},"new_questions":[{"question":"Which scope?","alternatives":["Owned records","Shared records"],"source_turn_ids":["u0"]}]}))).unwrap()
    }
    #[test]
    fn conditional_ownership_survives_empty_link_selection() {
        let w = open();
        let q = Questions::default();
        let mut proposal = response();
        let body = |items: Value| json!({"settled_requirements":items,"existing_questions":{},"new_questions":[]});
        proposal["settled_requirements"] = json!([
            {"kind":"information_need","statement":"Observe the threshold","source_turn_ids":["u0"]},
            {"kind":"conditional","predicate":{"statement":"Threshold exceeded","source_turn_ids":["u0"]},
             "when_true":body(json!([{"kind":"effect","statement":"Perform requested action","source_turn_ids":["u0"]}])),
             "when_false":body(json!([{"kind":"effect","statement":"Report current state","source_turn_ids":["u0"]}]))}
        ]);
        let issued = commit_request("fixture", &w, &q).unwrap();
        let (committed, _) = decode_commit(&w, &q, &issued, &envelope(proposal)).unwrap();
        let request = contract::linkage_request("fixture", &committed).unwrap();
        let linked =
            contract::decode_linkage(&committed, &request, &envelope(json!({"link_ids":[]})))
                .unwrap();
        let edges = linked.linked().unwrap().links.as_ref().unwrap();
        assert_eq!(
            edges,
            &vec![
                super::super::links::RequirementLink::WhenTrue {
                    source: "r1".into(),
                    target: "r2".into()
                },
                super::super::links::RequirementLink::WhenFalse {
                    source: "r1".into(),
                    target: "r3".into()
                },
            ]
        );
    }

    #[test]
    fn commit_recovers_missed_question_with_atomic_shared_ownership_and_impacts() {
        let w = open();
        let q = Questions::default();
        let issued = commit_request("fixture", &w, &q).unwrap();
        let mut body = response();
        let mut group = question();
        group["requirements"] = json!([owner(), owner()]);
        body["new_questions"] = json!([group]);
        let (mut updated, next) = decode_commit(&w, &q, &issued, &envelope(body)).unwrap();
        assert!(q.entries.is_empty());
        assert!(w.interpretation().is_none());
        assert_eq!(next.entries.len(), 1);
        assert_eq!(next.entries[0].id, "q0");
        assert_eq!(next.entries[0].source_turn_ids, vec!["u0"]);
        assert_eq!(next.owners["r0"], "q0");
        assert_eq!(next.owners["r1"], "q0");
        updated
            .set_links(
                updated.revision(),
                updated.current().unwrap().version,
                vec![],
            )
            .unwrap();
        let impacts = super::super::decisions::impacts(updated.linked().unwrap());
        assert_eq!(impacts.len(), 2);
        assert_eq!(impacts[0].decision.id, "d0");
        assert_eq!(impacts[1].decision.id, "d1");
        assert_eq!(
            impacts[0].decision.question.alternatives,
            vec!["Owned records", "Shared records"]
        );
        assert_eq!(impacts[0].requirement_ids, vec!["r0"]);
        assert!(decode_commit(&updated, &next, &issued, &envelope(response())).is_err());
    }
    #[test]
    fn existing_questions_cannot_be_omitted_forged_or_left_without_owners() {
        let w = open();
        let q = preliminary(&w);
        let issued = commit_request("fixture", &w, &q).unwrap();
        for groups in [json!({}), json!({"q99":[owner()]}), json!({"q0":[]})] {
            let mut body = response();
            body["existing_questions"] = groups;
            body["settled_requirements"] = json!([owner()]);
            assert!(decode_commit(&w, &q, &issued, &envelope(body)).is_err());
        }
        let mut body = response();
        body["existing_questions"] = json!({"q0":[owner()]});
        let (updated, next) = decode_commit(&w, &q, &issued, &envelope(body)).unwrap();
        assert_eq!(next.owners["r0"], "q0");
        assert!(updated.current().unwrap().requirements[0]
            .decision
            .is_some());
    }
    #[test]
    fn malformed_new_groups_and_old_wire_fail_without_state_change() {
        let w = open();
        let q = Questions::default();
        let issued = commit_request("fixture", &w, &q).unwrap();
        for invalid in [
            json!({"question":"Which?","alternatives":["A","B"],"requirements":[]}),
            json!({"question":"Which?","alternatives":["Same","Same"],"requirements":[owner()]}),
            json!({"question":"Which?","alternatives":["A","B"],"requirements":[owner()],"id":"q0"}),
            json!({"question":"Which?","alternatives":["A","B"],"requirements":[{"kind":"effect","statement":"Prepare","source_turn_ids":["u99"]}]}),
        ] {
            let mut body = response();
            body["new_questions"] = json!([invalid]);
            assert!(decode_commit(&w, &q, &issued, &envelope(body)).is_err());
        }
        let mut old = response();
        old["requirements"] = json!([]);
        assert!(decode_commit(&w, &q, &issued, &envelope(old)).is_err());
        for field in ["meaning", "decision_ref", "uncertainty"] {
            let mut row = owner();
            row[field] = Value::Null;
            let mut body = response();
            body["settled_requirements"] = json!([row]);
            assert!(decode_commit(&w, &q, &issued, &envelope(body)).is_err());
        }
        let mut body = response();
        body["new_questions"] = json!([question()]);
        body["existing_questions"] = json!({"q0":[owner()]});
        assert!(decode_commit(&w, &q, &issued, &envelope(body)).is_err());
        assert!(w.interpretation().is_none());
        assert!(q.entries.is_empty());
    }
    #[test]
    fn new_question_resolution_requires_current_turn_and_owner_retirement() {
        let w = open();
        let q = Questions::default();
        let issued = commit_request("fixture", &w, &q).unwrap();
        let mut body = response();
        body["new_questions"] = json!([question()]);
        let (mut w, q) = decode_commit(&w, &q, &issued, &envelope(body)).unwrap();
        w.append(1, "t1".into(), "Owned records only".into())
            .unwrap();
        let issued = question_request("fixture", &w, &q).unwrap();
        let mut resolution = json!({"dispositions":{"q0":{"action":"resolve","reason":"User chose scope","source_turn_ids":["u0"]}},"new_questions":[]});
        assert!(decode_questions(&w, &q, &issued, &envelope(resolution.clone())).is_err());
        resolution["dispositions"]["q0"]["source_turn_ids"] = json!(["u1"]);
        let q = decode_questions(&w, &q, &issued, &envelope(resolution)).unwrap();
        let issued = commit_request("fixture", &w, &q).unwrap();
        let mut retained = response();
        retained["dispositions"] = json!({"r0":{"action":"retain"}});
        assert!(decode_commit(&w, &q, &issued, &envelope(retained)).is_err());
        let mut retired = response();
        retired["dispositions"] =
            json!({"r0":{"action":"retire","reason":"Scope resolved","source_turn_ids":["u1"]}});
        retired["settled_requirements"] =
            json!([{"kind":"effect","statement":"Prepare owned records","source_turn_ids":["u1"]}]);
        let (w, q) = decode_commit(&w, &q, &issued, &envelope(retired)).unwrap();
        assert!(q.owners.is_empty());
        assert!(w.current().unwrap().requirements[0].decision.is_none());
    }
    #[test]
    fn group_allocation_is_bounded_and_follows_existing_ids() {
        let w = open();
        let q = preliminary(&w);
        let issued = commit_request("fixture", &w, &q).unwrap();
        let mut body = response();
        body["existing_questions"] = json!({"q0":[owner()]});
        body["new_questions"] = json!(vec![question(); 16]);
        let (_, next) = decode_commit(&w, &q, &issued, &envelope(body.clone())).unwrap();
        for n in 0..17 {
            assert_eq!(next.owners[&format!("r{n}")], format!("q{n}"));
        }
        body["new_questions"] = json!(vec![question(); 17]);
        assert!(decode_commit(&w, &q, &issued, &envelope(body))
            .unwrap_err()
            .to_string()
            .contains("too many new questions"));
        let mut body = response();
        body["existing_questions"] = json!({"q0":[owner()]});
        body["settled_requirements"] = json!(vec![owner(); 64]);
        assert!(decode_commit(&w, &q, &issued, &envelope(body))
            .unwrap_err()
            .to_string()
            .contains("too many requirements"));
    }
    #[test]
    fn duplicate_question_group_keys_are_rejected_before_owner_loss() {
        let w = open();
        let q = preliminary(&w);
        let issued = commit_request("fixture", &w, &q).unwrap();
        let raw = r#"{"dispositions":{},"settled_requirements":[],"existing_questions":{"q0":[],"q0":[]},"new_questions":[],"no_requirements_reason":null}"#;
        let response =
            json!({"choices":[{"finish_reason":"stop","message":{"content":raw}}]}).to_string();
        assert!(decode_commit(&w, &q, &issued, &response)
            .unwrap_err()
            .to_string()
            .contains("duplicate question group"));
    }
    fn branch_body(items: Value) -> Value {
        json!({"settled_requirements":items,"existing_questions":{},"new_questions":[]})
    }
    fn conditional_node(yes: Value, no: Value) -> Value {
        json!({"kind":"conditional","predicate":{"statement":"Predicate","source_turn_ids":["u0"]},"when_true":branch_body(yes),"when_false":branch_body(no)})
    }
    fn commit_nodes(w: &WorkflowIntent, nodes: Value) -> Result<WorkflowIntent> {
        let q = Questions::default();
        let issued = commit_request("fixture", w, &q)?;
        let mut v = response();
        v["settled_requirements"] = nodes;
        decode_commit(w, &q, &issued, &envelope(v)).map(|(w, _)| w)
    }

    #[test]
    fn branch_constraint_without_target_is_rejected_atomically() {
        let w = open();
        let q = Questions::default();
        let issued = commit_request("fixture", &w, &q).unwrap();
        let mut constraint = owner();
        constraint["kind"] = json!("selection_constraint");
        let mut proposal = response();
        proposal["settled_requirements"] =
            json!([conditional_node(json!([constraint]), json!([owner()]))]);
        let before = serde_json::to_value((&w, &q)).unwrap();
        assert!(decode_commit(&w, &q, &issued, &envelope(proposal))
            .unwrap_err()
            .to_string()
            .contains("no legal operation target"));
        assert_eq!(serde_json::to_value((&w, &q)).unwrap(), before);
    }

    #[test]
    fn restored_question_ownership_must_agree_with_requirements() {
        let w = open();
        let q = Questions::default();
        let mut proposal = response();
        proposal["new_questions"] = json!([question()]);
        let (w, q) = decode_commit(
            &w,
            &q,
            &commit_request("fixture", &w, &q).unwrap(),
            &envelope(proposal),
        )
        .unwrap();
        for mutation in 0..5 {
            let mut corrupt = serde_json::to_value(&q).unwrap();
            match mutation {
                0 => corrupt["owners"] = json!({"r99":"q0"}),
                1 => corrupt["owners"] = json!({"r0":"q99"}),
                2 => corrupt["owners"] = json!({}),
                3 => corrupt["entries"][0]["question"] = json!("Different question"),
                _ => corrupt["entries"][0]["source_turn_ids"] = json!(["u99"]),
            }
            let corrupt: Questions = serde_json::from_value(corrupt).unwrap();
            assert!(commit_request("fixture", &w, &corrupt).is_err());
        }
        let mut corrupt = serde_json::to_value(&w).unwrap();
        corrupt["interpretation"]["requirements"][0]["decision"] = Value::Null;
        let corrupt: WorkflowIntent = serde_json::from_value(corrupt).unwrap();
        assert!(question_request("fixture", &corrupt, &q).is_err());
    }

    #[test]
    fn commit_schema_bounds_depth_and_excludes_branch_prohibitions() {
        let request = commit_request("fixture", &open(), &Questions::default()).unwrap();
        let packet: Value = serde_json::from_str(&request.body).unwrap();
        let schema = &packet["response_format"]["json_schema"]["schema"];
        let defs = &schema["$defs"];
        assert_eq!(defs["node_0"]["anyOf"].as_array().unwrap().len(), 5);
        for depth in 1..=super::super::conditional::MAX_DEPTH {
            let variants = defs[format!("node_{depth}")]["anyOf"].as_array().unwrap();
            assert!(!variants.iter().any(|v| v["$ref"] == "#/$defs/prohibition"));
            assert_eq!(
                variants.len(),
                if depth == super::super::conditional::MAX_DEPTH {
                    3
                } else {
                    4
                }
            );
        }
        fn references(value: &Value, root: &Value) {
            match value {
                Value::Object(fields) => {
                    if let Some(Value::String(reference)) = fields.get("$ref") {
                        assert!(root.pointer(reference.strip_prefix('#').unwrap()).is_some());
                    }
                    for v in fields.values() {
                        references(v, root);
                    }
                }
                Value::Array(items) => {
                    for v in items {
                        references(v, root);
                    }
                }
                _ => {}
            }
        }
        references(schema, schema);
    }

    #[test]
    fn branch_local_question_resolution_replaces_unit_without_touching_independent_rows() {
        let w = open();
        let q = Questions::default();
        let mut conditional = conditional_node(json!([]), json!([owner()]));
        conditional["when_true"]["new_questions"] = json!([question()]);
        let mut proposal = response();
        proposal["settled_requirements"] = json!([owner(), conditional]);
        let (mut w, q) = decode_commit(
            &w,
            &q,
            &commit_request("fixture", &w, &q).unwrap(),
            &envelope(proposal),
        )
        .unwrap();
        assert_eq!(q.owners, BTreeMap::from([("r2".into(), "q0".into())]));
        let linked = contract::decode_linkage(
            &w,
            &contract::linkage_request("fixture", &w).unwrap(),
            &envelope(json!({"link_ids":[]})),
        )
        .unwrap();
        let impacts = super::super::decisions::impacts(linked.linked().unwrap());
        assert_eq!(impacts[0].requirement_ids, vec!["r2"]);
        w.append(
            1,
            "t1".into(),
            "Choose owned records; keep the opposite branch and independent action".into(),
        )
        .unwrap();
        let q = decode_questions(&w,&q,&question_request("fixture",&w,&q).unwrap(),
            &envelope(json!({"dispositions":{"q0":{"action":"resolve","reason":"User chose scope","source_turn_ids":["u1"]}},"new_questions":[]}))).unwrap();
        let mut replacement = response();
        replacement["dispositions"] = json!({"r0":{"action":"retain"},"r1":{"action":"retire","reason":"Branch scope resolved","source_turn_ids":["u1"]}});
        let mut resolved = owner();
        resolved["statement"] = json!("Prepare owned records");
        resolved["source_turn_ids"] = json!(["u1"]);
        replacement["settled_requirements"] =
            json!([conditional_node(json!([resolved]), json!([owner()]))]);
        let (w, q) = decode_commit(
            &w,
            &q,
            &commit_request("fixture", &w, &q).unwrap(),
            &envelope(replacement),
        )
        .unwrap();
        assert!(q.owners.is_empty());
        assert_eq!(w.current().unwrap().requirements[0].id, "r0");
        assert_eq!(w.current().unwrap().conditionals.len(), 1);
        assert_eq!(w.current().unwrap().retired.len(), 3);
    }

    proptest::proptest! {
        #[test]
        fn nested_guards_and_independent_order_preserve_meaning(inner_true in proptest::bool::ANY, reverse in proptest::bool::ANY) {
            let mut outside=owner(); outside["statement"]=json!("Independent operation");
            let mut inside=owner(); inside["statement"]=json!("Nested operation");
            let mut inner=conditional_node(if inner_true {json!([inside])}else{json!([])},
                if inner_true {json!([])}else{json!([inside])});
            inner["predicate"]["statement"]=json!("Inner predicate");
            let mut outer=conditional_node(json!([inner]),json!([]));
            outer["predicate"]["statement"]=json!("Outer predicate");
            let nodes=if reverse {json!([outer,outside])} else {json!([outside,outer])};
            let w=commit_nodes(&open(),nodes).unwrap();
            let linked=contract::decode_linkage(&w,&contract::linkage_request("fixture",&w).unwrap(),&envelope(json!({"link_ids":[]}))).unwrap();
            let graph=linked.linked().unwrap();
            let queries=super::super::links::queries(graph,graph.links.as_ref().unwrap()).unwrap();
            let nested=queries.iter().find(|(_,q)|q.starts_with("Nested operation")).unwrap();
            proptest::prop_assert!(nested.1.contains("Only when: Outer predicate"));
            let inner_guard = if inner_true {"Only when: Inner predicate"} else {"Only when false: Inner predicate"};
            proptest::prop_assert!(nested.1.contains(inner_guard));
            let independent=queries.iter().find(|(_,q)|q.starts_with("Independent operation")).unwrap();
            proptest::prop_assert_eq!(&independent.1,"Independent operation");
            proptest::prop_assert_eq!(graph.links.as_ref().unwrap().len(),2);
        }
    }
    #[test]
    fn conditional_revision_unit_retains_and_retires_atomically() {
        let w = open();
        let mut w = commit_nodes(
            &w,
            json!([conditional_node(json!([owner()]), json!([owner()]))]),
        )
        .unwrap();
        let initial = w.interpretation().unwrap().conditionals.clone();
        w.append(1, "t1".into(), "Keep the conditional operation".into())
            .unwrap();
        let q = Questions::default();
        let issued = commit_request("fixture", &w, &q).unwrap();
        let mut retained = response();
        retained["dispositions"] = json!({"r0":{"action":"retain"}});
        let (retained_w, _) = decode_commit(&w, &q, &issued, &envelope(retained)).unwrap();
        assert_eq!(retained_w.current().unwrap().conditionals, initial);
        assert_eq!(retained_w.current().unwrap().requirements.len(), 3);
        let restored: WorkflowIntent =
            serde_json::from_str(&serde_json::to_string(&retained_w).unwrap()).unwrap();
        restored.validate().unwrap();
        let req = contract::linkage_request("fixture", &restored).unwrap();
        let linked =
            contract::decode_linkage(&restored, &req, &envelope(json!({"link_ids":[]}))).unwrap();
        assert_eq!(linked.linked().unwrap().links.as_ref().unwrap().len(), 2);
        let before = w.clone();
        let mut partial = response();
        partial["dispositions"] = json!({"r0":{"action":"retain"},"r1":{"action":"retire","reason":"changed","source_turn_ids":["u1"]}});
        assert!(decode_commit(&w, &q, &issued, &envelope(partial)).is_err());
        assert_eq!(w, before);
        let mut retired = response();
        retired["dispositions"] = json!({"r0":{"action":"retire","reason":"User withdrew operation","source_turn_ids":["u1"]}});
        retired["no_requirements_reason"] = json!("No requested operations remain");
        let (removed, _) = decode_commit(&w, &q, &issued, &envelope(retired)).unwrap();
        assert!(removed.current().unwrap().requirements.is_empty());
        assert!(removed.current().unwrap().conditionals.is_empty());
        assert_eq!(removed.current().unwrap().retired.len(), 3);
    }
    #[test]
    fn nested_branch_guards_cannot_be_removed_or_scope_escape() {
        let nested = conditional_node(json!([owner()]), json!([owner()]));
        let constraint = json!({"kind":"selection_constraint","statement":"Eligible only","source_turn_ids":["u0"]});
        let w = commit_nodes(
            &open(),
            json!([
                owner(),
                conditional_node(json!([constraint, nested]), json!([owner()]))
            ]),
        )
        .unwrap();
        let i = w.current().unwrap();
        assert_eq!(i.conditionals.len(), 2);
        let choices = super::super::links::choices(i);
        assert!(!choices.iter().any(|c| matches!(
            c.link,
            super::super::links::RequirementLink::WhenTrue { .. }
                | super::super::links::RequirementLink::WhenFalse { .. }
        )));
        let allowed:Vec<_>=choices.iter().filter(|c|matches!(&c.link,super::super::links::RequirementLink::Constrains{source,..} if source=="r2")).map(|c|c.link.endpoints().1).collect();
        assert_eq!(allowed, vec!["r4", "r5"]);
        let mut broken = w.clone();
        assert!(broken.set_links(1, 1, vec![]).is_err());
        assert_eq!(broken, w);
        let mut state = serde_json::to_value(&w).unwrap();
        state["interpretation"]["conditionals"][1]["when_true"] = json!(["r2", "r3", "r4"]);
        let damaged: WorkflowIntent = serde_json::from_value(state).unwrap();
        assert!(damaged.validate().is_err());
    }
    #[test]
    fn conditional_shape_limits_reject_without_publishing() {
        let w = open();
        let before = w.clone();
        for node in [
            conditional_node(json!([]), json!([])),
            conditional_node(
                json!([{"kind":"prohibition","statement":"Do not act","source_turn_ids":["u0"]}]),
                json!([]),
            ),
            json!({"kind":"condition","statement":"Naked predicate","source_turn_ids":["u0"]}),
        ] {
            assert!(commit_nodes(&w, json!([node])).is_err());
        }
        let mut nested = owner();
        for _ in 0..super::super::conditional::MAX_DEPTH {
            nested = conditional_node(json!([nested]), json!([]));
        }
        commit_nodes(&w, json!([nested.clone()])).unwrap();
        assert!(commit_nodes(&w, json!([conditional_node(json!([nested]), json!([]))])).is_err());
        let many = vec![owner(); 64];
        assert!(commit_nodes(&w, json!([conditional_node(json!(many), json!([]))])).is_err());
        assert_eq!(w, before);
    }
    proptest::proptest! {
        #[test]
        fn conditional_generated_polarity_and_unconditional_isolation(yes in 0usize..12, no in 0usize..12, outside in 0usize..8) {
            proptest::prop_assume!(yes+no>0);
            let mut nodes=vec![owner();outside];
            nodes.push(conditional_node(json!(vec![owner();yes]),json!(vec![owner();no])));
            let w=commit_nodes(&open(),json!(nodes)).unwrap();
            let req=contract::linkage_request("fixture",&w).unwrap();
            let linked=contract::decode_linkage(&w,&req,&envelope(json!({"link_ids":[]}))).unwrap();
            let i=linked.linked().unwrap();let edges=i.links.as_ref().unwrap();
            proptest::prop_assert_eq!(edges.len(),yes+no);
            for (offset, edge) in edges.iter().enumerate() {
                let (source,target)=edge.endpoints();
                proptest::prop_assert_eq!(source,format!("r{outside}"));
                proptest::prop_assert_eq!(target,format!("r{}",outside+1+offset));
                proptest::prop_assert_eq!(matches!(edge,super::super::links::RequirementLink::WhenTrue{..}),offset<yes);
            }
            let restored:WorkflowIntent=serde_json::from_str(&serde_json::to_string(&linked).unwrap()).unwrap();
            proptest::prop_assert_eq!(restored,linked);
        }
    }
}
