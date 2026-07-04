//! Regression tests for query-cap teaching row emission (kept out of `mod.rs`).

use std::collections::HashMap;

use crate::loader::load_schema_dir;
use crate::symbol_tuning::{symbol_map_for_prompt, FocusSpec};

use super::{
    collect_entity_teaching_block, parse_trailing_projection_bracket,
    prompt_line_valid_cache_seed_cgs, RenderConfig, TEACHING_VALID_EXPR_MARKER,
    TSV_TEACHING_TABLE_HEADER,
};

fn matrix_fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_prompt_matrix")
}

fn meaning_cells(tsv: &str) -> impl Iterator<Item = &str> {
    tsv.lines()
        .skip(1)
        .filter_map(|line| line.split_once('\t').map(|(_, m)| m))
}

fn assert_meaning_cells_no_legacy_opt_prefix(tsv: &str) {
    for meaning in meaning_cells(tsv) {
        assert!(
            !meaning.contains("opt:"),
            "Meaning column must use compact `optional` token, not `opt:` lists: {meaning:?}"
        );
    }
}

/// B5 — teaching round-trip guard. The teaching TSV *is* the language surface; a synthesized
/// exemplar that does not parse under the live parser is a generated-surface defect of the same
/// severity as a compiler bug. Render the table for the designated prompt-regression fixture and
/// assert every concrete (non-placeholder, non-metadata) `plasm_expr` cell round-trips the parser.
#[test]
fn teaching_tsv_exemplars_round_trip_parser() {
    use crate::expr_parser::parse_with_cgs_layers_program;
    use crate::PromptPipelineConfig;

    let dir = matrix_fixture_dir();
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
        // Metadata-only rows (`p#` / `v#` / `r#` gloss) are never executable exemplars.
        if expr
            .strip_prefix(['p', 'v', 'r'])
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        // Template rows carry placeholders / metadata sigils — not literal exemplars to parse.
        if expr.contains('<') || expr.contains("..") || expr.contains('$') {
            continue;
        }
        let stack = [crate::CgsLayer::unset(&cgs)];
        parse_with_cgs_layers_program(expr, &stack, sym_map.clone(), None, false).unwrap_or_else(
            |e| panic!("teaching exemplar must round-trip the parser: `{expr}` -> {e:?}"),
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "expected at least one concrete teaching exemplar to round-trip"
    );
}

/// Teaching TSV marks optionality in Meaning; method rows list all params (no `,..` elision).
#[test]
fn prompt_matrix_tsv_optional_legend_is_compact() {
    let dir = matrix_fixture_dir();
    let cgs = load_schema_dir(&dir).expect("load plasm_prompt_matrix");
    let tsv = crate::PromptPipelineConfig::default().render_prompt_tsv(&cgs, None);
    assert_meaning_cells_no_legacy_opt_prefix(&tsv);
    assert!(
        tsv.contains("optional"),
        "matrix teaching TSV should mark optional invoke/query slots with `optional`"
    );
    for line in tsv.lines().skip(1) {
        let Some((expr, _meaning)) = line.split_once('\t') else {
            continue;
        };
        assert!(
            !expr.contains(",..") && !expr.ends_with("..)"),
            "teaching method rows must list all params (no `,..` elision): {expr:?}"
        );
    }
}

#[test]
fn proof_document_teaching_optional_legend_is_compact() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).expect("proof");
    let tsv = super::render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("Document")));
    assert_meaning_cells_no_legacy_opt_prefix(&tsv);
    assert!(
        tsv.contains("optional"),
        "proof invoke rows with optional tails should mark optionality in Meaning"
    );
}

#[test]
fn prompt_matrix_tsv_teaching_surface_invariants() {
    use super::truncate_inline_desc;

    let dir = matrix_fixture_dir();
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let ruleset_es = map.entity_sym_for("", "Ruleset");
    let ruleset_banner = cgs
        .get_entity("Ruleset")
        .and_then(|e| {
            let d = e.description.trim();
            (!d.is_empty()).then(|| truncate_inline_desc(d, 200))
        })
        .expect("Ruleset banner");
    let tsv = super::render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let mut lines = tsv.lines();
    let first = lines.next().expect("tsv header");
    assert_eq!(
        first,
        TSV_TEACHING_TABLE_HEADER.trim_end(),
        "TSV output should begin with plasm_expr/Meaning header"
    );
    assert!(
        !tsv.contains(TEACHING_VALID_EXPR_MARKER),
        "teaching TSV must not embed grammar contract"
    );
    assert_meaning_cells_no_legacy_opt_prefix(&tsv);
    let ruleset_projection_row = tsv.lines().find(|l| {
        let c: Vec<&str> = l.split('\t').collect();
        if c.len() != 2 {
            return false;
        }
        let expr = c[0].trim();
        expr.starts_with(&ruleset_es)
            && c[1].contains("· projection")
            && parse_trailing_projection_bracket(expr).is_some()
    });
    let ruleset_projection_row =
        ruleset_projection_row.expect("expected Ruleset projection witness TSV row");
    assert!(
        ruleset_projection_row.contains(&ruleset_banner),
        "projection witness Meaning should carry Ruleset entity prose once: {ruleset_projection_row:?}"
    );
    let body = tsv
        .lines()
        .skip_while(|line| *line != TSV_TEACHING_TABLE_HEADER.trim_end())
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !body.contains(";;"),
        "2-column TSV surface should remove compact `;;` gloss separators"
    );
    let zone_es = map.entity_sym_for("", "Zone");
    let zone_query = tsv.lines().find(|l| {
        let cols: Vec<&str> = l.split('\t').collect();
        cols.len() == 2
            && cols[0].starts_with(&format!("{zone_es}{{"))
            && cols[1].contains("inputs:")
            && !cols[1].contains("rows:")
    });
    assert!(
        zone_query.is_some(),
        "Zone query exemplar should carry inputs: (rows: only on divergent provides / witness)"
    );
    let ruleset_query = tsv.lines().find(|l| {
        let cols: Vec<&str> = l.split('\t').collect();
        cols.len() == 2
            && cols[0].starts_with(&format!("{ruleset_es}{{"))
            && !cols[1].contains("· projection")
            && cols[1].contains("inputs:")
    });
    let ruleset_query = ruleset_query.expect("Ruleset scoped query teaching row");
    let rq_expr = ruleset_query.split('\t').next().unwrap_or("");
    match parse_trailing_projection_bracket(rq_expr.trim()) {
        None => assert!(
            !ruleset_query.contains("rows:"),
            "set-equal Ruleset query omits rows: : {ruleset_query}"
        ),
        Some(_) => assert!(
            ruleset_query.contains("rows:"),
            "divergent Ruleset provides keeps rows: : {ruleset_query}"
        ),
    }
    assert!(
        tsv.lines().any(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            cols.len() == 2
                && cols[1].contains("· projection")
                && parse_trailing_projection_bracket(cols[0].trim()).is_some()
        }),
        "expected a projection witness row with trailing [p#,…]"
    );
    assert!(
        tsv.lines().any(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            c.len() == 2
                && ((c[0].starts_with('v') && c[1].contains(" · "))
                    || (c[0].starts_with('p') && c[1].starts_with('v') && c[1].contains(" · ")))
        }),
        "expected at least one value-domain gloss row in matrix teaching TSV"
    );
}

/// Prompt-size guard replacing the deleted full `apis/github` insta snapshot.
#[test]
fn prompt_matrix_full_tsv_size_within_baseline() {
    let dir = matrix_fixture_dir();
    let cgs = load_schema_dir(&dir).expect("plasm_prompt_matrix");
    let tsv = super::render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    const BASELINE_BYTES: usize = 48_000;
    assert!(
        tsv.len() <= BASELINE_BYTES,
        "plasm_prompt_matrix full TSV grew past baseline (got {} bytes, cap {BASELINE_BYTES})",
        tsv.len()
    );
    assert!(
        tsv.len() > 2_500,
        "plasm_prompt_matrix teaching TSV unexpectedly tiny ({} bytes)",
        tsv.len()
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
    let pokemon_es = map.entity_sym_for("", "Pokemon");
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
    let pokemon_es = map.entity_sym_for("", "Pokemon");
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
fn linear_workflow_state_scoped_query_validates_with_homograph_p() {
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
        super::domain_example_line_count(&cgs, "WorkflowState", Some(map.as_ref())) > 0,
        "WorkflowState must synthesize teaching lines"
    );
}
