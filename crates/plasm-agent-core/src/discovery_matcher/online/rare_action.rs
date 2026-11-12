//! Fresh action-discrimination controls for the IDF ablation.
use super::*;

#[derive(Deserialize)]
struct Profile {
    id: String,
    effect: String,
    distractor: String,
}

#[tokio::test]
#[ignore = "frozen rare-action controls; explicit output required"]
async fn frozen_rare_action_v1() {
    let directory = root().join("../idf-ablation-v1");
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(directory.join("cases.json")).unwrap()).unwrap();
    let profiles: Vec<Profile> =
        serde_json::from_slice(&std::fs::read(directory.join("profiles.json")).unwrap()).unwrap();
    let destination = PathBuf::from(std::env::var("PLASM_TRAJECTORY_REPORT").unwrap());
    assert!(!destination.exists());
    let mut results = Vec::new();
    for case in &fixture.cases {
        let profile = profiles.iter().find(|p| p.id == case.id).unwrap();
        for &count in &fixture.growth {
            for rotate in [false, true] {
                let mut results_by_order = Vec::new();
                for reverse in [false, true] {
                    let mut cgs = catalogs(0, false).remove("workshop").unwrap();
                    let mut target = cgs.capabilities["routine"].clone();
                    target.description = profile.effect.clone();
                    target.output_schema.as_mut().unwrap().output_type =
                        plasm_core::schema::OutputType::SideEffect {
                            description: profile.effect.clone(),
                        };
                    cgs.capabilities.clear();
                    cgs.capabilities.insert("routine".into(), target.clone());
                    for i in 0..count {
                        let mut cap = target.clone();
                        cap.name = format!("inspection_{i:04}").into();
                        cap.description = profile.distractor.clone();
                        cap.output_schema.as_mut().unwrap().output_type =
                            plasm_core::schema::OutputType::SideEffect {
                                description: profile.distractor.clone(),
                            };
                        cgs.capabilities.insert(cap.name.clone(), cap);
                    }
                    if reverse {
                        cgs.capabilities = cgs.capabilities.into_iter().rev().collect();
                    }
                    results_by_order.push(
                        measure_catalogs(
                            &fixture,
                            case,
                            count,
                            rotate,
                            false,
                            BTreeMap::from([("workshop".into(), cgs)]),
                        )
                        .await,
                    );
                }
                assert_eq!(results_by_order[0]["turns"], results_by_order[1]["turns"]);
                results.push(results_by_order.remove(0));
            }
        }
    }
    std::fs::write(
        destination,
        serde_json::to_vec_pretty(
            &json!({"policy":SEARCH_POLICY,"cases":results,"provider_calls":0}),
        )
        .unwrap(),
    )
    .unwrap();
}
