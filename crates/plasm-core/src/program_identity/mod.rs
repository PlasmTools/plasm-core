//! Durable identity of the complete typed program (not a plan authorization token).
//!
//! The versioned structural walk is independent of Serde field names, omission rules,
//! and map insertion order. Ordered collections remain ordered. All program fields,
//! including contracts and provenance, participate. This is identity, not a proof of
//! equivalence between different programs.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod model;
#[cfg(test)]
mod tests;
mod values;
mod walk;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CompiledProgramId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("compiled program identity must be a lowercase SHA-256 digest")]
pub struct InvalidCompiledProgramId;

impl CompiledProgramId {
    pub fn from_comp(comp: &crate::PlasmComp) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"plasm.typed-program.v1\0");
        walk::hash(comp, &mut digest);
        Self(format!("{:x}", digest.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for CompiledProgramId {
    type Err = InvalidCompiledProgramId;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(InvalidCompiledProgramId);
        }
        Ok(Self(value.into()))
    }
}
impl std::fmt::Display for CompiledProgramId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for CompiledProgramId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
