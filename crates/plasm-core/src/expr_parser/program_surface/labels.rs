//! Program labels and pipe-head catalog surface checks.

use super::errors::program_invalid_binding_label_error;

/// Whole binding names in these numbered symbol families are reserved.
pub const RESERVED_PROGRAM_SYMBOL_PREFIXES: &str = "emprv";

pub fn reserved_program_label_pattern() -> String {
    format!("[{RESERVED_PROGRAM_SYMBOL_PREFIXES}][0-9]+")
}

pub fn looks_like_domain_symbol(label: &str) -> bool {
    let mut chars = label.chars();
    matches!(chars.next(), Some(c) if RESERVED_PROGRAM_SYMBOL_PREFIXES.contains(c))
        && matches!(chars.next(), Some(c) if c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_digit())
}

/// Valid identifier for a program binding label (not `e1`/`p2`-style teaching symbols).
pub fn is_valid_program_label(label: &str) -> bool {
    let mut chars = label.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !looks_like_domain_symbol(label)
}

/// First identifier token in a pipe head (stops at `{`, `(`, `~`, `.`, etc.).
fn pipe_head_first_ident(head: &str) -> &str {
    let head = head.trim();
    head.char_indices()
        .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '_')
        .map(|(i, _)| &head[..i])
        .unwrap_or(head)
}

/// Pipe head uses catalog surface syntax (`e#`, `e#{…}`, `Entity(…)`, `Entity~"q"`, dotted chains).
pub fn pipe_head_has_catalog_surface_syntax(head: &str) -> bool {
    let head = head.trim();
    if head.is_empty() {
        return false;
    }
    let first = pipe_head_first_ident(head);
    if looks_like_domain_symbol(first) && first.starts_with('e') {
        return true;
    }
    let rest = head[first.len()..].trim_start();
    !rest.is_empty() && matches!(rest.as_bytes()[0], b'{' | b'(' | b'~' | b'.')
}

/// Parse-time pipe head: catalog-shaped surface or plain binding label.
pub fn validate_pipe_head_syntax(head: &str) -> Result<(), String> {
    let head = head.trim();
    if head.is_empty() {
        return Err("pipe head must not be empty".into());
    }
    if pipe_head_has_catalog_surface_syntax(head) {
        return Ok(());
    }
    if is_valid_program_label(head) {
        return Ok(());
    }
    Err(format!(
        "unknown pipe head `{head}`; use a catalog source (`e#{{…}} | …`) or a binding label (`rows | …`)"
    ))
}

pub fn validate_program_label(label: &str) -> Result<(), String> {
    if !is_valid_program_label(label) || matches!(label, "_" | "$" | "return") {
        return Err(program_invalid_binding_label_error(label));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn numbered_symbol_families_are_reserved(
            prefix in proptest::sample::select(RESERVED_PROGRAM_SYMBOL_PREFIXES.chars().collect::<Vec<_>>()),
            digits in "[0-9]{1,20}",
        ) {
            let reserved = format!("{prefix}{digits}");
            prop_assert!(validate_program_label(&reserved).is_err());
            let separated = format!("{prefix}_{digits}");
            prop_assert!(validate_program_label(&separated).is_ok());
            let descriptive = format!("items_{digits}");
            prop_assert!(validate_program_label(&descriptive).is_ok());
            let error = program_invalid_binding_label_error(&reserved);
            prop_assert!(error.contains(&reserved_program_label_pattern()));
        }
    }

    #[test]
    fn naming_contract_matches_shipped_teaching() {
        let teaching = crate::prompt_render::PLASM_TOOL_DESCRIPTION;
        assert!(teaching.contains("Program"));
        assert!(validate_program_label("items_1").is_ok());
        assert_eq!(
            teaching,
            include_str!("../../../../../packages/plasm-agent/src/prompts/assets/plasm_tool.txt")
        );
    }
}
