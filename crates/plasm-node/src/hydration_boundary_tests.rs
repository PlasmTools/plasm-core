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
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
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
            ["folders", "missing"] => json!({"id":"missing"}),
            ["folders", "empty"] => json!({"id":"empty", "notes":[]}),
            ["folders", id] => {
                json!({"id":id, "notes":self.ids.iter().map(|id| json!({"note_id":id})).collect::<Vec<_>>()})
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
    ) -> std::result::Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute GET")
    }
}

#[test]
fn native_boundary_hydration_repeated_live() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
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
    let seeds = ["Session", "SavedNote", "Note", "Owner", "Folder"].map(|entity| CapabilitySeed {
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
    let exposure = engine.exposure.as_ref().unwrap();
    let symbol = |name| exposure.qualified_entity_symbol("matrix", name).unwrap();
    let session = symbol("Session");
    let saved = symbol("SavedNote");
    let note = symbol("Note");
    let folder = symbol("Folder");
    let cgs = &engine.catalogs["matrix"];
    let login = plasm_core::prompt_render::python::capability_method_name(
        cgs,
        &exposure.to_symbol_map(),
        "matrix",
        &cgs.capabilities["login"],
    );
    let program = format!("class Read(Program):\n    def build(self):\n        auth = {session}.{login}()\n        saved = {saved}.query(access_token=auth.access_token)\n        notes = saved.flat_map(lambda row: {note}.get(row.note_id))\n        return notes.flat_map(lambda row: row.owners)\n");
    let owner = symbol("Owner");
    let owner_relation = plasm_core::symbol_tuning::SymbolRender::ident_sym_relation_for(
        &exposure.to_symbol_map(),
        "matrix",
        "Note",
        "owners",
    );
    let mapped_program = format!("class MapNotes(Program):\n    @compute\n    def names(self, owners: list[Value[{owner}]]) -> str:\n        return '|'.join(owner.name for owner in owners)\n    @compute\n    def document(self, rows: list[Row]) -> str:\n        return '\\n'.join(row.title + ':' + row.names for row in rows)\n    def build(self):\n        auth = {session}.{login}()\n        notes = {folder}.get(\"root\").notes.distinct(\"note_id\").order_by(\"note_id\")\n        mapped = notes.map(lambda note: {{\"title\": note.title, \"names\": self.names(note.{owner_relation})}}, max_parents=32)\n        return self.document(mapped)\n");
    let dry = engine.dry_run(&mapped_program).await.unwrap();
    let result = engine
        .run_plan_live(&dry.plan_commit_ref, transport.clone())
        .await
        .unwrap();
    assert!(result.ok, "{}", result.message);
    let envelope: serde_json::Value =
        serde_json::from_str(result.rows_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        envelope[0]["rows"][0]["value"],
        json!((1..=18)
            .map(|i| format!("note-{i}:owner-{i}"))
            .collect::<Vec<_>>()
            .join("\n"))
    );
    let folder_program = format!("class ReadFolder(Program):\n    def build(self):\n        auth = {session}.{login}()\n        notes = {folder}.get(\"root\").notes\n        return notes.aggregate(n=agg.count())\n");
    let dry = engine.dry_run(&folder_program).await.unwrap();
    let result = engine
        .run_plan_live(&dry.plan_commit_ref, transport.clone())
        .await
        .unwrap();
    assert!(result.ok, "{}", result.message);
    let envelope: serde_json::Value =
        serde_json::from_str(result.rows_json.as_deref().unwrap()).unwrap();
    assert_eq!(envelope[0]["rows"][0]["n"], json!(18));
    let compute_program = format!("class CountFolder(Program):\n    @compute\n    def size(self, rows: list[Value[{note}]]) -> str:\n        return str(len(rows))\n    def build(self):\n        auth = {session}.{login}()\n        notes = {folder}.get(\"root\").notes\n        return self.size(notes.select(\"note_id\"))\n");
    let dry = engine.dry_run(&compute_program).await.unwrap();
    let result = engine
        .run_plan_live(&dry.plan_commit_ref, transport.clone())
        .await
        .unwrap();
    assert!(result.ok, "{}", result.message);
    let envelope: serde_json::Value =
        serde_json::from_str(result.rows_json.as_deref().unwrap()).unwrap();
    assert_eq!(envelope[0]["rows"][0]["value"], json!("18"));
    let derived_program = compute_program
        .replace(&format!("list[Value[{note}]]"), "list[Row]")
        .replace("return self.size(notes.select(\"note_id\"))", "ids = notes.select(\"note_id\")\n        members = notes.where(lambda row: row.note_id in ids).select(\"note_id\")\n        combined = members.union(ids).distinct(\"note_id\")\n        return self.size(combined)");
    let dry = engine.dry_run(&derived_program).await.unwrap();
    let result = engine
        .run_plan_live(&dry.plan_commit_ref, transport.clone())
        .await
        .unwrap();
    assert!(result.ok, "{}", result.message);
    let envelope: serde_json::Value =
        serde_json::from_str(result.rows_json.as_deref().unwrap()).unwrap();
    assert_eq!(envelope[0]["rows"][0]["value"], json!("18"));

    for (identity, edge, expected) in [("empty", "notes", Some("0")), ("missing", "notes", None)] {
        let program =
            compute_program.replace("get(\"root\").notes", &format!("get({identity:?}).{edge}"));
        let dry = engine.dry_run(&program).await.unwrap();
        let result = engine
            .run_plan_live(&dry.plan_commit_ref, transport.clone())
            .await;
        if let Some(expected) = expected {
            let result = result.unwrap();
            assert!(result.ok, "{}", result.message);
            let envelope: serde_json::Value =
                serde_json::from_str(result.rows_json.as_deref().unwrap()).unwrap();
            assert_eq!(envelope[0]["rows"][0]["value"], json!(expected));
        } else {
            let result = result.unwrap();
            assert!(
                !result.ok,
                "incomplete input must not produce a compute result"
            );
            let failure: plasm_runtime::ExecutionFailure = serde_json::from_str(
                result
                    .failure_json
                    .as_ref()
                    .expect("typed runtime rejection"),
            )
            .unwrap();
            assert_eq!(
                failure.recovery,
                plasm_runtime::RecoveryDisposition::ReconcileEffects
            );
            assert!(failure
                .effects
                .iter()
                .any(|effect| effect.capability == "login" && effect.completed == 1));
            assert!(result.rows_json.is_none());
        }
    }

    for _ in 0..2 {
        let dry = engine.dry_run(&program).await.unwrap();
        let result = engine
            .run_plan_live(&dry.plan_commit_ref, transport.clone())
            .await
            .unwrap();
        assert!(result.ok, "{}", result.message);
        let envelope: serde_json::Value =
            serde_json::from_str(result.rows_json.as_deref().unwrap()).unwrap();
        // The explicit return is followed by the provider's operation receipt.
        assert!(envelope[1]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| {
                operation["capability"] == "login" && operation["completed"] == 1
            }));
        let ids: Vec<_> = envelope[0]["rows"]
            .as_array()
            .expect("owners rows")
            .iter()
            .map(|row| row["owner_id"].as_i64().expect("integer owner identity"))
            .collect();
        assert_eq!(ids, (1..=18).collect::<Vec<_>>());
    }
}
