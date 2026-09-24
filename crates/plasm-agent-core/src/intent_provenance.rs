//! Lossless ancestry for discovery. Position is node identity within one chain.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

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
    type Error = anyhow::Error;
    fn try_from(wire: WireProvenance) -> Result<Self> {
        ensure!(
            !wire.nodes.is_empty(),
            "intent provenance must not be empty"
        );
        for (index, node) in wire.nodes.iter().enumerate() {
            ensure!(
                node.parent == index.checked_sub(1),
                "intent parent must be the preceding node"
            );
            ensure!(
                !node.intent.trim().is_empty() && !node.intent.contains('\0'),
                "intent must be nonempty Unicode text without NUL"
            );
        }
        Ok(Self { nodes: wire.nodes })
    }
}

impl IntentProvenance {
    pub fn turns(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().map(|node| node.intent.as_str())
    }

    pub fn derived(&self, intent: String) -> Result<Self> {
        Self::from_turns(self.turns().map(str::to_owned).chain([intent]))
    }

    pub fn is_continuation_of(&self, previous: &Self) -> bool {
        self.nodes.starts_with(&previous.nodes) && self.nodes.len() <= previous.nodes.len() + 1
    }

    pub fn from_turns(turns: impl IntoIterator<Item = String>) -> Result<Self> {
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
}
