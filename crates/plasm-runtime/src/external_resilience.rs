//! Shared retry law. Callers own admission, transport and effect classification.
use std::time::Duration;

/// RFC Retry-After: delta seconds or HTTP-date, without shortening server hints.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
}

/// Only the owner of an operation can attest that repeating it is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaySafety {
    Repeatable,
    Never,
}

/// Policy seam shared by domain HTTP and read-only provider judgments.
/// Retry-After is a lower bound; jitter must never shorten it.
pub trait RetryPolicy {
    fn max_attempts(&self) -> usize;
    fn retry_budget(&self) -> Duration;
    fn initial_backoff(&self) -> Duration;
    fn max_backoff(&self) -> Duration;

    fn delay(&self, attempt: usize, retry_after: Option<Duration>, entropy: u64) -> Duration {
        let base = self
            .initial_backoff()
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(31))
            .min(self.max_backoff());
        let ms = base.as_millis().min(u128::from(u64::MAX)) as u64;
        let half = ms / 2;
        let jitter = Duration::from_millis(half + entropy % (ms - half + 1));
        jitter.max(retry_after.unwrap_or_default())
    }

    fn allows_retry(
        &self,
        safety: ReplaySafety,
        attempt: usize,
        elapsed: Duration,
        delay: Duration,
    ) -> bool {
        safety == ReplaySafety::Repeatable
            && attempt < self.max_attempts()
            && elapsed.saturating_add(delay) < self.retry_budget()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Policy;
    impl RetryPolicy for Policy {
        fn max_attempts(&self) -> usize {
            3
        }
        fn retry_budget(&self) -> Duration {
            Duration::from_secs(30)
        }
        fn initial_backoff(&self) -> Duration {
            Duration::from_millis(500)
        }
        fn max_backoff(&self) -> Duration {
            Duration::from_secs(2)
        }
    }
    #[test]
    fn server_delay_and_nonreplayable_effects_are_hard_bounds() {
        for entropy in 0..1000 {
            let delay = Policy.delay(1, Some(Duration::from_secs(20)), entropy);
            assert!(delay >= Duration::from_secs(20));
            assert!(!Policy.allows_retry(ReplaySafety::Never, 1, Duration::ZERO, delay));
            assert!(!Policy.allows_retry(ReplaySafety::Repeatable, 3, Duration::ZERO, delay));
            assert!(!Policy.allows_retry(
                ReplaySafety::Repeatable,
                1,
                Duration::from_secs(10),
                delay
            ));
        }
    }

    #[test]
    fn retry_after_supports_dates_and_does_not_clip_large_hints() {
        assert_eq!(
            parse_retry_after(" 172800 "),
            Some(Duration::from_secs(172800))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(Duration::ZERO)
        );
        assert!(parse_retry_after("Wed, 21 Oct 2099 07:28:00 GMT").is_some());
        assert_eq!(parse_retry_after("not a header"), None);
    }
}
