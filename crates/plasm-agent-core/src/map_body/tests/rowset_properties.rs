//! Generated Python programs over the abstract slice fixture. Expected values
//! come from fixture semantics; request counts bound redundant materialization.
use super::*;
use crate::plasm_compile::compile_python_program;
use proptest::prelude::*;

#[test]
fn typed_program_excerpts_recover_previewed_unicode_without_repeating_reads() {
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let unit = "é\n\t\"";
        let text = unit.repeat(1000);
        let literal = serde_json::to_string(unit).unwrap();
        let methods = (0..5).map(|index| format!(
            "    @compute\n    def part_{index}(self, row: Row) -> str:\n        return row.note[{}:{}]\n",
            index * 1000, (index + 1) * 1000,
        )).collect::<String>();
        let bindings = (0..5).map(|index| format!(
            "        part_{index} = full.map(lambda row: {{'text': self.part_{index}(row)}}, max_parents=1)\n"
        )).collect::<String>();
        let returns = (0..5)
            .map(|index| format!("part_{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let program = format!(
            "class Excerpts(Program):\n    @compute\n    def make_note(self, row: Row) -> str:\n        return row.title + {literal} * 1000\n{methods}    def build(self):\n        full = {item}.query().take(1).map(lambda row: {{'note': self.make_note(row)}}, max_parents=1)\n{bindings}        return full, {returns}\n"
        );
        let bundle = compile_python_program(&es, &program).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let run = execute(&es, &host, &bundle).await.unwrap();
        let source = &run.return_steps[0].result;
        let preview = crate::output::render_observation(source.as_ref(), None, 1, 1024);
        assert!(preview.fidelity.any_loss());
        assert!(preview.tsv.contains("(preview:"));
        let mut recovered = String::new();
        for step in &run.return_steps[1..] {
            let observation =
                crate::output::render_observation(step.result.as_ref(), None, 1, 4096);
            assert!(!observation.fidelity.any_loss(), "{}", observation.tsv);
            let mut lines = observation.tsv.lines();
            assert_eq!(lines.next(), Some("text"));
            recovered.push_str(&serde_json::from_str::<String>(lines.next().unwrap()).unwrap());
        }
        assert_eq!(recovered, format!("Title 0{text}"));
        assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
    });
}

#[test]
fn stored_computed_rows_can_be_delivered_in_full() {
    on_runtime(async {
        let (es, host, calls) = fixture(72);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let program = format!("class Copied(Program):\n    def build(self):\n        return {item}.query().map(lambda row: {{'copied_title': row.title}}, max_parents=256)\n");
        let bundle = compile_python_program(&es, &program).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let handle = run.return_steps[0].artifact.as_ref().unwrap();
        let payload = host
            .run_artifacts
            .get_payload(&es.prompt_hash, "map-body", handle.run_id)
            .await
            .unwrap();
        let doc: crate::run_artifacts::RunArtifactDocument =
            serde_json::from_slice(&payload.bytes).unwrap();
        assert_eq!(
            doc.row_materialization,
            plasm_runtime::execution::RowMaterialization::Computed
        );
        assert_eq!(
            doc.recorded_collection().unwrap().row_materialization(),
            plasm_runtime::execution::RowMaterialization::Computed
        );
        let session_ref = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let (out, _) = crate::terminal_result_format::hydrate_plan_run_from_artifact_formatted(
            &doc,
            &es,
            &doc.run_id,
            Some(session_ref),
        )
        .unwrap();
        assert!(out.markdown.contains("copied_title"));
        let mut delivered: Vec<_> = out.delivered_steps[0]
            .result
            .entities()
            .iter()
            .map(|entity| entity.payload_to_json().unwrap())
            .collect();
        let mut next = out.delivered_steps[0].result.paging_handle.clone();
        let trace = crate::trace_sink_emit::PlasmTraceContext {
            trace_id: uuid::Uuid::new_v4(),
            call_index: None,
            mcp_session_id: None,
            logical_session_id: None,
            logical_session_ref: Some(session_ref.into()),
        };
        while let Some(handle) = next {
            assert_eq!(
                crate::http_execute::resolve_paging_storage_handle(Some(&trace), &handle).unwrap(),
                handle
            );
            let bundle = crate::mcp_server::compile_page_continuation(&es, &handle, 0).unwrap();
            let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            let hooks = crate::trace_hub::PlanRunTraceHooks {
                trace: trace.clone(),
                sink: crate::trace_hub::McpPlasmTraceSink {
                    hub: host.trace_hub.clone(),
                    mcp_key: "artifact-paging".into(),
                    call_index: 1,
                },
                meta_index: None,
            };
            let page = run_plasm_comp(
                &es,
                &host,
                &es.prompt_hash,
                "map-body",
                &bundle,
                true,
                Some(hooks),
                None,
                Some(dry),
                None,
            )
            .await
            .unwrap();
            delivered.extend(result_rows(&page, 0));
            next = page.return_steps[0].result.paging_handle.clone();
        }
        let expected: Vec<_> = (0..72)
            .map(|i| json!({"copied_title":format!("Title {i}")}))
            .collect();
        assert_eq!(delivered, expected);
        assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
    });
}

fn result_rows(run: &crate::plasm_plan_run::PlasmPlanRunResult, index: usize) -> Vec<Value> {
    run.return_steps[index]
        .result
        .entities()
        .iter()
        .map(|entity| entity.payload_to_json().unwrap())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn rowset_properties_interleaved_compute_matches_reference_without_duplicate_reads(
        count in 0usize..9, take in 0usize..10, suffix in "[a-z]{0,12}", reverse in any::<bool>(),
    ) {
        on_runtime(async move {
            let (es, host, calls) = fixture(count);
            let item = es.teaching_exposure.as_ref().unwrap().to_symbol_map().entity_sym_for("fixture", "Item");
            let source = format!("class Mixed(Program):\n    @compute\n    def decorate(self, row: Row):\n        return row.title + '{suffix}'\n    def build(self):\n        raw = {item}.query()\n        selected = raw.take({take})\n        computed = selected.map(lambda row: {{'copied': row.title, 'decorated': self.decorate(row)}}, max_parents=256)\n        return {}\n", if reverse { "computed, raw" } else { "raw, computed" });
            let bundle = compile_python_program(&es, &source).await.unwrap();
            assert!(calls.lock().unwrap().is_empty(), "admission performs no reads");
            // Reloading the executable wire must preserve field contracts too.
            let mut artifact = bundle.into_artifact();
            artifact.comp = serde_json::from_slice(&serde_json::to_vec(&artifact.comp).unwrap()).unwrap();
            let bundle = PlasmCompBundle::new(artifact).unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            let raw = result_rows(&run, usize::from(reverse));
            assert_eq!(raw.len(), count);
            for (i, row) in raw.iter().enumerate() {
                assert_eq!(row["id"], format!("i{i}"));
                assert_eq!(row["title"], format!("Title {i}"));
            }
            let expected: Vec<_> = (0..count.min(take)).map(|i| json!({"copied":format!("Title {i}"), "decorated":format!("Title {i}{suffix}")})).collect();
            assert_eq!(result_rows(&run, usize::from(!reverse)), expected);
            assert_eq!(*calls.lock().unwrap(), vec!["/items"], "compute and both returns share one observation");
        });
    }

    #[test]
    fn rowset_properties_correlated_aliases_keep_parent_child_values_and_linear_io(
        count in 0usize..9, limit in 0usize..9,
    ) {
        on_runtime(async move {
            let (es, host, calls) = fixture(count);
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let item = symbols.entity_sym_for("fixture", "Item");
            let tags = symbols.ident_sym_relation_for("fixture", "Item", "tags");
            let source = format!("class Pairs(Program):\n    def build(self):\n        raw = {item}.query()\n        pairs = raw.take({limit}).flat_map(lambda parent: parent.{tags}.map(lambda child: {{'parent_title': parent.title, 'child_label': child.label}}, max_parents=256), max_parents=256)\n        return raw, pairs\n");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            let expected: Vec<_> = (0..count.min(limit)).flat_map(|i| (0..i).map(move |j| json!({"parent_title":format!("Title {i}"),"child_label":format!("L{j}")}))).collect();
            let delivered = result_rows(&run, 1);
            assert_eq!(delivered, expected[..delivered.len()], "context is an exact bounded prefix");
            let handle = run.return_steps[1].artifact.as_ref().unwrap();
            let payload = host.run_artifacts.get_payload(&es.prompt_hash, "map-body", handle.run_id).await.unwrap();
            let doc: crate::run_artifacts::RunArtifactDocument = serde_json::from_slice(&payload.bytes).unwrap();
            let stored: Vec<_> = doc.entities.iter().map(|entity| entity.payload_to_json().unwrap()).collect();
            assert_eq!(stored, expected, "context reduction must preserve every computed artifact row");
            assert_eq!(result_rows(&run, 0).len(), count);
            assert_eq!(calls.lock().unwrap().len(), 1 + count.min(limit));
            let result = &run.return_steps[1].result;
            let tsv = crate::output::render_observation(result.as_ref(), Some(&es.cgs), usize::MAX, usize::MAX).tsv;
            if !expected.is_empty() {
                assert!(tsv.lines().next().unwrap().contains("parent_title"));
                assert!(tsv.lines().next().unwrap().contains("child_label"));
                assert_eq!(tsv.lines().count(), delivered.len() + 1);
            }
        });
    }

    #[test]
    fn rowset_properties_parent_bound_fails_before_child_io(
        count in 0usize..10, bound in 1usize..9,
    ) {
        on_runtime(async move {
            let (es, host, calls) = fixture(count);
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let item = symbols.entity_sym_for("fixture", "Item");
            let tags = symbols.ident_sym_relation_for("fixture", "Item", "tags");
            let source = format!("class Bounded(Program):\n    def build(self):\n        return {item}.query().map(lambda parent: {{'copied': parent.title, 'tags': parent.{tags}}}, max_parents={bound})\n");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let result = execute(&es, &host, &bundle).await;
            if count > bound {
                assert!(result.unwrap_err().diagnostic().contains("budget exceeded"));
                assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
            } else {
                assert_eq!(result_rows(&result.unwrap(), 0).len(), count);
                assert_eq!(calls.lock().unwrap().len(), count + 1);
            }
        });
    }

    #[test]
    fn rowset_properties_copied_fields_keep_domain_through_nested_compute_input(
        field in prop::sample::select(vec!["id", "title", "state"]), depth in 0usize..5,
    ) {
        on_runtime(async move {
            let (es, _, calls) = fixture(0);
            let item = es.teaching_exposure.as_ref().unwrap().to_symbol_map().entity_sym_for("fixture", "Item");
            let mut value = format!("row.{field}");
            let mut access = "row.copied".to_string();
            for _ in 0..depth { value = format!("{{'nested': [{value}]}}"); access.push_str(".nested[0]"); }
            let source = format!("class Provenance(Program):\n    @compute\n    def consume(self, row: Row):\n        return {access}\n    def build(self):\n        copied = {item}.query().map(lambda row: {{'copied': {value}}}, max_parents=256)\n        computed = copied.map(lambda row: {{'again': self.consume(row)}}, max_parents=256)\n        return computed\n");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let wire = serde_json::to_value(&bundle.artifact().comp).unwrap();
            // Examine the declared compute input, not pretty-printed source or
            // a catalog lookup guessed from an output column name.
            fn contracts(v: &Value, found: &mut Vec<Value>) {
                if let Some(schema) = v.pointer("/compute/op/input_schema/fields") {
                    for f in schema.as_array().unwrap() { if f["name"] == "copied" { found.push(f["value_type"].clone()); } }
                }
                match v { Value::Object(o) => for child in o.values() { contracts(child, found); }, Value::Array(a) => for child in a { contracts(child, found); }, _ => {} }
            }
            let mut found = vec![];
            contracts(&wire, &mut found);
            assert!(!found.is_empty(), "copied field must reach a typed compute input");
            let expected_domain = match field { "id" => "item_id", other => other };
            let expected_contract = plasm_core::value_contract::ValueContract::from_domain(&es.cgs, "fixture", &plasm_core::ValueDomainKey::new(expected_domain).unwrap()).unwrap();
            for contract in found {
                let mut leaf = &contract;
                for _ in 0..depth { leaf = &leaf["shape"]["fields"]["nested"]["shape"]["element"]; }
                assert_eq!(*leaf, serde_json::to_value(&expected_contract).unwrap(), "preserve shape, nullability, catalogue owner and pinned domain");
            }
            let PlasmStepPayload::MapBody(body) = &bundle.artifact().comp.steps["computed"] else { panic!("computed rowset must retain its scoped output"); };
            let schema = crate::map_body_schema::output_schema(&es, body).unwrap();
            let again = schema.fields.iter().find(|f| f.name.as_str() == "again").unwrap().value_type.as_ref().unwrap();
            assert_eq!(again, &expected_contract, "compute output must preserve copied field provenance");
            assert!(calls.lock().unwrap().is_empty());
        });
    }
}
