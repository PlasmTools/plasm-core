//! Taught left-hand seats for identity Get and identity-required mutators.
//!
//! Diagnostics and prerequisite footers must recite these seats. Pathless
//! `eN.mK(args)` is never the required form when the capability needs an
//! identity Get (same honesty class as `opaque_read_kind_invoke_note`).

use crate::schema::EntityDef;
use crate::CGS;

/// Identity-get hole in teaching exemplars: `e#(<id>)` / compound `wire=<id>`.
pub(crate) const TEACHING_ID_HOLE: &str = "<id>";

/// Generic capability / filter param hole: `wire=<wire>` (never bare `$`).
pub(crate) const TEACHING_PARAM_VALUE_PLACEHOLDER: &str = "<wire>";

/// Card-family teaching hole: `<` + nonempty ASCII alnum/underscore + `>`.
///
/// Teaching TSV may still **print** these. An executable program must fill them
/// (bound value / row field / string of that sort) — they are not identifiers
/// or literals (PLP-10).
pub(crate) fn is_teaching_angle_hole(token: &str) -> bool {
    let t = token.trim();
    let Some(inner) = t.strip_prefix('<').and_then(|rest| rest.strip_suffix('>')) else {
        return false;
    };
    !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Receiver the card already taught for an identity Get: parens or token-identity braces.
pub(crate) fn taught_get_identity_receiver(
    entity: &str,
    id_wire: &str,
    ent: Option<&EntityDef>,
    cgs: Option<&CGS>,
) -> String {
    if ent.is_some_and(|e| cgs.is_some_and(|g| e.teaches_token_identity_braces(g)))
        && !id_wire.is_empty()
    {
        format!("{entity}{{{id_wire}={TEACHING_PARAM_VALUE_PLACEHOLDER}}}")
    } else {
        format!("{entity}({TEACHING_ID_HOLE})")
    }
}

/// Invoke shape: `{eN(<id>).mK(...)}` or the token-brace receiver plus method.
pub(crate) fn taught_identity_mutator_invoke_seat(
    entity: &str,
    method: &str,
    id_wire: &str,
    ent: Option<&EntityDef>,
    cgs: Option<&CGS>,
) -> String {
    format!(
        "{}.{method}(...)",
        taught_get_identity_receiver(entity, id_wire, ent, cgs)
    )
}

/// Prerequisite / legend basename: `{eN(<id>).mK}` without dummy args.
pub(crate) fn taught_identity_mutator_basename(
    entity: &str,
    method: &str,
    id_wire: &str,
    ent: Option<&EntityDef>,
    cgs: Option<&CGS>,
) -> String {
    format!(
        "{}.{method}",
        taught_get_identity_receiver(entity, id_wire, ent, cgs)
    )
}

#[cfg(test)]
mod tests {
    use crate::loader::load_schema_dir;
    use std::path::PathBuf;

    #[test]
    fn token_identity_mutator_seat_uses_braces() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/session_token_get");
        let cgs = load_schema_dir(&dir).unwrap();
        let ent = cgs.get_entity("Wallet").unwrap();
        let seat = super::taught_identity_mutator_invoke_seat(
            "e1",
            "m2",
            "access_token",
            Some(ent),
            Some(&cgs),
        );
        assert_eq!(seat, "e1{access_token=<wire>}.m2(...)");
        assert!(
            !seat.starts_with("e1.m2"),
            "must not advertise pathless e1.m2 as the seat: {seat}"
        );
    }

    #[test]
    fn unary_identity_mutator_seat_uses_parens() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let Ok(cgs) = load_schema_dir(&dir) else {
            return;
        };
        let ent = cgs.get_entity("LangItem").unwrap();
        let seat =
            super::taught_identity_mutator_invoke_seat("e3", "m5", "id", Some(ent), Some(&cgs));
        assert_eq!(seat, "e3(<id>).m5(...)");
    }

    #[test]
    fn teaching_angle_hole_family_matches_card_glyphs() {
        assert!(super::is_teaching_angle_hole("<id>"));
        assert!(super::is_teaching_angle_hole("<wire>"));
        assert!(super::is_teaching_angle_hole("<query>"));
        assert!(super::is_teaching_angle_hole("<member>"));
        assert!(super::is_teaching_angle_hole("  <id>  "));
        assert!(!super::is_teaching_angle_hole("$"));
        assert!(!super::is_teaching_angle_hole("i1"));
        assert!(!super::is_teaching_angle_hole("< id >"));
        assert!(!super::is_teaching_angle_hole("a<b>"));
    }
}
