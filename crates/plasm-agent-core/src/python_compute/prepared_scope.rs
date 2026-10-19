//! Static Python preparation retained only for one live execution.
//!
//! The immutable session borrow pins catalogs and teaching symbols for the
//! lifetime of these proofs. Values, membership, coverage and budgets are not
//! cached: every occurrence still passes through `PreparedCompute::run_observed`.

use std::sync::{Arc, Mutex};

use plasm_core::plasm_monad::ComputeOp;
use plasm_runtime::{ExecutionFailure, FailureCause};

use super::PreparedCompute;
use crate::execute_session::ExecuteSession;

#[cfg(test)]
thread_local! {
    // Per-test runtime thread: no cross-test or process-global accounting.
    static PREPARATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EXECUTION_ADMISSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn preparation_count() -> usize {
    PREPARATIONS.get()
}

#[cfg(test)]
pub(crate) fn execution_admission_count() -> usize {
    EXECUTION_ADMISSIONS.get()
}

#[cfg(test)]
pub(super) fn record_execution_admission() {
    EXECUTION_ADMISSIONS.set(EXECUTION_ADMISSIONS.get() + 1);
}

pub(crate) struct PreparedComputeScope<'session> {
    session: &'session ExecuteSession,
    entries: Mutex<Vec<(ComputeOp, Arc<PreparedCompute>)>>,
}

impl<'session> PreparedComputeScope<'session> {
    pub(crate) fn new(session: &'session ExecuteSession) -> Self {
        Self {
            session,
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Prepare on first reach, preserving rejection timing for skipped branches.
    /// Exact operation equality includes source, contracts, catalog pin, language
    /// version, input mode and selection evidence; no digest can alias proofs.
    /// The lock spans synchronous checking so concurrent branches prepare once.
    pub(crate) fn check(&self, op: &ComputeOp) -> Result<Arc<PreparedCompute>, ExecutionFailure> {
        let mut entries = self.entries.lock().map_err(|_| {
            ExecutionFailure::new(
                FailureCause::Runtime,
                "python_preparation_scope_poisoned",
                "execution-local Python preparation lock was poisoned",
            )
        })?;
        if let Some((_, prepared)) = entries.iter().find(|(stored, _)| stored == op) {
            return Ok(Arc::clone(prepared));
        }
        #[cfg(test)]
        PREPARATIONS.set(PREPARATIONS.get() + 1);
        let prepared = Arc::new(super::check_op(self.session, op)?);
        entries.push((op.clone(), Arc::clone(&prepared)));
        Ok(prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{symbol_tuning::SymbolRender, CgsContext, TeachingExposureSession};

    fn session() -> ExecuteSession {
        let cgs = Arc::new(
            plasm_core::load_schema(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/schemas/python_value_contract"),
            )
            .unwrap(),
        );
        let mut contexts = indexmap::IndexMap::new();
        contexts.insert(
            "types".into(),
            Arc::new(CgsContext::entry("types", cgs.clone())),
        );
        let exposure = TeachingExposureSession::new(&cgs, "types", &["Sample"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            contexts,
            "types".into(),
            String::new(),
            String::new(),
            None,
            vec!["Sample".into()],
            Some(exposure),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
        )
    }

    fn operation(es: &ExecuteSession) -> ComputeOp {
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let token = symbols.entity_sym_for("types", "Sample");
        let source =
            format!("@compute\ndef selected(row: Value[{token}]):\n    return row.count > 0\n");
        let prepared = PreparedCompute::prepare(
            &source,
            &es.cgs,
            "types",
            symbols.as_ref(),
            super::super::ComputeInputMode::Singleton,
        )
        .unwrap();
        ComputeOp::Python {
            source,
            entry_id: "types".into(),
            entity: Some("Sample".into()),
            catalog_hash: es.cgs.catalog_cgs_hash_hex(),
            contract_version: super::super::CONTRACT_VERSION,
            language_profile: super::super::LANGUAGE_PROFILE.into(),
            input_schema: None,
            output_type: prepared.output,
            per_row: true,
            selection: None,
        }
    }

    #[test]
    fn prepared_scope_reuses_exact_operation_and_rechecks_changed_contracts() {
        let es = session();
        let op = operation(&es);
        let scope = PreparedComputeScope::new(&es);
        let prepared = scope.check(&op).unwrap();
        assert!(Arc::ptr_eq(&prepared, &scope.check(&op).unwrap()));
        for change in 0..6 {
            let mut changed = op.clone();
            let ComputeOp::Python {
                catalog_hash,
                contract_version,
                output_type,
                source,
                input_schema,
                selection,
                ..
            } = &mut changed
            else {
                unreachable!()
            };
            match change {
                0 => *catalog_hash = "changed".into(),
                1 => *contract_version += 1,
                2 => {
                    *output_type = plasm_core::value_contract::ValueContract::scalar(
                        plasm_core::FieldType::String,
                    )
                }
                3 => *source = source.replace("row.count", "row.absent"),
                4 => {
                    *input_schema = Some(
                        plasm_core::plasm_monad::SyntheticResultSchema::for_value(
                            plasm_core::value_contract::ValueContract::record(
                                Default::default(),
                                Default::default(),
                            ),
                        )
                        .unwrap(),
                    )
                }
                _ => {
                    *selection = Some(Box::new(plasm_core::plasm_monad::PythonRowSelection {
                        owner: plasm_core::plasm_monad::PlanQualifiedEntityKey {
                            entry_id: "types".into(),
                            entity: "Sample".into(),
                        },
                        ports: vec![],
                    }))
                }
            }
            let error = scope
                .check(&changed)
                .err()
                .expect("changed invalid operation must be checked");
            assert_eq!(error.cause, FailureCause::Program);
        }
        assert!(Arc::ptr_eq(&prepared, &scope.check(&op).unwrap()));
        let other_scope = PreparedComputeScope::new(&es);
        assert!(!Arc::ptr_eq(&prepared, &other_scope.check(&op).unwrap()));
        let concurrent_scope = PreparedComputeScope::new(&es);
        std::thread::scope(|threads| {
            let first = threads.spawn(|| concurrent_scope.check(&op).unwrap());
            let second = threads.spawn(|| concurrent_scope.check(&op).unwrap());
            assert!(Arc::ptr_eq(&first.join().unwrap(), &second.join().unwrap()));
        });
        let mut other_session = session();
        other_session.teaching_exposure = None;
        assert!(PreparedComputeScope::new(&other_session)
            .check(&op)
            .is_err());
    }
}
