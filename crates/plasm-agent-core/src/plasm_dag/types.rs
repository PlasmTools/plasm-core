//! DAG node types and compile-time state.

use super::prelude::*;
use crate::plasm_plan::ResultShape;

/// Structural plan emission from a resolved DAG node.
pub(in crate::plasm_dag) trait PlanNodeEmitter {
    fn emit_plan_node(&self, node: &DagNode) -> Result<crate::plasm_plan::PlanNode, String>;
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
        /// A composed read view emits exactly one projection row on success.
        view_singleton: bool,
        parsed: plasm_core::expr_parser::ParsedExpr,
        kind: PlanNodeKind,
        qualified_entity: QualifiedEntityKey,
        effect_class: EffectClass,
        result_shape: crate::plasm_plan::ResultShape,
        uses_result: Vec<crate::plasm_plan::PlanResultUse>,
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
        inputs: Vec<crate::plasm_plan::PlanDataInput>,
    },
    /// PLP-1: checked static/bounded singleton field cell extract (`ℓ.wire` / `Get.wire`).
    /// Lowers to a typed derive node with a binding-field operand.
    ScalarExtract {
        source: String,
        wire: String,
    },
    ForEach {
        source: String,
        parsed_template: crate::plasm_plan::PlanExprTemplate,
        display_expr: String,
        effect_kind: PlanNodeKind,
        effect_class: EffectClass,
        result_shape: ResultShape,
        qualified_entity: QualifiedEntityKey,
        uses_result: Vec<crate::plasm_plan::PlanResultUse>,
    },
    /// PLP-8 / IT-2: state iterator (`iterate … step … until … take N`).
    IterateUntil {
        seed: String,
        parsed_step_template: crate::plasm_plan::PlanExprTemplate,
        step_display: String,
        effect_kind: PlanNodeKind,
        effect_class: EffectClass,
        result_shape: ResultShape,
        qualified_entity: QualifiedEntityKey,
        until_body: String,
        until_predicates: Vec<crate::plasm_plan::PlanPredicate>,
        take: u32,
        uses_result: Vec<crate::plasm_plan::PlanResultUse>,
    },
}

pub(in crate::plasm_dag) struct CompileState<'a> {
    /// Shared node bodies — scratch overlays Arc-clone this vec instead of deep-copying DAG payloads.
    pub(in crate::plasm_dag) nodes: Vec<Arc<DagNode>>,
    /// Copy-on-write label index (Arc::make_mut on insert / scratch extend).
    pub(in crate::plasm_dag) labels: Arc<BTreeMap<String, usize>>,
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
            labels: Arc::new(BTreeMap::new()),
            pipeline,
            cross_cache,
            sym_map: RefCell::new(None),
        }
    }

    pub(in crate::plasm_dag) fn sym_map_for(
        &self,
        session: &ExecuteSession,
    ) -> Arc<dyn plasm_core::SymbolSession> {
        if let Some(map) = self.sym_map.borrow().as_ref() {
            return Arc::clone(map);
        }
        let map = symbol_map_for_plasm_surface_parse(session, self.cross_cache);
        *self.sym_map.borrow_mut() = Some(Arc::clone(&map));
        map
    }

    pub(in crate::plasm_dag) fn insert(&mut self, node: DagNode) -> Result<(), String> {
        let labels = Arc::make_mut(&mut self.labels);
        if labels.contains_key(&node.id) {
            if node.id.starts_with("return_") {
                return Err(program_duplicate_return_node_error());
            }
            return Err(format!(
                "Duplicate program label `{label}` — use a unique binding name.",
                label = node.id
            ));
        }
        labels.insert(node.id.clone(), self.nodes.len());
        self.nodes.push(Arc::new(node));
        Ok(())
    }

    pub(in crate::plasm_dag) fn get(&self, id: &str) -> Option<&DagNode> {
        self.labels
            .get(id)
            .and_then(|i| self.nodes.get(*i).map(|n| n.as_ref()))
    }

    pub(in crate::plasm_dag) fn contains(&self, id: &str) -> bool {
        self.labels.contains_key(id)
    }

    pub(in crate::plasm_dag) fn program_node_id_set(&self) -> BTreeSet<String> {
        self.labels.keys().cloned().collect()
    }
}
