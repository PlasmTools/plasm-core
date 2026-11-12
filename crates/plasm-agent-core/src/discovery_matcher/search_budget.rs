//! Search-turn admission. Charge before dispatch, including failed requests.
//! UTF-8 bytes are an explicit conservative work limit, not a token estimate.
use std::collections::BTreeSet;

pub(super) const MAX_REQUESTS: usize = 1;
pub(super) const MAX_ATTEMPTS: usize = 3;
pub(super) const MAX_INPUT_BYTES: usize = 32_000;
// The packet allowance is independent of the per-search transmission budget.
// A larger packet consumes more of the same fixed budget, including on retry.
pub(super) const MAX_TRANSMITTED_BYTES: usize = 48_000;
pub(super) const MAX_REPORTED_INPUT_TOKENS: u64 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BudgetStop {
    Requests,
    InputBytes,
    ReportedInputTokens,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchBudget<K: Ord> {
    members: BTreeSet<K>,
    requests: usize,
    input_bytes: usize,
    reported_input_tokens: u64,
    missing_usage: usize,
}

impl<K: Ord> Default for SearchBudget<K> {
    fn default() -> Self {
        Self {
            members: BTreeSet::new(),
            requests: 0,
            input_bytes: 0,
            reported_input_tokens: 0,
            missing_usage: 0,
        }
    }
}

impl<K: Ord + Clone> SearchBudget<K> {
    /// Membership includes required acquisitions even though they need no model
    /// judgment. A rejected reservation changes no state and exposes no member.
    pub fn reserve(
        &mut self,
        members: impl IntoIterator<Item = K>,
        bodies: &[&str],
    ) -> Result<(), BudgetStop> {
        if self.reported_input_tokens > MAX_REPORTED_INPUT_TOKENS {
            return Err(BudgetStop::ReportedInputTokens);
        }
        let members: BTreeSet<_> = self.members.iter().cloned().chain(members).collect();
        let requests = self.requests.saturating_add(bodies.len());
        if requests > MAX_REQUESTS {
            return Err(BudgetStop::Requests);
        }
        let input_bytes = bodies
            .iter()
            .fold(self.input_bytes, |sum, body| sum.saturating_add(body.len()));
        if input_bytes > MAX_INPUT_BYTES {
            return Err(BudgetStop::InputBytes);
        }
        self.members = members;
        self.requests = requests;
        self.input_bytes = input_bytes;
        Ok(())
    }

    /// Missing usage is unavailable, never an observed zero. Neither failure nor
    /// cancellation refunds admission; continuations retain this same budget.
    pub fn record_usage(&mut self, input_tokens: Option<u64>) {
        match input_tokens {
            Some(tokens) => {
                self.reported_input_tokens = self.reported_input_tokens.saturating_add(tokens);
            }
            None => self.missing_usage = self.missing_usage.saturating_add(1),
        }
    }

    /// Retry the same admitted packet only; this does not admit another judgment
    /// or reset spent work. The selector retains the immutable packet bytes.
    pub fn reserve_retry(&mut self, body: &str) -> Result<(), BudgetStop> {
        if self.reported_input_tokens > MAX_REPORTED_INPUT_TOKENS {
            return Err(BudgetStop::ReportedInputTokens);
        }
        if self.requests == 0 || self.requests >= MAX_ATTEMPTS {
            return Err(BudgetStop::Requests);
        }
        let bytes = self.input_bytes.saturating_add(body.len());
        if body.len() > MAX_INPUT_BYTES || bytes > MAX_TRANSMITTED_BYTES {
            return Err(BudgetStop::InputBytes);
        }
        self.requests += 1;
        self.input_bytes = bytes;
        Ok(())
    }

    /// Admit a distinct follow-up packet against the same turn budget.
    /// Attempts, retries, bytes, and reported tokens remain cumulative.
    pub fn reserve_followup(
        &mut self,
        members: impl IntoIterator<Item = K>,
        body: &str,
    ) -> Result<(), BudgetStop> {
        if self.reported_input_tokens > MAX_REPORTED_INPUT_TOKENS {
            return Err(BudgetStop::ReportedInputTokens);
        }
        if self.requests == 0 || self.requests >= MAX_ATTEMPTS {
            return Err(BudgetStop::Requests);
        }
        let bytes = self.input_bytes.saturating_add(body.len());
        if body.len() > MAX_INPUT_BYTES || bytes > MAX_TRANSMITTED_BYTES {
            return Err(BudgetStop::InputBytes);
        }
        self.members.extend(members);
        self.requests += 1;
        self.input_bytes = bytes;
        Ok(())
    }

    pub fn members(&self) -> &BTreeSet<K> {
        &self.members
    }

    pub fn requests(&self) -> usize {
        self.requests
    }

    pub fn input_bytes(&self) -> usize {
        self.input_bytes
    }

    pub fn reported_input_tokens(&self) -> u64 {
        self.reported_input_tokens
    }

    pub fn missing_usage(&self) -> usize {
        self.missing_usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn larger_packets_share_the_unchanged_total_budget() {
        let large = "x".repeat(30_000);
        let mut budget = SearchBudget::default();
        budget.reserve([0], &[&large]).unwrap();
        let before = budget.clone();
        assert_eq!(budget.reserve_retry(&large), Err(BudgetStop::InputBytes));
        assert_eq!(budget, before);
        budget.reserve_followup([1], &"x".repeat(18_000)).unwrap();
        assert_eq!(budget.input_bytes(), 48_000);
        assert_eq!(
            budget.reserve_followup([2], "x"),
            Err(BudgetStop::InputBytes)
        );
    }

    #[test]
    fn distinct_followups_share_the_turn_attempt_and_byte_budget() {
        let mut budget = SearchBudget::default();
        budget.reserve([1], &["first"]).unwrap();
        budget.reserve_followup([2], "different packet").unwrap();
        budget.reserve_retry("different packet").unwrap();
        assert_eq!(budget.requests(), 3);
        assert_eq!(budget.members(), &BTreeSet::from([1, 2]));
        let before = budget.clone();
        assert_eq!(
            budget.reserve_followup([3], "fourth"),
            Err(BudgetStop::Requests)
        );
        assert_eq!(budget, before);
    }

    #[test]
    fn retries_charge_full_packet_without_readmitting_a_judgment() {
        let mut budget = SearchBudget::default();
        let body = "x".repeat(16_000);
        assert_eq!(budget.reserve_retry(&body), Err(BudgetStop::Requests));
        budget.reserve([0], &[&body]).unwrap();
        for _ in 1..MAX_ATTEMPTS {
            budget.record_usage(None);
            budget.reserve_retry(&body).unwrap();
        }
        assert_eq!(budget.requests(), MAX_ATTEMPTS);
        assert_eq!(budget.input_bytes(), MAX_TRANSMITTED_BYTES);
        let mut continued = budget.clone();
        assert_eq!(continued.reserve_retry(&body), Err(BudgetStop::Requests));
        assert_eq!(
            continued.reserve([1], &["another judgment"]),
            Err(BudgetStop::Requests)
        );
        assert_eq!(continued, budget);
        let mut budget = SearchBudget::default();
        budget.reserve([0], &["first"]).unwrap();
        let before = budget.clone();
        assert_eq!(
            budget.reserve_retry(&"x".repeat(MAX_INPUT_BYTES + 1)),
            Err(BudgetStop::InputBytes)
        );
        assert_eq!(budget, before);
        budget.record_usage(Some(MAX_REPORTED_INPUT_TOKENS + 1));
        assert_eq!(
            budget.reserve_retry("first"),
            Err(BudgetStop::ReportedInputTokens)
        );
    }

    #[test]
    fn required_members_are_charged_and_shared_members_count_once() {
        let mut budget = SearchBudget::default();
        budget
            .reserve((0..12).chain([1, 2]), &["root plus required closure"])
            .unwrap();
        let before = budget.clone();
        assert_eq!(
            budget.reserve([8], &["new member"]),
            Err(BudgetStop::Requests)
        );
        assert_eq!(budget, before);
        assert_eq!(budget.members().len(), 12);
    }

    #[test]
    fn repeated_context_and_utf8_are_charged_without_truncation() {
        let mut budget = SearchBudget::default();
        let body = "é".repeat(MAX_INPUT_BYTES / 2);
        budget.reserve([0], &[&body]).unwrap();
        assert_eq!(budget.input_bytes(), MAX_INPUT_BYTES);
        let mut budget = SearchBudget::default();
        let before = budget.clone();
        assert_eq!(
            budget.reserve([0], &[&(body + "x")]),
            Err(BudgetStop::InputBytes)
        );
        assert_eq!(budget, before);
    }

    #[test]
    fn failed_requests_and_continuation_never_reset_admission() {
        let mut budget = SearchBudget::default();
        for _ in 0..MAX_REQUESTS {
            budget.reserve([0], &["request"]).unwrap();
            budget.record_usage(None);
        }
        let mut continued = budget.clone();
        assert_eq!(
            continued.reserve([0], &["retry"]),
            Err(BudgetStop::Requests)
        );
        assert_eq!(continued.requests(), 1);
        assert_eq!(continued.missing_usage(), 1);
        assert_eq!(continued.reported_input_tokens(), 0);
    }

    #[test]
    fn actual_usage_overrun_stops_later_dispatch() {
        let mut budget = SearchBudget::default();
        budget.reserve([0], &["request"]).unwrap();
        budget.record_usage(Some(16_001));
        assert_eq!(
            budget.reserve([1], &["next"]),
            Err(BudgetStop::ReportedInputTokens)
        );
        assert_eq!(budget.requests(), 1);
    }
}
