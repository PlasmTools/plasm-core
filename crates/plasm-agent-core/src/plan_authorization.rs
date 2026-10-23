//! A reviewed plan authorizes one dispatch, independently of its semantic identity.
use std::sync::{Arc, Mutex};

use plasm_core::PlanCommitRef;
use plasm_runtime::{ExecutionFailure, FailureCause, RecoveryDisposition};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{execute_session::ExecuteSession, server_state::PlasmHostState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Ready,
    Running,
    Succeeded,
    Failed,
    Indeterminate,
}

impl ExecutionState {
    fn consumed_guidance(self) -> &'static str {
        match self {
            Self::Succeeded => "Continue from the previous results. For current state, review a new read program. A deliberate repeat requires a fresh review.",
            Self::Failed | Self::Indeterminate => "Check the previous failure and resulting state before planning recovery; completed effects may remain. Further execution requires a fresh review.",
            Self::Running => "Follow the existing execution's status; do not start another execution to check it.",
            Self::Ready => "Check the execution authorization state before continuing; no execution was authorized by this call.",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizationSnapshot {
    id: Uuid,
    state: ExecutionState,
}

#[derive(Debug, Clone)]
pub struct PlanAuthorization {
    id: Uuid,
    state: Arc<Mutex<ExecutionState>>,
}

impl Default for PlanAuthorization {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            state: Arc::new(Mutex::new(ExecutionState::Ready)),
        }
    }
}

impl PlanAuthorization {
    pub fn snapshot(&self) -> AuthorizationSnapshot {
        AuthorizationSnapshot {
            id: self.id,
            state: self
                .state
                .lock()
                .map(|s| *s)
                .unwrap_or(ExecutionState::Indeterminate),
        }
    }

    pub fn restore(snapshot: AuthorizationSnapshot) -> Self {
        let state = match snapshot.state {
            ExecutionState::Running => ExecutionState::Indeterminate,
            state => state,
        };
        Self {
            id: snapshot.id,
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn claim(&self) -> Result<ExecutionClaim, ExecutionState> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ExecutionState::Indeterminate)?;
        if *state != ExecutionState::Ready {
            return Err(*state);
        }
        *state = ExecutionState::Running;
        Ok(ExecutionClaim {
            authorization: self.clone(),
            finished: false,
        })
    }
}

struct ExecutionClaim {
    authorization: PlanAuthorization,
    finished: bool,
}

impl ExecutionClaim {
    fn finish(&mut self, outcome: ExecutionState) {
        if let Ok(mut state) = self.authorization.state.lock() {
            *state = outcome;
        }
        self.finished = true;
    }
}

impl Drop for ExecutionClaim {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(ExecutionState::Indeterminate);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthorizationError {
    #[error("authorization `{commit_ref}` is {state:?}; this call dispatched nothing. {}", state.consumed_guidance())]
    Consumed {
        commit_ref: PlanCommitRef,
        state: ExecutionState,
    },
    #[error("execution authorization store unavailable; no dispatch permitted: {0}")]
    Store(#[from] redis::RedisError),
    #[error("invalid execution authorization state in durable store: {0}")]
    Encoding(#[from] serde_json::Error),
}

impl From<AuthorizationError> for ExecutionFailure {
    fn from(error: AuthorizationError) -> Self {
        let (cause, code) = match &error {
            AuthorizationError::Consumed { .. } => {
                (FailureCause::Authorization, "plan_authorization_consumed")
            }
            _ => (FailureCause::Runtime, "plan_authorization_store"),
        };
        let mut failure = ExecutionFailure::new(cause, code, error.to_string());
        if matches!(error, AuthorizationError::Consumed { .. }) {
            failure.recovery = RecoveryDisposition::ReconcileEffects;
        }
        failure
    }
}

/// The future is not polled until the local and (when configured) durable claims succeed.
/// Dropping it consumes the authorization: unknown external effects are never retried.
pub async fn execute_authorized<T>(
    es: &ExecuteSession,
    host: &PlasmHostState,
    reference: Option<&PlanCommitRef>,
    execution: impl std::future::Future<Output = Result<T, ExecutionFailure>>,
) -> Result<T, ExecutionFailure> {
    let Some(reference) = reference else {
        return execution.await;
    };
    let record = es.get_plan_commit(reference).ok_or_else(|| {
        crate::plan_commit_store::PlanCommitVerifyError::Unknown {
            commit_ref: reference.clone(),
        }
    })?;
    if record.is_expired() {
        return Err(crate::plan_commit_store::PlanCommitVerifyError::Expired {
            commit_ref: reference.clone(),
        }
        .into());
    }
    let mut claim = record
        .authorization
        .claim()
        .map_err(|state| AuthorizationError::Consumed {
            commit_ref: reference.clone(),
            state,
        })?;
    let key = format!("plasm:execution-authorization:{}", record.authorization.id);
    // Longer than the remaining authorization lifetime; a stale descriptor cannot revive it.
    let ttl = record
        .expires_at
        .saturating_duration_since(std::time::Instant::now())
        .as_secs()
        + 120;
    if let Some(redis) = &host.redis_backend {
        if let Some(previous) = redis
            .claim_plan_authorization(&key, ttl)
            .await
            .map_err(AuthorizationError::from)?
        {
            let state = serde_json::from_str(&previous).map_err(AuthorizationError::from)?;
            claim.finish(state);
            return Err(AuthorizationError::Consumed {
                commit_ref: reference.clone(),
                state,
            }
            .into());
        }
    }
    let result = execution.await;
    let state = if result.is_ok() {
        ExecutionState::Succeeded
    } else {
        ExecutionState::Failed
    };
    claim.finish(state);
    if let Some(redis) = &host.redis_backend {
        // Failure to publish a terminal label cannot undo effects or mask their original result.
        // The original durable Running claim remains consumed until the token expires.
        let encoded = serde_json::to_string(&state).expect("execution state serialization");
        if let Err(error) = redis.finish_plan_authorization(&key, &encoded, ttl).await {
            tracing::error!(%error, %reference, "execution authorization remains consumed; terminal label persistence unconfirmed");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumed_authorization_recovery_matrix() {
        for (state, guidance) in [
            (ExecutionState::Running, "existing execution's status"),
            (
                ExecutionState::Succeeded,
                "Continue from the previous results",
            ),
            (ExecutionState::Failed, "completed effects may remain"),
            (
                ExecutionState::Indeterminate,
                "completed effects may remain",
            ),
        ] {
            let failure: ExecutionFailure = AuthorizationError::Consumed {
                commit_ref: PlanCommitRef::mint(7),
                state,
            }
            .into();
            assert_eq!(failure.cause, FailureCause::Authorization);
            assert_eq!(failure.recovery, RecoveryDisposition::ReconcileEffects);
            assert_eq!(failure.code, "plan_authorization_consumed");
            assert!(failure.effects.is_empty());
            assert!(failure.dispatches.is_empty());
            assert!(failure
                .recovery_instructions()
                .unwrap()
                .contains("does not establish its effects"));
            let rendered = crate::agent_presentation::render_failure(&failure).into_string();
            assert!(rendered.contains("`pc7`"));
            assert!(rendered.contains("this call dispatched nothing"));
            assert!(rendered.contains(guidance));
            assert!(!rendered.contains("Revise the program"));
            assert!(!rendered.contains("Obtain authorization"));
        }
        let ordinary =
            ExecutionFailure::new(FailureCause::Program, "invalid_program", "Invalid program");
        assert_eq!(ordinary.recovery, RecoveryDisposition::RepairProgram);
    }

    #[test]
    fn authorization_lifecycle_matrix() {
        for terminal in [
            ExecutionState::Succeeded,
            ExecutionState::Failed,
            ExecutionState::Indeterminate,
        ] {
            let authorization = PlanAuthorization::default();
            let mut first = authorization.claim().unwrap();
            assert_eq!(authorization.claim().err(), Some(ExecutionState::Running));
            first.finish(terminal);
            drop(first);
            assert_eq!(authorization.claim().err(), Some(terminal));
            let encoded = serde_json::to_vec(&authorization.snapshot()).unwrap();
            let restored = PlanAuthorization::restore(serde_json::from_slice(&encoded).unwrap());
            assert_eq!(restored.claim().err(), Some(terminal));
            assert!(
                PlanAuthorization::default().claim().is_ok(),
                "fresh authorization permits a deliberate repeat"
            );
        }
    }

    #[test]
    fn dropped_and_rehydrated_running_claims_are_not_retryable() {
        let authorization = PlanAuthorization::default();
        let claim = authorization.claim().unwrap();
        let restored = PlanAuthorization::restore(authorization.snapshot());
        assert_eq!(restored.claim().err(), Some(ExecutionState::Indeterminate));
        drop(claim);
        assert_eq!(
            authorization.claim().err(),
            Some(ExecutionState::Indeterminate)
        );
    }

    #[test]
    fn concurrent_claims_have_one_winner() {
        let authorization = PlanAuthorization::default();
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let winners = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let authorization = authorization.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        authorization.claim().is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| usize::from(handle.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(winners, 1);
    }

    #[tokio::test]
    #[ignore = "requires isolated PLASM_TEST_REDIS_URL"]
    async fn authorization_redis_replica_matrix() {
        use crate::mcp_transport_store::RedisBackend;
        let url = std::env::var("PLASM_TEST_REDIS_URL").expect("isolated Redis URL");
        let a = RedisBackend::connect(&url, std::time::Duration::from_secs(60))
            .await
            .unwrap();
        let b = RedisBackend::connect(&url, std::time::Duration::from_secs(60))
            .await
            .unwrap();
        for terminal in [
            ExecutionState::Succeeded,
            ExecutionState::Failed,
            ExecutionState::Indeterminate,
        ] {
            let key = format!("test:authorization:{}", Uuid::new_v4());
            let (first, second) = tokio::join!(
                a.claim_plan_authorization(&key, 60),
                b.claim_plan_authorization(&key, 60)
            );
            let claims = [first.unwrap(), second.unwrap()];
            assert_eq!(claims.iter().filter(|result| result.is_none()).count(), 1);
            assert_eq!(claims.iter().flatten().next().unwrap(), "\"running\"");
            let encoded = serde_json::to_string(&terminal).unwrap();
            a.finish_plan_authorization(&key, &encoded, 60)
                .await
                .unwrap();
            assert_eq!(
                b.claim_plan_authorization(&key, 60).await.unwrap(),
                Some(encoded)
            );
            a.delete_strict(&key).await.unwrap();
        }
    }
}
