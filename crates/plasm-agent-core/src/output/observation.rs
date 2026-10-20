//! Source-independent row observation. Storage handles and transport profiles are
//! intentionally absent: they cannot change which evidence the renderer exposes.
use super::InBandSummaryReport;
use plasm_runtime::{CachedEntity, ExecutionResult, ResultCoverage};

pub(crate) trait RowObservation {
    fn rows(&self) -> impl ExactSizeIterator<Item = &CachedEntity>;
    fn coverage(&self) -> ResultCoverage;
}

impl RowObservation for ExecutionResult {
    fn rows(&self) -> impl ExactSizeIterator<Item = &CachedEntity> {
        self.entities().iter()
    }
    fn coverage(&self) -> ResultCoverage {
        self.coverage()
    }
}

pub(crate) struct RenderedObservation {
    pub tsv: String,
    pub shown: usize,
    pub coverage: ResultCoverage,
    pub fidelity: InBandSummaryReport,
}

/// All row sources use this renderer. At least one row survives any display budget;
/// field previews are explicitly marked and cannot be mistaken for exact values.
pub(crate) fn render_observation(
    source: &impl RowObservation,
    cgs: Option<&plasm_core::CGS>,
    max_rows: usize,
    target_bytes: usize,
) -> RenderedObservation {
    let mut shown = source.rows().len().min(max_rows.max(1));
    loop {
        let (tsv, fidelity) = super::summary::format_result_tsv_observation(
            source,
            cgs,
            shown,
            (shown <= 1).then_some(target_bytes),
        );
        if tsv.len() <= target_bytes || shown <= 1 {
            return RenderedObservation {
                tsv,
                shown,
                coverage: source.coverage(),
                fidelity,
            };
        }
        // Prefer fewer complete rows to many rows with abbreviated fields.
        shown = (shown / 2).max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn source_observation_contract_precedes_every_tsv_cell() {
        use plasm_core::row_contract::{RowCodec, RowFieldContract};
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let wire = plasm_core::ValueRow::try_from(plasm_core::json_value_to_plasm_value(&serde_json::json!({
            "id":"a","title":"domain","_ref":{"kind":"simple","entity":"LangItem","id":"a"},
            "wire_embed":{"email":"identity","_ref":{"kind":"simple","entity":"User","id":"identity"}},
            "computed_alias":42
        }))).unwrap();
        let codec = RowCodec::new(Some(&cgs));
        let semantic = codec
            .decode_values("LangItem", &wire, RowFieldContract::Observation)
            .unwrap();
        let observation = DetachedRows(vec![CachedEntity::from_row(
            &semantic,
            0,
            plasm_runtime::EntityCompleteness::Complete,
        )]);
        let text = render_observation(&observation, Some(&cgs), 10, 4000).tsv;
        assert!(text.contains("domain"));
        assert!(!text.contains("wire_embed"));
        assert!(!text.contains("\"_ref\""));
        let projection = codec
            .decode_values("LangItem", &wire, RowFieldContract::Projection)
            .unwrap();
        let projected = DetachedRows(vec![CachedEntity::from_row(
            &projection,
            0,
            plasm_runtime::EntityCompleteness::Complete,
        )]);
        assert!(render_observation(&projected, Some(&cgs), 10, 4000)
            .tsv
            .contains("computed_alias"));
    }
    struct DetachedRows(Vec<CachedEntity>);
    #[test]
    fn tsv_nested_relation_cells_use_public_projection() {
        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let mut row = CachedEntity::new(plasm_core::Ref::new("LangItem", "a"), 0);
        row.fields.insert("summary".into(), plasm_core::TypedFieldValue::from(
            plasm_core::json_value_to_plasm_value(&serde_json::json!({
                "id":"s","headline":"visible", "_ref":{"kind":"simple","entity":"LangSummary","id":"s"},
                "detail":{"id":"d","body":"visible detail","_version":1}
            })),
        ));
        let text = render_observation(&DetachedRows(vec![row]), Some(&cgs), 10, 4000).tsv;
        assert!(text.contains("visible detail"), "{text}");
        assert!(!text.contains("_ref"), "{text}");
        assert!(!text.contains("_version"), "{text}");
    }

    #[test]
    fn invalid_observation_preserves_typed_cause_and_truthful_marker() {
        use crate::output::in_band_fidelity::SummaryFidelityLoss;
        use plasm_core::value_contract::ValueContractError;

        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        assert!(cgs.entities.shift_remove("LangSummary").is_some());
        let mut row = CachedEntity::new(plasm_core::Ref::new("LangItem", "a"), 0);
        row.fields.insert(
            "summary".into(),
            plasm_core::json_value_to_plasm_value(
                &serde_json::json!({"id": "s", "headline": "unprojected"}),
            )
            .into(),
        );
        let rows = DetachedRows(vec![row]);
        let rendered = render_observation(&rows, Some(&cgs), 10, 4000);
        let step =
            crate::test_support::execution_fixtures::synthetic_published_result_step(1, None);
        let mut result = step.result.as_ref().clone();
        result.collection =
            crate::test_support::execution_fixtures::collection(rows.0, ResultCoverage::Unknown);
        let (full_tsv, _, full_report) =
            super::super::summary::format_result_tsv_with_cgs(&result, Some(&cgs), None);
        for (text, report) in [(rendered.tsv, rendered.fidelity), (full_tsv, full_report)] {
            assert!(
                text.contains("(invalid observation: public contract mismatch)"),
                "{text}"
            );
            assert!(
                !text.contains("non-inline") && !text.contains("in artifact"),
                "{text}"
            );
            assert!(!text.contains("unprojected"), "{text}");
            let expected = ValueContractError::ReferenceTargetMissing {
                target: "LangSummary".into(),
            };
            assert_eq!(
                report.projection_errors_for("summary"),
                std::slice::from_ref(&expected)
            );
            let mut merged = InBandSummaryReport::default();
            merged.record("summary", SummaryFidelityLoss::ReferenceOnlyWithheld);
            merged.merge_from(&report);
            merged.record("summary", SummaryFidelityLoss::SchemaLossyCap);
            assert_eq!(
                merged.loss_for("summary"),
                Some(SummaryFidelityLoss::InvalidObservation)
            );
            assert_eq!(merged.projection_errors_for("summary"), &[expected]);
        }
    }
    impl RowObservation for DetachedRows {
        fn rows(&self) -> impl ExactSizeIterator<Item = &CachedEntity> {
            self.0.iter()
        }
        fn coverage(&self) -> ResultCoverage {
            ResultCoverage::Unknown
        }
    }

    fn notes(values: &[String]) -> DetachedRows {
        DetachedRows(
            values
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    let mut row =
                        CachedEntity::new(plasm_core::Ref::new("Document", i.to_string()), 0);
                    row.fields.insert(
                        "note".into(),
                        plasm_core::TypedFieldValue::String(text.clone()),
                    );
                    row.fields
                        .insert("selected".into(), plasm_core::Value::Bool(false).into());
                    row
                })
                .collect(),
        )
    }

    #[test]
    fn complete_note_is_not_previewed_when_the_table_fits() {
        for text in ["n".repeat(4_348), "é\n\t\"".repeat(700)] {
            let source = notes(std::slice::from_ref(&text));
            let rendered = render_observation(&source, None, 25, 12 * 1024);
            assert!(!rendered.fidelity.any_loss(), "{}", rendered.tsv);
            let cell = rendered
                .tsv
                .lines()
                .nth(1)
                .unwrap()
                .split('\t')
                .next()
                .unwrap();
            assert_eq!(serde_json::from_str::<String>(cell).unwrap(), text);
            assert!(rendered.tsv.len() <= 12 * 1024);
        }
    }

    #[test]
    fn page_rows_before_abbreviating_fitting_notes() {
        let source = notes(&vec!["n".repeat(4_348); 5]);
        let rendered = render_observation(&source, None, 25, 12 * 1024);
        assert!(rendered.shown < 5);
        assert!(!rendered.fidelity.any_loss(), "{}", rendered.tsv);
        assert!(rendered.tsv.len() <= 12 * 1024);
        assert_eq!(source.0.len(), 5);
    }

    #[test]
    fn enormous_field_does_not_abbreviate_fitting_siblings() {
        let mut source = notes(&["s".repeat(4500)]);
        source.0[0].fields.insert(
            "other_note".into(),
            plasm_core::TypedFieldValue::String("t".repeat(4500)),
        );
        source.0[0].fields.insert(
            "enormous".into(),
            plasm_core::TypedFieldValue::String("x".repeat(100_000)),
        );
        let rendered = render_observation(&source, None, 25, 12 * 1024);
        assert!(rendered.tsv.len() <= 12 * 1024);
        assert!(rendered.tsv.contains(&format!("\"{}\"", "s".repeat(4500))));
        assert!(rendered.tsv.contains(&format!("\"{}\"", "t".repeat(4500))));
        assert_eq!(
            rendered.fidelity.field_names().cloned().collect::<Vec<_>>(),
            ["enormous"]
        );
    }

    #[test]
    fn oversized_note_preserves_small_fields_and_exact_excerpts() {
        let text = format!("{}END", "é\n\t\"".repeat(10_000));
        let source = notes(std::slice::from_ref(&text));
        let rendered = render_observation(&source, None, 25, 4096);
        assert!(rendered.fidelity.any_loss());
        assert!(rendered.tsv.len() <= 4096);
        assert!(rendered.tsv.contains("(preview:"));
        assert!(rendered.tsv.contains("\tfalse"));
        assert_eq!(
            source.0[0].fields["note"].to_value(),
            plasm_core::Value::String(text.clone())
        );
        // Adjacent character ranges remain exact even for escaped multibyte text.
        let chars: Vec<_> = text.chars().collect();
        let mut recovered = String::new();
        for chunk in chars.chunks(500) {
            let excerpt: String = chunk.iter().collect();
            let page = render_observation(&notes(&[excerpt]), None, 1, 4096);
            assert!(!page.fidelity.any_loss());
            recovered.push_str(
                &serde_json::from_str::<String>(
                    page.tsv.lines().nth(1).unwrap().split('\t').next().unwrap(),
                )
                .unwrap(),
            );
        }
        assert_eq!(recovered, text);
    }

    #[tokio::test]
    async fn native_compute_excerpts_recover_previewed_text_exactly() {
        let text = format!("{}END", "é\n\t\"".repeat(2500));
        let source = notes(std::slice::from_ref(&text));
        assert!(render_observation(&source, None, 1, 4096)
            .fidelity
            .any_loss());
        let pool = crate::python_pool::PythonPool::default();
        let mut recovered = String::new();
        for start in (0..text.chars().count()).step_by(1000) {
            let excerpt = pool
                .compute(
                    format!("__input[0].note[{start}:{}]", start + 1000),
                    vec![std::collections::BTreeMap::from([(
                        "note".into(),
                        text.clone(),
                    )])],
                )
                .await
                .unwrap();
            let page = render_observation(&notes(&[excerpt]), None, 1, 4096);
            assert!(!page.fidelity.any_loss());
            recovered.push_str(
                &serde_json::from_str::<String>(
                    page.tsv.lines().nth(1).unwrap().split('\t').next().unwrap(),
                )
                .unwrap(),
            );
        }
        pool.close().await;
        assert_eq!(recovered, text);
    }
    #[test]
    fn detached_rows_and_execution_results_share_the_renderer() {
        let step =
            crate::test_support::execution_fixtures::synthetic_published_result_step(50, None);
        let detached = DetachedRows(step.result.entities().iter().cloned().collect());
        let live = render_observation(step.result.as_ref(), None, 25, 4000);
        let other = render_observation(&detached, None, 25, 4000);
        assert_eq!(live.tsv, other.tsv);
        assert_eq!(live.shown, other.shown);
        assert_eq!(live.coverage, other.coverage);
        assert_eq!(live.shown, 25);
    }
    #[test]
    fn exhausted_display_budget_preserves_a_row_but_not_a_false_empty_result() {
        let step =
            crate::test_support::execution_fixtures::synthetic_published_result_step(50, None);
        let rendered = render_observation(step.result.as_ref(), None, 0, 0);
        assert_eq!(rendered.shown, 1);
        assert_eq!(rendered.tsv.lines().count(), 2);
        let empty = DetachedRows(vec![]);
        assert_eq!(render_observation(&empty, None, 0, 0).shown, 0);
    }

    #[test]
    fn authored_reference_named_column_is_data_not_machine_identity() {
        let step =
            crate::test_support::execution_fixtures::synthetic_published_result_step(1, None);
        let mut rows: Vec<_> = step.result.entities().iter().cloned().collect();
        rows[0].fields.insert(
            "_ref".into(),
            plasm_core::TypedFieldValue::String("authored".into()),
        );
        let rendered = render_observation(&DetachedRows(rows), None, 10, 4000);
        assert!(rendered.tsv.contains("\"authored\""));
        assert!(rendered.tsv.lines().next().unwrap().contains("_ref"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        #[test]
        fn rowset_properties_source_independent_rendering_is_bounded_and_non_destructive(
            count in 0usize..300, max_rows in 0usize..300, budget in 0usize..16000,
            text in ".{0,1500}",
        ) {
            let step = crate::test_support::execution_fixtures::synthetic_published_result_step(count, None);
            let mut rows: Vec<_> = step.result.entities().iter().cloned().collect();
            for (i, row) in rows.iter_mut().enumerate() {
                row.fields.insert("computed_alias".into(), plasm_core::Value::String(format!("{i}:{text}")).into());
            }
            let source = DetachedRows(rows.clone());
            let mut live = step.result.as_ref().clone();
            live.collection = crate::test_support::execution_fixtures::collection(rows.clone(), ResultCoverage::Unknown);
            let a = render_observation(&source, None, max_rows, budget);
            let b = render_observation(&live, None, max_rows, budget);
            prop_assert_eq!(&a.tsv, &b.tsv);
            prop_assert_eq!(a.shown, b.shown);
            prop_assert!(a.shown <= count.min(max_rows.max(1)));
            prop_assert!(a.tsv.len() <= budget || a.shown <= 1);
            prop_assert_eq!(source.rows().len(), count);
            prop_assert_eq!(live.entities().len(), count);
            if count != 0 {
                prop_assert!(a.tsv.lines().next().unwrap().contains("computed_alias"));
                prop_assert!(a.shown >= 1);
            }
            // Increasing the context budget must never discard additional rows.
            let larger = render_observation(&source, None, max_rows, budget.saturating_mul(2));
            prop_assert!(larger.shown >= a.shown);
        }
    }
}
