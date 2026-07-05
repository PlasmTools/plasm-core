//! Row-to-text render lowering from postfix [`RenderTailParse`] into DAG compute nodes.

use std::collections::BTreeMap;

use plasm_core::expr_parser::RenderTailParse;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    ComputeOp, OutputName, SyntheticFieldSchema, SyntheticResultSchema, SyntheticValueKind,
};
use crate::plasm_plan_run::RenderColumns;
use crate::plasm_render_compile::{
    infer_render_column_tokens_from_template, parse_field_list_with_tokens,
    resolve_inferred_render_columns, resolve_render_collection_alias,
    validate_template_binding_labels,
};

use super::{
    compile_state_with_nodes, compile_surface_node, decompose_row_suffix_stream,
    infer_render_columns_for_node, lookup_dag_node, lower_suffix_stream,
    resolve_qualified_entity_for_dag_source, CompileState, DagNode, DagNodeSource, RowSuffix,
};

pub(super) fn plan_render_content_schema() -> Result<SyntheticResultSchema, String> {
    Ok(SyntheticResultSchema {
        entity: Some("PlanRender".to_string()),
        fields: vec![SyntheticFieldSchema {
            name: OutputName::new("content".to_string()).map_err(|e| e.to_string())?,
            value_kind: SyntheticValueKind::String,
            source: None,
        }],
    })
}

pub(super) fn compile_render_from_tail(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs_display: &str,
    tail: RenderTailParse,
) -> Result<Vec<DagNode>, String> {
    match tail {
        RenderTailParse::Explicit {
            source,
            fields,
            template,
        } => {
            let scratch = compile_state_with_nodes(state, &[]);
            let qe =
                resolve_qualified_entity_for_dag_source(&scratch, &[], source.trim().to_string());
            let field_pairs = parse_field_list_with_tokens(
                session,
                state.cross_cache,
                qe.as_ref(),
                fields.trim(),
            )?;
            let spec = RenderColumns::from_field_pairs(&field_pairs)?;
            compile_render_chain(
                session,
                state,
                id,
                rhs_display,
                &[source.trim().to_string()],
                Some(spec),
                template,
            )
        }
        RenderTailParse::Inferred { head, template } => compile_render_chain(
            session,
            state,
            id,
            rhs_display,
            &[head.trim().to_string()],
            None,
            template,
        ),
        RenderTailParse::CrossBinding { sources, template } => {
            for src in &sources {
                if !state.contains(src.trim()) {
                    return Err(format!(
                        "Plasm program `{id}`: cross-binding render source `{src}` is not in scope"
                    ));
                }
            }
            let labels: Vec<String> = sources.iter().map(|s| s.trim().to_string()).collect();
            compile_render_chain(session, state, id, rhs_display, &labels, None, template)
        }
    }
}

fn compile_render_chain(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs_display: &str,
    render_sources: &[String],
    explicit_render: Option<RenderColumns>,
    template: String,
) -> Result<Vec<DagNode>, String> {
    let head = render_sources
        .first()
        .map(String::as_str)
        .ok_or_else(|| format!("Plasm program `{id}`: render requires at least one source"))?;
    validate_template_binding_labels(&template, render_sources, id)?;

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
            vec![compile_surface_node(session, state, &tmp, head)?]
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

    let spec = if let Some(explicit) = explicit_render {
        explicit
    } else if let Some(raw_tokens) =
        infer_render_column_tokens_from_template(&template, head_core.trim())
    {
        let scratch = compile_state_with_nodes(state, &prefix);
        let qe = resolve_qualified_entity_for_dag_source(&scratch, &prefix, chain_tail_id.clone());
        resolve_inferred_render_columns(session, state.cross_cache, qe.as_ref(), &raw_tokens)?
    } else {
        let tail_node =
            lookup_dag_node(state, &prefix, chain_tail_id.as_str()).ok_or_else(|| {
                format!(
                    "Plasm program `{id}`: template column inference failed for `{chain_tail_id}`"
                )
            })?;
        let cols = infer_render_columns_for_node(session, state, &prefix, tail_node)
            .map_err(|e| format!("Plasm program `{id}`: cannot infer template columns: {e}"))?;
        RenderColumns::from_op_parts(cols, BTreeMap::new())
    };

    if spec.is_empty() {
        return Err(format!(
            "Plasm program `{id}`: row-to-text templates require at least one column; use `[field,...] <<TAG` after narrowing"
        ));
    }

    let (columns, column_aliases) = spec.into_op_parts();
    let collection_alias =
        resolve_render_collection_alias(head_core.trim(), &columns, |label| state.contains(label));

    let render_bindings: Vec<OutputName> = if render_sources.len() > 1 {
        render_sources
            .iter()
            .map(|label| OutputName::new(label.clone()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?
    } else if let Some(alias) = collection_alias.clone() {
        vec![alias]
    } else {
        vec![]
    };

    let mut render_node = DagNode {
        id: id.to_string(),
        expr: rhs_display.to_string(),
        singleton: true,
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
