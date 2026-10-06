//! Subprocess contract for the routed terminal. Abstract server responses; no model calls.
use axum::{extract::OriginalUri, routing::post, Json, Router};
use plasm_agent_core::{
    discovery_matcher::CapabilityMatchReceipt,
    discovery_service::RoutingReceipt,
    discovery_store::{DiscoveryAuthorization, RetrievalReceipt},
    intent_provenance::IntentProvenance,
};
use plasm_core::prerequisites::{CapabilityRef, PrerequisiteClosure};
use serde_json::{json, Value};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};

fn routing(intent: String) -> RoutingReceipt {
    RoutingReceipt {
        environment_support: None,
        intent_provenance: IntentProvenance::from_turns([intent.clone()]).unwrap(),
        authorization: DiscoveryAuthorization::catalogs(["matrix".into()].into()),
        intent_analysis: String::new(),
        intent,
        pin_id: "pin".into(),
        retrieval: RetrievalReceipt {
            generation: "generation-one".into(),
            candidates: vec![],
            lexical_count: 0,
            vector_count: 0,
            lexical_truncated: false,
            vector_truncated: false,
        },
        matching: CapabilityMatchReceipt { matches: vec![] },
        closure: Some(PrerequisiteClosure {
            acquisitions: vec![],
            business: vec![CapabilityRef { catalog: "matrix".into(), capability: "read".into() }],
            input_sources: vec![],
            prerequisites: vec![],
            edges: vec![],
        }),
        recovery: None,
    }
}

#[test]
fn cli_routed_context_and_plan_protocol() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requests = seen.clone();
    let (server, task) = runtime.block_on(async {
        let router = Router::new().fallback(post(move |OriginalUri(uri): OriginalUri, body: String| {
            let requests = requests.clone();
            async move {
                let payload: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!(body));
                requests.lock().unwrap().push((uri.to_string(), payload.clone()));
                if uri.path() == "/execute/ph/sid" { return Json(json!({"planned":true})); }
                Json(json!({
                    "routing": routing(payload["intent"].as_str().unwrap().to_owned()),
                    "context":{"prompt_hash":"ph","session_id":"sid","primary_entry_id":"matrix","principal":null,"waves":[{"mode":"new","entry_id":"matrix","entities":["Record"],"markdown_delta":"canonical teaching","reused_session":false,"teaching_prompt_chars_added":18}],"binding_updated":true,"new_symbol_space":true,"stale_execute_binding_recovered":false,"stale_binding_previous":null,"symbol_space_reset":false},
                    "prerequisite_guidance":"explicit provider binding"
                }))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("http://{}",listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener,router).await.unwrap(); });
        (server,task)
    });
    let workspace = tempfile::tempdir().unwrap();
    let profiles = workspace.path().join(".plasm/profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("default.json"),
        serde_json::to_vec(&json!({"server":server})).unwrap(),
    )
    .unwrap();
    let command = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_plasm"))
            .current_dir(workspace.path())
            .env("PLASM_WORKSPACE", workspace.path())
            .args(args)
            .output()
            .unwrap()
    };
    let opened = command(&["context", "--new", "--intent", "read records"]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let teaching = String::from_utf8_lossy(&opened.stdout);
    assert!(
        teaching.contains("canonical teaching") && teaching.contains("explicit provider binding")
    );
    assert!(command(&["context", "--intent", "read more"])
        .status
        .success());
    let program = workspace.path().join("program.py");
    let source = "class Inspect(Program):\n    def build(self):\n        return e1.query()\n";
    std::fs::write(&program, source).unwrap();
    let plan = command(&["run", "--mode", "plan", "--file", program.to_str().unwrap()]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(!command(&["context", "--intent", "read", "matrix:Record"])
        .status
        .success());
    let discovery = command(&["search", "read records"]);
    assert!(discovery.status.success());
    let receipt: Value = serde_json::from_slice(&discovery.stdout).unwrap();
    assert_eq!(
        receipt["routing"]["closure"]["business"][0]["capability"],
        "read"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, "/v1/context");
    assert_eq!(seen[1].0, "/execute/ph/sid/context");
    assert!(seen[2].0.contains("mode=plan"));
    assert_eq!(seen[2].1, source.trim_end());
    assert_eq!(seen[3].0, "/v1/discover");
    assert!(seen.iter().all(|(_, body)| body.get("seeds").is_none()));
    task.abort();
}
