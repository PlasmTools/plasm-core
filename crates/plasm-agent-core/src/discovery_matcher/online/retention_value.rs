//! Whole-pipeline retention ablation. No provider or catalogue effects.
use super::*;
use std::io::Write;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: String,
    resources: usize,
    descriptions: Vec<String>,
}

// Identical frozen v5 growth recipe; the original module/lock is left untouched.
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
#[ignore = "explicit frozen operational retention ablation"]
async fn frozen_retention_value_v1() {
    let out = PathBuf::from(std::env::var("PLASM_RETENTION_REPORT").unwrap());
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out)
        .unwrap();
    let mut variant = 0;
    for cohort in ["trajectory-v4", "resource-breadth-v5", "root-diversity-v1"] {
        let dir = root().join("..").join(cohort);
        let fixture: Fixture =
            serde_json::from_slice(&std::fs::read(dir.join("cases.json")).unwrap()).unwrap();
        assert_eq!((fixture.horizon, fixture.gate), (24, 4));
        let profiles: Vec<Option<Profile>> = if cohort == "resource-breadth-v5" {
            serde_json::from_slice::<Vec<Profile>>(
                &std::fs::read(dir.join("profiles.json")).unwrap(),
            )
            .unwrap()
            .into_iter()
            .map(Some)
            .collect()
        } else {
            vec![None]
        };
        for profile in &profiles {
            for case in &fixture.cases {
                for &count in &fixture.growth {
                    for rotate in [false, true] {
                        for retain in if variant % 2 == 0 {
                            [false, true]
                        } else {
                            [true, false]
                        } {
                            let construct = |reverse| match profile {
                                Some(p) => grown(count, reverse, p),
                                None => catalogs(count, reverse),
                            };
                            let mut forward = measure_retention(
                                &fixture,
                                case,
                                count,
                                rotate,
                                false,
                                construct(false),
                                retain,
                            )
                            .await;
                            let reverse = measure_retention(
                                &fixture,
                                case,
                                count,
                                rotate,
                                false,
                                construct(true),
                                retain,
                            )
                            .await;
                            assert_eq!(
                                forward["turns"], reverse["turns"],
                                "insertion parity: {cohort}/{}/{count}/{rotate}/{retain}",
                                case.id
                            );
                            if let Some(p) = profile {
                                forward["id"] = format!("{}/{}", p.id, case.id).into();
                            }
                            forward["cohort"] = cohort.into();
                            forward["arm"] = if retain {
                                "retain_admitted"
                            } else {
                                "fixture_judgment"
                            }
                            .into();
                            writeln!(file, "{}", forward).unwrap();
                            file.flush().unwrap();
                            println!(
                                "measured {cohort}/{}/{count}/{rotate}/retain={retain}",
                                case.id
                            );
                        }
                        variant += 1;
                    }
                }
            }
        }
    }
    assert_eq!(variant, 58);
}
