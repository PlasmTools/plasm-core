//! DAG node types and compile-time state.

use super::prelude::*;

/// Plan JSON emission from a lowered DAG node (`node_to_json` match lives here).
pub(in crate::plasm_dag) trait PlanNodeEmitter {
    fn emit_plan_json(&self, node: &DagNode) -> Result<serde_json::Value, String>;
}

/// Γ binding contract derivation from a lowered node (`binding_contract_inner` match lives here).
pub(in crate::plasm_dag) trait BindingContractSource {
    fn program_binding_contract(
        &self,
        state: &CompileState<'_>,
        label: &str,
        node_expr: &str,
    ) -> ProgramBindingContract;
}

/// Program RHS surface text for DAG lowering (opaque session symbols preserved).
///
/// Symbol resolution happens in the parser and per-token field helpers — not via a textual
/// pre-expansion pass. Construct only via [`Self::new`] at [`compile_node_expr`].
#[derive(Debug, Clone)]
pub struct ExpandedProgramSurface(String);

impl ExpandedProgramSurface {
    pub fn new(session: &ExecuteSession, pipeline: &PromptPipelineConfig, fragment: &str) -> Self {
        Self(
            crate::plasm_plan_run::expand_program_surface_for_session_lower(
                session, pipeline, fragment,
            ),
        )
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for ExpandedProgramSurface {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub(in crate::plasm_dag) struct DagNode {
    pub(in crate::plasm_dag) id: String,
    pub(in crate::plasm_dag) expr: String,
    pub(in crate::plasm_dag) source: DagNodeSource,
    pub(in crate::plasm_dag) singleton: bool,
    pub(in crate::plasm_dag) page_size: Option<usize>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub(in crate::plasm_dag) enum DagNodeSource {
    Surface {
        parsed: plasm_core::expr_parser::ParsedExpr,
        kind: PlanNodeKind,
        qualified_entity: QualifiedEntityKey,
        effect_class: EffectClass,
        result_shape: crate::plasm_plan::ResultShape,
        uses_result: Vec<serde_json::Value>,
    },
    /// CGS relation traversal compiled from `bound_label.relation…` (substitutes bound anchor Plasm).
    RelationTraversal {
        source_label: String,
        /// Expanded Plasm used as the continuation anchor for nested `label.…` bindings.
        expanded_plasm: String,
        parsed: plasm_core::expr_parser::ParsedExpr,
        plan_relation: PlanRelationTraversal,
        qualified_entity: QualifiedEntityKey,
        effect_class: EffectClass,
        result_shape: crate::plasm_plan::ResultShape,
    },
    Data(PlanValue),
    Compute {
        source: String,
        op: ComputeOp,
        schema: SyntheticResultSchema,
        collection_alias: Option<OutputName>,
    },
    Derive {
        source: String,
        value: PlanValue,
        inputs: Vec<serde_json::Value>,
    },
    ForEach {
        source: String,
        parsed_template: serde_json::Value,
        display_expr: String,
        effect_kind: PlanNodeKind,
        qualified_entity: QualifiedEntityKey,
        uses_result: Vec<serde_json::Value>,
    },
}

pub(in crate::plasm_dag) struct CompileState<'a> {
    pub(in crate::plasm_dag) nodes: Vec<DagNode>,
    pub(in crate::plasm_dag) labels: BTreeMap<String, usize>,
    pub(in crate::plasm_dag) pipeline: &'a PromptPipelineConfig,
    pub(in crate::plasm_dag) cross_cache: Option<&'a SymbolMapCrossRequestCache>,
    pub(in crate::plasm_dag) sym_map: RefCell<Option<Arc<dyn plasm_core::SymbolSession>>>,
}

impl<'a> CompileState<'a> {
    pub(in crate::plasm_dag) fn new(
        pipeline: &'a PromptPipelineConfig,
        cross_cache: Option<&'a SymbolMapCrossRequestCache>,
    ) -> Self {
        Self {
            nodes: Vec::new(),
            labels: BTreeMap::new(),
            pipeline,
            cross_cache,
            sym_map: RefCell::new(None),
        }
    }

    pub(in crate::plasm_dag) fn sym_map_for(&self, session: &ExecuteSession) -> Arc<dyn plasm_core::SymbolSession> {
        if let Some(map) = self.sym_map.borrow().as_ref() {
            return Arc::clone(map);
        }
        let map = symbol_map_for_plasm_surface_parse(session, self.cross_cache);
        *self.sym_map.borrow_mut() = Some(Arc::clone(&map));
        map
    }

    pub(in crate::plasm_dag) fn insert(&mut self, node: DagNode) -> Result<(), String> {
        if self.labels.contains_key(&node.id) {
            if node.id.starts_with("return_") {
                return Err(program_duplicate_return_node_error());
            }
            return Err(format!(
                "Duplicate program label `{label}` — use a unique binding name.",
                label = node.id
            ));
        }
        self.labels.insert(node.id.clone(), self.nodes.len());
        self.nodes.push(node);
        Ok(())
    }

    pub(in crate::plasm_dag) fn get(&self, id: &str) -> Option<&DagNode> {
        self.labels.get(id).and_then(|i| self.nodes.get(*i))
    }

    pub(in crate::plasm_dag) fn contains(&self, id: &str) -> bool {
        self.labels.contains_key(id)
    }

    pub(in crate::plasm_dag) fn program_node_id_set(&self) -> BTreeSet<String> {
        self.labels.keys().cloned().collect()
    }
}
