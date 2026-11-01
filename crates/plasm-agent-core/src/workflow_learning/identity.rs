//! Identity of a complete compiled program, distinct from a reviewed plan commit.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CompiledProgramId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("compiled program identity must be a lowercase SHA-256 digest")]
pub struct InvalidCompiledProgramId;

impl CompiledProgramId {
    pub fn from_comp(comp: &plasm_core::PlasmComp) -> Result<Self, serde_json::Error> {
        Ok(Self(plasm_core::catalog_discovery::content_hash(
            &serde_json::to_vec(comp)?,
        )))
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

#[cfg(test)]
mod tests {
    use super::super::{model::*, test_identities as ids};

    #[test]
    fn durable_receipt_rejects_invalid_and_cross_domain_wire_identities() {
        let receipt = ExecutionReceipt {
            scope: LibraryScope {
                tenant: "tenant".into(),
                project: "project".into(),
            },
            logical_session: ids::logical("logical"),
            session_revision: 1,
            run_id: ids::run("run"),
            plan_id: ids::plan("plan"),
            prompt_hash: ids::prompt("prompt"),
            execute_session: ids::execute("execute"),
            run_artifact: serde_json::json!({}),
            source: String::new(),
            compiled_plan: serde_json::json!({}),
            observation: Default::default(),
            catalogue_requirements: Default::default(),
            symbol_context: Default::default(),
            catalog_fingerprints: Default::default(),
        };
        let wire = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serde_json::from_value::<ExecutionReceipt>(wire.clone()).unwrap(),
            receipt
        );
        for (field, wrong) in [
            ("logical_session", "not-a-session".to_owned()),
            ("run_id", receipt.logical_session.to_string()),
            ("plan_id", receipt.run_id.to_wire()),
            ("prompt_hash", receipt.execute_session.to_string()),
            ("execute_session", receipt.prompt_hash.to_string()),
        ] {
            let mut malformed = wire.clone();
            malformed[field] = wrong.into();
            assert!(
                serde_json::from_value::<ExecutionReceipt>(malformed).is_err(),
                "{field}"
            );
        }
    }
}
