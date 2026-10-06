//! File-driven offline teaching experiment bridge. Never used by production admission.
use super::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn partial_patch_obeys_optional_fixture_inputs() {
    let result = execute(json!({"seeds":["LangItem"], "source":
        "class Patch(Program):\n    def build(self):\n        return e1.get('i1').m9(title='Reviewed').select('id','title')\n"})).await;
    assert_eq!(result["stage"], "complete", "{result}");
    let writes: Vec<_> = result["trace"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "PATCH")
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0]["path"], "/language/v1/items/i1");
    assert_eq!(writes[0]["body"], json!({"title":"Reviewed"}));
}

#[test]
#[ignore = "explicit offline experiment; PLASM_TEACHING_REQUEST and RESPONSE files required"]
fn offline_teaching_bridge() {
    let input = std::env::var("PLASM_TEACHING_REQUEST").expect("request path");
    let output = std::env::var("PLASM_TEACHING_RESPONSE").expect("response path");
    let request: Value = serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    let result = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(execute(request))
        })
        .unwrap()
        .join()
        .unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}

async fn execute(request: Value) -> Value {
    let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
    let client = reqwest::Client::new();
    if request["state"].as_u64() == Some(1) {
        let rows: Value = client
            .get(format!("{base}/language/v1/items"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        for (index, row) in rows.as_array().unwrap().iter().enumerate() {
            client
                .patch(format!(
                    "{base}/language/v1/items/{}",
                    row["id"].as_str().unwrap()
                ))
                .json(
                    &json!({"title":format!("Altered {index}"), "score": 21 + index * 7,
                    "owner": if index % 2 == 0 {"alice"} else {"bob"},
                    "status":"archived", "recorded_at":"2026-08-12T11:22:33Z",
                    "contact_email":"changed@example.test", "tag_labels":["alpha","gamma"]}),
                )
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
    }
    let trace = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = trace.clone();
    let issuances = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_issuances = issuances.clone();
    let upstream = base.clone();
    let fixture_row = request["fixture_row"].clone();
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let (client, trace, base) = (client.clone(), captured.clone(), upstream.clone());
        let fixture_row = fixture_row.clone();
        let issuances = captured_issuances.clone();
        async move {
            let method = req.method().clone();
            let path = req.uri().to_string();
            let headers = req.headers().clone();
            let bytes = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap();
            let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
            // Record dispatch before awaiting so the trace retains causal order.
            let request_index = {
                let mut trace = trace.lock().unwrap();
                let index = trace.len();
                trace.push(json!({"method":method.as_str(),"path":path,"body":body,
                    "authorization":headers.get("authorization").and_then(|h| h.to_str().ok())}));
                index
            };
            // Hermit's generated login examples may reuse one token for different
            // principals. This experiment requires distinct issuance identities
            // to test credential provenance across the federated fixture worlds.
            if path == "/language/v1/sessions/login" {
                let token = format!("offline-issued-{request_index}");
                issuances.lock().unwrap().push(json!({
                    "request_index": request_index,
                    "authorization": format!("Bearer {token}"),
                    "issuer": {"path": path, "body": body}
                }));
                return axum::response::Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({"access_token": token, "token_type": "Bearer"}))
                            .unwrap(),
                    ))
                    .unwrap();
            }
            if path.starts_with("/samples/") || path.starts_with("/records/") {
                return axum::response::Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&fixture_row).unwrap(),
                    ))
                    .unwrap();
            }
            let response = client
                .request(method, format!("{base}{path}"))
                .headers(headers)
                .body(bytes)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.bytes().await.unwrap();
            axum::response::Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(bytes))
                .unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let case = request["matrix_context"]
        .as_str()
        .and_then(|id| python::cases().find(|c| c.id == id))
        .unwrap_or(&python::CASES[0]);
    let (mut session, host) = python::parity_context(case, &proxy);
    let mut host = host;
    if let Some(schema) = request["schema"].as_str() {
        assert!(["python_value_contract", "python_union_matrix"].contains(&schema));
        let mut cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas")
                .join(schema),
        )
        .unwrap();
        cgs.bind_registry_entry_id(language_matrix::MATRIX_ENTRY_ID);
        cgs.http_backend = proxy.clone();
        let cgs = Arc::new(cgs);
        let exposure = plasm_core::TeachingExposureSession::new(
            &cgs,
            language_matrix::MATRIX_ENTRY_ID,
            &[if schema == "python_union_matrix" {
                "Record"
            } else {
                "Sample"
            }],
        );
        let contexts = indexmap::IndexMap::from([(
            language_matrix::MATRIX_ENTRY_ID.into(),
            Arc::new(plasm_core::CgsContext::entry(
                language_matrix::MATRIX_ENTRY_ID,
                cgs.clone(),
            )),
        )]);
        session = plasm_agent::execute_session::ExecuteSession::new(
            "offline".into(),
            String::new(),
            cgs.clone(),
            contexts,
            language_matrix::MATRIX_ENTRY_ID.into(),
            String::new(),
            String::new(),
            None,
            Vec::new(),
            Some(exposure),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
        );
        host = language_matrix::matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(proxy.clone()),
                ..Default::default()
            })
            .unwrap(),
            cgs,
        );
    }
    if let Some(seeds) = request["seeds"].as_array() {
        let seeds: Vec<_> = seeds.iter().map(|v| v.as_str().unwrap()).collect();
        session.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
            &session.cgs,
            language_matrix::MATRIX_ENTRY_ID,
            &seeds,
        ));
    }
    let exposure = session.teaching_exposure.as_ref().unwrap();
    let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
        exposure,
        &Default::default(),
    )
    .unwrap();
    // Read-only comparison with the retired reference renderer; never admission.
    let historical_pipeline = plasm_core::PromptPipelineConfig::default();
    let mut historical = if request["historical_comparison"] == true {
        let by_entry = session
            .contexts_by_entry
            .iter()
            .map(|(entry, context)| (entry.clone(), context.cgs.as_ref()))
            .collect();
        Some(
            historical_pipeline
                .render_teaching_first_wave_for_session_federated(&by_entry, exposure, None),
        )
    } else {
        None
    };
    let mut teaching = wave.declarations.clone();
    session.python_teaching = wave.next_state.clone();
    let first_state = serde_json::to_value(&wave.next_state).unwrap();
    if let Some(extend) = request["extend"].as_array() {
        let names: Vec<_> = extend.iter().map(|v| v.as_str().unwrap()).collect();
        let cgs = session.cgs.clone();
        session.teaching_exposure.as_mut().unwrap().expose_entities(
            &[&cgs],
            cgs.clone(),
            language_matrix::MATRIX_ENTRY_ID,
            &names,
        );
        let next = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            session.teaching_exposure.as_ref().unwrap(),
            &session.python_teaching,
        )
        .unwrap();
        if let Some(historical) = historical.as_mut() {
            historical.push_str(&historical_pipeline.render_teaching_exposure_delta(
                &cgs,
                session.teaching_exposure.as_ref().unwrap(),
                &names,
                None,
            ));
        }
        teaching.push_str("\n# Incremental exposure\n");
        teaching.push_str(&next.declarations);
        session.python_teaching = next.next_state;
    }
    let mut result = json!({"language":wave.language, "teaching":teaching,
        "state":session.python_teaching, "first_state":first_state,
        "values":session.cgs.values, "cgs":session.cgs,
        "contracts":wave.value_contracts});
    if let Some(historical) = historical {
        result["historical_teaching"] = json!(historical);
    }
    if let Some(source) = request["source"].as_str() {
        let pipeline = Default::default();
        match plasm_agent::compile_program(&pipeline, None, &session, "program", source).await {
            Err(
                error @ (plasm_agent::compilation_error::CompilationError::Host(_)
                | plasm_agent::compilation_error::CompilationError::Checker(_)),
            ) => {
                let failure: plasm_runtime::ExecutionFailure = error.into();
                result["stage"] = json!("host_failure");
                result["failure"] = json!(failure);
            }
            Err(plasm_agent::compilation_error::CompilationError::Program(error)) => {
                let diagnostic = plasm_agent::program_diagnostic::ProgramDiagnostic::from_stage(
                    &pipeline, None, &session, source, *error,
                );
                result["stage"] = json!("admission");
                result["diagnostic"] = json!(diagnostic.agent_markdown());
                result["diagnostic_meta"] = json!(diagnostic.agent_meta("offline", 0));
            }
            Ok(bundle) => {
                result["plan"] = serde_json::to_value(&bundle.artifact().comp).unwrap();
                match evaluate_plasm_comp_dry(&session, &bundle) {
                    Err(error) => {
                        result["stage"] = json!("planning");
                        result["error"] = json!(error.to_string());
                    }
                    Ok(dry) => {
                        assert_comp_witness(&dry).unwrap();
                        let run = plasm_agent::plasm_plan_run::run_plasm_comp(
                            &session,
                            &host,
                            &session.prompt_hash,
                            "offline-teaching",
                            &bundle,
                            true,
                            None,
                            None,
                            Some(dry),
                            None,
                        )
                        .await;
                        match run {
                            Ok(run) => {
                                result["stage"] = json!("complete");
                                result["outputs"] = json!(python::outputs(&run));
                                result["coverage"] = json!(run
                                    .return_steps
                                    .iter()
                                    .map(|step| step.result.coverage())
                                    .collect::<Vec<_>>());
                            }
                            Err(error) => {
                                result["stage"] = json!("runtime");
                                result["error"] = json!(error);
                            }
                        }
                    }
                }
            }
        }
    }
    result["trace"] = json!(*trace.lock().unwrap());
    result["auth_issuances"] = json!(*issuances.lock().unwrap());
    server.abort();
    result
}
