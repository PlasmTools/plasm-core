use super::*;

#[test]
fn stages_relation_target_without_admitting_an_unrequested_capability() {
    let session = crate::workflow_learning::teaching::tests::session(&[]);
    let required = vec![CapabilityRef {
        catalog: "langmatrix".into(),
        capability: "langitem_query".into(),
    }];
    let groups = BTreeMap::from([(
        "langmatrix".into(),
        BTreeSet::from(["LangItem".into(), "LangLine".into()]),
    )]);
    let (staged, _) = stage_example_symbols(session, &required, &groups, &[]).unwrap();
    let exposure = staged.teaching_exposure.as_ref().unwrap();
    assert!(exposure
        .exposed_relation_symbol_rows()
        .iter()
        .any(|row| row.wire == "lines"));
    assert!(!exposure
        .surface
        .capabilities
        .iter()
        .any(|cap| cap.domain.as_str() == "LangLine"));
}
