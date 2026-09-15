//! The same semantic operation and program over path- and body-carried identity.
#[path = "common/language_matrix.rs"]
mod language_matrix;

use axum::{
    body::to_bytes,
    extract::{Request, State},
    routing::any,
    Json, Router,
};
use plasm_agent::{
    plasm_compile::compile_plasm_program,
    plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp},
};
use plasm_core::{
    prompt_render::{render_prompt_tsv_with_config, RenderConfig},
    PromptPipelineConfig,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Writes = Arc<Mutex<Vec<(String, Value)>>>;

async fn endpoint(State(writes): State<Writes>, request: Request) -> Json<Value> {
    let path = request.uri().path().to_string();
    if request.method() == axum::http::Method::GET {
        let row = |id| json!({"id": id, "title": "before", "score": 1, "owner": "alice"});
        return Json(if path.ends_with("/items") {
            json!([row("i1"), row("i2")])
        } else {
            row(path.rsplit('/').next().unwrap())
        });
    }
    let bytes = to_bytes(request.into_body(), 65536).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| path.rsplit('/').next().unwrap())
        .to_string();
    writes.lock().unwrap().push((id.clone(), body.clone()));
    Json(json!({"id": id, "title": body["title"], "score": body["score"], "owner": body["owner"]}))
}

#[test]
fn semantic_receiver_transport_invariance_live() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(check_transports());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn check_transports() {
    let writes = Writes::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(any(endpoint))
        .with_state(writes.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut first_card = None;
    for (body_identity, query_only) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
        cgs.http_backend = base.clone();
        if query_only {
            cgs.capabilities.shift_remove("langitem_get");
        }
        if body_identity {
            let mapping = &mut cgs
                .capabilities
                .get_mut("langitem_update")
                .unwrap()
                .mapping
                .as_mut()
                .unwrap()
                .template
                .0;
            mapping["path"] = json!([{"type":"literal", "value":"change-item"}]);
            mapping["body"]["fields"]
                .as_array_mut()
                .unwrap()
                .push(json!(["id", {"type":"var", "name":"id"}]));
        }
        plasm_compile::validate_cgs_capability_templates(&cgs).expect("mapping coverage");
        if query_only && !body_identity {
            first_card = None;
        }
        let card = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("LangItem")))
            .lines()
            .filter(|line| !line.starts_with("evaluation_now\t"))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(expected) = &first_card {
            assert_eq!(&card, expected, "CML must not change teaching");
        } else {
            first_card = Some(card);
        }
        let cgs = Arc::new(cgs);
        for (program, expected_ids) in [
            ("LangItem(\"i1\").update(title=\"after\", score=2, owner=\"alice\")", vec!["i1"]),
            ("items = LangItem\none = items | take 1\nout = one.update(title=\"after\", score=2, owner=\"alice\")\nout", vec!["i1"]),
            ("items = LangItem\none = items | take 1 | select title\nout = one.update(title=\"after\", score=2, owner=\"alice\")\nout", vec!["i1"]),
            ("items = LangItem\nout = items => _.update(title=\"after\", score=2, owner=_.owner)\nout", vec!["i1", "i2"]),
            ("items = LangItem\none = items | where id = \"missing\" | take 1\nout = one.update(title=\"after\", score=2, owner=\"alice\")\nout", vec![]),
        ] {
            writes.lock().unwrap().clear();
            let es = language_matrix::matrix_execute_session(cgs.clone());
            let st = language_matrix::matrix_host_state(ExecutionEngine::new(ExecutionConfig {base_url: Some(base.clone()), ..Default::default()}).unwrap(), cgs.clone());
            let bundle = compile_plasm_program(&PromptPipelineConfig::default(), None, &es, "receiver", program)
                .unwrap_or_else(|e| panic!("compile {program}: {e}"));
            evaluate_plasm_comp_dry(&es, &bundle).unwrap_or_else(|e| panic!("dry {program}: {e}"));
            let live = Box::pin(run_plasm_comp(&es, &st, es.prompt_hash.as_str(), "receiver", &bundle, true, None, None, None, None)).await;
            if expected_ids.is_empty() {
                assert!(live.unwrap_err().contains("zero rows"));
            } else { live.unwrap_or_else(|e| panic!("live {program}: {e}")); }
            let observed = writes.lock().unwrap();
            let ids: Vec<_> = observed.iter().map(|(id, _)| id.as_str()).collect();
            assert_eq!(ids, expected_ids, "receiver identity must survive row selection/projection");
            for (_, payload) in observed.iter() {
                assert_eq!(payload["title"], "after");
                assert_eq!(payload["owner"], "alice");
                assert_eq!(payload.get("id").is_some(), body_identity);
            }
        }
    }
    server.abort();
}

#[test]
fn semantic_receiver_contract_defaults_and_explicit_overrides() {
    use plasm_core::{CapabilityKind, CapabilityReceiver};
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut cap = cgs.capabilities["langitem_update"].clone();
    cap.inputs.receiver = None;
    cap.mapping = None;
    for kind in [
        CapabilityKind::Get,
        CapabilityKind::Update,
        CapabilityKind::Delete,
    ] {
        cap.kind = kind;
        assert_eq!(cap.receiver_entity(), Some(&cap.domain));
    }
    for kind in [
        CapabilityKind::Query,
        CapabilityKind::Search,
        CapabilityKind::Create,
        CapabilityKind::Action,
    ] {
        cap.kind = kind;
        assert_eq!(cap.receiver_entity(), None);
    }
    cap.inputs.receiver = Some(CapabilityReceiver::Entity {
        entity: "LangItem".into(),
    });
    assert!(
        cap.requires_receiver(),
        "an action can declare its entity receiver"
    );
    cap.kind = CapabilityKind::Get;
    cap.inputs.receiver = Some(CapabilityReceiver::None);
    assert!(
        !cap.requires_receiver(),
        "receiver-free Get is explicitly semantic"
    );
    let mut invalid = (*cgs).clone();
    cap.inputs.receiver = Some(CapabilityReceiver::Entity {
        entity: "MissingEntity".into(),
    });
    assert!(invalid
        .add_capability(cap)
        .unwrap_err()
        .to_string()
        .contains("MissingEntity"));
}
