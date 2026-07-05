use super::atoms::{FieldPath, OutputName};
use super::value::PlanPredicate;
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
    Project {
        fields: std::collections::BTreeMap<OutputName, FieldPath>,
    },
    Filter {
        predicates: Vec<PlanPredicate>,
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
