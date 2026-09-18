//! Host-visible recovery after partial or missed discovery coverage.
//!
//! Selector wire uses `requirement_coverage`. Hosts surface unresolved clauses
//! derived from unresolved assessments, and attach
//! authorized catalog descriptions so agents can rediscover across available
//! integrations without inventing catalog ids.
//!
//! Limitation: recovery runs only when the selector reports insufficient
//! (any coverage entry unresolved). If the selector silently omits a required
//! clause and returns ready, this mechanism never runs — useful recovery
//! feedback is not proof that silent omission is fixed.

use plasm_core::schema::CGS;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::discovery_selection::{RequirementCoverage, SelectionStatus, UnsupportedWork};

/// Domain-general recovery copy. No app brands, no task-shaped reformulations.
///
/// Extend and new are not interchangeable: extend when a session exists; new
/// only when none exists yet.
///
/// One general distinction: searching rows searches **data**; needing another
/// kind of **operation** or **input source** calls for discovery extension.
/// Ambiguity among plausible records stays with the executing agent.
pub const RECOVERY_GUIDANCE: &str =
    "Partial capability coverage does not satisfy the full intent. \
Unresolved clauses above remain open. Searching rows searches data within taught capabilities; \
needing another kind of operation or another input source requires discovery extension — \
rephrase the unresolved need and call plasm_context again (session_mode extend with the same \
logical_session_ref when a logical session already exists; session_mode new only when none \
exists yet) so discovery can search again across the available integration descriptions listed \
below. Discovery exposes ways to investigate alternatives; it does not choose the intended \
record or authorize a substitute operation. Do not treat partial teaching as complete coverage.";

const MAX_DESCRIPTION_BYTES: usize = 720;
const MAX_ENTITY_BLURBS: usize = 8;

/// Floor `max_bytes` to a UTF-8 char boundary, then truncate.
///
/// `String::truncate` panics if the index is not a char boundary; multi-byte
/// sequences (including the `·` blurb separator) must not be split.
fn truncate_at_char_boundary(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Authorized catalog entry with a short domain description for broader rediscovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogAppDescription {
    pub entry_id: String,
    pub description: String,
}

/// Host recovery surface when selection status is insufficient.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecovery {
    /// Unresolved intent clauses (derived from requirement_coverage).
    pub unresolved: Vec<UnsupportedWork>,
    /// Full requirement coverage for audit (supporting IDs or unresolved reasons).
    #[serde(default)]
    pub requirement_coverage: Vec<RequirementCoverage>,
    /// Authorized catalogs in the pinned generation, with domain descriptions.
    pub available_catalogs: Vec<CatalogAppDescription>,
    /// Domain-general recovery instruction.
    pub guidance: String,
}

impl DiscoveryRecovery {
    pub fn from_insufficient(
        coverage: &[RequirementCoverage],
        catalogs: &BTreeMap<String, CGS>,
        allowed_catalog_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let allowed: std::collections::BTreeSet<String> = allowed_catalog_ids
            .into_iter()
            .map(|id| id.as_ref().to_owned())
            .collect();
        let available_catalogs = catalogs
            .iter()
            .filter(|(entry_id, _)| allowed.contains(entry_id.as_str()))
            .map(|(entry_id, cgs)| catalog_app_description(entry_id, cgs))
            .collect();
        let unresolved: Vec<UnsupportedWork> = coverage
            .iter()
            .filter_map(RequirementCoverage::as_unsupported)
            .collect();
        Self {
            unresolved,
            requirement_coverage: coverage.to_vec(),
            available_catalogs,
            guidance: RECOVERY_GUIDANCE.to_owned(),
        }
    }

    /// Agent-visible markdown. Leads with unresolved needs; surfaces coverage.
    pub fn render_markdown(&self, status: SelectionStatus) -> String {
        let mut lines = vec![format!("**plasm_context:** {status:?}")];
        if !self.unresolved.is_empty() {
            lines.push(
                "**Unresolved** (intent not fully covered by presented capabilities):".into(),
            );
            for work in &self.unresolved {
                lines.push(format!("- `{}` — {}", work.requirement, work.reason));
            }
        }
        if !self.requirement_coverage.is_empty() {
            lines.push("**Requirement coverage** (audit; not execution approval):".into());
            for entry in &self.requirement_coverage {
                if let Some(missing) = entry.assessment.missing() {
                    lines.push(format!(
                        "- `{}` — unresolved: {}; useful: {}",
                        entry.requirement,
                        missing,
                        entry.assessment.capability_ids().join(", ")
                    ));
                } else if let crate::discovery_selection::RequirementAssessment::NoCapabilityNeeded { no_capability_needed } = &entry.assessment {
                    lines.push(format!("- `{}` — local: {}", entry.requirement, no_capability_needed));
                } else {
                    lines.push(format!(
                        "- `{}` — supporting: {}",
                        entry.requirement,
                        entry.assessment.capability_ids().join(", ")
                    ));
                }
            }
        }
        lines.push(self.guidance.clone());
        if !self.available_catalogs.is_empty() {
            lines.push("**Available integrations** (descriptions for broader rediscovery):".into());
            for catalog in &self.available_catalogs {
                lines.push(format!(
                    "- `{}` — {}",
                    catalog.entry_id, catalog.description
                ));
            }
        }
        lines.join("\n\n")
    }
}

/// Synthesize a catalog-level description from entity prose (OpenAPI-derived catalogs included).
pub fn catalog_app_description(entry_id: &str, cgs: &CGS) -> CatalogAppDescription {
    let mut blurbs: Vec<String> = cgs
        .entities
        .iter()
        .filter_map(|(name, def)| {
            let description = def.description.trim();
            if description.is_empty() {
                None
            } else {
                Some(format!("{name}: {description}"))
            }
        })
        .take(MAX_ENTITY_BLURBS)
        .collect();
    if blurbs.is_empty() {
        blurbs = cgs
            .entities
            .keys()
            .take(MAX_ENTITY_BLURBS)
            .map(|name| name.to_string())
            .collect();
    }
    let mut description = if blurbs.is_empty() {
        format!("Integration `{entry_id}`")
    } else {
        blurbs.join(" · ")
    };
    if description.len() > MAX_DESCRIPTION_BYTES {
        truncate_at_char_boundary(&mut description, MAX_DESCRIPTION_BYTES);
        description.push('…');
    }
    CatalogAppDescription {
        entry_id: entry_id.to_owned(),
        description,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;
    use std::path::PathBuf;

    fn fixture_dir(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/schemas/{name}"))
    }

    fn covered(quote: &str, ids: &[&str]) -> RequirementCoverage {
        RequirementCoverage {
            requirement: quote.into(),
            assessment: crate::discovery_service::RequirementAssessment::Supported {
                supported_by: ids.iter().map(|s| (*s).to_owned()).collect(),
            },
        }
    }

    fn unresolved(quote: &str, reason: &str) -> RequirementCoverage {
        RequirementCoverage {
            requirement: quote.into(),
            assessment: crate::discovery_service::RequirementAssessment::Unresolved {
                useful_capabilities: vec![],
                missing: reason.into(),
            },
        }
    }

    #[test]
    fn partial_requirement_delivers_its_gap_and_useful_consumer() {
        let coverage = vec![RequirementCoverage {
            requirement: "perform effect using required input".into(),
            assessment: crate::discovery_service::RequirementAssessment::Unresolved {
                useful_capabilities: vec!["consumer".into()],
                missing: "required input acquisition".into(),
            },
        }];
        let recovery =
            DiscoveryRecovery::from_insufficient(&coverage, &BTreeMap::new(), Vec::<String>::new());
        assert_eq!(recovery.unresolved.len(), 1);
        assert_eq!(recovery.requirement_coverage, coverage);
        let markdown = recovery.render_markdown(SelectionStatus::Insufficient);
        assert!(markdown.contains("unresolved: required input acquisition; useful: consumer"));
        let stored = serde_json::to_value(&recovery).unwrap();
        assert_eq!(
            stored["requirement_coverage"][0]["assessment"]["useful_capabilities"],
            serde_json::json!(["consumer"])
        );
    }

    #[test]
    fn truncate_at_char_boundary_does_not_split_multibyte_utf8() {
        // 719 ASCII bytes + U+00B7 (`·`, 2 UTF-8 bytes): byte truncate at 720
        // lands mid-character and panics. Floor to the prior char boundary.
        let mut description = "a".repeat(719);
        description.push('·');
        description.push_str("rest-that-must-be-cut");
        assert!(description.len() > MAX_DESCRIPTION_BYTES);
        assert!(!description.is_char_boundary(MAX_DESCRIPTION_BYTES));
        truncate_at_char_boundary(&mut description, MAX_DESCRIPTION_BYTES);
        assert!(description.is_char_boundary(description.len()));
        assert_eq!(description.len(), 719);
        assert!(!description.contains('·'));
        assert!(!description.contains("rest"));
    }

    #[test]
    fn catalog_description_truncates_multibyte_without_panic() {
        let mut cgs =
            load_schema_dir(&fixture_dir("overshow_tools")).expect("abstract overshow_tools");
        // Force a description that would panic under byte-offset truncate(720)
        // when the cut lands inside a multi-byte separator / UTF-8 sequence.
        let pad = "α".repeat(400); // 2-byte codepoints
        for (_name, def) in cgs.entities.iter_mut() {
            def.description = format!("{pad} · trailing international text");
        }
        let catalog = catalog_app_description("alpha", &cgs);
        assert!(catalog.description.ends_with('…'));
        assert!(catalog
            .description
            .is_char_boundary(catalog.description.len()));
        // Prefix + ellipsis stay within one char of the byte budget.
        assert!(catalog.description.len() <= MAX_DESCRIPTION_BYTES + '…'.len_utf8());
    }

    #[test]
    fn recovery_guidance_does_not_treat_new_as_interchangeable_with_extend() {
        assert!(RECOVERY_GUIDANCE.contains("extend with the same logical_session_ref"));
        assert!(RECOVERY_GUIDANCE.contains("new only when none exists yet"));
        assert!(!RECOVERY_GUIDANCE.contains("or new)"));
        assert!(!RECOVERY_GUIDANCE.to_lowercase().contains("or new so"));
    }

    #[test]
    fn recovery_teaches_data_search_vs_discovery_extension() {
        assert!(RECOVERY_GUIDANCE.contains("Searching rows searches data"));
        assert!(RECOVERY_GUIDANCE.contains("another kind of operation"));
        assert!(RECOVERY_GUIDANCE.contains("another input source"));
        assert!(RECOVERY_GUIDANCE.contains("does not choose the intended record"));
        assert!(RECOVERY_GUIDANCE.contains("authorize a substitute operation"));
        assert!(!RECOVERY_GUIDANCE.to_lowercase().contains("gmail"));
        assert!(!RECOVERY_GUIDANCE.to_lowercase().contains("flight"));
        assert!(!RECOVERY_GUIDANCE.to_lowercase().contains("venmo"));
    }

    #[test]
    fn recovery_preserves_unresolved_and_lists_authorized_catalogs_only() {
        let alpha =
            load_schema_dir(&fixture_dir("overshow_tools")).expect("abstract overshow_tools");
        let beta =
            load_schema_dir(&fixture_dir("pokeapi_mini")).expect("abstract pokeapi_mini fixture");
        let mut catalogs = BTreeMap::new();
        catalogs.insert("alpha".into(), alpha);
        catalogs.insert("beta".into(), beta);
        let coverage = vec![unresolved(
            "retrieve allowance",
            "No presented capability returns that information",
        )];
        let recovery = DiscoveryRecovery::from_insufficient(&coverage, &catalogs, ["alpha"]);
        assert_eq!(recovery.unresolved.len(), 1);
        assert_eq!(recovery.requirement_coverage.len(), 1);
        assert_eq!(recovery.available_catalogs.len(), 1);
        assert_eq!(recovery.available_catalogs[0].entry_id, "alpha");
        assert!(
            !recovery.available_catalogs[0].description.is_empty(),
            "expected entity-derived description"
        );
        let markdown = recovery.render_markdown(SelectionStatus::Insufficient);
        assert!(markdown.contains("Unresolved"));
        assert!(markdown.contains("retrieve allowance"));
        assert!(markdown.contains("Requirement coverage"));
        assert!(markdown.contains("`alpha`"));
        assert!(!markdown.contains("`beta`"));
        assert!(markdown.contains("Partial capability coverage"));
        assert!(markdown.contains("Searching rows searches data"));
        assert!(markdown.contains("new only when none exists yet"));
        assert!(!markdown.to_lowercase().contains("gmail"));
        assert!(!markdown.to_lowercase().contains("flight"));
    }

    #[test]
    fn insufficient_with_selected_work_still_leads_with_unresolved() {
        let recovery = DiscoveryRecovery {
            unresolved: vec![UnsupportedWork {
                requirement: "confirm prior constraint".into(),
                reason: "Presented documents do not expose that information".into(),
            }],
            requirement_coverage: vec![
                covered("inspect records", &["c0"]),
                unresolved(
                    "confirm prior constraint",
                    "Presented documents do not expose that information",
                ),
            ],
            available_catalogs: vec![CatalogAppDescription {
                entry_id: "matrix".into(),
                description: "Abstract records".into(),
            }],
            guidance: RECOVERY_GUIDANCE.to_owned(),
        };
        let markdown = recovery.render_markdown(SelectionStatus::Insufficient);
        let unresolved_at = markdown.find("Unresolved").expect("unresolved heading");
        let coverage_at = markdown
            .find("Requirement coverage")
            .expect("coverage heading");
        let integrations_at = markdown
            .find("Available integrations")
            .expect("integrations heading");
        assert!(unresolved_at < coverage_at);
        assert!(coverage_at < integrations_at);
        assert!(markdown.starts_with("**plasm_context:** Insufficient"));
        assert!(markdown.contains("supporting: c0"));
        assert!(markdown.contains("unresolved: Presented documents"));
    }

    /// Composite request with a missing clause: host shows unresolved + coverage,
    /// rediscovery affordance, and does not authorize a substitute.
    #[test]
    fn behavior_composite_missing_clause_surfaces_unresolved_and_rediscovery() {
        let coverage = vec![
            covered("list abstract records", &["c0"]),
            unresolved(
                "emit side-effect signal",
                "Presented documents do not expose that operation",
            ),
        ];
        let recovery = DiscoveryRecovery::from_insufficient(
            &coverage,
            &BTreeMap::from([(
                "matrix".into(),
                load_schema_dir(&fixture_dir("overshow_tools")).expect("fixture"),
            )]),
            ["matrix"],
        );
        let markdown = recovery.render_markdown(SelectionStatus::Insufficient);
        assert!(markdown.contains("emit side-effect signal"));
        assert!(markdown.contains("list abstract records"));
        assert!(markdown.contains("supporting: c0"));
        assert!(markdown.contains("Available integrations"));
        assert!(markdown.contains("discovery extension"));
        assert!(markdown.contains("does not choose the intended record"));
        assert!(!markdown.to_lowercase().contains("guess"));
    }

    /// Newly needed input source: unresolved coverage + extend rediscovery, not substitute auth.
    #[test]
    fn behavior_newly_needed_input_source_calls_for_discovery_extension() {
        let coverage = vec![unresolved(
            "obtain upstream identifier",
            "No presented capability produces that input",
        )];
        let recovery = DiscoveryRecovery {
            unresolved: coverage
                .iter()
                .filter_map(RequirementCoverage::as_unsupported)
                .collect(),
            requirement_coverage: coverage,
            available_catalogs: vec![CatalogAppDescription {
                entry_id: "provider".into(),
                description: "Value acquisition integration".into(),
            }],
            guidance: RECOVERY_GUIDANCE.to_owned(),
        };
        let markdown = recovery.render_markdown(SelectionStatus::Insufficient);
        assert!(markdown.contains("obtain upstream identifier"));
        assert!(markdown.contains("another input source"));
        assert!(markdown.contains("discovery extension"));
        assert!(markdown.contains("`provider`"));
        assert!(markdown.contains("authorize a substitute operation"));
    }

    /// Two plausible records: discovery does not pick; agent must not be told to guess.
    #[test]
    fn behavior_two_plausible_records_leaves_choice_with_agent() {
        assert!(RECOVERY_GUIDANCE.contains("does not choose the intended record"));
        assert!(!RECOVERY_GUIDANCE
            .to_lowercase()
            .contains("guess the record"));
        assert!(!RECOVERY_GUIDANCE.to_lowercase().contains("pick one"));
        let markdown = DiscoveryRecovery {
            unresolved: vec![],
            requirement_coverage: vec![covered("locate matching record", &["c0", "c1"])],
            available_catalogs: vec![],
            guidance: RECOVERY_GUIDANCE.to_owned(),
        }
        .render_markdown(SelectionStatus::Ready);
        // Ready recovery is unusual, but wording still forbids substitute authorization.
        assert!(markdown.contains("does not choose the intended record"));
        assert!(markdown.contains("supporting: c0, c1"));
    }

    /// Ambiguity that genuinely requires a user answer — not discovery adjudication.
    #[test]
    fn behavior_ambiguity_requiring_user_answer_stays_with_agent() {
        assert!(RECOVERY_GUIDANCE.contains("investigate alternatives"));
        // Host recovery must not invent a clarification protocol or choose for the user.
        let markdown = DiscoveryRecovery {
            unresolved: vec![UnsupportedWork {
                requirement: "apply preferred option".into(),
                reason: "Presented capabilities expose alternatives; user choice is required"
                    .into(),
            }],
            requirement_coverage: vec![unresolved(
                "apply preferred option",
                "Presented capabilities expose alternatives; user choice is required",
            )],
            available_catalogs: vec![CatalogAppDescription {
                entry_id: "matrix".into(),
                description: "Abstract records".into(),
            }],
            guidance: RECOVERY_GUIDANCE.to_owned(),
        }
        .render_markdown(SelectionStatus::Insufficient);
        assert!(markdown.contains("user choice is required"));
        assert!(markdown.contains("does not choose the intended record"));
        assert!(!markdown.to_lowercase().contains("gmail"));
        assert!(!markdown.to_lowercase().contains("venmo"));
    }
}
