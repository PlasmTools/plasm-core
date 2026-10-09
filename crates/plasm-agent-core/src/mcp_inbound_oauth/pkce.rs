use sha2::{Digest, Sha256};

pub fn validate_pkce_s256(code_challenge: &str, code_verifier: &str) -> bool {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    if !(43..=128).contains(&code_verifier.len())
        || !code_verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
    {
        return false;
    }
    let hash = Sha256::digest(code_verifier.as_bytes());
    use subtle::ConstantTimeEq;
    URL_SAFE_NO_PAD
        .encode(hash)
        .as_bytes()
        .ct_eq(code_challenge.as_bytes())
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    #[test]
    fn verifier_length_and_alphabet_are_enforced() {
        for verifier in ["a".repeat(42), "a".repeat(129), "!".repeat(43)] {
            assert!(!validate_pkce_s256(
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                &verifier
            ));
        }
        let verifier = "a".repeat(43);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert!(validate_pkce_s256(&challenge, &verifier));
        assert!(!validate_pkce_s256(&challenge, &"b".repeat(43)));
    }
}
