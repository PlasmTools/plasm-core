//! Teaching projections consume compiler-issued dependency evidence.
use super::*;
use plasm_core::{prerequisites::CapabilityRef, PlasmReturn};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachingOutput {
    pub step: String,
    pub shape: plasm_core::plasm_monad::ResultShape,
    pub row_type: plasm_core::value_contract::ValueContract,
}

fn catalog<'a>(
    session: &'a ExecuteSession,
    entry: &str,
) -> Result<&'a plasm_core::CGS, TeachingError> {
    session
        .contexts_by_entry
        .get(entry)
        .map(|context| context.cgs.as_ref())
        .or_else(|| (session.entry_id == entry).then_some(session.cgs.as_ref()))
        .ok_or_else(|| TeachingError::CatalogDrift(entry.into()))
}
impl TeachingProgram {
    pub(super) fn complete_contracts(
        &mut self,
        session: &ExecuteSession,
        bundle: &PlasmCompBundle,
    ) -> Result<(), TeachingError> {
        let capabilities = bundle.compiled_dependencies()?;
        let mut owners: BTreeSet<_> = self
            .dependencies
            .iter()
            .map(|d| (d.entry_id.clone(), d.entity.clone()))
            .collect();
        for reference in capabilities {
            let cgs = catalog(session, &reference.catalog)?;
            let cap = cgs
                .get_capability(&reference.capability)
                .ok_or_else(|| TeachingError::CatalogDrift(reference.catalog.clone()))?;
            owners.insert((reference.catalog.clone(), cap.domain.to_string()));
            self.catalog_fingerprints
                .insert(reference.catalog.clone(), cgs.catalog_cgs_hash_hex());
        }
        self.dependencies = owners
            .into_iter()
            .map(|(entry_id, entity)| WorkflowSeed { entry_id, entity })
            .collect();
        self.required_capabilities = capabilities.iter().cloned().collect();
        self.semantic_contracts.clear();
        for occurrence in &self.occurrences {
            if let SemanticReference::ValueDomain {
                entry_id,
                value_ref,
            } = &occurrence.reference
            {
                let cgs = catalog(session, entry_id)?;
                let domain = cgs
                    .values
                    .get(value_ref)
                    .ok_or_else(|| TeachingError::CatalogDrift(entry_id.clone()))?;
                self.catalog_fingerprints
                    .insert(entry_id.clone(), cgs.catalog_cgs_hash_hex());
                let bytes = serde_json::to_vec(domain).map_err(|_| TeachingError::InvalidLedger)?;
                self.semantic_contracts.insert(
                    format!("value:{entry_id}:{value_ref}"),
                    hex::encode(Sha256::digest(bytes)),
                );
            }
        }
        for reference in &self.required_capabilities {
            let cgs = catalog(session, &reference.catalog)?;
            let capability = cgs
                .get_capability(&reference.capability)
                .ok_or_else(|| TeachingError::CatalogDrift(reference.catalog.clone()))?;
            let bytes = serde_json::to_vec(capability).map_err(|_| TeachingError::InvalidLedger)?;
            self.semantic_contracts.insert(
                format!("capability:{}:{}", reference.catalog, reference.capability),
                hex::encode(Sha256::digest(bytes)),
            );
        }
        for dependency in &self.dependencies {
            let cgs = catalog(session, &dependency.entry_id)?;
            let entity = cgs
                .get_entity(&dependency.entity)
                .ok_or_else(|| TeachingError::CatalogDrift(dependency.entry_id.clone()))?;
            let bytes = serde_json::to_vec(entity).map_err(|_| TeachingError::InvalidLedger)?;
            self.semantic_contracts.insert(
                format!("entity:{}:{}", dependency.entry_id, dependency.entity),
                hex::encode(Sha256::digest(bytes)),
            );
            // Shared value/security/transport context can alter effects without a
            // type error. Unreferenced entities and capabilities are immaterial.
            let mut shared = serde_json::to_value(cgs).map_err(|_| TeachingError::InvalidLedger)?;
            let object = shared.as_object_mut().ok_or(TeachingError::InvalidLedger)?;
            object.remove("entities");
            object.remove("capabilities");
            object.remove("version");
            object.remove("registry_aliases");
            let bytes = serde_json::to_vec(&shared).map_err(|_| TeachingError::InvalidLedger)?;
            self.semantic_contracts.insert(
                format!("context:{}", dependency.entry_id),
                hex::encode(Sha256::digest(bytes)),
            );
        }

        let dry = crate::plasm_plan_run::evaluate_plasm_comp_dry(session, bundle)
            .map_err(|e| TeachingError::ProgramAdmission(Box::new(e)))?;
        let returned: Vec<_> = match &bundle.artifact().comp.return_ {
            PlasmReturn::Step { step } => vec![step],
            PlasmReturn::Parallel { steps } => steps.iter().collect(),
        };
        self.outputs = returned
            .into_iter()
            .map(|step| {
                let payload = bundle
                    .artifact()
                    .comp
                    .steps
                    .get(step.as_str())
                    .ok_or(TeachingError::MissingCompilerDependency)?;
                let row_type = crate::map_body_schema::row_contract(
                    session,
                    dry.validated().nodes(),
                    step.as_str(),
                )
                .map_err(|e| TeachingError::OutputContract(Box::new(e)))?;
                Ok(TeachingOutput {
                    step: step.to_string(),
                    shape: payload.result_shape(),
                    row_type,
                })
            })
            .collect::<Result<_, TeachingError>>()?;
        Ok(())
    }
}

/// Re-pin only distribution identities after the owning semantic contracts match.
/// All scalar types, nullable/presence refinements and value-domain names remain exact.
pub(super) fn repin_contract(
    contract: &mut plasm_core::value_contract::ValueContract,
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Result<(), TeachingError> {
    use plasm_core::value_contract::ValueShape;
    if let Some(domain) = &mut contract.domain {
        if previous.get(&domain.entry_id) != Some(&domain.catalog_hash) {
            return Err(TeachingError::InvalidLedger);
        }
        domain.catalog_hash = current
            .get(&domain.entry_id)
            .ok_or(TeachingError::InvalidLedger)?
            .clone();
    }
    match &mut contract.shape {
        ValueShape::MappingRecord { record } => repin_contract(record, previous, current)?,
        ValueShape::Set { element } | ValueShape::Array { element } => {
            repin_contract(element, previous, current)?
        }
        ValueShape::Tuple { members } => {
            for member in members.prefix.iter_mut().chain(&mut members.suffix) {
                repin_contract(member, previous, current)?;
            }
            if let Some(member) = &mut members.variable {
                repin_contract(member, previous, current)?;
            }
        }
        ValueShape::Dictionary { key, value } => {
            repin_contract(key, previous, current)?;
            repin_contract(value, previous, current)?;
        }
        ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
            for field in fields.values_mut() {
                repin_contract(field, previous, current)?;
            }
        }
        ValueShape::Union { variants } => {
            for variant in variants {
                repin_contract(variant, previous, current)?;
            }
        }
        ValueShape::Temporal { .. }
        | ValueShape::Scalar { .. }
        | ValueShape::Null
        | ValueShape::Never => {}
    }
    Ok(())
}

/// Admission requirements describe observed execution independently of whether
/// its authored source is suitable for disclosure as teaching material.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramCatalogueRequirements {
    capability_contracts: Vec<CapabilityTeachingContract>,
    dependencies: Vec<WorkflowSeed>,
    required_capabilities: Vec<CapabilityRef>,
    catalog_fingerprints: BTreeMap<String, String>,
    source_sha256: String,
    compiled_sha256: String,
}
impl ProgramCatalogueRequirements {
    pub fn capability_contracts(&self) -> &[CapabilityTeachingContract] {
        &self.capability_contracts
    }
    pub fn dependencies(&self) -> &[WorkflowSeed] {
        &self.dependencies
    }
    pub fn required_capabilities(&self) -> &[CapabilityRef] {
        &self.required_capabilities
    }
    pub fn catalog_fingerprints(&self) -> &BTreeMap<String, String> {
        &self.catalog_fingerprints
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn compiled_sha256(&self) -> &str {
        &self.compiled_sha256
    }
    pub fn from_compiled(
        session: &ExecuteSession,
        bundle: &PlasmCompBundle,
    ) -> Result<Self, TeachingError> {
        let captured = bundle
            .compilation_symbols()
            .captured()
            .ok_or(TeachingError::ObservationUnavailable)?;
        let original_session = captured
            .restore_symbol_authority(session)
            .map_err(|error| TeachingError::SymbolContext(Box::new(error)))?;
        let session = &original_session;
        let source = bundle.source().ok_or(TeachingError::InvalidLedger)?;
        let source_sha256 = plasm_core::catalog_discovery::content_hash(source.as_bytes());
        let compiled_sha256 = plasm_core::catalog_discovery::content_hash(
            &serde_json::to_vec(
                &serde_json::to_value(&bundle.artifact().comp)
                    .map_err(|_| TeachingError::InvalidLedger)?,
            )
            .map_err(|_| TeachingError::InvalidLedger)?,
        );
        if captured.source_sha256() != source_sha256
            || captured.compiled_sha256() != compiled_sha256
        {
            return Err(TeachingError::InvalidLedger);
        }
        let catalogs: BTreeMap<_, _> = session
            .contexts_by_entry
            .iter()
            .map(|(entry, context)| (entry.clone(), context.cgs.as_ref()))
            .collect();
        let pins: BTreeMap<_, _> = catalogs
            .iter()
            .map(|(entry, cgs)| (entry.clone(), cgs.catalog_cgs_hash_hex()))
            .collect();
        if &pins != captured.catalog_fingerprints() {
            return Err(TeachingError::CatalogDrift(
                "observed compilation catalogues".into(),
            ));
        }
        let direct = bundle.compiled_dependencies()?;
        let closure = plasm_core::prerequisites::prerequisite_closure(
            &catalogs,
            &session.prerequisite_deployments,
            &direct.iter().cloned().collect::<Vec<_>>(),
            &catalogs.keys().cloned().collect(),
        )
        .map_err(|error| TeachingError::Prerequisite(Box::new(error)))?;
        let required_capabilities: Vec<_> = closure
            .business
            .into_iter()
            .chain(closure.input_sources)
            .chain(closure.prerequisites)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut dependencies = BTreeSet::new();
        let mut catalog_fingerprints = BTreeMap::new();
        let mut capability_contracts = Vec::new();
        for reference in &required_capabilities {
            let cgs = catalog(session, &reference.catalog)?;
            let cap = cgs
                .get_capability(&reference.capability)
                .ok_or_else(|| TeachingError::CatalogDrift(reference.catalog.clone()))?;
            capability_contracts.push(CapabilityTeachingContract::from_catalog(
                reference, cap, cgs,
            )?);
            dependencies.insert((reference.catalog.clone(), cap.domain.to_string()));
            catalog_fingerprints.insert(reference.catalog.clone(), cgs.catalog_cgs_hash_hex());
        }
        Ok(Self {
            capability_contracts,
            dependencies: dependencies
                .into_iter()
                .map(|(entry_id, entity)| WorkflowSeed { entry_id, entity })
                .collect(),
            required_capabilities,
            catalog_fingerprints,
            source_sha256,
            compiled_sha256,
        })
    }
}
