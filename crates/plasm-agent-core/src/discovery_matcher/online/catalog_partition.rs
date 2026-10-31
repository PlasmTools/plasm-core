//! Frozen catalogue-boundary stress controls; no production selection logic.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Partition {
    id: String,
    shards: usize,
    description: String,
}

fn partitioned(count: usize, profile: &Partition, reverse: bool) -> BTreeMap<String, CGS> {
    let mut result = catalogs(0, false);
    let template = result["workshop"].clone();
    for i in 0..count {
        let entry = if profile.shards == 0 {
            "workshop".to_string()
        } else {
            format!("archive{:03}", i % profile.shards)
        };
        let cgs = result.entry(entry.clone()).or_insert_with(|| {
            let mut cgs = template.clone();
            cgs.bind_registry_entry_id(&entry);
            cgs.capabilities.clear();
            cgs
        });
        let mut capability = template.capabilities["record"].clone();
        capability.name = format!("archive_entry_{i:04}").into();
        capability.description = profile.description.clone();
        cgs.capabilities.insert(capability.name.clone(), capability);
    }
    assert_eq!(
        result.values().map(|c| c.capabilities.len()).sum::<usize>(),
        template.capabilities.len() + count
    );
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
#[ignore = "frozen catalogue partition stress; explicit report path required"]
async fn frozen_catalog_partition_stress_v1() {
    let directory = root().join("../catalog-partition-v1");
    let fixture: Fixture =
        serde_json::from_slice(&std::fs::read(directory.join("cases.json")).unwrap()).unwrap();
    let profiles: Vec<Partition> =
        serde_json::from_slice(&std::fs::read(directory.join("profiles.json")).unwrap()).unwrap();
    assert_eq!((fixture.horizon, fixture.gate), (24, 4));
    let destination = PathBuf::from(std::env::var("PLASM_TRAJECTORY_REPORT").unwrap());
    assert!(!destination.exists());
    let mut results = Vec::new();
    for profile in &profiles {
        for case in &fixture.cases {
            for &count in &fixture.growth {
                for rotate in [false, true] {
                    let mut forward = measure_catalogs(
                        &fixture,
                        case,
                        count,
                        rotate,
                        false,
                        partitioned(count, profile, false),
                    )
                    .await;
                    let reverse = measure_catalogs(
                        &fixture,
                        case,
                        count,
                        rotate,
                        false,
                        partitioned(count, profile, true),
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
