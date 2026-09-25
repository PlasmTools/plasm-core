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
use crate::{CapabilityKind, FieldType, NamedValueSchema, CGS};
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
    /// Reads selected to populate business inputs or establish identity membership.
    /// These are not themselves requested business effects.
    #[serde(default)]
    pub input_sources: Vec<CapabilityRef>,
    pub prerequisites: Vec<CapabilityRef>,
    pub edges: Vec<PrerequisiteEdge>,
}

/// A source feeds either an ordinary argument or a typed operation receiver.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputSourceTarget {
    Argument { input: InputPath },
    Receiver { entity: crate::EntityName },
}

/// A structurally lawful way for one read capability to populate a required
/// business input. These edges are projected from CGS input/output types at
/// routing time; catalogs do not author or maintain a second workflow graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSourceBinding {
    pub consumer: CapabilityRef,
    pub input: InputSourceTarget,
    pub provider: CapabilityRef,
    pub output_field: String,
    /// The provider yields rows whose scalar field is collected into an array seat.
    pub collect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RowIdentity {
    Field { field: crate::EntityFieldName },
    Relation { relation: crate::RelationName },
}

/// A read whose identity values can establish membership or absence for rows
/// supplied to a business input. This is evidence, not an input assignment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSourceMembership {
    pub source: CapabilityRef,
    pub source_identity: RowIdentity,
    pub provider_identity: RowIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSourceCandidate {
    pub provider: CapabilityRef,
    pub projection: crate::entity_projection::EntityReadProjection,
    pub bindings: Vec<InputSourceBinding>,
    pub membership: Vec<InputSourceMembership>,
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
    /// Exact consumer seats supplied by declared acquisitions. Names, descriptions,
    /// primitive types and shared value domains are not acquisition evidence.
    /// Callers operating on untrusted catalogs must validate this contract first.
    pub fn acquired_inputs(&self, capability: &str) -> BTreeSet<&InputPath> {
        self.requirements
            .get(capability)
            .into_iter()
            .flatten()
            .flat_map(|requirement| requirement.bindings.iter().map(|binding| &binding.input))
            .collect()
    }

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
                Some(crate::schema::OutputType::Entity { entity_type })
                | Some(crate::schema::OutputType::Collection { entity_type, .. }) => {
                    entity_type.as_str()
                }
                _ => {
                    return Err(format!(
                        "provider {id} must produce a declared entity or collection"
                    ))
                }
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
                            // A complete local identity inhabits its entity-reference domain.
                            // A component of a compound key does not.
                            let identity_reference = entity.key_vars.len() <= 1
                                && field.as_str() == entity.id_field.as_str()
                                && expected.field_type.entity_ref_target()
                                    == Some(cap.domain.as_str())
                                && expected.field_type.entity_ref_entry_id()
                                    == cgs.entry_id.as_deref();
                            if !identity_reference {
                                same_type(expected, value, cap_name)?;
                            }
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

/// Close only explicit deployment prerequisites around selected capabilities.
/// Identity compatibility and possible source operations are discovery evidence,
/// never unconditional dependencies of publication or execution.
pub fn prerequisite_closure(
    catalogs: &BTreeMap<String, &CGS>,
    bindings: &DeploymentBindings,
    capabilities: &[CapabilityRef],
    allowed: &BTreeSet<String>,
) -> Result<PrerequisiteClosure, String> {
    prerequisite_closure_with_input_sources(catalogs, bindings, capabilities, &[], allowed)
}

/// Close infrastructure prerequisites around direct business work and the
/// input producers selected from [`project_input_source_candidates`]. The
/// caller may only select projected read capabilities with input or membership
/// evidence; this function rejects arbitrary or effectful additions.
pub fn prerequisite_closure_with_selected_sources<F>(
    catalogs: &BTreeMap<String, &CGS>,
    bindings: &DeploymentBindings,
    business: &[CapabilityRef],
    selected_input_sources: &[CapabilityRef],
    allowed: &BTreeSet<String>,
    source_permitted: F,
) -> Result<PrerequisiteClosure, String>
where
    F: Fn(&CapabilityRef) -> bool,
{
    let projected: BTreeSet<_> =
        project_input_source_candidates(catalogs, business, &source_permitted)?
            .into_iter()
            .map(|candidate| candidate.provider)
            .collect();
    for source in selected_input_sources {
        if !projected.contains(source) {
            return Err(format!(
                "selected input source is not a projected producer: {source:?}"
            ));
        }
    }
    let input_sources: Vec<_> = selected_input_sources
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    prerequisite_closure_with_input_sources(catalogs, bindings, business, &input_sources, allowed)
}

fn prerequisite_closure_with_input_sources(
    catalogs: &BTreeMap<String, &CGS>,
    bindings: &DeploymentBindings,
    business: &[CapabilityRef],
    input_sources: &[CapabilityRef],
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
    for source in input_sources {
        walker.visit(source, None, &BTreeMap::new())?;
    }
    let selected: BTreeSet<_> = business.iter().cloned().collect();
    let sources: BTreeSet<_> = input_sources.iter().cloned().collect();
    Ok(PrerequisiteClosure {
        acquisitions: walker.acquisitions,
        business: selected.iter().cloned().collect(),
        input_sources: sources.iter().cloned().collect(),
        prerequisites: walker
            .order
            .into_iter()
            .filter(|id| !selected.contains(id) && !sources.contains(id))
            .collect(),
        edges: walker.edges,
    })
}

/// Project candidate business-data producers from the current E/R graphs.
///
/// The projection is intentionally vocabulary-blind. It does not inspect the
/// intent, description wording, field names, or enum members. An unlabeled
/// wire registry row supplies no nominal semantic evidence. It connects required,
/// non-prerequisite consumer seats to fields populated by read capabilities
/// when their catalog types are compatible. A later selector may choose among
/// these lawful edges using the original intent; it cannot invent an edge.
pub fn project_input_source_candidates<F>(
    catalogs: &BTreeMap<String, &CGS>,
    business: &[CapabilityRef],
    source_permitted: &F,
) -> Result<Vec<InputSourceCandidate>, String>
where
    F: Fn(&CapabilityRef) -> bool,
{
    let mut by_provider: BTreeMap<CapabilityRef, BTreeSet<InputSourceBinding>> = BTreeMap::new();
    for consumer in business {
        let consumer_cgs = *catalogs
            .get(&consumer.catalog)
            .ok_or("missing input-source consumer catalog")?;
        let consumer_cap = capability(consumer_cgs, &consumer.capability)?;
        if let Some(receiver) = consumer_cap.receiver_entity() {
            let entity = &consumer_cgs.entities[receiver];
            for producer in consumer_cgs.capabilities.values().filter(|cap| {
                cap.domain == *receiver
                    && matches!(
                        cap.kind,
                        CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
                    )
            }) {
                let provider = CapabilityRef {
                    catalog: consumer.catalog.clone(),
                    capability: producer.name.to_string(),
                };
                if !source_permitted(&provider) {
                    continue;
                }
                let projection = consumer_cgs.entity_read_projection(producer, |cap| {
                    source_permitted(&CapabilityRef {
                        catalog: consumer.catalog.clone(),
                        capability: cap.name.to_string(),
                    })
                });
                if projection
                    .available_fields()
                    .any(|field| field == entity.id_field.as_str())
                {
                    by_provider
                        .entry(provider.clone())
                        .or_default()
                        .insert(InputSourceBinding {
                            consumer: consumer.clone(),
                            input: InputSourceTarget::Receiver {
                                entity: receiver.clone(),
                            },
                            provider,
                            output_field: entity.id_field.to_string(),
                            collect: false,
                        });
                }
            }
        }
        let prerequisite_seats = consumer_cgs
            .prerequisites
            .acquired_inputs(&consumer.capability);
        for input in required_inputs(consumer_cap)?
            .into_iter()
            .filter(|input| !prerequisite_seats.contains(input))
        {
            let Some(consumer_value_ref) = input_value_ref(consumer_cap, &input)? else {
                continue;
            };
            let consumer_value = input_type(consumer_cgs, consumer_cap, &input)?;
            for (provider_catalog, provider_cgs) in catalogs {
                for provider_cap in provider_cgs.capabilities.values().filter(|capability| {
                    matches!(
                        capability.kind,
                        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
                    )
                }) {
                    let provider = CapabilityRef {
                        catalog: provider_catalog.clone(),
                        capability: provider_cap.name.to_string(),
                    };
                    if !source_permitted(&provider) {
                        continue;
                    }
                    let entity = provider_cgs
                        .entities
                        .get(&provider_cap.domain)
                        .ok_or("input-source provider entity missing")?;
                    for output_field in readable_fields(
                        provider_cgs,
                        provider_catalog,
                        provider_cap,
                        source_permitted,
                    ) {
                        let Some(field) = entity.fields.get(output_field.as_str()) else {
                            continue;
                        };
                        let provider_value =
                            field.named_value(provider_cgs).map_err(|e| e.to_string())?;
                        let Some(collect) = projected_type_compatibility(
                            consumer_cgs,
                            provider_cgs,
                            consumer_value,
                            provider_value,
                            provider_cap.kind,
                            consumer.catalog == *provider_catalog
                                && field.kind.registry_key() == consumer_value_ref,
                            consumer.catalog == *provider_catalog
                                && consumer_value.array_items.as_ref().is_some_and(|left| {
                                    provider_value.array_items.as_ref().is_some_and(|right| {
                                        left.kind.registry_key() == right.kind.registry_key()
                                    })
                                }),
                            consumer.catalog == *provider_catalog
                                && consumer_value.array_items.as_ref().is_some_and(|items| {
                                    items.kind.registry_key() == field.kind.registry_key()
                                }),
                        ) else {
                            continue;
                        };
                        by_provider.entry(provider.clone()).or_default().insert(
                            InputSourceBinding {
                                consumer: consumer.clone(),
                                input: InputSourceTarget::Argument {
                                    input: input.clone(),
                                },
                                provider: provider.clone(),
                                output_field,
                                collect,
                            },
                        );
                    }
                }
            }
        }
    }
    let mut sources: BTreeSet<_> = by_provider.keys().cloned().collect();
    for reference in business {
        let cap = capability(catalogs[&reference.catalog], &reference.capability)?;
        if matches!(
            cap.kind,
            CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
        ) {
            sources.insert(reference.clone());
        }
    }
    let mut evidence = project_membership_evidence(catalogs, &sources, source_permitted)?;
    let providers: BTreeSet<_> = by_provider.keys().chain(evidence.keys()).cloned().collect();
    Ok(providers
        .into_iter()
        .map(|provider| InputSourceCandidate {
            projection: catalogs[&provider.catalog].entity_read_projection(
                &catalogs[&provider.catalog].capabilities[provider.capability.as_str()],
                |cap| {
                    source_permitted(&CapabilityRef {
                        catalog: provider.catalog.clone(),
                        capability: cap.name.to_string(),
                    })
                },
            ),
            bindings: by_provider
                .remove(&provider)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            membership: evidence
                .remove(&provider)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            provider,
        })
        .collect())
}

fn readable_fields(
    cgs: &CGS,
    catalog: &str,
    cap: &CapabilitySchema,
    permitted: &impl Fn(&CapabilityRef) -> bool,
) -> Vec<String> {
    cgs.entity_read_projection(cap, |cap| {
        permitted(&CapabilityRef {
            catalog: catalog.into(),
            capability: cap.name.to_string(),
        })
    })
    .available_fields()
    .map(str::to_owned)
    .collect()
}

fn project_membership_evidence<F>(
    catalogs: &BTreeMap<String, &CGS>,
    sources: &BTreeSet<CapabilityRef>,
    source_permitted: &F,
) -> Result<BTreeMap<CapabilityRef, BTreeSet<InputSourceMembership>>, String>
where
    F: Fn(&CapabilityRef) -> bool,
{
    // Membership evidence joins identity-bearing outputs of direct producers.
    // Do not infer membership from a fuzzy query, nor expand through arbitrary
    // primitive strings. Authorization applies to both ends of every witness.
    let mut evidence: BTreeMap<CapabilityRef, BTreeSet<InputSourceMembership>> = BTreeMap::new();
    for source in sources {
        let source_cgs = catalogs[&source.catalog];
        let source_cap = capability(source_cgs, &source.capability)?;
        let source_entity = &source_cgs.entities[&source_cap.domain];
        for source_field in
            readable_fields(source_cgs, &source.catalog, source_cap, source_permitted)
        {
            let Some(field) = source_entity.fields.get(source_field.as_str()) else {
                continue;
            };
            let value = field.named_value(source_cgs).map_err(|e| e.to_string())?;
            if !matches!(
                value.domain.profile,
                Some(crate::value_domain::ProfileId::Email | crate::value_domain::ProfileId::E164)
            ) && !matches!(value.field_type, FieldType::EntityRef { .. })
            {
                continue;
            }
            for (catalog, cgs) in catalogs {
                for cap in cgs.capabilities.values().filter(|cap| {
                    matches!(
                        cap.kind,
                        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
                    )
                }) {
                    let provider = CapabilityRef {
                        catalog: catalog.clone(),
                        capability: cap.name.to_string(),
                    };
                    if provider == *source || !source_permitted(&provider) {
                        continue;
                    }
                    let entity = &cgs.entities[&cap.domain];
                    for output_field in readable_fields(cgs, catalog, cap, source_permitted) {
                        let Some(other) = entity.fields.get(output_field.as_str()) else {
                            continue;
                        };
                        let other_value = other.named_value(cgs).map_err(|e| e.to_string())?;
                        let references_identity = value.field_type.entity_ref_entry_id()
                            == Some(catalog.as_str())
                            && value.field_type.entity_ref_target() == Some(cap.domain.as_str())
                            && output_field == entity.id_field.as_str();
                        if references_identity
                            || semantic_value_type_eq(
                                value,
                                other_value,
                                source.catalog == *catalog
                                    && field.kind.registry_key() == other.kind.registry_key(),
                            )
                        {
                            evidence.entry(provider.clone()).or_default().insert(
                                InputSourceMembership {
                                    source: source.clone(),
                                    source_identity: RowIdentity::Field {
                                        field: source_field.clone().into(),
                                    },
                                    provider_identity: RowIdentity::Field {
                                        field: output_field.into(),
                                    },
                                },
                            );
                        }
                    }
                }
            }
        }
    }
    // Existing E/R relations are identity witnesses too: an artist directory
    // can constrain credited-artist membership without supplying a song id.
    for source in sources {
        let cgs = catalogs[&source.catalog];
        let cap = capability(cgs, &source.capability)?;
        for (name, relation) in &cgs.entities[&cap.domain].relations {
            let target = &cgs.entities[&relation.target_resource];
            for provider_cap in cgs.capabilities.values().filter(|candidate| {
                candidate.domain == relation.target_resource
                    && matches!(
                        candidate.kind,
                        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
                    )
            }) {
                let provider = CapabilityRef {
                    catalog: source.catalog.clone(),
                    capability: provider_cap.name.to_string(),
                };
                if provider == *source || !source_permitted(&provider) {
                    continue;
                }
                if readable_fields(cgs, &source.catalog, provider_cap, source_permitted)
                    .iter()
                    .any(|field| field == target.id_field.as_str())
                {
                    evidence
                        .entry(provider)
                        .or_default()
                        .insert(InputSourceMembership {
                            source: source.clone(),
                            source_identity: RowIdentity::Relation {
                                relation: name.clone(),
                            },
                            provider_identity: RowIdentity::Field {
                                field: target.id_field.clone(),
                            },
                        });
                }
            }
        }
    }
    // Reverse E/R membership: a collection's relation can identify which
    // candidate rows belong to it, without supplying an action argument.
    for source in sources {
        let cgs = catalogs[&source.catalog];
        let source_cap = capability(cgs, &source.capability)?;
        let source_entity = &cgs.entities[&source_cap.domain];
        if !readable_fields(cgs, &source.catalog, source_cap, source_permitted)
            .iter()
            .any(|field| field == source_entity.id_field.as_str())
        {
            continue;
        }
        for cap in cgs.capabilities.values().filter(|cap| {
            matches!(
                cap.kind,
                CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
            )
        }) {
            let provider = CapabilityRef {
                catalog: source.catalog.clone(),
                capability: cap.name.to_string(),
            };
            if provider == *source || !source_permitted(&provider) {
                continue;
            }
            for (name, relation) in &cgs.entities[&cap.domain].relations {
                if relation.target_resource != source_cap.domain {
                    continue;
                }
                evidence
                    .entry(provider.clone())
                    .or_default()
                    .insert(InputSourceMembership {
                        source: source.clone(),
                        source_identity: RowIdentity::Field {
                            field: source_entity.id_field.clone(),
                        },
                        provider_identity: RowIdentity::Relation {
                            relation: name.clone(),
                        },
                    });
            }
        }
    }
    Ok(evidence)
}

// Compare both catalog contracts and their projection provenance together.
#[allow(clippy::too_many_arguments)]
fn projected_type_compatibility(
    consumer_cgs: &CGS,
    provider_cgs: &CGS,
    consumer: &NamedValueSchema,
    provider: &NamedValueSchema,
    provider_kind: CapabilityKind,
    same_value_ref: bool,
    same_array_item_value_ref: bool,
    same_collected_item_value_ref: bool,
) -> Option<bool> {
    if semantic_value_type_eq(consumer, provider, same_value_ref) {
        return Some(false);
    }
    if let (Some(consumer_items), Some(provider_items)) =
        (&consumer.array_items, &provider.array_items)
    {
        let consumer_item = consumer_cgs.named_value_for_slot(consumer_items).ok()?;
        let provider_item = provider_cgs.named_value_for_slot(provider_items).ok()?;
        if semantic_value_type_eq(consumer_item, provider_item, same_array_item_value_ref) {
            return Some(false);
        }
    }
    let items = consumer.array_items.as_ref()?;
    if !matches!(
        provider_kind,
        CapabilityKind::Query | CapabilityKind::Search
    ) {
        return None;
    }
    let item = consumer_cgs.named_value_for_slot(items).ok()?;
    semantic_value_type_eq(item, provider, same_collected_item_value_ref).then_some(true)
}

pub(crate) fn semantic_value_type_eq(
    left: &NamedValueSchema,
    right: &NamedValueSchema,
    same_value_ref: bool,
) -> bool {
    if left.domain != right.domain
        || left.field_type != right.field_type
        || left.array_items != right.array_items
        || left.value_format != right.value_format
        || left.allowed_values != right.allowed_values
        || left.currency != right.currency
    {
        return false;
    }
    // An interned unlabeled wire shape is not a nominal semantic domain, even
    // within one catalog. Cross-catalog edges require a core profile (email,
    // e164, temporal, enum, etc.), money, or an exact entity reference.
    (same_value_ref && left.has_authored_meaning() && right.has_authored_meaning())
        || left.domain.profile.is_some()
        || matches!(
            left.domain.kernel,
            crate::value_domain::KernelKind::Money
                | crate::value_domain::KernelKind::EntityRef { .. }
        )
}

fn input_value_ref<'a>(
    capability: &'a CapabilitySchema,
    input: &InputPath,
) -> Result<Option<&'a crate::ValueDomainKey>, String> {
    if capability.derived.as_ref().is_some_and(|derived| {
        input.lane == InputLane::Selection && input.path == [derived.identity_field.clone()]
    }) {
        return Ok(None);
    }
    let fields = match input.lane {
        InputLane::Scope => &capability.inputs.scope.0,
        InputLane::Selection => &capability.inputs.selection.0,
        InputLane::Controls => &capability.inputs.controls.0,
        InputLane::Arguments | InputLane::Payload => {
            let schema = if input.lane == InputLane::Arguments {
                &capability.inputs.arguments
            } else {
                &capability.inputs.payload
            };
            match &schema.as_ref().ok_or("missing input lane")?.input_type {
                InputType::Object { fields, .. } => fields,
                _ => return Err("input-source path must address an object field".into()),
            }
        }
    };
    descend_input_value_ref(fields, &input.path)
}

fn descend_input_value_ref<'a>(
    fields: &'a [InputFieldSchema],
    path: &[String],
) -> Result<Option<&'a crate::ValueDomainKey>, String> {
    let (head, rest) = path.split_first().ok_or("empty input-source path")?;
    let field = fields
        .iter()
        .find(|field| &field.name == head)
        .ok_or_else(|| format!("missing input field {head}"))?;
    if rest.is_empty() {
        return match &field.wire {
            InputFieldWire::Registry(value_ref) => Ok(Some(value_ref)),
            InputFieldWire::Inline(_) => Ok(None),
        };
    }
    match &field.wire {
        InputFieldWire::Inline(input) => match input.as_ref() {
            InputType::Object { fields, .. } => descend_input_value_ref(fields, rest),
            _ => Err("input-source path traverses non-object input".into()),
        },
        InputFieldWire::Registry(_) => Err("input-source path traverses scalar input".into()),
    }
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
    fn membership_projection_preserves_typed_identity_witnesses_across_wire() {
        for identity in [
            crate::value_domain::ProfileId::Email,
            crate::value_domain::ProfileId::E164,
        ] {
            let mut consumer = fixture();
            let mut source = fixture();
            let mut directory = fixture();
            consumer.bind_registry_entry_id("consumer");
            source.bind_registry_entry_id("source");
            directory.bind_registry_entry_id("directory");
            for cgs in [&mut consumer, &mut source] {
                cgs.values.get_mut("text").unwrap().domain.profile =
                    Some(crate::value_domain::ProfileId::E164);
            }
            let mut email = directory.values["text"].clone();
            email.domain.profile = Some(identity);
            directory.values.insert("text".into(), email.clone());
            source.values.insert("identity".into(), email);
            let entity = source.entities.get_mut("BusinessRecord").unwrap();
            let mut field = entity.fields["id"].clone();
            field.name = "identity".into();
            field.kind = crate::schema::FieldValueKind::Registry(
                crate::ValueDomainKey::new("identity").unwrap(),
            );
            entity.fields.insert("identity".into(), field);
            source
                .capabilities
                .get_mut("read")
                .unwrap()
                .provides
                .push("identity".into());
            let business = CapabilityRef {
                catalog: "consumer".into(),
                capability: "operate".into(),
            };
            let catalogs = BTreeMap::from([
                ("consumer".into(), &consumer),
                ("source".into(), &source),
                ("directory".into(), &directory),
            ]);
            let candidates = project_input_source_candidates(
                &catalogs,
                std::slice::from_ref(&business),
                &|_| true,
            )
            .unwrap();
            let candidate = candidates
                .iter()
                .find(|c| c.provider.catalog == "directory" && c.provider.capability == "read")
                .unwrap();
            // Retrieval route does not decide whether a provider gets evidence.
            let with_provider = project_input_source_candidates(
                &catalogs,
                &[business.clone(), candidate.provider.clone()],
                &|_| true,
            )
            .unwrap();
            let same = with_provider
                .iter()
                .find(|c| c.provider == candidate.provider)
                .unwrap();
            assert_eq!(same.projection, candidate.projection);
            assert!(candidate.bindings.iter().all(|b| same.bindings.contains(b)));
            assert!(candidate
                .membership
                .iter()
                .all(|m| same.membership.contains(m)));
            assert!(candidate
                .membership
                .iter()
                .any(|w| w.source.catalog == "source"
                    && w.source_identity
                        == RowIdentity::Field {
                            field: "identity".into()
                        }
                    && w.provider_identity == RowIdentity::Field { field: "id".into() }));
            assert_eq!(
                serde_json::from_slice::<Vec<InputSourceCandidate>>(
                    &serde_json::to_vec(&candidates).unwrap()
                )
                .unwrap(),
                candidates
            );
            let restricted = project_input_source_candidates(&catalogs, &[business], &|r| {
                r.catalog != "directory"
            })
            .unwrap();
            assert!(restricted.iter().all(|c| c.provider.catalog != "directory"));
            assert!(candidates
                .iter()
                .all(|c| c.provider.capability != "acquire" && c.provider.capability != "create"));
        }
    }

    #[test]
    fn membership_foreign_keys_retain_catalog_qualified_identity() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let mut left = crate::loader::load_schema_dir(&path).unwrap();
        let mut right = left.clone();
        left.bind_registry_entry_id("left");
        right.bind_registry_entry_id("right");
        let catalogs = BTreeMap::from([("left".into(), &left), ("right".into(), &right)]);
        let business = CapabilityRef {
            catalog: "left".into(),
            capability: "langtag_query".into(),
        };
        let candidates =
            project_input_source_candidates(&catalogs, &[business], &|_| true).unwrap();
        let target = candidates
            .iter()
            .find(|candidate| {
                candidate.provider.catalog == "left"
                    && candidate.provider.capability == "langitem_get"
            })
            .unwrap();
        assert!(target
            .membership
            .iter()
            .any(|witness| witness.source_identity
                == RowIdentity::Field {
                    field: "item_id".into()
                }
                && witness.provider_identity == RowIdentity::Field { field: "id".into() }));
        assert!(candidates
            .iter()
            .filter(|candidate| candidate.provider.catalog == "right")
            .all(|candidate| candidate.membership.is_empty()));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn unlabeled_source_projection_is_invariant_under_wire_interning(key in "[a-z]{1,16}") {
            let mut cgs = fixture();
            cgs.values.get_mut("text").unwrap().description.clear();
            let business = CapabilityRef { catalog: "matrix".into(), capability: "operate".into() };
            let project = |schema: &CGS| project_input_source_candidates(
                &BTreeMap::from([("matrix".into(), schema)]), std::slice::from_ref(&business), &|_| true).unwrap();
            let before = project(&cgs);
            proptest::prop_assert!(!before.iter().flat_map(|c| &c.bindings).any(|b| matches!(b.input, InputSourceTarget::Argument { .. })), "unlabeled argument correspondence");
            let renamed = crate::ValueDomainKey::new(format!("wire_{key}")).unwrap();
            cgs.values.insert(renamed.to_string(), cgs.values["text"].clone());
            for entity in cgs.entities.values_mut() {
                for field in entity.fields.values_mut() {
                    field.kind = crate::schema::FieldValueKind::Registry(renamed.clone());
                }
            }
            let decoded: CGS = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
            proptest::prop_assert_eq!(before, project(&decoded));
            // A semantic profile remains evidence without an English label.
            let value = cgs.values.get_mut("text").unwrap();
            value.domain.profile = Some(crate::value_domain::ProfileId::Email);
            proptest::prop_assert!(semantic_value_type_eq(value, value, false));
        }

        #[test]
        fn membership_projection_relation_laws(
            relation_name in "[a-z]{1,16}",
            reverse in proptest::bool::ANY,
            direct_read in proptest::bool::ANY,
            permitted in proptest::bool::ANY,
        ) {
            let mut cgs = fixture();
            cgs.bind_registry_entry_id("matrix");
            cgs.entities.get_mut("BusinessRecord").unwrap().relations.insert(
                relation_name.clone().into(), crate::RelationSchema {
                    name: relation_name.clone().into(), description: "Recorded ownership".into(),
                    target_resource: "ProviderResult".into(), cardinality: crate::Cardinality::Many,
                    materialize: None, discovery: None,
                });
            let mut directory = cgs.capabilities["read"].clone();
            directory.name = "owners".into();
            directory.domain = "ProviderResult".into();
            directory.provides = vec!["value".into()];
            cgs.capabilities.insert("owners".into(), directory);
            let business = CapabilityRef { catalog: "matrix".into(),
                capability: if reverse { "owners" } else if direct_read { "read" } else { "operate" }.into() };
            let target = if reverse { "read" } else { "owners" };
            let authorize = |reference: &CapabilityRef| permitted || reference.capability != target;
            let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
            let candidates = project_input_source_candidates(&catalogs, std::slice::from_ref(&business), &authorize).unwrap();
            let roundtrip: CGS = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
            let decoded = project_input_source_candidates(&BTreeMap::from([("matrix".into(), &roundtrip)]), &[business], &authorize).unwrap();
            proptest::prop_assert_eq!(&candidates, &decoded);
            let result = candidates.iter().find(|candidate| candidate.provider.capability == target);
            if permitted {
                let expected = RowIdentity::Relation { relation: relation_name.into() };
                proptest::prop_assert!(result.unwrap().membership.iter().any(|w| {
                    (if reverse { &w.provider_identity } else { &w.source_identity }) == &expected
                }), "typed relation witness missing");
            } else {
                proptest::prop_assert!(result.is_none());
            }
            let wire: Vec<InputSourceCandidate> = serde_json::from_slice(&serde_json::to_vec(&candidates).unwrap()).unwrap();
            proptest::prop_assert_eq!(candidates, wire);
        }
    }

    #[test]
    fn input_source_projection_uses_types_not_intent_vocabulary() {
        let mut consumer = fixture();
        let mut source = fixture();
        consumer.bind_registry_entry_id("consumer");
        source.bind_registry_entry_id("source");
        for cgs in [&mut consumer, &mut source] {
            let value = cgs.values.get_mut("text").unwrap();
            value.domain.profile = Some(crate::value_domain::ProfileId::Email);
        }
        consumer.values.insert(
            "emails".into(),
            NamedValueSchema::from_domain(
                "Email values collected from matching rows".into(),
                crate::value_domain::ValueDomain::new(
                    crate::value_domain::KernelKind::Array,
                    None,
                    Default::default(),
                    None,
                    None,
                )
                .unwrap(),
                Some(crate::schema::ArrayItemsSchema {
                    kind: crate::schema::FieldValueKind::Registry(
                        crate::ValueDomainKey::new("text").unwrap(),
                    ),
                    field_type: FieldType::String,
                    value_format: None,
                    allowed_values: None,
                }),
            ),
        );
        let operate = consumer.capabilities.get_mut("operate").unwrap();
        let InputType::Object { fields, .. } =
            &mut operate.inputs.payload.as_mut().unwrap().input_type
        else {
            panic!("fixture payload object");
        };
        fields[0].wire = InputFieldWire::Registry(crate::ValueDomainKey::new("emails").unwrap());
        let business = CapabilityRef {
            catalog: "consumer".into(),
            capability: "operate".into(),
        };
        let catalogs = BTreeMap::from([("consumer".into(), &consumer), ("source".into(), &source)]);
        let candidates =
            project_input_source_candidates(&catalogs, std::slice::from_ref(&business), &|_| true)
                .unwrap();
        let source_read = candidates
            .iter()
            .find(|candidate| {
                candidate.provider.catalog == "source" && candidate.provider.capability == "read"
            })
            .expect("profile-compatible read producer");
        assert!(source_read.bindings.iter().any(|binding| {
            binding.consumer == business
                && binding.input
                    == InputSourceTarget::Argument {
                        input: InputPath {
                            lane: InputLane::Payload,
                            path: vec!["id".into()],
                        },
                    }
                && binding.output_field == "id"
                && binding.collect
        }));
    }

    #[test]
    fn input_source_projection_does_not_globalize_plain_strings() {
        let mut consumer = fixture();
        let mut source = fixture();
        consumer.bind_registry_entry_id("consumer");
        source.bind_registry_entry_id("source");
        let business = CapabilityRef {
            catalog: "consumer".into(),
            capability: "operate".into(),
        };
        let catalogs = BTreeMap::from([("consumer".into(), &consumer), ("source".into(), &source)]);
        let candidates =
            project_input_source_candidates(&catalogs, std::slice::from_ref(&business), &|_| true)
                .unwrap();
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.provider.catalog == "source"));
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
    fn prerequisite_identity_reference_requires_complete_local_target() {
        let mut cgs = fixture();
        cgs.bind_registry_entry_id("matrix");
        cgs.capabilities.get_mut("read").unwrap().kind = crate::CapabilityKind::Get;
        let mut reference = cgs.values["text"].clone();
        reference.field_type = FieldType::EntityRef {
            entry_id: cgs.entry_id.clone().unwrap().into(),
            target: "BusinessRecord".into(),
        };
        cgs.values.insert("record_ref".into(), reference);
        cgs.prerequisites
            .contracts
            .get_mut("scoped_value")
            .unwrap()
            .inputs
            .insert("account".into(), "record_ref".into());
        let input = cgs
            .capabilities
            .get_mut("acquire")
            .unwrap()
            .inputs
            .arguments
            .as_mut()
            .unwrap();
        if let crate::schema::InputType::Object { fields, .. } = &mut input.input_type {
            fields[0].wire = crate::schema::InputFieldWire::Registry(
                crate::ValueDomainKey::new("record_ref").unwrap(),
            );
        } else {
            panic!("fixture has an object input");
        }
        cgs.prerequisites.requirements.get_mut("read").unwrap()[0]
            .arguments
            .insert(
                "account".into(),
                ArgumentSource::ConsumerIdentity { field: "id".into() },
            );
        cgs.prerequisites.validate(&cgs).unwrap();

        cgs.values.get_mut("record_ref").unwrap().field_type = FieldType::EntityRef {
            entry_id: "foreign".into(),
            target: "BusinessRecord".into(),
        };
        assert!(cgs.prerequisites.validate(&cgs).is_err());
        cgs.values.get_mut("record_ref").unwrap().field_type = FieldType::EntityRef {
            entry_id: cgs.entry_id.clone().unwrap().into(),
            target: "ProviderResult".into(),
        };
        assert!(cgs.prerequisites.validate(&cgs).is_err());
        cgs.values.get_mut("record_ref").unwrap().field_type = FieldType::EntityRef {
            entry_id: cgs.entry_id.clone().unwrap().into(),
            target: "BusinessRecord".into(),
        };
        cgs.entities.get_mut("BusinessRecord").unwrap().key_vars =
            vec!["id".into(), "scope".into()];
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
            guidance.contains(".query(...)"),
            "query consumers use taught e#.query(…) not e# / m#:\n{guidance}"
        );
        assert!(
            !guidance.contains(" / "),
            "prerequisite seats must not use e# / m#:\n{guidance}"
        );
        assert!(!guidance.contains("context="));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn declared_closure_is_total_for_valid_identity_shapes(
            shape in 0u8..4,
            relation_count in 0usize..16,
            reverse in proptest::bool::ANY,
        ) {
            let mut unrelated = fixture();
            unrelated.prerequisites = PrerequisiteCatalog::default();
            let mut entity = unrelated.entities["BusinessRecord"].clone();
            entity.name = "Derived".into();
            let mut field = entity.fields["id"].clone();
            field.name = "value".into();
            entity.fields.insert("value".into(), field);
            match shape {
                0 => {},
                1 => { entity.fields.shift_remove("id"); entity.id_from = Some(vec!["nested".into(), "value".into()]); },
                2 => { entity.fields.shift_remove("id"); entity.implicit_request_identity = true; },
                _ => { entity.key_vars = vec!["id".into(), "value".into()]; },
            }
            for index in 0..relation_count {
                let name = format!("related_{index}");
                entity.relations.insert(name.clone().into(), crate::RelationSchema {
                    name: name.into(), description: "Structural relation".into(),
                    target_resource: "Derived".into(), cardinality: crate::Cardinality::Many,
                    materialize: Some(crate::RelationMaterialization::QueryScoped { capability: "derived_read".into(), param: "credential".into() }), discovery: None,
                });
            }
            unrelated.entities.insert("Derived".into(), entity);
            let mut read = unrelated.capabilities["read"].clone();
            read.name = "derived_read".into();
            read.domain = "Derived".into();
            read.provides = vec!["value".into()];
            read.inputs.scope.0 = std::mem::take(&mut read.inputs.selection.0);
            read.output_schema.as_mut().unwrap().output_type = crate::schema::OutputType::Collection { entity_type: "Derived".into(), max_count: None };
            unrelated.capabilities.insert("derived_read".into(), read);
            let mut create = unrelated.capabilities["create"].clone();
            create.name = "derived_create".into();
            create.domain = "Derived".into();
            create.provides = vec!["value".into()];
            create.output_schema.as_mut().unwrap().output_type = crate::schema::OutputType::Entity { entity_type: "Derived".into() };
            unrelated.capabilities.insert("derived_create".into(), create);
            unrelated.bind_registry_entry_id("unrelated");
            let unrelated = unrelated.fresh_catalog_digest();
            unrelated.validate().unwrap();
            let decoded: CGS = serde_json::from_slice(&serde_json::to_vec(&unrelated).unwrap()).unwrap();
            decoded.validate().unwrap();
            let mut original = fixture();
            original.prerequisites = PrerequisiteCatalog::default();
            let refs: BTreeMap<String, &CGS> = BTreeMap::from([("matrix".into(), &original), ("unrelated".into(), &decoded)]);
            let mut business: Vec<_> = refs.iter().flat_map(|(catalog, cgs)| cgs.capabilities.keys().map(|cap|
                CapabilityRef { catalog: catalog.clone(), capability: cap.to_string() })).collect();
            if reverse { business.reverse(); }
            let closure = prerequisite_closure(&refs, &DeploymentBindings::default(), &business,
                &BTreeSet::from(["matrix".into(), "unrelated".into()])).unwrap();
            proptest::prop_assert!(closure.prerequisites.is_empty());
            proptest::prop_assert!(closure.input_sources.is_empty());
            proptest::prop_assert_eq!(closure.business.into_iter().collect::<BTreeSet<_>>(), business.into_iter().collect());
            // Unrelated graph size and identity representation do not affect a scoped request.
            let root = CapabilityRef { catalog: "matrix".into(), capability: "operate".into() };
            let alone = prerequisite_closure(&BTreeMap::from([("matrix".into(), &original)]),
                &DeploymentBindings::default(), std::slice::from_ref(&root), &BTreeSet::from(["matrix".into()])).unwrap();
            let together = prerequisite_closure(&refs, &DeploymentBindings::default(), &[root],
                &BTreeSet::from(["matrix".into(), "unrelated".into()])).unwrap();
            proptest::prop_assert_eq!(alone, together);
        }
    }

    #[test]
    fn identity_compatibility_does_not_create_prerequisites() {
        let cgs = fixture();
        let business = CapabilityRef {
            catalog: "matrix".into(),
            capability: "operate".into(),
        };
        let closure = prerequisite_closure(
            &BTreeMap::from([("matrix".into(), &cgs)]),
            &DeploymentBindings::default(),
            std::slice::from_ref(&business),
            &BTreeSet::from(["matrix".into()]),
        )
        .unwrap();
        assert_eq!(closure.business, vec![business]);
        assert!(closure.input_sources.is_empty());
        assert!(closure.prerequisites.is_empty());
        assert!(closure.acquisitions.is_empty());
    }

    #[test]
    fn selected_input_sources_are_exact_and_respect_capability_policy() {
        let cgs = fixture();
        let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
        let business = CapabilityRef {
            catalog: "matrix".into(),
            capability: "operate".into(),
        };
        let empty = prerequisite_closure_with_selected_sources(
            &catalogs,
            &DeploymentBindings {
                bindings: vec![DeploymentBinding {
                    consumer: CapabilityRef {
                        catalog: "matrix".into(),
                        capability: "read".into(),
                    },
                    requirement: "scoped_access".into(),
                    provider_catalog: "matrix".into(),
                    provider: "value_source".into(),
                }],
            },
            std::slice::from_ref(&business),
            &[],
            &BTreeSet::from(["matrix".into()]),
            |reference| reference.capability == "read",
        )
        .unwrap();
        assert!(empty.input_sources.is_empty());

        let read = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let closure = prerequisite_closure_with_selected_sources(
            &catalogs,
            &DeploymentBindings {
                bindings: vec![DeploymentBinding {
                    consumer: read.clone(),
                    requirement: "scoped_access".into(),
                    provider_catalog: "matrix".into(),
                    provider: "value_source".into(),
                }],
            },
            &[business],
            std::slice::from_ref(&read),
            &BTreeSet::from(["matrix".into()]),
            |reference| reference.capability == "read",
        )
        .unwrap();
        assert_eq!(closure.input_sources, vec![read]);
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
            guidance.contains(&format!("{get_sym}.get(...)")),
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
            guidance.contains(&format!("{get_sym}.get(\"account-a\")")),
            "constant Get identity must fill e#(\"…\"), got:\n{guidance}"
        );
        assert!(
            !guidance.contains(&format!("{get_sym}.get(...)")),
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
    fn provider_may_have_collection_output() {
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
        cgs.prerequisites.validate(&cgs).unwrap();
    }

    #[test]
    fn collection_provider_teaches_row_selection_before_field_binding() {
        let mut cgs = fixture();
        cgs.capabilities
            .get_mut("acquire")
            .unwrap()
            .output_schema
            .as_mut()
            .unwrap()
            .output_type = crate::schema::OutputType::Collection {
            entity_type: "ProviderResult".into(),
            max_count: None,
        };
        let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
        let business = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let closure = prerequisite_closure(
            &catalogs,
            &DeploymentBindings {
                bindings: vec![DeploymentBinding {
                    consumer: business,
                    requirement: "scoped_access".into(),
                    provider_catalog: "matrix".into(),
                    provider: "value_source".into(),
                }],
            },
            &[CapabilityRef {
                catalog: "matrix".into(),
                capability: "read".into(),
            }],
            &BTreeSet::from(["matrix".into()]),
        )
        .unwrap();
        let exposure = crate::TeachingExposureSession::new(
            &cgs,
            "matrix",
            &["BusinessRecord", "ProviderResult"],
        );
        let guidance = crate::prompt_render::render_prerequisite_bindings(
            &closure,
            &catalogs,
            exposure.to_symbol_map().as_ref(),
        )
        .unwrap();
        assert!(guidance.contains("select or fan out a row"), "{guidance}");
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
