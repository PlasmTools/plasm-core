//! Recursive, catalog-pinned materialized types. Domain constraints remain owned by CGS.
use crate::{FieldType, ValueDomainKey, CGS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainRef {
    pub entry_id: String,
    pub catalog_hash: String,
    pub value_ref: ValueDomainKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueContract {
    pub shape: ValueShape,
    pub domain: Option<DomainRef>,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum ValueShape {
    Scalar {
        field_type: FieldType,
    },
    Array {
        element: Box<ValueContract>,
    },
    Record {
        fields: BTreeMap<String, ValueContract>,
    },
    Null,
    /// No values inhabit an empty literal's element type.
    Never,
    Union {
        variants: Vec<ValueContract>,
    },
}

impl ValueContract {
    pub fn data_value(
        value: &crate::PlasmDataValue,
        resolve: &mut impl FnMut(&str, &[String]) -> Result<Self, String>,
    ) -> Result<Self, String> {
        use crate::PlasmDataValue as V;
        Ok(match value {
            V::BindingSymbol { binding, path } => resolve(binding, path)?,
            V::NodeSymbol { node, path, .. } => resolve(node, path)?,
            V::Literal { value } => Self::literal(value.value())?,
            V::Array { items } => Self::array(
                items
                    .iter()
                    .map(|v| Self::data_value(v, resolve))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            V::Object { fields } => Self {
                shape: ValueShape::Record {
                    fields: fields
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), Self::data_value(v, resolve)?)))
                        .collect::<Result<_, String>>()?,
                },
                domain: None,
                nullable: false,
            },
            V::Template { .. } => Self::scalar(FieldType::String),
            V::EntityRefKey { api, entity, .. } => Self::scalar(FieldType::EntityRef {
                entry_id: api.clone().into(),
                target: entity.clone().into(),
            }),
            V::Symbol { .. } => return Err("unresolved symbol has no materialized type".into()),
        })
    }

    fn array(elements: Vec<Self>) -> Self {
        let mut variants = Vec::new();
        for element in elements {
            if !variants.contains(&element) {
                variants.push(element);
            }
        }
        let element = if variants.len() == 1 {
            variants.remove(0)
        } else {
            Self {
                shape: if variants.is_empty() {
                    ValueShape::Never
                } else {
                    ValueShape::Union { variants }
                },
                domain: None,
                nullable: false,
            }
        };
        Self {
            shape: ValueShape::Array {
                element: Box::new(element),
            },
            domain: None,
            nullable: false,
        }
    }

    pub fn literal(value: &crate::Value) -> Result<Self, String> {
        use crate::Value as V;
        Ok(match value {
            V::Null => Self {
                shape: ValueShape::Null,
                domain: None,
                nullable: true,
            },
            V::Bool(_) => Self::scalar(FieldType::Boolean),
            V::Integer(_) => Self::scalar(FieldType::Integer),
            V::Float(_) => Self::scalar(FieldType::Number),
            V::String(_) => Self::scalar(FieldType::String),
            V::Money(_) => Self::scalar(FieldType::Money),
            V::Array(items) => {
                Self::array(items.iter().map(Self::literal).collect::<Result<_, _>>()?)
            }
            V::Object(fields) => Self {
                shape: ValueShape::Record {
                    fields: fields
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), Self::literal(v)?)))
                        .collect::<Result<_, String>>()?,
                },
                domain: None,
                nullable: false,
            },
            _ => return Err("unevaluated expression is not a materialized literal".into()),
        })
    }
    pub fn aggregate(function: crate::AggregateFunction, input: Option<&Self>) -> Self {
        use crate::AggregateFunction as A;
        let money_sum = function == A::Sum
            && input.is_some_and(|t| t.summary() == crate::SyntheticValueKind::Money);
        let mut result = Self::scalar(if function == A::Count {
            FieldType::Integer
        } else if money_sum {
            FieldType::Money
        } else {
            FieldType::Number
        });
        result.nullable = money_sum || !matches!(function, A::Count | A::Sum);
        result
    }
    /// Infer row expressions without executing them. Aliases retain their domain;
    /// computed scalars use the result type rather than impersonating an input domain.
    pub fn with_expr(
        expr: &crate::WithExpr,
        field: &mut impl FnMut(&crate::FieldPath) -> Result<Self, String>,
    ) -> Result<Self, String> {
        use crate::{WithExpr as E, WithLiteral as L};
        Ok(match expr {
            E::Field(path) => field(path)?,
            E::Literal(literal) => match literal {
                L::Null => Self {
                    shape: ValueShape::Null,
                    domain: None,
                    nullable: true,
                },
                L::Bool(_) => Self::scalar(FieldType::Boolean),
                L::Integer(_) => Self::scalar(FieldType::Integer),
                L::Number(_) => Self::scalar(FieldType::Number),
                L::String(_) => Self::scalar(FieldType::String),
            },
            E::Len { field: path } => {
                field(path)?;
                Self::scalar(FieldType::Integer)
            }
            E::Now => Self::scalar(FieldType::Date),
            E::Arith { op, lhs, rhs } => {
                let left = Self::with_expr(lhs, field)?;
                let right = Self::with_expr(rhs, field)?;
                use crate::ArithOp;
                let l = left.summary();
                let r = right.summary();
                use crate::SyntheticValueKind as K;
                let kind = if *op == ArithOp::Sub && l == K::Temporal && r == K::Temporal {
                    FieldType::Integer
                } else if *op == ArithOp::Add
                    && (l == K::String || r == K::String)
                    && !matches!(l, K::Temporal | K::Money)
                    && !matches!(r, K::Temporal | K::Money)
                {
                    FieldType::String
                } else if l == K::Money || r == K::Money {
                    FieldType::Money
                } else if *op == ArithOp::Div || l == K::Number || r == K::Number {
                    FieldType::Number
                } else if l == K::Integer && r == K::Integer {
                    FieldType::Integer
                } else {
                    return Err("unsupported arithmetic value contract".into());
                };
                let mut result = Self::scalar(kind);
                result.nullable = left.nullable || right.nullable;
                result
            }
            E::When { then, else_, .. } => {
                let mut left = Self::with_expr(then, field)?;
                let right = Self::with_expr(else_, field)?;
                if left.shape == ValueShape::Null {
                    let mut result = right;
                    result.nullable = true;
                    result
                } else if right.shape == ValueShape::Null {
                    left.nullable = true;
                    left
                } else if left.shape == right.shape {
                    left.nullable |= right.nullable;
                    if left.domain != right.domain {
                        left.domain = None;
                    }
                    left
                } else {
                    return Err("conditional branches have different materialized types".into());
                }
            }
        })
    }
    pub fn summary(&self) -> crate::SyntheticValueKind {
        use crate::SyntheticValueKind as K;
        match &self.shape {
            ValueShape::Never | ValueShape::Union { .. } => K::Unknown,
            ValueShape::Null => K::Null,
            ValueShape::Array { .. } => K::Array,
            ValueShape::Record { .. } => K::Object,
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => K::Boolean,
                FieldType::Integer => K::Integer,
                FieldType::Number => K::Number,
                FieldType::Money => K::Money,
                FieldType::EntityRef { .. } => K::EntityRef,
                FieldType::Date => K::Temporal,
                FieldType::Array | FieldType::MultiSelect => K::Array,
                FieldType::Json | FieldType::Blob => K::Object,
                _ => K::String,
            },
        }
    }
    pub fn scalar(field_type: FieldType) -> Self {
        Self {
            shape: ValueShape::Scalar { field_type },
            domain: None,
            nullable: false,
        }
    }

    pub fn from_domain(cgs: &CGS, entry: &str, key: &ValueDomainKey) -> Result<Self, String> {
        Self::resolve(cgs, entry, key, &mut Vec::new())
    }

    fn resolve(
        cgs: &CGS,
        entry: &str,
        key: &ValueDomainKey,
        stack: &mut Vec<ValueDomainKey>,
    ) -> Result<Self, String> {
        if stack.len() >= 64 || stack.contains(key) {
            return Err("recursive value domain exceeds materialized type bounds".into());
        }
        let value = cgs.values.get(key.as_str()).ok_or("unknown value domain")?;
        stack.push(key.clone());
        let shape = if value.field_type == FieldType::Array {
            let item = value
                .array_items
                .as_ref()
                .ok_or("array domain has no element contract")?;
            ValueShape::Array {
                element: Box::new(Self::resolve(cgs, entry, item.kind.registry_key(), stack)?),
            }
        } else {
            ValueShape::Scalar {
                field_type: value.field_type.clone(),
            }
        };
        stack.pop();
        Ok(Self {
            shape,
            domain: Some(DomainRef {
                entry_id: entry.into(),
                catalog_hash: cgs.catalog_cgs_hash_hex(),
                value_ref: key.clone(),
            }),
            nullable: false,
        })
    }

    /// Validate against the pinned registry; no coercion, stringification or numeric widening.
    pub fn validate(
        &self,
        value: &serde_json::Value,
        cgs: &CGS,
        entry: &str,
        path: &str,
    ) -> Result<(), String> {
        self.validate_at(value, cgs, entry, path, 0)
    }

    fn validate_at(
        &self,
        value: &serde_json::Value,
        cgs: &CGS,
        entry: &str,
        path: &str,
        depth: usize,
    ) -> Result<(), String> {
        if depth >= 64 {
            return Err(format!("{path}: nested value depth exceeded"));
        }
        let domain = if let Some(reference) = &self.domain {
            if reference.entry_id != entry || reference.catalog_hash != cgs.catalog_cgs_hash_hex() {
                return Err(format!("{path}: value domain catalog pin mismatch"));
            }
            Some(
                &cgs.values
                    .get(reference.value_ref.as_str())
                    .ok_or("unknown value domain")?
                    .domain,
            )
        } else {
            None
        };
        if value.is_null() && self.nullable {
            return Ok(());
        }
        let valid = match &self.shape {
            ValueShape::Never => false,
            ValueShape::Union { variants } => variants
                .iter()
                .any(|t| t.validate_at(value, cgs, entry, path, depth + 1).is_ok()),
            ValueShape::Null => value.is_null(),
            ValueShape::Array { element } => {
                let values = value
                    .as_array()
                    .ok_or_else(|| format!("{path}: expected array"))?;
                for (i, v) in values.iter().enumerate() {
                    element.validate_at(v, cgs, entry, &format!("{path}[{i}]"), depth + 1)?;
                }
                true
            }
            ValueShape::Record { fields } => {
                let values = value
                    .as_object()
                    .ok_or_else(|| format!("{path}: expected record"))?;
                for (name, field) in fields {
                    let v = values
                        .get(name)
                        .ok_or_else(|| format!("{path}.{name}: missing field"))?;
                    field.validate_at(v, cgs, entry, &format!("{path}.{name}"), depth + 1)?;
                }
                values.len() == fields.len()
            }
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => value.is_boolean(),
                FieldType::Integer => value.as_i64().is_some(),
                FieldType::Number => value.as_f64().is_some_and(f64::is_finite),
                FieldType::String | FieldType::Select | FieldType::Uuid | FieldType::DigitId => {
                    value.is_string()
                }
                FieldType::Date => value.is_string() || value.as_i64().is_some(),
                FieldType::MultiSelect => {
                    let items = value
                        .as_array()
                        .ok_or_else(|| format!("{path}: expected enum array"))?;
                    for (i, item) in items.iter().enumerate() {
                        let text = item
                            .as_str()
                            .ok_or_else(|| format!("{path}[{i}]: expected enum string"))?;
                        if let Some(d) = domain {
                            d.validate_string_value(text)
                                .map_err(|e| format!("{path}[{i}]: {e}"))?;
                            if !d
                                .enum_tokens()
                                .is_some_and(|tokens| tokens.iter().any(|t| t == text))
                            {
                                return Err(format!("{path}[{i}]: value is not in enum"));
                            }
                        }
                    }
                    true
                }
                FieldType::Money => {
                    let money: crate::money::MoneyValue = serde_json::from_value(value.clone())
                        .map_err(|e| format!("{path}: invalid money: {e}"))?;
                    if let Some(currency) = domain.and_then(|d| d.currency.as_deref()) {
                        if money.currency() != Some(currency) {
                            return Err(format!("{path}: money currency mismatch"));
                        }
                    }
                    true
                }
                FieldType::EntityRef { .. } => {
                    let native: crate::Value =
                        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
                    crate::entity_ref_value::EntityRefPayload::try_from_value(&native).is_ok()
                }
                FieldType::Json | FieldType::Blob => validate_json(value, depth + 1).is_ok(),
                FieldType::Array => false, // An array always requires its element contract.
            },
        };
        if !valid {
            return Err(format!("{path}: value violates materialized type"));
        }
        if let Some(domain) = domain {
            if let Some(items) = value.as_array() {
                if domain
                    .constraints
                    .min_length
                    .is_some_and(|n| items.len() < n)
                    || domain
                        .constraints
                        .max_length
                        .is_some_and(|n| items.len() > n)
                {
                    return Err(format!("{path}: array length violates domain constraints"));
                }
            }
            if let Some(text) = value.as_str() {
                domain
                    .validate_string_value(text)
                    .map_err(|e| format!("{path}: {e}"))?;
                use crate::value_domain::ProfileId;
                match domain.profile {
                    Some(ProfileId::Rfc3339) => {
                        chrono::DateTime::parse_from_rfc3339(text)
                            .map_err(|e| format!("{path}: invalid RFC3339: {e}"))?;
                    }
                    Some(ProfileId::Iso8601Date) => {
                        chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                            .map_err(|e| format!("{path}: invalid date: {e}"))?;
                    }
                    _ => {}
                }
            } else if let Some(number) = value.as_f64() {
                domain
                    .validate_number_value(number)
                    .map_err(|e| format!("{path}: {e}"))?;
            }
        }
        Ok(())
    }

    pub fn python_type(&self) -> String {
        let shape = match &self.shape {
            ValueShape::Never => "Never".into(),
            ValueShape::Union { variants } => variants
                .iter()
                .map(Self::python_type)
                .collect::<Vec<_>>()
                .join(" | "),
            ValueShape::Null => "None".into(),
            ValueShape::Array { element } => format!("list[{}]", element.python_type()),
            ValueShape::Record { .. } => "Record".into(),
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => "bool",
                FieldType::Integer => "int",
                FieldType::Number => "float",
                FieldType::MultiSelect => "list[str]",
                FieldType::Money => "Money",
                FieldType::EntityRef { .. } => "EntityRef",
                FieldType::Json => "JsonValue",
                FieldType::Blob => "Blob",
                FieldType::Date => "Temporal",
                FieldType::Array => "list",
                _ => "str",
            }
            .into(),
        };
        if self.nullable {
            format!("{shape} | None")
        } else {
            shape
        }
    }
}

fn validate_json(value: &serde_json::Value, depth: usize) -> Result<(), String> {
    if depth >= 64 {
        return Err("nested JSON depth exceeded".into());
    }
    match value {
        serde_json::Value::Array(values) => {
            for v in values {
                validate_json(v, depth + 1)?;
            }
        }
        serde_json::Value::Object(values) => {
            for v in values.values() {
                validate_json(v, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
