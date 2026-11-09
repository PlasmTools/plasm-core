//! Lossless ancestry for discovery. Position is node identity within one chain.
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub(crate) const SELECTION_HISTORY_HEADER: &str = "Ordered requests: later explicit revisions supersede earlier requirements while unchanged conditions remain applicable\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IntentProvenanceError {
    #[error("intent provenance must contain at least one turn")]
    Empty,
    #[error("intent provenance parent must be the preceding node")]
    InvalidParent,
    #[error("intent text must be nonempty and contain no NUL character")]
    InvalidIntentText,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentNode {
    #[serde(deserialize_with = "Option::deserialize")]
    parent: Option<usize>,
    intent: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WireProvenance")]
pub struct IntentProvenance {
    nodes: Vec<IntentNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProvenance {
    nodes: Vec<IntentNode>,
}

impl TryFrom<WireProvenance> for IntentProvenance {
    type Error = IntentProvenanceError;
    fn try_from(wire: WireProvenance) -> Result<Self, Self::Error> {
        if wire.nodes.is_empty() {
            return Err(IntentProvenanceError::Empty);
        }
        for (index, node) in wire.nodes.iter().enumerate() {
            if node.parent != index.checked_sub(1) {
                return Err(IntentProvenanceError::InvalidParent);
            }
            if node.intent.trim().is_empty() || node.intent.contains('\0') {
                return Err(IntentProvenanceError::InvalidIntentText);
            }
        }
        Ok(Self { nodes: wire.nodes })
    }
}

impl IntentProvenance {
    /// Original requested outcome, without interpreting later revisions.
    pub fn original(&self) -> &str {
        &self
            .nodes
            .first()
            .expect("validated nonempty provenance")
            .intent
    }

    pub fn turns(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().map(|node| node.intent.as_str())
    }

    /// Ordered selection context. Later explicit revisions supersede earlier
    /// requirements; unrelated earlier selection conditions remain applicable.
    pub fn selection_request(&self) -> String {
        let mut turns = Vec::new();
        for turn in self.turns() {
            if turns.last().copied() != Some(turn) {
                turns.push(turn);
            }
        }
        if turns.len() == 1 {
            return turns[0].to_owned();
        }
        let mut request = String::from(SELECTION_HISTORY_HEADER);
        for (index, turn) in turns.into_iter().enumerate() {
            use std::fmt::Write;
            let _ = writeln!(request, "Turn {}: {turn}", index + 1);
        }
        request
    }

    pub fn derived(&self, intent: String) -> Result<Self, IntentProvenanceError> {
        Self::from_turns(self.turns().map(str::to_owned).chain([intent]))
    }

    pub fn is_continuation_of(&self, previous: &Self) -> bool {
        self.nodes.starts_with(&previous.nodes) && self.nodes.len() <= previous.nodes.len() + 1
    }

    pub fn from_turns(
        turns: impl IntoIterator<Item = String>,
    ) -> Result<Self, IntentProvenanceError> {
        WireProvenance {
            nodes: turns
                .into_iter()
                .enumerate()
                .map(|(index, intent)| IntentNode {
                    parent: index.checked_sub(1),
                    intent,
                })
                .collect(),
        }
        .try_into()
    }

    pub fn current(&self) -> &str {
        &self
            .nodes
            .last()
            .expect("validated nonempty provenance")
            .intent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn provenance_roundtrip(turns in prop::collection::vec("[^\\x00]{1,80}", 1..32)) {
            let turns: Vec<_> = turns.into_iter().map(|s| format!("intent {s}")).collect();
            let chain = IntentProvenance::from_turns(turns.clone()).unwrap();
            let decoded: IntentProvenance = serde_json::from_str(&serde_json::to_string(&chain).unwrap()).unwrap();
            prop_assert_eq!(&decoded, &chain);
            let child = chain.derived("Next need".into()).unwrap();
            prop_assert!(child.is_continuation_of(&chain));
            prop_assert!(!chain.is_continuation_of(&child));
            let altered = IntentProvenance::from_turns(["Different ancestry".into(), "Next need".into()]).unwrap();
            prop_assert!(!altered.is_continuation_of(&chain));
            prop_assert_eq!(decoded.current(), turns.last().unwrap());
            prop_assert_eq!(decoded.original(), turns.first().unwrap());
        }
    }

    #[test]
    fn rejects_broken_parent_and_nul() {
        for wire in [
            r#"{"nodes":[]}"#,
            r#"{"nodes":[{"parent":0,"intent":"x"}]}"#,
            r#"{"nodes":[{"parent":null,"intent":"x\u0000"}]}"#,
        ] {
            assert!(serde_json::from_str::<IntentProvenance>(wire).is_err());
        }
    }

    #[test]
    fn consecutive_replays_do_not_change_selection_revision() {
        let initial = IntentProvenance::from_turns(["Inspect members".into()]).unwrap();
        let replay = initial.derived("Inspect members".into()).unwrap();
        assert_eq!(initial.selection_request(), replay.selection_request());
        let revised = replay.derived("Inspect owners".into()).unwrap();
        let restored = revised.derived("Inspect members".into()).unwrap();
        assert_ne!(initial.selection_request(), restored.selection_request());
    }
}
