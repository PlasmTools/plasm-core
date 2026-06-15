//! Plan/run response shaping.

use super::super::super::*;

pub(crate) fn respond_plan_run_live_result(
    kind: ExecResponseKind,
    result: &crate::plasm_plan_run::PlasmPlanRunResult,
    sess: &ExecuteSession,
) -> Response {
    let steps = &result.return_steps;
    if steps.is_empty() {
        if result.run_markdown.is_some() || result.run_plasm_meta.is_some() {
            let payload = serde_json::json!({
                "operation": true,
                "run_markdown": result.run_markdown,
                "_meta": result
                    .run_plasm_meta
                    .as_ref()
                    .map(|m| serde_json::Value::Object(m.clone())),
                "comp": result.comp,
            });
            if let ExecResponseKind::Table = kind {
                let md = result.run_markdown.as_deref().unwrap_or("");
                return (
                    StatusCode::OK,
                    [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                    md.to_string(),
                )
                    .into_response();
            }
            if matches!(kind, ExecResponseKind::Toon | ExecResponseKind::Ndjson) {
                return respond_plan_payload(kind, payload);
            }
            return (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/json; charset=utf-8")],
                Json(payload),
            )
                .into_response();
        }
        return problem_response(
            Problem::custom(
                ProblemStatus::INTERNAL_SERVER_ERROR,
                Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
            )
            .with_title("Internal Server Error")
            .with_detail("plan run returned no results"),
        );
    }
    if steps.len() == 1 {
        let step = &steps[0];
        let json_value = http_execute_results_value(&step.result);
        let cgs = step.cgs.as_deref().or(Some(sess.cgs.as_ref()));
        let omitted = reference_only_omitted_field_names(&step.result, cgs);
        let handles: Vec<RunArtifactHandle> = step.artifact.clone().into_iter().collect();
        let response_meta = tool_meta_from_handles(&handles, &omitted);
        return respond_execute_result(
            kind,
            json_value,
            &step.result,
            response_meta,
            cgs,
            step.artifact.as_ref(),
        );
    }
    let mut step_values = Vec::with_capacity(steps.len());
    let mut step_tables = if kind == ExecResponseKind::Table {
        Some(Vec::with_capacity(steps.len()))
    } else {
        None
    };
    let mut step_artifacts = Vec::new();
    let mut omitted_union: BTreeSet<String> = BTreeSet::new();
    for step in steps {
        if let Some(h) = &step.artifact {
            step_artifacts.push(h.clone());
        }
        let cgs = step.cgs.as_deref().or(Some(sess.cgs.as_ref()));
        step_values.push(http_execute_results_value(&step.result));
        if let Some(ref mut tabs) = step_tables {
            let (table, omitted, _) =
                format_result_with_cgs(&step.result, OutputFormat::Table, cgs);
            omitted_union.extend(omitted);
            tabs.push(table);
        } else {
            omitted_union.extend(reference_only_omitted_field_names(&step.result, cgs));
        }
    }
    let omitted_vec: Vec<String> = omitted_union.into_iter().collect();
    let steps_response_meta = tool_meta_from_handles(&step_artifacts, &omitted_vec);
    respond_staged_lines_execute_result(
        kind,
        step_values,
        step_tables,
        steps_response_meta,
        step_artifacts.last(),
    )
}
