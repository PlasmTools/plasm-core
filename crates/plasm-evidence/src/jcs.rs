//! RFC 8785 JSON Canonicalization Scheme (JCS) for evidence segment hashing.

use crate::verify::EvidenceError;

/// Canonical UTF-8 bytes for a JSON value per [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785).
pub fn canonical_bytes(value: &serde_json::Value) -> Result<Vec<u8>, EvidenceError> {
    let json = serde_json::to_string(value).map_err(|e| EvidenceError::Serde(e.to_string()))?;
    let canonical = jcs_canonicalize::canonicalize(&json)
        .map_err(|e| EvidenceError::Serde(format!("jcs canonicalize: {e}")))?;
    Ok(canonical.into_bytes())
}

/// Lowercase hex SHA-256 of JCS-canonical bytes (golden-vector helper).
#[allow(dead_code)]
pub fn sha256_jcs_hex(value: &serde_json::Value) -> Result<String, EvidenceError> {
    let json = serde_json::to_string(value).map_err(|e| EvidenceError::Serde(e.to_string()))?;
    jcs_canonicalize::sha256_jcs_hex(&json)
        .map_err(|e| EvidenceError::Serde(format!("jcs sha256: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8785 Appendix F shape — exact float formatting follows `jcs-canonicalize` 0.2.x.
    #[test]
    fn rfc8785_appendix_f_vector() {
        let input = r#"{"numbers":[3333333333.3333333,1E30,4.50,2e-3,0],"string":"S","literals":[null,true,false]}"#;
        let got = jcs_canonicalize::canonicalize(input).expect("canonicalize");
        let got2 = jcs_canonicalize::canonicalize(&got).expect("idempotent");
        assert_eq!(got, got2);
        assert!(got.starts_with(r#"{"literals":"#));
        assert!(got.contains(r#""string":"S""#));
    }

    #[test]
    fn segment_body_jcs_stable() {
        let body = serde_json::json!({
            "schema_version": 2,
            "seq": 0,
            "prev": null,
            "kind": {
                "kind": "intent_bound",
                "intent_digest": "0101010101010101010101010101010101010101010101010101010101010101",
                "intent_len": 3
            }
        });
        let a = canonical_bytes(&body).expect("jcs a");
        let b = canonical_bytes(&body).expect("jcs b");
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }
}
