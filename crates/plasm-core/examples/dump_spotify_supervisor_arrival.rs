//! Dump Spotify + supervisor first-wave teaching TSV for 325 compose.
//!
//! ```text
//! cargo run -p plasm-core --example dump_spotify_supervisor_arrival -- \
//!   /path/to/tsv_order/snapshots
//! ```

use indexmap::IndexMap;
use plasm_core::loader::load_schema_dir_unvalidated;
use plasm_core::symbol_tuning::TeachingExposureSession;
use plasm_core::{PromptPipelineConfig, CGS};
use std::path::PathBuf;
use std::sync::Arc;

fn apis_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld")
}

fn load_bound(name: &str) -> CGS {
    let dir = apis_root().join(name);
    let mut cgs = load_schema_dir_unvalidated(&dir).unwrap_or_else(|e| panic!("load {name}: {e}"));
    cgs.bind_registry_entry_id(name);
    cgs
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                "../../../scripts/appworld/cuga/ablation_offline/react_drift/tsv_order/snapshots",
            )
        });
    std::fs::create_dir_all(&out).expect("mkdir snapshots");

    let spotify = Arc::new(load_bound("spotify"));
    let supervisor = Arc::new(load_bound("supervisor"));
    let spotify_arrival = ["Player", "LikedSong", "AuthSession"];
    let supervisor_arrival = ["AccountPassword", "Supervisor"];
    let mut exp = TeachingExposureSession::new(spotify.as_ref(), "spotify", &spotify_arrival);
    exp.expose_entities(
        &[spotify.as_ref(), supervisor.as_ref()],
        supervisor.clone(),
        "supervisor",
        &supervisor_arrival,
    );
    let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
    by_entry.insert("spotify".into(), spotify.as_ref());
    by_entry.insert("supervisor".into(), supervisor.as_ref());
    let tsv = PromptPipelineConfig::default()
        .render_teaching_first_wave_for_session_federated(&by_entry, &exp, None);
    let path = out.join("spotify_supervisor_arrival.live.tsv");
    std::fs::write(&path, &tsv).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    eprintln!("wrote {} ({} B)", path.display(), tsv.len());

    let genre_arrival = ["LikedSong", "Song", "AuthSession"];
    let mut genre_exp = TeachingExposureSession::new(spotify.as_ref(), "spotify", &genre_arrival);
    genre_exp.expose_entities(
        &[spotify.as_ref(), supervisor.as_ref()],
        supervisor.clone(),
        "supervisor",
        &supervisor_arrival,
    );
    let genre = PromptPipelineConfig::default()
        .render_teaching_first_wave_for_session_federated(&by_entry, &genre_exp, None);
    let genre_path = out.join("spotify_genre_arrival.live.tsv");
    std::fs::write(&genre_path, &genre)
        .unwrap_or_else(|e| panic!("write {}: {e}", genre_path.display()));
    eprintln!("wrote {} ({} B)", genre_path.display(), genre.len());
}
