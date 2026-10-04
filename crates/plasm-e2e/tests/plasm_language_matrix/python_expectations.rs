//! Independent expectations derived from the matrix fixture and rowset laws.
//! No source compiler is used as an oracle, and no record/bless mode is provided.
use plasm_agent::plasm_plan_run::PlasmPlanRunResult;
use serde_json::{json, Value};

pub(super) fn assert_supplemental(id: &str, run: &PlasmPlanRunResult) {
    let returns = match id {
        "fanout_captured_created" | "static_literal_for_effects" => 3,
        "root_build_statements"
        | "text_write_reuse"
        | "fanout_update"
        | "fanout_captured_receiver"
        | "fanout_captured_bounded"
        | "value_closure_nested_write"
        | "value_recursive_write_argument"
        | "value_recursive_bound_write_argument"
        | "update"
        | "parallel" => 2,
        _ => 1,
    };
    assert_eq!(
        run.return_steps.len(),
        returns,
        "{id}: explicit returns and retained effects"
    );
    let result = &run.return_steps[0].result;
    if matches!(id, "iterate_expression_zero" | "iterate_expression_exact") {
        let invocations: usize = result
            .operations
            .entries()
            .iter()
            .map(|ack| {
                assert_eq!(ack.failed, 0, "{id}: iteration mutation failure");
                assert_eq!(ack.completed, ack.logical_invocations);
                ack.logical_invocations
            })
            .sum();
        assert_eq!(
            invocations,
            if id == "iterate_expression_zero" {
                0
            } else {
                2
            },
            "{id}: stop before effects or after exactly two reobserved steps"
        );
    }
    if returns > 1 && id != "parallel" {
        let acknowledgements = &run.return_steps[1..];
        assert!(
            acknowledgements
                .iter()
                .all(|step| step.result.entities().is_empty()),
            "{id}: retained effects are acknowledgements"
        );
        let invocations = acknowledgements
            .iter()
            .flat_map(|step| step.result.operations.entries())
            .map(|ack| {
                assert_eq!(ack.failed, 0);
                assert_eq!(ack.completed, ack.logical_invocations);
                ack.logical_invocations
            })
            .sum::<usize>();
        assert_eq!(
            invocations,
            match id {
                "fanout_captured_created" => 3,
                "static_literal_for_effects" => 2,
                "fanout_update" | "fanout_captured_receiver" | "fanout_captured_bounded" => 2,
                _ => 1,
            }
        );
    }
    if id == "parallel" {
        for (step, expected) in run.return_steps.iter().zip(["i1", "i2"]) {
            assert_eq!(step.result.entities().len(), 1);
            assert_eq!(
                step.result.entities()[0].fields["id"].to_value(),
                plasm_core::Value::String(expected.into())
            );
        }
        return;
    }
    let rows: Vec<Value> = result
        .entities()
        .iter()
        .map(|row| {
            Value::Object(
                row.fields
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.to_string(),
                            serde_json::to_value(value.to_value()).unwrap(),
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    let titles = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
    let expected = match id {
        "static_literal_comprehension" | "static_literal_local_comprehension" => {
            json!([{"ids": ["i1", "i2"]}])
        }
        "compute_inferred_callsite_inputs" => {
            json!([{"titles": ["Alpha", "Beta"], "first": "Alpha"}])
        }
        "predicate_truth_string" | "predicate_truth_and" | "predicate_truth_iteration" => {
            json!([{ "id": "i1" }])
        }
        "predicate_truth_empty" | "predicate_truth_null" => json!([]),
        "predicate_truth_any" => json!([{ "value": true }]),
        "predicate_truth_refinement" => json!([{ "value": 11 }]),
        "predicate_null_test" => json!([{"missing":false,"present":true,"null":true}]),
        "predicate_null_filter" => json!([{"id":"i1"}]),
        "predicate_scalar_capture"
        | "predicate_any_relation"
        | "predicate_all_relation"
        | "predicate_projection_relation"
        | "predicate_map_relation" => json!([{"value": true}]),
        "predicate_filter_relation" | "predicate_iteration_relation" => json!([{"id":"i1"}]),
        "record_value_nested_compute" => json!([{"value":"Alpha2"}]),
        "record_value_scalar_compute" => json!([{"value":"i1constant"}]),
        "value_closure_literal" => json!([{"value": 43}]),
        "value_closure_column" => json!([{"value": 11}, {"value": 21}]),
        "value_closure_empty" => json!([]),
        "value_closure_collection" => json!([{"value": 30}]),
        "value_closure_array" => json!([{"value": 5}]),
        "value_closure_record" => json!([{"value": 7}]),
        "value_closure_empty_record" => json!([{"value": 7}]),
        "value_closure_format" => json!([{"value": "hello 5"}]),
        "value_closure_nested" => json!([{"value": 7}]),
        "value_closure_bound_format" => json!([{"value": "n=7"}]),
        "value_closure_nested_projection" => json!([{"n": 11}, {"n": 21}]),
        "value_closure_nested_read" => json!([{"title": "Alpha"}]),
        "value_closure_nested_write" => json!([{"title": "Changed"}]),
        "value_closure_nested_format" => json!([{"value": "n=7"}]),
        "value_closure_field_scalar" => json!([{"value": 13}]),
        "value_closure_bool_scalar" => json!([{"value": true}]),
        "value_closure_nullable_scalar" => json!([{"value": 0}]),
        "value_closure_empty_collection" => json!([{"value": 0}]),
        "value_closure_structural_singleton" => json!([{"value": 7}]),
        "value_closure_nullable_collection" => json!([{"value":30}]),
        "value_closure_scalar_method_format" => json!([{"value":"HELLO"}]),
        "value_recursive_read_argument" => json!([{"title": "Alpha"}]),
        "value_recursive_write_argument" | "value_recursive_bound_write_argument" => {
            json!([{"title": "Alpha!", "score": 12}])
        }
        "value_recursive_scoped_empty" => json!([{}, {}]),
        "value_recursive_membership" => json!([{"yes": true, "no": true}]),
        "value_recursive_bindings" => json!([{"n": 2, "flag": true, "empty": null, "xs": [2, 3]}]),
        "value_recursive_arithmetic" => json!([{"n": 12, "nested": [{"n": 22}], "negative": -10}]),
        "value_recursive_conditional" => json!([{"n": 10}]),
        "value_recursive_inline_field" => json!([{"title": "Alpha"}]),
        "value_recursive_length" => json!([{"n": 2}]),
        "value_recursive_empty_record" => json!([{}]),
        "value_recursive_projection" => {
            json!([{"value": {"n": 11, "xs": ["Alpha", 2]}}, {"value": {"n": 21, "xs": ["Beta", 2]}}])
        }
        "value_recursive_scoped_relation" => json!([{
            "title": "Alpha",
            "lines": [
                {"id": "l1", "item_id": "i1", "note": "line-a"},
                {"id": "l2", "item_id": "i1", "note": "line-b"}
            ]
        }]),
        "value_recursive_lazy_projection" => json!([{"value": 10}]),
        "value_recursive_root_scalar" => json!([{"value": 42}]),
        "record_value_singleton" => json!([{"id":"i1", "title":"Alpha"}]),
        "record_value_nested" => {
            json!([{"left":{"name":"Alpha"},"right":"Beta","values":[10,20,null]}])
        }
        "record_value_constants" => {
            json!([{"integer":9007199254740993_i64,"signed":i64::MIN,"ratio":1.25,"flag":false,"nil":null,"items":[{"name":"constant"}]}])
        }
        "record_value_collection" => json!([{"items":[{"title":"Alpha"},{"title":"Beta"}]}]),
        "record_value_empty_collection" => json!([{"items":[]}]),
        "record_value_scalar_binding" => json!([{"key":"i1","label":"constant"}]),
        "record_value_reuse" => json!([{"header":{"name":"Alpha"},"copied":"Alpha"}]),
        "record_value_projection" => json!([{"renamed":"Alpha","count":10}]),
        "record_value_compute" => json!([{"value":"Alpha"}]),
        "operand_recursive_literals" => {
            json!([{"text":"ab","integer":9007199254740993_i64,"signed":i64::MIN,"ratio":1.25,"flag":false,"nil":null,"nested":[{"text":"x","nums":[1,2]}]}])
        }
        "callback_iteration"
        | "static_literal_for_effects"
        | "callback_projection"
        | "callback_lexical_capture"
        | "callback_branch_predicate"
        | "callback_branch_record"
        | "callback_lexical_record"
        | "root_build_statements" => json!([{"id":"i1"}]),
        "scoped_flat_map_format" => json!([
            {"parent":"Alpha","label":"alpha:alpha"},
            {"parent":"Beta","label":"alpha:beta"}
        ]),
        "scoped_nested_records" => json!([
            {"id":"i1","children":[{"parent":"Alpha","child":"Alpha"}]},
            {"id":"i2","children":[{"parent":"Beta","child":"Alpha"}]}
        ]),
        "scoped_nested_effects" => {
            assert_eq!(
                result
                    .operations
                    .entries()
                    .iter()
                    .map(|a| a.completed as u64)
                    .sum::<u64>(),
                2
            );
            json!([{"id":"i1","children":[{"child":"i1","sent":{"completed":1,"failed":0}}]}, {"id":"i2","children":[{"child":"i1","sent":{"completed":1,"failed":0}}]}])
        }
        "assembly_row_compute" => json!(titles
            .iter()
            .enumerate()
            .map(|(i, t)| json!({"value":format!("i{}={}", i+1,t.to_lowercase())}))
            .collect::<Vec<_>>()),
        "assembly_singleton_capture" => json!((1..=5)
            .map(|i| json!({"id":format!("i{i}"),"heading":"Alpha"}))
            .collect::<Vec<_>>()),
        "type_projected_integer" => json!([{"value":"10"}]),
        "type_projected_enum" => json!([{"value":"draft"}]),
        "type_projected_temporal" => json!([{"value":"2026-01-01 00:00:00+00:00"}]),
        "text_conditional_membership" => json!([{"value":"i1:matched"}]),
        "text_row_duplicates" => json!([{"value":"Alpha"},{"value":"Beta"}]),
        "text_empty_aggregate" => json!([{"value":"count=0"}]),
        "text_synthetic_count" => json!([{"value":"count=2"}]),
        "text_synthetic_group" => json!([{"value":"Alpha: 1"},{"value":"Beta: 1"}]),
        "text_multiline_whitespace" => {
            json!([{"value":"Header \\n\n            preserved\nid=i1\n尾\n"}])
        }
        "text_write_reuse" => json!([{"title":"rendered i1"}]),
        "reduce_aggregate_functions" => {
            json!([{"s":75,"a":15.0,"lo":5,"hi":25,"f":"Alpha","l":"Epsilon"}])
        }
        "reduce_aggregate_empty" => json!([{"n":0,"total":0}]),
        "reduce_group_empty" => json!([]),
        "reduce_aggregate_alias" => json!([{"total":75}]),
        "reduce_distinct_alias" => {
            json!([{"handle":"alice","title":"Alpha"},{"handle":"bob","title":"Beta"},{"handle":"carol","title":"Epsilon"}])
        }
        "reduce_distinct_multi" => {
            assert_eq!(rows.len(), 5, "{id}");
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(row["id"], format!("i{}", i + 1));
                assert_eq!(row["title"], titles[i]);
            }
            return;
        }
        "singleton" => json!([{"value":"i1"}]),
        "empty" | "quoted" => json!([]),
        "grain" => json!([{"title":"Alpha"}]),
        "where" | "coerce" => {
            let expected: &[&str] = if id == "where" {
                &["i2", "i3", "i4"]
            } else {
                &["i1", "i2", "i3", "i4"]
            };
            assert_eq!(
                rows.iter()
                    .map(|r| r["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(rows
                .iter()
                .all(|r| r["score"].as_i64().unwrap() >= if id == "where" { 11 } else { 10 }));
            return;
        }
        "union_collapses_duplicates" => json!([{"owner":"alice"}]),
        "prefix_serial_limit" => json!([{"score":10}]),
        "prefix_union_single_relation" => json!([{"id":"t1"},{"id":"t2"}]),
        "prefix_zero_rows" => json!([]),
        "prefix_zero_count" => json!([{"n":0}]),
        "record_literal_index" => json!([{"value":11}]),
        "projection_alias_topk_source_contract" => json!([{"rank":5},{"rank":10}]),
        "alias_reproject_sort" => {
            json!([{"handle":"alice"},{"handle":"alice"},{"handle":"alice"},{"handle":"bob"},{"handle":"carol"}])
        }
        "alias_replace_column" => json!(titles
            .iter()
            .map(|t| json!({"owner":t}))
            .collect::<Vec<_>>()),
        "iterate_exact" | "iterate_expression_exact" | "iterate_expression_zero" => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["phase"], "done");
            return;
        }
        "fanout_update" | "fanout_captured_receiver" | "fanout_captured_bounded" => {
            json!([{"title":"Alpha","score":9,"owner":"alice"},{"title":"Beta","score":9,"owner":"alice"}])
        }
        "fanout_captured_created" => {
            json!([{"title":"new","score":9,"owner":"alice"},{"title":"Alpha","score":9,"owner":"alice"}])
        }
        "fanout_read" => json!([{"title":"Alpha"},{"title":"Beta"}]),
        "update" => json!([{"id":"i1","title":"Changed","score":8,"owner":"alice"}]),
        "create" | "field_input" | "scalar_input" => {
            assert_eq!(rows.len(), 1, "{id}");
            let row = &rows[0];
            assert_eq!(row["id"], "created-5");
            assert_eq!(
                row["title"],
                if id == "create" { "Matrix" } else { "Alpha" }
            );
            assert_eq!(row["score"], if id == "create" { 7 } else { 8 });
            assert_eq!(row["owner"], "alice");
            if id == "create" {
                assert_eq!(row["active"], true);
            }
            return;
        }
        "delete" | "broadcast" | "fanout_delete" | "fanout_empty" => {
            let count = match id {
                "fanout_delete" => 2,
                "fanout_empty" => 0,
                _ => 1,
            };
            if id == "broadcast" {
                assert_eq!(rows.len(), 1, "broadcast response receipt");
            } else {
                assert!(rows.is_empty(), "{id}: delete has no entity response");
            }
            assert_eq!(
                result
                    .operations
                    .entries()
                    .iter()
                    .map(|a| a.logical_invocations)
                    .sum::<usize>(),
                count
            );
            assert_eq!(
                result
                    .operations
                    .entries()
                    .iter()
                    .map(|a| a.completed)
                    .sum::<usize>(),
                count
            );
            assert!(result.operations.entries().iter().all(|a| a.failed == 0));
            return;
        }
        _ => panic!("missing independent expectation for {id}"),
    };
    assert_eq!(json!(rows), expected, "{id}: typed fields and order");
}

/// These fixture relations supply rows but no exhaustive materialization witness.
/// An embed miss delegates to an exhausted scoped query, which supplies its own
/// complete proof. All other successful cases use closed fixture reads/local
/// transforms or effect receipts.
pub(super) fn assert_coverage(id: &str, run: &PlasmPlanRunResult) {
    use plasm_runtime::ResultCoverage::{Complete, Unknown};
    let expected = match id {
        "cert_bind_limit1_continuation"
        | "prefix_union_single_relation"
        | "cert_relation_opaque_r_symbol"
        | "relation_one_chain"
        | "relation_relation_lines"
        | "relation_relation_tags_scoped"
        | "relation_bind_projection_then_relation"
        | "relation_bind_relation_hop_one_one"
        | "relation_bind_filter_continuation"
        | "relation_relation_many_from_plural_query"
        | "relation_relation_prefer_embed_hit"
        | "relation_ra4_apply_relation_monolith"
        | "relation_ra4_apply_relation_bind_cut"
        | "complete_binding_continuation"
        | "complete_bind_plural_relation_opaque_p"
        | "complete_homograph_lhs_coercion"
        | "cert_federated_relation_r"
        | "cert_federated_target_entry" => Unknown,
        _ => Complete,
    };
    for step in &run.return_steps {
        assert_eq!(
            step.result.coverage(),
            expected,
            "{id}: coverage at {:?}",
            step.name
        );
    }
}

/// Recovery witnesses assert typed authority independently of diagnostic prose.
pub(super) fn assert_failure(id: &str, failure: &plasm_runtime::ExecutionFailure) {
    if matches!(id, "repair_runtime_projection" | "repair_runtime_compute") {
        assert_eq!(failure.cause, plasm_runtime::FailureCause::Program);
        assert_eq!(
            failure.recovery,
            plasm_runtime::RecoveryDisposition::RepairProgram
        );
        assert_eq!(failure.code, "python_exception");
        assert!(failure.effects.is_empty());
        assert!(!failure.effects_unresolved);
    }
}
