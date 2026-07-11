use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SeedRef {
    pub entry_id: String,
    pub entity: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeedExpect {
    #[serde(default)]
    pub decision_any: Vec<String>,
    #[serde(default)]
    pub acceptable_sets: Vec<Vec<SeedRef>>,
    #[serde(default)]
    pub must_exclude: Vec<SeedRef>,
    #[serde(default)]
    pub max_seeds: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryExpect {
    #[serde(default)]
    pub entry_id_any: Vec<String>,
    #[serde(default)]
    pub entity_any: Vec<String>,
    #[serde(default)]
    pub capability_name_any: Vec<String>,
    #[serde(default)]
    pub ambiguous: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryEvalCase {
    pub id: String,
    pub intent: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub expect: DiscoveryExpect,
    #[serde(default)]
    pub seed_expect: Option<SeedExpect>,
    #[serde(default)]
    pub notes: Option<String>,
}

pub fn default_cases_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/discovery-eval/cases.yaml")
}

pub fn default_catalogs_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/discovery-eval/catalogs.yaml")
}

#[derive(Debug, Deserialize)]
struct CatalogsFile {
    entry_ids: Vec<String>,
}

pub fn load_catalog_entry_ids(path: &Path) -> anyhow::Result<Vec<String>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read catalogs file {}", path.display()))?;
    let parsed: CatalogsFile = serde_yaml::from_str(&raw)?;
    if parsed.entry_ids.is_empty() {
        bail!("catalogs file {} has empty entry_ids", path.display());
    }
    Ok(parsed.entry_ids)
}

pub fn load_cases(path: &Path) -> anyhow::Result<Vec<DiscoveryEvalCase>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read cases file {}", path.display()))?;
    let cases: Vec<DiscoveryEvalCase> = serde_yaml::from_str(&raw)?;
    validate_cases(&cases)?;
    Ok(cases)
}

fn validate_cases(cases: &[DiscoveryEvalCase]) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for case in cases {
        if case.id.trim().is_empty() {
            bail!("case with empty id");
        }
        if case.intent.trim().is_empty() {
            bail!("case {} has empty intent", case.id);
        }
        if !seen.insert(case.id.clone()) {
            bail!("duplicate case id: {}", case.id);
        }
        if case.expect.entry_id_any.is_empty() {
            bail!("case {} has empty expect.entry_id_any", case.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cases_load() {
        let path = default_cases_path();
        if !path.is_file() {
            return;
        }
        let cases = load_cases(&path).expect("load cases");
        assert!(cases.len() >= 40);
    }

    #[test]
    fn holdout_cases_load() {
        let path = default_cases_path()
            .parent()
            .expect("parent")
            .join("cases-holdout.yaml");
        if !path.is_file() {
            return;
        }
        let cases = load_cases(&path).expect("load holdout cases");
        assert!(cases.len() >= 20);
        assert!(cases.iter().all(|c| c.id.starts_with("ho_")));
    }
}
