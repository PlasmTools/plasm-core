//! Provider sequencing and wire-roundtrip properties through real plan execution.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::{discovery::CgsRegistry, Value};
use plasm_runtime::{
    auth::ResolvedAuth, ExecutionConfig, ExecutionEngine, ExecutionMode, HttpTransport,
    RuntimeError,
};
use proptest::prelude::*;
use serde_json::json;

#[derive(Clone, Copy, Debug)]
enum LoginOutcome {
    Token,
    Empty,
    Failed,
}

struct Transport {
    observed: Arc<Mutex<Vec<String>>>,
    outcome: LoginOutcome,
    token: String,
}

#[async_trait]
impl HttpTransport for Transport {
    async fn send_compiled_http(
        &self,
        _: &str,
        req: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.observed.lock().unwrap().push(req.path.clone());
        if req.path.ends_with("/items/n1") {
            return Ok((
                json!({"id":"n1", "title":"fixture", "score":1, "owner":"u"}),
                None,
            ));
        }
        if req.path.ends_with("/sessions/login") {
            return match self.outcome {
                LoginOutcome::Token => Ok((
                    json!({"access_token":format!("{}-{}", self.token, self.observed.lock().unwrap().iter().filter(|p| p.ends_with("/sessions/login")).count()), "token_type":"bearer"}),
                    None,
                )),
                LoginOutcome::Empty => Ok((json!([]), None)),
                LoginOutcome::Failed => Err(RuntimeError::RequestError {
                    message: "fixture login rejected".into(),
                    attempts: 1,
                    status: Some(422),
                    body: None,
                }),
            };
        }
        assert!(
            req.path.contains("/secured_notes/"),
            "unexpected request {req:?}"
        );
        let Some(Value::Object(headers)) = &req.headers else {
            panic!("missing headers");
        };
        assert_eq!(
            headers.get("Authorization"),
            Some(&Value::String(format!(
                "Bearer {}-{}",
                self.token,
                req.path.rsplit('/').next().unwrap()
            ))),
            "must use the real login result"
        );
        Ok((
            json!({"note_id":req.path.rsplit('/').next().unwrap().parse::<i64>().unwrap(), "title":"Note", "body":"Authenticated content"}),
            None,
        ))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute GET")
    }
}

async fn run_case(
    token: String,
    delayed: bool,
    count: usize,
    applied: bool,
    outcome: LoginOutcome,
) {
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::server_state::CatalogBootstrap;
    let es = super::support::language_matrix_session();
    let observed = Arc::new(Mutex::new(vec![]));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            ..Default::default()
        },
        Arc::new(Transport {
            observed: observed.clone(),
            outcome,
            token,
        }),
        None,
    );
    let host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(CgsRegistry::from_pairs(vec![(
            "langmatrix".into(),
            "Matrix".into(),
            vec![],
            es.cgs.clone(),
        )])),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    let mut program = if delayed || applied {
        "user = LangItem(\"n1\")\n".to_string()
    } else {
        String::new()
    };
    for index in 1..=count {
        let username = if delayed { "user.title" } else { "\"u\"" };
        let get = if applied {
            "user => LangSecuredNote(_.score)".to_string()
        } else {
            format!("LangSecuredNote({index})")
        };
        program.push_str(&format!("auth{index} = LangAuthSession.login(username={username}, password=\"p\")\nnote{index} = {get}\n"));
    }
    program.push_str(&format!("note{count}"));
    let bundle = crate::compile_plasm_program(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        &es,
        "login-get",
        &program,
    )
    .expect("compile");
    // Exercise the persisted semantic contract, not only the compiler's in-memory result.
    let comp =
        serde_json::from_slice(&serde_json::to_vec(&bundle.artifact().comp).unwrap()).unwrap();
    let bundle = crate::PlasmCompBundle::new(
        crate::plasm_comp_wire::plasm_comp_artifact_from_comp(comp).unwrap(),
    )
    .unwrap();
    super::super::evaluate_plasm_comp_dry(&es, &bundle).expect("dry provider witness");
    assert!(
        observed.lock().unwrap().is_empty(),
        "dry must not send requests"
    );
    let result = Box::pin(super::super::run_plasm_comp(
        &es,
        &host,
        &es.prompt_hash,
        "provision-test",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await;
    let calls = observed.lock().unwrap();
    match outcome {
        LoginOutcome::Token => {
            assert!(result.is_ok(), "{result:?}");
            assert!(
                calls
                    .last()
                    .unwrap()
                    .ends_with(&format!("/secured_notes/{count}")),
                "{calls:?}"
            );
            assert_eq!(calls.len(), count * 2 + usize::from(delayed || applied));
        }
        _ => {
            assert!(result.is_err(), "failed/empty login cannot supply auth");
            assert!(
                calls.iter().any(|p| p.ends_with("/sessions/login")),
                "login must actually be attempted: {calls:?}"
            );
            assert!(
                !calls.iter().any(|p| p.contains("/secured_notes/")),
                "{calls:?}"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn session_provisions_roundtrip_executes_real_token_in_dependency_order(token in "[a-zA-Z0-9]{1,24}", delayed in any::<bool>(), count in 1usize..4, applied in any::<bool>()) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(run_case(token, delayed, if applied {1} else {count}, applied, LoginOutcome::Token));
    }
}

#[tokio::test]
async fn session_provisions_failed_or_empty_login_never_dispatches_get() {
    for outcome in [LoginOutcome::Empty, LoginOutcome::Failed] {
        run_case("unused-token".into(), true, 1, false, outcome).await;
    }
}
