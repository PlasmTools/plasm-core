//! Exhaustive semantic-obligation ledger with Python execution witnesses.
use super::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    Partial,
    Covered,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Obligation {
    status: Status,
    features: BTreeSet<String>,
    cases: Vec<String>,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewObligation {
    entity: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    rows: BTreeMap<String, Obligation>,
    supplemental_suites: BTreeMap<String, BTreeSet<String>>,
    non_row_features: BTreeMap<String, String>,
    cases: Vec<String>,
    views: BTreeMap<String, ViewObligation>,
}

fn ledger() -> Ledger {
    serde_json::from_str(include_str!("python_coverage.json")).expect("Python coverage ledger")
}

pub(super) fn covered_row(case: &python::Case) -> Option<&'static row::MatrixRow> {
    let id = case.existing?;
    (ledger().rows[id].status == Status::Covered).then(|| find_row(id).expect("original row"))
}

#[derive(Debug, PartialEq, Eq)]
enum CoverageError {
    SupplementalInventory {
        expected: BTreeMap<String, BTreeSet<String>>,
        actual: BTreeMap<String, BTreeSet<String>>,
    },
    RowInventory {
        missing: BTreeSet<String>,
        stale: BTreeSet<String>,
    },
    CaseInventory {
        missing: BTreeSet<String>,
        stale: BTreeSet<String>,
        executable_count: usize,
        ledger_count: usize,
        executable_unique: usize,
        ledger_unique: usize,
    },
    FeatureObligations {
        row: String,
        expected: BTreeSet<String>,
        actual: BTreeSet<String>,
    },
    MissingStatusReason {
        row: String,
    },
    UnprovedStatus {
        row: String,
        status: Status,
        witnesses: usize,
    },
    WitnessLinks {
        row: String,
        expected: BTreeSet<String>,
        actual: BTreeSet<String>,
        witness_count: usize,
    },
    FailureExpectation {
        row: String,
        case: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    ViewInventory {
        expected: BTreeMap<String, String>,
        actual: BTreeMap<String, String>,
    },
    MissingViewReason {
        view: String,
    },
    NonRowFeatures {
        expected: BTreeSet<String>,
        host: BTreeSet<String>,
        ledger: BTreeSet<String>,
    },
    MissingNonRowReason {
        feature: String,
    },
}

impl std::fmt::Display for CoverageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Python coverage obligation failed: {self:?}")
    }
}
impl std::error::Error for CoverageError {}

fn validate(ledger: &Ledger) -> Result<(), CoverageError> {
    let supplemental = BTreeMap::from([(
        "python_union_matrix".to_owned(),
        python::union_matrix::FEATURES
            .iter()
            .map(|tag| (*tag).to_owned())
            .collect(),
    )]);
    if ledger.supplemental_suites != supplemental {
        return Err(CoverageError::SupplementalInventory {
            expected: supplemental,
            actual: ledger.supplemental_suites.clone(),
        });
    }
    let rows: Vec<_> = all_rows().collect();
    let expected: BTreeSet<_> = rows.iter().map(|r| r.id).collect();
    let actual: BTreeSet<_> = ledger.rows.keys().map(String::as_str).collect();
    if expected != actual {
        return Err(CoverageError::RowInventory {
            missing: expected
                .difference(&actual)
                .map(|id| (*id).to_owned())
                .collect(),
            stale: actual
                .difference(&expected)
                .map(|id| (*id).to_owned())
                .collect(),
        });
    }
    let expected_cases: BTreeSet<_> = python::cases().map(|c| c.id).collect();
    let actual_cases: BTreeSet<_> = ledger.cases.iter().map(String::as_str).collect();
    if expected_cases != actual_cases
        || actual_cases.len() != ledger.cases.len()
        || expected_cases.len() != python::cases().count()
    {
        return Err(CoverageError::CaseInventory {
            missing: expected_cases
                .difference(&actual_cases)
                .map(|id| (*id).to_owned())
                .collect(),
            stale: actual_cases
                .difference(&expected_cases)
                .map(|id| (*id).to_owned())
                .collect(),
            executable_count: python::cases().count(),
            ledger_count: ledger.cases.len(),
            executable_unique: expected_cases.len(),
            ledger_unique: actual_cases.len(),
        });
    }
    for row in &rows {
        let entry = &ledger.rows[row.id];
        let tags: BTreeSet<_> = row.features.iter().copied().collect();
        if tags != entry.features.iter().map(String::as_str).collect() {
            return Err(CoverageError::FeatureObligations {
                row: row.id.into(),
                expected: tags.into_iter().map(str::to_owned).collect(),
                actual: entry.features.clone(),
            });
        }
        if entry.reason.trim().is_empty() {
            return Err(CoverageError::MissingStatusReason { row: row.id.into() });
        }
        if entry.status != Status::Covered || entry.cases.is_empty() {
            return Err(CoverageError::UnprovedStatus {
                row: row.id.into(),
                status: entry.status.clone(),
                witnesses: entry.cases.len(),
            });
        }
        let linked: BTreeSet<_> = python::cases()
            .filter(|c| c.existing == Some(row.id))
            .map(|c| c.id)
            .collect();
        if linked != entry.cases.iter().map(String::as_str).collect()
            || linked.len() != entry.cases.len()
        {
            return Err(CoverageError::WitnessLinks {
                row: row.id.into(),
                expected: linked.into_iter().map(str::to_owned).collect(),
                actual: entry.cases.iter().cloned().collect(),
                witness_count: entry.cases.len(),
            });
        }
        if entry.status == Status::Covered {
            for case in python::cases().filter(|c| linked.contains(c.id)) {
                if row.expect_live_error != case.expect_live_error {
                    return Err(CoverageError::FailureExpectation {
                        row: row.id.into(),
                        case: case.id.into(),
                        expected: row.expect_live_error.map(str::to_owned),
                        actual: case.expect_live_error.map(str::to_owned),
                    });
                }
            }
        }
    }
    let views: BTreeMap<_, _> = plasm_runtime::view_test_support::MATRIX_VIEW_PREFLIGHT_CASES
        .iter()
        .copied()
        .collect();
    let tracked: BTreeMap<_, _> = ledger
        .views
        .iter()
        .map(|(name, v)| (name.as_str(), v.entity.as_str()))
        .collect();
    if views != tracked {
        return Err(CoverageError::ViewInventory {
            expected: views
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            actual: tracked
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        });
    }
    if let Some((view, _)) = ledger
        .views
        .iter()
        .find(|(_, v)| v.reason.trim().is_empty())
    {
        return Err(CoverageError::MissingViewReason { view: view.clone() });
    }
    let non_row: BTreeSet<_> = features::REQUIRED_FEATURE_TAGS
        .iter()
        .copied()
        .filter(|tag| !rows.iter().any(|row| row.features.contains(tag)))
        .collect();
    if non_row
        != super::python_host_contract::FEATURES
            .iter()
            .copied()
            .collect()
        || non_row != ledger.non_row_features.keys().map(String::as_str).collect()
    {
        return Err(CoverageError::NonRowFeatures {
            expected: non_row.into_iter().map(str::to_owned).collect(),
            host: super::python_host_contract::FEATURES
                .iter()
                .map(|tag| (*tag).to_owned())
                .collect(),
            ledger: ledger.non_row_features.keys().cloned().collect(),
        });
    }
    if let Some((feature, _)) = ledger
        .non_row_features
        .iter()
        .find(|(_, reason)| reason.trim().is_empty())
    {
        return Err(CoverageError::MissingNonRowReason {
            feature: feature.clone(),
        });
    }
    Ok(())
}

#[test]
fn python_coverage_tracks_every_original_obligation() {
    let ledger = ledger();
    validate(&ledger).unwrap();
    let all_tags: BTreeSet<_> = ledger
        .rows
        .values()
        .flat_map(|r| &r.features)
        .chain(ledger.non_row_features.keys())
        .collect();
    println!("Original obligation inventory: {}/{} rows linked; {}/{} features accounted for; {} views linked to matrix_views_all_preflight. Execution results are separate; supplemental cases do not substitute for original obligations.", ledger.rows.len(), ledger.rows.len(), all_tags.len(), all_tags.len(), ledger.views.len());
    println!(
        "{} executable Python cases registered; execution verdict is reported by the live suite.",
        ledger.cases.len()
    );
    println!("{}", super::scoped_composition::report());
    println!("{}", super::semantic_contract::report());
}

#[test]
fn python_coverage_rejects_untracked_or_unproved_claims() {
    let mut value = ledger();
    value.rows.remove("lang_get_by_id");
    assert_eq!(
        validate(&value),
        Err(CoverageError::RowInventory {
            missing: BTreeSet::from(["lang_get_by_id".into()]),
            stale: BTreeSet::new(),
        })
    );
    let mut value = ledger();
    value
        .rows
        .get_mut("lang_get_by_id")
        .unwrap()
        .features
        .clear();
    assert_eq!(
        validate(&value),
        Err(CoverageError::FeatureObligations {
            row: "lang_get_by_id".into(),
            expected: find_row("lang_get_by_id")
                .unwrap()
                .features
                .iter()
                .map(|tag| (*tag).into())
                .collect(),
            actual: BTreeSet::new(),
        })
    );
    let mut value = ledger();
    let claimed = value.rows.get_mut("lang_get_by_id").unwrap();
    claimed.cases.clear();
    assert_eq!(
        validate(&value),
        Err(CoverageError::UnprovedStatus {
            row: "lang_get_by_id".into(),
            status: Status::Covered,
            witnesses: 0,
        })
    );
    let mut value = ledger();
    value.rows.get_mut("lang_get_by_id").unwrap().cases = vec!["unknown".into()];
    assert_eq!(
        validate(&value),
        Err(CoverageError::WitnessLinks {
            row: "lang_get_by_id".into(),
            expected: python::cases()
                .filter(|case| case.existing == Some("lang_get_by_id"))
                .map(|case| case.id.into())
                .collect(),
            actual: BTreeSet::from(["unknown".into()]),
            witness_count: 1,
        })
    );
    for incomplete in [Status::Pending, Status::Partial] {
        let mut value = ledger();
        let witnesses = value.rows["lang_get_by_id"].cases.len();
        value.rows.get_mut("lang_get_by_id").unwrap().status = incomplete.clone();
        assert_eq!(
            validate(&value),
            Err(CoverageError::UnprovedStatus {
                row: "lang_get_by_id".into(),
                status: incomplete,
                witnesses,
            })
        );
    }
    let mut value = ledger();
    value.views.clear();
    assert_eq!(
        validate(&value),
        Err(CoverageError::ViewInventory {
            expected: plasm_runtime::view_test_support::MATRIX_VIEW_PREFLIGHT_CASES
                .iter()
                .map(|(name, entity)| ((*name).into(), (*entity).into()))
                .collect(),
            actual: BTreeMap::new(),
        })
    );

    let mut value = ledger();
    value.cases.push("unknown".into());
    assert_eq!(
        validate(&value),
        Err(CoverageError::CaseInventory {
            missing: BTreeSet::new(),
            stale: BTreeSet::from(["unknown".into()]),
            executable_count: python::cases().count(),
            executable_unique: python::cases().count(),
            ledger_count: python::cases().count() + 1,
            ledger_unique: python::cases().count() + 1,
        })
    );
    let mut value = ledger();
    value.cases.push(value.cases[0].clone());
    assert_eq!(
        validate(&value),
        Err(CoverageError::CaseInventory {
            missing: BTreeSet::new(),
            stale: BTreeSet::new(),
            executable_count: python::cases().count(),
            executable_unique: python::cases().count(),
            ledger_count: python::cases().count() + 1,
            ledger_unique: python::cases().count(),
        })
    );

    let mut value = ledger();
    value.rows.get_mut("lang_get_by_id").unwrap().reason.clear();
    assert_eq!(
        validate(&value),
        Err(CoverageError::MissingStatusReason {
            row: "lang_get_by_id".into()
        })
    );
}

#[test]
fn python_coverage_requires_union_suite_evidence() {
    let mut value = ledger();
    value.supplemental_suites.clear();
    assert_eq!(
        validate(&value),
        Err(CoverageError::SupplementalInventory {
            expected: BTreeMap::from([(
                "python_union_matrix".into(),
                python::union_matrix::FEATURES
                    .iter()
                    .map(|tag| (*tag).into())
                    .collect()
            )]),
            actual: BTreeMap::new(),
        })
    );
}

#[test]
fn python_conformance_has_no_historical_source_compiler() {
    for (name, source) in [
        ("runner", include_str!("python.rs")),
        ("completion", include_str!("python_completion.rs")),
        ("federation", include_str!("python_federated_parity.rs")),
        ("render", include_str!("python_render_parity.rs")),
        ("relations", include_str!("relation_fanout.rs")),
        ("entry", include_str!("main.rs")),
        ("views", include_str!("../plasm_language_matrix_views.rs")),
        (
            "view programs",
            include_str!("../plasm_language_matrix_views/python.rs"),
        ),
    ] {
        for forbidden in [
            "compile_plasm_program",
            "compile_plasm_expression",
            "compile_plasm_surface_line",
            "parse_with_cgs_layers",
            "baseline_program",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} reintroduced historical compiler dependency {forbidden}"
            );
        }
    }
}
