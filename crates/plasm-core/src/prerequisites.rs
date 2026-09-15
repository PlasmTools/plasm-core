//! Explicit prerequisite contracts.
//!
//! Teaching recites acquisitions. **RA-17** is the compile/resolve law: a seat
//! bound to a requirement may only be filled from that requirement's deployed
//! provider (`provider_catalog:provider`). A foreign catalog's acquisition — or
//! the same program binding shared across distinct deployed providers — is a
//! typed reject, not a vendor 401. Token-string / JWT inspection is not a law.
//!
//! **RA-6 inherit** may copy a parent Get / session param onto a child seat only
//! when [`inherit_may_fill_param`] proves the same deployed provider, or the
//! child has no distinct foreign-provider seat. Unproven catalog identity omits
//! the binding — it does not silently send the parent token.

use crate::schema::{CapabilitySchema, InputFieldSchema, InputFieldWire, InputType};
use crate::{FieldType, NamedValueSchema, CGS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputLane {
    Scope,
    Selection,
    Controls,
    Arguments,
    Payload,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputPath {
    pub lane: InputLane,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrerequisiteCatalog {
    #[serde(default)]
    pub contracts: BTreeMap<String, Contract>,
    #[serde(default)]
    pub providers: BTreeMap<String, Provider>,
    #[serde(default)]
    pub requirements: BTreeMap<String, Vec<Requirement>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub version: u32,
    /// Port name to catalog-local value domain.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub contract: String,
    pub capability: String,
    /// Contract input port to an existing capability input.
    #[serde(default)]
    pub inputs: BTreeMap<String, InputPath>,
    /// Contract output port to a field populated by the capability.
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub contract: String,
    /// Contract input port to declared argument source.
    #[serde(default)]
    pub arguments: BTreeMap<String, ArgumentSource>,
    pub bindings: Vec<ConsumerBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArgumentSource {
    Constant {
        value: serde_json::Value,
    },
    ConsumerInput {
        input: InputPath,
    },
    /// Existing target identity, not an invocation input lane.
    ConsumerIdentity {
        field: String,
    },
    RequirementOutput {
        requirement: String,
        output: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerBinding {
    pub input: InputPath,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRef {
    pub catalog: String,
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentBinding {
    pub consumer: CapabilityRef,
    pub requirement: String,
    pub provider_catalog: String,
    pub provider: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentBindings {
    pub bindings: Vec<DeploymentBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrerequisiteEdge {
    pub consumer: CapabilityRef,
    pub consumer_instance: Option<String>,
    pub provider_instance: String,
    pub requirement: Requirement,
    pub provider: CapabilityRef,
    pub provider_inputs: BTreeMap<String, InputPath>,
    pub provider_outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrerequisiteClosure {
    pub acquisitions: Vec<Acquisition>,
    pub business: Vec<CapabilityRef>,
    pub prerequisites: Vec<CapabilityRef>,
    pub edges: Vec<PrerequisiteEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolvedArgument {
    Constant {
        value: serde_json::Value,
    },
    BusinessInput {
        consumer: CapabilityRef,
        input: InputPath,
    },
    BusinessIdentity {
        consumer: CapabilityRef,
        field: String,
    },
    ProviderOutput {
        instance: String,
        field: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Acquisition {
    pub id: String,
    pub provider_catalog: String,
    pub provider: String,
    pub capability: CapabilityRef,
    pub arguments: BTreeMap<String, ResolvedArgument>,
}

impl PrerequisiteCatalog {
    pub fn is_empty(&self) -> bool {
        self.contracts.is_empty() && self.providers.is_empty() && self.requirements.is_empty()
    }

    pub fn validate(&self, cgs: &CGS) -> Result<(), String> {
        for (id, contract) in &self.contracts {
            if id.trim().is_empty() || contract.version == 0 || contract.outputs.is_empty() {
                return Err(format!("invalid prerequisite contract {id}"));
            }
            for value in contract.inputs.values().chain(contract.outputs.values()) {
                value_type(cgs, value)?;
            }
        }
        for (id, provider) in &self.providers {
            let contract = self.contract(&provider.contract)?;
            let cap = capability(cgs, &provider.capability)?;
            if contract.inputs.keys().ne(provider.inputs.keys())
                || contract.outputs.keys().ne(provider.outputs.keys())
            {
                return Err(format!(
                    "provider {id} must map every contract port exactly once"
                ));
            }
            let mut supplied: BTreeSet<InputPath> = BTreeSet::new();
            for input in provider.inputs.values() {
                if !supplied.insert(input.clone()) {
                    return Err(format!("duplicate provider input on {id}"));
                }
            }
            if let Some(requirements) = self.requirements.get(&provider.capability) {
                for binding in requirements.iter().flat_map(|r| &r.bindings) {
                    if !supplied.insert(binding.input.clone()) {
                        return Err(format!("conflicting provider input on {id}"));
                    }
                }
            }
            for required in required_inputs(cap)? {
                if !supplied.contains(&required) {
                    return Err(format!(
                        "provider {id} has unsupplied required input {required:?}"
                    ));
                }
            }
            let output_entity = match cap.output_schema.as_ref().map(|o| &o.output_type) {
                Some(crate::schema::OutputType::Entity { entity_type }) => entity_type.as_str(),
                _ => return Err(format!("provider {id} must produce one declared entity")),
            };
            for (port, input) in &provider.inputs {
                same_type(
                    value_type(cgs, &contract.inputs[port])?,
                    input_type(cgs, cap, input)?,
                    id,
                )?;
            }
            for (port, output) in &provider.outputs {
                let entity = cgs
                    .entities
                    .get(output_entity)
                    .ok_or("provider entity missing")?;
                let field = entity
                    .fields
                    .get(output.as_str())
                    .ok_or_else(|| format!("provider output {output} missing"))?;
                if !cap.provides.iter().any(|name| name.as_str() == output) {
                    return Err(format!(
                        "provider {} does not populate {output}",
                        provider.capability
                    ));
                }
                same_type(
                    value_type(cgs, &contract.outputs[port])?,
                    field.named_value(cgs).map_err(|e| e.to_string())?,
                    id,
                )?;
            }
        }
        for (cap_name, requirements) in &self.requirements {
            let cap = capability(cgs, cap_name)?;
            let mut ids = BTreeSet::new();
            let mut bound = BTreeSet::new();
            for requirement in requirements {
                if requirement.id.is_empty() || !ids.insert(&requirement.id) {
                    return Err(format!("duplicate or empty requirement on {cap_name}"));
                }
                let contract = self.contract(&requirement.contract)?;
                if contract.inputs.keys().ne(requirement.arguments.keys()) {
                    return Err(format!(
                        "requirement {} must supply every contract input",
                        requirement.id
                    ));
                }
                let mut bound_outputs = BTreeSet::new();
                for binding in &requirement.bindings {
                    if !bound.insert(&binding.input) {
                        return Err(format!("duplicate input binding on {cap_name}"));
                    }
                    if !bound_outputs.insert(&binding.output) {
                        return Err(format!(
                            "requirement {} binds contract output {} to more than one input",
                            requirement.id, binding.output
                        ));
                    }
                    let value = contract
                        .outputs
                        .get(&binding.output)
                        .ok_or("unknown contract output")?;
                    same_type(
                        value_type(cgs, value)?,
                        input_type(cgs, cap, &binding.input)?,
                        cap_name,
                    )?;
                }
                for (port, source) in &requirement.arguments {
                    let expected = value_type(cgs, &contract.inputs[port])?;
                    match source {
                        ArgumentSource::ConsumerIdentity { field } => {
                            let entity = cgs
                                .get_entity(cap.domain.as_str())
                                .ok_or("missing consumer entity")?;
                            if matches!(
                                cap.kind,
                                crate::schema::CapabilityKind::Create
                                    | crate::schema::CapabilityKind::Query
                                    | crate::schema::CapabilityKind::Search
                            ) || (field.as_str() != entity.id_field.as_str()
                                && !entity.key_vars.iter().any(|key| key.as_str() == field))
                            {
                                return Err(format!("{cap_name}: prerequisite identity must name an existing target identity field"));
                            }
                            let value = entity
                                .fields
                                .get(field.as_str())
                                .ok_or("missing consumer identity field")?
                                .named_value(cgs)
                                .map_err(|e| e.to_string())?;
                            same_type(expected, value, cap_name)?;
                        }
                        ArgumentSource::ConsumerInput { input } => {
                            same_type(expected, input_type(cgs, cap, input)?, cap_name)?
                        }
                        ArgumentSource::RequirementOutput {
                            requirement: other,
                            output,
                        } => {
                            let other = requirements
                                .iter()
                                .find(|r| &r.id == other)
                                .ok_or("unknown prerequisite output source")?;
                            let other_contract = self.contract(&other.contract)?;
                            let value = other_contract
                                .outputs
                                .get(output)
                                .ok_or("unknown prerequisite output port")?;
                            same_type(expected, value_type(cgs, value)?, cap_name)?;
                        }
                        ArgumentSource::Constant { value } => {
                            // Constants must be scalar; structured arguments use typed bindings.
                            let compatible = match &expected.field_type {
                                FieldType::String => value.is_string(),
                                FieldType::Integer => {
                                    value.as_i64().is_some() || value.as_u64().is_some()
                                }
                                FieldType::Number => value.is_number(),
                                FieldType::Boolean => value.is_boolean(),
                                _ => false,
                            };
                            if !compatible {
                                return Err(format!("constant type mismatch on {cap_name}.{port}"));
                            }
                            if let Some(value) = value.as_str() {
                                expected.domain.validate_string_value(value)?;
                            }
                            if let Some(value) = value.as_f64() {
                                expected.domain.validate_number_value(value)?;
                            }
                        }
                    }
                }
            }
            for requirement in requirements {
                validate_argument_cycles(requirements, &requirement.id, &mut BTreeSet::new())?;
            }
        }
        Ok(())
    }

    fn contract(&self, id: &str) -> Result<&Contract, String> {
        self.contracts
            .get(id)
            .ok_or_else(|| format!("unknown prerequisite contract {id}"))
    }
}

fn validate_argument_cycles<'a>(
    requirements: &'a [Requirement],
    id: &'a str,
    visiting: &mut BTreeSet<&'a str>,
) -> Result<(), String> {
    if !visiting.insert(id) {
        return Err(format!("prerequisite argument cycle at {id}"));
    }
    let requirement = requirements
        .iter()
        .find(|r| r.id == id)
        .ok_or("unknown requirement")?;
    for source in requirement.arguments.values() {
        match source {
            ArgumentSource::RequirementOutput { requirement, .. } => {
                validate_argument_cycles(requirements, requirement, visiting)?
            }
            ArgumentSource::ConsumerInput { input } => {
                for owner in requirements
                    .iter()
                    .filter(|r| r.bindings.iter().any(|b| &b.input == input))
                {
                    validate_argument_cycles(requirements, &owner.id, visiting)?;
                }
            }
            ArgumentSource::Constant { .. } | ArgumentSource::ConsumerIdentity { .. } => {}
        }
    }
    visiting.remove(id);
    Ok(())
}

fn required_inputs(cap: &CapabilitySchema) -> Result<Vec<InputPath>, String> {
    fn collect(
        fields: &[InputFieldSchema],
        lane: &InputLane,
        prefix: &[String],
        out: &mut Vec<InputPath>,
    ) -> Result<(), String> {
        for field in fields.iter().filter(|f| f.required && f.default.is_none()) {
            let mut path = prefix.to_vec();
            path.push(field.name.clone());
            match &field.wire {
                InputFieldWire::Registry(_) => out.push(InputPath {
                    lane: lane.clone(),
                    path,
                }),
                InputFieldWire::Inline(ty) => match ty.as_ref() {
                    InputType::Object { fields, .. } => collect(fields, lane, &path, out)?,
                    InputType::Value { .. } => out.push(InputPath {
                        lane: lane.clone(),
                        path,
                    }),
                    _ => return Err("provider requires an unsupported structured input".into()),
                },
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if let Some(derived) = &cap.derived {
        out.push(InputPath {
            lane: InputLane::Selection,
            path: vec![derived.identity_field.clone()],
        });
    }
    for (lane, fields) in [
        (InputLane::Scope, &cap.inputs.scope.0),
        (InputLane::Selection, &cap.inputs.selection.0),
        (InputLane::Controls, &cap.inputs.controls.0),
    ] {
        collect(fields, &lane, &[], &mut out)?;
    }
    for (lane, schema) in [
        (InputLane::Arguments, &cap.inputs.arguments),
        (InputLane::Payload, &cap.inputs.payload),
    ] {
        if let Some(schema) = schema {
            match &schema.input_type {
                InputType::None => {}
                InputType::Object { fields, .. } => collect(fields, &lane, &[], &mut out)?,
                _ => return Err("provider requires an unsupported root input".into()),
            }
        }
    }
    Ok(out)
}

fn capability<'a>(cgs: &'a CGS, id: &str) -> Result<&'a CapabilitySchema, String> {
    cgs.capabilities
        .get(id)
        .ok_or_else(|| format!("unknown prerequisite capability {id}"))
}

fn value_type<'a>(cgs: &'a CGS, id: &str) -> Result<&'a NamedValueSchema, String> {
    cgs.values
        .get(id)
        .ok_or_else(|| format!("unknown prerequisite value domain {id}"))
}

fn same_type(
    left: &NamedValueSchema,
    right: &NamedValueSchema,
    location: &str,
) -> Result<(), String> {
    if left.domain != right.domain
        || left.field_type != right.field_type
        || left.array_items != right.array_items
        || left.value_format != right.value_format
        || left.allowed_values != right.allowed_values
        || left.currency != right.currency
    {
        return Err(format!("prerequisite value domain mismatch at {location}"));
    }
    Ok(())
}

fn input_type<'a>(
    cgs: &'a CGS,
    cap: &'a CapabilitySchema,
    input: &InputPath,
) -> Result<&'a NamedValueSchema, String> {
    if input.lane == InputLane::Selection {
        if let Some(derived) = &cap.derived {
            if input.path == [derived.identity_field.clone()] {
                return cgs
                    .entities
                    .get(cap.domain.as_str())
                    .and_then(|e| e.fields.get(derived.identity_field.as_str()))
                    .ok_or("missing derived identity field")?
                    .named_value(cgs)
                    .map_err(|e| e.to_string());
            }
        }
    }
    let fields = match input.lane {
        InputLane::Scope => &cap.inputs.scope.0,
        InputLane::Selection => &cap.inputs.selection.0,
        InputLane::Controls => &cap.inputs.controls.0,
        InputLane::Arguments | InputLane::Payload => {
            let schema = if input.lane == InputLane::Arguments {
                &cap.inputs.arguments
            } else {
                &cap.inputs.payload
            };
            match &schema.as_ref().ok_or("missing input lane")?.input_type {
                InputType::Object { fields, .. } => fields,
                _ => return Err("prerequisite input path must address an object field".into()),
            }
        }
    };
    descend_input(cgs, fields, &input.path)
}

fn descend_input<'a>(
    cgs: &'a CGS,
    fields: &'a [InputFieldSchema],
    path: &[String],
) -> Result<&'a NamedValueSchema, String> {
    let (head, rest) = path.split_first().ok_or("empty prerequisite input path")?;
    let field = fields
        .iter()
        .find(|f| &f.name == head)
        .ok_or_else(|| format!("missing input field {head}"))?;
    if rest.is_empty() {
        return field.named_value(cgs).map_err(|e| e.to_string());
    }
    match &field.wire {
        InputFieldWire::Inline(ty) => match ty.as_ref() {
            InputType::Object { fields, .. } => descend_input(cgs, fields, rest),
            _ => Err("prerequisite path traverses non-object input".into()),
        },
        _ => Err("prerequisite path traverses scalar input".into()),
    }
}

/// Program value wired to a capability input that may be a prerequisite seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatWiring {
    pub input: InputPath,
    /// Top-level program binding (`sw` in `sw.access_token`).
    pub binding: Option<String>,
    /// Catalog that produced the value when statically known (acquisition entity).
    pub source_catalog: Option<String>,
}

/// `provider_catalog:provider` — the identity RA-17 names in rejects.
pub fn deployed_provider_label(catalog: &str, provider: &str) -> String {
    format!("{catalog}:{provider}")
}

fn seat_wire_name(input: &InputPath) -> String {
    input
        .path
        .last()
        .cloned()
        .unwrap_or_else(|| format!("{:?}", input.lane).to_ascii_lowercase())
}

fn seat_binds_param(input: &InputPath, param: &str) -> bool {
    input.path.last().is_some_and(|p| p == param)
}

/// Deployed `provider_catalog` for `param` on `consumer` when that wire name is
/// a prerequisite seat with an explicit deployment.
pub fn deployed_seat_catalog_for_param<'a>(
    deployments: &'a DeploymentBindings,
    consumer: &CapabilityRef,
    requirements: &[Requirement],
    param: &str,
) -> Option<&'a str> {
    for req in requirements {
        if !req
            .bindings
            .iter()
            .any(|b| seat_binds_param(&b.input, param))
        {
            continue;
        }
        let catalog_unproven = consumer.catalog.trim().is_empty();
        return deployments
            .bindings
            .iter()
            .find(|b| {
                b.requirement == req.id
                    && b.consumer.capability == consumer.capability
                    && (catalog_unproven || b.consumer.catalog == consumer.catalog)
            })
            .map(|b| b.provider_catalog.as_str());
    }
    None
}

/// RA-6 ∩ RA-17: may inherit copy `param` from a parent fetch onto `consumer`?
///
/// No deployed seat → inherit (name intersection). Same `provider_catalog` as
/// the proven parent catalog → inherit. Foreign seat, or unproven parent
/// catalog while a seat is distinctly deployed → omit.
pub fn inherit_may_fill_param(
    deployments: &DeploymentBindings,
    consumer: &CapabilityRef,
    requirements: &[Requirement],
    param: &str,
    parent_catalog: Option<&str>,
) -> bool {
    let Some(seat_catalog) =
        deployed_seat_catalog_for_param(deployments, consumer, requirements, param)
    else {
        return true;
    };
    match parent_catalog.map(str::trim).filter(|s| !s.is_empty()) {
        Some(src) => src == seat_catalog,
        None => false,
    }
}

/// RA-17: a prerequisite seat may only be filled from its deployed provider.
///
/// Statically provable: producing catalog ≠ deployed `provider_catalog`, or the
/// same binding name used for two seats deployed to different providers.
/// Literals with no catalog provenance are not rejected unless they share a
/// binding name across distinct providers. JWT / token-string inspection is
/// not a law. RA-6 inherit uses [`inherit_may_fill_param`]: omit when the
/// parent catalog cannot prove the same deployed provider.
pub fn validate_deployed_prerequisite_seats(
    catalogs: &BTreeMap<String, &CGS>,
    deployments: &DeploymentBindings,
    consumer: &CapabilityRef,
    wirings: &[SeatWiring],
) -> Result<(), String> {
    if wirings.is_empty() {
        return Ok(());
    }
    let cgs = *catalogs
        .get(&consumer.catalog)
        .ok_or_else(|| format!("missing consumer catalog {}", consumer.catalog))?;
    let Some(requirements) = cgs.prerequisites.requirements.get(&consumer.capability) else {
        return Ok(());
    };
    if requirements.is_empty() {
        return Ok(());
    }

    let mut seat_provider: BTreeMap<&InputPath, (&str, &str, &str)> = BTreeMap::new();
    for req in requirements {
        let Some(dep) = deployments
            .bindings
            .iter()
            .find(|b| b.consumer == *consumer && b.requirement == req.id)
        else {
            continue;
        };
        for cb in &req.bindings {
            seat_provider.insert(
                &cb.input,
                (
                    req.id.as_str(),
                    dep.provider_catalog.as_str(),
                    dep.provider.as_str(),
                ),
            );
        }
    }
    if seat_provider.is_empty() {
        return Ok(());
    }

    let mut by_binding: BTreeMap<&str, Vec<(&InputPath, &str, &str)>> = BTreeMap::new();
    for w in wirings {
        let Some((_req_id, pcat, provider)) = seat_provider.get(&w.input) else {
            continue;
        };
        if let Some(src) = w.source_catalog.as_deref() {
            if src != *pcat {
                let seat = seat_wire_name(&w.input);
                return Err(format!(
                    "RA-17: prerequisite seat `{seat}` requires provider `{}`; bound value comes from catalog `{src}`",
                    deployed_provider_label(pcat, provider)
                ));
            }
        }
        if let Some(name) = w.binding.as_deref() {
            by_binding
                .entry(name)
                .or_default()
                .push((&w.input, *pcat, *provider));
        }
    }
    for (name, seats) in by_binding {
        let mut providers = BTreeSet::new();
        for (_, pcat, provider) in &seats {
            providers.insert((*pcat, *provider));
        }
        if providers.len() > 1 {
            let labels: Vec<String> = providers
                .iter()
                .map(|(c, p)| deployed_provider_label(c, p))
                .collect();
            let seat_names: Vec<String> = seats
                .iter()
                .map(|(inp, _, _)| seat_wire_name(inp))
                .collect();
            return Err(format!(
                "RA-17: prerequisite seats `{}` require distinct providers (`{}`); they cannot share binding `{name}`",
                seat_names.join("` and `"),
                labels.join("` vs `"),
            ));
        }
    }
    Ok(())
}

/// Resolve only explicit deployment bindings; every dependency must be permitted.
pub fn prerequisite_closure(
    catalogs: &BTreeMap<String, &CGS>,
    bindings: &DeploymentBindings,
    business: &[CapabilityRef],
    allowed: &BTreeSet<String>,
) -> Result<PrerequisiteClosure, String> {
    let mut binding_index = BTreeMap::new();
    for binding in &bindings.bindings {
        let key = (binding.consumer.clone(), binding.requirement.clone());
        if binding_index.insert(key, binding).is_some() {
            return Err("duplicate deployment prerequisite binding".into());
        }
    }
    let mut walker = ClosureWalker {
        catalogs,
        bindings: binding_index,
        allowed,
        visiting: BTreeSet::new(),
        done: BTreeSet::new(),
        edges: Vec::new(),
        order: Vec::new(),
        acquisitions: Vec::new(),
    };
    for selected in business {
        walker.visit(selected, None, &BTreeMap::new())?;
    }
    let selected: BTreeSet<_> = business.iter().cloned().collect();
    Ok(PrerequisiteClosure {
        acquisitions: walker.acquisitions,
        business: selected.iter().cloned().collect(),
        prerequisites: walker
            .order
            .into_iter()
            .filter(|id| !selected.contains(id))
            .collect(),
        edges: walker.edges,
    })
}

struct ClosureWalker<'a> {
    catalogs: &'a BTreeMap<String, &'a CGS>,
    bindings: BTreeMap<(CapabilityRef, String), &'a DeploymentBinding>,
    allowed: &'a BTreeSet<String>,
    visiting: BTreeSet<CapabilityRef>,
    done: BTreeSet<String>,
    edges: Vec<PrerequisiteEdge>,
    order: Vec<CapabilityRef>,
    acquisitions: Vec<Acquisition>,
}

#[derive(Default)]
struct RequirementResolutionState {
    resolved: BTreeMap<String, (String, BTreeMap<String, String>)>,
    visiting: BTreeSet<String>,
}

impl ClosureWalker<'_> {
    fn visit(
        &mut self,
        consumer: &CapabilityRef,
        instance: Option<&str>,
        inputs: &BTreeMap<InputPath, ResolvedArgument>,
    ) -> Result<(), String> {
        if !self.allowed.contains(&consumer.catalog) {
            return Err(format!(
                "prerequisite catalog not permitted: {}",
                consumer.catalog
            ));
        }
        if !self.visiting.insert(consumer.clone()) {
            return Err(format!("prerequisite capability cycle: {consumer:?}"));
        }
        let cgs = *self
            .catalogs
            .get(&consumer.catalog)
            .ok_or("missing prerequisite catalog")?;
        capability(cgs, &consumer.capability)?;
        cgs.prerequisites.validate(cgs)?;
        let requirements = cgs
            .prerequisites
            .requirements
            .get(&consumer.capability)
            .cloned()
            .unwrap_or_default();
        let mut resolution = RequirementResolutionState::default();
        for requirement in &requirements {
            self.resolve_requirement(
                consumer,
                instance,
                inputs,
                &requirements,
                requirement,
                &mut resolution,
            )?;
        }
        self.visiting.remove(consumer);
        if !self.order.contains(consumer) {
            self.order.push(consumer.clone());
        }
        Ok(())
    }

    fn resolve_requirement(
        &mut self,
        consumer: &CapabilityRef,
        consumer_instance: Option<&str>,
        inputs: &BTreeMap<InputPath, ResolvedArgument>,
        requirements: &[Requirement],
        requirement: &Requirement,
        resolution: &mut RequirementResolutionState,
    ) -> Result<(String, BTreeMap<String, String>), String> {
        if let Some(result) = resolution.resolved.get(&requirement.id) {
            return Ok(result.clone());
        }
        if !resolution.visiting.insert(requirement.id.clone()) {
            return Err("prerequisite argument cycle".into());
        }
        let cgs = *self
            .catalogs
            .get(&consumer.catalog)
            .ok_or("missing consumer catalog")?;
        let binding = *self
            .bindings
            .get(&(consumer.clone(), requirement.id.clone()))
            .ok_or_else(|| {
                format!(
                    "missing deployment binding: {consumer:?}/{}",
                    requirement.id
                )
            })?;
        if !self.allowed.contains(&binding.provider_catalog) {
            return Err("provider catalog not permitted".into());
        }
        let provider_cgs = *self
            .catalogs
            .get(&binding.provider_catalog)
            .ok_or("missing provider catalog")?;
        let provider = provider_cgs
            .prerequisites
            .providers
            .get(&binding.provider)
            .ok_or("missing declared provider")?
            .clone();
        let required = cgs.prerequisites.contract(&requirement.contract)?;
        let supplied = provider_cgs.prerequisites.contract(&provider.contract)?;
        if provider.contract != requirement.contract
            || required.version != supplied.version
            || required.inputs.keys().ne(supplied.inputs.keys())
            || required.outputs.keys().ne(supplied.outputs.keys())
        {
            return Err("incompatible provider contract".into());
        }
        for (port, value) in &required.inputs {
            same_type(
                value_type(cgs, value)?,
                value_type(provider_cgs, &supplied.inputs[port])?,
                port,
            )?;
        }
        for (port, value) in &required.outputs {
            same_type(
                value_type(cgs, value)?,
                value_type(provider_cgs, &supplied.outputs[port])?,
                port,
            )?;
        }
        let mut arguments = BTreeMap::new();
        for (port, source) in &requirement.arguments {
            let dependency = match source {
                ArgumentSource::RequirementOutput {
                    requirement,
                    output,
                } => Some((requirement.as_str(), output.as_str())),
                ArgumentSource::ConsumerInput { input } => requirements.iter().find_map(|r| {
                    r.bindings
                        .iter()
                        .find(|b| &b.input == input)
                        .map(|b| (r.id.as_str(), b.output.as_str()))
                }),
                _ => None,
            };
            let argument = if let Some((id, output)) = dependency {
                let other = requirements
                    .iter()
                    .find(|r| r.id == id)
                    .ok_or("unknown argument requirement")?;
                let (instance, outputs) = self.resolve_requirement(
                    consumer,
                    consumer_instance,
                    inputs,
                    requirements,
                    other,
                    resolution,
                )?;
                ResolvedArgument::ProviderOutput {
                    instance,
                    field: outputs
                        .get(output)
                        .ok_or("unknown provider output")?
                        .clone(),
                }
            } else {
                match source {
                    ArgumentSource::ConsumerIdentity { field } => {
                        ResolvedArgument::BusinessIdentity {
                            consumer: consumer.clone(),
                            field: field.clone(),
                        }
                    }
                    ArgumentSource::Constant { value } => ResolvedArgument::Constant {
                        value: value.clone(),
                    },
                    ArgumentSource::ConsumerInput { input } => inputs
                        .get(input)
                        .cloned()
                        .unwrap_or_else(|| ResolvedArgument::BusinessInput {
                            consumer: consumer.clone(),
                            input: input.clone(),
                        }),
                    ArgumentSource::RequirementOutput { .. } => {
                        return Err("unresolved prerequisite output".into())
                    }
                }
            };
            arguments.insert(port.clone(), argument);
        }
        let identity =
            serde_json::to_vec(&(&binding.provider_catalog, &binding.provider, &arguments))
                .map_err(|e| e.to_string())?;
        let id = crate::catalog_discovery::content_hash(&identity);
        let provider_ref = CapabilityRef {
            catalog: binding.provider_catalog.clone(),
            capability: provider.capability.clone(),
        };
        if !self.done.contains(&id) {
            let provider_inputs = provider
                .inputs
                .iter()
                .map(|(port, input)| (input.clone(), arguments[port].clone()))
                .collect();
            self.visit(&provider_ref, Some(&id), &provider_inputs)?;
            self.acquisitions.push(Acquisition {
                id: id.clone(),
                provider_catalog: binding.provider_catalog.clone(),
                provider: binding.provider.clone(),
                capability: provider_ref.clone(),
                arguments,
            });
            self.done.insert(id.clone());
        }
        self.edges.push(PrerequisiteEdge {
            consumer: consumer.clone(),
            consumer_instance: consumer_instance.map(str::to_owned),
            provider_instance: id.clone(),
            requirement: requirement.clone(),
            provider: provider_ref,
            provider_inputs: provider.inputs,
            provider_outputs: provider.outputs.clone(),
        });
        let result = (id, provider.outputs);
        resolution
            .resolved
            .insert(requirement.id.clone(), result.clone());
        resolution.visiting.remove(&requirement.id);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> CGS {
        crate::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .expect("abstract prerequisite matrix")
    }

    #[test]
    fn target_identity_is_structural_and_queries_cannot_supply_it() {
        let mut cgs = fixture();
        cgs.prerequisites.requirements.get_mut("read").unwrap()[0]
            .arguments
            .insert(
                "account".into(),
                ArgumentSource::ConsumerIdentity { field: "id".into() },
            );
        assert!(
            cgs.prerequisites.validate(&cgs).is_err(),
            "a collection has no single target identity"
        );
        cgs.capabilities.get_mut("read").unwrap().kind = crate::CapabilityKind::Get;
        cgs.prerequisites.validate(&cgs).unwrap();
        let business = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let bindings = DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: business.clone(),
                requirement: "scoped_access".into(),
                provider_catalog: "matrix".into(),
                provider: "value_source".into(),
            }],
        };
        let closure = prerequisite_closure(
            &BTreeMap::from([("matrix".into(), &cgs)]),
            &bindings,
            std::slice::from_ref(&business),
            &BTreeSet::from(["matrix".into()]),
        )
        .unwrap();
        assert!(closure.acquisitions.iter().any(|acquisition| acquisition.arguments.values().any(|argument| matches!(argument, ResolvedArgument::BusinessIdentity { consumer, field } if consumer == &business && field == "id"))));
        cgs.prerequisites.requirements.get_mut("read").unwrap()[0]
            .arguments
            .insert(
                "account".into(),
                ArgumentSource::ConsumerIdentity {
                    field: "invented".into(),
                },
            );
        assert!(cgs.prerequisites.validate(&cgs).is_err());
    }

    #[test]
    fn declared_dependency_needs_no_intent_terms() {
        let cgs = fixture();
        let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
        let business = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let bindings = DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: business.clone(),
                requirement: "scoped_access".into(),
                provider_catalog: "matrix".into(),
                provider: "value_source".into(),
            }],
        };
        let closure = prerequisite_closure(
            &catalogs,
            &bindings,
            &[business],
            &BTreeSet::from(["matrix".into()]),
        )
        .unwrap();
        assert_eq!(closure.prerequisites.len(), 1);
        assert_eq!(closure.prerequisites[0].capability, "acquire");
        assert_eq!(closure.edges[0].requirement.id, "scoped_access");
        let exposure = crate::TeachingExposureSession::new(
            &cgs,
            "matrix",
            &["BusinessRecord", "ProviderResult"],
        );
        let symbols = exposure.to_symbol_map();
        let guidance = crate::prompt_render::render_prerequisite_bindings(
            &closure,
            &catalogs,
            symbols.as_ref(),
        )
        .unwrap();
        assert!(guidance.contains("scoped_access"));
        assert!(
            guidance.contains(".m"),
            "action acquisitions use taught e#.m# not e# / m#:\n{guidance}"
        );
        assert!(
            guidance.contains("{…}"),
            "query consumers use taught e#{{…}} not e# / m#:\n{guidance}"
        );
        assert!(
            !guidance.contains(" / "),
            "prerequisite seats must not use e# / m#:\n{guidance}"
        );
        assert!(!guidance.contains("context="));
    }

    #[test]
    fn get_acquisition_teaches_parens_id_not_slash_method() {
        let mut cgs = fixture();
        cgs.capabilities.get_mut("acquire").unwrap().kind = crate::CapabilityKind::Get;
        let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
        let business = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let bindings = DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: business.clone(),
                requirement: "scoped_access".into(),
                provider_catalog: "matrix".into(),
                provider: "value_source".into(),
            }],
        };
        let closure = prerequisite_closure(
            &catalogs,
            &bindings,
            std::slice::from_ref(&business),
            &BTreeSet::from(["matrix".into()]),
        )
        .unwrap();
        let exposure = crate::TeachingExposureSession::new(
            &cgs,
            "matrix",
            &["BusinessRecord", "ProviderResult"],
        );
        let symbols = exposure.to_symbol_map();
        let guidance = crate::prompt_render::render_prerequisite_bindings(
            &closure,
            &catalogs,
            symbols.as_ref(),
        )
        .unwrap();
        let get_sym = symbols.entity_sym_for("matrix", "ProviderResult");
        assert!(
            guidance.contains(&format!("{get_sym}(<id>)")),
            "Get acquisition must name taught e#(<id>), got:\n{guidance}"
        );
        assert!(
            !guidance.contains(" / "),
            "Get must not be taught as e# / m#:\n{guidance}"
        );
        assert!(
            !guidance.contains(&format!("{get_sym}.m")),
            "Get must not be taught as a dotted mutator:\n{guidance}"
        );
    }

    #[test]
    fn get_identity_constant_fills_taught_parens() {
        let mut cgs = fixture();
        cgs.capabilities.get_mut("acquire").unwrap().kind = crate::CapabilityKind::Get;
        let ent = cgs.entities.get_mut("ProviderResult").unwrap();
        let value_field = ent.fields.get("value").expect("value field").clone();
        ent.fields.insert("account".into(), value_field);
        ent.id_field = "account".into();
        let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
        let business = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let bindings = DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: business.clone(),
                requirement: "scoped_access".into(),
                provider_catalog: "matrix".into(),
                provider: "value_source".into(),
            }],
        };
        let closure = prerequisite_closure(
            &catalogs,
            &bindings,
            std::slice::from_ref(&business),
            &BTreeSet::from(["matrix".into()]),
        )
        .unwrap();
        let exposure = crate::TeachingExposureSession::new(
            &cgs,
            "matrix",
            &["BusinessRecord", "ProviderResult"],
        );
        let symbols = exposure.to_symbol_map();
        let guidance = crate::prompt_render::render_prerequisite_bindings(
            &closure,
            &catalogs,
            symbols.as_ref(),
        )
        .unwrap();
        let get_sym = symbols.entity_sym_for("matrix", "ProviderResult");
        assert!(
            guidance.contains(&format!("{get_sym}(\"account-a\")")),
            "constant Get identity must fill e#(\"…\"), got:\n{guidance}"
        );
        assert!(
            !guidance.contains(&format!("{get_sym}(<id>)")),
            "filled identity should not also show the hole:\n{guidance}"
        );
        assert!(!guidance.contains(" / "));
    }

    #[test]
    fn acquisition_identity_includes_bound_arguments() {
        let mut cgs = fixture();
        let mut other_cap = cgs.capabilities.get("read").unwrap().clone();
        other_cap.name = "other_read".into();
        cgs.capabilities.insert("other_read".into(), other_cap);
        let requirements = cgs.prerequisites.requirements["read"].clone();
        cgs.prerequisites
            .requirements
            .insert("other_read".into(), requirements);
        let roots: Vec<_> = ["read", "other_read"]
            .into_iter()
            .map(|name| CapabilityRef {
                catalog: "matrix".into(),
                capability: name.into(),
            })
            .collect();
        let bindings = DeploymentBindings {
            bindings: roots
                .iter()
                .map(|consumer| DeploymentBinding {
                    consumer: consumer.clone(),
                    requirement: "scoped_access".into(),
                    provider_catalog: "matrix".into(),
                    provider: "value_source".into(),
                })
                .collect(),
        };
        let allowed = BTreeSet::from(["matrix".into()]);
        let first = prerequisite_closure(
            &BTreeMap::from([("matrix".into(), &cgs)]),
            &bindings,
            &roots,
            &allowed,
        )
        .unwrap();
        assert_eq!(first.acquisitions.len(), 1);
        assert_eq!(first.edges.len(), 2);
        cgs.prerequisites
            .requirements
            .get_mut("other_read")
            .unwrap()[0]
            .arguments
            .insert(
                "account".into(),
                ArgumentSource::Constant {
                    value: "account-b".into(),
                },
            );
        let second = prerequisite_closure(
            &BTreeMap::from([("matrix".into(), &cgs)]),
            &bindings,
            &roots,
            &allowed,
        )
        .unwrap();
        assert_eq!(second.acquisitions.len(), 2);
        assert_ne!(
            second.edges[0].provider_instance,
            second.edges[1].provider_instance
        );
    }

    #[test]
    fn provider_cannot_omit_required_input() {
        let mut cgs = fixture();
        cgs.prerequisites
            .contracts
            .get_mut("scoped_value")
            .unwrap()
            .inputs
            .clear();
        cgs.prerequisites
            .providers
            .get_mut("value_source")
            .unwrap()
            .inputs
            .clear();
        assert!(cgs
            .prerequisites
            .validate(&cgs)
            .unwrap_err()
            .contains("unsupplied required input"));
    }

    #[test]
    fn provider_must_have_single_entity_output() {
        let mut cgs = fixture();
        cgs.capabilities
            .get_mut("acquire")
            .unwrap()
            .output_schema
            .as_mut()
            .unwrap()
            .output_type = crate::schema::OutputType::Collection {
            entity_type: "ProviderResult".into(),
            max_count: Some(1),
        };
        assert!(cgs
            .prerequisites
            .validate(&cgs)
            .unwrap_err()
            .contains("one declared entity"));
    }

    #[test]
    fn consumer_input_alias_does_not_hide_cycle() {
        let mut cgs = fixture();
        let requirement = &mut cgs.prerequisites.requirements.get_mut("read").unwrap()[0];
        requirement.arguments.insert(
            "account".into(),
            ArgumentSource::ConsumerInput {
                input: requirement.bindings[0].input.clone(),
            },
        );
        assert!(cgs
            .prerequisites
            .validate(&cgs)
            .unwrap_err()
            .contains("argument cycle"));
    }

    fn dual_session_catalogs() -> (CGS, CGS) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/prerequisite_dual_session");
        let source = crate::loader::load_schema_dir(&root.join("source"))
            .expect("abstract source session catalog");
        let consumer = crate::loader::load_schema_dir(&root.join("consumer"))
            .expect("abstract consumer dual-token catalog");
        (source, consumer)
    }

    #[test]
    fn requirement_cannot_bind_one_output_to_two_inputs() {
        let (_, mut consumer) = dual_session_catalogs();
        let requirements = consumer
            .prerequisites
            .requirements
            .get_mut("attach")
            .unwrap();
        let extra = requirements[1].bindings[0].clone();
        requirements[0].bindings.push(extra);
        requirements.remove(1);
        let err = consumer.prerequisites.validate(&consumer).unwrap_err();
        assert!(
            err.contains("binds contract output token to more than one input"),
            "collapse must fail validate: {err}"
        );
    }

    #[test]
    fn dual_session_teaching_names_each_provider() {
        let (source, consumer) = dual_session_catalogs();
        let catalogs = BTreeMap::from([("source".into(), &source), ("consumer".into(), &consumer)]);
        let business = CapabilityRef {
            catalog: "consumer".into(),
            capability: "attach".into(),
        };
        let bindings = DeploymentBindings {
            bindings: vec![
                DeploymentBinding {
                    consumer: business.clone(),
                    requirement: "session".into(),
                    provider_catalog: "consumer".into(),
                    provider: "session".into(),
                },
                DeploymentBinding {
                    consumer: business.clone(),
                    requirement: "source_session".into(),
                    provider_catalog: "source".into(),
                    provider: "session".into(),
                },
            ],
        };
        let closure = prerequisite_closure(
            &catalogs,
            &bindings,
            std::slice::from_ref(&business),
            &BTreeSet::from(["source".into(), "consumer".into()]),
        )
        .unwrap();
        let consumer_acq = closure
            .acquisitions
            .iter()
            .find(|a| a.provider_catalog == "consumer")
            .expect("consumer login acquisition");
        let source_acq = closure
            .acquisitions
            .iter()
            .find(|a| a.provider_catalog == "source")
            .expect("source login acquisition");
        assert_ne!(consumer_acq.id, source_acq.id);

        let mut exposure = crate::TeachingExposureSession::new(&source, "source", &["AuthSession"]);
        exposure.expose_entities(
            &[&source, &consumer],
            std::sync::Arc::new(consumer.clone()),
            "consumer",
            &["AuthSession", "Record"],
        );
        let symbols = exposure.to_symbol_map();
        let guidance = crate::prompt_render::render_prerequisite_bindings(
            &closure,
            &catalogs,
            symbols.as_ref(),
        )
        .unwrap();
        assert!(
            guidance.contains(&format!(
                "Payload.access_token <- acquisition {} output",
                consumer_acq.id
            )),
            "local token must bind consumer login:\n{guidance}"
        );
        assert!(
            guidance.contains(&format!(
                "Payload.source_access_token <- acquisition {} output",
                source_acq.id
            )),
            "foreign token must bind source login:\n{guidance}"
        );
        assert!(
            !guidance.contains(&format!(
                "Payload.source_access_token <- acquisition {} output",
                consumer_acq.id
            )),
            "foreign token must not alias consumer login:\n{guidance}"
        );
        assert!(
            !guidance.contains(&format!(
                "Payload.access_token <- acquisition {} output",
                source_acq.id
            )),
            "local token must not alias source login:\n{guidance}"
        );
    }

    fn dual_session_deployments(business: &CapabilityRef) -> DeploymentBindings {
        DeploymentBindings {
            bindings: vec![
                DeploymentBinding {
                    consumer: business.clone(),
                    requirement: "session".into(),
                    provider_catalog: "consumer".into(),
                    provider: "session".into(),
                },
                DeploymentBinding {
                    consumer: business.clone(),
                    requirement: "source_session".into(),
                    provider_catalog: "source".into(),
                    provider: "session".into(),
                },
            ],
        }
    }

    fn attach_consumer() -> CapabilityRef {
        CapabilityRef {
            catalog: "consumer".into(),
            capability: "attach".into(),
        }
    }

    fn payload_seat(name: &str) -> InputPath {
        InputPath {
            lane: InputLane::Payload,
            path: vec![name.into()],
        }
    }

    #[test]
    fn ra17_rejects_consumer_acquisition_on_source_seat() {
        let (source, consumer) = dual_session_catalogs();
        let catalogs = BTreeMap::from([("source".into(), &source), ("consumer".into(), &consumer)]);
        let business = attach_consumer();
        let err = validate_deployed_prerequisite_seats(
            &catalogs,
            &dual_session_deployments(&business),
            &business,
            &[SeatWiring {
                input: payload_seat("source_access_token"),
                binding: Some("sw".into()),
                source_catalog: Some("consumer".into()),
            }],
        )
        .expect_err("foreign catalog on source seat");
        assert!(
            err.contains("source:session"),
            "reject must name required provider:\n{err}"
        );
        assert!(
            err.contains("consumer"),
            "reject must name the foreign catalog:\n{err}"
        );
        assert!(
            err.contains("source_access_token"),
            "reject must name the seat:\n{err}"
        );
    }

    #[test]
    fn ra17_rejects_shared_binding_across_distinct_providers() {
        let (source, consumer) = dual_session_catalogs();
        let catalogs = BTreeMap::from([("source".into(), &source), ("consumer".into(), &consumer)]);
        let business = attach_consumer();
        let err = validate_deployed_prerequisite_seats(
            &catalogs,
            &dual_session_deployments(&business),
            &business,
            &[
                SeatWiring {
                    input: payload_seat("access_token"),
                    binding: Some("sw".into()),
                    source_catalog: None,
                },
                SeatWiring {
                    input: payload_seat("source_access_token"),
                    binding: Some("sw".into()),
                    source_catalog: None,
                },
            ],
        )
        .expect_err("same binding on two providers");
        assert!(
            err.contains("source:session") && err.contains("consumer:session"),
            "reject must name both providers:\n{err}"
        );
        assert!(err.contains("`sw`"), "reject must name the binding:\n{err}");
    }

    #[test]
    fn ra17_accepts_matching_provider_catalogs() {
        let (source, consumer) = dual_session_catalogs();
        let catalogs = BTreeMap::from([("source".into(), &source), ("consumer".into(), &consumer)]);
        let business = attach_consumer();
        validate_deployed_prerequisite_seats(
            &catalogs,
            &dual_session_deployments(&business),
            &business,
            &[
                SeatWiring {
                    input: payload_seat("access_token"),
                    binding: Some("sw".into()),
                    source_catalog: Some("consumer".into()),
                },
                SeatWiring {
                    input: payload_seat("source_access_token"),
                    binding: Some("fs".into()),
                    source_catalog: Some("source".into()),
                },
            ],
        )
        .expect("matching deployments are lawful");
    }

    fn file_query_consumer() -> CapabilityRef {
        CapabilityRef {
            catalog: "consumer".into(),
            capability: "file_query".into(),
        }
    }

    fn file_query_source_deployments() -> DeploymentBindings {
        DeploymentBindings {
            bindings: vec![DeploymentBinding {
                consumer: file_query_consumer(),
                requirement: "source_session".into(),
                provider_catalog: "source".into(),
                provider: "session".into(),
            }],
        }
    }

    #[test]
    fn inherit_may_fill_param_omits_foreign_and_unproven() {
        let (_, consumer) = dual_session_catalogs();
        let reqs = consumer
            .prerequisites
            .requirements
            .get("file_query")
            .expect("file_query source seat");
        let file = file_query_consumer();
        let deps = file_query_source_deployments();
        assert!(
            inherit_may_fill_param(&deps, &file, reqs, "id", Some("consumer")),
            "scope id is not a deployed seat"
        );
        assert!(
            !inherit_may_fill_param(&deps, &file, reqs, "access_token", Some("consumer")),
            "consumer parent must not fill source-deployed access_token"
        );
        assert!(
            inherit_may_fill_param(&deps, &file, reqs, "access_token", Some("source")),
            "matching source parent may fill the source seat"
        );
        assert!(
            !inherit_may_fill_param(&deps, &file, reqs, "access_token", None),
            "unproven parent catalog must omit a deployed seat"
        );
        assert!(
            inherit_may_fill_param(
                &DeploymentBindings::default(),
                &file,
                reqs,
                "access_token",
                Some("consumer")
            ),
            "no deployment is not a distinct foreign seat"
        );
        let unproven = CapabilityRef {
            catalog: String::new(),
            capability: "file_query".into(),
        };
        assert!(
            !inherit_may_fill_param(&deps, &unproven, reqs, "access_token", Some("consumer")),
            "empty consumer catalog still finds the source deployment by capability"
        );
        assert!(
            inherit_may_fill_param(&deps, &unproven, reqs, "access_token", Some("source")),
            "matching provider still inherits when catalog id is unbound"
        );
    }
}
