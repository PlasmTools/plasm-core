//! Independent reference model: **backend state** vs **Plasm-visible observations**.
//!
//! The model never consults Plasm's session graph. Property checks compare engine
//! outputs against this ledger.

use std::collections::{BTreeMap, BTreeSet};

/// Stable property ids — mirrored in `docs/concurrent-execute-invariants.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum OphProperty {
    /// Response for A cannot populate B / contaminate unrelated rows.
    IdentityIsolation = 1,
    /// Fixed backend snapshot + distinct identities → completion order irrelevant.
    OrderInvariance = 2,
    /// After a multi-write plan fails, fresh reads observe every committed write.
    PartialWriteVisibility = 3,
    /// Stale responses cannot overwrite newer observations across a mutation boundary.
    Freshness = 4,
    /// Complete matches the reference result; missing/failed acquisition ≠ silent Complete.
    CoverageHonesty = 5,
    /// Reported successful effects agree with committed effects; failure ≠ rollback.
    AcknowledgmentHonesty = 6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendExpense {
    pub expense_id: String,
    pub group_id: String,
    pub description: String,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendNote {
    pub note_id: String,
    pub title: String,
    pub body: String,
    pub version: u64,
}

#[derive(Debug, Clone, Default)]
pub struct BackendState {
    pub expenses: BTreeMap<String, BackendExpense>,
    pub notes: BTreeMap<String, BackendNote>,
    pub groups: BTreeMap<String, String>,
    next_expense_seq: u64,
    next_version: u64,
}

impl BackendState {
    pub fn ensure_group(&mut self, group_id: &str, name: &str) {
        self.groups
            .entry(group_id.to_string())
            .or_insert_with(|| name.to_string());
    }

    pub fn commit_expense(&mut self, group_id: &str, description: &str) -> BackendExpense {
        self.next_expense_seq += 1;
        self.next_version += 1;
        let expense = BackendExpense {
            expense_id: format!("e{}", self.next_expense_seq),
            group_id: group_id.to_string(),
            description: description.to_string(),
            version: self.next_version,
        };
        self.expenses
            .insert(expense.expense_id.clone(), expense.clone());
        expense
    }

    pub fn expenses_for_group(&self, group_id: &str) -> BTreeSet<String> {
        self.expenses
            .values()
            .filter(|e| e.group_id == group_id)
            .map(|e| e.expense_id.clone())
            .collect()
    }

    pub fn seed_note(&mut self, note_id: &str, title: &str, body: &str) {
        self.next_version += 1;
        self.notes.insert(
            note_id.to_string(),
            BackendNote {
                note_id: note_id.to_string(),
                title: title.to_string(),
                body: body.to_string(),
                version: self.next_version,
            },
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedWrite {
    pub expense_id: String,
    pub group_id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedEffect {
    pub capability: String,
    pub completed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationCoverage {
    Complete,
    Partial,
    Unknown,
}

/// One HTTP response tied to a specific request identity.
#[derive(Debug, Clone)]
pub struct ResponseRecord {
    pub request_id: String,
    pub requested_identity: String,
    pub body_identity: String,
    pub fields: BTreeMap<String, String>,
    pub fault: ResponseFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFault {
    None,
    /// Body identity ≠ requested — Plasm may reject.
    WrongIdentity,
    /// Body identity matches; field values lie — generally undetectable.
    WrongFields,
}

#[derive(Debug, Default)]
pub struct ReferenceModel {
    pub backend: BackendState,
    pub committed_writes: Vec<CommittedWrite>,
    pub reported: Vec<ReportedEffect>,
    pub responses: Vec<ResponseRecord>,
}

impl ReferenceModel {
    pub fn record_commit(&mut self, expense: &BackendExpense) {
        self.committed_writes.push(CommittedWrite {
            expense_id: expense.expense_id.clone(),
            group_id: expense.group_id.clone(),
            description: expense.description.clone(),
        });
    }

    pub fn record_reported(&mut self, capability: &str, completed: usize, failed: usize) {
        self.reported.push(ReportedEffect {
            capability: capability.to_string(),
            completed,
            failed,
        });
    }

    pub fn committed_ids_for_group(&self, group_id: &str) -> BTreeSet<String> {
        self.committed_writes
            .iter()
            .filter(|w| w.group_id == group_id)
            .map(|w| w.expense_id.clone())
            .collect()
    }

    pub fn total_reported_completed(&self, capability: &str) -> usize {
        self.reported
            .iter()
            .filter(|r| r.capability == capability)
            .map(|r| r.completed)
            .sum()
    }

    pub fn total_reported_failed(&self, capability: &str) -> usize {
        self.reported
            .iter()
            .filter(|r| r.capability == capability)
            .map(|r| r.failed)
            .sum()
    }

    /// OPH-3: observed expense ids must equal every write that committed on the backend.
    pub fn check_partial_write_visibility(
        &self,
        group_id: &str,
        observed_ids: &BTreeSet<String>,
    ) -> Result<(), String> {
        let expected = self.backend.expenses_for_group(group_id);
        if observed_ids != &expected {
            return Err(format!(
                "OPH-3 partial-write visibility: observed={observed_ids:?} backend={expected:?} \
                 committed_ledger={:?}",
                self.committed_ids_for_group(group_id)
            ));
        }
        Ok(())
    }

    /// OPH-6: successful acks match committed writes; failures do not imply rollback.
    ///
    /// A lone create that returns `Err` may carry no `OperationAck` — the property is that
    /// prior successful acks still equal commits, not that the error path minted `failed`.
    pub fn check_acknowledgment_honesty(
        &self,
        capability: &str,
        plan_failed: bool,
    ) -> Result<(), String> {
        let completed = self.total_reported_completed(capability);
        let committed = self.committed_writes.len();
        if completed != committed {
            return Err(format!(
                "OPH-6 ack honesty: reported completed={completed} != committed={committed}"
            ));
        }
        if plan_failed && committed > 0 && completed == 0 {
            return Err(
                "OPH-6 ack honesty: failed batch implied rollback (completed wiped)".into(),
            );
        }
        let _ = self.total_reported_failed(capability);
        Ok(())
    }

    /// OPH-5: Complete coverage must match the reference full set; empty Complete with
    /// non-empty backend is dishonest.
    pub fn check_coverage_honesty(
        &self,
        group_id: &str,
        coverage: ObservationCoverage,
        observed_ids: &BTreeSet<String>,
    ) -> Result<(), String> {
        let expected = self.backend.expenses_for_group(group_id);
        match coverage {
            ObservationCoverage::Complete => {
                if observed_ids != &expected {
                    return Err(format!(
                        "OPH-5 coverage honesty: Complete but observed={observed_ids:?} \
                         != backend={expected:?}"
                    ));
                }
            }
            ObservationCoverage::Partial | ObservationCoverage::Unknown => {
                // Missing acquisition must not silently claim Complete — already enforced
                // by the match arm above. Partial/Unknown may under-report.
                if !observed_ids.is_subset(&expected) {
                    return Err(format!(
                        "OPH-5 coverage honesty: {coverage:?} invented ids beyond backend: \
                         observed={observed_ids:?} backend={expected:?}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// OPH-1: a response for requested identity A must not populate entity B.
    pub fn check_identity_isolation(
        published: &BTreeMap<String, BTreeMap<String, String>>,
        responses: &[ResponseRecord],
    ) -> Result<(), String> {
        for resp in responses {
            if resp.fault != ResponseFault::WrongIdentity {
                continue;
            }
            // Divergent body identity must not appear as a published/graph row.
            if published.contains_key(&resp.body_identity)
                && resp.body_identity != resp.requested_identity
            {
                return Err(format!(
                    "OPH-1 identity isolation: wrong-identity body `{}` for request `{}` \
                     was published",
                    resp.body_identity, resp.requested_identity
                ));
            }
            // Contaminate: requested row must not carry the injected foreign fields.
            if let Some(fields) = published.get(&resp.requested_identity) {
                for (k, v) in &resp.fields {
                    if fields.get(k) == Some(v) {
                        return Err(format!(
                            "OPH-1 identity isolation: injected field {k}={v} landed on \
                             retained request identity {}",
                            resp.requested_identity
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_rejects_foreign_id_even_when_expected_id_is_missing() {
        let mut model = ReferenceModel::default();
        model.backend.commit_expense("g", "a");
        model.backend.commit_expense("g", "b");
        let observed = BTreeSet::from(["e1".into(), "foreign".into()]);
        for coverage in [
            ObservationCoverage::Complete,
            ObservationCoverage::Partial,
            ObservationCoverage::Unknown,
        ] {
            assert!(model
                .check_coverage_honesty("g", coverage, &observed)
                .is_err());
        }
    }
}
