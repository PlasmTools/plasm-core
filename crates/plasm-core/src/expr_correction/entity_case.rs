use crate::CGS;

/// If `name` is not a valid entity but matches exactly one CGS entity under ASCII
/// case-folding, return that entity's canonical spelling.
pub fn resolve_entity_case_insensitive(cgs: &CGS, name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let mut matches: Vec<String> = cgs
        .entities
        .keys()
        .filter(|e| e.as_str().to_ascii_lowercase() == lower)
        .map(|e| e.to_string())
        .collect();
    matches.sort();
    if matches.len() == 1 {
        Some(matches.remove(0))
    } else {
        None
    }
}

/// Rewrite entity identifiers to canonical CGS casing when the match is unique
/// (ASCII case-insensitive). Touches: the **leading** entity token (`Pet{…}`,
/// `pet(1)`, `PET~""`) and every **`.^Target`** reverse-query segment.
///
/// Returns `None` when nothing changes.
pub fn try_normalize_entity_case(input: &str, cgs: &CGS) -> Option<String> {
    let bytes = input.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    while i < n && bytes[i].is_ascii_whitespace() {
        out.push(bytes[i] as char);
        i += 1;
    }

    if let Some((end, ident)) = take_ident_at(input, i) {
        out.push_str(&match_entity_case(cgs, ident));
        i = end;
    }

    while i < n {
        if i + 2 <= n && input.get(i..i + 2) == Some(".^") {
            out.push_str(".^");
            i += 2;
            while i < n && bytes[i].is_ascii_whitespace() {
                out.push(bytes[i] as char);
                i += 1;
            }
            if let Some((end, ident)) = take_ident_at(input, i) {
                out.push_str(&match_entity_case(cgs, ident));
                i = end;
                continue;
            }
        }
        let Some(ch) = input[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }

    (out != input).then_some(out)
}

fn match_entity_case(cgs: &CGS, ident: &str) -> String {
    if cgs.get_entity(ident).is_some() {
        return ident.to_string();
    }
    resolve_entity_case_insensitive(cgs, ident).unwrap_or_else(|| ident.to_string())
}

/// Byte index `i` must point at the start of an identifier (`[A-Za-z_]`…).
fn take_ident_at(s: &str, i: usize) -> Option<(usize, &str)> {
    let b = s.as_bytes();
    if i >= b.len() || (!b[i].is_ascii_alphabetic() && b[i] != b'_') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    Some((j, &s[i..j]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr_parser;

    #[test]
    fn normalize_entity_case_unique_match() {
        let dir = &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(dir).unwrap();
        let fixed = try_normalize_entity_case("pet{status=available}", &cgs).expect("case fix");
        assert!(fixed.starts_with("Pet{"), "got {fixed}");
        assert!(expr_parser::parse(&fixed, &cgs).is_ok());
        assert_eq!(
            resolve_entity_case_insensitive(&cgs, "PET").as_deref(),
            Some("Pet")
        );
    }

    #[test]
    fn normalize_entity_case_does_not_panic_on_unicode_outside_identifiers() {
        let cgs = crate::CGS::new();
        let s = "Task(9abcdef012345678).rename('Blocker — customer escalation')";
        let _ = try_normalize_entity_case(s, &cgs);
    }
}
