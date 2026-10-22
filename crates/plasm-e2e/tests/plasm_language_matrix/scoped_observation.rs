//! Argument-filtered parent observations must not share incompatible complete relations.
use plasm_agent::{execute_session::ExecuteSession, plasm_compile::compile_python_program};
use plasm_core::{symbol_tuning::SymbolRender, CgsContext, TeachingExposureSession};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use serde_json::json;
use std::sync::Arc;

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn scoped_reads(
    distinct_parent: bool,
    distinct_members: bool,
    whole: bool,
) -> Result<plasm_agent::plasm_plan_run::PlasmPlanRunResult, plasm_runtime::ExecutionFailure> {
    let app = axum::Router::new().route(
        "/collections/{id}",
        axum::routing::get(
            |axum::extract::Path(_id): axum::extract::Path<String>,
             axum::extract::Query(query): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >| async move {
                let members = query
                    .get("member")
                    .map(|id| json!([{"id":id}]))
                    .unwrap_or_else(|| json!([{"id":"left"}, {"id":"right"}]));
                // The parent identity comes from the request, not the response.
                axum::Json(json!({"members":members}))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let path = std::path::Path::new(plasm_e2e::test_manifest_dir())
        .join("../../fixtures/schemas/scoped_observation");
    let mut cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
    cgs.http_backend = base.clone();
    cgs.bind_registry_entry_id("matrix");
    let cgs = Arc::new(cgs);
    let entity = if whole {
        "WholeCollection"
    } else {
        "Collection"
    };
    let exposure = TeachingExposureSession::new(&cgs, "matrix", &[entity]);
    let token = exposure.to_symbol_map().entity_sym_for("matrix", entity);
    let es = ExecuteSession::new(
        "scoped-observation".into(),
        String::new(),
        cgs.clone(),
        indexmap::IndexMap::from([(
            "matrix".into(),
            Arc::new(CgsContext::entry("matrix", cgs.clone())),
        )]),
        "matrix".into(),
        String::new(),
        String::new(),
        None,
        vec![entity.into()],
        Some(exposure),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    );
    let host = super::language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .unwrap(),
        cgs,
    );
    let reads = (0..20)
        .map(|i| {
            let parent = if distinct_parent {
                format!("parent{i}")
            } else {
                "shared".into()
            };
            let member = if distinct_members {
                format!("member{i}")
            } else {
                "same".into()
            };
            let arguments = if whole {
                String::new()
            } else {
                format!(", member='{member}'")
            };
            format!("        r{i} = {token}.get('{parent}'{arguments})\n")
        })
        .collect::<String>();
    let returns = (0..20)
        .map(|i| format!("r{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let source =
        format!("class Observe(Program):\n    def build(self):\n{reads}        return {returns}\n");
    let bundle = compile_python_program(&es, &source).await.unwrap();
    plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "scoped-observation",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
}

#[tokio::test]
async fn scoped_parent_reads_keep_distinct_complete_relations() {
    for (distinct_parent, distinct_members) in [(false, false), (true, true)] {
        scoped_reads(distinct_parent, distinct_members, false)
            .await
            .unwrap_or_else(|error| {
                panic!("parent={distinct_parent}, members={distinct_members}: {error:?}")
            });
    }
    // Divergent concurrent membership is covered deterministically by the runtime
    // materialization-conflict matrix. Retry exhaustion is schedule-dependent and
    // must not be required here: serialized reads can legally replace each other.
    let run = scoped_reads(false, false, true)
        .await
        .expect("whole-parent observations must coexist");
    assert_eq!(run.return_steps.len(), 20);
    for step in &run.return_steps {
        let entities = step.result.entities();
        assert_eq!(entities.len(), 1);
        let members = &entities[0].relations["members"];
        assert!(members.is_exhaustive());
        assert_eq!(members.references().len(), 2);
    }
}
