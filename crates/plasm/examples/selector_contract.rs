//! Offline protocol bridge for evaluation tools. No network or database access.
use anyhow::Result;
use plasm_agent_core::discovery_service::selector_contract;
use plasm_agent_core::discovery_store::RetrievalReceipt;
use plasm_agent_core::workflow_intent::{contract as intent_contract, IntentScope, WorkflowIntent};
use plasm_core::prerequisites::CapabilityRef;
use serde::Deserialize;
use serde_json::json;
use std::io::{self, Read};

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    IntentOpen {
        request_id: String,
        text: String,
        scope: IntentScope,
    },
    IntentAppend {
        workflow: WorkflowIntent,
        expected_revision: u64,
        request_id: String,
        text: String,
    },
    LinkageRequest {
        model: String,
        workflow: WorkflowIntent,
    },
    LinkageDecode {
        workflow: WorkflowIntent,
        issued: intent_contract::BoundRequest,
        raw: String,
    },
    AuditRequest {
        model: String,
        workflow: WorkflowIntent,
    },
    AuditDecode {
        workflow: WorkflowIntent,
        issued: intent_contract::BoundRequest,
        raw: String,
    },
    AssessmentRequest {
        focus: String,
        model: String,
        workflow: WorkflowIntent,
        retrieval: RetrievalReceipt,
    },
    AssessmentDecode {
        workflow: WorkflowIntent,
        retrieval: RetrievalReceipt,
        issued: intent_contract::BoundRequest,
        raw: String,
    },
    RetrievalQueries {
        workflow: WorkflowIntent,
    },
    Documents {
        schema_dir: std::path::PathBuf,
    },
    Request {
        model: String,
        intent: String,
        exposed: Vec<CapabilityRef>,
        retrieval: RetrievalReceipt,
    },
    Decode {
        intent: String,
        raw: String,
        retrieval: RetrievalReceipt,
    },
}

fn run(command: Command) -> Result<serde_json::Value> {
    match command {
        Command::IntentOpen {
            request_id,
            text,
            scope,
        } => Ok(serde_json::to_value(WorkflowIntent::open(
            scope, request_id, text,
        )?)?),
        Command::IntentAppend {
            mut workflow,
            expected_revision,
            request_id,
            text,
        } => {
            workflow.append(expected_revision, request_id, text)?;
            Ok(serde_json::to_value(workflow)?)
        }
        Command::LinkageRequest { model, workflow } => {
            Ok(json!(intent_contract::linkage_request(&model, &workflow)?))
        }
        Command::LinkageDecode {
            workflow,
            issued,
            raw,
        } => Ok(json!(intent_contract::decode_linkage(
            &workflow, &issued, &raw
        )?)),
        Command::AuditRequest { model, workflow } => {
            Ok(json!(intent_contract::audit_request(&model, &workflow)?))
        }
        Command::AuditDecode {
            workflow,
            issued,
            raw,
        } => Ok(serde_json::to_value(intent_contract::decode_audit(
            &workflow, &issued, &raw,
        )?)?),
        Command::AssessmentRequest {
            focus,
            model,
            workflow,
            retrieval,
        } => Ok(json!(intent_contract::assessment_request(
            &model, &workflow, &retrieval, &focus
        )?)),
        Command::AssessmentDecode {
            workflow,
            retrieval,
            issued,
            raw,
        } => Ok(serde_json::to_value(intent_contract::decode_assessment(
            &workflow, &retrieval, &issued, &raw,
        )?)?),
        Command::RetrievalQueries { workflow } => Ok(serde_json::to_value(
            intent_contract::retrieval_queries(&workflow)?,
        )?),
        Command::Documents { schema_dir } => {
            let cgs =
                plasm_core::loader::load_schema_dir(&schema_dir).map_err(anyhow::Error::msg)?;
            cgs.validate().map_err(anyhow::Error::msg)?;
            let documents = plasm_core::catalog_discovery::capability_documents(&cgs)
                .map_err(anyhow::Error::msg)?;
            Ok(serde_json::to_value(documents)?)
        }
        Command::Request {
            model,
            intent,
            exposed,
            retrieval,
        } => selector_contract::request(&model, &intent, &exposed, &retrieval)
            .map(serde_json::Value::String),
        Command::Decode {
            raw,
            intent: _,
            retrieval,
        } => {
            let (selection, business) = selector_contract::decode(&raw, &retrieval)?;
            Ok(json!({"selection":selection,"business":business}))
        }
    }
}

fn main() -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let result = serde_json::from_str(&input)
        .map_err(anyhow::Error::from)
        .and_then(run);
    let output = match result {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
