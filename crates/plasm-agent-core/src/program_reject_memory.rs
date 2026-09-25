//! In-session memory of compile/plan rejects.
//!
//! The host must not re-present a normalize-equal rejected program (or the same
//! reject text) as a fresh error. This log names the replay; it does not halt
//! the session or prescribe a replacement form.

use std::collections::VecDeque;

/// Cap on remembered rejects (FIFO). Not a halt — new programs still compile.
const MAX_REMEMBERED_REJECTS: usize = 64;

/// How a new reject matches in-session memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReplayKind {
    /// Normalized program source already rejected in this session.
    ExactProgram,
    /// Same category + normalized correction already returned (program text differed).
    SameReject,
}

/// Prior identical reject observed in this execute session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectReplay {
    pub kind: RejectReplayKind,
    /// How many earlier rejects matched this kind (1 = first replay).
    pub prior_count: u32,
}

impl RejectReplay {
    pub fn kind_wire(self) -> &'static str {
        match self.kind {
            RejectReplayKind::ExactProgram => "exact_program",
            RejectReplayKind::SameReject => "same_reject",
        }
    }

    /// Agent-visible honesty. Domain-neutral — no replacement recipe.
    pub fn markdown_line(self) -> String {
        match self.kind {
            RejectReplayKind::ExactProgram => format!(
                "This exact program was already rejected in this session ({} prior). Submit a different program.",
                self.prior_count
            ),
            RejectReplayKind::SameReject => format!(
                "This same reject was already returned in this session ({} prior). Submit a different program.",
                self.prior_count
            ),
        }
    }
}

/// Normalize program / correction text for identical-reject comparison.
///
/// Unifies newline encodings only; indentation and string contents are semantic.
/// Does not collapse interior tokens.
pub fn normalize_reject_source(src: &str) -> String {
    src.replace("\r\n", "\n").replace('\r', "\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RejectRecord {
    program: String,
    category: String,
    correction: String,
}

/// FIFO log of needs_fix / deny rejects for one execute session.
#[derive(Debug, Default)]
pub struct SessionRejectLog {
    entries: VecDeque<RejectRecord>,
}

impl SessionRejectLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record this reject. `Some` means the host already returned a matching reject.
    pub fn record(
        &mut self,
        program: &str,
        category: &str,
        correction: &str,
    ) -> Option<RejectReplay> {
        let program = normalize_reject_source(program);
        let correction = normalize_reject_source(correction);
        let category = category.to_string();

        let program_prior = self.entries.iter().filter(|e| e.program == program).count() as u32;
        let reject_prior = self
            .entries
            .iter()
            .filter(|e| e.category == category && e.correction == correction)
            .count() as u32;

        self.entries.push_back(RejectRecord {
            program,
            category,
            correction,
        });
        while self.entries.len() > MAX_REMEMBERED_REJECTS {
            self.entries.pop_front();
        }

        if program_prior > 0 {
            Some(RejectReplay {
                kind: RejectReplayKind::ExactProgram,
                prior_count: program_prior,
            })
        } else if reject_prior > 0 {
            Some(RejectReplay {
                kind: RejectReplayKind::SameReject,
                prior_count: reject_prior,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_preserves_python_indentation_and_string_whitespace() {
        let a = "rows = e1\r\n  dest = rows | select x = (path | split_part('/', 0))  \n\n";
        let b = "rows = e1\n dest = rows | select x = (path | split_part('/', 0))";
        assert_ne!(normalize_reject_source(a), normalize_reject_source(b));
        assert_eq!(normalize_reject_source("x\r\ny"), "x\ny");
    }

    #[test]
    fn first_reject_is_fresh() {
        let mut log = SessionRejectLog::new();
        assert_eq!(
            log.record("rows = e1\nbad", "plan", "unknown pipe stage `split_part`"),
            None
        );
    }

    #[test]
    fn exact_program_replay_is_named() {
        let mut log = SessionRejectLog::new();
        let program = "rows = e1\nout = rows | select dest = (path | split_part('/', 0))";
        let correction = "unknown pipe stage `split_part('/', 0)`";
        assert!(log.record(program, "plan", correction).is_none());
        let replay = log
            .record(program, "plan", correction)
            .expect("second submit is replay");
        assert_eq!(replay.kind, RejectReplayKind::ExactProgram);
        assert_eq!(replay.prior_count, 1);
        let third = log
            .record(
                "rows = e1 \n out = rows | select dest = (path | split_part('/', 0))\n",
                "plan",
                correction,
            )
            .expect("normalize-equal program is replay");
        assert_eq!(third.kind, RejectReplayKind::SameReject);
        assert_eq!(third.prior_count, 2);
    }

    #[test]
    fn same_correction_different_program_is_same_reject() {
        let mut log = SessionRejectLog::new();
        let correction = "unknown pipe stage `split_part('/', 0)`";
        assert!(log
            .record(
                "a = e1 | select dest = (path | split_part('/', 0))",
                "plan",
                correction
            )
            .is_none());
        let replay = log
            .record(
                "b = e2 | select dest = (other | split_part('/', 0))",
                "plan",
                correction,
            )
            .expect("same reject text is replay");
        assert_eq!(replay.kind, RejectReplayKind::SameReject);
        assert_eq!(replay.prior_count, 1);
        assert!(replay.markdown_line().contains("same reject"));
        assert!(!replay.markdown_line().to_lowercase().contains("render"));
        assert!(!replay.markdown_line().contains("halt"));
    }

    #[test]
    fn different_program_and_correction_stay_fresh() {
        let mut log = SessionRejectLog::new();
        assert!(log.record("a = e99", "parse", "Change `e99`").is_none());
        assert!(log
            .record("b = e1 | where missing > 1", "type", "unknown field")
            .is_none());
    }
}
