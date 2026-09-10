//! Compact enum Meaning for teaching-table `v#` rows.
//!
//! **Opaque linking doctrine** ([`gloss_opaque` go_opaque_v2](../../../../../scripts/appworld/cuga/ablation_offline/gloss_opaque/GATE_RESULT.md)):
//! English→token is load-bearing when tokens are opaque. Among sealed forms (H2 / HG / HX),
//! the most token-efficient pass was packed token+gloss without a duplicated token list.
//!
//! Rendered shape (when any gloss is present):
//! `enum · pending: awaiting settlement; approved: fully settled`
//!
//! Tokens-only (no glosses): `enum · pending | approved | denied`
//! — pipe separators (tokens-first); never `token=gloss` (`=` invites gloss-as-value).
//!
//! Gloss text must not contain reserved delimiters (`;` `|` `=` `‖`); the loader rejects them
//! via [`crate::value_domain::EnumMembership::try_new`].

use indexmap::IndexMap;

/// Max bytes per enum token gloss in teaching Meaning (token budget).
pub const ENUM_TOKEN_GLOSS_MAX_BYTES: usize = 48;

/// Format the Meaning cell body after the type label for an enum / multi_enum `v#` row.
pub fn format_enum_membership_meaning(
    tokens: &[String],
    glosses: Option<&IndexMap<String, String>>,
) -> String {
    if tokens.is_empty() {
        return String::new();
    }
    let use_gloss = glosses.is_some_and(|g| {
        tokens
            .iter()
            .any(|t| g.get(t).is_some_and(|s| !s.trim().is_empty()))
    });
    if use_gloss {
        let gmap = glosses.expect("checked");
        let parts: Vec<String> = tokens
            .iter()
            .map(
                |t| match gmap.get(t).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    Some(g) => {
                        let truncated = crate::utf8_trunc::truncate_utf8_bytes_with_ellipsis(
                            g,
                            ENUM_TOKEN_GLOSS_MAX_BYTES,
                        );
                        format!("{t}: {truncated}")
                    }
                    None => t.clone(),
                },
            )
            .collect();
        parts.join("; ")
    } else {
        tokens.join(" | ")
    }
}

/// Full `v#` Meaning for select/enum domains: `{type_label} · …`.
pub fn format_enum_value_domain_row_meaning(
    type_label: &str,
    tokens: &[String],
    glosses: Option<&IndexMap<String, String>>,
) -> String {
    let body = format_enum_membership_meaning(tokens, glosses);
    if body.is_empty() {
        type_label.to_string()
    } else {
        format!("{type_label} · {body}")
    }
}

/// Prefer CGS [`EnumMembership`](crate::value_domain::EnumMembership) when present; else fall back
/// to denormalized `allowed_values` on the ident (fixture / partial maps).
pub fn enum_meaning_from_registry_row(
    type_label: &str,
    value_registry_key: &str,
    allowed_values: Option<&[String]>,
    cgs: Option<&crate::schema::CGS>,
) -> Option<String> {
    if let Some(m) = cgs
        .and_then(|c| c.values.get(value_registry_key))
        .and_then(|nv| nv.domain.enum_membership.as_ref())
    {
        if !m.tokens().is_empty() {
            return Some(format_enum_value_domain_row_meaning(
                type_label,
                m.tokens(),
                m.glosses(),
            ));
        }
    }
    let tokens = allowed_values.filter(|v| !v.is_empty())?;
    Some(format_enum_value_domain_row_meaning(
        type_label, tokens, None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_only_uses_pipes() {
        let toks = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(
            format_enum_value_domain_row_meaning("enum", &toks, None),
            "enum · a | b | c"
        );
    }

    #[test]
    fn gloss_map_is_colon_packed_no_duplicate_token_list() {
        let toks = vec!["k7".into(), "m2".into(), "p9".into()];
        let mut g = IndexMap::new();
        g.insert("k7".into(), "awaiting action on ledger".into());
        g.insert("m2".into(), "fully closed on ledger".into());
        g.insert("p9".into(), "struck from ledger".into());
        let s = format_enum_value_domain_row_meaning("enum", &toks, Some(&g));
        assert_eq!(
            s,
            "enum · k7: awaiting action on ledger; m2: fully closed on ledger; p9: struck from ledger"
        );
        assert!(!s.contains(" | "));
        assert!(!s.contains('='));
        assert!(!s.contains('‖'));
    }
}
