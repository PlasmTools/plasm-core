//! Collect barriers — the only legal materialize points.

use crate::plasm_monad::{OutputName, StepId};
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectReason {
    ProgramReturn {
        step: StepId,
    },
    PageContinue {
        step: StepId,
        page: PageCursor,
    },
    InvokeArg {
        consumer: StepId,
        hole: String,
    },
    Render {
        step: StepId,
        spec: RenderCollectSpec,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderCollectSpec {
    pub columns: Vec<OutputName>,
    pub column_aliases: std::collections::BTreeMap<String, OutputName>,
    pub template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_alias: Option<OutputName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub render_bindings: Vec<OutputName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageCursor {
    pub token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectCardinality {
    List,
    Single,
    Page { size: PageSize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PageSize(NonZeroUsize);

impl PageSize {
    pub fn new(n: usize) -> Option<Self> {
        NonZeroUsize::new(n).map(Self)
    }

    #[must_use]
    pub fn get(self) -> usize {
        self.0.get()
    }
}
