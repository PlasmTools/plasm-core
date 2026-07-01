//! Teaching-table TSV wire encoding (`plasm_expr` \t `Meaning`).

use std::collections::{HashMap, HashSet};

use crate::symbol_tuning::SymbolMap;

use super::contract::enforce_teaching_tsv_teaching_invariant;
use super::{
    DomainLineKind, EntityTeachingExprRow, TeachingExprLine, TeachingFieldGloss, TeachingHeading,
    TeachingPromptBundle, TEACHING_OPTIONAL_LEGEND_MARK, TSV_TEACHING_TABLE_HEADER,
};

fn tsv_expr_has_symbolic_method_call(expr: &str) -> bool {
    let b = expr.as_bytes();
    let mut i = 0usize;
    while i + 2 < b.len() {
        if b[i] == b'.' && b[i + 1] == b'm' && b[i + 2].is_ascii_digit() {
            return true;
        }
        i += 1;
    }
    false
}

fn tsv_identity_expr_is_entity_get(expr: &str) -> bool {
    let t = expr.trim_start();
    if tsv_expr_has_symbolic_method_call(t) {
        return false;
    }
    let Some(open) = t.find('(') else {
        return false;
    };
    !t[..open].contains('.')
}

fn compute_tsv_identity_row_index(teaching_expr_rows: &[&TeachingExprLine]) -> Option<usize> {
    teaching_expr_rows
        .iter()
        .position(|row| {
            !row.is_projection_teaching
                && tsv_identity_expr_is_entity_get(&row.expression)
                && !row.expression.contains('{')
                && !row.expression.contains('~')
                && !row.result_type.starts_with('[')
        })
        .or_else(|| {
            teaching_expr_rows.iter().position(|row| {
                !row.is_projection_teaching
                    && row.expression.contains('(')
                    && !row.expression.contains('{')
                    && !row.expression.contains('~')
                    && !row.result_type.starts_with('[')
            })
        })
        .or_else(|| {
            (teaching_expr_rows.len() == 1 && !teaching_expr_rows[0].is_projection_teaching)
                .then_some(0)
        })
}

/// Scalar projection bracket `[p#,…]` from a synthesized projection-teaching row (`TeachingExprLine`).
pub(crate) fn projection_bracket_from_teaching_rows(rows: &[&TeachingExprLine]) -> Option<String> {
    for row in rows {
        if !row.is_projection_teaching {
            continue;
        }
        if let Some(b) = parse_trailing_projection_bracket(row.expression.trim()) {
            return Some(b);
        }
    }
    None
}

/// Top-level union constructor teaching exemplars (`v101{p#=…}`), distinct from bare value-domain `v#` gloss symbols.
pub(crate) fn is_union_ctor_teaching_surface_line(expr: &str) -> bool {
    let e = expr.trim_start();
    let b = e.as_bytes();
    if b.first() != Some(&b'v') {
        return false;
    }
    let mut i = 1usize;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i > 1 && i < b.len() && b[i] == b'{'
}

/// Numeric ordering for opaque `pN` / `rN` / `vN` tokens (`p12` before `p101`, not lexicographic).
fn opaque_pv_symbol_sort_key(sym: &str) -> Option<(u32, u32)> {
    let mut it = sym.chars();
    let prefix = it.next()?;
    if prefix != 'p' && prefix != 'v' && prefix != 'r' {
        return None;
    }
    let rest: String = it.collect();
    let n = rest.parse::<u32>().ok()?;
    Some((prefix as u32, n))
}

pub(crate) fn teaching_relation_field_gloss(
    map: &SymbolMap,
    r_sym: &str,
    description: &str,
) -> Option<TeachingFieldGloss> {
    let wire = map.resolve_relation_ident(r_sym)?;
    Some(TeachingFieldGloss {
        symbol: r_sym.to_string(),
        field_type: wire.to_string(),
        allowed_values: String::new(),
        description: description.to_string(),
        is_inline_union_summary: false,
    })
}

fn write_sorted_symbol_prefix_gloss_rows(
    out: &mut String,
    field_gloss_rows: &[TeachingFieldGloss],
    prefix: char,
) {
    let mut gloss: Vec<&TeachingFieldGloss> = field_gloss_rows
        .iter()
        .filter(|g| g.symbol.starts_with(prefix))
        .collect();
    gloss.sort_by(|a, b| {
        let ka = opaque_pv_symbol_sort_key(&a.symbol);
        let kb = opaque_pv_symbol_sort_key(&b.symbol);
        ka.cmp(&kb).then_with(|| a.symbol.cmp(&b.symbol))
    });
    let mut emitted = HashSet::new();
    for g in gloss {
        if emitted.insert(g.symbol.clone()) {
            write_teaching_tsv_row(out, DomainTsvRow::FieldGloss(g));
        }
    }
}
pub(crate) fn render_prompt_tsv_from_bundle(bundle: &TeachingPromptBundle) -> String {
    let mut out = String::new();
    out.push_str(TSV_TEACHING_TABLE_HEADER);
    let mut global_p_gloss_emitted: HashMap<String, String> = HashMap::new();
    let gloss_emit_fingerprint =
        |g: &TeachingFieldGloss| format!("{}|{}|{}", g.field_type, g.allowed_values, g.description);
    for block in &bundle.teaching_blocks {
        let heading = &block.heading;
        let field_gloss_rows = &block.field_gloss_rows;
        let teaching_expr_rows: Vec<&TeachingExprLine> = block
            .teaching_rows
            .iter()
            .map(|r| &r.teaching_expr)
            .collect();
        let union_ctor_row_idxs: Vec<usize> = teaching_expr_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| is_union_ctor_teaching_surface_line(&r.expression))
            .map(|(i, _)| i)
            .collect();
        let union_ctor_row_set: HashSet<usize> = union_ctor_row_idxs.iter().copied().collect();
        let identity_idx = compute_tsv_identity_row_index(&teaching_expr_rows);
        let projection_first_idx = teaching_expr_rows
            .iter()
            .position(|r| r.is_projection_teaching);
        let entity_desc_attach_idx = projection_first_idx.or(identity_idx);
        // Do not read projection from the entity heading: legends may contain unrelated `[…]`
        // fragments (e.g. `[e1]` in result gloss). Teach projection only via a validated witness row
        // and/or
        // a trailing `[p#,…]` on the identity get line.
        let mut proj =
            projection_bracket_from_teaching_rows(&teaching_expr_rows).unwrap_or_default();
        if proj.is_empty() {
            if let Some(i) = identity_idx {
                if let Some(s) =
                    parse_trailing_projection_bracket(teaching_expr_rows[i].expression.trim())
                {
                    proj = s;
                }
            }
        }
        let projection_symbols = parse_projection_symbols(&proj);
        let projection_set: HashSet<&str> = projection_symbols.iter().map(|s| s.as_str()).collect();
        let mut field_gloss_by_symbol: HashMap<String, TeachingFieldGloss> = HashMap::new();
        for g in field_gloss_rows {
            field_gloss_by_symbol.insert(g.symbol.clone(), g.clone());
        }
        // Phase A: `v#` field gloss except deferred synthetic union summaries (`is_inline_union_summary`).
        let mut seen_v_phase_a: HashSet<String> = HashSet::new();
        for g in field_gloss_rows {
            if !g.symbol.starts_with('v') || g.is_inline_union_summary {
                continue;
            }
            if seen_v_phase_a.insert(g.symbol.clone()) {
                write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
            }
        }
        // Phase B: `p#` gloss — non-projection numeric order, then projection bracket tail.
        // `p#` slots for optional query params / invokes appear in teaching gloss lines but are not part
        // of the scalar projection bracket — emit them in stable numeric `p#` order before projection fields.
        let mut p_non_projection: Vec<&TeachingFieldGloss> = field_gloss_rows
            .iter()
            .filter(|g| g.symbol.starts_with('p') && !projection_set.contains(g.symbol.as_str()))
            .collect();
        p_non_projection.sort_by(|a, b| {
            let ka = opaque_pv_symbol_sort_key(&a.symbol);
            let kb = opaque_pv_symbol_sort_key(&b.symbol);
            ka.cmp(&kb).then_with(|| a.symbol.cmp(&b.symbol))
        });
        let mut emitted_p_slot: HashSet<String> = HashSet::new();
        for g in p_non_projection {
            if !emitted_p_slot.insert(g.symbol.clone()) {
                continue;
            }
            if g.symbol.starts_with('p') {
                let fp = gloss_emit_fingerprint(g);
                if global_p_gloss_emitted
                    .get(&g.symbol)
                    .is_some_and(|prev| prev == &fp)
                {
                    continue;
                }
                global_p_gloss_emitted.insert(g.symbol.clone(), fp);
            }
            write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
        }
        for sym in &projection_symbols {
            if emitted_p_slot.contains(sym) {
                continue;
            }
            if let Some(gloss) = field_gloss_by_symbol.get(sym.as_str()) {
                if sym.starts_with('p') {
                    let fp = gloss_emit_fingerprint(gloss);
                    if global_p_gloss_emitted
                        .get(sym.as_str())
                        .is_some_and(|prev| prev == &fp)
                    {
                        continue;
                    }
                    global_p_gloss_emitted.insert(sym.clone(), fp);
                }
                write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(gloss));
                emitted_p_slot.insert(sym.clone());
            }
        }

        // Phase B.5: `r#` gloss — stable numeric order (alias → wire name on the Meaning cell).
        write_sorted_symbol_prefix_gloss_rows(&mut out, field_gloss_rows, 'r');

        // Phase C: union constructor exemplars (`v101{p#=…}`) — before deferred union summary gloss.
        for &row_idx in &union_ctor_row_idxs {
            let row = teaching_expr_rows[row_idx];
            let identity_returns_row = Some(row_idx) == identity_idx;
            let attach_entity_heading = Some(row_idx) == entity_desc_attach_idx;
            write_teaching_tsv_row(
                &mut out,
                DomainTsvRow::TeachingExpr {
                    line: row,
                    identity_returns_row,
                    attach_entity_heading,
                    heading,
                },
            );
        }
        // Phase D: deferred inline union summary (`union · v101 | …`).
        for g in field_gloss_rows {
            if g.is_inline_union_summary {
                write_teaching_tsv_row(&mut out, DomainTsvRow::FieldGloss(g));
            }
        }

        // Phase E: remaining teaching expr rows (projection witnesses last).
        let mut emit_order: Vec<usize> = (0..teaching_expr_rows.len()).collect();
        emit_order.sort_by_key(|&i| {
            let is_proj = teaching_expr_rows[i].is_projection_teaching;
            (!is_proj, i)
        });
        for row_idx in emit_order {
            if union_ctor_row_set.contains(&row_idx) {
                continue;
            }
            let row = teaching_expr_rows[row_idx];
            let identity_returns_row = Some(row_idx) == identity_idx;
            let attach_entity_heading = Some(row_idx) == entity_desc_attach_idx;
            write_teaching_tsv_row(
                &mut out,
                DomainTsvRow::TeachingExpr {
                    line: row,
                    identity_returns_row,
                    attach_entity_heading,
                    heading,
                },
            );
        }
    }
    enforce_teaching_tsv_teaching_invariant(&out);
    out
}

const TSV_MEANING_JOIN: &str = " · ";

/// One logical TSV row before wire encoding ([`write_teaching_tsv_row`]).
pub(crate) enum DomainTsvRow<'a> {
    TeachingExpr {
        line: &'a TeachingExprLine,
        /// [`compute_tsv_identity_row_index`] — affects relation vs `returns …` gloss shaping.
        identity_returns_row: bool,
        /// Entity banner description at most once: first projection witness, else identity fallback.
        attach_entity_heading: bool,
        heading: &'a TeachingHeading,
    },
    FieldGloss(&'a TeachingFieldGloss),
}

/// Replace raw tabs inside a cell and trim edges (never used as column delimiter).
fn sanitize_tsv_cell(s: &str) -> String {
    if !s.contains('\t') {
        return s.trim().to_string();
    }
    s.replace('\t', " ").trim().to_string()
}

/// Typed fragment of a teaching-row `Meaning` cell (joined with [`TSV_MEANING_JOIN`], then sanitized as a whole).
#[derive(Clone, Debug)]
enum TeachingMeaningAtom {
    Returns { gloss: String },
    RelationNav { line: String },
    EntityHeadingDescription(String),
    LegendScope(String),
    LegendOptional,
    LegendCompactArgs(String),
    LegendDescription(String),
}

/// True when the teaching bundle includes at least one **relation navigation** exemplar row.
#[allow(dead_code)]
pub(crate) fn teaching_bundle_has_relation_nav_exemplar(bundle: &TeachingPromptBundle) -> bool {
    bundle.teaching_blocks.iter().any(|block| {
        block
            .teaching_rows
            .iter()
            .any(|row| row.meta.kind == DomainLineKind::RelationNav)
    })
}

/// True when an emitted teaching row already demonstrates **relation navigation** on `rel_sym`
/// (receiver`.rel_sym`), not merely a scoped query filter `e#{rel_sym=…}`.
pub(crate) fn relation_sym_shown_in_query_teaching_rows(
    teaching_rows: &[EntityTeachingExprRow],
    rel_sym: &str,
) -> bool {
    let dotted = format!(".{rel_sym}");
    teaching_rows.iter().any(|row| {
        row.teaching_expr
            .expression
            .as_str()
            .contains(dotted.as_str())
    })
}

impl TeachingMeaningAtom {
    fn encoded_fragment(&self) -> String {
        let raw = match self {
            TeachingMeaningAtom::Returns { gloss } => format!("→ {gloss}"),
            TeachingMeaningAtom::RelationNav { line } => line.clone(),
            TeachingMeaningAtom::EntityHeadingDescription(s) => s.clone(),
            TeachingMeaningAtom::LegendScope(s) => s.clone(),
            TeachingMeaningAtom::LegendOptional => TEACHING_OPTIONAL_LEGEND_MARK.to_string(),
            TeachingMeaningAtom::LegendCompactArgs(s) => format!("args: {s}"),
            TeachingMeaningAtom::LegendDescription(s) => s.clone(),
        };
        sanitize_tsv_cell(&raw)
    }
}

/// Typed fragment of a field-gloss `Meaning` cell.
#[derive(Clone, Debug)]
enum FieldGlossMeaningAtom {
    FieldType(String),
    AllowedValues(String),
    Description(String),
}

impl FieldGlossMeaningAtom {
    fn encoded_fragment(&self) -> String {
        let raw = match self {
            FieldGlossMeaningAtom::FieldType(s) => s.clone(),
            FieldGlossMeaningAtom::AllowedValues(s) => format!("allowed: {s}"),
            FieldGlossMeaningAtom::Description(s) => s.clone(),
        };
        sanitize_tsv_cell(&raw)
    }
}

/// Sanitized `plasm_expr` column for teaching table teaching TSV (no literal tabs; trimmed).
#[derive(Clone, Debug)]
struct DomainTsvExprCell(String);

impl DomainTsvExprCell {
    fn from_plasm_expr(expr: &str) -> Self {
        Self(sanitize_tsv_cell(expr))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Sanitized `Meaning` column for teaching table teaching TSV (no literal tabs; trimmed).
#[derive(Clone, Debug)]
struct DomainTsvMeaningCell(String);

impl DomainTsvMeaningCell {
    fn from_teaching_atoms(atoms: Vec<TeachingMeaningAtom>) -> Self {
        Self(Self::join_encoded_fragments(
            atoms.into_iter().map(|a| a.encoded_fragment()),
        ))
    }

    fn from_field_gloss_atoms(atoms: Vec<FieldGlossMeaningAtom>) -> Self {
        Self(Self::join_encoded_fragments(
            atoms.into_iter().map(|a| a.encoded_fragment()),
        ))
    }

    fn join_encoded_fragments(fragments: impl Iterator<Item = String>) -> String {
        let joined = fragments
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(TSV_MEANING_JOIN);
        sanitize_tsv_cell(&joined)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// One encoded teaching table teaching row: sanitized expr, **exactly one** U+0009, sanitized meaning, newline.
struct DomainTsvEncodedLine {
    expr: DomainTsvExprCell,
    meaning: DomainTsvMeaningCell,
}

impl DomainTsvEncodedLine {
    fn write_line(self, out: &mut String) {
        let expr_s = self.expr.as_str();
        let meaning_s = self.meaning.as_str();
        debug_assert!(
            !expr_s.contains('\t'),
            "expr cell must be tab-free before wire emit"
        );
        debug_assert!(
            !meaning_s.contains('\t'),
            "meaning cell must be tab-free before wire emit"
        );
        out.push_str(expr_s);
        out.push('\t');
        out.push_str(meaning_s);
        out.push('\n');
    }
}

fn teaching_expr_meaning_atoms(
    row: &TeachingExprLine,
    identity_returns_row: bool,
    attach_entity_heading: bool,
    heading: &TeachingHeading,
) -> Vec<TeachingMeaningAtom> {
    let mut atoms = Vec::new();
    push_teaching_meaning_result_atom(&mut atoms, row, identity_returns_row);
    if attach_entity_heading && !heading.description.is_empty() {
        atoms.push(TeachingMeaningAtom::EntityHeadingDescription(
            heading.description.clone(),
        ));
    }
    append_teaching_meaning_legend_tail_atoms(&mut atoms, row);
    atoms
}

fn field_gloss_meaning_atoms(g: &TeachingFieldGloss) -> Vec<FieldGlossMeaningAtom> {
    let mut atoms = vec![FieldGlossMeaningAtom::FieldType(g.field_type.clone())];
    if !g.allowed_values.is_empty() {
        atoms.push(FieldGlossMeaningAtom::AllowedValues(
            g.allowed_values.clone(),
        ));
    }
    if !g.description.is_empty() {
        atoms.push(FieldGlossMeaningAtom::Description(g.description.clone()));
    }
    atoms
}

fn append_teaching_meaning_legend_tail_atoms(
    atoms: &mut Vec<TeachingMeaningAtom>,
    row: &TeachingExprLine,
) {
    if !row.legend.scope.is_empty() {
        atoms.push(TeachingMeaningAtom::LegendScope(row.legend.scope.clone()));
    }
    if row.legend.optional.is_present() {
        atoms.push(TeachingMeaningAtom::LegendOptional);
    }
    if !row.legend.compact_args.is_empty() {
        atoms.push(TeachingMeaningAtom::LegendCompactArgs(
            row.legend.compact_args.clone(),
        ));
    }
    if !row.legend.description.is_empty() {
        atoms.push(TeachingMeaningAtom::LegendDescription(
            row.legend.description.clone(),
        ));
    }
}

/// When `identity_row`, always prefix with `returns …` (including relation-nav identity picks).
fn push_teaching_meaning_result_atom(
    atoms: &mut Vec<TeachingMeaningAtom>,
    row: &TeachingExprLine,
    identity_row: bool,
) {
    if row.result_type.is_empty() {
        return;
    }
    if identity_row {
        atoms.push(TeachingMeaningAtom::Returns {
            gloss: row.result_type.clone(),
        });
    } else if row.result_type.starts_with("relation ") {
        atoms.push(TeachingMeaningAtom::RelationNav {
            line: row.result_type.clone(),
        });
    } else {
        atoms.push(TeachingMeaningAtom::Returns {
            gloss: row.result_type.clone(),
        });
    }
}

pub(crate) fn write_teaching_tsv_row(out: &mut String, row: DomainTsvRow<'_>) {
    match row {
        DomainTsvRow::TeachingExpr {
            line,
            identity_returns_row,
            attach_entity_heading,
            heading,
        } => {
            DomainTsvEncodedLine {
                expr: DomainTsvExprCell::from_plasm_expr(&line.expression),
                meaning: DomainTsvMeaningCell::from_teaching_atoms(teaching_expr_meaning_atoms(
                    line,
                    identity_returns_row,
                    attach_entity_heading,
                    heading,
                )),
            }
            .write_line(out);
        }
        DomainTsvRow::FieldGloss(g) => {
            DomainTsvEncodedLine {
                expr: DomainTsvExprCell::from_plasm_expr(&g.symbol),
                meaning: DomainTsvMeaningCell::from_field_gloss_atoms(field_gloss_meaning_atoms(g)),
            }
            .write_line(out);
        }
    }
}

fn parse_projection_symbols(projection: &str) -> Vec<String> {
    projection
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(|inner| {
            inner
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Suffix on a get expression, e.g. `e#(p#=$,…)[p1,p2,…]`, for projection teaching on the same line
/// as the primary Get (avoids a duplicate list on the entity heading).
pub(crate) fn parse_trailing_projection_bracket(expr: &str) -> Option<String> {
    let t = expr.trim();
    if t.len() < 3 || !t.ends_with(']') {
        return None;
    }
    let open = t.rfind('[')?;
    (open + 1 < t.len()).then_some(t[open..].to_string())
}

#[cfg(test)]
pub(crate) fn teaching_row_meaning_text(
    row: &TeachingExprLine,
    identity_returns_row: bool,
    attach_entity_heading: bool,
    heading: &TeachingHeading,
) -> String {
    DomainTsvMeaningCell::from_teaching_atoms(teaching_expr_meaning_atoms(
        row,
        identity_returns_row,
        attach_entity_heading,
        heading,
    ))
    .as_str()
    .to_owned()
}
