//! Python row handles in cards. Compute ports infer materialized inputs at call sites.

/// A DAG result shape. These aliases name graph handles, not Python values.
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

    pub fn compute_input_correction(self) -> &'static str {
        match self {
            Self::Rows => "`Rows` is a DAG handle; omit the annotation because @compute infers a Python collection from the call",
            Self::Singleton => "`Singleton` is a DAG handle; omit the annotation because @compute infers a Python row from the call",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PythonRowShape;

    #[test]
    fn card_shapes_are_dag_handles_with_inferred_compute_inputs() {
        for (shape, alias) in [
            (PythonRowShape::Rows, "Rows"),
            (PythonRowShape::Singleton, "Singleton"),
        ] {
            assert_eq!(PythonRowShape::from_card_alias(alias), Some(shape));
            assert!(shape.compute_input_correction().contains("infers"));
        }
    }
}
