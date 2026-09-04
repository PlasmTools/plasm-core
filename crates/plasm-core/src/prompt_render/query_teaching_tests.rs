//! Regression tests for query-cap teaching row emission (kept out of `mod.rs`).

use std::collections::HashMap;

use crate::loader::load_schema_dir;
use crate::symbol_tuning::{symbol_map_for_prompt, FocusSpec};

use super::teaching_util::truncate_inline_desc;
use super::{
    collect_entity_teaching_block, parse_trailing_projection_bracket,
    prompt_line_valid_cache_seed_cgs, RenderConfig, PLASM_TOOL_DESCRIPTION,
    TEACHING_VALID_EXPR_MARKER, TSV_TEACHING_TABLE_HEADER,
};

/// True when `expr` is rooted on `entity_sym` (`e3`, `e3(…)`, `e3[…]`, …) but not a longer
/// symbol that shares the same digit prefix (`e3` must not match `e30`).
fn expr_starts_with_entity_sym(expr: &str, entity_sym: &str) -> bool {
    let Some(rest) = expr.strip_prefix(entity_sym) else {
        return false;
    };
    rest.is_empty() || !rest.as_bytes()[0].is_ascii_digit()
}

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

/// B5 — teaching round-trip guard. The language card *is* the language surface; a synthesized
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
        // Metadata-only rows (`p#` / `v#` / `r#` gloss, or wire slot gloss) are never executable exemplars.
        if expr
            .strip_prefix(['p', 'v', 'r'])
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        if !(expr.contains('.')
            || expr.contains('(')
            || expr.contains('{')
            || expr.contains('~')
            || (expr.starts_with('e')
                && expr.len() > 1
                && expr[1..].chars().all(|c| c.is_ascii_digit())))
            && expr
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            continue;
        }
        // Template rows carry angle-bracket placeholders / ellipsis — validate stand-ins, not raw holes.
        let expr_for_check = if expr.contains('<') {
            super::teaching_util::teaching_expr_for_validation(expr)
        } else if expr.contains("..") {
            continue;
        } else {
            expr.to_string()
        };
        let stack = [crate::CgsLayer::unset(&cgs)];
        parse_with_cgs_layers_program(&expr_for_check, &stack, sym_map.clone(), None, false)
            .unwrap_or_else(|e| {
                panic!(
                    "teaching exemplar must round-trip the parser: `{expr}` (check `{expr_for_check}`) -> {e:?}"
                )
            });
        checked += 1;
    }
    assert!(
        checked > 0,
        "expected at least one concrete teaching exemplar to round-trip"
    );
}

/// Language card marks optionality in Meaning; method rows list all params (no `,..` elision).
#[test]
fn prompt_matrix_tsv_optional_legend_is_compact() {
    let dir = matrix_fixture_dir();
    let cgs = load_schema_dir(&dir).expect("load plasm_prompt_matrix");
    let tsv = crate::PromptPipelineConfig::default().render_prompt_tsv(&cgs, None);
    assert_meaning_cells_no_legacy_opt_prefix(&tsv);
    assert!(
        tsv.contains("optional"),
        "matrix language card should mark optional invoke/query slots with `optional`"
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
fn github_pr_merge_zero_arity_invoke_omits_optional_meaning_when_schema_loads() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).expect("github");
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let mut cache = HashMap::new();
    let mut gloss = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "PullRequest",
        Some(&map),
        None,
        true,
        &mut cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss,
        None,
        None,
    );
    let merge = block
        .teaching_rows
        .iter()
        .find(|r| r.meta.source_capability.as_deref() == Some("pr_merge"))
        .expect("expected pr_merge teaching row on PullRequest");
    assert!(
        merge.teaching_expr.expression.contains("()"),
        "expected zero-arity merge teaching row: {:?}",
        merge.teaching_expr.expression
    );
    assert!(
        merge.teaching_expr.legend.optional_params.is_empty(),
        "zero-arity merge must not gloss optional when expr lists no optional params: {:?}",
        merge.teaching_expr.legend.optional_params
    );
    let tsv = super::render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let ms = map.method_sym_for("", "PullRequest", "pr_merge");
    let merge_line = tsv
        .lines()
        .find(|l| l.contains(&format!(".{ms}()")))
        .unwrap_or_else(|| panic!("expected .{ms}() in full language card"));
    assert!(
        merge_line.contains("Merge a pull request"),
        "rendered merge Meaning must include capability prose: {merge_line}"
    );
}

/// Two-column language card surface invariants on `plasm_prompt_matrix` (no `apis/` coupling).
#[test]
fn prompt_matrix_tsv_teaching_surface_invariants() {
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
        "TSV output should begin with plasm_expr/Meaning header (grammar is static in PLASM_TOOL_DESCRIPTION)"
    );
    assert!(
        !tsv.contains(TEACHING_VALID_EXPR_MARKER),
        "language card must not embed grammar contract"
    );
    assert!(
        PLASM_TOOL_DESCRIPTION.contains(TEACHING_VALID_EXPR_MARKER),
        "canonical grammar const must include contract marker"
    );
    assert_meaning_cells_no_legacy_opt_prefix(&tsv);

    // Compound identity get may carry first-use `[wires]`; entity banner rides the first executable
    // Meaning cell — never a noun card.
    let ruleset_identity_prefix = format!("{ruleset_es}(");
    let ruleset_meaning_prefix = format!("→ {ruleset_es}");
    let ruleset_identity = tsv
        .lines()
        .find(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            cols.len() == 2
                && cols[0].starts_with(&ruleset_identity_prefix)
                && cols[1].starts_with(&ruleset_meaning_prefix)
        })
        .expect("Ruleset compound identity get row");
    let identity_cols: Vec<&str> = ruleset_identity.split('\t').collect();
    assert_eq!(identity_cols.len(), 2, "identity row should have 2 columns");
    assert!(
        !tsv.lines().any(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            c.len() == 2 && expr_starts_with_entity_sym(c[0], &ruleset_es) && c[1].contains("noun")
        }),
        "Ruleset teaching must not emit noun cards:\n{tsv}"
    );
    let ruleset_first_executable = tsv
        .lines()
        .find(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            c.len() == 2 && expr_starts_with_entity_sym(c[0].trim(), &ruleset_es)
        })
        .expect("expected Ruleset executable TSV row");
    assert!(
        ruleset_first_executable.contains(&ruleset_banner),
        "first Ruleset executable Meaning should carry entity prose once: {ruleset_first_executable:?}"
    );

    // Select-backed field: v# row carries allowed values; omit redundant `kind` wire gloss when prose matches the domain row.
    assert!(
        tsv.lines().any(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            c.len() == 2
                && c[0].starts_with('v')
                && c[1].contains("custom")
                && c[1].contains("managed")
        }),
        "expected a v# row carrying Ruleset kind allowed values; excerpt missing custom/managed"
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

    // Capability-param `zone_id`: shared `ref:Zone` v# row carries type; wire gloss only for point-of-use prose.
    assert!(
        tsv.lines().any(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            c.len() == 2 && c[0].starts_with('v') && c[1].contains("ref:Zone")
        }),
        "expected ref:Zone value-domain row for ruleset_query zone_id filter"
    );

    // Action/mutator invoke references scope.
    let entrypoint_es = map.entity_sym_for("", "RulesetEntrypoint");
    let entrypoint_update = tsv
        .lines()
        .find(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            cols.len() == 2
                && expr_starts_with_entity_sym(cols[0], &entrypoint_es)
                && cols[0].contains(".m")
                && (cols[1].to_lowercase().contains("entrypoint")
                    || cols[1].contains("scope")
                    || cols[1].contains('↠'))
        })
        .expect("RulesetEntrypoint action invoke teaching table row");
    assert!(
        entrypoint_update.contains("[scope") || entrypoint_update.contains("scope"),
        "invoke row should reference scoping, got {entrypoint_update:?}"
    );

    let zone_es = map.entity_sym_for("", "Zone");
    let zone_query = tsv.lines().find(|l| {
        let cols: Vec<&str> = l.split('\t').collect();
        cols.len() == 2
            && cols[0].starts_with(&format!("{zone_es}{{"))
            && !cols[1].contains("rows:")
    });
    assert!(
        zone_query.is_some(),
        "Zone query exemplar should omit rows: in Meaning"
    );
    let ruleset_query = tsv.lines().find(|l| {
        let cols: Vec<&str> = l.split('\t').collect();
        cols.len() == 2
            && cols[0].starts_with(&format!("{ruleset_es}{{"))
            && !cols[1].contains("noun")
    });
    let ruleset_query = ruleset_query.expect("Ruleset scoped query teaching row");
    let rq_expr = ruleset_query.split('\t').next().unwrap_or("");
    match parse_trailing_projection_bracket(rq_expr.trim()) {
        None => assert!(
            !ruleset_query.contains("rows:"),
            "Ruleset query omits rows: : {ruleset_query}"
        ),
        Some(_) => assert!(
            !ruleset_query.contains("rows:"),
            "Ruleset query with bracket omits rows: in Meaning: {ruleset_query}"
        ),
    }
    assert!(
        !ruleset_query.contains("args:"),
        "capability legends omit inline `args:`; ruleset row was: {ruleset_query:?}"
    );
    assert!(
        ruleset_query.contains('↣'),
        "ruleset query Meaning should carry the list-return arrow (↣): {ruleset_query:?}"
    );
    assert!(
        tsv.lines().any(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            cols.len() == 2
                && !cols[1].contains("noun")
                && parse_trailing_projection_bracket(cols[0].trim()).is_some()
                && (cols[0].contains('{') || cols[0].contains('(') || cols[0].contains('~'))
        }),
        "expected an executable row with trailing [wires] by first use"
    );
    assert!(
        tsv.lines().any(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            c.len() == 2
                && ((c[0].starts_with('v') && c[1].contains(" · "))
                    || (c[0].starts_with('p') && c[1].starts_with('v') && c[1].contains(" · ")))
        }),
        "expected at least one value-domain gloss row in matrix language card"
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
        tsv.len() > 1_500,
        "plasm_prompt_matrix language card unexpectedly tiny ({} bytes)",
        tsv.len()
    );
}

#[test]
fn seeded_pokemon_teaching_includes_bare_query_row() {
    use crate::discovery::{
        derive_intent_exposure_surface_batch, ExposureSurfaceOptions, MutatorAdmit,
    };

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    if !dir.is_dir() {
        return;
    }
    let mut cgs = load_schema_dir(&dir).expect("pokeapi");
    cgs.bind_registry_entry_id("pokeapi");
    let endpoints = crate::relation_endpoint_keys("pokeapi", &["Pokemon".to_string()]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "pokeapi",
        "electric type pokemon chart",
        &endpoints,
        &["Pokemon".to_string()],
        None,
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
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

/// B2 — simple string-id entities teach positional `$` hole, not sample ids / `apis/` coupling.
#[test]
fn simple_string_id_identity_row_uses_id_hole() {
    use crate::schema::{
        CapabilityInputs, CapabilityKind, CapabilityMapping, CapabilitySchema, FieldSchema,
        FieldValueKind, NamedValueSchema, ResourceSchema, ValueDomainKey, CGS,
    };
    use crate::FieldType;

    let mut cgs = CGS::new();
    cgs.values.insert(
        "nv_str".into(),
        NamedValueSchema {
            domain: Default::default(),
            description: String::new(),
            field_type: FieldType::String,
            value_format: None,
            allowed_values: None,
            array_items: None,
            currency: None,
        },
    );
    let str_field = |name: &str| FieldSchema {
        name: name.into(),
        kind: FieldValueKind::Registry(ValueDomainKey::new("nv_str").expect("key")),
        description: String::new(),
        required: true,
        agent_presentation: None,
        mime_type_hint: None,
        attachment_media: None,
        wire_path: None,
        derive: None,
        data_class: None,
        currency_field: None,
    };
    cgs.add_resource(ResourceSchema {
        name: "Specimen".into(),
        description: "Simple string-id entity".into(),
        id_field: "name".into(),
        id_format: None,
        id_from: None,
        fields: vec![str_field("name")],
        relations: vec![],
        expression_aliases: vec![],
        implicit_request_identity: false,
        key_vars: vec![],
        abstract_entity: false,
        domain_projection_examples: true,
        primary_read: None,
        primary_query: None,
        primary_search: None,
        discovery: None,
    })
    .unwrap();
    let tmpl = serde_json::json!({
        "method": "GET",
        "path": [
            {"type": "literal", "value": "specimens"},
            {"type": "var", "name": "name"}
        ]
    });
    cgs.add_capability(CapabilitySchema {
        name: "specimen_get".into(),
        description: "Fetch one specimen by name".into(),
        kind: CapabilityKind::Get,
        domain: "Specimen".into(),
        identity_key: None,
        invalidates_entities: vec![],
        mapping: Some(CapabilityMapping {
            template: tmpl.into(),
        }),
        derived: None,
        inputs: CapabilityInputs::default(),
        output_schema: None,
        provides: vec!["name".into()],
        scope_aggregate_key_policy: Default::default(),
        preflight: None,
        discovery: None,
        sanitizes: vec![],
        deterministic: None,
    })
    .unwrap();
    cgs.validate().unwrap();

    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let es = map.entity_sym_for("", "Specimen");
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "Specimen",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss_emit_none,
        None,
        None,
    );
    let want_brace = format!("{es}{{name=<wire>}}");
    let identity = block.teaching_rows.iter().find(|r| {
        r.teaching_expr.expression.contains(&want_brace)
            || r.teaching_expr.expression.contains(&format!("{es}(<id>)"))
    });
    assert!(
        identity.is_some(),
        "string-id identity must teach `{want_brace}` or `{es}(<id>)`, rows={:?}",
        block
            .teaching_rows
            .iter()
            .map(|r| r.teaching_expr.expression.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        !block.teaching_rows.iter().any(|r| {
            let e = &r.teaching_expr.expression;
            e.contains("example-") || e.contains("pikachu") || e.contains("@example")
        }),
        "must not emit sample-id exemplars"
    );
}

#[test]
fn linear_workflow_state_scoped_query_validates_with_homograph_p() {
    use crate::loader::load_schema_dir_unvalidated;
    use crate::prompt_render::line_validate::{
        domain_line_validate_cached, prompt_line_valid_cache_seed_cgs,
    };
    use crate::symbol_tuning::{symbol_map_for_prompt, FocusSpec};

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/linear");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir_unvalidated(&dir).expect("linear");
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("map");
    let es = map.entity_sym_for("", "WorkflowState");
    let p_team =
        map.ident_sym_cap_param_for("", "WorkflowState", "workflow_state_query", "team_key");
    assert_eq!(
        p_team, "team_key",
        "team_key scope param teaches as catalog wire name"
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
