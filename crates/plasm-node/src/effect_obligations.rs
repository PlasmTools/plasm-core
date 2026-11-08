//! Native ownership of in-flight and unresolved reviewed executions.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

const MAX_WORKFLOWS: usize = 1024;
const MAX_RUNS_PER_WORKFLOW: usize = 128;

#[derive(Default)]
struct Workflow {
    sealed: bool,
    runs: HashSet<u64>,
    next_id: u64,
}

#[derive(Clone, Default)]
pub(crate) struct EffectObligations(Arc<Mutex<HashMap<String, Workflow>>>);

pub(crate) struct AdmittedRun {
    owner: EffectObligations,
    workflow: String,
    id: u64,
}

impl EffectObligations {
    /// A drained session may discard a completed seal, never an unresolved run.
    pub(crate) fn release(&self, workflow: &str) {
        let mut workflows = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if workflows
            .get(workflow)
            .is_some_and(|state| state.sealed && state.runs.is_empty())
        {
            workflows.remove(workflow);
        }
    }

    /// Reserve before execution. A dropped or panicked worker leaves the run
    /// unresolved; only an observed safe result removes its reservation.
    pub(crate) fn admit(&self, workflow: &str) -> Result<AdmittedRun, &'static str> {
        let mut workflows = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !workflows.contains_key(workflow) && workflows.len() >= MAX_WORKFLOWS {
            return Err("effect workflow capacity exhausted");
        }
        let state = workflows.entry(workflow.to_owned()).or_default();
        if state.sealed {
            return Err("workflow already completed");
        }
        if state.runs.len() >= MAX_RUNS_PER_WORKFLOW {
            return Err("effect obligation capacity exhausted");
        }
        let id = state.next_id;
        state.next_id = id.checked_add(1).ok_or("effect run identifier exhausted")?;
        state.runs.insert(id);
        Ok(AdmittedRun {
            owner: self.clone(),
            workflow: workflow.into(),
            id,
        })
    }

    /// Atomically prevent further execution and accept a terminal only when
    /// every named workflow has no running or unresolved executions.
    pub(crate) fn seal(&self, ids: &[String]) -> Result<(), &'static str> {
        let mut workflows = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if ids
            .iter()
            .any(|id| workflows.get(id).is_some_and(|s| !s.runs.is_empty()))
        {
            return Err("workflow has unfinished or uncertain effects");
        }
        let missing = ids
            .iter()
            .filter(|id| !workflows.contains_key(*id))
            .collect::<HashSet<_>>()
            .len();
        if workflows.len().saturating_add(missing) > MAX_WORKFLOWS {
            return Err("effect workflow capacity exhausted");
        }
        for id in ids {
            workflows.entry(id.clone()).or_default().sealed = true;
        }
        Ok(())
    }
}

impl AdmittedRun {
    /// Remove only when execution succeeded, or native failure evidence proves
    /// that no mutation was dispatched or acknowledged.
    pub(crate) fn finish(self, result: &crate::engine::RunPlanResult) {
        let safe = result.ok
            || result
                .failure_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<plasm_runtime::ExecutionFailure>(json).ok())
                .is_some_and(|failure| {
                    !failure.effects_unresolved
                        && failure.dispatches.is_empty()
                        && failure.effects.iter().all(|effect| effect.completed == 0)
                });
        if safe {
            let mut workflows = self.owner.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(state) = workflows.get_mut(&self.workflow) {
                state.runs.remove(&self.id);
            }
            if workflows
                .get(&self.workflow)
                .is_some_and(|state| state.runs.is_empty() && !state.sealed)
            {
                workflows.remove(&self.workflow);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::RunPlanResult;

    fn success() -> RunPlanResult {
        RunPlanResult {
            ok: true,
            message: String::new(),
            rows_json: None,
            meta_json: None,
            artifacts_json: None,
            failure_json: None,
        }
    }

    #[test]
    fn unrelated_success_cannot_discharge_unresolved_run() {
        let registry = EffectObligations::default();
        let original = registry.admit("workflow").unwrap();
        drop(original); // A lost worker is uncertain and must remain open.
        registry.admit("workflow").unwrap().finish(&success());
        assert!(registry.seal(&["workflow".into()]).is_err());
        assert!(registry.seal(&["other".into()]).is_ok());
        assert!(registry.admit("other").is_err());
    }

    #[test]
    fn terminal_and_execution_admission_are_serialized() {
        let registry = EffectObligations::default();
        let running = registry.admit("workflow").unwrap();
        assert!(registry.seal(&["workflow".into()]).is_err());
        running.finish(&success());
        registry.seal(&["workflow".into()]).unwrap();
        assert!(registry.admit("workflow").is_err());
    }

    #[test]
    fn refusing_one_workflow_does_not_seal_the_others() {
        let registry = EffectObligations::default();
        let running = registry.admit("b").unwrap();
        assert!(registry.seal(&["a".into(), "b".into()]).is_err());
        registry.admit("a").unwrap().finish(&success());
        running.finish(&success());
        assert!(registry.seal(&["a".into(), "b".into()]).is_ok());
    }

    #[test]
    fn releasing_a_session_does_not_erase_an_unresolved_run() {
        let registry = EffectObligations::default();
        drop(registry.admit("workflow").unwrap());
        registry.release("workflow");
        assert!(registry.seal(&["workflow".into()]).is_err());
    }
}
