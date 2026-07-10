//! Declarative **preflight** steps on mutating capabilities (create / update / action / delete).
//!
//! Runtime orchestration lives in `plasm-runtime::preflight`.

use crate::schema::{CapabilityKind, CapabilitySchema, InputSchema, InputType, CGS};
use crate::FieldType;
use crate::SchemaError;
use crate::Value;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Ordered steps run before CML compile on mutating capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct PreflightPlan(pub Vec<PreflightStep>);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreflightStep {
    /// GET `invoke.target` (skipped on create).
    HydrateInvokeTarget { get: String, prefix: String },
    /// When `param` is present: GET entity_ref, merge wire keys from decoded row fields.
    HydrateEntityRefParam {
        param: String,
        get: String,
        merge: IndexMap<String, String>,
    },
    /// Scoped query/search, exact pick, merge wire ids.
    QueryPick {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<String>,
        query: String,
        scope: IndexMap<String, ScopeBind>,
        pick: PickSpec,
        merge: IndexMap<String, String>,
    },
    /// Resolve add/remove label names → merged `labelIds`.
    LabelIdsDelta {
        add_when: String,
        remove_when: String,
        lookup: String,
        from_preflight: PreflightFieldPath,
        #[serde(default = "default_label_ids_merge_key")]
        merge: String,
    },
    /// Existence probe on identity_key params before write compile.
    ExistenceCheck {
        query: String,
        #[serde(default)]
        identity_from: ExistenceIdentityFrom,
        on_exists: ExistenceOnExists,
    },
}

/// Env key set by [`PreflightStep::ExistenceCheck`] when `on_exists: skip_write` finds a row.
pub const PLASM_EXISTENCE_SKIP_WRITE_ENV: &str = "__plasm_existence_skip_write";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExistenceIdentityFrom {
    #[default]
    Params,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistenceOnExists {
    Fail,
    SkipWrite,
}

fn default_label_ids_merge_key() -> String {
    "labelIds".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickSpec {
    pub field: String,
    pub equals_param: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ScopeBind {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_preflight: Option<PreflightFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub literal: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightFieldPath {
    pub prefix: String,
    pub path: Vec<String>,
}

const RESERVED_PREFIX: &str = "plasm_execute_";

fn preflight_err(cap: &CapabilitySchema, message: String) -> SchemaError {
    SchemaError::PreflightInvalid {
        capability: cap.name.to_string(),
        message,
    }
}

fn input_field<'a>(
    schema: &'a InputSchema,
    param: &str,
) -> Option<&'a crate::schema::InputFieldSchema> {
    match &schema.input_type {
        InputType::Object { fields, .. } => fields.iter().find(|f| f.name == param),
        _ => None,
    }
}

/// Validate preflight plan for a capability at CGS load time.
pub fn validate_capability_preflight(cgs: &CGS, cap: &CapabilitySchema) -> Result<(), SchemaError> {
    let Some(PreflightPlan(steps)) = cap.preflight.as_ref() else {
        return Ok(());
    };
    if steps.is_empty() {
        return Ok(());
    }

    let mut merged_wire_keys: indexmap::IndexSet<String> = indexmap::IndexSet::new();

    for (idx, step) in steps.iter().enumerate() {
        let step_label = format!("preflight[{idx}]");
        match step {
            PreflightStep::HydrateInvokeTarget { get, prefix } => {
                if prefix.trim().is_empty() {
                    return Err(preflight_err(
                        cap,
                        format!("{step_label} hydrate_invoke_target: prefix must not be empty"),
                    ));
                }
                if cap.kind == CapabilityKind::Create {
                    return Err(preflight_err(
                        cap,
                        format!("{step_label} hydrate_invoke_target is not allowed on kind create"),
                    ));
                }
                validate_get_on_domain(cgs, cap, get, &step_label)?;
            }
            PreflightStep::HydrateEntityRefParam { param, get, merge } => {
                require_mutating_kind(cap, &step_label)?;
                validate_param_exists(cap, param, &step_label)?;
                if merge.is_empty() {
                    return Err(preflight_err(
                        cap,
                        format!("{step_label} hydrate_entity_ref_param: merge must not be empty"),
                    ));
                }
                validate_get_for_param_entity(cgs, cap, param, get, &step_label)?;
                for wire_key in merge.keys() {
                    reject_reserved_wire_key(cap, wire_key, &step_label)?;
                    reject_duplicate_wire_key(cap, wire_key, &mut merged_wire_keys, &step_label)?;
                }
            }
            PreflightStep::QueryPick {
                when,
                query,
                scope,
                pick,
                merge,
            } => {
                require_mutating_kind(cap, &step_label)?;
                if let Some(w) = when {
                    validate_param_exists(cap, w, &step_label)?;
                }
                if merge.is_empty() {
                    return Err(preflight_err(
                        cap,
                        format!("{step_label} query_pick: merge must not be empty"),
                    ));
                }
                validate_query_cap(cgs, query, &step_label)?;
                validate_param_exists(cap, &pick.equals_param, &step_label)?;
                if scope.is_empty() {
                    return Err(preflight_err(
                        cap,
                        format!("{step_label} query_pick: scope must not be empty"),
                    ));
                }
                for wire_key in merge.keys() {
                    reject_reserved_wire_key(cap, wire_key, &step_label)?;
                    reject_duplicate_wire_key(cap, wire_key, &mut merged_wire_keys, &step_label)?;
                }
            }
            PreflightStep::LabelIdsDelta {
                add_when,
                remove_when,
                lookup,
                from_preflight,
                merge,
            } => {
                require_mutating_kind(cap, &step_label)?;
                validate_param_exists(cap, add_when, &step_label)?;
                validate_param_exists(cap, remove_when, &step_label)?;
                if from_preflight.prefix.trim().is_empty() {
                    return Err(preflight_err(
                        cap,
                        format!(
                            "{step_label} label_ids_delta: from_preflight.prefix must not be empty"
                        ),
                    ));
                }
                validate_query_cap(cgs, lookup, &step_label)?;
                reject_reserved_wire_key(cap, merge, &step_label)?;
                reject_duplicate_wire_key(cap, merge, &mut merged_wire_keys, &step_label)?;
            }
            PreflightStep::ExistenceCheck {
                query,
                identity_from: _,
                on_exists: _,
            } => {
                require_mutating_kind(cap, &step_label)?;
                let keys = cap
                    .identity_key
                    .as_ref()
                    .filter(|k| !k.is_empty())
                    .ok_or_else(|| {
                        preflight_err(
                            cap,
                            format!(
                                "{step_label} existence_check requires capability identity_key"
                            ),
                        )
                    })?;
                for key in keys {
                    validate_param_exists(cap, key, &step_label)?;
                }
                let lookup = cgs.get_capability(query).ok_or_else(|| {
                    preflight_err(
                        cap,
                        format!(
                            "{step_label} existence_check references unknown capability '{query}'"
                        ),
                    )
                })?;
                match lookup.kind {
                    CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => {}
                    _ => {
                        return Err(preflight_err(
                            cap,
                            format!(
                                "{step_label} existence_check lookup '{query}' must be query, search, or get"
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn require_mutating_kind(cap: &CapabilitySchema, step_label: &str) -> Result<(), SchemaError> {
    match cap.kind {
        CapabilityKind::Create
        | CapabilityKind::Update
        | CapabilityKind::Delete
        | CapabilityKind::Action => Ok(()),
        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => Err(preflight_err(
            cap,
            format!("{step_label} is only allowed on create/update/delete/action"),
        )),
    }
}

fn validate_param_exists(
    cap: &CapabilitySchema,
    param: &str,
    step_label: &str,
) -> Result<(), SchemaError> {
    let Some(schema) = cap.input_schema.as_ref() else {
        return Err(preflight_err(
            cap,
            format!("{step_label} references param '{param}' but capability has no input_schema"),
        ));
    };
    if input_field(schema, param).is_none() {
        return Err(preflight_err(
            cap,
            format!("{step_label} references unknown param '{param}'"),
        ));
    }
    Ok(())
}

fn validate_get_on_domain(
    cgs: &CGS,
    cap: &CapabilitySchema,
    get_name: &str,
    step_label: &str,
) -> Result<(), SchemaError> {
    let get_cap = cgs.get_capability(get_name).ok_or_else(|| {
        preflight_err(
            cap,
            format!("{step_label} references unknown capability '{get_name}'"),
        )
    })?;
    if get_cap.kind != CapabilityKind::Get {
        return Err(preflight_err(
            cap,
            format!("{step_label} capability '{get_name}' must be kind get"),
        ));
    }
    if get_cap.domain != cap.domain {
        return Err(preflight_err(
            cap,
            format!(
                "{step_label} get '{get_name}' is for entity {}, expected {}",
                get_cap.domain, cap.domain
            ),
        ));
    }
    Ok(())
}

fn validate_get_for_param_entity(
    cgs: &CGS,
    cap: &CapabilitySchema,
    param: &str,
    get_name: &str,
    step_label: &str,
) -> Result<(), SchemaError> {
    let get_cap = cgs.get_capability(get_name).ok_or_else(|| {
        preflight_err(
            cap,
            format!("{step_label} references unknown capability '{get_name}'"),
        )
    })?;
    if get_cap.kind != CapabilityKind::Get {
        return Err(preflight_err(
            cap,
            format!("{step_label} capability '{get_name}' must be kind get"),
        ));
    }
    let target = param_entity_ref_target(cgs, cap, param, step_label)?;
    if get_cap.domain.as_str() != target.as_str() {
        return Err(preflight_err(
            cap,
            format!(
                "{step_label} get '{get_name}' is for entity {}, param '{param}' is entity_ref to {target}",
                get_cap.domain
            ),
        ));
    }
    Ok(())
}

fn param_entity_ref_target(
    cgs: &CGS,
    cap: &CapabilitySchema,
    param: &str,
    step_label: &str,
) -> Result<String, SchemaError> {
    let schema = cap
        .input_schema
        .as_ref()
        .ok_or_else(|| preflight_err(cap, format!("{step_label} missing input_schema")))?;
    let field = input_field(schema, param)
        .ok_or_else(|| preflight_err(cap, format!("{step_label} unknown param '{param}'")))?;
    let nv = field
        .named_value(cgs)
        .map_err(|e| preflight_err(cap, format!("{step_label} param '{param}': {e}")))?;
    match &nv.field_type {
        FieldType::EntityRef { target } => Ok(target.to_string()),
        _ => Err(preflight_err(
            cap,
            format!("{step_label} param '{param}' must be entity_ref"),
        )),
    }
}

fn validate_query_cap(cgs: &CGS, query_name: &str, step_label: &str) -> Result<(), SchemaError> {
    let q = cgs
        .get_capability(query_name)
        .ok_or_else(|| SchemaError::PreflightInvalid {
            capability: query_name.to_string(),
            message: format!("{step_label} references unknown capability '{query_name}'"),
        })?;
    match q.kind {
        CapabilityKind::Query | CapabilityKind::Search => Ok(()),
        _ => Err(SchemaError::PreflightInvalid {
            capability: query_name.to_string(),
            message: format!("{step_label} capability '{query_name}' must be kind query or search"),
        }),
    }
}

fn reject_reserved_wire_key(
    cap: &CapabilitySchema,
    wire_key: &str,
    step_label: &str,
) -> Result<(), SchemaError> {
    if wire_key.starts_with(RESERVED_PREFIX) {
        return Err(preflight_err(
            cap,
            format!(
                "{step_label} merge key '{wire_key}' must not use reserved prefix '{RESERVED_PREFIX}'"
            ),
        ));
    }
    Ok(())
}

fn reject_duplicate_wire_key(
    cap: &CapabilitySchema,
    wire_key: &str,
    seen: &mut indexmap::IndexSet<String>,
    step_label: &str,
) -> Result<(), SchemaError> {
    if !seen.insert(wire_key.to_string()) {
        return Err(preflight_err(
            cap,
            format!("duplicate preflight merge wire key '{wire_key}' ({step_label})"),
        ));
    }
    Ok(())
}
