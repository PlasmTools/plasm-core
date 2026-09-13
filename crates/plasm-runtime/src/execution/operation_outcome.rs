//! HTTP-2 operation ledger: semantic acknowledgments on [`super::ExecutionResult`].
//!
//! Normative law: repository `docs/execution-result-contract.md` and HTTP-2
//! in `docs/plasm-language-surface-invariants.md`.

use super::ExecutionSource;
use plasm_core::{CapabilitySchema, OutputType};
use serde::{Deserialize, Serialize};

/// Stable merge key: catalog entry plus capability wire name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OperationIdentity {
    pub entry_id: String,
    pub capability: String,
}

/// One capability's invocation accounting for a step.
///
/// `description` is presentation only and is never a merge key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationAck {
    pub entry_id: String,
    pub capability: String,
    pub entity: String,
    pub logical_invocations: usize,
    pub completed: usize,
    pub failed: usize,
    pub source: ExecutionSource,
    pub description: String,
}

impl OperationAck {
    pub fn identity(&self) -> OperationIdentity {
        OperationIdentity {
            entry_id: self.entry_id.clone(),
            capability: self.capability.clone(),
        }
    }

    /// CGS side-effect / capability prose. Empty `provides` may still acknowledge a write.
    pub fn description_for_capability(capability: &CapabilitySchema) -> String {
        if let Some(os) = &capability.output_schema {
            if let OutputType::SideEffect { description } = &os.output_type {
                let trimmed = description.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        let trimmed = capability.description.trim();
        if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            "operation (no entity projection required)".into()
        }
    }

    pub fn from_capability(
        entry_id: impl Into<String>,
        entity: impl Into<String>,
        capability: &CapabilitySchema,
        source: ExecutionSource,
        completed: usize,
        failed: usize,
    ) -> Self {
        let logical_invocations = completed.saturating_add(failed);
        Self {
            entry_id: entry_id.into(),
            capability: capability.name.as_str().to_string(),
            entity: entity.into(),
            logical_invocations,
            completed,
            failed,
            source,
            description: Self::description_for_capability(capability),
        }
    }

    /// Create / delete / invoke only. Reads do not mint acknowledgments.
    pub fn try_from_mutating_expr(
        expr: &plasm_core::Expr,
        cgs: Option<&plasm_core::CGS>,
        source: ExecutionSource,
        completed: usize,
        failed: usize,
    ) -> Option<Self> {
        let (entry_id, entity, capability_name) = mutating_expr_identity(expr)?;
        if let Some(cgs) = cgs {
            if let Some(capability) = cgs.get_capability(capability_name.as_str()) {
                return Some(Self::from_capability(
                    entry_id, entity, capability, source, completed, failed,
                ));
            }
        }
        let logical_invocations = completed.saturating_add(failed);
        Some(Self {
            entry_id,
            capability: capability_name,
            entity,
            logical_invocations,
            completed,
            failed,
            source,
            description: String::new(),
        })
    }

    pub fn empty_iteration(
        entry_id: impl Into<String>,
        entity: impl Into<String>,
        capability: impl Into<String>,
        description: impl Into<String>,
        source: ExecutionSource,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            capability: capability.into(),
            entity: entity.into(),
            logical_invocations: 0,
            completed: 0,
            failed: 0,
            source,
            description: description.into(),
        }
    }
}

/// Per-step operation outcomes. Empty ledger = no acknowledged operations (pure read).
/// Wire JSON is a bare array (HTTP-2), not `{ "entries": [...] }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationLedger {
    entries: Vec<OperationAck>,
}

impl OperationLedger {
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[OperationAck] {
        &self.entries
    }

    pub fn from_ack(ack: OperationAck) -> Self {
        let mut ledger = Self::empty();
        ledger.merge_ack(ack);
        ledger
    }

    pub fn merge_ack(&mut self, ack: OperationAck) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.entry_id == ack.entry_id && e.capability == ack.capability)
        {
            existing.logical_invocations = existing
                .logical_invocations
                .saturating_add(ack.logical_invocations);
            existing.completed = existing.completed.saturating_add(ack.completed);
            existing.failed = existing.failed.saturating_add(ack.failed);
            existing.source = merge_operation_source(existing.source, ack.source);
            if existing.description.trim().is_empty() && !ack.description.trim().is_empty() {
                existing.description = ack.description;
            }
        } else {
            self.entries.push(ack);
        }
    }

    pub fn merge(&mut self, other: &Self) {
        for ack in &other.entries {
            self.merge_ack(ack.clone());
        }
    }
}

fn mutating_expr_identity(expr: &plasm_core::Expr) -> Option<(String, String, String)> {
    match expr {
        plasm_core::Expr::Create(c) => Some((
            c.catalog_entry_id.as_deref().unwrap_or("").to_string(),
            c.entity.as_str().to_string(),
            c.capability.as_str().to_string(),
        )),
        plasm_core::Expr::Delete(d) => Some((
            d.catalog_entry_id.as_deref().unwrap_or("").to_string(),
            d.target.entity_type.as_str().to_string(),
            d.capability.as_str().to_string(),
        )),
        plasm_core::Expr::Invoke(i) => Some((
            i.catalog_entry_id.as_deref().unwrap_or("").to_string(),
            i.target.entity_type.as_str().to_string(),
            i.capability.as_str().to_string(),
        )),
        _ => None,
    }
}

fn merge_operation_source(a: ExecutionSource, b: ExecutionSource) -> ExecutionSource {
    use ExecutionSource::*;
    match (a, b) {
        (Live, _) | (_, Live) => Live,
        (Replay, _) | (_, Replay) => Replay,
        (Cache, Cache) => Cache,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ack(cap: &str, completed: usize, failed: usize) -> OperationAck {
        OperationAck {
            entry_id: "langmatrix".into(),
            capability: cap.into(),
            entity: "LangItem".into(),
            logical_invocations: completed.saturating_add(failed),
            completed,
            failed,
            source: ExecutionSource::Live,
            description: format!("desc-{cap}"),
        }
    }

    #[test]
    fn merge_preserves_distinct_capability_identity() {
        let mut ledger = OperationLedger::empty();
        ledger.merge_ack(ack("langitem_delete", 2, 0));
        ledger.merge_ack(ack("langitem_ping", 1, 0));
        ledger.merge_ack(ack("langitem_delete", 0, 1));
        assert_eq!(ledger.entries().len(), 2);
        let delete = ledger
            .entries()
            .iter()
            .find(|e| e.capability == "langitem_delete")
            .expect("delete");
        assert_eq!(delete.logical_invocations, 3);
        assert_eq!(delete.completed, 2);
        assert_eq!(delete.failed, 1);
        assert_eq!(delete.description, "desc-langitem_delete");
        let ping = ledger
            .entries()
            .iter()
            .find(|e| e.capability == "langitem_ping")
            .expect("ping");
        assert_eq!(ping.completed, 1);
        assert_eq!(ping.failed, 0);
    }

    #[test]
    fn merge_does_not_use_description_as_key() {
        let mut ledger = OperationLedger::empty();
        ledger.merge_ack(OperationAck {
            description: "first prose".into(),
            ..ack("langitem_ping", 1, 0)
        });
        ledger.merge_ack(OperationAck {
            description: "second prose".into(),
            ..ack("langitem_ping", 1, 0)
        });
        assert_eq!(ledger.entries().len(), 1);
        assert_eq!(ledger.entries()[0].completed, 2);
        assert_eq!(ledger.entries()[0].description, "first prose");
    }

    #[test]
    fn live_source_wins_over_replay() {
        let mut ledger = OperationLedger::from_ack(OperationAck {
            source: ExecutionSource::Replay,
            ..ack("langitem_ping", 1, 0)
        });
        ledger.merge_ack(ack("langitem_ping", 1, 0));
        assert_eq!(ledger.entries()[0].source, ExecutionSource::Live);
    }
}
