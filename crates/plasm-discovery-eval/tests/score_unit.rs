use plasm_core::RankedCandidate;
use plasm_discovery_eval::{classify_failure, score_candidates, DiscoveryExpect, FailureClass};

#[test]
fn hit_at_1_entry() {
    let expect = DiscoveryExpect {
        entry_id_any: vec!["gitlab".into()],
        entity_any: vec![],
        capability_name_any: vec![],
        ambiguous: false,
    };
    let cands = vec![RankedCandidate {
        entry_id: "gitlab".into(),
        entity: "MergeRequest".into(),
        capability_name: "mr_create".into(),
        score: 3,
        reason_codes: vec![],
        capability_description: String::new(),
    }];
    assert!(score_candidates(&expect, &cands, 10).hit_at_1_entry);
}

#[test]
fn hard_miss_class() {
    let expect = DiscoveryExpect {
        entry_id_any: vec!["gitlab".into()],
        entity_any: vec![],
        capability_name_any: vec![],
        ambiguous: false,
    };
    let cands = vec![RankedCandidate {
        entry_id: "github".into(),
        entity: "PullRequest".into(),
        capability_name: "pr_create".into(),
        score: 5,
        reason_codes: vec![],
        capability_description: String::new(),
    }];
    let m = score_candidates(&expect, &cands, 10);
    assert_eq!(
        classify_failure(m.hit_at_1_entry, m.hit_at_3_entry, m.noise_at_k, false),
        FailureClass::HardMiss
    );
}
