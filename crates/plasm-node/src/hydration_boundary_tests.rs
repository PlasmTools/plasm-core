use super::*;
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::Value;
use plasm_runtime::{auth::ResolvedAuth, RuntimeError};
use serde_json::json;
use std::sync::Mutex;
struct Transport {
    calls: Arc<Mutex<Vec<String>>>,
    token: String,
    ids: Vec<i64>,
    full_embed: bool,
    delayed: bool,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send_compiled_http(
        &self,
        _: &str,
        req: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.calls.lock().unwrap().push(req.path.clone());
        let parts: Vec<_> = req.path.trim_matches('/').split('/').collect();
        let body = match parts.as_slice() {
            ["login"] => json!({"access_token":self.token}),
            ["saved", id] => json!({"note_id":id.parse::<i64>().unwrap()}),
            ["saved"] => json!(self
                .ids
                .iter()
                .map(|id| json!({"note_id":id}))
                .collect::<Vec<_>>()),
            ["folders", _] => {
                json!({"id":"root", "notes":self.ids.iter().map(|id| json!({"note_id":id})).collect::<Vec<_>>()})
            }
            ["notes", id] => {
                let id: i64 = id.parse().expect("note identity must be a wire integer");
                {
                    if self.delayed {
                        tokio::time::sleep(std::time::Duration::from_millis((id % 3) as u64)).await;
                    }
                    let owner = if self.full_embed && id % 2 == 0 {
                        json!({"owner_id":id,"name":format!("owner-{id}"),"_tag":format!("tag-{id}"),"description":null})
                    } else {
                        json!({"owner_id":id})
                    };
                    json!({"note_id":id,"title":format!("note-{id}"),"owners":[owner]})
                }
            }
            ["owners", id] => {
                let id: i64 = id
                    .parse()
                    .expect("owner identity must be a wire integer, never a display Ref");
                json!({"owner_id":id,"name":format!("owner-{id}"),"_tag":format!("tag-{id}"),"description":null})
            }
            _ => panic!("unexpected path {}", req.path),
        };
        if matches!(
            parts.first(),
            Some(&"folders") | Some(&"notes") | Some(&"saved")
        ) {
            let Some(Value::Object(headers)) = &req.headers else {
                panic!("missing headers");
            };
            assert_eq!(
                headers.get("Authorization"),
                Some(&Value::String(self.token.clone()))
            );
        }
        Ok((body, None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute GET")
    }
}

#[test]
fn native_boundary_hydration_repeated_live() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_native_hydration());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn run_native_hydration() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/hydration_boundary_matrix");
    let mut cgs = plasm_core::load_schema(&dir).unwrap();
    cgs.bind_registry_entry_id("matrix");
    let cgs =
        serde_json::from_slice::<plasm_core::CGS>(&serde_json::to_vec(&cgs).unwrap()).unwrap();
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let mut engine = AgentEngine::from_generation(
        [("matrix".into(), cgs)].into(),
        [("matrix".into(), compiled)].into(),
        "hydration-session".into(),
    );
    let seeds = ["Session", "SavedNote", "Note", "Owner"].map(|entity| CapabilitySeed {
        entry_id: "matrix".into(),
        entity: entity.into(),
    });
    engine
        .expose_seeds("read notes and their owners", &seeds)
        .unwrap();
    let transport = Arc::new(Transport {
        calls: Default::default(),
        token: "token".into(),
        ids: (1..=18).collect(),
        full_embed: true,
        delayed: true,
    });
    let program = "auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => Note(_.note_id)\nowners = notes => _.owners\nowners";
    for _ in 0..2 {
        let dry = engine.dry_run(program).unwrap();
        let result = engine
            .run_plan_live(&dry.plan_commit_ref, transport.clone())
            .await
            .unwrap();
        assert!(result.ok, "{}", result.message);
        let envelope: serde_json::Value =
            serde_json::from_str(result.rows_json.as_deref().unwrap()).unwrap();
        // The explicit return is followed by the provider's operation receipt.
        let ids: Vec<_> = envelope[0]["rows"]
            .as_array()
            .expect("owners rows")
            .iter()
            .map(|row| row["owner_id"].as_i64().expect("integer owner identity"))
            .collect();
        assert_eq!(ids, (1..=18).collect::<Vec<_>>());
    }
}
