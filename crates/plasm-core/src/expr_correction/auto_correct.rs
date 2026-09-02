use crate::domain_lexicon::{tokens, DomainLexicon};
use crate::expr_parser::{self, predicate_surface};
use crate::{CapabilityKind, FieldType, CGS};

use super::RecoveryHint;

/// Outcome of attempting auto-correction of a failed expression.
#[derive(Debug, Clone)]
pub enum CorrectionOutcome {
    /// Every failed predicate resolved uniquely — rewritten expression.
    Corrected(String),
    /// All unrecognised predicates had zero lexicon matches (narrative noise) and
    /// were dropped; remaining predicates form a valid expression.
    Dropped(String),
    /// At least one predicate matched 2+ candidates — ambiguous. Do not guess.
    /// Carries structured hints; render with [`crate::error_render::format_recovery_hints`].
    Ambiguous { hints: Vec<RecoveryHint> },
    /// Corrector could not produce a valid expression.
    Uncorrectable,
}

/// Attempt deterministic correction of a failed expression.
///
/// Only handles `Entity{pred,...}` query forms. Returns `Uncorrectable` for
/// get-by-id, chain, singleton, or other expression shapes.
pub fn try_auto_correct(input: &str, lexicon: &DomainLexicon, cgs: &CGS) -> CorrectionOutcome {
    // Only correct Entity{...} forms
    let input = input.trim();
    let Some((entity_name, pred_body)) = predicate_surface::split_query_brace_form(input) else {
        return CorrectionOutcome::Uncorrectable;
    };

    // Entity must exist in CGS
    if cgs.get_entity(entity_name).is_none() {
        return CorrectionOutcome::Uncorrectable;
    }

    // Build list of all valid field/param names for this entity (for quick lookup)
    let valid_for_entity = collect_valid_names(cgs, entity_name);

    // Parse predicates with the same surface scan as the main parser (commas, heredocs, quotes).
    let raw_preds: Vec<predicate_surface::PredicateSurfaceClause> =
        match predicate_surface::parse_loose_query_predicate_body(pred_body) {
            Ok(p) => p,
            Err(_) => return CorrectionOutcome::Uncorrectable,
        };
    if raw_preds.is_empty() {
        return CorrectionOutcome::Uncorrectable;
    }

    let mut corrected_parts: Vec<String> = Vec::new();
    let mut made_any_change = false;
    let mut ambiguous_hints: Vec<RecoveryHint> = Vec::new();

    for pred in &raw_preds {
        if valid_for_entity.contains(pred.field.as_str()) {
            // Valid as-is — keep the original string
            corrected_parts.push(pred.raw.clone());
            continue;
        }

        // Field is not valid — consult lexicon
        let field_tokens = tokens(&pred.field);
        let candidates = lexicon.resolve_for_entity(&field_tokens, entity_name);

        match candidates.len() {
            0 => {
                // No lexicon match.
                if looks_like_entity_ref(&pred.value) {
                    // Value is an EntityRef — this is a scoping predicate, not narrative noise.
                    // Check if the entity has exactly ONE required scope (structural fallback)
                    // or multiple scopes (ambiguous — cannot guess).
                    let all_scopes = collect_all_scopes(cgs, entity_name);
                    match all_scopes.len() {
                        0 => {
                            // No scope at all — drop
                            made_any_change = true;
                        }
                        1 => {
                            // Exactly one scope — apply it (safe, unambiguous)
                            let (scope_field, scope_target) = &all_scopes[0];
                            let id = extract_id_from_value(&pred.value);
                            corrected_parts.push(format!("{scope_field}={scope_target}({id})"));
                            made_any_change = true;
                        }
                        _ => {
                            ambiguous_hints.push(RecoveryHint::AmbiguousScopes {
                                entity: entity_name.to_string(),
                                scope_options: all_scopes.clone(),
                            });
                        }
                    }
                } else {
                    // Scalar value with no lexicon match → narrative noise → drop
                    made_any_change = true;
                }
            }
            1 => {
                // Unique match — substitute field (and entity type in value if EntityRef)
                let entry = candidates[0];
                if let Some(canonical_field) = entry.field_name() {
                    let new_value =
                        if let Some(FieldType::EntityRef { target, .. }) = entry.field_type() {
                            // Extract the id from the original value (may be Entity(id) or bare)
                            let id = extract_id_from_value(&pred.value);
                            format!("{target}({id})")
                        } else {
                            pred.value.clone()
                        };
                    corrected_parts
                        .push(format!("{canonical_field}{op}{new_value}", op = pred.op,));
                    made_any_change = true;
                } else {
                    // Entry is an entity-level match — can't substitute a field from it
                    corrected_parts.push(pred.raw.clone());
                }
            }
            _ => {
                // Ambiguous — collect hint, do NOT apply any rewrite
                let candidate_list: Vec<String> = candidates
                    .iter()
                    .filter_map(|e| e.field_name())
                    .map(|f| {
                        if let Some(FieldType::EntityRef { target, .. }) = candidates
                            .iter()
                            .find(|e| e.field_name() == Some(f))
                            .and_then(|e| e.field_type())
                        {
                            format!("{f}={target}(id)")
                        } else {
                            f.to_string()
                        }
                    })
                    .collect();
                let option_expressions: Vec<String> = candidate_list
                    .iter()
                    .map(|c| format!("{entity_name}{{{c}}}"))
                    .collect();
                ambiguous_hints.push(RecoveryHint::AmbiguousFieldCandidates {
                    entity: entity_name.to_string(),
                    option_expressions,
                });
            }
        }
    }

    // If any field was ambiguous, return Ambiguous regardless of others
    if !ambiguous_hints.is_empty() {
        return CorrectionOutcome::Ambiguous {
            hints: ambiguous_hints,
        };
    }

    if !made_any_change {
        return CorrectionOutcome::Uncorrectable;
    }

    // Reconstruct
    let corrected = if corrected_parts.is_empty() {
        entity_name.to_string()
    } else {
        format!("{entity_name}{{{}}}", corrected_parts.join(", "))
    };

    // Validate by re-parsing
    if expr_parser::parse(&corrected, cgs).is_ok() {
        if raw_preds.len() > corrected_parts.len() {
            CorrectionOutcome::Dropped(corrected)
        } else {
            CorrectionOutcome::Corrected(corrected)
        }
    } else {
        CorrectionOutcome::Uncorrectable
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the id portion from a value like `Space(42)`, `"abc"`, or bare `42`.
fn extract_id_from_value(value: &str) -> String {
    let v = value.trim();
    // Entity(id) pattern
    if let Some(open) = v.find('(') {
        if v.ends_with(')') {
            return v[open + 1..v.len() - 1].to_string();
        }
    }
    // Quoted string or bare number
    v.trim_matches('"').trim_matches('\'').to_string()
}

/// Returns `true` if `value` matches the `Word(...)` EntityRef pattern.
fn looks_like_entity_ref(value: &str) -> bool {
    let v = value.trim();
    let Some(open) = v.find('(') else {
        return false;
    };
    // Must have content before '(' that is all alphabetic
    let prefix = &v[..open];
    !prefix.is_empty() && prefix.chars().all(|c| c.is_alphabetic()) && v.ends_with(')')
}

/// Collect all distinct required scope fields (EntityRef type) for `entity_name`.
fn collect_all_scopes(cgs: &CGS, entity_name: &str) -> Vec<(String, String)> {
    let mut scopes: Vec<(String, String)> = Vec::new();

    for cap in cgs.find_capabilities(entity_name, CapabilityKind::Query) {
        for f in cap.scope_params() {
            if !f.required {
                continue;
            }
            let Ok(nv) = f.named_value(cgs) else {
                continue;
            };
            if let FieldType::EntityRef { target, .. } = &nv.field_type {
                let entry = (f.name.clone(), target.to_string());
                if !scopes.contains(&entry) {
                    scopes.push(entry);
                }
            }
        }
    }

    scopes
}

/// Collect all field/param names valid for querying `entity_name` in this CGS.
fn collect_valid_names<'a>(cgs: &'a CGS, entity_name: &str) -> std::collections::HashSet<&'a str> {
    let mut names = std::collections::HashSet::new();
    if let Some(ent) = cgs.get_entity(entity_name) {
        for field in ent.fields.keys() {
            names.insert(field.as_str());
        }
    }
    for cap in cgs.find_capabilities(entity_name, CapabilityKind::Query) {
        for field in cap.selection_params() {
            names.insert(field.name.as_str());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_lexicon::DomainLexicon;

    #[test]
    fn extract_id_from_entity_ref() {
        assert_eq!(extract_id_from_value("Space(42424242)"), "42424242");
        assert_eq!(extract_id_from_value("Team(abc)"), "abc");
        assert_eq!(extract_id_from_value("42"), "42");
    }

    #[test]
    fn correction_with_clickup_schema() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(dir).unwrap();
        let lexicon = DomainLexicon::from_cgs(&cgs);

        // Webhook{space_id=Space(42)} → Webhook{team_id=Team(42)}
        let result = try_auto_correct("Webhook{space_id=Space(42424242)}", &lexicon, &cgs);
        match &result {
            CorrectionOutcome::Corrected(s) | CorrectionOutcome::Dropped(s) => {
                assert!(s.contains("team_id"), "expected team_id in: {s}");
                assert!(s.contains("Team"), "expected Team entity ref in: {s}");
            }
            other => panic!("expected Corrected/Dropped, got: {other:?}"),
        }

        // Goal{team_id=Team(x), readout_time=next_week} → drop readout_time
        let result = try_auto_correct(
            "Goal{team_id=Team(999888777), readout_time=next_week}",
            &lexicon,
            &cgs,
        );
        match &result {
            CorrectionOutcome::Dropped(s) | CorrectionOutcome::Corrected(s) => {
                assert!(s.contains("team_id"), "expected team_id in: {s}");
                assert!(
                    !s.contains("readout_time"),
                    "should have dropped readout_time"
                );
            }
            other => panic!("expected Corrected/Dropped, got: {other:?}"),
        }

        // Member{space_id=Space(x)} → ambiguous (team_id | list_id | task_id)
        let result = try_auto_correct("Member{space_id=Space(555555555)}", &lexicon, &cgs);
        match result {
            CorrectionOutcome::Ambiguous { hints } => {
                assert!(!hints.is_empty());
                match &hints[0] {
                    RecoveryHint::AmbiguousScopes {
                        entity,
                        scope_options,
                    } => {
                        assert_eq!(entity, "Member");
                        assert!(
                            scope_options.len() >= 2,
                            "expected multiple scope rows: {scope_options:?}"
                        );
                    }
                    other => panic!("expected AmbiguousScopes for Member, got: {other:?}"),
                }
            }
            other => panic!("expected Ambiguous for Member, got: {other:?}"),
        }
    }
}
