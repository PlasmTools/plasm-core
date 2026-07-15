//! Authored discovery-alias coverage against an intent string.

use super::corpus::RequirementWitness;

/// True when an authored discovery alias phrase is covered by `intent`.
///
/// Authored aliases only — never entity-id SoftNLP / camelCase stem matching.
pub(crate) fn witness_named_in_intent(intent: &str, witness: &RequirementWitness) -> bool {
    for alias in witness
        .aliases
        .split([',', ';', '|', '\n'])
    {
        let a = alias.trim();
        if a.len() >= 3 && crate::catalog_search_index::phrase_tokens_covered_by_intent(a, intent) {
            return true;
        }
    }
    false
}
