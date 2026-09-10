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
use crate::PathSegment;
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
    CredentialBind(crate::CredentialBindTemplate),
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
    CredentialBind(crate::CompiledCredentialBind),
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
    let template = &crate::wire_normalize::normalize_wire_cml_template(template.clone());
    let transport = template
        .get("transport")
        .and_then(|v| v.as_str())
        .unwrap_or("http");

    let parsed = match transport {
        "credential_bind" => {
            let mut declaration = template.clone();
            declaration
                .as_object_mut()
                .expect("transport belongs to an object")
                .remove("transport");
            serde_json::from_value::<crate::CredentialBindTemplate>(declaration)
                .map(CapabilityTemplate::CredentialBind)
                .map_err(|_| CmlError::InvalidTemplate {
                    message: "invalid credential binding template".into(),
                })
        }
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
    }?;
    crate::expression_validation::validate_template(&parsed)?;
    Ok(parsed)
}

pub fn compile_operation(
    template: &CapabilityTemplate,
    env: &CmlEnv,
) -> Result<CompiledOperation, CmlError> {
    match template {
        CapabilityTemplate::CredentialBind(binding) => {
            binding.compile(env).map(CompiledOperation::CredentialBind)
        }
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
        CapabilityTemplate::View(_) | CapabilityTemplate::CredentialBind(_) => None,
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
        CapabilityTemplate::CredentialBind(binding) => {
            collect_expr_vars(&binding.resource, &mut vars);
        }
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
            if let Some(auth) = &req.auth {
                collect_auth_vars(auth, &mut vars);
            }
            if let Some(aux) = req
                .response
                .as_ref()
                .and_then(|r| r.auxiliary_merge.as_ref())
            {
                for part in &aux.path {
                    match part {
                        PathSegment::Var { name, .. } => {
                            vars.insert(name.clone());
                        }
                        PathSegment::If {
                            condition,
                            then_expr,
                            else_expr,
                        } => {
                            collect_cond_vars(condition, &mut vars);
                            collect_expr_vars(then_expr, &mut vars);
                            collect_expr_vars(else_expr, &mut vars);
                        }
                        PathSegment::Literal { .. } => {}
                    }
                }
                for expr in [&aux.query, &aux.headers].into_iter().flatten() {
                    collect_expr_vars(expr, &mut vars);
                }
                if let Some(auth) = &aux.auth {
                    collect_auth_vars(auth, &mut vars);
                }
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
        CmlExpr::Trim { value }
        | CmlExpr::Field { value, .. }
        | CmlExpr::UrlProject { value, .. } => collect_expr_vars(value, vars),
        CmlExpr::Assert {
            condition, value, ..
        } => {
            collect_cond_vars(condition, vars);
            collect_expr_vars(value, vars);
        }
        CmlExpr::FirstPresent { values } => {
            for value in values {
                collect_expr_vars(value, vars);
            }
        }
        CmlExpr::Let { bindings, value } => {
            let mut bound = IndexSet::new();
            for crate::LocalBinding {
                name,
                value: expression,
            } in bindings
            {
                let mut dependencies = IndexSet::new();
                collect_expr_vars(expression, &mut dependencies);
                vars.extend(dependencies.into_iter().filter(|key| !bound.contains(key)));
                bound.insert(name.clone());
            }
            let mut dependencies = IndexSet::new();
            collect_expr_vars(value, &mut dependencies);
            vars.extend(dependencies.into_iter().filter(|key| !bound.contains(key)));
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
        CmlExpr::MailMessage { headers, text } => {
            collect_expr_vars(headers, vars);
            collect_expr_vars(text, vars);
        }
        CmlExpr::MailReplyHeaders { parent, overrides } => {
            collect_expr_vars(parent, vars);
            collect_expr_vars(overrides, vars);
        }
        CmlExpr::Base64 { value, .. } => collect_expr_vars(value, vars),
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

pub(crate) fn collect_auth_vars(auth: &crate::RequestAuthentication, vars: &mut IndexSet<String>) {
    match auth {
        crate::RequestAuthentication::Host => {}
        crate::RequestAuthentication::Bearer { token } => collect_expr_vars(token, vars),
        crate::RequestAuthentication::Credential {
            resource,
            reference,
            ..
        } => {
            collect_expr_vars(resource, vars);
            collect_expr_vars(reference, vars);
        }
        crate::RequestAuthentication::When {
            condition,
            then_auth,
            else_auth,
        } => {
            collect_cond_vars(condition, vars);
            collect_auth_vars(then_auth, vars);
            collect_auth_vars(else_auth, vars);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod capability_template_round_trip_tests {
    use super::*;

    #[test]
    fn declared_bearer_compiles_and_tracks_its_input() {
        let wire = serde_json::json!({"method":"GET","path":[],"auth":{"scheme":"bearer","token":{"type":"var","name":"credential"}}});
        let template = parse_capability_template(&wire).unwrap();
        assert_eq!(template_var_names(&template), ["credential"]);
        let env = serde_json::from_value(serde_json::json!({"credential":"abc.def=="})).unwrap();
        let CompiledOperation::Http(request) = compile_operation(&template, &env).unwrap() else {
            panic!("http");
        };
        assert_eq!(
            request.to_json()["headers"]["Authorization"],
            "Bearer abc.def=="
        );
        for token in ["", "private\r\nInjected:x", " leading", "a=b"] {
            let env = serde_json::from_value(serde_json::json!({"credential":token})).unwrap();
            let error = compile_operation(&template, &env).unwrap_err().to_string();
            assert!(!error.contains("private"));
        }
        let mut duplicate = wire;
        duplicate["headers"] = serde_json::json!({"type":"object","fields":[["authorization",{"type":"const","value":"Bearer other"}]]});
        assert!(compile_operation(&parse_capability_template(&duplicate).unwrap(), &env).is_err());
    }

    #[test]
    fn mail_codec_round_trip_and_input_renaming() {
        let template = parse_capability_template(&serde_json::json!({"method":"POST","path":[],"body":{"type":"base64","alphabet":"url_safe_no_pad","value":{"type":"mail_message","headers":{"type":"var","name":"envelope"},"text":{"type":"var","name":"content"}}}})).unwrap();
        assert_eq!(template_var_names(&template), ["envelope", "content"]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&template, &mut bytes).unwrap();
        let restored: CapabilityTemplate = ciborium::from_reader(bytes.as_slice()).unwrap();
        let env = serde_json::from_value(serde_json::json!({"envelope":{"From":"a@example.test","To":"b@example.test","Subject":"Hello"},"content":"Message"})).unwrap();
        assert_eq!(
            compile_operation(&template, &env).unwrap(),
            compile_operation(&restored, &env).unwrap()
        );
        for abolished in ["gmail_rfc5322_send_body", "gmail_rfc5322_reply_send_body"] {
            assert!(parse_capability_template(
                &serde_json::json!({"method":"POST","path":[],"body":{"type":abolished}})
            )
            .is_err());
        }
    }

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

