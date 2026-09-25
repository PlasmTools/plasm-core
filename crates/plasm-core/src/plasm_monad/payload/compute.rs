use super::atoms::{FieldPath, OutputName};
use super::value::PlanPredicate;
use super::with_expr::WithColumn;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeTemplate {
    pub source: String,
    pub op: ComputeOp,
    pub schema: SyntheticResultSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    /// When set (row-to-text render), the projected list is also bound under this name in Minijinja.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "source_alias"
    )]
    pub collection_alias: Option<OutputName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComputeOp {
    /// Pure bounded rendering, checked against catalog or inferred row fields before execution.
    Python {
        source: String,
        entry_id: String,
        entity: String,
        catalog_hash: String,
        /// Version 3 seals recursive value contracts and collection/per-row cardinality.
        contract_version: u32,
        input_schema: Option<SyntheticResultSchema>,
        per_row: bool,
    },
    Project {
        fields: std::collections::BTreeMap<OutputName, FieldPath>,
    },
    Filter {
        predicates: crate::BooleanExpr<PlanPredicate>,
    },
    GroupBy {
        #[serde(alias = "key", deserialize_with = "deserialize_group_by_keys")]
        keys: Vec<FieldPath>,
        aggregates: Vec<AggregateSpec>,
    },
    Aggregate {
        aggregates: Vec<AggregateSpec>,
    },
    Sort {
        key: FieldPath,
        #[serde(default)]
        descending: bool,
    },
    Limit {
        count: usize,
    },
    DedupeBy {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<FieldPath>,
    },
    With {
        columns: Vec<WithColumn>,
    },
    /// RA-14 set-union; `source` is the left rowset, `other` is the right binding.
    Union {
        other: OutputName,
    },
    Render {
        columns: Vec<OutputName>,
        template: String,
        /// Teaching-surface tokens (e.g. `p23`) aliased onto wire column keys in Minijinja `rows`.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        column_aliases: std::collections::BTreeMap<String, OutputName>,
        /// In-scope binding labels merged into the Minijinja context (`label1,label2 <<TAG`, or the
        /// primary list alias for single-source templates).
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            alias = "cross_bindings"
        )]
        render_bindings: Vec<OutputName>,
    },
}

impl ComputeOp {
    /// RA-10: grain-preserving row algebra keeps parent Γ (entity, continuation).
    /// `Project` / `Filter` / `Sort` / `Limit` / `DedupeBy` / `With` inherit; grain-changing
    /// `Aggregate` / `GroupBy` / `Render` / `Union` do not.
    pub fn preserves_row_identity(&self) -> bool {
        matches!(
            self,
            Self::Project { .. }
                | Self::Filter { .. }
                | Self::Sort { .. }
                | Self::Limit { .. }
                | Self::DedupeBy { .. }
                | Self::With { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateSpec {
    pub name: OutputName,
    pub function: AggregateFunction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<FieldPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateFunction {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    First,
    Last,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticResultSchema {
    #[serde(default)]
    pub entity: Option<String>,
    pub fields: Vec<SyntheticFieldSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticFieldSchema {
    /// Authoritative recursive value type when known; value_kind is a display summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_type: Option<crate::value_contract::ValueContract>,
    pub name: OutputName,
    pub value_kind: SyntheticValueKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FieldPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticValueKind {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
    Money,
    Temporal,
    EntityRef,
    Duration,
    Unknown,
}

pub(crate) fn deserialize_group_by_keys<'de, D>(deserializer: D) -> Result<Vec<FieldPath>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::String(s) => FieldPath::from_dotted(s.as_str())
            .map(|k| vec![k])
            .map_err(D::Error::custom),
        serde_json::Value::Array(items) => {
            let mut keys = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        keys.push(FieldPath::from_dotted(s.as_str()).map_err(D::Error::custom)?);
                    }
                    serde_json::Value::Array(segs) => {
                        let parts: Vec<String> = segs
                            .iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect();
                        keys.push(FieldPath::new(parts).map_err(D::Error::custom)?);
                    }
                    other => {
                        return Err(D::Error::custom(format!(
                            "group_by key entry must be string or path array, got {other}"
                        )));
                    }
                }
            }
            if keys.is_empty() {
                return Err(D::Error::custom("group_by requires at least one key"));
            }
            Ok(keys)
        }
        other => Err(D::Error::custom(format!(
            "group_by keys must be a string or array, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_row_identity_table() {
        assert!(ComputeOp::Filter {
            predicates: Vec::new().into()
        }
        .preserves_row_identity());
        assert!(ComputeOp::Sort {
            key: FieldPath::from_dotted("title").expect("path"),
            descending: false,
        }
        .preserves_row_identity());
        assert!(ComputeOp::Limit { count: 1 }.preserves_row_identity());
        assert!(ComputeOp::DedupeBy { keys: Vec::new() }.preserves_row_identity());
        assert!(ComputeOp::With {
            columns: Vec::new()
        }
        .preserves_row_identity());
        assert!(ComputeOp::Project {
            fields: Default::default()
        }
        .preserves_row_identity());
        assert!(!ComputeOp::Aggregate {
            aggregates: Vec::new()
        }
        .preserves_row_identity());
        assert!(!ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("owner").expect("path")],
            aggregates: Vec::new(),
        }
        .preserves_row_identity());
        assert!(!ComputeOp::Render {
            columns: Vec::new(),
            template: String::new(),
            column_aliases: Default::default(),
            render_bindings: Vec::new(),
        }
        .preserves_row_identity());
        assert!(!ComputeOp::Union {
            other: OutputName::new("peers").expect("name"),
        }
        .preserves_row_identity());
    }
}
