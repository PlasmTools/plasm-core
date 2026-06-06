//! Pack-time validation for host-owned `bind.*` template references.

use crate::error::SchemaError;
use std::collections::HashSet;

/// Closed allowlist of binding wire names plugins may reference in templates.
pub const HOST_BINDING_WIRES: &[&str] = &["catalog_http_origin"];

pub fn is_known_host_binding_wire(name: &str) -> bool {
    HOST_BINDING_WIRES.contains(&name)
}

/// Collect unknown `bind.<wire>` references in a Minijinja/CML template string.
pub fn unknown_bind_wire_refs(template: &str) -> Vec<String> {
    let known: HashSet<&str> = HOST_BINDING_WIRES.iter().copied().collect();
    let mut unknown = HashSet::new();
    let mut i = 0;
    let bytes = template.as_bytes();
    while i + 5 <= bytes.len() {
        if bytes[i..].starts_with(b"bind.") {
            let start = i + 5;
            let mut end = start;
            while end < bytes.len() {
                let b = bytes[end];
                if b.is_ascii_alphanumeric() || b == b'_' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > start {
                let wire = &template[start..end];
                if !known.contains(wire) {
                    unknown.insert(wire.to_string());
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    unknown.into_iter().collect()
}

pub fn validate_bind_wire_refs(template: &str, context: &str) -> Result<(), SchemaError> {
    let unknown = unknown_bind_wire_refs(template);
    if unknown.is_empty() {
        return Ok(());
    }
    Err(SchemaError::SchemaOverlayInvalid {
        detail: format!(
            "{context}: unknown bind reference(s) {} — allowed: {}",
            unknown
                .iter()
                .map(|w| format!("bind.{w}"))
                .collect::<Vec<_>>()
                .join(", "),
            HOST_BINDING_WIRES
                .iter()
                .map(|w| format!("bind.{w}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_known_bind_wire() {
        assert!(validate_bind_wire_refs(
            "{{ bind.catalog_http_origin }}/api",
            "test"
        )
        .is_ok());
    }

    #[test]
    fn rejects_unknown_bind_wire() {
        let err = validate_bind_wire_refs("{{ bind.evil_origin }}", "cap `x`").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("bind.evil_origin"));
    }

    #[test]
    fn is_known_host_binding_wire_matches_allowlist() {
        assert!(is_known_host_binding_wire("catalog_http_origin"));
        assert!(!is_known_host_binding_wire("evil_origin"));
    }
}
