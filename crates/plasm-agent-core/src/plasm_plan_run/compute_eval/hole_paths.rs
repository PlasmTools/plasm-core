//! IR template `node_input` hole indexing for staged cardinality rules.

use crate::plasm_plan::InputAlias;
use std::collections::BTreeMap;

/// Indexed `node_input` paths per input alias from a template expression tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NodeInputHoleIndex(BTreeMap<String, Vec<Vec<String>>>);

impl NodeInputHoleIndex {
    pub(crate) fn from_template_expr(expr: &serde_json::Value) -> Self {
        Self(collect_node_input_hole_paths(expr))
    }

    pub(crate) fn alias_paths(&self) -> &BTreeMap<String, Vec<Vec<String>>> {
        &self.0
    }

    pub(crate) fn needs_singleton_row(&self, alias: &InputAlias) -> bool {
        self.0
            .get(alias.as_str())
            .map(|paths| alias_node_input_needs_singleton_row(paths))
            .unwrap_or(true)
    }
}

fn collect_node_input_hole_paths(value: &serde_json::Value) -> BTreeMap<String, Vec<Vec<String>>> {
    let mut out = BTreeMap::new();
    collect_node_input_hole_paths_rec(value, &mut out);
    out
}

fn collect_node_input_hole_paths_rec(
    value: &serde_json::Value,
    out: &mut BTreeMap<String, Vec<Vec<String>>>,
) {
    if let Some(hole) = value.as_object().and_then(|obj| obj.get("__plasm_hole")) {
        if hole.get("kind").and_then(|v| v.as_str()) == Some("node_input") {
            let alias = hole
                .get("alias")
                .or_else(|| hole.get("node"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let path = hole
                .get("path")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            out.entry(alias).or_default().push(path);
        }
        return;
    }
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_node_input_hole_paths_rec(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                collect_node_input_hole_paths_rec(v, out);
            }
        }
        _ => {}
    }
}

fn alias_node_input_needs_singleton_row(paths: &[Vec<String>]) -> bool {
    paths.is_empty() || paths.iter().any(|path| path.is_empty())
}
