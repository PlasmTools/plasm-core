//! Immutable compiler-time symbol authority for durable authored-program repair.
//! Restores the existing PLSL ledger; never reconstructs numbering from dependencies.
pub use crate::plasm_dag::dependencies::DependencyUnavailable;
use crate::{execute_path_ids::ExecuteSessionId, session_identity::LogicalSessionId};
use crate::{execute_session::ExecuteSession, plasm_comp_bundle::PlasmCompBundle};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum SymbolContextError {
    #[error("compilation had no authoritative teaching exposure")]
    MissingExposure,
    #[error("Python symbol authority failed: {0}")]
    PythonTeaching(#[from] plasm_core::prompt_render::python::PythonTeachingError),
    #[error("compilation symbol context is unavailable")]
    Unavailable,
    #[error("saved program identity or compilation digest differs")]
    Identity,
    #[error("saved compiler or comp wire version differs")]
    Version,
    #[error("effective catalogue set or fingerprints differ")]
    CatalogDrift,
    #[error("symbol ledger encoding failed: {0}")]
    Encode(#[from] plasm_core::PersistedSymbolLedgerEncodeError),
    #[error("symbol ledger decoding failed: {0}")]
    Decode(#[from] plasm_core::PersistedSymbolLedgerDecodeError),
    #[error("compiled program encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("restored program admission failed: {0}")]
    Admission(#[source] Box<crate::compilation_error::CompilationError>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "state", content = "ranges", rename_all = "snake_case")]
enum CapturedDisclosure {
    Classified {
        embedded_literals: Vec<(u32, u32)>,
        unresolved_dataflow: Vec<(u32, u32)>,
    },
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "state", content = "teaching", rename_all = "snake_case")]
enum PythonSymbolAuthority {
    Captured(plasm_core::prompt_render::python::PythonTeachingState),
    #[default]
    Unavailable,
}
impl PythonSymbolAuthority {
    fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompilationSymbols {
    tenant_scope: String,
    prompt_hash: String,
    domain_revision: u32,
    source_sha256: String,
    compiled_sha256: String,
    plan_id: String,
    compiler_version: String,
    comp_wire_version: u32,
    /// Every compiler context, including ambient contexts used by pure Python compute.
    catalog_fingerprints: BTreeMap<String, String>,
    /// The PLSL payload separately pins only graphs owning the exposed symbol ledger.
    ledger_bytes: Vec<u8>,
    #[serde(default, skip_serializing_if = "PythonSymbolAuthority::is_unavailable")]
    python_teaching: PythonSymbolAuthority,
    #[serde(default)]
    disclosure: CapturedDisclosure,
    #[serde(default)]
    dependencies: crate::plasm_dag::dependencies::DependencyEvidence,
}
impl CompilationSymbols {
    pub(crate) fn capture(
        session: &ExecuteSession,
        bundle: &PlasmCompBundle,
    ) -> Result<Self, SymbolContextError> {
        let exposure = session
            .teaching_exposure
            .as_ref()
            .ok_or(SymbolContextError::MissingExposure)?;
        let python_teaching = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            exposure,
            &session.python_teaching,
        )?
        .next_state;
        let mut pins = plasm_core::catalog_cgs_hashes_from_session(exposure);
        // Nominal-only declarations have catalogue authority without entity symbols.
        pins.extend(
            python_teaching
                .catalog_fingerprints()
                .iter()
                .map(|(entry, hash)| (entry.clone(), hash.clone())),
        );
        let ledger = plasm_core::PersistedSymbolLedger::from_session(exposure, pins.clone())?;
        let catalogs: BTreeMap<_, _> = session
            .contexts_by_entry
            .iter()
            .map(|(id, ctx)| (id.clone(), ctx.cgs.catalog_cgs_hash_hex()))
            .collect();
        if pins
            .iter()
            .any(|(entry, hash)| catalogs.get(entry) != Some(hash))
        {
            return Err(SymbolContextError::CatalogDrift);
        }
        Ok(Self {
            tenant_scope: session.tenant_scope.clone(),
            prompt_hash: session.prompt_hash.clone(),
            domain_revision: session.domain_revision,
            source_sha256: plasm_core::catalog_discovery::content_hash(
                bundle
                    .source()
                    .ok_or(SymbolContextError::Identity)?
                    .as_bytes(),
            ),
            compiled_sha256: plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
                &serde_json::to_value(&bundle.artifact().comp)?,
            )?),
            plan_id: plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
                &bundle.artifact().comp,
            )?),
            compiler_version: env!("CARGO_PKG_VERSION").into(),
            comp_wire_version: plasm_core::plasm_monad::PLASM_COMP_WIRE_VERSION,
            catalog_fingerprints: catalogs,
            ledger_bytes: ledger.encode()?,
            python_teaching: PythonSymbolAuthority::Captured(python_teaching),
            dependencies: crate::plasm_dag::dependencies::DependencyEvidence::capture(
                session,
                &bundle.artifact().comp,
            ),
            disclosure: match bundle.python_operands() {
                Some(operands) => {
                    use crate::plasm_dag::python::operand_evidence::OperandDisclosure;
                    let ranges = |kind| {
                        operands
                            .iter()
                            .filter(|operand| operand.disclosure == kind)
                            .map(|operand| {
                                (operand.range.start().into(), operand.range.end().into())
                            })
                            .collect()
                    };
                    CapturedDisclosure::Classified {
                        embedded_literals: ranges(OperandDisclosure::EmbeddedLiteral),
                        unresolved_dataflow: ranges(OperandDisclosure::UnresolvedSensitiveDataflow),
                    }
                }
                None => CapturedDisclosure::Unavailable,
            },
        })
    }
    pub(crate) fn dependencies(&self) -> &crate::plasm_dag::dependencies::DependencyEvidence {
        &self.dependencies
    }

    pub(crate) fn restore_symbol_authority(
        &self,
        template: &ExecuteSession,
    ) -> Result<ExecuteSession, SymbolContextError> {
        let catalogs: indexmap::IndexMap<_, _> = template
            .contexts_by_entry
            .iter()
            .filter(|(entry, _)| self.catalog_fingerprints.contains_key(*entry))
            .map(|(entry, context)| (entry.clone(), context.cgs.clone()))
            .collect();
        let ledger = plasm_core::PersistedSymbolLedger::decode(&self.ledger_bytes)?;
        if template.tenant_scope != self.tenant_scope {
            return Err(SymbolContextError::Identity);
        }
        let context_pins: indexmap::IndexMap<_, _> = self
            .catalog_fingerprints
            .iter()
            .map(|(entry, hash)| (entry.clone(), hash.clone()))
            .collect();
        if !plasm_core::catalog_pins_match(&context_pins, &catalogs)
            || ledger
                .catalog_cgs_hashes
                .iter()
                .any(|(entry, hash)| self.catalog_fingerprints.get(entry) != Some(hash))
        {
            return Err(SymbolContextError::CatalogDrift);
        }
        let symbol_catalogs: indexmap::IndexMap<_, _> = catalogs
            .iter()
            .filter(|(entry, _)| ledger.catalog_cgs_hashes.contains_key(*entry))
            .map(|(entry, cgs)| (entry.clone(), cgs.clone()))
            .collect();
        if !plasm_core::catalog_pins_match(&ledger.catalog_cgs_hashes, &symbol_catalogs) {
            return Err(SymbolContextError::CatalogDrift);
        }
        let mut session = template.clone();
        session
            .contexts_by_entry
            .retain(|entry, _| self.catalog_fingerprints.contains_key(entry));
        session
            .compiled_catalogs_by_entry
            .retain(|entry, _| self.catalog_fingerprints.contains_key(entry));
        session
            .bindings_by_entry
            .retain(|entry, _| self.catalog_fingerprints.contains_key(entry));
        let primary = session
            .contexts_by_entry
            .get(&session.entry_id)
            .ok_or(SymbolContextError::Identity)?;
        session.cgs = primary.cgs.clone();
        session.teaching_exposure = Some(ledger.hydrate(&symbol_catalogs)?);
        session.python_teaching = match &self.python_teaching {
            PythonSymbolAuthority::Captured(state) => state.clone(),
            PythonSymbolAuthority::Unavailable => return Err(SymbolContextError::Unavailable),
        };
        session.prompt_hash = self.prompt_hash.clone();
        session.domain_revision = self.domain_revision;
        isolate_repair_materialization(&mut session);
        Ok(session)
    }
    pub fn sensitive_operand_ranges(&self) -> Option<&[(u32, u32)]> {
        match &self.disclosure {
            CapturedDisclosure::Classified {
                embedded_literals, ..
            } => Some(embedded_literals),
            CapturedDisclosure::Unavailable => None,
        }
    }
    pub fn unresolved_sensitive_dataflow_ranges(&self) -> Option<&[(u32, u32)]> {
        match &self.disclosure {
            CapturedDisclosure::Classified {
                unresolved_dataflow,
                ..
            } => Some(unresolved_dataflow),
            CapturedDisclosure::Unavailable => None,
        }
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn compiled_sha256(&self) -> &str {
        &self.compiled_sha256
    }
    pub fn ledger_bytes(&self) -> &[u8] {
        &self.ledger_bytes
    }
    pub fn catalog_fingerprints(&self) -> &BTreeMap<String, String> {
        &self.catalog_fingerprints
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolContextUnavailable {
    HistoricalMissing,
    CompilationNotCaptured,
    InvalidExecuteSession,
    CaptureFailed { diagnostic: String },
}

/// Compiler provenance carried unchanged through reviewed plan persistence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum CompilationSymbolContext {
    Captured(Box<CompilationSymbols>),
    Unavailable(SymbolContextUnavailable),
}
impl Default for CompilationSymbolContext {
    fn default() -> Self {
        Self::Unavailable(SymbolContextUnavailable::CompilationNotCaptured)
    }
}
impl CompilationSymbolContext {
    pub fn captured(&self) -> Option<&CompilationSymbols> {
        match self {
            Self::Captured(value) => Some(value),
            Self::Unavailable(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SavedProgramContext {
    Captured {
        scope: crate::workflow_learning::model::LibraryScope,
        logical_session: LogicalSessionId,
        execute_session: ExecuteSessionId,
        compilation: Box<CompilationSymbols>,
    },
    Unavailable {
        reason: SymbolContextUnavailable,
    },
}
impl Default for SavedProgramContext {
    fn default() -> Self {
        Self::Unavailable {
            reason: SymbolContextUnavailable::HistoricalMissing,
        }
    }
}
impl SavedProgramContext {
    pub fn is_historical_missing(&self) -> bool {
        matches!(
            self,
            Self::Unavailable {
                reason: SymbolContextUnavailable::HistoricalMissing,
            }
        )
    }
    pub fn captured(&self) -> Option<&CompilationSymbols> {
        match self {
            Self::Captured { compilation, .. } => Some(compilation),
            Self::Unavailable { .. } => None,
        }
    }
    pub(crate) fn from_compilation(
        bundle: &PlasmCompBundle,
        scope: &crate::workflow_learning::model::LibraryScope,
        logical_session: &LogicalSessionId,
        execute_session: &str,
    ) -> Self {
        let Ok(execute_session) = execute_session.parse::<ExecuteSessionId>() else {
            return Self::Unavailable {
                reason: SymbolContextUnavailable::InvalidExecuteSession,
            };
        };
        match bundle.compilation_symbols() {
            CompilationSymbolContext::Captured(compilation) => Self::Captured {
                scope: scope.clone(),
                logical_session: *logical_session,
                execute_session,
                compilation: compilation.clone(),
            },
            CompilationSymbolContext::Unavailable(reason) => Self::Unavailable {
                reason: reason.clone(),
            },
        }
    }

    /// Repair validation only: this produces no execution receipt, review or publication.
    pub(crate) async fn recompile(
        &self,
        receipt: &crate::workflow_learning::model::ExecutionReceipt,
        template: &ExecuteSession,
    ) -> Result<PlasmCompBundle, SymbolContextError> {
        let Self::Captured {
            scope,
            logical_session,
            execute_session,
            compilation,
        } = self
        else {
            return Err(SymbolContextError::Unavailable);
        };
        if scope != &receipt.scope
            || logical_session != &receipt.logical_session
            || execute_session != &receipt.execute_session
            || crate::workflow_learning::learning_tenant(&compilation.tenant_scope) != scope.tenant
            || template.tenant_scope != compilation.tenant_scope
            || receipt.prompt_hash.as_str() != compilation.prompt_hash
            || receipt.catalog_fingerprints != compilation.catalog_fingerprints
            || receipt.plan_id.as_str() != compilation.plan_id
            || plasm_core::catalog_discovery::content_hash(receipt.source.as_bytes())
                != compilation.source_sha256
            || plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
                &receipt.compiled_plan,
            )?) != compilation.compiled_sha256
        {
            return Err(SymbolContextError::Identity);
        }
        if compilation.compiler_version != env!("CARGO_PKG_VERSION")
            || compilation.comp_wire_version != plasm_core::plasm_monad::PLASM_COMP_WIRE_VERSION
        {
            return Err(SymbolContextError::Version);
        }
        let session = compilation.restore_symbol_authority(template)?;
        let compiled = crate::plasm_compile::compile_program(
            &Default::default(),
            None,
            &session,
            "saved_program_repair",
            &receipt.source,
        )
        .await
        .map_err(|error| SymbolContextError::Admission(Box::new(error)))?;
        if plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&serde_json::to_value(
            &compiled.artifact().comp,
        )?)?)
            != compilation.compiled_sha256
        {
            return Err(SymbolContextError::Identity);
        }
        Ok(compiled)
    }
}

fn isolate_repair_materialization(session: &mut ExecuteSession) {
    session.core = std::sync::Arc::new(crate::execute_session::SessionCore::new());
    session.graph_cache = session.core.graph_cache();
    session.recorded_outputs = std::sync::Arc::new(Default::default());
    session.evidence_chain = crate::evidence_chain::new_evidence_chain_slot();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_learning::model::{ExecutionReceipt, LibraryScope};
    use crate::workflow_learning::{model::CompiledProgramId, test_identities as ids};

    fn receipt() -> (ExecutionReceipt, ExecuteSession) {
        let session = crate::workflow_learning::teaching::tests::session(&["LangItem", "LangLine"]);
        let source = crate::workflow_learning::teaching::tests::source(&session);
        let compiled = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
        let scope = LibraryScope {
            tenant: crate::workflow_learning::learning_tenant(&session.tenant_scope),
            project: "saved-program-test".into(),
        };
        let saved = SavedProgramContext::from_compilation(
            &compiled,
            &scope,
            &ids::logical("logical"),
            ids::execute("execute").as_str(),
        );
        assert!(saved.captured().is_some(), "{saved:?}");
        let receipt = ExecutionReceipt {
            scope,
            logical_session: ids::logical("logical"),
            session_revision: 0,
            run_id: ids::run("test-run"),
            plan_id: CompiledProgramId::from_comp(&compiled.artifact().comp).unwrap(),
            prompt_hash: session.prompt_hash.parse().unwrap(),
            execute_session: ids::execute("execute"),
            run_artifact: serde_json::json!({}),
            source,
            compiled_plan: serde_json::to_value(&compiled.artifact().comp).unwrap(),
            observation: Default::default(),
            catalogue_requirements: Default::default(),
            catalog_fingerprints: saved.captured().unwrap().catalog_fingerprints().clone(),
            symbol_context: saved,
        };
        (receipt, session)
    }

    #[tokio::test]
    async fn serialized_original_ledger_replays_after_session_exposure_changes_without_effects() {
        let (receipt, mut session) = receipt();
        let original = receipt
            .symbol_context
            .captured()
            .unwrap()
            .ledger_bytes()
            .to_vec();
        let reordered =
            crate::workflow_learning::teaching::tests::session(&["LangLine", "LangItem"]);
        session.teaching_exposure = reordered.teaching_exposure;
        let restored: ExecutionReceipt =
            serde_json::from_slice(&serde_json::to_vec(&receipt).unwrap()).unwrap();
        let compiled = restored
            .recompile_for_repair(&receipt.scope, &session)
            .await
            .unwrap();
        assert_eq!(compiled.source(), Some(receipt.source.as_str()));
        let expected: plasm_core::PlasmComp =
            serde_json::from_value(receipt.compiled_plan.clone()).unwrap();
        assert!(plasm_core::comp_semantic_eq(
            &expected,
            &compiled.artifact().comp
        ));
        assert_eq!(
            original,
            restored.symbol_context.captured().unwrap().ledger_bytes()
        );
        assert!(
            matches!(
                restored.observation,
                crate::workflow_learning::teaching::ProgramObservation::Unavailable(_)
            ),
            "repair compilation must not promote execution evidence"
        );
    }

    #[tokio::test]
    async fn saved_context_rejects_changed_scope_source_plan_catalogue_and_version() {
        let (receipt, session) = receipt();
        let mut wrongscope = receipt.scope.clone();
        wrongscope.project = "other".into();
        assert!(matches!(
            receipt.recompile_for_repair(&wrongscope, &session).await,
            Err(SymbolContextError::Identity)
        ));
        let mut changed = receipt.clone();
        changed.source.push_str("\n# tampered");
        assert!(matches!(
            changed.recompile_for_repair(&receipt.scope, &session).await,
            Err(SymbolContextError::Identity)
        ));
        let mut changed = receipt.clone();
        changed.compiled_plan["name"] = serde_json::json!("different-plan");
        assert!(matches!(
            changed.recompile_for_repair(&receipt.scope, &session).await,
            Err(SymbolContextError::Identity)
        ));
        let mut changed = receipt.clone();
        changed.logical_session = ids::logical("other");
        assert!(matches!(
            changed.recompile_for_repair(&receipt.scope, &session).await,
            Err(SymbolContextError::Identity)
        ));
        let mut changed_session = session.clone();
        changed_session.contexts_by_entry.clear();
        assert!(matches!(
            receipt
                .recompile_for_repair(&receipt.scope, &changed_session)
                .await,
            Err(SymbolContextError::CatalogDrift)
        ));
        let mut changed = receipt.clone();
        let SavedProgramContext::Captured { compilation, .. } = &mut changed.symbol_context else {
            panic!("captured")
        };
        compilation.compiler_version = "different-version".into();
        assert!(matches!(
            changed.recompile_for_repair(&receipt.scope, &session).await,
            Err(SymbolContextError::Version)
        ));
    }

    #[tokio::test]
    async fn historical_missing_field_preserves_receipt_json_and_refuses_repair() {
        let (receipt, session) = receipt();
        let mut old = serde_json::to_value(&receipt).unwrap();
        old.as_object_mut().unwrap().remove("symbol_context");
        let decoded: ExecutionReceipt = serde_json::from_value(old.clone()).unwrap();
        assert!(decoded.symbol_context.is_historical_missing());
        assert_eq!(serde_json::to_value(&decoded).unwrap(), old);
        assert!(matches!(
            decoded.recompile_for_repair(&decoded.scope, &session).await,
            Err(SymbolContextError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn repair_prefetch_cannot_borrow_a_live_recorded_output() {
        let (_, mut session) = receipt();
        let id = crate::run_artifacts::RunArtifactId::from_bytes([7; 32]);
        session
            .core
            .append_run_artifact(
                id,
                crate::execute_session::GraphEpoch(0),
                1,
                crate::run_artifacts::ArtifactPayload {
                    metadata: crate::run_artifacts::ArtifactPayloadMetadata::json_default(),
                    bytes: axum::body::Bytes::from_static(br#"{"ok":true}"#),
                },
            )
            .await;
        let source = format!(
            "class P(Program):\n    def build(self):\n        return self.recorded('{}')\n",
            id.to_wire()
        );
        let live = session.recorded_output_snapshot(&source).await;
        assert!(live.recorded_outputs.get(&id.to_wire()).unwrap().is_ok());
        isolate_repair_materialization(&mut session);
        let isolated = session.recorded_output_snapshot(&source).await;
        assert!(isolated
            .recorded_outputs
            .get(&id.to_wire())
            .unwrap()
            .is_err());
        assert!(crate::plasm_compile::compile_program(
            &Default::default(),
            None,
            &session,
            "repair",
            &source,
        )
        .await
        .is_err());
    }

    #[test]
    fn capture_pins_exposed_ledger_catalogues_not_unexposed_loaded_catalogues() {
        let mut session = crate::workflow_learning::teaching::tests::session(&["LangItem"]);
        session.contexts_by_entry.insert(
            "loaded_unexposed".into(),
            std::sync::Arc::new(plasm_core::CgsContext::entry(
                "loaded_unexposed",
                session.cgs.clone(),
            )),
        );
        let source = crate::workflow_learning::teaching::tests::source(&session);
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
        let captured = bundle
            .compilation_symbols()
            .captured()
            .expect("extra loaded catalogue must not invalidate exposed ledger");
        assert_eq!(
            captured
                .catalog_fingerprints()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["langmatrix", "loaded_unexposed"]
        );
        let ledger = plasm_core::PersistedSymbolLedger::decode(captured.ledger_bytes()).unwrap();
        assert_eq!(ledger.catalog_cgs_hashes.len(), 1);
        assert!(
            crate::workflow_learning::teaching::ProgramObservation::from_execution(
                &session, &source, &bundle
            )
            .available()
            .is_some()
        );
        let mut changed = (*session.cgs).clone();
        changed
            .capabilities
            .get_mut("langitem_query")
            .unwrap()
            .description
            .push_str(" changed");
        session.contexts_by_entry.insert(
            "langmatrix".into(),
            std::sync::Arc::new(plasm_core::CgsContext::entry(
                "langmatrix",
                std::sync::Arc::new(changed),
            )),
        );
        assert!(matches!(
            CompilationSymbols::capture(&session, &bundle),
            Err(SymbolContextError::CatalogDrift)
        ));
        session.contexts_by_entry.shift_remove("langmatrix");
        assert!(matches!(
            CompilationSymbols::capture(&session, &bundle),
            Err(SymbolContextError::CatalogDrift)
        ));
    }

    #[tokio::test]
    async fn original_compute_context_is_restored_without_exposing_its_symbols() {
        let mut session = crate::workflow_learning::teaching::tests::session(&["LangItem"]);
        session.contexts_by_entry.insert(
            "a_unexposed".into(),
            std::sync::Arc::new(plasm_core::CgsContext::entry(
                "a_unexposed",
                session.cgs.clone(),
            )),
        );
        let source = "class Example(Program):\n    def build(self):\n        rows = e1.query().take(2)\n        return rows.where(lambda row: row.title == \"fixture\")\n";
        let bundle = crate::plasm_compile::compile_program(
            &Default::default(),
            None,
            &session,
            "test",
            source,
        )
        .await
        .unwrap();
        let serialized = serde_json::to_value(&bundle.artifact().comp).unwrap();
        fn has_ambient_compute(value: &serde_json::Value) -> bool {
            match value {
                serde_json::Value::Object(object) => {
                    (object.get("entry_id").and_then(serde_json::Value::as_str)
                        == Some("a_unexposed")
                        && object.contains_key("language_profile"))
                        || object.values().any(has_ambient_compute)
                }
                serde_json::Value::Array(values) => values.iter().any(has_ambient_compute),
                _ => false,
            }
        }
        assert!(
            has_ambient_compute(&serialized),
            "fixture must exercise a compiler-pinned ambient context"
        );
        let captured = bundle.compilation_symbols().captured().unwrap();
        assert!(captured.catalog_fingerprints().contains_key("a_unexposed"));
        let ledger = plasm_core::PersistedSymbolLedger::decode(captured.ledger_bytes()).unwrap();
        assert!(!ledger.catalog_cgs_hashes.contains_key("a_unexposed"));
        let restored = captured.restore_symbol_authority(&session).unwrap();
        assert!(restored.contexts_by_entry.contains_key("a_unexposed"));
        assert!(!plasm_core::catalog_cgs_hashes_from_session(
            restored.teaching_exposure.as_ref().unwrap()
        )
        .contains_key("a_unexposed"));
        let observation = crate::workflow_learning::teaching::ProgramObservation::from_execution(
            &restored, source, &bundle,
        );
        assert!(observation.available().is_some(), "{observation:?}");
        let requirements =
            crate::workflow_learning::teaching::ProgramCatalogueRequirements::from_compiled(
                &restored, &bundle,
            )
            .unwrap();
        assert!(!requirements
            .catalog_fingerprints()
            .contains_key("a_unexposed"));
        let checked = crate::plasm_compile::compile_program(
            &Default::default(),
            None,
            &restored,
            "test",
            source,
        )
        .await
        .unwrap();
        assert!(plasm_core::comp_semantic_eq(
            &bundle.artifact().comp,
            &checked.artifact().comp
        ));
        let mut missing = session.clone();
        missing.contexts_by_entry.shift_remove("a_unexposed");
        assert!(matches!(
            captured.restore_symbol_authority(&missing),
            Err(SymbolContextError::CatalogDrift)
        ));
    }

    #[test]
    fn reviewed_plan_cache_roundtrip_retains_original_compilation_context() {
        let session = crate::workflow_learning::teaching::tests::session(&["LangItem", "LangLine"]);
        let source = crate::workflow_learning::teaching::tests::source(&session);
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
        let dry = crate::plasm_plan_run::evaluate_plasm_comp_dry(&session, &bundle).unwrap();
        let cache = crate::operation::PlanCommitDryCache::from_dry(&dry);
        let restored: crate::operation::PlanCommitDryCache =
            serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        let reconstructed = PlasmCompBundle::new(bundle.artifact().clone())
            .unwrap()
            .with_source(source)
            .with_compilation_symbols(restored.compilation_symbols.clone());
        assert!(reconstructed.compilation_symbols().captured().is_some());
        assert_eq!(
            reconstructed.compilation_symbols(),
            bundle.compilation_symbols()
        );
    }

    #[test]
    fn compilation_snapshot_is_immutable_even_when_live_exposure_changes() {
        let mut session =
            crate::workflow_learning::teaching::tests::session(&["LangItem", "LangLine"]);
        let source = crate::workflow_learning::teaching::tests::source(&session);
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
        let saved = bundle
            .compilation_symbols()
            .captured()
            .unwrap()
            .ledger_bytes()
            .to_vec();
        session.teaching_exposure =
            crate::workflow_learning::teaching::tests::session(&["LangLine", "LangItem"])
                .teaching_exposure;
        let new = CompilationSymbols::capture(&session, &bundle).unwrap();
        assert_ne!(saved, new.ledger_bytes());
        assert_eq!(
            saved,
            bundle
                .compilation_symbols()
                .captured()
                .unwrap()
                .ledger_bytes()
        );
    }
}
