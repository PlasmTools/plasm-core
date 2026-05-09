//! Transport-layer dispatch: template parsing and operation compilation.
//!
//! `CapabilityTemplate` and `CompiledOperation` are the two central sum types
//! that route execution to the correct transport backend.  HTTP is always
//! available; EVM variants are compiled in only when the `evm` feature is
//! enabled, in which case all EVM-specific types re-exported here come from
//! the gated `evm_transport` sub-module.

use crate::cml::{
    compile_request, path_var_names_from_request, CmlCond, CmlEnv, CmlExpr, CmlRequest,
    CompiledRequest, PaginationConfig,
};
use crate::error::CmlError;
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

#[cfg(feature = "evm")]
use crate::evm_transport;
#[cfg(feature = "evm")]
pub use crate::evm_transport::*;

// ---------------------------------------------------------------------------
// Central sum types
// ---------------------------------------------------------------------------

/// Parsed CML mapping template (HTTP or EVM). For YAML/schema loading, use [`parse_capability_template`].
///
/// **`serde` / CBOR**: uses Rust enum encoding on the wire (not the JSON object shape from `domain.yaml`).
/// Declarative composed read (`views:` on the CGS) — executed by the runtime, not HTTP CML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewTemplate {
    /// Stable view id matching the CGS `views:` map key.
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewCompiled {
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CapabilityTemplate {
    Http(CmlRequest),
    /// GraphQL over HTTP: same [`CmlRequest`] shape as HTTP (typically `POST` + JSON body with `query` / `variables`).
    /// Distinguished for fingerprints, replay, and future GraphQL-specific pagination.
    GraphQl(CmlRequest),
    /// Server-side composition over other capabilities (see CGS `views:`).
    View(ViewTemplate),
    #[cfg(feature = "evm")]
    EvmCall(EvmCallTemplate),
    #[cfg(feature = "evm")]
    EvmLogs(EvmLogsTemplate),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CompiledOperation {
    Http(CompiledRequest),
    /// Compiled GraphQL request (same wire payload as HTTP POST JSON today).
    GraphQl(CompiledRequest),
    View(ViewCompiled),
    #[cfg(feature = "evm")]
    EvmCall(CompiledEvmCall),
    #[cfg(feature = "evm")]
    EvmLogs(Box<CompiledEvmLogs>),
}

// ---------------------------------------------------------------------------
// Public dispatch functions
// ---------------------------------------------------------------------------

pub fn parse_capability_template(
    template: &serde_json::Value,
) -> Result<CapabilityTemplate, CmlError> {
    let transport = template
        .get("transport")
        .and_then(|v| v.as_str())
        .unwrap_or("http");

    match transport {
        "http" => serde_json::from_value::<CmlRequest>(template.clone())
            .map(CapabilityTemplate::Http)
            .map_err(|e| CmlError::InvalidTemplate {
                message: format!("invalid HTTP template: {e}"),
            }),
        "graphql" => serde_json::from_value::<CmlRequest>(template.clone())
            .map(CapabilityTemplate::GraphQl)
            .map_err(|e| CmlError::InvalidTemplate {
                message: format!("invalid GraphQL template: {e}"),
            }),
        "view" => serde_json::from_value::<ViewTemplate>(template.clone())
            .map(CapabilityTemplate::View)
            .map_err(|e| CmlError::InvalidTemplate {
                message: format!("invalid view template: {e}"),
            }),
        #[cfg(feature = "evm")]
        "evm_call" => serde_json::from_value::<EvmCallTemplate>(template.clone())
            .map(CapabilityTemplate::EvmCall)
            .map_err(|e| CmlError::InvalidTemplate {
                message: format!("invalid evm_call template: {e}"),
            }),
        #[cfg(feature = "evm")]
        "evm_logs" => serde_json::from_value::<EvmLogsTemplate>(template.clone())
            .map(CapabilityTemplate::EvmLogs)
            .map_err(|e| CmlError::InvalidTemplate {
                message: format!("invalid evm_logs template: {e}"),
            }),
        other => Err(CmlError::InvalidTemplate {
            message: format!("unsupported transport '{other}'"),
        }),
    }
}

pub fn compile_operation(
    template: &CapabilityTemplate,
    env: &CmlEnv,
) -> Result<CompiledOperation, CmlError> {
    match template {
        CapabilityTemplate::Http(req) => compile_request(req, env).map(CompiledOperation::Http),
        CapabilityTemplate::GraphQl(req) => {
            compile_request(req, env).map(CompiledOperation::GraphQl)
        }
        CapabilityTemplate::View(v) => {
            let _ = env;
            Ok(CompiledOperation::View(ViewCompiled {
                view: v.view.clone(),
            }))
        }
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmCall(req) => {
            evm_transport::compile_evm_call(req, env).map(CompiledOperation::EvmCall)
        }
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmLogs(req) => evm_transport::compile_evm_logs(req, env)
            .map(|l| CompiledOperation::EvmLogs(Box::new(l))),
    }
}

pub fn template_pagination(template: &CapabilityTemplate) -> Option<&PaginationConfig> {
    match template {
        CapabilityTemplate::Http(req) | CapabilityTemplate::GraphQl(req) => req.pagination.as_ref(),
        CapabilityTemplate::View(_) => None,
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmCall(_) => None,
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmLogs(req) => req.pagination.as_ref(),
    }
}

pub fn template_var_names(template: &CapabilityTemplate) -> Vec<String> {
    let mut vars = IndexSet::new();
    match template {
        CapabilityTemplate::View(_) => {}
        CapabilityTemplate::Http(req) | CapabilityTemplate::GraphQl(req) => {
            for name in path_var_names_from_request(req) {
                vars.insert(name);
            }
            if let Some(expr) = &req.query {
                collect_expr_vars(expr, &mut vars);
            }
            if let Some(expr) = &req.body {
                collect_expr_vars(expr, &mut vars);
            }
            if let Some(expr) = &req.headers {
                collect_expr_vars(expr, &mut vars);
            }
            if let Some(mp) = &req.multipart {
                for p in &mp.parts {
                    collect_expr_vars(&p.content, &mut vars);
                }
            }
        }
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmCall(req) => {
            collect_expr_vars(&req.contract, &mut vars);
            for arg in &req.args {
                collect_expr_vars(arg, &mut vars);
            }
            if let Some(block) = &req.block {
                collect_expr_vars(block, &mut vars);
            }
        }
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmLogs(req) => {
            collect_expr_vars(&req.contract, &mut vars);
            for topic in &req.topics {
                collect_expr_vars(topic, &mut vars);
            }
        }
    }
    vars.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Shared CML expression helpers (used by both HTTP and EVM arms above)
// ---------------------------------------------------------------------------

pub(crate) fn collect_expr_vars(expr: &CmlExpr, vars: &mut IndexSet<String>) {
    match expr {
        CmlExpr::Var { name } => {
            vars.insert(name.clone());
        }
        CmlExpr::Const { .. } => {}
        CmlExpr::Object { fields } => {
            for (_, expr) in fields {
                collect_expr_vars(expr, vars);
            }
        }
        CmlExpr::Array { elements } => {
            for expr in elements {
                collect_expr_vars(expr, vars);
            }
        }
        CmlExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_cond_vars(condition, vars);
            collect_expr_vars(then_expr, vars);
            collect_expr_vars(else_expr, vars);
        }
        CmlExpr::Join { expr, .. } => collect_expr_vars(expr, vars),
        CmlExpr::Format { vars: fmt_vars, .. } => {
            for expr in fmt_vars.values() {
                collect_expr_vars(expr, vars);
            }
        }
        CmlExpr::GmailRfc5322SendBody {} => {
            for key in [
                "from",
                "to",
                "subject",
                "plainBody",
                "threadId",
                "inReplyTo",
                "references",
            ] {
                vars.insert(key.to_string());
            }
        }
        CmlExpr::GmailRfc5322ReplySendBody {} => {
            for key in [
                "from",
                "plainBody",
                "to",
                "subject",
                "parent_threadId",
                "parent_headerFrom",
                "parent_headerReplyTo",
                "parent_headerSubject",
                "parent_headerMessageId",
                "parent_headerReferences",
                "parent_headerInReplyTo",
            ] {
                vars.insert(key.to_string());
            }
        }
    }
}

pub(crate) fn collect_cond_vars(cond: &CmlCond, vars: &mut IndexSet<String>) {
    match cond {
        CmlCond::Exists { var } => {
            vars.insert(var.clone());
        }
        CmlCond::Equals { left, right } => {
            collect_expr_vars(left, vars);
            collect_expr_vars(right, vars);
        }
        CmlCond::Bool { expr } => collect_expr_vars(expr, vars),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod capability_template_round_trip_tests {
    use super::*;

    #[test]
    fn http_template_parse_matches_cbor_round_trip() {
        let v = serde_json::json!({
            "method": "GET",
            "path": [{"type": "literal", "value": "x"}]
        });
        let t = parse_capability_template(&v).unwrap();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&t, &mut bytes).unwrap();
        let t2: CapabilityTemplate = ciborium::de::from_reader(&bytes[..]).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn graphql_template_parse_matches_cbor_round_trip() {
        let v = serde_json::json!({
            "transport": "graphql",
            "method": "POST",
            "path": [{"type": "literal", "value": "api"}],
            "body": {
                "type": "object",
                "fields": [["query", {"type": "const", "value": "{ ping }"}]]
            }
        });
        let t = parse_capability_template(&v).unwrap();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&t, &mut bytes).unwrap();
        let t2: CapabilityTemplate = ciborium::de::from_reader(&bytes[..]).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn graphql_template_pagination_body_merge_path_round_trip() {
        let v = serde_json::json!({
            "transport": "graphql",
            "method": "POST",
            "path": [{"type": "literal", "value": "api"}],
            "body": {
                "type": "object",
                "fields": [
                    ["query", {"type": "const", "value": "{ x }"}],
                    ["variables", {"type": "object", "fields": [["o", {"type": "object", "fields": []}]]}]
                ]
            },
            "pagination": {
                "location": "body",
                "body_merge_path": ["variables", "o", "paginate"],
                "params": {
                    "page": {"counter": 1},
                    "limit": {"fixed": 20}
                }
            },
            "response": {"items_path": ["data", "posts", "data"]}
        });
        let t = parse_capability_template(&v).unwrap();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&t, &mut bytes).unwrap();
        let t2: CapabilityTemplate = ciborium::de::from_reader(&bytes[..]).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn graphql_template_pagination_response_prefix_round_trip() {
        let v = serde_json::json!({
            "transport": "graphql",
            "method": "POST",
            "path": [{"type": "literal", "value": "graphql"}],
            "body": {
                "type": "object",
                "fields": [
                    ["query", {"type": "const", "value": "{ x }"}],
                    ["variables", {"type": "object", "fields": []}]
                ]
            },
            "pagination": {
                "location": "body",
                "body_merge_path": ["variables"],
                "response_prefix": ["data", "issues", "pageInfo"],
                "params": {
                    "first": {"fixed": 50},
                    "after": {"from_response": "endCursor"}
                },
                "stop_when": {"field": "hasNextPage", "eq": false}
            },
            "response": {"items_path": ["data", "issues", "nodes"]}
        });
        let t = parse_capability_template(&v).unwrap();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&t, &mut bytes).unwrap();
        let t2: CapabilityTemplate = ciborium::de::from_reader(&bytes[..]).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn http_template_multipart_cbor_round_trip() {
        let v = serde_json::json!({
            "method": "POST",
            "path": [{"type": "literal", "value": "pet"}, {"type": "literal", "value": "upload"}],
            "body_format": "multipart",
            "multipart": {
                "parts": [
                    {"name": "file", "file_name": "x.png", "content": {"type": "var", "name": "file"}}
                ]
            }
        });
        let t = parse_capability_template(&v).unwrap();
        assert!(
            template_var_names(&t).contains(&"file".to_string()),
            "multipart part expressions contribute template vars"
        );
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&t, &mut bytes).unwrap();
        let t2: CapabilityTemplate = ciborium::de::from_reader(&bytes[..]).unwrap();
        assert_eq!(t, t2);
    }
}

#[cfg(test)]
#[cfg(feature = "evm")]
mod tests {
    use super::*;

    #[test]
    fn evm_call_template_var_names_include_non_id_inputs() {
        let template = parse_capability_template(&serde_json::json!({
            "transport": "evm_call",
            "chain": 1,
            "contract": { "type": "var", "name": "contract" },
            "function": "function balanceOf(address owner) view returns (uint256)",
            "args": [{ "type": "var", "name": "owner" }],
            "block": { "type": "var", "name": "block" }
        }))
        .unwrap();

        assert_eq!(
            template_var_names(&template),
            vec![
                "contract".to_string(),
                "owner".to_string(),
                "block".to_string()
            ]
        );
    }
}
