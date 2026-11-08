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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageContinuation {
    More,
    Exhausted(PaginationTerminalReason),
}

impl PageContinuation {
    pub(crate) fn has_more(&self) -> bool {
        matches!(self, Self::More)
    }
}

pub(crate) enum PageRequest {
    Compiled(Box<CompiledOperation>),
    Absolute(String),
}

pub(crate) struct PageObservation<'a> {
    pub ordinal: u32,
    pub entity_ids: &'a [String],
    pub returned_rows: u32,
    pub accepted_rows: u32,
    pub response: &'a serde_json::Value,
    pub next_link: Option<&'a str>,
}

pub(crate) struct PageTransition {
    pub audit: PageAudit,
    pub continuation: PageContinuation,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaginationProgressGuard {
    seen_coords: BTreeSet<String>,
    seen_ids: BTreeSet<String>,
    last_cursor_digest: Option<String>,
}

impl PaginationProgressGuard {
    /// Internal accounting view; cursor and identity digests remain off the wire.
    pub(super) fn retained_components(
        &self,
    ) -> (&BTreeSet<String>, &BTreeSet<String>, &Option<String>) {
        (&self.seen_coords, &self.seen_ids, &self.last_cursor_digest)
    }

    fn observe_page(
        &mut self,
        coordinate_digest: &str,
        entity_ids: &[String],
        cursor_digest: Option<&str>,
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
    requested_url_digest: Option<String>,
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
            requested_url_digest: None,
        })
    }

    pub(crate) fn from_resume(contract: ValidatedPagination, state: PaginationLoopState) -> Self {
        Self {
            contract,
            state,
            requested_url_digest: None,
        }
    }

    pub fn try_from_config(
        config: PaginationConfig,
        user: &QueryPagination,
        consume: &StreamConsumeOpts,
    ) -> Result<Self, RuntimeError> {
        let contract = config
            .validate()
            .map_err(RuntimeError::PaginationContract)?;
        Self::new(contract, user, consume)
    }

    pub(crate) fn config(&self) -> &PaginationConfig {
        self.contract.config()
    }

    pub(crate) fn strategy(&self) -> PaginationStrategyKind {
        self.contract.strategy()
    }

    pub(crate) fn last_requested_limit(&self) -> u32 {
        self.state.last_requested_limit
    }

    pub(crate) fn snapshot(&self) -> QueryPaginationState {
        (&self.state).into()
    }

    pub(crate) fn prepare_request(
        &mut self,
        base: &CompiledOperation,
    ) -> Result<PageRequest, RuntimeError> {
        if let Some(url) = self.state.next_absolute_url.take() {
            self.requested_url_digest = Some(digest_hex(&url));
            return Ok(PageRequest::Absolute(url));
        }
        self.requested_url_digest = None;
        let mut compiled = base.clone();
        self.state
            .apply_request_params(&mut compiled, self.contract.config())?;
        Ok(PageRequest::Compiled(Box::new(compiled)))
    }

    /// Validate a decoded page and advance its provider continuation as one transition.
    pub(crate) fn observe_page(
        &mut self,
        page: PageObservation<'_>,
    ) -> Result<PageTransition, RuntimeError> {
        let PageObservation {
            ordinal: page_ordinal,
            entity_ids,
            returned_rows,
            accepted_rows,
            response,
            next_link,
        } = page;
        let snap = self.snapshot();
        let param_digest = sanitize_param_digest(&snap.param_values);
        let coordinate_digest = digest_hex(serde_json::to_vec(&(
            &snap.param_values,
            &self.requested_url_digest,
            snap.from_block,
            snap.last_requested_to_block,
        ))?);
        let cursor_digest = snap
            .param_values
            .iter()
            .find(|(n, _)| {
                let l = n.to_lowercase();
                l.contains("cursor") || l.contains("token") || l.contains("after")
            })
            .and_then(|(_, v)| v.as_ref())
            .map(|v| digest_hex(v.to_string()));
        let full_page = returned_rows > 0
            && self
                .contract
                .page_size()
                .is_some_and(|s| returned_rows >= s);

        let (novel, dup) = self
            .state
            .progress_guard
            .observe_page(
                &coordinate_digest,
                entity_ids,
                cursor_digest.as_deref(),
                full_page,
            )
            .map_err(|reason| RuntimeError::PaginationProgress { reason })?;

        self.state.record_page_observed();
        let continuation = self.state.advance_after_page(
            self.contract.config(),
            response,
            returned_rows as usize,
            self.state.last_requested_limit,
            next_link,
        )?;
        let terminal = match &continuation {
            PageContinuation::More => None,
            PageContinuation::Exhausted(reason) => Some(reason.clone()),
        };
        let continuation_digest = self.state.next_absolute_url.as_deref().map(digest_hex);
        if continuation_digest.is_some() && continuation_digest == self.requested_url_digest {
            return Err(RuntimeError::PaginationProgress {
                reason: PaginationTerminalReason::RepeatedCoordinate,
            });
        }

        Ok(PageTransition {
            audit: PageAudit {
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
                continuation: continuation_digest,
                terminal,
            },
            continuation,
        })
    }
}

impl std::fmt::Display for PaginationTerminalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ShortPage => "short page",
            Self::StopWhen => "stop when",
            Self::CursorExhausted => "cursor exhausted",
            Self::CounterMax => "counter max",
            Self::EmptyPage => "empty page",
            Self::MaxPages => "max pages",
            Self::MaxItems => "max items",
            Self::RowMatchBudget => "row match budget",
            Self::LinkHeaderExhausted => "link header exhausted",
            Self::NextUrlExhausted => "next url exhausted",
            Self::BlockRangeComplete => "block range complete",
            Self::NonProgress => "non progress",
            Self::RepeatedCoordinate => "repeated coordinate",
            Self::DuplicateIdentityOverlap => "duplicate identity overlap",
            Self::FullPageZeroNovel => "full page zero novel",
            Self::ResumeContractMismatch => "resume contract mismatch",
            Self::Complete => "complete",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_compile::{PaginationLocation, PaginationParam, PaginationParamRole};

    #[tokio::test]
    async fn pagination_progress_retention_accounts_for_seen_identities() {
        use super::super::effect_continuation::{
            EffectContinuation, EffectContinuationError, MAX_EFFECT_SELECTION_BYTES,
        };
        let mut guard = PaginationProgressGuard::default();
        guard
            .seen_ids
            .insert("x".repeat(MAX_EFFECT_SELECTION_BYTES));
        let (control, _pauses) =
            EffectContinuation::new_native(std::time::Duration::from_secs(1)).unwrap();
        control
            .scope(async {
                assert!(matches!(
                    EffectContinuation::reserve_retained(&guard.retained_components()),
                    Err(EffectContinuationError::SelectionBudget(
                        plasm_core::ValueBudgetError::BytesExceeded
                    ))
                ));
            })
            .await;
    }

    fn page(
        driver: &mut PaginationDriver,
        ordinal: u32,
        ids: &[String],
        count: u32,
    ) -> Result<PageTransition, RuntimeError> {
        driver.observe_page(PageObservation {
            ordinal,
            entity_ids: ids,
            returned_rows: count,
            accepted_rows: count,
            response: &serde_json::json!({}),
            next_link: None,
        })
    }

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
            initial_only_query_params: vec![],
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
            initial_only_query_params: vec![],
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
        g.observe_page("c0", &["a".into(), "b".into()], None, true)
            .unwrap();
        let err = g.observe_page("c1", &["b".into(), "c".into()], None, true);
        assert_eq!(err, Err(PaginationTerminalReason::DuplicateIdentityOverlap));
    }

    #[test]
    fn resume_preserves_seen_page_coordinates() {
        let config = page_number_config();
        let mut first = PaginationDriver::try_from_config(
            config.clone(),
            &QueryPagination::default(),
            &StreamConsumeOpts::default(),
        )
        .unwrap();
        page(&mut first, 0, &["first".into()], 1).unwrap();
        let mut snapshot = first.snapshot();
        snapshot
            .param_values
            .iter_mut()
            .find(|(name, _)| name == "page_index")
            .unwrap()
            .1 = Some(serde_json::json!(0));
        let state = PaginationLoopState::try_from(snapshot).unwrap();
        let mut resumed = PaginationDriver::from_resume(config.validate().unwrap(), state);
        assert!(matches!(
            page(&mut resumed, 1, &["second".into()], 1),
            Err(RuntimeError::PaginationProgress {
                reason: PaginationTerminalReason::RepeatedCoordinate
            })
        ));
    }

    #[test]
    fn distinct_block_ranges_have_distinct_coordinates() {
        let mut config = page_number_config();
        config.strategy = Some(PaginationStrategyKind::BlockRange);
        config.location = PaginationLocation::BlockRange;
        config.params.shift_remove("page_index");
        let user = QueryPagination {
            from_block: Some(0),
            to_block: Some(2000),
            ..Default::default()
        };
        let mut driver = PaginationDriver::try_from_config(
            config,
            &user,
            &StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
        )
        .unwrap();
        let request = || {
            CompiledOperation::Http(plasm_compile::CompiledRequest {
                credential: None,
                method: plasm_compile::HttpMethod::Get,
                path: "/logs".into(),
                query: None,
                body: None,
                body_format: Default::default(),
                multipart: None,
                headers: None,
            })
        };
        driver.prepare_request(&request()).unwrap();
        assert!(page(&mut driver, 0, &["first".into()], 1)
            .unwrap()
            .continuation
            .has_more());
        driver.prepare_request(&request()).unwrap();
        page(&mut driver, 1, &["second".into()], 1).unwrap();
    }

    #[test]
    fn first_only_pinned_identity_does_not_weaken_cross_page_guard() {
        use plasm_compile::{CompiledRequest, HttpMethod};
        use plasm_core::Value;

        let mut config = page_number_config();
        config.initial_only_query_params = vec!["include_self".into()];
        let mut driver = PaginationDriver::try_from_config(
            config,
            &QueryPagination::default(),
            &StreamConsumeOpts::default(),
        )
        .expect("driver");
        let request = || {
            CompiledOperation::Http(CompiledRequest {
                credential: None,
                method: HttpMethod::Get,
                path: "/users".into(),
                query: Some(Value::Object(indexmap::indexmap! {
                    "include_self".into() => Value::Bool(true)
                })),
                body: None,
                body_format: Default::default(),
                multipart: None,
                headers: None,
            })
        };

        let PageRequest::Compiled(first) =
            driver.prepare_request(&request()).expect("first request")
        else {
            unreachable!()
        };
        let CompiledOperation::Http(first) = *first else {
            unreachable!()
        };
        assert_eq!(first.to_json()["query"]["include_self"], true);
        let first_ids: Vec<String> = (0..20)
            .map(|n| format!("user-{n}"))
            .chain(["self".into()])
            .collect();
        assert!(page(&mut driver, 0, &first_ids, 21)
            .expect("first page")
            .continuation
            .has_more());

        let PageRequest::Compiled(second) =
            driver.prepare_request(&request()).expect("second request")
        else {
            unreachable!()
        };
        let CompiledOperation::Http(second) = *second else {
            unreachable!()
        };
        assert!(second.to_json()["query"].get("include_self").is_none());
        let second_ids: Vec<String> = (20..40).map(|n| format!("user-{n}")).collect();
        assert!(page(&mut driver, 1, &second_ids, 20)
            .expect("nonoverlapping second page")
            .continuation
            .has_more());
        let contract = driver.config().validate().unwrap();
        let state = PaginationLoopState::try_from(driver.snapshot()).unwrap();
        let mut driver = PaginationDriver::from_resume(contract, state);
        let overlap = (39..59).map(|n| format!("user-{n}")).collect::<Vec<_>>();
        assert!(matches!(
            page(&mut driver, 2, &overlap, 20),
            Err(RuntimeError::PaginationProgress {
                reason: PaginationTerminalReason::DuplicateIdentityOverlap
            })
        ));
    }
}
