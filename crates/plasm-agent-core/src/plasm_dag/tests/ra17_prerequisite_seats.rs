//! RA-17: consumer-login output on the source seat is a compile reject.

use super::*;
use plasm_core::prerequisites::{CapabilityRef, DeploymentBinding, DeploymentBindings};
use plasm_core::{CgsContext, PromptPipelineConfig, TeachingExposureSession};
use std::path::PathBuf;
use std::sync::Arc;

fn dual_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/prerequisite_dual_session");
    let source = Arc::new(
        plasm_core::loader::load_schema_dir(&root.join("source")).expect("source catalog"),
    );
    let mut consumer = plasm_core::loader::load_schema_dir(&root.join("consumer")).expect("consumer catalog");
    let mut read_requirement = consumer.prerequisites.requirements["attach"][0].clone();
    read_requirement.bindings[0].input.lane = plasm_core::prerequisites::InputLane::Arguments;
    consumer.prerequisites.requirements.insert("record_get".into(), vec![read_requirement]);
    let consumer = Arc::new(consumer);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "source".into(),
        Arc::new(CgsContext::entry("source", source.clone())),
    );
    ctxs.insert(
        "consumer".into(),
        Arc::new(CgsContext::entry("consumer", consumer.clone())),
    );
    let layers: Vec<&CGS> = vec![source.as_ref(), consumer.as_ref()];
    let mut exp = TeachingExposureSession::new(source.as_ref(), "source", &["AuthSession"]);
    exp.expose_entities(
        &layers,
        consumer.clone(),
        "consumer",
        &["AuthSession", "Record"],
    );
    let mut session = ExecuteSession::new(
        "ph".into(),
        "p".into(),
        consumer.clone(),
        ctxs,
        "consumer".into(),
        String::new(),
        String::new(),
        None,
        vec!["AuthSession".into(), "Record".into()],
        Some(exp),
        None,
        consumer.catalog_cgs_hash_hex(),
        None,
    );
    let business = CapabilityRef {
        catalog: "consumer".into(),
        capability: "attach".into(),
    };
    session.set_prerequisite_deployments(DeploymentBindings {
        bindings: vec![
            DeploymentBinding {
                consumer: CapabilityRef { catalog: "consumer".into(), capability: "record_get".into() },
                requirement: "session".into(), provider_catalog: "consumer".into(), provider: "session".into(),
            },
            DeploymentBinding {
                consumer: business.clone(),
                requirement: "session".into(),
                provider_catalog: "consumer".into(),
                provider: "session".into(),
            },
            DeploymentBinding {
                consumer: business,
                requirement: "source_session".into(),
                provider_catalog: "source".into(),
                provider: "session".into(),
            },
        ],
    });
    session
}

#[test]
fn ra17_rejects_consumer_login_on_source_seat() {
    let session = dual_session();
    let map = session
        .teaching_exposure
        .as_ref()
        .expect("exposure")
        .symbol_map_arc();
    let e_sw = map.entity_sym_for("consumer", "AuthSession");
    let e_rec = map.entity_sym_for("consumer", "Record");
    let m_login = map.method_sym_for("consumer", "AuthSession", "login");
    let m_attach = map.method_sym_for("consumer", "Record", "attach");
    let source = format!(
        r#"sw = {e_sw}.{m_login}()
done = {e_rec}("rec-1").{m_attach}(access_token=sw.access_token, file_path="/tmp/a", source_access_token=sw.access_token)
done"#
    );
    let err = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "ra17-foreign-seat",
        &source,
    )
    .expect_err("consumer token on source seat must compile-reject");
    let msg = err.to_string();
    assert!(
        msg.contains("source:session"),
        "reject must name required provider:\n{msg}"
    );
    assert!(
        msg.contains("source_access_token") || msg.contains("RA-17"),
        "reject must name the seat or law:\n{msg}"
    );
    assert!(
        !msg.contains("401"),
        "must not be a vendor status:\n{msg}"
    );
}

#[test]
fn ra17_accepts_matching_acquisitions() {
    let session = dual_session();
    let map = session
        .teaching_exposure
        .as_ref()
        .expect("exposure")
        .symbol_map_arc();
    let e_sw = map.entity_sym_for("consumer", "AuthSession");
    let e_fs = map.entity_sym_for("source", "AuthSession");
    let e_rec = map.entity_sym_for("consumer", "Record");
    let m_sw = map.method_sym_for("consumer", "AuthSession", "login");
    let m_fs = map.method_sym_for("source", "AuthSession", "login");
    let m_attach = map.method_sym_for("consumer", "Record", "attach");
    let source = format!(
        r#"sw = {e_sw}.{m_sw}()
fs = {e_fs}.{m_fs}()
done = {e_rec}("rec-1").{m_attach}(access_token=sw.access_token, file_path="/tmp/a", source_access_token=fs.access_token)
done"#
    );
    compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "ra17-lawful-seats",
        &source,
    )
    .expect("matching provider catalogs must compile");
}

#[tokio::test]
async fn python_writes_keep_qualified_prerequisite_seats() {
    let session = dual_session();
    let symbols = session.teaching_exposure.as_ref().unwrap().symbol_map_arc();
    let consumer = symbols.entity_sym_for("consumer", "AuthSession");
    let source = symbols.entity_sym_for("source", "AuthSession");
    let record = symbols.entity_sym_for("consumer", "Record");
    let consumer_login = symbols.method_sym_for("consumer", "AuthSession", "login");
    let source_login = symbols.method_sym_for("source", "AuthSession", "login");
    let attach = symbols.method_sym_for("consumer", "Record", "attach");
    let code = format!("class Attach(Program):\n    def build(self):\n        sw = {consumer}.{consumer_login}()\n        fs = {source}.{source_login}()\n        row = {record}.get(\"rec-1\", access_token=sw.access_token)\n        done = row.{attach}(access_token=sw.access_token, file_path=\"/tmp/a\", source_access_token=fs.access_token)\n        return done\n");
    crate::plasm_compile::compile_python_program(&session, &code).await.expect("matching seats");
    let foreign_read = code.replace("\"rec-1\", access_token=sw.", "\"rec-1\", access_token=fs.");
    let error = crate::plasm_compile::compile_python_program(&session, &foreign_read).await.unwrap_err();
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    let mut wrong_provider = false;
    while let Some(error) = cause {
        wrong_provider |= matches!(error.downcast_ref::<plasm_core::prerequisites::PrerequisiteError>(),
            Some(plasm_core::prerequisites::PrerequisiteError::WrongDeployedProviderSource { .. }));
        let dag = error.downcast_ref::<crate::plasm_dag::error::DagCompilationError>()
            .or_else(|| error.downcast_ref::<std::sync::Arc<crate::plasm_dag::error::DagCompilationError>>().map(std::sync::Arc::as_ref));
        wrong_provider |= matches!(dag,
            Some(crate::plasm_dag::error::DagCompilationError::Prerequisite(
                plasm_core::prerequisites::PrerequisiteError::WrongDeployedProviderSource { source_catalog, provider_catalog, provider }
            )) if source_catalog == "source" && provider_catalog == "consumer" && provider == "session");
        cause = error.source();
    }
    assert!(wrong_provider, "read must preserve the typed provider rejection: {error}");
    let fanout = code.replace("done = row.", "done = row.flat_map(lambda item: item.").replace("source_access_token=fs.access_token)", "source_access_token=fs.access_token))");
    crate::plasm_compile::compile_python_program(&session, &fanout).await.expect("matching fanout captures");
    let error = crate::plasm_compile::compile_python_program(&session, &fanout.replace("source_access_token=fs.", "source_access_token=sw.")).await.unwrap_err();
    assert!(error.to_string().contains("source:session"), "{error}");

    let error = crate::plasm_compile::compile_python_program(&session, &code.replace("source_access_token=fs.", "source_access_token=sw.")).await.unwrap_err();
    assert!(error.to_string().contains("source:session"), "{error}");
}
