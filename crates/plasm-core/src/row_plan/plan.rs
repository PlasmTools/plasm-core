//! Fused row-compute IR. EquiJoin and Render are not pipeline nodes.

use crate::plasm_monad::payload::{AggregateSpec, FieldPath};
use crate::plasm_monad::OutputName;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

use super::collect::{CollectCardinality, CollectReason};
use super::error::{FrameSchemaError, FusionError};
use super::expr::{ProjectSpec, WithColumn};
use super::filter::RowFilter;
use super::ids::{FrameId, RowNodeId, SurfaceMeaningId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowPlan {
    source: FrameId,
    nodes: Pipeline,
    collect: CollectReason,
    cardinality: CollectCardinality,
    meaning: SurfaceMeaningId,
}

impl RowPlan {
    pub fn new(
        source: FrameId,
        nodes: Pipeline,
        collect: CollectReason,
        cardinality: CollectCardinality,
        meaning: SurfaceMeaningId,
    ) -> Result<Self, FrameSchemaError> {
        if nodes.is_empty() && !matches!(collect, CollectReason::Render { .. }) {
            // Identity collect (return ingested rows) is legal.
        }
        Ok(Self {
            source,
            nodes,
            collect,
            cardinality,
            meaning,
        })
    }

    #[must_use]
    pub fn source(&self) -> FrameId {
        self.source
    }

    #[must_use]
    pub fn nodes(&self) -> &Pipeline {
        &self.nodes
    }

    #[must_use]
    pub fn collect(&self) -> &CollectReason {
        &self.collect
    }

    #[must_use]
    pub fn cardinality(&self) -> CollectCardinality {
        self.cardinality
    }

    #[must_use]
    pub fn meaning(&self) -> SurfaceMeaningId {
        self.meaning
    }
}

/// Append-only pipeline. Written order is meaning; no swap/insert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Pipeline(Vec<(RowNodeId, PlanNode)>);

impl Pipeline {
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, id: RowNodeId, node: PlanNode) -> Result<(), FusionError> {
        self.0.push((id, node));
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(RowNodeId, PlanNode)> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNode {
    Filter(RowFilter),
    Sort {
        key: FieldPath,
        descending: bool,
    },
    Limit {
        count: NonZeroUsize,
    },
    Dedupe {
        keys: Vec<FieldPath>,
    },
    Distinct {
        keys: Vec<FieldPath>,
    },
    Project(ProjectSpec),
    With {
        columns: Vec<WithColumn>,
    },
    GroupBy {
        keys: Vec<FieldPath>,
        aggs: Vec<TypedAggregate>,
    },
    Aggregate {
        aggs: Vec<TypedAggregate>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedAggregate {
    Count {
        name: OutputName,
    },
    Numeric {
        name: OutputName,
        fn_: NumericAgg,
        field: FieldPath,
    },
    MoneySum {
        name: OutputName,
        field: FieldPath,
        currency: MoneyAggLaw,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumericAgg {
    Sum,
    Avg,
    Min,
    Max,
    First,
    Last,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoneyAggLaw {
    RequireUniform,
    CurrencyIsGroupKey,
}

impl TypedAggregate {
    pub fn from_spec(spec: &AggregateSpec) -> Result<Self, FrameSchemaError> {
        use crate::plasm_monad::AggregateFunction;
        match spec.function {
            AggregateFunction::Count => Ok(Self::Count {
                name: spec.name.clone(),
            }),
            AggregateFunction::Sum => {
                let field = spec
                    .field
                    .clone()
                    .ok_or(FrameSchemaError::UnknownColumn("sum field".into()))?;
                Ok(Self::Numeric {
                    name: spec.name.clone(),
                    fn_: NumericAgg::Sum,
                    field,
                })
            }
            AggregateFunction::Avg => map_numeric(spec, NumericAgg::Avg),
            AggregateFunction::Min => map_numeric(spec, NumericAgg::Min),
            AggregateFunction::Max => map_numeric(spec, NumericAgg::Max),
            AggregateFunction::First => map_numeric(spec, NumericAgg::First),
            AggregateFunction::Last => map_numeric(spec, NumericAgg::Last),
        }
    }

    #[must_use]
    pub fn as_money_sum(spec: &AggregateSpec) -> Option<Self> {
        use crate::plasm_monad::AggregateFunction;
        if spec.function != AggregateFunction::Sum {
            return None;
        }
        spec.field.clone().map(|field| Self::MoneySum {
            name: spec.name.clone(),
            field,
            currency: MoneyAggLaw::RequireUniform,
        })
    }
}

fn map_numeric(spec: &AggregateSpec, fn_: NumericAgg) -> Result<TypedAggregate, FrameSchemaError> {
    let field = spec
        .field
        .clone()
        .ok_or(FrameSchemaError::UnknownColumn(format!("{fn_:?} field")))?;
    Ok(TypedAggregate::Numeric {
        name: spec.name.clone(),
        fn_,
        field,
    })
}
