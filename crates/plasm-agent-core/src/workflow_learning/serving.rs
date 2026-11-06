//! Optional examples are staged and compiled before any teaching is published.
use std::collections::{BTreeMap, BTreeSet};

use plasm_core::prerequisites::{prerequisite_closure, CapabilityRef};
use plasm_core::CgsCatalog;
use uuid::Uuid;

use super::model::LibraryScope;
use super::repository::{RepositoryError, WorkflowRepository};
use super::teaching::{TeachingOutput, TeachingProgram};
use crate::discovery_store::DiscoveryAuthorization;
use crate::execute_session::ExecuteSession;
use crate::http_execute::context::session::{
    prepare_federate_wave, stage_exposure_wave_delta, ExposureWaveSnapshot,
};
use crate::server_state::PlasmHostState;

#[derive(Debug, thiserror::Error)]
pub enum ServingError {
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error("workflow example is not applicable to this session")]
    Inapplicable,
    #[error("workflow example or session changed; retry from current state")]
    Conflict,
    #[error(
        "workflow example needs an active synchronized session; run normal context discovery first"
    )]
    ContextRequired,
    #[error("workflow example portability validation failed: {0}")]
    Teaching(#[source] Box<super::teaching::TeachingError>),
    #[error("workflow example failed validation: {0}")]
    Invalid(String),
    #[error("workflow catalogue preparation failed: {0}")]
    Catalog(#[from] crate::catalog_runtime::CatalogRuntimeError),
    #[error("workflow catalogue lookup failed: {0}")]
    Discovery(#[from] plasm_core::DiscoveryError),
    #[error("workflow prerequisite preparation failed: {0}")]
    Prerequisite(#[from] plasm_core::prerequisites::PrerequisiteError),
    #[error("workflow capability exposure failed: {0}")]
    CapabilityExposure(#[from] plasm_core::capability_exposure::CapabilityExposureError),
    #[error("workflow type declaration failed: {0}")]
    PythonTeaching(#[from] plasm_core::prompt_render::python::PythonTeachingError),
    #[error("workflow named domain is invalid: {0}")]
    ValueDomain(#[from] plasm_core::schema::ValueDomainKeyError),
    #[error("workflow session preparation failed: {0}")]
    Session(#[source] Box<crate::http_execute::context::session::SessionMutateError>),
    #[error(
        "catalogue {entry_id} schema overlay must already be materialized in the caller session"
    )]
    OverlayRequired { entry_id: String },
    #[error(transparent)]
    Compilation(#[from] crate::compilation_error::CompilationError),
    #[error(transparent)]
    Persistence(
        #[from] crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError,
    ),
}

/// Shared dependency policy for live retrieval and archive adoption. Session
/// materialization and publication remain owned by their respective callers.
pub(crate) struct ExampleDependencyProjection {
    pub required: Vec<CapabilityRef>,
    pub groups: BTreeMap<String, BTreeSet<String>>,
    pub domains: Vec<(String, plasm_core::ValueDomainKey)>,
}

impl ExampleDependencyProjection {
    pub(crate) fn resolve(
        program: &TeachingProgram,
        catalogs: &BTreeMap<String, std::sync::Arc<plasm_core::CgsContext>>,
        deployments: &plasm_core::prerequisites::DeploymentBindings,
        allowed: &DiscoveryAuthorization,
    ) -> Result<Self, ServingError> {
        if program
            .catalog_fingerprints
            .keys()
            .any(|entry| !allowed.catalogs.contains(entry) || !catalogs.contains_key(entry))
        {
            return Err(ServingError::Inapplicable);
        }
        let direct = program.required_capabilities.clone();
        if direct.iter().any(|reference| !allowed.permits(reference)) {
            return Err(ServingError::Inapplicable);
        }
        let borrowed = catalogs
            .iter()
            .map(|(entry, context)| (entry.clone(), context.cgs.as_ref()))
            .collect();
        let closure = prerequisite_closure(&borrowed, deployments, &direct, &allowed.catalogs)
            .map_err(ServingError::Prerequisite)?;
        let required: Vec<_> = closure
            .business
            .into_iter()
            .chain(closure.input_sources)
            .chain(closure.prerequisites)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if required.iter().any(|reference| !allowed.permits(reference)) {
            return Err(ServingError::Inapplicable);
        }
        let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for dependency in &program.dependencies {
            groups
                .entry(dependency.entry_id.clone())
                .or_default()
                .insert(dependency.entity.clone());
        }
        for reference in &required {
            let cap = catalogs
                .get(&reference.catalog)
                .and_then(|context| context.cgs.capabilities.get(reference.capability.as_str()))
                .ok_or_else(|| ServingError::Invalid("required capability missing".into()))?;
            groups
                .entry(reference.catalog.clone())
                .or_default()
                .insert(cap.domain.to_string());
        }
        let domains: Vec<_> = program
            .occurrences
            .iter()
            .filter_map(|occurrence| match &occurrence.reference {
                super::teaching::SemanticReference::ValueDomain {
                    entry_id,
                    value_ref,
                } => Some((entry_id.clone(), value_ref.clone())),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(entry, key)| plasm_core::ValueDomainKey::new(key).map(|key| (entry, key)))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            required,
            groups,
            domains,
        })
    }
}

pub struct PreparedExampleFetch {
    scope: LibraryScope,
    id: Uuid,
    prompt_hash: String,
    session_id: String,
    base_revision: u32,
    base_pin: Option<crate::discovery_store::DiscoverySessionPin>,
    required: Vec<CapabilityRef>,
    staged: ExecuteSession,
    source: String,
    teaching: String,
    outputs: Vec<TeachingOutput>,
    catalog_fingerprints: BTreeMap<String, String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServedExample {
    pub example_id: Uuid,
    pub source: String,
    pub teaching_delta: String,
    pub domain_revision: u32,
    pub outputs: Vec<TeachingOutput>,
    pub catalog_fingerprints: BTreeMap<String, String>,
}

impl ServedExample {
    /// Agent-facing projection; compatibility pins and internal contracts stay in the receipt.
    pub fn teaching_outputs(&self) -> Vec<String> {
        self.outputs.iter().map(ToString::to_string).collect()
    }

    pub fn agent_text(&self) -> String {
        let mut text = format!("Example {}\n\nProgram:\n{}", self.example_id, self.source);
        let outputs = self.teaching_outputs();
        if !outputs.is_empty() {
            text.push_str("\n\nOutputs:\n");
            for output in outputs {
                text.push_str(&format!("- {output}\n"));
            }
        }
        if !self.teaching_delta.trim().is_empty() {
            text.push_str("\nRequired declarations:\n");
            text.push_str(self.teaching_delta.trim());
        }
        text
    }
}

/// The caller authenticates `scope` and session ownership. Authorization is the
/// intersection of the current tenant policy and the logical session pin.
pub async fn prepare_example_fetch(
    host: &PlasmHostState,
    repository: &WorkflowRepository,
    scope: LibraryScope,
    id: Uuid,
    prompt_hash: &str,
    session_id: &str,
    allowed: &DiscoveryAuthorization,
) -> Result<PreparedExampleFetch, ServingError> {
    let example = repository.retrieve(&scope, id).await?;
    let portable = example
        .candidate
        .program()
        .ok_or_else(|| ServingError::Invalid("example has no checked program".into()))?
        .clone();
    if portable
        .catalog_fingerprints
        .keys()
        .any(|entry_id| !allowed.catalogs.contains(entry_id))
    {
        return Err(ServingError::Inapplicable);
    }
    let current = synchronized_hot_session(host, prompt_hash, session_id).await?;
    let mut view = host.clone();
    if let Some(pin) = &current.discovery_pin {
        view.oss.catalog = host
            .catalog
            .pinned_view(&pin.generation)
            .await
            .map_err(ServingError::Catalog)?;
    }
    let host = &view;
    let base_revision = current.domain_revision;
    let base_pin = current.discovery_pin.clone();
    let mut staged = (*current).clone();
    // Materialize effective dependency contracts before comparing fingerprints:
    // tenant schema overlays cannot be compared to the registry base schema.
    let mut dependency_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for dependency in &portable.dependencies {
        dependency_groups
            .entry(dependency.entry_id.clone())
            .or_default()
            .insert(dependency.entity.clone());
    }
    for (entry, entities) in &dependency_groups {
        materialize_dependency(host, &mut staged, entry, entities).await?;
    }
    let registry = host.catalog.snapshot();
    let mut catalogs = BTreeMap::new();
    for entry in &allowed.catalogs {
        let context = if let Some(context) = staged.contexts_by_entry.get(entry) {
            context.clone()
        } else {
            registry
                .load_context(entry)
                .map(std::sync::Arc::new)
                .map_err(ServingError::Discovery)?
        };
        catalogs.insert(entry.clone(), context);
    }
    let ExampleDependencyProjection {
        required,
        groups,
        domains,
    } = ExampleDependencyProjection::resolve(
        &portable,
        &catalogs,
        &staged.prerequisite_deployments,
        allowed,
    )?;
    for (entry, entities) in &groups {
        materialize_dependency(host, &mut staged, entry, entities).await?;
    }
    for (entry, _) in &domains {
        if !staged.contexts_by_entry.contains_key(entry) {
            let context = registry.load_context(entry)?;
            if context.cgs.schema_overlay.is_some() {
                return Err(ServingError::OverlayRequired {
                    entry_id: entry.clone(),
                });
            }
            staged
                .contexts_by_entry
                .insert(entry.clone(), std::sync::Arc::new(context));
            staged
                .compiled_catalogs_by_entry
                .insert(entry.clone(), host.catalog.compiled_catalog(entry)?);
        }
    }
    let (staged, teaching_delta) = stage_example_symbols(staged, &required, &groups, &domains)?;
    let TeachingProgram {
        source,
        outputs,
        catalog_fingerprints,
        ..
    } = match portable.rebind_teaching(&staged) {
        Ok(program) => program,
        Err(error) => {
            repository
                .quarantine(&scope, id, &format!("rebind validation: {error}"))
                .await?;
            return Err(ServingError::Teaching(Box::new(error)));
        }
    };
    if let Err(error) = crate::plasm_compile::compile_program(
        host.engine.prompt_pipeline(),
        None,
        &staged,
        "workflow_example",
        &source,
    )
    .await
    {
        if matches!(
            error,
            crate::compilation_error::CompilationError::Program(_)
        ) {
            repository
                .quarantine(&scope, id, &format!("typecheck: {error}"))
                .await?;
        }
        return Err(error.into());
    }
    Ok(PreparedExampleFetch {
        scope,
        id,
        prompt_hash: prompt_hash.into(),
        session_id: session_id.into(),
        base_revision,
        base_pin,
        required,
        staged,
        source,
        teaching: teaching_delta,
        outputs,
        catalog_fingerprints,
    })
}

/// Shared pure symbol staging for serving and host-owned archive validation.
/// Callers authenticate all catalogue/capability requirements before this boundary.
pub(crate) fn stage_example_symbols(
    mut staged: ExecuteSession,
    required: &[CapabilityRef],
    groups: &BTreeMap<String, BTreeSet<String>>,
    domains: &[(String, plasm_core::ValueDomainKey)],
) -> Result<(ExecuteSession, String), ServingError> {
    let mut exposure = staged
        .teaching_exposure
        .take()
        .ok_or(ServingError::Inapplicable)?;
    let snapshot = ExposureWaveSnapshot {
        slots_before: exposure.surface.slots.clone(),
        caps_before: exposure.surface.capabilities.clone(),
        entity_count_before: exposure.entities.len(),
        relation_keys: exposure.all_qualified_entities(),
    };
    let layers: Vec<_> = staged
        .contexts_by_entry
        .values()
        .map(|context| context.cgs.as_ref())
        .collect();
    for (entry, entities) in groups {
        let context = &staged.contexts_by_entry[entry];
        let capabilities = required
            .iter()
            .filter(|reference| &reference.catalog == entry)
            .map(|reference| reference.capability.clone())
            .collect::<Vec<_>>();
        let delta = plasm_core::capability_exposure::selected_capability_surface(
            &context.cgs,
            entry,
            &capabilities,
        )
        .map_err(ServingError::CapabilityExposure)?;
        let names: Vec<_> = entities.iter().map(String::as_str).collect();
        exposure.expose_surface(&layers, context.cgs.clone(), entry, &names, delta);
    }
    for (entry, _) in domains {
        exposure.expose_entities(
            &layers,
            staged.contexts_by_entry[entry].cgs.clone(),
            entry,
            &[],
        );
    }
    let (mut staged, mut delta) = stage_exposure_wave_delta(staged, exposure, snapshot)
        .map_err(|error| ServingError::Session(Box::new(error)))?;
    if !domains.is_empty() {
        let exposure = staged
            .teaching_exposure
            .as_ref()
            .ok_or(ServingError::Inapplicable)?;
        let wave = plasm_core::prompt_render::python::prepare_python_domain_wave(
            exposure,
            &staged.python_teaching,
            domains,
        )?;
        if !wave.declarations.is_empty() {
            staged.prompt_text.push_str("\n\n");
            staged.prompt_text.push_str(&wave.declarations);
            staged.domain_revision = staged.domain_revision.saturating_add(1);
            delta.markdown.push_str("\n\n");
            delta.markdown.push_str(&wave.declarations);
        }
        staged.python_teaching = wave.next_state;
    }
    Ok((staged, delta.markdown))
}

async fn materialize_dependency(
    host: &PlasmHostState,
    staged: &mut ExecuteSession,
    entry: &str,
    entities: &BTreeSet<String>,
) -> Result<(), ServingError> {
    if staged.contexts_by_entry.contains_key(entry) {
        return Ok(());
    }
    let catalog = host
        .catalog
        .snapshot()
        .load_context(entry)
        .map_err(ServingError::Discovery)?;
    if catalog.cgs.schema_overlay.is_some() {
        return Err(ServingError::OverlayRequired {
            entry_id: entry.to_owned(),
        });
    }
    let bindings: std::collections::HashMap<_, _> = staged
        .bindings_by_entry
        .iter()
        .map(|(entry, binding)| (entry.clone(), binding.clone()))
        .collect();
    let prepared = prepare_federate_wave(
        host,
        entry.to_owned(),
        entities.iter().cloned().collect(),
        staged.principal.clone(),
        Some(&staged.materialized_outbound_hosted_kv_by_entry),
        Some(&bindings),
    )
    .await
    .map_err(|error| ServingError::Session(Box::new(error)))?;
    staged
        .contexts_by_entry
        .insert(entry.to_owned(), prepared.ctx_arc);
    staged.compiled_catalogs_by_entry.insert(
        entry.to_owned(),
        host.catalog
            .compiled_catalog(entry)
            .map_err(ServingError::Catalog)?,
    );
    staged
        .registry_catalog_hashes_by_entry
        .insert(entry.to_owned(), prepared.registry_pin);
    if let Some(binding) = prepared.entry_bindings {
        staged.bindings_by_entry.insert(entry.to_owned(), binding);
    }
    if let Some(key) = prepared.materialized_outbound_hosted_kv {
        staged
            .materialized_outbound_hosted_kv_by_entry
            .insert(entry.to_owned(), key);
    }
    Ok(())
}

/// Call with freshly resolved authorization, never a cached discovery receipt.
pub async fn commit_example_fetch(
    host: &PlasmHostState,
    repository: &WorkflowRepository,
    prepared: PreparedExampleFetch,
    allowed: &DiscoveryAuthorization,
) -> Result<ServedExample, ServingError> {
    let key = crate::session_coordination::ExecuteCoordKey {
        prompt_hash: prepared.prompt_hash.clone(),
        session_id: prepared.session_id.clone(),
    };
    host.session_coordination
        .with_exposure_commit(&key, || async {
            let example_guard = repository
                .lock_active_example(&prepared.scope, prepared.id)
                .await?;
            if prepared
                .catalog_fingerprints
                .keys()
                .any(|entry| !allowed.catalogs.contains(entry))
                || prepared
                    .required
                    .iter()
                    .any(|reference| !allowed.permits(reference))
            {
                return Err(ServingError::Inapplicable);
            }
            let current =
                synchronized_hot_session(host, &prepared.prompt_hash, &prepared.session_id).await?;
            if let Some(receipt) = host
                .execute_session_registry
                .example_receipt(&prepared.prompt_hash, &prepared.session_id, prepared.id)
                .await?
            {
                if receipt.catalog_fingerprints == prepared.catalog_fingerprints {
                    example_guard.commit().await?;
                    return Ok(receipt);
                }
            }
            if current.domain_revision != prepared.base_revision
                || current.discovery_pin != prepared.base_pin
            {
                return Err(ServingError::Conflict);
            }
            let revision = prepared.staged.domain_revision;
            let receipt = ServedExample {
                example_id: prepared.id,
                source: prepared.source,
                teaching_delta: prepared.teaching,
                domain_revision: revision,
                outputs: prepared.outputs,
                catalog_fingerprints: prepared.catalog_fingerprints,
            };
            host.publish_example_fetch(
                &prepared.prompt_hash,
                &prepared.session_id,
                prepared.staged,
                receipt.clone(),
            )
            .await?;
            example_guard.commit().await?;
            Ok(receipt)
        })
        .await
}

pub(crate) async fn synchronized_hot_session(
    host: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
) -> Result<std::sync::Arc<ExecuteSession>, ServingError> {
    let current = host
        .sessions
        .get_by_strs(prompt_hash, session_id)
        .await
        .ok_or(ServingError::ContextRequired)?;
    let authoritative = host
        .execute_session_registry
        .load_for_example(prompt_hash, session_id)
        .await?
        .ok_or(ServingError::ContextRequired)?;
    if authoritative.domain_revision != current.domain_revision
        || authoritative.discovery_pin != current.discovery_pin
    {
        return Err(ServingError::ContextRequired);
    }
    let pins = current
        .contexts_by_entry
        .iter()
        .map(|(entry, context)| {
            (
                entry.clone(),
                crate::catalog_hash::EffectiveCatalogHash::from_effective_cgs(&context.cgs),
            )
        })
        .collect();
    let snapshot =
        crate::execute_session_materialize::build_durable_exposure_snapshot_reusing_pins(
            &current, pins,
        )
        .await?;
    if snapshot.symbol_ledger_bytes != authoritative.symbol_ledger_bytes {
        return Err(ServingError::ContextRequired);
    }
    Ok(current)
}
