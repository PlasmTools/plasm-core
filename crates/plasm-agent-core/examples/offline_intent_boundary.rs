//! Offline protocol adapter: production requests and decoders, no provider calls.
use anyhow::{Context, Result};
use plasm_agent_core::discovery_store::RetrievalReceipt;
use plasm_agent_core::workflow_intent::{
    contract, staged, IntentScope, InterpretationDraft, RequirementDraft, RequirementKind,
    WorkflowIntent,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, Read};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    stage: String,
    model: String,
    text: String,
    #[serde(default)]
    later_turns: Vec<String>,
    workflow: Option<WorkflowIntent>,
    #[serde(default)]
    questions: staged::Questions,
    retrieval: Option<RetrievalReceipt>,
    raw: Option<String>,
}

fn run(input: Input) -> Result<Value> {
    let mut workflow = match input.workflow {
        Some(w) => w,
        None => WorkflowIntent::open(IntentScope::Workflow, "turn-0".into(), input.text.clone())?,
    };
    let q = input.questions;
    match input.stage.as_str() {
        "prepare" => {
            for (index, text) in input.later_turns.into_iter().enumerate() {
                workflow.append(workflow.revision(), format!("turn-{}", index + 1), text)?;
            }
            Ok(json!({"workflow":workflow,"questions":q}))
        }
        "questions" => {
            let issued = staged::question_request(&input.model, &workflow, &q)?;
            match input.raw {
                Some(raw) => Ok(
                    json!({"workflow":workflow,"questions":staged::decode_questions(&workflow,&q,&issued,&raw)?}),
                ),
                None => Ok(json!({"body":issued.body,"workflow":workflow,"questions":q})),
            }
        }
        "commit" => {
            let issued = staged::commit_request(&input.model, &workflow, &q)?;
            match input.raw {
                Some(raw) => {
                    let (workflow, questions) =
                        staged::decode_commit(&workflow, &q, &issued, &raw)?;
                    Ok(json!({"workflow":workflow,"questions":questions}))
                }
                None => Ok(json!({"body":issued.body})),
            }
        }
        "linkage" => {
            let issued = contract::linkage_request(&input.model, &workflow)?;
            match input.raw {
                Some(raw) => {
                    Ok(json!({"workflow":contract::decode_linkage(&workflow,&issued,&raw)?}))
                }
                None => Ok(json!({"body":issued.body})),
            }
        }
        "assessment" => {
            // Isolate capability selection from interpretation quality: the sole
            // information need is the verbatim public test instruction.
            if workflow.interpretation().is_none() {
                workflow.interpret(
                    workflow.revision(),
                    0,
                    InterpretationDraft {
                        conditionals: vec![],
                        dispositions: Default::default(),
                        requirements: vec![RequirementDraft {
                            uncertainty: None,
                            kind: RequirementKind::InformationNeed,
                            statement: input.text,
                            source_turn_ids: vec![workflow.turns()[0].id.clone()],
                        }],
                        no_requirements_reason: None,
                    },
                )?;
                workflow.set_links(workflow.revision(), workflow.current()?.version, vec![])?;
            }
            let receipt = input.retrieval.context("assessment requires candidates")?;
            let issued = contract::assessment_request(&input.model, &workflow, &receipt, "")?;
            match input.raw {
                Some(raw) => Ok(serde_json::to_value(contract::decode_assessment(
                    &workflow, &receipt, &issued, &raw,
                )?)?),
                None => Ok(json!({"body":issued.body})),
            }
        }
        _ => anyhow::bail!("unknown stage"),
    }
}

fn main() -> Result<()> {
    let mut source = String::new();
    io::stdin().read_to_string(&mut source)?;
    let output = run(serde_json::from_str(&source)?);
    println!(
        "{}",
        match output {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error.to_string()}),
        }
    );
    Ok(())
}
