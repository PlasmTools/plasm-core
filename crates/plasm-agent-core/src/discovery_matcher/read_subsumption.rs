//! Remove a selected read only when the pinned catalogue proves another selected
//! read uses the identical execution contract and provides a strict field superset.
use plasm_core::{
    prerequisites::{CapabilityRef, DeploymentBindings},
    schema::CapabilitySchema,
    CapabilityKind, CGS,
};
use std::collections::{BTreeMap, BTreeSet};

fn subsumes(wider: &CapabilitySchema, narrower: &CapabilitySchema) -> bool {
    if wider.kind != CapabilityKind::Get
        || narrower.kind != CapabilityKind::Get
        || wider.domain != narrower.domain
        || wider.mapping.is_none()
        || wider.mapping != narrower.mapping
        || wider.derived.is_some()
        || narrower.derived.is_some()
        || wider.preflight.is_some()
        || narrower.preflight.is_some()
        || wider.inputs != narrower.inputs
        || serde_json::to_value(&wider.output_schema).ok()
            != serde_json::to_value(&narrower.output_schema).ok()
        || wider.scope_aggregate_key_policy != narrower.scope_aggregate_key_policy
        || wider.identity_key != narrower.identity_key
        || !wider.sanitizes.is_empty()
        || !narrower.sanitizes.is_empty()
        || wider.deterministic == Some(false)
        || narrower.deterministic == Some(false)
        || wider.provides.is_empty()
        || narrower.provides.is_empty()
    {
        return false;
    }
    let a: BTreeSet<_> = wider.provides.iter().collect();
    let b: BTreeSet<_> = narrower.provides.iter().collect();
    b.is_subset(&a) && b != a
}

pub(crate) fn remove_subsumed(
    selected: &mut BTreeSet<CapabilityRef>,
    catalogs: &BTreeMap<String, CGS>,
    bindings: &DeploymentBindings,
) {
    let all = selected.clone();
    selected.retain(|narrow_ref| {
        let Some(catalog) = catalogs.get(&narrow_ref.catalog) else {
            return true;
        };
        let Some(narrow) = catalog.capabilities.get(narrow_ref.capability.as_str()) else {
            return true;
        };
        // A third selected operation may refer to this exact read by identity.
        // Do not rewrite producer flows or remove declared prerequisite providers.
        if all
            .iter()
            .filter(|r| r.catalog == narrow_ref.catalog)
            .count()
            != 2
            || catalog
                .prerequisites
                .providers
                .values()
                .any(|p| p.capability == narrow_ref.capability)
        {
            return true;
        }
        if catalog
            .prerequisites
            .requirements
            .contains_key(&narrow_ref.capability)
            || bindings
                .bindings
                .iter()
                .any(|binding| &binding.consumer == narrow_ref)
        {
            return true;
        }
        !all.iter().any(|wide_ref| {
            if wide_ref == narrow_ref
                || wide_ref.catalog != narrow_ref.catalog
                || catalog
                    .prerequisites
                    .providers
                    .values()
                    .any(|p| p.capability == wide_ref.capability)
                || catalog
                    .prerequisites
                    .requirements
                    .contains_key(&wide_ref.capability)
                || bindings
                    .bindings
                    .iter()
                    .any(|binding| &binding.consumer == wide_ref)
            {
                return false;
            }
            catalog
                .capabilities
                .get(wide_ref.capability.as_str())
                .is_some_and(|wide| subsumes(wide, narrow))
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_identical_keyed_read_contract_with_strict_field_superset_subsumes() {
        let path = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let graph = plasm_core::loader::load_schema_dir(&path).unwrap();
        let base = graph
            .capabilities
            .values()
            .find(|cap| {
                cap.kind == CapabilityKind::Get && cap.mapping.is_some() && cap.preflight.is_none()
            })
            .unwrap()
            .clone();
        let mut wide = base.clone();
        let mut narrow = base.clone();
        wide.provides = vec!["first".into(), "second".into()];
        narrow.provides = vec!["first".into()];
        assert!(subsumes(&wide, &narrow));
        assert!(!subsumes(&narrow, &wide));
        let mut other = narrow.clone();
        other.kind = CapabilityKind::Query;
        assert!(!subsumes(&wide, &other));
        let mut other = narrow.clone();
        other.mapping = None;
        assert!(!subsumes(&wide, &other));
        let mut other = narrow.clone();
        other.provides.clear();
        assert!(!subsumes(&wide, &other));
        let mut other = narrow.clone();
        other.deterministic = Some(false);
        assert!(!subsumes(&wide, &other));
    }
    #[test]
    fn selected_reads_are_pruned_only_without_other_local_producer_flows() {
        let path = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let mut graph = plasm_core::loader::load_schema_dir(&path).unwrap();
        let base = graph
            .capabilities
            .values()
            .find(|cap| {
                cap.kind == CapabilityKind::Get && cap.mapping.is_some() && cap.preflight.is_none()
            })
            .unwrap()
            .clone();
        graph.capabilities.clear();
        graph.prerequisites = Default::default();
        let mut wide = base.clone();
        wide.name = "wide".into();
        wide.provides = vec!["first".into(), "second".into()];
        let mut narrow = base.clone();
        narrow.name = "narrow".into();
        narrow.provides = vec!["first".into()];
        let mut third = base;
        third.name = "third".into();
        graph.capabilities.insert("wide".into(), wide);
        graph.capabilities.insert("narrow".into(), narrow);
        graph.capabilities.insert("third".into(), third);
        let catalogs = BTreeMap::from([("fixture".to_owned(), graph)]);
        let reference = |name: &str| CapabilityRef {
            catalog: "fixture".into(),
            capability: name.into(),
        };
        let mut selected = BTreeSet::from([reference("wide"), reference("narrow")]);
        remove_subsumed(&mut selected, &catalogs, &DeploymentBindings::default());
        assert_eq!(selected, BTreeSet::from([reference("wide")]));
        let mut selected =
            BTreeSet::from([reference("wide"), reference("narrow"), reference("third")]);
        remove_subsumed(&mut selected, &catalogs, &DeploymentBindings::default());
        assert_eq!(selected.len(), 3);
    }
}
