use sha2::{Digest, Sha256};

pub fn validate_pkce_s256(code_challenge: &str, code_verifier: &str) -> bool {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let hash = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash) == code_challenge
}
