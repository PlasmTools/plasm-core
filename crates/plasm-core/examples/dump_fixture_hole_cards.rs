//! Dump focused fixture teaching TSV for the hole-fill instruction ladder.
//!
//! ```text
//! cargo run -p plasm-core --example dump_fixture_hole_cards -- \
//!   scripts/appworld/cuga/ablation_offline/hole_fill_instruction/assets/cards
//! ```

use plasm_core::loader::load_schema_dir;
use plasm_core::PromptPipelineConfig;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURES: &[(&str, &[&str])] = &[
    ("plasm_prompt_matrix", &["Zone", "SecurityOverview"]),
    (
        "plasm_language_matrix",
        &[
            "LangItem",
            "LangSecuredNote",
            "LangAuthSession",
            "CompoundBranch",
        ],
    ),
    (
        "plasm_language_matrix_views",
        &["LangItem", "LangSecuredNote", "LangKeyPick", "LangDigest"],
    ),
];

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas")
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                "../../../scripts/appworld/cuga/ablation_offline/hole_fill_instruction/assets/cards",
            )
        });
    fs::create_dir_all(&out).expect("mkdir cards");
    let pipeline = PromptPipelineConfig::default();
    let mut cards = Vec::new();
    for (fixture, entities) in FIXTURES {
        let dir = fixtures_root().join(fixture);
        let cgs = load_schema_dir(&dir).unwrap_or_else(|e| panic!("load {fixture}: {e}"));
        for entity in *entities {
            let tsv = pipeline.render_prompt_tsv(&cgs, Some(entity));
            let name = format!("{fixture}__{entity}.tsv");
            write(&out.join(&name), &tsv);
            cards.push(json!({
                "fixture": fixture,
                "entity": entity,
                "file": name,
                "id_field": cgs
                    .entities
                    .iter()
                    .find(|(n, _)| n.as_str() == *entity)
                    .map(|(_, e)| e.id_field.as_str().to_string()),
            }));
        }
    }
    write(
        &out.join("manifest.json"),
        &serde_json::to_string_pretty(&json!({
            "dump": "hfi_v1",
            "cards": cards,
        }))
        .unwrap(),
    );
}

fn write(path: &Path, body: &str) {
    fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    eprintln!("wrote {} ({} bytes)", path.display(), body.len());
}
