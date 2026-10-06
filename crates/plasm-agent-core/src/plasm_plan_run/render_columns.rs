//! Per-row render column projection: wire fields + teaching `p#` aliases for Minijinja.

use std::collections::BTreeMap;

use crate::plasm_plan::OutputName;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderColumnsError {
    #[error("render column wire `{wire}` is invalid")]
    InvalidWire {
        wire: String,
        #[source]
        source: plasm_core::plasm_monad::PlanAtomError,
    },
    #[error("render column token `{token}` is invalid")]
    InvalidToken {
        token: String,
        #[source]
        source: plasm_core::plasm_monad::PlanAtomError,
    },
    #[error("render row {row_index} is missing column `{column}`; Available row fields: {}", .available_fields.join(", "))]
    ColumnMissing {
        column: String,
        row_index: usize,
        available_fields: Vec<String>,
    },
    #[error("render input at row {row_index} is not an object")]
    RowNotObject { row_index: usize },
}

use super::value_at_dotted;

/// Wire columns plus optional teaching-surface aliases (`p#` → wire) for template bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderColumns {
    pub wires: Vec<OutputName>,
    pub aliases: BTreeMap<String, OutputName>,
}

impl RenderColumns {
    pub fn from_field_pairs(pairs: &[(String, String)]) -> Result<Self, RenderColumnsError> {
        let mut wires = Vec::with_capacity(pairs.len());
        let mut aliases = BTreeMap::new();
        for (raw, wire) in pairs {
            let wire_name = OutputName::new(wire.clone()).map_err(|source| {
                RenderColumnsError::InvalidWire {
                    wire: wire.clone(),
                    source,
                }
            })?;
            wires.push(wire_name.clone());
            if raw != wire {
                OutputName::new(raw.clone()).map_err(|source| {
                    RenderColumnsError::InvalidToken {
                        token: raw.clone(),
                        source,
                    }
                })?;
                aliases.insert(raw.clone(), wire_name);
            }
        }
        Ok(Self { wires, aliases })
    }

    pub fn from_op_parts(wires: Vec<OutputName>, aliases: BTreeMap<String, OutputName>) -> Self {
        Self { wires, aliases }
    }

    pub fn into_op_parts(self) -> (Vec<OutputName>, BTreeMap<String, OutputName>) {
        (self.wires, self.aliases)
    }

    pub fn is_empty(&self) -> bool {
        self.wires.is_empty()
    }

    pub fn project_row(
        &self,
        row: &plasm_core::Value,
        row_index: usize,
    ) -> Result<indexmap::IndexMap<String, plasm_core::Value>, RenderColumnsError> {
        let mut obj = indexmap::IndexMap::new();
        for column in &self.wires {
            obj.insert(
                column.as_str().to_string(),
                value_at_dotted(row, column.as_str())
                    .cloned()
                    .ok_or_else(|| match row.as_object() {
                        Some(fields) => RenderColumnsError::ColumnMissing {
                            column: column.as_str().to_owned(),
                            row_index,
                            available_fields: fields.keys().cloned().collect(),
                        },
                        None => RenderColumnsError::RowNotObject { row_index },
                    })?,
            );
        }
        for (alias, wire) in &self.aliases {
            if let Some(v) = obj.get(wire.as_str()) {
                obj.insert(alias.clone(), v.clone());
            }
        }
        Ok(obj)
    }

    pub fn access_hint(&self) -> String {
        let mut parts = Vec::new();
        for column in &self.wires {
            parts.push(column.as_str().to_string());
        }
        for (alias, wire) in &self.aliases {
            parts.push(format!("{alias} (alias for {})", wire.as_str()));
        }
        if parts.is_empty() {
            "Use `{{ field }}` for the current row, or a named program binding.".to_string()
        } else {
            format!("Valid row fields: {}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_field_pairs_builds_aliases_and_rejects_invalid_wire() {
        let pairs = vec![("p1".into(), "name".into()), ("name".into(), "name".into())];
        let cols = RenderColumns::from_field_pairs(&pairs).expect("pairs");
        assert_eq!(cols.wires.len(), 2);
        assert_eq!(cols.aliases.len(), 1);
        assert!(cols.aliases.contains_key("p1"));
        assert!(RenderColumns::from_field_pairs(&[("p1".into(), "".into())]).is_err());
    }

    #[test]
    fn project_row_applies_aliases() {
        let mut aliases = BTreeMap::new();
        aliases.insert("p23".into(), OutputName::new("name").expect("name"));
        let cols =
            RenderColumns::from_op_parts(vec![OutputName::new("name").expect("name")], aliases);
        let row = crate::fixture_value!({ "name": "a" });
        let projected = cols.project_row(&row, 0).expect("project");
        assert_eq!(projected.get("name").and_then(|v| v.as_str()), Some("a"));
        assert_eq!(projected.get("p23").and_then(|v| v.as_str()), Some("a"));
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    #[test]
    fn missing_column_reports_actual_keys_without_values() {
        let cols =
            RenderColumns::from_field_pairs(&[("missing".into(), "missing".into())]).unwrap();
        let error = cols
            .project_row(&crate::fixture_value!({"present":"private-value"}), 0)
            .unwrap_err();
        assert!(matches!(
            &error,
            RenderColumnsError::ColumnMissing { column, row_index: 0, available_fields }
                if column == "missing" && available_fields == &["present"]
        ));
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("Available row fields: present"));
        assert!(!diagnostic.contains("Valid row fields: missing"));
        assert!(!diagnostic.contains("private-value"));
    }
}
