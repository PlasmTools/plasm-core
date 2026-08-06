//! Ephemeral clarify receipts for semantic auto-seed (`routing_ref` + `clarify_choice`).

use std::time::{Duration, Instant};

use dashmap::DashMap;
use plasm_core::discovery_seed_select::SeedAlternativeSetRaw;
use uuid::Uuid;

/// Default TTL for unused clarify receipts.
const PENDING_CLARIFY_TTL: Duration = Duration::from_secs(30 * 60);

/// Session phase the clarify receipt was minted under — redeem must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClarifyBinding {
    /// Abstention before a logical session exists (`session_mode: "new"`).
    PreMintNew,
    /// Abstention on an existing logical session (`session_mode: "extend"`).
    BoundExtend { logical_session_id: Uuid },
}

impl ClarifyBinding {
    #[must_use]
    pub fn matches(&self, expected: &ClarifyBinding) -> bool {
        self == expected
    }
}

/// One abstention receipt that can be resolved without re-running the selector.
#[derive(Debug, Clone)]
pub struct PendingClarifyChoice {
    pub alternatives: Vec<SeedAlternativeSetRaw>,
    pub intent: String,
    pub binding: ClarifyBinding,
    created_at: Instant,
}

impl PendingClarifyChoice {
    #[must_use]
    pub fn new(
        alternatives: Vec<SeedAlternativeSetRaw>,
        intent: impl Into<String>,
        binding: ClarifyBinding,
    ) -> Self {
        Self {
            alternatives,
            intent: intent.into(),
            binding,
            created_at: Instant::now(),
        }
    }

    #[must_use]
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > PENDING_CLARIFY_TTL
    }
}

/// Why redeem failed (invalid args / binding mismatch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClarifyRedeemError {
    UnknownOrExpired,
    BindingMismatch {
        stored: ClarifyBinding,
        expected: ClarifyBinding,
    },
    Choice(String),
}

impl ClarifyRedeemError {
    pub fn to_message(&self) -> String {
        match self {
            Self::UnknownOrExpired => {
                "`routing_ref` is unknown or expired — rephrase `intent` or retry after a fresh clarify breakout"
                    .into()
            }
            Self::BindingMismatch { stored, expected } => match (stored, expected) {
                (ClarifyBinding::BoundExtend { .. }, ClarifyBinding::PreMintNew) => {
                    "`routing_ref` was issued on an extend session — redeem with the same `logical_session_ref` and `session_mode: \"extend\"`"
                        .into()
                }
                (ClarifyBinding::PreMintNew, ClarifyBinding::BoundExtend { .. }) => {
                    "`routing_ref` was issued before mint — redeem with `session_mode: \"new\"` (no `logical_session_ref`)"
                        .into()
                }
                (ClarifyBinding::BoundExtend { .. }, ClarifyBinding::BoundExtend { .. }) => {
                    "`routing_ref` belongs to a different logical session — use the `logical_session_ref` from the breakout"
                        .into()
                }
                _ => "`routing_ref` binding does not match this plasm_context call".into(),
            },
            Self::Choice(msg) => msg.clone(),
        }
    }
}

/// In-memory clarify receipt store (process-local).
#[derive(Debug, Default)]
pub struct PendingClarifyRegistry {
    inner: DashMap<String, PendingClarifyChoice>,
}

impl PendingClarifyRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert and return a fresh `routing_ref` wire token (`rc_…`).
    pub fn insert(&self, pending: PendingClarifyChoice) -> String {
        self.gc();
        let token = format!("rc_{}", Uuid::new_v4().as_simple());
        self.inner.insert(token.clone(), pending);
        token
    }

    /// Take a receipt (consume-once). Returns `None` if missing or expired.
    pub fn take(&self, routing_ref: &str) -> Option<PendingClarifyChoice> {
        let key = routing_ref.trim();
        let (_, pending) = self.inner.remove(key)?;
        if pending.is_expired() {
            return None;
        }
        Some(pending)
    }

    /// Consume and verify binding against the caller's phase.
    pub fn redeem(
        &self,
        routing_ref: &str,
        expected: &ClarifyBinding,
    ) -> Result<PendingClarifyChoice, ClarifyRedeemError> {
        let pending = self
            .take(routing_ref)
            .ok_or(ClarifyRedeemError::UnknownOrExpired)?;
        if !pending.binding.matches(expected) {
            return Err(ClarifyRedeemError::BindingMismatch {
                stored: pending.binding,
                expected: expected.clone(),
            });
        }
        Ok(pending)
    }

    /// Peek without consuming.
    pub fn get(&self, routing_ref: &str) -> Option<PendingClarifyChoice> {
        let pending = self.inner.get(routing_ref.trim())?.clone();
        if pending.is_expired() {
            self.inner.remove(routing_ref.trim());
            return None;
        }
        Some(pending)
    }

    fn gc(&self) {
        let stale: Vec<String> = self
            .inner
            .iter()
            .filter(|e| e.value().is_expired())
            .map(|e| e.key().clone())
            .collect();
        for k in stale {
            self.inner.remove(&k);
        }
    }
}

/// Resolve `clarify_choice` against stored alternatives.
///
/// Accepts a 1-based index (`"1"`) or a catalog-qualified candidate id / entity name
/// that uniquely matches one alternative's `candidate_ids`.
pub fn resolve_clarify_choice(
    alternatives: &[SeedAlternativeSetRaw],
    choice: &str,
) -> Result<Vec<(String, String)>, String> {
    let choice = choice.trim();
    if choice.is_empty() {
        return Err(
            "`clarify_choice` must be a 1-based alternative index or a catalog:entity id".into(),
        );
    }

    let selected = if let Ok(idx) = choice.parse::<usize>() {
        if idx == 0 || idx > alternatives.len() {
            return Err(format!(
                "`clarify_choice` index {idx} out of range (1..={})",
                alternatives.len()
            ));
        }
        &alternatives[idx - 1]
    } else {
        let matches: Vec<_> = alternatives
            .iter()
            .enumerate()
            .filter(|(_, alt)| {
                alt.candidate_ids.iter().any(|id| {
                    id.eq_ignore_ascii_case(choice)
                        || id
                            .split_once(':')
                            .map(|(_, entity)| entity.eq_ignore_ascii_case(choice))
                            .unwrap_or(false)
                        || alt.label.eq_ignore_ascii_case(choice)
                })
            })
            .collect();
        match matches.as_slice() {
            [(_, alt)] => *alt,
            [] => {
                return Err(format!(
                    "`clarify_choice` `{choice}` did not match any alternative; use a 1-based index or catalog:entity id from the breakout"
                ));
            }
            _ => {
                return Err(format!(
                    "`clarify_choice` `{choice}` matched multiple alternatives; use a 1-based index"
                ));
            }
        }
    };

    let mut seeds = Vec::new();
    for id in &selected.candidate_ids {
        let Some((entry_id, entity)) = id.split_once(':') else {
            return Err(format!(
                "internal: alternative candidate `{id}` is not catalog:entity shaped"
            ));
        };
        if !seeds
            .iter()
            .any(|(e, n): &(String, String)| e == entry_id && n == entity)
        {
            seeds.push((entry_id.to_string(), entity.to_string()));
        }
    }
    if seeds.is_empty() {
        return Err("selected alternative has no candidate entities".into());
    }
    Ok(seeds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alt(label: &str, ids: &[&str]) -> SeedAlternativeSetRaw {
        SeedAlternativeSetRaw {
            candidate_ids: ids.iter().map(|s| (*s).to_string()).collect(),
            label: label.into(),
        }
    }

    #[test]
    fn resolve_by_index_and_candidate_id() {
        let alts = vec![
            alt("GitHub", &["github:Repository"]),
            alt("PokéAPI", &["pokeapi:Pokemon"]),
        ];
        assert_eq!(
            resolve_clarify_choice(&alts, "2").unwrap(),
            vec![("pokeapi".into(), "Pokemon".into())]
        );
        assert_eq!(
            resolve_clarify_choice(&alts, "github:Repository").unwrap(),
            vec![("github".into(), "Repository".into())]
        );
        assert_eq!(
            resolve_clarify_choice(&alts, "Pokemon").unwrap(),
            vec![("pokeapi".into(), "Pokemon".into())]
        );
    }

    #[test]
    fn registry_insert_and_take() {
        let reg = PendingClarifyRegistry::new();
        let token = reg.insert(PendingClarifyChoice::new(
            vec![alt("A", &["github:Issue"])],
            "open issues",
            ClarifyBinding::PreMintNew,
        ));
        assert!(token.starts_with("rc_"));
        let pending = reg.take(&token).expect("pending");
        assert_eq!(pending.intent, "open issues");
        assert!(reg.take(&token).is_none());
    }

    #[test]
    fn redeem_enforces_binding() {
        let reg = PendingClarifyRegistry::new();
        let sid = Uuid::new_v4();
        let token = reg.insert(PendingClarifyChoice::new(
            vec![alt("A", &["github:Issue"])],
            "issues",
            ClarifyBinding::BoundExtend {
                logical_session_id: sid,
            },
        ));
        let err = reg
            .redeem(&token, &ClarifyBinding::PreMintNew)
            .expect_err("binding mismatch");
        assert!(matches!(err, ClarifyRedeemError::BindingMismatch { .. }));

        let token2 = reg.insert(PendingClarifyChoice::new(
            vec![alt("A", &["github:Issue"])],
            "issues",
            ClarifyBinding::BoundExtend {
                logical_session_id: sid,
            },
        ));
        let ok = reg
            .redeem(
                &token2,
                &ClarifyBinding::BoundExtend {
                    logical_session_id: sid,
                },
            )
            .expect("match");
        assert_eq!(ok.intent, "issues");

        let other = Uuid::new_v4();
        let token3 = reg.insert(PendingClarifyChoice::new(
            vec![alt("A", &["github:Issue"])],
            "issues",
            ClarifyBinding::BoundExtend {
                logical_session_id: sid,
            },
        ));
        let err = reg
            .redeem(
                &token3,
                &ClarifyBinding::BoundExtend {
                    logical_session_id: other,
                },
            )
            .expect_err("wrong session");
        assert!(matches!(err, ClarifyRedeemError::BindingMismatch { .. }));
    }
}
