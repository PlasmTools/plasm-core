//! Compiler-checked, catalog-qualified relocation of Python workflow examples.

use std::collections::{BTreeMap, BTreeSet};

use plasm_core::{SymbolMap, SymbolSession};
use ruff_python_ast::{
    visitor::{self, Visitor},
    Expr, ExprContext, Parameter, Stmt,
};
use ruff_text_size::{Ranged, TextRange};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    execute_session::ExecuteSession,
    plasm_comp_bundle::PlasmCompBundle,
    symbol_map_resolve::{resolve_session_symbol_map, SessionSymbolMapContext},
    workflow_manifest::WorkflowSeed,
};

#[path = "teaching_catalogue_context.rs"]
mod catalogue_context;
pub use catalogue_context::CapabilityTeachingContract;

pub(super) const TEACHING_SCHEMA_VERSION: u32 = 3;
#[path = "teaching_contracts.rs"]
mod contracts;
#[path = "teaching_display.rs"]
mod display;
pub use contracts::{ProgramCatalogueRequirements, TeachingOutput};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticReference {
    ValueDomain {
        entry_id: String,
        value_ref: String,
    },
    Entity {
        entry_id: String,
        entity: String,
    },
    Method {
        entry_id: String,
        entity: String,
        capability: String,
    },
    Relation {
        entry_id: String,
        entity: String,
        relation: String,
    },
}

impl SemanticReference {
    pub fn catalog(&self) -> &str {
        match self {
            Self::Entity { entry_id, .. }
            | Self::Method { entry_id, .. }
            | Self::Relation { entry_id, .. }
            | Self::ValueDomain { entry_id, .. } => entry_id,
        }
    }
    pub fn entity_owner(&self) -> Option<(&str, &str)> {
        match self {
            Self::Entity { entry_id, entity }
            | Self::Method {
                entry_id, entity, ..
            }
            | Self::Relation {
                entry_id, entity, ..
            } => Some((entry_id, entity)),
            Self::ValueDomain { .. } => None,
        }
    }

    /// Forward lookup is checked by reverse lookup: render helpers may return a
    /// canonical label when a symbol was never exposed; that is not admission.
    pub fn resolve(&self, symbols: &dyn SymbolSession) -> Result<String, TeachingError> {
        let (entry, entity) = self
            .entity_owner()
            .ok_or_else(|| TeachingError::UnexposedReference(self.clone()))?;
        let token = match self {
            Self::ValueDomain { .. } => {
                return Err(TeachingError::UnexposedReference(self.clone()))
            }
            Self::Entity { .. } => symbols.entity_sym_for(entry, entity),
            Self::Method { capability, .. } => symbols.method_sym_for(entry, entity, capability),
            Self::Relation { relation, .. } => {
                symbols.ident_sym_relation_for(entry, entity, relation)
            }
        };
        if reference_for_token(symbols, &token)?.as_ref() != Some(self) {
            return Err(TeachingError::UnexposedReference(self.clone()));
        }
        Ok(token)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolOccurrence {
    pub start: u32,
    pub end: u32,
    pub expected: String,
    pub reference: SemanticReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachingFormat {
    WholeProgramExample,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingProgram {
    pub format: TeachingFormat,
    pub schema_version: u32,
    pub compiler_version: String,
    pub comp_wire_version: u32,
    pub source: String,
    pub occurrences: Vec<SymbolOccurrence>,
    pub dependencies: Vec<WorkflowSeed>,
    pub structural_fingerprint: String,
    pub catalog_fingerprints: BTreeMap<String, String>,
    pub semantic_contracts: BTreeMap<String, String>,
    pub required_capabilities: Vec<plasm_core::prerequisites::CapabilityRef>,
    pub outputs: Vec<TeachingOutput>,
}

#[derive(Debug, Error)]
pub enum TeachingError {
    #[error("Python teaching type resolution failed: {0}")]
    PythonTeaching(#[source] Box<plasm_core::prompt_render::python::PythonTeachingError>),
    #[error("compiler dependency evidence unavailable: {0}")]
    Dependencies(#[from] crate::program_symbol_context::DependencyUnavailable),
    #[error("catalogue type contract cannot be resolved: {0}")]
    Schema(#[source] Box<plasm_core::SchemaError>),
    #[error("original compilation context cannot be restored: {0}")]
    SymbolContext(#[source] Box<crate::program_symbol_context::SymbolContextError>),
    #[error("portable Python cannot be parsed: {0}")]
    Parse(#[source] Box<ruff_python_parser::ParseError>),
    #[error("session symbol cannot be resolved: {0}")]
    SymbolResolution(#[source] Box<plasm_core::symbol_tuning::SymbolResolveError>),
    #[error("semantic reference is not exposed: {0}")]
    UnexposedReference(SemanticReference),
    #[error("admitted plan is missing compiler dependency evidence")]
    MissingCompilerDependency,
    #[error("authored program contains an ambiguous symbol binding or external session reference")]
    HiddenState,
    #[error("portable program occurrence ledger is invalid")]
    InvalidLedger,
    #[error("portable program has no catalog dependencies")]
    NoDependencies,
    #[error("catalog contract changed or is unavailable: {0}")]
    CatalogDrift(String),
    #[error("portable program failed normal Python admission: {0}")]
    ProgramAdmission(#[source] Box<crate::program_diagnostic::ProgramStageError>),
    #[error("portable program compilation failed: {0}")]
    Compilation(#[source] Box<crate::compilation_error::CompilationError>),
    #[error("portable output contract could not be derived: {0}")]
    OutputContract(#[source] Box<crate::map_body_schema::MapBodySchemaError>),
    #[error("rewritten program does not preserve the compiled computation")]
    ChangedComputation,
    #[error(
        "compiled preservation failed: {category}; original={original_hash}; recompiled={recompiled_hash}; path={}", display::PreservationLocation(path)
    )]
    PreservationMismatch {
        category: PreservationCategory,
        original_hash: String,
        recompiled_hash: String,
        path: Vec<PreservationPath>,
    },
    #[error("catalogue prerequisite resolution failed: {0}")]
    Prerequisite(#[source] Box<plasm_core::prerequisites::PrerequisiteError>),
    #[error("authored program observation is unavailable")]
    ObservationUnavailable,
    #[error("sensitive argument source origin is unresolved at bytes {start}..{end}")]
    UnresolvedSensitiveDataflow { start: u32, end: u32 },
    #[error("sensitive literal cannot be retained in a portable example at bytes {start}..{end}")]
    SensitiveOperand { start: u32, end: u32 },
    #[error("portable representation or compiler version changed; revalidation is required")]
    NeedsRevalidation,
}

fn reference_for_token(
    symbols: &dyn SymbolSession,
    token: &str,
) -> Result<Option<SemanticReference>, TeachingError> {
    let failure = |e: plasm_core::symbol_tuning::SymbolResolveError| {
        TeachingError::SymbolResolution(Box::new(e))
    };
    if SymbolMap::is_opaque_e_sym(token) {
        let b = symbols.resolve_session_entity(token).map_err(failure)?;
        Ok(Some(SemanticReference::Entity {
            entry_id: b.entry_id.to_string(),
            entity: b.entity.to_string(),
        }))
    } else if SymbolMap::is_opaque_m_sym(token) {
        let b = symbols.resolve_session_method(token).map_err(failure)?;
        Ok(Some(SemanticReference::Method {
            entry_id: b.entry_id.to_string(),
            entity: b.domain.to_string(),
            capability: b.capability.to_string(),
        }))
    } else if token
        .strip_prefix('r')
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        let b = symbols.resolve_session_relation(token).map_err(failure)?;
        Ok(Some(SemanticReference::Relation {
            entry_id: b.entry_id.to_string(),
            entity: b.source_entity.to_string(),
            relation: b.relation_wire.to_string(),
        }))
    } else {
        Ok(None)
    }
}

fn opaque(token: &str) -> bool {
    token
        .as_bytes()
        .first()
        .is_some_and(|c| matches!(c, b'e' | b'm' | b'r'))
        && token.len() > 1
        && token.as_bytes()[1..].iter().all(u8::is_ascii_digit)
}

fn value_domains(
    session: &ExecuteSession,
) -> Result<BTreeMap<String, SemanticReference>, TeachingError> {
    let exposure = session
        .teaching_exposure
        .as_ref()
        .ok_or(TeachingError::HiddenState)?;
    let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
        exposure,
        &session.python_teaching,
    )
    .map_err(|error| TeachingError::PythonTeaching(Box::new(error)))?;
    Ok(wave
        .value_contracts
        .into_iter()
        .filter_map(|(symbol, contract)| {
            contract.domain.map(|domain| {
                (
                    symbol,
                    SemanticReference::ValueDomain {
                        entry_id: domain.entry_id,
                        value_ref: domain.value_ref.to_string(),
                    },
                )
            })
        })
        .collect())
}

struct References<'a> {
    value_domains: BTreeMap<String, SemanticReference>,
    in_annotation: bool,
    symbols: &'a dyn SymbolSession,
    occurrences: Vec<SymbolOccurrence>,
    error: Option<TeachingError>,
}

impl References<'_> {
    fn capture(&mut self, token: &str, range: TextRange) {
        match reference_for_token(self.symbols, token) {
            Ok(Some(reference)) => self.occurrences.push(SymbolOccurrence {
                start: range.start().into(),
                end: range.end().into(),
                expected: token.into(),
                reference,
            }),
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
    }
}

impl<'a> Visitor<'a> for References<'_> {
    fn visit_annotation(&mut self, expression: &'a Expr) {
        // Stringized annotations hide identifier occurrences from this walker.
        // Retain such candidates for repair rather than guessing their binding.
        if matches!(expression, Expr::StringLiteral(_)) {
            self.error = Some(TeachingError::HiddenState);
        }
        let previous = self.in_annotation;
        self.in_annotation = true;
        visitor::walk_annotation(self, expression);
        self.in_annotation = previous;
    }
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        let binding = match stmt {
            Stmt::FunctionDef(def) => Some(def.name.as_str()),
            Stmt::ClassDef(def) => Some(def.name.as_str()),
            _ => None,
        };
        if binding.is_some_and(opaque) {
            self.error = Some(TeachingError::HiddenState);
        }
        visitor::walk_stmt(self, stmt);
    }
    fn visit_parameter(&mut self, parameter: &'a Parameter) {
        if opaque(parameter.name.as_str()) {
            self.error = Some(TeachingError::HiddenState);
        }
        visitor::walk_parameter(self, parameter);
    }
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name)
                if self.in_annotation && self.value_domains.contains_key(name.id.as_str()) =>
            {
                self.occurrences.push(SymbolOccurrence {
                    start: name.range().start().into(),
                    end: name.range().end().into(),
                    expected: name.id.to_string(),
                    reference: self.value_domains[name.id.as_str()].clone(),
                });
            }
            Expr::Call(call) if matches!(call.func.as_ref(), Expr::Attribute(attr) if SymbolMap::is_opaque_m_sym(attr.attr.as_str())) =>
            {
                let Expr::Attribute(attr) = call.func.as_ref() else {
                    unreachable!()
                };
                self.capture(attr.attr.as_str(), attr.attr.range());
                self.visit_expr(&attr.value);
                self.visit_arguments(&call.arguments);
                return;
            }
            Expr::Name(name) if opaque(name.id.as_str()) => {
                if name.ctx != ExprContext::Load || !SymbolMap::is_opaque_e_sym(name.id.as_str()) {
                    self.error = Some(TeachingError::HiddenState);
                } else {
                    self.capture(name.id.as_str(), name.range());
                }
            }
            Expr::Attribute(attr) => {
                if matches!(attr.attr.as_str(), "recorded" | "bindings")
                    && matches!(attr.value.as_ref(), Expr::Name(name) if name.id.as_str() == "self")
                {
                    self.error = Some(TeachingError::HiddenState);
                }
                if SymbolMap::is_opaque_m_sym(attr.attr.as_str())
                    || SymbolMap::is_opaque_e_sym(attr.attr.as_str())
                {
                    self.error = Some(TeachingError::HiddenState);
                } else if opaque(attr.attr.as_str()) {
                    self.capture(attr.attr.as_str(), attr.attr.range());
                }
            }
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreservationCategory {
    ApprovalGates,
    CanonicalComputation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreservationPath {
    Field(String),
    Index(usize),
}

#[cfg(test)]
fn preservation_difference(
    original: &serde_json::Value,
    replacement: &serde_json::Value,
) -> Vec<PreservationPath> {
    use serde_json::Value;
    let mut pending = vec![(Vec::new(), original, replacement)];
    while let Some((path, left, right)) = pending.pop() {
        if left == right {
            continue;
        }
        match (left, right) {
            (Value::Object(left), Value::Object(right)) if left.keys().eq(right.keys()) => {
                for (key, value) in left.iter().rev() {
                    let Some(other) = right.get(key) else {
                        return path;
                    };
                    let mut next = path.clone();
                    next.push(PreservationPath::Field(key.clone()));
                    pending.push((next, value, other));
                }
            }
            (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
                for (index, (value, other)) in left.iter().zip(right).enumerate().rev() {
                    let mut next = path.clone();
                    next.push(PreservationPath::Index(index));
                    pending.push((next, value, other));
                }
            }
            _ => return path,
        }
    }
    Vec::new()
}

#[cfg(test)]
fn preservation_error(original: &PlasmCompBundle, replacement: &PlasmCompBundle) -> TeachingError {
    let (category, original, replacement) =
        if original.artifact().approval_gates != replacement.artifact().approval_gates {
            (
                PreservationCategory::ApprovalGates,
                serde_json::json!(original.artifact().approval_gates),
                serde_json::json!(replacement.artifact().approval_gates),
            )
        } else {
            (
                PreservationCategory::CanonicalComputation,
                plasm_core::plasm_comp_commit_canonical(&original.artifact().comp),
                plasm_core::plasm_comp_commit_canonical(&replacement.artifact().comp),
            )
        };
    TeachingError::PreservationMismatch {
        category,
        original_hash: hex::encode(Sha256::digest(original.to_string().as_bytes())),
        recompiled_hash: hex::encode(Sha256::digest(replacement.to_string().as_bytes())),
        path: preservation_difference(&original, &replacement),
    }
}

pub fn compiled_preserves(original: &PlasmCompBundle, replacement: &PlasmCompBundle) -> bool {
    original.artifact().approval_gates == replacement.artifact().approval_gates
        && plasm_core::comp_semantic_eq(&original.artifact().comp, &replacement.artifact().comp)
}

pub(super) fn validation_session(session: &ExecuteSession) -> ExecuteSession {
    let mut isolated = session.clone();
    isolated.evidence_chain = crate::evidence_chain::new_evidence_chain_slot();
    isolated.core = std::sync::Arc::new(crate::execute_session::SessionCore::new());
    isolated.graph_cache = isolated.core.graph_cache();
    isolated.recorded_outputs = std::sync::Arc::new(Default::default());
    isolated
}

impl TeachingProgram {
    /// Conservative behavioral comparability: exact canonical AST after semantic
    /// symbol relocation. Predicates,
    /// effect order, bounds, and all authored literals remain significant.
    /// Different hashes require a separate preservation proof before replacement.
    pub fn behavior_fingerprint(&self) -> &str {
        &self.structural_fingerprint
    }

    /// Caller supplies the just-admitted bundle for this source and session.
    /// Source/bundle correspondence is verified again to avoid trusting a stale caller.
    pub fn from_compiled(
        session: &ExecuteSession,
        source: &str,
        bundle: &PlasmCompBundle,
    ) -> Result<Self, TeachingError> {
        if bundle.source() != Some(source) || source.trim().is_empty() {
            return Err(TeachingError::ChangedComputation);
        }
        let captured = bundle
            .compilation_symbols()
            .captured()
            .ok_or(TeachingError::ObservationUnavailable)?;
        let compiled_hash = plasm_core::catalog_discovery::content_hash(
            &serde_json::to_vec(
                &serde_json::to_value(&bundle.artifact().comp)
                    .map_err(|_| TeachingError::InvalidLedger)?,
            )
            .map_err(|_| TeachingError::InvalidLedger)?,
        );
        if captured.source_sha256()
            != plasm_core::catalog_discovery::content_hash(source.as_bytes())
            || captured.compiled_sha256() != compiled_hash
        {
            return Err(TeachingError::InvalidLedger);
        }
        let original_session = captured
            .restore_symbol_authority(session)
            .map_err(|error| TeachingError::SymbolContext(Box::new(error)))?;
        let session = &original_session;
        let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
            session,
            cross_cache: None,
        });
        let mut portable = Self::extract(source, symbols.as_ref(), session)?;
        portable.complete_contracts(session, bundle)?;
        Ok(portable)
    }

    fn extract(
        source: &str,
        symbols: &dyn SymbolSession,
        session: &ExecuteSession,
    ) -> Result<Self, TeachingError> {
        let ast = ruff_python_parser::parse_module(source)
            .map_err(|e| TeachingError::Parse(Box::new(e)))?;
        let mut visitor = References {
            value_domains: value_domains(session)?,
            in_annotation: false,
            symbols,
            occurrences: Vec::new(),
            error: None,
        };
        for stmt in ast.suite() {
            visitor.visit_stmt(stmt);
        }
        if let Some(error) = visitor.error {
            return Err(error);
        }
        visitor.occurrences.sort_by_key(|o| o.start);
        let mut owners = BTreeSet::new();
        for item in &visitor.occurrences {
            let Some((entry, entity)) = item.reference.entity_owner() else {
                continue;
            };
            owners.insert((entry.to_owned(), entity.to_owned()));
            if matches!(item.reference, SemanticReference::Relation { .. }) {
                let binding = symbols
                    .resolve_session_relation(&item.expected)
                    .map_err(|e| TeachingError::SymbolResolution(Box::new(e)))?;
                owners.insert((entry.to_owned(), binding.target_entity.to_string()));
            }
        }
        if owners.is_empty() {
            return Err(TeachingError::NoDependencies);
        }
        let mut program = Self {
            format: TeachingFormat::WholeProgramExample,
            schema_version: TEACHING_SCHEMA_VERSION,
            compiler_version: env!("CARGO_PKG_VERSION").into(),
            comp_wire_version: plasm_core::PLASM_COMP_WIRE_VERSION,
            source: source.into(),
            occurrences: visitor.occurrences,
            dependencies: owners
                .into_iter()
                .map(|(entry_id, entity)| WorkflowSeed { entry_id, entity })
                .collect(),
            structural_fingerprint: String::new(),
            catalog_fingerprints: BTreeMap::new(),
            semantic_contracts: BTreeMap::new(),
            required_capabilities: Vec::new(),
            outputs: Vec::new(),
        };
        for dependency in &program.dependencies {
            let layers = crate::plasm_plan_run::session_cgs_layer_stack(session);
            let layer = layers
                .iter()
                .find(|layer| layer.entry_id() == Some(dependency.entry_id.as_str()))
                .ok_or_else(|| TeachingError::CatalogDrift(dependency.entry_id.clone()))?;
            program.catalog_fingerprints.insert(
                dependency.entry_id.clone(),
                layer.cgs().catalog_cgs_hash_hex(),
            );
        }
        program.structural_fingerprint = program.fingerprint()?;
        Ok(program)
    }

    pub(super) fn substitute(
        &self,
        resolve: impl Fn(&SemanticReference) -> Result<String, TeachingError>,
    ) -> Result<String, TeachingError> {
        let mut end = 0;
        for item in &self.occurrences {
            if item.start < end
                || item.start >= item.end
                || self.source.get(item.start as usize..item.end as usize)
                    != Some(item.expected.as_str())
            {
                return Err(TeachingError::InvalidLedger);
            }
            end = item.end;
        }
        let mut result = self.source.clone();
        for item in self.occurrences.iter().rev() {
            result.replace_range(
                item.start as usize..item.end as usize,
                &resolve(&item.reference)?,
            );
        }
        Ok(result)
    }

    fn fingerprint(&self) -> Result<String, TeachingError> {
        let source = self.substitute(|reference| {
            let encoded =
                serde_json::to_vec(reference).map_err(|_| TeachingError::InvalidLedger)?;
            Ok(format!(
                "catalog_ref_{}",
                hex::encode(Sha256::digest(encoded))
            ))
        })?;
        let ast = ruff_python_parser::parse_module(&source)
            .map_err(|e| TeachingError::Parse(Box::new(e)))?;
        let canonical = ast
            .suite()
            .iter()
            .map(monty::statement_source)
            .collect::<Vec<_>>()
            .join("\n");
        let bytes = serde_json::to_vec(&(self.format, canonical))
            .map_err(|_| TeachingError::InvalidLedger)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    pub fn rebind(&self, session: &ExecuteSession) -> Result<String, TeachingError> {
        Ok(self.rebind_teaching(session)?.source)
    }

    pub(super) fn witness_version_current(&self) -> bool {
        self.schema_version == TEACHING_SCHEMA_VERSION
            && self.compiler_version == env!("CARGO_PKG_VERSION")
            && self.comp_wire_version == plasm_core::PLASM_COMP_WIRE_VERSION
    }

    /// Relocate only compiler-resolved session symbols; literal source is unchanged.
    pub fn rebind_teaching(&self, session: &ExecuteSession) -> Result<Self, TeachingError> {
        let isolated = validation_session(session);
        let session = &isolated;
        if !self.witness_version_current() {
            return Err(TeachingError::NeedsRevalidation);
        }
        if self.fingerprint()? != self.structural_fingerprint {
            return Err(TeachingError::InvalidLedger);
        }
        let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
            session,
            cross_cache: None,
        });
        let domains = value_domains(session)?;
        let source = self.substitute(|reference| match reference {
            SemanticReference::ValueDomain { .. } => domains
                .iter()
                .find_map(|(symbol, candidate)| (candidate == reference).then(|| symbol.clone()))
                .ok_or_else(|| TeachingError::UnexposedReference(reference.clone())),
            _ => reference.resolve(symbols.as_ref()),
        })?;
        let compiled = crate::plasm_dag::compile_python_program_checked(session, &source)
            .map_err(|e| TeachingError::ProgramAdmission(Box::new(e)))?;
        let mut verified = Self::extract(&source, symbols.as_ref(), session)?;
        verified.structural_fingerprint = verified.fingerprint()?;
        verified.complete_contracts(session, &compiled)?;
        if verified.structural_fingerprint != self.structural_fingerprint
            || verified.dependencies != self.dependencies
            || verified.semantic_contracts != self.semantic_contracts
            || verified.required_capabilities != self.required_capabilities
        {
            return Err(TeachingError::InvalidLedger);
        }
        // Value-domain pins refer to distribution hashes. After semantic
        // contracts match, repin old output domains before exact type equality.
        let mut outputs = self.outputs.clone();
        for output in &mut outputs {
            contracts::repin_contract(
                &mut output.row_type,
                &self.catalog_fingerprints,
                &verified.catalog_fingerprints,
            )?;
        }
        if outputs != verified.outputs {
            return Err(TeachingError::InvalidLedger);
        }
        Ok(verified)
    }
}

#[cfg(test)]
#[path = "teaching_tests.rs"]
pub(crate) mod tests;

/// Compiler-issued portable evidence. The contained program cannot be mutated or
/// wrapped by callers; serde is exclusively the trusted durable-storage boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedTeachingExample {
    program: TeachingProgram,
}

impl CheckedTeachingExample {
    pub fn from_observation(observation: &ProgramObservation) -> Result<Self, TeachingError> {
        let ProgramObservation::Available(observed) = observation else {
            return Err(TeachingError::ObservationUnavailable);
        };
        if let DisclosureAssessment::KnownSensitiveOperands { first: range, .. } =
            &observed.disclosure
        {
            return Err(TeachingError::SensitiveOperand {
                start: range.start,
                end: range.end,
            });
        }
        if let DisclosureAssessment::UnresolvedSensitiveDataflow { first: range, .. } =
            &observed.disclosure
        {
            return Err(TeachingError::UnresolvedSensitiveDataflow {
                start: range.start,
                end: range.end,
            });
        }
        let checked = Self {
            program: observed.program.clone(),
        };
        if !checked.integrity_valid() {
            return Err(TeachingError::InvalidLedger);
        }
        Ok(checked)
    }

    #[cfg(test)]
    pub async fn from_execution(
        session: &ExecuteSession,
        source: &str,
        bundle: &PlasmCompBundle,
    ) -> Result<Self, TeachingError> {
        Self::from_observation(&ProgramObservation::from_execution(session, source, bundle))
    }
    #[cfg(test)]
    pub fn observation(&self) -> ProgramObservation {
        ProgramObservation::Available(Box::new(ObservedTeachingSource {
            program: self.program.clone(),
            disclosure: DisclosureAssessment::NoKnownSensitiveOperands,
        }))
    }
    pub fn program(&self) -> &TeachingProgram {
        &self.program
    }
    pub fn integrity_valid(&self) -> bool {
        self.program.witness_version_current()
            && self
                .program
                .fingerprint()
                .is_ok_and(|hash| hash == self.program.structural_fingerprint)
            && !self.program.semantic_contracts.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveOperandRange {
    pub start: u32,
    pub end: u32,
}

/// This records compiler-known disclosure restrictions, not universal secret detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisclosureAssessment {
    NoKnownSensitiveOperands,
    UnresolvedSensitiveDataflow {
        first: SensitiveOperandRange,
        additional: Vec<SensitiveOperandRange>,
    },
    KnownSensitiveOperands {
        first: SensitiveOperandRange,
        additional: Vec<SensitiveOperandRange>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedTeachingSource {
    program: TeachingProgram,
    disclosure: DisclosureAssessment,
}
impl ObservedTeachingSource {
    pub fn program(&self) -> &TeachingProgram {
        &self.program
    }
    pub fn disclosure(&self) -> &DisclosureAssessment {
        &self.disclosure
    }
    /// Absence of known sensitive operands is not a universal secret detector.
    pub fn model_source(&self) -> Option<&str> {
        matches!(
            self.disclosure,
            DisclosureAssessment::NoKnownSensitiveOperands
        )
        .then_some(self.program.source.as_str())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationDiagnostic {
    MissingCompilerContext,
    Extraction { message: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum ProgramObservation {
    Available(Box<ObservedTeachingSource>),
    Unavailable(ObservationDiagnostic),
}
impl Default for ProgramObservation {
    fn default() -> Self {
        Self::Unavailable(ObservationDiagnostic::MissingCompilerContext)
    }
}
impl ProgramObservation {
    pub fn from_execution(
        session: &ExecuteSession,
        source: &str,
        bundle: &PlasmCompBundle,
    ) -> Self {
        match TeachingProgram::from_compiled(session, source, bundle) {
            Ok(program) => {
                let Some(captured) = bundle.compilation_symbols().captured() else {
                    return Self::Unavailable(ObservationDiagnostic::Extraction {
                        message: "compiler disclosure context unavailable".into(),
                    });
                };
                if captured.source_sha256()
                    != plasm_core::catalog_discovery::content_hash(source.as_bytes())
                {
                    return Self::Unavailable(ObservationDiagnostic::Extraction {
                        message: "compiler source identity differs".into(),
                    });
                }
                let Some(ranges) = captured.sensitive_operand_ranges() else {
                    return Self::Unavailable(ObservationDiagnostic::Extraction {
                        message: "compiler disclosure classification unavailable".into(),
                    });
                };
                let disclosure = match ranges.split_first() {
                    Some((&(start, end), rest)) => DisclosureAssessment::KnownSensitiveOperands {
                        first: SensitiveOperandRange { start, end },
                        additional: rest
                            .iter()
                            .map(|&(start, end)| SensitiveOperandRange { start, end })
                            .collect(),
                    },
                    None => match captured
                        .unresolved_sensitive_dataflow_ranges()
                        .and_then(|ranges| ranges.split_first())
                    {
                        Some((&(start, end), rest)) => {
                            DisclosureAssessment::UnresolvedSensitiveDataflow {
                                first: SensitiveOperandRange { start, end },
                                additional: rest
                                    .iter()
                                    .map(|&(start, end)| SensitiveOperandRange { start, end })
                                    .collect(),
                            }
                        }
                        None => DisclosureAssessment::NoKnownSensitiveOperands,
                    },
                };
                Self::Available(Box::new(ObservedTeachingSource {
                    program,
                    disclosure,
                }))
            }
            Err(error) => Self::Unavailable(ObservationDiagnostic::Extraction {
                message: error.to_string(),
            }),
        }
    }
    pub fn available(&self) -> Option<&ObservedTeachingSource> {
        match self {
            Self::Available(value) => Some(value),
            Self::Unavailable(_) => None,
        }
    }
}
