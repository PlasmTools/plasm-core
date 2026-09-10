//! Validate expression declarations before catalog publication, including dead branches.

use crate::{CapabilityTemplate, CmlCond, CmlError, CmlExpr, PathSegment, RequestAuthentication};
use indexmap::IndexSet;

fn invalid(message: &str) -> CmlError {
    CmlError::InvalidTemplate {
        message: message.into(),
    }
}

fn condition(condition: &CmlCond) -> Result<(), CmlError> {
    match condition {
        CmlCond::Exists { .. } => Ok(()),
        CmlCond::Equals { left, right } => {
            expression(left)?;
            expression(right)
        }
        CmlCond::Bool { expr } => expression(expr),
    }
}

fn expression(expr: &CmlExpr) -> Result<(), CmlError> {
    match expr {
        CmlExpr::Var { .. } | CmlExpr::Const { .. } => Ok(()),
        CmlExpr::Trim { value } | CmlExpr::Base64 { value, .. } => expression(value),
        CmlExpr::Field { value, path } => {
            if path.is_empty() || path.iter().any(String::is_empty) {
                return Err(invalid("field projection requires nonempty field names"));
            }
            expression(value)
        }
        CmlExpr::UrlProject { value, projection } => {
            projection.validate()?;
            expression(value)
        }
        CmlExpr::Assert {
            condition: predicate,
            code,
            value,
        } => {
            if code.is_empty() || !code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                return Err(invalid("assertion code must be a nonempty identifier"));
            }
            condition(predicate)?;
            expression(value)
        }
        CmlExpr::FirstPresent { values } => {
            if values.is_empty() {
                return Err(invalid("first_present requires at least one expression"));
            }
            values.iter().try_for_each(expression)
        }
        CmlExpr::Let { bindings, value } => {
            let mut remaining = IndexSet::new();
            for binding in bindings {
                if binding.name.is_empty() || !remaining.insert(binding.name.clone()) {
                    return Err(invalid(
                        "local expression names must be nonempty and unique",
                    ));
                }
            }
            for binding in bindings {
                expression(&binding.value)?;
                let mut dependencies = IndexSet::new();
                crate::transport::collect_expr_vars(&binding.value, &mut dependencies);
                if dependencies.iter().any(|name| remaining.contains(name)) {
                    return Err(invalid(
                        "local expressions cannot reference themselves or later declarations",
                    ));
                }
                remaining.shift_remove(&binding.name);
            }
            expression(value)
        }
        CmlExpr::If {
            condition: predicate,
            then_expr,
            else_expr,
        } => {
            condition(predicate)?;
            expression(then_expr)?;
            expression(else_expr)
        }
        CmlExpr::Object { fields } => fields.iter().try_for_each(|(_, value)| expression(value)),
        CmlExpr::Array { elements } => elements.iter().try_for_each(expression),
        CmlExpr::Join { expr, .. } => expression(expr),
        CmlExpr::Format { template, vars } => {
            template.validate_vars(vars)?;
            vars.values().try_for_each(expression)
        }
        CmlExpr::MailMessage { headers, text } => {
            expression(headers)?;
            expression(text)
        }
        CmlExpr::MailReplyHeaders { parent, overrides } => {
            expression(parent)?;
            expression(overrides)
        }
    }
}

fn paths(path: &[PathSegment]) -> Result<(), CmlError> {
    for part in path {
        if let PathSegment::If {
            condition: predicate,
            then_expr,
            else_expr,
        } = part
        {
            condition(predicate)?;
            expression(then_expr)?;
            expression(else_expr)?;
        }
    }
    Ok(())
}

fn authentication(auth: &RequestAuthentication) -> Result<(), CmlError> {
    match auth {
        RequestAuthentication::Host => Ok(()),
        RequestAuthentication::Bearer { token } => expression(token),
        RequestAuthentication::Credential {
            slot,
            resource,
            reference,
        } => {
            crate::credential::validate_slot(slot)?;
            expression(resource)?;
            expression(reference)
        }
        RequestAuthentication::When {
            condition: predicate,
            then_auth,
            else_auth,
        } => {
            condition(predicate)?;
            authentication(then_auth)?;
            authentication(else_auth)
        }
    }
}

pub(crate) fn validate_template(template: &CapabilityTemplate) -> Result<(), CmlError> {
    match template {
        CapabilityTemplate::CredentialBind(binding) => {
            binding.validate()?;
            expression(&binding.resource)
        }
        CapabilityTemplate::Http(request) | CapabilityTemplate::GraphQl(request) => {
            paths(&request.path)?;
            for expr in [&request.body, &request.query, &request.headers]
                .into_iter()
                .flatten()
            {
                expression(expr)?;
            }
            if let Some(auth) = &request.auth {
                authentication(auth)?;
            }
            if let Some(multipart) = &request.multipart {
                for part in &multipart.parts {
                    expression(&part.content)?;
                }
            }
            if let Some(aux) = request
                .response
                .as_ref()
                .and_then(|r| r.auxiliary_merge.as_ref())
            {
                paths(&aux.path)?;
                if let Some(auth) = &aux.auth {
                    authentication(auth)?;
                }
                for expr in [&aux.query, &aux.headers].into_iter().flatten() {
                    expression(expr)?;
                }
            }
            Ok(())
        }
        CapabilityTemplate::View(_) => Ok(()),
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmCall(request) => {
            expression(&request.contract)?;
            for argument in &request.args {
                expression(argument)?;
            }
            if let Some(block) = &request.block {
                expression(block)?;
            }
            Ok(())
        }
        #[cfg(feature = "evm")]
        CapabilityTemplate::EvmLogs(request) => {
            expression(&request.contract)?;
            request.topics.iter().try_for_each(expression)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{compile_operation, parse_capability_template, CapabilityTemplate};
    use serde_json::json;

    #[test]
    fn invalid_declarations_in_dead_branches_fail_at_parse() {
        for invalid in [
            json!({"type":"url_project","value":{"type":"const","value":null},"projection":{"origins":[],"path":[]}}),
            json!({"type":"assert","code":"not a code","condition":{"type":"exists","var":"x"},"value":{"type":"const","value":null}}),
            json!({"type":"let","bindings":[{"name":"self","value":{"type":"var","name":"self"}}],"value":{"type":"const","value":null}}),
            json!({"type":"first_present","values":[]}),
        ] {
            let template = json!({"method":"POST","path":[],"body":{"type":"if","condition":{"type":"bool","expr":{"type":"const","value":true}},"then_expr":{"type":"const","value":"ok"},"else_expr":invalid}});
            assert!(parse_capability_template(&template).is_err());
        }
    }

    #[test]
    fn ordered_locals_survive_packed_json_and_cbor() {
        let wire = json!({"method":"POST","path":[],"body":{"type":"let","bindings":[{"name":"z_first","value":{"type":"var","name":"input"}},{"name":"a_second","value":{"type":"var","name":"z_first"}}],"value":{"type":"var","name":"a_second"}}});
        let template =
            parse_capability_template(&serde_json::from_str(&wire.to_string()).unwrap()).unwrap();
        let mut bytes = Vec::new();
        ciborium::into_writer(&template, &mut bytes).unwrap();
        let packed: CapabilityTemplate = ciborium::from_reader(bytes.as_slice()).unwrap();
        let env = serde_json::from_value(json!({"input":"result"})).unwrap();
        assert_eq!(
            compile_operation(&template, &env).unwrap(),
            compile_operation(&packed, &env).unwrap()
        );
        assert_eq!(crate::template_var_names(&packed), ["input"]);
    }
}
