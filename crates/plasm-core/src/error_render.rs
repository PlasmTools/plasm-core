//! Structured [`StepError`](crate::step_semantics::StepError) values: **correction** for the LLM, **error** for logs.

use crate::domain_term::{resolve_parameter_slot, ParameterSlot};
use crate::expr_correction::RecoveryHint;
use crate::expr_parser::{ParseError, ParseErrorKind};
use crate::query_resolve::QueryCapabilityResolveError;
use crate::schema::{
    capability_is_zero_arity_invoke, capability_method_label_kebab, capability_path_method_segment,
    CapabilityKind, StringSemantics, CGS,
};
use crate::step_semantics::{append_correction_lines, StepError};
use crate::symbol_tuning::SymbolMap;
use crate::FieldType;
use crate::TypeError;

const LEV_ACCEPT: usize = 3;
const NEAREST_SHOW: usize = 5;

/// How to phrase [`StepError::correction`](crate::step_semantics::StepError) for the LLM.
/// The structured [`StepError::error`](crate::step_semantics::StepError) field stays canonical (parser / type raw line) in all modes.
#[derive(Clone)]
pub enum FeedbackStyle<'a> {
    /// Developer-oriented copy (entity names, path method segment spellings).
    CanonicalDev,
    /// LLM symbolic mode: `e#` / `m#` / `p#` aligned with prompt examples — no “zero-arity” / “kebab-case” sermons.
    SymbolicLlm { map: &'a SymbolMap },
}

#[inline]
fn feedback_ident_symbol(map: &SymbolMap, ident: &str) -> String {
    map.ident_sym_unambiguous(ident)
        .unwrap_or_else(|| ident.to_string())
}

fn feedback_predicate_ident_symbol(
    cgs: &CGS,
    map: &SymbolMap,
    entity: &str,
    ident: &str,
) -> String {
    let mut resolved: Option<String> = None;
    if cgs
        .get_entity(entity)
        .is_some_and(|ent| ent.fields.contains_key(ident))
    {
        resolved = Some(map.ident_sym_entity_field(entity, ident));
    }
    for kind in [CapabilityKind::Query, CapabilityKind::Search] {
        for cap in cgs.find_capabilities(entity, kind) {
            if let Some(fields) = cap.object_params() {
                for f in fields {
                    if f.name != ident {
                        continue;
                    }
                    let sym = map.ident_sym_cap_param(entity, cap.name.as_str(), ident);
                    match &resolved {
                        None => resolved = Some(sym),
                        Some(prev) if prev == &sym => {}
                        Some(_) => return ident.to_string(),
                    }
                }
            }
        }
    }
    resolved.unwrap_or_else(|| feedback_ident_symbol(map, ident))
}

/// Lexicon [`RecoveryHint`] rows → correction lines; symbolic mode stays all-`e#`/`p#` (no mixed canonical).
pub fn format_recovery_hints(hints: &[RecoveryHint], style: FeedbackStyle<'_>) -> String {
    hints
        .iter()
        .map(|h| format_one_recovery_hint(h, &style))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_one_recovery_hint(h: &RecoveryHint, style: &FeedbackStyle<'_>) -> String {
    match (h, style) {
        (
            RecoveryHint::AmbiguousScopes {
                entity,
                scope_options,
            },
            FeedbackStyle::CanonicalDev,
        ) => {
            let opts: Vec<String> = scope_options
                .iter()
                .map(|(f, t)| format!("{entity}{{{f}={t}(id)}}"))
                .collect();
            let scope_line = scope_options
                .iter()
                .map(|(f, t)| format!("`{f}` → `{t}`"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "`{entity}` must include exactly one scope in `{{…}}`: {scope_line}.\n\nPick one: {}",
                opts.join(" | ")
            )
        }
        (
            RecoveryHint::AmbiguousScopes {
                entity,
                scope_options,
            },
            FeedbackStyle::SymbolicLlm { map },
        ) => {
            let es = map.entity_sym(entity);
            let opts: Vec<String> = scope_options
                .iter()
                .map(|(f, t)| {
                    let ps = feedback_ident_symbol(map, f);
                    let ts = map.entity_sym(t);
                    format!("{es}{{{ps}={ts}(id)}}")
                })
                .collect();
            let scope_line = scope_options
                .iter()
                .map(|(f, t)| {
                    let ps = feedback_ident_symbol(map, f);
                    let ts = map.entity_sym(t);
                    format!("`{ps}` → `{ts}`")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "`{es}` must include exactly one scope in `{{…}}`: {scope_line}.\n\nPick one: {}",
                opts.join(" | ")
            )
        }
        (
            RecoveryHint::AmbiguousFieldCandidates {
                option_expressions, ..
            },
            FeedbackStyle::CanonicalDev,
        ) => {
            format!("Pick one: {}", option_expressions.join(" | "))
        }
        (
            RecoveryHint::AmbiguousFieldCandidates {
                option_expressions, ..
            },
            FeedbackStyle::SymbolicLlm { map },
        ) => {
            let collapsed: Vec<String> = option_expressions
                .iter()
                .map(|s| map.collapse_tokens_for_feedback(s))
                .collect();
            format!("Pick one: {}", collapsed.join(" | "))
        }
    }
}

/// LLM-facing **correction** line for query capability resolution (symbolic entity symbols vs canonical names).
pub fn render_query_resolve_error_for_feedback(
    e: &QueryCapabilityResolveError,
    style: FeedbackStyle<'_>,
) -> String {
    const PREFIX: &str = "Query capability resolution failed: ";
    match style {
        FeedbackStyle::CanonicalDev => format!("{PREFIX}{e}"),
        FeedbackStyle::SymbolicLlm { map } => {
            let body = match e {
                QueryCapabilityResolveError::CapabilityNotFound {
                    capability: _,
                    entity,
                } => {
                    let es = map.entity_sym(entity);
                    format!("named query capability not found for entity {es} (check the query example lines in the prompt for `{es}`).")
                }
                QueryCapabilityResolveError::Ambiguous { entity, names: _ } => {
                    let es = map.entity_sym(entity);
                    format!(
                        "ambiguous query for entity {es}: predicate matches more than one capability; narrow filters or scope per the `;;` lines in the prompt."
                    )
                }
                QueryCapabilityResolveError::NoMatchingCapability { entity, message } => {
                    let es = map.entity_sym(entity);
                    // Avoid listing raw capability keys (`list_query`, …) in LLM-facing text; examples teach shape.
                    let msg = message
                        .split("Available:")
                        .next()
                        .unwrap_or(message.as_str())
                        .trim()
                        .trim_end_matches(['.', ' '])
                        .to_string();
                    let scope_hint = if msg.contains("scope") {
                        "\n\nIf the predicate already supplies some scope fields but resolution still fails, add each remaining scope key listed on that entity's query line in the TSV teaching table, or use another entity whose query rows match the result shape you need."
                    } else {
                        ""
                    };
                    format!(
                        "{msg}. See the query example lines in the prompt for `{es}` for which `p#` scopes and filters apply.{scope_hint}"
                    )
                }
            };
            format!("{PREFIX}{body}")
        }
    }
}

/// Parse failure → `StepError` with imperative **correction**; raw parse line in `error`.
pub fn render_parse_error(err: &ParseError, input: &str, cgs: &CGS) -> StepError {
    render_parse_error_with_feedback(err, input, input, cgs, FeedbackStyle::CanonicalDev)
}

/// Line to echo in **`correction`** text (`Expression: …`). Use the model’s original step when
/// symbolic; use `work` (expanded canonical) when [`FeedbackStyle::CanonicalDev`].
fn expr_line_for_feedback<'a>(
    work: &'a str,
    display_input: &'a str,
    style: &FeedbackStyle<'_>,
) -> &'a str {
    match style {
        FeedbackStyle::CanonicalDev => work,
        FeedbackStyle::SymbolicLlm { .. } => display_input,
    }
}

/// Heuristic: structured text (markdown-like) near the failure offset — suggest quoted strings or tagged `<<TAG` heredocs.
fn markdown_like_payload_near(work: &str, offset: usize) -> bool {
    let start = offset.saturating_sub(160);
    let end = (offset + 120).min(work.len());
    if start >= end {
        return false;
    }
    let slice = &work[start..end];
    slice.contains("##")
        || slice.contains("**")
        || slice.contains("```")
        || slice.contains("\n- ")
        || slice.contains("\n* ")
        || slice.contains("\n1. ")
}

fn looks_like_p_sym_token(name: &str) -> bool {
    name.len() > 1 && name.starts_with('p') && name[1..].chars().all(|c| c.is_ascii_digit())
}

fn resolve_wire_param_name_for_feedback(name: &str, style: &FeedbackStyle<'_>) -> String {
    match style {
        FeedbackStyle::SymbolicLlm { map } if looks_like_p_sym_token(name) => {
            map.resolve_ident(name).unwrap_or(name).to_string()
        }
        _ => name.to_string(),
    }
}

/// Best-effort: parameter LHS immediately before the `=` whose value contains `offset` (quoted strings, raw tokens like `##`, etc.).
fn infer_param_lhs_name(work: &str, offset: usize) -> Option<&str> {
    let head = work.get(..offset)?;
    let eq_idx = head.rfind('=')?;
    let before_eq = head.get(..eq_idx)?;
    let before_trim = before_eq.trim_end();
    let start = before_trim
        .rfind(|c: char| c == '(' || c == ',' || c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    let name = before_trim.get(start..)?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn string_semantics_for_wire_param(
    cgs: &CGS,
    full_entities: &[&str],
    wire_name: &str,
) -> Option<StringSemantics> {
    let slot = resolve_parameter_slot(cgs, full_entities, wire_name)?;
    match slot {
        ParameterSlot::EntityField { entity, field } => {
            let f = cgs
                .get_entity(entity.as_str())?
                .fields
                .get(field.as_str())?;
            let nv = cgs.named_value_for_slot(f).ok()?;
            if matches!(nv.field_type, FieldType::Blob) {
                Some(crate::StringSemantics::Blob)
            } else if matches!(nv.field_type, FieldType::String) {
                Some(f.effective_string_semantics(cgs))
            } else {
                None
            }
        }
        ParameterSlot::CapabilityInput {
            domain,
            capability,
            param,
        } => {
            let cap = cgs.capabilities.values().find(|c| {
                c.domain.as_str() == domain.as_str() && c.name.as_str() == capability.as_str()
            })?;
            let fields = cap.object_params()?;
            let f = fields.iter().find(|p| p.name == param)?;
            let nv = cgs.named_value_for_slot(f).ok()?;
            if matches!(nv.field_type, FieldType::Blob) {
                Some(crate::StringSemantics::Blob)
            } else if matches!(nv.field_type, FieldType::String) {
                Some(f.effective_string_semantics(cgs))
            } else {
                None
            }
        }
        ParameterSlot::Relation { .. } => None,
    }
}

/// When [`ParseErrorKind::UnterminatedString`] fires after a `<<TAG` opener, return a targeted tagged-close correction
/// (mirrors [`crate::expr_parser::value::Parser::parse_structured_heredoc`]).
fn correction_unterminated_heredoc(prefix: &str) -> Option<String> {
    let bytes = prefix.as_bytes();
    let mut last_open = None;
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] != b'<' || bytes[i + 1] != b'<' {
            continue;
        }
        if i + 2 < bytes.len() && bytes[i + 2] == b'<' {
            continue;
        }
        last_open = Some(i);
    }
    let open_idx = last_open?;
    let j = open_idx + 2;
    if j >= bytes.len() {
        return None;
    }
    if bytes[j] == b'\n' {
        return Some(
            "Multiline structured strings use **tagged** heredocs only: replace `<<` + newline with `<<TAG` + newline (pick a `TAG` that does not appear as a whole trimmed body line), then end the value with a line whose trimmed text is `TAG`. Old untagged `<<` … `>>>` is removed."
                .to_string(),
        );
    }
    let b0 = bytes[j];
    if !(b0.is_ascii_alphabetic() || b0 == b'_') {
        return None;
    }
    let mut k = j + 1;
    while k < bytes.len() && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_') {
        k += 1;
    }
    let tag = &prefix[j..k];
    if k >= bytes.len() || bytes[k] != b'\n' {
        return None;
    }
    Some(format!(
        "Tagged heredoc `<<{tag}` is not closed: after the body, add `{tag}` with a **hard newline** after it before `)` / `,` / `}}`, or put `{tag}` immediately before that delimiter on the same line. For normal `\"…\"` add the closing quote. If you already wrote `{tag}` but parse still reaches end-of-input here, check whether **another line inside the body trimmed to `{tag}`** — the heredoc closes on the **first** such line; use a longer opaque `TAG` for MIME/RFC822 or unconstrained text.",
        tag = tag
    ))
}

/// Same as [`render_parse_error`], with [`FeedbackStyle`] for symbolic eval / prompt-aligned feedback.
///
/// - **`work`**: expanded string the parser used (canonical entity names); offsets in [`ParseError`] refer to this buffer.
/// - **`display_input`**: text to show in corrections — same as `work` for dev mode; **original model line** for symbolic eval.
///
/// The **`error`** field remains the canonical parser diagnostic (`err.message()` + `work`) for logs.
/// Only **`correction`** uses `display_input` for echoed lines when `style` is [`FeedbackStyle::SymbolicLlm`].
pub fn render_parse_error_with_feedback(
    err: &ParseError,
    work: &str,
    display_input: &str,
    cgs: &CGS,
    style: FeedbackStyle<'_>,
) -> StepError {
    let error = format!("{} (input: {work:?})", err.message());
    let expr_line = expr_line_for_feedback(work, display_input, &style);
    let full_entity_refs: Vec<&str> = cgs.entities.keys().map(|k| k.as_str()).collect();

    let correction = match &err.kind {
        ParseErrorKind::NoEntityRefBridge {
            target_entity,
            source_entity,
        } => correction_no_entity_ref_bridge(cgs, target_entity, source_entity, &style),
        ParseErrorKind::NotNavigable {
            field,
            entity,
            span_start,
            span_end: _,
        } => correction_not_navigable(cgs, field, entity, expr_line, *span_start, &style),
        ParseErrorKind::PredicateFieldNotFound {
            field,
            entity,
            span_start,
            span_end: _,
        } => correction_predicate_field(cgs, field, entity, expr_line, *span_start, &style),
        ParseErrorKind::NotFieldOrRelation {
            field,
            entity,
            span_start,
            span_end: _,
        } => correction_navigation_name(cgs, field, entity, expr_line, *span_start, &style),
        ParseErrorKind::UnknownEntity { name, span_opt } => {
            correction_unknown_entity(cgs, name, expr_line, span_opt.as_ref(), &style)
        }
        ParseErrorKind::UnexpectedTrailingInput { .. } => {
            "Delete everything after the first complete path expression on this line. Only one expression per step."
                .into()
        }
        ParseErrorKind::InvalidTemporalValue { .. } => {
            let slot = infer_param_lhs_name(work, err.offset)
                .map(|n| resolve_wire_param_name_for_feedback(n, &style))
                .filter(|s| !s.is_empty());
            let head = match (&style, slot.as_deref()) {
                (FeedbackStyle::SymbolicLlm { .. }, Some(sym)) => format!(
                    "The `{sym}` slot expects a **date/time** value (see the `Meaning` cell for that field), not a boolean or arbitrary string. "
                ),
                (FeedbackStyle::CanonicalDev, Some(name)) => format!(
                    "The `{name}` parameter expects a **date/time** value, not a boolean or arbitrary string. "
                ),
                _ => String::new(),
            };
            format!(
                "{head}Use a date/time format allowed for that field (ISO-8601, RFC3339, Unix ms, or GNU-style English (chrono-english): e.g. `2024-06-01T12:00:00Z`, `next friday 8pm`, `30 June 2018`)."
            )
        }
        ParseErrorKind::UnterminatedString | ParseErrorKind::UnterminatedEscape => {
            let prefix_end = err.offset.min(work.len());
            let inferred_sem = infer_param_lhs_name(work, err.offset)
                .map(|n| resolve_wire_param_name_for_feedback(n, &style))
                .and_then(|wire| {
                    string_semantics_for_wire_param(cgs, &full_entity_refs, wire.as_str())
                });
            let structured_slot = inferred_sem
                .map(StringSemantics::is_structured_or_multiline)
                .unwrap_or(false);
            if work[..prefix_end].contains("<<") {
                correction_unterminated_heredoc(&work[..prefix_end]).unwrap_or_else(|| {
                    "Close the tagged heredoc: after the body, add `TAG` with a **hard newline** after it before `)` / `,` / `}}`, or put `TAG` immediately before that delimiter on the same line (same `TAG` as after `<<`). For normal `\"…\"` add the closing quote."
                        .to_string()
                })
            } else if structured_slot {
                "Close the string: the teaching-table gloss marks this parameter as structured text (not plain `str`). For multiline or quote-containing values you MUST use a tagged `<<TAG` … `TAG` heredoc; if you used normal `\"…\"` instead, add the closing quote and use only `\\\"` / `\\\\` escapes—`\\n` inside quotes is two characters, not a newline."
                    .into()
            } else {
                "Close string quotes and fix `\\` escapes.".into()
            }
        }
        ParseErrorKind::IdMustBeStringOrNumber => example_id_in_parens(cgs, &style),
        ParseErrorKind::EmptyGetParens { entity } => {
            correction_empty_get_parens(cgs, entity.as_str(), &style)
        },
        ParseErrorKind::ColonAfterEntityName { entity } => match style {
            FeedbackStyle::CanonicalDev => {
                format!("Use `{entity}(id)` for get-by-id. The `{entity}:id` shape is not Plasm syntax.")
            }
            FeedbackStyle::SymbolicLlm { map: _ } => {
                "Use the get form `e#(id)` from the teaching table for that entity — not `e#:…`.".into()
            }
        },
        ParseErrorKind::SearchTextMustBeString => match style {
            FeedbackStyle::CanonicalDev => {
                "Put the search text in quotes: `Entity~\"query\"`.".into()
            }
            FeedbackStyle::SymbolicLlm { map: _ } => {
                "Put the search text in quotes: `e#~\"query\"` using the entity symbol from the prompt."
                    .into()
            }
        },
        ParseErrorKind::SearchNotSupported { entity } => match style {
            FeedbackStyle::CanonicalDev => format!(
                "Entity `{entity}` has no Search capability in this schema: do not use `{entity}~…`. Use a query or get form from `{entity}`'s teaching-table block (e.g. `{entity}{{…}}` or `{entity}(id)`)."
            ),
            FeedbackStyle::SymbolicLlm { map: _ } => format!(
                "The entity for symbol covering `{entity}` has no `~` search line in the teaching table: use only example shapes from that entity's block (query `{{…}}`, get `(id)`, relations)."
            ),
        },
        ParseErrorKind::InvalidFloat { .. } | ParseErrorKind::InvalidInteger { .. } => {
            "Use a plain number literal.".into()
        }
        ParseErrorKind::NoZeroArityMethod { entity, label } => {
            correction_no_zero_arity_method(cgs, entity, label, &style)
        }
        ParseErrorKind::AmbiguousZeroArityMethod {
            entity,
            label,
            capability_names: _,
        } => correction_ambiguous_zero_arity_method(cgs, entity, label, &style),
        ParseErrorKind::DottedCallNoMatch {
            anchor_entity,
            label,
        } => correction_dotted_call_no_match(cgs, anchor_entity, label, &style),
        ParseErrorKind::DottedCallAmbiguous {
            anchor_entity,
            label,
        } => correction_dotted_call_ambiguous(cgs, anchor_entity, label, &style),
        ParseErrorKind::DottedCreateAmbiguous {
            anchor_entity,
            label,
        } => correction_dotted_create_ambiguous(cgs, anchor_entity, label, &style),
        ParseErrorKind::InvokeRequiresTargetId { .. } => match style {
            FeedbackStyle::CanonicalDev => {
                "This action needs an id from the path: write `Entity(<id>).method()` (see the expression examples in the prompt)."
                    .into()
            }
            FeedbackStyle::SymbolicLlm { map: _ } => {
                "This action needs an id from the path: write `e#(<id>).m#(...)` using symbols from the expression examples in the prompt."
                    .into()
            }
        },
        ParseErrorKind::ExpectedChar { expected, got } => {
            correction_expected_char(
                *expected,
                got.as_ref(),
                work,
                expr_line,
                err.offset,
                cgs,
                &style,
            )
        }
        ParseErrorKind::ExpectedIdentifier
        | ParseErrorKind::ExpectedOperator
        | ParseErrorKind::ExpectedValue => {
            let base = match style {
                FeedbackStyle::CanonicalDev => {
                    "Fix spelling so identifiers, `=`, `{{}}`, `.`, and parentheses match the expression examples in the prompt."
                        .to_string()
                }
                FeedbackStyle::SymbolicLlm { map: _ } => {
                    "Fix spelling so `e#` / `m#` / `p#`, `=`, `{{}}`, `.`, and parentheses match the example lines in the prompt."
                        .to_string()
                }
            };
            let inferred_sem = infer_param_lhs_name(work, err.offset)
                .map(|n| resolve_wire_param_name_for_feedback(n, &style))
                .and_then(|wire| {
                    string_semantics_for_wire_param(cgs, &full_entity_refs, wire.as_str())
                });
            let structured_slot = inferred_sem
                .map(StringSemantics::is_structured_or_multiline)
                .unwrap_or(false);
            let markdown_like = markdown_like_payload_near(work, err.offset);
            if matches!(err.kind, ParseErrorKind::ExpectedValue)
                && work.as_bytes().windows(3).any(|w| w == b"<<\n")
            {
                format!(
                    "{base} Use a **tagged** heredoc: `<<TAG` newline, body, closing line `TAG` (same identifier). The old untagged `<<` newline … `>>>` form was removed.",
                    base = base
                )
            } else if markdown_like && structured_slot {
                format!(
                    "{base} Teaching-table gloss marks this parameter as structured text (not plain `str`). For prose or markdown (headings, bullets, commas, embedded quotes) you MUST use a tagged `<<TAG` … `TAG` heredoc—not normal `\"…\"`."
                )
            } else if markdown_like && !structured_slot {
                format!(
                    "{base} If the value contains characters that break parsing (e.g. commas or unescaped quotes), wrap it in a quoted string and escape internal double quotes with `\\\"`."
                )
            } else {
                base
            }
        }
        ParseErrorKind::CapabilityMissingInternal { .. } => {
            "Internal schema error: capability missing after resolution (report upstream).".into()
        }
        ParseErrorKind::ManyRelationUnmaterialized {
            entity,
            relation,
            target,
            ..
        } => match style {
            FeedbackStyle::CanonicalDev => format!(
                "Many-relation `{entity}({{id}}).{relation}` is not supported: `{target}` has no chain materialization. Use a scoped query on `{target}` (see the teaching table) or add `materialize` for `{relation}` in the schema."
            ),
            FeedbackStyle::SymbolicLlm { map: _ } => format!(
                "This prompt does not support bare many-relation navigation to `{target}` for `{relation}`: use a list/query form from the `{target}` block in the teaching table, or the schema must declare materialization for that edge."
            ),
        },
        ParseErrorKind::Other { message } => message.clone(),
    };

    StepError::parse_correction(correction, error, Some(err.offset))
}

fn correction_expected_char(
    expected: char,
    got: Option<&char>,
    work: &str,
    _expr_line: &str,
    offset: usize,
    cgs: &CGS,
    style: &FeedbackStyle<'_>,
) -> String {
    if expected == ')' && got.map(|g| *g != ')').unwrap_or(true) {
        if let Some(h) = hint_invoke_payload_not_in_parens(work, offset, cgs, style) {
            return h;
        }
        if open_paren_is_invoke_style(work, offset) {
            let mut s = match style {
                FeedbackStyle::CanonicalDev => {
                    "Inside `method(` use either empty `)` for zero-arity methods (`Entity(id).method()`), or comma-separated `key=value` pairs when the prompt lists parameters for that operation. \
Check spelling, `=`, commas, and closing `)`."
                        .to_string()
                }
                FeedbackStyle::SymbolicLlm { .. } => {
                    "Inside `m#(` use either empty `)` when the prompt lists that method with no parameters (`e#(id).m#()`), or comma-separated `key=value` pairs when the prompt lists parameters for that operation. \
Check spelling, `=`, commas, and closing `)`."
                        .to_string()
                }
            };
            if let Some(ent) = leading_get_entity_name(work) {
                let labels = update_invoke_method_labels_for_feedback(cgs, ent, style);
                if !labels.is_empty() {
                    let joined = labels.join("`, `");
                    match style {
                        FeedbackStyle::CanonicalDev => {
                            s.push_str(&format!(
                                " Update-style operation names listed in the prompt for `{ent}` include `{joined}`."
                            ));
                        }
                        FeedbackStyle::SymbolicLlm { map } => {
                            let es = map.entity_sym(ent);
                            s.push_str(&format!(
                                " Update-style operations listed in the prompt for `{es}` include `{joined}`."
                            ));
                        }
                    }
                }
            }
            match style {
                FeedbackStyle::CanonicalDev => {
                    s.push_str(
                        "\n\nExample (zero-arity): `Task(id).create-checklist()` when the prompt lists that method.",
                    );
                }
                FeedbackStyle::SymbolicLlm { .. } => {
                    s.push_str(
                        "\n\nExample: `e#(id).m#()` when the prompt shows that `m#` with `;;` and no required parameters.",
                    );
                }
            }
            return s;
        }
        if get_id_region_has_ascii_whitespace(work, offset, got) {
            return match style {
                FeedbackStyle::CanonicalDev => {
                    "Do not put spaces inside `Entity(id)` — the id must be one contiguous token. Remove spaces, or use a quoted string for the id: `Entity(\"…\")`.".into()
                }
                FeedbackStyle::SymbolicLlm { .. } => {
                    "Do not put spaces inside `e#(id)` — the id must be one contiguous token. Remove spaces, or use a quoted string if the id needs other characters.".into()
                }
            };
        }
        return match style {
            FeedbackStyle::CanonicalDev => {
                "Expected `)` to close `Entity(…)` after the id. The id must be a single token (no spaces). Use digits, a contiguous hex string (0–9 and a–f), or a quoted string if the id needs other characters.".into()
            }
            FeedbackStyle::SymbolicLlm { .. } => {
                "Expected `)` to close `e#(…)` after the id. The id must be a single token (no spaces). Use digits, a contiguous hex string (0–9 and a–f), or a quoted string if the id needs other characters.".into()
            }
        };
    }
    let got_s = got
        .map(|c| format!("`{c}`"))
        .unwrap_or_else(|| "end of input".into());
    format!("Expected `{expected}` here; got {got_s}. Match the expression examples in the prompt.")
}

/// True when the parser failed while closing `…(id)` and the bytes between the last `(` before
/// the bad character and the start of that character contain ASCII whitespace — e.g.
/// `Task(123 456)` parses `123` then, after skipping spaces, hits `4` instead of `)`.
fn get_id_region_has_ascii_whitespace(input: &str, offset: usize, got: Option<&char>) -> bool {
    let got_len = got.map(|c| c.len_utf8()).unwrap_or(0);
    let inner_end = offset.saturating_sub(got_len);
    let Some(before) = input.get(..inner_end) else {
        return false;
    };
    let Some(lp) = before.rfind('(') else {
        return false;
    };
    let Some(inner) = input.get(lp + 1..inner_end) else {
        return false;
    };
    inner.chars().any(|c| c.is_ascii_whitespace())
}

/// After `.rename(` / `.update(` the parser expects `)` immediately; non-empty content is usually a mistaken payload.
fn hint_invoke_payload_not_in_parens(
    input: &str,
    offset: usize,
    cgs: &CGS,
    style: &FeedbackStyle<'_>,
) -> Option<String> {
    let pat_rename = ".rename(";
    if let Some(p) = input.find(pat_rename) {
        let after_open = p + pat_rename.len();
        if offset >= after_open {
            let mut s = match style {
                FeedbackStyle::CanonicalDev => {
                    "This expression language does not allow `rename(...)` with arguments — use `Entity(id)`, queries, navigation, zero-arity `method()`, or path-segment create/update/delete/action calls with `method(key=value,...)` when the prompt lists them. \
To point at a row, use `Entity(id)` (e.g. `Task(9abc…)`)."
                        .to_string()
                }
                FeedbackStyle::SymbolicLlm { .. } => {
                    "This expression language does not allow `rename(...)` with arguments — use `e#(id)`, queries, navigation, or `m#(key=value,...)` when the prompt lists parameters. \
To point at a row, use `e#(id)` as in the examples."
                        .to_string()
                }
            };
            if let Some(ent) = leading_get_entity_name(input) {
                let labels = update_invoke_method_labels_for_feedback(cgs, ent, style);
                if !labels.is_empty() {
                    let joined = labels.join("`, `");
                    match style {
                        FeedbackStyle::CanonicalDev => {
                            s.push_str(&format!(
                                " Changing `{ent}` fields is not spelled as `rename(...)` here; the prompt lists update operations such as `{joined}`."
                            ));
                        }
                        FeedbackStyle::SymbolicLlm { map } => {
                            let es = map.entity_sym(ent);
                            s.push_str(&format!(
                                " Changing `{es}` fields is not spelled as `rename(...)` here; the prompt lists update operations such as `{joined}`."
                            ));
                        }
                    }
                }
            }
            return Some(s);
        }
    }
    let pat_update = ".update(";
    if let Some(p) = input.find(pat_update) {
        let after_open = p + pat_update.len();
        if offset >= after_open {
            let mut s = match style {
                FeedbackStyle::CanonicalDev => {
                    "Do not use a literal `.update(...)` path segment — the prompt names update operations as path segments after `Entity(id).`. \
Use `Entity(id).operation()` for methods with no parameters, or `Entity(id).operation(key=value,...)` when the prompt lists parameters for that operation."
                        .to_string()
                }
                FeedbackStyle::SymbolicLlm { .. } => {
                    "Do not use a literal `.update(...)` path segment — the prompt lists update operations as `m#` after `e#(id).`. \
Use `e#(id).m#()` when the prompt shows no required parameters, or `e#(id).m#(key=value,...)` when the prompt lists parameters."
                        .to_string()
                }
            };
            if let Some(ent) = leading_get_entity_name(input) {
                let labels = update_invoke_method_labels_for_feedback(cgs, ent, style);
                if !labels.is_empty() {
                    let joined = labels.join("`, `");
                    match style {
                        FeedbackStyle::CanonicalDev => {
                            s.push_str(&format!(
                                " Field updates for `{ent}` use the update operation names the prompt lists (`{joined}`), not `.update(...)` as a path segment."
                            ));
                        }
                        FeedbackStyle::SymbolicLlm { map } => {
                            let es = map.entity_sym(ent);
                            s.push_str(&format!(
                                " Field updates for `{es}` use the update operations the prompt shows (`{joined}`), not `.update(...)` as a path segment."
                            ));
                        }
                    }
                }
            }
            return Some(s);
        }
    }
    None
}

/// Entity name in a leading `Entity(` … `)` get, if the prefix matches `^[A-Za-z_][A-Za-z0-9_]*\\(`.
fn leading_get_entity_name(input: &str) -> Option<&str> {
    let i = input.find('(')?;
    let name = input.get(..i)?;
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Some(name)
    } else {
        None
    }
}

/// Update-capability path method segments as strings for canonical feedback, or `m#` symbols for symbolic.
fn update_invoke_method_labels_for_feedback(
    cgs: &CGS,
    entity: &str,
    style: &FeedbackStyle<'_>,
) -> Vec<String> {
    let mut v: Vec<String> = cgs
        .find_capabilities(entity, CapabilityKind::Update)
        .iter()
        .map(|c| capability_path_method_segment(c))
        .map(|seg| match style {
            FeedbackStyle::CanonicalDev => seg.to_string(),
            FeedbackStyle::SymbolicLlm { map } => map.method_sym(entity, seg.as_str()),
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// True when the innermost `(` before `offset` is `ident(` after a `.` (invoke), not `Entity(` for Get.
fn open_paren_is_invoke_style(input: &str, offset: usize) -> bool {
    let Some(before) = input.get(..offset) else {
        return false;
    };
    let Some(lp) = before.rfind('(') else {
        return false;
    };
    if lp == 0 {
        return false;
    }
    let b = input.as_bytes();
    let mut j = lp;
    if j == 0 {
        return false;
    }
    j -= 1;
    while j > 0 && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'-') {
        j -= 1;
    }
    b.get(j) == Some(&b'.')
}

fn correction_empty_get_parens(cgs: &CGS, entity: &str, style: &FeedbackStyle<'_>) -> String {
    let get_caps = cgs.find_capabilities(entity, CapabilityKind::Get);
    let singletons: Vec<_> = get_caps
        .into_iter()
        .filter(|cap| {
            crate::schema::path_var_names_from_mapping_json(&cap.mapping.template.0).is_empty()
                && capability_is_zero_arity_invoke(cap)
        })
        .collect();
    match style {
        FeedbackStyle::CanonicalDev => {
            if !singletons.is_empty() {
                let methods: Vec<String> = singletons
                    .iter()
                    .map(|c| {
                        let lab = capability_method_label_kebab(c);
                        format!("{entity}.{lab}")
                    })
                    .collect();
                format!(
                    "Empty parentheses after `{entity}` are not valid for Get. Use `{entity}(<id>)` with an id, or a pathless singleton fetch shown in the prompt: {}.",
                    methods.join(", ")
                )
            } else {
                format!(
                    "Put a non-empty id inside parentheses: `{entity}(<id>)` (see the expression examples for the id field type)."
                )
            }
        }
        FeedbackStyle::SymbolicLlm { map } => {
            let es = map.entity_sym(entity);
            if !singletons.is_empty() {
                let methods: Vec<String> = singletons
                    .iter()
                    .map(|c| {
                        let lab = capability_method_label_kebab(c);
                        let ms = map.method_sym(entity, lab.as_str());
                        format!("{es}.{ms}()")
                    })
                    .collect();
                format!(
                    "Empty `()` after `{es}` is not valid. Use `{es}(<id>)` with an id, or a pathless singleton method shown in the prompt: {}.",
                    methods.join(", ")
                )
            } else {
                format!(
                    "Put a non-empty id inside parentheses: `{es}(<id>)` (see the expression examples for the id field type)."
                )
            }
        }
    }
}

fn correction_ambiguous_zero_arity_method(
    _cgs: &CGS,
    entity: &str,
    label: &str,
    style: &FeedbackStyle<'_>,
) -> String {
    match style {
        FeedbackStyle::CanonicalDev => {
            format!(
                "ambiguous zero-arity method `{label}` on entity `{entity}` — multiple capabilities share this surface label; check the `;;` / legend lines in the prompt to pick the intended operation."
            )
        }
        FeedbackStyle::SymbolicLlm { map } => {
            let es = map.entity_sym(entity);
            let ms = map.method_sym(entity, label);
            format!(
                "ambiguous `{ms}` on `{es}` — multiple capabilities share this `m#`; use the `;;` line that matches your intent."
            )
        }
    }
}

fn correction_dotted_call_no_match(
    _cgs: &CGS,
    anchor_entity: &str,
    label: &str,
    style: &FeedbackStyle<'_>,
) -> String {
    match style {
        FeedbackStyle::CanonicalDev => {
            format!(
                "no `{label}(…)` create/update/delete/action matches this expression (check capability names in the prompt)"
            )
        }
        FeedbackStyle::SymbolicLlm { map } => {
            let es = map.entity_sym(anchor_entity);
            let ms = map.method_sym(anchor_entity, label);
            format!(
                "no `{ms}(…)` create/update/delete/action matches this expression after `{es}` (check the `;;` lines in the prompt for parameters and capability shape)."
            )
        }
    }
}

fn correction_dotted_call_ambiguous(
    _cgs: &CGS,
    anchor_entity: &str,
    label: &str,
    style: &FeedbackStyle<'_>,
) -> String {
    match style {
        FeedbackStyle::CanonicalDev => {
            format!("ambiguous capability label `{label}` for entity `{anchor_entity}`")
        }
        FeedbackStyle::SymbolicLlm { map } => {
            let es = map.entity_sym(anchor_entity);
            let ms = map.method_sym(anchor_entity, label);
            format!(
                "ambiguous `{ms}` on `{es}` — multiple operations match; use `;;` to disambiguate."
            )
        }
    }
}

fn correction_dotted_create_ambiguous(
    _cgs: &CGS,
    anchor_entity: &str,
    label: &str,
    style: &FeedbackStyle<'_>,
) -> String {
    match style {
        FeedbackStyle::CanonicalDev => {
            format!("ambiguous create label `{label}`")
        }
        FeedbackStyle::SymbolicLlm { map } => {
            let es = map.entity_sym(anchor_entity);
            let ms = map.method_sym(anchor_entity, label);
            format!(
                "ambiguous create `{ms}` after `{es}` — multiple create lines in the prompt match; check `;;` and required scopes."
            )
        }
    }
}

fn correction_no_zero_arity_method(
    cgs: &CGS,
    entity: &str,
    label: &str,
    style: &FeedbackStyle<'_>,
) -> String {
    match style {
        FeedbackStyle::SymbolicLlm { map } => {
            correction_no_zero_arity_method_symbolic(cgs, map, entity, label)
        }
        FeedbackStyle::CanonicalDev => {
            correction_no_zero_arity_method_canonical(cgs, entity, label)
        }
    }
}

fn correction_no_zero_arity_method_symbolic(
    cgs: &CGS,
    map: &SymbolMap,
    entity: &str,
    label: &str,
) -> String {
    let es = map.entity_sym(entity);
    let attempt = map.method_sym(entity, label);
    if let Some(ent) = cgs.get_entity(entity) {
        if ent.relations.contains_key(label) {
            let rel = feedback_ident_symbol(map, label);
            return format!(
                "`{rel}` is a **relation** on `{es}`, not a method you call with empty `()` here. Use `{es}(id).{rel}` to reach related rows, or `{es}(id).{rel}{{…}}` with filters the schema allows. Use `.{rel}()` only when the prompt lists that `m#` with `;;` and no required parameters."
            );
        }
    }
    let mut pipeline: Vec<String> = Vec::new();
    for kind in [
        CapabilityKind::Action,
        CapabilityKind::Update,
        CapabilityKind::Delete,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if capability_is_zero_arity_invoke(cap) {
                let seg = capability_path_method_segment(cap);
                pipeline.push(map.method_sym(entity, seg.as_str()));
            }
        }
    }
    pipeline.sort();
    pipeline.dedup();
    if !pipeline.is_empty() {
        let full = pipeline.join("`, `");
        return format!(
            "No empty-`()` method matching `{attempt}` after `{es}`. The prompt lists these method symbols for `{es}`: `{full}`. Match the `m#` token from those lines (use `e#(id)` when the method needs a target id)."
        );
    }
    if matches!(label, "rename" | "update" | "patch") {
        let ups = cgs.find_capabilities(entity, CapabilityKind::Update);
        if !ups.is_empty() {
            let kb: Vec<String> = ups
                .iter()
                .map(|c| map.method_sym(entity, capability_path_method_segment(c).as_str()))
                .collect();
            let listed = kb.join("`, `");
            return format!(
                "`{attempt}` is not a path segment here. Use `{es}(id)` to identify the resource. To change fields, the prompt lists update operations `{listed}` — not `.{attempt}(...)` in the expression string."
            );
        }
    }
    format!(
        "No argument-free methods are defined after `.` on `{es}` for this path. Follow the `;;` lines in the prompt: use `e#(id).m#()` only when the prompt lists that `m#`."
    )
}

fn correction_no_zero_arity_method_canonical(cgs: &CGS, entity: &str, label: &str) -> String {
    if let Some(ent) = cgs.get_entity(entity) {
        if ent.relations.contains_key(label) {
            return format!(
                "`{label}` is a **relation** on `{entity}`, not a zero-arity method. Use `{entity}(id).{label}` to reach related rows, or `{entity}(id).{label}{{…}}` with filters the schema allows. Use `.{label}()` only when the prompt lists that name as a zero-arity method on `{entity}`."
            );
        }
    }
    let mut pipeline: Vec<String> = Vec::new();
    for kind in [
        CapabilityKind::Action,
        CapabilityKind::Update,
        CapabilityKind::Delete,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if capability_is_zero_arity_invoke(cap) {
                pipeline.push(capability_path_method_segment(cap).to_string());
            }
        }
    }
    pipeline.sort();
    pipeline.dedup();
    if !pipeline.is_empty() {
        let full = pipeline.join("`, `");
        return format!(
            "No zero-arity method `{label}` on `{entity}`. The prompt lists these zero-arity method names for `{entity}`: `{full}`. Match spelling and kebab-case (use `Entity(id)` when the method needs a target id)."
        );
    }
    if matches!(label, "rename" | "update" | "patch") {
        let ups = cgs.find_capabilities(entity, CapabilityKind::Update);
        if !ups.is_empty() {
            let kb: Vec<String> = ups
                .iter()
                .map(|c| capability_path_method_segment(c).to_string())
                .collect();
            let listed = kb.join("`, `");
            return format!(
                "`{label}` is not a path segment here. Use `{entity}(id)` to identify the resource. To change fields, the prompt names update operations `{listed}` — not `.{label}(...)` in the expression string."
            );
        }
    }
    "No zero-arity methods are defined for this entity on the left of `.`. Use kebab-case names with `()` when listed in the prompt, e.g. `User.get-me()` not `get_me`, or `Team(1).seats()` when the prompt lists `seats` for `Team`.".into()
}

fn correction_unknown_entity(
    cgs: &CGS,
    bad: &str,
    expr_line: &str,
    span_opt: Option<&(usize, usize)>,
    style: &FeedbackStyle<'_>,
) -> String {
    let (cands_canon, cands_disp) = entity_names_canonical_and_display(cgs, style);
    let empty = "This schema lists no entities.";
    if cands_canon.is_empty() {
        return format!("{empty}\n\nExpression: {expr_line}");
    }
    let phrase = levenshtein_replace_phrase_with_display(bad, &cands_canon, &cands_disp);
    let n = cands_disp.len();
    let full = cands_disp.join(", ");
    let head = format!(
        "Expression: {expr_line}\n\n`{bad}` is not an entity name in this schema. {phrase}\n\nValid entity symbols ({n}): {full}"
    );
    if span_opt.is_none() {
        let mut sorted_disp = cands_disp.clone();
        sorted_disp.sort();
        let eg = example_two_names(&sorted_disp);
        return format!("{head}\n\nFor example: {eg}.");
    }
    format!("{head}\n\nFor example: use the same spelling as in the expression examples (case-sensitive).")
}

fn correction_predicate_field(
    cgs: &CGS,
    field: &str,
    entity: &str,
    expr_line: &str,
    _span_start: usize,
    style: &FeedbackStyle<'_>,
) -> String {
    let es = entity_label_for_feedback(entity, style);
    let bad = ident_label_for_feedback(field, style);
    let (cands_canon, cands_disp) = predicate_field_canonical_and_display(cgs, entity, style);
    if cands_canon.is_empty() {
        return format!(
            "No filters or query parameters are defined for `{es}`.\n\nExpression: {expr_line}"
        );
    }
    let phrase = levenshtein_replace_phrase_with_display(field, &cands_canon, &cands_disp);
    let n = cands_disp.len();
    let full = cands_disp.join(", ");
    let pri = prioritize_query_field_examples(cands_canon.clone());
    let first_canon = pri
        .first()
        .cloned()
        .unwrap_or_else(|| cands_canon[0].clone());
    let first_disp = match style {
        FeedbackStyle::CanonicalDev => first_canon.clone(),
        FeedbackStyle::SymbolicLlm { map } => feedback_ident_symbol(map, &first_canon),
    };
    format!(
        "Expression: {expr_line}\n\n`{bad}` is not a valid filter name inside `{es}{{…}}`. {phrase}\n\nAllowed parameter symbols ({n}): {full}\n\nFor example: `{es}{{{first_disp}=…}}` using a name from the allowed list."
    )
}

fn correction_navigation_name(
    cgs: &CGS,
    field: &str,
    entity: &str,
    expr_line: &str,
    _span_start: usize,
    style: &FeedbackStyle<'_>,
) -> String {
    let es = entity_label_for_feedback(entity, style);
    let bad = ident_label_for_feedback(field, style);
    let (cands_canon, cands_disp) =
        navigable_fields_relations_and_pipeline_pairs(cgs, entity, style);
    if cands_canon.is_empty() {
        return format!("Nothing is defined after `.` on `{es}`.\n\nExpression: {expr_line}");
    }
    let phrase = levenshtein_replace_phrase_with_display(field, &cands_canon, &cands_disp);
    let n = cands_disp.len();
    let full = cands_disp.join(", ");
    let nav_canon = navigable_entityrefs_and_relations_only(cgs, entity);
    let ex_canon = if nav_canon.is_empty() {
        nav_example_for_entity(cgs, entity, &cands_canon)
    } else {
        nav_example_for_entity(cgs, entity, &nav_canon)
    };
    let ex_disp = display_nav_segment(cgs, entity, &ex_canon, style);
    let scalars = scalar_field_names_for_projection(cgs, entity);
    let proj = projection_bracket_example_feedback(&es, &scalars, style);
    let example_block = if scalars.is_empty() {
        format!("For example: `{es}(id).{ex_disp}`.")
    } else {
        format!(
            "For reading scalar fields: {proj}.\n\nTo navigate: `{es}(id).{ex_disp}` (relation, EntityRef, or `m#` listed for this entity in the prompt)."
        )
    };
    let tail_note = match style {
        FeedbackStyle::CanonicalDev => "(field, relation, or zero-arity method).",
        FeedbackStyle::SymbolicLlm { .. } => {
            "(field, relation, or `m#` listed for this entity in the prompt)."
        }
    };
    format!(
        "Expression: {expr_line}\n\n`{bad}` is not a valid name after `{es}.` {tail_note} {phrase}\n\nAllowed segment names ({n}): {full}\n\n{example_block}"
    )
}

fn correction_not_navigable(
    cgs: &CGS,
    field: &str,
    entity: &str,
    expr_line: &str,
    _span_start: usize,
    style: &FeedbackStyle<'_>,
) -> String {
    let es = entity_label_for_feedback(entity, style);
    let bad = ident_label_for_feedback(field, style);
    if field_is_declared_scalar(cgs, entity, field) {
        let scalars = scalar_field_names_for_projection(cgs, entity);
        let proj = projection_bracket_example_feedback(&es, &scalars, style);
        let (cands_canon, cands_disp) = entityrefs_relations_pairs(cgs, entity, style);
        let tail = if cands_canon.is_empty() {
            String::new()
        } else {
            let n = cands_disp.len();
            let full = cands_disp.join(", ");
            let ex_canon = nav_example_for_entity(cgs, entity, &cands_canon);
            let ex_disp = display_nav_segment(cgs, entity, &ex_canon, style);
            format!(
                "\n\nEntityRef or relation on `{es}` ({n}): {full}\n\nExample nav: `{es}(id).{ex_disp}`."
            )
        };
        return format!(
            "Expression: {expr_line}\n\n`{bad}` is a field on `{es}`; use projection `[…]` after `Get`, not `{es}.{bad}`.\n\nFor example: {proj}{tail}",
        );
    }
    let (cands_canon, cands_disp) = entityrefs_relations_pairs(cgs, entity, style);
    if cands_canon.is_empty() {
        return format!(
            "`{es}` has no EntityRef fields or relations to follow with `.`.\n\nExpression: {expr_line}"
        );
    }
    let phrase = levenshtein_replace_phrase_with_display(field, &cands_canon, &cands_disp);
    let n = cands_disp.len();
    let full = cands_disp.join(", ");
    let ex_canon = nav_example_for_entity(cgs, entity, &cands_canon);
    let ex_disp = display_nav_segment(cgs, entity, &ex_canon, style);
    let scalars = scalar_field_names_for_projection(cgs, entity);
    let proj = projection_bracket_example_feedback(&es, &scalars, style);
    let example_block = if scalars.is_empty() {
        format!("For example: `{es}(id).{ex_disp}`.")
    } else {
        format!(
            "For reading scalar fields: {proj}.\n\nTo navigate: `{es}(id).{ex_disp}` (relation or EntityRef)."
        )
    };
    format!(
        "Expression: {expr_line}\n\n`{bad}` is not an EntityRef field or relation on `{es}`, so `.` cannot continue there. {phrase}\n\nAllowed ({n}): {full}\n\n{example_block}"
    )
}

fn correction_no_entity_ref_bridge(
    cgs: &CGS,
    target: &str,
    source: &str,
    style: &FeedbackStyle<'_>,
) -> String {
    let mut pivots: Vec<String> = Vec::new();
    if let Some(ent) = cgs.get_entity(target) {
        for (fname, field) in &ent.fields {
            let Ok(nv) = cgs.named_value_for_slot(field) else {
                continue;
            };
            if let FieldType::EntityRef { target: t } = &nv.field_type {
                pivots.push(match style {
                    FeedbackStyle::CanonicalDev => format!("{fname} (→ {})", t),
                    FeedbackStyle::SymbolicLlm { map } => {
                        let ps = feedback_ident_symbol(map, fname);
                        let ts = map.entity_sym(t.as_str());
                        format!("{ps} (→ {ts})")
                    }
                });
            }
        }
        for cap in cgs.find_capabilities(target, CapabilityKind::Query) {
            if let Some(fields) = cap.object_params() {
                for f in fields {
                    let Ok(nv) = cgs.named_value_for_slot(f) else {
                        continue;
                    };
                    if let FieldType::EntityRef { target: t } = &nv.field_type {
                        pivots.push(match style {
                            FeedbackStyle::CanonicalDev => {
                                format!("{} (→ {})", f.name, t)
                            }
                            FeedbackStyle::SymbolicLlm { map } => {
                                let ps = feedback_ident_symbol(map, f.name.as_str());
                                let ts = map.entity_sym(t.as_str());
                                format!("{ps} (→ {ts})")
                            }
                        });
                    }
                }
            }
        }
        for (rname, rel) in &ent.relations {
            pivots.push(match style {
                FeedbackStyle::CanonicalDev => {
                    format!("{rname} (→ {})", rel.target_resource)
                }
                FeedbackStyle::SymbolicLlm { map } => {
                    let rs = feedback_ident_symbol(map, rname.as_str());
                    let ts = map.entity_sym(rel.target_resource.as_str());
                    format!("{rs} (→ {ts})")
                }
            });
        }
    }
    pivots.sort();
    pivots.dedup();
    let list = pivots.join(", ");
    let n = pivots.len();
    let tgt = entity_label_for_feedback(target, style);
    if pivots.is_empty() {
        format!(
            "Reverse navigation `.^{source}` from `{tgt}` is not available (no link toward `{source}` in this schema)."
        )
    } else {
        format!(
            "For `.^{source}` from `{tgt}`, follow one link toward `{source}` ({n}): {list}\n\nFor example: expand the path using a field the prompt shows from `{tgt}` toward `{source}`."
        )
    }
}

/// Distances are computed on **canonical** names while the suggested spellings use **display**
/// strings (e.g. `m#` / `p#` for LLM corrections).
fn levenshtein_replace_phrase_with_display(
    needle: &str,
    canonical_candidates: &[String],
    display_candidates: &[String],
) -> String {
    assert_eq!(canonical_candidates.len(), display_candidates.len());
    let mut scored: Vec<(usize, String)> = canonical_candidates
        .iter()
        .zip(display_candidates.iter())
        .map(|(c, d)| (levenshtein(needle, c), d.clone()))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let min_d = scored[0].0;
    if min_d == 0 {
        return "That spelling matches an allowed name; check commas, `{{}}`, and `.` around the name."
            .to_string();
    }
    let at_min: Vec<&String> = scored
        .iter()
        .filter(|(d, _)| *d == min_d)
        .map(|(_, n)| n)
        .collect();
    if min_d <= LEV_ACCEPT {
        if at_min.len() == 1 {
            format!("Change it to `{}`.", at_min[0])
        } else {
            format!(
                "Use one of: {}.",
                at_min
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    } else {
        let near: Vec<&str> = scored
            .iter()
            .take(NEAREST_SHOW)
            .map(|(_, name)| name.as_str())
            .collect();
        format!("Closest names: {}.", near.join(", "))
    }
}

fn entity_label_for_feedback(entity: &str, style: &FeedbackStyle<'_>) -> String {
    match style {
        FeedbackStyle::CanonicalDev => entity.to_string(),
        FeedbackStyle::SymbolicLlm { map } => map.entity_sym(entity),
    }
}

/// Field / relation / parameter name as shown in **correction** text only — canonical when
/// [`FeedbackStyle::CanonicalDev`], `p#` when [`FeedbackStyle::SymbolicLlm`]. Raw [`TypeError`] /
/// parse `err.message()` stay canonical regardless.
fn ident_label_for_feedback(ident: &str, style: &FeedbackStyle<'_>) -> String {
    match style {
        FeedbackStyle::CanonicalDev => ident.to_string(),
        FeedbackStyle::SymbolicLlm { map } => feedback_ident_symbol(map, ident),
    }
}

fn entity_names(cgs: &CGS) -> Vec<String> {
    cgs.entities.keys().map(|e| e.to_string()).collect()
}

fn first_entity_name(cgs: &CGS) -> Option<String> {
    let mut n = entity_names(cgs);
    n.sort();
    n.into_iter().next()
}

fn example_id_in_parens(cgs: &CGS, style: &FeedbackStyle<'_>) -> String {
    match first_entity_name(cgs) {
        Some(e) => {
            let ed = entity_label_for_feedback(&e, style);
            format!(
                "Put a string or number id inside parentheses, e.g. `{ed}(\"1\")` or `{ed}(42)`."
            )
        }
        None => "Put a string or number id inside parentheses.".into(),
    }
}

fn example_two_names(names: &[String]) -> String {
    match names.len() {
        0 => "see the expression examples".to_string(),
        1 => format!("`{}`", names[0]),
        _ => format!("`{}` or `{}`", names[0], names[1]),
    }
}

/// Prefer common scope parameters (`team_id`, `list_id`, …) in the example so queries match typical prompt patterns.
fn prioritize_query_field_examples(mut names: Vec<String>) -> Vec<String> {
    let scope_order = [
        "team_id",
        "list_id",
        "view_id",
        "space_id",
        "folder_id",
        "task_id",
    ];
    names.sort();
    names.dedup();
    let mut out = Vec::new();
    for s in scope_order {
        if let Some(i) = names.iter().position(|x| x == s) {
            out.push(names.remove(i));
            if out.len() >= 2 {
                break;
            }
        }
    }
    names.sort();
    out.extend(names);
    out
}

/// Query shape example for **correction** text — canonical names in dev mode, `e#`/`p#` in symbolic mode.
fn example_query_filter_for_entity(
    cgs: &CGS,
    entity: &str,
    field_names: &[String],
    style: &FeedbackStyle<'_>,
) -> String {
    let ed = entity_label_for_feedback(entity, style);
    if field_names.is_empty() {
        return format!("use fields listed for `{ed}` in the prompt.");
    }
    let s = prioritize_query_field_examples(field_names.to_vec());
    if s.len() >= 2 {
        let (da, db) = match style {
            FeedbackStyle::CanonicalDev => (s[0].clone(), s[1].clone()),
            FeedbackStyle::SymbolicLlm { map } => (
                feedback_predicate_ident_symbol(cgs, map, entity, &s[0]),
                feedback_predicate_ident_symbol(cgs, map, entity, &s[1]),
            ),
        };
        format!("`{ed}{{{da}=…, {db}=…}}`")
    } else {
        let da = match style {
            FeedbackStyle::CanonicalDev => s[0].clone(),
            FeedbackStyle::SymbolicLlm { map } => {
                feedback_predicate_ident_symbol(cgs, map, entity, &s[0])
            }
        };
        format!("`{ed}{{{da}=…}}`")
    }
}

fn field_names_on(cgs: &CGS, entity: &str) -> Vec<String> {
    cgs.get_entity(entity)
        .map(|e| e.fields.keys().map(|k| k.as_str().to_string()).collect())
        .unwrap_or_default()
}

fn query_object_param_names(cgs: &CGS, entity: &str) -> Vec<String> {
    let mut names = Vec::new();
    for kind in [CapabilityKind::Query, CapabilityKind::Search] {
        for cap in cgs.find_capabilities(entity, kind) {
            if let Some(fields) = cap.object_params() {
                for f in fields {
                    names.push(f.name.clone());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn navigable_all_fields_and_relations(cgs: &CGS, entity: &str) -> Vec<String> {
    let Some(ent) = cgs.get_entity(entity) else {
        return Vec::new();
    };
    let mut v: Vec<String> = ent.fields.keys().map(|k| k.as_str().to_string()).collect();
    v.extend(ent.relations.keys().map(|k| k.as_str().to_string()));
    v.sort();
    v.dedup();
    v
}

fn navigable_entityrefs_and_relations_only(cgs: &CGS, entity: &str) -> Vec<String> {
    let Some(ent) = cgs.get_entity(entity) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for (k, f) in &ent.fields {
        if cgs
            .named_value_for_slot(f)
            .ok()
            .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }))
        {
            names.push(k.as_str().to_string());
        }
    }
    names.extend(ent.relations.keys().map(|r| r.as_str().to_string()));
    names.sort();
    names.dedup();
    names
}

fn is_zero_arity_pipeline_method_label(cgs: &CGS, entity: &str, label: &str) -> bool {
    for kind in [
        CapabilityKind::Action,
        CapabilityKind::Update,
        CapabilityKind::Delete,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if capability_is_zero_arity_invoke(cap)
                && capability_path_method_segment(cap).as_str() == label
            {
                return true;
            }
        }
    }
    false
}

/// Map a navigable segment (field, relation, or path method label) to LLM-facing display text.
fn display_nav_segment(
    cgs: &CGS,
    entity: &str,
    canonical_seg: &str,
    style: &FeedbackStyle<'_>,
) -> String {
    match style {
        FeedbackStyle::CanonicalDev => canonical_seg.to_string(),
        FeedbackStyle::SymbolicLlm { map } => {
            if is_zero_arity_pipeline_method_label(cgs, entity, canonical_seg) {
                map.method_sym(entity, canonical_seg)
            } else {
                feedback_ident_symbol(map, canonical_seg)
            }
        }
    }
}

/// Canonical + display lists for navigable segments after `e#.` / `Entity.`.
fn navigable_fields_relations_and_pipeline_pairs(
    cgs: &CGS,
    entity: &str,
    style: &FeedbackStyle<'_>,
) -> (Vec<String>, Vec<String>) {
    let mut canon: Vec<String> = navigable_all_fields_and_relations(cgs, entity);
    for kind in [
        CapabilityKind::Action,
        CapabilityKind::Update,
        CapabilityKind::Delete,
    ] {
        for cap in cgs.find_capabilities(entity, kind) {
            if capability_is_zero_arity_invoke(cap) {
                canon.push(capability_path_method_segment(cap).to_string());
            }
        }
    }
    canon.sort();
    canon.dedup();
    let disp: Vec<String> = canon
        .iter()
        .map(|c| display_nav_segment(cgs, entity, c, style))
        .collect();
    (canon, disp)
}

fn entityrefs_relations_pairs(
    cgs: &CGS,
    entity: &str,
    style: &FeedbackStyle<'_>,
) -> (Vec<String>, Vec<String>) {
    let canon = navigable_entityrefs_and_relations_only(cgs, entity);
    let disp: Vec<String> = canon
        .iter()
        .map(|c| display_nav_segment(cgs, entity, c, style))
        .collect();
    (canon, disp)
}

fn predicate_field_canonical_and_display(
    cgs: &CGS,
    entity: &str,
    style: &FeedbackStyle<'_>,
) -> (Vec<String>, Vec<String>) {
    let mut canon = field_names_on(cgs, entity);
    canon.extend(query_object_param_names(cgs, entity));
    canon.sort();
    canon.dedup();
    let disp: Vec<String> = canon
        .iter()
        .map(|n| match style {
            FeedbackStyle::CanonicalDev => n.clone(),
            FeedbackStyle::SymbolicLlm { map } => {
                feedback_predicate_ident_symbol(cgs, map, entity, n)
            }
        })
        .collect();
    (canon, disp)
}

fn entity_names_canonical_and_display(
    cgs: &CGS,
    style: &FeedbackStyle<'_>,
) -> (Vec<String>, Vec<String>) {
    let mut canon = entity_names(cgs);
    canon.sort();
    let disp: Vec<String> = canon
        .iter()
        .map(|n| match style {
            FeedbackStyle::CanonicalDev => n.clone(),
            FeedbackStyle::SymbolicLlm { map } => map.entity_sym(n),
        })
        .collect();
    (canon, disp)
}

fn projection_bracket_example_feedback(
    entity_display: &str,
    scalars: &[String],
    style: &FeedbackStyle<'_>,
) -> String {
    match scalars.len() {
        0 => format!("`{entity_display}(id)[name,status]`"),
        1 => {
            let s0 = match style {
                FeedbackStyle::CanonicalDev => scalars[0].clone(),
                FeedbackStyle::SymbolicLlm { map } => feedback_ident_symbol(map, &scalars[0]),
            };
            format!("`{entity_display}(id)[{s0}]`")
        }
        _ => {
            let s0 = match style {
                FeedbackStyle::CanonicalDev => scalars[0].clone(),
                FeedbackStyle::SymbolicLlm { map } => feedback_ident_symbol(map, &scalars[0]),
            };
            let s1 = match style {
                FeedbackStyle::CanonicalDev => scalars[1].clone(),
                FeedbackStyle::SymbolicLlm { map } => feedback_ident_symbol(map, &scalars[1]),
            };
            format!("`{entity_display}(id)[{s0},{s1}]`")
        }
    }
}

/// Prefer a relation or EntityRef segment for "For example", not a zero-arity method name.
fn nav_example_for_entity(cgs: &CGS, entity: &str, cands: &[String]) -> String {
    for c in cands {
        if !is_zero_arity_pipeline_method_label(cgs, entity, c) {
            return c.clone();
        }
    }
    cands.first().cloned().unwrap_or_default()
}

fn field_is_declared_scalar(cgs: &CGS, entity: &str, field: &str) -> bool {
    cgs.get_entity(entity)
        .and_then(|e| e.fields.get(field))
        .map(|f| {
            !cgs.named_value_for_slot(f)
                .ok()
                .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }))
        })
        .unwrap_or(false)
}

fn scalar_field_names_for_projection(cgs: &CGS, entity: &str) -> Vec<String> {
    let Some(ent) = cgs.get_entity(entity) else {
        return Vec::new();
    };
    let mut names: Vec<String> = ent
        .fields
        .iter()
        .filter_map(|(k, f)| {
            if cgs
                .named_value_for_slot(f)
                .ok()
                .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }))
            {
                None
            } else {
                Some(k.as_str().to_string())
            }
        })
        .collect();
    names.sort();
    prioritize_projection_scalars(names)
}

/// Prefer human-meaningful scalars in `[…]` examples (schema-agnostic order).
fn prioritize_projection_scalars(mut names: Vec<String>) -> Vec<String> {
    let preferred = [
        "name",
        "status",
        "title",
        "id",
        "description",
        "priority",
        "due_date",
        "archived",
        "date_created",
    ];
    let mut out = Vec::new();
    for p in preferred {
        if let Some(i) = names.iter().position(|x| x == p) {
            out.push(names.remove(i));
            if out.len() >= 2 {
                break;
            }
        }
    }
    names.sort();
    out.extend(names);
    out
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in dp.iter_mut().enumerate().take(a.len() + 1) {
        row[0] = i;
    }
    if let Some(row0) = dp.first_mut() {
        for (j, cell) in row0.iter_mut().enumerate().take(b.len() + 1) {
            *cell = j;
        }
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[a.len()][b.len()]
}

fn suggest_closest(candidates: &[String], needle: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for c in candidates {
        let d = levenshtein(needle, c);
        if d > 0 && d <= 3 && (best.is_none() || d < best.as_ref().unwrap().0) {
            best = Some((d, c.clone()));
        }
    }
    best.map(|(_, s)| s)
}

/// Entity resource fields plus query-capability parameter names (same union as typecheck for predicates).
fn field_names_list(cgs: &CGS, entity: &str) -> Vec<String> {
    let mut names = field_names_on(cgs, entity);
    names.extend(query_object_param_names(cgs, entity));
    names.sort();
    names.dedup();
    names
}

fn field_names_full_hint(
    cgs: &CGS,
    entity: &str,
    names: &[String],
    style: &FeedbackStyle<'_>,
) -> String {
    let el = entity_label_for_feedback(entity, style);
    if names.is_empty() {
        return format!("`{el}` has no declared fields");
    }
    let mut sorted: Vec<_> = names.to_vec();
    sorted.sort();
    let n = sorted.len();
    let joined = match style {
        FeedbackStyle::CanonicalDev => sorted.join(", "),
        FeedbackStyle::SymbolicLlm { map } => sorted
            .iter()
            .map(|s| feedback_predicate_ident_symbol(cgs, map, entity, s))
            .collect::<Vec<_>>()
            .join(", "),
    };
    format!("Fields on `{el}` ({n}): {joined}")
}

fn relations_full_hint(entity: &str, rels: &[String], style: &FeedbackStyle<'_>) -> String {
    let el = entity_label_for_feedback(entity, style);
    if rels.is_empty() {
        return format!("`{el}` has no declared relations");
    }
    let mut sorted: Vec<_> = rels.to_vec();
    sorted.sort();
    let n = sorted.len();
    let joined = match style {
        FeedbackStyle::CanonicalDev => sorted.join(", "),
        FeedbackStyle::SymbolicLlm { map } => sorted
            .iter()
            .map(|s| feedback_ident_symbol(map, s))
            .collect::<Vec<_>>()
            .join(", "),
    };
    format!("Relations on `{el}` ({n}): {joined}")
}

fn example_entity_pair_line_for_feedback(cgs: &CGS, style: &FeedbackStyle<'_>) -> String {
    let mut cands = entity_names(cgs);
    cands.sort();
    match cands.len() {
        0 => "see the expression examples".to_string(),
        1 => format!("`{}`", entity_label_for_feedback(&cands[0], style)),
        _ => format!(
            "`{}` or `{}`",
            entity_label_for_feedback(&cands[0], style),
            entity_label_for_feedback(&cands[1], style)
        ),
    }
}

/// Map a Plasm type-check error to a structured [`StepError`].
pub fn render_type_error(err: &TypeError, cgs: &CGS) -> StepError {
    render_type_error_with_feedback(err, cgs, FeedbackStyle::CanonicalDev)
}

/// Same as [`render_type_error`], with [`FeedbackStyle`] for symbolic eval prompts.
///
/// The structured **`error`** field is always the canonical [`TypeError`] string for logs. Only
/// **`correction`** uses `e#` / `p#` / `m#` when `style` is [`FeedbackStyle::SymbolicLlm`].
pub fn render_type_error_with_feedback(
    err: &TypeError,
    cgs: &CGS,
    style: FeedbackStyle<'_>,
) -> StepError {
    let error = err.to_string();

    match err {
        TypeError::FieldNotFound { field, entity } => {
            let names = field_names_list(cgs, entity);
            let mut extra = Vec::new();
            if let Some(s) = suggest_closest(&names, field) {
                let disp = ident_label_for_feedback(&s, &style);
                extra.push(format!("Close spelling: `{disp}`"));
            }
            extra.push(field_names_full_hint(cgs, entity, &names, &style));
            let example = example_query_filter_for_entity(cgs, entity, &names, &style);
            let bad = ident_label_for_feedback(field, &style);
            let ent = entity_label_for_feedback(entity, &style);
            let correction = append_correction_lines(
                format!(
                    "Change `{bad}` to a field or query-parameter name declared for `{ent}` in the schema (resource fields and `{{…}}` filter names).\n\nFor example: {example}"
                ),
                extra,
            );
            StepError::type_correction(correction, error)
        }
        TypeError::RelationNotFound { relation, entity } => {
            let mut extra = Vec::new();
            if let Some(ent) = cgs.get_entity(entity) {
                let rels: Vec<String> = ent
                    .relations
                    .keys()
                    .map(|k| k.as_str().to_string())
                    .collect();
                if let Some(s) = suggest_closest(&rels, relation) {
                    let disp = ident_label_for_feedback(&s, &style);
                    extra.push(format!("Close spelling: `{disp}`"));
                }
                extra.push(relations_full_hint(entity, &rels, &style));
            }
            let rel = ident_label_for_feedback(relation, &style);
            let ent = entity_label_for_feedback(entity, &style);
            let correction = append_correction_lines(
                format!(
                    "Change `{rel}` to a relation name declared on `{ent}` in the schema.\n\nFor example: `{ent}(id).<relation>` using a name from the list below."
                ),
                extra,
            );
            StepError::type_correction(correction, error)
        }
        TypeError::EntityNotFound { entity } => {
            let mut names = entity_names(cgs);
            names.sort();
            let mut extra = Vec::new();
            if let Some(s) = suggest_closest(&names, entity) {
                let disp = entity_label_for_feedback(&s, &style);
                extra.push(format!("Close spelling: `{disp}`"));
            }
            let n = names.len();
            let joined = match &style {
                FeedbackStyle::CanonicalDev => names.join(", "),
                FeedbackStyle::SymbolicLlm { map } => names
                    .iter()
                    .map(|name| map.entity_sym(name))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            extra.push(format!("Valid entities ({n}): {joined}"));
            let eg = example_entity_pair_line_for_feedback(cgs, &style);
            let bad = entity_label_for_feedback(entity, &style);
            let correction = append_correction_lines(
                format!(
                    "Replace `{bad}` with an entity name from the expression examples (case-sensitive).\n\nFor example: {eg}."
                ),
                extra,
            );
            StepError::type_correction(correction, error)
        }
        TypeError::RefKeyMismatch { entity, message } => {
            let ent = entity_label_for_feedback(entity, &style);
            let correction = format!("Get on `{ent}`: {message}");
            StepError::type_correction(correction, error)
        }
        TypeError::ChainTargetMissingGet {
            source_entity,
            selector,
            target_entity,
        } => {
            let se = entity_label_for_feedback(source_entity, &style);
            let te = entity_label_for_feedback(target_entity, &style);
            let sel = ident_label_for_feedback(selector, &style);
            let correction = format!(
                "`{se}(…).{sel}` points to `{te}`, but `{te}` has no Get in this schema, so auto-fetch after the chain is not available.\n\n\
Change the plan: fetch `{te}` another way, or extend the schema with a Get for `{te}`.\n\n\
For example: `{te}(<id>)` when you already know the id, instead of relying on `{se}(…).{sel}` to load it."
            );
            StepError::type_correction(correction, error)
        }
        TypeError::IncompatibleOperator {
            field,
            op,
            field_type,
        } => {
            let fd = ident_label_for_feedback(field, &style);
            let correction = format!(
                "Use an operator allowed for `{fd}` ({field_type}). `{op}` is not valid for that type.\n\nFor example: `=` for equality where the examples show comparisons."
            );
            StepError::type_correction(correction, error)
        }
        TypeError::IncompatibleValue {
            field,
            value_type,
            field_type,
        } => {
            let fd = ident_label_for_feedback(field, &style);
            let mut correction = format!(
                "Change the value for `{fd}` to match `{field_type}` (you used something like {value_type}).\n\nFor example: a quoted string for text, or a number for numeric fields."
            );
            if value_type == "object" && matches!(field_type.as_str(), "String" | "Blob") {
                correction.push_str(
                    "\n\nIf you passed a **program binding** created by a row-to-text template (`label = rows[p#,…] <<TAG … TAG`), that binding is a row object with a **`content`** field. Use **`binding.content`** for plain string / body parameters—not the bare binding name.",
                );
            }
            StepError::type_correction(correction, error)
        }
        TypeError::DomainPlaceholderLiteral {
            field,
            expected_type,
            description,
        } => {
            let fd = ident_label_for_feedback(field, &style);
            let mut correction = format!(
                "`$` is only a teaching placeholder in the examples — never emit it in your step. Replace the fill-in for `{fd}` with a real API value.\n\nExpected: {expected_type}."
            );
            if let Some(d) = description {
                let d = d.trim();
                if !d.is_empty() {
                    correction.push_str(&format!("\n\nWhat this field is: {d}"));
                }
            }
            StepError::type_correction(correction, error)
        }
        TypeError::CapabilityNotFound { capability } => {
            let cap = ident_label_for_feedback(capability, &style);
            let correction = format!(
                "`{cap}` is not a capability name in this schema for the loaded API. Use an id listed in SCHEMA.\n\nFor example: match the exact spelling from the capability table in the prompt."
            );
            StepError::type_correction(correction, error)
        }
        TypeError::InputRequired { capability } => {
            let cap = ident_label_for_feedback(capability, &style);
            let correction = format!(
                "Fill in the request body the schema requires for `{cap}`.\n\nFor example: see SCHEMA in the prompt for required fields."
            );
            StepError::type_correction(correction, error)
        }
        TypeError::RecursiveError { relation, source } => {
            let inner = render_type_error_with_feedback(source, cgs, style.clone());
            let mut nested = error;
            if let Some(ref e) = inner.error {
                nested.push('\n');
                nested.push_str(e);
            }
            let rel = ident_label_for_feedback(relation, &style);
            let correction = format!(
                "Fix the inner part first (relation `{rel}`): {}",
                inner.correction
            );
            StepError::type_correction(correction, nested)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr_correction::RecoveryHint;
    use crate::expr_parser;
    use crate::loader;
    use crate::query_resolve::QueryCapabilityResolveError;
    use crate::step_semantics::StepErrorCategory;
    use crate::symbol_tuning::SymbolMap;

    #[test]
    fn format_recovery_hints_symbolic_collapses_lexicon_options() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let map = SymbolMap::build(&cgs, &["Pet"]);
        let hints = vec![RecoveryHint::AmbiguousFieldCandidates {
            entity: "Pet".into(),
            option_expressions: vec!["Pet{status=available}".into()],
        }];
        let s = format_recovery_hints(&hints, FeedbackStyle::SymbolicLlm { map: &map });
        assert!(
            s.starts_with("Pick one: "),
            "expected Pick one prefix, got: {s}"
        );
        assert!(
            !s.contains("Pet"),
            "symbolic hints must not mix canonical entity names: {s}"
        );
        assert!(
            s.contains("e1{") && s.contains("available"),
            "expected collapsed entity token in hint: {s}"
        );
    }

    #[test]
    fn query_resolve_no_matching_scope_symbolic_appends_actionable_hint() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let map = SymbolMap::build(&cgs, &["Pet"]);
        let err = QueryCapabilityResolveError::NoMatchingCapability {
            entity: "Pet".to_string(),
            message: "every query capability for this entity requires scope parameters in the predicate; include every required scope field so one query row can match (partial scope is not enough). Available: list_query".to_string(),
        };
        let s =
            render_query_resolve_error_for_feedback(&err, FeedbackStyle::SymbolicLlm { map: &map });
        assert!(s.starts_with("Query capability resolution failed: "), "{s}");
        assert!(
            s.contains("each remaining scope key") && s.contains("another entity"),
            "expected generic scope follow-up for partial-scope / wrong-entity mistakes: {s}"
        );
        assert!(
            !s.contains("Available:") && !s.contains("list_query"),
            "must strip capability list from LLM feedback: {s}"
        );
    }

    #[test]
    fn feedback_ident_symbol_uses_canonical_name_when_symbol_is_ambiguous() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let map = SymbolMap::build(
            &cgs,
            &[
                "CaptureItem",
                "CaptureSession",
                "DetectedQuestion",
                "Meeting",
                "PipelineEvent",
                "PipelineSnapshot",
                "Profile",
                "PromptRun",
                "RecordedContent",
            ],
        );
        assert_eq!(
            feedback_ident_symbol(&map, "workers"),
            map.ident_sym_entity_field("PipelineSnapshot", "workers")
        );
        assert_eq!(feedback_ident_symbol(&map, "id"), "id");
    }

    #[test]
    fn ambiguous_field_candidates_do_not_collapse_ambiguous_id_to_wrong_p_symbol() {
        let dir = std::path::Path::new("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let map = SymbolMap::build(
            &cgs,
            &[
                "CaptureItem",
                "CaptureSession",
                "DetectedQuestion",
                "Meeting",
                "PipelineEvent",
                "PipelineSnapshot",
                "Profile",
                "PromptRun",
                "RecordedContent",
            ],
        );
        let hints = vec![RecoveryHint::AmbiguousFieldCandidates {
            entity: "Profile".into(),
            option_expressions: vec!["Profile{id=1}".into()],
        }];
        let s = format_recovery_hints(&hints, FeedbackStyle::SymbolicLlm { map: &map });
        assert!(
            s.contains("e7{") && s.contains("id=1"),
            "ambiguous `id` should stay canonical inside collapsed recovery examples: {s}"
        );
        assert!(
            !s.contains("p47=1") && !s.contains("p48=1"),
            "ambiguous `id` must not collapse to a misleading p# in recovery examples: {s}"
        );
    }

    #[test]
    fn parse_error_expected_identifier_generic_when_markdown_like_without_resolved_slot() {
        let cgs = crate::CGS::new();
        let work = "Issue(1).update(description=## Scope, x=1)";
        let err = expr_parser::ParseError {
            kind: expr_parser::ParseErrorKind::ExpectedIdentifier,
            offset: work.find("##").expect("## in fixture"),
        };
        let se = render_parse_error(&err, work, &cgs);
        assert!(
            !se.correction.contains("raw block"),
            "without CGS resolution for the parameter, omit raw-block emphasis: {}",
            se.correction
        );
        assert!(
            se.correction.contains("quoted") || se.correction.contains("escape"),
            "correction={}",
            se.correction
        );
    }

    #[test]
    fn parse_error_expected_identifier_suggests_raw_block_when_markdown_slot_resolves() {
        let dir = std::path::Path::new("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let work = "Issue(1).update(description=## Scope, x=1)";
        let err = expr_parser::ParseError {
            kind: expr_parser::ParseErrorKind::ExpectedIdentifier,
            offset: work.find("##").expect("## in fixture"),
        };
        let se = render_parse_error(&err, work, &cgs);
        assert!(
            se.correction.contains("<<TAG") && se.correction.contains("`TAG`"),
            "correction={}",
            se.correction
        );
    }

    #[test]
    fn parse_error_unterminated_string_tagged_heredoc_names_tag() {
        let cgs = crate::CGS::new();
        let work = "e1.m1(p2=<<NOTE\nhello\n";
        let err = expr_parser::ParseError {
            kind: expr_parser::ParseErrorKind::UnterminatedString,
            offset: work.len(),
        };
        let se = render_parse_error(&err, work, &cgs);
        assert!(
            se.correction.contains("Tagged heredoc") && se.correction.contains("<<NOTE"),
            "correction={}",
            se.correction
        );
        assert!(
            se.correction.contains("`NOTE`") && se.correction.contains("hard newline"),
            "expected explicit tag and newline hint in correction: {}",
            se.correction
        );
        assert!(
            se.correction.contains("first") && se.correction.contains("opaque"),
            "expected tag-collision hint for unterminated tagged heredoc: {}",
            se.correction
        );
    }

    #[test]
    fn parse_error_unterminated_string_after_untagged_opener_suggests_tagged_migration() {
        let cgs = crate::CGS::new();
        let work = "Pet{status=<<\nhello\n";
        let err = expr_parser::ParseError {
            kind: expr_parser::ParseErrorKind::UnterminatedString,
            offset: work.len(),
        };
        let se = render_parse_error(&err, work, &cgs);
        assert!(
            se.correction.contains("tagged") && se.correction.contains(">>>"),
            "correction={}",
            se.correction
        );
    }

    #[test]
    fn parse_error_correction_not_raw_diagnostic() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let err = expr_parser::parse("Pett{}", &cgs).unwrap_err();
        let se = render_parse_error(&err, "Pett{}", &cgs);
        assert_eq!(se.category, StepErrorCategory::Parse);
        assert!(se.error.is_some());
        assert!(
            se.correction.contains("Pet") || se.correction.to_lowercase().contains("pet"),
            "correction={}",
            se.correction
        );
        assert!(
            !se.correction.contains("input:"),
            "LLM correction should not echo raw input line: {}",
            se.correction
        );
    }

    #[test]
    fn type_error_field_not_found_hints_name_issue_and_full_field_list() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let err = crate::TypeError::FieldNotFound {
            field: "list_id".into(),
            entity: "Pet".into(),
        };
        let se = render_type_error(&err, &cgs);
        assert!(
            se.correction.contains("list_id") && se.correction.contains("Pet"),
            "correction={}",
            se.correction
        );
        assert!(
            se.error
                .as_ref()
                .is_some_and(|d| d.contains("list_id") && d.contains("not found")),
            "error={:?}",
            se.error
        );
        let idx = se
            .correction
            .find("Fields on `Pet`")
            .expect("full field list in correction");
        let tail = &se.correction[idx..];
        assert!(tail.contains("name") && tail.contains("status"), "{tail}");
    }

    #[test]
    fn type_error_literal_dollar_placeholder_correction_includes_schema_hint() {
        let cgs = crate::CGS::new();
        let err = crate::TypeError::DomainPlaceholderLiteral {
            field: "workspace_id".into(),
            expected_type: "a real id or reference for `Workspace` (`$` in examples is only a stand-in, not a wire value)".into(),
            description: Some("Workspace to filter by.".into()),
        };
        let se = render_type_error_with_feedback(&err, &cgs, FeedbackStyle::CanonicalDev);
        assert!(
            se.correction.contains("teaching placeholder"),
            "correction={}",
            se.correction
        );
        assert!(se.correction.contains("workspace_id"));
        assert!(
            se.correction.contains("Workspace to filter"),
            "correction={}",
            se.correction
        );
    }

    #[test]
    fn type_error_incompatible_value_object_for_string_hints_bracket_render_content() {
        let cgs = crate::CGS::new();
        let err = crate::TypeError::IncompatibleValue {
            field: "plainBody".into(),
            value_type: "object".into(),
            field_type: "String".into(),
        };
        let se = render_type_error(&err, &cgs);
        assert!(
            se.correction.contains("row-to-text template") && se.correction.contains(".content"),
            "expected row-to-text / .content hint, correction={}",
            se.correction
        );
    }

    #[test]
    fn gmail_nav_typo_levenshtein_to_attachments() {
        let dir = std::path::Path::new("../../apis/gmail");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let err = expr_parser::parse("Message(1).awachment", &cgs).unwrap_err();
        let se = render_parse_error(&err, "Message(1).awachment", &cgs);
        assert!(
            se.correction.contains("attachments"),
            "expected suggestion toward attachments, got: {}",
            se.correction
        );
    }

    #[test]
    fn parse_correction_spaces_in_entity_id() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let expr = "Task(123 456)";
        let err = expr_parser::parse(expr, &cgs).unwrap_err();
        let se = render_parse_error(&err, expr, &cgs);
        assert!(
            se.correction.to_lowercase().contains("space"),
            "expected whitespace hint, got: {}",
            se.correction
        );
        assert!(
            !se.correction.contains("hex run"),
            "should not blame hex parsing when the issue is spaces: {}",
            se.correction
        );
    }

    #[test]
    fn predicate_unknown_field_explicit_correction() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let expr = r#"Member{space_id=Space(555555555)}"#;
        let err = expr_parser::parse(expr, &cgs).unwrap_err();
        let se = render_parse_error(&err, expr, &cgs);
        assert!(
            se.correction.contains("Member{space_id"),
            "correction={}",
            se.correction
        );
        assert!(
            se.correction.contains("task_id") && se.correction.contains("team_id"),
            "correction={}",
            se.correction
        );
        assert!(
            !se.correction.contains("query predicate"),
            "correction={}",
            se.correction
        );
    }

    #[test]
    fn symbolic_feedback_skips_zero_arity_and_kebab_jargon() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let (full, _) = crate::symbol_tuning::entity_slices_for_render(
            &cgs,
            crate::symbol_tuning::FocusSpec::All,
        );
        let map = crate::symbol_tuning::SymbolMap::build(&cgs, &full);
        let expr = "Pet(1).not-a-real-method()";
        let err = expr_parser::parse(expr, &cgs).unwrap_err();
        assert!(
            matches!(
                err.kind,
                expr_parser::ParseErrorKind::NoZeroArityMethod { .. }
            ),
            "expected NoZeroArityMethod, got {:?}",
            err.kind
        );
        let se = render_parse_error_with_feedback(
            &err,
            expr,
            expr,
            &cgs,
            FeedbackStyle::SymbolicLlm { map: &map },
        );
        let lower = se.correction.to_lowercase();
        assert!(!lower.contains("zero-arity"), "{}", se.correction);
        assert!(!lower.contains("kebab"), "{}", se.correction);
    }

    #[test]
    fn canonical_feedback_bad_zero_arity_method_keeps_dev_wording() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let expr = "Pet(1).not-a-real-method()";
        let err = expr_parser::parse(expr, &cgs).unwrap_err();
        let se = render_parse_error(&err, expr, &cgs);
        let lower = se.correction.to_lowercase();
        assert!(
            lower.contains("zero-arity") || lower.contains("kebab"),
            "expected dev terminology: {}",
            se.correction
        );
    }

    #[test]
    fn symbolic_not_field_or_relation_lists_segment_symbols() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let (full, _) = crate::symbol_tuning::entity_slices_for_render(
            &cgs,
            crate::symbol_tuning::FocusSpec::All,
        );
        let map = crate::symbol_tuning::SymbolMap::build(&cgs, &full);
        let expr = "Pet(1).not-a-valid-nav-segment";
        let err = expr_parser::parse(expr, &cgs).unwrap_err();
        assert!(
            matches!(
                err.kind,
                expr_parser::ParseErrorKind::NotFieldOrRelation { .. }
            ),
            "expected NotFieldOrRelation, got {:?}",
            err.kind
        );
        let se = render_parse_error_with_feedback(
            &err,
            expr,
            expr,
            &cgs,
            FeedbackStyle::SymbolicLlm { map: &map },
        );
        assert!(
            se.correction.contains("Allowed segment names"),
            "expected symbolic list heading: {}",
            se.correction
        );
        assert!(
            !se.correction.to_lowercase().contains("zero-arity"),
            "{}",
            se.correction
        );
    }

    #[test]
    fn symbolic_type_error_field_not_found_keeps_canonical_error_symbolic_correction() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = loader::load_schema_dir(dir).unwrap();
        let (full, _) = crate::symbol_tuning::entity_slices_for_render(
            &cgs,
            crate::symbol_tuning::FocusSpec::All,
        );
        let map = crate::symbol_tuning::SymbolMap::build(&cgs, &full);
        let err = crate::TypeError::FieldNotFound {
            field: "not_a_field".into(),
            entity: "Task".into(),
        };
        let se =
            render_type_error_with_feedback(&err, &cgs, FeedbackStyle::SymbolicLlm { map: &map });
        assert!(
            se.error
                .as_ref()
                .is_some_and(|e| { e.contains("not_a_field") && e.contains("Task") }),
            "error log should stay canonical: {:?}",
            se.error
        );
        assert!(
            !se.correction.contains("list_id"),
            "correction should use p# tokens, not canonical scope param names; correction={}",
            se.correction
        );
    }
}
