//! Independent finite ordered-row algebra. No Plasm compiler/runtime imports.
use serde_json::Value;
use std::collections::BTreeMap;

pub type Row = BTreeMap<String, Value>;
pub type Shape = BTreeMap<String, Type>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Text,
    Integer,
    Nullable(Box<Type>),
    Record(Shape),
    ObservedRecord(Shape),
    Array(Box<Type>),
}

impl Type {
    pub fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Text => value.is_string(),
            Self::Integer => value.as_i64().is_some(),
            Self::Nullable(inner) => value.is_null() || inner.accepts(value),
            Self::ObservedRecord(fields) => value.as_object().is_some_and(|row| {
                row.iter()
                    .all(|(name, value)| fields.get(name).is_some_and(|ty| ty.accepts(value)))
            }),
            Self::Record(fields) => value.as_object().is_some_and(|row| {
                row.len() == fields.len()
                    && fields
                        .iter()
                        .all(|(key, ty)| row.get(key).is_some_and(|v| ty.accepts(v)))
            }),
            Self::Array(inner) => value
                .as_array()
                .is_some_and(|values| values.iter().all(|v| inner.accepts(v))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    Complete,
    Partial,
    Unknown,
}

impl Coverage {
    pub fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Partial, _) | (_, Self::Partial) => Self::Partial,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            _ => Self::Complete,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rows {
    pub shape: Shape,
    pub observed: bool,
    pub values: Vec<Row>,
    pub coverage: Coverage,
    pub continuation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownField(String),
    MissingField(String),
    WrongType(String),
    IncompatibleUnion,
    Incomplete,
    Bound,
    Unsupported(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Source,
    ObservedSource,
    Project(Box<Expr>, Vec<String>),
    KeepTitle(Box<Expr>, String),
    Take(Box<Expr>, usize),
    OrderId(Box<Expr>, bool),
    Union(Box<Expr>, Box<Expr>),
    /// Explicit synthetic row construction, not raw entity-value embedding.
    Record(Box<Expr>),
    /// One record per parent; children capture that parent's title.
    Nest {
        parents: Box<Expr>,
        children: Box<Expr>,
        bound: usize,
        capture: bool,
    },
}

fn field(shape: &Shape, name: &str) -> Result<Type, Error> {
    shape
        .get(name)
        .cloned()
        .ok_or_else(|| Error::UnknownField(name.into()))
}

impl Expr {
    pub fn shape(&self, source: &Shape) -> Result<Shape, Error> {
        match self {
            Self::Source | Self::ObservedSource => Ok(source.clone()),
            Self::Project(inner, keys) => {
                let shape = inner.shape(source)?;
                keys.iter()
                    .map(|key| Ok((key.clone(), field(&shape, key)?)))
                    .collect()
            }
            Self::KeepTitle(inner, _) | Self::OrderId(inner, _) => {
                let shape = inner.shape(source)?;
                let name = if matches!(self, Self::KeepTitle(..)) {
                    "title"
                } else {
                    "id"
                };
                if field(&shape, name)? != Type::Text {
                    return Err(Error::WrongType(name.into()));
                }
                Ok(shape)
            }
            Self::Take(inner, _) => inner.shape(source),
            Self::Union(left, right) => {
                let left = left.shape(source)?;
                if left != right.shape(source)? {
                    return Err(Error::IncompatibleUnion);
                }
                Ok(left)
            }
            Self::Record(inner) => {
                let shape = inner.shape(source)?;
                ["id", "title", "score"]
                    .into_iter()
                    .map(|key| Ok((key.into(), field(&shape, key)?)))
                    .collect()
            }
            Self::Nest {
                parents,
                children,
                capture,
                ..
            } => {
                let parent = parents.shape(source)?;
                let mut child = children.shape(source)?;
                if *capture {
                    child.insert("parent_title".into(), field(&parent, "title")?);
                }
                Ok(BTreeMap::from([
                    ("id".into(), field(&parent, "id")?),
                    (
                        "children".into(),
                        Type::Array(Box::new(if children.observed_output() {
                            Type::ObservedRecord(child)
                        } else {
                            Type::Record(child)
                        })),
                    ),
                ]))
            }
        }
    }

    pub fn eval(&self, source: &Rows) -> Result<Rows, Error> {
        let shape = self.shape(&source.shape)?;
        let mut result = match self {
            Self::Source => {
                let mut rows = source.clone();
                rows.observed = false;
                rows
            }
            Self::ObservedSource => {
                let mut rows = source.clone();
                rows.observed = true;
                rows
            }
            Self::Project(inner, keys) => {
                let mut rows = inner.eval(source)?;
                rows.values = rows
                    .values
                    .into_iter()
                    .map(|row| {
                        keys.iter()
                            .map(|key| {
                                Ok((
                                    key.clone(),
                                    row.get(key)
                                        .cloned()
                                        .ok_or_else(|| Error::MissingField(key.clone()))?,
                                ))
                            })
                            .collect()
                    })
                    .collect::<Result<_, Error>>()?;
                rows
            }
            Self::KeepTitle(inner, title) => {
                let mut rows = inner.eval(source)?;
                rows.values
                    .retain(|row| row.get("title").and_then(Value::as_str) == Some(title));
                rows
            }
            Self::Take(inner, limit) => {
                let mut rows = inner.eval(source)?;
                if rows.coverage != Coverage::Complete || rows.continuation {
                    return Err(Error::Unsupported(
                        "take over an unproven acquisition prefix",
                    ));
                }
                rows.values.truncate(*limit);
                rows
            }
            Self::OrderId(inner, descending) => {
                let mut rows = inner.eval(source)?;
                rows.values.sort_by(|a, b| {
                    let order = a["id"].as_str().cmp(&b["id"].as_str());
                    if *descending {
                        order.reverse()
                    } else {
                        order
                    }
                });
                rows
            }
            Self::Union(left, right) => {
                let mut a = left.eval(source)?;
                let b = right.eval(source)?;
                a.observed |= b.observed;
                a.coverage = a.coverage.meet(b.coverage);
                a.continuation |= b.continuation;
                let mut unique = Vec::new();
                for row in a.values.into_iter().chain(b.values) {
                    if !unique.contains(&row) {
                        unique.push(row);
                    }
                }
                a.values = unique;
                a
            }
            Self::Record(inner) => {
                let input = inner.eval(source)?;
                input.require_materializable()?;
                if input.values.len() > 8 {
                    return Err(Error::Bound);
                }
                let mut out = input;
                out.values = out
                    .values
                    .into_iter()
                    .map(|row| {
                        ["id", "title", "score"]
                            .into_iter()
                            .map(|key| {
                                Ok((
                                    key.into(),
                                    row.get(key)
                                        .cloned()
                                        .ok_or_else(|| Error::MissingField(key.into()))?,
                                ))
                            })
                            .collect()
                    })
                    .collect::<Result<_, Error>>()?;
                out
            }
            Self::Nest {
                parents,
                children,
                bound,
                capture,
            } => {
                let parents = parents.eval(source)?;
                parents.require_materializable()?;
                if parents.values.len() > *bound {
                    return Err(Error::Bound);
                }
                let mut values = Vec::new();
                for parent in &parents.values {
                    let children = children.eval(source)?;
                    children.require_materializable()?;
                    if children.values.len() > *bound {
                        return Err(Error::Bound);
                    }
                    let child_values: Vec<_> = children
                        .values
                        .into_iter()
                        .map(|mut row| {
                            if *capture {
                                row.insert("parent_title".into(), parent["title"].clone());
                            }
                            serde_json::to_value(row).expect("row serializes")
                        })
                        .collect();
                    values.push(BTreeMap::from([
                        ("id".into(), parent["id"].clone()),
                        ("children".into(), Value::Array(child_values)),
                    ]));
                }
                Rows {
                    shape: shape.clone(),
                    observed: false,
                    values,
                    coverage: parents.coverage,
                    continuation: false,
                }
            }
        };
        result.observed = self.observed_output();
        result.shape = shape;
        result.validate_shape()?;
        Ok(result)
    }

    pub fn observed_output(&self) -> bool {
        match self {
            Self::ObservedSource => true,
            Self::KeepTitle(inner, _) | Self::Take(inner, _) | Self::OrderId(inner, _) => {
                inner.observed_output()
            }
            Self::Union(a, b) => a.observed_output() || b.observed_output(),
            _ => false,
        }
    }
    pub fn size(&self) -> usize {
        1 + match self {
            Self::Source | Self::ObservedSource => 0,
            Self::Project(e, _)
            | Self::KeepTitle(e, _)
            | Self::Take(e, _)
            | Self::OrderId(e, _)
            | Self::Record(e) => e.size(),
            Self::Union(a, b)
            | Self::Nest {
                parents: a,
                children: b,
                ..
            } => a.size() + b.size(),
        }
    }
}

impl Rows {
    pub fn require_materializable(&self) -> Result<(), Error> {
        if self.coverage != Coverage::Complete || self.continuation {
            return Err(Error::Incomplete);
        }
        self.validate_shape()
    }

    pub fn validate_shape(&self) -> Result<(), Error> {
        let ty = if self.observed {
            Type::ObservedRecord(self.shape.clone())
        } else {
            Type::Record(self.shape.clone())
        };
        for row in &self.values {
            if !ty.accepts(&serde_json::to_value(row).expect("row serializes")) {
                for key in self.shape.keys() {
                    if !row.contains_key(key) {
                        return Err(Error::MissingField(key.clone()));
                    }
                }
                return Err(Error::WrongType("record".into()));
            }
        }
        Ok(())
    }
}
