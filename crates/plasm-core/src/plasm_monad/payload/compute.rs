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
    /// Pure bounded computation with recursive input and declared output contracts.
    Python {
        source: String,
        entry_id: String,
        entity: Option<String>,
        catalog_hash: String,
        /// Version 9 seals direct result types without a synthetic content field.
        contract_version: u32,
        language_profile: String,
        input_schema: Option<SyntheticResultSchema>,
        output_type: crate::value_contract::ValueContract,
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
    /// Merge complementary value branches. Exactly one total row is required;
    /// branch field types are joined, and no receiver authority is preserved.
    MergeBranches {
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
    /// Exact global consumers need the whole source. Row-local operators may
    /// transform observed occurrences, retaining their source's uncertainty.
    pub fn collection_demand(&self) -> crate::collection_codec::Demand {
        use crate::collection_codec::Demand;
        match self {
            Self::Python { .. }
            | Self::GroupBy { .. }
            | Self::Aggregate { .. }
            | Self::Sort { .. }
            | Self::MergeBranches { .. }
            | Self::Render { .. } => Demand::Whole,
            Self::Project { .. }
            | Self::Filter { .. }
            | Self::Limit { .. }
            | Self::DedupeBy { .. }
            | Self::With { .. }
            | Self::Union { .. } => Demand::Observed,
        }
    }

    /// Meet of existing row authority. A union requires evidence from both arms;
    /// equal row shapes alone are never evidence. Returns a borrowed witness.
    /// Callers provide catalog-qualified witnesses from the same pinned session.
    pub fn preserved_identity<'a, T: Eq>(
        &self,
        left: Option<&'a T>,
        right: Option<&'a T>,
    ) -> Option<&'a T> {
        if self.preserves_row_identity() {
            left
        } else if matches!(self, Self::Union { .. }) {
            left.filter(|identity| right == Some(*identity))
        } else {
            None
        }
    }

    /// RA-10: grain-preserving row algebra keeps parent Γ (entity, continuation).
    /// `Project` / `Filter` / `Sort` / `Limit` / `DedupeBy` / `With` inherit; grain-changing
    /// `Aggregate` / `GroupBy` / `Render` do not. Union requires two witnesses:
    /// use `preserved_identity` for its authority meet.
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
    /// Observation fields may be absent independently of value nullability.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub optional_fields: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub entity: Option<String>,
    pub fields: Vec<SyntheticFieldSchema>,
}

impl SyntheticResultSchema {
    /// Row-storage layout for a typed value. Records retain their fields; all other
    /// values use the ordinary scalar value column, without a language-level accessor.
    pub fn for_value(
        value: crate::value_contract::ValueContract,
    ) -> Result<SyntheticResultSchema, String> {
        use crate::value_contract::ValueShape;
        let (fields, optional_fields) = if !value.is_non_null_record() {
            (
                std::collections::BTreeMap::from([("value".into(), value)]),
                Default::default(),
            )
        } else {
            match value.shape {
                ValueShape::Record { fields } => (fields, Default::default()),
                ValueShape::ObservedRecord {
                    fields,
                    optional_fields,
                } => (fields, optional_fields),
                _ => unreachable!("non-null record shape"),
            }
        };
        Ok(SyntheticResultSchema {
            optional_fields,
            entity: None,
            fields: fields
                .into_iter()
                .map(|(name, value_type)| {
                    Ok(SyntheticFieldSchema {
                        name: OutputName::new(name)?,
                        value_kind: value_type.summary(),
                        value_type: Some(value_type),
                        source: None,
                    })
                })
                .collect::<Result<_, String>>()?,
        })
    }

    pub fn row_contract(&self) -> Result<crate::value_contract::ValueContract, String> {
        let fields = self
            .fields
            .iter()
            .map(|field| {
                Ok((
                    field.name.to_string(),
                    field
                        .value_type
                        .clone()
                        .ok_or_else(|| format!("untyped record field {}", field.name))?,
                ))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
        if !self
            .optional_fields
            .iter()
            .all(|name| fields.contains_key(name))
        {
            return Err("presence contract names an undeclared field".into());
        }
        Ok(crate::value_contract::ValueContract::record(
            fields,
            self.optional_fields.clone(),
        ))
    }
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
    fn global_consumers_reject_unproven_empty_input() {
        use crate::collection_codec::{
            CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
        };
        let codec = RecordingCodec::new();
        let input = codec
            .record(
                CollectionIdentity::for_untyped_observation(&"unproven empty page").unwrap(),
                Vec::<u64>::new(),
                Observation::UnprovenPage,
            )
            .unwrap();
        for op in [
            ComputeOp::Aggregate { aggregates: vec![] },
            ComputeOp::GroupBy {
                keys: vec![],
                aggregates: vec![],
            },
            ComputeOp::Sort {
                key: FieldPath::from_dotted("id").unwrap(),
                descending: false,
            },
            ComputeOp::Render {
                columns: vec![],
                template: String::new(),
                column_aliases: Default::default(),
                render_bindings: vec![],
            },
            ComputeOp::MergeBranches {
                other: OutputName::new("other").unwrap(),
            },
        ] {
            assert!(
                codec.materialize(&input, op.collection_demand()).is_err(),
                "{op:?}"
            );
        }
        assert!(codec
            .materialize(&input, ComputeOp::Limit { count: 0 }.collection_demand())
            .is_ok());
    }

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

#[cfg(test)]
mod identity_meet_tests {
    use super::*;

    #[test]
    fn union_identity_meet_is_commutative_associative_and_never_creates_evidence() {
        let union = ComputeOp::Union {
            other: OutputName::new("right").unwrap(),
        };
        // Qualified identity witnesses: equal entity names in different catalogs
        // are deliberately distinct. Absence is absorbing, not an identity.
        let a = ("catalog-a", "Item");
        let b = ("catalog-b", "Item");
        let c = ("catalog-a", "Other");
        let witnesses = [None, Some(&a), Some(&b), Some(&c)];
        for x in witnesses {
            assert_eq!(union.preserved_identity(x, x), x);
            for y in witnesses {
                let xy = union.preserved_identity(x, y);
                assert_eq!(xy, union.preserved_identity(y, x));
                assert_eq!(xy.is_some(), x.is_some() && x == y);
                if let Some(witness) = xy {
                    assert!(std::ptr::eq(witness, x.unwrap()));
                }
                for z in witnesses {
                    assert_eq!(
                        union.preserved_identity(xy, z),
                        union.preserved_identity(x, union.preserved_identity(y, z))
                    );
                }
            }
        }
        let branches = ComputeOp::MergeBranches {
            other: OutputName::new("right").unwrap(),
        };
        assert!(branches.preserved_identity(Some(&a), Some(&a)).is_none());
    }
}
