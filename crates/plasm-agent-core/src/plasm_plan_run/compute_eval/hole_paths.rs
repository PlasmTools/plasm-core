//! IR template `node_input` hole indexing for staged cardinality rules.

use crate::plasm_plan::InputAlias;
use std::collections::BTreeMap;

/// Indexed `node_input` paths per input alias from a template expression tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NodeInputHoleIndex(BTreeMap<String, Vec<Vec<String>>>);

impl NodeInputHoleIndex {
    pub(crate) fn from_template_expr(expr: &plasm_core::Expr) -> Self {
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

fn collect_node_input_hole_paths(expr: &plasm_core::Expr) -> BTreeMap<String, Vec<Vec<String>>> {
    let mut out: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    for reference in plasm_core::operand_binding::input_references(expr) {
        if let plasm_core::PlasmInputRef::NodeInput { node, path } = reference {
            out.entry(node).or_default().push(path);
        }
    }
    out
}

fn alias_node_input_needs_singleton_row(paths: &[Vec<String>]) -> bool {
    paths.is_empty() || paths.iter().any(|path| path.is_empty())
}
