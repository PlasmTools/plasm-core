//! Curated natural-language intents that stress multi-catalog discovery.
//!
//! Intents are written as plausible user goals **without** embedding the “correct” integration
//! name unless a real user would say it. Companion metadata (`plausible_primary_entry_ids`,
//! `stress_note`) is for harnesses and reviewers — not mixed into the intent strings.
//!
//! Each intent is **satisfiable in principle** by at least one capability in the corresponding
//! Plasm CGS under `apis/<entry_id>/` (narrow slices only—e.g. Cloudflare Phase 1 has zones,
//! rulesets, entrypoints, WAF packages—not cache purge).

use crate::discovery::CapabilityQuery;

/// How a bad discovery outcome tends to present for a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscoveryAdversarialFailureKind {
    /// Goal catalog / entities fail to surface where an expert would need them.
    HardMiss,
    /// Many unrelated catalogs score similarly or clutter results.
    SoftNoise,
}

/// One adversarial discovery probe.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryAdversarialCase {
    pub id: &'static str,
    /// End-user style goal text (HTTP/MCP discovery `phrases`, etc.).
    pub intent: &'static str,
    pub kind: DiscoveryAdversarialFailureKind,
    /// Why this case is adversarial for routing / lexicon overlap.
    pub stress_note: &'static str,
    /// Catalog `entry_id`s a domain expert would treat as primary targets (weak signal for harnesses).
    pub plausible_primary_entry_ids: &'static [&'static str],
}

impl DiscoveryAdversarialCase {
    /// [`CapabilityQuery`] matching typical HTTP/MCP discovery usage (`phrases` only).
    pub fn capability_query(&self) -> CapabilityQuery {
        CapabilityQuery {
            phrases: vec![self.intent.to_string()],
            ..Default::default()
        }
    }
}

/// Cross-cutting intents that collide across several APIs in the inventory.
pub const CROSS_CUTTING: &[DiscoveryAdversarialCase] = &[
    DiscoveryAdversarialCase {
        id: "cross_open_work_release_triage",
        intent: "Open work assigned to me blocking release this week; I only care about stuff still in triage.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Shared PM vocabulary: assignee, blocked, triage, release across trackers.",
        plausible_primary_entry_ids: &[
            "clickup", "github", "gitlab", "jira", "linear",
        ],
    },
    DiscoveryAdversarialCase {
        id: "cross_threads_followup_task",
        intent: "Find threads where people agreed on the rollout date but nobody created the follow-up task.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "\"Thread\" + \"task\" spans chat, mail, and docs products.",
        plausible_primary_entry_ids: &[
            "slack", "microsoft-teams", "gmail", "outlook", "notion",
        ],
    },
    DiscoveryAdversarialCase {
        id: "cross_shared_doc_qbr_access",
        intent: "Who has access to edit the shared doc for the QBR?",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "\"Shared doc\" + permissions overlaps docs, drive, and wiki surfaces.",
        plausible_primary_entry_ids: &[
            "google-docs", "google-drive", "notion", "github",
        ],
    },
    DiscoveryAdversarialCase {
        id: "cross_cloudflare_firewall_entrypoint_read",
        intent: "Fetch the current managed HTTP firewall phase entrypoint ruleset for our Cloudflare zone before we change it.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Matches Cloudflare Phase 1 `ruleset_entrypoint_get`; generic rules/firewall tokens still noise elsewhere.",
        plausible_primary_entry_ids: &["cloudflare"],
    },
    DiscoveryAdversarialCase {
        id: "cross_spreadsheet_exec_dashboard",
        intent: "Mirror the spreadsheet tab into something the exec dashboard can poll.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Sheets values/export APIs can satisfy; Drive files and BI-ish wording add noise.",
        plausible_primary_entry_ids: &["google-sheets", "google-drive"],
    },
];

/// Per-domain selections aligned with common inventory folders (`apis/<entry_id>/`).
pub const PER_CATALOG_SELECTION: &[DiscoveryAdversarialCase] = &[
    DiscoveryAdversarialCase {
        id: "tracker_sprint_story_points_gap",
        intent: "List sprint tasks that moved to In Progress yesterday but still have no story points.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Sprint/story-point jargon targets trackers; less overlap with HN Item-style feeds than generic item/list.",
        plausible_primary_entry_ids: &["jira", "linear", "clickup", "github"],
    },
    DiscoveryAdversarialCase {
        id: "code_review_before_merge",
        intent: "What still needs review before I can merge — excluding drafts?",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Merge/review/draft language overlaps Git hosts.",
        plausible_primary_entry_ids: &["github", "gitlab"],
    },
    DiscoveryAdversarialCase {
        id: "gitlab_mr_from_branch",
        intent: "Open a merge request from fix/login-timeout into main and assign reviewers.",
        kind: DiscoveryAdversarialFailureKind::HardMiss,
        stress_note: "Satisfiable via GitLab MR APIs; risk is discovery ranking GitHub PR flows instead.",
        plausible_primary_entry_ids: &["gitlab"],
    },
    DiscoveryAdversarialCase {
        id: "calendar_list_events_tuesday_window",
        intent: "List calendar events next Tuesday between 9am and 5pm on my primary calendar so I can slot the retro.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Matches read-only Google Calendar `event_list` slice; no event-create in that CGS.",
        plausible_primary_entry_ids: &["google-calendar"],
    },
    DiscoveryAdversarialCase {
        id: "comms_pin_company_decision",
        intent: "Pin the decision summary where the whole company will see it.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Slack/Discord pin capabilities satisfy pin semantics; which comms surface wins stays ambiguous.",
        plausible_primary_entry_ids: &["slack", "microsoft-teams", "linkedin", "notion"],
    },
    DiscoveryAdversarialCase {
        id: "mail_finance_net45_unlabeled",
        intent: "Find invoices forwarded from finance that mention NET-45 but aren’t labeled yet.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Mailbox vocabulary overlaps Gmail and Outlook.",
        plausible_primary_entry_ids: &["gmail", "outlook"],
    },
    DiscoveryAdversarialCase {
        id: "google_drive_auditors_view_only_file",
        intent: "Add view-only sharing on the QBR deck file in Drive for the external auditors.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Matches Drive `permissions_create` / sharing; Docs CGS does not model ACLs.",
        plausible_primary_entry_ids: &["google-drive"],
    },
    DiscoveryAdversarialCase {
        id: "google_sheets_csv_append_formulas",
        intent: "Append the CSV rows to the live workbook without wiping existing formulas.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Maps to Sheets append/value-range ops; generic row/table tokens still attract other catalogs.",
        plausible_primary_entry_ids: &["google-sheets"],
    },
    DiscoveryAdversarialCase {
        id: "cloudflare_list_zone_waf_packages",
        intent: "List which WAF packages Cloudflare shows as available for this zone before we enable anything.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Maps to `waf_package_query`; generic zone/security tokens may still pull other catalogs.",
        plausible_primary_entry_ids: &["cloudflare"],
    },
    DiscoveryAdversarialCase {
        id: "cloudflare_ruleset_entrypoint_update",
        intent: "Replace the managed HTTP firewall phase entrypoint ruleset on our Cloudflare zone with the reviewed rules.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Maps to `ruleset_entrypoint_update`; broad rules/firewall vocabulary stays noisy cross-catalog.",
        plausible_primary_entry_ids: &["cloudflare"],
    },
    DiscoveryAdversarialCase {
        id: "social_trending_regulation_bots",
        intent: "Surface trending discussion about the regulation change and filter out bot spam.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Trending + moderation vocabulary spans social and news APIs.",
        plausible_primary_entry_ids: &["reddit", "hackernews", "twitter", "nytimes"],
    },
    DiscoveryAdversarialCase {
        id: "spotify_playlist_wrong_credits",
        intent: "Fix wrong song credits on a few tracks in my public playlists.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Spotify playlist/track ops plus Musixmatch lyrics reads satisfy this story.",
        plausible_primary_entry_ids: &["spotify", "musixmatch"],
    },
    DiscoveryAdversarialCase {
        id: "tavily_competitor_soc2_claims",
        intent: "Search for our competitor official pricing page URLs, extract each page’s text, and note whether SOC2 is claimed.",
        kind: DiscoveryAdversarialFailureKind::SoftNoise,
        stress_note: "Aligns with Tavily `web_search` + `url_extract`; wiki/docs APIs may still overlap on page text.",
        plausible_primary_entry_ids: &["tavily"],
    },
];

/// Total adversarial cases ([`CROSS_CUTTING`] then [`PER_CATALOG_SELECTION`]).
pub const fn adversarial_case_count() -> usize {
    CROSS_CUTTING.len() + PER_CATALOG_SELECTION.len()
}

/// Iterate every case: cross-cutting first, then per-catalog selection.
pub fn iter_all_cases() -> impl Clone + Iterator<Item = &'static DiscoveryAdversarialCase> {
    CROSS_CUTTING.iter().chain(PER_CATALOG_SELECTION.iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn adversarial_case_ids_unique_and_intents_nonempty() {
        let mut seen = HashSet::new();
        for c in iter_all_cases() {
            assert!(!c.intent.trim().is_empty(), "empty intent for id {}", c.id);
            assert!(seen.insert(c.id), "duplicate adversarial case id: {}", c.id);
            assert!(
                !c.plausible_primary_entry_ids.is_empty(),
                "plausible_primary_entry_ids empty for {}",
                c.id
            );
        }
        assert_eq!(iter_all_cases().count(), adversarial_case_count());
    }
}
