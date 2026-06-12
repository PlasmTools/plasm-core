use super::error::EvidenceEmitError;
use std::env;
use std::sync::OnceLock;

pub const ENV_PLASM_EVIDENCE_CHAIN: &str = "PLASM_EVIDENCE_CHAIN";
pub const ENV_PLASM_EVIDENCE_SIGNING_KEY: &str = "PLASM_EVIDENCE_SIGNING_KEY";
pub const ENV_PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS: &str = "PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS";

static TRUSTED_PUBLIC_KEYS_CACHE: OnceLock<Vec<String>> = OnceLock::new();
static SIGNING_SEED_HEX_CACHE: OnceLock<String> = OnceLock::new();

pub fn evidence_chain_enabled() -> bool {
    env::var(ENV_PLASM_EVIDENCE_CHAIN)
        .ok()
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

pub fn trusted_public_keys_from_env() -> Vec<String> {
    TRUSTED_PUBLIC_KEYS_CACHE
        .get_or_init(|| {
            env::var(ENV_PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS)
                .ok()
                .map(|raw| plasm_evidence::sign::parse_trusted_public_keys_csv(&raw))
                .unwrap_or_default()
        })
        .clone()
}

pub(crate) fn signing_seed_hex_from_env() -> Result<Option<String>, EvidenceEmitError> {
    if cfg!(test) {
        return signing_seed_hex_from_env_uncached();
    }
    if let Some(cached) = SIGNING_SEED_HEX_CACHE.get() {
        return Ok(Some(cached.clone()));
    }
    let seed = match signing_seed_hex_from_env_uncached()? {
        Some(s) => s,
        None => return Ok(None),
    };
    let _ = SIGNING_SEED_HEX_CACHE.set(seed.clone());
    Ok(Some(seed))
}

fn signing_seed_hex_from_env_uncached() -> Result<Option<String>, EvidenceEmitError> {
    let raw = match env::var(ENV_PLASM_EVIDENCE_SIGNING_KEY) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let seed = raw.trim();
    if seed.is_empty() {
        return Ok(None);
    }
    let bytes = hex::decode(seed)
        .map_err(|e| EvidenceEmitError::SigningKeyInvalid(format!("invalid seed hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(EvidenceEmitError::SigningKeyInvalid(
            "PLASM_EVIDENCE_SIGNING_KEY must be 32-byte hex".into(),
        ));
    }
    Ok(Some(seed.to_string()))
}
