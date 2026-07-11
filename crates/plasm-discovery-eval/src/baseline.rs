use anyhow::Context;
use plasm_core::discovery::{CapabilityQuery, CgsDiscovery, InMemoryCgsRegistry, RankedCandidate};

pub fn capability_query_from_intent(intent: &str) -> CapabilityQuery {
    CapabilityQuery {
        phrases: vec![intent.to_string()],
        ..Default::default()
    }
}

pub fn baseline_discover(
    registry: &InMemoryCgsRegistry,
    intent: &str,
    k: usize,
) -> anyhow::Result<Vec<RankedCandidate>> {
    let result = registry
        .discover(&capability_query_from_intent(intent))
        .with_context(|| format!("discover failed for intent: {intent}"))?;
    Ok(result.candidates.into_iter().take(k).collect())
}

pub fn compact_candidates_for_llm(candidates: &[RankedCandidate]) -> String {
    let mut lines = vec!["rank\tentry_id\tentity\tcapability\tdescription".to_string()];
    for (i, c) in candidates.iter().enumerate() {
        let desc = if c.capability_description.len() > 120 {
            format!("{}…", &c.capability_description[..120])
        } else {
            c.capability_description.clone()
        };
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}",
            i,
            c.entry_id,
            c.entity,
            c.capability_name,
            desc.trim()
        ));
    }
    lines.join("\n")
}
