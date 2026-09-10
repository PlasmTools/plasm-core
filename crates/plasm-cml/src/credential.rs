//! Typed local credential effects and explicit transport references.

use crate::{eval_cml, CmlEnv, CmlError, CmlExpr};
use plasm_core::Value;
use serde::{Deserialize, Serialize};

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

fn invalid(message: &str) -> CmlError {
    CmlError::InvalidTemplate {
        message: message.into(),
    }
}

pub(crate) fn validate_slot(slot: &str) -> Result<(), CmlError> {
    if slot.is_empty()
        || !slot
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(invalid("credential slot must be a nonempty identifier"));
    }
    Ok(())
}

pub(crate) fn resource_key(resource: Value) -> Result<serde_json::Value, CmlError> {
    let Value::Object(fields) = resource else {
        return Err(invalid("credential resource key must be an object"));
    };
    if fields.is_empty()
        || fields.iter().any(|(name, value)| {
            name.is_empty() || !matches!(value, Value::String(s) if !s.is_empty())
        })
    {
        return Err(invalid(
            "credential resource key requires named nonempty string identities",
        ));
    }
    serde_json::to_value(fields).map_err(|_| invalid("invalid credential resource key"))
}

impl CredentialBindTemplate {
    pub fn validate(&self) -> Result<(), CmlError> {
        validate_slot(&self.slot)?;
        if self.lifetime_seconds == 0 {
            return Err(invalid("credential lifetime_seconds must be positive"));
        }
        crate::UrlProjection {
            origins: vec![self.origin.clone()],
            path: vec![],
            query: Default::default(),
        }
        .validate()
    }

    pub fn compile(&self, env: &CmlEnv) -> Result<CompiledCredentialBind, CmlError> {
        self.validate()?;
        let resource = resource_key(eval_cml(&self.resource, env)?)?;
        let origin = url::Url::parse(&self.origin)
            .map_err(|_| invalid("invalid credential origin"))?
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
