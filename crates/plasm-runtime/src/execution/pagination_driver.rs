//! Deep pagination driver: traversal coordinates, progress guards, and page audit evidence.
//!
//! One caller entry: a validated contract plus loop state. Host delivery budgets truncate
//! **locally** after decode and never rewrite Fixed page-size parameters.

use std::collections::BTreeSet;

use plasm_compile::{
    CompiledOperation, PaginationConfig, PaginationStrategyKind, ValidatedPagination,
};
use plasm_core::QueryPagination;
use serde::{Deserialize, Serialize};

use super::pagination_state::PaginationLoopState;
use super::{QueryPaginationState, RuntimeError, StreamConsumeOpts};

/// Why pagination stopped (authoritative terminal or fail-closed corruption).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaginationTerminalReason {
    ShortPage,
    StopWhen,
    CursorExhausted,
    CounterMax,
    EmptyPage,
    MaxPages,
    MaxItems,
    RowMatchBudget,
    LinkHeaderExhausted,
    NextUrlExhausted,
    BlockRangeComplete,
    NonProgress,
    RepeatedCoordinate,
    DuplicateIdentityOverlap,
    FullPageZeroNovel,
    ResumeContractMismatch,
    Complete,
}

/// Sanitized per-page audit record (no auth, no raw cursors, no signed URLs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageAudit {
    pub page_ordinal: u32,
    pub strategy: Option<String>,
    pub param_digest: String,
    pub requested_size: u32,
    pub returned_rows: u32,
    pub novel_rows: u32,
    pub duplicate_rows: u32,
    pub accepted_rows: u32,
    pub coordinate_digest: String,
    pub fingerprint: String,
    pub continuation: Option<String>,
    pub terminal: Option<PaginationTerminalReason>,
}

fn digest_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(blake3::hash(bytes.as_ref()).as_bytes())
}

fn sanitize_param_digest(values: &[(String, Option<serde_json::Value>)]) -> String {
    let mut parts = Vec::new();
    for (name, val) in values {
        let n = name.to_lowercase();
        // Never include raw cursor/token/url contents — hash them so coordinates still progress.
        let sensitive = n.contains("cursor")
            || n.contains("token")
            || n.contains("link")
            || n.contains("url")
            || n.contains("continuation");
        match val {
            Some(serde_json::Value::Number(num)) => parts.push(format!("{name}={num}")),
            Some(serde_json::Value::Bool(b)) => parts.push(format!("{name}={b}")),
            Some(val) if sensitive => {
                parts.push(format!("{name}=h:{}", digest_hex(val.to_string())));
            }
            Some(serde_json::Value::String(s))
                if s.len() <= 24
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) =>
            {
                parts.push(format!("{name}={s}"));
            }
            Some(_) => parts.push(format!("{name}=<opaque>")),
            None => parts.push(format!("{name}=<absent>")),
        }
    }
    parts.join("&")
}

/// Progress / overlap guard state across pages.
#[derive(Debug, Default)]
pub(crate) struct PaginationProgressGuard {
    seen_coords: BTreeSet<String>,
    seen_ids: BTreeSet<String>,
    last_cursor_digest: Option<String>,
    last_next_url_digest: Option<String>,
}

impl PaginationProgressGuard {
    fn observe_page(
        &mut self,
        coordinate_digest: &str,
        entity_ids: &[String],
        cursor_digest: Option<&str>,
        next_url_digest: Option<&str>,
        full_page: bool,
    ) -> Result<(u32, u32), PaginationTerminalReason> {
        if !self.seen_coords.insert(coordinate_digest.to_string()) {
            return Err(PaginationTerminalReason::RepeatedCoordinate);
        }
        if let Some(c) = cursor_digest {
            if self.last_cursor_digest.as_deref() == Some(c) {
                return Err(PaginationTerminalReason::NonProgress);
            }
            self.last_cursor_digest = Some(c.to_string());
        }
        if let Some(u) = next_url_digest {
            if self.last_next_url_digest.as_deref() == Some(u) {
                return Err(PaginationTerminalReason::NonProgress);
            }
            self.last_next_url_digest = Some(u.to_string());
        }

        let mut novel = 0u32;
        let mut dup = 0u32;
        for id in entity_ids {
            if self.seen_ids.insert(id.clone()) {
                novel += 1;
            } else {
                dup += 1;
            }
        }
        if dup > 0 && novel == 0 && full_page {
            return Err(PaginationTerminalReason::FullPageZeroNovel);
        }
        if dup > 0 && novel > 0 {
            return Err(PaginationTerminalReason::DuplicateIdentityOverlap);
        }
        Ok((novel, dup))
    }
}

/// Driver: validated contract + loop state + progress guards. Audit records are returned,
/// not retained — telemetry is the sink.
pub struct PaginationDriver {
    contract: ValidatedPagination,
    state: PaginationLoopState,
    guard: PaginationProgressGuard,
}

impl PaginationDriver {
    pub(crate) fn new(
        contract: ValidatedPagination,
        user: &QueryPagination,
        consume: &StreamConsumeOpts,
    ) -> Result<Self, RuntimeError> {
        let state = PaginationLoopState::new(contract.config(), user, consume)?;
        Ok(Self {
            contract,
            state,
            guard: PaginationProgressGuard::default(),
        })
    }

    pub(crate) fn from_resume(contract: ValidatedPagination, state: PaginationLoopState) -> Self {
        Self {
            contract,
            state,
            guard: PaginationProgressGuard::default(),
        }
    }

    pub fn try_from_config(
        config: PaginationConfig,
        user: &QueryPagination,
        consume: &StreamConsumeOpts,
    ) -> Result<Self, RuntimeError> {
        let contract = config
            .validate()
            .map_err(|e| RuntimeError::ConfigurationError {
                message: e.to_string(),
            })?;
        Self::new(contract, user, consume)
    }

    pub(crate) fn config(&self) -> &PaginationConfig {
        self.contract.config()
    }

    pub(crate) fn strategy(&self) -> PaginationStrategyKind {
        self.contract.strategy()
    }

    pub(crate) fn take_next_absolute_url(&mut self) -> Option<String> {
        self.state.next_absolute_url.take()
    }

    pub(crate) fn last_requested_limit(&self) -> u32 {
        self.state.last_requested_limit
    }

    pub(crate) fn snapshot(&self) -> QueryPaginationState {
        (&self.state).into()
    }

    pub(crate) fn apply_request_params(
        &mut self,
        compiled: &mut CompiledOperation,
    ) -> Result<(), RuntimeError> {
        self.state
            .apply_request_params(compiled, self.contract.config())
    }

    pub(crate) fn advance_after_page(
        &mut self,
        response: &serde_json::Value,
        full_page_len: usize,
        link_next: Option<&str>,
        last_entity_id: Option<&str>,
    ) -> Result<bool, RuntimeError> {
        let limit = self.state.last_requested_limit;
        self.state.advance_after_page(
            self.contract.config(),
            response,
            full_page_len,
            limit,
            link_next,
            last_entity_id,
        )
    }

    /// Observe one decoded page: fail closed on non-progress / overlap, return sanitized audit.
    pub fn record_page(
        &mut self,
        page_ordinal: u32,
        entity_ids: &[String],
        returned_rows: u32,
        accepted_rows: u32,
        continuation: Option<&str>,
        terminal: Option<PaginationTerminalReason>,
    ) -> Result<PageAudit, RuntimeError> {
        let snap = self.snapshot();
        let param_digest = sanitize_param_digest(&snap.param_values);
        let coordinate_digest = digest_hex(param_digest.as_bytes());
        let cursor_digest = snap
            .param_values
            .iter()
            .find(|(n, _)| {
                let l = n.to_lowercase();
                l.contains("cursor") || l.contains("token") || l.contains("after")
            })
            .and_then(|(_, v)| v.as_ref())
            .map(|v| digest_hex(v.to_string()));
        let next_url_digest = snap
            .next_absolute_url
            .as_ref()
            .map(|u| digest_hex(u.as_bytes()));
        let full_page = returned_rows > 0
            && self
                .contract
                .page_size()
                .is_some_and(|s| returned_rows >= s);

        let (novel, dup) = self
            .guard
            .observe_page(
                &coordinate_digest,
                entity_ids,
                cursor_digest.as_deref(),
                next_url_digest.as_deref(),
                full_page,
            )
            .map_err(|reason| RuntimeError::ConfigurationError {
                message: format!("pagination progress guard: {reason:?}"),
            })?;

        Ok(PageAudit {
            page_ordinal,
            strategy: Some(self.strategy().to_string()),
            param_digest,
            requested_size: self.last_requested_limit(),
            returned_rows,
            novel_rows: novel,
            duplicate_rows: dup,
            accepted_rows,
            fingerprint: digest_hex(format!(
                "{page_ordinal}|{coordinate_digest}|{returned_rows}|{novel}|{dup}"
            )),
            coordinate_digest,
            continuation: continuation.map(str::to_string),
            terminal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_compile::{PaginationLocation, PaginationParam, PaginationParamRole};

    fn page_number_config() -> PaginationConfig {
        PaginationConfig {
            strategy: Some(PaginationStrategyKind::PageNumber),
            params: indexmap::indexmap! {
                "page_index".into() => PaginationParam::Counter { counter: 0, step: 1, max: None },
                "page_limit".into() => PaginationParam::Fixed {
                    fixed: serde_json::json!(20),
                    role: Some(PaginationParamRole::PageSize),
                },
            },
            location: PaginationLocation::Query,
            body_merge_path: None,
            response_prefix: None,
            response_next_url_field: None,
            stop_when: None,
        }
    }

    #[test]
    fn rejects_missing_strategy() {
        let err = PaginationConfig {
            strategy: None,
            params: indexmap::indexmap! {},
            location: PaginationLocation::Query,
            body_merge_path: None,
            response_prefix: None,
            response_next_url_field: None,
            stop_when: None,
        }
        .validate();
        assert!(err.is_err());
    }

    #[test]
    fn page_number_contract_ok() {
        let c = page_number_config().validate().expect("contract");
        assert_eq!(c.page_size(), Some(20));
    }

    #[test]
    fn guard_detects_duplicate_identity_overlap() {
        let mut g = PaginationProgressGuard::default();
        g.observe_page("c0", &["a".into(), "b".into()], None, None, true)
            .unwrap();
        let err = g.observe_page("c1", &["b".into(), "c".into()], None, None, true);
        assert_eq!(err, Err(PaginationTerminalReason::DuplicateIdentityOverlap));
    }
}
