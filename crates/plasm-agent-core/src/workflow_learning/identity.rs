//! Identity of a complete compiled program, distinct from a reviewed plan commit.
pub use plasm_core::CompiledProgramId;

/// Identity of one completed program execution, independent of result snapshots.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct ExecutionId([u8; 32]);

impl ExecutionId {
    pub fn from_attempt(attempt: uuid::Uuid) -> Self {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"plasm/workflow-execution/v1\0");
        hash.update(attempt.as_bytes());
        Self(hash.finalize().into())
    }

    pub fn to_wire(self) -> String {
        format!("we{}", hex::encode(self.0))
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        let bytes = value.strip_prefix("we")?;
        if bytes.len() != 64
            || !bytes
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return None;
        }
        let decoded = hex::decode(bytes).ok()?;
        Some(Self(decoded.try_into().ok()?))
    }
}

impl TryFrom<String> for ExecutionId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_wire(&value).ok_or("invalid execution identity")
    }
}

impl From<ExecutionId> for String {
    fn from(value: ExecutionId) -> Self {
        value.to_wire()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{model::*, test_identities as ids};

    #[test]
    fn execution_identity_is_distinct_from_result_artifact_identity() {
        let attempt = uuid::Uuid::new_v4();
        let id = ExecutionId::from_attempt(attempt);
        assert_eq!(id, ExecutionId::from_attempt(attempt));
        assert_ne!(id, ExecutionId::from_attempt(uuid::Uuid::new_v4()));
        assert_eq!(ExecutionId::from_wire(&id.to_wire()), Some(id));
        assert!(crate::run_artifacts::RunArtifactId::from_wire(&id.to_wire()).is_none());
        let old = crate::run_artifacts::RunArtifactId::from_bytes([7; 32]);
        assert!(ExecutionId::from_wire(&old.to_wire()).is_none());
    }

    #[tokio::test]
    async fn compiled_multi_argument_program_keeps_identity_through_jsonb() {
        let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
            return;
        };
        let session = crate::workflow_learning::teaching::tests::session(&["LangItem"]);
        let symbols = crate::symbol_map_resolve::resolve_session_symbol_map(
            &crate::symbol_map_resolve::SessionSymbolMapContext {
                session: &session,
                cross_cache: None,
            },
        );
        let entity = symbols.entity_sym_for("langmatrix", "LangItem");
        let method = symbols.method_sym_for("langmatrix", "LangItem", "langitem_create");
        let source = format!("class Create(Program):\n    def build(self):\n        return {entity}.{method}(title='Fixture', score=25, owner='fixture-owner', active=True)\n");
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
        let original = &bundle.artifact().comp;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let persisted: serde_json::Value = sqlx::query_scalar("SELECT $1::jsonb")
            .bind(serde_json::to_value(original).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
        let restored: plasm_core::PlasmComp = serde_json::from_value(persisted).unwrap();
        assert_eq!(
            CompiledProgramId::from_comp(original),
            CompiledProgramId::from_comp(&restored)
        );
    }

    #[test]
    fn durable_receipt_rejects_invalid_and_cross_domain_wire_identities() {
        let receipt = ExecutionReceipt {
            scope: LibraryScope {
                tenant: "tenant".into(),
                project: "project".into(),
            },
            logical_session: ids::logical("logical"),
            session_revision: 1,
            execution_id: ids::run("run"),
            plan_id: ids::plan("plan"),
            prompt_hash: ids::prompt("prompt"),
            execute_session: ids::execute("execute"),
            source: String::new(),
            compiled_plan: serde_json::json!({}),
            observation: Default::default(),
            catalogue_requirements: crate::workflow_learning::catalogue_guidance::CatalogueRequirementEvidence::Unavailable { diagnostic: "fixture".into() },
            teaching_plan: super::super::review_packet::TeachingPlan::Unavailable {
                reason: super::super::review_packet::TeachingPlanUnavailable::PlanningFailed,
            },

            catalog_fingerprints: Default::default(),
        };
        let wire = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serde_json::from_value::<ExecutionReceipt>(wire.clone()).unwrap(),
            receipt
        );
        for (field, wrong) in [
            ("logical_session", "not-a-session".to_owned()),
            ("execution_id", receipt.logical_session.to_string()),
            ("plan_id", receipt.execution_id.to_wire()),
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
