pub mod common;
pub mod compile;
pub mod er_diagram;
pub mod error;
pub mod execute;
pub mod predicate;
pub mod replay;
pub mod schema;
pub mod validate;

pub use error::{CommandError, InputKind};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_inputs_are_typed_at_command_entry_points() {
        let path =
            std::env::temp_dir().join(format!("plasm-cli-missing-input-{}", std::process::id()));
        assert!(!path.exists(), "test input must not exist");
        let input = path.to_str().unwrap();
        let failures = [
            schema::execute(crate::SchemaAction::Validate { file: input.into() })
                .await
                .unwrap_err(),
            compile::execute(input, "{}").await.unwrap_err(),
            predicate::execute(crate::PredicateAction::Check {
                schema: input.into(),
                predicate: "{}".into(),
            })
            .await
            .unwrap_err(),
            execute::execute(input, "{}", plasm_runtime::ExecutionMode::Replay)
                .await
                .unwrap_err(),
        ];
        for failure in failures {
            assert!(
                matches!(failure, CommandError::InputMissing { kind: InputKind::Schema, path: actual } if actual == path)
            );
        }
        assert!(
            matches!(replay::execute(crate::ReplayAction::Test { dir: input.into() }).await,
            Err(CommandError::InputMissing { kind: InputKind::ReplayDirectory, path: actual }) if actual == path)
        );
    }
}
