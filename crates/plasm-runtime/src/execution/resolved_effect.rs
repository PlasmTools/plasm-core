//! One resolved call owns both dispatch operands and their public provenance.
//! Values stay private; public references never serialize receiver keys or arguments.
use super::*;
use serde::{Deserialize, Serialize};
use std::future::Future;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectInputReference {
    pub binding: String,
    pub producer: String,
    pub collection: plasm_core::collection_codec::CollectionIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectSelectionReference {
    pub collection: plasm_core::collection_codec::CollectionIdentity,
    pub index: usize,
}

/// Immediate plan-input references, not field-level provenance through Python.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectOrigin {
    #[serde(with = "uuid_wire")]
    pub execution: uuid::Uuid,
    pub scope_path: Vec<String>,
    pub occurrence_path: Vec<usize>,
    pub inputs: Vec<EffectInputReference>,
    pub selection: Option<EffectSelectionReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectTargetKind {
    Create,
    Receiver,
    Capability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedEffectEvidence {
    /// Fresh for each resolved invocation, even for identical request bytes.
    #[serde(with = "uuid_wire")]
    pub invocation: uuid::Uuid,
    pub operation: OperationIdentity,
    pub entity: String,
    pub target_kind: EffectTargetKind,
    /// Direct runtime expression calls have no enclosing validated plan.
    pub origin: Option<EffectOrigin>,
}

tokio::task_local! {
    static ORIGIN: EffectOrigin;
    static SELECTION: EffectSelectionReference;
    static EFFECT: ResolvedEffectEvidence;
}

// Keep the runtime's wire contract independent of UUID features unified by hosts.
mod uuid_wire {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &uuid::Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<uuid::Uuid, D::Error> {
        let value = String::deserialize(deserializer)?;
        uuid::Uuid::parse_str(&value).map_err(serde::de::Error::custom)
    }
}

impl EffectOrigin {
    pub async fn scope<T>(self, future: impl Future<Output = T>) -> T {
        ORIGIN.scope(self, future).await
    }
}
impl EffectSelectionReference {
    pub fn current() -> Option<Self> {
        SELECTION.try_with(Clone::clone).ok()
    }
    pub async fn scope<T>(self, future: impl Future<Output = T>) -> T {
        SELECTION.scope(self, future).await
    }
}
pub(crate) fn current_effect() -> Option<ResolvedEffectEvidence> {
    EFFECT.try_with(Clone::clone).ok()
}

/// Closed resolved operand forms. Construction is runtime-internal after admission.
/// No Debug/Serialize implementation: operands can contain credentials.
pub(super) enum ResolvedEffect {
    Create(plasm_core::CreateExpr),
    Delete(plasm_core::DeleteExpr),
    Invoke(plasm_core::InvokeExpr),
}

impl ResolvedEffect {
    pub(super) async fn execute(
        self,
        engine: &ExecutionEngine,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let (entry, capability, entity, target_kind) = match &self {
            Self::Create(call) => (
                call.catalog_entry_id.as_deref(),
                call.capability.as_str(),
                call.entity.as_str(),
                EffectTargetKind::Create,
            ),
            Self::Delete(call) => (
                call.catalog_entry_id.as_deref(),
                call.capability.as_str(),
                call.target.entity_type.as_str(),
                EffectTargetKind::Receiver,
            ),
            Self::Invoke(call) => (
                call.catalog_entry_id.as_deref(),
                call.capability.as_str(),
                call.target.entity_type.as_str(),
                if cgs
                    .get_capability(call.capability.as_str())
                    .ok_or_else(|| RuntimeError::CapabilityNotFound {
                        capability: call.capability.to_string(),
                        entity: call.target.entity_type.to_string(),
                    })?
                    .requires_receiver()
                {
                    EffectTargetKind::Receiver
                } else {
                    EffectTargetKind::Capability
                },
            ),
        };
        let evidence = ResolvedEffectEvidence {
            invocation: uuid::Uuid::new_v4(),
            operation: OperationIdentity {
                entry_id: entry.unwrap_or("").into(),
                capability: capability.into(),
            },
            entity: entity.into(),
            target_kind,
            origin: ORIGIN.try_with(Clone::clone).ok(),
        };
        EFFECT
            .scope(evidence, async {
                match self {
                    Self::Create(call) => engine.execute_create(&call, cgs, mat, mode).await,
                    Self::Delete(call) => engine.execute_delete(&call, cgs, mat, mode).await,
                    Self::Invoke(call) => engine.execute_invoke(&call, cgs, mat, mode).await,
                }
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_effect_wire_preserves_identity_and_rejects_raw_operands() {
        let evidence = ResolvedEffectEvidence {
            invocation: uuid::Uuid::new_v4(),
            operation: OperationIdentity {
                entry_id: "matrix".into(),
                capability: "write".into(),
            },
            entity: "Item".into(),
            target_kind: EffectTargetKind::Receiver,
            origin: Some(EffectOrigin {
                execution: uuid::Uuid::new_v4(),
                scope_path: vec!["write".into()],
                occurrence_path: vec![],
                inputs: vec![],
                selection: None,
            }),
        };
        let mut wire = serde_json::to_value(&evidence).unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedEffectEvidence>(wire.clone()).unwrap(),
            evidence
        );
        wire["arguments"] = serde_json::json!({"password": "private"});
        assert!(serde_json::from_value::<ResolvedEffectEvidence>(wire).is_err());
        let mut wire = serde_json::to_value(&evidence).unwrap();
        wire["invocation"] = serde_json::json!("not-an-invocation");
        assert!(serde_json::from_value::<ResolvedEffectEvidence>(wire).is_err());
    }
}
