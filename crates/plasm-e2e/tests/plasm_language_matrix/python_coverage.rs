//! Exhaustive migration ledger over the original matrix, never a second feature universe.
use super::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Deserialize, PartialEq, Eq)]
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

/// A covered obligation executes the original matrix program, including synthesized symbols.
/// Supplemental experiments do not certify an original feature.
pub(super) fn baseline_program(
    case: &python::Case,
    es: &plasm_agent::execute_session::ExecuteSession,
) -> String {
    if let Some(row) = covered_row(case) {
        return matrix_program_for_row(row, es);
    }
    case.plasm.to_owned()
}

pub(super) fn covered_row(case: &python::Case) -> Option<&'static row::MatrixRow> {
    let id = case.existing?;
    (ledger().rows[id].status == Status::Covered).then(|| find_row(id).expect("original row"))
}

fn validate(ledger: &Ledger) -> Result<(), String> {
    let supplemental = BTreeMap::from([(
        "python_union_matrix".to_owned(),
        python::union_matrix::FEATURES
            .iter()
            .map(|tag| (*tag).to_owned())
            .collect(),
    )]);
    if ledger.supplemental_suites != supplemental {
        return Err("supplemental suite inventory changed".into());
    }
    let rows: Vec<_> = all_rows().collect();
    let expected: BTreeSet<_> = rows.iter().map(|r| r.id).collect();
    let actual: BTreeSet<_> = ledger.rows.keys().map(String::as_str).collect();
    if expected != actual {
        return Err(format!(
            "matrix row inventory changed: missing {:?}, stale {:?}",
            expected.difference(&actual),
            actual.difference(&expected)
        ));
    }
    let expected_cases: BTreeSet<_> = python::cases().map(|c| c.id).collect();
    let actual_cases: BTreeSet<_> = ledger.cases.iter().map(String::as_str).collect();
    if expected_cases != actual_cases
        || actual_cases.len() != ledger.cases.len()
        || expected_cases.len() != python::cases().count()
    {
        return Err("Python evidence inventory changed or contains duplicate IDs".into());
    }
    for row in &rows {
        let entry = &ledger.rows[row.id];
        let tags: BTreeSet<_> = row.features.iter().copied().collect();
        if tags != entry.features.iter().map(String::as_str).collect() {
            return Err(format!("{} feature obligations changed", row.id));
        }
        if entry.reason.trim().is_empty() {
            return Err(format!("{} needs a status reason", row.id));
        }
        if entry.status != Status::Covered || entry.cases.is_empty() {
            return Err(format!(
                "{} status requires covered with executable evidence",
                row.id
            ));
        }
        let linked: BTreeSet<_> = python::cases()
            .filter(|c| c.existing == Some(row.id))
            .map(|c| c.id)
            .collect();
        if linked != entry.cases.iter().map(String::as_str).collect()
            || linked.len() != entry.cases.len()
        {
            return Err(format!(
                "{} evidence must match executable case links",
                row.id
            ));
        }
        if entry.status == Status::Covered {
            for case in python::cases().filter(|c| linked.contains(c.id)) {
                if row.expect_live_error != case.expect_live_error {
                    return Err(format!(
                        "{} must retain original failure expectation",
                        row.id
                    ));
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
    if views != tracked || ledger.views.values().any(|v| v.reason.trim().is_empty()) {
        return Err("views inventory changed or missing evidence reason".into());
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
        || ledger
            .non_row_features
            .values()
            .any(|reason| reason.trim().is_empty())
    {
        return Err("non-row feature inventory changed or missing evidence reason".into());
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
    println!("Python parity: {}/{} rows covered; {}/{} features covered; {} views covered by matrix_views_all_preflight. Supplemental pairs are not substitutes for original obligations.", ledger.rows.len(), ledger.rows.len(), all_tags.len(), all_tags.len(), ledger.views.len());
}

#[test]
fn python_coverage_rejects_untracked_or_unproved_claims() {
    let mut value = ledger();
    value.rows.remove("lang_get_by_id");
    assert!(validate(&value).unwrap_err().contains("inventory changed"));
    let mut value = ledger();
    value
        .rows
        .get_mut("lang_get_by_id")
        .unwrap()
        .features
        .clear();
    assert!(validate(&value)
        .unwrap_err()
        .contains("feature obligations"));
    let mut value = ledger();
    let claimed = value.rows.get_mut("lang_get_by_id").unwrap();
    claimed.cases.clear();
    assert!(validate(&value).unwrap_err().contains("status requires"));
    let mut value = ledger();
    value.rows.get_mut("lang_get_by_id").unwrap().cases = vec!["unknown".into()];
    assert!(validate(&value)
        .unwrap_err()
        .contains("executable case links"));
    for incomplete in [Status::Pending, Status::Partial] {
        let mut value = ledger();
        value.rows.get_mut("lang_get_by_id").unwrap().status = incomplete;
        assert!(validate(&value).unwrap_err().contains("status requires"));
    }
    let mut value = ledger();
    value.views.clear();
    assert!(validate(&value).unwrap_err().contains("views inventory"));
}

#[test]
fn python_coverage_requires_union_suite_evidence() {
    let mut value = ledger();
    value.supplemental_suites.clear();
    assert!(validate(&value).unwrap_err().contains("supplemental suite"));
}
