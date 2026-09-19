//! Prompt render integration tests (matrix/proof fixtures; no full-catalog snapshots).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::loader::load_schema_dir;
use crate::prompt_pipeline::PromptPipelineConfig;
use crate::schema::{
    CapabilityMapping, CapabilitySchema, FieldSchema, FieldValueKind, InputType, NamedValueSchema,
    RelationSchema, ResourceSchema, ValueDomainKey,
};
use crate::symbol_tuning::{
    entity_slices_for_render, resolve_prompt_surface_entities, symbol_map_for_prompt,
    ExposureEntityKey, ExposureSlotKey, ExposureSurface, FocusSpec, SymbolMap,
    TeachingExposureSession,
};
use crate::CapabilityKind;
use crate::Cardinality;
use crate::EntityName;
use crate::FieldType;
use crate::CGS;
use line_validate::validate_teaching_line_wire;

use super::line_validate::domain_line_validate_cached;
use super::*;

#[cfg(test)]
mod lazy_field_gloss_tests {
    use super::*;

    #[test]
    fn collect_opaque_domain_symbols_finds_em_pv_tokens() {
        let syms = collect_opaque_domain_symbols("e1{p14=e3(p5=$), p71=open}[p1,p2]");
        assert!(syms.contains("e1"));
        assert!(syms.contains("e3"));
        assert!(syms.contains("p14"));
        assert!(syms.contains("p5"));
        assert!(syms.contains("p71"));
        assert!(syms.contains("p1"));
        assert!(syms.contains("p2"));
        let rel = collect_opaque_domain_symbols("e5(p7=$).r8");
        assert!(rel.contains("r8"));
    }
}

/// Raw teaching lines for an entity (for per-capability witness checks).
#[cfg(test)]
pub(crate) fn domain_example_lines(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> Vec<String> {
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(cgs);
    let map_arc: Option<std::sync::Arc<SymbolMap>> = map.map(|m| std::sync::Arc::new(m.clone()));
    collect_entity_teaching_block(
        cgs,
        ename,
        map_arc.as_ref(),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface_filter,
        None,
    )
    .teaching_rows
    .into_iter()
    .map(|r| r.teaching_expr.expression.clone())
    .collect()
}

/// Count teaching rows that still claim a deleted noun/projection-witness flag (must be 0).
#[cfg(test)]
fn count_projection_teaching_witness_rows(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> usize {
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(cgs);
    let map_arc: Option<std::sync::Arc<SymbolMap>> = map.map(|m| std::sync::Arc::new(m.clone()));
    collect_entity_teaching_block(
        cgs,
        ename,
        map_arc.as_ref(),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface_filter,
        None,
    )
    .teaching_rows
    .iter()
    .filter(|r| r.teaching_expr.is_projection_teaching)
    .count()
}

/// First executable teaching row with a trailing projection bracket (wires by first use).
#[cfg(test)]
fn first_bracketed_executable_row(block: &EntityTeachingBlock) -> Option<&EntityTeachingExprRow> {
    block.teaching_rows.iter().find(|r| {
        !r.teaching_expr.is_projection_teaching
            && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
    })
}

/// Primary-get projection bracket for the teaching table entity heading (when enabled); test-only helper.
#[cfg(test)]
#[allow(dead_code)] // Retained for debugging / synthesis parity checks; tests prefer [`domain_projection_bracket_from_final_bundle`].
fn domain_heading_projection_bracket(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> Option<String> {
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(cgs);
    let map_arc: Option<std::sync::Arc<SymbolMap>> = map.map(|m| std::sync::Arc::new(m.clone()));
    let block = collect_entity_teaching_block(
        cgs,
        ename,
        map_arc.as_ref(),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface_filter,
        None,
    );
    let refs: Vec<&TeachingExprLine> = block
        .teaching_rows
        .iter()
        .map(|r| &r.teaching_expr)
        .collect();
    projection_bracket_from_teaching_rows(&refs)
}

/// Full scalar projection list `[p#,…]` from the projection teaching row or a legacy get suffix.
#[cfg(test)]
#[allow(dead_code)] // Superseded by [`domain_projection_bracket_from_final_bundle`] for prompt-aligned assertions.
fn domain_projection_bracket_exemplar(
    cgs: &CGS,
    ename: &str,
    map: Option<&SymbolMap>,
    surface_filter: Option<&ExposureSurface>,
) -> Option<String> {
    if let Some(b) = domain_heading_projection_bracket(cgs, ename, map, surface_filter) {
        return Some(b);
    }
    for line in domain_example_lines(cgs, ename, map, surface_filter) {
        if let Some(b) = parse_trailing_projection_bracket(line.trim()) {
            return Some(b);
        }
    }
    None
}

/// [`domain_projection_bracket_exemplar`] reads pre–post-pass teaching synthesis; this uses the same
/// [`render_teaching_prompt_bundle_for_exposure`] path as production prompts (opaque alias rewrite applied).
#[cfg(test)]
fn domain_projection_bracket_from_final_bundle(
    cgs: &CGS,
    exposure: &crate::symbol_tuning::TeachingExposureSession,
    config: RenderConfig<'_>,
    ename: &str,
) -> Option<String> {
    let bundle = render_teaching_prompt_bundle_for_exposure(cgs, config, exposure, None);
    let refs: Vec<&str> = exposure.entities.iter().map(|s| s.as_str()).collect();
    let focus = crate::symbol_tuning::FocusSpec::SeedsExact(&refs);
    let (full_entities, _) = crate::symbol_tuning::entity_slices_for_render(cgs, focus);
    let idx = full_entities.iter().position(|e| *e == ename)?;
    let block = bundle.teaching_blocks.get(idx)?;
    let lines: Vec<&TeachingExprLine> = block
        .teaching_rows
        .iter()
        .map(|r| &r.teaching_expr)
        .collect();
    projection_bracket_from_teaching_rows(&lines)
}

/// Turn a teaching scope variant into the **same shape as a path expression**: bare `e#` when unscoped,
/// else `e#{p#=e#(id),…}` with `*` stripped from scope hints (teaching-table-only marker).
#[cfg(test)]
pub(crate) fn query_construct_display(es: &str, scope_variant: &str) -> String {
    if scope_variant == es {
        return es.to_string();
    }
    let inner: String = scope_variant
        .split_whitespace()
        .map(|tok| tok.strip_prefix('*').unwrap_or(tok))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{es}{{{inner}}}")
}

#[cfg(test)]
fn is_field_gloss_line(trimmed: &str) -> bool {
    let t = trimmed.trim_start();
    let rest = if let Some(r) = t.strip_prefix('p') {
        r
    } else if let Some(r) = t.strip_prefix('v') {
        r
    } else if let Some(r) = t.strip_prefix('r') {
        r
    } else {
        return false;
    };
    let mut len = 0usize;
    for c in rest.chars() {
        if c.is_ascii_digit() {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    if len == 0 {
        return false;
    }
    rest[len..].trim_start().starts_with(";;")
}

/// Extract expression strings from the rendered teaching section: **tsv** uses the `plasm_expr` column
/// after the `plasm_expr\tMeaning` header.
#[cfg(test)]
fn example_expressions_from_prompt(prompt: &str) -> Vec<String> {
    if prompt.contains(TSV_TEACHING_TABLE_HEADER) {
        return example_expressions_from_prompt_tsv(prompt);
    }
    let mut out = Vec::new();
    let mut in_domain = false;
    for line in prompt.lines() {
        if line.contains(TEACHING_VALID_EXPR_MARKER) {
            in_domain = true;
            continue;
        }
        if in_domain {
            if line.trim_start().starts_with("---") {
                break;
            }
            let t = line.trim_start();
            if t.starts_with("--") {
                continue;
            }
            if t.starts_with('(') {
                continue;
            }
            // Plasm examples live under `    ` (four-space indent under each entity header).
            if !line.starts_with("    ") {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if is_field_gloss_line(trimmed) {
                continue;
            }
            let expr_only = crate::symbol_tuning::strip_prompt_expression_annotations(trimmed);
            if !expr_only.is_empty() {
                out.push(expr_only);
            }
        }
    }
    out
}

#[cfg(test)]
fn is_tsv_expression_column_slot_def(expr_cell: &str) -> bool {
    let s = expr_cell.trim();
    let rest = if let Some(r) = s.strip_prefix('p') {
        r
    } else if let Some(r) = s.strip_prefix('v') {
        r
    } else if let Some(r) = s.strip_prefix('r') {
        r
    } else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
fn example_expressions_from_prompt_tsv(prompt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in prompt.lines() {
        if line == TSV_TEACHING_TABLE_HEADER.trim_end() {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if line.trim_start().starts_with("---") {
            break;
        }
        let Some((expr_cell, meaning)) = line.split_once('\t') else {
            continue;
        };
        let trimmed = expr_cell.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !meaning.contains('→') {
            continue;
        }
        if is_tsv_expression_column_slot_def(trimmed) {
            continue;
        }
        let expr_only = crate::symbol_tuning::strip_prompt_expression_annotations(trimmed);
        if !expr_only.is_empty() {
            out.push(expr_only);
        }
    }
    out
}

/// [`Path::new`] relative segments are resolved against the **test process** current
/// directory, which is not always `crates/plasm-core` (e.g. it may be a workspace root).
/// Build paths from [`CARGO_MANIFEST_DIR`] so `fixtures/…` resolve correctly in
/// `cargo test` and CI the same as local `cd plasm-oss && cargo test`.
fn repo_path(components: &[&str]) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for c in components {
        p.push(c);
    }
    p
}
fn fixture_schema_dir(name: &str) -> std::path::PathBuf {
    repo_path(&["..", "..", "fixtures", "schemas", name])
}

#[test]
fn digit_id_identity_is_taught_not_integer() {
    let dir = fixtures_schemas_dir("digit_id_identity");
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("DigitAccount")));
    assert!(
        tsv.contains("v1\tdigit_id"),
        "PAN identity must gloss as digit_id, not integer\n{tsv}"
    );
    assert!(
        !tsv.contains("v1\tinteger"),
        "digit_id_identity fixture must not teach integer for the PAN value row\n{tsv}"
    );
    assert!(
        tsv.contains("[pan,") || tsv.contains("pan"),
        "query projection must teach the digit_id identity column\n{tsv}"
    );
}

#[test]
fn date_identity_query_is_taught_and_not_a_dry_lie() {
    let dir = fixtures_schemas_dir("date_identity");
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("DateLedger")));
    assert!(
        tsv.contains("e1{access_token=<wire>}"),
        "taught Query of a Date-identity entity must appear on the card\n{tsv}"
    );
    assert!(
        tsv.contains("[occurred_at,"),
        "RA-12 must teach Date identity on the query projection\n{tsv}"
    );
}

#[test]
fn langitem_create_tags_param_is_not_relation_nav_gloss() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("LangItem")));
    for needle in ["tags\t=> LangTag ·", "tags\t→ LangTag"] {
        assert!(
            !tsv.contains(needle),
            "create `tags` param must not reuse relation-style LangTag gloss; relation nav stays on r# rows.\n{tsv}"
        );
    }
}

/// Selection-lane `type: enum` params must teach `enum · tokens` on a `v#` row and link the wire
/// (`status → v#`), not collapse to OpaqueLegend `status → status`.
#[test]
fn selection_lane_enum_param_teaches_enum_gloss_not_wire_echo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("domain.yaml"),
        r#"http_backend: http://localhost:1080
values:
  nv_id:
    type: string
  nv_status:
    type: enum
    description: Request status filter.
    enum:
    - pending
    - approved
    - denied
entities:
  PaymentRequest:
    id_field: id
    fields:
      id:
        value_ref: nv_id
        required: true
capabilities:
  payment_request_query:
    kind: query
    entity: PaymentRequest
    selection:
    - name: status
      value_ref: nv_status
      required: false
    provides:
    - id
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("mappings.yaml"),
        "payment_request_query: {}\n",
    )
    .unwrap();
    let cgs = load_schema_dir(dir.path()).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    assert!(
        tsv.lines()
            .any(|l| l.contains("enum · pending | approved | denied")),
        "expected enum membership Meaning on a v# row; tsv:\n{tsv}"
    );
    assert!(
        !tsv.lines().any(|l| l == "status\tstatus"),
        "selection enum must not OpaqueLegend-echo the wire name; tsv:\n{tsv}"
    );
    let status_link = tsv.lines().find(|l| l.starts_with("status\t"));
    assert!(
        status_link.is_some_and(|l| {
            let meaning = l.split_once('\t').map(|(_, m)| m).unwrap_or("");
            meaning.starts_with('v') && meaning[1..].chars().all(|c| c.is_ascii_digit())
                || meaning.split(" · ").next().is_some_and(|head| {
                    head.starts_with('v') && head[1..].chars().all(|c| c.is_ascii_digit())
                })
        }),
        "status wire should RegistryWire-link a v#; got {status_link:?}\ntsv:\n{tsv}"
    );
}

#[test]
fn langitem_create_requires_title_parameter() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let cap = cgs
        .get_capability("langitem_create")
        .expect("langitem_create");
    assert!(
        cap.has_any_required_param(),
        "langitem_create must require a parameter so teaching cannot offer a no-arg create"
    );
    let title = cap
        .invocation_input_schemas()
        .filter_map(|schema| match &schema.input_type {
            InputType::Object { fields, .. } => Some(fields.as_slice()),
            _ => None,
        })
        .flatten()
        .find(|f| f.name == "title")
        .expect("langitem_create: missing `title` parameter");
    assert!(title.required, "langitem_create: `title` must be required");
}

/// Classifier-agreement regression: every teaching row's stored return arrow (`→` / `↣` / `↠`)
/// must equal what [`super::ReturnArrow::classify`] derives from that row's validated
/// [`super::DomainLineKind`] + result gloss. Locks the glyph to the kind so mutations always read
/// as terminal (`↠`), queries/searches as lists (`↣`), and gets/single-hops as single (`→`).
#[test]
fn return_arrow_classifier_agrees_with_domain_line_kind_on_language_matrix() {
    use super::{DomainLineKind, ReturnArrow};
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let bundle = render_teaching_prompt_bundle(
        &cgs,
        RenderConfig {
            focus: FocusSpec::All,
            render_mode: PromptRenderMode::Canonical,
            include_domain_execution_model: true,
            symbol_map_cross_cache: None,
        },
    );
    let mut seen_terminal = false;
    let mut seen_list = false;
    let mut seen_single = false;
    for block in &bundle.teaching_blocks {
        for row in &block.teaching_rows {
            let expected = ReturnArrow::classify(row.meta.kind, &row.teaching_expr.result_type);
            assert_eq!(
                row.teaching_expr.arrow, expected,
                "arrow drifted from kind {:?} for `{}` (gloss {:?})",
                row.meta.kind, row.teaching_expr.expression, row.teaching_expr.result_type
            );
            match row.teaching_expr.arrow {
                ReturnArrow::Terminal => seen_terminal = true,
                ReturnArrow::List => seen_list = true,
                ReturnArrow::Single => seen_single = true,
            }
            // A write is always terminal regardless of whether it provides an entity slice or `()`.
            if row.meta.kind == DomainLineKind::Method {
                assert_eq!(
                    row.teaching_expr.arrow,
                    ReturnArrow::Terminal,
                    "write `{}` must be terminal (↠)",
                    row.teaching_expr.expression
                );
            }
        }
    }
    assert!(
        seen_terminal && seen_list && seen_single,
        "language matrix must exercise all three return shapes (terminal={seen_terminal}, list={seen_list}, single={seen_single})"
    );
}

/// Rendered-glyph regression: the language matrix TSV must show `↠ e#[…]` on an
/// entity-providing write (no obsolete `chain:` hint), `↠ ()` on a void write, `↣ [` on a
/// query, and `→` on a get.
#[test]
fn teaching_tsv_return_glyphs_mutation_result_field_alphabet_language_matrix() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let meanings: Vec<&str> = tsv
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(_, m)| m))
        .collect();
    assert!(
        meanings
            .iter()
            .any(|m| m.contains('↠') && m.contains('[') && !m.contains("chain:")),
        "expected a provides-write row with terminal glyph + field alphabet, no chain hint; meanings:\n{}",
        meanings.join("\n")
    );
    assert!(
        !meanings.iter().any(|m| m.contains("chain:")),
        "MutationResult field-dot is lawful — teaching must not emit chain: hints; meanings:\n{}",
        meanings.join("\n")
    );
    assert!(
        meanings
            .iter()
            .any(|m| m.contains("↠ ()") && !m.contains("chain:")),
        "expected a void write row `↠ ()` with no chain hint; meanings:\n{}",
        meanings.join("\n")
    );
    assert!(
        meanings.iter().any(|m| m.contains("↣ [")),
        "expected a list-return query row (↣ […]); meanings:\n{}",
        meanings.join("\n")
    );
    assert!(
        meanings.iter().any(|m| m.trim_start().starts_with("→ ")),
        "expected a single-return get row (→ …); meanings:\n{}",
        meanings.join("\n")
    );
    assert!(
        meanings
            .iter()
            .any(|m| { m.contains("↠ ()") && m.contains('·') && !m.contains("chain:") }),
        "expected void write row with capability gloss and no chain hint; meanings:\n{}",
        meanings.join("\n")
    );
}

#[test]
fn query_only_primary_query_teaching_order_and_gloss() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let delta = crate::capability_exposure::explicit_entity_capability_surface(
        &cgs,
        "",
        &["QueryOnlyRequest".to_string()],
    )
    .expect("explicit fixture capability exposure");
    assert!(
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == "queryonly_request_received_query"),
        "seeded QueryOnlyRequest must expose primary query on surface"
    );
    let exp =
        TeachingExposureSession::new_with_intent_delta(&cgs, "", &["QueryOnlyRequest"], delta);
    let config = RenderConfig::for_eval_seeds(&["QueryOnlyRequest"]);
    let bundle = render_teaching_prompt_bundle_for_exposure(&cgs, config, &exp, None);
    let block_idx = bundle
        .model
        .entities
        .iter()
        .position(|e| e.entity == "QueryOnlyRequest")
        .expect("QueryOnlyRequest block");
    let block = &bundle.teaching_blocks[block_idx];
    let tsv = render_prompt_tsv_from_bundle(&bundle);
    let entity_banner = block.heading.description.trim();
    assert!(
        !entity_banner.is_empty(),
        "QueryOnlyRequest entity banner must be non-empty"
    );

    let query_idx = block
        .teaching_rows
        .iter()
        .position(|r| {
            r.meta.kind == DomainLineKind::Query
                && r.meta.source_capability.as_deref() == Some("queryonly_request_received_query")
        })
        .expect("primary query witness row");
    let first_mutator_idx = block
        .teaching_rows
        .iter()
        .position(|r| r.meta.kind == DomainLineKind::Method)
        .expect("at least one mutator row under IntentOnly");
    assert!(
        query_idx < first_mutator_idx,
        "primary query row must precede mutators (query={query_idx}, mutator={first_mutator_idx})"
    );

    let query_line = block.teaching_rows[query_idx]
        .teaching_expr
        .expression
        .as_str();
    assert!(
        query_line.contains("status=<wire>") || query_line.contains("status=\"<member>\""),
        "query Select filter must be a hole, not the first enum member; got: {query_line}"
    );
    assert!(
        !query_line.contains("status=\"draft\""),
        "query Select must not privilege first allowed_values member; got: {query_line}"
    );
    let query_meaning = tsv
        .lines()
        .find(|l| l.starts_with(query_line.split('[').next().unwrap_or(query_line)))
        .and_then(|l| l.split_once('\t').map(|(_, m)| m))
        .expect("query row in TSV");
    assert!(
        query_meaning.contains("↣ [") && !query_meaning.contains(entity_banner),
        "query anchor is list gloss only — noun-card must not ride the filter head; got: {query_meaning}"
    );
    assert!(
        tsv.lines().any(|l| {
            l.split_once('\t')
                .is_some_and(|(e, m)| e == "QueryOnlyRequest" && m.contains(entity_banner))
        }),
        "query-only noun-card must be a non-executable entity-name row; tsv:\n{tsv}"
    );
    assert!(
        query_meaning.contains("List payment requests other people sent to you"),
        "primary query row must include capability gloss; got: {query_meaning}"
    );
    assert!(
        !query_meaning.contains("existing rows only"),
        "QueryOnlyRequest has no create peer — must not name a new-row create; got: {query_meaning}"
    );

    let mutator_meanings: Vec<&str> = block
        .teaching_rows
        .iter()
        .filter(|r| r.meta.kind == DomainLineKind::Method)
        .filter_map(|r| {
            let expr = r.teaching_expr.expression.as_str();
            tsv.lines()
                .find(|l| l.split_once('\t').is_some_and(|(e, _)| e == expr))?
                .split_once('\t')
                .map(|(_, m)| m)
        })
        .collect();
    assert!(
        mutator_meanings.len() >= 3,
        "expected at least three void action mutators"
    );
    for m in &mutator_meanings {
        assert!(
            m.contains("↠ ()") && m.contains('·') && !m.contains(entity_banner),
            "mutator must have cap gloss, not entity banner: {m}"
        );
    }
    let distinct: std::collections::HashSet<_> = mutator_meanings.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        mutator_meanings.len(),
        "void mutator Meaning rows must be pairwise distinct"
    );
}

#[test]
fn query_meaning_names_create_peer_when_entity_has_both() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let bundle = render_teaching_prompt_bundle(&cgs, RenderConfig::for_eval_seeds(&["LangItem"]));
    let tsv = render_prompt_tsv_from_bundle(&bundle);
    let block_idx = bundle
        .model
        .entities
        .iter()
        .position(|e| e.entity == "LangItem")
        .expect("LangItem block");
    let block = &bundle.teaching_blocks[block_idx];
    let query_row = block
        .teaching_rows
        .iter()
        .find(|r| r.meta.kind == DomainLineKind::Query)
        .expect("LangItem query row");
    let expr = query_row.teaching_expr.expression.as_str();
    let meaning = tsv
        .lines()
        .find(|l| l.split_once('\t').is_some_and(|(e, _)| e == expr))
        .and_then(|l| l.split_once('\t').map(|(_, m)| m))
        .expect("LangItem query in TSV");
    assert!(
        meaning.contains("existing rows only"),
        "query+create entity must say listing is not a new row; got: {meaning}"
    );
    assert!(
        meaning.contains(".m") && meaning.contains('('),
        "query Meaning must name the taught create invoke; got: {meaning}"
    );
}

#[test]
fn langitem_tsv_topo_field_gloss_precedes_first_projection_use() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("LangItem")));
    let lines: Vec<&str> = tsv.lines().collect();
    let use_idx = lines.iter().position(|l| {
        parse_trailing_projection_bracket(l.split('\t').next().unwrap_or("").trim()).is_some()
    });
    let Some(use_idx) = use_idx else {
        panic!("expected a projection-bracket teaching row on LangItem");
    };
    let br =
        parse_trailing_projection_bracket(lines[use_idx].split('\t').next().unwrap_or("").trim())
            .expect("bracket");
    let inner = br
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .expect("bracket");
    for sym in inner.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if crate::symbol_tuning::SymbolMap::is_opaque_p_sym(sym) {
            continue;
        }
        let def = format!("{sym}\t");
        if let Some(def_idx) = lines.iter().position(|l| l.starts_with(&def)) {
            assert!(
                def_idx < use_idx,
                "wire gloss `{sym}` must precede projection use (def_idx={def_idx}, use_idx={use_idx})"
            );
        }
    }
}

fn fixtures_schemas_dir(name: &str) -> std::path::PathBuf {
    repo_path(&["..", "..", "fixtures", "schemas", name])
}

/// Upper bound for [`prompt_matrix_full_tsv_synthesis_benchmark`] (best-of-three wall time after warmup).
///
/// Override for slow CI hosts or local profiling: `PLASM_PROMPT_MATRIX_SYNTH_MAX_MS` (milliseconds).
fn prompt_matrix_synthesis_time_limit() -> std::time::Duration {
    const DEFAULT_MS: u64 = 3000;
    match std::env::var("PLASM_PROMPT_MATRIX_SYNTH_MAX_MS") {
        Ok(s) => s
            .parse::<u64>()
            .map(std::time::Duration::from_millis)
            .unwrap_or_else(|_| std::time::Duration::from_millis(DEFAULT_MS)),
        Err(_) => std::time::Duration::from_millis(DEFAULT_MS),
    }
}

/// Insta resolves the default `snapshots/` path from `file!()`. In the parent
/// `plasm/` virtual workspace, path remaps can make that resolve under a spurious
/// `plasm-oss/plasm-oss/...` tree, so the committed `.snap` is not found. Anchor to
/// [`CARGO_MANIFEST_DIR`], which is always the `plasm-core` crate root.
///
/// Serialize snapshot reads/writes: parallel `cargo test` threads share Insta's global settings and
/// can otherwise flake snapshot comparisons.
fn pin_teaching_snapshot_clock(s: &str) -> String {
    s.lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("evaluation_now\t") {
                if let Some((_, gloss)) = rest.split_once(" · ") {
                    format!("evaluation_now\t<temporal-now> · {gloss}")
                } else {
                    "evaluation_now\t<temporal-now>".to_string()
                }
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_insta_snapshots<R>(f: impl FnOnce() -> R) -> R {
    static INSTA_SNAPSHOT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = INSTA_SNAPSHOT_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = insta::Settings::clone_current();
    settings
        .set_snapshot_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots"));
    settings.bind(f)
}

#[test]
fn plasm_language_contract_is_tsv_first_and_avoids_legacy_terms() {
    let contract = super::PLASM_TOOL_DESCRIPTION;
    assert!(
        contract.contains("Language-card Meaning"),
        "contract should teach Meaning marks once in plasm_tool"
    );
    assert!(
        !contract.contains("TSV table semantics:"),
        "retired per-wave TSV table semantics heading"
    );
    assert!(
        contract.contains("Replace teaching placeholders")
            || contract.contains("substitute placeholders")
            || contract.contains("substitute session symbols")
            || contract.contains("fill with real values")
            || contract.contains("fill each from a bound value"),
        "symbolic contract must teach placeholder substitution"
    );
    assert!(
        contract.contains("run_ref"),
        "MCP contract must teach paging via run_ref on plasm_run"
    );
    assert!(
        !contract.contains("page_handle"),
        "contract must not advertise removed page_handle param"
    );
    assert!(
        !contract.contains("plan_commit_ref"),
        "plasm tool contract must not advertise plan_commit_ref as plasm_run param"
    );
    assert!(
        !contract.contains("program continuations"),
        "MCP contract must not advertise legacy program continuations on plasm_run"
    );
    assert!(
        !contract.contains("teaching table") && !contract.contains(";;") && !contract.contains("p#=v"),
        "contract must not reintroduce legacy teaching table/compact separators or bare-v placeholders:\n{contract}"
    );
}

#[test]
fn bundled_matrix_and_petstore_full_entities_emit_domain_lines() {
    for p in [
        fixtures_schemas_dir("plasm_language_matrix"),
        fixtures_schemas_dir("plasm_prompt_matrix"),
        fixtures_schemas_dir("petstore"),
    ] {
        if !p.exists() {
            continue;
        }
        let cgs = load_schema_dir(&p).unwrap_or_else(|e| panic!("load {}: {e}", p.display()));
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
        for ename in &full {
            let n = domain_example_line_count(&cgs, ename, map.as_deref());
            assert!(
                n > 0,
                "{}: entity `{ename}` is in full_entities but collect_entity_teaching_block emitted no teaching rows",
                p.display()
            );
        }
    }
}

#[test]
fn compound_branch_get_entity_ref_key_var_emits_valid_domain_line() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let lines = domain_example_lines(&cgs, "CompoundBranch", None, None);
    let expected = "CompoundBranch(owner=<id>, item_id=<id>, name=<id>)";
    assert!(
        lines.iter().any(|l| l.starts_with(expected)),
        "missing compound dotted-call-safe get witness for entity_ref key var: expected prefix `{expected}` in {:?}",
        lines
    );
    let expected_for_validate = super::teaching_util::teaching_expr_for_validation(expected);
    assert!(
        validate_teaching_line_wire(&cgs, &expected_for_validate).is_some(),
        "expected synthesized compound get witness to parse+typecheck: `{expected_for_validate}`"
    );
}

/// Regression: entity teaching has no noun card; projection brackets ride on executable producers.
#[test]
fn langitem_domain_emits_single_full_projection_exemplar() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let exposure = crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
    let surface = Some(&exposure.surface);
    let Some(ent) = cgs.get_entity("LangItem") else {
        panic!("missing LangItem entity");
    };
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
    let prefixes = cgs.projection_prompt_field_prefixes("LangItem", ent);
    assert_eq!(
        prefixes.len(),
        1,
        "expected one full projection exemplar vector; got {}",
        prefixes.len()
    );
    assert!(
        prefixes[0].len() >= 3,
        "LangItem primary get should expose response fields for teaching; got {}",
        prefixes[0].len()
    );
    let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
    let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "LangItem").expect(
        "LangItem should carry a full projection bracket (heading or primary get) after alias pass",
    );
    assert!(
        br.starts_with('[') && br.ends_with(']') && br.len() > 2,
        "unexpected projection bracket: {br}"
    );
    let lines = domain_example_lines(&cgs, "LangItem", map.as_deref(), surface);
    assert_eq!(
        count_projection_teaching_witness_rows(&cgs, "LangItem", map.as_deref(), surface),
        0,
        "noun / projection-witness flag must be gone"
    );
    let block = {
        let mut line_valid_cache = HashMap::new();
        let mut gloss_emit_none = None;
        let seed = prompt_line_valid_cache_seed_cgs(&cgs);
        collect_entity_teaching_block(
            &cgs,
            "LangItem",
            map.as_ref(),
            None,
            false,
            &mut line_valid_cache,
            seed,
            &mut gloss_emit_none,
            surface,
            None,
        )
    };
    let witness = first_bracketed_executable_row(&block).expect("LangItem bracket on executable");
    assert!(
        !witness.teaching_expr.result_type.contains("noun"),
        "Meaning must not contain noun: {:?}",
        witness.teaching_expr.result_type
    );
    let canon_syms = projection_bracket_syms(
        &parse_trailing_projection_bracket(witness.teaching_expr.expression.trim())
            .expect("witness bracket"),
    );
    assert!(
        !canon_syms.is_empty(),
        "expected projection wires on executable: {:?}",
        witness.teaching_expr.expression
    );
    assert!(
        lines.iter().any(|l| {
            parse_trailing_projection_bracket(l.trim()).is_some_and(|b| {
                projection_field_sets_equal(&projection_bracket_syms(&b), &canon_syms)
            })
        }),
        "canonical projection field set must appear on an executable line: {lines:?}"
    );
    for row in &block.teaching_rows {
        assert!(
            !row.teaching_expr.is_projection_teaching,
            "no noun/projection-teaching rows"
        );
        assert!(
            !row.teaching_expr.result_type.contains("noun"),
            "no noun in Meaning: {}",
            row.teaching_expr.result_type
        );
    }
    let out = render_prompt_with_config(&cgs, cfg);
    assert!(
        !out.contains("Federated sessions"),
        "single-catalog language card should not embed grammar pitfalls"
    );
    assert!(
        out.contains(br.as_str()),
        "full prompt should include the full projection list `{br}` (heading or primary get)"
    );
    let bracket_hits = out.matches(br.as_str()).count();
    assert!(
        bracket_hits >= 1,
        "canonical projection list must appear on LangItem read lines (got {bracket_hits})"
    );
}

/// Linear uses zero-arity method-style Get exemplars (`e2.m8()`); heading projection must still
/// teach scalar fields from `issue_get.provides` (see [`CGS::domain_projection_heading_fields`]).
#[test]
fn langitem_heading_projection_despite_method_style_get() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let exposure = crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
    let surface = Some(&exposure.surface);
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
    let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
    let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "LangItem")
        .expect("LangItem should carry a full projection bracket (heading or primary get)");
    assert!(
        br.starts_with('[') && br.ends_with(']') && br.len() > 2,
        "unexpected projection bracket: {br}"
    );
    let lines = domain_example_lines(&cgs, "LangItem", map.as_deref(), surface);
    assert_eq!(
        count_projection_teaching_witness_rows(&cgs, "LangItem", map.as_deref(), surface),
        0,
        "noun / projection-witness flag must be gone"
    );
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let block = collect_entity_teaching_block(
        &cgs,
        "LangItem",
        map.as_ref(),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface,
        None,
    );
    let witness = first_bracketed_executable_row(&block).expect("LangItem bracket on executable");
    let canon_syms = projection_bracket_syms(
        &parse_trailing_projection_bracket(witness.teaching_expr.expression.trim())
            .expect("witness bracket"),
    );
    let same_set_brackets = lines
        .iter()
        .filter(|l| {
            parse_trailing_projection_bracket(l.trim()).is_some_and(|b| {
                projection_field_sets_equal(&projection_bracket_syms(&b), &canon_syms)
            })
        })
        .count();
    assert!(
        same_set_brackets >= 1,
        "canonical projection field set must be taught on LangItem (got {same_set_brackets}): {lines:?}"
    );
    let out = render_prompt_with_config(&cgs, cfg);
    assert!(
        out.contains(br.as_str()),
        "full prompt should include the full projection list `{br}` (heading or primary get)"
    );
    assert!(
        out.matches(br.as_str()).count() >= 1,
        "canonical projection list must appear on LangItem read lines"
    );
}

/// Intent-scoped Issue surface: query/search share the witness field set → bare producers, no `rows:`.
#[test]
fn langitem_intent_surface_omits_set_equal_projection_on_query_search() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let delta = crate::capability_exposure::explicit_entity_capability_surface(
        &cgs,
        "langmatrix",
        &["LangItem".to_string()],
    )
    .expect("explicit fixture capability exposure");
    let exp =
        TeachingExposureSession::new_with_intent_delta(&cgs, "langmatrix", &["LangItem"], delta);
    let surface = Some(&exp.surface);
    let map = exp.symbol_map_arc();
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let block = collect_entity_teaching_block(
        &cgs,
        "LangItem",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface,
        Some("langmatrix"),
    );
    let witness = first_bracketed_executable_row(&block).expect("LangItem bracket on executable");
    let canon = parse_trailing_projection_bracket(witness.teaching_expr.expression.trim())
        .expect("witness bracket");
    let mut saw_list_producer = false;
    for row in &block.teaching_rows {
        assert!(!row.teaching_expr.is_projection_teaching);
        assert!(!row.teaching_expr.result_type.contains("noun"));
        let expr = row.teaching_expr.expression.as_str();
        if !(expr.contains('{') || expr.contains('~')) {
            continue;
        }
        saw_list_producer = true;
        let gloss = row.teaching_expr.result_type.as_str();
        assert!(!gloss.contains("rows:"), "no rows: in Meaning: {gloss}");
    }
    assert!(saw_list_producer, "expected query/search teaching rows");
    let lines: Vec<_> = block
        .teaching_rows
        .iter()
        .map(|r| r.teaching_expr.expression.as_str())
        .collect();
    assert!(
        lines.iter().any(|l| {
            parse_trailing_projection_bracket(l).is_some_and(|b| {
                projection_field_sets_equal(
                    &projection_bracket_syms(&b),
                    &projection_bracket_syms(&canon),
                )
            })
        }),
        "canonical projection set on an executable: {lines:?}"
    );
}

#[test]
fn heading_projection_symbols_are_declared_before_heading_use() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let exposure = crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
    let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
    let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "LangItem")
        .expect("LangItem should carry a projection list");
    let out = render_prompt_with_config(&cgs, cfg);
    let lines: Vec<&str> = out.lines().collect();
    let use_idx = lines
        .iter()
        .position(|l| l.contains(br.as_str()))
        .expect("full projection list should appear on heading or primary get line");
    let inner = br
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .expect("bracket");
    let symbols: Vec<&str> = inner
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !symbols.is_empty(),
        "LangItem projection should include at least one wire field symbol"
    );
    for sym in &symbols {
        if crate::symbol_tuning::SymbolMap::is_opaque_p_sym(sym) {
            continue;
        }
        let def = format!("{sym}\t");
        if let Some(def_idx) = lines.iter().position(|l| l.starts_with(&def)) {
            assert!(
                def_idx < use_idx,
                "wire gloss `{sym}` must precede projection use (def_idx={def_idx}, use_idx={use_idx})"
            );
        }
    }
}

#[test]
fn tsv_additive_wave_omits_global_contract_but_keeps_column_header() {
    let dir = fixtures_schemas_dir("petstore");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let pipeline = PromptPipelineConfig::default();
    let mut exp = TeachingExposureSession::new(&cgs, "", &["Pet"]);
    let first = pipeline.render_teaching_first_wave_for_session(&cgs, &exp, None);
    assert!(
        !first.contains(TEACHING_VALID_EXPR_MARKER),
        "execute first wave must not repeat global grammar contract"
    );
    assert!(
        first.starts_with(TSV_TEACHING_TABLE_HEADER),
        "first wave should start with teaching table header"
    );
    let (c, table) = split_tsv_teaching_contract_and_table(&first);
    assert!(c.is_none(), "execute first wave has no contract prefix");
    assert!(
        table.starts_with(TSV_TEACHING_TABLE_HEADER),
        "table body should start with plasm_expr/Meaning"
    );
    let eval_prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    assert!(
        !eval_prompt.contains(TEACHING_VALID_EXPR_MARKER),
        "language card is table-only; grammar lives in PLASM_TOOL_DESCRIPTION"
    );
    assert!(
        super::PLASM_TOOL_DESCRIPTION.contains(TEACHING_VALID_EXPR_MARKER),
        "canonical grammar const must include contract marker"
    );
    exp.expose_entities(&[&cgs], std::sync::Arc::new(cgs.clone()), "", &["Order"]);
    let delta = pipeline.render_teaching_exposure_delta(&cgs, &exp, &["Order"], None);
    assert!(
        !delta.contains(TEACHING_VALID_EXPR_MARKER),
        "additive TSV must not repeat global contract comments"
    );
    assert!(
        delta.contains(TSV_TEACHING_TABLE_HEADER.trim_end()),
        "additive TSV should keep column header"
    );
    for body in [&first, &delta] {
        for banned in [
            "Meaning arrows:",
            "Entity heads vs rows:",
            "Language-card table semantics",
            "TSV table semantics",
            "Language-card Meaning",
        ] {
            assert!(
                !body.contains(banned),
                "teaching wave must stay table-only (no glossary prose `{banned}`):\n{body}"
            );
        }
        assert!(
            body.lines().all(|l| {
                l.split_once('\t')
                    .map(|(_, m)| !m.contains("noun"))
                    .unwrap_or(true)
            }),
            "teaching Meaning must not contain noun:\n{body}"
        );
    }
}

#[test]
fn expand_wave_emits_parent_relation_edge_for_langitem_summary() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let pipeline = PromptPipelineConfig::default();
    let delta1 = crate::capability_exposure::explicit_entity_capability_surface(
        &cgs,
        "langmatrix",
        &["LangItem".to_string()],
    )
    .expect("explicit fixture capability exposure");
    let mut exp =
        TeachingExposureSession::new_with_intent_delta(&cgs, "langmatrix", &["LangItem"], delta1);
    let slots_before = exp.surface.slots.clone();
    let cgs_arc = std::sync::Arc::new(cgs.clone());
    let delta2 = crate::capability_exposure::explicit_entity_capability_surface(
        &cgs,
        "langmatrix",
        &["LangSummary".to_string()],
    )
    .expect("explicit fixture capability exposure");
    exp.expose_surface(&[&cgs], cgs_arc, "langmatrix", &["LangSummary"], delta2);
    let added = exp.qualified_entities_since(1);
    let new_relation_slots = exp.relation_edge_delta_slots(&slots_before, &added);
    exp.admit_relation_edge_slots_for_render(&[&cgs], &new_relation_slots);
    assert!(
        new_relation_slots.iter().any(|slot| {
            matches!(
                slot,
                ExposureSlotKey::Relation {
                    source,
                    relation,
                } if source.entity.as_str() == "LangItem" && relation.as_str() == "summary"
            )
        }),
        "expand should add LangItem.summary relation slot"
    );
    let delta = pipeline.render_teaching_exposure_delta_with_edges(
        &cgs,
        &exp,
        &["LangSummary"],
        &new_relation_slots,
        None,
    );
    assert!(
        delta.contains("relation e1 → e2"),
        "delta should teach parent hop: {delta}"
    );
    assert!(
        delta.contains(".r"),
        "delta should include opaque relation symbol: {delta}"
    );
}

#[test]
fn split_tsv_teaching_contract_and_table_table_only() {
    let t = "plasm_expr\tMeaning\na\tb\n";
    let (c, b) = split_tsv_teaching_contract_and_table(t);
    assert_eq!(c, None);
    assert_eq!(b, t);
}

#[test]
fn split_tsv_teaching_contract_and_table_with_comment_prefix() {
    let t = "# Plasm contract line\n# second\n\nplasm_expr\tMeaning\na\tb\n";
    let (c, b) = split_tsv_teaching_contract_and_table(t);
    assert_eq!(c.as_deref(), Some("# Plasm contract line\n# second"));
    assert_eq!(b, "plasm_expr\tMeaning\na\tb\n");
}

#[test]
fn rendered_teaching_tsv_teaching_rows_single_tab_separator() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let (_, body) = split_tsv_teaching_contract_and_table(&tsv);
    validate_teaching_tsv_teaching_table(&body).expect("every teaching row must be expr\\tMeaning");
}

/// Regression guard: full symbolic TSV prompt synthesis for [`fixtures/schemas/plasm_prompt_matrix`]
/// must stay within a fixed wall-time budget (best of three timed runs after warmup).
///
/// Calibrated for **small** matrix fixtures (~tens of ms on a laptop); failures usually mean
/// accidental quadratic work or extra clones on the prompt path. Relax only with cause:
/// `PLASM_PROMPT_MATRIX_SYNTH_MAX_MS`.
#[test]
fn prompt_matrix_full_tsv_synthesis_benchmark() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let config = RenderConfig::for_eval(None);

    let warmup = render_prompt_tsv_with_config(&cgs, config);
    assert!(
        warmup.contains(TSV_TEACHING_TABLE_HEADER.trim_end()),
        "warmup must emit language-card header"
    );

    let mut best = std::time::Duration::MAX;
    for _ in 0..3 {
        let t0 = Instant::now();
        let tsv = render_prompt_tsv_with_config(&cgs, config);
        best = best.min(t0.elapsed());
        assert!(
            tsv.len() > 2000,
            "sanity: symbolic prompt should be substantial (got {} chars)",
            tsv.len()
        );
    }

    let limit = prompt_matrix_synthesis_time_limit();
    assert!(
        best <= limit,
        "plasm_prompt_matrix TSV synthesis too slow: best-of-3 {:?} > limit {:?}. \
         Set PLASM_PROMPT_MATRIX_SYNTH_MAX_MS to raise the cap (milliseconds).",
        best,
        limit
    );
}

/// Canonical tool-model render must synthesize at least one teaching row for create-only entities
/// (e.g. `PromptRun.prompt-run-create(slug=$)` — wire method segment is kebab, not raw cap id).
#[test]
fn overshow_prompt_run_has_canonical_teaching_witness() {
    let dir = fixtures_schemas_dir("overshow_tools");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let bundle = render_teaching_prompt_bundle(
        &cgs,
        RenderConfig {
            focus: FocusSpec::All,
            render_mode: PromptRenderMode::Canonical,
            include_domain_execution_model: true,
            symbol_map_cross_cache: None,
        },
    );
    let prompt_run = bundle
        .model
        .entities
        .iter()
        .find(|e| e.entity == "PromptRun")
        .expect("PromptRun in teaching model");
    assert!(
        !prompt_run.lines.is_empty(),
        "create-only PromptRun must have a canonical teaching witness"
    );
    assert!(
        prompt_run
            .lines
            .iter()
            .any(|l| l.source_capability.as_deref() == Some("prompt_run_create")),
        "expected prompt_run_create witness, got {:?}",
        prompt_run.lines
    );
}

/// block, not `full_entities[idx]` by YAML insertion order (symbolic bundle uses sorted
/// [`TeachingExposureSession::entities`]). Overshow has `RecordedContent.id` (string) and
/// `CaptureItem.id` (integer); mis-alignment produced `str · id` for CaptureItem's block.
#[test]
fn tsv_symbolic_blocks_align_ident_gloss_with_exposure_entity_order() {
    let dir = fixtures_schemas_dir("overshow_tools");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let (names, _) = resolve_prompt_surface_entities(&cgs, FocusSpec::All, true);
    assert_eq!(
        names.first().map(|s| s.as_str()),
        Some("CaptureItem"),
        "exposure order should sort entities alphabetically; CaptureItem first"
    );
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let after_header = tsv
        .split(TSV_TEACHING_TABLE_HEADER)
        .nth(1)
        .expect("tsv plasm_expr/Meaning table");
    let first_block: String = after_header
        .lines()
        .take_while(|l| {
            let t = l.trim_start();
            !(t.starts_with("e2.") || t.starts_with("e2("))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let id_typing_on_v = first_block.lines().any(|l| {
        let mut cols = l.split('\t');
        let Some(sym) = cols.next() else {
            return false;
        };
        let Some(meaning) = cols.next() else {
            return false;
        };
        sym.starts_with('v') && meaning.contains("int")
    });
    let id_slot_teaches_v = first_block.lines().any(|l| {
        let mut cols = l.split('\t');
        let Some(sym) = cols.next() else {
            return false;
        };
        let Some(meaning) = cols.next() else {
            return false;
        };
        sym == "id" && meaning.starts_with('v') && !meaning.contains(" · id")
    });
    assert!(
        id_typing_on_v || id_slot_teaches_v,
        "CaptureItem `id` should type on v# and/or teach wire id gloss; first block:\n{first_block}"
    );
}

/// `Profile.recorded_matches` targets `RecordedContent`, which has Search/Query but no Get — teaching table
/// must still teach chain nav for `query_scoped` many relations using a **validated** receiver
/// (query-scoped `e7{…}` preferred over bare `e7($)` when that is the anchor that type-checks).
#[test]
fn overshow_tsv_includes_query_scoped_profile_relation_nav() {
    let dir = fixtures_schemas_dir("overshow_tools");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    assert!(
        tsv.lines()
            .any(|l| { l.contains("Content scoped to this profile") && l.starts_with('r') })
            && tsv.lines().any(|l| {
                l.contains("e7")
                    && (l.contains(".r") || l.contains(".recorded_matches"))
                    && l.contains("relation e7")
            }),
        "expected Profile → RecordedContent r# gloss and relation nav; e7 lines:\n{}",
        tsv.lines()
            .filter(|l| l.contains("e7"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn teaching_line_is_list_query_brace_relation(expr: &str) -> bool {
    let expr = expr.split('\t').next().unwrap_or(expr).trim();
    let Some(brace) = expr.find('{') else {
        return false;
    };
    let Some(close) = expr[brace..].find('}') else {
        return false;
    };
    let after = &expr[brace + close + 1..];
    after.starts_with(".r") && after.chars().nth(2).is_some_and(|c| c.is_ascii_digit())
}

pub(crate) fn strip_teaching_projection_for_test(expr: &str) -> &str {
    let t = expr.trim();
    match parse_trailing_projection_bracket(t) {
        Some(br) => t.strip_suffix(br.as_str()).map(str::trim).unwrap_or(t),
        None => t,
    }
}

/// Zone.rulesets on plasm_prompt_matrix: singleton + bind-then-fanout echoing the taught query.
#[test]
fn teaching_tsv_relation_nav_teaches_singleton_and_program_fanout() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let lines = domain_example_lines(&cgs, "Zone", Some(map.as_ref()), None);
    let r = map.ident_sym_relation_for("", "Zone", "rulesets");
    let query_head = lines
        .iter()
        .find_map(|l| {
            let expr = l.split('\t').next().unwrap_or(l).trim();
            if expr.contains(" => ")
                || crate::expr_parser::split_assignment_for_binding(expr).is_some()
            {
                return None;
            }
            if teaching_line_is_list_query_brace_relation(expr) {
                return None;
            }
            let head = strip_teaching_projection_for_test(expr);
            (head.starts_with('e') && head.contains('{') && !head.contains('('))
                .then(|| head.to_string())
        })
        .unwrap_or_else(|| {
            panic!(
                "expected taught Zone list query; Zone lines:\n{}",
                lines.join("\n")
            )
        });
    let bind = format!("rows = {query_head}");
    let fanout = format!("rows => _.{}", r);
    assert!(
        lines.iter().any(|l| l.contains(&format!("(<id>).{r}"))),
        "card must teach StaticSingleton eN(<id>).r# when Get-dot admits; Zone lines:\n{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|l| l == &bind || l.starts_with(&bind)),
        "fanout bind must echo the taught query `{bind}`; Zone lines:\n{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|l| l == &fanout || l.starts_with(&fanout)),
        "card must teach program-stratum {fanout}; Zone lines:\n{}",
        lines.join("\n")
    );
    assert!(
        !lines
            .iter()
            .any(|l| teaching_line_is_list_query_brace_relation(l)),
        "card must not teach eN{{…}}.r#; Zone lines:\n{}",
        lines.join("\n")
    );
    let mut cache = HashMap::new();
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    assert!(
        domain_line_validate_cached(&mut cache, seed, &cgs, &bind, Some(&map)).is_some(),
        "fanout bind must parse/validate: {bind}"
    );
    assert!(
        domain_line_validate_cached(&mut cache, seed, &cgs, &fanout, Some(&map)).is_some(),
        "fanout teaching line must parse/validate: {fanout}"
    );
}

#[test]
fn teaching_tsv_relation_nav_is_not_list_query_brace() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let offenders: Vec<&str> = tsv
        .lines()
        .filter(|l| teaching_line_is_list_query_brace_relation(l))
        .collect();
    assert!(
        offenders.is_empty(),
        "list-query brace-dot relation nav is heresy:\n{}",
        offenders.join("\n")
    );
}

/// Regression: compound-key `CaptureItem` get witness must be taught (covers `capture_item_get`).
#[test]
fn overshow_tsv_includes_compound_capture_item_get_witness() {
    let dir = fixtures_schemas_dir("overshow_tools");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let p_id = map.ident_sym_entity_field_for("", "CaptureItem", "id");
    let p_ct = map.ident_sym_entity_field_for("", "CaptureItem", "content_type");
    assert!(
        tsv.lines().any(|line| {
            line.starts_with("e1(")
                && line.contains(&format!("{p_id}=<id>"))
                && line.contains(&format!("{p_ct}=<id>"))
                && line.contains("→ e1")
        }),
        "expected compound-key capture-item get witness in TSV; e1 lines:\n{}",
        tsv.lines()
            .filter(|l| l.starts_with("e1"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn tsv_teaching_emitted_directly_has_no_compact_domain_separator_in_table() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let Some(idx) = prompt.find(TSV_TEACHING_TABLE_HEADER) else {
        panic!(
            "expected {} in rendered prompt",
            TSV_TEACHING_TABLE_HEADER.trim_end()
        );
    };
    let table = &prompt[idx..];
    validate_teaching_tsv_teaching_table(table).expect("TSV teaching invariant");
    for line in table.lines().skip(1) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        assert!(
            !line.contains(";;"),
            "direct TSV emission must not leak compact teaching table transcript tokens: {line:?}"
        );
    }
}

#[test]
fn teaching_expr_line_from_layers_splits_result_and_capability_legend() {
    let row = teaching_expr_line_from_layers(
        "e2(p20=$, p11=$)",
        Some("e2 · gloss with no delimiter issue"),
        Some("[scope p20→e4] — cap desc"),
        RowContractLegend::default(),
    );
    assert_eq!(row.expression, "e2(p20=$, p11=$)");
    assert_eq!(row.result_type, "e2 · gloss with no delimiter issue");
    assert!(
        row.legend.scope.contains("scope") || row.legend.description.contains("cap"),
        "expected capability legend in scope/description: scope={:?} desc={:?}",
        row.legend.scope,
        row.legend.description
    );
}

#[test]
fn teaching_expr_line_from_layers_preserves_double_spaces_in_result_gloss() {
    let row = teaching_expr_line_from_layers(
        "e1()",
        Some("part1  part2"),
        Some("[scope x]"),
        RowContractLegend::default(),
    );
    assert_eq!(row.result_type, "part1  part2");
}

#[test]
fn teaching_expr_line_from_layers_double_space_in_result_before_scope() {
    let row = teaching_expr_line_from_layers(
        "e1()",
        Some("e2 · tail  "),
        Some("[scope x]"),
        RowContractLegend::default(),
    );
    assert_eq!(row.result_type, "e2 · tail");
    assert!(row.legend.scope.contains("scope") || row.legend.description.contains('['));
}

#[test]
fn prompt_matrix_zone_domain_no_unary_placeholder_relation_or_fake_projection_meaning() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let lines = domain_example_lines(&cgs, "Zone", Some(map.as_ref()), None);
    for line in &lines {
        let head = line.trim();
        assert!(
            !(head.contains("($)") && head.contains('.')),
            "relation/method recv must not use invalid unary identity get `e#(<id>).…`: {head}"
        );
    }
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "Zone",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss_emit_none,
        None,
        None,
    );
    let witness_row = first_bracketed_executable_row(&block);
    let Some(row) = witness_row else {
        panic!("expected bracketed executable for Zone teaching table; lines={lines:?}");
    };
    let expr = row.teaching_expr.expression.as_str();
    let legend = teaching_row_meaning_text(
        &row.teaching_expr,
        false,
        false,
        &TeachingHeading::default(),
    );
    assert!(
        domain_line_validate_cached(
            &mut HashMap::new(),
            prompt_line_valid_cache_seed_cgs(&cgs),
            &cgs,
            expr,
            Some(&map),
        )
        .is_some(),
        "projection witness must parse+typecheck: {expr}"
    );
    assert!(
        !legend.contains("projection [") && !legend.contains("projection ["),
        "projection Meaning must not use legacy `projection […]` gloss prefix: {legend:?}"
    );
    assert!(
        !legend.contains("$)["),
        "projection Meaning must not embed a fake `…($)[…]` exemplar: {legend:?}"
    );
}

#[test]
fn plasm_language_contract_defines_ref_meaning_prefix() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    assert!(
        prompt.contains("ref:Zone") && prompt.contains("string · Zone identifier"),
        "language card must include entity-ref value-domain gloss with canonical entity (not e#):\n{prompt}"
    );
}

#[test]
fn prompt_matrix_symbolic_prompt_uses_wire_names_in_projection_brackets() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    for line in prompt.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((expr, _)) = line.split_once('\t') else {
            continue;
        };
        if expr == "plasm_expr" {
            continue;
        }
        if let Some(inner) = parse_trailing_projection_bracket(expr.trim()) {
            for sym in super::row_producer_teaching::projection_bracket_syms(&format!("[{inner}]"))
            {
                assert!(
                    !crate::symbol_tuning::SymbolMap::is_opaque_p_sym(sym.as_str()),
                    "projection brackets teach wire names, not legacy p#: {expr}"
                );
            }
        }
    }
}

#[test]
fn prompt_matrix_zone_identity_gloss_is_scalar_with_reference_scope() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let v = map
        .value_sym_for_wire("", "Ruleset", "zone_id")
        .expect("Ruleset.zone_id should map to a value-domain symbol");
    let g = map
        .value_domain_gloss_for_v_sym(&v)
        .expect("value-domain gloss");
    assert_eq!(g, "string", "identity fields teach their scalar wire type");
    let scope = cgs.get_capability("ruleset_query").unwrap().scope_params();
    let zone = scope.iter().find(|p| p.name == "zone_id").unwrap();
    assert!(
        matches!(&zone.named_value(&cgs).unwrap().field_type, crate::FieldType::EntityRef { target, .. } if target.as_str() == "Zone"),
        "the operation scope still accepts a Zone reference"
    );
}

#[test]
fn exposure_surface_omits_entity_ref_nav_when_target_entity_not_exposed() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let entry = cgs.entry_id.clone().unwrap_or_default();
    let delta = crate::capability_exposure::explicit_entity_capability_surface(
        &cgs,
        entry.as_str(),
        &["Ruleset".to_string()],
    )
    .expect("explicit fixture capability exposure");
    assert!(
        delta
            .required
            .entities
            .iter()
            .any(|e| e.entity.as_str() == "Ruleset"),
        "expected Ruleset in exposure entities"
    );
    assert!(
        !delta
            .required
            .entities
            .iter()
            .any(|e| e.entity.as_str() == "Zone"),
        "narrow wave should not list Zone as an exposed entity"
    );
    let map =
        symbol_map_for_prompt(&cgs, FocusSpec::SeedsExact(&["Ruleset"]), true).expect("symbol map");
    let zone_nav_sym = map.ident_sym_entity_field_for("", "Ruleset", "zone_id");
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "Ruleset",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss_emit_none,
        Some(&delta.required),
        Some(entry.as_str()),
    );
    let has_zone_nav = block.teaching_rows.iter().any(|r| {
        let ex = strip_teaching_projection_for_test(r.teaching_expr.expression.as_str());
        ex.contains('.') && ex.contains(zone_nav_sym.as_str())
    });
    assert!(
        !has_zone_nav,
        "zone_id navigation should be omitted when Zone is not on the exposure entity set; exprs={:?}",
        block
            .teaching_rows
            .iter()
            .map(|r| r.teaching_expr.expression.as_str())
            .collect::<Vec<_>>()
    );

    let mut surface_with_zone = delta.required.clone();
    surface_with_zone.entities.insert(ExposureEntityKey {
        entry_id: entry.clone(),
        entity: EntityName::from("Zone"),
    });
    let mut line_valid_cache2 = HashMap::new();
    let mut gloss_emit_none2 = None;
    let block2 = collect_entity_teaching_block(
        &cgs,
        "Ruleset",
        Some(&map),
        None,
        false,
        &mut line_valid_cache2,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss_emit_none2,
        Some(&surface_with_zone),
        Some(entry.as_str()),
    );
    assert!(
        block2.teaching_rows.iter().any(|r| {
            let ex = strip_teaching_projection_for_test(r.teaching_expr.expression.as_str());
            (ex.contains('.') && ex.contains(zone_nav_sym.as_str()))
                || ex.contains("Zone(")
                || ex.contains("Zone($)")
        }),
        "adding Zone to exposure entities should admit zone_id navigation again; exprs={:?}",
        block2
            .teaching_rows
            .iter()
            .map(|r| r.teaching_expr.expression.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn incoming_relation_nav_bases_respect_exposure_surface_parent_and_slots() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let entry = cgs.entry_id.clone().unwrap_or_default();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let zone_es = map.entity_sym_for("", "Zone");
    let mut nav_cache = HashMap::new();
    let nav_seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let unfiltered = super::incoming_relation_nav_bases_to_entity(
        &cgs,
        "Ruleset",
        Some(map.as_ref()),
        None,
        entry.as_str(),
        &mut nav_cache,
        nav_seed,
        Some(&map),
    );
    assert!(
        unfiltered.iter().any(|line| line.contains(zone_es.as_str())),
        "without surface filter expect Zone-anchored incoming bases toward Ruleset; got {unfiltered:?}"
    );

    let delta = crate::capability_exposure::explicit_entity_capability_surface(
        &cgs,
        entry.as_str(),
        &["Ruleset".to_string()],
    )
    .expect("explicit fixture capability exposure");
    let filtered = super::incoming_relation_nav_bases_to_entity(
        &cgs,
        "Ruleset",
        Some(map.as_ref()),
        Some(&delta.required),
        entry.as_str(),
        &mut nav_cache,
        nav_seed,
        Some(&map),
    );
    assert!(
        !filtered
            .iter()
            .any(|line| line.contains(zone_es.as_str())),
        "Zone must not anchor incoming projection bases when Zone is absent from exposure.entities; got {filtered:?}"
    );
}

#[test]
fn prompt_matrix_zone_id_p_slot_gloss_omits_duplicate_values_row_prose() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let p = map.ident_sym_entity_field_for("", "Ruleset", "zone_id");
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    for line in prompt.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((expr, meaning)) = line.split_once('\t') else {
            continue;
        };
        if expr == p {
            assert!(
                !meaning.contains("Zone identifier"),
                "compact wire gloss must not repeat values: row description; got {meaning:?}"
            );
            assert!(
                meaning.starts_with('v') && !meaning.contains(" · zone_id"),
                "wire gloss must link to v# only, not echo wire name; got {meaning:?}"
            );
        }
    }
}

#[test]
fn prompt_matrix_zone_projection_tsv_row_has_exactly_one_machine_tab() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "Zone",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss_emit_none,
        None,
        None,
    );
    let witness_row = first_bracketed_executable_row(&block);
    let Some(row) = witness_row else {
        panic!("expected bracketed executable for Zone teaching table");
    };
    let expr = row.teaching_expr.expression.as_str();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let line = prompt.lines().find(|l| {
        if l.is_empty() || l.starts_with('#') {
            return false;
        }
        l.split_once('\t').is_some_and(|(e, _)| e == expr)
    });
    let Some(line) = line else {
        panic!("TSV row for witness expr not found: {expr:?}");
    };
    assert_eq!(
        line.bytes().filter(|b| *b == b'\t').count(),
        1,
        "teaching table row must use exactly one U+0009 column delimiter; line={line:?}"
    );
}

#[test]
fn prompt_matrix_ruleset_tsv_teaching_semantics() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    assert!(
        !prompt.contains("List rulesets on a zone"),
        "ruleset_query capability prose must not leak into TSV Meaning"
    );
    let desc = "Rules configuration held here";
    assert_eq!(
        prompt.matches(desc).count(),
        1,
        "Ruleset entity description should appear exactly once (terminal `.` stripped for agent gloss); excerpt around Ruleset teaching rows should be inspected"
    );
    let bundle = render_teaching_prompt_bundle(&cgs, RenderConfig::for_eval(None));
    let (names, _) = resolve_prompt_surface_entities(&cgs, FocusSpec::All, true);
    let idx = names
        .iter()
        .position(|n| n == "Ruleset")
        .expect("Ruleset in surface");
    let block = &bundle.teaching_blocks[idx];
    let rows: Vec<_> = block
        .teaching_rows
        .iter()
        .map(|r| &r.teaching_expr)
        .collect();
    assert!(
        rows.iter().all(|r| !r.is_projection_teaching),
        "Ruleset must not emit noun/projection-teaching rows"
    );
    let first = rows.first().expect("Ruleset teaching rows");
    assert!(
        first.expression.contains('{')
            || first.expression.contains('(')
            || first.expression.contains('~')
            || first.expression.chars().all(|c| c.is_ascii_alphanumeric()),
        "first Ruleset row must be executable, got {}",
        first.expression
    );
}

#[test]
fn prompt_matrix_waf_package_query_projection_witness_row() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let block = collect_entity_teaching_block(
        &cgs,
        "WafPackage",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        prompt_line_valid_cache_seed_cgs(&cgs),
        &mut gloss_emit_none,
        None,
        None,
    );
    let witness = first_bracketed_executable_row(&block);
    let Some(row) = witness else {
        panic!(
            "expected bracketed executable for WafPackage; rows={:?}",
            block
                .teaching_rows
                .iter()
                .map(|r| r.teaching_expr.expression.as_str())
                .collect::<Vec<_>>()
        );
    };
    assert!(
        row.teaching_expr.expression.contains('{')
            || row.teaching_expr.expression.contains('(')
            || row.teaching_expr.expression.contains('~'),
        "bracket must ride on executable producer: {}",
        row.teaching_expr.expression
    );
    let expr = row.teaching_expr.expression.as_str();
    let Some(wp_ent) = cgs.get_entity("WafPackage") else {
        panic!("missing WafPackage entity");
    };
    if wp_ent.abstract_entity {
        // Abstract entities are omitted from default teaching slices — explicit teaching
        // collection still synthesizes witness rows for tooling/tests.
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        assert!(
            !prompt.lines().any(|l| {
                !l.starts_with('#')
                    && !l.is_empty()
                    && l.split_once('\t').is_some_and(|(e, _)| e == expr)
            }),
            "abstract WafPackage lines must not appear in default language card: {expr:?}"
        );
        return;
    }
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let line = prompt.lines().find(|l| {
        !l.starts_with('#') && !l.is_empty() && l.split_once('\t').is_some_and(|(e, _)| e == expr)
    });
    let Some(line) = line else {
        panic!("TSV row for WafPackage projection witness not found: {expr:?}");
    };
    assert_eq!(
        line.bytes().filter(|b| *b == b'\t').count(),
        1,
        "single tab delimiter; line={line:?}"
    );
    assert!(
        line.split_once('\t')
            .is_some_and(|(_, m)| m.contains("noun")),
        "Meaning should include projection gloss: {line:?}"
    );
}

#[test]
fn seeded_abstract_entity_assigns_symbol_and_teaching_row() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let pipeline = PromptPipelineConfig::default();
    let exp = TeachingExposureSession::new(&cgs, "", &["WafPackage"]);
    assert!(exp.contains_qualified_entity("", "WafPackage"));
    assert_eq!(exp.entities, vec!["WafPackage".to_string()]);
    let first = pipeline.render_teaching_first_wave_for_session(&cgs, &exp, None);
    let (_, body) = split_tsv_teaching_contract_and_table(&first);
    validate_teaching_tsv_teaching_table(&body).expect("valid teaching rows for abstract seed");
    assert!(
        body.lines().any(|l| l.starts_with("e1")),
        "abstract WafPackage seed must produce executable e1 row: {body}"
    );
}

#[test]
fn prompt_matrix_duplicate_registry_p_slot_gloss_suppressed() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let Some(idx) = prompt.find(TSV_TEACHING_TABLE_HEADER) else {
        panic!("expected language-card header");
    };
    fn count_slot_rows(body: &str, prefix: &str) -> usize {
        body.lines()
            .filter(|l| {
                let l = l.strip_suffix('\r').unwrap_or(l);
                !l.is_empty()
                    && !l.starts_with('#')
                    && l.split_once('\t').is_some_and(|(cell, _)| cell == prefix)
            })
            .count()
    }
    let table = &prompt[idx..];
    assert!(
        count_slot_rows(table, "p14") <= 1,
        "shared p14 id slot must dedupe to at most one gloss row"
    );
    assert!(
        count_slot_rows(table, "p15") <= 1,
        "shared p15 name slot must dedupe to at most one gloss row"
    );
}

#[test]
fn prompt_render_mode_user_surface_helpers_cover_public_modes() {
    assert_eq!(PromptRenderMode::USER_FACING_VALUES, ["tsv"]);
    assert_eq!(
        PromptRenderMode::parse_user_facing("verbose"),
        Some(PromptRenderMode::Tsv)
    );
    assert_eq!(
        PromptRenderMode::parse_user_facing("compact"),
        Some(PromptRenderMode::Tsv)
    );
    assert_eq!(
        PromptRenderMode::parse_user_facing("tsv"),
        Some(PromptRenderMode::Tsv)
    );
    assert_eq!(PromptRenderMode::parse_user_facing("canonical"), None);
    assert_eq!(
        PromptRenderMode::parse_user_facing_or_default("unknown"),
        PromptRenderMode::Tsv
    );
    assert_eq!(PromptRenderMode::Canonical.user_facing_name(), None);
    assert_eq!(
        PromptRenderMode::Compact.user_facing_name(),
        Some("compact")
    );
    assert_eq!(PromptRenderMode::Tsv.markdown_fence_info_string(), "tsv");
    assert_eq!(
        PromptRenderMode::Compact.markdown_fence_info_string(),
        "tsv"
    );
}

/// Canonical static `plasm` tool description; update with `INSTA_UPDATE=1 cargo test -p plasm-core plasm_tool_description_snapshot`.
#[test]
fn plasm_tool_description_snapshot() {
    with_insta_snapshots(|| {
        insta::assert_snapshot!("plasm_tool_description", super::PLASM_TOOL_DESCRIPTION);
    });
}

#[test]
fn plasm_tool_description_includes_composition_strata() {
    let frontmatter = super::PLASM_TOOL_DESCRIPTION;
    assert!(frontmatter.contains("Three strata"));
    assert!(frontmatter.contains("| where"));
    assert!(frontmatter.contains("| select"));
    assert!(frontmatter.contains("| take"));
    assert!(
        frontmatter.contains("| union") && frontmatter.contains("peers = a | union b"),
        "RA-14 must teach the executable `| union` skeleton"
    );
    assert!(frontmatter.contains("=>"));
    assert!(frontmatter.contains("<<TAG"));
    assert!(
        !frontmatter.contains("context=ℓ") && !frontmatter.contains("(context="),
        "RA-5 source frame abolished — no context= in tool card"
    );
    assert!(
        frontmatter.contains("membership")
            || frontmatter.contains("Membership")
            || frontmatter.contains("membership holes"),
        "must teach typed membership hole fill"
    );
    assert!(
        frontmatter.contains("e#~\"q\"") || frontmatter.contains("e#~\"<query>\""),
        "search exemplar must show a real-query hole, not metasyntax text/$"
    );
    assert!(
        !frontmatter.contains("e#~$") && !frontmatter.contains("e#~\"text\""),
        "must not teach literal e#~$ / e#~\"text\" placeholders that weak models copy"
    );
    assert!(
        !frontmatter.contains("Session and symbol discipline"),
        "session discipline belongs in tool workflow descriptions, not duplicated in grammar"
    );
    assert!(
        !frontmatter.contains(" ::="),
        "full pseudo-EBNF block retired; canonical syntax is three-strata surface"
    );
    assert!(
        frontmatter.contains("label = e#") || frontmatter.contains("rows = e#"),
        "must teach bind-before-filter preference"
    );
    assert!(
        !frontmatter.contains("Entity heads vs rows:"),
        "P06/P07 entity-head pedagogy is CEILING DROP — omit from rewrite"
    );
    assert!(
        !frontmatter.contains("**SQL map:**"),
        "P08 SQL map is DROP — omit from rewrite"
    );
    assert!(
        !frontmatter.contains("PLASM_RPT_TAG"),
        "worked row-to-text few-shot removed — composition_ladder owns template teachability"
    );
    assert!(
        !frontmatter.contains("Worked row-to-text"),
        "problem-shaped worked examples purged from production card"
    );
    assert!(
        !frontmatter.contains("e2(p10="),
        "canonical frontmatter must not hardcode catalog-specific symbol indices"
    );
    assert!(
        frontmatter.contains("label.wire")
            && !frontmatter.contains("label.field")
            && !frontmatter.contains("e_issue.m_create"),
        "post-write label.wire rite is LOAD-BEARING (P11); multi-write few-shot is DROP (P10)"
    );
    assert!(
        !frontmatter.contains("e2.m2") && !frontmatter.contains("e3.m3"),
        "P10 multi-write worked block must stay omitted"
    );
    assert!(
        frontmatter.contains("never invent a `.wire`")
            || frontmatter.contains("never invent a .wire"),
        "must warn against inventing .wire from binding.wire prose"
    );
    assert!(
        !frontmatter.contains("owner=\"org\"") && !frontmatter.contains("e_repo"),
        "worked examples must stay domain-neutral (no github-shaped repo/issue)"
    );
    assert!(
        !frontmatter.contains("```tsv"),
        "P14 lookup mini-card few-shot is CEILING DROP"
    );
    assert!(
        frontmatter.contains("| summarize") && frontmatter.contains("summarize [by keys]"),
        "P19 Q5 row-op signatures must spell pipe summarize forms"
    );
    assert!(
        frontmatter.contains("binding.content"),
        "P21 R3 must teach .content for string params"
    );
    assert!(
        !frontmatter.contains("access_token=sess.")
            && !frontmatter.contains("user_email=peer.")
            && !frontmatter.contains("token=sess.")
            && !frontmatter.contains("sess.k")
            && !frontmatter.contains("peer.id")
            && !frontmatter.contains("peer_id=")
            && !frontmatter.contains("user=\"u\"")
            && !frontmatter.contains("secret=\"s\"")
            && !frontmatter.contains("alice"),
        "plasm_tool must not bake catalog or near-domain names into worked examples"
    );
    assert!(
        !frontmatter.contains("co-committed gates"),
        "retired undefined co-committed-gates phrasing"
    );
    assert!(
        !frontmatter.contains("re-read `e#(id=…)` to continue"),
        "↠ must not be framed as an in-program re-read ban"
    );
    assert!(
        frontmatter.contains("done = iterate cur step e#(_.id).m#"),
        "state iterate must teach the executable invoke-after-step form"
    );
    assert!(
        !frontmatter.contains("`done = iterate cur step`"),
        "must not close the iterate span after `step` (invites `step =` binder heresy)"
    );
    assert!(
        frontmatter.contains("done = rows => _.m#(args)"),
        "apply-bind must teach the executable rows=> method form"
    );
    assert!(
        !frontmatter.contains("`done = rows =>`"),
        "must not close the apply-bind span after `=>` (incomplete apply; same class as `step`)"
    );
}

#[test]
fn mcp_static_tool_descriptions_byte_budget() {
    const MAX_WORKFLOW_BYTES: usize = 1600;

    let workflow = super::MCP_INITIALIZE_WORKFLOW;
    let plasm_tool = super::PLASM_TOOL_DESCRIPTION;
    let context = super::PLASM_CONTEXT_TOOL_DESCRIPTION;
    let param = super::PLASM_PROGRAM_PARAM_DESCRIPTION;

    assert!(
        workflow.len() <= MAX_WORKFLOW_BYTES,
        "initialize workflow too long: {} bytes",
        workflow.len()
    );
    assert!(
        plasm_tool.len() <= super::PLASM_TOOL_DESCRIPTION_MAX_BYTES,
        "plasm tool description too long: {} bytes",
        plasm_tool.len()
    );
    assert!(
        context.len() <= 1800,
        "plasm_context tool description too long: {} bytes",
        context.len()
    );
    assert!(plasm_tool.contains(super::MCP_TOOL_SYNTAX_CONTRACT_MARKER));
    assert!(
        plasm_tool.contains("logical_session_ref") && plasm_tool.contains("run_ref"),
        "P01 MCP session/tool split must remain in plasm_tool"
    );

    let violations = super::program_param_contract_violations(param);
    assert!(
        violations.is_empty(),
        "program param contract violations: {violations:?}\n{param}"
    );
}

#[test]
fn plasm_tool_description_truncation_prefix_has_composition_mandate() {
    let full = super::PLASM_TOOL_DESCRIPTION;
    let prefix_n = super::PLASM_TOOL_DESCRIPTION_PREFIX_BYTES;
    let end = full.floor_char_boundary(full.len().min(prefix_n));
    let prefix = &full[..end];
    assert!(
        prefix.contains("Batch independent reads"),
        "batching mandate must be in first {prefix_n} bytes (host truncation)"
    );
    assert!(
        prefix.contains("labels, branches")
            || prefix.contains("a, b")
            || prefix.contains("bars, bazs"),
        "multi-root return example must be in first {prefix_n} bytes"
    );
    assert!(
        prefix.contains("run_ref") && (prefix.contains("review") || prefix.contains("approval")),
        "mutation/gate policy must be in first {prefix_n} bytes"
    );
    assert!(
        prefix.contains("\\n") || prefix.contains("`\\n`"),
        "JSON-style escape note must be in first {prefix_n} bytes"
    );
    assert!(
        prefix.contains("<<TAG") || prefix.contains("heredoc"),
        "heredoc pointer must be in first {prefix_n} bytes"
    );
    assert!(
        prefix.contains("Program shape:"),
        "program-shape contract must be in first {prefix_n} bytes (host truncation)"
    );

    let wide_n = super::PLASM_TOOL_DESCRIPTION_WIDE_PREFIX_BYTES;
    let wide_end = full.floor_char_boundary(full.len().min(wide_n));
    let wide = &full[..wide_end];
    assert!(
        wide.contains("Composition:") || wide.contains("Three strata"),
        "composition strata must be in first {wide_n} bytes (host truncation)"
    );
    assert!(
        wide.contains("| where") && wide.contains("=>"),
        "pipe + apply surface must be in first {wide_n} bytes"
    );
}

#[test]
fn plasm_tool_description_stats() {
    let full = super::PLASM_TOOL_DESCRIPTION;
    assert!(
        full.len() <= super::PLASM_TOOL_DESCRIPTION_MAX_BYTES,
        "grammar contract grew past max budget: {} bytes",
        full.len()
    );

    let full_stats = super::grammar_frontmatter_stats_from_contract(full);
    assert!(
        full_stats
            .section_bytes
            .get("core_surface")
            .copied()
            .unwrap_or(0)
            > 400
    );
    assert!(
        full_stats
            .section_bytes
            .get("composition")
            .copied()
            .unwrap_or(0)
            > 100
    );
    assert!(
        full_stats
            .section_bytes
            .get("tsv_semantics")
            .copied()
            .unwrap_or(0)
            > 100
    );

    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let full_prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let single_prompt = render_prompt_tsv_with_config(
        &cgs,
        RenderConfig {
            focus: FocusSpec::Single("Ruleset"),
            ..RenderConfig::for_eval(None)
        },
    );
    let full_prompt_stats = super::grammar_frontmatter_stats_from_prompt(&full_prompt);
    let single_stats = super::grammar_frontmatter_stats_from_prompt(&single_prompt);
    assert!(
        single_stats.contract_comment_bytes <= full_prompt_stats.contract_comment_bytes,
        "single-entity slice should not add contract comments to language card"
    );
}

/// Reports contract/table ratio for matrix fixture and one real catalog (stderr only on failure).
#[test]
fn grammar_frontmatter_stats_matrix_and_catalog() {
    let matrix_dir = fixtures_schemas_dir("plasm_prompt_matrix");
    if !matrix_dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&matrix_dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let st = super::grammar_frontmatter_stats_from_prompt(&prompt);
    assert_eq!(st.contract_comment_bytes, 0);
    assert!(st.table_bytes > 0);
    assert!(
        st.contract_comment_bytes + st.table_bytes <= st.total_prompt_bytes,
        "contract + table should not exceed prompt size"
    );
    eprintln!(
        "grammar_frontmatter_stats plasm_prompt_matrix: {}",
        st.summary_line_body()
    );

    let lang_dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&lang_dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let st = super::grammar_frontmatter_stats_from_prompt(&prompt);
    assert_eq!(st.contract_comment_bytes, 0);
    assert!(st.table_bytes > 0);
    eprintln!(
        "grammar_frontmatter_stats plasm_language_matrix: {}",
        st.summary_line_body()
    );
}

/// Language-matrix Search TSV + tool card: `~` is Search text, not exact match or a complete page.
#[test]
fn language_matrix_search_tilde_teaches_search_text_not_exact_or_complete() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    assert!(dir.exists(), "missing plasm_language_matrix fixture");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let search_rows: Vec<&str> = prompt
        .lines()
        .filter(|l| l.contains("~\"<query>\"") && l.starts_with('e'))
        .collect();
    assert!(
        !search_rows.is_empty(),
        "expected e#~\"<query>\" teaching rows; prompt=\n{prompt}"
    );
    for line in &search_rows {
        let meaning = line.split_once('\t').map(|(_, m)| m).unwrap_or("");
        assert!(
            meaning.contains('[') && !meaning.to_ascii_lowercase().contains("exact"),
            "Search Meaning is a collection gloss, not exact-match: {line}"
        );
        assert!(
            !meaning.to_ascii_lowercase().contains("complete"),
            "Search Meaning must not claim completeness: {line}"
        );
    }
    let card = super::PLASM_TOOL_DESCRIPTION;
    assert!(
        card.contains(r#"`e#` / `e#{wire=value}` / `e#~"q"` / `e#(<id>)`"#),
        "catalog-source line must list e#~\"q\" literally"
    );
    assert!(
        card.contains("Backend WHERE only on an **entity head**."),
        "catalog-source line must keep Backend WHERE seat"
    );
    assert!(
        card.contains("Membership plane (prefer `| where`)"),
        "card teaches membership via | where, not ~"
    );
    assert!(
        !card.contains("exact match") && !card.contains("must contain the literal"),
        "card must not claim ~ is exact slot match"
    );
    let run = include_str!("assets/plasm_run_tool_base.txt");
    assert!(
        run.contains(r#"more pages — call plasm_run with run_ref: "…""#),
        "remainder is taught only when the result says more pages"
    );
}

/// Receivers are domain entities; selected rows carry their own identity and scope.
#[test]
fn plasm_tool_teaches_semantic_operation_receivers() {
    let card = super::PLASM_TOOL_DESCRIPTION;
    for taught in [
        "e#(<id>).m#(args)",
        "item.m#(args)",
        "including `| take 1`",
        "rows => _.m#(args)",
        "The row supplies identity and scope",
        "Empty singleton receivers fail",
        "Receiver-free operations use `e#.m#(args)` as printed on the card",
    ] {
        assert!(card.contains(taught), "missing receiver law: {taught}");
    }
    for transport_ceremony in [
        "if pathless",
        "retry with",
        "path_vars",
        "item = e#(row.id)",
    ] {
        assert!(
            !card.contains(transport_ceremony),
            "transport or identity reconstruction leaked: {transport_ceremony}"
        );
    }
}

#[test]
fn plasm_tool_teaches_typed_identity_holes() {
    let card = super::PLASM_TOOL_DESCRIPTION;
    assert!(card.contains("`<id>` is the Get identity (taught `id_field`)"));
    assert!(card.contains("fill each from a bound value"));
    assert!(card.contains("of that hole's sort"));
    assert!(!card.contains("copy the card left-column seat exactly"));
    assert!(!card.contains(r#"e#("…")"#));
}

/// `plasm_prompt_matrix` TSV method rows follow catalog seat: pathless `eN.mK(` only when pathless.
#[test]
fn prompt_matrix_action_rows_use_catalog_seat() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let map_arc = std::sync::Arc::new(map.clone());
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let mut checked = 0usize;
    let mut entities: Vec<&str> = cgs
        .capabilities
        .values()
        .filter(|cap| {
            matches!(
                cap.kind,
                CapabilityKind::Action
                    | CapabilityKind::Create
                    | CapabilityKind::Update
                    | CapabilityKind::Delete
            )
        })
        .map(|cap| cap.domain.as_str())
        .collect();
    entities.sort_unstable();
    entities.dedup();
    for ename in entities {
        let mut line_valid_cache = HashMap::new();
        let mut gloss_emit_none = None;
        let block = collect_entity_teaching_block(
            &cgs,
            ename,
            Some(&map_arc),
            None,
            true,
            &mut line_valid_cache,
            seed,
            &mut gloss_emit_none,
            None,
            None,
        );
        let es = map.entity_sym_for("", ename);
        for cap in cgs.capabilities.values().filter(|cap| {
            cap.domain.as_str() == ename
                && matches!(
                    cap.kind,
                    CapabilityKind::Action
                        | CapabilityKind::Create
                        | CapabilityKind::Update
                        | CapabilityKind::Delete
                )
        }) {
            let ms = map.method_sym_for_cap("", cap);
            let row = block
                .teaching_rows
                .iter()
                .find(|r| r.meta.source_capability.as_deref() == Some(cap.name.as_str()));
            let Some(row) = row else {
                continue;
            };
            let expr = row.teaching_expr.expression.as_str();
            let pathless = super::invoke_teaching::receiver_absent(cap);
            if pathless {
                assert!(
                    expr.starts_with(&format!("{es}.{ms}(")),
                    "pathless {} must teach bare {es}.{ms}(, got {expr}",
                    cap.name
                );
            } else {
                assert!(
                    !expr.starts_with(&format!("{es}.{ms}(")),
                    "identity-required {} must not teach pathless {es}.{ms}(, got {expr}",
                    cap.name
                );
                assert!(
                    expr.contains(&format!(".{ms}(")),
                    "identity-required {} must teach a seated .{ms}(, got {expr}",
                    cap.name
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "plasm_prompt_matrix must expose at least one action/create/update teaching row"
    );
}

/// Search teaching rows must not invite copy-paste of `e#~$` / `e#~"text"` (grammar teaches `e#~"<query>"`).
#[test]
fn domain_search_teaching_rows_use_quoted_query_hole() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    for line in prompt.lines() {
        if line.contains('~') && line.starts_with('e') {
            assert!(
                !line.contains("~$") && !line.contains("~\"text\""),
                "search teaching row must use ~\"<query>\" hole, not ~$ / ~\"text\": {line}"
            );
            assert!(
                line.contains("~\"<query>\"") || line.contains("~\""),
                "search teaching row should quote the query operand: {line}"
            );
        }
    }
}

/// Literal Get vs Search seats on `auth_bearer_search` — Get Meaning is identity;
/// Search is the authenticated collection (`↣ [e#]` + `{access_token=}`). P06 keeps Search bare.
#[test]
fn auth_bearer_search_literal_get_vs_search_tsv_seats() {
    let dir = fixtures_schemas_dir("auth_bearer_search");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let note_lines: Vec<&str> = prompt
        .lines()
        .filter(|l| l.starts_with('e') && (l.contains("~\"<query>\"") || l.contains("(<id>)")))
        .collect();
    assert_eq!(
        note_lines,
        [
            "e2(<id>)[note_id,title]\t→ e2 · Secured note",
            "e2~\"<query>\"{access_token=<wire>}[note_id,title]\t↣ [e2]",
        ],
        "Get Meaning is identity, not collection/search; Search writes required Bearer selection; prompt=\n{prompt}"
    );
    let get_meaning = note_lines[0].split_once('\t').map(|(_, m)| m).unwrap_or("");
    let get_lc = get_meaning.to_ascii_lowercase();
    assert!(
        !get_lc.contains("search")
            && !get_lc.contains("list")
            && !get_lc.contains("bearer")
            && !get_lc.contains("send money")
            && !get_lc.contains("or send")
            && !get_lc.contains("deposit or withdraw")
            && !get_lc.contains("or move")
            && !get_lc.contains("creates"),
        "Get Meaning must stay identity polarity: {get_meaning}"
    );
}

/// Federated hole-fill twin: `LangSecuredNote` Get Meaning is the identity noun; Search keeps
/// `↣ [e#]` + `{access_token=}`. Same law as [`auth_bearer_search_literal_get_vs_search_tsv_seats`].
#[test]
fn language_matrix_secured_note_literal_get_vs_search_tsv_seats() {
    for fixture in ["plasm_language_matrix", "plasm_language_matrix_views"] {
        let dir = fixtures_schemas_dir(fixture);
        assert!(dir.exists(), "missing {fixture}");
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt =
            render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval_seeds(&["LangSecuredNote"]));
        let note_lines: Vec<&str> = prompt
            .lines()
            .filter(|l| l.starts_with('e') && (l.contains("~\"<query>\"") || l.contains("(<id>)")))
            .filter(|l| l.contains("note_id"))
            .collect();
        assert_eq!(
            note_lines,
            [
                "e1(<id>)[note_id,body,title]\t→ e1 · Secured note",
                "e1~\"<query>\"{access_token=<wire>}[note_id,body,title]\t↣ [e1]",
            ],
            "{fixture}: Get Meaning is identity, not collection/search; Search writes required Bearer; prompt=\n{prompt}"
        );
        let get_meaning = note_lines[0].split_once('\t').map(|(_, m)| m).unwrap_or("");
        let get_lc = get_meaning.to_ascii_lowercase();
        assert!(
            !get_lc.contains("search")
                && !get_lc.contains("list")
                && !get_lc.contains("bearer")
                && !get_lc.contains("send money")
                && !get_lc.contains("or send")
                && !get_lc.contains("deposit or withdraw")
                && !get_lc.contains("or move")
                && !get_lc.contains("creates"),
            "{fixture}: Get Meaning must stay identity polarity: {get_meaning}"
        );
    }
}

/// Get-bearing language-matrix entities must not inherit a collection/search
/// banner or sibling create-shelf verbs on Get Meaning (identity What only).
#[test]
fn language_matrix_get_meaning_stays_identity_polarity() {
    for fixture in ["plasm_language_matrix", "plasm_language_matrix_views"] {
        let dir = fixtures_schemas_dir(fixture);
        assert!(dir.exists(), "missing {fixture}");
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        for line in prompt.lines() {
            if !line.starts_with('e') {
                continue;
            }
            let Some((expr, meaning)) = line.split_once('\t') else {
                continue;
            };
            let is_get_meaning =
                meaning.contains("→ e") && !expr.contains(".m") && !expr.contains('~');
            if !is_get_meaning {
                continue;
            }
            let lc = meaning.to_ascii_lowercase();
            assert!(
                !lc.contains("search")
                    && !lc.contains("list")
                    && !lc.contains("bearer")
                    && !lc.contains("send money")
                    && !lc.contains("or send")
                    && !lc.contains("deposit or withdraw")
                    && !lc.contains("or move")
                    && !lc.contains("creates"),
                "{fixture}: Get Meaning must stay identity polarity: {line}"
            );
        }
        let group_prompt = render_prompt_tsv_with_config(
            &cgs,
            RenderConfig::for_eval_seeds(&["LangSecuredGroup"]),
        );
        let group_gets: Vec<&str> = group_prompt
            .lines()
            .filter(|l| {
                l.starts_with('e')
                    && l.contains("(<id>)")
                    && !l.contains('~')
                    && l.contains("group_id")
            })
            .collect();
        assert!(
            group_gets.iter().any(|l| l.contains("→ e") && l.contains("Secured group")),
            "{fixture}: LangSecuredGroup Get Meaning must be identity noun; prompt=\n{group_prompt}"
        );
        assert!(
            group_gets.iter().all(|l| {
                let meaning = l.split_once('\t').map(|(_, m)| m).unwrap_or("");
                let lc = meaning.to_ascii_lowercase();
                !lc.contains("search")
                    && !lc.contains("list")
                    && !lc.contains("bearer")
                    && !lc.contains("send money")
                    && !lc.contains("or send")
                    && !lc.contains("deposit or withdraw")
                    && !lc.contains("or move")
                    && !lc.contains("creates")
            }),
            "{fixture}: LangSecuredGroup Get Meaning must stay identity polarity; gets={group_gets:?}"
        );
    }
}

/// Query `scope:` `required: false` is a taught `{wire=}` hole (RA-1), not a silent drop.
#[test]
fn language_matrix_views_optional_scope_is_taught_query_brace_hole() {
    let dir = fixtures_schemas_dir("plasm_language_matrix_views");
    assert!(dir.exists(), "missing plasm_language_matrix_views");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt =
        render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval_seeds(&["LangPivotRow"]));
    let query_rows: Vec<&str> = prompt
        .lines()
        .filter(|l| {
            l.starts_with('e')
                && l.contains('{')
                && l.contains("title=<wire>")
                && !l.contains('~')
                && !l.contains(".m")
        })
        .collect();
    assert_eq!(
        query_rows.len(),
        1,
        "expected one LangPivotRow Query brace row; prompt=\n{prompt}"
    );
    let line = query_rows[0];
    let (expr, meaning) = line.split_once('\t').expect("TSV Query row");
    assert!(
        expr.contains("{item_id=<id>, title=<wire>}"),
        "optional scope must share the Query brace with selection; got: {line}"
    );
    assert!(
        meaning.contains("optional: item_id,title"),
        "optional scope must be named in optional:; got: {line}"
    );
}

/// Required non-text selection (e.g. access_token) rides the primary `~` row; no barren twin
/// that only re-states credentials as optional filters.
#[test]
fn search_teaching_puts_required_selection_on_primary_tilde_row() {
    let dir = fixtures_schemas_dir("auth_bearer_search");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let search_rows: Vec<&str> = prompt
        .lines()
        .filter(|l| l.contains("~\"<query>\"") && l.starts_with('e'))
        .collect();
    assert!(
        !search_rows.is_empty(),
        "expected search teaching rows; prompt=\n{prompt}"
    );
    assert!(
        search_rows
            .iter()
            .any(|l| l.contains("~\"<query>\"{") && l.contains("access_token=")),
        "primary search row must include required access_token; rows={search_rows:?}"
    );
    assert!(
        search_rows
            .iter()
            .all(|l| l.contains("access_token=") || !l.contains("~\"<query>\"")),
        "no bare ~\"<query>\" without required access_token; rows={search_rows:?}"
    );
    assert_eq!(
        search_rows.len(),
        1,
        "credential-only selection must not spawn an optional-filter twin; rows={search_rows:?}"
    );
}

/// Intent-selected Search stays exact even when the same entity also authors Get.
#[test]
fn selected_search_does_not_teach_unselected_get() {
    let dir = fixtures_schemas_dir("auth_bearer_search");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let delta = crate::capability_exposure::selected_capability_surface(
        &cgs,
        "",
        &["securednote_search".into()],
    )
    .expect("search-only selection");
    assert!(
        !delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == "securednote_get"),
        "unselected Get must stay out; caps={:?}",
        delta
            .required
            .capabilities
            .iter()
            .map(|c| c.capability.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == "securednote_search"),
        "Search must remain on the surface"
    );
    let exp = TeachingExposureSession::new_with_intent_delta(&cgs, "", &["SecuredNote"], delta);
    let config = RenderConfig::for_eval_seeds(&["SecuredNote"]);
    let bundle = render_teaching_prompt_bundle_for_exposure(&cgs, config, &exp, None);
    let prompt = render_prompt_tsv_from_bundle(&bundle);
    let note_lines: Vec<&str> = prompt
        .lines()
        .filter(|l| {
            l.starts_with('e')
                && (l.contains("~\"<query>\"") || l.contains("(<id>)") || l.contains("{note_id="))
        })
        .collect();
    let has_search = note_lines.iter().any(|l| l.contains("~\"<query>\""));
    let has_get = note_lines
        .iter()
        .any(|l| l.contains("(<id>)") && !l.contains('~'));
    assert!(has_search && !has_get, "Search-only selection must teach Search without sibling Get; note_lines={note_lines:?}\n{prompt}");
    assert!(
        note_lines
            .iter()
            .all(|l| l.contains('~') || !l.contains("{note_id=")),
        "File polarity: unary Get must not steal token-brace {{note_id=}}; note_lines={note_lines:?}\n{prompt}"
    );
}

/// When routing explicitly selects an entity-ref source and a mutator, their
/// rows compose without admitting unrelated capabilities.
#[test]
fn selected_entity_ref_source_and_mutator_compose_without_siblings() {
    let dir = fixtures_schemas_dir("scoped_query_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let delta = crate::capability_exposure::selected_capability_surface(
        &cgs,
        "scoped_query_matrix",
        &[
            "child_query".into(),
            "child_settle".into(),
            "parent_query".into(),
            "parent_get".into(),
        ],
    )
    .expect("child query surface");
    let exp = TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "scoped_query_matrix",
        &["Child"],
        delta,
    );
    let config = RenderConfig::for_eval_seeds(&["Child"]);
    let bundle = render_teaching_prompt_bundle_for_exposure(&cgs, config, &exp, None);
    let prompt = render_prompt_tsv_from_bundle(&bundle);
    let lines: Vec<&str> = prompt.lines().collect();
    assert!(
        lines.contains(&"e1{parent_id=e2(<id>)}[id]\t↣ [e1]"),
        "Child Query must teach parent_id=e2(<id>), not a bare <wire>; prompt:\n{prompt}"
    );
    assert!(
        lines.contains(&"e2[id]\t↣ [e2] · List owning records"),
        "Parent Query must be first-wave; prompt:\n{prompt}"
    );
    assert!(
        lines.contains(&"e2(<id>)[id]\t→ e2 · An owning record"),
        "Parent Get must be first-wave; prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("Record a settlement on a child"),
        "seeded Child must teach authored mutator child_settle; prompt:\n{prompt}"
    );
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("unrelated peer") && !l.contains("Friend")),
        "Friend must stay untaught; prompt:\n{prompt}"
    );
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("cohort_id") && !l.contains("Create an owning record")),
        "integer cohort_id and Parent create must stay untaught; prompt:\n{prompt}"
    );
}

/// Token-identity Get (`implicit_request_identity`, no Query) keeps `e#{access_token=<wire>}`.
#[test]
fn session_token_get_teaches_token_identity_braces_not_paren() {
    let dir = fixtures_schemas_dir("session_token_get");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval_seeds(&["Wallet"]));
    let wallet_lines: Vec<&str> = prompt
        .lines()
        .filter(|l| l.starts_with('e') && (l.contains("{access_token=") || l.contains("(<id>)")))
        .collect();
    assert!(
        wallet_lines
            .iter()
            .any(|l| l.contains("{access_token=<wire>}") && !l.contains("(<id>)")),
        "Wallet token-identity Get must teach e#{{access_token=<wire>}}; wallet_lines={wallet_lines:?}\n{prompt}"
    );
    assert!(
        wallet_lines.iter().all(|l| !l.contains("(<id>)")),
        "Wallet must not teach a second e#(<id>) hole; wallet_lines={wallet_lines:?}\n{prompt}"
    );
}

/// Executable producers carry `[wires]` by first use; Meaning never says `noun`.
#[test]
fn row_producer_teaching_includes_inputs_and_rows_contract() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    assert!(
        prompt.lines().all(|l| {
            l.split_once('\t')
                .map(|(_, m)| !m.contains("noun"))
                .unwrap_or(true)
        }),
        "teaching Meaning must not contain noun:\n{prompt}"
    );
    assert!(
        prompt.lines().any(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            cols.len() == 2
                && cols[0].contains('{')
                && !cols[0].contains(".r")
                && parse_trailing_projection_bracket(cols[0].trim()).is_some()
                && !cols[1].contains("rows:")
                && !cols[1].contains("noun")
        }),
        "query producers teach brackets by first use:\n{prompt}"
    );
    assert!(
        prompt.lines().any(|l| {
            l.contains("~\"<query>\"")
                && parse_trailing_projection_bracket(l.split('\t').next().unwrap_or("").trim())
                    .is_some()
                && !l.contains("rows:")
        }),
        "divergent search provides keep bracket on expr without rows: in Meaning:\n{prompt}"
    );
}

/// RA-12: Get/Query/Search `[…]` is the full authored field set, not a `provides` summary.
/// Locks the NAPI / `exposeSeeds` path (`selected_capability_surface` + intent delta).
#[test]
fn return_projection_teaching_includes_every_authored_field() {
    let dir = fixtures_schemas_dir("return_projection_teaching");
    let cgs = load_schema_dir(&dir).expect("return_projection_teaching");
    let ent = cgs.get_entity("Notice").expect("Notice");
    let want = CGS::default_ordered_entity_field_names(ent);
    let full = "[notice_id,author_email,body,created_at,title]";
    let summary = "[notice_id,title]";
    assert_eq!(format!("[{}]", want.join(",")), full);

    let delta =
        crate::capability_exposure::selected_capability_surface(&cgs, "", &["notice_get".into()])
            .expect("notice get surface");
    let exp = TeachingExposureSession::new_with_intent_delta(&cgs, "", &["Notice"], delta);
    let config = RenderConfig::for_eval_seeds(&["Notice"]);
    let bundle = render_teaching_prompt_bundle_for_exposure(&cgs, config, &exp, None);
    let prompt = render_prompt_tsv_from_bundle(&bundle);
    let map = exp.to_symbol_map();
    let want_tokens: Vec<String> = want
        .iter()
        .map(|k| map.ident_sym_entity_field_for("", "Notice", k))
        .collect();
    let full_tokens = format!("[{}]", want_tokens.join(","));

    let taught: Vec<&str> = prompt
        .lines()
        .filter(|l| {
            !l.starts_with('#')
                && !l.is_empty()
                && parse_trailing_projection_bracket(l.split('\t').next().unwrap_or("").trim())
                    .is_some()
        })
        .collect();
    assert!(
        !taught.is_empty(),
        "expected a teaching line with `[…]`; card:\n{prompt}"
    );
    for line in &taught {
        let expr = line.split('\t').next().unwrap_or("");
        let br = parse_trailing_projection_bracket(expr.trim()).expect("bracket");
        assert_eq!(
            br, full_tokens,
            "NAPI surface `[…]` must be RA-12 {full} (tokens {full_tokens}); BEFORE provides-summary was {summary}:\n{line}"
        );
        assert_ne!(
            br, summary,
            "must not teach the provides display subset:\n{line}"
        );
    }
    assert!(
        prompt.contains("author_email") && prompt.contains("created_at"),
        "extra decode fields must appear on the card:\n{prompt}"
    );

    let tx = cgs.get_entity("Transaction").expect("Transaction");
    let tx_want = CGS::default_ordered_entity_field_names(tx);
    let tx_full = "[transaction_id,amount,created_at,description,private]";
    let tx_summary = "[transaction_id,amount,description]";
    assert_eq!(format!("[{}]", tx_want.join(",")), tx_full);

    let tx_delta = crate::capability_exposure::selected_capability_surface(
        &cgs,
        "",
        &["transaction_get".into()],
    )
    .expect("transaction get surface");
    let tx_exp =
        TeachingExposureSession::new_with_intent_delta(&cgs, "", &["Transaction"], tx_delta);
    let tx_config = RenderConfig::for_eval_seeds(&["Transaction"]);
    let tx_bundle = render_teaching_prompt_bundle_for_exposure(&cgs, tx_config, &tx_exp, None);
    let tx_prompt = render_prompt_tsv_from_bundle(&tx_bundle);
    let tx_map = tx_exp.to_symbol_map();
    let tx_tokens: Vec<String> = tx_want
        .iter()
        .map(|k| tx_map.ident_sym_entity_field_for("", "Transaction", k))
        .collect();
    let tx_full_tokens = format!("[{}]", tx_tokens.join(","));

    let tx_taught: Vec<&str> = tx_prompt
        .lines()
        .filter(|l| {
            !l.starts_with('#')
                && !l.is_empty()
                && parse_trailing_projection_bracket(l.split('\t').next().unwrap_or("").trim())
                    .is_some()
        })
        .collect();
    assert!(
        !tx_taught.is_empty(),
        "expected a Transaction teaching line with `[…]`; card:\n{tx_prompt}"
    );
    for line in &tx_taught {
        let expr = line.split('\t').next().unwrap_or("");
        let br = parse_trailing_projection_bracket(expr.trim()).expect("bracket");
        assert_eq!(
            br, tx_full_tokens,
            "NAPI surface `[…]` must be RA-12 {tx_full} (tokens {tx_full_tokens}); BEFORE provides-summary was {tx_summary}:\n{line}"
        );
        assert_ne!(
            br, tx_summary,
            "must not teach the Transaction provides display subset:\n{line}"
        );
    }
    assert!(
        tx_prompt.contains("created_at") && tx_prompt.contains("private"),
        "Transaction decode fields sheared by provides must appear on the card:\n{tx_prompt}"
    );
}

#[test]
fn static_grammar_includes_symbols_only_rule() {
    let g = super::PLASM_TOOL_DESCRIPTION;
    assert!(
        g.contains("wire names"),
        "canonical static grammar must teach wire names"
    );
    assert!(
        g.contains("Never emit `v#`") || g.contains("Never emit v#"),
        "canonical static grammar must forbid emitting v#"
    );
    assert!(
        g.contains("→ e") && g.contains("label = e#"),
        "bare-entity / bind-before-project rite must remain (P17)"
    );
    assert!(
        !g.contains("Entity heads vs rows:") && !g.contains("↣ [e]"),
        "P06/P07/P13 entity-head and arrow-legend pedagogy are CEILING DROP"
    );
}

#[test]
fn sole_nullary_get_fixture_teaches_bare_e_first_with_gloss() {
    use crate::FocusSpec;
    use crate::TeachingExposureSession;

    let dir = fixtures_schemas_dir("sole_nullary_get");
    assert!(dir.exists(), "missing fixture {dir:?}");
    let cgs = load_schema_dir(&dir).unwrap();
    let pipeline = PromptPipelineConfig::default();
    let exp = TeachingExposureSession::new(&cgs, "", &["Profile"]);
    let first = pipeline.render_teaching_first_wave_for_session(&cgs, &exp, None);
    let (_, body) = split_tsv_teaching_contract_and_table(&first);
    validate_teaching_tsv_teaching_table(&body).expect("valid teaching rows");

    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let e = map.entity_sym_for("", "Profile");
    let mut e_rows = body.lines().filter_map(|l| {
        l.split_once('\t').and_then(|(expr, meaning)| {
            let expr = expr.trim();
            if expr.starts_with('e')
                && (expr == e.as_str()
                    || expr.starts_with(&format!("{e}["))
                    || expr.starts_with(&format!("{e}."))
                    || expr.starts_with(&format!("{e}(")))
            {
                Some((expr.to_string(), meaning.to_string()))
            } else {
                None
            }
        })
    });
    let (first_expr, first_meaning) = e_rows.next().expect("Profile e# row");
    assert!(
        first_expr.starts_with(&format!("{e}[")) && first_expr.contains("first_name"),
        "first e# must be singleton fetch with field alphabet, got {first_expr:?}\n{body}"
    );
    assert!(
        first_meaning.contains("→")
            && first_meaning.contains("Show phone profile by phone number")
            && !first_meaning.contains("materialize"),
        "expected →e + gloss without materialize mark, got {first_meaning:?}"
    );
    assert!(
        !body
            .lines()
            .any(|l| l.contains(&format!("{e}.m")) && l.contains("()\t")),
        "must not teach redundant e#.m#() for sole singleton Get:\n{body}"
    );
}

#[test]
fn sole_nullary_get_bare_program_normalizes_and_typechecks() {
    let dir = fixtures_schemas_dir("sole_nullary_get");
    let cgs = load_schema_dir(&dir).unwrap();
    let mut parsed = crate::expr_parser::parse("Profile", &cgs).expect("parse bare Profile");
    assert!(matches!(parsed.expr, crate::Expr::Query(_)));
    crate::normalize_expr_query_capabilities(&mut parsed.expr, &cgs).unwrap();
    assert!(
        matches!(parsed.expr, crate::Expr::Get(_)),
        "bare Profile must desugar to Get"
    );
    crate::type_check_expr(&parsed.expr, &cgs).expect("typecheck Get");
}

#[test]
fn singleton_row_fetch_tsv_meaning_has_gloss_not_chain_hint() {
    use super::input_legend::{CapabilityInputLegend, RowContractLegend, TeachingExprLine};
    use super::tsv_emit::{write_teaching_tsv_row, DomainTsvRow};
    use super::{ReturnArrow, TeachingHeading};

    let legend = CapabilityInputLegend {
        description: "Show the current profile".to_string(),
        ..Default::default()
    };
    let line = TeachingExprLine {
        expression: "e2.m2()".to_string(),
        result_type: "e2".to_string(),
        legend,
        is_projection_teaching: false,
        is_singleton_row_fetch: true,
        row_contract: RowContractLegend::default(),
        arrow: ReturnArrow::Single,
    };
    let heading = TeachingHeading::default();
    let mut out = String::new();
    write_teaching_tsv_row(
        &mut out,
        DomainTsvRow::TeachingExpr {
            line: &line,
            identity_returns_row: false,
            attach_entity_heading: false,
            heading: &heading,
        },
    );
    assert!(
        out.contains("e2.m2()\t→ e2 · Show the current profile"),
        "expected →e + capability gloss, got {out:?}"
    );
    assert!(
        !out.contains("materialize"),
        "must not emit obsolete materialize mark: {out:?}"
    );
    assert!(
        !out.contains("chain:"),
        "singleton row fetch must not emit write chain hint: {out:?}"
    );
    assert!(
        !out.contains("op=query_all"),
        "must not emit verbose T1 tags: {out:?}"
    );
}

#[test]
fn teaching_prompt_bundle_tags_relation_nav_materialization() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let bundle = render_teaching_prompt_bundle(&cgs, RenderConfig::for_eval_seeds(&["LangItem"]));
    let found = bundle
        .model
        .entities
        .iter()
        .flat_map(|e| &e.lines)
        .any(|l| {
            l.kind == DomainLineKind::RelationNav
                && matches!(
                    l.relation_materialization,
                    Some(RelationMaterializationSummary::FromParentGet)
                )
        });
    assert!(
        found,
        "expected a relation teaching line with FromParentGet metadata"
    );
    let mut cfg = RenderConfig::for_eval_canonical(None);
    cfg.include_domain_execution_model = false;
    let bundle2 = render_teaching_prompt_bundle(&cgs, cfg);
    assert!(bundle2.model.entities.is_empty());
}

#[test]
fn petstore_domain_lists_capabilities() {
    let dir = fixtures_schemas_dir("petstore");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let output = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
    assert!(
        output.contains("Pet") && output.contains("plasm_expr\tMeaning"),
        "TSV prompt should list Pet"
    );
    assert!(
        !output.contains("shape:"),
        "TSV prompt should not prefix every line with shape:"
    );
    assert!(
        output.contains("Pet{") && output.contains("status"),
        "domain should surface query brace form with status from CGS"
    );
}

#[test]
fn petstore_domain_line_meta_includes_source_capability() {
    let dir = fixtures_schemas_dir("petstore");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let bundle = render_teaching_prompt_bundle(
        &cgs,
        RenderConfig {
            focus: FocusSpec::All,
            render_mode: PromptRenderMode::Canonical,
            include_domain_execution_model: true,
            symbol_map_cross_cache: None,
        },
    );
    let pet = bundle
        .model
        .entities
        .iter()
        .find(|e| e.entity == "Pet")
        .expect("Pet teaching block");
    let bound = pet
        .lines
        .iter()
        .filter(|l| l.source_capability.is_some())
        .count();
    assert!(
        bound > 0,
        "expected at least one teaching table line bound to a CGS capability id"
    );
    assert!(pet
        .lines
        .iter()
        .all(|l| { l.kind != DomainLineKind::RelationNav || l.source_capability.is_none() }));
}

#[test]
fn focus_subsetting_shows_full_and_dim() {
    let dir = fixtures_schemas_dir("petstore");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let output = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(Some("Order")));
    assert!(output.contains("Order"));
    assert!(output.contains("User") || output.contains("Pet"));
}

#[test]
fn language_matrix_bundle_is_reasonable_size() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let out = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
    assert!(out.len() < 50_000, "bundle should stay bounded");
    assert!(!out.contains("EXAMPLES:") && out.contains("plasm_expr\tMeaning"));
}

/// `LangItem(id).tags` uses prefer-embed / scoped materialization — teaching shows
/// anchored relation nav plus scoped `LangTag{…}`.
#[test]
fn langitem_domain_includes_materialized_tags_nav() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let mut cgs = load_schema_dir(&dir).unwrap();
    cgs.bind_registry_entry_id("langmatrix");
    let sym = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    let raw = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let item_sym = map.entity_sym_for("langmatrix", "LangItem");
    let tags_rel = map.ident_sym_relation_for("langmatrix", "LangItem", "tags");
    let item_ent = cgs.get_entity("LangItem").expect("LangItem");
    let p_item_identity =
        map.ident_sym_entity_field_for("langmatrix", "LangItem", item_ent.id_field.as_str());
    assert!(
        raw.contains(".tags")
            && (raw.contains("LangItem(<id>)")
                || raw.contains(&format!("LangItem({p_item_identity})"))
                || raw.contains("LangItem{"))
            && raw.contains("LangItem"),
        "expected LangItem→tags relation line (chain materialization)"
    );
    assert!(
        sym.contains(&format!(".{tags_rel}"))
            || sym.contains(&format!("{item_sym}(<id>).{tags_rel}"))
            || sym.contains(&format!("{item_sym}({p_item_identity}).{tags_rel}"))
            || sym.contains(&format!("{item_sym}{{")),
        "expected symbol-tuned LangItem→tags relation (`.{tags_rel}` on a `{item_sym}` receiver)"
    );
    assert!(
        raw.contains("LangTag{") && raw.contains("item_id"),
        "LangTag scoped query with item_id should remain in teaching table (canonical)"
    );
    assert!(
        sym.contains("LangTag{")
            || (sym.contains("{") && sym.contains(&format!("={item_sym}(")))
            || raw.contains("LangTag{"),
        "LangTag scoped query should remain in teaching table"
    );
}

/// `team_query` is query-shaped (`e1` in teaching table); capability prose is intentionally omitted from
/// `Meaning` (types teach shape); see `omit_capability_prose` in teaching synthesis.
#[test]
fn langitem_domain_gloss_and_symbol_map_queries() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let mut cgs = load_schema_dir(&dir).unwrap();
    cgs.bind_registry_entry_id("langmatrix");
    let sym = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    assert!(
        !sym.contains("FIELDS\n"),
        "global FIELDS block removed — wire gloss is inline before first use"
    );
    assert!(
        sym.lines().any(|line| {
            line.split_once('\t').is_some_and(|(expr, _)| {
                parse_trailing_projection_bracket(expr.trim())
                    .map(|inner| {
                        super::row_producer_teaching::projection_bracket_syms(&format!("[{inner}]"))
                            .iter()
                            .all(|s| !crate::symbol_tuning::SymbolMap::is_opaque_p_sym(s.as_str()))
                    })
                    .unwrap_or(false)
            })
        }),
        "projection witness should use wire field names in brackets"
    );
    assert!(
        !sym.contains("QUERIES\n"),
        "QUERIES table removed — capability text lives on teaching lines"
    );
    assert!(
        !sym.contains("METHODS\n"),
        "METHODS table removed — invoke glosses live on teaching lines"
    );
    let domain_start = sym
        .find(TSV_TEACHING_TABLE_HEADER.trim_end())
        .expect("teaching table header");
    let domain_block = &sym[domain_start..];
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let team_sym = map.entity_sym_for("langmatrix", "LangItem");
    assert!(
        super::PLASM_TOOL_DESCRIPTION.contains(super::TEACHING_VALID_EXPR_MARKER),
        "canonical grammar const should include valid-expression rules"
    );
    assert!(
        domain_block.lines().any(|line| {
            line.split_once('\t').is_some_and(|(expr, meaning)| {
                expr.starts_with(team_sym.as_str())
                    && (meaning.contains('↣')
                        || meaning.contains('→')
                        || meaning.contains("returns"))
                    && meaning.contains(&format!("[{team_sym}]"))
            })
        }),
        "TSV langitem_query should teach collection result gloss for LangItem (`[{team_sym}]`) without capability prose"
    );
    assert!(
        !domain_block.contains(" -> "),
        "relation / field nav lines must use `;;  => e#` (or `[e#]`), not `expr -> e#` before ;;"
    );
    let task_sym = map.entity_sym_for("langmatrix", "LangTag");
    let p_team_id =
        map.ident_sym_cap_param_for("langmatrix", "LangTag", "langtag_query", "item_id");
    let team_ent = cgs.get_entity("LangItem").expect("LangItem");
    let p_team_identity =
        map.ident_sym_entity_field_for("langmatrix", "LangItem", team_ent.id_field.as_str());
    assert!(
        domain_block.contains(&format!(
            "{}{{{}={}({})",
            task_sym, p_team_id, team_sym, p_team_identity
        )) || domain_block.contains(&format!(
            "{}{{{}={}(<id>)",
            task_sym, p_team_id, team_sym
        )),
        "item-scoped LangTag query should teach scope with unary entity-ref fill-in (p#=e#(id_slot) or e#(<id>))"
    );
    assert!(
        !domain_block.contains("2000-01-01") && !domain_block.contains("p10>=\""),
        "query teaching table brace form must not teach concrete ISO datetimes or `>=` date literals"
    );
}

/// Query-primary LangItem first e# row is `e#` or `e#[…]` (not `e#(42)` / `e#.m#()`).
#[test]
fn langitem_query_primary_first_row_is_bare_or_projected() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let mut cgs = load_schema_dir(&dir).unwrap();
    cgs.bind_registry_entry_id("langmatrix");
    let sym = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let user_sym = map.entity_sym_for("langmatrix", "LangItem");
    let first_user_e = sym.lines().find_map(|l| {
        l.split_once('\t').and_then(|(expr, meaning)| {
            let expr = expr.trim();
            if expr == user_sym.as_str()
                || expr.starts_with(&format!("{user_sym}["))
                || expr.starts_with(&format!("{user_sym}."))
                || expr.starts_with(&format!("{user_sym}("))
            {
                Some((expr.to_string(), meaning.to_string()))
            } else {
                None
            }
        })
    });
    let (expr, meaning) = first_user_e.expect("LangItem must have an e# teaching row");
    assert!(
        expr == user_sym.as_str() || expr.starts_with(&format!("{user_sym}[")),
        "query-primary first e# row must be bare or projected {user_sym}, got {expr:?}"
    );
    assert!(
        meaning.contains("↣") || meaning.contains("→"),
        "LangItem Meaning should carry a return arrow, got {meaning:?}"
    );
}

/// Derived unary Get teaches `e#(<id>)`, not `e#.m#()`, and lowers to `Expr::Get`.
#[test]
fn keyed_view_get_teaches_brace_identity_not_method_invoke() {
    let dir = fixtures_schemas_dir("plasm_language_matrix_views");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exp = TeachingExposureSession::new(&cgs, "", &["LangKeyPick"]);
    let body =
        PromptPipelineConfig::default().render_teaching_first_wave_for_session(&cgs, &exp, None);
    let (_, table) = split_tsv_teaching_contract_and_table(&body);
    validate_teaching_tsv_teaching_table(&table).expect("valid teaching rows");

    assert!(
        table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            expr.contains("(<id>)") && !expr.contains(".m") && !expr.contains("{key=")
        }),
        "expected unary get row e#(<id>) in teaching table:\n{table}"
    );
    assert!(
        !table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("");
            expr.contains(".m") && expr.ends_with("()") && expr.starts_with('e')
        }),
        "derived keyed Get must not teach invalid e#.m#() invoke:\n{table}"
    );
    let body_filled = body.replace("<id>", "\"item-1\"");
    let line = body_filled
        .lines()
        .find(|l| l.contains("(\"item-1\")"))
        .expect("unary identity line");
    let expr = line.split('\t').next().unwrap().trim();
    let parsed = crate::expr_parser::parse_session_line(expr, &cgs, Some(exp.symbol_map_arc()))
        .expect("parse unary teaching");
    assert!(
        matches!(parsed.expr, crate::Expr::Get(_)),
        "unary LangKeyPick teaching must lower to Get, got {:?}",
        parsed.expr
    );
}

/// Pathless Action (`langitem_broadcast`) teaches bare `eN.mN(...)`, never `eN(<id>).mN(...)`.
#[test]
fn pathless_action_teaches_bare_entity_method_not_identity_paren() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let cap = cgs
        .get_capability("langitem_broadcast")
        .expect("langitem_broadcast");
    assert!(
        super::invoke_teaching::receiver_absent(cap),
        "broadcast must be pathless for this witness"
    );
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let map_arc = std::sync::Arc::new(map.clone());
    let block = collect_entity_teaching_block(
        &cgs,
        "LangItem",
        Some(&map_arc),
        None,
        true, // need source_capability metadata
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        None,
        None,
    );
    let es = map.entity_sym_for("", "LangItem");
    let ms = map.method_sym_for_cap("", cap);
    let row = block
        .teaching_rows
        .iter()
        .find(|r| {
            r.meta.source_capability.as_deref() == Some("langitem_broadcast")
                || r.teaching_expr
                    .expression
                    .starts_with(&format!("{es}.{ms}("))
        })
        .unwrap_or_else(|| {
            let caps: Vec<_> = block
                .teaching_rows
                .iter()
                .map(|r| {
                    (
                        r.meta.source_capability.as_deref(),
                        r.teaching_expr.expression.as_str(),
                    )
                })
                .collect();
            panic!("broadcast teaching row missing; rows={caps:?}");
        });
    let expr = row.teaching_expr.expression.as_str();
    assert!(
        expr.starts_with(&format!("{es}.{ms}(")),
        "pathless Action must teach bare {es}.{ms}(...), got {expr}"
    );
    assert!(
        !expr.starts_with(&format!("{es}(")),
        "pathless Action must not teach identity paren receiver, got {expr}"
    );
}

/// Book —(shelf)—> Shelf; two query caps; one navigation edge from Book.
fn prompt_stats_fixture_cgs() -> CGS {
    let mut cgs = CGS::new();
    cgs.values.insert(
        "fixture_str".into(),
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
    let id_field = FieldSchema {
        name: "id".into(),
        kind: FieldValueKind::Registry(ValueDomainKey::new("fixture_str").expect("key")),
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
        name: "Book".into(),
        description: String::new(),
        id_field: "id".into(),
        id_format: None,
        id_from: None,
        fields: vec![id_field.clone()],
        relations: vec![RelationSchema {
            name: "shelf".into(),
            description: String::new(),
            target_resource: "Shelf".into(),
            // One + no materialize stays valid under the executable-materialize gate;
            // teaching still omits a nav TSV line when the hop is not materialized.
            cardinality: Cardinality::One,
            materialize: None,
            discovery: None,
        }],
        expression_aliases: vec![],
        implicit_request_identity: false,
        key_vars: vec![],
        abstract_entity: false,
        domain_projection_examples: false,
        primary_read: None,
        primary_query: None,
        primary_search: None,
        discovery: None,
    })
    .unwrap();
    cgs.add_resource(ResourceSchema {
        name: "Shelf".into(),
        description: String::new(),
        id_field: "id".into(),
        id_format: None,
        id_from: None,
        fields: vec![id_field],
        relations: vec![],
        expression_aliases: vec![],
        implicit_request_identity: false,
        key_vars: vec![],
        abstract_entity: false,
        domain_projection_examples: false,
        primary_read: None,
        primary_query: None,
        primary_search: None,
        discovery: None,
    })
    .unwrap();
    let tmpl = serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "x"}]});
    for (name, domain) in [("book_query", "Book"), ("shelf_query", "Shelf")] {
        cgs.add_capability(CapabilitySchema {
            name: name.into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: domain.into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: tmpl.clone().into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
    }
    cgs.validate().unwrap();
    cgs
}

#[test]
fn prompt_surface_stats_counts_caps_nav_and_domain_tools() {
    let cgs = prompt_stats_fixture_cgs();
    // Symbolic render modes — same entity slice as execute / [`teaching_exposure_session_from_focus`]
    // (seed-only for Single/Seeds; no 2-hop union).
    let (c_all, n_all) = json_tool_surface_counts(&cgs, FocusSpec::All, true);
    assert_eq!((c_all, n_all), (2, 1));

    let (c_book, n_book) = json_tool_surface_counts(&cgs, FocusSpec::Single("Book"), true);
    assert_eq!((c_book, n_book), (1, 1));

    let (c_shelf, n_shelf) = json_tool_surface_counts(&cgs, FocusSpec::Single("Shelf"), true);
    assert_eq!((c_shelf, n_shelf), (1, 0));

    // Legacy 2-hop neighbourhood when render mode is canonical.
    let (c_book_2hop, n_book_2hop) =
        json_tool_surface_counts(&cgs, FocusSpec::Single("Book"), false);
    assert_eq!((c_book_2hop, n_book_2hop), (2, 1));

    let cfg = RenderConfig::for_eval(None);
    let (names, exposure_opt) =
        resolve_prompt_surface_entities(&cgs, cfg.focus, cfg.uses_symbols());
    let domain_tools = super::domain_expression_tool_count_resolved(
        &cgs,
        &names,
        exposure_opt.as_ref(),
        cfg.uses_symbols(),
    );
    // Book: list query + `rows = <query>` + `rows => _.r#`; Shelf: list query.
    // Cardinality::One is nav-admissible; fanout echoes the taught Book query head.
    assert_eq!(domain_tools, 4);

    let prompt = "αβγδε"; // 5 chars → legacy est 1; o200k is model-based
    let st = prompt_surface_stats(&cgs, cfg, prompt);
    assert_eq!(st.prompt_chars, 5);
    assert_eq!(st.token_estimate, 1);
    assert_eq!(
        st.prompt_tokens_o200k,
        crate::o200k_token_count::o200k_token_count(prompt)
    );
    assert_eq!(st.capability_tools, 2);
    assert_eq!(st.navigation_tools, 1);
    assert_eq!(st.json_tool_estimate, domain_tools);
    let sum = st.summary_line_body();
    assert!(sum.contains("tok (o200k)"));
    assert!(sum.contains("chars/4)"));
}

fn string_id_field(description: &str) -> FieldSchema {
    FieldSchema {
        name: "id".into(),
        kind: FieldValueKind::Registry(ValueDomainKey::new("fixture_str").expect("key")),
        description: description.to_string(),
        required: true,
        agent_presentation: None,
        mime_type_hint: None,
        attachment_media: None,
        wire_path: None,
        derive: None,
        data_class: None,
        currency_field: None,
    }
}

/// Two entities, same wire field `id` (maps to one `p#`), optional distinct descriptions — for
/// [`emit_field_def_lines_before_example`] identity tests.
fn p_slot_redefinition_fixture_cgs(id_desc_a: &str, id_desc_b: &str) -> CGS {
    let mut cgs = CGS::new();
    cgs.values.insert(
        "fixture_str".into(),
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
    for (name, desc) in [("Anvil", id_desc_a), ("Beryl", id_desc_b)] {
        cgs.add_resource(ResourceSchema {
            name: name.into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![string_id_field(desc)],
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
        let cap_name: String = format!("{}_get", name.to_lowercase());
        cgs.add_capability(CapabilitySchema {
            name: cap_name.into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: name.into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": name.to_lowercase()},
                        {"type": "var", "name": "id"},
                    ],
                })
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
    }
    cgs.validate()
        .expect("p_slot_redefinition fixture must validate");
    cgs
}

/// Same `p#` for wire `id`, same structural type — description change forces a second gloss line.
#[test]
fn compact_domain_re_emits_p_slot_gloss_when_description_identity_changes() {
    let cgs = p_slot_redefinition_fixture_cgs("P_SLOT_REIDENT_ALPHA", "P_SLOT_REIDENT_BETA");
    let prompt = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    let domain = prompt
        .find(TEACHING_VALID_EXPR_MARKER)
        .map(|i| &prompt[i..])
        .unwrap_or(&prompt);
    let gloss_hits: Vec<_> = domain
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("id\t") && t.contains("P_SLOT_REIDENT_")
        })
        .collect();
    assert!(
        gloss_hits
            .iter()
            .any(|l| l.contains("P_SLOT_REIDENT_ALPHA")),
        "expected first-entity id gloss with ALPHA marker; gloss lines: {gloss_hits:?}"
    );
    assert!(
        gloss_hits.iter().any(|l| l.contains("P_SLOT_REIDENT_BETA")),
        "expected second-entity id re-gloss with BETA marker; gloss lines: {gloss_hits:?}"
    );
}

/// Same-shaped `id` slots on different entities share one opaque `p#`; identical compact gloss is taught once.
#[test]
fn compact_domain_dedupes_identical_p_slot_gloss_across_entities() {
    let same = "P_SLOT_REIDENT_SAME";
    let cgs = p_slot_redefinition_fixture_cgs(same, same);
    let prompt = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    let domain = prompt
        .find(TEACHING_VALID_EXPR_MARKER)
        .map(|i| &prompt[i..])
        .unwrap_or(&prompt);
    let count = domain
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("id\t") && t.contains("P_SLOT_REIDENT_SAME")
        })
        .count();
    assert_eq!(
        count, 1,
        "expected one wire `id` gloss row when teaching strings match across entities; domain excerpt:\n{domain}"
    );
}

fn assert_prompt_examples_parse(dir: &std::path::Path) {
    assert_prompt_examples_valid(dir, RenderConfig::for_eval(None));
}

/// teaching lines must **parse**, **resolve** query capabilities where applicable, and **type-check**
/// — the same baseline as execution (not merely syntactic validity).
fn assert_prompt_examples_valid(dir: &std::path::Path, config: RenderConfig<'_>) {
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(dir).unwrap();
    let map =
        crate::symbol_tuning::symbol_map_for_prompt(&cgs, config.focus, config.uses_symbols());
    let prompt = if config.render_mode.is_tsv() {
        render_prompt_tsv_with_config(&cgs, config)
    } else {
        render_prompt_with_config(&cgs, config)
    };
    let exprs = example_expressions_from_prompt(&prompt);
    assert!(
        !exprs.is_empty(),
        "expected teaching section with expressions for {}",
        dir.display()
    );
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let mut cache = HashMap::new();
    for expr in &exprs {
        // Angle-bracket teaching holes are templates; validate the `$` / `"q"` stand-in form.
        let expr_for_check = if expr.contains('<') {
            super::teaching_util::teaching_expr_for_validation(expr)
        } else {
            expr.clone()
        };
        assert!(
            domain_line_validate_cached(&mut cache, seed, &cgs, &expr_for_check, map.as_ref())
                .is_some(),
            "teaching table expr should validate for {}: {expr_for_check:?}",
            dir.display()
        );
    }
}

#[test]
fn petstore_rendered_examples_parse() {
    assert_prompt_examples_parse(&fixtures_schemas_dir("petstore"));
}

#[test]
fn language_matrix_rendered_examples_parse() {
    assert_prompt_examples_parse(&fixtures_schemas_dir("plasm_language_matrix"));
}

#[test]
fn prompt_matrix_rendered_examples_parse() {
    assert_prompt_examples_parse(&fixtures_schemas_dir("plasm_prompt_matrix"));
}

#[test]
fn query_domain_lines_match_expr_shape() {
    assert_eq!(query_construct_display("e4", "e4"), "e4");
    assert_eq!(
        query_construct_display("e4", "*p41=e2(id) *p25=e3(id)"),
        "e4{p41=e2(id), p25=e3(id)}"
    );
    assert_eq!(
        query_construct_display("e4", "*p41=e2(id)"),
        "e4{p41=e2(id)}"
    );
}

/// Locks compact teaching table + symbol preamble for `fixtures/schemas/overshow_tools`.
/// Update with `INSTA_UPDATE=always cargo test -p plasm-core overshow_tools_compact_prompt_snapshot -- --exact`.
#[test]
fn overshow_tools_compact_prompt_snapshot() {
    let dir = fixtures_schemas_dir("overshow_tools");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = pin_teaching_snapshot_clock(&render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    ));
    with_insta_snapshots(|| {
        insta::assert_snapshot!("overshow_tools_compact_prompt", prompt);
    });
}

/// Locks language card render for the same fixture (review diffs with compact snapshot above).
#[test]
fn overshow_tools_prompt_tsv_snapshot() {
    let dir = fixtures_schemas_dir("overshow_tools");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = pin_teaching_snapshot_clock(&render_prompt_tsv_with_config(
        &cgs,
        RenderConfig::for_eval(None),
    ));
    with_insta_snapshots(|| {
        insta::assert_snapshot!("overshow_tools_prompt_tsv", tsv);
    });
}

/// Federated open: colliding wire entity names get distinct `e#` in language card rows (B1).
#[test]
fn federated_duplicate_entity_wire_names_use_distinct_e_in_teaching_tsv() {
    use std::sync::Arc;

    let root = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&root).expect("plasm_language_matrix");
    let layers = [&cgs, &cgs];
    let mut exp = TeachingExposureSession::new(&cgs, "langmatrix_a", &["LangItem"]);
    exp.expose_entities(
        &layers,
        Arc::new(cgs.clone()),
        "langmatrix_b",
        &["LangItem"],
    );
    let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
    by_entry.insert("langmatrix_a".into(), &cgs);
    by_entry.insert("langmatrix_b".into(), &cgs);
    let bundle = render_teaching_prompt_bundle_for_exposure_federated(
        &by_entry,
        RenderConfig::for_eval(None),
        &exp,
        None,
    );
    assert!(
        bundle.teaching_blocks.len() >= 2,
        "expected langmatrix_a + langmatrix_b LangItem blocks"
    );
    let row_text = |block: &EntityTeachingBlock| {
        block
            .teaching_rows
            .iter()
            .map(|r| r.teaching_expr.expression.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let primary_text = row_text(&bundle.teaching_blocks[0]);
    let secondary_text = row_text(&bundle.teaching_blocks[1]);
    assert!(
        primary_text.contains("e1"),
        "langmatrix_a LangItem teaching rows should use e1: {primary_text}"
    );
    assert!(
        !primary_text.contains("e2"),
        "langmatrix_a block must not bleed langmatrix_b e2: {primary_text}"
    );
    assert!(
        secondary_text.contains("e2"),
        "langmatrix_b LangItem teaching rows should use e2: {secondary_text}"
    );
}

/// Federated homographs: method/query Meaning return atoms must be opaque `e#` / `[e#]` / `()`,
/// never the colliding wire entity name (AppWorld `AuthSession` class of bug).
#[test]
fn federated_homograph_method_returns_use_opaque_e_never_bare_wire() {
    use std::sync::Arc;

    let root = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&root).expect("plasm_language_matrix");
    let layers = [&cgs, &cgs];
    let mut exp = TeachingExposureSession::new(&cgs, "langmatrix_a", &["LangItem"]);
    exp.expose_entities(
        &layers,
        Arc::new(cgs.clone()),
        "langmatrix_b",
        &["LangItem"],
    );
    let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
    by_entry.insert("langmatrix_a".into(), &cgs);
    by_entry.insert("langmatrix_b".into(), &cgs);
    let bundle = render_teaching_prompt_bundle_for_exposure_federated(
        &by_entry,
        RenderConfig::for_eval(None),
        &exp,
        None,
    );
    assert!(bundle.teaching_blocks.len() >= 2);

    // Unqualified wire must not appear as a return atom after ↠ / → / ↣.
    let bare_return = regex::Regex::new(r"[↠→↣]\s*LangItem\b").expect("regex");
    let bare_list = regex::Regex::new(r"[↠→↣]\s*\[LangItem\]").expect("regex");
    let mut saw_opaque_e = false;
    for block in &bundle.teaching_blocks {
        for row in &block.teaching_rows {
            let rt = row.teaching_expr.result_type.as_str();
            assert!(
                !bare_return.is_match(rt) && !bare_list.is_match(rt),
                "result_type must not use bare wire LangItem: expr={} result_type={rt}",
                row.teaching_expr.expression
            );
            assert!(
                rt != "LangItem" && rt != "[LangItem]",
                "result_type must not be bare wire alone: expr={} result_type={rt}",
                row.teaching_expr.expression
            );
            if rt.contains("e1") || rt.contains("e2") {
                saw_opaque_e = true;
            }
        }
    }
    assert!(
        saw_opaque_e,
        "expected opaque e# in federated Meaning cells"
    );
}

/// Production catalogs: `github/Issue` + `linear/Issue` federated TSV uses e1 vs e2.
/// Matrix fixture: `from_parent_get` many-relation without target Get must type-check and
/// produce validated relation edge-delta teaching when the child entity is seeded on extend.
#[test]
fn from_parent_get_nav_matrix_relation_fanout_type_checks_and_edge_delta_validates() {
    use crate::type_checker::type_check_chain;
    use crate::{ChainExpr, Expr, GetExpr};

    let dir = fixture_schema_dir("from_parent_get_nav");
    let mut cgs = load_schema_dir(&dir).expect("from_parent_get_nav fixture");
    cgs.bind_registry_entry_id("from_parent_get_nav");
    let chain = ChainExpr::auto_get(Expr::Get(GetExpr::new("ParentItem", "p-1")), "tags");
    type_check_chain(&chain, &cgs).expect("ParentItem.tags from_parent_get chain");

    let mut cache = HashMap::new();
    let pipeline = PromptPipelineConfig::default();
    let mut exp = TeachingExposureSession::new(&cgs, "from_parent_get_nav", &["ParentItem"]);
    let slots_before = exp.surface.slots.clone();
    let cgs_arc = std::sync::Arc::new(cgs.clone());
    exp.expose_entities(&[&cgs], cgs_arc, "from_parent_get_nav", &["Tag"]);
    let map_arc = exp.symbol_map_arc();
    let added = exp.qualified_entities_since(1);
    let new_relation_slots = exp.relation_edge_delta_slots(&slots_before, &added);
    exp.admit_relation_edge_slots_for_render(&[&cgs], &new_relation_slots);
    assert!(
        new_relation_slots.iter().any(|slot| {
            matches!(
                slot,
                ExposureSlotKey::Relation {
                    source,
                    relation,
                } if source.entity.as_str() == "ParentItem" && relation.as_str() == "tags"
            )
        }),
        "Tag extend should unlock ParentItem.tags relation slot"
    );
    let delta = pipeline.render_teaching_exposure_delta_with_edges(
        &cgs,
        &exp,
        &["Tag"],
        &new_relation_slots,
        None,
    );
    assert!(
        delta.contains(".r"),
        "edge delta should include validated relation nav exemplar: {delta}"
    );
    for line in delta.lines() {
        let expr = line.split('\t').next().unwrap_or("").trim();
        if expr.is_empty() || expr.starts_with('#') || !expr.contains(".r") {
            continue;
        }
        assert!(
            domain_line_validate_cached(&mut cache, 0, &cgs, expr, Some(&map_arc)).is_some(),
            "edge-delta relation row must type-check: {expr}"
        );
    }
}

#[test]
fn langitem_prompt_tier1_typed_gloss_dedupe() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let Some(idx) = prompt.find(TSV_TEACHING_TABLE_HEADER) else {
        panic!("expected language-card header");
    };
    let table = &prompt[idx..];
    fn count_slot_rows(body: &str, prefix: &str) -> usize {
        body.lines()
            .filter(|l| {
                let l = l.strip_suffix('\r').unwrap_or(l);
                !l.is_empty()
                    && !l.starts_with('#')
                    && l.split_once('\t').is_some_and(|(cell, _)| cell == prefix)
            })
            .count()
    }
    let title_gloss_rows = count_slot_rows(table, "title");
    assert!(
        title_gloss_rows <= 4,
        "title wire gloss is per typed slot; RA-12 may teach it on more than one entity (got {title_gloss_rows})"
    );
    for line in table.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() != 2 {
            continue;
        }
        if parse_trailing_projection_bracket(cols[0].trim()).is_some() {
            assert!(
                !cols[1].contains("rows:"),
                "expr with bracket must not duplicate rows: in Meaning: {line}"
            );
        }
    }
}

/// Dump actual renderer TSV for `scalar_auth_pipe` ablation (opt-in via `SAP_TSV_OUT`).
#[test]
fn dump_scalar_auth_pipe_tsv_for_ablation() {
    let Ok(out_root) = std::env::var("SAP_TSV_OUT") else {
        return;
    };
    let fixtures = std::env::var("SAP_FIXTURES").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../scripts/appworld/cuga/ablation_offline/scalar_auth_pipe/fixtures"
        )
        .to_string()
    });
    let out = PathBuf::from(out_root);
    std::fs::create_dir_all(&out).expect("create SAP_TSV_OUT");
    let fixtures = PathBuf::from(fixtures);
    for entry in std::fs::read_dir(&fixtures).expect("fixtures dir") {
        let entry = entry.expect("entry");
        if !entry.file_type().expect("ft").is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let dir = entry.path();
        if !dir.join("domain.yaml").is_file() {
            continue;
        }
        let cgs = load_schema_dir(&dir).unwrap_or_else(|e| panic!("load {name}: {e}"));
        let thing = cgs
            .entities
            .keys()
            .find(|k| k.as_str() != "AuthSession")
            .map(|s| s.to_string())
            .expect("ledger entity");
        let seeds = ["AuthSession".to_string(), thing.clone()];
        let seed_refs: Vec<&str> = seeds.iter().map(|s| s.as_str()).collect();
        let cfg = RenderConfig::for_eval_seeds(&seed_refs);
        let tsv = render_prompt_tsv_with_config(&cgs, cfg);
        assert!(
            tsv.contains("access_token"),
            "{name} TSV must teach access_token"
        );
        assert!(
            !tsv.contains("context="),
            "{name} TSV must not teach abolished context="
        );
        let dest = out.join(format!("{name}.tsv"));
        std::fs::write(&dest, &tsv).expect("write tsv");
        eprintln!("wrote {}", dest.display());
    }
}
