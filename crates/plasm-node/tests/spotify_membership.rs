//! Intentional provider-catalog integration witness; not a language conformance test.
use plasm_agent_core::http_execute::CapabilitySeed;
use plasm_compile::CompiledRequest;
use plasm_node::AgentEngine;
use plasm_runtime::{auth::ResolvedAuth, HttpTransport, RuntimeError};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct CatalogTransport {
    paths: Mutex<Vec<String>>,
}
#[async_trait::async_trait]
impl HttpTransport for CatalogTransport {
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute fetch");
    }
    async fn send_compiled_http(
        &self,
        _: &str,
        request: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(Value, Option<String>), RuntimeError> {
        self.paths.lock().unwrap().push(request.path.clone());
        let rows = match request.path.as_str() {
            "/spotify/library/songs" => json!([{"song_id":1,"title":"direct"}]),
            "/spotify/library/albums" => json!([{"album_id":10,"title":"saved album"}]),
            "/spotify/library/playlists" => json!([{"playlist_id":20,"title":"saved playlist"}]),
            "/spotify/albums/10" => json!({"album_id":10,"title":"saved album","songs":[{"id":2}]}),
            "/spotify/playlists/20" => {
                json!({"playlist_id":20,"title":"saved playlist","songs":[3]})
            }
            "/spotify/songs/2" => json!({"song_id":2,"title":"album only"}),
            "/spotify/songs/3" => json!({"song_id":3,"title":"playlist only"}),
            "/spotify/albums/10/songs" => json!([{"song_id":2,"title":"album only"}]),
            "/spotify/playlists/20/songs" => json!([{"song_id":3,"title":"playlist only"}]),
            path => panic!("unexpected catalog request: {path}"),
        };
        Ok((rows, None))
    }
}

#[tokio::test]
async fn spotify_library_view_observes_direct_membership_without_containment_requests() {
    let path = std::env::var_os("PLASM_SPOTIFY_WITNESS_CATALOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/spotify")
        });
    let mut cgs = plasm_core::load_schema_dir(&path).unwrap();
    cgs.bind_registry_entry_id("spotify");
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let mut engine = AgentEngine::from_generation(
        [("spotify".into(), cgs)].into(),
        [("spotify".into(), compiled)].into(),
        "spotify-membership-witness".into(),
    );
    let teaching = engine
        .expose_seeds(
            "read saved membership",
            &[CapabilitySeed {
                entry_id: "spotify".into(),
                entity: "Library".into(),
            }],
        )
        .unwrap();
    // The single seeded entity owns e1 in this fresh session.
    assert!(teaching.prompt.contains("e1.query(access_token:"));
    let plan = engine.dry_run("class Membership(Program):\n    def build(self):\n        return e1.query(access_token='fixture-token')\n").await.unwrap();
    assert!(plan.failure_json.is_none(), "{}", plan.summary);
    let transport = Arc::new(CatalogTransport {
        paths: Mutex::new(vec![]),
    });
    let result = engine
        .run_plan_live(&plan.plan_commit_ref, transport.clone())
        .await
        .unwrap();
    assert!(result.ok, "{} {:?}", result.message, result.failure_json);
    let rows: Value = serde_json::from_str(result.rows_json.as_ref().unwrap()).unwrap();
    assert_eq!(rows["coverage"], "complete");
    assert_eq!(rows["rows"][0]["songs"], json!(["Song:1"]));
    assert_eq!(rows["rows"][0]["albums"], json!(["Album:10"]));
    assert_eq!(rows["rows"][0]["playlists"], json!(["Playlist:20"]));
    let paths: std::collections::BTreeSet<_> =
        transport.paths.lock().unwrap().iter().cloned().collect();
    assert_eq!(
        paths,
        [
            "/spotify/library/songs",
            "/spotify/library/albums",
            "/spotify/library/playlists"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}
