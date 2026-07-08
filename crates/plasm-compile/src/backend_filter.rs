use plasm_core::Value;
use serde::{Deserialize, Serialize};

/// An intermediate representation for backend filters.
/// This sits between normalized predicates and raw backend requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BackendFilter {
    /// Always matches
    #[serde(rename = "true")]
    True,

    /// Never matches
    #[serde(rename = "false")]
    False,

    /// Field comparison filter
    #[serde(rename = "field")]
    Field {
        field: String,
        operator: BackendOp,
        value: Value,
    },

    /// Logical AND
    #[serde(rename = "and")]
    And { filters: Vec<BackendFilter> },

    /// Logical OR
    #[serde(rename = "or")]
    Or { filters: Vec<BackendFilter> },

    /// Logical NOT
    #[serde(rename = "not")]
    Not { filter: Box<BackendFilter> },

    /// Relation traversal filter
    #[serde(rename = "relation")]
    Relation {
        relation: String,
        filter: Option<Box<BackendFilter>>,
    },
}

/// Backend-specific operators.
/// These map to the actual filter operations supported by different backends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendOp {
    Equals,
    NotEquals,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
    In,
    Contains,
    Exists,
    StartsWith,
    EndsWith,
}

impl BackendFilter {
    /// Create a field filter.
    pub fn field(field: impl Into<String>, op: BackendOp, value: impl Into<Value>) -> Self {
        BackendFilter::Field {
            field: field.into(),
            operator: op,
            value: value.into(),
        }
    }

    /// Create an AND filter.
    pub fn and(filters: Vec<BackendFilter>) -> Self {
        BackendFilter::And { filters }
    }

    /// Create an OR filter.
    pub fn or(filters: Vec<BackendFilter>) -> Self {
        BackendFilter::Or { filters }
    }

    /// Create a NOT filter.
    pub fn negate(filter: BackendFilter) -> Self {
        BackendFilter::Not {
            filter: Box::new(filter),
        }
    }

    /// Create a relation filter.
    pub fn relation(relation: impl Into<String>, filter: Option<BackendFilter>) -> Self {
        BackendFilter::Relation {
            relation: relation.into(),
            filter: filter.map(Box::new),
        }
    }

    /// Check if this filter is trivial (always true/false).
    pub fn is_trivial(&self) -> bool {
        matches!(self, BackendFilter::True | BackendFilter::False)
    }

    /// Simplify this filter by eliminating trivial cases.
    pub fn simplify(self) -> Self {
        match self {
            BackendFilter::And { filters } => {
                let simplified: Vec<_> = filters.into_iter().map(|f| f.simplify()).collect();

                // If any filter is False, the entire AND is False
                if simplified.iter().any(|f| matches!(f, BackendFilter::False)) {
                    return BackendFilter::False;
                }

                // Remove True filters
                let non_trivial: Vec<_> = simplified
                    .into_iter()
                    .filter(|f| !matches!(f, BackendFilter::True))
                    .collect();

                match non_trivial.len() {
                    0 => BackendFilter::True,
                    1 => non_trivial.into_iter().next().unwrap(),
                    _ => BackendFilter::And {
                        filters: non_trivial,
                    },
                }
            }

            BackendFilter::Or { filters } => {
                let simplified: Vec<_> = filters.into_iter().map(|f| f.simplify()).collect();

                // If any filter is True, the entire OR is True
                if simplified.iter().any(|f| matches!(f, BackendFilter::True)) {
                    return BackendFilter::True;
                }

                // Remove False filters
                let non_trivial: Vec<_> = simplified
                    .into_iter()
                    .filter(|f| !matches!(f, BackendFilter::False))
                    .collect();

                match non_trivial.len() {
                    0 => BackendFilter::False,
                    1 => non_trivial.into_iter().next().unwrap(),
                    _ => BackendFilter::Or {
                        filters: non_trivial,
                    },
                }
            }

            BackendFilter::Not { filter } => {
                let simplified = filter.simplify();
                match simplified {
                    BackendFilter::True => BackendFilter::False,
                    BackendFilter::False => BackendFilter::True,
                    BackendFilter::Not { filter } => filter.simplify(),
                    _ => BackendFilter::Not {
                        filter: Box::new(simplified),
                    },
                }
            }

            BackendFilter::Relation { relation, filter } => BackendFilter::Relation {
                relation,
                filter: filter.map(|f| Box::new(f.simplify())),
            },

            // Base cases
            filter => filter,
        }
    }

    /// Convert this backend filter to a JSON representation suitable for HTTP requests.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Parse a backend filter from JSON.
    pub fn from_json(json: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(json.clone())
    }
}

impl From<plasm_core::CompOp> for BackendOp {
    fn from(op: plasm_core::CompOp) -> Self {
        match op {
            plasm_core::CompOp::Eq => BackendOp::Equals,
            plasm_core::CompOp::Neq => BackendOp::NotEquals,
            plasm_core::CompOp::Gt => BackendOp::GreaterThan,
            plasm_core::CompOp::Lt => BackendOp::LessThan,
            plasm_core::CompOp::Gte => BackendOp::GreaterThanOrEqual,
            plasm_core::CompOp::Lte => BackendOp::LessThanOrEqual,
            plasm_core::CompOp::In => BackendOp::In,
            plasm_core::CompOp::Contains => BackendOp::Contains,
            plasm_core::CompOp::Exists => BackendOp::Exists,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::Value;

    #[test]
    fn test_field_filter() {
        let filter = BackendFilter::field("name", BackendOp::Equals, "test");

        if let BackendFilter::Field {
            field,
            operator,
            value,
        } = filter
        {
            assert_eq!(field, "name");
            assert_eq!(operator, BackendOp::Equals);
            assert_eq!(value, Value::String("test".to_string()));
        } else {
            panic!("Expected field filter");
        }
    }

    #[test]
    fn test_simplify_and_with_false() {
        let filter = BackendFilter::and(vec![
            BackendFilter::field("a", BackendOp::Equals, 1),
            BackendFilter::False,
            BackendFilter::field("b", BackendOp::Equals, 2),
        ]);

        assert_eq!(filter.simplify(), BackendFilter::False);
    }

    #[test]
    fn test_simplify_and_with_true() {
        let filter = BackendFilter::and(vec![
            BackendFilter::field("a", BackendOp::Equals, 1),
            BackendFilter::True,
            BackendFilter::field("b", BackendOp::Equals, 2),
        ]);

        let simplified = filter.simplify();
        if let BackendFilter::And { filters } = simplified {
            assert_eq!(filters.len(), 2); // True should be removed
        } else {
            panic!("Expected And filter");
        }
    }

    #[test]
    fn test_simplify_single_and() {
        let filter = BackendFilter::and(vec![BackendFilter::field("a", BackendOp::Equals, 1)]);

        let simplified = filter.simplify();
        assert!(matches!(simplified, BackendFilter::Field { .. }));
    }

    #[test]
    fn test_double_not() {
        let filter = BackendFilter::negate(BackendFilter::negate(BackendFilter::field(
            "a",
            BackendOp::Equals,
            1,
        )));

        let simplified = filter.simplify();
        assert!(matches!(simplified, BackendFilter::Field { .. }));
    }

    #[test]
    fn test_json_round_trip() {
        let filter = BackendFilter::and(vec![
            BackendFilter::field("region", BackendOp::Equals, "EMEA"),
            BackendFilter::field("revenue", BackendOp::GreaterThan, 1000),
        ]);

        let json = filter.to_json();
        let parsed = BackendFilter::from_json(&json).unwrap();
        assert_eq!(filter, parsed);
    }
}
