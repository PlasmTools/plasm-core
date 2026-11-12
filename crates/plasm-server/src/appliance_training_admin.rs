//! Authenticated local-owner training controls, dispatched by the appliance admin worker.
use plasm_agent_core::{
    server_state::PlasmHostState,
    session_identity::LogicalSessionId,
    workflow_learning::training::{
        TrainingCommand, TrainingCommandEnvelope, TrainingError, TrainingStatus,
    },
};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct OwnerSetupSecret(String);
impl std::fmt::Debug for OwnerSetupSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerSetupSecret([redacted])")
    }
}
impl OwnerSetupSecret {
    pub fn push(&mut self, value: char) {
        self.0.push(value);
    }
    pub fn pop(&mut self) {
        self.0.pop();
    }
    pub fn character_count(&self) -> usize {
        self.0.chars().count()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TrainingAction {
    Status,
    AuthorizeController {
        expected_revision: u64,
        idempotency_key: uuid::Uuid,
    },
    Stop {
        expected_revision: u64,
        idempotency_key: uuid::Uuid,
    },
}

pub struct TrainingJob {
    pub logical: LogicalSessionId,
    pub secret: OwnerSetupSecret,
    pub action: TrainingAction,
}

#[derive(Debug, Clone)]
pub struct TrainingSnapshot {
    pub logical: LogicalSessionId,
    pub status: TrainingStatus,
}

pub async fn run(
    host: Arc<PlasmHostState>,
    job: TrainingJob,
) -> Result<TrainingSnapshot, TrainingError> {
    host.live_plan_pool()
        .run_local(move || async move {
            let actor =
                plasm_agent_core::http::authenticate_local_training_owner(&host, &job.secret.0)?;
            let repository = host
                .workflow_learning
                .repository()
                .await
                .map_err(|_| TrainingError::Unavailable)?;
            repository
                .check_training_controller(&actor, job.logical)
                .await?;
            let status = match job.action {
                TrainingAction::Status => {
                    repository
                        .training_status(actor.scope(), job.logical)
                        .await?
                }
                TrainingAction::Stop {
                    expected_revision,
                    idempotency_key,
                } => {
                    repository
                        .training_command(
                            &actor,
                            job.logical,
                            &TrainingCommandEnvelope {
                                expected_revision,
                                idempotency_key,
                                command: TrainingCommand::Stop,
                            },
                        )
                        .await?
                }
                TrainingAction::AuthorizeController {
                    expected_revision,
                    idempotency_key,
                } => {
                    repository
                        .training_command(
                            &actor,
                            job.logical,
                            &TrainingCommandEnvelope {
                                expected_revision,
                                idempotency_key,
                                command: TrainingCommand::AuthorizeController,
                            },
                        )
                        .await?
                }
            };
            Ok(TrainingSnapshot {
                logical: job.logical,
                status,
            })
        })
        .await
        .map_err(|_| TrainingError::Unavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_setup_secret_never_appears_in_debug() {
        let mut secret = OwnerSetupSecret::default();
        for c in "private-control-credential".chars() {
            secret.push(c);
        }
        assert!(!format!("{secret:?}").contains("private-control"));
        assert_eq!(secret.character_count(), 26);
    }
}
