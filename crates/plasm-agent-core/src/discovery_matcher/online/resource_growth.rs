//! Frozen resource-count controls; never used by production selection.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: String,
    resources: usize,
    descriptions: Vec<String>,
}

fn grown(count: usize, reverse: bool, profile: &Profile) -> BTreeMap<String, CGS> {
    let mut result = catalogs(count, false);
    if let Some(journal) = result.get_mut("journal") {
        let template = journal.entities["Receipt"].clone();
        let names: Vec<_> = journal.capabilities.keys().cloned().collect();
        for (i, name) in names.iter().enumerate() {
            let resource = format!("Resource{:04}", i % profile.resources);
            let mut entity = template.clone();
            entity.name = resource.clone().into();
            journal.entities.insert(resource.clone().into(), entity);
            let cap = journal.capabilities.get_mut(name).unwrap();
            cap.domain = resource.into();
            cap.description = profile.descriptions[i % profile.descriptions.len()].clone();
        }
    }
    if reverse {
        for cgs in result.values_mut() {
            cgs.capabilities = std::mem::take(&mut cgs.capabilities)
                .into_iter()
                .rev()
                .collect();
        }
    }
    result
}

#[tokio::test]
#[ignore = "frozen resource breadth; explicit report path required"]
async fn frozen_discovery_resource_breadth_v5() {
    let directory = root().join("../resource-breadth-v5");
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(directory.join("cases.json")).unwrap()).unwrap();
    let profiles: Vec<Profile> =
        serde_json::from_slice(&std::fs::read(directory.join("profiles.json")).unwrap()).unwrap();
    assert_eq!((fixture.version, fixture.horizon, fixture.gate), (5, 24, 4));
    let destination = PathBuf::from(std::env::var("PLASM_TRAJECTORY_REPORT").unwrap());
    assert!(!destination.exists());
    let mut results = Vec::new();
    for profile in &profiles {
        assert!(profile.resources > 0 && !profile.descriptions.is_empty());
        for case in &fixture.cases {
            for &count in &fixture.growth {
                for rotate in [false, true] {
                    let mut forward = measure_catalogs(
                        &fixture,
                        case,
                        count,
                        rotate,
                        false,
                        grown(count, false, profile),
                    )
                    .await;
                    let reverse = measure_catalogs(
                        &fixture,
                        case,
                        count,
                        rotate,
                        false,
                        grown(count, true, profile),
                    )
                    .await;
                    assert_eq!(forward["turns"], reverse["turns"]);
                    forward["id"] = format!("{}/{}", profile.id, case.id).into();
                    results.push(forward);
                }
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
