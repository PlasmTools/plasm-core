//! Row-to-text render column projection: wire fields + teaching `p#` aliases for Minijinja `rows`.

use std::collections::BTreeMap;

use crate::plasm_plan::OutputName;

use super::compute_eval::value_at_dotted;

/// Wire columns plus optional teaching-surface aliases (`p#` → wire) for template bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderColumns {
    pub wires: Vec<OutputName>,
    pub aliases: BTreeMap<String, OutputName>,
}

impl RenderColumns {
    pub fn from_field_pairs(pairs: &[(String, String)]) -> Result<Self, String> {
        let mut wires = Vec::with_capacity(pairs.len());
        let mut aliases = BTreeMap::new();
        for (raw, wire) in pairs {
            let wire_name = OutputName::new(wire.clone())
                .map_err(|e| format!("invalid render column wire `{wire}`: {e}"))?;
            wires.push(wire_name.clone());
            if raw != wire {
                OutputName::new(raw.clone())
                    .map_err(|e| format!("invalid render column token `{raw}`: {e}"))?;
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
        row: &serde_json::Value,
        row_index: usize,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let mut obj = serde_json::Map::new();
        for column in &self.wires {
            obj.insert(
                column.as_str().to_string(),
                value_at_dotted(row, column.as_str())
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "Plan.render column {:?} did not resolve in source row {row_index}. {}",
                            column.as_str(),
                            self.access_hint()
                        )
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
            parts.push(format!("r.{}", column.as_str()));
        }
        for (alias, wire) in &self.aliases {
            parts.push(format!("r.{alias} (alias for r.{})", wire.as_str()));
        }
        format!("Valid row fields: {}", parts.join(", "))
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
        let row = serde_json::json!({ "name": "a" });
        let projected = cols.project_row(&row, 0).expect("project");
        assert_eq!(projected.get("name").and_then(|v| v.as_str()), Some("a"));
        assert_eq!(projected.get("p23").and_then(|v| v.as_str()), Some("a"));
    }
}
