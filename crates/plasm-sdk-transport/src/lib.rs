//! Trait boundary between plasm and external Rust SDKs.
//!
//! An SDK implements [`SdkTransport`] so that plasm can invoke it as a
//! first-class transport alongside HTTP and EVM.  This crate is kept
//! separate so SDK authors can depend on it without pulling in plasm.
//!
//! # Contract
//!
//! - `capability` is the capability name string from the mapping YAML.
//! - `input` is a `serde_json::Value::Object` whose keys come from the
//!   CML template's variable expressions, evaluated against the query
//!   environment.
//! - The return value must be a `serde_json::Value` that plasm's
//!   schema-driven decoder can traverse into entities.
//!
//! # Stability
//!
//! This trait is **pre-1.0**.  The `serde_json::Value` boundary will be
//! replaced with typed capability inputs/outputs once plasm's capability
//! type system stabilises.  SDK authors should pin to exact versions and
//! expect a migration when typed capabilities land.

use async_trait::async_trait;
use thiserror::Error;

/// JSON shape metadata for input validation, without retaining input contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonInputKind {
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
}

impl From<&serde_json::Value> for JsonInputKind {
    fn from(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(_) => Self::Boolean,
            serde_json::Value::Number(_) => Self::Number,
            serde_json::Value::String(_) => Self::String,
            serde_json::Value::Array(_) => Self::Array,
            serde_json::Value::Object(_) => Self::Object,
        }
    }
}

/// Errors returned by an [`SdkTransport`] implementation.
#[derive(Debug, Error)]
pub enum SdkTransportError {
    /// The SDK does not recognize the requested capability name.
    #[error("unknown capability '{capability}'")]
    UnknownCapability { capability: String },

    /// The SDK requires an object, but received a different JSON shape.
    #[error("invalid input for capability '{capability}': expected JSON object, got {actual:?}")]
    ObjectInputRequired {
        capability: String,
        actual: JsonInputKind,
    },

    /// Input cannot deserialize into the SDK operation's declared input type.
    #[error("invalid JSON input for capability '{capability}': {source}")]
    InputDeserialization {
        capability: String,
        #[source]
        source: serde_json::Error,
    },

    /// The SDK operation failed during execution.
    #[error("SDK execution failed for capability '{capability}': {source}")]
    ExecutionFailed {
        capability: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The SDK is missing required configuration (RPC URLs, API keys, etc.).
    #[error("SDK not configured: missing {setting}")]
    NotConfigured { setting: String },

    /// Catch-all for SDK-internal errors that should preserve their source chain.
    #[error("SDK error for capability '{capability}': {source}")]
    Other {
        capability: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// A Rust SDK that can be driven by plasm as a transport backend.
///
/// Each capability maps to one SDK operation.  The plasm runtime holds an
/// `Arc<dyn SdkTransport>` and dispatches `CompiledOperation::Sdk` through
/// this trait.
///
/// # Stability
///
/// The `serde_json::Value` boundary is intentional for the prototype phase.
/// It will be replaced with typed capability inputs/outputs in a future
/// breaking release.  See the [crate-level docs](crate) for details.
#[async_trait]
pub trait SdkTransport: Send + Sync {
    /// Execute one capability and return a JSON response.
    ///
    /// `input` is typically a [`serde_json::Value::Object`] with keys from the
    /// mapping template.  SDKs that require an object should return
    /// [`SdkTransportError::ObjectInputRequired`] if `input` is not a `Value::Object`.
    /// Typed JSON decoding failures use [`SdkTransportError::InputDeserialization`],
    /// retaining the original [`serde_json::Error`] as their cause.
    async fn execute_capability(
        &self,
        capability: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, SdkTransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Mock SDK that echoes input for known capabilities and rejects unknown ones.
    struct MockSdk;

    #[async_trait]
    impl SdkTransport for MockSdk {
        async fn execute_capability(
            &self,
            capability: &str,
            input: serde_json::Value,
        ) -> Result<serde_json::Value, SdkTransportError> {
            match capability {
                "echo" => Ok(input),
                "fail" => Err(SdkTransportError::ExecutionFailed {
                    capability: capability.into(),
                    source: Box::new(std::io::Error::other("intentional failure")),
                }),
                "validate" => {
                    if !input.is_object() {
                        return Err(SdkTransportError::ObjectInputRequired {
                            capability: capability.into(),
                            actual: JsonInputKind::from(&input),
                        });
                    }
                    Ok(input)
                }
                _ => Err(SdkTransportError::UnknownCapability {
                    capability: capability.into(),
                }),
            }
        }
    }

    #[tokio::test]
    async fn known_capability_returns_response() {
        let sdk = MockSdk;
        let input = json!({"key": "value"});
        let result = sdk.execute_capability("echo", input.clone()).await;
        assert_eq!(result.unwrap(), input);
    }

    #[tokio::test]
    async fn unknown_capability_returns_error() {
        let sdk = MockSdk;
        let result = sdk.execute_capability("nonexistent", json!({})).await;
        let err = result.unwrap_err();
        assert!(matches!(err, SdkTransportError::UnknownCapability { .. }));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn execution_failure_preserves_message() {
        let sdk = MockSdk;
        let err = sdk.execute_capability("fail", json!({})).await.unwrap_err();
        assert!(matches!(err, SdkTransportError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("intentional failure"));
    }

    #[tokio::test]
    async fn invalid_input_on_non_object() {
        let sdk = MockSdk;
        let err = sdk
            .execute_capability("validate", json!([1, 2, 3]))
            .await
            .unwrap_err();
        assert!(matches!(err, SdkTransportError::ObjectInputRequired {
            capability, actual: JsonInputKind::Array,
        } if capability == "validate"));
    }

    #[tokio::test]
    async fn every_non_object_shape_is_rejected_without_input_contents() {
        for (input, expected) in [
            (json!(null), JsonInputKind::Null),
            (json!(false), JsonInputKind::Boolean),
            (json!(42), JsonInputKind::Number),
            (json!("private-input"), JsonInputKind::String),
            (json!([]), JsonInputKind::Array),
        ] {
            let error = MockSdk
                .execute_capability("validate", input)
                .await
                .unwrap_err();
            assert!(
                matches!(&error, SdkTransportError::ObjectInputRequired { capability, actual }
                if capability == "validate" && *actual == expected)
            );
            assert!(!error.to_string().contains("private-input"));
        }
        assert_eq!(JsonInputKind::from(&json!({})), JsonInputKind::Object);
        assert_eq!(
            MockSdk
                .execute_capability("validate", json!({}))
                .await
                .unwrap(),
            json!({})
        );
    }

    #[test]
    fn input_deserialization_preserves_typed_cause() {
        use std::error::Error;
        let source = serde_json::from_value::<u64>(json!([])).unwrap_err();
        let error = SdkTransportError::InputDeserialization {
            capability: "count".into(),
            source,
        };
        assert!(
            matches!(&error, SdkTransportError::InputDeserialization { capability, source }
            if capability == "count" && source.is_data())
        );
        assert!(error
            .source()
            .unwrap()
            .downcast_ref::<serde_json::Error>()
            .unwrap()
            .is_data());
    }

    #[tokio::test]
    async fn not_configured_error() {
        let err = SdkTransportError::NotConfigured {
            setting: "RPC URL".into(),
        };
        assert!(err.to_string().contains("missing RPC URL"));
    }

    #[tokio::test]
    async fn other_variant_preserves_source_chain() {
        let inner = std::io::Error::new(std::io::ErrorKind::TimedOut, "RPC timed out");
        let err = SdkTransportError::Other {
            capability: "burn".into(),
            source: Box::new(inner),
        };
        assert!(err.to_string().contains("RPC timed out"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn trait_is_object_safe() {
        // Proves Arc<dyn SdkTransport> compiles — required by the runtime.
        fn _assert_object_safe(_: std::sync::Arc<dyn SdkTransport>) {}
    }
}
