//! Prompt render integration tests (matrix/proof fixtures; no full-catalog snapshots).

use std::collections::HashMap;
use std::time::Instant;

use crate::loader::load_schema_dir;
use crate::prompt_pipeline::PromptPipelineConfig;
use crate::schema::{
    CapabilityMapping, CapabilitySchema, FieldSchema, FieldValueKind, NamedValueSchema,
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

/// Count canonical `· projection ·` witness rows (one per entity; query/search omit the same bracket).
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
/// Build paths from [`CARGO_MANIFEST_DIR`] so `apis/…` and `fixtures/…` resolve correctly in
/// `cargo test` and CI the same as local `cd plasm-oss && cargo test`.
fn repo_path(components: &[&str]) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for c in components {
        p.push(c);
    }
    p
}
fn apis_dir(name: &str) -> std::path::PathBuf {
    repo_path(&["..", "..", "apis", name])
}
fn fixture_schema_dir(name: &str) -> std::path::PathBuf {
    repo_path(&["..", "..", "fixtures", "schemas", name])
}

/// Locks Proof `Document`-focused symbolic teaching TSV (`apis/proof`): union ctor teaching rows,
/// value-domain gloss, and `document_edit_v2` witness line. Update with
/// `INSTA_UPDATE=1 cargo test -p plasm-core proof_document_teaching_tsv_snapshot`.
#[test]
fn proof_document_teaching_tsv_snapshot() {
    let dir = apis_dir("proof");
    if !dir.is_dir() {
        eprintln!(
            "skip: apis/proof not at {} (incomplete plasm-oss tree?)",
            dir.display()
        );
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("Document")));
    with_insta_snapshots(|| {
        insta::assert_snapshot!("proof_document_teaching_tsv", tsv);
    });
}

#[test]
fn proof_document_blocks_operation_params_are_not_relation_nav_gloss() {
    let dir = apis_dir("proof");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("Document")));
    for sym in ["blocks", "ref", "markdown"] {
        let needle = format!("{sym}\t=> Block ·");
        assert!(
            !tsv.contains(&needle),
            "capability `blocks` ctor params must not reuse relation-style `→ Block` gloss (symbol {sym}); relation nav stays on `e1($).r6`-style rows.\n{tsv}"
        );
    }
}

#[test]
fn proof_bug_report_capabilities_require_report_parameter() {
    let dir = apis_dir("proof");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    for cap_name in ["bug_report_submit", "document_bug_report_submit"] {
        let cap = cgs
            .get_capability(cap_name)
            .unwrap_or_else(|| panic!("missing capability {cap_name}"));
        assert!(
            cap.has_any_required_param(),
            "{cap_name}: expected at least one required parameter so teaching table cannot teach a no-arg bug report"
        );
        let fields = cap.object_params().unwrap_or_else(|| {
            panic!("{cap_name}: expected merged object input schema from parameters:")
        });
        let report = fields
            .iter()
            .find(|f| f.name == "report")
            .unwrap_or_else(|| panic!("{cap_name}: missing `report` parameter"));
        assert!(report.required, "{cap_name}: `report` must be required");
    }
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

/// Rendered-glyph regression: the language matrix TSV must show `↠` + a `chain:` reconstruction hint
/// on an entity-providing write, `↠ ()` **without** a chain hint on a void write, `↣ [` on a query,
/// and `→` on a get.
#[test]
fn teaching_tsv_return_glyphs_and_terminal_chain_hint_language_matrix() {
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
            .any(|m| m.contains('↠') && m.contains("chain:") && m.contains("(id=…).m#")),
        "expected a provides-write row with terminal glyph + reconstruction hint; meanings:\n{}",
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
}

#[test]
fn proof_document_tsv_topo_p_gloss_before_union_ctor_and_summary_after() {
    let dir = apis_dir("proof");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(Some("Document")));
    fn expr_cell(line: &str) -> Option<&str> {
        line.split_once('\t').map(|(e, _)| e)
    }
    fn meaning_cell(line: &str) -> Option<&str> {
        line.split_once('\t').map(|(_, m)| m)
    }
    let mut ctor_idxs = Vec::new();
    let mut union_summary_idx = None;
    for (i, line) in tsv.lines().enumerate() {
        if line == "plasm_expr\tMeaning" {
            continue;
        }
        let Some(expr) = expr_cell(line) else {
            continue;
        };
        if is_union_ctor_teaching_surface_line(expr) {
            ctor_idxs.push(i);
        }
        if meaning_cell(line).is_some_and(|m| m.trim_start().starts_with("union ·")) {
            union_summary_idx = Some(i);
        }
    }
    let Some(u) = union_summary_idx else {
        panic!("expected union summary row with Meaning starting `union ·`");
    };
    assert!(!ctor_idxs.is_empty(), "expected union ctor exemplar rows");
    let first_ctor = *ctor_idxs.iter().min().expect("ctor rows");
    for &c in &ctor_idxs {
        assert!(c < u, "union ctor at {c} must precede union summary at {u}");
    }
    assert!(
        first_ctor < u,
        "union ctor exemplars must precede union summary; first_ctor={first_ctor} summary={u}"
    );
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
        contract.contains("TSV table semantics:"),
        "contract should teach TSV interpretation before catalog rows"
    );
    assert!(
        contract.contains("Replace teaching placeholders")
            || contract.contains("substitute placeholders"),
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
fn bundled_github_petstore_clickup_full_entities_emit_domain_lines() {
    for p in [
        apis_dir("github"),
        fixtures_schemas_dir("petstore"),
        apis_dir("clickup"),
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
fn google_sheets_compound_get_entity_ref_key_var_emits_valid_domain_line() {
    let dir = apis_dir("google-sheets");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let lines = domain_example_lines(&cgs, "ValueRange", None, None);
    let expected = "ValueRange(spreadsheetId=$, range=$)";
    assert!(
        lines.iter().any(|l| l.starts_with(expected)),
        "missing compound dotted-call-safe get witness for entity_ref key var: expected prefix `{expected}` in {:?}",
        lines
    );
    assert!(
        validate_teaching_line_wire(&cgs, expected).is_some(),
        "expected synthesized compound get witness to parse+typecheck: `{expected}`"
    );
}

/// Regression: Issue teaching table teaches **one** canonical `· projection ·` witness row.
/// Scoped query/search exemplars omit the same trailing `[p#,…]` / `rows:` contract.
#[test]
fn github_issue_domain_emits_single_full_projection_exemplar() {
    let dir = apis_dir("github");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exposure = crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
    let surface = Some(&exposure.surface);
    let Some(ent) = cgs.get_entity("Issue") else {
        panic!("missing Issue entity");
    };
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
    let prefixes = cgs.projection_prompt_field_prefixes("Issue", ent);
    assert_eq!(
        prefixes.len(),
        1,
        "expected one full projection exemplar vector; got {}",
        prefixes.len()
    );
    assert!(
        prefixes[0].len() >= 10,
        "Issue primary get should expose many response fields for teaching; got {}",
        prefixes[0].len()
    );
    let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
    let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "Issue").expect(
        "Issue should carry a full projection bracket (heading or primary get) after alias pass",
    );
    assert!(
        br.starts_with('[') && br.ends_with(']') && br.len() > 2,
        "unexpected projection bracket: {br}"
    );
    let lines = domain_example_lines(&cgs, "Issue", map.as_deref(), surface);
    assert_eq!(
        count_projection_teaching_witness_rows(&cgs, "Issue", map.as_deref(), surface),
        1,
        "expect exactly one `· projection ·` witness row per entity"
    );
    let block = {
        let mut line_valid_cache = HashMap::new();
        let mut gloss_emit_none = None;
        let seed = prompt_line_valid_cache_seed_cgs(&cgs);
        collect_entity_teaching_block(
            &cgs,
            "Issue",
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
    let witness = block
        .teaching_rows
        .iter()
        .find(|r| r.teaching_expr.is_projection_teaching)
        .expect("Issue projection witness");
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
    assert_eq!(
        same_set_brackets, 1,
        "canonical projection field set taught once (got {same_set_brackets}): {lines:?}"
    );
    for row in &block.teaching_rows {
        if row.teaching_expr.is_projection_teaching {
            continue;
        }
        let expr = row.teaching_expr.expression.as_str();
        let gloss = row.teaching_expr.result_type.as_str();
        if !(expr.contains('{') || expr.contains('~')) {
            continue;
        }
        match parse_trailing_projection_bracket(expr.trim()) {
            None => {
                assert!(
                    !gloss.contains("rows:"),
                    "omitted bracket must omit rows: : {gloss}"
                );
                if expr.contains('{') {
                    assert!(
                        gloss.contains("inputs:"),
                        "query filter lines keep inputs: gloss: {gloss}"
                    );
                }
            }
            Some(b) => {
                assert!(
                    !projection_field_sets_equal(&projection_bracket_syms(&b), &canon_syms),
                    "set-equal bracket must be suppressed: {expr}"
                );
                assert!(
                    !gloss.contains("rows:"),
                    "divergent provides keep bracket on expr without rows: in Meaning: {gloss}"
                );
            }
        }
    }
    let out = render_prompt_with_config(&cgs, cfg);
    assert!(
        !out.contains("Federated sessions"),
        "single-catalog github slice teaching TSV should not embed grammar pitfalls"
    );
    assert!(
        out.contains(br.as_str()),
        "full prompt should include the full projection list `{br}` (heading or primary get)"
    );
    let bracket_hits = out.matches(br.as_str()).count();
    assert_eq!(
        bracket_hits, 1,
        "canonical projection list must appear exactly once in the full TSV (got {bracket_hits})"
    );
    assert!(
        out.len() > 8_000,
        "full apis/github teaching table+legend should be substantial (got {} bytes)",
        out.len()
    );
    // Baseline after teaching projection once (no per-query bracket/`rows:` duplication).
    const GITHUB_FULL_PROMPT_BASELINE_V0173: usize = 32_000;
    const GITHUB_FULL_PROMPT_BASELINE_V0179: usize = 32_000;
    assert!(
        out.len() <= GITHUB_FULL_PROMPT_BASELINE_V0179,
        "github full prompt regressed above v0.1.79 baseline (got {} bytes, baseline {})",
        out.len(),
        GITHUB_FULL_PROMPT_BASELINE_V0179
    );
    assert!(
        out.len() < GITHUB_FULL_PROMPT_BASELINE_V0173,
        "github full prompt should remain smaller than v0.1.73 baseline (got {} bytes, baseline {})",
        out.len(),
        GITHUB_FULL_PROMPT_BASELINE_V0173
    );
}

/// Linear uses zero-arity method-style Get exemplars (`e2.m8()`); heading projection must still
/// teach scalar fields from `issue_get.provides` (see [`CGS::domain_projection_heading_fields`]).
#[test]
fn linear_issue_heading_projection_despite_method_style_get() {
    let dir = apis_dir("linear");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exposure = crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
    let surface = Some(&exposure.surface);
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true);
    let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
    let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "Issue")
        .expect("Linear Issue should carry a full projection bracket (heading or primary get)");
    assert!(
        br.starts_with('[') && br.ends_with(']') && br.len() > 2,
        "unexpected projection bracket: {br}"
    );
    let lines = domain_example_lines(&cgs, "Issue", map.as_deref(), surface);
    assert_eq!(
        count_projection_teaching_witness_rows(&cgs, "Issue", map.as_deref(), surface),
        1,
        "expect exactly one `· projection ·` witness row per entity"
    );
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let block = collect_entity_teaching_block(
        &cgs,
        "Issue",
        map.as_ref(),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface,
        None,
    );
    let witness = block
        .teaching_rows
        .iter()
        .find(|r| r.teaching_expr.is_projection_teaching)
        .expect("Linear Issue projection witness");
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
    assert_eq!(
        same_set_brackets, 1,
        "canonical projection field set taught once on Linear Issue (got {same_set_brackets}): {lines:?}"
    );
    let out = render_prompt_with_config(&cgs, cfg);
    assert!(
        out.contains(br.as_str()),
        "full prompt should include the full projection list `{br}` (heading or primary get)"
    );
    assert_eq!(
        out.matches(br.as_str()).count(),
        1,
        "canonical projection list must appear exactly once for Linear Issue"
    );
}

/// Intent-scoped Issue surface: query/search share the witness field set → bare producers, no `rows:`.
#[test]
fn github_issue_intent_surface_omits_set_equal_projection_on_query_search() {
    use crate::discovery::MutatorAdmit;

    let dir = apis_dir("github");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let endpoints = vec![ExposureEntityKey {
        entry_id: "github".into(),
        entity: EntityName::from("Issue"),
    }];
    let delta = crate::discovery::derive_intent_exposure_surface_batch(
        &cgs,
        "github",
        "list issues and create or update issue labels",
        &endpoints,
        &["Issue".to_string()],
        None,
        crate::discovery::ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    let exp = TeachingExposureSession::new_with_intent_delta(&cgs, "github", &["Issue"], delta);
    let surface = Some(&exp.surface);
    let map = exp.symbol_map_arc();
    let mut line_valid_cache = HashMap::new();
    let mut gloss_emit_none = None;
    let seed = prompt_line_valid_cache_seed_cgs(&cgs);
    let block = collect_entity_teaching_block(
        &cgs,
        "Issue",
        Some(&map),
        None,
        false,
        &mut line_valid_cache,
        seed,
        &mut gloss_emit_none,
        surface,
        Some("github"),
    );
    let witness = block
        .teaching_rows
        .iter()
        .find(|r| r.teaching_expr.is_projection_teaching)
        .expect("Issue projection witness");
    let canon = parse_trailing_projection_bracket(witness.teaching_expr.expression.trim())
        .expect("witness bracket");
    let mut saw_list_producer = false;
    for row in &block.teaching_rows {
        if row.teaching_expr.is_projection_teaching {
            continue;
        }
        let expr = row.teaching_expr.expression.as_str();
        if !(expr.contains('{') || expr.contains('~')) {
            continue;
        }
        saw_list_producer = true;
        assert!(
            parse_trailing_projection_bracket(expr.trim()).is_none(),
            "intent-scoped query/search must omit set-equal bracket: {expr}"
        );
        let gloss = row.teaching_expr.result_type.as_str();
        assert!(
            !gloss.contains("rows:"),
            "intent-scoped query/search must omit rows: : {gloss}"
        );
    }
    assert!(saw_list_producer, "expected query/search teaching rows");
    let lines: Vec<_> = block
        .teaching_rows
        .iter()
        .map(|r| r.teaching_expr.expression.as_str())
        .collect();
    let same_set = lines
        .iter()
        .filter(|l| {
            parse_trailing_projection_bracket(l).is_some_and(|b| {
                projection_field_sets_equal(
                    &projection_bracket_syms(&b),
                    &projection_bracket_syms(&canon),
                )
            })
        })
        .count();
    assert_eq!(same_set, 1, "canonical set once: {lines:?}");
}

#[test]
fn heading_projection_symbols_are_declared_before_heading_use() {
    let dir = apis_dir("github");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exposure = crate::symbol_tuning::teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
    let cfg = RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact);
    let br = domain_projection_bracket_from_final_bundle(&cgs, &exposure, cfg, "Issue")
        .expect("Issue should carry a projection list");
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
        "Issue projection should include at least one wire field symbol"
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
        "teaching TSV is table-only; grammar lives in PLASM_TOOL_DESCRIPTION"
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
}

#[test]
fn expand_wave_emits_parent_relation_edge_for_pokeapi_berry_firmness() {
    use crate::discovery::{
        derive_intent_exposure_surface_batch, ExposureSurfaceOptions, MutatorAdmit,
    };
    use crate::symbol_tuning::ExposureEntityKey;

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let pipeline = PromptPipelineConfig::default();
    let intent = "cheri berry firmness";
    let relation_keys = vec![ExposureEntityKey {
        entry_id: "pokeapi".to_string(),
        entity: crate::EntityName::from("Berry"),
    }];
    let delta1 = derive_intent_exposure_surface_batch(
        &cgs,
        "pokeapi",
        intent,
        &relation_keys,
        &["Berry".to_string()],
        None,
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    let mut exp =
        TeachingExposureSession::new_with_intent_delta(&cgs, "pokeapi", &["Berry"], delta1);
    let slots_before = exp.surface.slots.clone();
    let cgs_arc = std::sync::Arc::new(cgs.clone());
    let relation_keys_wave2 =
        exp.relation_endpoint_keys_for_wave("pokeapi", &["BerryFirmness".to_string()]);
    let delta2 = derive_intent_exposure_surface_batch(
        &cgs,
        "pokeapi",
        intent,
        &relation_keys_wave2,
        &["BerryFirmness".to_string()],
        None,
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    exp.expose_surface(&[&cgs], cgs_arc, "pokeapi", &["BerryFirmness"], delta2);
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
                } if source.entity.as_str() == "Berry" && relation.as_str() == "firmness"
            )
        }),
        "expand should add Berry.firmness relation slot"
    );
    let delta = pipeline.render_teaching_exposure_delta_with_edges(
        &cgs,
        &exp,
        &["BerryFirmness"],
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
        "warmup must emit teaching TSV header"
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
                && line.contains(&format!("{p_id}=$"))
                && line.contains(&format!("{p_ct}=$"))
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
    let dir = apis_dir("dnd5e");
    if !dir.is_dir() {
        return;
    }
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
            "relation/method recv must not use invalid unary identity get `e#($).…`: {head}"
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
    let witness_row = block.teaching_rows.iter().find(|r| {
        r.teaching_expr.is_projection_teaching
            && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
    });
    let Some(row) = witness_row else {
        panic!("expected a projection witness row for Zone teaching table; lines={lines:?}");
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
        !legend.contains("projection [") && !legend.contains("· projection ["),
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
        prompt.contains("ref:Zone") && prompt.contains("str · Zone identifier"),
        "teaching TSV must include entity-ref value-domain gloss with canonical entity (not e#):\n{prompt}"
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
fn prompt_matrix_zone_entity_ref_value_domain_gloss_includes_id_primitive() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let v = map
        .value_sym_for_wire("", "Ruleset", "zone_id")
        .expect("Ruleset.zone_id should map to a value-domain symbol");
    let g = map
        .value_domain_gloss_for_v_sym(&v)
        .expect("value-domain gloss");
    assert!(
        g.starts_with("ref:Zone · str ·"),
        "expected ref:Zone · str · … value-domain gloss, got {g:?}"
    );
}

#[test]
fn exposure_surface_omits_entity_ref_nav_when_target_entity_not_exposed() {
    let dir = fixtures_schemas_dir("plasm_prompt_matrix");
    let cgs = load_schema_dir(&dir).unwrap();
    let entry = cgs.entry_id.clone().unwrap_or_default();
    let delta = crate::discovery::derive_intent_exposure_surface_batch(
        &cgs,
        entry.as_str(),
        "rules traffic handling Cloudflare zone firewall WAF",
        &crate::relation_endpoint_keys(entry.as_str(), &["Ruleset".to_string()]),
        &["Ruleset".to_string()],
        None,
        crate::discovery::ExposureSurfaceOptions::default(),
    );
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
        let ex = r.teaching_expr.expression.as_str();
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
            let ex = r.teaching_expr.expression.as_str();
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

    let delta = crate::discovery::derive_intent_exposure_surface_batch(
        &cgs,
        entry.as_str(),
        "rules traffic handling Cloudflare zone firewall WAF",
        &crate::relation_endpoint_keys(entry.as_str(), &["Ruleset".to_string()]),
        &["Ruleset".to_string()],
        None,
        crate::discovery::ExposureSurfaceOptions::default(),
    );
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
    let witness_row = block.teaching_rows.iter().find(|r| {
        r.teaching_expr.is_projection_teaching
            && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
    });
    let Some(row) = witness_row else {
        panic!("expected a projection witness row for Zone teaching table");
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
    let proj_i = rows
        .iter()
        .position(|r| r.is_projection_teaching)
        .expect("Ruleset projection witness");
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (!rows[i].is_projection_teaching, i));
    assert_eq!(
        order[0], proj_i,
        "TSV encoder emits projection witness rows before other teaching rows"
    );
    let compound_i = rows.iter().position(|r| {
        r.expression.contains('(')
            && r.expression.contains(',')
            && !r.expression.contains('{')
            && !r.is_projection_teaching
    });
    let query_i = rows
        .iter()
        .position(|r| r.expression.contains('{') && !r.is_projection_teaching);
    if let Some(ci) = compound_i {
        assert!(
            proj_i < ci,
            "projection witness should precede compound get in synthesis order"
        );
    }
    if let Some(qi) = query_i {
        assert!(
            proj_i < qi,
            "projection witness should precede query brace line in synthesis order"
        );
    }
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
    let witness = block.teaching_rows.iter().find(|r| {
        r.teaching_expr.is_projection_teaching
            && parse_trailing_projection_bracket(r.teaching_expr.expression.trim()).is_some()
    });
    let Some(row) = witness else {
        panic!(
            "expected query-backed projection witness for WafPackage; rows={:?}",
            block
                .teaching_rows
                .iter()
                .map(|r| r.teaching_expr.expression.as_str())
                .collect::<Vec<_>>()
        );
    };
    assert!(
        row.teaching_expr.expression.contains('{'),
        "witness base should be query-shaped brace form: {}",
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
            "abstract WafPackage lines must not appear in default teaching TSV: {expr:?}"
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
            .is_some_and(|(_, m)| m.contains("· projection")),
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
        panic!("expected teaching TSV header");
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
fn plasm_tool_description_includes_row_compute_worked_example() {
    let frontmatter = super::PLASM_TOOL_DESCRIPTION;
    assert!(frontmatter.contains(".filter{"));
    assert!(frontmatter.contains(".limit(10)"));
    assert!(frontmatter.contains("Core surface:"));
    assert!(
        frontmatter.contains("Worked transform") || frontmatter.contains("Worked shape"),
        "expected worked transform/shape example"
    );
    assert!(
        frontmatter.contains("Replace teaching placeholders") || frontmatter.contains("substitute")
    );
    assert!(frontmatter.contains("e#~$"));
    assert!(
        !frontmatter.contains("Session and symbol discipline"),
        "session discipline belongs in tool workflow descriptions, not duplicated in grammar"
    );
    assert!(
        !frontmatter.contains(" ::="),
        "full pseudo-EBNF block retired; canonical syntax is Core surface + worked examples"
    );
    assert!(
        frontmatter.contains("label = e#"),
        "pitfalls must teach bind-before-filter preference"
    );
    assert!(
        frontmatter.contains("PLASM_RPT_TAG"),
        "row-to-text worked example must show explicit bracket + Minijinja wire body"
    );
    assert!(
        frontmatter.contains("r.name"),
        "row-to-text worked example must use wire field names in template body"
    );
    assert!(
        frontmatter.contains("wire names in bracket"),
        "row-to-text contract must note wire names in projection brackets"
    );
    assert!(
        frontmatter.contains("source binding name also works"),
        "row-to-text contract must note source alias for collection iteration"
    );
    assert!(
        frontmatter.contains("or \"—\""),
        "row-to-text worked example must show nullable field coalescing"
    );
    assert!(
        !frontmatter.contains("e2(p10="),
        "canonical frontmatter must not hardcode catalog-specific symbol indices"
    );
    assert!(
        frontmatter.contains("bind-ordered")
            && frontmatter.contains("e_issue.m_create")
            && frontmatter.contains("e_comment.m_create"),
        "write-batch guidance must prefer one multi-write program with create→write example"
    );
    assert!(
        !frontmatter.contains("co-committed gates"),
        "retired undefined co-committed-gates phrasing"
    );
    assert!(
        !frontmatter.contains("re-read `e#(id=…)` to continue"),
        "↠ must not be framed as an in-program re-read ban"
    );
}

#[test]
fn mcp_static_tool_descriptions_byte_budget() {
    const MAX_WORKFLOW_BYTES: usize = 1200;

    let workflow = super::MCP_INITIALIZE_WORKFLOW;
    let plasm_tool = super::PLASM_TOOL_DESCRIPTION;
    let discover = super::DISCOVER_TOOL_DESCRIPTION;
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
        discover.len() <= 550,
        "discover tool description too long: {} bytes",
        discover.len()
    );
    assert!(
        context.len() <= 1800,
        "plasm_context tool description too long: {} bytes",
        context.len()
    );
    assert!(plasm_tool.contains(super::MCP_TOOL_SYNTAX_CONTRACT_MARKER));
    assert!(plasm_tool.contains("literal no-op"));

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
    let prefix = &full[..full.len().min(prefix_n)];
    assert!(
        prefix.contains("Batch independent reads"),
        "batching mandate must be in first {prefix_n} bytes (host truncation)"
    );
    assert!(
        prefix.contains("labels, branches") || prefix.contains("a, b"),
        "multi-root return example must be in first {prefix_n} bytes"
    );
    assert!(
        prefix.contains("run_ref") && prefix.contains("approval"),
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
    let wide = &full[..full.len().min(wide_n)];
    assert!(
        wide.contains("Composition rules:"),
        "composition rules must be in first {wide_n} bytes (host truncation)"
    );
    assert!(
        wide.contains("Worked transform") || wide.contains("Worked shape"),
        "a worked composition example must be in first {wide_n} bytes"
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
            .get("symbol_rules")
            .copied()
            .unwrap_or(0)
            > 500
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
        "single-entity slice should not add contract comments to teaching TSV"
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

    for catalog in ["linear", "github"] {
        let dir = apis_dir(catalog);
        if !dir.is_dir() {
            continue;
        }
        let cgs = load_schema_dir(&dir).unwrap();
        let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
        let st = super::grammar_frontmatter_stats_from_prompt(&prompt);
        assert_eq!(st.contract_comment_bytes, 0);
        assert!(st.table_bytes > 0);
        eprintln!(
            "grammar_frontmatter_stats apis/{catalog}: {}",
            st.summary_line_body()
        );
        return;
    }
}

/// Search teaching rows must not invite copy-paste of `e#~$` (grammar teaches `e#~"text"`).
#[test]
fn domain_search_teaching_rows_use_quoted_text_not_dollar() {
    let dir = apis_dir("linear");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    for line in prompt.lines() {
        if line.contains('~') && line.starts_with('e') {
            assert!(
                !line.contains("~$"),
                "search teaching row must not contain ~$: {line}"
            );
        }
    }
}

/// Projection witness teaches `[p#,…]` once; set-equal query omits `rows:`; divergent keeps it.
#[test]
fn row_producer_teaching_includes_inputs_and_rows_contract() {
    let dir = fixtures_schemas_dir("plasm_language_matrix");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    assert!(
        prompt.lines().any(|l| l.contains("· projection")),
        "teaching rows should include a projection witness:\n{prompt}"
    );
    assert!(
        prompt.lines().any(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            cols.len() == 2
                && cols[0].contains('{')
                && !cols[0].contains(".r")
                && parse_trailing_projection_bracket(cols[0].trim()).is_none()
                && !cols[1].contains("rows:")
                && !cols[1].contains("· projection")
        }),
        "set-equal query omits bracket/rows: in Meaning:\n{prompt}"
    );
    assert!(
        prompt.lines().any(|l| {
            l.contains("~\"text\"")
                && parse_trailing_projection_bracket(l.split('\t').next().unwrap_or("").trim())
                    .is_some()
                && !l.contains("rows:")
        }),
        "divergent search provides keep bracket on expr without rows: in Meaning:\n{prompt}"
    );
}

#[test]
fn static_grammar_includes_symbols_only_rule() {
    assert!(
        super::PLASM_TOOL_DESCRIPTION.contains("**Symbolic only:**")
            && super::PLASM_TOOL_DESCRIPTION.contains("wire names"),
        "canonical static grammar must teach TSV-only program tokens and wire names"
    );
}

#[test]
fn teaching_prompt_bundle_tags_relation_nav_materialization() {
    let dir = apis_dir("pokeapi");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let bundle = render_teaching_prompt_bundle(&cgs, RenderConfig::for_eval_seeds(&["Type"]));
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
fn pokeapi_bundle_is_reasonable_size() {
    let dir = apis_dir("pokeapi");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let out = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
    assert!(out.len() < 50_000, "bundle should stay bounded");
    assert!(!out.contains("EXAMPLES:") && out.contains("plasm_expr\tMeaning"));
}

/// `Team(id).spaces` uses `query_scoped` materialization — it parses as [`Expr::Chain`]; teaching table shows
/// anchored relation nav plus scoped `Space{…}` under Space.
#[test]
fn clickup_domain_includes_materialized_team_spaces_nav() {
    let dir = apis_dir("clickup");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let sym = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    let raw = render_prompt_with_config(&cgs, RenderConfig::for_eval_canonical(None));
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let team_sym = map.entity_sym_for("", "Team");
    let spaces_rel = map.ident_sym_relation_for("", "Team", "spaces");
    let team_ent = cgs.get_entity("Team").expect("Team");
    let p_team_identity = map.ident_sym_entity_field_for("", "Team", team_ent.id_field.as_str());
    assert!(
        raw.contains(".spaces")
            && (raw.contains("Team($)")
                || raw.contains(&format!("Team({p_team_identity})"))
                || raw.contains("Team{"))
            && raw.contains("Team"),
        "expected Team→spaces relation line (chain materialization; receiver may be `Team($)`, `Team({p_team_identity})`, or query-scoped `Team{{…}}`)"
    );
    assert!(
        sym.contains(&format!(".{spaces_rel}"))
            || sym.contains(&format!("{team_sym}($).{spaces_rel}"))
            || sym.contains(&format!("{team_sym}({p_team_identity}).{spaces_rel}"))
            || sym.contains(&format!("{team_sym}{{")),
        "expected symbol-tuned Team→spaces relation (`.{spaces_rel}` on a `{team_sym}` receiver)"
    );
    assert!(
        raw.contains("Space{") && raw.contains("team_id"),
        "Space scoped query with team_id should remain in teaching table (canonical)"
    );
    assert!(
        sym.contains("Space{")
            || (sym.contains("{") && sym.contains(&format!("={team_sym}(")))
            || raw.contains("Space{"),
        "Space scoped query should remain in teaching table"
    );
}

/// `team_query` is query-shaped (`e1` in teaching table); capability prose is intentionally omitted from
/// `Meaning` (types teach shape); see `omit_capability_prose` in teaching synthesis.
#[test]
fn clickup_domain_gloss_and_symbol_map_queries() {
    let dir = apis_dir("clickup");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
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
    let team_sym = map.entity_sym_for("", "Team");
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
        "TSV team_query should teach collection result gloss for Team (`[{team_sym}]`) without capability prose"
    );
    assert!(
        !domain_block.contains(" -> "),
        "relation / field nav lines must use `;;  => e#` (or `[e#]`), not `expr -> e#` before ;;"
    );
    let task_sym = map.entity_sym_for("", "Task");
    let p_team_id = map.ident_sym_cap_param_for("", "Task", "task_query", "team_id");
    let team_ent = cgs.get_entity("Team").expect("Team");
    let p_team_identity = map.ident_sym_entity_field_for("", "Team", team_ent.id_field.as_str());
    assert!(
        domain_block.contains(&format!(
            "{}{{{}={}({})",
            task_sym, p_team_id, team_sym, p_team_identity
        )) || domain_block.contains(&format!(
            "{}{{{}={}($)",
            task_sym, p_team_id, team_sym
        )),
        "workspace-scoped task query should teach scope with unary entity-ref fill-in (p#=e#(id_slot) or e#($)), not bare team id literals"
    );
    assert!(
        !domain_block.contains("2000-01-01") && !domain_block.contains("p10>=\""),
        "query teaching table brace form must not teach concrete ISO datetimes or `>=` date literals"
    );
    assert!(
        !domain_block.contains("List all accessible workspaces"),
        "query capability long-form description must not surface in TSV Meaning"
    );
}

/// User has only pathless singleton `user_get_me` — teaching table must show `e#.m#()` (get-me) and not mislead with `e#(42)`.
#[test]
fn clickup_user_singleton_get_me_line_in_domain() {
    let dir = apis_dir("clickup");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let sym = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
    let user_sym = map.entity_sym_for("", "User");
    assert!(
        sym.lines().any(|l| {
            l.split_once('\t').is_some_and(|(expr, _)| {
                expr.contains(&format!("{user_sym}.m")) && expr.contains("()")
            })
        }),
        "User TSV should teach singleton get-me as e#.m#(), not id-based e#(42)"
    );
}

/// Book —(shelf)—> Shelf; two query caps; one navigation edge from Book.
fn prompt_stats_fixture_cgs() -> CGS {
    let mut cgs = CGS::new();
    cgs.values.insert(
        "fixture_str".into(),
        NamedValueSchema {
            description: String::new(),
            field_type: FieldType::String,
            value_format: None,
            allowed_values: None,
            string_semantics: None,
            array_items: None,
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
        discovery: None,
    })
    .unwrap();
    let tmpl = serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "x"}]});
    for (name, domain) in [("book_query", "Book"), ("shelf_query", "Shelf")] {
        cgs.add_capability(CapabilitySchema {
            name: name.into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            effect: None,
            domain: domain.into(),
            identity_key: None,
            mapping: CapabilityMapping {
                template: tmpl.clone().into(),
            },
            input_schema: None,
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
    // Book: one query line; Shelf: one. One `shelf` relation without materialize → no nav line in teaching table.
    assert_eq!(domain_tools, 2);

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
    }
}

/// Two entities, same wire field `id` (maps to one `p#`), optional distinct descriptions — for
/// [`emit_field_def_lines_before_example`] identity tests.
fn p_slot_redefinition_fixture_cgs(id_desc_a: &str, id_desc_b: &str) -> CGS {
    let mut cgs = CGS::new();
    cgs.values.insert(
        "fixture_str".into(),
        NamedValueSchema {
            description: String::new(),
            field_type: FieldType::String,
            value_format: None,
            allowed_values: None,
            string_semantics: None,
            array_items: None,
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
            discovery: None,
        })
        .unwrap();
        let cap_name: String = format!("{}_get", name.to_lowercase());
        cgs.add_capability(CapabilitySchema {
            name: cap_name.into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            effect: None,
            domain: name.into(),
            identity_key: None,
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": name.to_lowercase()},
                        {"type": "var", "name": "id"},
                    ],
                })
                .into(),
            },
            input_schema: None,
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
    for expr in &exprs {
        if let Some(arc) = map.as_ref() {
            let layers = [crate::CgsLayer::unset(&cgs)];
            let mut r = crate::expr_parser::parse_with_cgs_layers(expr, &layers, arc.clone())
                .unwrap_or_else(|e| {
                    panic!(
                        "teaching table expr should parse (opaque) for {}: {expr:?}\n{e}",
                        dir.display()
                    );
                });
            if let Err(e) = crate::normalize_expr_query_capabilities(&mut r.expr, &cgs) {
                panic!(
                    "teaching table expr should resolve query capability for {}: {expr:?}\n{e}",
                    dir.display()
                );
            }
            if let Err(e) = crate::type_check_expr(&r.expr, &cgs) {
                panic!(
                    "teaching table expr should type-check for {}: {expr:?}\n{e}",
                    dir.display()
                );
            }
        } else {
            let work = expr.clone();
            let mut r = crate::expr_parser::parse(&work, &cgs).unwrap_or_else(|e| {
                panic!(
                    "teaching table expr should parse for {}: {expr:?}\n{e}",
                    dir.display()
                );
            });
            if let Err(e) = crate::normalize_expr_query_capabilities(&mut r.expr, &cgs) {
                panic!(
                    "teaching table expr should resolve query capability for {}: {expr:?}\n{e}",
                    dir.display()
                );
            }
            if let Err(e) = crate::type_check_expr(&r.expr, &cgs) {
                panic!(
                    "teaching table expr should type-check for {}: {expr:?}\n{e}",
                    dir.display()
                );
            }
        }
    }
}

#[test]
fn petstore_rendered_examples_parse() {
    assert_prompt_examples_parse(&fixtures_schemas_dir("petstore"));
}

#[test]
fn clickup_rendered_examples_parse() {
    assert_prompt_examples_parse(&apis_dir("clickup"));
}

#[test]
fn github_rendered_examples_parse() {
    assert_prompt_examples_parse(&apis_dir("github"));
}

/// Writes `apis/<name>/eval/prompt_symbol_tuning.txt` for inspection (eval/REPL bundle).
/// Does not run in normal `cargo test`; use:
/// `cargo test -p plasm-core write_clickup_prompt_fixture -- --ignored --exact --nocapture`
#[test]
#[ignore = "manual: dumps prompt bundle to apis/.../eval/prompt_symbol_tuning.txt"]
fn write_clickup_prompt_fixture() {
    let dir = apis_dir("clickup");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let s = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let out = dir.join("eval/prompt_symbol_tuning.txt");
    std::fs::write(&out, &s).unwrap();
    eprintln!("wrote {} bytes to {}", s.len(), out.display());
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
    let prompt = render_prompt_with_config(
        &cgs,
        RenderConfig::for_eval(None).with_render_mode(PromptRenderMode::Compact),
    );
    with_insta_snapshots(|| {
        insta::assert_snapshot!("overshow_tools_compact_prompt", prompt);
    });
}

/// Locks teaching TSV render for the same fixture (review diffs with compact snapshot above).
#[test]
fn overshow_tools_prompt_tsv_snapshot() {
    let dir = fixtures_schemas_dir("overshow_tools");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let tsv = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    with_insta_snapshots(|| {
        insta::assert_snapshot!("overshow_tools_prompt_tsv", tsv);
    });
}

/// Federated open: colliding wire entity names get distinct `e#` in teaching TSV rows (B1).
#[test]
fn federated_duplicate_entity_wire_names_use_distinct_e_in_teaching_tsv() {
    use std::sync::Arc;

    let root = fixtures_schemas_dir("plasm_language_matrix");
    let cgs = load_schema_dir(&root).expect("plasm_language_matrix");
    let layers = [&cgs, &cgs];
    let mut exp = TeachingExposureSession::new(&cgs, "github", &["LangItem"]);
    exp.expose_entities(&layers, Arc::new(cgs.clone()), "linear", &["LangItem"]);
    let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
    by_entry.insert("github".into(), &cgs);
    by_entry.insert("linear".into(), &cgs);
    let bundle = render_teaching_prompt_bundle_for_exposure_federated(
        &by_entry,
        RenderConfig::for_eval(None),
        &exp,
        None,
    );
    assert!(
        bundle.teaching_blocks.len() >= 2,
        "expected github + linear LangItem blocks"
    );
    let row_text = |block: &EntityTeachingBlock| {
        block
            .teaching_rows
            .iter()
            .map(|r| r.teaching_expr.expression.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let github_text = row_text(&bundle.teaching_blocks[0]);
    let linear_text = row_text(&bundle.teaching_blocks[1]);
    assert!(
        github_text.contains("e1"),
        "github LangItem teaching rows should use e1: {github_text}"
    );
    assert!(
        !github_text.contains("e2"),
        "github block must not bleed linear e2: {github_text}"
    );
    assert!(
        linear_text.contains("e2"),
        "linear LangItem teaching rows should use e2: {linear_text}"
    );
}

/// Production catalogs: `github/Issue` + `linear/Issue` federated TSV uses e1 vs e2.
#[test]
fn federated_github_linear_issue_distinct_e_symbols_when_apis_present() {
    use std::sync::Arc;

    let github_dir = apis_dir("github");
    let linear_dir = apis_dir("linear");
    if !github_dir.is_dir() || !linear_dir.is_dir() {
        return;
    }
    let mut cgs_github = load_schema_dir(&github_dir).expect("github");
    cgs_github.entry_id = Some("github".into());
    let mut cgs_linear = load_schema_dir(&linear_dir).expect("linear");
    cgs_linear.entry_id = Some("linear".into());
    let layers = [&cgs_github, &cgs_linear];
    let mut exp = TeachingExposureSession::new(&cgs_github, "github", &["Issue"]);
    exp.expose_entities(&layers, Arc::new(cgs_linear.clone()), "linear", &["Issue"]);
    let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
    by_entry.insert("github".into(), &cgs_github);
    by_entry.insert("linear".into(), &cgs_linear);
    let bundle = render_teaching_prompt_bundle_for_exposure_federated(
        &by_entry,
        RenderConfig::for_eval(None),
        &exp,
        None,
    );
    assert!(bundle.teaching_blocks.len() >= 2);
    let github_has_e1 = bundle.teaching_blocks[0]
        .teaching_rows
        .iter()
        .any(|r| r.teaching_expr.expression.contains("e1"));
    let linear_has_e2 = bundle.teaching_blocks[1]
        .teaching_rows
        .iter()
        .any(|r| r.teaching_expr.expression.contains("e2"));
    let linear_not_only_e1 = !bundle.teaching_blocks[1].teaching_rows.iter().all(|r| {
        r.teaching_expr.expression.contains("e1") && !r.teaching_expr.expression.contains("e2")
    });
    assert!(github_has_e1, "github Issue block should teach e1");
    assert!(linear_has_e2, "linear Issue block should teach e2");
    assert!(linear_not_only_e1, "linear block must not reuse github e1");
}

/// Matrix fixture: `from_parent_get` many-relation without target Get must type-check and
/// produce validated relation edge-delta teaching when the child entity is seeded on extend.
#[test]
fn from_parent_get_nav_matrix_relation_fanout_type_checks_and_edge_delta_validates() {
    use crate::type_checker::type_check_chain;
    use crate::{ChainExpr, Expr, GetExpr};

    let dir = fixture_schema_dir("from_parent_get_nav");
    let mut cgs = load_schema_dir(&dir).expect("from_parent_get_nav fixture");
    cgs.entry_id = Some("from_parent_get_nav".into());
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

/// Linear `Issue.labels` (`from_parent_get`, target Label has no Get) must type-check and
/// produce validated relation edge-delta teaching when Label is seeded on extend.
#[test]
fn linear_issue_labels_relation_fanout_type_checks_and_edge_delta_validates() {
    use crate::type_checker::type_check_chain;
    use crate::{ChainExpr, Expr, GetExpr};

    let dir = apis_dir("linear");
    if !dir.exists() {
        return;
    }
    let mut cgs = load_schema_dir(&dir).expect("linear");
    cgs.entry_id = Some("linear".into());
    let chain = ChainExpr::auto_get(Expr::Get(GetExpr::new("Issue", "ENG-42")), "labels");
    type_check_chain(&chain, &cgs).expect("Issue.labels from_parent_get chain");

    let mut cache = HashMap::new();
    let pipeline = PromptPipelineConfig::default();
    let mut exp = TeachingExposureSession::new(&cgs, "linear", &["Issue"]);
    let slots_before = exp.surface.slots.clone();
    let cgs_arc = std::sync::Arc::new(cgs.clone());
    exp.expose_entities(&[&cgs], cgs_arc, "linear", &["Label"]);
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
                } if source.entity.as_str() == "Issue" && relation.as_str() == "labels"
            )
        }),
        "Label extend should unlock Issue.labels relation slot"
    );
    let delta = pipeline.render_teaching_exposure_delta_with_edges(
        &cgs,
        &exp,
        &["Label"],
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
fn github_prompt_tier1_typed_gloss_dedupe() {
    let dir = apis_dir("github");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let prompt = render_prompt_tsv_with_config(&cgs, RenderConfig::for_eval(None));
    let Some(idx) = prompt.find(TSV_TEACHING_TABLE_HEADER) else {
        panic!("expected teaching TSV header");
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
    assert!(
        count_slot_rows(table, "repository") <= 1,
        "repository gloss should dedupe globally (typed RegistryWireSlot identity)"
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
