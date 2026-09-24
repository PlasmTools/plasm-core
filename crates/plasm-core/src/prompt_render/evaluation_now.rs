//! PLP-9: when the language card teaches temporal slots, it must name evaluation `now`.

use chrono::{DateTime, SecondsFormat, Utc};

use super::gloss_dedup::FieldGlossMeaning;
use super::{TeachingFieldGloss, TeachingPromptBundle};

/// Non-executable language-card left column for the evaluation clock (not a program root).
pub const EVALUATION_NOW_EXPR: &str = "evaluation_now";

/// Meaning tail after the RFC3339 instant (domain-general; no harness calendar liturgy).
pub const EVALUATION_NOW_MEANING_SUFFIX: &str =
    "relative date/time phrases resolve against this instant";

/// RFC3339 UTC (`…Z`, seconds precision) for the taught evaluation clock.
pub fn format_evaluation_now_instant(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Full Meaning cell for the [`EVALUATION_NOW_EXPR`] row.
pub fn format_evaluation_now_teaching_meaning(now: DateTime<Utc>) -> String {
    format!(
        "{} · {EVALUATION_NOW_MEANING_SUFFIX}",
        format_evaluation_now_instant(now)
    )
}

/// True when any taught field-gloss row is a temporal profile (or leftover `date` type label).
pub fn teaching_bundle_teaches_temporal(bundle: &TeachingPromptBundle) -> bool {
    bundle
        .teaching_blocks
        .iter()
        .any(|b| b.field_gloss_rows.iter().any(field_gloss_teaches_temporal))
}

fn field_gloss_teaches_temporal(g: &TeachingFieldGloss) -> bool {
    if profile_token_is_temporal(g.field_type.as_str()) {
        return true;
    }
    if let FieldGlossMeaning::TypedField { type_label, .. } = &g.meaning {
        if profile_token_is_temporal(type_label) {
            return true;
        }
    }
    let head = g
        .description
        .trim()
        .split(|c: char| c == ' ' || c == '·' || c == '\t')
        .find(|s| !s.is_empty())
        .unwrap_or("");
    profile_token_is_temporal(head)
}

fn profile_token_is_temporal(name: &str) -> bool {
    let name = name.trim();
    crate::value_domain::ProfileId::from_type_name(name).is_some_and(|p| p.is_temporal())
        || name.eq_ignore_ascii_case("date")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt_render::gloss_dedup::GlossDescription;
    use crate::prompt_render::{
        EntityTeachingBlock, TeachingHeading, TeachingPromptBundle, TeachingPromptModel,
    };
    use chrono::TimeZone;

    fn gloss(symbol: &str, field_type: &str, description: &str) -> TeachingFieldGloss {
        TeachingFieldGloss {
            symbol: symbol.to_string(),
            field_type: field_type.to_string(),
            allowed_values: String::new(),
            description: description.to_string(),
            is_inline_union_summary: false,
            meaning: FieldGlossMeaning::OpaqueLegend {
                description: description.to_string(),
            },
            catalog_entry_id: String::new(),
            entity: String::new(),
            emit_identity: None,
        }
    }

    fn bundle_with_gloss(rows: Vec<TeachingFieldGloss>) -> TeachingPromptBundle {
        TeachingPromptBundle {
            teaching_blocks: vec![EntityTeachingBlock {
                row_type: None,
                heading: TeachingHeading {
                    description: String::new(),
                },
                field_gloss_rows: rows,
                teaching_rows: Vec::new(),
            }],
            model: TeachingPromptModel::default(),
        }
    }

    #[test]
    fn format_evaluation_now_instant_is_rfc3339_z() {
        let now = Utc.with_ymd_and_hms(2020, 1, 15, 12, 0, 0).unwrap();
        assert_eq!(format_evaluation_now_instant(now), "2020-01-15T12:00:00Z");
        assert_eq!(
            format_evaluation_now_teaching_meaning(now),
            "2020-01-15T12:00:00Z · relative date/time phrases resolve against this instant"
        );
    }

    #[test]
    fn detects_rfc3339_value_domain_gloss() {
        let bundle = bundle_with_gloss(vec![gloss("v3", "", "rfc3339")]);
        assert!(teaching_bundle_teaches_temporal(&bundle));
    }

    #[test]
    fn detects_iso8601_typed_field() {
        let mut g = gloss("start_date", "", "");
        g.meaning = FieldGlossMeaning::TypedField {
            type_label: "iso8601_date".to_string(),
            allowed_values: String::new(),
            description: GlossDescription::from_trimmed(""),
        };
        let bundle = bundle_with_gloss(vec![g]);
        assert!(teaching_bundle_teaches_temporal(&bundle));
    }

    #[test]
    fn non_temporal_gloss_is_not_a_clock_trigger() {
        let bundle = bundle_with_gloss(vec![gloss("v1", "string", "string · display name")]);
        assert!(!teaching_bundle_teaches_temporal(&bundle));
    }
}
