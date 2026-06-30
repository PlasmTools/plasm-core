//! Regression tests for query-cap teaching row emission (kept out of `mod.rs`).

use std::collections::HashMap;

use crate::loader::load_schema_dir;
use crate::symbol_tuning::{symbol_map_for_prompt, FocusSpec};

use super::{collect_entity_teaching_block, prompt_line_valid_cache_seed_cgs};

/// B5 — teaching round-trip guard. The teaching TSV *is* the language surface; a synthesized
/// exemplar that does not parse under the live parser is a generated-surface defect of the same
/// severity as a compiler bug. Render the table for the designated prompt-regression fixture and
/// assert every concrete (non-placeholder, non-metadata) `plasm_expr` cell round-trips the parser.
#[test]
fn teaching_tsv_exemplars_round_trip_parser() {
    use crate::expr_parser::parse_with_cgs_layers_program;
    use crate::PromptPipelineConfig;

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).expect("load plasm_prompt_matrix");
    // Use the same teaching-exposure symbol map the renderer uses so `e#`/`p#` numbering matches.
    let sym_map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");

    let tsv = PromptPipelineConfig::default().render_prompt_tsv(&cgs, None);
    let mut checked = 0usize;
    for line in tsv.lines() {
        let Some((expr, _meaning)) = line.split_once('\t') else {
            continue;
        };
        let expr = expr.trim();
        if expr.is_empty() || expr == "plasm_expr" {
            continue;
        }
        // Metadata-only rows (`p#` / `v#` gloss, union summary) are never executable exemplars.
        if expr
            .strip_prefix(['p', 'v'])
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        // Template rows carry placeholders / metadata sigils — not literal exemplars to parse.
        if expr.contains('<') || expr.contains("..") || expr.contains('$') {
            continue;
        }
        parse_with_cgs_layers_program(expr, &[&cgs], sym_map.clone(), None, false, None)
            .unwrap_or_else(|e| {
                panic!("teaching exemplar must round-trip the parser: `{expr}` -> {e:?}")
            });
        checked += 1;
    }
    assert!(
        checked > 0,
        "expected at least one concrete teaching exemplar to round-trip"
    );
}

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
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
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

/// B2 — simple string-id entities teach positional literals (e.g. `e#(pikachu)`), not `e#(p#)`.
#[test]
fn seeded_pokemon_identity_row_uses_positional_literal() {
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
    let map =
        symbol_map_for_prompt(&cgs, FocusSpec::SeedsExact(&["Pokemon"]), true).expect("symbol map");
    let pokemon_es = map.entity_sym("Pokemon");
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "Pokemon",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss_emit_none,
        Some(&delta.required),
        Some("pokeapi"),
    );
    let identity = block.teaching_rows.iter().find(|r| {
        r.teaching_expr
            .expression
            .contains(&format!("{pokemon_es}(pikachu)"))
    });
    assert!(
        identity.is_some(),
        "Pokemon identity row must teach positional literal `{pokemon_es}(pikachu)`, rows={:?}",
        block
            .teaching_rows
            .iter()
            .map(|r| r.teaching_expr.expression.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn linear_workflow_state_scoped_query_teaching_line_validates_with_opaque_p_sym() {
    use super::domain_example_line_count;
    use crate::loader::load_schema_dir_unvalidated;
    use crate::prompt_render::line_validate::{
        domain_line_validate_cached, prompt_line_valid_cache_seed_cgs,
    };
    use crate::symbol_tuning::{symbol_map_for_prompt, FocusSpec, SymbolMap};

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/linear");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir_unvalidated(&dir).expect("linear");
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("map");
    let es = map.entity_sym_for("", "WorkflowState");
    let p_team =
        map.ident_sym_cap_param_for("", "WorkflowState", "workflow_state_query", "team_key");
    assert!(
        SymbolMap::is_opaque_p_sym(p_team.as_str()),
        "team_key scope param must be opaque in full linear exposure"
    );
    let expr = format!("{es}{{{p_team}=$}}");
    let mut cache = std::collections::HashMap::new();
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    assert!(
        domain_line_validate_cached(&mut cache, seed, &cgs, &expr, Some(&map)).is_some(),
        "WorkflowState scoped query must validate with homograph p#: {expr}"
    );
    assert!(
        domain_example_line_count(&cgs, "WorkflowState", Some(map.as_ref())) > 0,
        "WorkflowState must synthesize teaching lines"
    );
}
