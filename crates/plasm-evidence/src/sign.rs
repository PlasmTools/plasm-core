#[cfg(feature = "signatures")]
use crate::bundle::{EvidenceBundle, EvidenceSignature};
#[cfg(feature = "signatures")]
use crate::jcs;
#[cfg(feature = "signatures")]
use crate::verify::EvidenceError;
#[cfg(feature = "signatures")]
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

#[cfg(feature = "signatures")]
pub fn signable_head_bytes(bundle: &EvidenceBundle) -> Result<Vec<u8>, EvidenceError> {
    let head = bundle
        .chain_head()
        .ok_or(EvidenceError::EmptyChain)?;
    let v = serde_json::json!({
        "schema_version": 2u32,
        "scope": bundle.scope,
        "chain_head": head.to_hex(),
    });
    jcs::canonical_bytes(&v)
}

#[cfg(feature = "signatures")]
pub fn sign_bundle(
    bundle: &EvidenceBundle,
    signing_key: &SigningKey,
) -> Result<EvidenceSignature, EvidenceError> {
    let bytes = signable_head_bytes(bundle)?;
    let sig = signing_key.sign(&bytes);
    Ok(EvidenceSignature {
        public_key_hex: hex::encode(signing_key.verifying_key().as_bytes()),
        signature_hex: hex::encode(sig.to_bytes()),
    })
}

#[cfg(feature = "signatures")]
pub fn verify_bundle_signature(
    bundle: &EvidenceBundle,
    sig: &EvidenceSignature,
) -> Result<(), EvidenceError> {
    verify_bundle_signature_trusted(bundle, sig, &[])
}

/// When `trusted_public_keys` is non-empty, the signature's public key must appear in the list.
#[cfg(feature = "signatures")]
pub fn verify_bundle_signature_trusted(
    bundle: &EvidenceBundle,
    sig: &EvidenceSignature,
    trusted_public_keys: &[String],
) -> Result<(), EvidenceError> {
    if !trusted_public_keys.is_empty() {
        let pk = sig.public_key_hex.trim().to_ascii_lowercase();
        if !trusted_public_keys.iter().any(|t| t == &pk) {
            return Err(EvidenceError::SignatureInvalid);
        }
    }
    let pk_bytes = hex::decode(sig.public_key_hex.trim()).map_err(|_| EvidenceError::SignatureInvalid)?;
    if pk_bytes.len() != 32 {
        return Err(EvidenceError::SignatureInvalid);
    }
    let mut pk_arr = [0u8; 32];
    pk_arr.copy_from_slice(&pk_bytes);
    let vk = VerifyingKey::from_bytes(&pk_arr).map_err(|_| EvidenceError::SignatureInvalid)?;
    let sig_bytes = hex::decode(sig.signature_hex.trim()).map_err(|_| EvidenceError::SignatureInvalid)?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|_| EvidenceError::SignatureInvalid)?;
    let bytes = signable_head_bytes(bundle)?;
    vk.verify(&bytes, &signature)
        .map_err(|_| EvidenceError::SignatureInvalid)
}

#[cfg(feature = "signatures")]
pub fn signing_key_from_seed_hex(seed_hex: &str) -> Result<SigningKey, String> {
    let bytes = hex::decode(seed_hex.trim()).map_err(|e| format!("invalid seed hex: {e}"))?;
    if bytes.len() != 32 {
        return Err("PLASM_EVIDENCE_SIGNING_KEY must be 32-byte hex".into());
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&seed))
}

#[cfg(feature = "signatures")]
pub fn parse_trusted_public_keys_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

#[cfg(all(test, feature = "signatures"))]
mod tests {
    use super::*;
    use crate::bundle::{EvidenceAnchors, EvidenceBundle};
    use crate::chain::ChainBuilder;
    use crate::digest::IntentDigest;
    use crate::scope::EvidenceScope;
    use crate::segment::EvidenceKind;

    #[test]
    fn sign_and_verify_bundle_head() {
        let seed = [7u8; 32];
        let key = SigningKey::from_bytes(&seed);
        let mut b = ChainBuilder::new();
        b.push(
            EvidenceKind::IntentBound {
                intent_digest: IntentDigest::from_bytes([1u8; 32]),
                intent_len: 4,
            },
            None,
        )
        .expect("push");
        let chain = b.finish();
        let bundle = EvidenceBundle {
            scope: EvidenceScope::new_v1("p".repeat(64), "s1", "c".repeat(64), 0, "demo"),
            chain,
            anchors: EvidenceAnchors::default(),
            signature: None,
        };
        let sig = sign_bundle(&bundle, &key).expect("sign");
        let mut signed = bundle.clone();
        signed.signature = Some(sig.clone());
        verify_bundle_signature(&signed, &sig).expect("verify");
    }

    #[test]
    fn trusted_pubkey_enforcement() {
        let seed = [7u8; 32];
        let key = SigningKey::from_bytes(&seed);
        let mut b = ChainBuilder::new();
        b.push(
            EvidenceKind::IntentBound {
                intent_digest: IntentDigest::from_bytes([1u8; 32]),
                intent_len: 4,
            },
            None,
        )
        .expect("push");
        let bundle = EvidenceBundle {
            scope: EvidenceScope::new_v1("p".repeat(64), "s1", "c".repeat(64), 0, "demo"),
            chain: b.finish(),
            anchors: EvidenceAnchors::default(),
            signature: None,
        };
        let sig = sign_bundle(&bundle, &key).expect("sign");
        let mut signed = bundle;
        signed.signature = Some(sig.clone());
        let pk = sig.public_key_hex.clone();
        verify_bundle_signature_trusted(&signed, &sig, &[pk]).expect("trusted ok");
        assert!(verify_bundle_signature_trusted(&signed, &sig, &["00".repeat(32)]).is_err());
    }
}
