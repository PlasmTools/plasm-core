//! Typed local credential effects and explicit transport references.

use crate::{eval_cml, CmlEnv, CmlError, CmlExpr};
use plasm_core::Value;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, thiserror::Error)]
pub enum CredentialBindError {
    #[error("credential slot must be a nonempty identifier")]
    InvalidSlot,
    #[error("credential lifetime must be positive")]
    ZeroLifetime,
    #[error("credential resource identity must be an object")]
    ResourceNotObject,
    #[error("credential resource identity must contain at least one field")]
    EmptyResource,
    #[error("credential resource field {field} must have a nonempty name and string value")]
    InvalidResourceField { field: String },
    #[error("credential resource identity could not be serialized")]
    ResourceSerialization(#[source] std::sync::Arc<serde_json::Error>),
    #[error("credential origin is not a valid URL")]
    OriginParse(#[source] url::ParseError),
    #[error("credential origin must use HTTP or HTTPS")]
    OriginScheme,
    #[error("credential origin cannot contain user information")]
    OriginUserInfo,
    #[error("credential origin cannot contain a query")]
    OriginQuery,
    #[error("credential origin cannot contain a fragment")]
    OriginFragment,
    #[error("credential origin must not contain a path")]
    OriginPath,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialBindTemplate {
    pub slot: String,
    pub lifetime_seconds: u64,
    pub origin: String,
    /// Explicit resource key (object of typed identity fields).
    pub resource: CmlExpr,
    pub source: CredentialSource,
}

/// A catalog declaration selects the existing host injection source, never a secret value or key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialSource {
    Host {},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledCredentialBind {
    pub slot: String,
    pub lifetime_seconds: u64,
    pub origin: String,
    pub resource: serde_json::Value,
    pub source: CredentialSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledCredentialUse {
    pub slot: String,
    pub resource: serde_json::Value,
    pub reference: String,
}

pub(crate) fn validate_slot(slot: &str) -> Result<(), CredentialBindError> {
    if slot.is_empty()
        || !slot
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(CredentialBindError::InvalidSlot);
    }
    Ok(())
}

pub(crate) fn resource_key(resource: Value) -> Result<serde_json::Value, CredentialBindError> {
    let Value::Object(fields) = resource else {
        return Err(CredentialBindError::ResourceNotObject);
    };
    if fields.is_empty() {
        return Err(CredentialBindError::EmptyResource);
    }
    for (name, value) in &fields {
        if name.is_empty() || !matches!(value, Value::String(s) if !s.is_empty()) {
            return Err(CredentialBindError::InvalidResourceField {
                field: name.clone(),
            });
        }
    }
    serde_json::to_value(fields)
        .map_err(|error| CredentialBindError::ResourceSerialization(std::sync::Arc::new(error)))
}

fn credential_origin(origin: &str) -> Result<Url, CredentialBindError> {
    let url = Url::parse(origin).map_err(CredentialBindError::OriginParse)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(CredentialBindError::OriginScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CredentialBindError::OriginUserInfo);
    }
    if url.query().is_some() {
        return Err(CredentialBindError::OriginQuery);
    }
    if url.fragment().is_some() {
        return Err(CredentialBindError::OriginFragment);
    }
    if url.path() != "/" {
        return Err(CredentialBindError::OriginPath);
    }
    Ok(url)
}

impl CredentialBindTemplate {
    pub fn validate(&self) -> Result<(), CredentialBindError> {
        validate_slot(&self.slot)?;
        if self.lifetime_seconds == 0 {
            return Err(CredentialBindError::ZeroLifetime);
        }
        credential_origin(&self.origin)?;
        Ok(())
    }

    pub fn compile(&self, env: &CmlEnv) -> Result<CompiledCredentialBind, CmlError> {
        self.validate()?;
        let resource = resource_key(eval_cml(&self.resource, env)?)?;
        let origin = credential_origin(&self.origin)?
            .origin()
            .ascii_serialization();
        Ok(CompiledCredentialBind {
            slot: self.slot.clone(),
            lifetime_seconds: self.lifetime_seconds,
            origin,
            resource,
            source: self.source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn binding_compiles_only_an_injection_reference() {
        let wire = json!({"slot":"reader","lifetime_seconds":3600,"origin":"https://example.test","resource":{"type":"object","fields":[["record",{"type":"var","name":"key"}]]},"source":{"scheme":"host"}});
        let template: CredentialBindTemplate = serde_json::from_value(wire.clone()).unwrap();
        let env = serde_json::from_value(json!({"key":"a"})).unwrap();
        let compiled = template.compile(&env).unwrap();
        assert_eq!(compiled.source, CredentialSource::Host {});
        assert_eq!(compiled.resource, json!({"record":"a"}));
        let serialized = serde_json::to_value(&compiled).unwrap();
        assert!(serialized.get("secret").is_none());
        assert_eq!(
            serde_json::from_value::<CompiledCredentialBind>(serialized).unwrap(),
            compiled
        );
        let mut literal = wire;
        literal["secret"] = json!({"type":"const","value":"forbidden-literal"});
        assert!(serde_json::from_value::<CredentialBindTemplate>(literal).is_err());
        assert!(serde_json::from_value::<CredentialSource>(
            json!({"scheme":"host", "key":"arbitrary-tenant-key"})
        )
        .is_err());
    }
}
