//! Declarative **preflight** steps on mutating capabilities (create / update / action / delete).
//!
//! Runtime orchestration lives in `plasm-runtime::preflight`.

use crate::schema::{CapabilityKind, CapabilitySchema, CGS};
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

#[derive(Debug, Clone, thiserror::Error)]
pub enum PreflightValidationError {
    #[error("idempotent output requires a read-backed reconciliation contract")]
    ReconciliationMissing,
    #[error("reconciliation read capability does not exist")]
    ReconciliationCapabilityMissing,
    #[error("reconciliation must use an effect-free read")]
    ReconciliationEffectful,
    #[error("reconciliation requires a non-empty identity_key")]
    ReconciliationIdentityMissing,
    #[error("reconciliation read entity does not exist")]
    ReconciliationEntityMissing,
    #[error("reconciliation read must expose identity field {field}")]
    ReconciliationFieldMissing { field: String },
    #[error("{step} hydrate_invoke_target: prefix must not be empty")]
    HydrationPrefixEmpty { step: String },
    #[error("{step} hydrate_invoke_target is not allowed on kind create")]
    HydrationOnCreate { step: String },
    #[error("{step} {kind}: merge must not be empty")]
    MergeEmpty {
        step: String,
        kind: PreflightMergeKind,
    },
    #[error("{step} query_pick: scope must not be empty")]
    QueryScopeEmpty { step: String },
    #[error("{step} label_ids_delta: from_preflight.prefix must not be empty")]
    LabelPrefixEmpty { step: String },
    #[error("{step} existence_check requires capability identity_key")]
    ExistenceIdentityMissing { step: String },
    #[error("{step} references unknown capability '{capability}'")]
    CapabilityMissing { step: String, capability: String },
    #[error("{step} capability '{capability}' must be {expected}")]
    CapabilityKind {
        step: String,
        capability: String,
        expected: PreflightReadKind,
    },
    #[error("{step} is only allowed on create/update/delete/action")]
    MutatingCapabilityRequired { step: String },
    #[error("{step} references unknown param '{param}'")]
    ParamMissing { step: String, param: String },
    #[error("{step} get '{capability}' is for entity {actual}, expected {expected}")]
    GetDomainMismatch {
        step: String,
        capability: String,
        actual: String,
        expected: String,
    },
    #[error("{step} get '{capability}' is for entity {actual}, param '{param}' is entity_ref to {expected}")]
    ParamDomainMismatch {
        step: String,
        capability: String,
        actual: String,
        param: String,
        expected: String,
    },
    #[error("{step} param '{param}' references unknown value domain '{value_ref}'")]
    ParamValueMissing {
        step: String,
        param: String,
        value_ref: String,
    },
    #[error("{step} param '{param}' must be entity_ref")]
    ParamNotEntityRef { step: String, param: String },
    #[error("{step} merge key '{wire_key}' must not use reserved prefix 'plasm_execute_'")]
    ReservedWireKey { step: String, wire_key: String },
    #[error("duplicate preflight merge wire key '{wire_key}' ({step})")]
    DuplicateWireKey { step: String, wire_key: String },
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum PreflightMergeKind {
    #[error("hydrate_entity_ref_param")]
    HydrateEntityRefParam,
    #[error("query_pick")]
    QueryPick,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum PreflightReadKind {
    #[error("kind get")]
    Get,
    #[error("kind query or search")]
    QueryOrSearch,
    #[error("query, search, or get")]
    AnyRead,
}

fn preflight_err(cap: &CapabilitySchema, source: PreflightValidationError) -> SchemaError {
    SchemaError::PreflightInvalid {
        capability: cap.name.to_string(),
        source,
    }
}

/// Validate preflight plan for a capability at CGS load time.
pub fn validate_capability_preflight(cgs: &CGS, cap: &CapabilitySchema) -> Result<(), SchemaError> {
    if let Some(output) = cap
        .output_schema
        .as_ref()
        .filter(|output| output.idempotent)
    {
        let reconcile = output
            .reconcile
            .as_ref()
            .ok_or_else(|| preflight_err(cap, PreflightValidationError::ReconciliationMissing))?;
        let lookup = cgs.get_capability(&reconcile.via).ok_or_else(|| {
            preflight_err(
                cap,
                PreflightValidationError::ReconciliationCapabilityMissing,
            )
        })?;
        if !matches!(
            lookup.kind,
            CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
        ) {
            return Err(preflight_err(
                cap,
                PreflightValidationError::ReconciliationEffectful,
            ));
        }
        let keys = cap
            .identity_key
            .as_ref()
            .filter(|keys| !keys.is_empty())
            .ok_or_else(|| {
                preflight_err(cap, PreflightValidationError::ReconciliationIdentityMissing)
            })?;
        let entity = cgs.get_entity(lookup.domain.as_str()).ok_or_else(|| {
            preflight_err(cap, PreflightValidationError::ReconciliationEntityMissing)
        })?;
        for key in keys {
            validate_param_exists(cap, key, "reconciliation identity")?;
            if !entity.fields.contains_key(key.as_str()) {
                return Err(preflight_err(
                    cap,
                    PreflightValidationError::ReconciliationFieldMissing {
                        field: key.to_string(),
                    },
                ));
            }
        }
    }
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
                        PreflightValidationError::HydrationPrefixEmpty {
                            step: step_label.clone(),
                        },
                    ));
                }
                if cap.kind == CapabilityKind::Create {
                    return Err(preflight_err(
                        cap,
                        PreflightValidationError::HydrationOnCreate {
                            step: step_label.clone(),
                        },
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
                        PreflightValidationError::MergeEmpty {
                            step: step_label.clone(),
                            kind: PreflightMergeKind::HydrateEntityRefParam,
                        },
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
                        PreflightValidationError::MergeEmpty {
                            step: step_label.clone(),
                            kind: PreflightMergeKind::QueryPick,
                        },
                    ));
                }
                validate_query_cap(cgs, query, &step_label)?;
                validate_param_exists(cap, &pick.equals_param, &step_label)?;
                if scope.is_empty() {
                    return Err(preflight_err(
                        cap,
                        PreflightValidationError::QueryScopeEmpty {
                            step: step_label.clone(),
                        },
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
                        PreflightValidationError::LabelPrefixEmpty {
                            step: step_label.clone(),
                        },
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
                            PreflightValidationError::ExistenceIdentityMissing {
                                step: step_label.clone(),
                            },
                        )
                    })?;
                for key in keys {
                    validate_param_exists(cap, key, &step_label)?;
                }
                let lookup = cgs.get_capability(query).ok_or_else(|| {
                    preflight_err(
                        cap,
                        PreflightValidationError::CapabilityMissing {
                            step: step_label.clone(),
                            capability: query.clone(),
                        },
                    )
                })?;
                match lookup.kind {
                    CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => {}
                    _ => {
                        return Err(preflight_err(
                            cap,
                            PreflightValidationError::CapabilityKind {
                                step: step_label.clone(),
                                capability: query.clone(),
                                expected: PreflightReadKind::AnyRead,
                            },
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
            PreflightValidationError::MutatingCapabilityRequired {
                step: step_label.to_owned(),
            },
        )),
    }
}

fn validate_param_exists(
    cap: &CapabilitySchema,
    param: &str,
    step_label: &str,
) -> Result<(), SchemaError> {
    if !cap.input_fields().any(|field| field.name == param) {
        return Err(preflight_err(
            cap,
            PreflightValidationError::ParamMissing {
                step: step_label.to_owned(),
                param: param.to_owned(),
            },
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
            PreflightValidationError::CapabilityMissing {
                step: step_label.to_owned(),
                capability: get_name.to_owned(),
            },
        )
    })?;
    if get_cap.kind != CapabilityKind::Get {
        return Err(preflight_err(
            cap,
            PreflightValidationError::CapabilityKind {
                step: step_label.to_owned(),
                capability: get_name.to_owned(),
                expected: PreflightReadKind::Get,
            },
        ));
    }
    if get_cap.domain != cap.domain {
        return Err(preflight_err(
            cap,
            PreflightValidationError::GetDomainMismatch {
                step: step_label.to_owned(),
                capability: get_name.to_owned(),
                actual: get_cap.domain.to_string(),
                expected: cap.domain.to_string(),
            },
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
            PreflightValidationError::CapabilityMissing {
                step: step_label.to_owned(),
                capability: get_name.to_owned(),
            },
        )
    })?;
    if get_cap.kind != CapabilityKind::Get {
        return Err(preflight_err(
            cap,
            PreflightValidationError::CapabilityKind {
                step: step_label.to_owned(),
                capability: get_name.to_owned(),
                expected: PreflightReadKind::Get,
            },
        ));
    }
    let target = param_entity_ref_target(cgs, cap, param, step_label)?;
    if get_cap.domain.as_str() != target.as_str() {
        return Err(preflight_err(
            cap,
            PreflightValidationError::ParamDomainMismatch {
                step: step_label.to_owned(),
                capability: get_name.to_owned(),
                actual: get_cap.domain.to_string(),
                param: param.to_owned(),
                expected: target,
            },
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
    let field = cap
        .input_fields()
        .find(|field| field.name == param)
        .ok_or_else(|| {
            preflight_err(
                cap,
                PreflightValidationError::ParamMissing {
                    step: step_label.to_owned(),
                    param: param.to_owned(),
                },
            )
        })?;
    let crate::schema::InputFieldWire::Registry(value_ref) = &field.wire else {
        return Err(preflight_err(
            cap,
            PreflightValidationError::ParamNotEntityRef {
                step: step_label.to_owned(),
                param: param.to_owned(),
            },
        ));
    };
    let nv = cgs.values.get(value_ref.as_str()).ok_or_else(|| {
        preflight_err(
            cap,
            PreflightValidationError::ParamValueMissing {
                step: step_label.to_owned(),
                param: param.to_owned(),
                value_ref: value_ref.to_string(),
            },
        )
    })?;
    match &nv.field_type {
        FieldType::EntityRef { target, .. } => Ok(target.to_string()),
        _ => Err(preflight_err(
            cap,
            PreflightValidationError::ParamNotEntityRef {
                step: step_label.to_owned(),
                param: param.to_owned(),
            },
        )),
    }
}

fn validate_query_cap(cgs: &CGS, query_name: &str, step_label: &str) -> Result<(), SchemaError> {
    let q = cgs
        .get_capability(query_name)
        .ok_or_else(|| SchemaError::PreflightInvalid {
            capability: query_name.to_string(),
            source: PreflightValidationError::CapabilityMissing {
                step: step_label.to_owned(),
                capability: query_name.to_owned(),
            },
        })?;
    match q.kind {
        CapabilityKind::Query | CapabilityKind::Search => Ok(()),
        _ => Err(SchemaError::PreflightInvalid {
            capability: query_name.to_string(),
            source: PreflightValidationError::CapabilityKind {
                step: step_label.to_owned(),
                capability: query_name.to_owned(),
                expected: PreflightReadKind::QueryOrSearch,
            },
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
            PreflightValidationError::ReservedWireKey {
                step: step_label.to_owned(),
                wire_key: wire_key.to_owned(),
            },
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
            PreflightValidationError::DuplicateWireKey {
                step: step_label.to_owned(),
                wire_key: wire_key.to_owned(),
            },
        ));
    }
    Ok(())
}
