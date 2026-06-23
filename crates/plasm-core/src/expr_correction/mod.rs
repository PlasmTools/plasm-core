//! Deterministic expression correction: surface rewrites + domain lexicon.
//!
//! # Genericity: CGS-sourced only
//!
//! This module must remain **generic**: every rewrite rule is driven by the loaded
//! [`crate::CGS`] and a [`crate::domain_lexicon::DomainLexicon`] built from it (entity
//! names, fields, capabilities, scope roles). **Do not** embed API-specific names,
//! hardcoded field lists, or domain exceptions here—new behavior belongs in schema
//! (`domain.yaml` / `mappings.yaml`) or lexicon construction, not in ad hoc string
//! matches in this crate.
//!
//! # Correction pipeline (REPL and eval apply in this order)
//!
//! 1. Strict [`crate::expr_parser::parse`].
//! 2. [`try_normalize_entity_case`] — if the parse failed, rewrite **entity**
//!    identifiers that match **exactly one** CGS entity name under ASCII
//!    case-insensitive comparison (leading token and each `.^Target` reverse
//!    segment). Example: `message{…}` → `Message{…}` when only `Message` exists.
//! 3. [`try_auto_correct`] — only `Entity{predicate,…}` query shapes; uses
//!    [`DomainLexicon`](crate::domain_lexicon::DomainLexicon) for synonym / scope resolution and predicate rewrite.
//!    Predicate commas and comparison operators are scanned via
//!    [`crate::expr_parser::predicate_surface`] so correction agrees with the main parser surface.
//!
//! # Lexicon safety contract
//!
//! The lexicon corrector only rewrites when it resolves a failed predicate field
//! to **exactly one** valid schema term for the entity being queried.
//!
//! - [`CorrectionOutcome::Corrected`] / [`CorrectionOutcome::Dropped`] —
//!   the expression was rewritten safely; the caller should accept it.
//! - [`CorrectionOutcome::Ambiguous`] — 2+ candidates found; carries structured
//!   disambiguation hints for the LLM correction round.
//! - [`CorrectionOutcome::Uncorrectable`] — corrector could not help; fall through
//!   to the original diagnostic.
//!
//! In no case does the lexicon corrector silently guess or discard information.
//!
//! # Extension roadmap (same small grammars; not all implemented)
//!
//! - **Entity case** — implemented ([`try_normalize_entity_case`]).
//! - **Field / relation segment case** — fold `.foo` / `.^Bar` field tokens against
//!   `fields ∪ relations` on the active entity (unique case-insensitive match).
//! - **Predicate punctuation** — trim stray commas; optional `==` → `=` in `{ }`.
//! - **String literals** — normalize fancy quotes to ASCII `"` / `'`.
//! - **Search lane** — lexicon-driven fixes for `Entity~"text"` (map to search param).
//! - **Thin wrappers** — strip redundant `Entity(id)` around bare IDs when unambiguous.

mod auto_correct;
mod entity_case;
mod recovery;
mod recovery_hint;

pub use auto_correct::{try_auto_correct, CorrectionOutcome};
pub use entity_case::{resolve_entity_case_insensitive, try_normalize_entity_case};
pub use recovery::{
    parse_session_line_with_rewrite_recovery, recover_parse, recover_parse_with_rewrite,
};
pub use recovery_hint::RecoveryHint;
