use crate::typed_literal::TypedComparisonValue;
use crate::{CompOp, Value};
use serde::{Deserialize, Serialize};

/// A typed predicate for filtering resources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Predicate {
    /// Always true
    #[serde(rename = "true")]
    True,

    /// Always false
    #[serde(rename = "false")]
    False,

    /// Field comparison
    #[serde(rename = "comparison")]
    Comparison {
        field: String,
        op: CompOp,
        value: TypedComparisonValue,
    },

    /// Logical AND
    #[serde(rename = "and")]
    And { args: Vec<Predicate> },

    /// Logical OR
    #[serde(rename = "or")]
    Or { args: Vec<Predicate> },

    /// Logical NOT
    #[serde(rename = "not")]
    Not {
        #[serde(rename = "arg")]
        predicate: Box<Predicate>,
    },

    /// Existential quantification over relations
    #[serde(rename = "exists_relation")]
    ExistsRelation {
        relation: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        predicate: Option<Box<Predicate>>,
    },
}

impl Predicate {
    /// Create a field comparison predicate.
    pub fn comparison(
        field: impl Into<String>,
        op: CompOp,
        value: impl Into<TypedComparisonValue>,
    ) -> Self {
        Predicate::Comparison {
            field: field.into(),
            op,
            value: value.into(),
        }
    }

    /// Create an equality comparison.
    pub fn eq(field: impl Into<String>, value: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::Eq, value)
    }

    /// Create a not-equal comparison.
    pub fn neq(field: impl Into<String>, value: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::Neq, value)
    }

    /// Create a greater-than comparison.
    pub fn gt(field: impl Into<String>, value: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::Gt, value)
    }

    /// Create a less-than comparison.
    pub fn lt(field: impl Into<String>, value: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::Lt, value)
    }

    /// Create a greater-than-or-equal comparison.
    pub fn gte(field: impl Into<String>, value: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::Gte, value)
    }

    /// Create a less-than-or-equal comparison.
    pub fn lte(field: impl Into<String>, value: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::Lte, value)
    }

    /// Create an 'in' comparison (value in array).
    pub fn in_(field: impl Into<String>, values: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::In, values)
    }

    /// Create a 'contains' comparison (array/string contains value).
    pub fn contains(field: impl Into<String>, value: impl Into<TypedComparisonValue>) -> Self {
        Self::comparison(field, CompOp::Contains, value)
    }

    /// Create an 'exists' check (field is not null).
    pub fn exists(field: impl Into<String>) -> Self {
        Self::comparison(
            field,
            CompOp::Exists,
            TypedComparisonValue::from(Value::Null),
        )
    }

    /// Create a logical AND of predicates.
    pub fn and(args: Vec<Predicate>) -> Self {
        Predicate::And { args }
    }

    /// Create a logical OR of predicates.
    pub fn or(args: Vec<Predicate>) -> Self {
        Predicate::Or { args }
    }

    /// Create a logical NOT of a predicate.
    pub fn negate(predicate: Predicate) -> Self {
        Predicate::Not {
            predicate: Box::new(predicate),
        }
    }

    /// Create an existential relation predicate.
    pub fn exists_relation(relation: impl Into<String>, predicate: Option<Predicate>) -> Self {
        Predicate::ExistsRelation {
            relation: relation.into(),
            predicate: predicate.map(Box::new),
        }
    }

    /// Check if this is a trivial True/False predicate.
    pub fn is_trivial(&self) -> bool {
        matches!(self, Predicate::True | Predicate::False)
    }

    /// Check if this predicate is always true.
    pub fn is_true(&self) -> bool {
        matches!(self, Predicate::True)
    }

    /// Check if this predicate is always false.
    pub fn is_false(&self) -> bool {
        matches!(self, Predicate::False)
    }

    /// Get the logical depth of this predicate (for complexity limits).
    pub fn depth(&self) -> usize {
        let mut max_depth = 0;
        let mut work = vec![(self, 1usize)];
        while let Some((predicate, depth)) = work.pop() {
            max_depth = max_depth.max(depth);
            match predicate {
                Predicate::And { args } | Predicate::Or { args } => {
                    work.extend(args.iter().map(|arg| (arg, depth + 1)));
                }
                Predicate::Not { predicate }
                | Predicate::ExistsRelation {
                    predicate: Some(predicate),
                    ..
                } => work.push((predicate, depth + 1)),
                Predicate::True
                | Predicate::False
                | Predicate::Comparison { .. }
                | Predicate::ExistsRelation {
                    predicate: None, ..
                } => {}
            }
        }
        max_depth
    }

    /// Collect all field names referenced in this predicate.
    pub fn referenced_fields(&self) -> Vec<String> {
        let mut fields = Vec::new();
        self.collect_fields(&mut fields);
        fields.sort();
        fields.dedup();
        fields
    }

    fn collect_fields(&self, fields: &mut Vec<String>) {
        let mut work = vec![self];
        while let Some(predicate) = work.pop() {
            match predicate {
                Predicate::Comparison { field, .. } => fields.push(field.clone()),
                Predicate::And { args } | Predicate::Or { args } => work.extend(args),
                Predicate::Not { predicate }
                | Predicate::ExistsRelation {
                    predicate: Some(predicate),
                    ..
                } => work.push(predicate),
                Predicate::True
                | Predicate::False
                | Predicate::ExistsRelation {
                    predicate: None, ..
                } => {}
            }
        }
    }

    /// Collect all relation names referenced in this predicate.
    pub fn referenced_relations(&self) -> Vec<String> {
        let mut relations = Vec::new();
        self.collect_relations(&mut relations);
        relations.sort();
        relations.dedup();
        relations
    }

    fn collect_relations(&self, relations: &mut Vec<String>) {
        let mut work = vec![self];
        while let Some(predicate) = work.pop() {
            match predicate {
                Predicate::ExistsRelation {
                    relation,
                    predicate,
                } => {
                    relations.push(relation.clone());
                    if let Some(predicate) = predicate {
                        work.push(predicate);
                    }
                }
                Predicate::And { args } | Predicate::Or { args } => work.extend(args),
                Predicate::Not { predicate } => work.push(predicate),
                Predicate::True | Predicate::False | Predicate::Comparison { .. } => {}
            }
        }
    }
}
