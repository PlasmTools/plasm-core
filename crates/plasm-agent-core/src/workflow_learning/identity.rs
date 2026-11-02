//! Identity of a complete compiled program, distinct from a reviewed plan commit.
pub use plasm_core::CompiledProgramId;

#[cfg(test)]
mod tests {
    use super::super::{model::*, test_identities as ids};

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
