//! Paired role reversals: full requests reach judgment, lexical word sets cannot distinguish them.
use super::*;

#[derive(Deserialize)]
struct Profile {
    id: String,
    left_effect: String,
    right_effect: String,
    distractor: String,
}

#[tokio::test]
#[ignore = "frozen action-role diagnostic; explicit report path required"]
async fn frozen_action_role_pairs_v1() {
    let directory = root().join("../action-role-pairs-v1");
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(directory.join("cases.json")).unwrap()).unwrap();
    let profiles: Vec<Profile> =
        serde_json::from_slice(&std::fs::read(directory.join("profiles.json")).unwrap()).unwrap();
    let destination = PathBuf::from(std::env::var("PLASM_TRAJECTORY_REPORT").unwrap());
    assert!(!destination.exists());
    let mut results = Vec::new();
    for profile in &profiles {
        for &count in &fixture.growth {
            let mut pair = Vec::new();
            for side in ["left", "right"] {
                let case = fixture
                    .cases
                    .iter()
                    .find(|c| c.id == format!("{}/{side}", profile.id))
                    .unwrap();
                let mut order_results = Vec::new();
                for reverse in [false, true] {
                    let mut cgs = catalogs(0, false).remove("workshop").unwrap();
                    let template = cgs.capabilities["routine"].clone();
                    cgs.capabilities.clear();
                    let descriptions = [
                        ("routine".to_owned(), profile.left_effect.clone()),
                        ("record".to_owned(), profile.right_effect.clone()),
                    ]
                    .into_iter()
                    .chain(
                        (0..count).map(|i| (format!("context_{i:04}"), profile.distractor.clone())),
                    );
                    for (name, description) in descriptions {
                        let mut cap = template.clone();
                        cap.name = name.clone().into();
                        cap.description = description.clone();
                        cap.output_schema.as_mut().unwrap().output_type =
                            plasm_core::schema::OutputType::SideEffect { description };
                        cgs.capabilities.insert(name.into(), cap);
                    }
                    if reverse {
                        cgs.capabilities = cgs.capabilities.into_iter().rev().collect();
                    }
                    order_results.push(
                        measure_catalogs(
                            &fixture,
                            case,
                            count,
                            false,
                            false,
                            BTreeMap::from([("workshop".into(), cgs)]),
                        )
                        .await,
                    );
                }
                assert_eq!(order_results[0]["turns"], order_results[1]["turns"]);
                pair.push(order_results.remove(0));
            }
            // Diagnostic assertion, not desired semantic equivalence: the roles differ.
            assert_eq!(
                pair[0]["turns"][0]["attribution"]["scores"],
                pair[1]["turns"][0]["attribution"]["scores"]
            );
            results.extend(pair);
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
