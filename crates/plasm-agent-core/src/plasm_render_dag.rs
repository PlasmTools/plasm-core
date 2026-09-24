//! Per-row render lowering from typed applicator data into DAG compute nodes (PLP-12).

use std::collections::BTreeSet;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    ComputeOp, OutputName, SyntheticFieldSchema, SyntheticResultSchema, SyntheticValueKind,
};
use crate::plasm_plan_run::RenderColumns;
use crate::plasm_render_compile::{
    classify_per_row_template_names, resolve_render_collection_alias,
};

use super::pipeline::compile_surface_nodes;
use super::row_suffix::{
    compile_state_with_nodes, decompose_row_suffix_stream, lower_suffix_stream,
};
// compile_state_with_nodes: Arc-share base nodes; only prefix payloads are cloned once.
use super::prelude::*;
use super::schema_validate::{infer_render_columns_for_node, lookup_dag_node};
use super::types::{CompileState, DagNode, DagNodeSource};

pub(in crate::plasm_dag) fn plan_render_content_schema() -> Result<SyntheticResultSchema, String> {
    Ok(SyntheticResultSchema {
        entity: Some("PlanRender".to_string()),
        fields: vec![SyntheticFieldSchema {
            name: OutputName::new("content".to_string()).map_err(|e| e.to_string())?,
            value_kind: SyntheticValueKind::String,
            source: None,
        }],
    })
}

pub(in crate::plasm_dag) fn compile_render_from_applicator(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs_display: &str,
    sources: &[String],
    template: String,
) -> Result<Vec<DagNode>, String> {
    if sources.len() != 1 {
        return Err(plasm_core::plp::plp12_per_row_apply(format!(
            "Plasm program `{id}`: comma-separated render sources are abolished — `=>` applies once per row. Whole-collection text uses a plain template (`report = <<TAG {{% for item in items %}}… TAG`)"
        )));
    }
    for src in sources {
        if !state.contains(src.trim()) {
            return Err(format!(
                "Plasm program `{id}`: render source `{src}` is not in scope"
            ));
        }
    }
    let labels: Vec<String> = sources.iter().map(|s| s.trim().to_string()).collect();
    compile_render_chain(session, state, id, rhs_display, &labels, template)
}

fn compile_render_chain(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs_display: &str,
    render_sources: &[String],
    template: String,
) -> Result<Vec<DagNode>, String> {
    let head = render_sources
        .first()
        .map(String::as_str)
        .ok_or_else(|| format!("Plasm program `{id}`: render requires at least one source"))?;

    let (head_core, suffixes) = decompose_row_suffix_stream(session, state, head)?;
    let tail_singleton = suffixes.iter().any(|s| matches!(s, RowSuffix::Singleton));
    let tail_page_size = suffixes.iter().find_map(|s| {
        if let RowSuffix::PageSize { n } = s {
            Some(*n as usize)
        } else {
            None
        }
    });

    let tmp = format!("__plasm_render_src_{id}");
    let prefix: Vec<DagNode> = if suffixes.is_empty() {
        if state.contains(head_core.trim()) {
            vec![]
        } else {
            compile_surface_nodes(session, state, &tmp, head)?
        }
    } else {
        lower_suffix_stream(session, state, &tmp, head, &head_core, suffixes, None)
            .map_err(|e| format!("Plasm program `{id}`: {e}"))?
    };

    let chain_tail_id: String = if prefix.is_empty() {
        head_core.trim().to_string()
    } else {
        prefix
            .last()
            .map(|n| n.id.clone())
            .ok_or_else(|| format!("Plasm program `{id}`: empty render chain"))?
    };

    let scratch = compile_state_with_nodes(state, &prefix);
    let tail_node =
        lookup_dag_node(&scratch, &prefix, chain_tail_id.as_str()).ok_or_else(|| {
            format!("Plasm program `{id}`: render source `{chain_tail_id}` is unknown")
        })?;
    plasm_core::validate_interpolation_syntax(&template, |e| format!("Plasm program `{id}`: {e}"))?;
    let source_field_names: BTreeSet<String> =
        match infer_render_columns_for_node(session, &scratch, &prefix, tail_node) {
            Ok(cols) => cols.into_iter().map(|n| n.as_str().to_string()).collect(),
            Err(e) if plasm_core::contains_minijinja_markers(&template) => {
                return Err(format!(
                    "Plasm program `{id}`: cannot infer render source fields: {e}"
                ));
            }
            Err(_) => BTreeSet::new(),
        };
    let mut binding_names = scratch.program_node_id_set();
    binding_names.insert(head_core.trim().to_string());
    binding_names.insert(chain_tail_id.clone());
    for node in &prefix {
        binding_names.insert(node.id.clone());
    }

    let (row_tokens, binding_labels) =
        classify_per_row_template_names(&template, &source_field_names, &binding_names, id)?;

    // These names have already been resolved against the actual source schema.
    // Re-resolving them against an ancestral entity loses projected/derived names.
    let spec = RenderColumns::from_field_pairs(
        &row_tokens
            .into_iter()
            .map(|name| (name.clone(), name))
            .collect::<Vec<_>>(),
    )?;

    let (columns, column_aliases) = spec.into_op_parts();
    let collection_alias =
        resolve_render_collection_alias(head_core.trim(), &columns, |label| state.contains(label));

    let render_bindings: Vec<OutputName> = binding_labels
        .into_iter()
        .map(|label| OutputName::new(label).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;

    let source_singleton = tail_node.singleton || tail_singleton;

    let mut render_node = DagNode {
        id: id.to_string(),
        expr: rhs_display.to_string(),
        singleton: source_singleton,
        page_size: if prefix.is_empty() {
            tail_page_size
        } else {
            None
        },
        source: DagNodeSource::Compute {
            source: chain_tail_id,
            op: ComputeOp::Render {
                columns,
                template,
                column_aliases,
                render_bindings,
            },
            schema: plan_render_content_schema()?,
            collection_alias,
        },
    };
    render_node.singleton |= tail_singleton;

    let mut out = prefix;
    out.push(render_node);
    Ok(out)
}
