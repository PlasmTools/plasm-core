//! Regression tests for query-cap teaching row emission (kept out of `mod.rs`).

use std::collections::HashMap;

use crate::loader::load_schema_dir;
use crate::symbol_tuning::{symbol_map_for_prompt, FocusSpec};

use super::{collect_entity_teaching_block, prompt_line_valid_cache_seed_cgs};

#[test]
fn seeded_pokemon_teaching_includes_bare_query_row() {
    use crate::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    if !dir.is_dir() {
        return;
    }
    let mut cgs = load_schema_dir(&dir).expect("pokeapi");
    cgs.entry_id = Some("pokeapi".into());
    let endpoints = crate::relation_endpoint_keys("pokeapi", &["Pokemon".to_string()]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "pokeapi",
        "electric type pokemon chart",
        &endpoints,
        &["Pokemon".to_string()],
        None,
        ExposureSurfaceOptions {
            read_first_seeded: true,
        },
    );
    assert!(
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == "pokemon_query"),
        "seeded Pokemon must expose pokemon_query on surface"
    );
    let map =
        symbol_map_for_prompt(&cgs, FocusSpec::SeedsExact(&["Pokemon"]), true).expect("symbol map");
    let pokemon_es = map.entity_sym("Pokemon");
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "Pokemon",
        Some(map.as_ref()),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        None,
        &mut gloss_emit_none,
        Some(&delta.required),
        Some("pokeapi"),
    );
    let bare_query = block
        .teaching_rows
        .iter()
        .any(|r| r.teaching_expr.expression.as_str() == pokemon_es.as_str());
    assert!(
        bare_query,
        "seeded Pokemon with pokemon_query on surface must teach bare query row `{pokemon_es}`; exprs={:?}",
        block
            .teaching_rows
            .iter()
            .map(|r| r.teaching_expr.expression.as_str())
            .collect::<Vec<_>>()
    );
}
