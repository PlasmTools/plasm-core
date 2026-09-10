//! Offline protocol bridge for evaluation tools. No network or database access.
use anyhow::Result;
use plasm_agent_core::discovery_service::selector_contract;
use plasm_agent_core::discovery_store::RetrievalReceipt;
use plasm_core::prerequisites::CapabilityRef;
use serde::Deserialize;
use serde_json::json;
use std::io::{self, Read};

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
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
            intent,
            retrieval,
        } => {
            let (selection, business) = selector_contract::decode(&raw, &intent, &retrieval)?;
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
