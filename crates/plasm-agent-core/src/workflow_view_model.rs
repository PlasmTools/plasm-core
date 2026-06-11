//! WorkflowViewModel — manifest + tool-model field types for MCP App parameter form.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::workflow_manifest::{WorkflowManifest, WorkflowParameter, WorkflowSeed};
use crate::workflow_program_template::{
    instantiate_template, InstantiateError, SymExposureMap, WorkflowProgramTemplate,
};
use crate::workflow_readiness::assess_workflow_readiness;
use crate::workflow_registry::WorkflowRegistry;
use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::ExposedEntitySymbolRow;
use plasm_core::Value;

pub const WORKFLOW_VIEW_MODEL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowFieldView {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub wire_type: String,
    pub bind_kind: String,
    pub entry_id: String,
    pub entity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowViewModel {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub description: String,
    pub seeds: Vec<WorkflowSeed>,
    pub fields: Vec<WorkflowFieldView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking_errors: Vec<String>,
}

pub fn build_workflow_view_model(manifest: &WorkflowManifest) -> WorkflowViewModel {
    build_workflow_view_model_with_readiness(manifest, None)
}

pub fn build_workflow_view_model_with_readiness(
    manifest: &WorkflowManifest,
    readiness: Option<(
        &InMemoryCgsRegistry,
        Option<&crate::mcp_runtime_config::McpRuntimeConfig>,
    )>,
) -> WorkflowViewModel {
    let fields = manifest
        .parameters
        .iter()
        .map(parameter_to_field_view)
        .collect();
    let assessed = readiness.map(|(reg, tenant)| assess_workflow_readiness(manifest, reg, tenant));
    let (ready, blocking_errors) = assessed
        .as_ref()
        .map(|r| (r.ready, r.blocking_errors.clone()))
        .unwrap_or((true, vec![]));
    let mut warnings = Vec::new();
    if !ready {
        warnings.extend(blocking_errors.clone());
    }
    WorkflowViewModel {
        schema_version: WORKFLOW_VIEW_MODEL_SCHEMA_VERSION,
        id: manifest.id.clone(),
        title: manifest.title.clone(),
        description: manifest.description.clone(),
        seeds: manifest.seeds.clone(),
        fields,
        warnings,
        ready,
        blocking_errors,
    }
}

fn parameter_to_field_view(p: &WorkflowParameter) -> WorkflowFieldView {
    WorkflowFieldView {
        name: p.name.clone(),
        description: p.description.clone(),
        required: p.required,
        wire_type: wire_type_for_bind(&p.bind),
        bind_kind: format!("{:?}", p.bind.kind).to_ascii_lowercase(),
        entry_id: p.bind.entry_id.clone(),
        entity: p.bind.entity.clone(),
    }
}

fn wire_type_for_bind(bind: &crate::workflow_manifest::WorkflowSemanticBind) -> String {
    if bind.param.as_deref() == Some("limit") {
        return "integer".into();
    }
    match bind.kind {
        crate::workflow_manifest::WorkflowBindKind::TemplateString => "text".into(),
        crate::workflow_manifest::WorkflowBindKind::EntityRef => "entity_ref".into(),
        _ => "string".into(),
    }
}

/// Build sym exposure from live execute session entity symbols (preferred for MCP App).
pub fn sym_exposure_from_entity_symbols(
    rows: &[ExposedEntitySymbolRow],
) -> BTreeMap<(String, String), String> {
    rows.iter()
        .map(|r| ((r.entry_id.clone(), r.entity.clone()), r.symbol.clone()))
        .collect()
}

pub fn instantiate_workflow_program(
    _manifest: &WorkflowManifest,
    template: &WorkflowProgramTemplate,
    params_json: &BTreeMap<String, serde_json::Value>,
    exposure: &SymExposureMap<'_>,
) -> Result<String, InstantiateError> {
    let params = json_params_to_values(params_json);
    instantiate_template(template, &params, exposure)
}

fn json_params_to_values(raw: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, Value> {
    raw.iter()
        .map(|(k, v)| (k.clone(), json_to_value(v)))
        .collect()
}

fn json_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::String(v.to_string()),
    }
}

pub fn lookup_view_model(registry: &WorkflowRegistry, id: &str) -> Option<WorkflowViewModel> {
    registry.get(id).map(|m| build_workflow_view_model(&m))
}

pub fn lookup_view_model_with_readiness(
    workflows: &WorkflowRegistry,
    id: &str,
    catalog: &InMemoryCgsRegistry,
    tenant_cfg: Option<&crate::mcp_runtime_config::McpRuntimeConfig>,
) -> Option<WorkflowViewModel> {
    workflows
        .get(id)
        .map(|m| build_workflow_view_model_with_readiness(&m, Some((catalog, tenant_cfg))))
}

/// Map entity exposure order to `e1`, `e2`, … for sym holes (display-only in emitted program).
pub fn default_sym_exposure(manifest: &WorkflowManifest) -> BTreeMap<(String, String), String> {
    manifest
        .seeds
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                (s.entry_id.clone(), s.entity.clone()),
                format!("e{}", i + 1),
            )
        })
        .collect()
}

pub fn sym_exposure_refs<'a>(owned: &'a BTreeMap<(String, String), String>) -> SymExposureMap<'a> {
    owned.iter().map(|(k, v)| (k.clone(), v.as_str())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_registry::workflow_matrix_manifest;

    #[test]
    fn view_model_from_manifest() {
        let m = workflow_matrix_manifest();
        let vm = build_workflow_view_model(&m);
        assert_eq!(vm.fields.len(), 1);
        assert_eq!(vm.seeds.len(), 2);
    }

    #[test]
    fn instantiate_workflow_matrix_program() {
        let m = workflow_matrix_manifest();
        let tpl = m.parsed_template().expect("parse");
        let mut params = BTreeMap::new();
        params.insert("limit".into(), serde_json::json!(5));
        let exposure_map = default_sym_exposure(&m);
        let refs = sym_exposure_refs(&exposure_map);
        let prog = instantiate_workflow_program(&m, &tpl, &params, &refs).expect("inst");
        assert!(prog.contains("e1"));
        assert!(prog.contains("5"));
    }
}
