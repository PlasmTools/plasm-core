//! Registry + tenant MCP policy checks for workflow manifests.

use plasm_core::discovery::{CgsCatalog, InMemoryCgsRegistry};

use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::workflow_manifest::WorkflowManifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowReadiness {
    pub ready: bool,
    pub blocking_errors: Vec<String>,
}

pub fn workflow_catalog_entry_ids(manifest: &WorkflowManifest) -> Vec<String> {
    if !manifest.catalog_pins.is_empty() {
        return manifest.catalog_pins.clone();
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for seed in &manifest.seeds {
        if seen.insert(seed.entry_id.clone()) {
            out.push(seed.entry_id.clone());
        }
    }
    out
}

pub fn assess_workflow_readiness(
    manifest: &WorkflowManifest,
    registry: &InMemoryCgsRegistry,
    tenant_cfg: Option<&McpRuntimeConfig>,
) -> WorkflowReadiness {
    let mut blocking_errors = Vec::new();
    for entry_id in workflow_catalog_entry_ids(manifest) {
        if registry.load_context(entry_id.as_str()).is_err() {
            blocking_errors.push(format!("registry missing catalog entry `{entry_id}`"));
        }
        if let Some(cfg) = tenant_cfg {
            if !cfg.entry_allowed(entry_id.as_str()) {
                blocking_errors.push(format!(
                    "catalog `{entry_id}` not allowed by tenant MCP policy"
                ));
            }
        }
    }
    WorkflowReadiness {
        ready: blocking_errors.is_empty(),
        blocking_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_registry::workflow_matrix_manifest;
    use plasm_core::discovery::InMemoryCgsRegistry;
    use std::sync::Arc;

    #[test]
    fn readiness_fails_when_registry_missing_pins() {
        let m = workflow_matrix_manifest();
        let reg = InMemoryCgsRegistry::from_pairs(vec![]);
        let r = assess_workflow_readiness(&m, &reg, None);
        assert!(!r.ready);
        assert!(r.blocking_errors.iter().any(|e| e.contains("catalog_a")));
    }
}
