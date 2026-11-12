//! Runnable monadic comp: wire [`PlasmCompArtifact`] + lifted executable payload (in-memory only).

use crate::plasm_comp_lift::{lift_executable_comp, ExecutablePlasmComp};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlasmCompBundleError {
    #[error(transparent)]
    Lift(#[from] crate::plasm_comp_lift::PlasmCompLiftError),
}

impl From<PlasmCompBundleError> for plasm_runtime::ExecutionFailure {
    fn from(error: PlasmCompBundleError) -> Self {
        Self::new(
            plasm_runtime::FailureCause::Program,
            "plasm_comp_bundle_invalid",
            error.to_string(),
        )
    }
}
use plasm_core::PlasmCompArtifact;

/// Execution evidence belongs to authored programs. Result delivery is a
/// downstream operation and cannot open or complete a program learning ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionEvidenceKind {
    Program,
    ResultDelivery,
}

/// Canonical compile/run artifact. Wire consumers see [`PlasmCompArtifact::comp`] only.
#[derive(Debug, Clone)]
pub struct PlasmCompBundle {
    artifact: PlasmCompArtifact,
    executable: ExecutablePlasmComp,
    source: Option<std::sync::Arc<str>>,
    compilation_symbols: crate::program_symbol_context::CompilationSymbolContext,
    python_operands:
        Option<std::sync::Arc<[crate::plasm_dag::python::operand_evidence::AdmittedOperand]>>,
}

impl PlasmCompBundle {
    pub fn new(artifact: PlasmCompArtifact) -> Result<Self, PlasmCompBundleError> {
        let executable = lift_executable_comp(&artifact)?;
        Ok(Self {
            artifact,
            executable,
            source: None,
            compilation_symbols: Default::default(),
            python_operands: None,
        })
    }

    /// Source provenance is retained in memory, outside the semantic plan artifact.
    pub fn with_source(mut self, source: impl Into<std::sync::Arc<str>>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub(crate) fn capture_compilation_symbols(
        mut self,
        session: &crate::execute_session::ExecuteSession,
    ) -> Self {
        use crate::program_symbol_context::{
            CompilationSymbolContext, CompilationSymbols, SymbolContextUnavailable,
        };
        self.compilation_symbols = match CompilationSymbols::capture(session, &self) {
            Ok(value) => CompilationSymbolContext::Captured(Box::new(value)),
            Err(error) => {
                CompilationSymbolContext::Unavailable(SymbolContextUnavailable::CaptureFailed {
                    diagnostic: error.to_string(),
                })
            }
        };
        self
    }

    pub(crate) fn compiled_dependencies(
        &self,
    ) -> Result<
        &std::collections::BTreeSet<plasm_core::prerequisites::CapabilityRef>,
        crate::plasm_dag::dependencies::DependencyUnavailable,
    > {
        self.compilation_symbols
            .captured()
            .ok_or(crate::plasm_dag::dependencies::DependencyUnavailable::NotCaptured)?
            .dependencies()
            .capabilities(&self.artifact.comp)
    }

    pub fn compilation_symbols(&self) -> &crate::program_symbol_context::CompilationSymbolContext {
        &self.compilation_symbols
    }

    pub fn with_compilation_symbols(
        mut self,
        context: crate::program_symbol_context::CompilationSymbolContext,
    ) -> Self {
        self.compilation_symbols = context;
        self
    }

    pub(crate) fn with_python_operands(
        mut self,
        operands: Vec<crate::plasm_dag::python::operand_evidence::AdmittedOperand>,
    ) -> Self {
        self.python_operands = Some(operands.into());
        self
    }

    pub(crate) fn python_operands(
        &self,
    ) -> Option<&[crate::plasm_dag::python::operand_evidence::AdmittedOperand]> {
        self.python_operands.as_deref()
    }

    pub(crate) fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub fn artifact(&self) -> &PlasmCompArtifact {
        &self.artifact
    }

    pub(crate) fn execution_evidence_kind(&self) -> ExecutionEvidenceKind {
        use plasm_core::{Expr, PlasmStepPayload};
        // Page continuations are compiled as a single invoke. Classify the
        // typed operation, independently of source text, transport or success.
        let mut steps = self.artifact.comp.steps.values();
        match (steps.next(), steps.next()) {
            (Some(PlasmStepPayload::Invoke(invoke)), None)
                if invoke
                    .ir
                    .as_ref()
                    .is_some_and(|ir| matches!(ir.expr, Expr::Page(_))) =>
            {
                ExecutionEvidenceKind::ResultDelivery
            }
            _ => ExecutionEvidenceKind::Program,
        }
    }

    pub(crate) fn executable(&self) -> &ExecutablePlasmComp {
        &self.executable
    }

    pub fn into_artifact(self) -> PlasmCompArtifact {
        self.artifact
    }
}

impl TryFrom<PlasmCompArtifact> for PlasmCompBundle {
    type Error = PlasmCompBundleError;

    fn try_from(artifact: PlasmCompArtifact) -> Result<Self, Self::Error> {
        Self::new(artifact)
    }
}
