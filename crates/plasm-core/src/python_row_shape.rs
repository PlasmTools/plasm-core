//! Python row handles in cards and their materialized compute-input counterparts.

/// A DAG result shape. These aliases name graph handles, not Python values that
/// can be used as `@compute` parameter annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonRowShape {
    Rows,
    Singleton,
}

impl PythonRowShape {
    pub fn from_card_alias(alias: &str) -> Option<Self> {
        match alias {
            "Rows" => Some(Self::Rows),
            "Singleton" => Some(Self::Singleton),
            _ => None,
        }
    }

    pub fn card_annotation(self, entity: &str) -> String {
        match self {
            Self::Rows => format!("Rows[{entity}]"),
            Self::Singleton => format!("Singleton[{entity}]"),
        }
    }

    pub fn materialized_annotation(self, entity: &str) -> String {
        match self {
            Self::Rows => format!("list[Row[{entity}]]"),
            Self::Singleton => format!("Row[{entity}]"),
        }
    }

    pub fn compute_input_correction(self) -> &'static str {
        match self {
            Self::Rows => "`Rows` is a DAG result, not a materialized input type; use `list[Row]` or `list[Row[eN]]` for a collection",
            Self::Singleton => "`Singleton` is a DAG result, not a materialized input type; use `Row` or `Row[eN]` for a proven singleton",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PythonRowShape;

    #[test]
    fn card_shapes_have_distinct_materialized_compute_types() {
        for (shape, alias) in [
            (PythonRowShape::Rows, "Rows"),
            (PythonRowShape::Singleton, "Singleton"),
        ] {
            assert_eq!(PythonRowShape::from_card_alias(alias), Some(shape));
            assert_ne!(
                shape.card_annotation("e1"),
                shape.materialized_annotation("e1")
            );
        }
        assert_eq!(
            PythonRowShape::Rows.materialized_annotation("e1"),
            "list[Row[e1]]"
        );
        assert_eq!(
            PythonRowShape::Singleton.materialized_annotation("e1"),
            "Row[e1]"
        );
    }
}
